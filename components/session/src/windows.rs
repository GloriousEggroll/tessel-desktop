//! Open-window discovery for the graceful end session: a port of the parts of
//! tessel-dock/window_backend.py that session_end.py uses. It runs Tessel Dock's
//! `tessel-dock-toplevel` helper (wlr-foreign-toplevel-management + xdg-output),
//! reads its JSON-lines events and writes `close <native_id>\n` requests.
//! Window ids are `<helper session>:<native id>` and are valid only for one
//! helper session; a lost helper is relaunched (1 s, doubling up to 15 s).

use gio::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use tessel_core::timer::{self, Timeout};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

impl Window {
    pub fn new(id: &str, app_id: &str, title: &str) -> Self {
        Self { id: id.into(), app_id: app_id.into(), title: title.into(), ..Self::default() }
    }
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

/// What EndSession needs from Tessel Dock's `WindowBackend`.
pub trait WindowSource {
    fn start(&self);
    fn ready(&self) -> bool;
    /// Open windows in discovery order (Python dict order).
    fn windows(&self) -> Vec<Window>;
    fn close_window(&self, id: &str) -> bool;
    fn close(&self);
}

pub type Changed = Rc<dyn Fn()>;
pub type ErrorFn = Rc<dyn Fn(String)>;
/// `WindowBackend(on_changed, on_error)`.
pub type WindowFactory = Rc<dyn Fn(Changed, ErrorFn) -> Rc<dyn WindowSource>>;

/// Tessel Dock's window helper (`tessel-dock-toplevel`, found with
/// [`tessel_core::paths::program`] or on `PATH`), or `None` when it is not installed.
/// `SESSION_MANAGER_WINDOW_HELPER` overrides the search (development and tests).
pub fn helper_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SESSION_MANAGER_WINDOW_HELPER").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let helper = tessel_core::paths::program("tessel-dock-toplevel");
    if helper.is_absolute() { Some(helper) } else { glib::find_program_in_path(&helper) }
}

/// Factory for the real helper, or `None` when no helper is installed
/// (EndSession then reports "Open apps could not be checked.").
pub fn helper_factory() -> Option<WindowFactory> {
    let helper = helper_path()?;
    Some(Rc::new(move |changed, error| {
        HelperWindows::new(vec![helper.to_string_lossy().into_owned()], changed, error) as Rc<dyn WindowSource>
    }))
}

struct Inner {
    command: Vec<String>,
    on_changed: Changed,
    on_error: ErrorFn,
    ready: Cell<bool>,
    can_activate: Cell<bool>,
    protocol_version: Cell<i64>,
    error: RefCell<String>,
    windows: RefCell<Vec<Window>>,
    outputs: RefCell<HashMap<String, Output>>,
    output_names: RefCell<HashMap<i64, String>>,
    started: Cell<bool>,
    closed: Cell<bool>,
    session: Cell<u64>,
    process: RefCell<Option<gio::Subprocess>>,
    reader: RefCell<Option<gio::DataInputStream>>,
    cancel: RefCell<Option<gio::Cancellable>>,
    sources: RefCell<HashMap<&'static str, Timeout>>,
    writes: RefCell<VecDeque<(String, String)>>,
    writing: Cell<bool>,
    retry_delay: Cell<u32>,
}

#[derive(Clone)]
pub struct HelperWindows(Rc<Inner>);

impl HelperWindows {
    pub fn new(command: Vec<String>, on_changed: Changed, on_error: ErrorFn) -> Rc<Self> {
        Rc::new(Self(Rc::new(Inner {
            command, on_changed, on_error, ready: Cell::new(false), can_activate: Cell::new(false),
            protocol_version: Cell::new(0), error: RefCell::default(), windows: RefCell::default(),
            outputs: RefCell::default(), output_names: RefCell::default(), started: Cell::new(false),
            closed: Cell::new(false), session: Cell::new(0), process: RefCell::new(None), reader: RefCell::new(None),
            cancel: RefCell::new(None), sources: RefCell::default(), writes: RefCell::default(),
            writing: Cell::new(false), retry_delay: Cell::new(1000),
        })))
    }

    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn can_activate(&self) -> bool { self.0.can_activate.get() }
    pub fn protocol_version(&self) -> i64 { self.0.protocol_version.get() }
    pub fn outputs(&self) -> HashMap<String, Output> { self.0.outputs.borrow().clone() }
    pub fn session(&self) -> u64 { self.0.session.get() }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    fn later(&self, name: &'static str, milliseconds: u32, callback: impl FnOnce(&HelperWindows) + 'static) {
        if self.0.closed.get() || self.0.sources.borrow().contains_key(name) {
            return;
        }
        let weak = self.weak();
        let source = timer::once(milliseconds, move || {
            let Some(inner) = weak.upgrade() else { return };
            let this = HelperWindows(inner);
            this.0.sources.borrow_mut().remove(name);
            if !this.0.closed.get() {
                callback(&this);
            }
        });
        self.0.sources.borrow_mut().insert(name, source);
    }

