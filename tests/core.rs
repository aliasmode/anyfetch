use fetchroam::{
    fetch::extract,
    search::{parse_duckduckgo, parse_results, parse_searxng},
};

#[test]
fn duckduckgo_returns_organic_destinations_and_unicode() {
    let html = r#"<div class="result result--ad"><a class="result__a" href="https://ads.example/">Ad</a></div>
    <div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2F%E6%97%A5%E6%9C%AC%E8%AA%9E%3Fx%3D1%26y%3D2&amp;rut=fixture">Rust &amp; 日本語</a><a class="result__snippet">Useful <b>reference</b>.</a></div>
    <div class="result"><a class="result__a" href="https://example.org/direct">Direct</a><div class="result__snippet">Plain text</div></div>"#;
    let hits = parse_duckduckgo(html, 200).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].title, "Rust & 日本語");
    assert_eq!(
        hits[0].url,
        "https://example.org/%E6%97%A5%E6%9C%AC%E8%AA%9E?x=1&y=2"
    );
    assert_eq!(hits[0].snippet, "Useful reference.");
    assert_eq!(hits[1].url, "https://example.org/direct");
}

#[test]
fn duckduckgo_distinguishes_challenges_empty_results_and_bad_pages() {
    for (body, status) in [
        ("", 202),
        ("<form id=challenge-form></form>", 200),
        ("<div class=anomaly-modal></div>", 200),
    ] {
        let error = parse_duckduckgo(body, status).unwrap_err();
        assert_eq!(error.code, "challenge_required");
        assert_eq!(error.status, Some(status));
    }
    assert!(
        parse_duckduckgo(
            "<div class=no-results><div class=no-results__message>No results</div></div>",
            200
        )
        .unwrap()
        .is_empty()
    );
    for body in [
        "<html>Homepage</html>",
        "<div class=result><a class=result__a href='javascript:alert(1)'>Bad</a></div>",
        "<div class=result><a class=result__a>Missing</a></div>",
    ] {
        assert_eq!(
            parse_duckduckgo(body, 200).unwrap_err().code,
            "unexpected_response"
        );
    }
}

#[test]
fn searxng_requires_structured_search_results() {
    let hits = parse_searxng(
        r#"{"results":[{"title":"日本語","url":"https://example.org/","content":"Useful"}]}"#,
        200,
    )
    .unwrap();
    assert_eq!(hits[0].title, "日本語");
    assert_eq!(hits[0].snippet, "Useful");
    assert!(parse_searxng(r#"{"results":[]}"#, 200).unwrap().is_empty());
    for body in [
        "<html>Homepage</html>",
        "{}",
        r#"{"results":[{"title":"Bad","url":"javascript:alert(1)"}]}"#,
    ] {
        assert_eq!(
            parse_searxng(body, 200).unwrap_err().code,
            "unexpected_response"
        );
    }
}

#[tokio::test]
async fn searxng_requires_a_valid_instance_before_networking() {
    use fetchroam::search::{Provider, search};
    for url in [None, Some("not a URL"), Some("file:///tmp/search")] {
        let error = search("query", Provider::Searxng, url).await.unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert_eq!(error.provider, Some("searxng"));
    }
}

#[test]
fn search_reads_real_rss_results_with_direct_destinations() {
    let hits = parse_results(include_str!("fixtures/bing-rss.xml"), 200).unwrap();
    assert_eq!(hits.len(), 10);
    assert_eq!(hits[0].url, "https://www.rfc-editor.org/info/rfc6585/");
    assert!(
        hits.iter()
            .all(|hit| !hit.title.is_empty() && !hit.snippet.is_empty())
    );
}

#[test]
fn search_decodes_entities_and_preserves_unicode() {
    let xml = r#"<rss><channel><item><title>Rust &amp; 日本語</title>
      <link>https://example.org/guide?x=1&amp;y=2</link>
      <description><![CDATA[Useful <code> information.]]></description>
      </item></channel></rss>"#;
    let hits = parse_results(xml, 200).unwrap();
    assert_eq!(hits[0].title, "Rust & 日本語");
    assert_eq!(hits[0].url, "https://example.org/guide?x=1&y=2");
    assert_eq!(hits[0].snippet, "Useful <code> information.");
}

#[test]
fn soft_blocks_and_challenge_pages_are_not_empty_successes() {
    assert_eq!(
        parse_results("", 202).unwrap_err().code,
        "provider_unavailable"
    );
    for body in [
        "<html><form id=challenge-form></form></html>",
        "<html>Different layout</html>",
        "<rss>",
    ] {
        let error = parse_results(body, 200).unwrap_err();
        assert_eq!(error.code, "unexpected_response");
        assert_eq!(error.status, Some(200));
    }
}

#[test]
fn only_recognized_no_results_returns_an_empty_list() {
    assert!(
        parse_results("<rss><channel/></rss>", 200)
            .unwrap()
            .is_empty()
    );
    for body in [
        "<rss/>",
        "<html><channel/></html>",
        "<rss><channel><item><title>Missing link</title></item></channel></rss>",
        "<rss><channel><item><title>Bad link</title><link>javascript:alert(1)</link></item></channel></rss>",
    ] {
        assert!(parse_results(body, 200).is_err());
    }
}

#[test]
fn unexpected_success_status_does_not_override_real_results() {
    assert_eq!(
        parse_results(include_str!("fixtures/bing-rss.xml"), 202)
            .unwrap()
            .len(),
        10
    );
}

#[test]
fn article_extraction_keeps_links_code_and_tables() {
    let paragraphs = "<p>This reference explains how to build a useful command line research tool. It describes its configuration, expected outputs, and reusable interfaces. The examples below contain the details needed to complete the task.</p>".repeat(5);
    let html = format!(
        r#"<html><head><title>Research reference</title></head><body>
      <nav><a href="/login">Account login</a></nav><article><h1>Research reference</h1>{paragraphs}
      <p>Read the <a href="/guide">full guide</a> before using this example.</p>
      <pre><code class="language-rust">fn main() {{ println!("ready"); }}</code></pre>
      <table><thead><tr><th>Field</th><th>Type</th></tr></thead><tbody><tr><td>query</td><td>string</td></tr></tbody></table>
      </article></body></html>"#
    );
    let result = extract(&html, "https://example.org/reference").unwrap();
    assert!(result.content.contains("https://example.org/guide"));
    assert!(result.content.contains("```"));
    assert!(result.content.contains("fn main()"));
    assert!(result.content.contains("query"));
    assert!(result.content.contains("|"));
    assert!(!result.content.contains("Account login"));
}
