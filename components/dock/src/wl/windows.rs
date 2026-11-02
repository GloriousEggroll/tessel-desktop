//! Asynchronous native Wayland window discovery and exact-window actions
//! (port of window_backend.py).
//!
//! wlr-foreign-toplevel-management + xdg-output on Tessel Dock's **own**
//! `wl_display` connection (in-process; the `tessel-dock-toplevel` helper stays
//! installed for other consumers). Window IDs are `"<session>:<native>"` and
//! valid only for one connection session: every (re)connection, loss or
//! `close()` bumps the session, so stale clicks are rejected. The protocol has
//! no workspace metadata ([`WindowBackend::WORKSPACE_VISIBILITY_KNOWN`] is
//! false); output membership describes geometry only.
//!
//! Public API (Python names):
//! - [`WindowBackend::new(on_changed, on_error)`] (real compositor) or
//!   [`WindowBackend::with_parts`] (injectable [`Connector`] + [`Scheduler`]).
//! - `start()`, `close()`; state: `ready()`, `error()`, `can_activate()`,
//!   `protocol_version()`, `windows()` (ordered `IndexMap<id, Window>`, Python
//!   dict order), `window(id)`, `outputs()` (`IndexMap<name, Output>`),
//!   `snapshot()` (owned, `Send`: for the fullscreen guard's worker).
//! - actions → bool (false + `on_error` text when refused): `activate`
//!   (needs a seat; unminimizes first), `minimize`, `unminimize`, `close_window`.
//! - `on_changed` is coalesced to one idle callback; `on_error(&str)` gets the
//!   English texts of the Python module (it does not translate them either).
//! - Startup timeout 8 s, reconnect backoff 1 s doubling to 15 s, connection
//!   loss clears all state.

use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use indexmap::IndexMap;
use serde_json::Value;
use tessel_core::pyjson::{py_int, py_str, truthy};
use tessel_core::scheduler::{GlibScheduler, Scheduler};
use tessel_wayland::model::{error_json, Event as ModelEvent};
use tessel_wayland::Protocols;

use super::link::{Connector, Link, LinkEvent, Request, Startup, WaylandConnector};

/// One foreign toplevel (Python `Window` dataclass; field order kept).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// `Window(id, app_id, title)` with every other field at its default.
    pub fn new(id: &str, app_id: &str, title: &str) -> Self {
        Self { id: id.to_owned(), app_id: app_id.to_owned(), title: title.to_owned(), ..Default::default() }
    }
}

/// One output (logical geometry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub name: String,
    pub description: String,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub scale: i64,
}

impl Output {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_owned(), description: String::new(), x: 0, y: 0, width: 0, height: 0, scale: 1 }
    }
}

/// Owned copy of the window state (Python's `SimpleNamespace(windows=…, outputs=…)`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsSnapshot {
    pub windows: IndexMap<String, Window>,
    pub outputs: IndexMap<String, Output>,
}

/// A malformed helper event (Python's ValueError/TypeError/KeyError).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub &'static str);

/// `[1-9][0-9]{0,19}` — the only native id form accepted.
pub(crate) fn native_id(value: Option<&Value>) -> Result<&str, Invalid> {
    match value {
        Some(Value::String(text))
            if (1..=20).contains(&text.len())
                && text.as_bytes()[0] != b'0'
                && text.bytes().all(|b| b.is_ascii_digit()) => Ok(text),
        _ => Err(Invalid("invalid native ID")),
    }
}

/// Python `int(value)` (KeyError for a missing key is the caller's `None`).
pub(crate) fn int(value: Option<&Value>) -> Result<i64, Invalid> {
    match value {
        None => Err(Invalid("missing field")),
        Some(v) => py_int(Some(v)).ok_or(Invalid("invalid integer")),
    }
}

fn int_or(event: &Value, key: &str, default: i64) -> Result<i64, Invalid> {
    match event.get(key) {
        None => Ok(default),
        some => int(some),
    }
}

fn string_or_empty<'a>(event: &'a Value, key: &str) -> Result<&'a str, Invalid> {
    match event.get(key) {
        None => Ok(""),
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(Invalid("invalid string")),
    }
}

