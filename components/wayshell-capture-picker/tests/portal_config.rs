//! test_capture_portal_config.py.

use std::path::{Path, PathBuf};
use wayshell_capture_picker::portal_config::*;

/// configparser-like reading of the last `[screencast]` values (strict=False).
fn screencast(text: &str) -> std::collections::HashMap<String, String> {
    let mut section = String::new();
    let mut values = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_owned();
        } else if section == "screencast" {
            if let Some((k, v)) = line.split_once('=') {
                values.insert(k.trim().to_owned(), v.trim().to_owned());
            }
        }
    }
    values
}

#[test]
fn updates_only_chooser_and_is_idempotent() {
    let original = "# custom capture options\n[screencast]\nmax_fps=60\nchooser_type=default\nchooser_cmd=slurp -o\nforce_mod_linear=1\nexec_before=notify-start\n[other]\nchoice=preserved\n";
    let launcher = Path::new("/home/a user/.local/bin/wayshell-capture-picker");
    let result = chooser_config(original, launcher).unwrap();
    assert!(result.contains("chooser_type=dmenu\n"));
    assert!(result.contains("chooser_cmd=exec '/home/a user/.local/bin/wayshell-capture-picker'\n"));
    assert!(result.contains("max_fps=60\n"));
    assert!(result.contains("exec_before=notify-start\n"));
    assert!(result.contains("[other]\nchoice=preserved\n"));
    assert_eq!(chooser_config(&result, launcher).unwrap(), result);
}

#[test]
fn fills_missing_chooser_keys_without_losing_other_sections() {
    for original in ["", "# comment", "[screencast]\nmax_fps=30\n", "[screencast]\nchooser_type=simple\n[other]\nx=y\n"] {
        let result = chooser_config(original, Path::new("/tmp/picker")).unwrap();
        let parsed = screencast(&result);
        assert_eq!(parsed["chooser_type"], "dmenu", "{original:?}");
        assert_eq!(parsed["chooser_cmd"], "exec /tmp/picker", "{original:?}");
    }
}

#[test]
fn byte_exact_python_outputs() {
    // Expected strings produced by capture_portal_config.chooser_config.
    let launcher = Path::new("/tmp/picker");
    let cases = [
        ("", "[screencast]\nchooser_type=dmenu\nchooser_cmd=exec /tmp/picker\n"),
        ("# comment", "# comment\n\n[screencast]\nchooser_type=dmenu\nchooser_cmd=exec /tmp/picker\n"),
        ("[screencast]\nmax_fps=30\n", "[screencast]\nmax_fps=30\nchooser_type=dmenu\nchooser_cmd=exec /tmp/picker\n"),
        ("[screencast]\nchooser_type=simple\n[other]\nx=y\n",
         "[screencast]\nchooser_type=dmenu\n[other]\nx=y\n\n[screencast]\nchooser_cmd=exec /tmp/picker\n"),
        ("[screencast] ; c\r\n  chooser_cmd = old\r\n", "[screencast] ; c\r\nchooser_cmd=exec /tmp/picker\nchooser_type=dmenu\n"),
        ("[screencast]\x0bchooser_type=x\n", "[screencast]\x0bchooser_type=dmenu\nchooser_cmd=exec /tmp/picker\n"),
    ];
    for (original, expected) in cases {
        assert_eq!(chooser_config(original, launcher).unwrap(), expected, "{original:?}");
    }
    assert!(chooser_config("", Path::new("/a\nb")).is_err());
}

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("capture-config-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn desktop_config_inherits_effective_fallback_without_changing_kde() {
    let temporary = scratch("inherit");
    let (config, system) = (temporary.join("user"), temporary.join("system"));
    let local = config.join("xdg-desktop-portal-wlr");
    let shared = system.join("xdg-desktop-portal-wlr");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(local.join("config"), "[screencast]\nmax_fps=72\n").unwrap();
    std::fs::write(shared.join("labwc"), "[screencast]\nmax_fps=90\n").unwrap();
    std::fs::write(local.join("wlroots"), "[screencast]\nmax_fps=120\n").unwrap();
    std::fs::write(local.join("KDE"), "untouched").unwrap();
    let files = configuration_files(&config, Path::new("/tmp/picker"), &system).unwrap();
    let names: std::collections::BTreeSet<_> = files.iter().map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["labwc".to_owned(), "wlroots".to_owned()].into());
    let value = |name: &str| String::from_utf8(files.iter().find(|(p, _, _)| p.ends_with(name)).unwrap().1.clone()).unwrap();
    assert!(value("wlroots").contains("max_fps=120"));
    assert!(value("labwc").contains("max_fps=72"));
    assert!(files.iter().all(|(_, _, mode)| *mode == 0o644));
    assert_eq!(std::fs::read_to_string(local.join("KDE")).unwrap(), "untouched");
    assert!(!local.join("labwc").exists(), "plan remains read-only");
    std::fs::remove_dir_all(&temporary).unwrap();
}

#[test]
fn splitlines_matches_python() {
    assert_eq!(splitlines_keepends("a\r\nb\rc\x0bd\u{2028}e\n\nf"), ["a\r\n", "b\r", "c\x0b", "d\u{2028}", "e\n", "\n", "f"]);
    assert!(splitlines_keepends("").is_empty());
}
