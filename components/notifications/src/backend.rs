//! Asynchronous native Wayland window discovery through Tessel Dock's
//! `tessel-dock-toplevel` helper (read-only port of tessel-dock/window_backend.py).
//!
//! The notification daemon only observes windows and outputs, so window
//! actions (activate/minimize/close requests on the helper's stdin) are not
//! ported here.

use gio::prelude::*;
use indexmap::IndexMap;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Window {
    pub id: String,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub outputs: Vec<String>,
    pub parent_id: String,
    pub maximized: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub name: String,
    pub description: String,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub scale: i64,
}

struct Inner {
    on_changed: Box<dyn Fn()>,
    command: Vec<PathBuf>,
    ready: Cell<bool>,
    error: RefCell<String>,
    windows: RefCell<IndexMap<String, Window>>,
    outputs: RefCell<IndexMap<String, Output>>,
    output_names: RefCell<HashMap<i64, String>>,
    started: Cell<bool>,
    closed: Cell<bool>,
    session: Cell<u64>,
    process: RefCell<Option<gio::Subprocess>>,
    cancel: RefCell<Option<gio::Cancellable>>,
    sources: RefCell<HashMap<&'static str, glib::SourceId>>,
    retry_delay: Cell<u64>,
}

#[derive(Clone)]
pub struct WindowBackend(Rc<Inner>);

/// Where `tessel-dock-toplevel` and `fullscreen_guard.py` live: the Rust-only
/// `NOTIFICATION_MANAGER_HELPER_DIR` override, the executable's directory when
/// it holds the helper, else the installed component directory.
pub fn helper_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NOTIFICATION_MANAGER_HELPER_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        if dir.join("tessel-dock-toplevel").is_file() {
            return dir;
        }
    }
    tessel_core::paths::config_home().join("waybar/scripts/tessel-notifications")
}

fn native_id(value: &Value) -> Result<String, ()> {
    let text = value.as_str().ok_or(())?;
    let valid = !text.is_empty() && text.len() <= 20 && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit());
    if valid { Ok(text.to_owned()) } else { Err(()) }
}

fn py_int(value: Option<&Value>, default: i64) -> Result<i64, ()> {
    match value {
        None => Ok(default),
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)).ok_or(()),
        Some(Value::Bool(flag)) => Ok(*flag as i64),
        Some(Value::String(text)) => text.trim().parse().map_err(|_| ()),
        _ => Err(()),
    }
}

impl WindowBackend {
    pub fn new(on_changed: impl Fn() + 'static, command: Option<Vec<PathBuf>>) -> Self {
        Self(Rc::new(Inner {
            on_changed: Box::new(on_changed),
            command: command.unwrap_or_else(|| vec![helper_dir().join("tessel-dock-toplevel")]),
            ready: Cell::new(false), error: RefCell::new(String::new()), windows: RefCell::new(IndexMap::new()),
            outputs: RefCell::new(IndexMap::new()), output_names: RefCell::new(HashMap::new()), started: Cell::new(false),
            closed: Cell::new(false), session: Cell::new(0), process: RefCell::new(None), cancel: RefCell::new(None),
            sources: RefCell::new(HashMap::new()), retry_delay: Cell::new(1000),
        }))
    }

    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn windows(&self) -> IndexMap<String, Window> { self.0.windows.borrow().clone() }
    pub fn outputs(&self) -> IndexMap<String, Output> { self.0.outputs.borrow().clone() }

