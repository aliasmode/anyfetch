use crate::{
    ToolError, anubis, challenge,
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
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// The identity for requests that carry a clearance from the user's Firefox.
const FIREFOX: (Profile, Platform) = (Profile::Firefox151, Platform::Linux);

fn client(
    (profile, platform): (Profile, Platform),
    cleared: Option<(&Clearance, &str)>,
    direct: bool,
) -> Result<(Client, Arc<Jar>), ToolError> {
    let jar = Arc::new(Jar::default());
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
        .cookie_provider(jar.clone())
        .redirect(wreq::redirect::Policy::default())
        .timeout(TIMEOUT)
        .no_proxy();
    if !direct && let (Some(proxy), _) = proxy()? {
        builder = builder.proxy(proxy);
    }
    Ok((builder.build()?, jar))
}

/// Optional HTTP proxy from `~/.config/anyfetch/proxy`: one `host:port:username:password` line.
/// Then every request goes through the proxy. With a second line `direct`, a request that is
/// refused through the proxy is sent again straight to the site.
fn proxy() -> Result<(Option<Proxy>, bool), ToolError> {
    let Some(home) = std::env::home_dir() else {
        return Ok((None, false));
    };
    let Ok(file) = std::fs::read_to_string(home.join(".config/anyfetch/proxy")) else {
        return Ok((None, false));
    };
    let invalid = || {
        ToolError::new(
            "proxy_error",
            "The proxy file must contain host:port:username:password, and can have a second line: direct",
        )
    };
    let mut lines = file.lines().map(str::trim).filter(|line| !line.is_empty());
    let mut parts = lines.next().unwrap_or_default().splitn(4, ':');
    let (Some(host), Some(port), Some(username), Some(password)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    let both = match lines.next() {
        None => false,
        Some("direct") => true,
        Some(_) => return Err(invalid()),
    };
    let proxy = Proxy::all(format!("http://{host}:{port}"))
        .map_err(|_| invalid())?
        .basic_auth(username, password)
        .no_proxy(NoProxy::from_string("localhost,127.0.0.1,::1"));
    Ok((Some(proxy), both))
}

/// Runs `request` through the proxy if there is one. When the site refuses the proxy's address,
/// or the proxy does not answer, and the user allows both routes, runs it again without the
/// proxy. `request` gets `true` for the route without the proxy.
async fn routed<F: Future<Output = Result<Body, ToolError>>>(
    request: impl Fn(bool) -> F,
) -> Result<Body, ToolError> {
    let (proxy, both) = proxy()?;
    match request(proxy.is_none()).await {
        Err(error) if both && refused(&error) => request(true).await.or(Err(error)),
        result => result,
    }
}

fn refused(error: &ToolError) -> bool {
    // A slow site is slow on both routes, so a timeout is not a refusal.
    let codes = ["challenge_required", "rate_limited", "network_error"];
    codes.contains(&error.code) || matches!(error.status, Some(401..=403))
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
    routed(|direct| attempt(url, accept, IDENTITIES[0], None, direct)).await
}

/// One request to a data address of a site, with the headers that the site's own pages send.
/// Some of these answer only a visitor of the site: with `entry`, the same client opens that
/// page first.
pub(crate) async fn get_with(
    url: &str,
    headers: &[(&str, &str)],
    entry: Option<&str>,
) -> Result<Body, ToolError> {
    routed(|direct| async move {
        let (client, jar) = client(IDENTITIES[0], None, direct)?;
        if let Some(entry) = entry {
            open(&client, &jar, entry, |_| true, None).await?;
        }
        let mut request = client.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        Ok(read(request.send().await?, true, |_| true).await?.0)
    })
    .await
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
    routed(|direct| page(url, accept, browser, direct)).await
}

async fn page(
    url: &str,
    accept: fn(&str) -> bool,
    browser: Option<&Path>,
    direct: bool,
) -> Result<Body, ToolError> {
    let host = Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    // A clearance is tied to the address that earned it, so it is no use behind a proxy.
    if direct
        && let Some(host) = &host
        && let Some(saved) = clearance::load(host)
    {
        match attempt(url, accept, FIREFOX, Some(&saved), direct).await {
            Err(error) if cloudflare_challenge(&error) => clearance::forget(host),
            // Let the usual identities try a page that this one is refused.
            Err(error) if matches!(error.status, Some(401 | 403)) => {}
            result => return result,
        }
    }
    let first = || attempt(url, accept, IDENTITIES[0], None, direct);
    let mut result = first().await;
    for _ in 0..2 {
        if !matches!(&result, Err(error) if cloudflare_challenge(error)) {
            break;
        }
        result = first().await;
    }
    match result {
        Err(error) if cloudflare_challenge(&error) => {
            let Some(browser) = browser.filter(|_| direct && public_site(url)) else {
                return Err(error);
            };
            match clearance::ensure(browser, url).await {
                Ok(fresh) => attempt(url, accept, FIREFOX, Some(&fresh), direct).await,
                Err(_) => Err(error),
            }
        }
        Err(error) if matches!(error.status, Some(401 | 403)) => {
            match attempt(url, accept, IDENTITIES[1], None, direct).await {
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

/// True when requests may go straight to the site and not only through a proxy.
pub(crate) fn direct() -> bool {
    matches!(proxy(), Ok((None, _) | (_, true)))
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
    direct: bool,
) -> Result<Body, ToolError> {
    let (client, jar) = client(identity, cleared.map(|clearance| (clearance, url)), direct)?;
    let agent = cleared.map(|clearance| clearance.user_agent.as_str());
    open(&client, &jar, url, accept, agent).await
}

/// What a page asks a browser to do before it shows its content.
enum Check {
    Anubis(anubis::Challenge),
    /// Set this cookie and ask for the page again.
    Cookie(String),
    /// Ask for the page with this query.
    Query(String),
}

/// Asks for `url` in `client`. If the answer is a bot check that a plain client can pass, does
/// what the check asks for and returns the page that follows.
async fn open(
    client: &Client,
    jar: &Jar,
    url: &str,
    accept: fn(&str) -> bool,
    agent: Option<&str>,
) -> Result<Body, ToolError> {
    let get = |target: &str| match agent {
        Some(agent) => client.get(target).header("user-agent", agent),
        None => client.get(target),
    };
    let (body, check) = read(get(url).send().await?, false, accept).await?;
    let next = match check {
        None => return Ok(body),
        Some(Check::Anubis(challenge)) => challenge.completion_url(&body.url).await?.into(),
        Some(Check::Cookie(cookie)) => {
            jar.add(format!("{cookie}; Path=/"), body.url.as_str());
            body.url.clone()
        }
        Some(Check::Query(query)) => {
            let mut page = Url::parse(&body.url)
                .map_err(|_| ToolError::new("network_error", "The page has no valid address"))?;
            page.set_query(Some(&query));
            page.into()
        }
    };
    let response = get(&next)
        .header("referer", &body.url)
        .send()
        .await
        .map_err(|_| ToolError::new("network_error", "The request after the bot check failed"))?;
    Ok(read(response, true, accept).await?.0)
}

async fn read(
    response: Response,
    completed: bool,
    accept: fn(&str) -> bool,
) -> Result<(Body, Option<Check>), ToolError> {
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
        return Ok((body, Some(Check::Anubis(challenge))));
    }
    if is_html {
        let page = body.text();
        let check = challenge::cookie(&page).map(Check::Cookie);
        if let Some(check) = check.or_else(|| challenge::doubled(&page).map(Check::Query)) {
            if completed {
                return Err(ToolError {
                    status: Some(status),
                    ..ToolError::new("challenge_failed", "The bot check remains after its step")
                });
            }
            return Ok((body, Some(check)));
        }
        if challenge::waiting_page(&page) {
            return Err(ToolError {
                status: Some(status),
                ..ToolError::new(
                    "challenge_required",
                    "The site answered with a bot check page",
                )
            });
        }
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
