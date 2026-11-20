use super::*;
use serde_json::json;

#[test]
fn lang_validator() {
    for ok in ["en", "es", "pt-BR", "zh-Hans", "fil", "es-419"] {
        assert!(valid_lang(ok), "{ok}");
    }
    for bad in [
        "",
        "all",
        "e",
        "en-",
        "en-x",
        "en_US",
        "en,fr",
        "-en",
        "en-abcdefghi",
        "e1",
        "--all",
    ] {
        assert!(!valid_lang(bad), "{bad}");
    }
}

#[test]
fn stderr_last_line() {
    let text = "WARNING: x\nERROR: [youtube] xyz: Video unavailable  \n\n  \n";
    assert_eq!(
        last_line(text),
        Some("ERROR: [youtube] xyz: Video unavailable")
    );
    assert_eq!(last_line(" \n\n"), None);
    assert_eq!(last_line(""), None);
}

#[test]
fn routes_known_pages_to_their_public_endpoint() {
    let route = |url: &str| route(&Url::parse(url).unwrap());
    for (url, expected) in [
        (
            "https://stackoverflow.com/questions/231767/what-does-yield-do",
            Some(Route::Question("stackoverflow".into(), "231767".into())),
        ),
        (
            "https://unix.stackexchange.com/questions/42/title?noredirect=1",
            Some(Route::Question("unix".into(), "42".into())),
        ),
        ("https://example.org/questions/42/title", None),
        ("https://stackoverflow.com/questions/tagged/rust", None),
        (
            "https://x.com/user/status/15194807",
            Some(Route::Tweet("15194807".into())),
        ),
        ("https://x.com/user", None),
        (
            "https://pypi.org/project/requests/",
            Some(Route::PyPi("requests".into())),
        ),
        (
            "https://crates.io/crates/serde",
            Some(Route::Crate("serde".into())),
        ),
        (
            "https://www.reddit.com/r/rust/comments/abc/a_post/",
            Some(Route::Reddit(
                "/r/rust/comments/abc/a_post".into(),
                String::new(),
            )),
        ),
        (
            "https://www.reddit.com/r/rust/top/?t=week",
            Some(Route::Reddit("/r/rust/top".into(), "&t=week".into())),
        ),
        ("https://www.reddit.com/r/rust/.rss", None),
        ("https://www.reddit.com/r/rust/wiki/index", None),
        (
            "https://www.reuters.com/world/us/early-voting-begins-2026-10-06/",
            Some(Route::Reuters(
                "https://www.reuters.com/world/us/early-voting-begins-2026-10-06/?outputType=json".into(),
            )),
        ),
        ("https://www.reuters.com/world/us/", None),
        ("https://pypi.org/project/requests/2.31.0/", None),
        ("https://crates.io/crates/serde/versions", None),
    ] {
        assert_eq!(route(url), expected, "{url}");
    }
}

#[test]
fn renders_a_question_with_its_answers() {
    let question = json!({"items":[{"title":"What does &quot;yield&quot; do?","score":13000,
        "tags":["python","generator"],"body":"<p>How does it work?</p><pre><code>yield x\n</code></pre>"}]});
    let answers = json!({"items":[
        {"score":17000,"is_accepted":true,"body":"<p>It makes a generator.</p>"},
        {"score":12,"is_accepted":false,"body":"<p>Another view.</p>"}]});
    let page = question_page(&question, &answers).unwrap();
    assert_eq!(page.title, "What does \"yield\" do?");
    assert!(page.content.starts_with("How does it work?"));
    assert!(page.content.contains("```\nyield x\n"));
    assert!(
        page.content
            .contains("Tags: python, generator. Score: 13000.")
    );
    assert!(
        page.content
            .contains("## Answer (score 17000, accepted)\n\nIt makes a generator")
    );
    assert!(
        page.content
            .contains("## Answer (score 12)\n\nAnother view")
    );
    assert!(question_page(&json!({"items":[]}), &answers).is_none());
}

