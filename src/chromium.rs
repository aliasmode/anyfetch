//! Fallback renderer: a Chromium-based browser driven over the Chrome DevTools Protocol, for pages
//! that need their scripts when Lightpanda is missing, and for pages that refuse plain requests.
//!
//! The browser is one already open for remote debugging, or one started on a profile. anyfetch
//! keeps one WebSocket to it and opens a tab per page. The profile and launch options can be
//! shared with the Playwright MCP server through `PLAYWRIGHT_USERDATA_DIR` and
//! `PLAYWRIGHT_MCP_CONFIG`. `ANYFETCH_CHROMIUM=off` turns the fallback off.

use crate::ToolError;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, LazyLock, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
use wreq::ws::{WebSocket, message::Message};

/// How long a started browser may take to open its debugging port.
const START: Duration = Duration::from_secs(30);
const PAGE: Duration = Duration::from_secs(45);
const LOAD: Duration = Duration::from_secs(30);
/// After the load event, how long to wait for the network to go quiet. Pages that keep a
/// connection open never do; what loaded by then is what they show.
const SETTLE: Duration = Duration::from_secs(8);
/// A failed start is not tried again for this long, so a batch of fetches fails fast.
const RETRY: Duration = Duration::from_secs(300);
const IDLE: Duration = Duration::from_secs(300);
const DEBUG_PORT: u16 = 9222;
/// Large enough for any page's HTML in one message.
const MAX_MESSAGE: usize = 512 << 20;
const STOPPED: &str = "The browser stopped";
const PAGE_SCRIPT: &str = "({ url: location.href, \
    status: performance.getEntriesByType('navigation')[0]?.responseStatus ?? 0, \
    html: document.documentElement ? document.documentElement.outerHTML : '' })";

/// A page as the browser loaded it.
pub struct Rendered {
    pub url: String,
    pub status: u16,
    pub html: String,
}

pub fn enabled() -> bool {
    std::env::var_os("ANYFETCH_CHROMIUM").is_none_or(|value| value != "off")
}

/// Closes a browser that anyfetch started, or leaves one it attached to. Call before the runtime
/// ends: a started browser outlives anyfetch otherwise, until a later run adopts it.
pub async fn shutdown() {
    let state = std::mem::replace(&mut *STATE.lock().await, State::Closed);
    if let State::Running(browser) = state {
        close(browser).await;
    }
}

/// Loads `url` in the browser and returns the page after its scripts ran.
pub async fn render(url: &str) -> Result<Rendered, ToolError> {
    // A task of its own, so that a fetch given up during the start does not orphan the browser.
    let browser = tokio::spawn(acquire())
        .await
        .map_err(|_| failed(STOPPED))??;
    tokio::time::timeout(PAGE, load(&browser.0, url))
        .await
        .map_err(|_| failed("The browser took longer than 45 seconds to load the page"))?
}

/// The running browser, started when there is none.
async fn acquire() -> Result<Use, ToolError> {
    let mut state = STATE.lock().await;
    if let State::Running(browser) = &*state
        && !browser.routes.open.load(Ordering::SeqCst)
    {
        let State::Running(browser) = std::mem::replace(&mut *state, State::Idle) else {
            unreachable!("the state was just matched");
        };
        close(browser).await;
    }
    if let State::Failed(at, error) = &*state {
        if at.elapsed() < RETRY {
            return Err(error.clone());
        }

        *state = State::Idle;
    }
    if matches!(*state, State::Closed) {
        return Err(failed("anyfetch is shutting down"));
    }

    if matches!(*state, State::Idle) {
        match start().await {
            Ok(browser) => {
                let browser = Arc::new(browser);
                tokio::spawn(close_when_idle(Arc::downgrade(&browser)));
                *state = State::Running(browser);
            }
            Err(error) => {
                // The client keeps a stdio server's stderr in its MCP logs, for the user.
                eprintln!("anyfetch: {}", error.message);
                *state = State::Failed(Instant::now(), error.clone());
                return Err(error);
            }
        }
    }
    let State::Running(browser) = &*state else {
        unreachable!("the browser was just started");
    };
    // Under the lock, so that the idle check never closes a browser about to be used.
    Ok(Use::begin(Arc::clone(browser)))
}