/// `tuple(dict.fromkeys(names))`.
pub(crate) fn unique(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

const STARTUP_TIMEOUT_MS: u32 = 8000;
const RETRY_START_MS: u32 = 1000;
const RETRY_MAX_MS: u32 = 15000;
const LINE_LIMIT: usize = 1024 * 1024;

const MSG_NOT_STARTED: &str = "Tessel Dock's window helper could not start. Build or reinstall it.";
const MSG_TIMEOUT: &str = "The compositor did not provide window information.";
const MSG_ENDED: &str = "The compositor's window connection ended.";
const MSG_INVALID: &str = "The window helper returned invalid window information.";
const MSG_UNSUPPORTED: &str = "This compositor does not support foreign-toplevel window management.";
const MSG_UNAVAILABLE: &str = "Window management is unavailable.";
const MSG_GONE: &str = "That window is no longer available.";
const MSG_NO_SEAT: &str = "No input seat is available to focus windows.";
const MSG_REQUEST_FAILED: &str = "The window request failed.";

#[derive(Default)]
struct State {
    ready: bool,
    error: String,
    windows: IndexMap<String, Window>,
    outputs: IndexMap<String, Output>,
    can_activate: bool,
    protocol_version: i64,
    started: bool,
    closed: bool,
    session: u64,
    output_names: HashMap<i64, String>,
    retry_delay: u32,
    link: Option<Rc<dyn Link>>,
    abort: Option<Box<dyn FnOnce()>>,
    sources: HashMap<&'static str, u64>,
}

struct Inner {
    state: RefCell<State>,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    connector: Rc<dyn Connector>,
    scheduler: Rc<dyn Scheduler>,
}

/// See the module documentation. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct WindowBackend {
    inner: Rc<Inner>,
}

fn is_fatal(event: &ModelEvent) -> bool { matches!(event, ModelEvent::Error { fatal: true, .. }) }

impl WindowBackend {
    /// The protocol carries no workspace membership.
    pub const WORKSPACE_VISIBILITY_KNOWN: bool = false;

    /// Real compositor connection on the thread-default main context.
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) -> Self {
        Self::with_parts(on_changed, on_error, Rc::new(WaylandConnector), Rc::new(GlibScheduler::default()))
    }

    pub fn with_parts(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static,
                      connector: Rc<dyn Connector>, scheduler: Rc<dyn Scheduler>) -> Self {
        let state = State { retry_delay: RETRY_START_MS, ..Default::default() };
        Self {
            inner: Rc::new(Inner {
                state: RefCell::new(state), on_changed: Box::new(on_changed), on_error: Box::new(on_error),
                connector, scheduler,
            }),
        }
    }

    pub fn start(&self) {
        {
            let mut st = self.inner.state.borrow_mut();
            if st.started || st.closed {
                return;
            }
            st.started = true;
        }
        self.inner.launch();
    }

    pub fn ready(&self) -> bool { self.inner.state.borrow().ready }
    pub fn error(&self) -> String { self.inner.state.borrow().error.clone() }
    pub fn can_activate(&self) -> bool { self.inner.state.borrow().can_activate }
    pub fn protocol_version(&self) -> i64 { self.inner.state.borrow().protocol_version }
    pub fn is_closed(&self) -> bool { self.inner.state.borrow().closed }
    /// Current windows in Python dict order (first seen first). Do not hold
    /// the borrow across calls into the backend.
    pub fn windows(&self) -> Ref<'_, IndexMap<String, Window>> { Ref::map(self.inner.state.borrow(), |s| &s.windows) }
    pub fn window(&self, id: &str) -> Option<Window> { self.inner.state.borrow().windows.get(id).cloned() }
    pub fn outputs(&self) -> Ref<'_, IndexMap<String, Output>> { Ref::map(self.inner.state.borrow(), |s| &s.outputs) }
    pub fn snapshot(&self) -> WindowsSnapshot {
        let st = self.inner.state.borrow();
        WindowsSnapshot { windows: st.windows.clone(), outputs: st.outputs.clone() }
    }
    /// Current connection session (the id prefix).
    pub fn session(&self) -> u64 { self.inner.state.borrow().session }
    /// Names of pending timers (`changed`, `startup`, `reconnect`) — Python `_sources`.
    pub fn pending_sources(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.inner.state.borrow().sources.keys().copied().collect();
        names.sort_unstable();
        names
    }
    /// Current reconnect delay in ms (Python `_retry_delay`).
    pub fn retry_delay(&self) -> u32 { self.inner.state.borrow().retry_delay }
    #[doc(hidden)]
    pub fn set_retry_delay(&self, ms: u32) { self.inner.state.borrow_mut().retry_delay = ms }
    #[doc(hidden)]
    /// Test hook (Python tests assign `_session`).
    pub fn set_session(&self, session: u64) { self.inner.state.borrow_mut().session = session }

    /// Apply one helper-protocol event (Python `_consume_event`). Errors leave
    /// the state untouched.
    pub fn consume_event(&self, event: &Value) -> Result<(), Invalid> { self.inner.consume_event(event) }

    pub fn activate(&self, id: &str) -> bool { self.inner.send("activate", id) }
    pub fn minimize(&self, id: &str) -> bool { self.inner.send("minimize", id) }
    pub fn unminimize(&self, id: &str) -> bool { self.inner.send("unminimize", id) }
    pub fn close_window(&self, id: &str) -> bool { self.inner.send("close", id) }

    /// Stop for good: no callbacks, timers or reconnects afterwards.
    pub fn close(&self) {
        let (abort, link, sources) = {
            let mut st = self.inner.state.borrow_mut();
            if st.closed {
                return;
            }
            st.closed = true;
            st.session += 1;
            st.ready = false;
            st.can_activate = false;
            st.windows.clear();
            st.outputs.clear();
            (st.abort.take(), st.link.take(), std::mem::take(&mut st.sources))
        };
        for id in sources.into_values() {
            self.inner.scheduler.cancel(id);
        }
        if let Some(abort) = abort {
            abort();
        }
        if let Some(link) = link {
            link.close();
        }
    }
}

