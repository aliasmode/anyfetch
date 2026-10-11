#![cfg(unix)]
use anyfetch::clearance::obtain;
use std::os::unix::fs::PermissionsExt;

/// Plays the browser: asks the start page as "FakeFox", reads the redirect target, and stores a
/// clearance cookie for that site in the profile, as Firefox does.
const FAKE_BROWSER: &str = r#"#!/usr/bin/env python3
import sqlite3, sys, time, urllib.error, urllib.request

args = sys.argv[1:]
assert args[:2] == ["--headless", "--no-remote"], args
profile = args[args.index("--profile") + 1]

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

try:
    request = urllib.request.Request(args[-1], headers={"User-Agent": "FakeFox/1.0"})
    urllib.request.build_opener(NoRedirect).open(request)
except urllib.error.HTTPError as redirect:
    host = redirect.headers["Location"].split("/")[2]
database = sqlite3.connect(profile + "/cookies.sqlite")
database.execute("CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT, expiry INTEGER)")
database.execute("INSERT INTO moz_cookies VALUES ('cf_clearance', 'pass', ?, 1900000000)", ("." + host,))
database.execute("INSERT INTO moz_cookies VALUES ('other', 'x', '.elsewhere.test', 1900000000)")
database.commit()
database.close()
time.sleep(60)
"#;

#[tokio::test]
async fn obtains_clearance_from_a_browser_started_without_automation() {
    let directory =
        std::env::temp_dir().join(format!("anyfetch-fake-browser-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let browser = directory.join("browser");
    std::fs::write(&browser, FAKE_BROWSER).unwrap();
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
    let clearance = obtain(&browser, "https://example.org/page").await;
    std::fs::remove_dir_all(&directory).unwrap();
    let clearance = clearance.unwrap();
    assert_eq!(clearance.user_agent, "FakeFox/1.0");
    assert_eq!(clearance.expires, 1_900_000_000);
    assert_eq!(clearance.cookies.len(), 1);
    assert_eq!(clearance.cookies[0].name, "cf_clearance");
    assert_eq!(clearance.cookies[0].value, "pass");
    assert_eq!(clearance.cookies[0].domain, ".example.org");
}
