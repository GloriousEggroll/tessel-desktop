//! status_backend.ProcessJob: a bounded asynchronous stdout reader with a
//! startup (stream) / one-shot deadline, behind the `StatusJob` trait so the
//! backend can be driven by fake jobs in tests.
//!
//! - lines are split on `\n`, decoded as UTF-8 and handed to `on_line`; an
//!   `Err` from `on_line` (bad JSON/indicator) finishes the job with that text;
//! - any line (or unterminated buffer) longer than 65536 bytes →
//!   `"status command returned an oversized line"`;
//! - the final unterminated line is delivered before completion;
//! - streams cancel the deadline after their first line; otherwise
//!   `"status command timed out"` force-exits the child;
//! - completion needs both EOF and exit: `None` on success else
//!   `"status command failed"`. `on_done` is called at most once.

use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::rc::{Rc, Weak};

/// `on_line(text)`; `Err(message)` ends the job (Python's ValueError).
pub type LineHandler = Rc<dyn Fn(&str) -> Result<(), String>>;
/// `on_done(error)`.
pub type DoneHandler = Rc<dyn Fn(Option<String>)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobOptions {
    pub stream: bool,
    /// Milliseconds.
    pub timeout: u32,
}

impl Default for JobOptions {
    fn default() -> Self { Self { stream: false, timeout: 10000 } }
}

pub trait StatusJob {
    fn start(&self);
    fn close(&self);
}

/// `job_factory(command, on_line, on_done, stream=, timeout=)`.
pub type JobFactory = Rc<dyn Fn(Vec<String>, LineHandler, DoneHandler, JobOptions) -> Rc<dyn StatusJob>>;

pub const MAX_LINE: usize = 65536;
pub const OVERSIZED: &str = "status command returned an oversized line";

/// Python `str(UnicodeDecodeError)` for `bytes.decode("utf-8")`.
pub fn utf8_error_text(bytes: &[u8], error: &std::str::Utf8Error) -> String {
    let start = error.valid_up_to();
    let byte = bytes[start];
    match error.error_len() {
        None => {
            let end = bytes.len() - 1;
            if end == start {
                format!("'utf-8' codec can't decode byte {byte:#04x} in position {start}: unexpected end of data")
            } else {
                format!("'utf-8' codec can't decode bytes in position {start}-{end}: unexpected end of data")
            }
        }
        Some(_) => {
            let reason = if matches!(byte, 0x80..=0xC1 | 0xF5..=0xFF) { "invalid start byte" } else { "invalid continuation byte" };
            format!("'utf-8' codec can't decode byte {byte:#04x} in position {start}: {reason}")
        }
    }
}

struct Inner {
    command: Vec<String>,
    on_line: LineHandler,
    on_done: DoneHandler,
    options: JobOptions,
    process: RefCell<Option<gio::Subprocess>>,
    cancel: gio::Cancellable,
    buffer: RefCell<Vec<u8>>,
    timer: RefCell<Option<tessel_core::timer::Timeout>>,
    closed: Cell<bool>,
    eof: Cell<bool>,
    exited: Cell<bool>,
    success: Cell<bool>,
}

#[derive(Clone)]
pub struct ProcessJob(Rc<Inner>);

impl ProcessJob {
    pub fn new(command: Vec<String>, on_line: LineHandler, on_done: DoneHandler, options: JobOptions) -> Self {
        Self(Rc::new(Inner {
            command, on_line, on_done, options, process: RefCell::new(None), cancel: gio::Cancellable::new(),
            buffer: RefCell::new(Vec::new()), timer: RefCell::new(None), closed: Cell::new(false),
            eof: Cell::new(false), exited: Cell::new(false), success: Cell::new(false),
        }))
    }

    /// The default `JobFactory`.
    pub fn factory() -> JobFactory {
        Rc::new(|command, on_line, on_done, options| Rc::new(ProcessJob::new(command, on_line, on_done, options)) as Rc<dyn StatusJob>)
    }

    pub fn closed(&self) -> bool { self.0.closed.get() }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    fn read(&self) {
        let Some(pipe) = self.0.process.borrow().as_ref().and_then(|p| p.stdout_pipe()) else { return };
        let weak = self.weak();
        pipe.read_bytes_async(4096, glib::Priority::DEFAULT, Some(&self.0.cancel), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            ProcessJob(inner).read_done(result);
        });
    }

    fn read_done(&self, result: Result<glib::Bytes, glib::Error>) {
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                if !self.0.closed.get() {
                    self.finish(Some(tessel_core::pyerr::python_error_text(&error)));
                }
                return;
            }
        };
        if self.0.closed.get() {
            return;
        }
        self.0.buffer.borrow_mut().extend_from_slice(&data);
        loop {
            let line = {
                let mut buffer = self.0.buffer.borrow_mut();
                let Some(index) = buffer.iter().position(|b| *b == b'\n') else { break };
                let rest = buffer.split_off(index + 1);
                let mut line = std::mem::replace(&mut *buffer, rest);
                line.pop();
                line
            };
            if !self.line(&line) {
                return;
            }
        }
        if self.0.buffer.borrow().len() > MAX_LINE {
            self.finish(Some(OVERSIZED.into()));
            return;
        }
        if data.is_empty() {
            let rest = std::mem::take(&mut *self.0.buffer.borrow_mut());
            if !rest.is_empty() && !self.line(&rest) {
                return;
            }
            self.0.eof.set(true);
            self.complete();
        } else {
            self.read();
        }
    }

    fn line(&self, line: &[u8]) -> bool {
        if line.len() > MAX_LINE {
            self.finish(Some(OVERSIZED.into()));
            return false;
        }
        let result = match std::str::from_utf8(line) {
            Ok(text) => (self.0.on_line)(text),
            Err(error) => Err(utf8_error_text(line, &error)),
        };
        if let Err(error) = result {
            self.finish(Some(error));
            return false;
        }
        if self.0.options.stream {
            if let Some(timer) = self.0.timer.take() {
                timer.cancel();
            }
        }
        true
    }

    fn complete(&self) {
        if self.0.eof.get() && self.0.exited.get() && !self.0.closed.get() {
            self.finish(if self.0.success.get() { None } else { Some("status command failed".into()) });
        }
    }

    fn finish(&self, error: Option<String>) {
        if self.0.closed.get() {
            return;
        }
        self.close();
        (self.0.on_done)(error);
    }
}

impl StatusJob for ProcessJob {
    fn start(&self) {
        let argv: Vec<&OsStr> = self.0.command.iter().map(OsStr::new).collect();
        let process = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) {
            Ok(process) => process,
            Err(error) => {
                self.finish(Some(tessel_core::pyerr::python_error_text(&error)));
                return;
            }
        };
        self.0.process.replace(Some(process.clone()));
        let weak = self.weak();
        self.0.timer.replace(Some(tessel_core::timer::once(self.0.options.timeout, move || {
            let Some(inner) = weak.upgrade() else { return };
            inner.timer.take();
            ProcessJob(inner).finish(Some("status command timed out".into()));
        })));
        let weak = self.weak();
        process.wait_async(None::<&gio::Cancellable>, move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let job = ProcessJob(inner);
            let success = result.is_ok() && job.0.process.borrow().as_ref().is_some_and(|p| p.is_successful());
            job.0.success.set(success);
            job.0.exited.set(true);
            job.complete();
        });
        self.read();
    }

    fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.cancel.cancel();
        if let Some(timer) = self.0.timer.take() {
            timer.cancel();
        }
        if !self.0.exited.get() {
            if let Some(process) = self.0.process.borrow().as_ref() {
                process.force_exit();
            }
        }
    }
}
