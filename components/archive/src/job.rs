//! Background jobs: the engine blocks, so every operation runs on its own
//! thread and talks to the GTK main loop over a channel (progress, conflict
//! questions, the result). The GTK thread never waits for a job.

use crate::archive::{Ctx, Result};
use crate::ops::{Conflict, Resolution};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

enum Message<T> {
    Progress(f64, String),
    Conflict(Conflict, std::sync::mpsc::Sender<Resolution>),
    Done(Result<T>),
}

/// How a job shows itself.
pub struct JobUi {
    pub progress: Rc<dyn Fn(f64, &str)>,
    /// Answer a conflict (asynchronously, e.g. with a dialog).
    pub conflict: Rc<dyn Fn(Conflict, Box<dyn FnOnce(Resolution)>)>,
}

impl JobUi {
    pub fn silent() -> JobUi {
        JobUi { progress: Rc::new(|_, _| {}), conflict: Rc::new(|_, reply| reply(Resolution::Skip)) }
    }
}

#[derive(Clone)]
pub struct JobHandle {
    cancel: Arc<AtomicBool>,
    running: Rc<RefCell<bool>>,
}

impl JobHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn running(&self) -> bool {
        *self.running.borrow()
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Run `work(ctx, resolve)` on a thread; `done` gets its result on the GTK thread.
pub fn spawn<T: Send + 'static>(password: Option<String>, ui: JobUi,
                                work: impl FnOnce(&Ctx, &mut dyn FnMut(&Conflict) -> Resolution) -> Result<T> + Send + 'static,
                                done: impl FnOnce(Result<T>) + 'static) -> JobHandle {
    let (sender, receiver) = async_channel::unbounded::<Message<T>>();
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = JobHandle { cancel: cancel.clone(), running: Rc::new(RefCell::new(true)) };
    let progress_sender = sender.clone();
    let last = std::sync::Mutex::new(Instant::now() - Duration::from_secs(1));
    let ctx = Ctx {
        password,
        cancel,
        progress: Arc::new(move |fraction, item| {
            // At most ~30 updates a second reach the GTK thread.
            let mut last = last.lock().unwrap();
            if last.elapsed() >= Duration::from_millis(33) || fraction >= 1.0 {
                *last = Instant::now();
                let _ = progress_sender.send_blocking(Message::Progress(fraction, item.to_string()));
            }
        }),
    };
    std::thread::Builder::new().name("tessel-archive-job".into()).spawn(move || {
        let conflict_sender = sender.clone();
        let mut resolve = move |conflict: &Conflict| {
            let (reply, answer) = std::sync::mpsc::channel();
            if conflict_sender.send_blocking(Message::Conflict(conflict.clone(), reply)).is_err() {
                return Resolution::Cancel;
            }
            answer.recv().unwrap_or(Resolution::Cancel)
        };
        let result = work(&ctx, &mut resolve);
        let _ = sender.send_blocking(Message::Done(result));
    }).expect("spawn a job thread");
    let running = handle.running.clone();
    glib::spawn_future_local(async move {
        let mut done = Some(done);
        while let Ok(message) = receiver.recv().await {
            match message {
                Message::Progress(fraction, item) => (ui.progress)(fraction, &item),
                Message::Conflict(conflict, reply) => (ui.conflict)(conflict, Box::new(move |r| {
                    let _ = reply.send(r);
                })),
                Message::Done(result) => {
                    *running.borrow_mut() = false;
                    if let Some(done) = done.take() {
                        done(result);
                    }
                    break;
                }
            }
        }
    });
    handle
}
