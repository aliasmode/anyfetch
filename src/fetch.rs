use crate::{ToolError, alternates, cache, focus, pdf, render, transport};
use dom_query::{Document, NodeRef};
use dom_smoothie::Readability;
use encoding_rs::{Encoding, UTF_8};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// Content characters returned when the caller does not ask for a number. Larger answers
/// overflow the tool-output limit of common agent harnesses.
pub const DEFAULT_MAX_CHARS: usize = 50_000;

/// Content characters returned for a query when the caller does not ask for a number: enough
/// for the matching parts of a page, and a tenth of a whole page.
pub const DEFAULT_QUERY_CHARS: usize = 5_000;

#[derive(Debug, Serialize)]
pub struct Extracted {
    pub title: String,
    pub description: Option<String>,
    pub content: String,
    /// The page gave no article and no data. `content` is its description, or what the page
    /// shows around its scripts.
    pub summary_only: bool,
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
    /// `content` holds only the parts of the page that match the query.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
}

impl FetchResponse {
    /// Keeps at most `max_chars` characters of content, starting at character `offset`.
    pub fn window(mut self, offset: usize, max_chars: usize) -> Self {
        self.content = self.content.chars().skip(offset).take(max_chars).collect();
        self
    }

    /// Keeps the parts of the content that match `query`, at most `max_chars` characters. A
    /// page with no match keeps its first `max_chars`, and so does data such as JSON, which is
    /// of no use in parts.
    pub fn focus(mut self, query: &str, max_chars: usize) -> Self {
        let kind = &self.content_type;
        let data = (kind.contains("json") || kind.contains("xml")) && !kind.contains("html");
        match focus::focus(&self.content, query, max_chars).filter(|_| !data) {
            Some(content) => {
                self.content = content;
                self.focused = true;
                self
            }
            None => self.window(0, max_chars),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Options<'a> {
    /// A Firefox-based browser that passes Cloudflare checks which plain requests cannot. See
    /// [`crate::clearance::browser`].
    pub browser: Option<&'a Path>,
    /// A program that runs a page's scripts and prints the finished HTML, for pages that hold
    /// no text without them. See [`crate::render::renderer`].
    pub renderer: Option<&'a Path>,
    /// Keep a copy of each public page for 15 minutes and answer from it.
    pub cache: bool,
    /// Fetch the page again and replace the copy.
    pub fresh: bool,
}

/// The helper programs found on this machine.
pub struct Helpers {
    browser: Option<PathBuf>,
    renderer: Option<PathBuf>,
}

impl Helpers {
    pub fn find() -> Self {
        Self {
            browser: crate::clearance::browser(),
            renderer: render::renderer(),
        }
    }

    /// Options for a fetch that uses the helpers and the page cache.
    pub fn options(&self, fresh: bool) -> Options<'_> {
        Options {
            browser: self.browser.as_deref(),
            renderer: self.renderer.as_deref(),
            cache: true,
            fresh,
        }
    }
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
    let page = download(url, options).await?;
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
/// Without `max_chars`, the pages share [`DEFAULT_MAX_CHARS`]. With a `query`, each page keeps
/// only the parts that match it, [`DEFAULT_QUERY_CHARS`] without `max_chars`.
pub fn pages(
    urls: &[String],
    results: Vec<Result<FetchResponse, ToolError>>,
    max_chars: Option<usize>,
    query: Option<&str>,
) -> serde_json::Value {
    let share = DEFAULT_MAX_CHARS / urls.len().max(1);
    let entries: Vec<_> = urls
        .iter()
        .zip(results)
        .map(|(url, result)| match (result, query) {
            (Ok(page), Some(query)) => {
                serde_json::json!(page.focus(query, max_chars.unwrap_or(DEFAULT_QUERY_CHARS)))
            }
            (Ok(page), None) => serde_json::json!(page.window(0, max_chars.unwrap_or(share))),
            (Err(error), _) => serde_json::json!({ "requested_url": url, "error": error }),
        })
        .collect();
    serde_json::json!({ "pages": entries })
}

async fn download(url: &str, options: Options<'_>) -> Result<FetchResponse, ToolError> {
    if let Ok(parsed) = url::Url::parse(url)
        && let Some(page) = alternates::read(&parsed).await
    {
        return Ok(FetchResponse {
            requested_url: url.to_owned(),
            url: page.source.unwrap_or_else(|| url.to_owned()),
            status: 200,
            content_type: "text/markdown".to_owned(),
            retrieved_at: crate::retrieved_at(),
            title: Some(page.title),
            description: None,
            total_chars: page.content.chars().count(),
            content: page.content,
            focused: false,
        });
    }
    let address = alternates::same_page(url).unwrap_or(url);
    let response = transport::get_page(address, supported, options.browser).await?;
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
            let own = alternates::parse(&final_url, &body).map(|page| Extracted {
                title: page.title,
                description: None,
                content: page.content,
                summary_only: false,
            });
            let mut extracted = own.map_or_else(|| extract(&body, &final_url), Ok);
            // A page with no article and no data needs its scripts. Run them when a renderer
            // is installed; it makes its own request, so not behind a proxy.
            let shell = extracted.as_ref().map_or(true, |page| page.summary_only);
            if shell
                && let Some(renderer) = options.renderer
                && transport::direct()
                && let Some(html) = render::render(renderer, &final_url).await
                && let Some(page) = extract_rendered(&html, &final_url)
                && page.content.len() > extracted.as_ref().map_or(0, |plain| plain.content.len())
            {
                extracted = Ok(page);
            }
            let extracted = extracted?;
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
        focused: false,
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
    let (embedded, bodies) = embedded_text(&document);
    plain_code_blocks(&document);
    // Readability takes the document apart, so the page view gets a copy.
    let page = document.clone();
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
    let mut summary_only = content.trim().is_empty();
    if summary_only {
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
    // A page can show the start of an article and hold all of it in its data. A rich-text body
    // is that article when it has the words that the page shows.
    let body = bodies.iter().max_by_key(|body| body.len());
    let body = body
        .filter(|body| body.len() > content.len())
        .filter(|body| {
            let inside = words(body);
            shown.iter().filter(|word| inside.contains(*word)).count() * 2 >= shown.len()
        });
    if let Some(body) = body {
        return Ok(Extracted {
            title,
            description,
            content: body.clone(),
            summary_only,
        });
    }
    let unseen: Vec<&str> = embedded
        .iter()
        .chain(&bodies)
        .map(String::as_str)
        .filter(|text| {
            let words = words(text);
            let known = words.iter().filter(|word| shown.contains(*word)).count();
            known * 2 <= words.len() && !content.contains(text)
        })
        .collect();
    if unseen.iter().map(|text| text.len()).sum::<usize>() > content.len() {
        summary_only = false;
        content = [content.trim_end()]
            .into_iter()
            .chain(unseen)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    // A home page or a listing has many headlines and no single article, so Readability keeps
    // a small part of it or nothing. When the page shows over five times more, and a third
    // of its lines or more are links, return what it shows. Measured on saved pages: home
    // pages have 35% to 97% link lines, articles 8% to 26%. A page shows less than its HTML
    // holds, so a long article skips this check.
    if html.len() > content.len() * 5 {
        let whole = page_view(&page, url);
        let lines = || whole.lines().filter(|line| !line.trim().is_empty());
        let links = lines().filter(|line| line.contains("](")).count();
        if whole.len() > content.len() * 5 && links * 3 >= lines().count() {
            content = whole;
        }
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
        summary_only,
    })
}

/// Content of a page after its scripts ran. An article is extracted as usual. An application
/// page (a feed, a listing, a dashboard) has no article, so the whole page is converted when
/// the article view would lose most of it.
fn extract_rendered(html: &str, url: &str) -> Option<Extracted> {
    let document = Document::from(html);
    let description = description(&document);
    let title = document.select("title").text().trim().to_owned();
    plain_code_blocks(&document);
    document
        .select("script, style, noscript, template, svg")
        .remove();
    document.select("head").remove();
    let whole = document.md(None).trim().to_owned();
    match extract(html, url) {
        Ok(page) if !page.summary_only && page.content.len() * 2 >= whole.len() => Some(page),
        _ if whole.is_empty() => None,
        _ => Some(Extracted {
            title,
            description,
            content: whole,
            summary_only: false,
        }),
    }
}

/// What a home page or a listing shows, as Markdown: its headings, text and links in page
/// order, without scripts, styles, images, controls and navigation. Links are absolute.
fn page_view(document: &Document, url: &str) -> String {
    // React sends the late parts of a page as hidden elements with an id such as "S:1", and a
    // script shows them. They are content; all other hidden elements are not shown.
    document
        .select(
            "head, script, style, noscript, template, dialog, [hidden]:not([id^='S:']), \
             [style*='display:none'], [style*='display: none'], svg, img, button, select, \
             textarea, nav, aside, footer, [role=navigation], [role=complementary], \
             [role=contentinfo], [role=banner], \
             header:not(article header, section header, main header)",
        )
        .remove();
    let base = url::Url::parse(url).ok();
    for link in document.select("a[href]").nodes() {
        // The words of a link, without the elements that the page puts around each part.
        let mut label = String::new();
        for part in link.descendants_it().filter(NodeRef::is_text) {
            label.extend(part.text().split_whitespace().flat_map(|word| [word, " "]));
        }
        link.set_text(label.trim_end());
        let target = link.attr("href").zip(base.as_ref());
        if let Some(target) = target.and_then(|(href, base)| base.join(&href).ok()) {
            link.set_attr("href", target.as_str());
        }
        // Links that touch get a line each.
        if link.next_sibling().is_some_and(|next| next.has_name("a")) {
            link.after_html("<br>");
        }
    }
    // A list item or a spot that scripts fill later leaves a line with no words, and a page
    // can show one link in several places. Drop those lines, but keep every line of code.
    let markdown = document.md(None);
    let (mut view, mut code, mut seen) = (String::new(), false, HashSet::new());
    for line in markdown.lines() {
        let fence = line.trim_start().starts_with("```");
        let words = line.chars().any(char::is_alphanumeric);
        let repeated = line.contains("](") && !seen.insert(line);
        if code || fence || (words && !repeated) {
            view.push_str(line);
            view.push('\n');
        } else if !view.ends_with("\n\n") {
            view.push('\n');
        }
        code ^= fence;
    }
    view.trim().to_owned()
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
/// passes to a call, or holds in an object. Each text appears once, in page order. The second
/// list holds article bodies that the data stores as rich text.
fn embedded_text(document: &Document) -> (Vec<String>, Vec<String>) {
    let mut found = Vec::new();
    let mut bodies = Vec::new();
    for script in document.select("script").iter() {
        let source = script.text();
        let kind = script.attr("type").unwrap_or_default().to_ascii_lowercase();
        if kind.ends_with("json") {
            if let Ok(data) = serde_json::from_str(&source) {
                prose(&data, "", &mut found);
                rich_text(&data, &mut bodies);
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
                    rich_text(&data, &mut bodies);
                    rest = &rest[at + values.byte_offset()..];
                }
                _ => rest = &rest[at + 2..],
            }
        }
    }
    let mut seen = HashSet::new();
    found.retain(|text| seen.insert(text.clone()));
    (found, bodies)
}

/// Article bodies stored as rich text: a node of type "document" whose content is a list of
/// blocks, with the words in nodes of type "text". Each body is its blocks in order.
fn rich_text(data: &Value, found: &mut Vec<String>) {
    let kind = |node: &Value| node["type"].as_str().unwrap_or_default().to_owned();
    match data {
        Value::Object(fields) => {
            let Some(blocks) = data["content"]
                .as_array()
                .filter(|_| kind(data) == "document")
            else {
                return fields.values().for_each(|value| rich_text(value, found));
            };
            let mut lines = Vec::new();
            for block in blocks {
                // A list has one line for each item.
                let items = block["content"]
                    .as_array()
                    .filter(|_| kind(block).contains("list"));
                for (part, mark) in match items {
                    Some(items) => items.iter().map(|item| (item, "- ")).collect(),
                    None if kind(block).starts_with("heading") => vec![(block, "## ")],
                    None => vec![(block, "")],
                } {
                    let mut line = String::new();
                    inline(part, &mut line);
                    if !line.trim().is_empty() {
                        lines.push(format!("{mark}{}", line.trim()));
                    }
                }
            }
            found.push(lines.join("\n\n"));
        }
        Value::Array(items) => items.iter().for_each(|item| rich_text(item, found)),
        _ => {}
    }
}

/// Appends the words of every "text" node under `node`.
fn inline(node: &Value, line: &mut String) {
    match node {
        Value::Object(fields) => match (node["type"].as_str(), node["value"].as_str()) {
            (Some("text"), Some(words)) => line.push_str(words),
            _ => fields.values().for_each(|value| inline(value, line)),
        },
        Value::Array(items) => items.iter().for_each(|item| inline(item, line)),
        _ => {}
    }
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
