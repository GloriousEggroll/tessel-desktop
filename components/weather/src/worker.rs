//! weather_ui.Worker: run a blocking request on a GLib pool thread and drop
//! completions that are stale (a newer `run`/`cancel`) or arrive after `close`.
//! Closing never waits for the thread.

use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct Worker {
    generation: Rc<Cell<u64>>,
    closed: Rc<Cell<bool>>,
}

impl Worker {
    pub fn new() -> Self { Self::default() }

    pub fn cancel(&self) { self.generation.set(self.generation.get() + 1); }

    pub fn close(&self) {
        self.cancel();
        self.closed.set(true);
    }

    pub fn closed(&self) -> bool { self.closed.get() }

    /// `callback(Some(value), None)` or `callback(None, Some(error))` on the main loop.
    pub fn run<T: Send + 'static>(&self, function: impl FnOnce() -> Result<T, String> + Send + 'static,
                                  callback: impl FnOnce(Option<T>, Option<String>) + 'static) {
        self.cancel();
        let generation = self.generation.get();
        let (current, closed) = (self.generation.clone(), self.closed.clone());
        let handle = gio::spawn_blocking(function);
        glib::spawn_future_local(async move {
            let result = handle.await;
            if closed.get() || current.get() != generation {
                return;
            }
            match result {
                Ok(Ok(value)) => callback(Some(value), None),
                Ok(Err(error)) => callback(None, Some(error)),
                Err(_) => callback(None, Some("The request failed.".into())),
            }
        });
    }
}
