use super::*;

#[cfg(not(windows))]
#[test]
fn only_a_live_lock_owner_holds_the_profile() {
    let host = hostname();
    let own = format!("{host}-{}", std::process::id());
    assert!(lock_held(&own, &host));
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let gone = child.id();
    child.wait().unwrap();
    assert!(!lock_held(&format!("{host}-{gone}"), &host));
    assert!(lock_held(&format!("other-machine-{gone}"), &host));
    assert!(lock_held("garbage", &host));

    let data = std::env::temp_dir().join(format!("anyfetch-lock-test-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    assert!(!in_use(&data));
    std::os::unix::fs::symlink(format!("{host}-{gone}"), data.join("SingletonLock")).unwrap();
    assert!(!in_use(&data));
    std::fs::remove_file(data.join("SingletonLock")).unwrap();
    std::os::unix::fs::symlink(&own, data.join("SingletonLock")).unwrap();
    assert!(in_use(&data));
    std::fs::remove_dir_all(&data).unwrap();
}

fn exited_pid() -> u32 {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/c", "exit"]);
        command
    } else {
        std::process::Command::new("true")
    };
    let mut child = command.spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn a_marker_names_only_its_own_browser() {
    let marker = Marker::new(1_000_000, Some(4242), Some(53124));
    assert!(marker.names(4242, 53124, None));
    // Another browser on the profile, or the same process number on another port.
    assert!(!marker.names(4243, 53124, None));
    assert!(!marker.names(4242, 9222, Some(1_000_500)));
    // A start that did not finish names the browser that started right after it.
    let pending = Marker::new(1_000_000, None, None);
    assert!(pending.names(7, 9222, Some(1_000_500)));
    assert!(pending.names(7, 9222, Some(999_500)));
    assert!(!pending.names(7, 9222, Some(900_000)));
    assert!(!pending.names(7, 9222, Some(1_000_000 + PENDING_MS + 1)));
    assert!(!pending.names(7, 9222, None));
}

#[test]
fn a_marker_of_a_gone_browser_is_stale_and_removed() {
    let data =
        std::env::temp_dir().join(format!("anyfetch-marker-test-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    assert_eq!(read_marker(&data), None);
    let live = Marker::new(now_ms(), Some(std::process::id()), Some(53124));
    write_marker(&data, &live);
    assert_eq!(read_marker(&data), Some(live));
    let pending = Marker::new(1, None, None);
    write_marker(&data, &pending);
    assert_eq!(read_marker(&data), Some(pending));
    let gone = Marker::new(now_ms(), Some(exited_pid()), Some(53124));
    write_marker(&data, &gone);
    assert_eq!(read_marker(&data), None);
    assert!(!data.join(MARKER).exists());
    std::fs::write(data.join(MARKER), "not json").unwrap();
    assert_eq!(read_marker(&data), None);
    assert!(!data.join(MARKER).exists());
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn only_a_browser_whose_anyfetch_is_gone_is_adopted() {
    let mut marker = Marker::new(now_ms(), Some(4242), Some(53124));
    // Written by this run, as after a start that timed out.
    assert!(marker.orphaned());
    // Another anyfetch that still runs keeps its browser.
    let mut other = if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/c", "ping -n 30 127.0.0.1 >nul"]);
        command
    } else {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        command
    };
    let mut other = other.spawn().unwrap();
    marker.owner = Some(other.id());
    marker.owner_started = started_at(other.id());
    assert!(!marker.orphaned());
    // The same number, now another process.
    marker.owner_started = marker.owner_started.map(|started| started - 60_000);
    assert_eq!(marker.orphaned(), marker.owner_started.is_some());
    other.kill().unwrap();
    other.wait().unwrap();
    marker.owner = Some(exited_pid());
    assert!(marker.orphaned());
    marker.owner = None;
    assert!(marker.orphaned());
}

#[test]
fn tells_live_processes_and_their_start() {
    assert!(alive(std::process::id()));
    assert!(!alive(exited_pid()));
    if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        let started = started_at(std::process::id()).unwrap();
        assert!(started <= now_ms() + 1000, "{started}");
        assert!(started + 24 * 3600 * 1000 > now_ms(), "{started}");
    }
}

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
    assert!(
        debugger(&format!("http://127.0.0.1:{port}"))
            .await
            .is_none()
    );
    let socket = "ws://127.0.0.1:1/devtools/browser/x";
    assert_eq!(debugger(socket).await.as_deref(), Some(socket));
}

#[test]
fn reads_the_debugging_port_file() {
    let text = "53124\n/devtools/browser/0b1c-77\n";
    assert_eq!(
        active_port(text),
        Some((53124, "/devtools/browser/0b1c-77".to_owned()))
    );
    assert_eq!(
        active_port("53124\r\n/devtools/browser/a\r\n").unwrap().0,
        53124
    );
    // Half written, or not a port.
    assert_eq!(active_port("53124\n"), None);
    assert_eq!(active_port("0\n/devtools/browser/a"), None);
    assert_eq!(active_port("port\n/devtools/browser/a"), None);
}

#[test]
fn reads_the_shared_playwright_config() {
    let config = json!({ "browser": {
        "cdpEndpoint": "",
        "userDataDir": "T:/Profiles",
        "launchOptions": { "executablePath": "C:/chrome.exe", "headless": false },
    }});
    let shared = parse_shared(&config);
    assert_eq!(shared.cdp, None);
    assert_eq!(shared.data, Some(PathBuf::from("T:/Profiles")));
    assert_eq!(shared.program, Some(PathBuf::from("C:/chrome.exe")));
    assert!(shared.launch.is_some());
    let empty = parse_shared(&json!({}));
    assert!(empty.cdp.is_none() && empty.data.is_none() && empty.launch.is_none());
}

#[test]
fn finds_the_browser_process() {
    let info = json!({ "processInfo": [
        { "type": "renderer", "id": 7 },
        { "type": "browser", "id": 4242 },
    ]});
    assert_eq!(browser_pid(&info), Some(4242));
    assert_eq!(browser_pid(&json!({})), None);
}

fn launch(profile: Option<&str>, options: Option<Value>) -> Launch {
    Launch {
        name: "Chrome",
        program: PathBuf::from("chrome"),
        data: PathBuf::from("data"),
        profile: profile.map(str::to_owned),
        options,
        default: false,
    }
}

#[test]
fn launches_headless_unless_the_config_says_not() {
    let args = launch_args(&launch(Some("Work"), None));
    assert_eq!(
        args,
        [
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--profile-directory=Work",
            "--user-data-dir=data",
            "--remote-debugging-port=0",
            "about:blank",
        ]
    );
    let options =
        json!({ "headless": false, "args": ["--profile-directory=Other", "--lang=de"] });
    let args = launch_args(&launch(Some("Work"), Some(options)));
    assert!(!args.iter().any(|arg| arg.starts_with("--headless")));
    assert!(!args.contains(&"--profile-directory=Work".to_owned()));
    assert!(args.contains(&"--profile-directory=Other".to_owned()));
    assert!(args.contains(&"--lang=de".to_owned()));
}
