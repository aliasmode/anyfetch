use super::*;

const PAGE: &str = "<html><head><title>T</title><style>p{}</style></head><body>\
    <h1>Head</h1><p class=a>One <a href=/x>link</a>\n   here</p>\
    <script>var x = 1;</script><div class=a><p class=a>Two</p></div><p>Three</p>\
    </body></html>";

#[test]
fn parses_render_modes() {
    let parse = |text: &str| serde_json::from_str::<Render>(text).unwrap();
    assert_eq!(parse("true"), Render::On);
    assert_eq!(parse("false"), Render::Off);
    assert_eq!(parse("\"auto\""), Render::Auto);
    assert_eq!(parse("\"true\""), Render::On);
    assert_eq!(parse("\"false\""), Render::Off);
    assert!(serde_json::from_str::<Render>("\"maybe\"").is_err());
    assert_eq!("TRUE".parse(), Ok(Render::On));
    assert_eq!("auto".parse(), Ok(Render::Auto));
    assert!("x".parse::<Render>().is_err());
    assert_eq!(Render::default(), Render::Auto);
}

#[test]
fn cache_keys_do_not_collide() {
    let url = "https://example.com/a";
    let with = |format, selector, render| Options {
        format,
        selector,
        render,
        ..Options::default()
    };
    assert_eq!(cache_key(url, &Options::default()), url);
    let one = cache_key(url, &with(Format::Text, Some("a\nb"), Render::Auto));
    let two = cache_key(url, &with(Format::Text, Some("a"), Render::Auto));
    assert_ne!(one, two);
    let plain = cache_key(url, &with(Format::Readable, None, Render::Auto));
    let rendered = cache_key(url, &with(Format::Readable, None, Render::On));
    assert_ne!(plain, rendered);
    assert_ne!(
        cache_key(url, &with(Format::Html, None, Render::Auto)),
        cache_key(url, &with(Format::Html, None, Render::Off))
    );
    // A selector holding what used to be the next part of the key.
    let tricky = cache_key(url, &with(Format::Html, Some("x"), Render::Auto));
    let split = cache_key(
        "https://example.com/a\nHtml",
        &with(Format::Html, Some("x"), Render::Auto),
    );
    assert_ne!(tricky, split);
}

#[test]
fn language_is_in_the_key_of_a_readable_video_only() {
    let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    let lang = |lang, format| Options {
        lang,
        format,
        ..Options::default()
    };
    assert_ne!(
        cache_key(url, &lang(Some("es"), Format::Readable)),
        cache_key(url, &lang(None, Format::Readable))
    );
    assert_eq!(
        cache_key(url, &lang(None, Format::Readable)),
        cache_key(url, &lang(Some("en"), Format::Readable))
    );
    assert_eq!(
        cache_key(url, &lang(Some("es"), Format::Text)),
        cache_key(url, &lang(None, Format::Text))
    );
}

#[test]
fn an_empty_selector_is_not_set() {
    assert_eq!(non_empty(Some("")), None);
    assert_eq!(non_empty(None), None);
    assert_eq!(non_empty(Some("p")), Some("p"));
}

fn view(format: Format, selector: Option<&str>) -> Result<String, ToolError> {
    convert(PAGE, format, selector).map(|page| page.content)
}

#[test]
fn converts_the_whole_page() {
    let markdown = view(Format::Markdown, None).unwrap();
    assert!(markdown.starts_with("# Head"), "{markdown}");
    assert!(markdown.contains("[link](/x)"), "{markdown}");
    assert!(!markdown.contains("var x"), "{markdown}");
    assert_eq!(
        view(Format::Text, None).unwrap(),
        "Head\nOne link here\nTwo\nThree"
    );
    assert_eq!(view(Format::Html, None).unwrap(), PAGE);
    assert_eq!(convert(PAGE, Format::Text, None).unwrap().title, "T");
}

#[test]
fn keeps_the_outer_matches_in_page_order() {
    let html = view(Format::Html, Some(".a")).unwrap();
    assert_eq!(
        html,
        "<p class=\"a\">One <a href=\"/x\">link</a>\n   here</p>\n\
         <div class=\"a\"><p class=\"a\">Two</p></div>"
    );
    assert_eq!(
        view(Format::Text, Some("p")).unwrap(),
        "One link here\nTwo\nThree"
    );
    let markdown = view(Format::Markdown, Some("h1, p:last-child")).unwrap();
    assert_eq!(markdown, "# Head\n\nTwo\n\nThree");
    assert_eq!(view(Format::Readable, Some("h1")).unwrap(), "# Head");
}

#[test]
fn reports_a_selector_that_fails() {
    assert_eq!(
        view(Format::Html, Some("table")).unwrap_err().code,
        "no_match"
    );
    let error = view(Format::Text, Some("p[")).unwrap_err();
    assert_eq!(error.code, "invalid_request");
}
