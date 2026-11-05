use crate::{
    ToolError, anubis, challenge,
    clearance::{self, Clearance},
};
use encoding_rs::{Encoding, UTF_8};
use futures_util::StreamExt;
use std::{
    error::Error,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
    sync::Arc,
    time::Duration,
};
use url::{Host, Url};
use wreq::{
    Client, NoProxy, Proxy, Response,
    cookie::Jar,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect,
};
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

/// The largest answer read into memory.
pub(crate) const MAX_BODY: usize = 20 * 1024 * 1024;

fn client(
    (profile, platform): (Profile, Platform),
    cleared: Option<(&Clearance, &str)>,
    direct: bool,
    local: bool,
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
        .redirect(redirect::Policy::custom(move |attempt| {
            match guard(&attempt.uri.to_string(), local) {
                Ok(()) => redirect::Policy::default().redirect(attempt),
                Err(error) => attempt.error(error),
            }
        }))
        .timeout(TIMEOUT)
        .no_proxy();
    // Through a proxy the proxy resolves names, so only the address checks in `guard` apply.
    if !direct && let (Some(proxy), _) = proxy()? {
        builder = builder.proxy(proxy);
    } else {
        builder = builder.dns_resolver(Resolver { local });
    }
    Ok((builder.build()?, jar))
}

/// Where an address leads.
#[derive(Debug, PartialEq)]
enum Reach {
    Public,
    /// This machine or its local network.
    Private,
    /// Link-local and cloud metadata addresses, never fetched.
    Blocked,
}

fn ip_reach(address: IpAddr) -> Reach {
    match address {
        IpAddr::V4(address) => ipv4_reach(address),
        IpAddr::V6(address) => match address.to_ipv4_mapped() {
            Some(mapped) => ipv4_reach(mapped),
            None => ipv6_reach(address),
        },
    }
}

fn ipv4_reach(address: Ipv4Addr) -> Reach {
    let [first, second, ..] = address.octets();
    // 100.100.100.200 is the Alibaba Cloud metadata service.
    if address.is_link_local() || address == Ipv4Addr::new(100, 100, 100, 200) {
        Reach::Blocked
    } else if address.is_loopback()
        || address.is_private()
        || address.is_unspecified()
        // 100.64.0.0/10, carrier-grade NAT: the top two bits of the second octet are 01.
        || (first == 100 && second & 0xc0 == 0x40)
    {
        Reach::Private
    } else {
        Reach::Public
    }
}

fn ipv6_reach(address: Ipv6Addr) -> Reach {
    let aws_metadata = Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254);
    let prefix = &address.segments()[..6];
    // NAT64 (64:ff9b::/96) and IPv4-compatible (::a.b.c.d) addresses lead to the IPv4 address
    // in their low 32 bits; `::` and `::1` keep their own meaning.
    let nat64 = prefix == [0x64, 0xff9b, 0, 0, 0, 0];
    let compatible = prefix == [0; 6] && !address.is_loopback() && !address.is_unspecified();
    if nat64 || compatible {
        return ipv4_reach(Ipv4Addr::from_bits(address.to_bits() as u32));
    }
    if address.is_unicast_link_local() || address == aws_metadata {
        Reach::Blocked
    } else if address.is_loopback() || address.is_unspecified() || address.is_unique_local() {
        Reach::Private
    } else {
        Reach::Public
    }
}

fn host_reach(host: &Host<&str>) -> Reach {
    match host {
        Host::Domain(domain) => {
            let name = domain.trim_end_matches('.');
            if matches!(name, "metadata" | "metadata.google.internal") {
                Reach::Blocked
            } else if name == "localhost" || name.ends_with(".localhost") {
                Reach::Private
            } else {
                Reach::Public
            }
        }
        Host::Ipv4(address) => ipv4_reach(*address),
        Host::Ipv6(address) => ip_reach(IpAddr::V6(*address)),
    }
}

/// This machine and its network are allowed only with `ANYFETCH_ALLOW_PRIVATE=1`. With `local`,
/// they are allowed anyway.
fn allowed(reach: Reach, local: bool) -> bool {
    match reach {
        Reach::Public => true,
        Reach::Private => {
            local || std::env::var_os("ANYFETCH_ALLOW_PRIVATE").is_some_and(|value| value == "1")
        }
        Reach::Blocked => false,
    }
}

