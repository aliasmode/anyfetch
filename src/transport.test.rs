use super::*;

#[test]
fn classifies_hosts_by_where_they_lead() {
    for (url, reach) in [
        ("http://example.com/", Reach::Public),
        ("http://8.8.8.8/", Reach::Public),
        ("http://100.128.0.1/", Reach::Public),
        ("http://[2606:4700::1111]/", Reach::Public),
        ("http://localhost/", Reach::Loopback),
        ("http://localhost./", Reach::Loopback),
        ("http://app.localhost/", Reach::Loopback),
        ("http://127.0.0.1/", Reach::Loopback),
        ("http://127.8.0.1/", Reach::Loopback),
        ("http://10.1.2.3/", Reach::Private),
        ("http://172.16.0.1/", Reach::Private),
        ("http://192.168.1.1/", Reach::Private),
        ("http://100.64.0.1/", Reach::Private),
        ("http://0.0.0.0/", Reach::Loopback),
        ("http://[::]/", Reach::Loopback),
        ("http://[::1]/", Reach::Loopback),
        ("http://[fd12::1]/", Reach::Private),
        ("http://[::ffff:127.0.0.1]/", Reach::Loopback),
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

const LOCALHOST: Rules = Rules {
    localhost: true,
    private: false,
    public_to_private: false,
};
const PRIVATE: Rules = Rules {
    localhost: false,
    private: true,
    public_to_private: false,
};
const DANGEROUS: Rules = Rules {
    localhost: false,
    private: true,
    public_to_private: true,
};

fn admits(rules: Rules, origin: Origin, reach: Reach) -> bool {
    rules.admit(origin, reach, "http://target/").is_ok()
}

#[test]
fn the_agent_reaches_what_the_settings_allow() {
    use {Origin::Agent, Reach::*};
    let none = Rules::default();
    assert!(admits(none, Agent, Public));
    assert!(!admits(none, Agent, Loopback) && !admits(none, Agent, Private));
    // ANYFETCH_ALLOW_LOCALHOST: this machine, not the network.
    assert!(admits(LOCALHOST, Agent, Loopback));
    assert!(!admits(LOCALHOST, Agent, Private));
    assert!(admits(PRIVATE, Agent, Loopback) && admits(PRIVATE, Agent, Private));
    for rules in [none, LOCALHOST, PRIVATE, DANGEROUS] {
        assert!(!admits(rules, Agent, Blocked));
        assert!(!admits(rules, Origin::Private, Blocked));
        assert!(!admits(rules, Origin::Public, Blocked));
    }
}

#[test]
fn a_public_site_may_not_redirect_off_the_web_without_the_dangerous_setting() {
    use Reach::*;
    for rules in [Rules::default(), LOCALHOST, PRIVATE] {
        assert!(admits(rules, Origin::Public, Public));
        for reach in [Loopback, Private] {
            let error = rules
                .admit(Origin::Public, reach, "http://target/")
                .unwrap_err();
            assert_eq!(error.code, "blocked_address");
            assert!(
                error.message.contains(PUBLIC_TO_PRIVATE),
                "{}",
                error.message
            );
        }
    }
    assert!(admits(DANGEROUS, Origin::Public, Loopback));
    assert!(admits(DANGEROUS, Origin::Public, Private));
    assert!(admits(DANGEROUS, Origin::Agent, Private));
}

#[test]
fn pages_off_the_web_redirect_within_what_is_allowed() {
    use {Origin::Private as Off, Reach::*};
    assert!(admits(PRIVATE, Off, Private) && admits(PRIVATE, Off, Loopback));
    assert!(admits(PRIVATE, Off, Public));
    assert!(admits(LOCALHOST, Off, Loopback));
    let error = LOCALHOST
        .admit(Off, Private, "http://192.168.1.1/")
        .unwrap_err();
    assert!(error.message.contains(ALLOW_PRIVATE), "{}", error.message);
    let error = Rules::default().admit(Origin::Agent, Loopback, "http://127.0.0.1/");
    assert!(error.unwrap_err().message.contains(ALLOW_LOCALHOST));
}

#[test]
fn redirects_are_judged_by_the_page_that_sends_them() {
    let scope = Scope::new(PRIVATE);
    assert!(scope.begin("http://192.168.1.1/").is_ok());
    assert!(scope.hop("http://192.168.1.1/", "http://10.0.0.2/").is_ok());
    assert!(
        scope
            .hop("http://10.0.0.2/", "https://example.com/")
            .is_ok()
    );
    // The public page sends anyfetch back into the network.
    assert!(
        scope
            .hop("https://example.com/", "http://10.0.0.2/")
            .is_err()
    );
    assert!(scope.begin("http://169.254.169.254/").is_err());

    let scope = Scope::new(LOCALHOST);
    assert!(scope.begin("http://127.0.0.1:3000/").is_ok());
    assert!(
        scope
            .hop("http://127.0.0.1:3000/", "http://localhost:3000/")
            .is_ok()
    );
    assert!(
        scope
            .hop("http://localhost:3000/", "http://192.168.1.1/")
            .is_err()
    );
    assert!(scope.begin("http://192.168.1.1/").is_err());
    assert!(
        scope
            .hop("https://example.com/", "http://127.0.0.1/")
            .is_err()
    );
}

#[test]
fn a_name_that_resolved_off_the_web_counts_as_a_page_there() {
    let scope = Scope::new(PRIVATE);
    let lan: SocketAddr = "192.168.1.5:0".parse().unwrap();
    let public: SocketAddr = "93.184.216.34:0".parse().unwrap();
    scope.begin("http://router.example/").unwrap();
    assert!(scope.resolved("Router.Example.", &[lan]).is_ok());
    assert!(
        scope
            .hop("http://router.example/", "http://10.0.0.2/")
            .is_ok()
    );
    // Back from a public page over a pooled connection, which does not resolve again.
    assert!(
        scope
            .hop("https://sso.example/", "http://router.example/x")
            .is_err()
    );
    // A public page whose redirect target resolves into the network.
    scope
        .hop("https://example.com/", "https://nas.example/")
        .unwrap();
    assert!(scope.resolved("nas.example", &[lan]).is_err());
    assert!(scope.resolved("other.example", &[public]).is_ok());
    let scope = Scope::new(Rules::default());
    scope.begin("https://nas.example/").unwrap();
    assert!(scope.resolved("nas.example", &[lan]).is_err());
}

#[tokio::test]
async fn local_names_and_addresses_count_as_off_the_web() {
    for (url, reach) in [
        ("http://127.0.0.1:3000/", Reach::Loopback),
        ("http://localhost/", Reach::Loopback),
        ("http://intranet/", Reach::Private),
        ("http://printer.local/", Reach::Private),
        ("http://192.168.1.1/", Reach::Private),
        ("http://169.254.169.254/", Reach::Blocked),
        ("http://93.184.216.34/", Reach::Public),
    ] {
        let parsed = Url::parse(url).unwrap();
        assert_eq!(url_reach(&parsed).await, Some(reach), "{url}");
    }
    assert_eq!(origin_of("http://10.0.0.1/").await, Origin::Private);
    assert_eq!(origin_of("http://93.184.216.34/").await, Origin::Public);
}

#[tokio::test]
async fn the_browser_opens_and_ends_where_a_redirect_could_go() {
    let local = "http://127.0.0.1:3000/app";
    assert!(judge(LOCALHOST, local, local).await.is_ok());
    assert!(judge(Rules::default(), local, local).await.is_err());
    assert!(
        judge(LOCALHOST, local, "http://192.168.1.1/")
            .await
            .is_err()
    );
    assert!(
        judge(PRIVATE, "http://10.0.0.1/", "http://10.0.0.2/")
            .await
            .is_ok()
    );
    // A public page that navigated into the network.
    let public = "http://93.184.216.34/";
    assert!(judge(PRIVATE, public, "http://10.0.0.2/").await.is_err());
    assert!(judge(DANGEROUS, public, "http://10.0.0.2/").await.is_ok());
    assert!(judge(Rules::default(), public, "about:blank").await.is_ok());
    let blob = "blob:http://192.168.1.1/0b1c";
    assert!(judge(PRIVATE, public, blob).await.is_err());
    assert!(
        judge(DANGEROUS, public, "http://169.254.169.254/")
            .await
            .is_err()
    );
}

struct Refuses;

impl Resolve for Refuses {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move { Err(blocked(name.as_str(), Reach::Private).into()) })
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
