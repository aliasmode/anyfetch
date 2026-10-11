use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
    time::timeout,
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anyfetch"));
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
    assert!(help.contains("[default: auto]"));
    for args in [
        vec!["query", "--provider", "bing"],
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
    let home = std::env::temp_dir().join(format!("anyfetch-proxy-{}", std::process::id()));
    let config = home.join(".config/anyfetch");
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

#[tokio::test]
async fn cli_fetch_sends_a_saved_clearance_for_the_site() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let home = std::env::temp_dir().join(format!("anyfetch-clearance-{}", std::process::id()));
    let store = home.join(".config/anyfetch/clearance");
    std::fs::create_dir_all(&store).unwrap();
    let saved = r#"{"user_agent":"FakeFox/1.0","expires":4000000000,
        "cookies":[{"name":"cf_clearance","value":"pass","domain":"127.0.0.1"}]}"#;
    std::fs::write(store.join("127.0.0.1.json"), saved).unwrap();
    let fixture = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await.unwrap();
        String::from_utf8(request).unwrap().to_ascii_lowercase()
    });
    let output = timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_anyfetch"))
            .args(["fetch", &url])
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("ANYFETCH_BROWSER", "off")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["content"], "hello");
    let request = fixture.await.unwrap();
    assert!(request.contains("user-agent: fakefox/1.0"));
    assert!(request.contains("cookie: cf_clearance=pass"));
}

/// Plays the renderer: records how it was called and prints the page as its scripts would
/// leave it.
#[cfg(unix)]
const FAKE_RENDERER: &str = r#"#!/bin/sh
echo "$@" > "$(dirname "$0")/arguments"
cat <<'HTML'
<html><head><title>App</title></head><body><article><h1>Dashboard</h1>
<p>This text exists only after the page's scripts run, so a plain request cannot read it.</p>
<p>The renderer runs the scripts and prints the finished page for the same extraction.</p>
<p>A third paragraph gives the article enough length to count as readable content here.</p>
</article></body></html>
HTML
"#;

#[cfg(unix)]
#[tokio::test]
async fn cli_fetch_renders_a_page_that_has_no_text_without_its_scripts() {
    use std::os::unix::fs::PermissionsExt;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/app", listener.local_addr().unwrap());
    let home = std::env::temp_dir().join(format!("anyfetch-render-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let renderer = home.join("renderer");
    std::fs::write(&renderer, FAKE_RENDERER).unwrap();
    std::fs::set_permissions(&renderer, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fixture = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let shell = "<html><head><title>App</title></head><body><div id=root></div><script src=/app.js></script></body></html>";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{shell}", shell.len()).as_bytes()).await.unwrap();
    });
    let output = timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_anyfetch"))
            .args(["fetch", &url])
            .env("HOME", &home)
            .env("ANYFETCH_BROWSER", "off")
            .env("ANYFETCH_RENDERER", &renderer)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    fixture.await.unwrap();
    let arguments = std::fs::read_to_string(home.join("arguments")).unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["content"]
            .as_str()
            .unwrap()
            .contains("only after the page's scripts run")
    );
    assert_eq!(value["title"], "App");
    assert_eq!(
        arguments.trim(),
        format!("fetch --dump html --wait-until networkidle --wait-ms 10000 {url}")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cli_setup_connects_the_harnesses_on_the_path() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!("anyfetch-setup-test-{}", std::process::id()));
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    // A harness that records its arguments, and one whose command fails.
    for (name, script) in [
        (
            "codex",
            "#!/bin/sh\necho \"$@\" > \"$HOME/codex-arguments\"\n",
        ),
        (
            "gemini",
            "#!/bin/sh\necho 'Error: no settings' >&2\nexit 3\n",
        ),
    ] {
        std::fs::write(bin.join(name), script).unwrap();
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_anyfetch"))
        .arg("setup")
        .env("HOME", &home)
        .env("PATH", &bin)
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let lines = String::from_utf8(output.stdout).unwrap();
    assert!(lines.contains("Codex: connected\n"), "{lines}");
    assert!(lines.contains("Gemini CLI: failed: Error: no settings\n"));
    assert!(lines.contains("Cursor: connected\n"));
    assert!(lines.contains("Not found: Claude Code, Hermes Agent, Pi, Cline, Amp, OpenClaw."));
    let program = env!("CARGO_BIN_EXE_anyfetch");
    let arguments = std::fs::read_to_string(home.join("codex-arguments")).unwrap();
    assert_eq!(arguments, format!("mcp add anyfetch -- {program} mcp\n"));
    let cursor = std::fs::read(home.join(".cursor/mcp.json")).unwrap();
    let cursor: Value = serde_json::from_slice(&cursor).unwrap();
    assert_eq!(cursor["mcpServers"]["anyfetch"]["command"], program);
    let _ = std::fs::remove_dir_all(home);
}
