//! Public APIs and feeds of sites whose own pages need JavaScript or refuse plain requests.
//!
//! Each route returns the same content as the page, from an endpoint the site publishes for
//! programs. A route that fails is skipped and the page itself is fetched as usual.
//!
//! [`parse`] is for a page that arrives, but holds its content where the general extractor
//! cannot read it.

use crate::transport;
use dom_query::Document;
use serde_json::Value;
use url::Url;

pub(crate) struct Page {
    pub title: String,
    pub content: String,
}

#[derive(Debug, PartialEq)]
enum Route {
    /// A question on a Stack Exchange site: (API site name, question id).
    Question(String, String),
    Tweet(String),
    PyPi(String),
    Crate(String),
    /// The Atom feed of a Reddit listing or comment thread.
    Reddit(String),
    /// The data of a Reuters article page.
    Reuters(String),
}

fn route(url: &Url) -> Option<Route> {
    let host = url.host_str()?.trim_start_matches("www.");
    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    let number = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    match (host, segments.as_slice()) {
        ("x.com" | "twitter.com", [_, "status", id, ..]) if number(id) => {
            Some(Route::Tweet(id.to_string()))
        }
        ("pypi.org", ["project", name]) => Some(Route::PyPi(name.to_string())),
        ("crates.io", ["crates", name]) => Some(Route::Crate(name.to_string())),
        // A subreddit listing or a comment thread; other Reddit pages have no such feed.
        ("reddit.com" | "old.reddit.com", ["r", _] | ["r", _, "comments", ..]) => {
            let path = url.path().trim_end_matches('/');
            Some(Route::Reddit(format!("https://www.reddit.com{path}/.rss")))
        }
        // Reuters answers an article page with a bot check, but gives the data of that page.
        ("reuters.com", [.., slug]) if dated(slug) => {
            let path = url.path();
            Some(Route::Reuters(format!(
                "https://www.reuters.com{path}?outputType=json"
            )))
        }
        (_, ["questions", id, ..]) if number(id) => {
            let site = match host {
                "stackoverflow.com" | "superuser.com" | "serverfault.com" | "askubuntu.com"
                | "mathoverflow.net" => host.split('.').next()?,
                _ => host.strip_suffix(".stackexchange.com")?,
            };
            Some(Route::Question(site.to_owned(), id.to_string()))
        }
        _ => None,
    }
}

/// Reuters ends the address of an article with its date, such as `-2026-10-06`.
fn dated(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    let date = bytes.len().checked_sub(11).map(|start| &bytes[start..]);
    date.is_some_and(|date| {
        let fits = |(index, byte): (usize, &u8)| match index {
            0 | 5 | 8 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        };
        date.iter().enumerate().all(fits)
    })
}

/// Reads the content of `url` from the site's public API or feed, if the site has one.
pub(crate) async fn read(url: &Url) -> Option<Page> {
    match route(url)? {
        Route::Question(site, id) => {
            let api = format!("https://api.stackexchange.com/2.3/questions/{id}");
            let question = json(&format!("{api}?site={site}&filter=withbody")).await?;
            let query = "filter=withbody&sort=votes&order=desc&pagesize=10";
            let answers = json(&format!("{api}/answers?site={site}&{query}")).await;
            question_page(&question, &answers.unwrap_or_default())
        }
        Route::Tweet(id) => {
            let api = "https://cdn.syndication.twimg.com/tweet-result";
            tweet_page(&json(&format!("{api}?id={id}&token=a")).await?)
        }
        Route::PyPi(name) => {
            let package = json(&format!("https://pypi.org/pypi/{name}/json")).await?;
            let info = &package["info"];
            Some(Page {
                title: format!("{} {}", info["name"].as_str()?, text(&info["version"])),
                content: [text(&info["summary"]), text(&info["description"])].join("\n\n"),
            })
        }
        Route::Crate(name) => {
            let api = format!("https://crates.io/api/v1/crates/{name}");
            let package = json(&api).await?;
            let package = &package["crate"];
            let version = package["max_stable_version"].as_str()?;
            let readme = transport::get(&format!("{api}/{version}/readme"), |_| true).await;
            let readme = readme
                .map(|body| markdown(&body.text()))
                .unwrap_or_default();
            let links = ["repository", "documentation"].map(|key| text(&package[key]));
            let summary = text(&package["description"]);
            Some(Page {
                title: format!("{} {version}", package["name"].as_str()?),
                content: [summary.as_str(), &links.join("\n"), &readme].join("\n\n"),
            })
        }
        Route::Reddit(feed) => {
            let body = transport::get(&feed, |_| true).await.ok()?;
            feed_page(&body.text())
        }
        Route::Reuters(data) => reuters_page(&json(&data).await?),
    }
}