    fn remove_source(&self, name: &str) {
        let source = self.0.sources.borrow_mut().remove(name);
        if let Some(source) = source {
            source.cancel();
        }
    }

    fn changed(&self) {
        self.later("changed", 0, |this| (this.0.on_changed)());
    }

    fn fail(&self, message: &str) -> bool {
        self.0.error.replace(message.to_owned());
        if !self.0.closed.get() {
            (self.0.on_error)(message.to_owned());
            self.changed();
        }
        false
    }

    fn clear(&self) {
        self.0.ready.set(false);
        self.0.can_activate.set(false);
        self.0.windows.borrow_mut().clear();
        self.0.outputs.borrow_mut().clear();
        self.0.output_names.borrow_mut().clear();
        self.0.writes.borrow_mut().clear();
        self.0.writing.set(false);
    }

    fn launch(&self) {
        if self.0.closed.get() {
            return;
        }
        let session = self.0.session.get() + 1;
        self.0.session.set(session);
        self.clear();
        let cancel = gio::Cancellable::new();
        self.0.cancel.replace(Some(cancel));
        let argv: Vec<&std::ffi::OsStr> = self.0.command.iter().map(std::ffi::OsStr::new).collect();
        let flags = gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE;
        let process = match gio::Subprocess::newv(&argv, flags) {
            Ok(process) => process,
            Err(_) => {
                self.lost(session, "Tessel Dock's window helper could not start. Build or reinstall it.");
                return;
            }
        };
        let reader = gio::DataInputStream::new(&process.stdout_pipe().expect("stdout pipe"));
        reader.set_newline_type(gio::DataStreamNewlineType::Lf);
        self.0.process.replace(Some(process.clone()));
        self.0.reader.replace(Some(reader));
        self.read(session);
        let weak = self.weak();
        let waited = process.clone();
        process.wait_async(None::<&gio::Cancellable>, move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = HelperWindows(inner);
            let current = this.0.process.borrow().as_ref() == Some(&waited);
            if !this.0.closed.get() && session == this.0.session.get() && current {
                let error = this.error();
                let message = if error.is_empty() { "The compositor's window helper stopped.".to_owned() } else { error };
                this.lost(session, &message);
            }
        });
        self.later("startup", 8000, move |this| {
            if !this.0.ready.get() {
                this.lost(session, "The compositor did not provide window information.");
            }
        });
        self.changed();
    }

    fn read(&self, session: u64) {
        if self.0.closed.get() || session != self.0.session.get() {
            return;
        }
        let Some(reader) = self.0.reader.borrow().clone() else { return };
        let cancel = self.0.cancel.borrow().clone();
        let weak = self.weak();
        reader.read_line_async(glib::Priority::DEFAULT, cancel.as_ref(), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let this = HelperWindows(inner);
            this.read_done(result.map(|line| line.map(|bytes| bytes.to_vec())), session);
        });
    }

    fn read_done(&self, result: Result<Option<Vec<u8>>, glib::Error>, session: u64) {
        let line = match result {
            Ok(line) => line,
            Err(_) => {
                if !self.0.closed.get() && session == self.0.session.get() {
                    self.lost(session, "The connection to the window helper was interrupted.");
                }
                return;
            }
        };
        if self.0.closed.get() || session != self.0.session.get() {
            return;
        }
        let Some(line) = line else {
            let error = self.error();
            let message = if error.is_empty() { "The compositor's window connection ended.".to_owned() } else { error };
            self.lost(session, &message);
            return;
        };
        let parsed = if line.len() > 1024 * 1024 {
            Err(())
        } else {
            serde_json::from_str::<Value>(&String::from_utf8_lossy(&line)).map_err(|_| ()).and_then(|event| self.consume(&event))
        };
        if parsed.is_err() {
            self.lost(session, "The window helper returned invalid window information.");
            return;
        }
        self.read(session);
    }

    fn lost(&self, session: u64, message: &str) {
        if self.0.closed.get() || session != self.0.session.get() {
            return;
        }
        // Invalidate every queued/read callback and every former window id.
        self.0.session.set(session + 1);
        self.clear();
        if let Some(cancel) = self.0.cancel.borrow().as_ref() {
            cancel.cancel();
        }
        let process = self.0.process.take();
        self.0.reader.replace(None);
        if let Some(process) = process {
            process.force_exit();
        }
        self.remove_source("startup");
        self.fail(message);
        let delay = self.0.retry_delay.get();
        self.later("reconnect", delay, |this| this.launch());
        self.0.retry_delay.set((delay * 2).min(15000));
    }

    fn ident(&self, native: &Value) -> Result<String, ()> {
        let text = native.as_str().ok_or(())?;
        let valid = !text.is_empty() && text.len() <= 20 && text.as_bytes()[0] != b'0' && text.bytes().all(|b| b.is_ascii_digit());
        if !valid {
            return Err(());
        }
        Ok(format!("{}:{text}", self.0.session.get()))
    }

    fn consume(&self, event: &Value) -> Result<(), ()> {
        let object = event.as_object().ok_or(())?;
        let kind = object.get("event").ok_or(())?;
        let text = |key: &str| -> Result<String, ()> {
            match object.get(key) {
                None => Ok(String::new()),
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(()),
            }
        };
        let is_true = |key: &str| object.get(key) == Some(&Value::Bool(true));
        let int = |key: &str, default: i64| -> Result<i64, ()> {
            match object.get(key) {
                None => Ok(default),
                Some(value) => tessel_core::pyjson::py_int(Some(value)).ok_or(()),
            }
        };
        match kind.as_str() {
            Some("ready") => {
                self.0.ready.set(true);
                self.0.can_activate.set(is_true("seat"));
                self.0.protocol_version.set(int("protocol_version", 0)?);
                self.0.error.replace(String::new());
                self.0.retry_delay.set(1000);
                self.remove_source("startup");
            }
            Some("window") => {
                let id = self.ident(object.get("id").ok_or(())?)?;
                let outputs = match object.get("outputs") {
                    None => Vec::new(),
                    Some(Value::Array(items)) => {
                        let mut names: Vec<String> = Vec::new();
                        for item in items {
                            let name = item.as_str().ok_or(())?;
                            if !names.iter().any(|known| known == name) {
                                names.push(name.into());
                            }
                        }
                        names
                    }
                    Some(_) => return Err(()),
                };
                let app_id = text("app_id")?;
                let title = text("title")?;
                let parent_id = match object.get("parent") {
                    Some(parent) if tessel_core::pyjson::truthy(parent) => self.ident(parent)?,
                    _ => String::new(),
                };
                let window = Window { id: id.clone(), app_id, title, activated: is_true("activated"),
                    fullscreen: is_true("fullscreen"), minimized: is_true("minimized"), outputs, parent_id,
                    maximized: is_true("maximized") };
                let mut windows = self.0.windows.borrow_mut();
                match windows.iter_mut().find(|known| known.id == id) {
                    Some(known) => *known = window,
                    None => windows.push(window),
                }
            }
            Some("closed") => {
                let id = self.ident(object.get("id").ok_or(())?)?;
                self.0.windows.borrow_mut().retain(|window| window.id != id);
            }
            Some("output") => {
                let name = match object.get("name") {
                    Some(Value::String(name)) if !name.is_empty() => name.clone(),
                    _ => return Err(()),
                };
                let description = text("description")?;
                let native = tessel_core::pyjson::py_int(object.get("id")).ok_or(())?;
                let previous = self.0.output_names.borrow().get(&native).cloned();
                if let Some(previous) = previous.filter(|previous| *previous != name) {
                    self.0.outputs.borrow_mut().remove(&previous);
                }
                self.0.output_names.borrow_mut().insert(native, name.clone());
                let output = Output { name: name.clone(), description, x: int("x", 0)?, y: int("y", 0)?,
                    width: int("width", 0)?, height: int("height", 0)?, scale: int("scale", 1)?.max(1) };
                self.0.outputs.borrow_mut().insert(name, output);
            }
            Some("output-removed") => {
                let native = tessel_core::pyjson::py_int(object.get("id")).ok_or(())?;
                let name = self.0.output_names.borrow_mut().remove(&native);
                if let Some(name) = name {
                    self.0.outputs.borrow_mut().remove(&name);
                }
            }
            Some("error") => {
                let message = match object.get("message") {
                    Some(value) => tessel_core::pyjson::py_str(value),
                    None => "The window request failed.".into(),
                };
                self.fail(&message);
                return Ok(());
            }
            _ => return Err(()),
        }
        self.changed();
        Ok(())
    }

    fn send(&self, action: &str, id: &str) -> bool {
        if self.0.closed.get() || !self.0.ready.get() || self.0.process.borrow().is_none() {
            return self.fail("Window management is unavailable.");
        }
        let known = self.0.windows.borrow().iter().any(|window| window.id == id);
        if !known || !id.starts_with(&format!("{}:", self.0.session.get())) {
            return self.fail("That window is no longer available.");
        }
        if action == "activate" && !self.0.can_activate.get() {
            return self.fail("No input seat is available to focus windows.");
        }
        if self.0.writes.borrow().len() >= 128 {
            return self.fail("Too many window requests are pending.");
        }
        self.0.error.replace(String::new());
        self.0.writes.borrow_mut().push_back((action.into(), id.into()));
        self.write_next();
        true
    }

    fn write_next(&self) {
        if self.0.writing.get() || self.0.closed.get() || self.0.process.borrow().is_none() {
            return;
        }
        loop {
            let Some((action, id)) = self.0.writes.borrow_mut().pop_front() else { return };
            if !self.0.windows.borrow().iter().any(|window| window.id == id) {
                continue;
            }
            let native = id.split_once(':').map(|(_, native)| native.to_owned()).unwrap_or_default();
            let data = format!("{action} {native}\n").into_bytes();
            self.0.writing.set(true);
            let session = self.0.session.get();
            let stdin = self.0.process.borrow().as_ref().and_then(|p| p.stdin_pipe());
            let Some(stdin) = stdin else {
                self.lost(session, "The window request could not be sent.");
                return;
            };
            let cancel = self.0.cancel.borrow().clone();
            let weak = self.weak();
            stdin.write_all_async(data, glib::Priority::DEFAULT, cancel.as_ref(), move |result| {
                let Some(inner) = weak.upgrade() else { return };
                let this = HelperWindows(inner);
                let ok = matches!(result, Ok((_, _, None)));
                if !ok {
                    if !this.0.closed.get() && session == this.0.session.get() {
                        this.lost(session, "The window request could not be sent.");
                    }
                    return;
                }
                if !this.0.closed.get() && session == this.0.session.get() {
                    this.0.writing.set(false);
                    this.write_next();
                }
            });
            return;
        }
    }

    pub fn activate(&self, id: &str) -> bool { self.send("activate", id) }
    pub fn minimize(&self, id: &str) -> bool { self.send("minimize", id) }
    pub fn unminimize(&self, id: &str) -> bool { self.send("unminimize", id) }
}