static STATE: Mutex<State> = Mutex::const_new(State::Idle);
static CLIENT: LazyLock<wreq::Client> = LazyLock::new(|| {
    wreq::Client::builder()
        .no_proxy()
        .build()
        .expect("a plain client builds")
});

enum State {
    Idle,
    Running(Arc<Browser>),
    Failed(Instant, ToolError),
    /// After [`shutdown`]: nothing starts a browser that would outlive anyfetch.
    Closed,
}

type Reply = Result<Value, String>;

/// What the connection task and the browser handle share.
#[derive(Default)]
struct Routes {
    waiting: StdMutex<HashMap<u64, oneshot::Sender<Reply>>>,
    /// Events of each open tab, by session.
    sessions: StdMutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    open: AtomicBool,
}

impl Routes {
    fn deliver(&self, text: &str) {
        let Ok(mut message) = serde_json::from_str::<Value>(text) else {
            return;
        };

        if let Some(id) = message["id"].as_u64() {
            let sender = self.waiting.lock().expect("waiting lock").remove(&id);
            let reply = match message.get("error") {
                Some(error) => Err(error["message"].as_str().unwrap_or("error").to_owned()),
                None => Ok(message["result"].take()),
            };
            if let Some(sender) = sender {
                let _ = sender.send(reply);
            }
            return;
        }

        let Some(session) = message["sessionId"].as_str().map(str::to_owned) else {
            return;
        };

        let sessions = self.sessions.lock().expect("sessions lock");
        if let Some(sender) = sessions.get(&session) {
            let _ = sender.send(message);
        }
    }
}

/// One connection to a browser.
struct Browser {
    outbox: mpsc::UnboundedSender<String>,
    routes: Arc<Routes>,
    next: AtomicU64,
    /// A browser that anyfetch started: it is closed with anyfetch. On Windows this is only the
    /// launcher, which exits once the browser runs on its own.
    child: StdMutex<Option<Child>>,
    /// Started by anyfetch, now or by an earlier run that left it (see [`Marker`]): closed when
    /// idle and with anyfetch. Any other browser is only disconnected.
    launched: bool,
    /// The browser process of a started browser, to end it when it does not close.
    pid: Option<u32>,
    /// When that process started, so that a reused process number is never ended.
    created: Option<u64>,
    /// The marker of a started browser, removed once the browser is gone.
    marker: Option<PathBuf>,
    connection: StdMutex<Option<JoinHandle<()>>>,
    pages: AtomicUsize,
    used: StdMutex<Instant>,
}

impl Browser {
    async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Reply {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = oneshot::channel();
        self.routes
            .waiting
            .lock()
            .expect("waiting lock")
            .insert(id, sender);
        // The connection clears `waiting` after it marks itself closed.
        if !self.routes.open.load(Ordering::SeqCst) || !self.send(id, method, params, session) {
            self.routes
                .waiting
                .lock()
                .expect("waiting lock")
                .remove(&id);
            return Err(STOPPED.to_owned());
        }
        receiver.await.unwrap_or_else(|_| Err(STOPPED.to_owned()))
    }

    /// Sends without waiting for the answer.
    fn send(&self, id: u64, method: &str, params: Value, session: Option<&str>) -> bool {
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        self.outbox.send(message.to_string()).is_ok()
    }

    fn idle(&self) -> bool {
        self.pages.load(Ordering::SeqCst) == 0
            && self.used.lock().expect("used lock").elapsed() >= IDLE
    }
}

/// A page being loaded: keeps the browser from closing as idle.
struct Use(Arc<Browser>);

impl Use {
    fn begin(browser: Arc<Browser>) -> Self {
        browser.pages.fetch_add(1, Ordering::SeqCst);
        *browser.used.lock().expect("used lock") = Instant::now();
        Self(browser)
    }
}

