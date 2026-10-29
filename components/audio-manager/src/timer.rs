//! `GLib.timeout_add` for non-`Send` closures on the *thread-default* main
//! context (so unit tests can run each case on its own private context).

use glib::ffi;
use glib::translate::{from_glib_full, ToGlibPtr};
use std::time::Instant;

/// A pending timeout; `cancel()` is `GLib.source_remove`.
#[derive(Debug)]
pub struct Timeout(glib::Source);

impl Timeout {
    pub fn cancel(self) {
        self.0.destroy();
    }
    pub fn is_pending(&self) -> bool {
        !self.0.is_destroyed()
    }
}

type Callback = Box<dyn FnMut() -> bool>;

unsafe extern "C" fn trampoline(data: ffi::gpointer) -> ffi::gboolean {
    let callback = &mut *(data as *mut Callback);
    callback() as ffi::gboolean
}

unsafe extern "C" fn destroy(data: ffi::gpointer) {
    drop(Box::from_raw(data as *mut Callback));
}

fn attach(milliseconds: u32, callback: Callback) -> Timeout {
    unsafe {
        let source: glib::Source = from_glib_full(ffi::g_timeout_source_new(milliseconds));
        let data = Box::into_raw(Box::new(callback));
        ffi::g_source_set_callback(source.to_glib_none().0, Some(trampoline), data as ffi::gpointer, Some(destroy));
        let context = glib::MainContext::ref_thread_default();
        ffi::g_source_attach(source.to_glib_none().0, context.to_glib_none().0);
        Timeout(source)
    }
}

/// Run `callback` once after `milliseconds`.
pub fn once(milliseconds: u32, callback: impl FnOnce() + 'static) -> Timeout {
    let mut callback = Some(callback);
    attach(milliseconds, Box::new(move || {
        if let Some(callback) = callback.take() {
            callback();
        }
        false
    }))
}

/// Run `callback` every `milliseconds` while it returns true.
pub fn repeat(milliseconds: u32, callback: impl FnMut() -> bool + 'static) -> Timeout {
    attach(milliseconds, Box::new(callback))
}

/// `time.monotonic()` in seconds.
pub fn monotonic() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    // Offset so deadlines of 0 (the Python tests' "expired") are in the past.
    START.get_or_init(Instant::now).elapsed().as_secs_f64() + 1000.0
}

/// Iterate `context` for `duration` (tests and headless checks).
pub fn settle(context: &glib::MainContext, duration: std::time::Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Iterate `context` until `predicate` or `timeout`; returns the predicate.
pub fn wait_for(context: &glib::MainContext, timeout: std::time::Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

extern "C" {
    fn g_unix_signal_add_full(priority: i32, signum: i32, handler: ffi::GSourceFunc, data: ffi::gpointer, notify: ffi::GDestroyNotify) -> u32;
}

/// `GLib.unix_signal_add(PRIORITY_DEFAULT, signum, callback)` on the default context.
pub fn unix_signal(signum: i32, callback: impl FnMut() -> bool + 'static) {
    let data = Box::into_raw(Box::new(Box::new(callback) as Callback));
    unsafe { g_unix_signal_add_full(ffi::G_PRIORITY_DEFAULT, signum, Some(trampoline), data as ffi::gpointer, Some(destroy)) };
}
