use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
    time::timeout,
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fetchroam"));
    command
        .arg("search")
        .kill_on_drop(true)
        .stderr(Stdio::piped());
    command
}

#[tokio::test]
async fn cli_exposes_default_and_rejects_invalid_provider_arguments() {
    let output = command().arg("--help").output().await.unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("[default: duckduckgo]"));
    for args in [
        vec!["query", "--provider", "brave"],
        vec!["query", "--provider", "searxng"],
    ] {
        let output = command().args(args).output().await.unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[tokio::test]
async fn cli_uses_the_selected_searxng_instance() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/nested", listener.local_addr().unwrap());
    let fixture = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let body = r#"{"results":[]}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let output = timeout(
        Duration::from_secs(10),
        command()
            .args(["query", "--provider", "searxng", "--searxng-url", &base])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["provider"], "searxng");
    assert_eq!(value["results"], serde_json::json!([]));
    assert!(
        fixture
            .await
            .unwrap()
            .starts_with("GET /nested/search?q=query&format=json ")
    );
}

#[tokio::test]
async fn cli_sends_requests_through_the_proxy_file() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let home = std::env::temp_dir().join(format!("fetchroam-proxy-{}", std::process::id()));
    let config = home.join(".config/fetchroam");
    std::fs::create_dir_all(&config).unwrap();
    let proxy = format!("{}:user:pa:ss\n", listener.local_addr().unwrap());
    std::fs::write(config.join("proxy"), proxy).unwrap();
    let fixture = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let body = r#"{"results":[]}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let output = timeout(
        Duration::from_secs(10),
        command()
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .args(["query", "--provider", "searxng"])
            .args(["--searxng-url", "http://searx.test"])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    assert!(output.status.success());
    let request = fixture.await.unwrap();
    assert!(request.starts_with("GET http://searx.test/search?q=query&format=json "));
    assert!(request.contains("Basic dXNlcjpwYTpzcw=="));
}
