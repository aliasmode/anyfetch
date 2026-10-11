use crate::{ToolError, alternates, cache, pdf, transport};
use dom_query::{Document, NodeRef};
use dom_smoothie::Readability;
use encoding_rs::{Encoding, UTF_8};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashSet, path::Path};

/// Content characters returned when the caller does not ask for a number. Larger answers
/// overflow the tool-output limit of common agent harnesses.
pub const DEFAULT_MAX_CHARS: usize = 50_000;

#[derive(Debug, Serialize)]
pub struct Extracted {
    pub title: String,
    pub description: Option<String>,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FetchResponse {
    pub requested_url: String,
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub retrieved_at: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub total_chars: usize,
    pub content: String,
}

impl FetchResponse {
    /// Keeps at most `max_chars` characters of content, starting at character `offset`.
    pub fn window(mut self, offset: usize, max_chars: usize) -> Self {
        self.content = self.content.chars().skip(offset).take(max_chars).collect();
        self
    }
}

#[derive(Clone, Copy, Default)]
pub struct Options<'a> {
    /// A Firefox-based browser that passes Cloudflare checks which plain requests cannot. See
    /// [`crate::clearance::browser`].
    pub browser: Option<&'a Path>,
    /// Keep a copy of each public page for 15 minutes and answer from it.
    pub cache: bool,
    /// Fetch the page again and replace the copy.
    pub fresh: bool,
}

pub async fn fetch(url: &str) -> Result<FetchResponse, ToolError> {
    fetch_with(url, Options::default()).await
}

pub async fn fetch_with(url: &str, options: Options<'_>) -> Result<FetchResponse, ToolError> {
    let cache = options.cache && transport::public_site(url);
    if cache
        && !options.fresh
        && let Some(page) = cache::get(url)
    {
        return Ok(page);
    }
    let page = download(url, options.browser).await?;
    if cache {
        cache::put(&page);
    }
    Ok(page)
}

/// Fetches several pages at the same time. Results are in the order of `urls`.
pub async fn fetch_many(
    urls: &[String],
    options: Options<'_>,
) -> Vec<Result<FetchResponse, ToolError>> {
    futures_util::future::join_all(urls.iter().map(|url| fetch_with(url, options))).await
}

/// The JSON answer for several pages: each entry is the page, cut to `max_chars`, or its error.
/// Without `max_chars`, the pages share [`DEFAULT_MAX_CHARS`].
pub fn pages(
    urls: &[String],
    results: Vec<Result<FetchResponse, ToolError>>,
    max_chars: Option<usize>,
) -> serde_json::Value {
    let max_chars = max_chars.unwrap_or(DEFAULT_MAX_CHARS / urls.len().max(1));
    let entries: Vec<_> = urls
        .iter()
        .zip(results)
        .map(|(url, result)| match result {
            Ok(page) => serde_json::json!(page.window(0, max_chars)),
            Err(error) => serde_json::json!({ "requested_url": url, "error": error }),
        })
        .collect();
    serde_json::json!({ "pages": entries })
}

async fn download(url: &str, browser: Option<&Path>) -> Result<FetchResponse, ToolError> {
    if let Ok(parsed) = url::Url::parse(url)
        && let Some(page) = alternates::read(&parsed).await
    {
        return Ok(FetchResponse {
            requested_url: url.to_owned(),
            url: url.to_owned(),
            status: 200,
            content_type: "text/markdown".to_owned(),
            retrieved_at: crate::retrieved_at(),
            title: Some(page.title),
            description: None,
            total_chars: page.content.chars().count(),
            content: page.content,
        });
    }
    let response = transport::get_page(url, supported, browser).await?;
    let final_url = response.url;
    let status = response.status;
    let content_type = response.content_type;
    let mime = content_type.parse::<mime::Mime>().ok();
    let media_type = mime.as_ref().map_or("", mime::Mime::essence_str);
    let is_html = matches!(media_type, "text/html" | "application/xhtml+xml");
    let bytes = response.bytes;
    let (title, description, content) = if media_type == "application/pdf" {
        let parsed = tokio::task::spawn_blocking(move || pdf::read(&bytes)).await;
        let document = parsed
            .unwrap_or(Err("The PDF is damaged"))
            .map_err(|message| ToolError::new("extraction_failed", message))?;
        let pages = document.pages.iter().enumerate();
        let pages = pages.filter(|(_, text)| !text.is_empty());
        let content: Vec<String> = pages
            .map(|(index, text)| format!("[Page {}]\n\n{text}", index + 1))
            .collect();
        if content.is_empty() {
            return Err(ToolError::new(
                "extraction_failed",
                "The PDF has no text; it may be a scan",
            ));
        }
        (document.title, None, content.join("\n\n"))
    } else {
        let encoding = Encoding::for_bom(&bytes)
            .map(|(encoding, _)| encoding)
            .or_else(|| mime.as_ref().and_then(charset))
            .or_else(|| if is_html { html_charset(&bytes) } else { None })
            .unwrap_or(UTF_8);
        let (body, _, _) = encoding.decode(&bytes);
        if is_html {
            let extracted = extract(&body, &final_url)?;
            (
                Some(extracted.title),
                extracted.description,
                extracted.content,
            )
        } else {
            (None, None, body.into_owned())
        }
    };
    Ok(FetchResponse {
        requested_url: url.to_owned(),
        url: final_url,
        status,
        content_type,
        retrieved_at: crate::retrieved_at(),
        title,
        description,
        total_chars: content.chars().count(),
        content,
    })
}