impl Inner {
    fn later(self: &Rc<Self>, name: &'static str, ms: u32, callback: impl FnOnce(&Rc<Inner>) + 'static) {
        {
            let st = self.state.borrow();
            if st.closed || st.sources.contains_key(name) {
                return;
            }
        }
        let weak: Weak<Inner> = Rc::downgrade(self);
        let id = self.scheduler.timeout(ms, Box::new(move || {
            let Some(inner) = weak.upgrade() else { return };
            let closed = {
                let mut st = inner.state.borrow_mut();
                st.sources.remove(name);
                st.closed
            };
            if !closed {
                callback(&inner);
            }
        }));
        self.state.borrow_mut().sources.insert(name, id);
    }

    fn changed(self: &Rc<Self>) { self.later("changed", 0, |inner| (inner.on_changed)()) }

    fn fail(self: &Rc<Self>, message: &str) -> bool {
        let closed = {
            let mut st = self.state.borrow_mut();
            st.error = message.to_owned();
            st.closed
        };
        if !closed {
            (self.on_error)(message);
            self.changed();
        }
        false
    }

    fn reset(st: &mut State) {
        st.ready = false;
        st.can_activate = false;
        st.windows.clear();
        st.outputs.clear();
        st.output_names.clear();
    }

    fn launch(self: &Rc<Self>) {
        let session = {
            let mut st = self.state.borrow_mut();
            if st.closed {
                return;
            }
            st.session += 1;
            Self::reset(&mut st);
            st.session
        };
        let weak = Rc::downgrade(self);
        let abort = self.connector.connect(Protocols::TOPLEVELS, Box::new(move |startup| match weak.upgrade() {
            Some(inner) => inner.started(session, startup),
            None => {
                if let Startup::Ready(link) = startup {
                    link.close();
                }
            }
        }));
        let stale = {
            let mut st = self.state.borrow_mut();
            if st.session == session && !st.closed && st.link.is_none() {
                st.abort = Some(abort);
                None
            } else {
                Some(abort)
            }
        };
        if let Some(abort) = stale {
            abort();
        }
        self.later("startup", STARTUP_TIMEOUT_MS, move |inner| inner.startup_timeout(session));
        self.changed();
    }

    fn current(&self, session: u64) -> bool {
        let st = self.state.borrow();
        !st.closed && st.session == session
    }

    fn error_or(&self, default: &str) -> String {
        let st = self.state.borrow();
        if st.error.is_empty() { default.to_owned() } else { st.error.clone() }
    }