impl Drop for Use {
    fn drop(&mut self) {
        *self.0.used.lock().expect("used lock") = Instant::now();
        self.0.pages.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A tab opened for one page, closed when the load ends or is given up.
struct Tab<'a> {
    browser: &'a Browser,
    target: String,
    session: Option<String>,
}

impl Drop for Tab<'_> {
    fn drop(&mut self) {
        if let Some(session) = &self.session {
            let sessions = &self.browser.routes.sessions;
            sessions.lock().expect("sessions lock").remove(session);
        }
        let id = self.browser.next.fetch_add(1, Ordering::Relaxed) + 1;
        let params = json!({ "targetId": self.target });
        self.browser.send(id, "Target.closeTarget", params, None);
    }
}

async fn load(browser: &Browser, url: &str) -> Result<Rendered, ToolError> {
    let lost = |error: String| failed(format!("The browser could not load the page: {error}"));
    let created = browser
        .call("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .map_err(lost)?;
    let mut tab = Tab {
        browser,
        target: text(&created["targetId"]).ok_or_else(|| failed("The browser opened no tab"))?,
        session: None,
    };
    let params = json!({ "targetId": tab.target, "flatten": true });
    let attached = browser
        .call("Target.attachToTarget", params, None)
        .await
        .map_err(lost)?;
    let session =
        text(&attached["sessionId"]).ok_or_else(|| failed("The browser opened no tab"))?;
    let (sender, mut events) = mpsc::unbounded_channel();
    let sessions = &browser.routes.sessions;
    sessions
        .lock()
        .expect("sessions lock")
        .insert(session.clone(), sender);
    tab.session = Some(session.clone());
    let session = Some(session.as_str());
    // Network events give the status of a page that failed to load.
    tokio::try_join!(
        browser.call("Page.enable", json!({}), session),
        browser.call("Network.enable", json!({}), session),
        browser.call(
            "Page.setLifecycleEventsEnabled",
            json!({ "enabled": true }),
            session
        ),
    )
    .map_err(lost)?;
    let navigated = browser
        .call("Page.navigate", json!({ "url": url }), session)
        .await
        .map_err(lost)?;
    let loader = text(&navigated["loaderId"]).unwrap_or_default();
    if let Some(error) = text(&navigated["errorText"]) {
        // An error status with an empty page shows the browser's error page instead.
        let status = document_status(&mut events, &loader).await;
        if let Some(status) = status.filter(|status| *status >= 400) {
            return Ok(Rendered {
                url: url.to_owned(),
                status,
                html: String::new(),
            });
        }

        return Err(lost(error));
    }

    settle(&tab, &mut events, loader).await?;
    let expression = json!({ "expression": PAGE_SCRIPT, "returnByValue": true });
    let mut page = evaluate(browser, &tab, &mut events, expression.clone()).await;
    if page.is_err() {
        // A script that navigated again destroys the page being read; read its successor.
        tokio::time::sleep(Duration::from_secs(1)).await;
        page = evaluate(browser, &tab, &mut events, expression).await;
    }
    let page = page.map_err(lost)?;
    let value = &page["result"]["value"];
    let thrown = &page["exceptionDetails"];
    if value.is_null() && !thrown.is_null() {
        let reason = text(&thrown["exception"]["description"]).or_else(|| text(&thrown["text"]));
        return Err(lost(reason.unwrap_or_default()));
    }

    let html = text(&value["html"]).ok_or_else(|| failed("The browser returned no page"))?;
    let status = value["status"]
        .as_u64()
        .and_then(|status| status.try_into().ok());
    Ok(Rendered {
        url: text(&value["url"]).unwrap_or_else(|| url.to_owned()),
        status: status.filter(|status| *status > 0).unwrap_or(200),
        html,
    })
}

/// Reads the page, dismissing dialogs that would block the script meanwhile.
async fn evaluate(
    browser: &Browser,
    tab: &Tab<'_>,
    events: &mut mpsc::UnboundedReceiver<Value>,
    expression: Value,
) -> Reply {
    let call = browser.call("Runtime.evaluate", expression, tab.session.as_deref());
    tokio::pin!(call);
    loop {
        tokio::select! {
            reply = &mut call => return reply,
            event = events.recv() => match event {
                Some(event) => dismiss(tab, &event),
                None => return Err(STOPPED.to_owned()),
            },
        }
    }
}

/// Answers an `alert`, `confirm` or `prompt`, which holds the page until answered.
fn dismiss(tab: &Tab<'_>, event: &Value) {
    if event["method"] == "Page.javascriptDialogOpening" {
        let id = tab.browser.next.fetch_add(1, Ordering::Relaxed) + 1;
        let params = json!({ "accept": false });
        let session = tab.session.as_deref();
        tab.browser
            .send(id, "Page.handleJavaScriptDialog", params, session);
    }
}

/// Waits for the page to load, then for its network to go quiet. A page that the main frame
/// navigates to on its own, such as after a redirect by script, starts the wait again.
async fn settle(
    tab: &Tab<'_>,
    events: &mut mpsc::UnboundedReceiver<Value>,
    mut loader: String,
) -> Result<(), ToolError> {
    let now = tokio::time::Instant::now;
    // Pages that keep navigating get read as they are by then.
    let end = now() + LOAD + SETTLE;
    let mut deadline = now() + LOAD;
    let (mut ready, mut loaded, mut quiet, mut shown) = (false, false, false, false);
    loop {
        let event = match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => return Err(failed(STOPPED)),
            Err(_) if loaded || ready || shown => return Ok(()),
            Err(_) => {
                return Err(failed(
                    "The browser took longer than 30 seconds to load the page",
                ));
            }
        };
        let frame = &event["params"]["frame"];
        match event["method"].as_str() {
            Some("Inspector.targetCrashed") => return Err(failed("The browser tab crashed")),
            Some("Inspector.detached") => return Err(failed("The browser closed the tab")),
            Some("Page.javascriptDialogOpening") => {
                dismiss(tab, &event);
                continue;
            }
            // The main frame's id is the tab's.
            Some("Page.frameNavigated")
                if frame["id"] == tab.target.as_str() && frame["loaderId"] != loader.as_str() =>
            {
                loader = text(&frame["loaderId"]).unwrap_or_default();
                shown |= ready;
                (ready, loaded, quiet) = (false, false, false);
                deadline = (now() + LOAD).min(end);
                continue;
            }
            Some("Page.lifecycleEvent") if event["params"]["loaderId"] == loader.as_str() => {}
            _ => continue,
        }
        match event["params"]["name"].as_str() {
            Some("DOMContentLoaded") => ready = true,
            Some("load") if quiet => return Ok(()),
            Some("load") => {
                loaded = true;
                deadline = (now() + SETTLE).min(end);
            }
            Some("networkIdle") if loaded => return Ok(()),
            Some("networkIdle") => quiet = true,
            _ => {}
        }
    }
}

