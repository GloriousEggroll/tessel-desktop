//! A tiny `ThreadPoolExecutor(max_workers=n)` with `shutdown(cancel_futures=True)`.
//! Jobs submitted with a generation token are skipped (never run) once the
//! token moved on, like Python's `future.cancel()` on queued futures.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

type Job = Box<dyn FnOnce() + Send>;

struct Shared {
    queue: Mutex<VecDeque<(Option<(Arc<AtomicU64>, u64)>, Job)>>,
    ready: Condvar,
    shutdown: AtomicBool,
}

pub struct Pool {
    shared: Arc<Shared>,
    workers: usize,
    started: Mutex<usize>,
    name: &'static str,
}

impl Pool {
    pub fn new(workers: usize, name: &'static str) -> Self {
        Self {
            shared: Arc::new(Shared { queue: Mutex::new(VecDeque::new()), ready: Condvar::new(), shutdown: AtomicBool::new(false) }),
            workers,
            started: Mutex::new(0),
            name,
        }
    }

    /// Queue `job`; with `token = Some((counter, value))` it only runs while
    /// `counter == value`.
    pub fn submit(&self, token: Option<(Arc<AtomicU64>, u64)>, job: impl FnOnce() + Send + 'static) {
        if self.shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        self.shared.queue.lock().unwrap().push_back((token, Box::new(job)));
        let mut started = self.started.lock().unwrap();
        if *started < self.workers {
            *started += 1;
            let shared = self.shared.clone();
            let _ = std::thread::Builder::new().name(self.name.into()).spawn(move || worker(shared));
        }
        self.shared.ready.notify_one();
    }

    /// Drop queued jobs; running ones finish; workers exit.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.queue.lock().unwrap().clear();
        self.shared.ready.notify_all();
    }

    pub fn is_shutdown(&self) -> bool {
        self.shared.shutdown.load(Ordering::SeqCst)
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = shared.ready.wait(queue).unwrap();
            }
        };
        let (token, job) = job;
        if let Some((counter, value)) = token {
            if counter.load(Ordering::SeqCst) != value {
                continue;
            }
        }
        job();
    }
}
