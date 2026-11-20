//! Fallback renderer: a Chromium-based browser driven by Playwright, for pages that need their
//! scripts when Lightpanda is missing, and for pages that refuse plain requests.
//!
//! The browser is one already open for remote debugging, or one started headless on a profile.
//! Playwright runs in Bun or Node (`playwright.mjs`, one long-lived helper per server): a global
//! install is used when found, otherwise `bunx` or `npx` fetch it. The profile and launch options
//! can be shared with the Playwright MCP server through `PLAYWRIGHT_USERDATA_DIR` and
//! `PLAYWRIGHT_MCP_CONFIG`. `ANYFETCH_PLAYWRIGHT=off` turns the fallback off.

use crate::ToolError;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, OnceCell, oneshot},
};

const SCRIPT: &str = include_str!("playwright.mjs");
const PACKAGE: &str = "playwright-core@1";
/// The first start includes `bunx` or `npx` downloading Playwright.
const START: Duration = Duration::from_secs(90);
const PAGE: Duration = Duration::from_secs(45);
/// A failed start is not tried again for this long, so a batch of fetches fails fast.
const RETRY: Duration = Duration::from_secs(300);
const DEBUG_PORT: u16 = 9222;

/// A page as the browser loaded it.
pub struct Rendered {
    pub url: String,
    pub status: u16,
    pub html: String,
}

pub fn enabled() -> bool {
    std::env::var_os("ANYFETCH_PLAYWRIGHT").is_none_or(|value| value != "off")
}

/// Stops the helper, and with it a browser that it started. Call before the runtime ends: on
/// Windows the runtime waits for the thread that reads the helper's output.
pub async fn shutdown() {
    // Dropping its stdin ends the helper; its output then closes.
    *STATE.lock().await = State::Idle;
}

/// Loads `url` in the browser and returns the page after its scripts ran.
pub async fn render(url: &str) -> Result<Rendered, ToolError> {
    let answer = {
        let mut state = STATE.lock().await;
        if let State::Running(helper) = &mut *state
            && !matches!(helper.child.try_wait(), Ok(None))
        {
            *state = State::Idle;
        }
        if let State::Failed(at, error) = &*state {
            if at.elapsed() < RETRY {
                return Err(error.clone());
            }

            *state = State::Idle;
        }
        if matches!(*state, State::Idle) {
            match start().await {
                Ok(helper) => *state = State::Running(Box::new(helper)),
                Err(error) => {
                    // The client keeps a stdio server's stderr in its MCP logs, for the user.
                    eprintln!("anyfetch: {}", error.message);
                    *state = State::Failed(Instant::now(), error.clone());
                    return Err(error);
                }
            }
        }
        let State::Running(helper) = &mut *state else {
            unreachable!("the helper was just started");
        };
        helper.ask(url).await?
    };
    let reply = tokio::time::timeout(PAGE, answer)
        .await
        .map_err(|_| failed("The browser took longer than 45 seconds to load the page"))?
        .map_err(|_| failed("The browser helper stopped"))?;
    match (reply.html, reply.error) {
        (Some(html), _) => Ok(Rendered {
            url: reply.url.unwrap_or_else(|| url.to_owned()),
            status: reply.status.unwrap_or(200),
            html,
        }),
        (None, error) => Err(failed(format!(
            "The browser could not load the page: {}",
            error.unwrap_or_default()
        ))),
    }
}

static STATE: Mutex<State> = Mutex::const_new(State::Idle);
static RUNNER: OnceCell<Result<Runner, ToolError>> = OnceCell::const_new();

enum State {
    Idle,
    Running(Box<Helper>),
    Failed(Instant, ToolError),
}

type Waiting = Arc<StdMutex<HashMap<u64, oneshot::Sender<Answer>>>>;

struct Helper {
    child: Child,
    stdin: ChildStdin,
    waiting: Waiting,
    next: u64,
}

impl Helper {
    async fn ask(&mut self, url: &str) -> Result<oneshot::Receiver<Answer>, ToolError> {
        self.next += 1;
        let (sender, receiver) = oneshot::channel();
        self.waiting
            .lock()
            .expect("waiting lock")
            .insert(self.next, sender);
        let line = serde_json::json!({ "id": self.next, "url": url }).to_string() + "\n";
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|_| failed("The browser helper stopped"))?;
        Ok(receiver)
    }
}

