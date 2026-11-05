//! Saving tool results to a file, so they can be read, shared and searched later without
//! filling the model context.

use crate::{ToolError, fetch::Format};
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
    time::Duration,
};

/// Where to save: a file path, or `true` for a name under `.anyfetch/`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Out {
    Auto(bool),
    Path(String),
}

impl std::str::FromStr for Out {
    type Err = std::convert::Infallible;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(match text {
            "true" => Self::Auto(true),
            "false" => Self::Auto(false),
            path => Self::Path(path.to_owned()),
        })
    }
}

/// Which lines of the saved content the answer keeps.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Preview {
    /// `true`: the first 20 lines. `false`: none.
    Show(bool),
    /// N: the first N lines. -N: the last N lines.
    Lines(i64),
    /// [start, count]: `count` lines from line `start` (1-based). [40, 20] is lines 40-59.
    Range([i64; 2]),
}

impl Default for Preview {
    fn default() -> Self {
        Self::Show(true)
    }
}

impl std::str::FromStr for Preview {
    type Err = String;

    /// `true`, `false`, `20`, `-20` or `40:20` (start 40, 20 lines).
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let number = |part: &str| {
            part.trim()
                .parse::<i64>()
                .map_err(|_| format!("Expected true, false, N, -N or START:COUNT, not {text}"))
        };
        match text {
            "true" => Ok(Self::Show(true)),
            "false" => Ok(Self::Show(false)),
            _ => match text.split_once(':') {
                Some((start, end)) => Ok(Self::Range([number(start)?, number(end)?])),
                None => Ok(Self::Lines(number(text)?)),
            },
        }
    }
}

/// Lines a preview of `true` keeps.
const DEFAULT_PREVIEW_LINES: usize = 20;
/// Most characters of a preview, whatever its lines, so a preview cannot fill the context.
const PREVIEW_CHARS: usize = 50_000;
const EXTENSIONS: [&str; 5] = ["md", "txt", "html", "json", "jsonl"];
const SLUG_CHARS: usize = 80;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

impl Preview {
    fn check(self) -> Result<Self, ToolError> {
        match self {
            Self::Lines(0) => Err(invalid("preview must not be 0".to_owned())),
            Self::Range([start, count]) if start < 1 || count < 1 => Err(invalid(format!(
                "preview [{start}, {count}] needs start >= 1 and count >= 1"
            ))),
            preview => Ok(preview),
        }
    }

    /// The previewed lines of `content` and a label such as `1-20 of 340`. `None` for `false`.
    fn cut(self, content: &str) -> Option<(String, String)> {
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let clamp = |line: i64| usize::try_from(line).unwrap_or(usize::MAX).min(total);
        let (start, end) = match self {
            Self::Show(false) => return None,
            Self::Show(true) => (1, DEFAULT_PREVIEW_LINES.min(total)),
            Self::Lines(count) if count > 0 => (1, clamp(count)),
            Self::Lines(count) => {
                let count = usize::try_from(count.unsigned_abs()).unwrap_or(usize::MAX);
                (total.saturating_sub(count) + 1, total)
            }
            Self::Range([start, count]) => (
                usize::try_from(start).unwrap_or(usize::MAX),
                clamp(start.saturating_add(count - 1)),
            ),
        };
        if start > end {
            return Some((String::new(), format!("none of {total}")));
        }
        let mut text = lines[start - 1..end].join("\n");
        if let Some((cut, _)) = text.char_indices().nth(PREVIEW_CHARS) {
            text.truncate(cut);
            text.push_str(&format!(
                "\n[... preview cut at {PREVIEW_CHARS} characters]"
            ));
        }
        Some((text, format!("{start}-{end} of {total}")))
    }
}

/// A checked place to save entries, and the preview to answer with.
pub struct Target {
    place: Place,
    preview: Preview,
}

enum Place {
    /// Every entry goes to this file.
    File(PathBuf),
    /// Each URL gets a file of its own under this folder.
    Auto(PathBuf),
}

