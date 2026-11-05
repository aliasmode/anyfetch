//! Public APIs and feeds of sites whose own pages need JavaScript or refuse plain requests.
//!
//! Each route returns the same content as the page, from an endpoint the site publishes for
//! programs. A route that fails is skipped and the page itself is fetched as usual.
//!
//! [`parse`] is for a page that arrives, but holds its content where the general extractor
//! cannot read it.

use crate::{ToolError, transport};
use dom_query::Document;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use url::Url;

pub(crate) struct Page {
    pub title: String,
    pub content: String,
    /// The address that was read, when it holds another form of the page: a feed.
    pub source: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Route {
    /// A question on a Stack Exchange site: (API site name, question id).
    Question(String, String),
    Tweet(String),
    PyPi(String),
    Crate(String),
    /// A Reddit listing or comment thread: its path, and the time window of a sorted listing.
    Reddit(String, String),
    /// A title on IMDb, by its id.
    Imdb(String),
    /// A feed that holds what a home page shows.
    Feed(&'static str),
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
        (
            "reddit.com" | "old.reddit.com",
            ["r", _]
            | ["r", _, "comments", ..]
            | [
                "r",
                _,
                "hot" | "new" | "top" | "rising" | "controversial" | "best",
            ],
        ) => {
            // A sorted listing can name a time window, as in `/top/?t=week`.
            let mut window = url.query_pairs().filter(|(key, _)| key == "t");
            let window = window.next().map(|(_, value)| value);
            let window = window.filter(|value| value.bytes().all(|byte| byte.is_ascii_lowercase()));
            let window = window.map(|value| format!("&t={value}"));
            Some(Route::Reddit(
                url.path().trim_end_matches('/').to_owned(),
                window.unwrap_or_default(),
            ))
        }
        // IMDb answers every page with a bot check. Its pages get their data from this API.
        ("imdb.com" | "m.imdb.com", ["title", id, ..])
            if id.strip_prefix("tt").is_some_and(number) =>
        {
            Some(Route::Imdb(id.to_string()))
        }
        // The Economist answers its home page with a bot check, and its feed without one.
        ("economist.com", []) => Some(Route::Feed("https://www.economist.com/latest/rss.xml")),
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

/// The id of the video at a YouTube watch, short, shorts or youtu.be address.
fn youtube_id(url: &Url) -> Option<String> {
    let host = url.host_str()?.trim_start_matches("www.");
    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    let id = match (host, segments.as_slice()) {
        ("youtube.com" | "m.youtube.com" | "music.youtube.com", ["watch"]) => {
            let mut ids = url.query_pairs().filter(|(key, _)| key == "v");
            ids.next()?.1.into_owned()
        }
        ("youtu.be", [id, ..]) | ("youtube.com" | "m.youtube.com", ["shorts", id, ..]) => {
            id.to_string()
        }
        _ => return None,
    };
    let valid = id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
    valid.then_some(id)
}

pub(crate) fn is_youtube(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| youtube_id(&url).is_some())
}

/// The yt-dlp program on PATH, which YouTube transcripts require.
pub(crate) fn yt_dlp() -> Option<PathBuf> {
    let name = format!("yt-dlp{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut candidates = std::env::split_paths(&path).map(|folder| folder.join(&name));
    candidates.find(|candidate| candidate.is_file())
}

/// Like [`read`], but a YouTube video gives its transcript, or the reason it has none.
pub(crate) async fn read_page(
    url: &Url,
    lang: &str,
    yt_dlp: Option<&Path>,
) -> Result<Option<Page>, ToolError> {
    match youtube_id(url) {
        Some(id) => transcript(&id, lang, yt_dlp).await.map(Some),
        None => Ok(read(url).await),
    }
}

/// One caption cue, or one line of merged cues.
#[derive(Debug, PartialEq)]
struct Cue {
    start_ms: u64,
    text: String,
}

struct Video {
    title: String,
    channel: String,
    seconds: u64,
}

/// A transcript line covers cues that start within this time of its first cue.
const LINE_MS: u64 = 20_000;

/// Longest yt-dlp may take to read one video.
const YT_DLP_TIMEOUT: Duration = Duration::from_secs(60);

/// Most of yt-dlp's stderr that is kept.
const STDERR_CAP: usize = 4096;

/// A language code: 2-3 letters, optionally `-` and 2-8 letters or digits (en, pt-BR, zh-Hans).
fn valid_lang(lang: &str) -> bool {
    let (base, region) = match lang.split_once('-') {
        Some((base, region)) => (base, Some(region)),
        None => (lang, None),
    };
    !base.eq_ignore_ascii_case("all") // yt-dlp's "every language"
        && (2..=3).contains(&base.len())
        && base.bytes().all(|b| b.is_ascii_alphabetic())
        && region.is_none_or(|r| {
            (2..=8).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

/// The last non-empty line of yt-dlp's stderr, trimmed.
fn last_line(stderr: &str) -> Option<&str> {
    stderr.lines().map(str::trim).rfind(|line| !line.is_empty())
}

/// Removes the directory when dropped, also when the request is cancelled.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn transcript(id: &str, lang: &str, yt_dlp: Option<&Path>) -> Result<Page, ToolError> {
    let lang = if lang.is_empty() { "en" } else { lang };
    if !valid_lang(lang) {
        let message = "lang must be a language code such as en or pt-BR";
        return Err(ToolError::new("invalid_request", message));
    }
    let needs = "YouTube transcripts need yt-dlp on PATH";
    let Some(program) = yt_dlp else {
        return Err(ToolError::new(
            "transcript_unavailable",
            format!("{needs}; yt-dlp was not found"),
        ));
    };
    match with_yt_dlp(program, id, lang).await {
        Ok(Some((video, cues))) => transcript_page(&video, merge(cues)),
        Ok(None) => Err(ToolError::new(
            "transcript_unavailable",
            format!("{needs}; yt-dlp ran but could not read this video"),
        )),
        Err(message) => Err(ToolError::new("transcript_unavailable", message)),
    }
}

/// `Err` is yt-dlp's own failure message; `Ok(None)` is any other failure.
async fn with_yt_dlp(
    program: &Path,
    id: &str,
    lang: &str,
) -> Result<Option<(Video, Vec<Cue>)>, String> {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "anyfetch-yt-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    );
    let dir = TempDir(std::env::temp_dir().join(name));
    if tokio::fs::create_dir_all(&dir.0).await.is_err() {
        return Ok(None);
    }
    run_yt_dlp(program, id, lang, &dir.0).await
}

/// Has yt-dlp write the video's data and captions into `dir`, and reads them. The `-orig`
/// language is where YouTube keeps the automatic captions of some videos.
async fn run_yt_dlp(
    program: &Path,
    id: &str,
    lang: &str,
    dir: &Path,
) -> Result<Option<(Video, Vec<Cue>)>, String> {
    let languages = format!("{lang},{lang}-orig");
    let address = format!("https://www.youtube.com/watch?v={id}");
    let run = tokio::process::Command::new(program)
        .args([
            "--quiet",
            "--no-warnings",
            "--skip-download",
            "--no-playlist",
        ])
        .args(["--write-info-json", "--write-subs", "--write-auto-subs"])
        .args(["--sub-format", "json3", "--sub-langs", &languages, "-o"])
        .arg(dir.join("v"))
        .args(["--", &address])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(output)) = tokio::time::timeout(YT_DLP_TIMEOUT, run).await else {
        return Ok(None);
    };
    if !output.status.success() {
        let tail = &output.stderr[output.stderr.len().saturating_sub(STDERR_CAP)..];
        let stderr = String::from_utf8_lossy(tail);
        let reason = last_line(&stderr).unwrap_or("no error output");
        return Err(format!("yt-dlp failed ({}): {reason}", output.status));
    }
    Ok(read_output(lang, dir).await)
}

/// Reads what yt-dlp wrote into `dir`.
async fn read_output(lang: &str, dir: &Path) -> Option<(Video, Vec<Cue>)> {
    let bytes = tokio::fs::read(dir.join("v.info.json")).await.ok()?;
    let info: Value = serde_json::from_slice(&bytes).ok()?;
    let video = Video {
        title: info["title"].as_str()?.to_owned(),
        channel: text(if info["channel"].is_string() {
            &info["channel"]
        } else {
            &info["uploader"]
        }),
        seconds: info["duration"].as_f64().unwrap_or_default() as u64,
    };
    // Manual captions are written under the plain language, automatic ones under either.
    let files = [format!("v.{lang}.json3"), format!("v.{lang}-orig.json3")];
    let mut captions = None;
    for file in &files {
        if let Ok(bytes) = tokio::fs::read(dir.join(file)).await {
            captions = Some(bytes);
            break;
        }
    }
    let data = captions.and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Some((video, data.map(|data| cues(&data)).unwrap_or_default()))
}

/// The cues of a json3 caption file, with their text on one line.
fn cues(data: &Value) -> Vec<Cue> {
    let events = data["events"].as_array().into_iter().flatten();
    events
        .filter_map(|event| {
            let segs = event["segs"].as_array()?;
            let words: String = segs.iter().filter_map(|seg| seg["utf8"].as_str()).collect();
            let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
            (!words.is_empty()).then(|| Cue {
                start_ms: event["tStartMs"].as_u64().unwrap_or_default(),
                text: words,
            })
        })
        .collect()
}

/// Joins cues into lines of about [`LINE_MS`], so a line is not a few words.
fn merge(cues: Vec<Cue>) -> Vec<Cue> {
    let mut lines: Vec<Cue> = Vec::new();
    for cue in cues {
        match lines.last_mut() {
            Some(line) if cue.start_ms.saturating_sub(line.start_ms) < LINE_MS => {
                line.text.push(' ');
                line.text.push_str(&cue.text);
            }
            _ => lines.push(cue),
        }
    }
    lines
}

/// `mm:ss`, or `h:mm:ss` from one hour.
fn clock(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn transcript_page(video: &Video, lines: Vec<Cue>) -> Result<Page, ToolError> {
    if lines.is_empty() {
        let message = "The video has no captions in that language";
        return Err(ToolError::new("transcript_unavailable", message));
    }
    let duration = (video.seconds > 0).then(|| clock(video.seconds * 1000));
    let about = [
        Some(video.channel.clone()).filter(|name| !name.is_empty()),
        duration,
    ];
    let about = about.into_iter().flatten().collect::<Vec<_>>().join(", ");
    let lines = lines
        .iter()
        .map(|line| format!("[{}] {}", clock(line.start_ms), line.text));
    let content = [
        format!("# {}", video.title),
        about,
        lines.collect::<Vec<_>>().join("\n"),
    ];
    let content: Vec<_> = content.iter().filter(|part| !part.is_empty()).collect();
    Ok(Page {
        title: video.title.clone(),
        content: content
            .iter()
            .map(|part| part.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        source: None,
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
                source: None,
            })
        }
        Route::Crate(name) => {
            let api = format!("https://crates.io/api/v1/crates/{name}");
            let package = json(&api).await?;
            let package = &package["crate"];
            // A crate with only pre-releases has no stable version.
            let stable = package["max_stable_version"].as_str();
            let version = stable.or_else(|| package["max_version"].as_str())?;
            let readme = transport::get(&format!("{api}/{version}/readme"), |_| true).await;
            let readme = readme
                .map(|body| markdown(&body.text()))
                .unwrap_or_default();
            let links = ["repository", "documentation"].map(|key| text(&package[key]));
            let summary = text(&package["description"]);
            Some(Page {
                title: format!("{} {version}", package["name"].as_str()?),
                content: [summary.as_str(), &links.join("\n"), &readme].join("\n\n"),
                source: None,
            })
        }
        Route::Reddit(path, window) => {
            let page = format!("https://www.reddit.com{path}/");
            // The data answers only a visitor of the page. It has all comments and their scores.
            let data = format!("{page}.json?raw_json=1&limit=500{window}");
            let listing = transport::get_with(&data, &[], Some(&page)).await.ok();
            let listing = listing.and_then(|body| serde_json::from_slice(&body.bytes).ok());
            if let Some(page) = listing.as_ref().and_then(reddit_page) {
                return Some(page);
            }
            // The feed has less, and Reddit gives a new visitor one feed request in a while.
            let feed = format!("{page}.rss{}", window.replacen('&', "?", 1));
            let body = transport::get(&feed, |_| true).await.ok()?;
            feed_page(&body.text())
        }
        Route::Imdb(id) => {
            let query = format!(
                "query {{ title(id: \"{id}\") {{ titleText {{ text }} titleType {{ text }} \
                 releaseYear {{ year }} runtime {{ seconds }} certificate {{ rating }} \
                 ratingsSummary {{ aggregateRating voteCount }} metacritic {{ metascore {{ score }} }} \
                 genres {{ genres {{ text }} }} plots(first: 3) {{ edges {{ node {{ plotText {{ plainText }} }} }} }} \
                 credits(first: 15) {{ edges {{ node {{ category {{ text }} name {{ nameText {{ text }} }} \
                 ... on Cast {{ characters {{ name }} }} }} }} }} }} }}"
            );
            let api = "https://caching.graphql.imdb.com/";
            let api = Url::parse_with_params(api, [("query", query)]).ok()?;
            // The API reads a plus sign as a plus sign, not as a space.
            let api = api.as_str().replace('+', "%20");
            let headers = [
                ("accept", "application/json"),
                ("content-type", "application/json"),
                ("x-imdb-client-name", "imdb-web-next-localized"),
            ];
            let body = transport::get_with(&api, &headers, None).await.ok()?;
            imdb_page(&serde_json::from_slice(&body.bytes).ok()?)
        }
        Route::Feed(feed) => {
            let body = transport::get(feed, |_| true).await.ok()?;
            let page = feed_page(&body.text())?;
            Some(Page {
                source: Some(feed.to_owned()),
                ..page
            })
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
        source: None,
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
        source: None,
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
        source: None,
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
        source: None,
    })
}

/// Renders an Atom or RSS feed as one section per entry.
fn feed_page(xml: &str) -> Option<Page> {
    // The HTML parser drops a CDATA section, which RSS feeds put around each text.
    let feed = Document::from(xml.replace("<![CDATA[", "").replace("]]>", ""));
    let sections: Vec<String> = feed
        .select("entry, item")
        .iter()
        .map(|entry| {
            let field = |selector: &str| entry.select(selector).text().trim().to_owned();
            let link = entry.select("link");
            // Atom has the address in an attribute. RSS has it as text, which the HTML parser
            // puts after the element.
            let after = || link.nodes().first()?.next_sibling().map(|text| text.text());
            let link = link.attr("href").or_else(after).unwrap_or_default();
            let by = [field("author name"), field("updated, pubdate")];
            let by = by.iter().filter(|part| !part.is_empty());
            format!(
                "## {}\n\n{}\n{}\n\n{}",
                field("title"),
                link.trim(),
                by.map(String::as_str).collect::<Vec<_>>().join(", "),
                markdown(&field("content, description"))
            )
            .trim_end()
            .to_owned()
        })
        .collect();
    // The feed's own title comes before the entries.
    let title = feed.select("title").iter().next();
    let title = title.map_or_else(String::new, |title| title.text().trim().to_owned());
    (!sections.is_empty()).then(|| Page {
        title,
        content: sections.join("\n\n"),
        source: None,
    })
}

fn children(listing: &Value) -> impl Iterator<Item = &Value> {
    let children = listing["data"]["children"].as_array();
    children.into_iter().flatten().map(|child| &child["data"])
}

/// A Reddit thread or listing from its data.
fn reddit_page(data: &Value) -> Option<Page> {
    let about = |post: &Value| {
        let time = post["created_utc"].as_f64().unwrap_or_default() as i64;
        let day = time::OffsetDateTime::from_unix_timestamp(time).map(|time| time.date());
        format!(
            "r/{}, u/{}, {}, score {}, {} comments",
            text(&post["subreddit"]),
            text(&post["author"]),
            day.map(|day| day.to_string()).unwrap_or_default(),
            post["score"],
            post["num_comments"]
        )
    };
    let (title, parts) = if let Some([post, comments]) = data.as_array().map(Vec::as_slice) {
        let post = children(post).next()?;
        let link = (post["is_self"] != true).then(|| format!("Link: {}", text(&post["url"])));
        let mut parts = vec![
            about(post),
            link.unwrap_or_default(),
            text(&post["selftext"]),
        ];
        replies(comments, 0, &mut parts);
        (text(&post["title"]), parts)
    } else {
        let posts = children(data).filter(|post| post["title"].is_string());
        let parts = posts.map(|post| {
            format!(
                "## {}\n\nhttps://www.reddit.com{}\n{}\n\n{}",
                text(&post["title"]),
                text(&post["permalink"]),
                about(post),
                text(&post["selftext"])
            )
        });
        let community = text(&children(data).next()?["subreddit"]);
        (format!("r/{community}"), parts.collect())
    };
    let parts = parts.iter().map(|part| part.trim_end());
    Some(Page {
        title,
        content: parts
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        source: None,
    })
}

/// Adds the comments of `listing`. A reply is one heading level under the comment it answers.
fn replies(listing: &Value, depth: usize, parts: &mut Vec<String>) {
    for comment in children(listing) {
        // Reddit sends the first comments and the number of the others.
        if let Some(more) = comment["count"].as_u64().filter(|more| *more > 0) {
            parts.push(format!("({more} more comments are not shown.)"));
        }
        if !comment["body"].is_string() {
            continue;
        }
        let marks = "#".repeat((depth + 2).min(6));
        let (author, body) = (text(&comment["author"]), text(&comment["body"]));
        parts.push(format!(
            "{marks} u/{author}, score {}\n\n{body}",
            comment["score"]
        ));
        replies(&comment["replies"], depth + 1, parts);
    }
}

/// A title on IMDb from the answer of its data API.
fn imdb_page(data: &Value) -> Option<Page> {
    let title = &data["data"]["title"];
    let name = title["titleText"]["text"].as_str()?;
    let year = &title["releaseYear"]["year"];
    let list = |items: &Value| items.as_array().cloned().unwrap_or_default();
    let genres = list(&title["genres"]["genres"]);
    let genres: Vec<String> = genres.iter().map(|genre| text(&genre["text"])).collect();
    let plots = list(&title["plots"]["edges"]);
    let plots = plots
        .iter()
        .map(|plot| text(&plot["node"]["plotText"]["plainText"]));
    let credits = list(&title["credits"]["edges"]);
    let credits = credits.iter().map(|credit| {
        let credit = &credit["node"];
        let roles = list(&credit["characters"]);
        let roles: Vec<String> = roles.iter().map(|role| text(&role["name"])).collect();
        let role = if roles.is_empty() {
            String::new()
        } else {
            format!(" ({})", roles.join(", "))
        };
        format!(
            "- {}: {}{role}",
            text(&credit["category"]["text"]),
            text(&credit["name"]["nameText"]["text"])
        )
    });
    // A title that is not out yet has no year, no rating and no length.
    let known = |label: &str, value: &Value| {
        let value = text(value);
        (!value.is_empty()).then(|| label.replace("{}", &value))
    };
    let minutes = title["runtime"]["seconds"]
        .as_u64()
        .map(|seconds| Value::from(seconds / 60));
    let facts = [
        known("{}", &title["titleType"]["text"]),
        known("{}", year),
        known("{} minutes", &minutes.unwrap_or_default()),
        known("rated {}", &title["certificate"]["rating"]),
        known("genres: {}", &Value::from(genres.join(", "))),
        known(
            "IMDb rating {}",
            &title["ratingsSummary"]["aggregateRating"],
        ),
        known("{} votes", &title["ratingsSummary"]["voteCount"]),
        known("Metascore {}", &title["metacritic"]["metascore"]["score"]),
    ];
    let facts = facts.into_iter().flatten().collect::<Vec<_>>().join(", ");
    let plot = plots.max_by_key(String::len).unwrap_or_default();
    let credits = credits.collect::<Vec<_>>().join("\n");
    let year = known(" ({})", year).unwrap_or_default();
    Some(Page {
        title: format!("{name}{year}"),
        content: format!("{facts}.\n\n{plot}\n\n## Credits\n\n{credits}"),
        source: None,
    })
}

/// Another address of the same page, for a page whose usual address is behind a bot check.
pub(crate) fn same_page(url: &str) -> Option<&'static str> {
    let url = Url::parse(url).ok()?;
    // The check of Amazon covers only the address of its home page.
    let home = url.host_str()?.trim_start_matches("www.") == "amazon.com" && url.path() == "/";
    home.then_some("https://www.amazon.com/ref=nav_logo")
}

#[cfg(test)]
mod tests {
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
            same_page("https://www.amazon.com/"),
            Some("https://www.amazon.com/ref=nav_logo")
        );
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
}
