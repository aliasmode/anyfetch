//! Cloudflare clearance from the user's own Firefox.
//!
//! Some sites answer every first request with a JavaScript challenge. A real browser passes it in
//! a few seconds and receives a `cf_clearance` cookie. Plain requests that carry that cookie and
//! the browser's User-Agent are then served, so the browser runs once per site, not once per page.

use crate::ToolError;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{sleep, timeout},
};
use url::Url;

/// How long the browser gets to pass the challenge. Measured passes took 3 to 4 seconds; pages
/// that had not passed by then never did.
const WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize, Serialize)]
pub struct Clearance {
    pub user_agent: String,
    pub cookies: Vec<Cookie>,
    /// Unix seconds at which `cf_clearance` expires.
    pub expires: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
}

/// The Firefox-based browser to use, or `None` when clearance is off. `ANYFETCH_BROWSER=off`
/// turns it off; `ANYFETCH_BROWSER=<path>` selects a browser; otherwise Firefox is looked up.
pub fn browser() -> Option<PathBuf> {
    match std::env::var_os("ANYFETCH_BROWSER") {
        Some(value) if value == "off" => None,
        Some(value) if !value.is_empty() => Some(value.into()),
        _ => installed(),
    }
}

// The Windows launcher detaches from the browser it starts, which this code has not been tested
// to stop again. Windows users opt in with ANYFETCH_BROWSER.
#[cfg(windows)]
fn installed() -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
fn installed() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .flat_map(|dir| ["firefox", "firefox-esr"].map(|name| dir.join(name)))
        .chain(["/Applications/Firefox.app/Contents/MacOS/firefox".into()])
        .find(|candidate| candidate.is_file() && !snap(candidate))
}