fn supported(media_type: &str) -> bool {
    media_type.starts_with("text/")
        || matches!(
            media_type,
            "application/json" | "application/xml" | "application/pdf"
        )
        || media_type.ends_with("+json")
        || media_type.ends_with("+xml")
}

fn charset(mime: &mime::Mime) -> Option<&'static Encoding> {
    mime.get_param(mime::CHARSET)
        .and_then(|label| Encoding::for_label(label.as_str().as_bytes()))
}

fn html_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let html = String::from_utf8_lossy(bytes);
    let document = Document::from(html.as_ref());
    for meta in document.select("meta").iter() {
        if let Some(encoding) = meta
            .attr("charset")
            .and_then(|label| Encoding::for_label(label.as_bytes()))
        {
            return Some(encoding);
        }
        if meta
            .attr("http-equiv")
            .is_some_and(|value| value.eq_ignore_ascii_case("content-type"))
            && let Some(encoding) = meta
                .attr("content")
                .and_then(|value| value.parse::<mime::Mime>().ok())
                .as_ref()
                .and_then(charset)
        {
            return Some(encoding);
        }
    }
    None
}

pub fn extract(html: &str, url: &str) -> Result<Extracted, ToolError> {
    let document = Document::from(html);
    let description = description(&document);
    let page_title = document.select("title").text().trim().to_owned();
    let embedded = embedded_text(&document);
    plain_code_blocks(&document);
    let article = Readability::with_document(document, Some(url), None)
        .and_then(|mut readability| readability.parse());
    let (title, mut content) = match article {
        Ok(article) => {
            let content = Document::from(article.content.as_ref()).md(None);
            (article.title, content.to_string())
        }
        Err(_) => (page_title, String::new()),
    };
    // A script-rendered page has no article. Its summary still says what the page is, and its
    // embedded data holds the text that the scripts would show.
    if content.trim().is_empty() {
        content = description.clone().unwrap_or_default();
    }
    // Embedded data often repeats the article in another form, so a text counts as already
    // shown when most of its words are on the page.
    let words = |text: &str| -> HashSet<String> {
        let words = text.split(|letter: char| !letter.is_alphanumeric());
        let long = words.filter(|word| word.len() >= 4);
        long.map(str::to_lowercase).collect()
    };
    let shown = words(&content);
    let unseen: Vec<&str> = embedded
        .iter()
        .map(String::as_str)
        .filter(|text| {
            let words = words(text);
            let known = words.iter().filter(|word| shown.contains(*word)).count();
            known * 2 <= words.len() && !content.contains(text)
        })
        .collect();
    if unseen.iter().map(|text| text.len()).sum::<usize>() > content.len() {
        content = [content.trim_end()]
            .into_iter()
            .chain(unseen)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    if content.trim().is_empty() {
        return Err(ToolError::new(
            "extraction_failed",
            "No readable content was extracted",
        ));
    }
    Ok(Extracted {
        title,
        description,
        content,
    })
}

/// The page's own summary. Script-rendered pages often carry their only server-side text here.
fn description(document: &Document) -> Option<String> {
    ["meta[name=description]", "meta[property='og:description']"]
        .iter()
        .filter_map(|selector| document.select(selector).attr("content"))
        .map(|content| content.trim().to_owned())
        .find(|content| !content.is_empty())
}

/// Prose in the page's embedded data: JSON-LD, framework state, and JSON that a script assigns,
/// passes to a call, or holds in an object. Each text appears once, in page order.
fn embedded_text(document: &Document) -> Vec<String> {
    let mut found = Vec::new();
    for script in document.select("script").iter() {
        let source = script.text();
        let kind = script.attr("type").unwrap_or_default().to_ascii_lowercase();
        if kind.ends_with("json") {
            if let Ok(data) = serde_json::from_str(&source) {
                prose(&data, "", &mut found);
            }
            continue;
        }
        let mut rest = &*source;
        while let Some(at) = rest.find("{\"") {
            let assigned = rest[..at].trim_end().ends_with(['=', '(', ':']);
            let mut values = serde_json::Deserializer::from_str(&rest[at..]).into_iter();
            match values.next().filter(|_| assigned) {
                Some(Ok(data)) => {
                    prose(&data, "", &mut found);
                    rest = &rest[at + values.byte_offset()..];
                }
                _ => rest = &rest[at + 2..],
            }
        }
    }
    let mut seen = HashSet::new();
    found.retain(|text| seen.insert(text.clone()));
    found
}

/// Collects strings that sit under keys which name text for people, such as `description`.
fn prose(data: &Value, key: &str, found: &mut Vec<String>) {
    const TEXT: [&str; 15] = [
        "title",
        "name",
        "headline",
        "description",
        "shortdescription",
        "text",
        "simpletext",
        "body",
        "caption",
        "full_text",
        "bio",
        "summary",
        "articlebody",
        "content",
        "snippet",
    ];
    const NOT_CONTENT: [&str; 12] = [
        "i18n",
        "translations",
        "translation",
        "messages",
        "config",
        "experiments",
        "routes",
        "manifest",
        "ads",
        "tracking",
        "analytics",
        "locales",
    ];
    match data {
        Value::Object(fields) => {
            for (name, value) in fields {
                let name = name.to_ascii_lowercase();
                if !NOT_CONTENT.contains(&&*name) {
                    prose(value, &name, found);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| prose(item, key, found)),
        Value::String(text) if TEXT.contains(&key) => {
            let text = if text.contains('<') {
                Document::from(text.as_str())
                    .select("body")
                    .text()
                    .to_string()
            } else {
                text.clone()
            };
            let text = text.trim();
            // A label is short or has no spaces; a sentence has both length and spaces.
            if text.len() >= 15 && text.contains(' ') {
                found.push(text.to_owned());
            }
        }
        _ => {}
    }
}

/// Highlighters split code lines into elements without newlines, and Readability drops code
/// inside layout wrappers. Reduce each `<pre>` to plain text and lift it out of such wrappers.
fn plain_code_blocks(document: &Document) {
    for pre in document.select("pre").nodes() {
        let language = ["data-lang", "data-language", "language"]
            .iter()
            .find_map(|name| pre.attr(name))
            .map(|language| language.to_string())
            .or_else(|| {
                let class = pre
                    .descendants()
                    .iter()
                    .find_map(|node| node.attr("class"))?;
                let language = class
                    .split_whitespace()
                    .find_map(|token| token.strip_prefix("language-"))?;
                Some(language.to_owned())
            });
        let mut text = String::new();
        code_text(pre, &mut text, &mut false, false);
        let text = text.trim_end();
        pre.set_text(text);
        if let Some(language) = language {
            pre.set_attr("data-lang", &language);
        }
        while let Some(wrapper) = pre.parent().filter(|parent| {
            parent
                .node_name()
                .is_some_and(|name| matches!(&*name, "div" | "span"))
                && parent.text().trim() == text.trim_start()
        }) {
            wrapper.replace_with(pre);
        }
    }
}

fn code_text(node: &NodeRef, text: &mut String, pending: &mut bool, in_line: bool) {
    for child in node.children() {
        if child.is_text() {
            let value = child.text();
            if *pending && (in_line || !value.starts_with('\n')) {
                text.push('\n');
            }
            *pending = false;
            text.push_str(&value);
            continue;
        }
        let Some(name) = child.node_name() else {
            continue;
        };
        if &*name == "br" {
            text.push('\n');
            *pending = false;
            continue;
        }
        let line = matches!(&*name, "div" | "p")
            || child.has_attr("data-line")
            || child.attr("class").is_some_and(|class| {
                class
                    .split_whitespace()
                    .any(|token| token == "line" || token.ends_with("-line"))
            });
        let start = text.len();
        code_text(&child, text, pending, in_line || line);
        if line && text.len() == start {
            if *pending {
                text.push('\n');
            }
            *pending = true;
        } else if line {
            *pending = !text.ends_with('\n');
        }
    }
}