impl Target {
    /// Checks `out` and `preview` before anything is fetched. `None` when nothing is to be
    /// saved; `preview` is then not used.
    pub async fn new(
        out: Option<&Out>,
        preview: Option<Preview>,
    ) -> Result<Option<Self>, ToolError> {
        let path = match out {
            None | Some(Out::Auto(false)) => return Ok(None),
            Some(Out::Auto(true)) => None,
            Some(Out::Path(path)) => Some(path),
        };
        let preview = preview.unwrap_or_default().check()?;
        let base = working_dir()?;
        let Some(path) = path else {
            let place = Place::Auto(auto_root(&base));
            return Ok(Some(Self { place, preview }));
        };
        let planned = planned_path(&base, path)?;
        let canonical_base = base.canonicalize().map_err(write_failed)?;
        let relative = planned.strip_prefix(&canonical_base).unwrap_or(&planned);
        git_ignored(&base, relative, path).await?;
        let file = create_folders(&base, &planned, path)?;
        let place = Place::File(file);
        Ok(Some(Self { place, preview }))
    }

    /// Saves the content of a page entry and leaves the preview in its place. Entries that
    /// hold an error are left as they are. JSON content is saved as one compact line, but the
    /// preview cuts the content as the tool returns it (pretty-printed), so `preview_lines`
    /// counts the lines of that text.
    pub fn save_page(&self, entry: &mut Value, format: Format) -> Result<(), ToolError> {
        let Some(content) = entry["content"].as_str() else {
            return Ok(());
        };
        let url = entry["url"].as_str().unwrap_or_default();
        let path = match &self.place {
            Place::File(path) => path.clone(),
            Place::Auto(root) => {
                let named = entry["requested_url"].as_str().unwrap_or(url);
                let content_type = entry["content_type"].as_str().unwrap_or_default();
                auto_path(root, named, extension(format, content_type))
            }
        };
        let retrieved_at = entry["retrieved_at"].as_str().unwrap_or_default();
        let mut saved = append(&path, url, retrieved_at, content)?;
        let preview = self.preview.cut(content);
        let object = entry.as_object_mut().expect("a page entry is an object");
        match preview {
            Some((text, lines)) => {
                object.insert("content".to_owned(), Value::String(text));
                saved["preview_lines"] = Value::String(lines);
            }
            None => {
                object.remove("content");
            }
        }
        object.insert("saved".to_owned(), saved);
        Ok(())
    }

    /// Saves each page of a `{ "pages": [...] }` answer, in order.
    pub fn save_pages(&self, value: &mut Value, format: Format) -> Result<(), ToolError> {
        let Some(pages) = value["pages"].as_array_mut() else {
            return Ok(());
        };
        for page in pages {
            self.save_page(page, format)?;
        }
        Ok(())
    }

    /// Saves a search answer, with the content of any read results, as Markdown (or a JSON
    /// line for a .json or .jsonl path). The answer keeps the results without their content,
    /// and the preview of the saved text as `content`.
    pub fn save_search(&self, value: &mut Value) -> Result<(), ToolError> {
        let query = value["query"].as_str().unwrap_or_default();
        let path = match &self.place {
            Place::File(path) => path.clone(),
            Place::Auto(root) => root.join("search").join(format!("{}.md", slug(query))),
        };
        // For a JSON file the preview shows this pretty text, though the file gets one compact
        // line; `preview_lines` counts the lines of the pretty text.
        let text = if is_json_lines(&path) {
            serde_json::to_string_pretty(value).expect("a JSON value serializes")
        } else {
            search_markdown(value)
        };
        let source = value["source_url"].as_str().unwrap_or_default();
        let retrieved_at = value["retrieved_at"].as_str().unwrap_or_default();
        let mut saved = append(&path, source, retrieved_at, &text)?;
        for hit in value["results"].as_array_mut().into_iter().flatten() {
            if let Some(hit) = hit.as_object_mut() {
                hit.remove("content");
            }
        }
        if let Some((preview, lines)) = self.preview.cut(&text) {
            value["content"] = Value::String(preview);
            saved["preview_lines"] = Value::String(lines);
        }
        value["saved"] = saved;
        Ok(())
    }
}

