//! The Bluetooth settings shortcut (`BatteryApplication.open_bluetooth`):
//! an independent transient user service, never a scope, never waited on.

use std::path::{Path, PathBuf};

pub const FORWARDED_ENVIRONMENT: [&str; 7] = ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
                                              "DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"];

/// The installed `tessel-bluetooth` program, if any.
pub fn bluetooth_program() -> Option<PathBuf> {
    found(tessel_core::paths::program("tessel-bluetooth"))
}

/// `program()` gives an absolute path when it found the program, else the bare name for `PATH`.
fn found(path: PathBuf) -> Option<PathBuf> {
    if path.is_absolute() {
        tessel_core::shell::is_executable(&path).then_some(path)
    } else {
        tessel_core::shell::which(&path.to_string_lossy())
    }
}

/// `systemd-run --user --collect --quiet --service-type=exec --slice=app.slice
/// --expand-environment=no [--setenv=NAME…] -- <program>`.
pub fn command(launcher: &Path, program: &Path, has_env: impl Fn(&str) -> bool) -> Vec<std::ffi::OsString> {
    let mut command: Vec<std::ffi::OsString> = vec![launcher.into()];
    command.extend(["--user", "--collect", "--quiet", "--service-type=exec", "--slice=app.slice", "--expand-environment=no"].map(Into::into));
    command.extend(FORWARDED_ENVIRONMENT.iter().filter(|name| has_env(name)).map(|name| format!("--setenv={name}").into()));
    command.extend(["--".into(), program.into()]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_launch_is_independent_and_preserves_spaced_path() {
        let program = Path::new("/tmp/test suite/tessel-bluetooth");
        let command = command(Path::new("/usr/bin/systemd-run"), program,
                              |name| name == "WAYLAND_DISPLAY" || name == "XDG_RUNTIME_DIR");
        let words: Vec<String> = command.iter().map(|w| w.to_string_lossy().into_owned()).collect();
        assert!(words.contains(&"--service-type=exec".to_owned()));
        assert!(!words.iter().any(|w| w == "--scope" || w == "--wait"));
        assert_eq!(words.last().unwrap(), "/tmp/test suite/tessel-bluetooth");
        assert_eq!(words[7..9], ["--setenv=WAYLAND_DISPLAY".to_owned(), "--setenv=XDG_RUNTIME_DIR".to_owned()]);
        assert_eq!(words[9], "--");
        assert_eq!(words.len(), 11);
    }

    #[test]
    fn program_lookup_needs_an_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("battery-bt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let program = root.join("tessel-bluetooth");
        assert_eq!(found(program.clone()), None);
        std::fs::write(&program, "").unwrap();
        assert_eq!(found(program.clone()), None);
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(found(program.clone()), Some(program));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
