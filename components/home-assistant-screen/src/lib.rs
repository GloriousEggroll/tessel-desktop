//! Rust port of ~/btmanager/home-assistant-screen (see
//! contracts/home-assistant-screen.md): the Kasumi idle dashboard
//! (`luna_clock.py` → [`clock`]), its WebKit admin panel ([`panel`]) with the
//! embedded Waykeys keyboard ([`keyboard`]), idle-screen levels
//! ([`levels`]), timer/alarm cards ([`schedules`]) and the local voice
//! command backend ([`voice`]). The dashboard widgets themselves are
//! `suite_lock`'s (lock_dashboard, lock_info, lock_visuals, …).
pub mod clock;
pub mod keyboard;
pub mod levels;
pub mod panel;
pub mod schedules;
pub mod voice;
pub mod webkit;

/// Seconds on a monotonic clock (`time.monotonic()`).
pub fn monotonic() -> f64 { suite_lock::voice_state::monotonic() }

/// `logging.basicConfig(format='%(asctime)s %(levelname)s %(message)s')` on stderr.
pub fn log(level: &str, message: &str) {
    let now = glib::DateTime::now_local().ok();
    let stamp = now.as_ref().and_then(|n| n.format("%Y-%m-%d %H:%M:%S").ok()).map(|s| s.to_string()).unwrap_or_default();
    let millis = now.map(|n| n.microsecond() / 1000).unwrap_or(0);
    eprintln!("{stamp},{millis:03} {level} {message}");
}