#[test]
fn renders_a_post_and_a_feed() {
    let tweet = json!({"text":"A short post","created_at":"2022-04-28T00:56:58.000Z",
        "favorite_count":41,"user":{"name":"Some One","screen_name":"someone"}});
    let page = tweet_page(&tweet).unwrap();
    assert_eq!(page.title, "Some One (@someone) on X");
    assert_eq!(
        page.content,
        "A short post\n\nSome One (@someone), 2022-04-28T00:56:58.000Z. Likes: 41."
    );
    let feed = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"><title>r/rust</title>
            <entry><author><name>/u/ann</name></author><content type="html">&lt;p&gt;Body &lt;b&gt;text&lt;/b&gt;&lt;/p&gt;</content>
            <link href="https://www.reddit.com/r/rust/comments/1/"/><updated>2026-10-11T00:00:00+00:00</updated>
            <title>First post</title></entry></feed>"#;
    let page = feed_page(feed).unwrap();
    assert_eq!(page.title, "r/rust");
    assert_eq!(
        page.content,
        "## First post\n\nhttps://www.reddit.com/r/rust/comments/1/\n/u/ann, 2026-10-11T00:00:00+00:00\n\nBody **text**"
    );
    assert!(feed_page("<html><body>blocked</body></html>").is_none());
}

#[test]
fn renders_a_reuters_article_from_the_data_of_its_page() {
    let blocks = json!([
        {"type": "page-metadata", "data": {"title": "Site title"}},
        {"type": "article_detail", "data": {"article": {
            "title": "Rates rise",
            "published_time": "2026-10-07T04:33:49Z",
            "sign_off": "Reporting by \u{200b}A. Writer",
            "content_elements": [
                {"type": "paragraph", "content": "The bank raised its <a href=\"/x\">rate</a> to 5.5% on Wed\u{200c}nesday."},
                {"type": "placeholder", "content": ["adSlot_mpu"]},
                {"type": "header", "level": 1, "content": "WHAT COMES NEXT"},
                {"type": "paragraph", "content": "Markets expect more."}
            ]}}}
    ]);
    let page = reuters_page(&blocks).unwrap();
    assert_eq!(page.title, "Rates rise");
    assert_eq!(
        page.content,
        "Reuters, 2026-10-07T04:33:49Z.\n\nThe bank raised its rate to 5.5% on Wednesday.\n\n\
         ## WHAT COMES NEXT\n\nMarkets expect more.\n\nReporting by A. Writer"
    );
    assert!(reuters_page(&json!([{"data": {}}])).is_none());
}

#[test]
fn reads_a_linkedin_profile_without_the_sign_in_windows() {
    let data = json!({"@graph": [
        {"@type": "Person", "name": "Ada Lovelace"},
        {"@type": "Article", "headline": "Notes", "url": "https://www.linkedin.com/pulse/notes",
            "datePublished": "2026-01-02T03:04:05.000+00:00",
            "interactionStatistic": {"userInteractionCount": 7}},
        {"@type": "DiscussionForumPosting", "text": "First\nprogram", "author": {"name": "Ada Lovelace"},
            "url": "https://www.linkedin.com/posts/ada_1", "datePublished": "2026-02-03T00:00:00.000+00:00"}
    ]});
    let html = format!(
        r#"<script type="application/ld+json">{data}</script>
            <h1 class="top-card-layout__title">Ada  Lovelace</h1>
            <h2 class="top-card-layout__headline">Mathematician</h2>
            <div class="profile-info-subheader"><span>London</span></div>
            <div class="top-card-layout__first-subline"><span class="not-first-middot">
              <span>9 followers</span></span></div>
            <div data-section="currentPositionsDetails">
              <span class="top-card-link__description">Analytical Engines <img alt></span></div>
            <section data-section="summary"><h2>About</h2>
              <div class="core-section-container__content"><p>I write programs.</p>
                <div class="modal"><h2>Welcome back</h2><button>Sign in</button></div></div></section>
            <section data-section="experience"><h2>Experience</h2><ul>
              <li class="profile-section-card"><h3>Analyst</h3><h4>Analytical Engines</h4>
                <span class="date-range">1842 - 1843</span>
                <div class="show-more-less-text"><p class="show-more-less-text__text--less">The first…</p>
                  <p class="show-more-less-text__text--more">The first program.</p></div>
                <ul><li><a class="face-pile__url" href="https://uk.linkedin.com/in/cb" title="C. B."><img></a></li>
                  <li><button>Show more</button></li></ul></li></ul></section>
            <section data-section="skills"><h2>Skills</h2><ul><li>Mathematics</li></ul></section>
            <section data-section="posts"><h2>Posts</h2><ul><li>Sign in to see</li></ul></section>"#
    );
    let page = parse("https://uk.linkedin.com/in/ada", &html).unwrap();
    assert_eq!(page.title, "Ada Lovelace | LinkedIn");
    assert_eq!(
        page.content,
        "Mathematician\n\nLondon · 9 followers · Analytical Engines\n\n## About\n\nI write programs.\n\n## Experience\n\n\
         - **Analyst**, Analytical Engines\n  1842 - 1843\n  The first program.\n  \
         - [C. B.](https://uk.linkedin.com/in/cb)\n\n\
         ## Skills\n\n- Mathematics\n\n\
         ## Articles\n\n- [Notes](https://www.linkedin.com/pulse/notes), 2026-01-02, 7 likes\n\n\
         ## Posts\n\n- Ada Lovelace, 2026-02-03: First program https://www.linkedin.com/posts/ada_1"
    );
    // A page of LinkedIn that is not a profile, and the sign-in wall.
    assert!(parse("https://www.linkedin.com/company/ada", &html).is_none());
    assert!(parse("https://www.linkedin.com/in/ada", "<html></html>").is_none());
}