    fn started(self: &Rc<Self>, session: u64, startup: Startup) {
        if !self.current(session) {
            if let Startup::Ready(link) = startup {
                link.close();
            }
            return;
        }
        self.state.borrow_mut().abort = None;
        match startup {
            Startup::Ready(link) => {
                self.state.borrow_mut().link = Some(link.clone());
                let weak = Rc::downgrade(self);
                link.set_handler(Box::new(move |event| {
                    if let Some(inner) = weak.upgrade() {
                        inner.link_event(session, event);
                    }
                }));
                let fatal = link.take_events().iter().any(is_fatal);
                if !self.drain(session, &*link) {
                    return;
                }
                let mut running = !fatal;
                if !link.has_toplevels() {
                    if !self.consume_line(session, &error_json(MSG_UNSUPPORTED)) {
                        return;
                    }
                    running = false;
                }
                if running {
                    let ready = format!("{{\"event\":\"ready\",\"seat\":{},\"protocol_version\":{}}}",
                                        link.has_seat(), link.toplevel_version());
                    self.consume_line(session, &ready);
                } else {
                    let message = self.error_or(MSG_ENDED);
                    self.lost(session, &message);
                }
            }
            Startup::Failed { lines, message } => {
                for line in lines {
                    if !self.consume_line(session, &line) {
                        return;
                    }
                }
                if let Some(message) = message {
                    if !self.consume_line(session, &error_json(&message)) {
                        return;
                    }
                }
                let message = self.error_or(MSG_ENDED);
                self.lost(session, &message);
            }
            Startup::NotStarted => self.lost(session, MSG_NOT_STARTED),
        }
    }

    /// Consume every queued line; false once the session was lost.
    fn drain(self: &Rc<Self>, session: u64, link: &dyn Link) -> bool {
        for line in link.take_lines() {
            if !self.consume_line(session, &line) {
                return false;
            }
        }
        self.current(session)
    }

    fn link_event(self: &Rc<Self>, session: u64, event: LinkEvent) {
        if !self.current(session) {
            return;
        }
        let Some(link) = self.state.borrow().link.clone() else { return };
        if !self.drain(session, &*link) {
            return;
        }
        let ended = match event {
            LinkEvent::Changed(change) => is_fatal(&change),
            LinkEvent::Disconnected(_) => true,
        };
        if ended {
            let message = self.error_or(MSG_ENDED);
            self.lost(session, &message);
        }
    }

    fn startup_timeout(self: &Rc<Self>, session: u64) {
        if !self.state.borrow().ready {
            self.lost(session, MSG_TIMEOUT);
        }
    }

    /// One helper line; false if it ended the session.
    fn consume_line(self: &Rc<Self>, session: u64, line: &str) -> bool {
        if !self.current(session) {
            return false;
        }
        let result = if line.len() > LINE_LIMIT {
            Err(Invalid("oversized event"))
        } else {
            serde_json::from_str::<Value>(line).map_err(|_| Invalid("invalid JSON")).and_then(|v| self.consume_event(&v))
        };
        if result.is_err() {
            self.lost(session, MSG_INVALID);
            return false;
        }
        self.current(session)
    }

    fn lost(self: &Rc<Self>, session: u64, message: &str) {
        let (abort, link, startup) = {
            let mut st = self.state.borrow_mut();
            if st.closed || st.session != session {
                return;
            }
            // Invalidate every queued callback and every former window ID.
            st.session += 1;
            Self::reset(&mut st);
            (st.abort.take(), st.link.take(), st.sources.remove("startup"))
        };
        if let Some(abort) = abort {
            abort();
        }
        if let Some(link) = link {
            link.close();
        }
        if let Some(id) = startup {
            self.scheduler.cancel(id);
        }
        self.fail(message);
        let delay = self.state.borrow().retry_delay;
        self.later("reconnect", delay, |inner| inner.launch());
        let mut st = self.state.borrow_mut();
        st.retry_delay = (st.retry_delay.saturating_mul(2)).min(RETRY_MAX_MS);
    }

