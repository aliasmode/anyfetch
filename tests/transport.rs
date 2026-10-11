use fetchroam::fetch::fetch;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    thread::{self, JoinHandle},
};

fn serve(responses: Vec<impl AsRef<[u8]> + Send + 'static>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            requests.push(request);
            stream.write_all(response.as_ref()).unwrap();
        }
        requests
    });
    (url, worker)
}

#[tokio::test]
async fn decodes_http_and_html_charset_declarations() {
    for (content_type, meta, expected) in [
        (
            "text/html; charset=windows-1252",
            "<meta charset=utf-8>",
            "café",
        ),
        ("text/html", "<meta charset=windows-1252>", "café"),
        (
            "Text/HTML",
            "<meta http-equiv=Content-Type content='text/html; charset=windows-1252'>",
            "café",
        ),
    ] {
        let paragraphs = "<p>A useful café reference. This article explains its sources and provides enough detail for readable article extraction.</p>".repeat(8);
        let html = format!(
            "<html><head>{meta}<title>Reference</title></head><body><article>{paragraphs}</article></body></html>"
        );
        let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(&html);
        let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).into_bytes();
        response.extend_from_slice(&bytes);
        let (url, worker) = serve(vec![response]);
        let result = fetch(&url).await.unwrap();
        assert!(result.content.contains(expected));
        assert!(!result.content.contains('\u{fffd}'));
        worker.join().unwrap();
    }
}

#[tokio::test]
async fn follows_redirects_preserves_cookies_and_reports_final_url() {
    let (url, worker) = serve(vec![
        "HTTP/1.1 302 Found\r\nLocation: /final\r\nSet-Cookie: test=ok; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_owned(),
    ]);
    let result = fetch(&format!("{url}/start")).await.unwrap();
    assert_eq!(result.requested_url, format!("{url}/start"));
    assert_eq!(result.url, format!("{url}/final"));
    assert_eq!(result.content, "hello");
    let requests = worker.join().unwrap();
    assert!(requests[1].to_ascii_lowercase().contains("cookie: test=ok"));
    assert!(requests[0].contains("Version/26.4 Safari"));
}

#[tokio::test]
async fn returns_feeds_as_text() {
    let feed = "<feed><entry><title>Post</title></entry></feed>";
    let (url, worker) = serve(vec![format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/atom+xml; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{feed}",
        feed.len()
    )]);
    let result = fetch(&url).await.unwrap();
    assert_eq!(result.content, feed);
    assert_eq!(result.total_chars, feed.len());
    worker.join().unwrap();
}

#[tokio::test]
async fn reads_the_text_and_title_of_a_pdf() {
    let pdf = include_bytes!("fixtures/page.pdf");
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        pdf.len()
    )
    .into_bytes();
    response.extend_from_slice(pdf);
    let (url, worker) = serve(vec![response]);
    let result = fetch(&url).await.unwrap();
    assert_eq!(result.title.as_deref(), Some("Fixture report"));
    assert_eq!(result.content, "[Page 1]\n\nAn efficient fixture page");
    worker.join().unwrap();

    let broken = "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: 9\r\nConnection: close\r\n\r\n%PDF-1.4\n";
    let (url, worker) = serve(vec![broken]);
    assert_eq!(fetch(&url).await.unwrap_err().code, "extraction_failed");
    worker.join().unwrap();
}

#[tokio::test]
async fn preserves_rate_limit_status_and_server_retry_timing() {
    let (url, worker) = serve(vec![
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 17\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
    ]);
    let error = fetch(&url).await.unwrap_err();
    assert_eq!(error.code, "rate_limited");
    assert_eq!(error.status, Some(429));
    assert_eq!(error.retry_after.as_deref(), Some("17"));
    worker.join().unwrap();
}

fn challenge(prefix: &str, algorithm: &str) -> String {
    format!(
        r#"<script id="anubis_challenge" type="application/json">{{"rules":{{"algorithm":"{algorithm}","difficulty":2}},"challenge":{{"id":"fixture-id","randomData":"fixture-seed"}}}}</script><script id="anubis_base_prefix" type="application/json">"{prefix}"</script>"#
    )
}

fn html_response(status: u16, html: &str, headers: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{html}",
        html.len()
    )
}

#[tokio::test]
async fn completes_anubis_with_the_same_session_and_final_response() {
    for (status, prefix) in [(200, ""), (403, "/protected")] {
        let (url, worker) = serve(vec![
            html_response(status, &challenge(prefix, "fast"), &format!("Set-Cookie: verification=fixture; Path={prefix}/\r\n")),
            format!("HTTP/1.1 302 Found\r\nLocation: {prefix}/final\r\nSet-Cookie: authenticated=fixture; Path={prefix}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=windows-1252\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".into(),
        ]);
        let requested = format!("{url}{prefix}/search?q=hello%20world&format=json");
        let result = fetch(&requested).await.unwrap();
        assert_eq!(result.content, "hello");
        assert_eq!(result.requested_url, requested);
        assert_eq!(result.url, format!("{url}{prefix}/final"));
        let requests = worker.join().unwrap();
        let pass = url::Url::parse(&format!(
            "{url}{}",
            requests[1].split_whitespace().nth(1).unwrap()
        ))
        .unwrap();
        assert_eq!(
            pass.path(),
            format!("{prefix}/.within.website/x/cmd/anubis/api/pass-challenge")
        );
        let fields: std::collections::HashMap<_, _> = pass.query_pairs().collect();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields["id"], "fixture-id");
        assert_eq!(fields["nonce"], "291");
        assert_eq!(
            fields["response"],
            "00e637e9abfbddeb3edefde7294f37bf078e20ee5f51163b99847a5efca0c0bc"
        );
        assert_eq!(fields["redir"], requested);
        assert!(fields["elapsedTime"].parse::<f64>().unwrap() >= 0.0);
        assert!(requests[1].contains("verification=fixture"));
        assert!(requests[2].contains("authenticated=fixture"));
        assert!(requests.iter().all(|r| r.contains("Version/26.4 Safari")));
    }
}

