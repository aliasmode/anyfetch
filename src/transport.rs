use crate::{ToolError, anubis};
use encoding_rs::{Encoding, UTF_8};
use wreq::{Client, Response};
use wreq_util::{Emulation, Profile};

pub fn client() -> Result<Client, ToolError> {
    Ok(Client::builder()
        .emulation(Emulation::builder().profile(Profile::Firefox151).build())
        .cookie_store(true)
        .redirect(wreq::redirect::Policy::default())
        .no_proxy()
        .build()?)
}

pub(crate) struct Body {
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl Body {
    pub fn text(&self) -> String {
        let encoding = self
            .content_type
            .parse::<mime::Mime>()
            .ok()
            .and_then(|mime| {
                mime.get_param(mime::CHARSET)
                    .and_then(|label| Encoding::for_label(label.as_str().as_bytes()))
            })
            .unwrap_or(UTF_8);
        encoding.decode(&self.bytes).0.into_owned()
    }
}

pub(crate) async fn get(url: &str, accept: fn(&str) -> bool) -> Result<Body, ToolError> {
    let client = client()?;
    let (body, challenge) = read(client.get(url).send().await?, false, accept).await?;
    let Some(challenge) = challenge else {
        return Ok(body);
    };
    let completion = challenge.completion_url(&body.url).await?;
    let response = client
        .get(completion.as_str())
        .send()
        .await
        .map_err(|_| ToolError::new("network_error", "Anubis completion request failed"))?;
    Ok(read(response, true, accept).await?.0)
}

async fn read(
    response: Response,
    completed: bool,
    accept: fn(&str) -> bool,
) -> Result<(Body, Option<anubis::Challenge>), ToolError> {
    let status = response.status().as_u16();
    let error = check_response(&response).err();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let mime = content_type.parse::<mime::Mime>().ok();
    let media_type = mime.as_ref().map_or("", mime::Mime::essence_str);
    let is_html = matches!(media_type, "text/html" | "application/xhtml+xml");
    if error
        .as_ref()
        .is_some_and(|error| !is_html || error.code != "http_error")
    {
        return Err(error.unwrap());
    }
    if !accept(media_type) {
        return Err(ToolError {
            status: Some(status),
            ..ToolError::new(
                "unsupported_content",
                format!("Unsupported content type: {content_type}"),
            )
        });
    }
    let url = response.uri().to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| {
            if completed {
                ToolError::new("network_error", "Could not read Anubis completion response")
            } else {
                error.into()
            }
        })?
        .to_vec();
    let body = Body {
        url,
        status,
        content_type,
        bytes,
    };
    if is_html
        && let Some(challenge) = anubis::parse(&body.text()).map_err(|mut error| {
            error.status = Some(status);
            error
        })?
    {
        if completed {
            return Err(ToolError {
                status: Some(status),
                ..ToolError::new(
                    "challenge_failed",
                    "Anubis challenge remains after completion",
                )
            });
        }
        return Ok((body, Some(challenge)));
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok((body, None))
}

pub fn check_response(response: &Response) -> Result<(), ToolError> {
    let status = response.status().as_u16();
    let challenge = response
        .headers()
        .get("cf-mitigated")
        .is_some_and(|value| value == "challenge");
    let code = if challenge {
        "challenge_required"
    } else if status == 429 {
        "rate_limited"
    } else if !response.status().is_success() {
        "http_error"
    } else {
        return Ok(());
    };
    Err(ToolError {
        code,
        message: format!("The server returned HTTP {status}"),
        provider: None,
        status: Some(status),
        retry_after: response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    })
}
