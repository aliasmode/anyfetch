use crate::{ToolError, transport};
use dom_query::{Document, NodeRef};
use dom_smoothie::Readability;
use encoding_rs::{Encoding, UTF_8};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Extracted {
    pub title: String,
    pub description: Option<String>,
    pub content: String,
}

#[derive(Debug, Serialize)]
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
    pub fn window(mut self, offset: usize, max_chars: Option<usize>) -> Self {
        let kept = self.content.chars().skip(offset);
        self.content = kept.take(max_chars.unwrap_or(usize::MAX)).collect();
        self
    }
}

pub async fn fetch(url: &str) -> Result<FetchResponse, ToolError> {
    fetch_with(url, None).await
}

/// Like `fetch`. When `browser` is a Firefox-based browser, a Cloudflare challenge that plain
/// requests cannot pass is passed once in it. See [`crate::clearance::browser`].
pub async fn fetch_with(url: &str, browser: Option<&Path>) -> Result<FetchResponse, ToolError> {
    let response = transport::get_page(url, supported, browser).await?;
    let final_url = response.url;
    let status = response.status;
    let content_type = response.content_type;
    let mime = content_type.parse::<mime::Mime>().ok();
    let media_type = mime.as_ref().map_or("", mime::Mime::essence_str);
    let is_html = matches!(media_type, "text/html" | "application/xhtml+xml");
    let bytes = response.bytes;
    let encoding = Encoding::for_bom(&bytes)
        .map(|(encoding, _)| encoding)
        .or_else(|| mime.as_ref().and_then(charset))
        .or_else(|| if is_html { html_charset(&bytes) } else { None })
        .unwrap_or(UTF_8);
    let (body, _, _) = encoding.decode(&bytes);
    let (title, description, content) = if is_html {
        let extracted = extract(&body, &final_url)?;
        (
            Some(extracted.title),
            extracted.description,
            extracted.content,
        )
    } else {
        (None, None, body.into_owned())
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
        || matches!(media_type, "application/json" | "application/xml")
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
    plain_code_blocks(&document);
    let failed = |message: String| ToolError::new("extraction_failed", message);
    let article = Readability::with_document(document, Some(url), None)
        .and_then(|mut readability| readability.parse());
    let (title, content) = match article {
        Ok(article) => {
            let content = Document::from(article.content.as_ref()).md(None);
            (article.title, content.to_string())
        }
        // A script-rendered page has no article, but its summary still says what the page is.
        Err(_) if description.is_some() => (page_title, String::new()),
        Err(error) => return Err(failed(error.to_string())),
    };
    let content = match &description {
        _ if !content.trim().is_empty() => content,
        Some(description) => description.clone(),
        None => return Err(failed("No readable content was extracted".into())),
    };
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
