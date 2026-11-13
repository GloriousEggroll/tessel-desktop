//! Launch and mpvpaper supervision with FAKE `systemd-run`, terminals and
//! `mpvpaper` written into a private bin directory under TMPDIR. Nothing real
//! is started; nothing outside TMPDIR is touched.
#![cfg(feature = "desktop")]

use gio::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_shell::desktop::launch;
use tessel_shell::desktop::wallpaper::{MpvSupervisor, StartError};

fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir();
    assert!(!base.starts_with("/tmp") || std::env::var_os("TMPDIR").is_some(), "set TMPDIR");
    let dir = base.join(format!("ws-desk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    dir
}

fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join("bin").join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn logging_fake(dir: &Path, name: &str, exit: i32) -> PathBuf {
    let log = dir.join(format!("{name}.log"));
    fake(dir, name, &format!("for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nprintf 'cwd=%s\\n' \"$PWD\" >> '{}'\nprintf 'ENV_MARK=%s\\n' \"$ENV_MARK\" >> '{}'\necho boom >&2\nexit {exit}", log.display(), log.display(), log.display()))
}

fn wait(context: &glib::MainContext, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while !done() && Instant::now() < end {
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
    done()
}

#[test]
fn launches_leave_the_cgroup_through_systemd_run() {
    let dir = scratch("launch");
    let systemd_run = logging_fake(&dir, "systemd-run", 0);
    let context = glib::MainContext::new();
    context.with_thread_default(|| {
        launch::set_systemd_run_path(Some(systemd_run.clone()));
        launch::set_helper_prefix(Some(vec!["/opt/tessel-shell".into()]));
        let mut environment: BTreeMap<String, String> = BTreeMap::new();
        environment.insert("PATH".into(), format!("{}:/usr/bin:/bin", dir.join("bin").display()));
        environment.insert("ENV_MARK".into(), "kept".into());
        environment.insert("INVOCATION_ID".into(), "service-only".into());

        // launch_process: Type=exec, working directory, --setenv names only.
        let result: Rc<RefCell<Option<Option<String>>>> = Rc::default();
        let r = result.clone();
        launch::launch_process(&["ptyxis".into(), "--new-window".into()], Some(&environment), Some("/home/u"), Some(Box::new(move |e| { r.replace(Some(e)); }))).unwrap();
        assert!(wait(&context, || result.borrow().is_some()));
        assert_eq!(*result.borrow(), Some(None));
        let log = std::fs::read_to_string(dir.join("systemd-run.log")).unwrap();
        let argv: Vec<&str> = log.lines().collect();
        assert_eq!(argv[..4], ["--user", "--collect", "--quiet", "--service-type=exec"]);
        assert_eq!(argv[4..9], ["--property=GuessMainPID=no", "--property=ExitType=cgroup", "--property=TimeoutStartSec=90s", "--slice=app.slice", "--expand-environment=no"]);
        assert!(argv[9].starts_with("--unit=app-tessel-dock-") && argv[9].ends_with(".service"));
        assert_eq!(argv[10..15], ["--working-directory=/home/u", "--setenv=ENV_MARK", "--setenv=PATH", "--", "ptyxis"]);
        assert!(!log.contains("INVOCATION_ID"), "service metadata must not leak into the app");
        assert!(log.contains("ENV_MARK=kept"), "systemd-run receives exactly the launch environment");

        // Terminal: first candidate on the launch PATH, HOME cwd.
        fake(&dir, "ptyxis", "exit 0");
        let resolved = launch::resolve_terminal(&environment, "/home/u").unwrap();
        assert_eq!(resolved.0, ["ptyxis", "--new-window", "--working-directory", "/home/u"]);
        let mut with_terminal = environment.clone();
        fake(&dir, "foot", "exit 0");
        with_terminal.insert("TERMINAL".into(), "foot".into());
        assert_eq!(launch::resolve_terminal(&with_terminal, "/h").unwrap().0, ["foot"]);
        // $TERMINAL with arguments (a Flatpak, or flags) as lab-sensible-terminal runs it.
        fake(&dir, "kitty", "exit 0");
        fake(&dir, "flatpak", "exit 0");
        with_terminal.insert("TERMINAL".into(), "kitty --class Chosen".into());
        assert_eq!(launch::resolve_terminal(&with_terminal, "/h").unwrap().0, ["kitty", "--class", "Chosen", "--directory", "/h"]);
        with_terminal.insert("TERMINAL".into(), "flatpak run --command=wezterm org.wezfurlong.wezterm".into());
        let (argv, exe) = launch::resolve_terminal(&with_terminal, "/h").unwrap();
        assert_eq!(argv, ["flatpak", "run", "--command=wezterm", "org.wezfurlong.wezterm"]);
        assert_eq!(exe, dir.join("bin/flatpak"));
        // Without $TERMINAL, xdg-terminal-exec (it reads xdg-terminals.list) comes before the fixed list.
        let xdg = fake(&dir, "xdg-terminal-exec", "exit 0");
        assert_eq!(launch::resolve_terminal(&environment, "/home/u").unwrap(), (vec!["xdg-terminal-exec".to_owned()], xdg.clone()));
        std::fs::remove_file(&xdg).unwrap();
        let mut empty = environment.clone();
        empty.insert("PATH".into(), dir.join("nothing").display().to_string());
        assert!(launch::resolve_terminal(&empty, "/h").is_none());
        assert!(launch::open_default_terminal(&empty, "/h", |_| {}).is_err());
        let done: Rc<RefCell<Option<Option<String>>>> = Rc::default();
        let d = done.clone();
        launch::open_default_terminal(&environment, "/home/u", move |e| { d.replace(Some(e)); }).unwrap();
        assert!(wait(&context, || done.borrow().is_some()));
        let log = std::fs::read_to_string(dir.join("systemd-run.log")).unwrap();
        assert!(log.contains("--working-directory=/home/u\n") && log.contains("--\nptyxis\n--new-window\n--working-directory\n/home/u\n"));

        // URIs and .desktop files go through the in-service helper (Type=forking).
        let desktop = dir.join("An App.desktop");
        std::fs::write(&desktop, "[Desktop Entry]\nType=Application\nName=An App\nExec=true\n").unwrap();
        let done: Rc<RefCell<Option<Option<String>>>> = Rc::default();
        let d = done.clone();
        let gio_context = gio::AppLaunchContext::new();
        gio_context.setenv("ENV_MARK", "from-context");
        launch::launch_desktop_file(desktop.to_str().unwrap(), Some(&gio_context), Some(Box::new(move |e| { d.replace(Some(e)); }))).unwrap();
        assert!(wait(&context, || done.borrow().is_some()));
        let log = std::fs::read_to_string(dir.join("systemd-run.log")).unwrap();
        assert!(log.contains("--service-type=forking\n"));
        assert!(log.contains(&format!("--\n/opt/tessel-shell\n--launch-file\n{}\n", desktop.display())));
        assert!(log.contains("ENV_MARK=from-context"));
        assert!(launch::launch_desktop_file(dir.join("missing.desktop").to_str().unwrap(), None, None).is_err());

        let done: Rc<RefCell<Option<Option<String>>>> = Rc::default();
        let d = done.clone();
        launch::launch_uri("file:///tmp/a%20picture.png", Some(&gio_context), Some(Box::new(move |e| { d.replace(Some(e)); }))).unwrap();
        assert!(wait(&context, || done.borrow().is_some()));
        let log = std::fs::read_to_string(dir.join("systemd-run.log")).unwrap();
        assert!(log.contains("--\n/opt/tessel-shell\n--open-uri\nfile:///tmp/a%20picture.png\n"));

        // A failing service start reports stderr with Python's prefix.
        let failing = logging_fake(&dir, "systemd-run-fail", 3);
        launch::set_systemd_run_path(Some(failing));
        let done: Rc<RefCell<Option<Option<String>>>> = Rc::default();
        let d = done.clone();
        launch::launch_process(&["x".into()], Some(&environment), None, Some(Box::new(move |e| { d.replace(Some(e)); }))).unwrap();
        assert!(wait(&context, || done.borrow().is_some()));
        assert_eq!(*done.borrow(), Some(Some("Could not launch the application: boom".to_owned())));
        assert!(launch::launch_process(&[], None, None, None).is_err());
        launch::set_systemd_run_path(None);
        launch::set_helper_prefix(None);
    })
    .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mpvpaper_is_supervised_with_the_labwc_environment() {
    let dir = scratch("mpv");
    let log = dir.join("mpv.log");
    fake(&dir, "mpvpaper", &format!("for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{0}'\nprintf 'ENV_MARK=%s\\n' \"$ENV_MARK\" >> '{0}'\nexec sleep 30", log.display()));
    let mut environment: BTreeMap<String, String> = BTreeMap::new();
    environment.insert("PATH".into(), format!("{}:/usr/bin:/bin", dir.join("bin").display()));
    environment.insert("ENV_MARK".into(), "labwc".into());
    let mut supervisor = MpvSupervisor::default();
    supervisor.start("HEADLESS-1", "HEADLESS-1", "/videos/loop.mp4", "Fill", &environment).unwrap();
    let pid = supervisor.running("HEADLESS-1").unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&log).unwrap_or_default().contains("ENV_MARK") && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "-l\nbackground\n-o\nno-audio osc=no cursor-autohide=no keep-open=yes loop-file=inf loop-playlist=inf keepaspect=yes\nHEADLESS-1\n/videos/loop.mp4\nENV_MARK=labwc\n");
    // Restarting the same monitor stops the previous player first.
    supervisor.start("HEADLESS-1", "HEADLESS-1", "/videos/b.webm", "Center", &environment).unwrap();
    assert_ne!(supervisor.running("HEADLESS-1"), Some(pid));
    assert!(!Path::new(&format!("/proc/{pid}")).exists() || std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default().contains(" Z "));
    let started = Instant::now();
    supervisor.stop("HEADLESS-1");
    assert!(started.elapsed() < Duration::from_secs(2), "terminate is enough for a cooperative player");
    assert!(supervisor.running("HEADLESS-1").is_none());

    // A player ignoring SIGTERM is killed after 2 s.
    fake(&dir, "mpvpaper", "trap '' TERM\nwhile :; do sleep 0.05; done");
    supervisor.start("HEADLESS-2", "HEADLESS-2", "/v.mp4", "Fit", &environment).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    supervisor.stop("HEADLESS-2");
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(1900) && elapsed < Duration::from_secs(4), "{elapsed:?}");

    // Missing player.
    let mut nowhere = environment.clone();
    nowhere.insert("PATH".into(), dir.join("none").display().to_string());
    assert_eq!(supervisor.start("X", "X", "/v.mp4", "Fill", &nowhere), Err(StartError::Missing));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn launch_helper_runs_the_entry_with_the_transferred_token() {
    let dir = scratch("helper");
    let marker = dir.join("marker");
    let script = fake(&dir, "app.sh", &format!("printf '%s|%s' \"$XDG_ACTIVATION_TOKEN\" \"$DESKTOP_STARTUP_ID\" > '{}'", marker.display()));
    let entry = dir.join("app.desktop");
    std::fs::write(&entry, format!("[Desktop Entry]\nType=Application\nName=Fake\nExec={}\nStartupNotify=true\n", script.display())).unwrap();
    std::env::set_var("TESSEL_DOCK_LAUNCH_STARTUP_ID", "token-123");
    let code = glib::MainContext::new().with_thread_default(|| launch::helper_main(&["--launch-file".into(), entry.display().to_string()])).unwrap();
    assert_eq!(code, 0);
    assert!(std::env::var_os("TESSEL_DOCK_LAUNCH_STARTUP_ID").is_none(), "the token is not inherited as-is");
    let end = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(50));
    let text = std::fs::read_to_string(&marker).unwrap();
    assert!(text.split('|').any(|t| t == "token-123"), "startup token reaches the app: {text:?}");
    assert_eq!(launch::helper_main(&["--launch-file".into(), dir.join("none.desktop").display().to_string()]), 1);
    assert_eq!(launch::helper_main(&["--bogus".into(), "x".into()]), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}
