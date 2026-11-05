//! jq programs run in a child process, so one that exhausts memory or the stack fails alone.

use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
    time::timeout,
};

/// Serves `body` as JSON to every request until the test ends.
async fn serve(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/data.json", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let Ok(byte) = socket.read_u8().await else {
                        return;
                    };
                    request.push(byte);
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(format!("{head}{body}").as_bytes()).await;
            });
        }
    });
    url
}

/// Fetches `body` through the CLI with the jq `program` and returns the printed JSON.
async fn fetch(body: &str, program: &str) -> Value {
    let url = serve(body.to_owned()).await;
    let home = std::env::temp_dir().join(format!(
        "anyfetch-jq-{}-{}",
        std::process::id(),
        url.rsplit(':').next().unwrap().replace('/', "")
    ));
    std::fs::create_dir_all(&home).unwrap();
    let output = timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_anyfetch"))
            .args(["fetch", &url, "--fresh", "--selector", program])
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("ANYFETCH_BROWSER", "off")
            .env("ANYFETCH_RENDERER", "off")
            .env("ANYFETCH_ALLOW_PRIVATE", "1")
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
}

#[tokio::test]
async fn jq_runs_a_normal_program() {
    let value = fetch(r#"{"items":[{"name":"a"},{"name":"b"}]}"#, ".items[].name").await;
    let content: Value = serde_json::from_str(value["content"].as_str().unwrap()).unwrap();
    assert_eq!(content, serde_json::json!(["a", "b"]));
}

#[tokio::test]
async fn jq_that_exhausts_memory_fails_without_ending_the_parent() {
    let value = fetch("{}", r#""a" * 1000000000000"#).await;
    assert_eq!(value["error"]["code"], "invalid_request", "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("more than 1 GB"),
        "{value}"
    );
}

#[tokio::test]
async fn jq_that_recurses_forever_fails() {
    let value = fetch("{}", "def f: [f]; f").await;
    assert_eq!(value["error"]["code"], "invalid_request", "{value}");
}

#[tokio::test]
async fn jq_rejects_deeply_nested_input() {
    let body = format!("{}{}", "[".repeat(300), "]".repeat(300));
    let value = fetch(&body, ".").await;
    assert_eq!(value["error"]["code"], "invalid_request", "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("deeper than 256"),
        "{value}"
    );
}