#[test]
fn reads_the_answers_in_the_scripts_of_a_quora_page() {
    let rich = |text: &str| {
        json!({"sections": [{"type": "plain", "spans": [{"text": text}]}]}).to_string()
    };
    let answer = |question: u64, name: &str, text: &str| {
        json!({"content": rich(text), "author": {"names": [{"givenName": name, "familyName": ""}]},
            "question": {"qid": question}})
    };
    let list = json!({"sections": [
        {"type": "plain", "spans": [{"text": "Light "}, {"text": "scatters."}]},
        {"type": "unordered-list", "spans": [{"text": "Blue scatters most"}]},
        {"type": "image", "spans": [{"text": ""}]},
        {"type": "plain", "quoted": true, "spans": [{"text": "Rayleigh, 1871"}]}
    ]});
    let mut second = answer(7, "Bo", "");
    second["content"] = json!(list.to_string());
    let documents = [
        json!({"data": {"question": {"qid": 7, "title": rich("Why is the sky blue?"), "answerCount": 702,
            "answers": {"edges": [{"node": {"answer": answer(7, "Ann", "Because of the air.")}}]},
            "ads": [{"ad": {"answer": answer(9, "Seller", "Buy this.")}}]}}}),
        json!({"data": {"node": {"answer": second}}}),
    ];
    let literal = |document: &Value| serde_json::to_string(&document.to_string()).unwrap();
    let html = format!(
        "<script>results[\"a\"].push({});results[\"b\"].push({});list.push(1);</script>",
        literal(&documents[0]),
        literal(&documents[1])
    );
    let page = parse("https://www.quora.com/Why-is-the-sky-blue", &html).unwrap();
    assert_eq!(page.title, "Why is the sky blue?");
    assert_eq!(
        page.content,
        "The first 2 of 702 answers on Quora.\n\n## Answer by Ann\n\nBecause of the air.\n\n\
         ## Answer by Bo\n\nLight scatters.\n\n- Blue scatters most\n\n> Rayleigh, 1871"
    );
    assert!(parse("https://www.quora.com/x", "<html>Just a moment</html>").is_none());
    assert!(parse("https://example.org/x", &html).is_none());
}

