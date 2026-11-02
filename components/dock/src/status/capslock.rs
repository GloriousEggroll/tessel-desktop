//! The Caps Lock status stream (`tessel-capslock-status`): reads the keyboard
//! LEDs in sysfs (no `/dev/input` access needed), polls every 100 ms and
//! prints one JSON line whenever the state changes:
//! `{"text": "⇪" | "", "tooltip": "Caps Lock: on|off", "class": "locked|unlocked"}`.

use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use tessel_core::pyfmt::{json_dumps, JsonStyle};

/// Where the kernel exposes the LED class devices.
pub const LEDS: &str = "/sys/class/leds";
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Any `<leds>/*::capslock/brightness` reads as a positive number.
pub fn capslock_is_on(leds: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(leds) else { return false };
    entries.flatten().filter(|entry| entry.file_name().to_string_lossy().ends_with("::capslock")).any(|entry| {
        std::fs::read_to_string(entry.path().join("brightness"))
            .ok()
            .and_then(|text| {
                let text = text.trim();
                if text.is_empty() { Some(0) } else { text.parse::<i64>().ok() }
            })
            .is_some_and(|value| value > 0)
    })
}

/// The status line for one state (Python `json.dumps` formatting).
pub fn status_line(locked: bool) -> String {
    let value = json!({
        "text": if locked { "⇪" } else { "" },
        "tooltip": format!("Caps Lock: {}", if locked { "on" } else { "off" }),
        "class": if locked { "locked" } else { "unlocked" },
    });
    json_dumps(&value, &JsonStyle::DEFAULT)
}

/// Tracks the last printed state; `next` returns a line only on change.
#[derive(Default)]
pub struct Watcher {
    previous: Option<bool>,
}

impl Watcher {
    pub fn next(&mut self, leds: &Path) -> Option<String> {
        let current = capslock_is_on(leds);
        if self.previous == Some(current) {
            return None;
        }
        self.previous = Some(current);
        Some(status_line(current))
    }
}

/// Stream forever (until stdout closes). Returns the exit status.
pub fn main() -> i32 {
    let leds = Path::new(LEDS);
    let mut watcher = Watcher::default();
    let stdout = std::io::stdout();
    loop {
        if let Some(line) = watcher.next(leds) {
            let mut out = stdout.lock();
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                return 0;
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testutil::TempDir;

    fn led(dir: &Path, name: &str, brightness: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("brightness"), brightness).unwrap();
    }

    #[test]
    fn lines_match_the_old_stream() {
        // Python's json.dumps escapes non-ASCII (U+21EA UPWARDS WHITE ARROW FROM BAR).
        assert_eq!(status_line(true), concat!(r#"{"text": ""#, "\\", r#"u21ea", "tooltip": "Caps Lock: on", "class": "locked"}"#));
        assert_eq!(status_line(false), r#"{"text": "", "tooltip": "Caps Lock: off", "class": "unlocked"}"#);
    }

    #[test]
    fn reads_any_capslock_led_and_prints_only_changes() {
        let dir = TempDir::new();
        let path = dir.path();
        assert!(!capslock_is_on(&path.join("missing")));
        led(path, "input3::numlock", "1\n");
        led(path, "input3::capslock", "0\n");
        led(path, "input9::capslock", "");
        let mut watcher = Watcher::default();
        assert_eq!(watcher.next(path).as_deref(), Some(status_line(false).as_str()));
        assert_eq!(watcher.next(path), None);
        led(path, "input9::capslock", "1\n");
        assert_eq!(watcher.next(path).as_deref(), Some(status_line(true).as_str()));
        assert_eq!(watcher.next(path), None);
        led(path, "input9::capslock", "garbage");
        assert_eq!(watcher.next(path).as_deref(), Some(status_line(false).as_str()));
        led(path, "input3::capslock", " 2 ");
        assert_eq!(watcher.next(path).as_deref(), Some(status_line(true).as_str()));
    }
}
