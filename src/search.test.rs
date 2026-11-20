use super::*;
use serde_json::json;

fn pairs(url: &Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

#[test]
fn provider_selection_builds_only_the_selected_endpoint() {
    assert_eq!(Provider::default().name(), "auto");
    let filters = Filters {
        site: Some("example.com".into()),
        time: Some(Time::Month),
        page: 2,
    };
    let instance = Some("https://example.org/nested/?old=value#fragment");
    for (provider, address, expected) in [
        (
            Provider::Brave,
            "https://search.brave.com/search",
            vec![
                ("q", "Rust & 日本語 site:example.com"),
                ("source", "web"),
                ("tf", "pm"),
                ("offset", "1"),
            ],
        ),
        (
            Provider::Searxng,
            "https://example.org/nested/search",
            vec![
                ("q", "Rust & 日本語 site:example.com"),
                ("format", "json"),
                ("time_range", "month"),
                ("pageno", "2"),
            ],
        ),
        (
            Provider::Github,
            "https://api.github.com/search/repositories",
            vec![("q", "Rust & 日本語"), ("per_page", "10"), ("page", "2")],
        ),
        (
            Provider::Arxiv,
            "https://export.arxiv.org/api/query",
            vec![
                ("search_query", "all:Rust & 日本語"),
                ("max_results", "10"),
                ("start", "10"),
            ],
        ),
    ] {
        let url = provider.url("Rust & 日本語", instance, &filters).unwrap();
        assert_eq!(&url[..url::Position::AfterPath], address);
        let expected: Vec<_> = expected
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(pairs(&url), expected);
        assert!(url.fragment().is_none());
    }
    // DuckDuckGo: first page only, the domain as a keyword, and its own date codes.
    assert!(Provider::Duckduckgo.url("rust", None, &filters).is_err());
    let first = Filters { page: 1, ..filters };
    let url = Provider::Duckduckgo.url("rust", None, &first).unwrap();
    let far = Filters {
        page: 101,
        ..Filters::default()
    };
    let error = Provider::Brave.url("rust", None, &far).unwrap_err();
    assert_eq!(error.code, "invalid_request");
    let zero = Filters::default();
    assert!(Provider::Brave.url("rust", None, &zero).is_ok());
    assert_eq!(
        pairs(&url),
        [
            ("q".into(), "rust example.com".into()),
            ("df".into(), "m".into())
        ]
    );
}

#[test]
fn auto_merges_engines_and_ranks_shared_pages_first() {
    let list = |urls: &[&str]| {
        urls.iter()
            .map(|url| SearchHit::new("t", *url, "s"))
            .collect()
    };
    let merged = fuse(vec![
        list(&["https://a.org/", "https://b.org/x", "https://c.org/"]),
        list(&["https://d.org/", "http://www.c.org#top"]),
    ]);
    let urls: Vec<_> = merged.iter().map(|hit| &*hit.url).collect();
    assert_eq!(
        urls,
        [
            "https://c.org/",
            "https://a.org/",
            "https://d.org/",
            "https://b.org/x"
        ]
    );
}

#[test]
fn specialist_sources_become_titles_links_and_summaries() {
    let github = json!({"items":[{"full_name":"tokio-rs/tokio","html_url":"https://github.com/tokio-rs/tokio",
        "description":"A runtime.","stargazers_count":30000,"language":"Rust","pushed_at":"2026-10-01T10:00:00Z"}]});
    let hit = &parse_api(Provider::Github, &github)[0];
    assert_eq!(
        (&*hit.title, &*hit.url),
        ("tokio-rs/tokio", "https://github.com/tokio-rs/tokio")
    );
    assert_eq!(
        hit.snippet,
        "A runtime. Stars: 30000. Language: Rust. Updated: 2026-10-01."
    );

    let questions = json!({"items":[{"title":"What does &quot;yield&quot; do?","link":"https://stackoverflow.com/q/1",
        "score":12,"answer_count":3,"tags":["python","generator"]}]});
    let hit = &parse_api(Provider::Stackoverflow, &questions)[0];
    assert_eq!(hit.title, "What does \"yield\" do?");
    assert_eq!(
        hit.snippet,
        "Score: 12. Answers: 3. Tags: python, generator."
    );

    let stories = json!({"hits":[{"title":"Show HN","url":null,"objectID":"42","points":10,"num_comments":2,
        "created_at":"2026-10-11T08:00:00Z"}]});
    let hit = &parse_api(Provider::Hackernews, &stories)[0];
    assert_eq!(hit.url, "https://news.ycombinator.com/item?id=42");
    assert!(
        hit.snippet
            .starts_with("Points: 10. Comments: 2. Date: 2026-10-11.")
    );

    let papers = json!({"results":[{"id":"https://openalex.org/W1","doi":"https://doi.org/10.1/x",
        "title":"A paper","publication_year":2017,"cited_by_count":9,
        "open_access":{"oa_url":"https://example.org/paper.pdf"},
        "abstract_inverted_index":{"show":[1],"We":[0,3],"that":[2]}}]});
    let hit = &parse_api(Provider::Papers, &papers)[0];
    assert_eq!(hit.url, "https://example.org/paper.pdf");
    assert_eq!(hit.snippet, "Year: 2017. Citations: 9. We show that We");
    assert!(parse_api(Provider::Github, &json!({"message":"rate limited"})).is_empty());

    let arxiv = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>query</title>
            <entry><id>http://arxiv.org/abs/1706.03762v7</id><published>2017-06-12T17:57:34Z</published>
            <title>Attention Is All
              You Need</title><summary>  The dominant models.  </summary></entry></feed>"#;
    let hit = &parse_arxiv(arxiv)[0];
    assert_eq!(hit.title, "Attention Is All You Need");
    assert_eq!(hit.url, "http://arxiv.org/abs/1706.03762v7");
    assert_eq!(hit.snippet, "Published: 2017-06-12. The dominant models.");
}
