use crate::{
    ToolError, anubis,
    clearance::{self, Clearance},
};
use encoding_rs::{Encoding, UTF_8};
use std::{path::Path, sync::Arc, time::Duration};
use url::{Host, Url};
use wreq::{Client, NoProxy, Proxy, Response, cookie::Jar};
use wreq_util::{Emulation, Platform, Profile};

/// Browser identities in the order tried. Measured on 97 bot-filtered sites (2026-10-11), Safari
/// read the most; some DataDome sites accepted only a Chromium identity on Windows.
const IDENTITIES: [(Profile, Platform); 2] = [
    (Profile::Safari26_4, Platform::MacOS),
    (Profile::Edge148, Platform::Windows),
];

/// The time one request may take, from connecting to the last byte of the answer.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The identity for requests that carry a clearance from the user's Firefox.
const FIREFOX: (Profile, Platform) = (Profile::Firefox151, Platform::Linux);

fn client(
    (profile, platform): (Profile, Platform),
    cleared: Option<(&Clearance, &str)>,
) -> Result<Client, ToolError> {
    let jar = Jar::default();
    for cookie in cleared.iter().flat_map(|(clearance, _)| &clearance.cookies) {
        let (name, value) = (&cookie.name, &cookie.value);
        let domain = cookie.domain.trim_start_matches('.');
        let url = cleared.map_or("", |(_, url)| url);
        jar.add(format!("{name}={value}; Domain={domain}; Path=/"), url);
    }
    let mut builder = Client::builder()
        .emulation(
            Emulation::builder()
                .profile(profile)
                .platform(platform)
                .build(),
        )
        .cookie_provider(Arc::new(jar))
        .redirect(wreq::redirect::Policy::default())
        .timeout(TIMEOUT)
        .no_proxy();
    if let Some(proxy) = proxy()? {
        builder = builder.proxy(proxy);
    }
    Ok(builder.build()?)
}

/// Optional HTTP proxy from `~/.config/fetchroam/proxy`: one `host:port:username:password` line.
fn proxy() -> Result<Option<Proxy>, ToolError> {
    let Some(home) = std::env::home_dir() else {
        return Ok(None);
    };
    let Ok(line) = std::fs::read_to_string(home.join(".config/fetchroam/proxy")) else {
        return Ok(None);
    };
    let invalid = || {
        ToolError::new(
            "proxy_error",
            "The proxy file must contain host:port:username:password",
        )
    };
    let mut parts = line.trim().splitn(4, ':');
    let (Some(host), Some(port), Some(username), Some(password)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    Ok(Some(
        Proxy::all(format!("http://{host}:{port}"))
            .map_err(|_| invalid())?
            .basic_auth(username, password)
            .no_proxy(NoProxy::from_string("localhost,127.0.0.1,::1")),
    ))
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
    attempt(url, accept, IDENTITIES[0], None).await
}

/// Like `get`, but retries a refused page. Cloudflare's verdict on one identical request varies
/// between attempts (measured: about two in three pass), so a Cloudflare challenge is tried again
/// as the same identity. Any other refusal is tried once as the second browser identity. A
/// challenge that stays is passed once in `browser`, and the clearance is reused for the site.
pub(crate) async fn get_page(
    url: &str,
    accept: fn(&str) -> bool,
    browser: Option<&Path>,
) -> Result<Body, ToolError> {
    let host = Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    // A clearance is tied to the address that earned it, so it is no use behind a proxy.
    let direct = matches!(proxy(), Ok(None));
    if direct
        && let Some(host) = &host
        && let Some(saved) = clearance::load(host)
    {
        match attempt(url, accept, FIREFOX, Some(&saved)).await {
            Err(error) if cloudflare_challenge(&error) => clearance::forget(host),
            // Let the usual identities try a page that this one is refused.
            Err(error) if matches!(error.status, Some(401 | 403)) => {}
            result => return result,
        }
    }
    let mut result = get(url, accept).await;
    for _ in 0..2 {
        if !matches!(&result, Err(error) if cloudflare_challenge(error)) {
            break;
        }
        result = get(url, accept).await;
    }
    match result {
        Err(error) if cloudflare_challenge(&error) => {
            let Some(browser) = browser.filter(|_| direct && public_site(url)) else {
                return Err(error);
            };
            match clearance::ensure(browser, url).await {
                Ok(fresh) => attempt(url, accept, FIREFOX, Some(&fresh)).await,
                Err(_) => Err(error),
            }
        }
        Err(error) if matches!(error.status, Some(401 | 403)) => {
            match attempt(url, accept, IDENTITIES[1], None).await {
                Err(retry) if matches!(retry.code, "challenge_required" | "rate_limited") => {
                    Err(retry)
                }
                Err(_) => Err(error),
                page => page,
            }
        }
        result => result,
    }
}

fn cloudflare_challenge(error: &ToolError) -> bool {
    error.code == "challenge_required" && error.status == Some(403)
}

/// The browser is kept away from addresses that are not public sites.
pub(crate) fn public_site(url: &str) -> bool {
    match Url::parse(url).ok().as_ref().and_then(Url::host) {
        // A name without a dot is a machine on the local network.
        Some(Host::Domain(domain)) => domain.contains('.') && domain != "localhost",
        Some(Host::Ipv4(address)) => {
            !(address.is_loopback() || address.is_private() || address.is_link_local())
        }
        Some(Host::Ipv6(address)) => !(address.is_loopback() || address.is_unique_local()),
        None => false,
    }
}

async fn attempt(
    url: &str,
    accept: fn(&str) -> bool,
    identity: (Profile, Platform),
    cleared: Option<&Clearance>,
) -> Result<Body, ToolError> {
    let client = client(identity, cleared.map(|clearance| (clearance, url)))?;
    let get = |target: &str| match cleared {
        Some(clearance) => client
            .get(target)
            .header("user-agent", &clearance.user_agent),
        None => client.get(target),
    };
    let (body, challenge) = read(get(url).send().await?, false, accept).await?;
    let Some(challenge) = challenge else {
        return Ok(body);
    };
    let completion = challenge.completion_url(&body.url).await?;
    let response = get(completion.as_str())
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
    let headers = response.headers();
    let challenge = headers
        .get("cf-mitigated")
        .is_some_and(|value| value == "challenge")
        || headers.contains_key("x-amzn-waf-action");
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