/// A snap-packaged Firefox has its own temporary folder, so it cannot use the throwaway profile.
/// Ubuntu's `/usr/bin/firefox` only starts the snap.
#[cfg(not(windows))]
fn snap(candidate: &Path) -> bool {
    Path::new("/snap/bin/firefox").exists()
        && (candidate.starts_with("/snap") || candidate == Path::new("/usr/bin/firefox"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn store(host: &str) -> Option<PathBuf> {
    let directory = std::env::home_dir()?.join(".config/anyfetch/clearance");
    Some(directory.join(format!("{host}.json")))
}

/// The saved clearance for `host`, if it has not expired.
pub(crate) fn load(host: &str) -> Option<Clearance> {
    let clearance: Clearance = serde_json::from_slice(&std::fs::read(store(host)?).ok()?).ok()?;
    (clearance.expires > now()).then_some(clearance)
}

pub(crate) fn forget(host: &str) {
    if let Some(path) = store(host) {
        let _ = std::fs::remove_file(path);
    }
}

/// Returns the saved clearance for the host of `url`, or gets one from `browser` and saves it.
/// One browser runs at a time, so parallel fetches of one site share a single pass.
pub(crate) async fn ensure(browser: &Path, url: &str) -> Result<Clearance, ToolError> {
    static ONE_BROWSER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _running = ONE_BROWSER.lock().await;
    let host = Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    let host = host.ok_or_else(|| failed("The URL has no host"))?;
    if let Some(clearance) = load(&host) {
        return Ok(clearance);
    }
    let clearance = obtain(browser, url).await?;
    if let Some(path) = store(&host)
        && let Some(directory) = path.parent()
        && std::fs::create_dir_all(directory).is_ok()
    {
        let _ = std::fs::write(path, serde_json::to_vec(&clearance).unwrap_or_default());
    }
    Ok(clearance)
}

fn failed(message: &str) -> ToolError {
    ToolError::new("clearance_failed", message)
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Opens `url` in `browser`, headless and with a throwaway profile, and waits for the Cloudflare
/// clearance cookie. No automation protocol is used: the browser runs as it does for a person.
pub async fn obtain(browser: &Path, url: &str) -> Result<Clearance, ToolError> {
    let target = Url::parse(url).map_err(|_| failed("The URL is not valid"))?;
    let host = target
        .host_str()
        .ok_or_else(|| failed("The URL has no host"))?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch = Scratch(
        std::env::temp_dir().join(format!("anyfetch-browser-{}-{unique}", std::process::id())),
    );
    let profile = scratch.0.join("profile");
    std::fs::create_dir_all(&profile).map_err(|_| failed("Could not create a browser profile"))?;
    // The browser first asks this listener, which records its User-Agent and redirects it on.
    let listener = TcpListener::bind("127.0.0.1:0").await;
    let listener = listener.map_err(|_| failed("Could not open a local port"))?;
    let port = listener.local_addr().map_or(0, |address| address.port());
    let mut child = tokio::process::Command::new(browser)
        .args(["--headless", "--no-remote", "--profile"])
        .arg(&profile)
        .arg(format!("http://127.0.0.1:{port}/"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| failed("Could not start the browser"))?;
    let pass = async {
        let user_agent = redirect(&listener, url).await?;
        loop {
            sleep(Duration::from_secs(1)).await;
            let (cookies, expires) = read_cookies(&scratch.0, host);
            if expires > 0 {
                return Ok(Clearance {
                    user_agent,
                    cookies,
                    expires,
                });
            }
        }
    };
    let result = timeout(WAIT, pass).await;
    let _ = child.kill().await;
    result.unwrap_or_else(|_| Err(failed("The browser did not pass the challenge")))
}

/// Answers the browser's first request with a redirect to `url` and returns its User-Agent.
async fn redirect(listener: &TcpListener, url: &str) -> Result<String, ToolError> {
    loop {
        let accepted = listener.accept().await;
        let (mut stream, _) = accepted.map_err(|_| failed("The browser did not connect"))?;
        let mut request = Vec::new();
        let read = async {
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                request.push(stream.read_u8().await?);
            }
            std::io::Result::Ok(())
        };
        // A browser may open a spare connection and send nothing on it.
        if !matches!(timeout(Duration::from_secs(2), read).await, Ok(Ok(()))) {
            continue;
        }
        let request = String::from_utf8_lossy(&request);
        let user_agent = request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("user-agent")
                .then(|| value.trim().to_owned())
        });
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes()).await;
        if let Some(user_agent) = user_agent {
            return Ok(user_agent);
        }
    }
}

/// Cookies in the Firefox profile under `scratch` that belong to `host`, and the expiry of
/// `cf_clearance` in Unix seconds (0 when the browser does not have that cookie yet).
fn read_cookies(scratch: &Path, host: &str) -> (Vec<Cookie>, u64) {
    // Firefox keeps recent writes in the `-wal` file, so read a copy of both files.
    let copy = scratch.join("cookies");
    let _ = std::fs::create_dir_all(&copy);
    for name in ["cookies.sqlite", "cookies.sqlite-wal"] {
        let _ = std::fs::remove_file(copy.join(name));
        let _ = std::fs::copy(scratch.join("profile").join(name), copy.join(name));
    }
    let rows = || -> rusqlite::Result<Vec<(String, String, String, i64)>> {
        let database = rusqlite::Connection::open(copy.join("cookies.sqlite"))?;
        let mut query = database.prepare("SELECT name, value, host, expiry FROM moz_cookies")?;
        let rows = query.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    };
    let mut expires = 0;
    let mut cookies = Vec::new();
    for (name, value, domain, expiry) in rows().unwrap_or_default() {
        let bare = domain.trim_start_matches('.');
        if host != bare && !host.ends_with(&format!(".{bare}")) {
            continue;
        }
        if name == "cf_clearance" {
            // Newer Firefox versions store milliseconds.
            let seconds = if expiry > 100_000_000_000 {
                expiry / 1000
            } else {
                expiry
            };
            expires = seconds.max(0) as u64;
        }
        cookies.push(Cookie {
            name,
            value,
            domain,
        });
    }
    (cookies, expires)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_cookies_of_one_site_in_both_expiry_units() {
        for (expiry, expected) in [
            (1_900_000_000_i64, 1_900_000_000),
            (1_900_000_000_123, 1_900_000_000),
        ] {
            let scratch = Scratch(std::env::temp_dir().join(format!(
                "anyfetch-cookie-test-{}-{expiry}",
                std::process::id()
            )));
            let profile = scratch.0.join("profile");
            std::fs::create_dir_all(&profile).unwrap();
            let database = rusqlite::Connection::open(profile.join("cookies.sqlite")).unwrap();
            database
                .execute_batch(&format!(
                    "CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT, expiry INTEGER);
                     INSERT INTO moz_cookies VALUES ('cf_clearance', 'pass', '.example.org', {expiry});
                     INSERT INTO moz_cookies VALUES ('session', 's1', 'www.example.org', 0);
                     INSERT INTO moz_cookies VALUES ('cf_clearance', 'other', '.example.com', {expiry});"
                ))
                .unwrap();
            drop(database);
            let (cookies, expires) = read_cookies(&scratch.0, "www.example.org");
            assert_eq!(expires, expected);
            let names: Vec<_> = cookies
                .iter()
                .map(|cookie| (&*cookie.name, &*cookie.value))
                .collect();
            assert_eq!(names, [("cf_clearance", "pass"), ("session", "s1")]);
            assert_eq!(read_cookies(&scratch.0, "example.net").1, 0);
        }
    }
}
