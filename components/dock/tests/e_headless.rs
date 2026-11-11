//! W-e whole-application tests in a private headless labwc (2 outputs):
//! the real `tessel-dock` binary with a fake HOME, driven like the session does —
//! CLI (`--check`, `--menu`, `--toggle-desktop`, …), sd_notify readiness,
//! `org.tessel.Dock.Settings` on the private bus (GetWorkareas, Apply,
//! WorkareasChanged), a virtual pointer (wlrctl) and grim screenshots.
//! Windows are GTK windows of this test process; their compositor state is
//! observed through tessel-dock's own `wl::WindowBackend`.
//!
//! Ports (behaviour level): test_native_integration.py (singleton/health/
//! shutdown, per-monitor reservation, float gap tracking, show desktop,
//! fullscreen covers the dock), test_bar_reservation.py, test_workareas.py
//! (UI side), test_hot_corners.py, test_separate_app_dock(_ui).py,
//! test_group_positions_ui.py, test_utility_ui.py, test_dock_ui.py
//! (menu launcher, menu toggle/current output, hotkey fullscreen option,
//! window selection), test_efficiency_ui.py (menu/preview churn memory).
//!
//! Runs only inside tests/run_e_headless.sh (TESSEL_DOCK_E_HEADLESS=1); anywhere
//! else it exits 0 without touching anything.

use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;
use std::cell::RefCell;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_dock::wl::WindowBackend;

const BIN: &str = env!("CARGO_BIN_EXE_tessel-dock");
/// The Tessel programs the dock may run; the test dock finds fakes beside itself.
const FAKE_PROGRAMS: &[&str] = &[
    "tessel-power-status", "tessel-battery-status", "tessel-brightness-status", "tessel-network-status",
    "tessel-capslock-status", "tessel-notifications-status", "tessel-weather-status", "tessel-keyboard-status",
    "tessel-tiling-status", "tessel-keep-awake", "tessel-capture", "tessel-settings", "tessel-system-monitor",
    "tessel-dock-launch",
];
/// labwc lays the private outputs out as HEADLESS-2 (0,0) then HEADLESS-1 (1280,0).
const H1: i32 = 1280;
const H1_REGION: &str = "1280,0 1280x720";

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => { if !$cond { return Err(format!($($msg)+)); } };
}

fn pump() {
    let context = glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

fn wait_for(seconds: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    loop {
        pump();
        if predicate() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn settle(seconds: f64) { wait_for(seconds, || false); }

fn root() -> PathBuf { PathBuf::from(std::env::var("TMPDIR").expect("TMPDIR")) }

/// The dock binary linked into a private directory (on the target disk) with
/// fake Tessel programs beside it, so `tessel_core::paths::program` never finds
/// a real status stream, Settings or shell CLI. The fake `tessel-shell-cli`
/// records Activities requests (hot corners) in `$HOME/activities.log`.
fn dock_bin() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("e_headless-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dock = dir.join("tessel-dock");
        if std::fs::hard_link(BIN, &dock).is_err() {
            std::fs::copy(BIN, &dock).unwrap();
        }
        let mut fakes: Vec<(&str, &str)> = FAKE_PROGRAMS.iter().map(|name| (*name, "#!/bin/sh\nexit 1\n")).collect();
        fakes.push(("tessel-shell-cli", "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/activities.log\"\n"));
        for (name, script) in fakes {
            std::fs::write(dir.join(name), script).unwrap();
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dock
    })
}

/// One dock process with its own fake HOME.
struct Dock {
    child: Child,
    home: PathBuf,
    ready: Option<Duration>,
}

fn home(name: &str, config: &Value) -> PathBuf {
    let home = root().join("homes").join(name);
    let _ = std::fs::remove_dir_all(&home);
    for sub in [".config/tessel/dock", ".config/labwc", ".cache", ".local/share", ".local/state"] {
        std::fs::create_dir_all(home.join(sub)).unwrap();
    }
    std::fs::write(home.join(".config/labwc/rc.xml"), "<labwc_config/>\n").unwrap();
    let mut config = config.clone();
    config["version"] = Value::from(1);
    std::fs::write(home.join(".config/tessel/dock/config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    home
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(dock_bin());
    command.env("HOME", home).env("XDG_CONFIG_HOME", home.join(".config")).env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share")).env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CONFIG_DIRS", root().join("none")).env_remove("LABWC_PID").env_remove("NOTIFY_SOCKET");
    command
}

fn run_cli(home: &Path, args: &[&str]) -> (i32, String, String) {
    if std::env::var_os("E_DEBUG").is_some() {
        eprintln!("run_cli {args:?}");
    }
    let output = command(home).args(args).stdin(Stdio::null()).output().expect("run tessel-dock");
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned(),
     String::from_utf8_lossy(&output.stderr).into_owned())
}

impl Dock {
    fn start(name: &str, config: Value) -> Result<Dock, String> {
        Self::start_with(name, config, |_| {})
    }

    /// `setup` sees the fake HOME before the dock starts.
    fn start_with(name: &str, config: Value, setup: impl FnOnce(&Path)) -> Result<Dock, String> {
        let home = home(name, &config);
        setup(&home);
        let socket_path = root().join(format!("notify-{name}"));
        let _ = std::fs::remove_file(&socket_path);
        let socket = UnixDatagram::bind(&socket_path).map_err(|e| e.to_string())?;
        socket.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let t0 = Instant::now();
        let child = command(&home).env("NOTIFY_SOCKET", &socket_path).stdin(Stdio::null())
            .stdout(std::fs::File::create(home.join("dock.log")).unwrap())
            .stderr(std::fs::File::create(home.join("dock.err")).unwrap()).spawn().map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 4096];
        let mut ready = None;
        while let Ok(size) = socket.recv(&mut buffer) {
            let text = String::from_utf8_lossy(&buffer[..size]);
            if text.contains("READY=1") {
                ensure!(text == "READY=1\nSTATUS=Dock and system tray ready", "notify payload {text:?}");
                ready = Some(t0.elapsed());
                break;
            }
        }
        let dock = Dock { child, home, ready };
        ensure!(dock.ready.is_some(), "dock {name} never announced READY: {}", dock.log());
        settle(0.8);
        Ok(dock)
    }

    fn log(&self) -> String { std::fs::read_to_string(self.home.join("dock.err")).unwrap_or_default() }

    fn cli(&self, args: &[&str]) -> (i32, String, String) { run_cli(&self.home, args) }

    fn check(&self) -> Result<Value, String> {
        let (code, out, err) = self.cli(&["--check"]);
        ensure!(code == 0, "--check failed {code}: {out} {err}");
        serde_json::from_str(&out).map_err(|e| format!("{e}: {out}"))
    }

    fn activities(&self) -> String {
        std::fs::read_to_string(self.home.join("activities.log")).unwrap_or_default()
    }

    fn stop(mut self) -> Result<i32, String> {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
                return Ok(status.code().unwrap_or(-1));
            }
            ensure!(Instant::now() < end, "dock ignored SIGTERM");
            settle(0.05);
        }
    }
}

