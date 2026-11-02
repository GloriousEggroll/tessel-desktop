//! `ThreadPoolExecutor(max_workers=1, thread_name_prefix="language-settings")`
//! with results delivered on the GLib main loop (`GLib.idle_add`).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

type Job = Box<dyn FnOnce() + Send>;
type Done = Box<dyn FnOnce(Box<dyn Any + Send>)>;

thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(1) };
    static DONE: RefCell<HashMap<u64, Done>> = RefCell::new(HashMap::new());
    static LISTENERS: RefCell<HashMap<u64, std::rc::Rc<dyn Fn(String)>>> = RefCell::new(HashMap::new());
}

fn next_id() -> u64 {
    NEXT.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    })
}

/// A Send handle that runs a main-thread listener with a message
/// (`GLib.idle_add(update)` from the worker).
#[derive(Clone, Copy, Debug)]
pub struct Poster(u64);

impl Poster {
    /// Register on the main thread; [`Poster::remove`] when the owner closes.
    pub fn new(listener: impl Fn(String) + 'static) -> Self {
        let id = next_id();
        LISTENERS.with(|map| map.borrow_mut().insert(id, std::rc::Rc::new(listener)));
        Poster(id)
    }

    pub fn post(&self, message: &str) {
        let (id, message) = (self.0, message.to_owned());
        glib::MainContext::default().invoke(move || {
            let listener = LISTENERS.with(|map| map.borrow().get(&id).cloned());
            if let Some(listener) = listener {
                listener(message);
            }
        });
    }

    pub fn remove(&self) {
        LISTENERS.with(|map| map.borrow_mut().remove(&self.0));
    }
}

pub struct Worker {
    sender: mpsc::Sender<Job>,
    cancelled: Arc<AtomicBool>,
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

impl Worker {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        std::thread::Builder::new().name("language-settings_0".into()).spawn(move || {
            while let Ok(job) = receiver.recv() {
                if !stop.load(Ordering::SeqCst) {
                    job();
                }
            }
        }).expect("worker thread");
        Worker { sender, cancelled }
    }

    /// Run `job` on the worker; `done(result)` runs later on the main loop
    /// (a panic becomes `Err(text)`, Python's `str(failure)`).
    pub fn run<R: Send + 'static>(&self, job: impl FnOnce() -> Result<R, String> + Send + 'static,
                                  done: impl FnOnce(Result<R, String>) + 'static) {
        let id = next_id();
        DONE.with(|map| map.borrow_mut().insert(id, Box::new(move |value: Box<dyn Any + Send>| {
            if let Ok(value) = value.downcast::<Result<R, String>>() {
                done(*value)
            }
        })));
        let cancelled = self.cancelled.clone();
        let job: Job = Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or_else(|panic| {
                Err(panic.downcast_ref::<String>().cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default())
            });
            if cancelled.load(Ordering::SeqCst) {
                return;
            }
            let value: Box<dyn Any + Send> = Box::new(result);
            glib::MainContext::default().invoke(move || {
                if let Some(done) = DONE.with(|map| map.borrow_mut().remove(&id)) {
                    done(value);
                }
            });
        });
        let _ = self.sender.send(job);
    }

    /// `shutdown(wait=False, cancel_futures=True)`.
    pub fn shutdown(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}