fn working_dir() -> Result<PathBuf, ToolError> {
    std::env::current_dir().map_err(write_failed)
}

fn write_failed(error: std::io::Error) -> ToolError {
    ToolError::new(
        "write_failed",
        format!("Could not save the content: {error}"),
    )
}

fn invalid(message: String) -> ToolError {
    ToolError::new("invalid_request", message)
}

/// `.anyfetch` in the working directory, ignored by git through its own .gitignore, or a folder
/// in the temporary directory when the working directory is not writable.
fn auto_root(base: &Path) -> PathBuf {
    let root = base.join(".anyfetch");
    let ignore = root.join(".gitignore");
    let ready = std::fs::create_dir_all(&root).and_then(|()| {
        if ignore.exists() {
            Ok(())
        } else {
            std::fs::write(&ignore, "*\n")
        }
    });
    match ready {
        Ok(()) => root,
        Err(_) => std::env::temp_dir().join("anyfetch"),
    }
}

/// The file for `path`, which must be a text file inside a subfolder of `base`, also after
/// links are followed. Creates its folders.
#[cfg(test)]
fn checked_path(base: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let planned = planned_path(base, path)?;
    create_folders(base, &planned, path)
}

/// Checks `path` as [`checked_path`] does, but creates nothing. Returns the file inside the
/// canonical working directory.
fn planned_path(base: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let requested = Path::new(path);
    if !EXTENSIONS.contains(&file_extension(requested).as_str()) {
        let message = format!("out must end in .md, .txt, .html, .json or .jsonl: {path}");
        return Err(invalid(message));
    }
    if requested
        .components()
        .any(|part| part == Component::ParentDir)
    {
        return Err(invalid(format!("out must not hold '..': {path}")));
    }
    let outside = || {
        invalid(format!(
            "out must be inside a subfolder of the working directory {}: {path}",
            shown(base)
        ))
    };
    let canonical_base = base.canonicalize().map_err(write_failed)?;
    let joined = base.join(requested);
    let relative = joined
        .strip_prefix(base)
        .or_else(|_| joined.strip_prefix(&canonical_base))
        .map_err(|_| outside())?;
    let parts: PathBuf = relative
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .collect();
    if parts.components().count() < 2 {
        return Err(outside());
    }
    let file = canonical_base.join(&parts);
    let parent = file.parent().expect("the file is in a subfolder");
    // A link in the path could lead out; check the deepest existing folder before making any.
    let existing = parent.ancestors().find(|folder| folder.exists());
    let existing = existing.unwrap_or(&canonical_base).canonicalize();
    if !existing.map_err(write_failed)?.starts_with(&canonical_base) {
        return Err(outside());
    }
    Ok(file)
}

/// Creates the folders of a planned `file` and checks that they are inside `base`.
fn create_folders(base: &Path, file: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let outside = || {
        invalid(format!(
            "out must be inside a subfolder of the working directory {}: {path}",
            shown(base)
        ))
    };
    let canonical_base = base.canonicalize().map_err(write_failed)?;
    let parent = file.parent().expect("the file is in a subfolder");
    std::fs::create_dir_all(parent).map_err(write_failed)?;
    let real_parent = parent.canonicalize().map_err(write_failed)?;
    if !real_parent.starts_with(&canonical_base) || real_parent == canonical_base {
        return Err(outside());
    }
    let real = real_parent.join(file.file_name().expect("the path has a file name"));
    let metadata = std::fs::symlink_metadata(&real);
    if metadata.is_ok_and(|metadata| !metadata.file_type().is_file()) {
        return Err(invalid(format!(
            "out must be a plain file, not a link or folder: {path}"
        )));
    }
    Ok(real)
}

