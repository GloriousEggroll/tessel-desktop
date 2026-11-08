//! One persistent native input connection; no per-key process launching (osk_backend.py).
//!
//! The helper (`tessel-keyboard-input`, native/input.c) is the only code that binds the
//! virtual-keyboard and input-method protocols; it connects to `$WAYLAND_DISPLAY`.
use crate::config::valid_layout_name;
use gio::prelude::*;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

pub const QUEUE_LIMIT: usize = 128;
pub const LINE_LIMIT: usize = 32768;
pub const CLOSED_MESSAGE: &str = "The keyboard input connection closed. Hide and reopen the keyboard to reconnect.";
pub const UNAVAILABLE_MESSAGE: &str = "The keyboard input connection is unavailable.";

/// Where the helper lives: `$TESSEL_KEYBOARD_INPUT` (tests), else the installed
/// `tessel-keyboard-input` program.
pub fn helper_path() -> PathBuf {
    if let Some(path) = std::env::var_os("TESSEL_KEYBOARD_INPUT").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    tessel_core::paths::program("tessel-keyboard-input")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Key(i64, i64),
    Text(i64, i64),
    Layout(String),
}

impl Command {
    /// `kind` is `key` or `text` (the keyboard widget's sends).
    pub fn from_kind(kind: &str, value: i64, flags: i64) -> Result<Self, String> {
        match kind {
            "key" => Ok(Self::Key(value, flags)),
            "text" => Ok(Self::Text(value, flags)),
            _ => Err("Invalid key".into()),
        }
    }

    /// The validated stdin line (`send()` raises `ValueError` otherwise).
    pub fn line(&self) -> Result<String, String> {
        match self {
            Self::Layout(name) => {
                if !valid_layout_name(name) {
                    return Err("Invalid layout".into());
                }
                Ok(format!("layout {name}\n"))
            }
            Self::Key(value, flags) | Self::Text(value, flags) => {
                if !(0..=31).contains(flags) {
                    return Err("Invalid key".into());
                }
                match self {
                    Self::Key(..) if (0..=247).contains(value) => Ok(format!("key {value} {flags}\n")),
                    Self::Text(..) if *value > 0 && *value <= 0x10ffff => Ok(format!("text {value}\n")),
                    _ => Err("Invalid key".into()),
                }
            }
        }
    }
}

/// Parse one stdout line: at most 32768 bytes, a JSON object.
pub fn parse_event(data: &[u8]) -> Result<Map<String, Value>, String> {
    if data.len() > LINE_LIMIT {
        return Err("Input response is too large.".into());
    }
    match serde_json::from_slice::<Value>(data).map_err(|e| e.to_string())? {
        Value::Object(map) => Ok(map),
        _ => Err("Invalid input response.".into()),
    }
}

type Changed = Rc<dyn Fn(Map<String, Value>)>;
type Failed = Rc<dyn Fn(String)>;

struct Inner {
    process: gio::Subprocess,
    stdin: gio::OutputStream,
    reader: gio::DataInputStream,
    cancel: gio::Cancellable,
    changed: Changed,
    failed: Failed,
    closed: bool,
    exited: bool,
    queue: VecDeque<String>,
    writing: bool,
}

#[derive(Clone)]
pub struct InputBridge(Rc<RefCell<Inner>>);