    fn consume_event(self: &Rc<Self>, event: &Value) -> Result<(), Invalid> {
        if !event.is_object() {
            return Err(Invalid("invalid event"));
        }
        let kind = event.get("event").ok_or(Invalid("missing event"))?;
        let session = self.state.borrow().session;
        let id = |native: &str| format!("{session}:{native}");
        match kind.as_str() {
            Some("ready") => {
                let version = int_or(event, "protocol_version", 0)?;
                let startup = {
                    let mut st = self.state.borrow_mut();
                    st.ready = true;
                    st.can_activate = event.get("seat") == Some(&Value::Bool(true));
                    st.protocol_version = version;
                    st.error.clear();
                    st.retry_delay = RETRY_START_MS;
                    st.sources.remove("startup")
                };
                if let Some(source) = startup {
                    self.scheduler.cancel(source);
                }
            }
            Some("window") => {
                let ident = id(native_id(event.get("id"))?);
                let app_id = string_or_empty(event, "app_id")?;
                let title = string_or_empty(event, "title")?;
                let outputs = match event.get("outputs") {
                    None => Vec::new(),
                    Some(Value::Array(items)) => items.iter().map(|item| item.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>().ok_or(Invalid("invalid window"))?,
                    Some(_) => return Err(Invalid("invalid window")),
                };
                let parent = if event.get("parent").is_some_and(truthy) { id(native_id(event.get("parent"))?) } else { String::new() };
                let flag = |key: &str| event.get(key) == Some(&Value::Bool(true));
                let window = Window {
                    id: ident.clone(), app_id: app_id.to_owned(), title: title.to_owned(), activated: flag("activated"),
                    fullscreen: flag("fullscreen"), minimized: flag("minimized"), outputs: unique(outputs),
                    parent_id: parent, maximized: flag("maximized"),
                };
                self.state.borrow_mut().windows.insert(ident, window);
            }
            Some("closed") => {
                let ident = id(native_id(event.get("id"))?);
                self.state.borrow_mut().windows.shift_remove(&ident);
            }
            Some("output") => {
                let name = match event.get("name") {
                    None => return Err(Invalid("missing name")),
                    Some(Value::String(name)) if !name.is_empty() => name.clone(),
                    Some(_) => return Err(Invalid("invalid output")),
                };
                let description = string_or_empty(event, "description").map_err(|_| Invalid("invalid output"))?.to_owned();
                let native = int(event.get("id"))?;
                let output = Output {
                    name: name.clone(), description, x: int_or(event, "x", 0)?, y: int_or(event, "y", 0)?,
                    width: int_or(event, "width", 0)?, height: int_or(event, "height", 0)?,
                    scale: int_or(event, "scale", 1)?.max(1),
                };
                let mut st = self.state.borrow_mut();
                if let Some(previous) = st.output_names.get(&native).cloned() {
                    if previous != name {
                        st.outputs.shift_remove(&previous);
                    }
                }
                st.output_names.insert(native, name.clone());
                st.outputs.insert(name, output);
            }
            Some("output-removed") => {
                let native = int(event.get("id"))?;
                let mut st = self.state.borrow_mut();
                if let Some(name) = st.output_names.remove(&native) {
                    st.outputs.shift_remove(&name);
                }
            }
            Some("error") => {
                let message = event.get("message").map(py_str).unwrap_or_else(|| MSG_REQUEST_FAILED.to_owned());
                self.fail(&message);
                return Ok(());
            }
            _ => return Err(Invalid("unknown event")),
        }
        self.changed();
        Ok(())
    }

    fn send(self: &Rc<Self>, action: &str, ident: &str) -> bool {
        let (link, native) = {
            let st = self.state.borrow();
            let link = match (&st.link, st.closed, st.ready) {
                (Some(link), false, true) => link.clone(),
                _ => {
                    drop(st);
                    return self.fail(MSG_UNAVAILABLE);
                }
            };
            let prefix = format!("{}:", st.session);
            let native = ident.strip_prefix(&prefix).and_then(|n| n.parse::<u64>().ok());
            let Some(native) = native.filter(|_| st.windows.contains_key(ident)) else {
                drop(st);
                return self.fail(MSG_GONE);
            };
            if action == "activate" && !st.can_activate {
                drop(st);
                return self.fail(MSG_NO_SEAT);
            }
            (link, native)
        };
        self.state.borrow_mut().error.clear();
        let request = match action {
            "activate" => Request::Activate(native),
            "minimize" => Request::Minimize(native),
            "unminimize" => Request::Unminimize(native),
            _ => Request::Close(native),
        };
        match link.request(request) {
            Ok(()) => true,
            // The helper reported these as an `error` event.
            Err(error) => self.fail(&error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
