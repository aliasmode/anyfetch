use crate::{
    ToolError, anubis, challenge,
    clearance::{self, Clearance},
};
use encoding_rs::{Encoding, UTF_8};
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    error::Error,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex as StdMutex},
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
) -> Result<(Client, Arc<Jar>, Arc<Scope>), ToolError> {
    let scope = Arc::new(Scope::new(Rules::from_env(local)));
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
        .redirect(redirect::Policy::custom({
            let scope = Arc::clone(&scope);
            move |attempt| {
                // The last address requested is the page that sent the redirect.
                let from = attempt.previous.last().map(ToString::to_string);
                match scope.hop(
                    from.as_deref().unwrap_or_default(),
                    &attempt.uri.to_string(),
                ) {
                    Ok(()) => redirect::Policy::default().redirect(attempt),
                    Err(error) => attempt.error(error),
                }
            }
        }))
        .timeout(TIMEOUT)
        .no_proxy();
    // Through a proxy the proxy resolves names, so only the address checks in `Scope` apply.
    if !direct && let (Some(proxy), _) = proxy()? {
        builder = builder.proxy(proxy);
    } else {
        builder = builder.dns_resolver(Resolver(Arc::clone(&scope)));
    }
    Ok((builder.build()?, jar, scope))
}

/// Where an address leads, ordered from least to most restricted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    Public,
    /// This machine: loopback, `localhost` names, and the unspecified address.
    Loopback,
    /// The local or a private network.
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
    } else if address.is_loopback() || address.is_unspecified() {
        Reach::Loopback
    } else if address.is_private()
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
        // A translator elsewhere reaches the IPv4 address, so its loopback is not this machine.
        return match ipv4_reach(Ipv4Addr::from_bits(address.to_bits() as u32)) {
            Reach::Loopback => Reach::Private,
            reach => reach,
        };
    }
    if address.is_unicast_link_local() || address == aws_metadata {
        Reach::Blocked
    } else if address.is_loopback() || address.is_unspecified() {
        Reach::Loopback
    } else if address.is_unique_local() {
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
                Reach::Loopback
            } else {
                Reach::Public
            }
        }
        Host::Ipv4(address) => ipv4_reach(*address),
        Host::Ipv6(address) => ip_reach(IpAddr::V6(*address)),
    }
}

pub(crate) const ALLOW_LOCALHOST: &str = "ANYFETCH_ALLOW_LOCALHOST";
pub(crate) const ALLOW_PRIVATE: &str = "ANYFETCH_ALLOW_PRIVATE";
pub(crate) const PUBLIC_TO_PRIVATE: &str = "ANYFETCH_DANGEROUSLY_ALLOW_PUBLIC_TO_PRIVATE_REDIRECTS";

fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| value == "1")
}

/// Which addresses off the public web may be reached. Link-local and metadata addresses never are.
/// The agent's own URLs, and pages off the public web, may lead to what the user allowed; a public
/// site may lead off it only with `public_to_private`.
#[derive(Clone, Copy, Debug, Default)]
struct Rules {
    /// This machine: `ANYFETCH_ALLOW_LOCALHOST=1`.
    localhost: bool,
    /// This machine and private networks: `ANYFETCH_ALLOW_PRIVATE=1`, or a server of the user's
    /// such as their SearXNG.
    private: bool,
    /// A public site may send anyfetch to both. Includes `private`.
    public_to_private: bool,
}

/// Who sends a request: the agent, or a page at a public address or one off the public web.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Origin {
    Agent,
    Public,
    Private,
}

impl Rules {
    fn from_env(local: bool) -> Self {
        let public_to_private = flag(PUBLIC_TO_PRIVATE);
        Self {
            localhost: flag(ALLOW_LOCALHOST),
            private: local || public_to_private || flag(ALLOW_PRIVATE),
            public_to_private,
        }
    }

    /// Refuses a request from `origin` to `target`, which leads to `reach`.
    fn admit(self, origin: Origin, reach: Reach, target: &str) -> Result<(), ToolError> {
        match reach {
            Reach::Public => Ok(()),
            Reach::Blocked => Err(blocked(target, reach)),
            _ if self.public_to_private => Ok(()),
            _ if origin == Origin::Public => Err(redirected(target)),
            Reach::Loopback if self.localhost || self.private => Ok(()),
            Reach::Private if self.private => Ok(()),
            _ => Err(blocked(target, reach)),
        }
    }
}