#[tokio::test]
async fn rejected_or_repeated_anubis_does_not_retry() {
    for (response, code, status) in [
        (
            html_response(200, &challenge("", "fast"), ""),
            "challenge_failed",
            200,
        ),
        (html_response(403, "rejected", ""), "http_error", 403),
        (
            html_response(429, &challenge("", "fast"), "Retry-After: 17\r\n"),
            "rate_limited",
            429,
        ),
    ] {
        let (url, worker) = serve(vec![
            html_response(403, &challenge("", "fast"), ""),
            response,
        ]);
        let error = fetch(&url).await.unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.status, Some(status));
        if status == 429 {
            assert_eq!(error.retry_after.as_deref(), Some("17"));
        }
        assert!(!error.message.contains("fixture-id"));
        assert_eq!(worker.join().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn unsupported_and_malformed_anubis_are_explicit_errors() {
    for html in [
        challenge("", "unknown"),
        "<script id=anubis_challenge>{broken</script>".into(),
        "<script id=anubis_challenge>{}</script>".into(),
    ] {
        let (url, worker) = serve(vec![html_response(403, &html, "")]);
        let error = fetch(&url).await.unwrap_err();
        assert_eq!(error.code, "challenge_unsupported");
        assert_eq!(error.status, Some(403));
        assert_eq!(worker.join().unwrap().len(), 1);
    }
    let (url, worker) = serve(vec![html_response(403, "ordinary forbidden page", "")]);
    assert_eq!(fetch(&url).await.unwrap_err().code, "http_error");
    worker.join().unwrap();
}

#[tokio::test]
async fn rejects_unsupported_content_before_reading_the_body() {
    for challenged in [false, true] {
        let mut responses = Vec::new();
        if challenged {
            responses.push(html_response(403, &challenge("", "fast"), ""));
        }
        responses.push("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n".into());
        let (url, worker) = serve(responses);
        let error = fetch(&url).await.unwrap_err();
        assert_eq!(error.code, "unsupported_content");
        assert_eq!(error.status, Some(200));
        worker.join().unwrap();
    }
}

#[tokio::test]
async fn challenge_header_is_not_a_successful_fetch() {
    for (status, header) in [
        ("200 OK", "cf-mitigated: challenge"),
        ("202 Accepted", "x-amzn-waf-action: challenge"),
    ] {
        let (url, worker) = serve(vec![format!(
            "HTTP/1.1 {status}\r\n{header}\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )]);
        let error = fetch(&url).await.unwrap_err();
        assert_eq!(error.code, "challenge_required");
        assert_eq!(error.status, status[..3].parse().ok());
        worker.join().unwrap();
    }
}

#[tokio::test]
async fn cloudflare_challenge_is_retried_twice_with_the_same_browser_identity() {
    let challenge = "HTTP/1.1 403 Forbidden\r\ncf-mitigated: challenge\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let page = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
    let (url, worker) = serve(vec![challenge, challenge, page]);
    assert_eq!(fetch(&url).await.unwrap().content, "hello");
    let requests = worker.join().unwrap();
    assert!(requests.iter().all(|r| r.contains("Version/26.4 Safari")));

    let (url, worker) = serve(vec![challenge, challenge, challenge]);
    let error = fetch(&url).await.unwrap_err();
    assert_eq!(error.code, "challenge_required");
    assert_eq!(worker.join().unwrap().len(), 3);
}

#[tokio::test]
async fn refused_request_is_retried_once_with_the_second_browser_identity() {
    let page = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
    for refusal in ["401 Unauthorized", "403 Forbidden"] {
        let (url, worker) = serve(vec![
            format!("HTTP/1.1 {refusal}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            page.to_owned(),
        ]);
        assert_eq!(fetch(&url).await.unwrap().content, "hello");
        let requests = worker.join().unwrap();
        assert!(requests[0].contains("Version/26.4 Safari"));
        assert!(requests[1].contains("Edg/148"));
    }
    let refused = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let limited = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 17\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    for (second, code) in [(refused, "http_error"), (limited, "rate_limited")] {
        let (url, worker) = serve(vec![refused, second]);
        assert_eq!(fetch(&url).await.unwrap_err().code, code);
        assert_eq!(worker.join().unwrap().len(), 2);
    }
}
