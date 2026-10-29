//! Explicit dock panel actions, launched independently of Waydock's lifetime
//! (port of panel_actions.py; contract §7 "Clicks").
//!
//! API: [`APPLICATIONS`], [`application_command`] (exact argv per applet and
//! edge), [`PanelActions`] (`open`, `capture`, `logout`,
//! `open_global_settings`, `open_system_monitor`) with an injectable
//! [`Spawner`], file check, home directory and environment lookup. Failure
//! texts are Python's (English; the dock's error dialog shows them as is).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::spawn::{GioSpawner, Spawner};

/// `sys.executable` of the Python dock service.
pub const PYTHON: &str = "/usr/bin/python3";

pub const APPLICATIONS: &[(&str, (&str, &str))] = &[
    ("media", ("media-manager", "media_manager.py")),
    ("session", ("session-manager", "session_manager.py")),
    ("notifications", ("notification-manager", "notification_manager.py")),
    ("idle_inhibitor", ("keep-awake", "keep_awake.py")),
    ("power", ("power-manager", "power_manager.py")),
    ("brightness", ("brightness-manager", "brightness_manager.py")),
    ("battery", ("battery-manager", "battery_manager.py")),
    ("network", ("network-manager", "network_manager.py")),
    ("discovery", ("network-discovery", "discovery_manager.py")),
    ("bluetooth", ("btmanager", "btmanager.py")),
    ("tiling", ("tiling-manager", "tiling_manager.py")),
    ("keyboard", ("on-screen-keyboard", "osk_manager.py")),
    ("audio", ("audio-manager", "audio_manager.py")),
    ("calendar", ("calendar-manager", "calendar_manager.py")),
    ("weather", ("weather-manager", "weather_manager.py")),
];

/// Session variables forwarded by name to the detached popup service.
pub const FORWARDED_ENVIRONMENT: &[&str] = &[
    "WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE", "LABWC_PID",
];

pub const WAYCAPTURE: &str = "/usr/local/bin/waycapture";
pub const SYSTEM_MONITOR: &str = "/usr/local/bin/system-monitor";

fn scripts_root() -> PathBuf {
    suite_core::paths::home().join(".config/waybar/scripts")
}

/// `application_command(name, settings, edge, offset, root)` with
/// `python` = `sys.executable`. `Err` is Python's `ValueError` text.
pub fn application_command(
    name: &str,
    settings: bool,
    edge: &str,
    offset: i64,
    root: Option<&Path>,
    python: &str,
) -> Result<Vec<String>, String> {
    let Some((_, (folder, script))) = APPLICATIONS.iter().find(|(n, _)| *n == name) else {
        return Err("Unknown panel application".into());
    };
    if !["top", "bottom", "left", "right"].contains(&edge) || offset < 0 {
        return Err("Invalid popup placement".into());
    }
    let root = root.map(Path::to_path_buf).unwrap_or_else(scripts_root);
    let mut command = vec![python.to_owned(), root.join(folder).join(script).to_string_lossy().into_owned()];
    let popup = |command: &mut Vec<String>| {
        command.extend(["--popup", "--popup-edge", edge, "--popup-offset"].map(String::from));
        command.push(offset.to_string());
    };
    match name {
        "discovery" => popup(&mut command),
        "keyboard" => command.push(if settings { "--settings" } else { "--toggle" }.into()),
        "tiling" => {
            if settings {
                command.extend(["--settings", "--popup-edge", edge, "--popup-offset"].map(String::from));
                command.push(offset.to_string());
            } else {
                command.push("--toggle".into());
            }
        }
        _ if settings && !["bluetooth", "idle_inhibitor", "session", "media"].contains(&name) => command.push("--settings".into()),
        _ => popup(&mut command),
    }
    Ok(command)
}

/// Capture actions → waycapture arguments.
pub fn capture_arguments(action: &str) -> Option<&'static [&'static str]> {
    Some(match action {
        "settings" => &["--record"],
        "stop" => &["--stop"],
        "none" => &["--start-recording", "--audio-preset", "none"],
        "desktop" => &["--start-recording", "--audio-preset", "desktop"],
        "desktop-mic" => &["--start-recording", "--audio-preset", "desktop-mic"],
        _ => return None,
    })
}