fn blocked(target: &str, reach: Reach) -> ToolError {
    let message = match reach {
        Reach::Loopback => format!(
            "{target} is on this machine. {ALLOW_LOCALHOST}=1 allows this machine, \
             {ALLOW_PRIVATE}=1 also private networks"
        ),
        Reach::Private => format!(
            "{target} is on a local or private network. {ALLOW_PRIVATE}=1 allows local and private networks"
        ),
        _ => format!("{target} is a link-local or cloud metadata address, which is never fetched"),
    };
    ToolError::new("blocked_address", message)
}

fn redirected(target: &str) -> ToolError {
    ToolError::new(
        "blocked_address",
        format!(
            "{target} is on this machine or a private network, and a public site sent anyfetch there. \
             {ALLOW_LOCALHOST}=1 and {ALLOW_PRIVATE}=1 do not allow that. {PUBLIC_TO_PRIVATE}=1 does, \
             and lets any website make anyfetch read your router and local services"
        ),
    )
}

/// The rules of one client, and where its current request comes from.
struct Scope {
    rules: Rules,
    origin: StdMutex<Origin>,
    /// Names that resolved to an address on this machine or a private network, and the most
    /// restricted reach of their addresses. A pooled connection skips `Resolver`, so these count.
    private_names: StdMutex<HashMap<String, Reach>>,
}

impl Scope {
    fn new(rules: Rules) -> Self {
        Self {
            rules,
            origin: StdMutex::new(Origin::Agent),
            private_names: StdMutex::default(),
        }
    }

    /// Where `host` leads: by itself, or by the addresses its name resolved to for this client.
    fn reach(&self, host: &Host<&str>) -> Reach {
        let own = host_reach(host);
        let Host::Domain(domain) = host else {
            return own;
        };
        let names = self.private_names.lock().expect("names lock");
        names
            .get(&name_key(domain))
            .map_or(own, |&resolved| own.max(resolved))
    }

    fn origin(&self) -> Origin {
        *self.origin.lock().expect("origin lock")
    }

    /// A request that anyfetch makes for the agent: refuses `url` by its host.
    fn begin(&self, url: &str) -> Result<(), ToolError> {
        *self.origin.lock().expect("origin lock") = Origin::Agent;
        self.check(url)
    }

    /// The page at `from` sends the next request to `to`, by a redirect or a bot check.
    fn hop(&self, from: &str, to: &str) -> Result<(), ToolError> {
        let origin = if self.leads_private(from) {
            Origin::Private
        } else {
            Origin::Public
        };
        *self.origin.lock().expect("origin lock") = origin;
        self.check(to)
    }

    fn check(&self, url: &str) -> Result<(), ToolError> {
        // An address that does not parse fails in the request itself.
        let Ok(parsed) = Url::parse(url) else {
            return Ok(());
        };
        match parsed.host() {
            Some(host) => self.rules.admit(self.origin(), self.reach(&host), url),
            None => Ok(()),
        }
    }

    /// Whether the page at `url` is on this machine or a private network.
    fn leads_private(&self, url: &str) -> bool {
        let Ok(parsed) = Url::parse(url) else {
            return false;
        };
        parsed.host().is_some_and(|host| off_web(self.reach(&host)))
    }

    /// Refuses the addresses of `name` that the current request may not go to, and remembers a
    /// name that leads to this machine or a private network.
    fn resolved(&self, name: &str, addresses: &[SocketAddr]) -> Result<(), ToolError> {
        let origin = self.origin();
        let mut worst = Reach::Public;
        for address in addresses {
            let reach = ip_reach(address.ip());
            worst = worst.max(reach);
            self.rules.admit(origin, reach, name)?;
        }
        if off_web(worst) {
            let mut names = self.private_names.lock().expect("names lock");
            names.insert(name_key(name), worst);
        }
        Ok(())
    }
}

/// This machine or a private network.
fn off_web(reach: Reach) -> bool {
    matches!(reach, Reach::Loopback | Reach::Private)
}

fn name_key(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Resolves names with the system and refuses the addresses that `Scope` refuses, so a public
/// name that points at this machine or its network is caught too.
struct Resolver(Arc<Scope>);

impl Resolve for Resolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(lookup(name, Arc::clone(&self.0)))
    }
}

async fn lookup(name: Name, scope: Arc<Scope>) -> Result<Addrs, Box<dyn Error + Send + Sync>> {
    let addresses: Vec<_> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
    scope.resolved(name.as_str(), &addresses)?;
    Ok(Box::new(addresses.into_iter()))
}