/// The HTTP status of the page that `loader` requested, from events already sent or due.
async fn document_status(events: &mut mpsc::UnboundedReceiver<Value>, loader: &str) -> Option<u16> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .ok()??;
        let params = &event["params"];
        if event["method"] == "Network.responseReceived" && params["requestId"] == loader {
            return params["response"]["status"].as_u64()?.try_into().ok();
        }
    }
}

/// Closes the browser once no page used it for [`IDLE`].
async fn close_when_idle(browser: Weak<Browser>) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let Some(strong) = browser.upgrade() else {
            return;
        };

        if !strong.idle() {
            continue;
        }

        let mut state = STATE.lock().await;
        let current = matches!(&*state, State::Running(running) if Arc::ptr_eq(running, &strong));
        if !current {
            return;
        }

        if !strong.idle() {
            continue;
        }

        *state = State::Idle;
        // Under the lock: a render meanwhile would find the closing browser and its profile.
        close(strong).await;
        return;
    }
}

/// Closes a browser that anyfetch started and disconnects.
async fn close(browser: Arc<Browser>) {
    let connection = browser.connection.lock().expect("connection lock").take();
    let Some(mut connection) = connection else {
        return;
    };

    if browser.launched {
        let ask = browser.call("Browser.close", json!({}), None);
        let asked = tokio::time::timeout(Duration::from_secs(5), ask).await;
        // The browser drops the connection as it exits.
        let ended = tokio::time::timeout(Duration::from_secs(5), &mut connection).await;
        // A connection that failed on its own, or a browser that stays, leaves only the process.
        if (ended.is_err() || matches!(asked, Ok(Err(_))))
            && let Some(pid) = browser.pid
            && same_process(pid, browser.created)
        {
            terminate(pid);
        }
        let child = browser.child.lock().expect("child lock").take();
        if let Some(child) = child {
            reap(child).await;
        }
        if let Some(marker) = &browser.marker {
            gone(marker, browser.pid).await;
        }
        if ended.is_ok() {
            return;
        }
    }
    // Ends the connection once pending messages, such as closing tabs, went out.
    drop(browser);
    let _ = tokio::time::timeout(Duration::from_secs(2), connection).await;
}

