//! Main-loop services behind a trait: `GLib.idle_add`, `GLib.timeout_add`,
//! `GLib.source_remove`, `GLib.get_monotonic_time`. `GlibEnv` attaches to the
//! *thread-default* context (so tests can run on a private context);
//! `FakeEnv` queues idles for the tests to drain explicitly.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub trait Env {
    fn idle(&self, callback: Box<dyn FnOnce()>);
    /// Repeating timer; the callback returns whether to continue.
    fn repeat(&self, milliseconds: u32, callback: Box<dyn FnMut() -> bool>) -> u64;
    fn cancel(&self, id: u64);
    /// Microseconds (`GLib.get_monotonic_time`).
    fn monotonic(&self) -> i64;
}

/// One idle callback on the thread-default main context.
pub fn idle_once(callback: impl FnOnce() + 'static) {
    let guard = glib::thread_guard::ThreadGuard::new(Some(callback));
    let mut guard = Some(guard);
    let source = glib::idle_source_new(None, glib::Priority::DEFAULT_IDLE, move || {
        if let Some(mut guard) = guard.take() {
            if let Some(callback) = guard.get_mut().take() {
                callback();
            }
        }
        glib::ControlFlow::Break
    });
    source.attach(Some(&glib::MainContext::ref_thread_default()));
}

#[derive(Default)]
pub struct GlibEnv {
    timers: RefCell<HashMap<u64, tessel_core::timer::Timeout>>,
    next: Cell<u64>,
}

impl GlibEnv {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }
}

impl Env for GlibEnv {
    fn idle(&self, callback: Box<dyn FnOnce()>) {
        idle_once(callback);
    }

    fn repeat(&self, milliseconds: u32, mut callback: Box<dyn FnMut() -> bool>) -> u64 {
        let source = tessel_core::timer::repeat(milliseconds, move || callback());
        self.next.set(self.next.get() + 1);
        self.timers.borrow_mut().insert(self.next.get(), source);
        self.next.get()
    }

    fn cancel(&self, id: u64) {
        if let Some(source) = self.timers.borrow_mut().remove(&id) {
            if source.is_pending() {
                source.cancel();
            }
        }
    }

    fn monotonic(&self) -> i64 {
        glib::monotonic_time()
    }
}

/// Explicitly drained idles; timers are recorded, never fired by themselves.
#[derive(Default)]
pub struct FakeEnv {
    pub idles: RefCell<Vec<Box<dyn FnOnce()>>>,
    pub timers: RefCell<Vec<(u64, u32)>>,
    pub cancelled: RefCell<Vec<u64>>,
    pub now: Cell<i64>,
    next: Cell<u64>,
}

impl FakeEnv {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// `drain_events()`: run idles (including ones queued meanwhile).
    pub fn drain(&self) {
        for _ in 0..1000 {
            let queued: Vec<Box<dyn FnOnce()>> = self.idles.borrow_mut().drain(..).collect();
            if queued.is_empty() {
                return;
            }
            for callback in queued {
                callback();
            }
        }
        panic!("idle callbacks kept queueing");
    }

    pub fn pending_idles(&self) -> usize {
        self.idles.borrow().len()
    }
}

impl Env for FakeEnv {
    fn idle(&self, callback: Box<dyn FnOnce()>) {
        self.idles.borrow_mut().push(callback);
    }

    fn repeat(&self, milliseconds: u32, _callback: Box<dyn FnMut() -> bool>) -> u64 {
        self.next.set(self.next.get() + 1);
        self.timers.borrow_mut().push((self.next.get(), milliseconds));
        self.next.get()
    }

    fn cancel(&self, id: u64) {
        self.cancelled.borrow_mut().push(id);
    }

    fn monotonic(&self) -> i64 {
        self.now.get()
    }
}