/// Where `url` leads: by its host, then by every address of its name. A name that does not
/// resolve, or that names a machine on the local network, leads there. `None` without a host.
async fn url_reach(url: &Url) -> Option<Reach> {
    let host = url.host()?;
    let reach = host_reach(&host);
    let Host::Domain(domain) = host else {
        return Some(reach);
    };
    if reach != Reach::Public {
        return Some(reach);
    }
    if local_name(domain) {
        return Some(Reach::Private);
    }
    let addresses: Vec<_> = match tokio::net::lookup_host((domain, 0)).await {
        Ok(addresses) => addresses.collect(),
        Err(_) => Vec::new(),
    };
    if addresses.is_empty() {
        return Some(Reach::Private);
    }
    addresses.iter().map(|address| ip_reach(address.ip())).max()
}

/// `public_site`, and every address of the name is public, even with `ANYFETCH_ALLOW_PRIVATE=1`.
/// For the Cloudflare browser, which makes its own requests, so skips `Resolver`.
pub(crate) async fn resolves_public(url: &str) -> bool {
    match Url::parse(url) {
        Ok(parsed) => url_reach(&parsed).await == Some(Reach::Public),
        Err(_) => false,
    }
}

/// A page at `start` counts as public unless it leads to this machine or a private network.
async fn origin_of(start: &str) -> Origin {
    let reach = match Url::parse(start) {
        Ok(parsed) => url_reach(&parsed).await,
        Err(_) => None,
    };
    if reach.is_some_and(off_web) {
        Origin::Private
    } else {
        Origin::Public
    }
}

/// Whether the renderer or the browser, which make their own requests and skip `Resolver`, may
/// open `url` for a fetch that the agent started at `start`. The rules of redirects apply: a
/// page off the public web needs the setting that allows it, and a `start` off the public web.
pub(crate) async fn may_open(start: &str, url: &str) -> bool {
    let web = Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"));
    web && judge(Rules::from_env(false), start, url).await.is_ok()
}

/// Refuses the page that the browser ended on, for a fetch that the agent started at `start`,
/// when the rules of redirects would not let anyfetch go there. A page that is not on the
/// network, such as `about:blank`, passes.
pub(crate) async fn check_landing(start: &str, landed: &str) -> Result<(), ToolError> {
    judge(Rules::from_env(false), start, landed).await
}

async fn judge(rules: Rules, start: &str, url: &str) -> Result<(), ToolError> {
    let Ok(mut parsed) = Url::parse(url) else {
        return Ok(());
    };
    // `blob:https://host/id` belongs to the page at its inner address.
    if parsed.scheme() == "blob"
        && let Ok(inner) = Url::parse(parsed.path())
    {
        parsed = inner;
    }
    if !matches!(parsed.scheme(), "http" | "https" | "ws" | "wss") {
        return Ok(());
    }
    let reach = url_reach(&parsed).await.unwrap_or(Reach::Private);
    rules.admit(origin_of(start).await, reach, url)
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
        let (client, jar, scope) = client(IDENTITIES[0], None, direct, true)?;
        open(&client, &jar, &scope, url, accept, None).await
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
        let (client, jar, scope) = client(IDENTITIES[0], None, direct, false)?;
        if let Some(entry) = entry {
            open(&client, &jar, &scope, entry, |_| true, None).await?;
        }
        scope.begin(url)?;
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
    let named_local = matches!(&host, Host::Domain(domain) if local_name(domain));
    !named_local && host_reach(&host) == Reach::Public
}

/// A name of a machine on the local network: one without a dot, or under `.local` or `.internal`.
fn local_name(domain: &str) -> bool {
    let name = domain.trim_end_matches('.');
    !name.contains('.') || name.ends_with(".local") || name.ends_with(".internal")
}

async fn attempt(
    url: &str,
    accept: fn(&str) -> bool,
    identity: (Profile, Platform),
    cleared: Option<&Clearance>,
    direct: bool,
) -> Result<Body, ToolError> {
    let cleared_for = cleared.map(|clearance| (clearance, url));
    let (client, jar, scope) = client(identity, cleared_for, direct, false)?;
    let agent = cleared.map(|clearance| clearance.user_agent.as_str());
    open(&client, &jar, &scope, url, accept, agent).await
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
    scope: &Scope,
    url: &str,
    accept: fn(&str) -> bool,
    agent: Option<&str>,
) -> Result<Body, ToolError> {
    let get = |target: &str| match agent {
        Some(agent) => client.get(target).header("user-agent", agent),
        None => client.get(target),
    };
    scope.begin(url)?;
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
    // The page asks for the next request, as a redirect would.
    scope.hop(&body.url, &next)?;
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
        || headers.contains_key("x-amzn-waf-action")
        // LinkedIn sends its sign-in wall with this status.
        || status == 999;
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
#[path = "transport.test.rs"]
mod tests;