    pub fn start(&self) {
        if self.0.started.get() || self.0.closed.get() {
            return;
        }
        self.0.started.set(true);
        self.launch();
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    fn later(&self, name: &'static str, milliseconds: u64, callback: impl FnOnce(WindowBackend) + 'static) {
        if self.0.closed.get() || self.0.sources.borrow().contains_key(name) {
            return;
        }
        let weak = self.weak();
        let run = move || {
            if let Some(inner) = weak.upgrade() {
                let backend = WindowBackend(inner);
                backend.0.sources.borrow_mut().remove(name);
                if !backend.0.closed.get() {
                    callback(backend);
                }
            }
        };
        let id = if milliseconds > 0 {
            glib::timeout_add_local_once(Duration::from_millis(milliseconds), run)
        } else {
            glib::idle_add_local_once(run)
        };
        self.0.sources.borrow_mut().insert(name, id);
    }

    fn changed(&self) {
        self.later("changed", 0, |backend| (backend.0.on_changed)());
    }

    fn fail(&self, message: &str) {
        self.0.error.replace(message.to_owned());
        if !self.0.closed.get() {
            self.changed();
        }
    }

    fn clear(&self) {
        self.0.ready.set(false);
        self.0.windows.borrow_mut().clear();
        self.0.outputs.borrow_mut().clear();
        self.0.output_names.borrow_mut().clear();
    }

    fn launch(&self) {
        if self.0.closed.get() {
            return;
        }
        let session = self.0.session.get() + 1;
        self.0.session.set(session);
        self.clear();
        let cancel = gio::Cancellable::new();
        self.0.cancel.replace(Some(cancel.clone()));
        let argv: Vec<&std::ffi::OsStr> = self.0.command.iter().map(|p| p.as_os_str()).collect();
        let process = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE
                                                   | gio::SubprocessFlags::STDERR_SILENCE) {
            Ok(process) => process,
            Err(_) => {
                self.lost(session, "Tessel Dock's window helper could not start. Build or reinstall it.");
                return;
            }
        };
        let reader = gio::DataInputStream::new(&process.stdout_pipe().expect("stdout pipe"));
        reader.set_newline_type(gio::DataStreamNewlineType::Lf);
        self.0.process.replace(Some(process.clone()));
        self.read(session, reader, cancel);
        let weak = self.weak();
        let watched = process.clone();
        process.wait_async(None::<&gio::Cancellable>, move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = WindowBackend(inner);
            let current = backend.0.process.borrow().as_ref() == Some(&watched);
            if !backend.0.closed.get() && session == backend.0.session.get() && current {
                let error = backend.error();
                backend.lost(session, if error.is_empty() { "The compositor's window helper stopped." } else { &error });
            }
        });
        self.later("startup", 8000, move |backend| {
            if !backend.0.ready.get() {
                backend.lost(session, "The compositor did not provide window information.");
            }
        });
        self.changed();
    }

    fn read(&self, session: u64, reader: gio::DataInputStream, cancel: gio::Cancellable) {
        if self.0.closed.get() || session != self.0.session.get() {
            return;
        }
        let weak = self.weak();
        let next = reader.clone();
        reader.read_line_async(glib::Priority::DEFAULT, Some(&cancel.clone()), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let backend = WindowBackend(inner);
            if backend.0.closed.get() || session != backend.0.session.get() {
                return;
            }
            let line = match result {
                Ok(Some(line)) => line,
                Ok(None) => {
                    let error = backend.error();
                    backend.lost(session, if error.is_empty() { "The compositor's window connection ended." } else { &error });
                    return;
                }
                Err(_) => {
                    backend.lost(session, "The connection to the window helper was interrupted.");
                    return;
                }
            };
            let parsed = if line.len() > 1024 * 1024 {
                Err(())
            } else {
                serde_json::from_slice::<Value>(line.as_slice()).map_err(|_| ()).and_then(|event| backend.consume(&event))
            };
            if parsed.is_err() {
                backend.lost(session, "The window helper returned invalid window information.");
                return;
            }
            backend.read(session, next, cancel);
        });
    }

    fn lost(&self, session: u64, message: &str) {
        if self.0.closed.get() || session != self.0.session.get() {
            return;
        }
        // Invalidate every queued/read callback and every former window ID.
        self.0.session.set(session + 1);
        self.clear();
        if let Some(cancel) = self.0.cancel.take() {
            cancel.cancel();
        }
        if let Some(process) = self.0.process.take() {
            process.force_exit();
        }
        if let Some(source) = self.0.sources.borrow_mut().remove("startup") {
            source.remove();
        }
        self.fail(message);
        self.later("reconnect", self.0.retry_delay.get(), |backend| backend.launch());
        self.0.retry_delay.set((self.0.retry_delay.get() * 2).min(15000));
    }

    fn id(&self, value: &Value) -> Result<String, ()> {
        Ok(format!("{}:{}", self.0.session.get(), native_id(value)?))
    }

    fn consume(&self, event: &Value) -> Result<(), ()> {
        let event = event.as_object().ok_or(())?;
        let kind = event.get("event").and_then(Value::as_str).ok_or(())?;
        match kind {
            "ready" => {
                self.0.ready.set(true);
                self.0.error.replace(String::new());
                self.0.retry_delay.set(1000);
                if let Some(source) = self.0.sources.borrow_mut().remove("startup") {
                    source.remove();
                }
            }
            "window" => {
                let ident = self.id(event.get("id").ok_or(())?)?;
                let string = |key: &str| match event.get(key) {
                    None => Ok(String::new()),
                    Some(Value::String(text)) => Ok(text.clone()),
                    _ => Err(()),
                };
                let (app_id, title) = (string("app_id")?, string("title")?);
                let mut outputs: Vec<String> = Vec::new();
                match event.get("outputs") {
                    None => {}
                    Some(Value::Array(list)) => for name in list {
                        let name = name.as_str().ok_or(())?;
                        if !outputs.iter().any(|known| known == name) {
                            outputs.push(name.to_owned());
                        }
                    },
                    _ => return Err(()),
                }
                let parent = match event.get("parent") {
                    Some(value) if tessel_core::pyjson::truthy(value) => self.id(value)?,
                    _ => String::new(),
                };
                let flag = |key: &str| event.get(key) == Some(&Value::Bool(true));
                let window = Window { id: ident.clone(), app_id, title, activated: flag("activated"),
                                      fullscreen: flag("fullscreen"), minimized: flag("minimized"), outputs,
                                      parent_id: parent, maximized: flag("maximized") };
                self.0.windows.borrow_mut().insert(ident, window);
            }
            "closed" => {
                let ident = self.id(event.get("id").ok_or(())?)?;
                self.0.windows.borrow_mut().shift_remove(&ident);
            }
            "output" => {
                let name = event.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).ok_or(())?.to_owned();
                let description = match event.get("description") {
                    None => String::new(),
                    Some(Value::String(text)) => text.clone(),
                    _ => return Err(()),
                };
                let native = py_int(event.get("id"), i64::MIN).and_then(|v| if v == i64::MIN { Err(()) } else { Ok(v) })?;
                let previous = self.0.output_names.borrow().get(&native).cloned();
                if let Some(previous) = previous.filter(|p| *p != name) {
                    self.0.outputs.borrow_mut().shift_remove(&previous);
                }
                self.0.output_names.borrow_mut().insert(native, name.clone());
                let output = Output { name: name.clone(), description, x: py_int(event.get("x"), 0)?, y: py_int(event.get("y"), 0)?,
                                      width: py_int(event.get("width"), 0)?, height: py_int(event.get("height"), 0)?,
                                      scale: py_int(event.get("scale"), 1)?.max(1) };
                self.0.outputs.borrow_mut().insert(name, output);
            }
            "output-removed" => {
                let native = py_int(event.get("id"), i64::MIN).and_then(|v| if v == i64::MIN { Err(()) } else { Ok(v) })?;
                if let Some(name) = self.0.output_names.borrow_mut().remove(&native) {
                    self.0.outputs.borrow_mut().shift_remove(&name);
                }
            }
            "error" => {
                let message = event.get("message").map(tessel_core::pyjson::py_str_or_empty)
                    .unwrap_or_else(|| "The window request failed.".into());
                self.fail(&message);
                return Ok(());
            }
            _ => return Err(()),
        }
        self.changed();
        Ok(())
    }

    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        self.0.session.set(self.0.session.get() + 1);
        if let Some(cancel) = self.0.cancel.take() {
            cancel.cancel();
        }
        for (_, source) in self.0.sources.borrow_mut().drain() {
            source.remove();
        }
        if let Some(process) = self.0.process.take() {
            process.force_exit();
        }
        self.clear();
    }
}