#[test]
fn routes_imdb_titles_and_the_economist_home_page() {
    let route = |url: &str| route(&Url::parse(url).unwrap());
    assert_eq!(
        route("https://www.imdb.com/title/tt0111161/"),
        Some(Route::Imdb("tt0111161".into()))
    );
    assert_eq!(route("https://www.imdb.com/title/tt01x/"), None);
    assert_eq!(
        route("https://www.economist.com/"),
        Some(Route::Feed("https://www.economist.com/latest/rss.xml"))
    );
    assert_eq!(
        route("https://www.economist.com/leaders/2026/10/11/a-title"),
        None
    );
    assert_eq!(
        same_page("https://www.amazon.com/").as_deref(),
        Some("https://www.amazon.com/ref=nav_logo")
    );
    assert_eq!(
        same_page("http://linkedin.com/in/someone/?trk=x").as_deref(),
        Some("https://www.linkedin.com/in/someone?trk=x")
    );
    assert_eq!(same_page("https://uk.linkedin.com/in/someone"), None);
    assert_eq!(same_page("https://www.amazon.com/dp/B000"), None);
    assert_eq!(same_page("https://example.org/"), None);
}

#[test]
fn finds_the_video_id_of_a_youtube_address() {
    let id = |url: &str| youtube_id(&Url::parse(url).unwrap());
    let some = Some("jNQXAC9IVRw".to_owned());
    for url in [
        "https://www.youtube.com/watch?v=jNQXAC9IVRw",
        "https://www.youtube.com/watch?feature=share&v=jNQXAC9IVRw&t=5",
        "https://m.youtube.com/watch?v=jNQXAC9IVRw",
        "https://music.youtube.com/watch?v=jNQXAC9IVRw",
        "https://youtu.be/jNQXAC9IVRw?si=x",
        "https://www.youtube.com/shorts/jNQXAC9IVRw",
    ] {
        assert_eq!(id(url), some, "{url}");
    }
    for url in [
        "https://www.youtube.com/",
        "https://www.youtube.com/watch",
        "https://www.youtube.com/watch?v=short",
        "https://www.youtube.com/watch?v=--help&&&&&&",
        "https://www.youtube.com/playlist?list=PL1",
        "https://example.org/watch?v=jNQXAC9IVRw",
    ] {
        assert_eq!(id(url), None, "{url}");
    }
    assert!(is_youtube("https://youtu.be/jNQXAC9IVRw"));
    assert!(!is_youtube("not a url"));
}

#[test]
fn turns_captions_into_timed_lines() {
    let data = json!({"events": [
        {"tStartMs": 0, "dDurationMs": 100},
        {"tStartMs": 1200, "segs": [{"utf8": "All right, so here we are,"}, {"utf8": " in front\nof the"}]},
        {"tStartMs": 5000, "segs": [{"utf8": "\n"}]},
        {"tStartMs": 8000, "segs": [{"utf8": "elephants"}]},
        {"tStartMs": 21200, "segs": [{"utf8": "Next line"}]},
        {"tStartMs": 3_725_000, "segs": [{"utf8": "Much later"}]}
    ]});
    let found = cues(&data);
    assert_eq!(found.len(), 4);
    assert_eq!(found[0].text, "All right, so here we are, in front of the");
    let lines = merge(found);
    let lines: Vec<_> = lines
        .iter()
        .map(|line| (line.start_ms, &*line.text))
        .collect();
    assert_eq!(
        lines,
        [
            (1200, "All right, so here we are, in front of the elephants"),
            (21200, "Next line"),
            (3_725_000, "Much later")
        ]
    );
    assert_eq!(clock(65_000), "01:05");
    assert_eq!(clock(3_725_000), "1:02:05");
    let video = Video {
        title: "Me at the zoo".into(),
        channel: "jawed".into(),
        seconds: 19,
    };
    let page = transcript_page(&video, merge(cues(&data))).unwrap();
    assert!(
        page.content
            .starts_with("# Me at the zoo\n\njawed, 00:19\n\n[00:01] All right")
    );
    assert!(page.content.ends_with("[1:02:05] Much later"));
    let error = transcript_page(&video, Vec::new()).err().unwrap();
    assert_eq!(error.code, "transcript_unavailable");
}

