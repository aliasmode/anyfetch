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
    let parsed: Preview = serde_json::from_value(json!("40:20")).unwrap();
    assert_eq!(parsed, Preview::Range([40, 20]));
    let parsed: Preview = serde_json::from_value(json!("false")).unwrap();
    assert_eq!(parsed, Preview::Show(false));
    assert!(serde_json::from_value::<Preview>(json!("ten")).is_err());
}

#[test]
fn reads_out_as_a_flag_or_a_path() {
    let parse = |value| serde_json::from_value::<Out>(value).unwrap();
    assert_eq!(parse(json!(true)), Out::Auto(true));
    assert_eq!(parse(json!("true")), Out::Auto(true));
    assert_eq!(parse(json!("false")), Out::Auto(false));
    assert_eq!(parse(json!("notes/a.md")), Out::Path("notes/a.md".into()));
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
