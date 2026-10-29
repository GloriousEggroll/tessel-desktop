//! Timer scheduling, injectable so backend logic is testable without a running
//! main loop (Python tests inspect `backend._sources`). Mirrors power-manager.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub trait Scheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64;
    fn cancel(&self, id: u64);
}

/// The real GLib main loop (`GLib.timeout_add`).
#[derive(Default)]
pub struct GlibScheduler {
    next: Cell<u64>,
    sources: Rc<RefCell<HashMap<u64, glib::SourceId>>>,
}

impl Scheduler for GlibScheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        let sources = self.sources.clone();
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(milliseconds as u64), move || {
            sources.borrow_mut().remove(&id);
            callback();
        });
        self.sources.borrow_mut().insert(id, source);
        id
    }

    fn cancel(&self, id: u64) {
        let source = self.sources.borrow_mut().remove(&id);
        if let Some(source) = source {
            source.remove();
        }
    }
}

type Timer = (u64, u32, Option<Box<dyn FnOnce()>>);

/// A manual timer queue for tests.
#[derive(Default)]
pub struct FakeScheduler {
    next: Cell<u64>,
    pub pending: RefCell<Vec<Timer>>,
    pub cancelled: RefCell<Vec<u64>>,
}

impl FakeScheduler {
    /// Run the timer `id` (if still pending).
    pub fn fire(&self, id: u64) -> bool {
        let callback = {
            let mut pending = self.pending.borrow_mut();
            let Some(index) = pending.iter().position(|entry| entry.0 == id) else { return false };
            pending.remove(index).2
        };
        if let Some(callback) = callback {
            callback();
        }
        true
    }

    pub fn delays(&self) -> Vec<u32> {
        self.pending.borrow().iter().map(|entry| entry.1).collect()
    }
}

impl Scheduler for FakeScheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        self.pending.borrow_mut().push((id, milliseconds, Some(callback)));
        id
    }

    fn cancel(&self, id: u64) {
        self.cancelled.borrow_mut().push(id);
        self.pending.borrow_mut().retain(|entry| entry.0 != id);
    }
}