pub struct PanelActions {
    failed: Rc<dyn Fn(String)>,
    pub spawner: Rc<dyn Spawner>,
    /// `sys.executable` for the applet scripts.
    pub python: String,
    /// `Path.home()`; scripts live under `<home>/.config/waybar/scripts`.
    pub home: PathBuf,
    /// `systemd-run` (resolved through `PATH`, like Python).
    pub systemd_run: String,
    pub waycapture: PathBuf,
    pub system_monitor: PathBuf,
    /// `Path.is_file` (injectable for tests).
    pub is_file: Box<dyn Fn(&Path) -> bool>,
    /// `key in os.environ` (injectable for tests).
    pub has_env: Box<dyn Fn(&str) -> bool>,
}

impl PanelActions {
    pub fn new(failed: impl Fn(String) + 'static) -> Self {
        Self::with_spawner(failed, Rc::new(GioSpawner))
    }

    pub fn with_spawner(failed: impl Fn(String) + 'static, spawner: Rc<dyn Spawner>) -> Self {
        Self {
            failed: Rc::new(failed),
            spawner,
            python: PYTHON.into(),
            home: suite_core::paths::home(),
            systemd_run: "systemd-run".into(),
            waycapture: WAYCAPTURE.into(),
            system_monitor: SYSTEM_MONITOR.into(),
            is_file: Box::new(|path| path.is_file()),
            has_env: Box::new(|key| std::env::var_os(key).is_some()),
        }
    }

    fn fail(&self, message: impl Into<String>) {
        (self.failed)(message.into());
    }

    /// Open an applet popup (`settings` = right click). `Err` = ValueError.
    pub fn open(&self, name: &str, settings: bool, edge: &str, offset: i64) -> Result<bool, String> {
        if name == "screen_record" {
            return self.capture("settings");
        }
        let root = self.home.join(".config/waybar/scripts");
        let command = application_command(name, settings, edge, offset, Some(&root), &self.python)?;
        if !(self.is_file)(Path::new(&command[1])) {
            self.fail(format!("The {name} app is not installed."));
            return Ok(false);
        }
        Ok(self.run_independent(command))
    }

    pub fn capture(&self, action: &str) -> Result<bool, String> {
        let Some(arguments) = capture_arguments(action) else {
            return Err("Unknown capture action".into());
        };
        if !(self.is_file)(&self.waycapture) {
            self.fail("Screen Recorder is not installed.");
            return Ok(false);
        }
        let mut command = vec![self.waycapture.to_string_lossy().into_owned()];
        command.extend(arguments.iter().map(|a| a.to_string()));
        Ok(self.run_independent(command))
    }

