//! A single ordered worker thread (`ThreadPoolExecutor(max_workers=1)`), started lazily.

use futures_channel::oneshot;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

type Job = Box<dyn FnOnce() + Send>;

pub struct Worker {
    name: &'static str,
    sender: std::cell::RefCell<Option<mpsc::Sender<Job>>>,
    thread: std::cell::RefCell<Option<std::thread::JoinHandle<()>>>,
    cancelled: Arc<AtomicBool>,
}

impl Worker {
    pub fn new(name: &'static str) -> Self {
        Self { name, sender: Default::default(), thread: Default::default(), cancelled: Arc::new(AtomicBool::new(false)) }
    }

    fn sender(&self) -> Option<mpsc::Sender<Job>> {
        if self.cancelled.load(Ordering::SeqCst) {
            return None;
        }
        let mut slot = self.sender.borrow_mut();
        if slot.is_none() {
            let (sender, receiver) = mpsc::channel::<Job>();
            let cancelled = self.cancelled.clone();
            let handle = std::thread::Builder::new().name(self.name.into()).spawn(move || {
                for job in receiver {
                    if !cancelled.load(Ordering::SeqCst) {
                        job();
                    }
                }
            }).ok()?;
            self.thread.replace(Some(handle));
            *slot = Some(sender);
        }
        slot.clone()
    }

    /// Queue a job; the result arrives on the receiver (dropped when cancelled).
    pub fn run<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> oneshot::Receiver<T> {
        let (tx, rx) = oneshot::channel();
        if let Some(sender) = self.sender() {
            let _ = sender.send(Box::new(move || {
                let _ = tx.send(job());
            }));
        }
        rx
    }

    pub fn submit(&self, job: impl FnOnce() + Send + 'static) {
        if let Some(sender) = self.sender() {
            let _ = sender.send(Box::new(job));
        }
    }

    /// Block until every job queued so far has run (`submit(lambda: None).result()`).
    pub fn wait(&self) {
        if self.sender.borrow().is_none() {
            return;
        }
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        self.submit(move || {
            let _ = tx.send(());
        });
        let _ = rx.recv();
    }

    /// `shutdown(wait=False, cancel_futures=True)`: the running job finishes, queued ones are dropped.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.sender.replace(None);
        self.thread.replace(None);
    }

    /// `shutdown(wait=True)`.
    pub fn join(&self) {
        self.sender.replace(None);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_wait_and_cancel() {
        let worker = Worker::new("test-worker");
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        for index in 0..5 {
            let log = log.clone();
            worker.submit(move || log.lock().unwrap().push(index));
        }
        worker.wait();
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2, 3, 4]);
        let receiver = worker.run(|| 42);
        worker.wait();
        assert_eq!(futures_executor_block(receiver), Some(42));
        worker.cancel();
        let receiver = worker.run(|| 1);
        assert_eq!(futures_executor_block(receiver), None);
        worker.join();
    }

    fn futures_executor_block<T>(mut receiver: oneshot::Receiver<T>) -> Option<T> {
        receiver.try_recv().ok().flatten()
    }
}
