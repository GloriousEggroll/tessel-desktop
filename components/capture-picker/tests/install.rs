//! test_install_capture_picker.py, on scratch homes under $TMPDIR with a fake
//! command runner: nothing is ever restarted or enabled.

use std::path::{Path, PathBuf};
use tessel_capture_picker::install::*;

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("capture-install-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[derive(Default)]
struct Fake {
    calls: Vec<Vec<String>>,
    session_paths: String,
    fail_restart: bool,
}

impl Commands for Fake {
    fn run(&mut self, argv: &[&str], check: bool) -> Result<CommandResult, String> {
        self.calls.push(argv.iter().map(|s| s.to_string()).collect());
        if argv.contains(&"restart") && check && self.fail_restart {
            return Err("restart failed".into());
        }
        let stdout = if argv.contains(&"is-enabled") { "static\n".to_owned() }
                     else if argv.contains(&"tree") { self.session_paths.clone() } else { String::new() };
        Ok(CommandResult { returncode: 0, stdout, stderr: String::new() })
    }
}

#[test]
fn plan_bundles_the_binary_launcher_and_only_desktop_chooser_configs() {
    let home = scratch("plan");
    let files = plan(&home, None, &home.join("system"), b"\x7fELF").unwrap();
    let paths: Vec<&Path> = files.iter().map(|(p, _, _)| p.as_path()).collect();
    assert!(paths.contains(&home.join(".local/bin/tessel-capture-picker").as_path()));
    assert!(paths.contains(&home.join(".local/share/tessel-shell/tessel-capture-picker").as_path()));
    let configs: std::collections::BTreeSet<String> = paths.iter().filter(|p| p.starts_with(home.join(".config")))
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(configs, ["labwc".to_owned(), "wlroots".to_owned()].into());
    assert!(!paths.iter().any(|p| p.exists()));
    let launcher = &files.iter().find(|(p, _, _)| p.ends_with(".local/bin/tessel-capture-picker")).unwrap().1;
    assert_eq!(String::from_utf8(launcher.clone()).unwrap(),
               format!("#!/bin/sh\nexec {} \"$@\"\n", tessel_core::shell::shell_quote(&home.join(BINARY).to_string_lossy())));
    // The chooser command keeps pointing at the launcher (unchanged for xdpw).
    let config = String::from_utf8(files.iter().find(|(p, _, _)| p.ends_with("wlroots")).unwrap().1.clone()).unwrap();
    assert!(config.contains(&format!("chooser_cmd=exec {}\n", tessel_core::shell::shell_quote(&home.join(LAUNCHER).to_string_lossy()))));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn failed_restart_rolls_back_files_without_enabling_services() {
    let home = scratch("rollback");
    let target = home.join(".config/xdg-desktop-portal-wlr/wlroots");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let original = b"[screencast]\nmax_fps=60\n";
    std::fs::write(&target, original).unwrap();
    let files = plan(&home, None, &home.join("system"), b"binary").unwrap();
    let mut fake = Fake { fail_restart: true, ..Default::default() };
    let error = install(&home, &files, true, false, &mut fake, &mut Vec::new()).unwrap_err();
    assert!(error.contains("restart failed"));
    assert_eq!(std::fs::read(&target).unwrap(), original);
    assert!(!home.join(".local/bin/tessel-capture-picker").exists());
    assert!(!home.join(BINARY).exists());
    assert!(!fake.calls.iter().any(|argv| argv.iter().any(|a| a == "enable" || a == "disable")));
    // The backup keeps the original and a manifest.
    let backups: Vec<_> = std::fs::read_dir(home.join(".local/state/tessel/backups")).unwrap().flatten().collect();
    assert_eq!(backups.len(), 1);
    let manifest = std::fs::read_to_string(backups[0].path().join("manifest.json")).unwrap();
    assert!(manifest.starts_with("[\n  {\n    \"path\": ") && manifest.contains("\"before\": \"file\""), "{manifest}");
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn active_capture_is_not_interrupted() {
    let home = scratch("active");
    let files = plan(&home, None, &home.join("system"), b"binary").unwrap();
    let mut fake = Fake { session_paths: "/org/freedesktop/portal/desktop/session/1_20/obs\n".into(), ..Default::default() };
    let error = install(&home, &files, true, false, &mut fake, &mut Vec::new()).unwrap_err();
    assert!(error.contains("sharing session is active"));
    assert!(!home.join(".local/bin/tessel-capture-picker").exists());
    assert!(!fake.calls.iter().any(|argv| argv.iter().any(|a| a == "restart")));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn preview_and_successful_apply_are_idempotent() {
    let home = scratch("apply");
    let files = plan(&home, None, &home.join("system"), b"binary").unwrap();
    let mut fake = Fake::default();
    let mut out = Vec::new();
    install(&home, &files, false, false, &mut fake, &mut out).unwrap();
    assert!(fake.calls.is_empty(), "preview runs nothing");
    assert!(String::from_utf8(out).unwrap().ends_with("4 files prepared. KDE portal preferences and service enablement are unchanged.\n"));
    let mut out = Vec::new();
    install(&home, &files, true, false, &mut fake, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Installed Tessel Shell capture picker. Refreshed the wlroots portal."), "{text}");
    for (path, data, mode) in &files {
        assert_eq!(&std::fs::read(path).unwrap(), data);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o7777, *mode);
    }
    let mut out = Vec::new();
    install(&home, &files, true, false, &mut fake, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "0 files prepared. KDE portal preferences and service enablement are unchanged.\n");
    std::fs::remove_dir_all(&home).unwrap();
}
