//! Audio-specific clock. The thread-default-context timers (`GLib.timeout_add`
//! for non-`Send` closures) and main-loop test helpers live in `tessel_core::timer`.

use std::time::Instant;

pub use tessel_core::timer::{once, repeat, settle, wait_for, Timeout};

/// `time.monotonic()` in seconds.
pub fn monotonic() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    // Offset so deadlines of 0 (the Python tests' "expired") are in the past.
    START.get_or_init(Instant::now).elapsed().as_secs_f64() + 1000.0
}