impl InputBridge {
    pub fn new(layout: &str, changed: impl Fn(Map<String, Value>) + 'static, failed: impl Fn(String) + 'static,
               executable: Option<PathBuf>) -> Result<Self, String> {
        let executable = executable.unwrap_or_else(helper_path);
        let argv = [executable.as_os_str(), std::ffi::OsStr::new(layout)];
        let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE
            | gio::SubprocessFlags::STDERR_SILENCE).map_err(|e| e.message().to_string())?;
        let reader = gio::DataInputStream::new(&process.stdout_pipe().expect("stdout pipe"));
        let stdin = process.stdin_pipe().expect("stdin pipe");
        let bridge = Self(Rc::new(RefCell::new(Inner {
            process: process.clone(), stdin, reader, cancel: gio::Cancellable::new(), changed: Rc::new(changed),
            failed: Rc::new(failed), closed: false, exited: false, queue: VecDeque::new(), writing: false })));
        bridge.read();
        let weak = Rc::downgrade(&bridge.0);
        process.wait_async(None::<&gio::Cancellable>, move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let (closed, failed) = {
                let mut inner = inner.borrow_mut();
                inner.exited = true;
                (inner.closed, inner.failed.clone())
            };
            if !closed {
                failed(CLOSED_MESSAGE.into());
            }
        });
        Ok(bridge)
    }

    pub fn process(&self) -> gio::Subprocess { self.0.borrow().process.clone() }
    pub fn is_closed(&self) -> bool { self.0.borrow().closed }
    /// Same helper connection (Python `is`).
    pub fn same(&self, other: &Self) -> bool { Rc::ptr_eq(&self.0, &other.0) }

    fn read(&self) {
        let inner = self.0.borrow();
        if inner.closed {
            return;
        }
        let this = self.clone();
        inner.reader.read_line_async(glib::Priority::DEFAULT, Some(&inner.cancel), move |result| {
            if this.0.borrow().closed {
                return;
            }
            let event = match result {
                Ok(Some(data)) => parse_event(&data),
                Ok(None) => return,
                Err(error) => Err(error.message().to_string()),
            };
            match event {
                Ok(event) => {
                    let changed = this.0.borrow().changed.clone();
                    changed(event);
                }
                Err(error) => {
                    let failed = this.0.borrow().failed.clone();
                    if !this.0.borrow().closed {
                        failed(error);
                    }
                    return;
                }
            }
            this.read();
        });
    }

    /// Queue one validated command (silently dropped past 128 queued lines).
    pub fn send(&self, command: &Command) -> Result<(), String> {
        if self.0.borrow().closed {
            return Ok(());
        }
        let line = command.line()?;
        {
            let mut inner = self.0.borrow_mut();
            if inner.queue.len() >= QUEUE_LIMIT {
                return Ok(());
            }
            inner.queue.push_back(line);
        }
        self.write();
        Ok(())
    }

    fn write(&self) {
        let (stdin, line, cancel) = {
            let mut inner = self.0.borrow_mut();
            if inner.closed || inner.writing || inner.queue.is_empty() {
                return;
            }
            inner.writing = true;
            (inner.stdin.clone(), inner.queue.pop_front().unwrap(), inner.cancel.clone())
        };
        let this = self.clone();
        stdin.write_all_async(line.into_bytes(), glib::Priority::DEFAULT, Some(&cancel), move |result| {
            this.0.borrow_mut().writing = false;
            let failed = match result {
                Ok((_, _, None)) => false,
                _ => true,
            };
            if failed {
                let (closed, callback) = { let i = this.0.borrow(); (i.closed, i.failed.clone()) };
                if !closed {
                    callback(UNAVAILABLE_MESSAGE.into());
                }
                return;
            }
            this.write();
        });
    }

    /// EOF lets the helper reset modifiers and destroy its virtual keyboard;
    /// it is force-killed if still running after 2 s.
    pub fn close(&self) {
        let (stdin, cancel) = {
            let mut inner = self.0.borrow_mut();
            if inner.closed {
                return;
            }
            inner.closed = true;
            inner.queue.clear();
            (inner.stdin.clone(), inner.cancel.clone())
        };
        cancel.cancel();
        let _ = stdin.close(None::<&gio::Cancellable>);
        let inner = self.0.clone();
        glib::timeout_add_local_once(Duration::from_secs(2), move || {
            let inner = inner.borrow();
            if !inner.exited {
                inner.process.force_exit();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_validation() {
        assert_eq!(Command::Key(30, 1).line().unwrap(), "key 30 1\n");
        assert_eq!(Command::Key(247, 31).line().unwrap(), "key 247 31\n");
        assert!(Command::Key(248, 0).line().is_err());
        assert!(Command::Key(-1, 0).line().is_err());
        assert!(Command::Key(30, 32).line().is_err());
        assert_eq!(Command::Text(0x20ac, 0).line().unwrap(), "text 8364\n");
        assert!(Command::Text(0, 0).line().is_err());
        assert!(Command::Text(0x110000, 0).line().is_err());
        assert_eq!(Command::Layout("de".into()).line().unwrap(), "layout de\n");
        assert!(Command::Layout("us;false".into()).line().is_err());
        assert!(Command::Layout("a".repeat(40)).line().is_err());
        assert!(Command::from_kind("layout", 1, 0).is_err());
    }

    #[test]
    fn event_parsing() {
        assert_eq!(parse_event(br#"{"event":"ready","input_method":true}"#).unwrap()["event"], "ready");
        assert_eq!(parse_event(b"[1]").unwrap_err(), "Invalid input response.");
        assert!(parse_event(b"nope").is_err());
        let mut big = br#"{"event":""#.to_vec();
        big.extend(std::iter::repeat_n(b'a', LINE_LIMIT));
        big.extend(br#""}"#);
        assert_eq!(parse_event(&big).unwrap_err(), "Input response is too large.");
    }
}