impl WindowSource for HelperWindows {
    fn start(&self) {
        if self.0.started.get() || self.0.closed.get() {
            return;
        }
        self.0.started.set(true);
        self.launch();
    }

    fn ready(&self) -> bool { self.0.ready.get() }

    fn windows(&self) -> Vec<Window> { self.0.windows.borrow().clone() }

    fn close_window(&self, id: &str) -> bool { self.send("close", id) }

    fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.session.set(self.0.session.get() + 1);
        if let Some(cancel) = self.0.cancel.borrow().as_ref() {
            cancel.cancel();
        }
        let sources: Vec<Timeout> = self.0.sources.borrow_mut().drain().map(|(_, source)| source).collect();
        for source in sources {
            source.cancel();
        }
        self.0.writes.borrow_mut().clear();
        self.0.reader.replace(None);
        if let Some(process) = self.0.process.take() {
            process.force_exit();
        }
        self.0.ready.set(false);
        self.0.windows.borrow_mut().clear();
    }
}

impl WindowSource for Rc<HelperWindows> {
    fn start(&self) { (**self).start() }
    fn ready(&self) -> bool { (**self).ready() }
    fn windows(&self) -> Vec<Window> { (**self).windows() }
    fn close_window(&self, id: &str) -> bool { (**self).close_window(id) }
    fn close(&self) { (**self).close() }
}
