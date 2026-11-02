//! `--settings`: launch `tessel-displays.desktop` the way Tessel Dock's
//! `app_catalog.AppCatalog().launch(id, context, callback)` does: a separate
//! systemd user service running Tessel Dock's Gio launch helper, so the display
//! manager survives this service restarting and is never tied to it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SETTINGS_DESKTOP_ID: &str = "tessel-displays.desktop";

/// Execution metadata that belongs to this service, not to the new app.
pub const SERVICE_ENV: [&str; 19] = [
    "INVOCATION_ID", "JOURNAL_STREAM", "NOTIFY_SOCKET", "WATCHDOG_PID", "WATCHDOG_USEC", "WATCHDOG_DEVICE", "LISTEN_PID",
    "LISTEN_FDS", "LISTEN_FDNAMES", "SYSTEMD_EXEC_PID", "MAINPID", "MANAGERPID", "PIDFILE", "EXIT_CODE", "EXIT_STATUS",
    "SERVICE_RESULT", "GIO_LAUNCHED_DESKTOP_FILE", "GIO_LAUNCHED_DESKTOP_FILE_PID", "WAYDOCK_LAUNCH_STARTUP_ID",
];

/// `app_catalog.launch_command(helper, environment, unit, service_type="forking")`.
pub fn launch_command(helper: &[String], environment: &BTreeMap<String, String>, unit: &str) -> Vec<String> {
    let valid = |name: &str| {
        let mut chars = name.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let mut command: Vec<String> = ["/usr/bin/systemd-run", "--user", "--collect", "--quiet", "--service-type=forking",
        "--property=GuessMainPID=no", "--property=ExitType=cgroup", "--property=TimeoutStartSec=90s", "--slice=app.slice",
        "--expand-environment=no"].iter().map(|s| s.to_string()).collect();
    command.push(format!("--unit={unit}"));
    command.extend(environment.keys().filter(|k| valid(k)).map(|k| format!("--setenv={k}")));
    command.push("--".into());
    command.extend(helper.iter().cloned());
    command
}

/// `app-tessel-dock-<uuid4 hex>.service`.
pub fn unit_name() -> String {
    let mut bytes = [0u8; 16];
    // SAFETY: fills the local buffer.
    unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!("app-tessel-dock-{}.service", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Tessel Dock's launch helper: `../tessel-dock/app_catalog.py` next to this
/// install's scripts directory, else `~/.local/share/tessel-dock/app_catalog.py`.
pub fn helper_script(executable_dir: Option<&Path>) -> Option<PathBuf> {
    let source = executable_dir.and_then(Path::parent).map(|p| p.join("tessel-dock/app_catalog.py"));
    if let Some(source) = source.filter(|p| p.is_file()) {
        return Some(source);
    }
    let installed = tessel_core::paths::data_home().join("tessel-dock/app_catalog.py");
    installed.is_file().then_some(installed)
}

/// The environment handed to the app service (launch context minus service metadata).
pub fn launch_environment(base: impl IntoIterator<Item = (String, String)>, startup_id: Option<String>) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = base.into_iter().filter(|(k, _)| !SERVICE_ENV.contains(&k.as_str())).collect();
    if let Some(token) = startup_id.filter(|t| !t.is_empty()) {
        environment.insert("WAYDOCK_LAUNCH_STARTUP_ID".into(), token);
    }
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_matches_tessel_dock_launcher() {
        let environment: BTreeMap<String, String> = launch_environment(
            [("WAYLAND_DISPLAY".into(), "wayland-1".into()), ("INVOCATION_ID".into(), "x".into()), ("BAD-NAME".into(), "y".into())],
            Some("token".into()));
        let helper = vec!["/usr/bin/python3".to_owned(), "/w/app_catalog.py".into(), "--launch-desktop".into(), SETTINGS_DESKTOP_ID.into()];
        let command = launch_command(&helper, &environment, "app-tessel-dock-0.service");
        assert_eq!(command[..11].join(" "), "/usr/bin/systemd-run --user --collect --quiet --service-type=forking --property=GuessMainPID=no --property=ExitType=cgroup --property=TimeoutStartSec=90s --slice=app.slice --expand-environment=no --unit=app-tessel-dock-0.service");
        assert_eq!(&command[11..13], ["--setenv=WAYDOCK_LAUNCH_STARTUP_ID", "--setenv=WAYLAND_DISPLAY"]);
        assert!(!command.iter().any(|w| w.contains("INVOCATION_ID") || w.contains("BAD-NAME")));
        assert_eq!(&command[13..], ["--", "/usr/bin/python3", "/w/app_catalog.py", "--launch-desktop", "tessel-displays.desktop"]);
        let unit = unit_name();
        assert!(unit.starts_with("app-tessel-dock-") && unit.ends_with(".service") && unit.len() == "app-tessel-dock-.service".len() + 32);
    }
}