async fn json(url: &str) -> Option<Value> {
    let body = transport::get(url, |_| true).await.ok()?;
    serde_json::from_slice(&body.bytes).ok()
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.trim().to_owned(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn markdown(html: &str) -> String {
    Document::from(html).md(None).trim().to_owned()
}

/// The text of an HTML fragment, without its tags.
fn plain(html: &str) -> String {
    let text = Document::from(html).select("body").text();
    // Reuters puts zero-width characters inside words.
    text.replace(['\u{200b}', '\u{200c}', '\u{200d}'], "")
        .trim()
        .to_owned()
}

/// A Reuters article from the data of its page: a list of blocks, one of which holds the article.
fn reuters_page(blocks: &Value) -> Option<Page> {
    let articles = blocks.as_array()?.iter();
    let article = articles
        .map(|block| &block["data"]["article"])
        .find(|article| article["content_elements"].is_array())?;
    let mut content = vec![format!("Reuters, {}.", text(&article["published_time"]))];
    for element in article["content_elements"].as_array()? {
        // Other elements are places for advertisements and newsletters.
        let body = element["content"].as_str().map(plain).unwrap_or_default();
        match element["type"].as_str() {
            Some("paragraph") if !body.is_empty() => content.push(body),
            Some("header") if !body.is_empty() => content.push(format!("## {body}")),
            _ => {}
        }
    }
    content.push(plain(&text(&article["sign_off"])));
    Some(Page {
        title: plain(article["title"].as_str()?),
        content: content.join("\n\n").trim_end().to_owned(),
    })
}

/// The content of a page that holds it where the general extractor cannot read it.
pub(crate) fn parse(url: &str, html: &str) -> Option<Page> {
    let url = Url::parse(url).ok()?;
    match url.host_str()?.trim_start_matches("www.") {
        "quora.com" => quora_page(html),
        _ => None,
    }
}

/// A Quora question page. Its scripts hold the question and the first answers as JSON
/// documents inside string literals: `.push("{...}")`.
fn quora_page(html: &str) -> Option<Page> {
    let mut asked = None;
    let mut count = None;
    let mut found = Vec::new();
    for literal in html.split(".push(").skip(1) {
        let mut literals = serde_json::Deserializer::from_str(literal).into_iter::<String>();
        let Some(Ok(document)) = literals.next() else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&document) else {
            continue;
        };
        let question = &data["data"]["question"];
        if asked.is_none()
            && let Some(title) = question["title"].as_str()
        {
            asked = Some((question["qid"].clone(), rich_text(title)));
        }
        count = count.or(question["answerCount"].as_u64());
        answers(&data, &mut found);
    }
    let (asked, title) = asked.filter(|(question, _)| !question.is_null())?;
    // Promoted answers belong to other questions.
    found.retain(|(question, _, _)| *question == asked);
    let mut sections: Vec<String> = Vec::new();
    for (_, author, answer) in found {
        let section = format!("## Answer by {author}\n\n{answer}");
        if !sections.contains(&section) {
            sections.push(section);
        }
    }
    let all = count.map_or_else(String::new, |count| format!(" of {count}"));
    (!sections.is_empty()).then(|| Page {
        title,
        content: format!(
            "The first {}{all} answers on Quora.\n\n{}",
            sections.len(),
            sections.join("\n\n")
        ),
    })
}

/// Collects (question id, author, text) for every answer in a document of a Quora page.
fn answers(data: &Value, found: &mut Vec<(Value, String, String)>) {
    match data {
        Value::Object(fields) => {
            if let Some(content) = fields.get("content").and_then(Value::as_str)
                && content.starts_with("{\"sections\"")
                && fields.contains_key("author")
            {
                let name = &data["author"]["names"][0];
                let author = [text(&name["givenName"]), text(&name["familyName"])].join(" ");
                let question = data["question"]["qid"].clone();
                found.push((question, author.trim().to_owned(), rich_text(content)));
            }
            fields.values().for_each(|value| answers(value, found));
        }
        Value::Array(items) => items.iter().for_each(|item| answers(item, found)),
        _ => {}
    }
}

/// Quora's rich text as Markdown: a JSON document of sections, each a list of text spans.
fn rich_text(document: &str) -> String {
    let document: Value = serde_json::from_str(document).unwrap_or_default();
    let sections = document["sections"].as_array().into_iter().flatten();
    let lines = sections.filter_map(|section| {
        let spans = section["spans"].as_array()?.iter();
        let line: String = spans.filter_map(|span| span["text"].as_str()).collect();
        let list = section["type"]
            .as_str()
            .is_some_and(|kind| kind.ends_with("-list"));
        let mark = match (list, section["quoted"] == true) {
            (true, _) => "- ",
            (false, true) => "> ",
            (false, false) => "",
        };
        (!line.trim().is_empty()).then(|| format!("{mark}{}", line.trim()))
    });
    lines.collect::<Vec<_>>().join("\n\n")
}

fn question_page(question: &Value, answers: &Value) -> Option<Page> {
    let question = question["items"].get(0)?;
    let tags: Vec<String> = question["tags"].as_array()?.iter().map(text).collect();
    let mut content = format!(
        "{}\n\nTags: {}. Score: {}.",
        markdown(question["body"].as_str()?),
        tags.join(", "),
        question["score"]
    );
    for answer in answers["items"].as_array().into_iter().flatten() {
        let accepted = if answer["is_accepted"] == true {
            ", accepted"
        } else {
            ""
        };
        content += &format!(
            "\n\n## Answer (score {}{accepted})\n\n{}",
            answer["score"],
            markdown(answer["body"].as_str().unwrap_or_default())
        );
    }
    Some(Page {
        // The API escapes the title as HTML.
        title: Document::from(question["title"].as_str()?)
            .select("body")
            .text()
            .to_string(),
        content,
    })
}

fn tweet_page(tweet: &Value) -> Option<Page> {
    let (name, handle) = (
        text(&tweet["user"]["name"]),
        text(&tweet["user"]["screen_name"]),
    );
    let mut content = format!(
        "{}\n\n{name} (@{handle}), {}. Likes: {}.",
        tweet["text"].as_str()?,
        text(&tweet["created_at"]),
        tweet["favorite_count"]
    );
    if let Some(quoted) = tweet["quoted_tweet"]["text"].as_str() {
        content += &format!("\n\nQuoted post:\n\n{quoted}");
    }
    Some(Page {
        title: format!("{name} (@{handle}) on X"),
        content,
    })
}

/// Renders an Atom feed as one section per entry.
fn feed_page(xml: &str) -> Option<Page> {
    let feed = Document::from(xml);
    let sections: Vec<String> = feed
        .select("entry")
        .iter()
        .map(|entry| {
            let field = |selector: &str| entry.select(selector).text().trim().to_owned();
            let link = entry.select("link").attr("href").unwrap_or_default();
            format!(
                "## {}\n\n{link}\n{}, {}\n\n{}",
                field("title"),
                field("author name"),
                field("updated"),
                markdown(&field("content"))
            )
        })
        .collect();
    // The feed's own title comes before the entries.
    let title = feed.select("title").iter().next();
    let title = title.map_or_else(String::new, |title| title.text().trim().to_owned());
    (!sections.is_empty()).then(|| Page {
        title,
        content: sections.join("\n\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
                    "https://www.reddit.com/r/rust/comments/abc/a_post/.rss".into(),
                )),
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
}
