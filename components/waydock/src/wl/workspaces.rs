//! Atomic desktop workspace state and activation through ext-workspace-v1
//! (port of workspace_backend.py).
//!
//! In-process on Waydock's own `wl_display` (the `waydock-workspaces` helper
//! stays installed for other consumers). No desktop change is requested until
//! [`WorkspaceBackend::activate`]. Native ids are scoped per connection
//! session (`"<session>:<native>"`) so clicks cannot target a recycled handle
//! after a reconnect. An unsupported compositor leaves the navigator
//! unavailable (`supported() == Some(false)`) without retry loops or errors.
//!
//! Public API: `new(on_changed, on_error)` / `with_parts(.., connector,
//! scheduler)`, `start()`, `close()`, `ready()`, `error()`, `supported()`,
//! `protocol_version()`, `workspaces()` / `groups()` (ordered maps),
//! `sorted_workspaces(Option<output>)`, `activate(id) -> bool`, and the free
//! function [`natural_name`] (sort key).

use std::cell::{Ref, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use indexmap::IndexMap;
use serde_json::Value;
use suite_core::pyjson::{casefold, py_str};
use suite_core::scheduler::{GlibScheduler, Scheduler};
use suite_wayland::model::{error_json, Event as ModelEvent};
use suite_wayland::Protocols;

use super::link::{Connector, Link, LinkEvent, Request, Startup, WaylandConnector};
use super::windows::{int, native_id, unique, Invalid};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub urgent: bool,
    pub hidden: bool,
    pub can_activate: bool,
    pub coordinates: Vec<u64>,
    pub group_id: String,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceGroup {
    pub id: String,
    pub outputs: Vec<String>,
}

/// One part of [`natural_name`]: `(1, int)` for decimal runs, `(0, casefold)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamePart {
    Text(String),
    /// Decimal value as digits without leading zeros ("" = 0).
    Number(String),
}

impl Ord for NamePart {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            (Self::Text(_), Self::Number(_)) => Ordering::Less,
            (Self::Number(_), Self::Text(_)) => Ordering::Greater,
            (Self::Number(a), Self::Number(b)) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
        }
    }
}
impl PartialOrd for NamePart {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

/// Code points of the Unicode `Nd` zeros (each starts a run of ten digits):
/// Python's `str.isdecimal()` / `int()` accept all of them.
const DECIMAL_ZEROS: [u32; 76] = [
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6, 0xE50, 0xED0,
    0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620,
    0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136,
    0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0,
    0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE, 0x1D7D8,
    0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

fn decimal_value(c: char) -> Option<u8> {
    let code = c as u32;
    DECIMAL_ZEROS.iter().find(|zero| (**zero..**zero + 10).contains(&code)).map(|zero| (code - zero) as u8)
}

/// `natural_name(name)`: `re.split(r"([0-9]+)", name)` parts, decimal parts
/// (`str.isdecimal()`, any Unicode digits) as numbers, others casefolded.
pub fn natural_name(name: &str) -> Vec<NamePart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = name.chars().peekable();
    let finish = |part: String| -> NamePart {
        let digits: Option<String> = (!part.is_empty())
            .then(|| part.chars().map(|c| decimal_value(c).map(|d| (b'0' + d) as char)).collect())
            .flatten();
        match digits {
            Some(digits) => NamePart::Number(digits.trim_start_matches('0').to_owned()),
            None => NamePart::Text(casefold(&part)),
        }
    };
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            parts.push(finish(std::mem::take(&mut text)));
            let mut run = String::from(c);
            while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                run.push(d);
                chars.next();
            }
            parts.push(finish(run));
        } else {
            text.push(c);
        }
    }
    parts.push(finish(text));
    parts
}

const STARTUP_TIMEOUT_MS: u32 = 8000;
const RETRY_START_MS: u32 = 1000;
const RETRY_MAX_MS: u32 = 15000;
const LINE_LIMIT: usize = 1024 * 1024;

const MSG_NOT_STARTED: &str = "Waydock's workspace helper could not start. Build or reinstall it.";
const MSG_TIMEOUT: &str = "The compositor did not provide workspace information.";
const MSG_ENDED: &str = "The compositor's workspace connection ended.";
const MSG_INVALID: &str = "The workspace helper returned invalid workspace information.";
const MSG_UNSUPPORTED_HELPER: &str = "This compositor does not expose desktop workspaces.";
const MSG_UNAVAILABLE: &str = "Desktop workspace navigation is unavailable.";
const MSG_GONE: &str = "That workspace is no longer available.";
const MSG_CANNOT: &str = "This workspace cannot be activated.";
const MSG_REQUEST_FAILED: &str = "The workspace request failed.";