/// Removes the marker once the browser process has exited, waiting for it a little.
async fn gone(marker: &Path, pid: Option<u32>) {
    for _ in 0..20 {
        if !pid.is_some_and(alive) {
            let _ = std::fs::remove_file(marker);
            return;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Ends the browser process: a started browser that does not close, or one adopted from an
/// earlier run, which is no child of this one.
#[cfg(windows)]
fn terminate(pid: u32) {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
    };
    // SAFETY: the handle is checked and closed; a process that already exited fails to open.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !process.is_null() {
            TerminateProcess(process, 1);
            CloseHandle(process);
        }
    }
}

#[cfg(not(windows))]
fn terminate(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    if pid > 0 {
        // SAFETY: sends a signal to one process; one that already exited fails with ESRCH.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Whether a process runs. One that cannot be checked counts as running.
#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    // SAFETY: the handle is checked and closed; the exit code is written to a local.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0;
        let read = GetExitCodeProcess(process, &mut code) != 0;
        CloseHandle(process);
        !read || code == STILL_ACTIVE as u32
    }
}

/// Whether a process runs. EPERM means it runs as another user.
#[cfg(not(windows))]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// When a process started, in milliseconds since 1970, where the system says.
#[cfg(windows)]
fn started_at(pid: u32) -> Option<u64> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    let zero = || FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
    // SAFETY: the handle is checked and closed; the times are written to locals.
    let read = unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let read = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(process);
        read != 0
    };
    // 100-nanosecond steps since 1601.
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    (read && ticks > 0).then(|| (ticks / 10_000).saturating_sub(11_644_473_600_000))
}

#[cfg(target_os = "linux")]
fn started_at(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name in parentheses may hold spaces; the start time is field 22, in clock ticks.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: u64 = fields.get(19)?.parse().ok()?;
    let boot = std::fs::read_to_string("/proc/stat").ok()?;
    let boot: u64 = boot
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    // SAFETY: sysconf only reads a system setting.
    let per_second = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
    (per_second > 0).then(|| boot * 1000 + ticks * 1000 / per_second)
}

