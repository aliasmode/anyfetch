use super::*;

#[test]
fn returns_a_stored_page_for_the_same_url_only() {
    let directory =
        std::env::temp_dir().join(format!("anyfetch-cache-test-{}", std::process::id()));
    let page = FetchResponse {
        requested_url: "https://example.org/a".into(),
        url: "https://example.org/a".into(),
        status: 200,
        content_type: "text/html".into(),
        retrieved_at: "2026-01-01T00:00:00Z".into(),
        title: Some("A".into()),
        description: None,
        total_chars: 5,
        focused: false,
        content: "hello".into(),
    };
    write(&directory, "https://example.org/a", &page);
    assert_eq!(
        read(&directory, "https://example.org/a").unwrap().content,
        "hello"
    );
    assert!(read(&directory, "https://example.org/b").is_none());
    std::fs::remove_dir_all(directory).unwrap();
}
