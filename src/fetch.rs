use crate::{ToolError, transport};
use dom_query::Document;
use dom_smoothie::Readability;
use encoding_rs::{Encoding, UTF_8};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Extracted {
    pub title: String,
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
    pub content: String,
}

pub async fn fetch(url: &str) -> Result<FetchResponse, ToolError> {
    let response = transport::get(url, supported).await?;
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
    let (title, content) = if is_html {
        let extracted = extract(&body, &final_url)?;
        (Some(extracted.title), extracted.content)
    } else {
        (None, body.into_owned())
    };
    Ok(FetchResponse {
        requested_url: url.to_owned(),
        url: final_url,
        status,
        content_type,
        retrieved_at: crate::retrieved_at(),
        title,
        content,
    })
}

fn supported(media_type: &str) -> bool {
    media_type.starts_with("text/")
        || matches!(media_type, "application/xhtml+xml" | "application/json")
        || media_type.ends_with("+json")
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
    let mut readability = Readability::new(html, Some(url), None)
        .map_err(|error| ToolError::new("extraction_failed", error.to_string()))?;
    let article = readability
        .parse()
        .map_err(|error| ToolError::new("extraction_failed", error.to_string()))?;
    let document = Document::from(article.content.as_ref());
    let content = document.md(None).to_string();
    if content.trim().is_empty() {
        return Err(ToolError::new(
            "extraction_failed",
            "No readable content was extracted",
        ));
    }
    Ok(Extracted {
        title: article.title,
        content,
    })
}