#[cfg(target_os = "macos")]
fn started_at(pid: u32) -> Option<u64> {
    // SAFETY: `proc_bsdinfo` is plain integers and byte arrays, valid as zeros.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is a `proc_bsdinfo` of the size given.
    let read = unsafe {
        libc::proc_pidinfo(
            pid.try_into().ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    (read == size).then(|| info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn started_at(_pid: u32) -> Option<u64> {
    None
}

const MARKER: &str = "anyfetch-browser.json";

/// Left by anyfetch in the user data folder of a browser it started, so that a later run that
/// finds the browser still open, after this one was killed, knows to close it. Without the
/// browser's process and port, the start did not finish, and a browser that started then counts.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Marker {
    /// Milliseconds since 1970 when anyfetch ran the browser.
    started: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    /// The anyfetch process that uses the browser, and when it started: while it runs, another
    /// anyfetch on the same profile only attaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_started: Option<u64>,
}

/// How long after the browser was run a browser that started counts as that one.
const PENDING_MS: u64 = START.as_millis() as u64 + 60_000;

impl Marker {
    /// A marker owned by this process.
    fn new(started: u64, pid: Option<u32>, port: Option<u16>) -> Self {
        let owner = std::process::id();
        Self {
            started,
            pid,
            port,
            owner: Some(owner),
            owner_started: started_at(owner),
        }
    }

    /// Whether the anyfetch that wrote the marker is gone, or is this one.
    fn orphaned(&self) -> bool {
        self.owner.is_none_or(|owner| {
            owner == std::process::id() || !same_process(owner, self.owner_started)
        })
    }

    /// Whether the browser with process `pid`, debugging port `port` and start time `created`
    /// is the one this marker names.
    fn names(&self, pid: u32, port: u16, created: Option<u64>) -> bool {
        match (self.pid, self.port) {
            (Some(own), Some(own_port)) => own == pid && own_port == port,
            _ => created.is_some_and(|created| {
                created + 1000 >= self.started && created <= self.started + PENDING_MS
            }),
        }
    }

    /// A marker whose browser is gone. One without a process names a browser by its start time,
    /// which stays the same, so it stays until the next start replaces it.
    fn stale(&self) -> bool {
        self.pid.is_some_and(|pid| !alive(pid))
    }
}

/// Whether `pid` still is the process that started at `started`, when that is known: a process
/// number can be reused once its process exited.
fn same_process(pid: u32, started: Option<u64>) -> bool {
    alive(pid)
        && match (started, started_at(pid)) {
            (Some(then), Some(now)) => then.abs_diff(now) < 2000,
            _ => true,
        }
}

fn now_ms() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |since| since.as_millis() as u64)
}

fn write_marker(data: &Path, marker: &Marker) {
    let text = serde_json::to_string(marker).expect("a marker serializes");
    let _ = std::fs::write(data.join(MARKER), text);
}

/// The marker in `data`, after removing one that is stale or unreadable.
fn read_marker(data: &Path) -> Option<Marker> {
    let path = data.join(MARKER);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Marker>(&text) {
        Ok(marker) if !marker.stale() => Some(marker),
        _ => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// Waits for a started browser to exit, and kills it if it does not.
async fn reap(mut child: Child) {
    for _ in 0..50 {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
}

enum Target {
    /// A browser that already listens for remote debugging, by its HTTP or WebSocket endpoint.
    Attach(String),
    /// A browser found through `DevToolsActivePort` in its user data folder, on this port.
    Found(String, PathBuf, u16),
    Launch(Launch),
}

struct Launch {
    name: &'static str,
    program: PathBuf,
    /// The user data folder, and the profile inside it when one other than the default was named.
    data: PathBuf,
    profile: Option<String>,
    /// `launchOptions` from the shared config.
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

async fn start() -> Result<Browser, ToolError> {
    let target = target().await?;
    let started = match &target {
        Target::Attach(endpoint) | Target::Found(endpoint, ..) => match debugger(endpoint).await {
            // Only a browser found in a profile folder can be one that anyfetch started.
            Some(socket) => connect(&socket, None, matches!(target, Target::Found(..))).await,
            None => Err("it does not answer".to_owned()),
        },
        Target::Launch(launch) => launch_browser(launch).await,
    };
    let mut browser = started.map_err(|reason| start_failed(&target, &reason))?;
    if let Target::Found(_, data, port) = &target {
        adopt(&mut browser, data, *port);
    }
    Ok(browser)
}

/// Takes over a browser that an earlier run of anyfetch started and left open, as its marker
/// says, so that it closes like one started now. One that a running anyfetch uses stays its own.
fn adopt(browser: &mut Browser, data: &Path, port: u16) {
    let (Some(marker), Some(pid)) = (read_marker(data), browser.pid) else {
        return;
    };

    if marker.orphaned() && marker.names(pid, port, browser.created) {
        browser.launched = true;
        browser.marker = Some(data.join(MARKER));
        // Its own process and port, and this run as the owner, for when this run is killed too.
        write_marker(data, &Marker::new(marker.started, Some(pid), Some(port)));
    }
}

/// Starts the browser with a debugging port of its choosing, which it writes to
/// `DevToolsActivePort`. Its output is not read: on Windows the launcher hands off to a browser
/// process that keeps no pipe to anyfetch.
async fn launch_browser(launch: &Launch) -> Result<Browser, String> {
    let port_file = launch.data.join("DevToolsActivePort");
    // Left by an earlier run; the profile is not in use, so the file is stale.
    let _ = std::fs::remove_file(&port_file);
    // Before the browser runs: a start that is given up may leave a browser that opens its port
    // later, and a later run adopts it.
    let started = now_ms();
    write_marker(&launch.data, &Marker::new(started, None, None));
    let mut child = std::process::Command::new(&launch.program)
        .args(launch_args(launch))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", launch.program.display()))?;
    let deadline = Instant::now() + START;
    let (port, path) = loop {
        let found = std::fs::read_to_string(&port_file).ok();
        if let Some(found) = found.as_deref().and_then(active_port) {
            break found;
        }
        if !cfg!(windows)
            && let Ok(Some(status)) = child.try_wait()
        {
            return Err(format!("it exited ({status})"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err("it did not open a debugging port within 30 seconds".to_owned());
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let url = format!("ws://127.0.0.1:{port}{path}");
    let mut browser = connect(&url, Some(child), true).await?;
    browser.marker = Some(launch.data.join(MARKER));
    if let Some(pid) = browser.pid {
        write_marker(&launch.data, &Marker::new(started, Some(pid), Some(port)));
    }
    Ok(browser)
}

fn launch_args(launch: &Launch) -> Vec<String> {
    let options = launch.options.as_ref().unwrap_or(&Value::Null);
    let mut args = Vec::new();
    if options["headless"].as_bool() != Some(false) {
        args.push("--headless=new".to_owned());
    }
    args.push("--no-first-run".to_owned());
    args.push("--no-default-browser-check".to_owned());
    let extra: Vec<String> = options["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arg| arg.as_str().map(str::to_owned))
        .collect();
    if let Some(profile) = &launch.profile
        && !extra
            .iter()
            .any(|arg| arg.starts_with("--profile-directory="))
    {
        args.push(format!("--profile-directory={profile}"));
    }
    args.extend(extra);
    // Last, so that they win over the same switches in the config.
    args.push(format!("--user-data-dir={}", launch.data.display()));
    args.push("--remote-debugging-port=0".to_owned());
    args.push("about:blank".to_owned());
    args
}

/// The port and browser path in a `DevToolsActivePort` file.
fn active_port(text: &str) -> Option<(u16, String)> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    (port != 0 && path.starts_with("/devtools/browser/")).then(|| (port, path.to_owned()))
}

/// With `identify`, learns the browser's process, to close a started browser that stays and to
/// tell whether an earlier run started a browser found in a profile folder.
async fn connect(url: &str, child: Option<Child>, identify: bool) -> Result<Browser, String> {
    let open = async {
        CLIENT
            .websocket(url)
            .max_frame_size(MAX_MESSAGE)
            .max_message_size(MAX_MESSAGE)
            .send()
            .await?
            .into_websocket()
            .await
    };
    let socket = match tokio::time::timeout(Duration::from_secs(10), open).await {
        Ok(Ok(socket)) => socket,
        failure => {
            if let Some(mut child) = child {
                let _ = child.kill();
            }
            return Err(match failure {
                Ok(Err(error)) => format!("could not connect to {url}: {error}"),
                _ => format!("could not connect to {url} within 10 seconds"),
            });
        }
    };
    let routes = Arc::new(Routes::default());
    routes.open.store(true, Ordering::SeqCst);
    let (outbox, inbox) = mpsc::unbounded_channel();
    let connection = tokio::spawn(run(socket, inbox, Arc::clone(&routes)));
    let mut browser = Browser {
        outbox,
        routes,
        next: AtomicU64::new(0),
        launched: child.is_some(),
        child: StdMutex::new(child),
        pid: None,
        created: None,
        marker: None,
        connection: StdMutex::new(Some(connection)),
        pages: AtomicUsize::new(0),
        used: StdMutex::new(Instant::now()),
    };
    if identify {
        let ask = browser.call("SystemInfo.getProcessInfo", json!({}), None);
        if let Ok(Ok(info)) = tokio::time::timeout(Duration::from_secs(5), ask).await {
            browser.pid = browser_pid(&info);
            browser.created = browser.pid.and_then(started_at);
        }
    }
    Ok(browser)
}

fn browser_pid(info: &Value) -> Option<u32> {
    let processes = info["processInfo"].as_array()?;
    let process = processes
        .iter()
        .find(|process| process["type"] == "browser")?;
    process["id"].as_u64()?.try_into().ok()
}

/// Sends what the browser handle queues, and routes what the browser answers.
async fn run(
    mut socket: WebSocket,
    mut inbox: mpsc::UnboundedReceiver<String>,
    routes: Arc<Routes>,
) {
    loop {
        tokio::select! {
            outgoing = inbox.recv() => {
                let Some(text) = outgoing else {
                    break;
                };

                if socket.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => routes.deliver(&text),
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }
    routes.open.store(false, Ordering::SeqCst);
    // Dropping the senders tells every open request and tab that the browser stopped.
    routes.waiting.lock().expect("waiting lock").clear();
    routes.sessions.lock().expect("sessions lock").clear();
}

/// The browser's WebSocket address, from its HTTP endpoint, when it answers.
async fn debugger(endpoint: &str) -> Option<String> {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Some(endpoint.to_owned());
    }

    let address = format!("{}/json/version", endpoint.trim_end_matches('/'));
    let ask = async { CLIENT.get(address).send().await.ok()?.text().await.ok() };
    let body = tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .ok()
        .flatten()?;
    let version: Value = serde_json::from_str(&body).ok()?;
    text(&version["webSocketDebuggerUrl"])
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
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
        Target::Attach(url) | Target::Found(url, ..) => unavailable(format!(
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
         on the copy. Set ANYFETCH_CHROMIUM=off to turn the fallback off.",
        name = launch.name,
        data = launch.data.display(),
    )
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
    Ok(parse_shared(&config))
}

fn parse_shared(config: &Value) -> Shared {
    let browser = &config["browser"];
    Shared {
        cdp: text(&browser["cdpEndpoint"]),
        data: text(&browser["userDataDir"]).map(PathBuf::from),
        program: text(&browser["launchOptions"]["executablePath"]).map(PathBuf::from),
        launch: Some(browser["launchOptions"].clone()).filter(Value::is_object),
    }
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
        if let Some((url, port)) = debug_url(&folder).await {
            return Ok(Target::Found(url, folder, port));
        }
    }
    let local = format!("http://127.0.0.1:{DEBUG_PORT}");
    if debugger(&local).await.is_some() {
        return Ok(Target::Attach(local));
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
                "The profile in PLAYWRIGHT_USERDATA_DIR ({}) is open in another browser. Close it \
                 and try again, or start it with --remote-debugging-port={DEBUG_PORT} so that \
                 anyfetch attaches to it.",
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
async fn debug_url(data: &Path) -> Option<(String, u16)> {
    let text = std::fs::read_to_string(data.join("DevToolsActivePort")).ok()?;
    let (port, _) = active_port(&text)?;
    let url = format!("http://127.0.0.1:{port}");
    debugger(&url).await.map(|_| (url, port))
}

/// A running browser holds `lockfile` open without sharing it.
#[cfg(windows)]
fn in_use(data: &Path) -> bool {
    let lock = data.join("lockfile");
    lock.exists() && std::fs::OpenOptions::new().write(true).open(&lock).is_err()
}

/// A running browser keeps the `SingletonLock` link to `<host>-<pid>`. A crash leaves it behind,
/// and the browser then removes it itself, so only a live process, or another machine's, counts.
#[cfg(not(windows))]
fn in_use(data: &Path) -> bool {
    let lock = data.join("SingletonLock");
    if lock.symlink_metadata().is_err() {
        return false;
    }

    std::fs::read_link(&lock).map_or(true, |target| {
        lock_held(&target.to_string_lossy(), &hostname())
    })
}

/// Whether the owner `<host>-<pid>` of a lock is a live process on `host`. An owner that cannot
/// be read counts as live, so a profile is never opened twice.
#[cfg(not(windows))]
fn lock_held(owner: &str, host: &str) -> bool {
    let Some((owner_host, pid)) = owner.rsplit_once('-') else {
        return true;
    };
    let Ok(pid) = pid.parse::<i64>() else {
        return true;
    };
    if owner_host != host || pid <= 0 {
        return true;
    }

    u32::try_from(pid).map_or(true, alive)
}

#[cfg(not(windows))]
fn hostname() -> String {
    let mut name = [0u8; 256];
    // SAFETY: the buffer is writable for its whole length; the name ends at the first NUL.
    let named = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } == 0;
    let end = name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len());
    if named {
        String::from_utf8_lossy(&name[..end]).into_owned()
    } else {
        String::new()
    }
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
#[path = "chromium.test.rs"]
mod tests;
