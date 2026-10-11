//! Optional JavaScript rendering with Lightpanda, a small headless engine that the user installs.
//!
//! Some pages hold no text until their scripts run. Lightpanda runs the scripts without a full
//! browser and prints the finished HTML.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

/// The renderer to use, or `None` when rendering is off. `FETCHROAM_RENDERER=off` turns it off;
/// `FETCHROAM_RENDERER=<path>` selects a program; otherwise `lightpanda` is looked up on PATH.
pub fn renderer() -> Option<PathBuf> {
    match std::env::var_os("FETCHROAM_RENDERER") {
        Some(value) if value == "off" => None,
        Some(value) if !value.is_empty() => Some(value.into()),
        _ => {
            let name = format!("lightpanda{}", std::env::consts::EXE_SUFFIX);
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut candidates = std::env::split_paths(&path).map(|folder| folder.join(&name));
            candidates.find(|candidate| candidate.is_file())
        }
    }
}

/// The HTML of `url` after its scripts ran, or `None` when the renderer fails.
pub async fn render(renderer: &Path, url: &str) -> Option<String> {
    let run = tokio::process::Command::new(renderer)
        .args(["fetch", "--dump", "html", "--wait-until", "networkidle"])
        .args(["--wait-ms", "10000", url])
        .env("LIGHTPANDA_DISABLE_TELEMETRY", "true")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(crate::transport::TIMEOUT, run).await;
    let output = output.ok()?.ok()?;
    let html = String::from_utf8_lossy(&output.stdout);
    (output.status.success() && !html.trim().is_empty()).then(|| html.into_owned())
}
