//! Resolve capture notification placement from the chosen portal source
//! (capture_output.py), and the `waydock-toplevel --snapshot` runner shared
//! with `icons` (capture_icons.py).

use crate::sources::{display_text, CaptureSource, Kind};
use serde_json::{Map, Value};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `[<dir of this program>/../waydock/waydock-toplevel, ~/.local/share/waydock/waydock-toplevel]`
/// (Python: `Path(__file__).parent.parent / 'waydock/waydock-toplevel'`).
pub fn helper_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(override_path) = std::env::var_os("WAYSHELL_CAPTURE_TOPLEVEL_HELPER") {
        // Rust-only, for tests: an explicit helper replaces the search.
        return vec![PathBuf::from(override_path)];
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent().and_then(|p| p.parent()) {
            candidates.push(parent.join("waydock/waydock-toplevel"));
        }
    }
    candidates.push(suite_core::paths::home().join(".local/share/waydock/waydock-toplevel"));
    candidates
}

pub fn helper() -> Option<PathBuf> {
    helper_candidates().into_iter().find(|path| path.is_file())
}

/// `subprocess.run([helper, '--snapshot'], capture_output=True, check=True, timeout=…)`
/// parsed as JSON lines. Any failure (spawn, timeout, non-zero exit, bad UTF-8
/// or JSON, a non-object line) is `None`.
pub fn snapshot(helper: &std::path::Path, timeout: Duration) -> Option<Vec<Map<String, Value>>> {
    let mut child = Command::new(helper).arg("--snapshot").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = stdout.read_to_end(&mut data);
        data
    });
    let drain = std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = stderr.read_to_end(&mut data);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let data = reader.join().ok()?;
    let _ = drain.join();
    if !status?.success() {
        return None;
    }
    let text = String::from_utf8(data).ok()?;
    let mut items = Vec::new();
    for line in suite_lines(&text) {
        match serde_json::from_str::<Value>(line).ok()? {
            Value::Object(map) => items.push(map),
            _ => return None,
        }
    }
    Some(items)
}

/// `str.splitlines()` (without line ends).
fn suite_lines(text: &str) -> Vec<&str> {
    crate::portal_config::splitlines_keepends(text)
        .into_iter()
        .map(|line| {
            let line = line.strip_suffix("\r\n").unwrap_or(line);
            let mut chars = line.chars();
            match chars.next_back() {
                Some('\n' | '\r' | '\x0B' | '\x0C' | '\x1C' | '\x1D' | '\x1E' | '\u{85}' | '\u{2028}' | '\u{2029}') => chars.as_str(),
                _ => line,
            }
        })
        .collect()
}

fn text(item: &Map<String, Value>, key: &str) -> String {
    match item.get(key) {
        Some(Value::String(value)) => value.clone(),
        _ => String::new(),
    }
}

pub fn matching_outputs(source: Option<&CaptureSource>, windows: &[Map<String, Value>]) -> Vec<String> {
    let Some(source) = source else { return Vec::new() };
    if source.kind == Kind::Monitor {
        return vec![source.subtitle.clone()];
    }
    let mut candidates: Vec<Vec<String>> = Vec::new();
    for item in windows {
        if item.get("event").and_then(Value::as_str) != Some("window") || display_text(&text(item, "title")) != source.title {
            continue;
        }
        let outputs = match item.get("outputs") {
            Some(Value::Array(list)) => list.iter().map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())).collect(),
            _ => Vec::new(),
        };
        candidates.push(outputs);
    }
    // Do not guess between indistinguishable same-title windows on different
    // screens. Their requesting app retains its pointer-screen fallback.
    match candidates.first() {
        Some(first) if candidates.iter().all(|c| c == first) => first.clone(),
        _ => Vec::new(),
    }
}

/// Monitor geometry for the `capture-output` match: (connector, x, y, w, h).
pub type MonitorGeometry = (String, i64, i64, i64, i64);

/// The current GDK display's monitors (requires GTK initialised).
pub fn gdk_monitors() -> Vec<MonitorGeometry> {
    use gtk::prelude::*;
    // Python would raise on a missing display; without GTK there is no match.
    if !gtk::is_initialized_main_thread() {
        return Vec::new();
    }
    let Some(display) = gtk::gdk::Display::default() else { return Vec::new() };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok()))
        .map(|monitor| {
            let area = monitor.geometry();
            (monitor.connector().map(|c| c.to_string()).unwrap_or_default(), area.x() as i64, area.y() as i64,
             area.width() as i64, area.height() as i64)
        })
        .collect()
}

/// The tiling answer for `capture-output <identifier>`: `Ok(Some(outputs))`
/// when the compositor supports it, `Ok(None)` to fall back, `Err` likewise.
pub fn tiling_outputs(reply: &Map<String, Value>, monitors: &[MonitorGeometry]) -> Option<Vec<String>> {
    if !reply.get("capture_output_supported").is_some_and(suite_core::pyjson::truthy) {
        return None;
    }
    let boxed = reply.get("capture_output");
    if !boxed.is_some_and(suite_core::pyjson::truthy) {
        return Some(Vec::new());
    }
    // `box[key]` for x/y/width/height; a missing key (KeyError) or a
    // non-dict falls back to the helper, like Python's except clause.
    let Some(Value::Object(area)) = boxed else { return None };
    let mut wanted = Vec::new();
    for key in ["x", "y", "width", "height"] {
        wanted.push(area.get(key)?.clone());
    }
    for (connector, x, y, width, height) in monitors {
        let have = [*x, *y, *width, *height];
        if wanted.iter().zip(have).all(|(value, have)| suite_core::pyjson::eq_int(value, Some(have))) {
            return Some(vec![connector.clone()]);
        }
    }
    Some(Vec::new())
}

pub struct Resolvers<'a> {
    pub tiling: &'a dyn Fn(&str) -> Result<Map<String, Value>, String>,
    pub monitors: &'a dyn Fn() -> Vec<MonitorGeometry>,
    pub snapshot: &'a dyn Fn() -> Option<Vec<Map<String, Value>>>,
}

pub fn selected_outputs_with(source: Option<&CaptureSource>, resolvers: &Resolvers) -> Vec<String> {
    let Some(window) = source.filter(|s| s.kind == Kind::Window) else { return matching_outputs(source, &[]) };
    // New labwc exposes the portal's exact foreign-toplevel identifier. It is
    // independent of activation, duplicate titles, tiling, and GTK focus.
    if let Ok(reply) = (resolvers.tiling)(&format!("capture-output {}", window.subtitle)) {
        if let Some(outputs) = tiling_outputs(&reply, &(resolvers.monitors)()) {
            return outputs;
        }
    }
    // Compatibility while the previous compositor finishes its session.
    match (resolvers.snapshot)() {
        Some(windows) => matching_outputs(source, &windows),
        None => Vec::new(),
    }
}

pub fn selected_outputs(source: Option<&CaptureSource>) -> Vec<String> {
    let tiling = |command: &str| tiling_manager::client::request(command).map_err(|e| e.to_string());
    let snapshot = || helper().and_then(|path| snapshot(&path, Duration::from_secs(1)));
    selected_outputs_with(source, &Resolvers { tiling: &tiling, monitors: &gdk_monitors, snapshot: &snapshot })
}
