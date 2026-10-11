//! Connects Anyfetch to the agent harnesses on this machine.
//!
//! Each harness has its own command or file for MCP servers. `setup` uses it for every harness
//! it finds, so one step after the install is enough.

use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, time::timeout};

/// Harnesses that add an MCP server from their command line: name, program, arguments. `{}`
/// stands for the path of this program.
const HARNESSES: [(&str, &str, &[&str]); 8] = [
    (
        "Claude Code",
        "claude",
        &[
            "mcp", "add", "--scope", "user", "anyfetch", "--", "{}", "mcp",
        ],
    ),
    (
        "Codex",
        "codex",
        &["mcp", "add", "anyfetch", "--", "{}", "mcp"],
    ),
    (
        "Gemini CLI",
        "gemini",
        &["mcp", "add", "-s", "user", "anyfetch", "{}", "mcp"],
    ),
    (
        "Hermes Agent",
        "hermes",
        &["mcp", "add", "anyfetch", "--command", "{}", "--args", "mcp"],
    ),
    ("Pi", "pi", &["mcp", "add", "anyfetch", "--", "{}", "mcp"]),
    (
        "Cline",
        "cline",
        &["mcp", "add", "anyfetch", "--yes", "--", "{}", "mcp"],
    ),
    ("Amp", "amp", &["mcp", "add", "anyfetch", "--", "{}", "mcp"]),
    (
        "OpenClaw",
        "openclaw",
        &["mcp", "add", "anyfetch", "--command", "{}", "--arg", "mcp"],
    ),
];

/// How long one harness gets to add the server. Hermes starts the server and lists its tools
/// first, which took 4 seconds in tests.
const WAIT: Duration = Duration::from_secs(30);

/// Connects every harness found on this machine. Returns the lines to show the user.
pub async fn setup() -> Vec<String> {
    let Ok(program) = std::env::current_exe() else {
        return vec!["Could not find the path of this program".to_owned()];
    };
    // The full path works in harnesses that do not have the install folder on their PATH.
    let program = program.to_string_lossy();
    let mut lines = Vec::new();
    let mut missing = Vec::new();
    for (name, command, arguments) in HARNESSES {
        match find(command) {
            Some(harness) => {
                lines.push(format!(
                    "{name}: {}",
                    add(&harness, arguments, &program).await
                ));
            }
            None => missing.push(name),
        }
    }
    match cursor(&program) {
        Some(result) => lines.push(format!("Cursor: {result}")),
        None => missing.push("Cursor"),
    }
    if lines.is_empty() {
        lines.push("No supported agent harness was found on this machine.".to_owned());
    } else {
        lines.push("Start a new agent session to use the tools.".to_owned());
    }
    if !missing.is_empty() {
        lines.push(format!("Not found: {}.", missing.join(", ")));
    }
    lines.push(format!(
        "For any other harness, add a stdio MCP server with the command: {program} mcp"
    ));
    lines
}

/// The file of `program` on the PATH.
fn find(program: &str) -> Option<PathBuf> {
    let endings: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|folder| {
            let names = endings.iter().map(|ending| format!("{program}{ending}"));
            names.map(move |name| folder.join(name))
        })
        .find(|candidate| candidate.is_file())
}

/// Runs the harness's own command that adds this program as an MCP server.
async fn add(harness: &Path, arguments: &[&str], program: &str) -> String {
    let arguments = arguments.iter().map(|part| part.replace("{}", program));
    let child = tokio::process::Command::new(harness)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = child else {
        return "failed: could not start its command".to_owned();
    };
    // Some harnesses ask to confirm, and stop when they get no answer.
    if let Some(mut input) = child.stdin.take() {
        let _ = input.write_all(b"y\ny\ny\n").await;
    }
    let Ok(Ok(output)) = timeout(WAIT, child.wait_with_output()).await else {
        return "failed: its command did not finish".to_owned();
    };
    let text = [output.stdout, output.stderr].concat();
    let text = String::from_utf8_lossy(&text);
    if text.contains("already exists") {
        "already connected".to_owned()
    } else if output.status.success() {
        "connected".to_owned()
    } else {
        let reason = text.lines().rev().find(|line| !line.trim().is_empty());
        format!("failed: {}", reason.unwrap_or("no message").trim())
    }
}

/// Adds the server to Cursor's MCP file. `None` when Cursor is not on this machine.
fn cursor(program: &str) -> Option<String> {
    let folder = std::env::home_dir()?.join(".cursor");
    if !folder.is_dir() {
        return None;
    }
    let file = folder.join("mcp.json");
    let current = std::fs::read_to_string(&file).unwrap_or_default();
    let saved = with_server(&current, program).and_then(|config| std::fs::write(file, config).ok());
    Some(match saved {
        Some(()) => "connected".to_owned(),
        None => "failed: ~/.cursor/mcp.json could not be changed".to_owned(),
    })
}

/// The MCP file `config` with this server added. The rest of the file stays as it is.
fn with_server(config: &str, program: &str) -> Option<String> {
    let config = if config.trim().is_empty() {
        "{}"
    } else {
        config
    };
    let mut config: Value = serde_json::from_str(config).ok()?;
    let servers = config.as_object_mut()?.entry("mcpServers");
    let servers = servers.or_insert_with(|| json!({})).as_object_mut()?;
    servers.insert(
        "anyfetch".to_owned(),
        json!({ "command": program, "args": ["mcp"] }),
    );
    Some(serde_json::to_string_pretty(&config).ok()? + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_the_server_to_an_mcp_file_and_keeps_the_rest() {
        let current = r#"{"mcpServers": {"other": {"command": "x"}}, "theme": "dark"}"#;
        let config: Value =
            serde_json::from_str(&with_server(current, "/bin/anyfetch").unwrap()).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert_eq!(
            config["mcpServers"]["anyfetch"],
            json!({ "command": "/bin/anyfetch", "args": ["mcp"] })
        );
        assert!(
            with_server("", "/bin/anyfetch")
                .unwrap()
                .contains("anyfetch")
        );
        // A file with comments or errors is left alone.
        assert!(with_server("{ // note\n}", "/bin/anyfetch").is_none());
        assert!(with_server("[]", "/bin/anyfetch").is_none());
    }
}
