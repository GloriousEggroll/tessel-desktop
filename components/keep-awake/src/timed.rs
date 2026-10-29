//! Turn Keep Awake off automatically after the chosen time
//! (port of keep_awake_backend.py `TimedBackend`, `DURATIONS`, `timer_path`).
//!
//! The end time is saved, so a restarted session service keeps an unexpired
//! timer, and an expired one is released instead of being renewed.

use crate::backend::InhibitorBackend;
use serde_json::{Map, Number, Value};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use suite_core::pyfmt::{json_dumps, JsonStyle};

/// Minutes; 0 keeps the session awake until Keep Awake is turned off.
pub const DURATIONS: [u32; 4] = [30, 60, 120, 0];
pub const UNSUPPORTED_DURATION: &str = "Unsupported Keep Awake duration.";

/// `${XDG_STATE_HOME:-~/.local/state}/keep-awake/timer.json`.
pub fn timer_path() -> PathBuf { suite_core::paths::state_home().join("keep-awake").join("timer.json") }

/// The inhibitor the timer drives (the real [`InhibitorBackend`] or a test double).
pub trait Inhibit {
    fn start(&self);
    fn set_enabled(&self, enabled: bool) -> bool;
    fn toggle(&self) -> bool { self.set_enabled(!self.requested()) }
    fn requested(&self) -> bool;
    fn active(&self) -> bool { false }
    fn ready(&self) -> bool { false }
    fn pending(&self) -> bool { false }
    fn error(&self) -> String { String::new() }
    /// The backend's change callback (`backend.on_changed()`).
    fn on_changed(&self);
    fn close(&self);
}

/// The real backend together with the callback it was created with.
pub struct Login {
    pub backend: InhibitorBackend,
    pub on_changed: Rc<dyn Fn()>,
}

impl Inhibit for Login {
    fn start(&self) { self.backend.start() }
    fn set_enabled(&self, enabled: bool) -> bool { self.backend.set_enabled(enabled) }
    fn toggle(&self) -> bool { self.backend.toggle() }
    fn requested(&self) -> bool { self.backend.requested() }
    fn active(&self) -> bool { self.backend.active() }
    fn ready(&self) -> bool { self.backend.ready() }
    fn pending(&self) -> bool { self.backend.pending() }
    fn error(&self) -> String { self.backend.error() }
    fn on_changed(&self) { (self.on_changed)() }
    fn close(&self) { self.backend.close() }
}

struct Inner {
    backend: Rc<dyn Inhibit>,
    path: Option<PathBuf>,
    clock: Box<dyn Fn() -> f64>,
    duration: Cell<u32>,
    until: Cell<Option<f64>>,
    timer: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct TimedBackend(Rc<Inner>);

/// `(duration, until)` read from timer.json (`_load` rules).
pub fn load_timer(path: &Path, now: f64) -> (u32, Option<f64>) {
    let mut result = (0, None);
    let Ok(text) = std::fs::read_to_string(path) else { return result };
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&text) else { return result };
    // Python: `duration in DURATIONS` (False == 0 also matches; 30.0 == 30).
    match data.get("duration") {
        None => {}
        Some(Value::Bool(false)) => result.0 = 0,
        Some(Value::Number(number)) => {
            if let Some(value) = number.as_f64() {
                if let Some(minutes) = DURATIONS.iter().find(|m| **m as f64 == value) {
                    result.0 = *minutes;
                }
            }
        }
        Some(_) => {}
    }
    if let Some(Value::Number(number)) = data.get("until") {
        if let Some(until) = number.as_f64().filter(|u| u.is_finite() && *u > now) {
            result.1 = Some(until);
        }
    }
    result
}

/// The exact timer.json text Python writes.
pub fn timer_text(duration: u32, until: Option<f64>) -> String {
    let mut map = Map::new();
    map.insert("duration".into(), duration.into());
    map.insert("until".into(), until.and_then(Number::from_f64).map(Value::Number).unwrap_or(Value::Null));
    json_dumps(&Value::Object(map), &JsonStyle::DEFAULT) + "\n"
}

