use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

struct Session {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl Session {
    async fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fetchroam"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut session = Self {
            child,
            input,
            output,
        };
        session
            .send(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                    "protocolVersion":"2025-11-25","capabilities":{},
                    "clientInfo":{"name":"fetchroam-test","version":"1"}
                }}),
            )
            .await;
        let initialized = session.receive().await;
        assert_eq!(initialized["id"], 1);
        assert_eq!(initialized["result"]["serverInfo"]["name"], "fetchroam");
        session
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        session
    }

    async fn send(&mut self, value: Value) {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
    }

    async fn receive(&mut self) -> Value {
        let line = timeout(Duration::from_secs(10), self.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("MCP response");
        serde_json::from_str(&line).expect("stdout must contain only JSON-RPC")
    }

    async fn receive_after_cancellation(&mut self, id: u64) -> Value {
        let reply = self.receive().await;
        if reply["id"] == id {
            assert_eq!(
                reply["result"]["structuredContent"]["error"]["code"],
                "cancelled"
            );
            self.receive().await
        } else {
            reply
        }
    }

    async fn close(mut self, cancelled_id: Option<u64>) {
        drop(self.input);
        assert!(
            timeout(Duration::from_secs(10), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        while let Some(line) = self.output.next_line().await.unwrap() {
            let reply: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(reply["id"].as_u64(), cancelled_id);
            assert_eq!(
                reply["result"]["structuredContent"]["error"]["code"],
                "cancelled"
            );
        }
    }
}

#[tokio::test]
async fn stdio_exposes_two_tools_and_preserves_results_and_errors() {
    let mut session = Session::start().await;
    session
        .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    let listed = session.receive().await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let mut names = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["web_fetch", "web_search"]);
    for tool in tools {
        let field = if tool["name"] == "web_fetch" {
            "url"
        } else {
            "query"
        };
        assert_eq!(tool["inputSchema"]["required"], json!([field]));
        if field == "query" {
            assert_eq!(
                tool["inputSchema"]["properties"]["provider"]["default"],
                "duckduckgo"
            );
            assert_eq!(
                tool["inputSchema"]["$defs"]["Provider"]["enum"],
                json!(["duckduckgo", "bing", "searxng"])
            );
        }
    }

    for (id, response, expected_error) in [
        (
            3,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            None,
        ),
        (
            4,
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 17\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Some("rate_limited"),
        ),
        (
            5,
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Some("unsupported_content"),
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let fixture = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        session
            .send(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                    "name":"web_fetch","arguments":{"url":url}
                }}),
            )
            .await;
        let reply = session.receive().await;
        assert_eq!(reply["id"], id);
        let result = &reply["result"];
        assert_eq!(result["isError"], expected_error.is_some());
        let structured = &result["structuredContent"];
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            *structured
        );
        if let Some(code) = expected_error {
            assert_eq!(structured["error"]["code"], code);
            if code == "rate_limited" {
                assert_eq!(structured["error"]["status"], 429);
                assert_eq!(structured["error"]["retry_after"], "17");
            }
        } else {
            assert_eq!(structured["content"], "hello");
            assert_eq!(structured["url"], url);
            assert!(structured["retrieved_at"].as_str().unwrap().ends_with('Z'));
        }
        fixture.await.unwrap();
    }
    session
        .send(
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
                "name":"web_search","arguments":{}
            }}),
        )
        .await;
    assert_eq!(session.receive().await["result"]["isError"], true);
    session.close(None).await;
}

#[tokio::test]
async fn search_provider_selection_preserves_results_and_errors() {
    let mut session = Session::start().await;
    for arguments in [
        json!({"query":"test","provider":"brave"}),
        json!({"query":"test","provider":"searxng"}),
    ] {
        session.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"web_search","arguments":arguments}})).await;
        let reply = session.receive().await;
        assert_eq!(reply["result"]["isError"], true);
        if arguments["provider"] == "searxng" {
            assert_eq!(
                reply["result"]["structuredContent"]["error"]["provider"],
                "searxng"
            );
            assert_eq!(
                reply["result"]["structuredContent"]["error"]["code"],
                "invalid_request"
            );
        }
    }
    for (status, body, expected) in [
        (
            200,
            r#"{"results":[{"title":"Reference","url":"https://example.org/","content":"Snippet"}]}"#,
            None,
        ),
        (200, "<html>Homepage</html>", Some("unexpected_response")),
        (403, "Forbidden", Some("http_error")),
        (429, "Limited", Some("rate_limited")),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/instance/", listener.local_addr().unwrap());
        let fixture = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nRetry-After: 17\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            drop(socket);
            assert!(
                timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
            String::from_utf8(request).unwrap()
        });
        session.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"web_search","arguments":{"query":"Rust & 日本語","provider":"searxng","searxng_url":base}}})).await;
        let reply = session.receive().await;
        let value = &reply["result"]["structuredContent"];
        assert_eq!(reply["result"]["isError"], expected.is_some());
        if let Some(code) = expected {
            assert_eq!(value["error"]["code"], code);
            assert_eq!(value["error"]["provider"], "searxng");
            assert_eq!(value["error"]["status"], status);
            if status == 429 {
                assert_eq!(value["error"]["retry_after"], "17");
            }
        } else {
            assert_eq!(value["provider"], "searxng");
            assert_eq!(value["results"][0]["url"], "https://example.org/");
            assert!(value["source_url"].as_str().unwrap().starts_with(&base));
        }
        let request = fixture.await.unwrap();
        let request_url = url::Url::parse(&format!(
            "http://fixture{}",
            request.split_whitespace().nth(1).unwrap()
        ))
        .unwrap();
        assert_eq!(request_url.path(), "/instance/search");
        assert_eq!(
            request_url.query_pairs().collect::<Vec<_>>(),
            [
                ("q".into(), "Rust & 日本語".into()),
                ("format".into(), "json".into())
            ]
        );
    }
    session.close(None).await;
}

#[tokio::test]
async fn cancellation_stops_anubis_work_and_allows_clean_shutdown() {
    let mut session = Session::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    session
        .send(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"web_fetch","arguments":{"url":url}
            }}),
        )
        .await;
    let (mut socket, _) = timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        request.push(socket.read_u8().await.unwrap());
    }
    let html = r#"<script id="anubis_challenge">{"rules":{"algorithm":"fast","difficulty":64},"challenge":{"id":"synthetic","randomData":"synthetic"}}</script>"#;
    socket.write_all(format!("HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len()).as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    session
        .send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
        .await;
    assert_eq!(session.receive().await["id"], 3);
    session
        .send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}))
        .await;
    session
        .send(json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
        .await;
    assert_eq!(session.receive_after_cancellation(2).await["id"], 4);
    session.close(Some(2)).await;
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_drops_the_http_request_and_server_remains_usable() {
    let mut session = Session::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    session
        .send(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"web_fetch","arguments":{"url":url}
            }}),
        )
        .await;
    let (mut socket, _) = timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        request.push(socket.read_u8().await.unwrap());
    }
    session
        .send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}))
        .await;
    let mut remaining = Vec::new();
    assert_eq!(
        timeout(Duration::from_secs(10), socket.read_to_end(&mut remaining))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    session
        .send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
        .await;
    assert_eq!(session.receive_after_cancellation(2).await["id"], 3);
    session.close(Some(2)).await;
}