    /// The detached service argv for `command` (exposed for tests/parity).
    pub fn independent_command(&self, command: Vec<String>) -> Vec<String> {
        let mut launcher: Vec<String> =
            ["--user", "--quiet", "--collect", "--service-type=exec", "--slice=app.slice", "--expand-environment=no"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        launcher.insert(0, self.systemd_run.clone());
        launcher.extend(FORWARDED_ENVIRONMENT.iter().filter(|k| (self.has_env)(k)).map(|k| format!("--setenv={k}")));
        launcher.push("--".into());
        launcher.extend(command);
        launcher
    }

    fn run_independent(&self, command: Vec<String>) -> bool {
        // A primary popup process must survive stopping/restarting the dock.
        let argv = self.independent_command(command);
        self.run(argv)
    }

    /// Called only after the logout dialog's affirmative response.
    pub fn logout(&self) -> bool {
        self.run(vec!["labwc".into(), "--exit".into()])
    }

    pub fn open_global_settings(&self) -> bool {
        let command = self.home.join(".local/bin/global-settings");
        if !(self.is_file)(&command) {
            self.fail("Global Settings is not installed.");
            return false;
        }
        self.run_independent(vec![command.to_string_lossy().into_owned()])
    }

    pub fn open_system_monitor(&self) -> bool {
        if !(self.is_file)(&self.system_monitor) {
            self.fail("System Monitor is not installed.");
            return false;
        }
        self.run_independent(vec![self.system_monitor.to_string_lossy().into_owned()])
    }

    fn run(&self, argv: Vec<String>) -> bool {
        let failed = self.failed.clone();
        let result = self.spawner.spawn(
            &argv,
            None,
            Box::new(move |result| match result {
                Ok((true, _)) => {}
                Ok((false, stderr)) => {
                    let text = if stderr.is_empty() { "The application could not be opened.".to_owned() } else { stderr };
                    failed(suite_core::pyjson::py_strip(&text).to_owned());
                }
                Err(error) => failed(error.message().to_owned()),
            }),
        );
        match result {
            Ok(()) => true,
            Err(error) => {
                self.fail(error.message());
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::spawn::{error, FakeSpawner};
    use std::cell::RefCell;

    const EXPECTED_APPS: &[(&str, &str)] = &[
        ("media", "media-manager/media_manager.py"),
        ("session", "session-manager/session_manager.py"),
        ("notifications", "notification-manager/notification_manager.py"),
        ("power", "power-manager/power_manager.py"),
        ("brightness", "brightness-manager/brightness_manager.py"),
        ("battery", "battery-manager/battery_manager.py"),
        ("network", "network-manager/network_manager.py"),
        ("bluetooth", "btmanager/btmanager.py"),
        ("audio", "audio-manager/audio_manager.py"),
        ("calendar", "calendar-manager/calendar_manager.py"),
        ("weather", "weather-manager/weather_manager.py"),
    ];

    fn cmd(name: &str, settings: bool, edge: &str, offset: i64, root: &str) -> Vec<String> {
        application_command(name, settings, edge, offset, Some(Path::new(root)), PYTHON).unwrap()
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn discovery_clicks_open_the_device_popup() {
        let entry = [PYTHON, "/fixture/network-discovery/discovery_manager.py"];
        assert_eq!(cmd("discovery", false, "left", 50, "/fixture"), v(&[entry[0], entry[1], "--popup", "--popup-edge", "left", "--popup-offset", "50"]));
        assert_eq!(cmd("discovery", true, "top", 42, "/fixture"), v(&[entry[0], entry[1], "--popup", "--popup-edge", "top", "--popup-offset", "42"]));
    }

    #[test]
    fn all_popups_use_fixed_installed_entrypoints_and_four_dock_edges() {
        let root = "/a path with spaces/$literal/scripts";
        for (name, relative) in EXPECTED_APPS {
            for edge in ["top", "bottom", "left", "right"] {
                let path = format!("{root}/{relative}");
                assert_eq!(cmd(name, false, edge, 123, root), v(&[PYTHON, &path, "--popup", "--popup-edge", edge, "--popup-offset", "123"]));
            }
        }
    }

    #[test]
    fn settings_select_settings_mode_except_single_popup_apps() {
        for (name, relative) in EXPECTED_APPS {
            let path = format!("/fixture/{relative}");
            let command = cmd(name, true, "bottom", 76, "/fixture");
            if ["bluetooth", "session", "media"].contains(name) {
                assert_eq!(command, v(&[PYTHON, &path, "--popup", "--popup-edge", "bottom", "--popup-offset", "76"]));
            } else {
                assert_eq!(command, v(&[PYTHON, &path, "--settings"]));
            }
        }
        // idle_inhibitor right click also gets the popup again.
        assert_eq!(cmd("idle_inhibitor", true, "top", 9, "/f")[2..], v(&["--popup", "--popup-edge", "top", "--popup-offset", "9"]));
    }

    #[test]
    fn tiling_and_keyboard() {
        let tiling = "/fixture/tiling-manager/tiling_manager.py";
        assert_eq!(cmd("tiling", false, "bottom", 76, "/fixture"), v(&[PYTHON, tiling, "--toggle"]));
        assert_eq!(cmd("tiling", true, "top", 40, "/fixture"), v(&[PYTHON, tiling, "--settings", "--popup-edge", "top", "--popup-offset", "40"]));
        let osk = "/fixture/on-screen-keyboard/osk_manager.py";
        assert_eq!(cmd("keyboard", false, "bottom", 76, "/fixture"), v(&[PYTHON, osk, "--toggle"]));
        assert_eq!(cmd("keyboard", true, "bottom", 76, "/fixture"), v(&[PYTHON, osk, "--settings"]));
    }

    #[test]
    fn unknown_app_and_invalid_placement_never_form_commands() {
        for name in ["../helper", "network;exit", "", "unknown"] {
            assert_eq!(application_command(name, false, "bottom", 76, None, PYTHON).unwrap_err(), "Unknown panel application");
        }
        assert_eq!(application_command("audio", false, "bottom;exit", 76, None, PYTHON).unwrap_err(), "Invalid popup placement");
        assert!(application_command("audio", false, "bottom", -1, None, PYTHON).is_err());
        assert_eq!(application_command("audio", false, "bottom", 0, None, PYTHON).unwrap().last().unwrap(), "0");
    }

    struct Harness {
        actions: PanelActions,
        spawner: FakeSpawner,
        errors: Rc<RefCell<Vec<String>>>,
    }

    fn harness(env: &'static [&'static str], files: bool) -> Harness {
        let errors = Rc::new(RefCell::new(Vec::new()));
        let sink = errors.clone();
        let spawner = FakeSpawner::new();
        let mut actions = PanelActions::with_spawner(move |e| sink.borrow_mut().push(e), Rc::new(spawner.clone()));
        actions.home = PathBuf::from("/a path/$literal");
        actions.is_file = Box::new(move |_| files);
        actions.has_env = Box::new(move |key| env.contains(&key));
        Harness { actions, spawner, errors }
    }

    #[test]
    fn capture_presets_and_stop_are_detached_literal_commands() {
        let h = harness(&[], true);
        for (action, args) in [
            ("settings", v(&["--record"])),
            ("stop", v(&["--stop"])),
            ("none", v(&["--start-recording", "--audio-preset", "none"])),
            ("desktop", v(&["--start-recording", "--audio-preset", "desktop"])),
            ("desktop-mic", v(&["--start-recording", "--audio-preset", "desktop-mic"])),
        ] {
            assert!(h.actions.capture(action).unwrap());
            let command = h.spawner.last_argv();
            assert_eq!(command[0], "systemd-run");
            let sep = command.iter().position(|a| a == "--").unwrap();
            let mut expected = vec![WAYCAPTURE.to_string()];
            expected.extend(args);
            assert_eq!(command[sep + 1..], expected);
        }
        assert_eq!(h.actions.capture("bad;command").unwrap_err(), "Unknown capture action");
        assert!(h.actions.open("screen_record", false, "bottom", 76).unwrap());
        assert_eq!(h.spawner.last_argv().last().unwrap(), "--record");
        let missing = harness(&[], false);
        assert!(!missing.actions.capture("stop").unwrap());
        assert_eq!(*missing.errors.borrow(), v(&["Screen Recorder is not installed."]));
    }

    #[test]
    fn detached_service_preserves_literal_command_and_forwards_only_session_environment() {
        let h = harness(
            &["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE", "INVOCATION_ID", "UNRELATED"],
            true,
        );
        assert!(h.actions.open("calendar", false, "left", 91).unwrap());
        assert!(h.errors.borrow().is_empty());
        let command = h.spawner.last_argv();
        assert_eq!(h.spawner.last_environment(), None, "inherits the dock environment like Gio.Subprocess.new");
        let sep = command.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            command[sep + 1..],
            v(&[PYTHON, "/a path/$literal/.config/waybar/scripts/calendar-manager/calendar_manager.py", "--popup", "--popup-edge", "left", "--popup-offset", "91"])
        );
        assert_eq!(
            command[..sep],
            v(&[
                "systemd-run", "--user", "--quiet", "--collect", "--service-type=exec", "--slice=app.slice", "--expand-environment=no",
                "--setenv=WAYLAND_DISPLAY", "--setenv=DISPLAY", "--setenv=XAUTHORITY", "--setenv=XDG_RUNTIME_DIR",
                "--setenv=DBUS_SESSION_BUS_ADDRESS", "--setenv=XDG_CURRENT_DESKTOP", "--setenv=XDG_SESSION_TYPE",
            ])
        );
        h.spawner.finish_last(Ok((true, String::new())));
        assert!(h.errors.borrow().is_empty());
    }

    #[test]
    fn settings_launch_and_absent_environment_are_supported() {
        let h = harness(&[], true);
        assert!(h.actions.open("audio", true, "bottom", 76).unwrap());
        let command = h.spawner.last_argv();
        assert_eq!(command.last().unwrap(), "--settings");
        assert!(!command.iter().any(|a| a == "--popup" || a.starts_with("--setenv=")));
    }

    #[test]
    fn missing_application_reports_error_without_starting_a_process() {
        let h = harness(&[], false);
        assert!(!h.actions.open("network", false, "bottom", 76).unwrap());
        assert_eq!(h.spawner.count(), 0);
        assert_eq!(*h.errors.borrow(), v(&["The network app is not installed."]));
    }

    #[test]
    fn settings_and_monitor_shortcuts_start_our_apps_in_independent_services() {
        let h = harness(&["WAYLAND_DISPLAY", "INVOCATION_ID"], true);
        assert!(h.actions.open_global_settings());
        let prefix = v(&["systemd-run", "--user", "--quiet", "--collect", "--service-type=exec", "--slice=app.slice", "--expand-environment=no", "--setenv=WAYLAND_DISPLAY", "--"]);
        let mut expected = prefix.clone();
        expected.push("/a path/$literal/.local/bin/global-settings".into());
        assert_eq!(h.spawner.last_argv(), expected);
        assert!(h.actions.open_system_monitor());
        let mut expected = prefix;
        expected.push(SYSTEM_MONITOR.into());
        assert_eq!(h.spawner.last_argv(), expected);
        let missing = harness(&[], false);
        assert!(!missing.actions.open_global_settings());
        assert!(!missing.actions.open_system_monitor());
        assert_eq!(*missing.errors.borrow(), v(&["Global Settings is not installed.", "System Monitor is not installed."]));
        assert_eq!(missing.spawner.count(), 0);
    }

    #[test]
    fn systemd_launch_failure_reports_error_and_does_not_fall_back() {
        let h = harness(&[], true);
        *h.spawner.fail_next.borrow_mut() = Some(error("Could not start systemd-run"));
        assert!(!h.actions.open("power", false, "bottom", 76).unwrap());
        assert_eq!(h.spawner.count(), 1);
        assert_eq!(*h.errors.borrow(), v(&["Could not start systemd-run"]));
    }

    #[test]
    fn async_failure_is_reported_after_launch_returns() {
        for (result, expected) in [
            (Ok((false, "  Unit startup failed\n".to_string())), "Unit startup failed"),
            (Ok((false, String::new())), "The application could not be opened."),
            (Err(error("Transport failure")), "Transport failure"),
        ] {
            let h = harness(&[], true);
            assert!(h.actions.open("battery", false, "bottom", 76).unwrap());
            assert!(h.errors.borrow().is_empty());
            h.spawner.finish_last(result);
            assert_eq!(*h.errors.borrow(), v(&[expected]));
        }
    }

    #[test]
    fn logout_uses_only_explicit_compositor_exit_command() {
        let h = harness(&[], true);
        assert!(h.actions.logout());
        assert_eq!(h.spawner.last_argv(), v(&["labwc", "--exit"]));
    }

    #[test]
    fn fake_systemd_run_receives_exact_argv_and_reports_failure() {
        let dir = crate::core::testutil::TempDir::new();
        let record = dir.join("argv.txt");
        let script = dir.join("systemd-run");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\necho '  denied ' >&2\nexit 3\n", record.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = errors.clone();
        let mut actions = PanelActions::new(move |e| sink.borrow_mut().push(e));
        actions.systemd_run = script.to_string_lossy().into_owned();
        actions.home = dir.path().to_path_buf();
        actions.is_file = Box::new(|_| true);
        actions.has_env = Box::new(|_| false);
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                assert!(actions.open("weather", false, "right", 64).unwrap());
                suite_core::timer::wait_for(&context, std::time::Duration::from_secs(10), || !errors.borrow().is_empty());
            })
            .unwrap();
        assert_eq!(*errors.borrow(), v(&["denied"]));
        let argv: Vec<String> = std::fs::read_to_string(&record).unwrap().lines().map(String::from).collect();
        let script_path = format!("{}/.config/waybar/scripts/weather-manager/weather_manager.py", dir.path().display());
        assert_eq!(
            argv,
            v(&["--user", "--quiet", "--collect", "--service-type=exec", "--slice=app.slice", "--expand-environment=no", "--", PYTHON, &script_path, "--popup", "--popup-edge", "right", "--popup-offset", "64"])
        );
    }
}
