//! Public APIs and feeds of sites whose own pages need JavaScript or refuse plain requests.
//!
//! Each route returns the same content as the page, from an endpoint the site publishes for
//! programs. A route that fails is skipped and the page itself is fetched as usual.

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
}
