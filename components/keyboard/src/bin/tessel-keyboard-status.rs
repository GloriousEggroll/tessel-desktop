//! Tessel Dock status stream for Tessel Keyboard (osk_status.py): prints a JSON line
//! whenever the status changes, checking the state file once per second.
//! Never initialises GTK.
// Compiled in directly (not through the GTK library) so this process stays tiny.
#[allow(dead_code)]
#[path = "../status.rs"]
mod status;

use std::io::Write;
use tessel_core::pyfmt::{json_dumps, JsonStyle};

fn main() {
    let mut previous = None;
    let stdout = std::io::stdout();
    loop {
        let value = status::status(None);
        if previous.as_ref() != Some(&value) {
            let mut out = stdout.lock();
            if writeln!(out, "{}", json_dumps(&value, &JsonStyle::DEFAULT)).and_then(|_| out.flush()).is_err() {
                return; // BrokenPipeError: the dock went away.
            }
            previous = Some(value);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