/// One line from the helper: its start, or the answer to one request.
#[derive(Deserialize)]
struct Answer {
    ready: Option<bool>,
    fatal: Option<String>,
    id: Option<u64>,
    url: Option<String>,
    status: Option<u16>,
    html: Option<String>,
    error: Option<String>,
}

/// How to run the helper script.
struct Runner {
    program: PathBuf,
    args: &'static [&'static str],
    /// A global Playwright package; without it, `bunx` or `npx` put one on the helper's PATH.
    module: Option<PathBuf>,
}

enum Target {
    /// A browser that already listens for remote debugging.
    Attach(String),
    Launch(Launch),
}

struct Launch {
    name: &'static str,
    program: PathBuf,
    /// The user data folder, and the profile inside it when one other than the default was named.
    data: PathBuf,
    profile: Option<String>,
    /// Playwright `launchOptions` from the shared config.
    options: Option<Value>,
    /// The browser's own folder, not a copy made for anyfetch.
    default: bool,
}

/// A Chromium-based browser on this machine and its own user data folder.
struct Chromium {
    name: &'static str,
    program: PathBuf,
    data: Option<PathBuf>,
    /// Ignores remote debugging on its own user data folder (Chrome and Edge 136 and later).
    refuses_default: bool,
}

async fn start() -> Result<Helper, ToolError> {
    let runner = RUNNER
        .get_or_init(find_runner)
        .await
        .as_ref()
        .map_err(Clone::clone)?;
    let target = target().await?;
    let script = script_file()?;
    let mut command = Command::new(&runner.program);
    command
        .args(runner.args)
        .arg(&script)
        .current_dir(script.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(module) = &runner.module {
        command.env("ANYFETCH_PW_MODULE", module);
    }
    match &target {
        Target::Attach(url) => {
            command.env("ANYFETCH_PW_CDP", url);
        }
        Target::Launch(launch) => {
            command
                .env("ANYFETCH_PW_EXE", &launch.program)
                .env("ANYFETCH_PW_PROFILE", &launch.data);
            if let Some(profile) = &launch.profile {
                command.env("ANYFETCH_PW_PROFILE_DIR", profile);
            }
            if let Some(options) = &launch.options {
                command.env("ANYFETCH_PW_LAUNCH", options.to_string());
            }
        }
    }
    let mut child = command.spawn().map_err(|_| {
        unavailable(format!(
            "Could not start {} for the browser fallback",
            runner.program.display()
        ))
    })?;
    let stdin = child.stdin.take().expect("stdin is piped");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
    let first = tokio::time::timeout(START, lines.next_line()).await;
    let first = first.ok().and_then(Result::ok).flatten();
    let first = first.and_then(|line| serde_json::from_str::<Answer>(&line).ok());
    if !first
        .as_ref()
        .is_some_and(|answer| answer.ready == Some(true))
    {
        let reason = first.and_then(|answer| answer.fatal);
        let reason = reason.unwrap_or_else(|| "the Playwright helper did not start".to_owned());
        return Err(start_failed(&target, &reason));
    }
    let waiting = Waiting::default();
    tokio::spawn(read_answers(lines, Arc::clone(&waiting)));
    Ok(Helper {
        child,
        stdin,
        waiting,
        next: 0,
    })
}

async fn read_answers(mut lines: Lines<BufReader<ChildStdout>>, waiting: Waiting) {
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(answer) = serde_json::from_str::<Answer>(&line) else {
            continue;
        };

        let sender = answer
            .id
            .and_then(|id| waiting.lock().expect("waiting lock").remove(&id));
        if let Some(sender) = sender {
            let _ = sender.send(answer);
        }
    }
    // Dropping the senders tells every open request that the helper stopped.
    waiting.lock().expect("waiting lock").clear();
}

fn start_failed(target: &Target, reason: &str) -> ToolError {
    match target {
        Target::Launch(launch) if launch.default => unavailable(copy_profile(
            launch,
            &format!("could not be opened for automation ({reason})"),
        )),
        Target::Launch(launch) => unavailable(format!(
            "{} did not start on the profile {}: {reason}",
            launch.name,
            launch.data.display()
        )),
        Target::Attach(url) => unavailable(format!(
            "Could not attach to the browser at {url}: {reason}"
        )),
    }
}

