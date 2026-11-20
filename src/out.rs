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
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(from = "OutRepr")]
pub enum Out {
    Auto(bool),
    Path(String),
}

/// How `out` is written: a JSON boolean, or a string that is `true`, `false` or a path.
#[derive(Deserialize)]
#[serde(untagged)]
enum OutRepr {
    Flag(bool),
    Text(String),
}

impl From<OutRepr> for Out {
    fn from(repr: OutRepr) -> Self {
        match repr {
            OutRepr::Flag(flag) => Self::Auto(flag),
            OutRepr::Text(text) => text.parse().unwrap_or_else(|never| match never {}),
        }
    }
}

// Out and Preview have hand-written schemas with one `type` each. A derived `anyOf` shows as
// "unknown" in agent harnesses, and a `type` array is refused by some clients (Gemini's
// OpenAPI subset). Both still accept their JSON forms besides the string.
impl schemars::JsonSchema for Out {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Out".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
}

impl schemars::JsonSchema for Preview {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Preview".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
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
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(try_from = "PreviewRepr")]
pub enum Preview {
    /// `true`: the first 20 lines. `false`: none.
    Show(bool),
    /// N: the first N lines. -N: the last N lines.
    Lines(i64),
    /// [start, count]: `count` lines from line `start` (1-based). [40, 20] is lines 40-59.
    Range([i64; 2]),
}

/// How `preview` is written: a string as for [`Preview::from_str`], or the JSON forms.
#[derive(Deserialize)]
#[serde(untagged)]
enum PreviewRepr {
    Show(bool),
    Lines(i64),
    Range([i64; 2]),
    Text(String),
}

impl TryFrom<PreviewRepr> for Preview {
    type Error = String;

    fn try_from(repr: PreviewRepr) -> Result<Self, String> {
        Ok(match repr {
            PreviewRepr::Show(show) => Self::Show(show),
            PreviewRepr::Lines(lines) => Self::Lines(lines),
            PreviewRepr::Range(range) => Self::Range(range),
            PreviewRepr::Text(text) => text.parse()?,
        })
    }
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
#[path = "out.test.rs"]
mod tests;
