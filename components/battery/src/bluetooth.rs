//! The Bluetooth settings shortcut (`BatteryApplication.open_bluetooth`):
//! an independent transient user service, never a scope, never waited on.

use std::path::{Path, PathBuf};

pub const FORWARDED_ENVIRONMENT: [&str; 7] = ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
                                              "DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"];

/// The suite's scripts root (`~/.config/waybar/scripts`, the Python file's
/// `parent.parent`); the binary's own `../..` wins when it holds the Bluetooth manager.
pub fn script_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = std::env::current_exe().ok().and_then(|exe| exe.parent()?.parent().map(Path::to_path_buf)) {
        roots.push(root);
    }
    roots.push(tessel_core::paths::config_home().join("waybar/scripts"));
    roots
}

/// `_bluetooth_script()`: `<root>/tessel-bluetooth/tessel-bluetooth.py` or `<root>/tessel-bluetooth.py`.
pub fn bluetooth_script(roots: &[PathBuf]) -> Option<PathBuf> {
    roots.iter().flat_map(|root| [root.join("tessel-bluetooth/tessel-bluetooth.py"), root.join("tessel-bluetooth.py")])
        .find(|path| path.is_file())
}

/// The Python interpreter that runs the (still Python) Bluetooth manager.
pub fn python() -> PathBuf {
    tessel_core::shell::which("python3").unwrap_or_else(|| PathBuf::from("/usr/bin/python3"))
}

/// `systemd-run --user --collect --quiet --service-type=exec --slice=app.slice
/// --expand-environment=no [--setenv=NAME…] -- <python> <script>`.
pub fn command(launcher: &Path, python: &Path, script: &Path, has_env: impl Fn(&str) -> bool) -> Vec<std::ffi::OsString> {
    let mut command: Vec<std::ffi::OsString> = vec![launcher.into()];
    command.extend(["--user", "--collect", "--quiet", "--service-type=exec", "--slice=app.slice", "--expand-environment=no"].map(Into::into));
    command.extend(FORWARDED_ENVIRONMENT.iter().filter(|name| has_env(name)).map(|name| format!("--setenv={name}").into()));
    command.extend(["--".into(), python.into(), script.into()]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_launch_is_independent_and_preserves_spaced_path() {
        let script = Path::new("/tmp/test suite/tessel-bluetooth.py");
        let command = command(Path::new("/usr/bin/systemd-run"), Path::new("/usr/bin/python3"), script,
                              |name| name == "WAYLAND_DISPLAY" || name == "XDG_RUNTIME_DIR");
        let words: Vec<String> = command.iter().map(|w| w.to_string_lossy().into_owned()).collect();
        assert!(words.contains(&"--service-type=exec".to_owned()));
        assert!(!words.iter().any(|w| w == "--scope" || w == "--wait"));
        assert_eq!(words.last().unwrap(), "/tmp/test suite/tessel-bluetooth.py");
        assert_eq!(words[7..9], ["--setenv=WAYLAND_DISPLAY".to_owned(), "--setenv=XDG_RUNTIME_DIR".to_owned()]);
        assert_eq!(words[9], "--");
    }

    #[test]
    fn script_lookup_prefers_package_directory() {
        let root = std::env::temp_dir().join(format!("battery-bt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("tessel-bluetooth")).unwrap();
        assert_eq!(bluetooth_script(std::slice::from_ref(&root)), None);
        std::fs::write(root.join("tessel-bluetooth.py"), "").unwrap();
        assert_eq!(bluetooth_script(std::slice::from_ref(&root)), Some(root.join("tessel-bluetooth.py")));
        std::fs::write(root.join("tessel-bluetooth/tessel-bluetooth.py"), "").unwrap();
        assert_eq!(bluetooth_script(std::slice::from_ref(&root)), Some(root.join("tessel-bluetooth/tessel-bluetooth.py")));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