/// Fails unless git ignores `relative`, so saved pages do not end up in a commit.
async fn git_ignored(base: &Path, relative: &Path, path: &str) -> Result<(), ToolError> {
    let status = tokio::process::Command::new("git")
        .args(["check-ignore", "-q", "--"])
        .arg(relative)
        .current_dir(base)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    match tokio::time::timeout(GIT_TIMEOUT, status).await {
        Ok(Ok(status)) if status.code() == Some(0) => Ok(()),
        Ok(Ok(status)) if status.code() == Some(1) => Err(invalid(format!(
            "{path} is not git-ignored. Add it to .gitignore, or set out to true to save under .anyfetch/"
        ))),
        _ => Err(invalid(format!(
            "Could not check that {path} is git-ignored: the working directory must be in a git repository and git on PATH. Set out to true to save under .anyfetch/"
        ))),
    }
}

/// `<root>/<host>/<slug of path and query>.<extension>`.
fn auto_path(root: &Path, url: &str, extension: &str) -> PathBuf {
    let (host, rest) = match url::Url::parse(url) {
        Ok(parsed) => {
            let query = parsed.query().map(|query| format!("?{query}"));
            let rest = format!("{}{}", parsed.path(), query.unwrap_or_default());
            (parsed.host_str().unwrap_or_default().to_owned(), rest)
        }
        Err(_) => (String::new(), url.to_owned()),
    };
    root.join(slug(&host))
        .join(format!("{}.{extension}", slug(&rest)))
}

/// Letters, digits, `_` and `.` of `text`, other runs as one `-`, at most [`SLUG_CHARS`].
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '.') {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // Only ASCII was pushed, so truncating at a byte count is safe.
    let trimmed = slug.trim_matches(['-', '.']);
    let cut = trimmed[..trimmed.len().min(SLUG_CHARS)].trim_matches(['-', '.']);
    if cut.is_empty() {
        "index".to_owned()
    } else {
        cut.to_owned()
    }
}

fn extension(format: Format, content_type: &str) -> &'static str {
    match format {
        Format::Json => "jsonl",
        Format::Html => "html",
        Format::Text => "txt",
        // Readable keeps the served type, such as text/html, for content that is Markdown.
        Format::Readable | Format::Markdown if content_type.contains("json") => "jsonl",
        Format::Readable | Format::Markdown => "md",
    }
}

/// Files of .json and .jsonl paths hold JSON Lines: one compact JSON value per line.
fn is_json_lines(path: &Path) -> bool {
    matches!(file_extension(path).as_str(), "json" | "jsonl")
}

fn file_extension(path: &Path) -> String {
    let extension = path.extension().and_then(|extension| extension.to_str());
    extension.unwrap_or_default().to_ascii_lowercase()
}

/// The path without the `\\?\` that `canonicalize` adds on Windows.
fn shown(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => text.into_owned(),
    }
}

/// Appends one entry after a line that names its source. JSON entries (.json, .jsonl) are one
/// compact line each, with no source line, so the file is JSON Lines.
fn append(path: &Path, url: &str, retrieved_at: &str, content: &str) -> Result<Value, ToolError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(write_failed)?;
    }
    let appended = std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0);
    let entry = if is_json_lines(path) {
        // One compact line, whatever the layout of the content.
        let value =
            serde_json::from_str::<Value>(content).unwrap_or(Value::String(content.to_owned()));
        format!(
            "{}\n",
            serde_json::to_string(&value).expect("a JSON value serializes")
        )
    } else {
        // "--" would end the comment early.
        let url = url.replace("--", "%2D%2D");
        let gap = if appended { "\n" } else { "" };
        format!("{gap}<!-- anyfetch: {url} {retrieved_at} -->\n{content}\n")
    };
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(entry.as_bytes()))
        .map_err(write_failed)?;
    Ok(json!({
        "path": shown(path),
        "bytes_written": entry.len(),
        "appended": appended,
    }))
}