/// The steps to give anyfetch a profile of its own.
fn copy_profile(launch: &Launch, why: &str) -> String {
    format!(
        "The browser fallback needs a profile. {name}'s default profile ({data}) {why}. Close {name}, \
         copy that folder to a new folder, set PLAYWRIGHT_USERDATA_DIR to the copy in the MCP \
         server's environment and restart the server: anyfetch then starts its own headless {name} \
         on the copy. Set ANYFETCH_PLAYWRIGHT=off to turn the fallback off.",
        name = launch.name,
        data = launch.data.display(),
    )
}

/// The runtime for the helper: a global Playwright under Bun or Node, else `bunx`, else `npx`.
async fn find_runner() -> Result<Runner, ToolError> {
    let find = crate::setup::find;
    let (bun, node) = (find("bun"), find("node"));
    if let Some(runtime) = bun.as_ref().or(node.as_ref())
        && let Some(module) = global_package().await
    {
        return Ok(Runner {
            program: runtime.clone(),
            args: &[],
            module: Some(module),
        });
    }

    if bun.is_some()
        && let Some(bunx) = find("bunx")
    {
        return Ok(Runner {
            program: bunx,
            args: &["--package", PACKAGE, "bun"],
            module: None,
        });
    }

    if node.is_some()
        && let Some(npx) = find("npx")
    {
        return Ok(Runner {
            program: npx,
            args: &["--yes", "--package", PACKAGE, "node"],
            module: None,
        });
    }

    Err(unavailable(
        "The browser fallback needs Bun (https://bun.sh) or Node.js with npx (https://nodejs.org) \
         on PATH. Install one and restart the MCP server. Set ANYFETCH_PLAYWRIGHT=off to turn the \
         fallback off.",
    ))
}

/// `playwright-core` or `playwright` installed globally with Bun or npm.
async fn global_package() -> Option<PathBuf> {
    let bun_home = std::env::var_os("BUN_INSTALL")
        .map(PathBuf::from)
        .or_else(|| Some(std::env::home_dir()?.join(".bun")));
    let mut roots: Vec<PathBuf> = bun_home
        .map(|home| home.join("install/global/node_modules"))
        .into_iter()
        .collect();
    if let Some(npm) = crate::setup::find("npm") {
        roots.extend(npm_root(&npm).await);
    }
    roots
        .iter()
        .flat_map(|root| ["playwright-core", "playwright"].map(|name| root.join(name)))
        .find(|folder| folder.join("index.js").is_file())
}

async fn npm_root(npm: &Path) -> Option<PathBuf> {
    let run = Command::new(npm)
        .args(["root", "-g"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .ok()?
        .ok()?;
    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !root.is_empty()).then(|| root.into())
}

/// The helper script, written next to the page cache so that it runs outside any project.
fn script_file() -> Result<PathBuf, ToolError> {
    let folder = std::env::home_dir()
        .map(|home| home.join(".cache/anyfetch/playwright"))
        .unwrap_or_else(|| std::env::temp_dir().join("anyfetch-playwright"));
    let file = folder.join("helper.mjs");
    if std::fs::read_to_string(&file).is_ok_and(|text| text == SCRIPT) {
        return Ok(file);
    }

    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&file, SCRIPT))
        .map_err(|_| unavailable(format!("Could not write {}", file.display())))?;
    Ok(file)
}

/// Browser settings shared with the Playwright MCP server: its config file, named by
/// `PLAYWRIGHT_MCP_CONFIG`.
#[derive(Default)]
struct Shared {
    cdp: Option<String>,
    data: Option<PathBuf>,
    program: Option<PathBuf>,
    launch: Option<Value>,
}

fn shared() -> Result<Shared, ToolError> {
    let Some(path) = env_text("PLAYWRIGHT_MCP_CONFIG") else {
        return Ok(Shared::default());
    };

    let text = std::fs::read_to_string(&path)
        .map_err(|_| unavailable(format!("Could not read PLAYWRIGHT_MCP_CONFIG ({path})")))?;
    let config: Value = serde_json::from_str(&text)
        .map_err(|_| unavailable(format!("PLAYWRIGHT_MCP_CONFIG ({path}) is not valid JSON")))?;
    let browser = &config["browser"];
    let text = |value: &Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Ok(Shared {
        cdp: text(&browser["cdpEndpoint"]),
        data: text(&browser["userDataDir"]).map(PathBuf::from),
        program: text(&browser["launchOptions"]["executablePath"]).map(PathBuf::from),
        launch: Some(browser["launchOptions"].clone()).filter(Value::is_object),
    })
}

