//! `GLib.timeout_add` for non-`Send` closures on the *thread-default* main
//! context (the global default unless a context was pushed), so unit tests can
//! run each case on its own private context; plus main-loop test helpers.

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

/// `time.time()`: seconds since the Unix epoch.
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn timers_run_on_the_thread_default_context() {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let (fired, ticks) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(0)));
            let flag = fired.clone();
            let pending = once(1, move || flag.set(true));
            let counter = ticks.clone();
            let _repeat = repeat(1, move || { counter.set(counter.get() + 1); counter.get() < 3 });
            let cancelled = once(1, || panic!("cancelled timer ran"));
            cancelled.cancel();
            assert!(pending.is_pending());
            assert!(wait_for(&context, Duration::from_secs(2), || fired.get() && ticks.get() == 3));
            assert!(!pending.is_pending());
            settle(&context, Duration::from_millis(10));
            assert_eq!(ticks.get(), 3);
        }).unwrap();
        assert!(now() > 1.6e9);
    }
}
