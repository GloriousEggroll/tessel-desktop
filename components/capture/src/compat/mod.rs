//! Helpers the shared crates do not provide yet (upstream candidates):
//! - `capture_selection`: shared/capture_selection.py server side (SelectionBroker)
//!   plus the client calls (duplicated in tessel-capture-picker/selection.rs).
//! - `dl`: a tiny dlopen/dlsym wrapper (also used ad hoc by tessel-network's libnm.rs).
//! - `xkb`: libxkbcommon keysym name lookup (ctypes in Python).
//! - `monotonic`: `time.monotonic()` (CLOCK_MONOTONIC seconds).

pub mod capture_selection;
pub mod dl;
pub mod xkb;

use std::path::PathBuf;

/// `Path.home()`: `$HOME`, else the passwd entry.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home);
    }
    glib::home_dir()
}

/// `time.monotonic()`: CLOCK_MONOTONIC in seconds (the clock the PipeWire reader
/// stamps frames with and tessel-lock passes to `lock-shot`).
pub fn monotonic() -> f64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as f64 + now.tv_nsec as f64 / 1e9
}

/// Python `repr()` of a str (single quotes unless the text contains only single quotes).
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr(list_of_str)`.
pub fn py_list_repr<S: AsRef<str>>(items: &[S]) -> String {
    format!("[{}]", items.iter().map(|s| py_repr(s.as_ref())).collect::<Vec<_>>().join(", "))
}

/// `json.dumps(value, indent=2)`.
pub fn json_indent2(value: &serde_json::Value) -> String {
    tessel_core::pyfmt::json_dumps(value, &tessel_core::pyfmt::JsonStyle { indent: Some(2), ..tessel_core::pyfmt::JsonStyle::DEFAULT })
}
