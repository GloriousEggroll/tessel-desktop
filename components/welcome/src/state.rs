//! Setup state and launch helpers from welcome_ui.py.
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// `state_directory(environ)`: `$XDG_STATE_HOME or ~/.local/state` + `tessel`.
pub fn state_directory_from(state_home: Option<&str>, home: &Path) -> PathBuf {
    match state_home.filter(|s| !s.is_empty()) {
        Some(base) => PathBuf::from(base),
        None => home.join(".local/state"),
    }
    .join("tessel")
}

pub fn state_directory() -> PathBuf {
    state_directory_from(std::env::var("XDG_STATE_HOME").ok().as_deref(), &tessel_core::paths::home())
}

pub fn setup_done_path() -> PathBuf { state_directory().join("setup-done") }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStatus {
    pub ok: bool,
    pub failed: Vec<String>,
}

/// The last `install_all.py --apply` result, or None when there is none.
pub fn install_status(path: Option<&Path>) -> Option<InstallStatus> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(|| state_directory().join("install-status.json"));
    let text = std::fs::read(&path).ok()?;
    let data: Value = serde_json::from_slice(&text).ok()?;
    let object = data.as_object()?;
    let mut failed = Vec::new();
    // data.get('steps', []) — a non-list value iterates like Python only for lists/strings/dicts;
    // anything else raised in Python, which only lists hit in practice.
    if let Some(steps) = object.get("steps").and_then(Value::as_array) {
        for item in steps {
            if let Some(item) = item.as_object() {
                let status = item.get("status").and_then(Value::as_str);
                if matches!(status, Some("failed") | Some("not run")) {
                    failed.push(match item.get("step") {
                        None => "?".to_owned(),
                        Some(value) => tessel_core::pyjson::py_str(value),
                    });
                }
            }
        }
    }
    let ok = object.get("ok").is_some_and(tessel_core::pyjson::truthy) && failed.is_empty();
    Some(InstallStatus { ok, failed })
}

/// `datetime.now(timezone.utc).isoformat(timespec='seconds')`.
pub fn utc_now_iso() -> String {
    let now = glib::DateTime::now_utc().expect("utc time");
    now.format("%Y-%m-%dT%H:%M:%S+00:00").map(|s| s.to_string()).unwrap_or_default()
}

/// Atomically record that setup is complete (mkstemp `.setup-done-*` + replace).
pub fn mark_setup_done(path: Option<&Path>) -> std::io::Result<PathBuf> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(setup_done_path);
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;
    let (temporary, mut file) = loop {
        let name = parent.join(format!(".setup-done-{}", tessel_core::secrets::token_urlsafe(6)));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&name) {
            Ok(file) => break (name, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let written = file.write_all(format!("{}\n", utc_now_iso()).as_bytes()).and_then(|_| file.flush());
    drop(file);
    if let Err(error) = written.and_then(|_| std::fs::rename(&temporary, &path)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(path)
}

/// argv for Global Settings: the installed launcher, else the Rust binary
/// beside this executable, else the installed Python entry point.
pub fn settings_command(arguments: &[&str], home: Option<&Path>) -> Vec<String> {
    let home = home.map(Path::to_path_buf).unwrap_or_else(tessel_core::paths::home);
    let launcher = home.join(".local/bin/tessel-settings");
    let mut argv = if launcher.is_file() && tessel_core::shell::is_executable(&launcher) {
        vec![launcher.to_string_lossy().into_owned()]
    } else if let Some(binary) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|p| p.join("tessel-settings")))
        .filter(|p| p.is_file() && tessel_core::shell::is_executable(p)) {
        vec![binary.to_string_lossy().into_owned()]
    } else {
        vec!["/usr/bin/python3".to_owned(),
             home.join(".local/share/tessel-settings/global_settings.py").to_string_lossy().into_owned()]
    };
    argv.extend(arguments.iter().map(|a| a.to_string()));
    argv
}

/// `spawn(argv)`: start without waiting, output silenced.
pub fn spawn(argv: &[String]) -> bool {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    match gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("ERROR:root:Could not start {}: {}", argv.first().map(String::as_str).unwrap_or(""), error.message());
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("welcome-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn state_paths_follow_xdg_state_home() {
        assert_eq!(state_directory_from(Some("/x/state"), Path::new("/home/u")).join("setup-done"),
                   PathBuf::from("/x/state/tessel/setup-done"));
        assert_eq!(state_directory_from(Some(""), Path::new("/home/u")), PathBuf::from("/home/u/.local/state/tessel"));
    }

    #[test]
    fn install_status_reads_failed_steps() {
        let base = scratch("status");
        let path = base.join("status.json");
        assert_eq!(install_status(Some(&path)), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(install_status(Some(&path)), None);
        std::fs::write(&path, r#"{"ok": true, "steps": [{"step": "tessel_dock", "status": "ok"}]}"#).unwrap();
        assert_eq!(install_status(Some(&path)), Some(InstallStatus { ok: true, failed: vec![] }));
        std::fs::write(&path, r#"{"ok": false, "steps": [{"step": "gtk4", "status": "failed"}, {"step": "weather", "status": "not run"}, {"step": "hdr", "status": "skipped"}]}"#).unwrap();
        assert_eq!(install_status(Some(&path)), Some(InstallStatus { ok: false, failed: vec!["gtk4".into(), "weather".into()] }));
        std::fs::write(&path, r#"{"ok": true, "steps": [{"status": "failed"}]}"#).unwrap();
        assert_eq!(install_status(Some(&path)), Some(InstallStatus { ok: false, failed: vec!["?".into()] }));
        std::fs::write(&path, "[1]").unwrap();
        assert_eq!(install_status(Some(&path)), None);
    }

    #[test]
    fn mark_setup_done_is_atomic_and_repeatable() {
        let base = scratch("done");
        let path = base.join("state/tessel/setup-done");
        mark_setup_done(Some(&path)).unwrap();
        mark_setup_done(Some(&path)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("+00:00\n") && text.len() == "2026-10-29T12:00:00+00:00\n".len(), "{text:?}");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("setup-done")]);
    }

    #[test]
    fn settings_command_prefers_the_installed_launcher() {
        let base = scratch("launcher");
        let command = settings_command(&["--shortcuts"], Some(&base));
        assert_eq!(command.last().unwrap(), "--shortcuts");
        let launcher = base.join(".local/bin/tessel-settings");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&launcher, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&launcher, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(settings_command(&["--shortcuts"], Some(&base)), vec![launcher.to_string_lossy().into_owned(), "--shortcuts".into()]);
    }
}
