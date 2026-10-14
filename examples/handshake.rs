//! One GET request with a chosen handshake and a chosen header set, for the handshake test in
//! `bench/other-tools/run_handshake.py`.
//!
//! Usage: `handshake <safari|plain> <safari|curl> <url>`
//!
//! `safari` as the handshake sends the TLS and HTTP/2 fingerprint of Safari 26.4, as Anyfetch
//! does. `plain` sends the default fingerprint of the same library. The header sets are copied
//! from what Anyfetch 0.8.2 and curl 8.5.0 send, in their order. Prints one JSON line with
//! `status` and `body`, or `error`.

use std::time::Duration;
use wreq::Client;
use wreq_util::{Emulation, Platform, Profile};

const SAFARI: [(&str, &str); 8] = [
    ("sec-fetch-dest", "document"),
    (
        "user-agent",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.4 Safari/605.1.15",
    ),
    (
        "accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    ),
    ("sec-fetch-site", "none"),
    ("sec-fetch-mode", "navigate"),
    ("accept-language", "en-US,en;q=0.9"),
    ("priority", "u=0, i"),
    ("accept-encoding", "gzip, deflate, br"),
];

const CURL: [(&str, &str); 3] = [
    ("user-agent", "curl/8.5.0"),
    ("accept", "*/*"),
    ("accept-encoding", "deflate, gzip, br, zstd"),
];

#[tokio::main]
async fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, handshake, headers, url] = arguments.as_slice() else {
        eprintln!("usage: handshake <safari|plain> <safari|curl> <url>");
        std::process::exit(2);
    };
    let mut builder = Client::builder()
        .redirect(wreq::redirect::Policy::default())
        .timeout(Duration::from_secs(30))
        .no_proxy();
    if handshake == "safari" {
        let safari = Emulation::builder()
            .profile(Profile::Safari26_4)
            .platform(Platform::MacOS)
            .headers(false)
            .build();
        builder = builder.emulation(safari);
    }
    let client = builder.build().expect("client");
    let list: &[(&str, &str)] = if headers == "safari" { &SAFARI } else { &CURL };
    let mut request = client.get(url);
    for (name, value) in list {
        request = request.header(*name, *value);
    }
    let line = match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            serde_json::json!({ "status": status, "body": body })
        }
        Err(error) => serde_json::json!({ "error": error.to_string() }),
    };
    println!("{line}");
}