impl TimedBackend {
    pub fn new(backend: Rc<dyn Inhibit>, path: Option<PathBuf>, clock: Option<Box<dyn Fn() -> f64>>) -> Self {
        let clock = clock.unwrap_or_else(|| Box::new(suite_core::timer::now));
        let (duration, until) = path.as_deref().map(|p| load_timer(p, clock())).unwrap_or((0, None));
        Self(Rc::new(Inner {
            backend, path, clock, duration: Cell::new(duration), until: Cell::new(until), timer: RefCell::new(None),
        }))
    }

    pub fn inner(&self) -> &Rc<dyn Inhibit> { &self.0.backend }
    pub fn duration(&self) -> u32 { self.0.duration.get() }
    pub fn until(&self) -> Option<f64> { self.0.until.get() }
    pub fn requested(&self) -> bool { self.0.backend.requested() }
    pub fn active(&self) -> bool { self.0.backend.active() }
    pub fn ready(&self) -> bool { self.0.backend.ready() }
    pub fn pending(&self) -> bool { self.0.backend.pending() }
    pub fn error(&self) -> String { self.0.backend.error() }
    pub fn has_timer(&self) -> bool { self.0.timer.borrow().is_some() }

    fn now(&self) -> f64 { (self.0.clock)() }

    fn save(&self) {
        let Some(path) = &self.0.path else { return };
        if let Some(parent) = path.parent() {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
        }
        // Write errors are ignored: the timer still runs for this session.
        let _ = suite_core::clock::atomic_write(path, timer_text(self.duration(), self.until()).as_bytes(), ".timer-");
    }

    pub fn start(&self) {
        self.0.backend.start();
        if self.until().is_some() {
            // Resume an unexpired timer from before the service restarted.
            self.0.backend.set_enabled(true);
            self.schedule();
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> bool {
        let duration = self.duration();
        self.0.until.set(if enabled && duration != 0 { Some(self.now() + duration as f64 * 60.0) } else { None });
        self.save();
        self.schedule();
        self.0.backend.set_enabled(enabled)
    }

    pub fn toggle(&self) -> bool { self.set_enabled(!self.0.backend.requested()) }

    pub fn set_duration(&self, minutes: u32) -> Result<bool, String> {
        if !DURATIONS.contains(&minutes) {
            return Err(UNSUPPORTED_DURATION.into());
        }
        self.0.duration.set(minutes);
        if self.0.backend.requested() {
            self.0.until.set(if minutes != 0 { Some(self.now() + minutes as f64 * 60.0) } else { None });
        }
        self.save();
        self.schedule();
        self.0.backend.on_changed();
        Ok(true)
    }

    /// Seconds left while requested with an end time.
    pub fn remaining(&self) -> Option<f64> {
        let until = self.until()?;
        if !self.0.backend.requested() {
            return None;
        }
        Some((until - self.now()).max(0.0))
    }

    fn cancel_timer(&self) {
        if let Some(id) = self.0.timer.take() {
            id.remove();
        }
    }

    fn schedule(&self) {
        self.cancel_timer();
        if let Some(until) = self.until() {
            // Re-check at least every minute; the wall clock also covers suspend.
            let left = (until - self.now()).trunc();
            let delay = (left + 1.0).clamp(1.0, 60.0) as u32;
            let weak: Weak<Inner> = Rc::downgrade(&self.0);
            let id = glib::timeout_add_seconds_local(delay, move || {
                if let Some(inner) = weak.upgrade() {
                    // This source is finishing: forget it without removing it.
                    inner.timer.replace(None);
                    TimedBackend(inner).check();
                }
                glib::ControlFlow::Break
            });
            self.0.timer.replace(Some(id));
        }
    }

    /// Run the expiry check now (tests; Python `_check()`).
    pub fn check_now(&self) {
        self.cancel_timer();
        self.check();
    }

    fn check(&self) {
        match self.until() {
            Some(until) if self.now() >= until => {
                self.set_enabled(false);
            }
            Some(_) if !self.0.backend.requested() => {
                self.0.until.set(None); // Turned off elsewhere or failed; forget the timer.
                self.save();
            }
            _ => self.schedule(),
        }
    }

    pub fn close(&self) {
        self.cancel_timer();
        self.0.backend.close();
    }
}
