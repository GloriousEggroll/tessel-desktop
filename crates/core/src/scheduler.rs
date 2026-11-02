//! Injectable `GLib.timeout_add` / `GLib.source_remove`, so policy logic is
//! testable without a running main loop (Python tests patch GLib.timeout_add
//! or inspect `_sources`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// One-shot timers; ids are never reused.
pub trait Scheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64;
    fn cancel(&self, id: u64);
}

/// The real GLib main loop (`GLib.timeout_add` on the thread-default context).
#[derive(Default)]
pub struct GlibScheduler {
    next: Cell<u64>,
    sources: Rc<RefCell<HashMap<u64, crate::timer::Timeout>>>,
}

impl Scheduler for GlibScheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        let sources = Rc::downgrade(&self.sources);
        let timeout = crate::timer::once(milliseconds, move || {
            if let Some(sources) = sources.upgrade() {
                sources.borrow_mut().remove(&id);
            }
            callback();
        });
        self.sources.borrow_mut().insert(id, timeout);
        id
    }

    fn cancel(&self, id: u64) {
        let timeout = self.sources.borrow_mut().remove(&id);
        if let Some(timeout) = timeout {
            timeout.cancel();
        }
    }
}

/// A manual timer queue for tests.
#[derive(Default)]
pub struct FakeScheduler {
    next: Cell<u64>,
    pub pending: RefCell<Vec<(u64, u32, Option<Box<dyn FnOnce()>>)>>,
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

    /// How many timers were ever created.
    pub fn created(&self) -> u64 {
        self.next.get()
    }

    pub fn ids(&self) -> Vec<u64> {
        self.pending.borrow().iter().map(|entry| entry.0).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_scheduler_queues_fires_and_cancels() {
        let fake = FakeScheduler::default();
        let fired = Rc::new(Cell::new(0));
        let (a, b) = (fired.clone(), fired.clone());
        let first = fake.timeout(100, Box::new(move || a.set(a.get() + 1)));
        let second = fake.timeout(5000, Box::new(move || b.set(b.get() + 10)));
        assert_eq!(fake.delays(), [100, 5000]);
        fake.cancel(second);
        assert_eq!(*fake.cancelled.borrow(), [second]);
        assert!(fake.fire(first));
        assert!(!fake.fire(first));
        assert_eq!((fired.get(), fake.created(), fake.ids()), (1, 2, vec![]));
    }

    #[test]
    fn glib_scheduler_runs_and_cancels_on_thread_default_context() {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let scheduler = GlibScheduler::default();
            let fired = Rc::new(Cell::new(0));
            let (a, b) = (fired.clone(), fired.clone());
            scheduler.timeout(1, Box::new(move || a.set(a.get() + 1)));
            let cancelled = scheduler.timeout(1, Box::new(move || b.set(b.get() + 10)));
            scheduler.cancel(cancelled);
            scheduler.cancel(cancelled);
            assert!(crate::timer::wait_for(&context, std::time::Duration::from_secs(2), || fired.get() == 1));
            crate::timer::settle(&context, std::time::Duration::from_millis(10));
            assert_eq!(fired.get(), 1);
        }).unwrap();
    }
}