fn blocked(target: &str) -> ToolError {
    ToolError::new(
        "blocked_address",
        format!(
            "{target} is a local, link-local or cloud metadata address. ANYFETCH_ALLOW_PRIVATE=1 allows local and private networks"
        ),
    )
}

/// Refuses a URL whose host requests may not go to.
fn guard(url: &str, local: bool) -> Result<(), ToolError> {
    // An address that does not parse fails in the request itself.
    let Ok(parsed) = Url::parse(url) else {
        return Ok(());
    };
    match parsed.host() {
        Some(host) if !allowed(host_reach(&host), local) => Err(blocked(url)),
        _ => Ok(()),
    }
}

/// Resolves names with the system and refuses the addresses that `guard` refuses, so a public
/// name that points at this machine or its network is caught too.
struct Resolver {
    local: bool,
}

impl Resolve for Resolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(lookup(name, self.local))
    }
}

async fn lookup(name: Name, local: bool) -> Result<Addrs, Box<dyn Error + Send + Sync>> {
    let addresses: Vec<_> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
    if addresses
        .iter()
        .any(|address| !allowed(ip_reach(address.ip()), local))
    {
        return Err(blocked(name.as_str()).into());
    }
    Ok(Box::new(addresses.into_iter()))
}

/// `public_site`, and every address of the name is public, even with `ANYFETCH_ALLOW_PRIVATE=1`.
/// For programs that run page scripts and make their own requests, so skip `Resolver`.
pub(crate) async fn resolves_public(url: &str) -> bool {
    if !public_site(url) {
        return false;
    }
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    // `public_site` has checked an address host already.
    let Some(Host::Domain(domain)) = parsed.host() else {
        return true;
    };
    match tokio::net::lookup_host((domain, 0)).await {
        Ok(mut addresses) => addresses.all(|address| ip_reach(address.ip()) == Reach::Public),
        Err(_) => false,
    }
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
        Err(error) if both && refused(&error) => request(true).await.map_err(|direct| ToolError {
            message: format!("{} Without the proxy: {}", error.message, direct.message),
            ..error
        }),
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

/// Like `get`, for a server of the user's, such as their SearXNG, that may run on this machine
/// or its network. Link-local and metadata addresses stay refused.
pub(crate) async fn get_local(url: &str, accept: fn(&str) -> bool) -> Result<Body, ToolError> {
    routed(|direct| async move {
        let (client, jar) = client(IDENTITIES[0], None, direct, true)?;
        open(&client, &jar, url, accept, None, true).await
    })
    .await
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
        let (client, jar) = client(IDENTITIES[0], None, direct, false)?;
        if let Some(entry) = entry {
            open(&client, &jar, entry, |_| true, None, false).await?;
        }
        guard(url, false)?;
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
            let Some(browser) = browser.filter(|_| direct) else {
                return Err(error);
            };
            if !resolves_public(url).await {
                return Err(error);
            }
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
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host() else {
        return false;
    };
    let local_name = match &host {
        Host::Domain(domain) => {
            let name = domain.trim_end_matches('.');
            // A name without a dot is a machine on the local network.
            !name.contains('.') || name.ends_with(".local") || name.ends_with(".internal")
        }
        _ => false,
    };
    !local_name && host_reach(&host) == Reach::Public
}

async fn attempt(
    url: &str,
    accept: fn(&str) -> bool,
    identity: (Profile, Platform),
    cleared: Option<&Clearance>,
    direct: bool,
) -> Result<Body, ToolError> {
    let cleared_for = cleared.map(|clearance| (clearance, url));
    let (client, jar) = client(identity, cleared_for, direct, false)?;
    let agent = cleared.map(|clearance| clearance.user_agent.as_str());
    open(&client, &jar, url, accept, agent, false).await
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
    local: bool,
) -> Result<Body, ToolError> {
    let get = |target: &str| match agent {
        Some(agent) => client.get(target).header("user-agent", agent),
        None => client.get(target),
    };
    guard(url, local)?;
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
    guard(&next, local)?;
    let response = get(&next)
        .header("referer", &body.url)
        .send()
        .await
        .map_err(|error| match ToolError::from(error) {
            blocked @ ToolError {
                code: "blocked_address",
                ..
            } => blocked,
            other => ToolError::new(
                "network_error",
                format!("The request after the bot check failed: {}", other.message),
            ),
        })?;
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
    let too_large = || ToolError {
        status: Some(status),
        ..ToolError::new(
            "too_large",
            format!("The answer is larger than {} MB", MAX_BODY / 1024 / 1024),
        )
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    let mut stream = std::pin::pin!(response.bytes_stream());
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > MAX_BODY {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = Body {
        url,
        status,
        content_type,
        bytes,
    };
    if is_html {
        let page = body.text();
        if page.contains("anubis_challenge")
            && let Some(challenge) = anubis::parse(&page).map_err(|mut error| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_hosts_by_where_they_lead() {
        for (url, reach) in [
            ("http://example.com/", Reach::Public),
            ("http://8.8.8.8/", Reach::Public),
            ("http://100.128.0.1/", Reach::Public),
            ("http://[2606:4700::1111]/", Reach::Public),
            ("http://localhost/", Reach::Private),
            ("http://localhost./", Reach::Private),
            ("http://app.localhost/", Reach::Private),
            ("http://127.0.0.1/", Reach::Private),
            ("http://10.1.2.3/", Reach::Private),
            ("http://172.16.0.1/", Reach::Private),
            ("http://192.168.1.1/", Reach::Private),
            ("http://100.64.0.1/", Reach::Private),
            ("http://0.0.0.0/", Reach::Private),
            ("http://[::]/", Reach::Private),
            ("http://[::1]/", Reach::Private),
            ("http://[fd12::1]/", Reach::Private),
            ("http://[::ffff:127.0.0.1]/", Reach::Private),
            ("http://169.254.169.254/", Reach::Blocked),
            ("http://[::ffff:169.254.169.254]/", Reach::Blocked),
            ("http://[fe80::1]/", Reach::Blocked),
            ("http://[fd00:ec2::254]/", Reach::Blocked),
            ("http://100.100.100.200/", Reach::Blocked),
            ("http://metadata.google.internal/", Reach::Blocked),
            ("http://[64:ff9b::808:808]/", Reach::Public),
            ("http://[64:ff9b::7f00:1]/", Reach::Private),
            ("http://[64:ff9b::a9fe:a9fe]/", Reach::Blocked),
            ("http://[::a00:1]/", Reach::Private),
            ("http://[::a9fe:a9fe]/", Reach::Blocked),
        ] {
            let parsed = Url::parse(url).unwrap();
            assert_eq!(host_reach(&parsed.host().unwrap()), reach, "{url}");
        }
    }

    #[test]
    fn local_allows_private_but_never_blocked() {
        assert!(allowed(Reach::Private, true));
        assert!(!allowed(Reach::Blocked, true));
    }

    struct Refuses;

    impl Resolve for Refuses {
        fn resolve(&self, name: Name) -> Resolving {
            Box::pin(async move { Err(blocked(name.as_str()).into()) })
        }
    }

    #[tokio::test]
    async fn request_errors_keep_the_code_of_a_tool_error_inside() {
        let client = Client::builder().dns_resolver(Refuses).build().unwrap();
        let error = client.get("http://example.com/").send().await.unwrap_err();
        let error = ToolError::from(error);
        assert_eq!(error.code, "blocked_address");
        assert!(
            error.message.starts_with("example.com is"),
            "{}",
            error.message
        );
        assert!(!refused(&error));
    }

    #[test]
    fn public_site_excludes_local_names_and_addresses() {
        for url in [
            "http://intranet/",
            "http://printer.local/",
            "http://db.internal/",
            "http://localhost./",
            "http://app.localhost/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://[fe80::1]/",
            "http://[::ffff:10.0.0.1]/",
        ] {
            assert!(!public_site(url), "{url}");
        }
        assert!(public_site("https://example.com/"));
    }
}
