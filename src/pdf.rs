//! Text of a PDF, page by page. Scanned PDFs have no text layer and yield nothing.

use pdf_extract::{Document, Object, PlainTextOutput};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub(crate) struct Pdf {
    pub title: Option<String>,
    /// The text of each page, in order.
    pub pages: Vec<String>,
}

/// The parser panics on some damaged files, so every call into it is contained.
pub(crate) fn read(bytes: &[u8]) -> Result<Pdf, &'static str> {
    catch_unwind(AssertUnwindSafe(|| parse(bytes))).unwrap_or(Err("The PDF is damaged"))
}

fn parse(bytes: &[u8]) -> Result<Pdf, &'static str> {
    let mut document = Document::load_mem(bytes).map_err(|_| "The PDF could not be parsed")?;
    if document.is_encrypted() {
        let unlocked = document.decrypt("");
        unlocked.map_err(|_| "The PDF needs a password")?;
    }
    let info = document.trailer.get(b"Info").ok();
    let info = info.and_then(|info| document.dereference(info).ok());
    let title = info
        .and_then(|(_, info)| info.as_dict().ok()?.get(b"Title").ok())
        .and_then(|title| match title {
            Object::String(bytes, _) => Some(decode(bytes)),
            _ => None,
        })
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty());
    let mut pages = Vec::new();
    for &number in document.get_pages().keys() {
        let mut text = String::new();
        let written = catch_unwind(AssertUnwindSafe(|| {
            pdf_extract::output_doc_page(&document, &mut PlainTextOutput::new(&mut text), number)
        }));
        // One bad page must not lose the whole document.
        if !matches!(written, Ok(Ok(()))) {
            text.clear();
        }
        pages.push(ligatures(text.trim()));
    }
    Ok(Pdf { title, pages })
}

/// A PDF text string is UTF-16 with a byte-order mark, UTF-8 with one, or a Latin-1 superset.
fn decode(bytes: &[u8]) -> String {
    if let Some(utf16) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let pairs = utf16.as_chunks::<2>().0.iter();
        let units: Vec<u16> = pairs.map(|pair| u16::from_be_bytes(*pair)).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(utf8) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(utf8).into_owned()
    } else {
        bytes.iter().map(|&byte| byte as char).collect()
    }
}

/// Typeset text joins letter pairs into one glyph. Searching and quoting need the letters.
fn ligatures(text: &str) -> String {
    [
        ("ﬀ", "ff"),
        ("ﬁ", "fi"),
        ("ﬂ", "fl"),
        ("ﬃ", "ffi"),
        ("ﬄ", "ffl"),
    ]
    .iter()
    .fold(text.to_owned(), |text, (glyph, letters)| {
        text.replace(glyph, letters)
    })
}