#[test]
fn renders_a_reddit_thread_and_a_listing_from_their_data() {
    let post = json!({"subreddit": "rust", "author": "ann", "created_utc": 1791676800.0, "score": 12,
        "num_comments": 2, "is_self": true, "title": "Why is it slow?", "selftext": "My loop is slow.",
        "permalink": "/r/rust/comments/1/why/", "url": "https://www.reddit.com/r/rust/comments/1/why/"});
    let reply = json!({"kind": "Listing", "data": {"children": [
        {"kind": "t1", "data": {"author": "cy", "score": 3, "body": "Thanks.", "replies": ""}}]}});
    let comments = json!({"kind": "Listing", "data": {"children": [
        {"kind": "t1", "data": {"author": "bo", "score": 7, "body": "Build with --release.", "replies": reply}},
        {"kind": "more", "data": {"count": 4}}]}});
    let listing =
        json!({"kind": "Listing", "data": {"children": [{"kind": "t3", "data": post}]}});
    let page = reddit_page(&json!([listing, comments])).unwrap();
    assert_eq!(page.title, "Why is it slow?");
    assert_eq!(
        page.content,
        "r/rust, u/ann, 2026-10-11, score 12, 2 comments\n\nMy loop is slow.\n\n\
         ## u/bo, score 7\n\nBuild with --release.\n\n### u/cy, score 3\n\nThanks.\n\n\
         (4 more comments are not shown.)"
    );
    let page = reddit_page(&listing).unwrap();
    assert_eq!(page.title, "r/rust");
    assert_eq!(
        page.content,
        "## Why is it slow?\n\nhttps://www.reddit.com/r/rust/comments/1/why/\n\
         r/rust, u/ann, 2026-10-11, score 12, 2 comments\n\nMy loop is slow."
    );
    assert!(reddit_page(&json!({"error": 403})).is_none());
}

#[test]
fn renders_an_imdb_title_and_an_rss_feed() {
    let data = json!({"data": {"title": {"titleText": {"text": "The Film"}, "titleType": {"text": "Movie"},
        "releaseYear": {"year": 1994}, "runtime": {"seconds": 8520}, "certificate": {"rating": "R"},
        "ratingsSummary": {"aggregateRating": 9.3, "voteCount": 3251463},
        "metacritic": {"metascore": {"score": 82}}, "genres": {"genres": [{"text": "Drama"}]},
        "plots": {"edges": [{"node": {"plotText": {"plainText": "Short."}}},
            {"node": {"plotText": {"plainText": "A banker goes to prison."}}}]},
        "credits": {"edges": [
            {"node": {"category": {"text": "Director"}, "name": {"nameText": {"text": "F. D."}}}},
            {"node": {"category": {"text": "Actor"}, "name": {"nameText": {"text": "T. R."}},
                "characters": [{"name": "Andy"}]}}]}}}});
    let page = imdb_page(&data).unwrap();
    assert_eq!(page.title, "The Film (1994)");
    assert_eq!(
        page.content,
        "Movie, 1994, 142 minutes, rated R, genres: Drama, IMDb rating 9.3, 3251463 votes, \
         Metascore 82.\n\nA banker goes to prison.\n\n\
         ## Credits\n\n- Director: F. D.\n- Actor: T. R. (Andy)"
    );
    let unreleased = json!({"data": {"title": {"titleText": {"text": "Next"}, "titleType": {"text": "Movie"}}}});
    let page = imdb_page(&unreleased).unwrap();
    assert_eq!(page.title, "Next");
    assert!(page.content.starts_with("Movie.\n\n"));
    assert!(imdb_page(&json!({"data": {"title": null}})).is_none());

    let feed = "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title><![CDATA[Latest]]></title>\
        <item><title><![CDATA[Oil & gas]]></title><description><![CDATA[Prices <b>rise</b>]]></description>\
        <link>https://example.org/oil</link><guid>1</guid><pubDate>Sun, 11 Oct 2026 16:33:11 +0000</pubDate></item>\
        </channel></rss>";
    let page = feed_page(feed).unwrap();
    assert_eq!(page.title, "Latest");
    assert_eq!(
        page.content,
        "## Oil & gas\n\nhttps://example.org/oil\nSun, 11 Oct 2026 16:33:11 +0000\n\nPrices rise"
    );
}
