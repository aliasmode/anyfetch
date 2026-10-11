use fetchroam::{
    fetch::extract,
    search::{parse_brave, parse_duckduckgo, parse_searxng},
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
fn brave_returns_web_results_with_direct_links() {
    let hits = parse_brave(include_str!("fixtures/brave.html"), 200).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].url, "https://corrode.dev/blog/async/");
    assert_eq!(
        hits[0].title,
        "The State of Async Rust: Runtimes | corrode Rust Consulting"
    );
    assert!(
        hits[0]
            .snippet
            .contains("Tokio stands as Rust’s canonical async runtime.")
    );
    let error = parse_brave("<html><body>Checking your browser</body></html>", 200).unwrap_err();
    assert_eq!(error.code, "unexpected_response");
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

#[test]
fn extraction_reports_the_page_description() {
    let body = "<p>A script-rendered profile shell that still has enough plain words for readable extraction.</p>".repeat(5);
    for (meta, expected) in [
        (
            r#"<meta property="og:description" content="Open graph"><meta name="description" content=" Profile bio ">"#,
            Some("Profile bio"),
        ),
        (
            r#"<meta property="og:description" content="Open graph">"#,
            Some("Open graph"),
        ),
        ("", None),
    ] {
        let html = format!(
            "<html><head><title>Profile</title>{meta}</head><body><article>{body}</article></body></html>"
        );
        let result = extract(&html, "https://example.org/profile").unwrap();
        assert_eq!(result.description.as_deref(), expected);
    }
}

#[test]
fn script_rendered_page_falls_back_to_its_title_and_description() {
    let shell = |meta: &str| {
        format!(
            "<html><head><title> Video title </title>{meta}</head><body><div id=app></div><script>render()</script></body></html>"
        )
    };
    let page = extract(
        &shell(r#"<meta name="description" content="What the video shows.">"#),
        "https://example.org/watch",
    )
    .unwrap();
    assert_eq!(page.title, "Video title");
    assert_eq!(page.content, "What the video shows.");
    assert!(page.summary_only);
    let error = extract(&shell(""), "https://example.org/watch").unwrap_err();
    assert_eq!(error.code, "extraction_failed");
}

#[test]
fn script_rendered_page_yields_the_text_in_its_embedded_data() {
    let html = r#"<html><head><title>Video</title>
        <meta name="description" content="Short summary.">
        <script type="application/ld+json">{"@type":"VideoObject","name":"Ignored label",
          "description":"The full description of the video, from the structured data."}</script>
        <script>var playerResponse = {"videoDetails":{"videoId":"x1",
          "shortDescription":"Line one of the uploader's notes.\nLine two has more detail.",
          "keywords":["a","b"]},"config":{"description":"internal settings text, not content"}};
          render(playerResponse);</script>
        </head><body><div id="app"></div></body></html>"#;
    let content = extract(html, "https://example.org/watch").unwrap().content;
    assert!(content.starts_with("Short summary."));
    assert!(content.contains("The full description of the video, from the structured data."));
    assert!(content.contains("Line one of the uploader's notes.\nLine two has more detail."));
    assert!(!content.contains("internal settings"));
    assert!(!content.contains("Ignored label"));
}

#[test]
fn article_is_not_replaced_by_its_smaller_embedded_data() {
    let paragraphs = "<p>This reference explains how to build a useful command line research tool. It describes its configuration, expected outputs, and reusable interfaces.</p>".repeat(6);
    let html = format!(
        r#"<html><head><title>Reference</title><script type="application/ld+json">
        {{"headline":"A headline that is only in the data","description":"A teaser that is only in the data."}}
        </script></head><body><article><h1>Reference</h1>{paragraphs}</article></body></html>"#
    );
    let content = extract(&html, "https://example.org/reference")
        .unwrap()
        .content;
    assert!(content.contains("command line research tool"));
    assert!(!content.contains("only in the data"));
}

#[test]
fn extraction_keeps_wrapped_and_line_split_code_blocks() {
    let prose = "<p>This guide explains how to configure the tool. It lists each option, the expected values, and the commands that apply the change.</p>".repeat(4);
    let html = format!(
        r#"<html><head><title>Guide</title></head><body><article><h1>Guide</h1>{prose}
<div class="code-block"><div class="overflow-y-hidden scroll-area"><div><pre class="shiki" language="bash"><code><span class="line"><span>tool add one</span></span>
<span class="line"></span>
<span class="line"><span>tool run</span></span></code></pre></div></div></div>{prose}
<figure><figcaption>config.json</figcaption><pre data-language="json"><code><div class="ec-line"><div class="code"><span>{{</span></div></div><div class="ec-line"><div class="code"><span class="indent">  </span><span>"a": 1</span></div></div><div class="ec-line"><div class="code"><span>}}</span></div></div></code></pre></figure>{prose}
<pre><code class="language-sh"><span class="token-line">first<br></span><span class="code-line" data-line="2">second <span class="hl">cmd</span></span><span class="code-line" data-line="3">third</span></code></pre>
</article></body></html>"#
    );
    let content = extract(&html, "https://example.org/guide").unwrap().content;
    assert!(content.contains("```bash\ntool add one\n\ntool run\n```"));
    assert!(content.contains("```json\n{\n  \"a\": 1\n}\n```"));
    assert!(content.contains("```sh\nfirst\nsecond cmd\nthird\n```"));
}
