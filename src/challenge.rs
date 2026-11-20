//! Bot checks that ask a browser for one small, fixed action.
//!
//! Each check is matched in its one known form. A page with any other script is left alone.

use dom_query::Document;

fn hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A page whose script sets one cookie and loads the page again. Returns the cookie.
pub(crate) fn cookie(html: &str) -> Option<String> {
    let (_, rest) = html.split_once("document.cookie = \"js_challenge_value=")?;
    let (value, rest) = rest.split_once(';')?;
    (hex(value) && rest.contains("window.location.reload()"))
        .then(|| format!("js_challenge_value={value}"))
}

/// Reddit's check: a script doubles a string and sends it with the token of a hidden form.
/// Returns the query that the form sends.
pub(crate) fn doubled(html: &str) -> Option<String> {
    let (_, rest) = html.split_once("await(async e=>e+e)(\"")?;
    let (text, _) = rest.split_once('"')?;
    let form = Document::from(html);
    let token = form.select("input[name=jsc_token]").attr("value")?;
    (hex(text) && hex(&token))
        .then(|| format!("solution={text}{text}&js_challenge=1&jsc_token={token}&jsc_orig_r="))
}

/// A page that only tells the visitor that a script checks the browser. Sites send these two
/// with a normal status, so the status does not show that the page is missing.
pub(crate) fn waiting_page(html: &str) -> bool {
    let title = html
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"));
    let title = title.map_or("", |(title, _)| title.trim());
    matches!(title, "Pardon Our Interruption..." | "Client Challenge")
}

#[cfg(test)]
#[path = "challenge.test.rs"]
mod tests;