fn search_markdown(value: &Value) -> String {
    let query = value["query"].as_str().unwrap_or_default();
    let mut text = format!("# {query}\n");
    for hit in value["results"].as_array().into_iter().flatten() {
        let field = |name: &str| hit[name].as_str().unwrap_or_default().to_owned();
        text.push_str(&format!(
            "\n## [{}]({})\n\n{}\n",
            field("title"),
            field("url"),
            field("snippet")
        ));
        if let Some(content) = hit["content"].as_str() {
            text.push_str(&format!("\n{content}\n"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("anyfetch-out-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn code(result: Result<PathBuf, ToolError>) -> &'static str {
        result.map_or_else(|error| error.code, |_| "ok")
    }

    #[test]
    fn accepts_text_files_in_a_subfolder_only() {
        let base = base("paths");
        let file = checked_path(&base, "notes/page.md").unwrap();
        assert!(file.parent().unwrap().is_dir());
        assert!(file.ends_with("notes/page.md"));
        assert_eq!(code(checked_path(&base, "notes/deep/Page.MD")), "ok");
        let inside = base.join("notes").join("b.json");
        assert_eq!(code(checked_path(&base, inside.to_str().unwrap())), "ok");
        assert_eq!(code(checked_path(&base, "page.md")), "invalid_request");
        assert_eq!(code(checked_path(&base, "./page.md")), "invalid_request");
        assert_eq!(
            code(checked_path(&base, "notes/../../x.md")),
            "invalid_request"
        );
        assert_eq!(
            code(checked_path(&base, "notes/../x.md")),
            "invalid_request"
        );
        let outside = std::env::temp_dir().join("anyfetch-elsewhere").join("x.md");
        assert_eq!(
            code(checked_path(&base, outside.to_str().unwrap())),
            "invalid_request"
        );
        assert_eq!(
            code(checked_path(&base, "notes/page.exe")),
            "invalid_request"
        );
        assert_eq!(code(checked_path(&base, "notes/page")), "invalid_request");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn names_automatic_files_from_the_url() {
        let root = Path::new("root");
        let index = auto_path(root, "https://example.org/", "md");
        assert_eq!(index, root.join("example.org").join("index.md"));
        let page = auto_path(root, "https://docs.rs/serde/latest/Serde Book?x=1", "html");
        assert_eq!(
            page,
            root.join("docs.rs")
                .join("serde-latest-Serde-20Book-x-1.html")
        );
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug(&"a/".repeat(100)).len(), SLUG_CHARS - 1);
        assert_eq!(slug("ünïcode"), "n-code");
    }

    #[test]
    fn appends_each_entry_after_a_separator() {
        let base = base("append");
        let path = base.join("saved").join("page.md");
        let content: String = (1..=30).map(|line| format!("line {line}\n")).collect();
        let entry = || {
            json!({
                "url": "https://example.org/a",
                "retrieved_at": "2026-01-01T00:00:00Z",
                "content_type": "text/html",
                "content": content,
            })
        };
        let target = Target {
            place: Place::File(path.clone()),
            preview: Preview::default(),
        };
        let mut first = entry();
        target.save_page(&mut first, Format::Readable).unwrap();
        assert_eq!(first["saved"]["appended"], false);
        assert_eq!(first["saved"]["preview_lines"], "1-20 of 30");
        assert_eq!(first["content"].as_str().unwrap().lines().count(), 20);
        let hidden = Target {
            place: Place::File(path.clone()),
            preview: Preview::Show(false),
        };
        let mut second = entry();
        hidden.save_page(&mut second, Format::Readable).unwrap();
        assert_eq!(second["saved"]["appended"], true);
        assert!(second.get("content").is_none());
        assert!(second["saved"].get("preview_lines").is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        let separator = "<!-- anyfetch: https://example.org/a 2026-01-01T00:00:00Z -->\n";
        assert_eq!(text.matches(separator).count(), 2);
        assert!(text.starts_with(separator));
        assert_eq!(text.matches(&content).count(), 2);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn saves_json_as_one_line_per_entry() {
        let base = base("jsonl");
        let path = base.join("saved").join("data.jsonl");
        let target = Target {
            place: Place::File(path.clone()),
            preview: Preview::default(),
        };
        for id in 1..=2 {
            let pretty = format!("{{\n  \"id\": {id},\n  \"tags\": [\"a\", \"b\"]\n}}");
            let mut entry = json!({ "url": "https://example.org/a", "content": pretty });
            target.save_page(&mut entry, Format::Json).unwrap();
            assert_eq!(entry["saved"]["preview_lines"], "1-4 of 4");
            assert_eq!(entry["content"], pretty);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let values: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[1]["id"], 2);
        assert!(!text.contains("anyfetch:"));
        assert_eq!(extension(Format::Json, "application/json"), "jsonl");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn previews_the_lines_asked_for() {
        let content: String = (1..=100).map(|line| format!("{line}\n")).collect();
        let cut = |preview: Preview| {
            preview
                .check()
                .ok()
                .and_then(|preview| preview.cut(&content))
        };
        let lines = |preview| cut(preview).map(|(_, lines)| lines);
        assert_eq!(lines(Preview::Show(true)).unwrap(), "1-20 of 100");
        assert_eq!(
            cut(Preview::Show(true)).unwrap().0.lines().last(),
            Some("20")
        );
        assert_eq!(cut(Preview::Show(false)), None);
        assert_eq!(lines(Preview::Lines(5)).unwrap(), "1-5 of 100");
        assert_eq!(lines(Preview::Lines(500)).unwrap(), "1-100 of 100");
        assert_eq!(cut(Preview::Lines(-3)).unwrap().0, "98\n99\n100");
        assert_eq!(lines(Preview::Lines(-500)).unwrap(), "1-100 of 100");
        assert_eq!(cut(Preview::Range([40, 3])).unwrap().0, "40\n41\n42");
        assert_eq!(lines(Preview::Range([40, 20])).unwrap(), "40-59 of 100");
        assert_eq!(lines(Preview::Range([95, 200])).unwrap(), "95-100 of 100");
        assert_eq!(lines(Preview::Range([150, 20])).unwrap(), "none of 100");
        assert_eq!(
            lines(Preview::Range([1, i64::MAX])).unwrap(),
            "1-100 of 100"
        );
        assert_eq!(lines(Preview::Show(true)), lines(Preview::default()));
        for wrong in [
            Preview::Lines(0),
            Preview::Range([0, 5]),
            Preview::Range([5, 0]),
            Preview::Range([5, -2]),
        ] {
            assert_eq!(wrong.check().unwrap_err().code, "invalid_request");
        }
        assert_eq!("40:20".parse(), Ok(Preview::Range([40, 20])));
        assert_eq!("-20".parse(), Ok(Preview::Lines(-20)));
        assert_eq!("false".parse(), Ok(Preview::Show(false)));
        assert!("ten".parse::<Preview>().is_err());
        let parsed: Preview = serde_json::from_value(json!([40, 60])).unwrap();
        assert_eq!(parsed, Preview::Range([40, 60]));
        let parsed: Preview = serde_json::from_value(json!(-5)).unwrap();
        assert_eq!(parsed, Preview::Lines(-5));
    }

    #[test]
    fn caps_a_long_preview() {
        let long = "é".repeat(PREVIEW_CHARS + 10);
        let (text, lines) = Preview::Show(true).cut(&long).unwrap();
        assert_eq!(lines, "1-1 of 1");
        let note = format!("\n[... preview cut at {PREVIEW_CHARS} characters]");
        assert!(text.ends_with(&note));
        assert_eq!(text.chars().count(), PREVIEW_CHARS + note.chars().count());
    }

    #[tokio::test]
    async fn needs_the_path_to_be_git_ignored() {
        let base = base("git");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&base)
            .status();
        if !init.is_ok_and(|status| status.success()) {
            return;
        }
        std::fs::write(base.join(".gitignore"), "ignored/\n").unwrap();
        let ignored = git_ignored(&base, Path::new("ignored/a.md"), "ignored/a.md").await;
        assert!(ignored.is_ok());
        let kept = git_ignored(&base, Path::new("kept/a.md"), "kept/a.md").await;
        assert_eq!(kept.unwrap_err().code, "invalid_request");
        // A refused path leaves no folder behind: planning creates nothing.
        let planned = planned_path(&base, "kept/deep/a.md").unwrap();
        let relative = planned.strip_prefix(base.canonicalize().unwrap()).unwrap();
        assert!(
            git_ignored(&base, relative, "kept/deep/a.md")
                .await
                .is_err()
        );
        assert!(!base.join("kept").exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