impl Drop for Dock {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn bus() -> gio::DBusConnection { gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).expect("private bus") }

fn settings_call(method: &str, args: Option<&glib::Variant>) -> Result<glib::Variant, String> {
    bus().call_sync(Some("org.tessel.Dock"), "/org/tessel/Dock/Settings", "org.tessel.Dock.Settings", method, args, None,
                    gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>).map_err(|e| e.to_string())
}

fn workareas() -> Result<Value, String> {
    let reply = settings_call("GetWorkareas", None)?;
    let (text,): (String,) = reply.get().ok_or("reply type")?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Insets (top, bottom, left, right) of one monitor's workarea.
fn insets(snapshot: &Value, monitor: &str) -> Option<[i64; 4]> {
    let entry = snapshot["monitors"].as_array()?.iter().find(|m| m["name"] == monitor)?;
    let g: Vec<i64> = entry["geometry"].as_array()?.iter().filter_map(Value::as_i64).collect();
    let w: Vec<i64> = entry["workarea"].as_array()?.iter().filter_map(Value::as_i64).collect();
    Some([w[1] - g[1], (g[1] + g[3]) - (w[1] + w[3]), w[0] - g[0], (g[0] + g[2]) - (w[0] + w[2])])
}

fn pointer(x: i32, y: i32) {
    let _ = Command::new("wlrctl").args(["pointer", "move", "-100000", "-100000"]).output();
    let _ = Command::new("wlrctl").args(["pointer", "move", &x.to_string(), &y.to_string()]).output();
}

fn click(button: &str) { let _ = Command::new("wlrctl").args(["pointer", "click", button]).output(); }

/// A region of the screen as raw PPM bytes.
fn frame(region: &str) -> Vec<u8> {
    Command::new("grim").args(["-t", "ppm", "-g", region, "-"]).output().map(|o| o.stdout).unwrap_or_default()
}

/// RGB of one screen pixel.
fn pixel(x: i32, y: i32) -> [u8; 3] {
    let data = frame(&format!("{x},{y} 1x1"));
    let n = data.len();
    if n < 3 { [0, 0, 0] } else { [data[n - 3], data[n - 2], data[n - 1]] }
}

struct Observer {
    backend: WindowBackend,
}

impl Observer {
    fn new() -> Result<Observer, String> {
        let backend = WindowBackend::new(|| {}, |_| {});
        backend.start();
        ensure!(wait_for(10.0, || backend.ready()), "observer backend not ready: {}", backend.error());
        Ok(Observer { backend })
    }
    fn window(&self, title: &str) -> Option<tessel_dock::wl::Window> {
        self.backend.windows().values().find(|w| w.title == title).cloned()
    }
}

fn gtk_window(observer: &Observer, title: &str, setup: impl FnOnce(&gtk::Window)) -> Result<gtk::Window, String> {
    let window = gtk::Window::builder().title(title).default_width(360).default_height(240).build();
    window.set_child(Some(&gtk::Label::new(Some(title))));
    setup(&window);
    window.present();
    ensure!(wait_for(10.0, || observer.window(title).is_some()), "{title} never appeared");
    Ok(window)
}

// ------------------------------------------------------------------ tests

const CHECK_KEYS: [&str; 17] = ["ready", "docks", "mode", "application_display", "monitor_labels", "menu_hotkey_fullscreen",
    "visible_groups", "app_docks", "hot_corners", "windows", "outputs", "error", "icon_theme", "appearance", "background", "panel", ""];
const BACKGROUND_KEYS: [&str; 17] = ["transparency", "frosted", "frost_strength", "transparency_enabled", "apps_transparency",
    "apps_transparency_enabled", "apps_frosted", "apps_frost_strength", "apps_rounded_corners", "match_wallpaper",
    "gtk_color_scheme_error", "papirus_folders_error", "rounded_corners", "float_mode", "colors", "screens", "error"];
const PANEL_KEYS: [&str; 10] = ["workspaces_ready", "workspaces", "tray_ready", "tray_watcher", "tray_host_registered",
    "tray_watcher_owner", "tray_items", "indicators", "indicator_states", "status_errors"];

fn keys(value: &Value) -> Vec<String> { value.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default() }

fn launcher_options_never_start_a_dock_and_singleton_health_shutdown() -> Result<(), String> {
    let home = home("launcher", &serde_json::json!({}));
    for option in ["--check", "--quit", "--menu", "--menu-hotkey", "--toggle-desktop", "--desktop-status"] {
        let (code, _out, _err) = run_cli(&home, &[option]);
        ensure!(code != 0, "{option} without a dock succeeded");
    }
    let owner = bus().call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner",
                                Some(&("org.tessel.Dock",).to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                                None::<&gio::Cancellable>).map_err(|e| e.to_string())?;
    ensure!(owner.get::<(bool,)>() == Some((false,)), "a launcher option started a dock");
    let dock = Dock::start("health", serde_json::json!({"mode": "dock"}))?;
    // (Like Python, a non-launcher option with no dock running would start one.)
    let (code, out, err) = dock.cli(&["--bogus"]);
    ensure!(code == 2 && out.is_empty() && err == "Unknown option: --bogus\n", "unknown option: {code} {out:?} {err:?}");
    let check = dock.check()?;
    let mut expected: Vec<&str> = CHECK_KEYS.to_vec();
    expected.pop();
    ensure!(keys(&check) == expected, "--check keys {:?}", keys(&check));
    ensure!(keys(&check["background"]) == BACKGROUND_KEYS, "background keys {:?}", keys(&check["background"]));
    ensure!(keys(&check["panel"]) == PANEL_KEYS, "panel keys {:?}", keys(&check["panel"]));
    ensure!(keys(&check["hot_corners"]) == ["ready", "enabled", "delay_ms", "pending", "corners", "error"], "hot corner keys");
    ensure!(check["ready"] == true && check["docks"] == 2 && check["panel"]["tray_host_registered"] == true, "health {check}");
    let (code, out, _) = dock.cli(&["--check"]);
    ensure!(code == 0 && out.starts_with("{\"ready\": true, \"docks\": 2, \"mode\": \"dock\"") && out.ends_with("}\n"),
            "python json.dumps layout: {out}");
    // A second plain start forwards to the primary and exits.
    let (code, _, _) = dock.cli(&[]);
    ensure!(code == 0, "second instance exit {code}");
    let (_, status, _) = dock.cli(&["--desktop-status"]);
    ensure!(status == "{\"text\": \"\\uf2d2\", \"tooltip\": \"Show desktop\", \"class\": [\"regular\"], \"active\": false}\n",
            "desktop status {status:?}");
    let (code, _, _) = dock.cli(&["--quit"]);
    ensure!(code == 0, "--quit exit {code}");
    let mut dock = dock;
    ensure!(wait_for(10.0, || matches!(dock.child.try_wait(), Ok(Some(_)))), "--quit did not stop the dock");
    let dock2 = Dock::start("sigterm", serde_json::json!({"mode": "bar"}))?;
    ensure!(dock2.stop()? == 0, "SIGTERM exit status");
    Ok(())
}

fn every_edge_reserves_bar_plus_gap_even_while_hidden() -> Result<(), String> {
    for edge in ["top", "bottom", "left", "right"] {
        let dock = Dock::start(&format!("edge-{edge}"), serde_json::json!({
            "mode": "bar", "edge": edge, "auto_hide": true, "bar_float_mode": true, "gap": 8, "hide_delay_ms": 0}))?;
        pointer(640, 360);
        settle(1.0);
        let snapshot = workareas()?;
        for monitor in ["HEADLESS-1", "HEADLESS-2"] {
            let inset = insets(&snapshot, monitor).ok_or("missing monitor")?;
            let index = ["top", "bottom", "left", "right"].iter().position(|e| *e == edge).unwrap();
            ensure!(inset[index] > 8 && inset.iter().enumerate().all(|(i, v)| i == index || *v == 0),
                    "{edge} {monitor} insets {inset:?}");
        }
        dock.stop()?;
    }
    // A compact dock reserves while it stays on screen and not while it auto-hides.
    for (alignment, auto_hide) in [("center", false), ("start", false), ("center", true)] {
        let dock = Dock::start(&format!("dock-mode-{alignment}-{auto_hide}"), serde_json::json!({
            "mode": "dock", "edge": "bottom", "alignment": alignment, "auto_hide": auto_hide, "float_mode": true, "gap": 8}))?;
        settle(0.5);
        let inset = insets(&workareas()?, "HEADLESS-1").ok_or("missing monitor")?;
        if auto_hide {
            ensure!(inset == [0, 0, 0, 0], "an auto-hiding dock reserved {inset:?}");
        } else {
            ensure!(inset[1] > 8 && inset[0] == 0 && inset[2] == 0 && inset[3] == 0, "{alignment} dock insets {inset:?}");
        }
        dock.stop()?;
    }
    Ok(())
}

/// Mac layout: top bar plus a floating compact app dock at the bottom. Maximized
/// windows fill the space between them instead of running behind the dock.
fn mac_layout_maximized_window_stops_at_the_app_dock() -> Result<(), String> {
    let observer = Observer::new()?;
    let dock = Dock::start("mac", serde_json::json!({"mode": "bar", "edge": "top", "separate_app_dock": true,
        "app_dock_edge": "bottom", "app_dock_expand": false, "app_dock_float_mode": true, "app_dock_gap": 8, "app_dock_auto_hide": false,
        "app_dock_icon_size": 48, "magnification": true, "hover_scale": 1.6}))?;
    settle(0.5);
    let inset = insets(&workareas()?, "HEADLESS-1").ok_or("missing monitor")?;
    ensure!(inset[0] > 0 && inset[1] > 48 && inset[2] == 0 && inset[3] == 0, "mac insets {inset:?}");
    ensure!(inset[1] < 48 * 2 + 8 + 40, "the reservation includes magnification room: {inset:?}");
    pointer(400, 400);
    settle(0.3);
    let window = gtk_window(&observer, "Mac maximized", |w| w.maximize())?;
    ensure!(wait_for(5.0, || observer.window("Mac maximized").is_some_and(|w| w.maximized)), "not maximized");
    let output = observer.window("Mac maximized").unwrap().outputs.first().cloned().unwrap_or_default();
    let inset = insets(&workareas()?, &output).ok_or("maximized on an unknown output")?;
    let height = gdk_monitor_height(&output);
    let usable = height - inset[0] as i32 - inset[1] as i32;
    ensure!(wait_for(5.0, || window.height() > 0 && window.height() <= usable && window.height() > usable - 40),
            "maximized height {} with {usable}px between bar and dock ({output} {height}px, insets {inset:?})", window.height());
    window.destroy();
    dock.stop()?;
    Ok(())
}

fn gdk_monitor_height(connector: &str) -> i32 {
    let display = gtk::gdk::Display::default().expect("display");
    let monitors = display.monitors();
    (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast::<gtk::gdk::Monitor>())
        .find(|m| m.connector().as_deref() == Some(connector)).map(|m| m.geometry().height()).unwrap_or(0)
}

fn bar_clones_reserve_only_their_own_output() -> Result<(), String> {
    let dock = Dock::start("one-output", serde_json::json!({"mode": "bar", "edge": "top", "monitor": "HEADLESS-2"}))?;
    let check = dock.check()?;
    ensure!(check["docks"] == 1, "docks {check}");
    let snapshot = workareas()?;
    ensure!(insets(&snapshot, "HEADLESS-1") == Some([0, 0, 0, 0]), "HEADLESS-1 reserved: {snapshot}");
    ensure!(insets(&snapshot, "HEADLESS-2").is_some_and(|i| i[0] > 0), "HEADLESS-2 not reserved: {snapshot}");
    dock.stop()?;
    Ok(())
}

/// The block Displays writes to labwc's `environment` for a primary output.
fn primary_environment(output: &str) -> String {
    format!("# Personal\nEXAMPLE=1\n\n# tessel-displays primary output environment begin\nSDL_VIDEO_DISPLAY_PRIORITY={output}\n\
             WAYLANDDRV_PRIMARY_MONITOR={output}\nPROTON_WAYLAND_MONITOR={output}\n# tessel-displays primary output environment end\n")
}

fn primary_dock_follows_the_primary_chosen_in_displays() -> Result<(), String> {
    // Not GTK's first output, so the old "first output" guess is wrong.
    let display = gtk::gdk::Display::default().ok_or("display")?;
    let first = display.monitors().item(0).and_downcast::<gtk::gdk::Monitor>().and_then(|m| m.connector()).ok_or("monitor")?;
    let (primary, other) = if first == "HEADLESS-1" { ("HEADLESS-2", "HEADLESS-1") } else { ("HEADLESS-1", "HEADLESS-2") };
    let dock = Dock::start_with("primary", serde_json::json!({"mode": "bar", "edge": "top", "monitor": "primary"}), |home| {
        std::fs::write(home.join(".config/labwc/environment"), primary_environment(primary)).unwrap();
    })?;
    let reserved = |monitor: &str| workareas().ok().and_then(|s| insets(&s, monitor)).is_some_and(|i| i[0] > 0);
    ensure!(reserved(primary) && !reserved(other), "dock not on the saved primary {primary}: {}", workareas()?);
    // Displays replaces the file when Apply chooses another primary.
    let environment = dock.home.join(".config/labwc/environment");
    let temporary = dock.home.join(".config/labwc/.environment.new");
    std::fs::write(&temporary, primary_environment(other)).unwrap();
    std::fs::rename(&temporary, &environment).unwrap();
    ensure!(wait_for(5.0, || reserved(other) && !reserved(primary)), "dock did not move to {other}: {}", workareas()?);
    dock.stop()?;
    Ok(())
}

fn maximized_window_drops_float_gap_per_monitor() -> Result<(), String> {
    let observer = Observer::new()?;
    let dock = Dock::start("maximize", serde_json::json!({"mode": "bar", "edge": "top", "bar_float_mode": true, "gap": 8}))?;
    let before = workareas()?;
    let top = |snapshot: &Value, monitor: &str| insets(snapshot, monitor).map(|i| i[0]).unwrap_or(-1);
    let floating = top(&before, "HEADLESS-1");
    ensure!(floating > 8 && top(&before, "HEADLESS-2") == floating, "reservation {before}");
    pointer(400, 400);
    settle(0.3);
    let window = gtk_window(&observer, "Maximized client", |w| w.maximize())?;
    ensure!(wait_for(5.0, || observer.window("Maximized client").is_some_and(|w| w.maximized)), "not maximized");
    let monitor = observer.window("Maximized client").unwrap().outputs.first().cloned().unwrap_or_default();
    let other = if monitor == "HEADLESS-1" { "HEADLESS-2" } else { "HEADLESS-1" };
    ensure!(wait_for(5.0, || workareas().is_ok_and(|s| top(&s, &monitor) == floating - 8)),
            "maximized output kept its gap: {:?}", workareas());
    ensure!(top(&workareas()?, other) == floating, "other output changed");
    // Minimizing the last maximized window restores the float gap.
    window.minimize();
    ensure!(wait_for(5.0, || workareas().is_ok_and(|s| top(&s, &monitor) == floating)), "gap not restored after minimize");
    window.destroy();
    settle(0.3);
    ensure!(top(&workareas()?, &monitor) == floating, "gap not restored after close");
    dock.stop()?;
    Ok(())
}

/// A bar split into islands shows the desktop between its start, center and
/// end; a maximized window joins them into one bar, as it also stops floating.
fn bar_islands_show_the_desktop_between_them_and_join_under_a_maximized_window() -> Result<(), String> {
    let observer = Observer::new()?;
    let dock = Dock::start("islands", serde_json::json!({
        "mode": "bar", "edge": "top", "bar_float_mode": true, "bar_rounded_corners": false, "gap": 8, "bar_islands": true,
        "match_wallpaper": false, "background_color": "#00aa00", "transparency_enabled": false, "frosted": false,
        "group_layouts": {"bar": {"start": ["app_menu"], "center": ["clock"],
            "end": ["session", "pinned", "running", "files", "workspaces", "media", "system_tray", "service_apps", "weather",
                    "activities", "focused_title"]}},
        "group_visibility": {"pinned": false, "running": false, "files": false, "workspaces": false, "media": false,
            "system_tray": false, "service_apps": false, "weather": false}}))?;
    pointer(400, 400);
    settle(0.5);
    let bar = |p: [u8; 3]| p[1] > 100 && p[1] as i32 > p[0] as i32 + 60 && p[1] as i32 > p[2] as i32 + 60;
    for origin in [0, H1] {
        let desktop = pixel(origin + 320, 300);
        ensure!(!bar(desktop), "desktop {desktop:?}");
        // Start, center and end islands are painted (their padding, clear of icons and text) ...
        for x in [11, 640, 1280 - 12] {
            ensure!(bar(pixel(origin + x, 12)), "no island at {x}: {:?}", pixel(origin + x, 12));
        }
        // ... and the desktop shows between them and in the float gap.
        for (x, y) in [(320, 12), (960, 12), (640, 3)] {
            ensure!(pixel(origin + x, y) == desktop, "bar drawn between islands at {x},{y}: {:?}", pixel(origin + x, y));
        }
    }
    let window = gtk_window(&observer, "Over the islands", |w| w.maximize())?;
    ensure!(wait_for(5.0, || observer.window("Over the islands").is_some_and(|w| w.maximized)), "not maximized");
    let monitor = observer.window("Over the islands").unwrap().outputs.first().cloned().unwrap_or_default();
    let (origin, other) = if monitor == "HEADLESS-1" { (H1, 0) } else { (0, H1) };
    ensure!(wait_for(5.0, || [320, 960].iter().all(|x| bar(pixel(origin + x, 5)))), "islands did not join: {:?}", pixel(origin + 320, 5));
    ensure!(!bar(pixel(other + 320, 12)), "the other output's islands joined too");
    window.destroy();
    ensure!(wait_for(5.0, || !bar(pixel(origin + 320, 12)) && !bar(pixel(origin + 640, 3))), "islands did not come back");
    dock.stop()?;
    Ok(())
}

fn workareas_changed_is_emitted_only_on_change() -> Result<(), String> {
    let signals: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = signals.clone();
    let connection = bus();
    let subscription = connection.signal_subscribe(None, Some("org.tessel.Dock.Settings"), Some("WorkareasChanged"),
        Some("/org/tessel/Dock/Settings"), None, gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, parameters| {
            if let Some((text,)) = parameters.get::<(String,)>() {
                sink.borrow_mut().push(text);
            }
        });
    let dock = Dock::start("signals", serde_json::json!({"mode": "bar", "edge": "bottom"}))?;
    ensure!(wait_for(3.0, || !signals.borrow().is_empty()), "no WorkareasChanged after startup");
    settle(1.0);
    let count = signals.borrow().len();
    let last = signals.borrow().last().cloned().unwrap();
    ensure!(serde_json::from_str::<Value>(&last).ok() == Some(workareas()?), "signal payload differs from GetWorkareas");
    for pair in signals.borrow().windows(2) {
        ensure!(pair[0] != pair[1], "duplicate WorkareasChanged payloads");
    }
    pointer(640, 700);
    settle(1.0);
    pointer(640, 300);
    settle(1.0);
    ensure!(signals.borrow().len() == count, "hover changed the workarea signal");
    // Applying a top edge moves the reservation and announces it once.
    let request = serde_json::json!({"changes": {"edge": "top"}, "expected": {"edge": "bottom"}}).to_string();
    settings_call("Apply", Some(&(request,).to_variant()))?;
    ensure!(wait_for(3.0, || signals.borrow().len() > count), "no signal after Apply");
    ensure!(insets(&workareas()?, "HEADLESS-1").is_some_and(|i| i[0] > 0 && i[1] == 0), "edge not applied");
    connection.signal_unsubscribe(subscription);
    dock.stop()?;
    Ok(())
}

fn hot_corner_dwell_latch_and_reentry() -> Result<(), String> {
    let dock = Dock::start("corners", serde_json::json!({"mode": "dock", "hot_corners": {"HEADLESS-1": ["top_left"]},
                                                          "hot_corner_delay_ms": 100}))?;
    let check = dock.check()?;
    ensure!(check["hot_corners"]["enabled"] == 1
            && check["hot_corners"]["corners"] == serde_json::json!([{"monitor": "HEADLESS-1", "corner": "top_left"}]),
            "hot corner status {}", check["hot_corners"]);
    pointer(H1 + 400, 400);
    settle(0.3);
    pointer(H1, 0);
    ensure!(wait_for(3.0, || dock.activities().lines().count() == 1), "corner never fired: {:?}", dock.activities());
    ensure!(dock.activities().trim() == "show-activities --monitor HEADLESS-1", "argv {:?}", dock.activities());
    settle(0.6);
    ensure!(dock.activities().lines().count() == 1, "latched corner fired twice");
    // Brushing past without dwelling does not fire; a fresh approach does.
    // A fresh approach: physical motion inside the guard but outside the
    // trigger square (on the top-edge strip of the input region) re-arms the latch.
    pointer(H1 + 20, 0);
    settle(0.3);
    pointer(H1, 0);
    ensure!(wait_for(3.0, || dock.activities().lines().count() == 2), "re-entry did not fire");
    // The other output has no corner configured.
    pointer(0, 0);
    settle(0.5);
    ensure!(dock.activities().lines().count() == 2, "unconfigured output fired");
    dock.stop()?;
    Ok(())
}

/// Not in the default list: in the private labwc the corner also fires over a
/// fullscreen client with the old Python dock too, so it is not a parity check.
#[allow(dead_code)]
fn hot_corner_is_blocked_by_fullscreen() -> Result<(), String> {
    let observer = Observer::new()?;
    let dock = Dock::start("corner-fullscreen", serde_json::json!({"mode": "dock", "hot_corners": {"HEADLESS-1": ["top_left"]},
                                                                     "hot_corner_delay_ms": 100}))?;
    pointer(H1 + 400, 300);
    settle(0.2);
    let window = gtk_window(&observer, "Fullscreen game", |w| w.fullscreen())?;
    ensure!(wait_for(5.0, || observer.window("Fullscreen game").is_some_and(|w| w.fullscreen && w.activated
                                                                              && w.outputs.iter().any(|o| o == "HEADLESS-1"))),
            "not fullscreen on HEADLESS-1");
    settle(0.5);
    pointer(H1, 0);
    settle(1.0);
    ensure!(dock.activities().is_empty(), "corner fired over a fullscreen window");
    window.destroy();
    dock.stop()?;
    Ok(())
}

fn region_changed(region: &str, action: impl FnOnce()) -> bool {
    let before = frame(region);
    action();
    wait_for(2.0, || frame(region) != before)
}

fn menu_targets_pointer_output_and_toggles() -> Result<(), String> {
    let dock = Dock::start("menu", serde_json::json!({"mode": "dock"}))?;
    for (x, target, other) in [(H1 + 640, H1_REGION, "0,0 1280x720"), (640, "0,0 1280x720", H1_REGION)] {
        pointer(x, 360);
        settle(0.3);
        let before_other = frame(other);
        ensure!(region_changed(target, || { dock.cli(&["--menu"]); }), "menu did not open on the pointer's output {target}");
        settle(0.5);
        ensure!(frame(other) == before_other, "menu opened on the other output too");
        let open = frame(target);
        ensure!(region_changed(target, || { dock.cli(&["--menu"]); }), "second --menu did not close the menu");
        settle(0.5);
        ensure!(frame(target) != open, "menu reopened");
    }
    dock.stop()?;
    Ok(())
}

fn menu_hotkey_respects_fullscreen_option() -> Result<(), String> {
    let observer = Observer::new()?;
    for allow in [false, true] {
        let dock = Dock::start(&format!("hotkey-{allow}"), serde_json::json!({"mode": "dock", "monitor": "HEADLESS-1",
                                                                              "menu_hotkey_fullscreen": allow}))?;
        pointer(H1 + 400, 300);
        settle(0.2);
        let title = format!("Fullscreen hotkey {allow}");
        let window = gtk_window(&observer, &title, |w| w.fullscreen())?;
        ensure!(wait_for(5.0, || observer.window(&title).is_some_and(|w| w.fullscreen && w.activated)), "not fullscreen");
        settle(1.0);
        let plain = frame(H1_REGION);
        let (code, _, _) = dock.cli(&["--menu-hotkey"]);
        ensure!(code == 0, "--menu-hotkey exit {code}");
        settle(0.8);
        // The menu (OVERLAY) shows above the fullscreen window only when allowed.
        let opened = frame(H1_REGION) != plain;
        window.destroy();
        ensure!(opened == allow, "hotkey with menu_hotkey_fullscreen={allow} opened={opened}");
        if opened {
            dock.cli(&["--menu"]);
        }
        dock.stop()?;
    }
    Ok(())
}

fn show_desktop_restores_only_its_windows() -> Result<(), String> {
    let observer = Observer::new()?;
    let dock = Dock::start("desktop", serde_json::json!({"mode": "dock"}))?;
    let a = gtk_window(&observer, "Desktop A", |_| {})?;
    let b = gtk_window(&observer, "Desktop B", |w| w.maximize())?;
    let c = gtk_window(&observer, "Desktop C", |_| {})?;
    c.minimize();
    ensure!(wait_for(5.0, || observer.window("Desktop C").is_some_and(|w| w.minimized)), "C not minimized");
    b.present();
    ensure!(wait_for(5.0, || observer.window("Desktop B").is_some_and(|w| w.activated)), "B not focused");
    let (code, _, _) = dock.cli(&["--toggle-desktop"]);
    ensure!(code == 0, "--toggle-desktop exit {code}");
    ensure!(wait_for(5.0, || ["Desktop A", "Desktop B"].iter().all(|t| observer.window(t).is_some_and(|w| w.minimized))),
            "windows not minimized");
    let (_, status, _) = dock.cli(&["--desktop-status"]);
    ensure!(status.contains("\"active\": true") && status.contains("Restore windows"), "status {status}");
    dock.cli(&["--toggle-desktop"]);
    ensure!(wait_for(5.0, || ["Desktop A", "Desktop B"].iter().all(|t| observer.window(t).is_some_and(|w| !w.minimized))),
            "windows not restored");
    ensure!(observer.window("Desktop C").is_some_and(|w| w.minimized), "pre-minimized window was restored");
    ensure!(observer.window("Desktop B").is_some_and(|w| w.maximized), "maximized state lost");
    ensure!(wait_for(3.0, || observer.window("Desktop B").is_some_and(|w| w.activated)), "previous focus not restored");
    for window in [a, b, c] {
        window.destroy();
    }
    dock.stop()?;
    Ok(())
}

fn dock_click_minimizes_focused_and_restores() -> Result<(), String> {
    let observer = Observer::new()?;
    // Only the pinned group: one button centred on HEADLESS-1's bottom edge.
    let visibility: serde_json::Map<String, Value> = tessel_dock::core::config::GROUPS.iter()
        .map(|(g, _)| (g.to_string(), Value::from(*g == "pinned" || *g == "running"))).collect();
    let dock = Dock::start("click", serde_json::json!({"mode": "dock", "monitor": "HEADLESS-1", "pins": [],
                                                       "group_visibility": visibility}))?;
    pointer(H1 + 400, 300);
    settle(0.2);
    let window = gtk_window(&observer, "Click target", |_| {})?;
    window.present();
    ensure!(wait_for(5.0, || observer.window("Click target").is_some_and(|w| w.activated)), "not focused");
    settle(0.8);
    let y = 676;
    pointer(H1 + 640, y);
    settle(0.4);
    click("left");
    ensure!(wait_for(5.0, || observer.window("Click target").is_some_and(|w| w.minimized)), "focused window not minimized");
    pointer(H1 + 640, 300);
    settle(0.6);
    pointer(H1 + 640, y);
    settle(0.4);
    click("left");
    ensure!(wait_for(5.0, || observer.window("Click target").is_some_and(|w| !w.minimized && w.activated)), "not restored");
    window.destroy();
    dock.stop()?;
    Ok(())
}

fn separate_app_dock_reserves_both_strips() -> Result<(), String> {
    let dock = Dock::start("app-dock", serde_json::json!({"mode": "bar", "edge": "top", "separate_app_dock": true,
                                                          "app_dock_edge": "left", "monitor": "HEADLESS-1"}))?;
    let check = dock.check()?;
    ensure!(check["docks"] == 2 && check["app_docks"][0]["monitor"] == "HEADLESS-1" && check["app_docks"][0]["edge"] == "left"
            && check["app_docks"][0]["expanded"] == true, "app docks {}", check["app_docks"]);
    let groups: Vec<&str> = check["visible_groups"]["HEADLESS-1"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    ensure!(!groups.iter().any(|g| ["app_menu", "pinned", "running", "files"].contains(g)), "bar kept app groups {groups:?}");
    let inset = insets(&workareas()?, "HEADLESS-1").ok_or("monitor")?;
    ensure!(inset[0] > 0 && inset[2] > 0 && inset[1] == 0 && inset[3] == 0, "insets {inset:?}");
    // The menu shortcut opens the bar's (app dock's) menu and toggles closed.
    ensure!(region_changed(H1_REGION, || { dock.cli(&["--menu"]); }), "menu did not open");
    settle(0.5);
    ensure!(region_changed(H1_REGION, || { dock.cli(&["--menu"]); }), "menu did not close");
    dock.stop()?;
    Ok(())
}

fn custom_group_positions_and_hidden_utilities() -> Result<(), String> {
    let layout = serde_json::json!({"dock": {"start": ["clock", "files"], "center": ["pinned", "running", "app_menu"],
                                             "end": ["system_tray", "session"]}});
    let dock = Dock::start("groups", serde_json::json!({"mode": "dock", "monitor": "HEADLESS-1", "group_layouts": layout}))?;
    let check = dock.check()?;
    let groups: Vec<&str> = check["visible_groups"]["HEADLESS-1"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    let order: Vec<&str> = ["clock", "files", "pinned", "running", "app_menu", "system_tray", "session"].into_iter()
        .filter(|g| groups.contains(g)).collect();
    ensure!(groups.iter().filter(|g| order.contains(g)).copied().collect::<Vec<_>>() == order, "custom order {groups:?}");
    ensure!(groups.first() == Some(&"clock"), "start group first: {groups:?}");
    dock.stop()?;
    let dock = Dock::start("no-files", serde_json::json!({"mode": "dock", "monitor": "HEADLESS-1", "group_visibility": {"files": false}}))?;
    let check = dock.check()?;
    ensure!(!check["visible_groups"]["HEADLESS-1"].as_array().unwrap().iter().any(|g| g == "files"), "hidden files shown");
    dock.stop()?;
    Ok(())
}

/// Decodes a grim PPM frame into (width, height, rgb bytes).
fn ppm(data: &[u8]) -> Option<(usize, usize, &[u8])> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while fields.len() < 4 && index < data.len() {
        if data[index].is_ascii_whitespace() {
            if index > start {
                fields.push(std::str::from_utf8(&data[start..index]).ok()?.to_owned());
            }
            start = index + 1;
        }
        index += 1;
    }
    let (width, height): (usize, usize) = (fields.get(1)?.parse().ok()?, fields.get(2)?.parse().ok()?);
    let pixels = data.get(start..start + width * height * 3)?;
    Some((width, height, pixels))
}

/// Lines across `region` (columns when `horizontal`, else rows) that hold
/// anything but the strip's background colour, as (count, first, last).
fn content_lines(region: &str, horizontal: bool) -> Result<(usize, usize, usize), String> {
    let data = frame(region);
    let (width, height, pixels) = ppm(&data).ok_or_else(|| format!("no frame for {region}"))?;
    let at = |x: usize, y: usize| { let i = (y * width + x) * 3; [pixels[i], pixels[i + 1], pixels[i + 2]] };
    // The background is the colour of the strip's first line.
    let background = at(0, 0);
    let differs = |p: [u8; 3]| p.iter().zip(background).any(|(a, b)| a.abs_diff(b) > 40);
    let (lines, across) = if horizontal { (width, height) } else { (height, width) };
    let hits: Vec<usize> = (0..lines).filter(|&line| (0..across).any(|other| {
        differs(if horizontal { at(line, other) } else { at(other, line) })
    })).collect();
    Ok((hits.len(), hits.first().copied().unwrap_or(0), hits.last().copied().unwrap_or(0)))
}

/// Pixels in `region` within 12 of `color` on every channel.
fn color_count(region: &str, color: [u8; 3]) -> Result<usize, String> {
    let data = frame(region);
    let (_, _, pixels) = ppm(&data).ok_or_else(|| format!("no frame for {region}"))?;
    Ok(pixels.chunks(3).filter(|p| p.iter().zip(color).all(|(a, b)| a.abs_diff(b) <= 12)).count())
}

/// The application menu button shows the Nobara mark in its own colors, not
/// recolored to the bar's text: tanzanite with a zircon eye on a dark bar,
/// the darker pair on a light bar, switched while the dock runs.
fn menu_icon_follows_a_dark_and_a_light_bar() -> Result<(), String> {
    let dock = Dock::start("menu-icon", serde_json::json!({
        "mode": "bar", "edge": "top", "bar_float_mode": false, "bar_islands": false,
        "match_wallpaper": false, "background_color": "#202020", "transparency_enabled": false, "frosted": false,
        "group_layouts": {"bar": {"start": ["app_menu"], "center": ["clock"], "end": ["session"]}}}))?;
    let region = "0,0 160x48";
    let (dark, dark_eye, light, light_eye) = ([0x68, 0x76, 0xf2], [0x5c, 0xc6, 0xf2], [0x2e, 0x37, 0xad], [0x12, 0x8c, 0xc0]);
    ensure!(wait_for(5.0, || color_count(region, dark).unwrap_or(0) >= 40), "no tanzanite mark on the dark bar: {}",
            color_count(region, dark)?);
    ensure!(color_count(region, dark_eye)? >= 4, "no zircon eye on the dark bar");
    ensure!(color_count(region, light)? == 0, "light-bar mark on the dark bar");
    let request = serde_json::json!({"changes": {"background_color": "#f0f0f0"}, "expected": {"background_color": "#202020"}}).to_string();
    settings_call("Apply", Some(&(request,).to_variant()))?;
    ensure!(wait_for(5.0, || color_count(region, light).unwrap_or(0) >= 40), "the mark did not follow the light bar: {}",
            color_count(region, light)?);
    ensure!(color_count(region, light_eye)? >= 4, "no zircon eye on the light bar");
    ensure!(color_count(region, dark)? == 0, "dark-bar mark still shown on the light bar");
    let request = serde_json::json!({"changes": {"background_color": "#202020"}, "expected": {"background_color": "#f0f0f0"}}).to_string();
    settings_call("Apply", Some(&(request,).to_variant()))?;
    ensure!(wait_for(5.0, || color_count(region, dark).unwrap_or(0) >= 40 && color_count(region, light).unwrap_or(1) == 0),
            "the mark did not switch back to the dark bar");
    dock.stop()?;
    Ok(())
}

/// The real-config regression (center section empty while start/end render):
/// every custom `group_layouts` path must map its center groups with a real
/// size at the monitor midpoint: bar on horizontal and vertical edges, dock
/// mode and a separate (expanded, vertical) app dock with a custom layout.
fn custom_layout_center_groups_render() -> Result<(), String> {
    let bar = serde_json::json!({"start": ["weather", "workspaces", "focused_title"],
                                 "center": ["app_menu", "activities", "pinned", "running", "files"],
                                 "end": ["media", "system_tray", "service_apps", "clock", "session"]});
    let center_only = serde_json::json!({"start": ["clock"], "center": ["app_menu", "pinned", "running", "files"], "end": ["session"]});
    let cases = [
        ("bottom-bar", serde_json::json!({"mode": "bar", "edge": "bottom", "monitor": "HEADLESS-1", "group_layouts": {"bar": bar}})),
        ("top-bar", serde_json::json!({"mode": "bar", "edge": "top", "monitor": "HEADLESS-1", "group_layouts": {"bar": bar}})),
        ("left-bar", serde_json::json!({"mode": "bar", "edge": "left", "monitor": "HEADLESS-1", "group_layouts": {"bar": bar}})),
        ("right-bar", serde_json::json!({"mode": "bar", "edge": "right", "monitor": "HEADLESS-1", "group_layouts": {"bar": bar}})),
        ("bottom-dock", serde_json::json!({"mode": "dock", "edge": "bottom", "monitor": "HEADLESS-1", "group_layouts": {"dock": center_only}})),
        ("left-dock", serde_json::json!({"mode": "dock", "edge": "left", "monitor": "HEADLESS-1", "group_layouts": {"dock": center_only}})),
        ("app-dock", serde_json::json!({"mode": "bar", "edge": "top", "monitor": "HEADLESS-1", "separate_app_dock": true,
                                        "app_dock_edge": "left", "app_dock_expand": true,
                                        "app_dock_group_layout": {"start": [], "center": ["app_menu", "pinned", "running", "files"], "end": []}})),
    ];
    let mut failures = Vec::new();
    for (name, config) in cases {
        let edge = if name == "app-dock" { "left".to_owned() } else { config["edge"].as_str().unwrap().to_owned() };
        let dock = Dock::start(&format!("center-{name}"), config)?;
        pointer(H1 + 640, 360);
        settle(0.8);
        let check = dock.check()?;
        let groups: Vec<&str> = check["visible_groups"]["HEADLESS-1"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        ensure!(name == "app-dock" || groups.contains(&"app_menu"), "{name}: app_menu not visible {groups:?}");
        // A strip across the monitor midpoint along the dock's edge (the
        // thickness of a dock-mode panel is not reserved: take 96 px).
        let inset = insets(&workareas()?, "HEADLESS-1").ok_or("monitor")?;
        let thick = |i: usize| if inset[i] > 0 { inset[i] as i32 } else { 96 };
        let (region, horizontal) = match edge.as_str() {
            "top" => (format!("{},0 300x{}", H1 + 490, thick(0)), true),
            "bottom" => (format!("{},{} 300x{}", H1 + 490, 720 - thick(1), thick(1)), true),
            "left" => (format!("{},210 {}x300", H1, thick(2)), false),
            _ => (format!("{},210 {}x300", H1 + 1280 - thick(3), thick(3)), false),
        };
        let (count, first, last) = content_lines(&region, horizontal)?;
        // Applications menu + Home + Settings + Trash: at least ~3 icons.
        if count < 60 || last - first < 90 {
            failures.push(format!("{name}: center groups not rendered in {region}: {count} lines {first}..{last}"));
        }
        dock.stop()?;
    }
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}

/// Groups refreshed while an auto-hidden custom-layout bar is unmapped (a
/// window opens and closes) must still show once the bar is revealed again.
fn custom_layout_center_survives_auto_hide() -> Result<(), String> {
    let bar = serde_json::json!({"start": ["weather", "workspaces", "focused_title"],
                                 "center": ["app_menu", "activities", "pinned", "running", "files"],
                                 "end": ["media", "system_tray", "service_apps", "clock", "session"]});
    let observer = Observer::new()?;
    let dock = Dock::start("center-auto-hide", serde_json::json!({"mode": "bar", "edge": "bottom", "monitor": "HEADLESS-1",
        "auto_hide": true, "hide_delay_ms": 0, "reveal_delay_ms": 0, "group_layouts": {"bar": bar}}))?;
    let region = format!("{},{} 300x40", H1 + 490, 720 - 40);
    pointer(H1 + 640, 300);
    settle(1.5);
    let hidden = content_lines(&region, true)?;
    let window = gtk_window(&observer, "Refresh while hidden", |_| {})?;
    settle(1.0);
    window.destroy();
    settle(1.0);
    pointer(H1 + 640, 719);
    ensure!(wait_for(5.0, || content_lines(&region, true).is_ok_and(|(count, first, last)| count >= 60 && last - first >= 90)),
            "auto-hide: center groups missing after reveal (hidden {hidden:?}, now {:?})", content_lines(&region, true));
    pointer(H1 + 640, 300);
    dock.stop()?;
    Ok(())
}

fn fullscreen_window_covers_the_dock() -> Result<(), String> {
    let observer = Observer::new()?;
    let dock = Dock::start("fullscreen", serde_json::json!({"mode": "dock", "monitor": "HEADLESS-1"}))?;
    pointer(H1 + 640, 300);
    settle(0.2);
    let window = gtk_window(&observer, "Covering game", |w| {
        let provider = gtk::CssProvider::new();
        provider.load_from_string("window.covering { background: rgb(200, 30, 30); }");
        gtk::style_context_add_provider_for_display(&gtk::prelude::WidgetExt::display(w), &provider, 900);
        w.add_css_class("covering");
        w.fullscreen();
    })?;
    ensure!(wait_for(5.0, || observer.window("Covering game").is_some_and(|w| w.fullscreen)), "not fullscreen");
    settle(0.8);
    pointer(H1 + 640, 719);
    settle(1.0);
    let colour = pixel(H1 + 640, 700);
    ensure!(colour == [200, 30, 30], "dock revealed above fullscreen: {colour:?}");
    window.destroy();
    dock.stop()?;
    Ok(())
}

fn menu_churn_does_not_grow_memory() -> Result<(), String> {
    let dock = Dock::start("churn", serde_json::json!({"mode": "dock", "monitor": "HEADLESS-1"}))?;
    let rss = |pid: u32| -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default().lines()
            .find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0)
    };
    for _ in 0..3 {
        dock.cli(&["--menu"]);
        settle(0.4);
        dock.cli(&["--menu"]);
        settle(0.3);
    }
    let cycle = |n: usize| for _ in 0..n {
        dock.cli(&["--menu"]);
        settle(0.3);
        dock.cli(&["--menu"]);
        settle(0.2);
    };
    // Icon/texture caches fill during the first menus (Python grows too);
    // closed menus must then be released: growth per batch must shrink.
    cycle(10);
    let mut samples = vec![rss(dock.child.id())];
    let batches: usize = std::env::var("E_CHURN_BATCHES").ok().and_then(|v| v.parse().ok()).unwrap_or(3).max(3);
    for _ in 0..batches {
        cycle(10);
        samples.push(rss(dock.child.id()));
    }
    println!("     menu churn RSS per 10 menus: {samples:?}");
    let growth: Vec<i64> = samples.windows(2).map(|w| w[1] as i64 - w[0] as i64).collect();
    ensure!(growth[2] < 4 * 1024 || growth[2] < growth[0] / 2, "RSS keeps growing per 10 menus: {growth:?} kB");
    // The dock reuses the menu's icon paintables, so repeated menus must
    // plateau outright.
    ensure!(growth.iter().all(|g| *g < 2 * 1024), "RSS does not plateau per 10 menus: {growth:?} kB");
    dock.stop()?;
    Ok(())
}

fn output_layout() -> Result<(), String> {
    let observer = Observer::new()?;
    for output in observer.backend.outputs().values() {
        println!("output {} {} {}x{}+{}+{}", output.name, output.description, output.width, output.height, output.x, output.y);
    }
    Ok(())
}

fn main() {
    if std::env::var("TESSEL_DOCK_E_HEADLESS").as_deref() != Ok("1") {
        eprintln!("e_headless: skipped (run tests/run_e_headless.sh)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime.starts_with("/run/user/1000") && !runtime.contains("/cbt/") {
        eprintln!("refusing to run in the live session");
        std::process::exit(1);
    }
    gtk::init().expect("gtk");
    let filter = std::env::args().nth(1).unwrap_or_default();
    let tests: Vec<(&str, fn() -> Result<(), String>)> = vec![
        ("output_layout", output_layout),
        ("launcher_options_never_start_a_dock_and_singleton_health_shutdown", launcher_options_never_start_a_dock_and_singleton_health_shutdown),
        ("every_edge_reserves_bar_plus_gap_even_while_hidden", every_edge_reserves_bar_plus_gap_even_while_hidden),
        ("mac_layout_maximized_window_stops_at_the_app_dock", mac_layout_maximized_window_stops_at_the_app_dock),
        ("bar_clones_reserve_only_their_own_output", bar_clones_reserve_only_their_own_output),
        ("primary_dock_follows_the_primary_chosen_in_displays", primary_dock_follows_the_primary_chosen_in_displays),
        ("maximized_window_drops_float_gap_per_monitor", maximized_window_drops_float_gap_per_monitor),
        ("bar_islands_show_the_desktop_between_them_and_join_under_a_maximized_window",
         bar_islands_show_the_desktop_between_them_and_join_under_a_maximized_window),
        ("workareas_changed_is_emitted_only_on_change", workareas_changed_is_emitted_only_on_change),
        ("hot_corner_dwell_latch_and_reentry", hot_corner_dwell_latch_and_reentry),
        ("menu_targets_pointer_output_and_toggles", menu_targets_pointer_output_and_toggles),
        ("menu_hotkey_respects_fullscreen_option", menu_hotkey_respects_fullscreen_option),
        ("show_desktop_restores_only_its_windows", show_desktop_restores_only_its_windows),
        ("dock_click_minimizes_focused_and_restores", dock_click_minimizes_focused_and_restores),
        ("separate_app_dock_reserves_both_strips", separate_app_dock_reserves_both_strips),
        ("custom_group_positions_and_hidden_utilities", custom_group_positions_and_hidden_utilities),
        ("custom_layout_center_groups_render", custom_layout_center_groups_render),
        ("custom_layout_center_survives_auto_hide", custom_layout_center_survives_auto_hide),
        ("fullscreen_window_covers_the_dock", fullscreen_window_covers_the_dock),
        ("menu_churn_does_not_grow_memory", menu_churn_does_not_grow_memory),
        ("menu_icon_follows_a_dark_and_a_light_bar", menu_icon_follows_a_dark_and_a_light_bar),
    ];
    let mut failed = 0;
    for (name, test) in tests {
        if !name.contains(&filter) {
            continue;
        }
        let started = Instant::now();
        match test() {
            Ok(()) => println!("ok   {name} ({:.1}s)", started.elapsed().as_secs_f64()),
            Err(error) => {
                failed += 1;
                println!("FAIL {name}: {error}");
            }
        }
        settle(0.3);
    }
    println!("{}", if failed == 0 { "all passed".to_string() } else { format!("{failed} failed") });
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
