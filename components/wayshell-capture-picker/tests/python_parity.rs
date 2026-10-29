//! Byte parity with the Python modules on a generated corpus (skips when
//! python3 or ~/btmanager/wayshell is missing). Python is run isolated (-I).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use wayshell_capture_picker::portal_config::chooser_config;
use wayshell_capture_picker::sources::parse_sources;

fn python_dir() -> Option<PathBuf> {
    let dir = suite_core::paths::home().join("btmanager/wayshell");
    dir.join("capture_sources.py").is_file().then_some(dir)
}

fn python(script: &str, input: &[u8]) -> Option<Vec<u8>> {
    let dir = python_dir()?;
    let mut child = Command::new("python3").arg("-I").arg("-c").arg(script).arg(dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().ok()?;
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().ok()?;
    assert!(output.status.success());
    Some(output.stdout)
}

/// A deterministic corpus of awkward records.
fn corpus() -> Vec<u8> {
    let pieces = ["Monitor: ", "Window: ", "DP-1", " ", "  ", "\t", "(", ")", "(id)", " (x-1)", "\u{200b}", "\u{200e}", "\u{3000}",
                  "\u{a0}", "\x1f", "\x1c", "\u{e000}", "\u{378}", "日本語", "é", "\u{301}", "\u{2028}", "\r", "\u{feff}", "a", "Z"];
    let mut state: u64 = 0x9e3779b97f4a7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = String::new();
    for _ in 0..4000 {
        let length = 1 + next() % 8;
        for i in 0..length {
            let index = if i == 0 { (next() % 2) as usize } else { (next() % pieces.len() as u64) as usize };
            out.push_str(pieces[index]);
        }
        out.push('\n');
    }
    out.into_bytes()
}

#[test]
fn parse_sources_matches_python() {
    let data = corpus();
    let script = "import sys, json; sys.path.insert(0, sys.argv[1]); from capture_sources import parse_sources\n\
                  print(json.dumps([[s.token, s.kind, s.title, s.subtitle, s.icon_name] for s in parse_sources(sys.stdin.buffer.read())]))";
    let Some(expected) = python(script, &data) else { eprintln!("skipped: no python reference"); return };
    let expected: serde_json::Value = serde_json::from_slice(&expected).unwrap();
    let ours: Vec<serde_json::Value> = parse_sources(&data).unwrap().into_iter()
        .map(|s| serde_json::json!([s.token, s.kind.name(), s.title, s.subtitle, s.icon_name])).collect();
    let expected = expected.as_array().unwrap();
    assert!(expected.len() > 100, "corpus too small: {}", expected.len());
    assert_eq!(ours.len(), expected.len());
    for (a, b) in ours.iter().zip(expected) {
        assert_eq!(a, b);
    }
}

#[test]
fn chooser_config_matches_python() {
    let inputs = ["", "x", "[screencast]", "[screencast]\nchooser_cmd=a\nchooser_cmd=b\n", " [screencast] # c\nchooser_type =x\n",
                  "[ screencast ]\nchooser_type=x\n", "[screencast]]\nchooser_type=x\n", "[a]\n[screencast]\n\x0cchooser_type=x\u{85}y\n",
                  "[screencast]\r\nchooser_type=x\r\n[b]\r\n", "chooser_type=top\n[screencast]\n", "[screencast]\nchooser_typex=1\n",
                  "[screencast]\u{2029}chooser_cmd=1", "[screencast]\n\u{3000}chooser_cmd\u{3000}=1\n"];
    let launchers = ["/tmp/picker", "/home/a user/it's/picker"];
    let payload = serde_json::to_vec(&serde_json::json!([inputs, launchers])).unwrap();
    let script = "import sys, json; sys.path.insert(0, sys.argv[1]); from pathlib import Path; from capture_portal_config import chooser_config\n\
                  inputs, launchers = json.load(sys.stdin)\n\
                  print(json.dumps([[chooser_config(i, Path(l)) for l in launchers] for i in inputs]))";
    let Some(expected) = python(script, &payload) else { eprintln!("skipped: no python reference"); return };
    let expected: Vec<Vec<String>> = serde_json::from_slice(&expected).unwrap();
    for (input, row) in inputs.iter().zip(expected) {
        for (launcher, want) in launchers.iter().zip(row) {
            assert_eq!(chooser_config(input, std::path::Path::new(launcher)).unwrap(), want, "{input:?} {launcher}");
        }
    }
}