#[derive(Default)]
struct State {
    ready: bool,
    error: String,
    workspaces: IndexMap<String, Workspace>,
    groups: IndexMap<String, WorkspaceGroup>,
    supported: Option<bool>,
    protocol_version: i64,
    started: bool,
    closed: bool,
    session: u64,
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
pub struct WorkspaceBackend {
    inner: Rc<Inner>,
}

fn is_fatal(event: &ModelEvent) -> bool { matches!(event, ModelEvent::Error { fatal: true, .. }) }

fn output_list(value: Option<&Value>) -> Result<Vec<String>, Invalid> {
    let items = match value {
        None => return Ok(Vec::new()),
        Some(Value::Array(items)) if items.len() <= 1024 => items,
        _ => return Err(Invalid("invalid workspace outputs")),
    };
    let names = items.iter().map(|item| item.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
        .collect::<Option<Vec<_>>>().ok_or(Invalid("invalid workspace outputs"))?;
    Ok(unique(names))
}

impl WorkspaceBackend {
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
    /// None = unknown yet, Some(false) = the compositor has no ext-workspace.
    pub fn supported(&self) -> Option<bool> { self.inner.state.borrow().supported }
    pub fn protocol_version(&self) -> i64 { self.inner.state.borrow().protocol_version }
    pub fn workspaces(&self) -> Ref<'_, IndexMap<String, Workspace>> { Ref::map(self.inner.state.borrow(), |s| &s.workspaces) }
    pub fn workspace(&self, id: &str) -> Option<Workspace> { self.inner.state.borrow().workspaces.get(id).cloned() }
    pub fn groups(&self) -> Ref<'_, IndexMap<String, WorkspaceGroup>> { Ref::map(self.inner.state.borrow(), |s| &s.groups) }
    pub fn session(&self) -> u64 { self.inner.state.borrow().session }
    pub fn pending_sources(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.inner.state.borrow().sources.keys().copied().collect();
        names.sort_unstable();
        names
    }
    pub fn retry_delay(&self) -> u32 { self.inner.state.borrow().retry_delay }
    #[doc(hidden)]
    pub fn set_retry_delay(&self, ms: u32) { self.inner.state.borrow_mut().retry_delay = ms }
    #[doc(hidden)]
    pub fn set_session(&self, session: u64) { self.inner.state.borrow_mut().session = session }

    /// Apply one helper-protocol event (Python `_consume_event`); a malformed
    /// snapshot never publishes partial changes.
    pub fn consume_event(&self, event: &Value) -> Result<(), Invalid> { self.inner.consume_event(event) }

    /// Visible workspaces (not hidden; on `output`, or on no output), in
    /// compositor coordinate order, else natural name order.
    pub fn sorted_workspaces(&self, output: Option<&str>) -> Vec<Workspace> {
        let st = self.inner.state.borrow();
        let mut visible: Vec<Workspace> = st.workspaces.values()
            .filter(|w| !w.hidden && (output.is_none_or(str::is_empty) || w.outputs.is_empty()
                                      || output.is_some_and(|o| w.outputs.iter().any(|n| n == o))))
            .cloned().collect();
        let native = |w: &Workspace| w.id.rsplit(':').next().and_then(|n| n.parse::<u128>().ok()).unwrap_or(0);
        visible.sort_by(|a, b| {
            a.coordinates.is_empty().cmp(&b.coordinates.is_empty())
                .then_with(|| a.coordinates.cmp(&b.coordinates))
                .then_with(|| natural_name(&a.name).cmp(&natural_name(&b.name)))
                .then_with(|| a.group_id.cmp(&b.group_id))
                .then_with(|| native(a).cmp(&native(b)))
        });
        visible
    }

    pub fn activate(&self, id: &str) -> bool { self.inner.activate(id) }

    pub fn close(&self) {
        let (abort, link, sources) = {
            let mut st = self.inner.state.borrow_mut();
            if st.closed {
                return;
            }
            st.closed = true;
            st.session += 1;
            st.ready = false;
            st.supported = None;
            st.workspaces.clear();
            st.groups.clear();
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
        st.supported = None;
        st.workspaces.clear();
        st.groups.clear();
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
        let abort = self.connector.connect(Protocols::WORKSPACES, Box::new(move |startup| match weak.upgrade() {
            Some(inner) => inner.started(session, startup),
            None => {
                if let Startup::Ready(link) = startup {
                    link.close();
                }
            }
        }));
        let stale = {
            let mut st = self.state.borrow_mut();
            if st.session == session && !st.closed && st.link.is_none() && st.supported.is_none() {
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
                let supported = link.has_workspaces();
                if !supported && !fatal {
                    let line = format!("{{\"event\":\"unsupported\",\"message\":\"{MSG_UNSUPPORTED_HELPER}\"}}");
                    self.consume_line(session, &line);
                    return;
                }
                if fatal {
                    let message = self.error_or(MSG_ENDED);
                    self.lost(session, &message);
                    return;
                }
                link.publish_workspaces();
                if self.drain(session, &*link) {
                    self.consume_line(session, "{\"event\":\"ready\",\"protocol_version\":1}");
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
            if st.closed || st.session != session || st.supported == Some(false) {
                return;
            }
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
                let version = match event.get("protocol_version") {
                    None => 0,
                    some => int(some)?,
                };
                let startup = {
                    let mut st = self.state.borrow_mut();
                    st.ready = true;
                    st.supported = Some(true);
                    st.protocol_version = version;
                    st.error.clear();
                    st.retry_delay = RETRY_START_MS;
                    st.sources.remove("startup")
                };
                if let Some(source) = startup {
                    self.scheduler.cancel(source);
                }
            }
            Some("unsupported") => {
                let message = event.get("message").map(py_str).unwrap_or_else(|| MSG_UNAVAILABLE.to_owned());
                let (startup, abort, link) = {
                    let mut st = self.state.borrow_mut();
                    st.ready = false;
                    st.supported = Some(false);
                    st.workspaces.clear();
                    st.groups.clear();
                    st.error = message;
                    st.session += 1;
                    (st.sources.remove("startup"), st.abort.take(), st.link.take())
                };
                if let Some(source) = startup {
                    self.scheduler.cancel(source);
                }
                if let Some(abort) = abort {
                    abort();
                }
                if let Some(link) = link {
                    link.close();
                }
            }
            Some("state") => {
                let (values, group_values) = match (event.get("workspaces"), event.get("groups")) {
                    (Some(Value::Array(v)), Some(Value::Array(g))) if v.len() <= 4096 && g.len() <= 1024 => (v, g),
                    _ => return Err(Invalid("invalid workspace snapshot")),
                };
                // Parse into new maps first: no partial state, no transient double-active desktop.
                let mut groups: IndexMap<String, WorkspaceGroup> = IndexMap::new();
                for value in group_values {
                    if !value.is_object() {
                        return Err(Invalid("invalid workspace group"));
                    }
                    let ident = id(native_id(value.get("id"))?);
                    let outputs = output_list(value.get("outputs"))?;
                    if groups.contains_key(&ident) {
                        return Err(Invalid("duplicate workspace group"));
                    }
                    groups.insert(ident.clone(), WorkspaceGroup { id: ident, outputs });
                }
                let mut workspaces: IndexMap<String, Workspace> = IndexMap::new();
                for value in values {
                    if !value.is_object() {
                        return Err(Invalid("invalid workspace"));
                    }
                    let ident = id(native_id(value.get("id"))?);
                    let name = match value.get("name") {
                        None => "",
                        Some(Value::String(name)) if name.chars().count() <= 16384 => name,
                        Some(_) => return Err(Invalid("invalid workspace properties")),
                    };
                    let coordinates = match value.get("coordinates") {
                        None => Vec::new(),
                        Some(Value::Array(items)) if items.len() <= 256 => items.iter()
                            .map(|n| n.as_u64().filter(|n| *n <= u32::MAX as u64))
                            .collect::<Option<Vec<_>>>().ok_or(Invalid("invalid workspace properties"))?,
                        Some(_) => return Err(Invalid("invalid workspace properties")),
                    };
                    let mut flags = [false; 4];
                    for (slot, key) in flags.iter_mut().zip(["active", "urgent", "hidden", "can_activate"]) {
                        *slot = match value.get(key) {
                            None => false,
                            Some(Value::Bool(flag)) => *flag,
                            Some(_) => return Err(Invalid("invalid workspace properties")),
                        };
                    }
                    let group_id = if value.get("group_id").is_some_and(suite_core::pyjson::truthy) {
                        id(native_id(value.get("group_id"))?)
                    } else {
                        String::new()
                    };
                    let outputs = output_list(value.get("outputs"))?;
                    if workspaces.contains_key(&ident) || (!group_id.is_empty() && !groups.contains_key(&group_id)) {
                        return Err(Invalid("duplicate workspace or invalid group"));
                    }
                    workspaces.insert(ident.clone(), Workspace {
                        id: ident, name: name.to_owned(), active: flags[0], urgent: flags[1], hidden: flags[2],
                        can_activate: flags[3], coordinates, group_id, outputs,
                    });
                }
                let mut st = self.state.borrow_mut();
                st.groups = groups;
                st.workspaces = workspaces;
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

    fn activate(self: &Rc<Self>, ident: &str) -> bool {
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
            let Some((native, workspace)) = native.and_then(|n| st.workspaces.get(ident).map(|w| (n, w))) else {
                drop(st);
                return self.fail(MSG_GONE);
            };
            if !workspace.can_activate || workspace.hidden {
                drop(st);
                return self.fail(MSG_CANNOT);
            }
            (link, native)
        };
        self.state.borrow_mut().error.clear();
        match link.request(Request::ActivateWorkspace(native)) {
            Ok(()) => true,
            Err(error) => self.fail(&error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
