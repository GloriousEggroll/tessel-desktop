//! The production `login-settings-helper` binary, run only as this unprivileged
//! test process: it must refuse before reading stdin or touching any file,
//! exactly like login_settings_helper.py (`Administrator authorization is required`).
use std::io::Write;
use std::process::{Command, Stdio};

fn helper(args: &[&str], stdin: &[u8]) -> (Option<i32>, String, String) {
    // SAFETY: geteuid has no preconditions.
    assert_ne!(unsafe { libc::geteuid() }, 0, "never run the helper tests as root");
    let mut child = Command::new(env!("CARGO_BIN_EXE_login-settings-helper")).args(args).env_clear()
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let _ = child.stdin.take().unwrap().write_all(stdin);
    let output = child.wait_with_output().unwrap();
    (output.status.code(), String::from_utf8(output.stdout).unwrap(), String::from_utf8(output.stderr).unwrap())
}

#[test]
fn refuses_when_not_root() {
    let request = br#"{"expected": {}, "appearance": {}}"#;
    assert_eq!(helper(&[], request), (Some(1), String::new(), "Administrator authorization is required\n".into()));
}

#[test]
fn refuses_any_argument() {
    for args in [&["--config"][..], &["/tmp/x"], &["-"], &[""]] {
        assert_eq!(helper(args, b""), (Some(1), String::new(), "Administrator authorization is required\n".into()));
    }
}

#[test]
fn oversized_input_is_still_refused_without_root() {
    assert_eq!(helper(&[], &vec![b'x'; 70000]).0, Some(1));
}

#[test]
fn helper_does_not_link_gtk() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_login-settings-helper")).unwrap();
    for library in [&b"libgtk-4.so"[..], b"libgtk4-layer-shell", b"libudev"] {
        assert!(!binary.windows(library.len()).any(|w| w == library), "{}", String::from_utf8_lossy(library));
    }
}