fn env_text(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The browser to use: one open for remote debugging, else one to start on a profile.
async fn target() -> Result<Target, ToolError> {
    let shared = shared()?;
    if let Some(url) = shared.cdp {
        return Ok(Target::Attach(url));
    }

    let installed = installed();
    let copy = env_text("PLAYWRIGHT_USERDATA_DIR")
        .map(PathBuf::from)
        .or(shared.data)
        .map(|path| split_profile(&path));
    let folders = copy.iter().map(|(data, _)| data.clone());
    let folders = folders.chain(installed.iter().filter_map(|browser| browser.data.clone()));
    for folder in folders {
        if let Some(url) = debug_url(&folder).await {
            return Ok(Target::Attach(url));
        }
    }
    if answers_cdp(DEBUG_PORT).await {
        return Ok(Target::Attach(format!("http://127.0.0.1:{DEBUG_PORT}")));
    }

    let chosen = match shared.program {
        Some(program) => Chromium {
            name: "The browser",
            program,
            data: None,
            refuses_default: false,
        },
        None => installed.into_iter().next().ok_or_else(|| {
            unavailable(
                "The browser fallback found no Chromium-based browser (Chrome, Brave, Edge or \
                 Chromium). Install one, or set launchOptions.executablePath in the file of \
                 PLAYWRIGHT_MCP_CONFIG.",
            )
        })?,
    };
    if let Some((data, profile)) = copy {
        if in_use(&data) {
            return Err(unavailable(format!(
                "The profile in PLAYWRIGHT_USERDATA_DIR ({}) is open in another browser, such as \
                 one of the Playwright MCP server. Close it and try again, or start it with \
                 --remote-debugging-port={DEBUG_PORT} so that anyfetch attaches to it.",
                data.display()
            )));
        }

        return Ok(Target::Launch(Launch {
            name: chosen.name,
            program: chosen.program,
            data,
            profile,
            options: shared.launch,
            default: false,
        }));
    }

    let Some(data) = chosen.data else {
        return Err(unavailable(
            "The browser fallback needs a profile: set PLAYWRIGHT_USERDATA_DIR to a copy of one.",
        ));
    };
    let launch = Launch {
        name: chosen.name,
        program: chosen.program,
        data,
        profile: None,
        options: shared.launch,
        default: true,
    };
    if chosen.refuses_default {
        let why = "cannot be automated: Chrome and Edge 136 and later refuse it";
        return Err(unavailable(copy_profile(&launch, why)));
    }

    if in_use(&launch.data) {
        return Err(unavailable(copy_profile(&launch, "is in use")));
    }

    Ok(Target::Launch(launch))
}

/// A user data folder, and the profile named when the path is a profile inside one.
fn split_profile(path: &Path) -> (PathBuf, Option<String>) {
    let parent = path
        .parent()
        .filter(|parent| parent.join("Local State").is_file());
    match (parent, path.file_name()) {
        (Some(parent), Some(name)) if path.join("Preferences").is_file() => {
            (parent.to_owned(), Some(name.to_string_lossy().into_owned()))
        }
        _ => (path.to_owned(), None),
    }
}

/// The browser writes its debugging port to `DevToolsActivePort` and leaves it after a crash.
async fn debug_url(data: &Path) -> Option<String> {
    let text = std::fs::read_to_string(data.join("DevToolsActivePort")).ok()?;
    let port: u16 = text.lines().next()?.trim().parse().ok()?;
    answers_cdp(port)
        .await
        .then(|| format!("http://127.0.0.1:{port}"))
}

/// Whether a browser debugging endpoint answers on this machine's `port`.
async fn answers_cdp(port: u16) -> bool {
    let ask = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        let request = "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut reply = Vec::new();
        stream.take(16 * 1024).read_to_end(&mut reply).await.ok()?;
        Some(String::from_utf8_lossy(&reply).contains("webSocketDebuggerUrl"))
    };
    tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// A running browser holds `lockfile` open without sharing it.
#[cfg(windows)]
fn in_use(data: &Path) -> bool {
    let lock = data.join("lockfile");
    lock.exists() && std::fs::OpenOptions::new().write(true).open(&lock).is_err()
}

/// A running browser keeps the `SingletonLock` link; a crash can leave it behind.
#[cfg(not(windows))]
fn in_use(data: &Path) -> bool {
    data.join("SingletonLock").symlink_metadata().is_ok()
}

/// Chromium-based browsers found on this machine, in order of preference.
fn installed() -> Vec<Chromium> {
    let browser = |name, program: Option<PathBuf>, data: Option<PathBuf>| {
        let refuses_default = matches!(name, "Chrome" | "Edge");
        program
            .filter(|program| program.is_file())
            .map(|program| Chromium {
                name,
                program,
                data,
                refuses_default,
            })
    };
    let home = std::env::home_dir();
    let found: Vec<Option<Chromium>> = if cfg!(windows) {
        let env = |name| std::env::var_os(name).map(PathBuf::from);
        let local = env("LOCALAPPDATA");
        let roots = [env("ProgramFiles"), env("ProgramFiles(x86)"), local.clone()];
        // Joined part by part, so that paths in messages use one kind of slash.
        let under = |root: &PathBuf, relative: &str| {
            relative
                .split('/')
                .fold(root.clone(), |path, part| path.join(part))
        };
        let first = |relative: &str| {
            roots
                .iter()
                .flatten()
                .map(|root| under(root, relative))
                .find(|path| path.is_file())
        };
        let data = |relative: &str| local.as_ref().map(|local| under(local, relative));
        vec![
            browser(
                "Chrome",
                first("Google/Chrome/Application/chrome.exe"),
                data("Google/Chrome/User Data"),
            ),
            browser(
                "Brave",
                first("BraveSoftware/Brave-Browser/Application/brave.exe"),
                data("BraveSoftware/Brave-Browser/User Data"),
            ),
            browser(
                "Edge",
                first("Microsoft/Edge/Application/msedge.exe"),
                data("Microsoft/Edge/User Data"),
            ),
            browser(
                "Chromium",
                first("Chromium/Application/chrome.exe"),
                data("Chromium/User Data"),
            ),
        ]
    } else if cfg!(target_os = "macos") {
        let app = |name: &str| {
            Some(PathBuf::from(format!(
                "/Applications/{name}.app/Contents/MacOS/{name}"
            )))
        };
        let data = |relative: &str| {
            home.as_ref()
                .map(|home| home.join("Library/Application Support").join(relative))
        };
        vec![
            browser("Chrome", app("Google Chrome"), data("Google/Chrome")),
            browser(
                "Brave",
                app("Brave Browser"),
                data("BraveSoftware/Brave-Browser"),
            ),
            browser("Edge", app("Microsoft Edge"), data("Microsoft Edge")),
            browser("Chromium", app("Chromium"), data("Chromium")),
        ]
    } else {
        let first = |names: &[&str]| names.iter().find_map(|name| crate::setup::find(name));
        let data = |relative: &str| {
            home.as_ref()
                .map(|home| home.join(".config").join(relative))
        };
        vec![
            browser(
                "Chrome",
                first(&["google-chrome", "google-chrome-stable"]),
                data("google-chrome"),
            ),
            browser(
                "Brave",
                first(&["brave-browser", "brave"]),
                data("BraveSoftware/Brave-Browser"),
            ),
            browser(
                "Edge",
                first(&["microsoft-edge", "microsoft-edge-stable"]),
                data("microsoft-edge"),
            ),
            browser(
                "Chromium",
                first(&["chromium", "chromium-browser"]),
                data("chromium"),
            ),
        ]
    };
    found.into_iter().flatten().collect()
}

fn unavailable(message: impl Into<String>) -> ToolError {
    ToolError::new("browser_unavailable", message)
}

fn failed(message: impl Into<String>) -> ToolError {
    ToolError::new("render_failed", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_path_names_its_user_data_folder() {
        let root =
            std::env::temp_dir().join(format!("anyfetch-profile-test-{}", std::process::id()));
        let profile = root.join("Work");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(root.join("Local State"), "{}").unwrap();
        std::fs::write(profile.join("Preferences"), "{}").unwrap();
        assert_eq!(
            split_profile(&profile),
            (root.clone(), Some("Work".to_owned()))
        );
        assert_eq!(split_profile(&root), (root.clone(), None));
        assert!(!in_use(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_closed_port_is_no_browser() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!answers_cdp(port).await);
    }
}
