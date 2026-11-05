//! A short-lived page cache, so reading a long page in parts fetches it once.

use crate::fetch::FetchResponse;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// The lifetime that Claude Code's built-in fetch uses for its page cache.
const LIFETIME: Duration = Duration::from_secs(15 * 60);

fn directory() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join(".cache/anyfetch/pages"))
}

fn fresh(path: &Path) -> bool {
    let modified = path.metadata().and_then(|metadata| metadata.modified());
    modified.is_ok_and(|time| time.elapsed().is_ok_and(|age| age < LIFETIME))
}

pub(crate) fn get(url: &str) -> Option<FetchResponse> {
    read(&directory()?, url)
}

pub(crate) fn put(page: &FetchResponse) {
    if let Some(directory) = directory() {
        write(&directory, page);
    }
}

fn file(directory: &Path, url: &str) -> PathBuf {
    directory.join(format!("{}.json", crate::hex(&Sha256::digest(url))))
}

fn read(directory: &Path, url: &str) -> Option<FetchResponse> {
    let path = file(directory, url);
    if !fresh(&path) {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write(directory: &Path, page: &FetchResponse) {
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        if !fresh(&entry.path()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    if let Ok(json) = serde_json::to_vec(page) {
        let _ = write_private(&file(directory, &page.requested_url), &json);
    }
}

/// Writes `bytes` to `path` readable by the owner only (on Unix). A temporary file and a rename
/// keep concurrent readers from seeing a partial file.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // The counter separates parallel writes from this process.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}-{unique}.tmp", std::process::id()));
    let temporary = PathBuf::from(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&temporary)
        .and_then(|mut file| std::io::Write::write_all(&mut file, bytes))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_stored_page_for_the_same_url_only() {
        let directory =
            std::env::temp_dir().join(format!("anyfetch-cache-test-{}", std::process::id()));
        let page = FetchResponse {
            requested_url: "https://example.org/a".into(),
            url: "https://example.org/a".into(),
            status: 200,
            content_type: "text/html".into(),
            retrieved_at: "2026-01-01T00:00:00Z".into(),
            title: Some("A".into()),
            description: None,
            total_chars: 5,
            focused: false,
            content: "hello".into(),
        };
        write(&directory, &page);
        assert_eq!(
            read(&directory, "https://example.org/a").unwrap().content,
            "hello"
        );
        assert!(read(&directory, "https://example.org/b").is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
