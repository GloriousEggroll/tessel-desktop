//! bttransfer.py: outgoing BlueZ OBEX Object Push (`ObexSender`), the
//! incoming-file agent (`ObexReceiver`, `ReceiveRequest`, `org.bluez.obex.Agent1`
//! at `/org/tessel/Bluetooth/obex/agent`) and a small `org.freedesktop.Notifications`
//! client (`Notifier`). All callbacks run on the GLib main loop.
//!
//! No bus is opened until `start`. The source adapter address is always
//! explicit, so choosing another adapter cannot redirect an in-flight transfer.

use crate::backend::{first_props, Props};
use crate::dbus::{object_tuple, vardict, BtError, Conn, Invocation, Reply, Signal};
use glib::prelude::*;
use glib::Variant;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use tessel_core::pyjson;

pub const OBEX: &str = "org.bluez.obex";
pub const ROOT: &str = "/org/bluez/obex";
pub const CLIENT: &str = "org.bluez.obex.Client1";
pub const PUSH: &str = "org.bluez.obex.ObjectPush1";
pub const TRANSFER: &str = "org.bluez.obex.Transfer1";
pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
pub const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
pub const AGENT_MANAGER: &str = "org.bluez.obex.AgentManager1";
pub const AGENT: &str = "org.bluez.obex.Agent1";
pub const SESSION: &str = "org.bluez.obex.Session1";
pub const AGENT_PATH: &str = "/org/tessel/Bluetooth/obex/agent";
pub const REJECTED: &str = "org.bluez.obex.Error.Rejected";
pub const CANCELED: &str = "org.bluez.obex.Error.Canceled";
pub const DBUS: &str = "org.freedesktop.DBus";
pub const DBUS_PATH: &str = "/org/freedesktop/DBus";
pub const AGENT_XML: &str = r#"<node>
  <interface name="org.bluez.obex.Agent1">
    <method name="Release"/>
    <method name="AuthorizePush">
      <arg type="o" name="transfer" direction="in"/>
      <arg type="s" name="filename" direction="out"/>
    </method>
    <method name="Cancel"/>
  </interface>
</node>"#;

/// Whether an error needs the session-bus / BlueZ OBEX setup hint.
pub fn is_unavailable_error(error: &BtError) -> bool {
    use gio::DBusError::*;
    error.unavailable || matches!(error.dbus_code,
        Some(ServiceUnknown | NameHasNoOwner | SpawnServiceNotFound | SpawnExecFailed | UnknownInterface | UnknownMethod))
}

/// `os.path.abspath` (lexical normalization, no symlink resolution).
pub fn abspath(path: &str) -> String {
    let joined = if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(path)
    };
    let mut parts: Vec<String> = Vec::new();
    for component in joined.to_string_lossy().split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_owned()),
        }
    }
    format!("/{}", parts.join("/"))
}

fn props_u64(props: &Props, key: &str) -> Option<u64> {
    props.get(key).and_then(|v| pyjson::py_int(Some(v))).map(|n| n.max(0) as u64)
}

fn props_text(props: &Props, key: &str) -> Option<String> {
    props.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// `ObexSender.on_progress(dict)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub filename: String,
    pub index: usize,
    pub count: usize,
    pub transferred: u64,
    pub size: u64,
    pub completed: u64,
    pub total: u64,
    pub status: String,
}

pub type ProgressFn = Rc<dyn Fn(&Progress)>;
pub type DoneFn = Box<dyn FnOnce(Option<BtError>, bool)>;
/// Opens the private session-bus connection (`Gio.DBusConnection.new_for_address`).
pub type Connector = Rc<dyn Fn(Box<dyn FnOnce(Result<Rc<dyn Conn>, BtError>)>)>;

/// The real private-connection opener.
pub fn session_connector() -> Connector {
    Rc::new(|done| {
        let address = match gio::dbus_address_get_for_bus_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
            Ok(address) => address,
            Err(error) => return done(Err(BtError::from_glib(&error).into_unavailable())),
        };
        gio::DBusConnection::for_address(&address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None, None::<&gio::Cancellable>, move |result| match result {
                Ok(connection) => done(Ok(crate::dbus::GioConn::new(connection) as Rc<dyn Conn>)),
                Err(error) => done(Err(BtError::from_glib(&error).into_unavailable())),
            });
    })
}

/// Anything that sends files (the UI uses fakes in tests).
pub trait Sender {
    fn start(&self);
    fn cancel(&self);
}

#[derive(Default)]
struct SenderState {
    done: bool,
    cancelled: bool,
    started: bool,
    finishing: bool,
    creating: bool,
    session: Option<String>,
    transfer: Option<String>,
    early_props: HashMap<String, Props>,
    early_removed: HashSet<String>,
    index: usize,
    sizes: Vec<u64>,
    completed: u64,
    props: Props,
}

pub struct SenderInner {
    pub source: String,
    pub destination: String,
    pub files: Vec<String>,
    pub on_progress: RefCell<Option<ProgressFn>>,
    on_done: RefCell<Option<DoneFn>>,
    conn: RefCell<Option<Rc<dyn Conn>>>,
    owns_connection: Cell<bool>,
    connector: Connector,
    error: RefCell<Option<BtError>>,
    signal_ids: RefCell<Vec<u32>>,
    s: RefCell<SenderState>,
}

/// Send a file or sequence using one session. `on_done(error, cancelled)`
/// runs once after cleanup; instances are single-use.
#[derive(Clone)]
pub struct ObexSender(pub Rc<SenderInner>);

impl ObexSender {
    /// `connection`: inject a connection (tests); otherwise a private session
    /// connection is opened through `connector` (default: [`session_connector`]).
    pub fn new(source: &str, destination: &str, files: &[String], on_progress: Option<ProgressFn>, on_done: Option<DoneFn>,
               connection: Option<Rc<dyn Conn>>, connector: Option<Connector>) -> Self {
        Self(Rc::new(SenderInner {
            source: source.into(), destination: destination.into(), files: files.iter().map(|f| abspath(f)).collect(),
            on_progress: RefCell::new(on_progress), on_done: RefCell::new(on_done), owns_connection: Cell::new(connection.is_none()),
            conn: RefCell::new(connection), connector: connector.unwrap_or_else(session_connector), error: RefCell::new(None),
            signal_ids: RefCell::default(), s: RefCell::default(),
        }))
    }

    fn weak(&self) -> Weak<SenderInner> {
        Rc::downgrade(&self.0)
    }

    fn with(weak: &Weak<SenderInner>) -> Option<ObexSender> {
        weak.upgrade().map(ObexSender)
    }

    pub fn done(&self) -> bool { self.0.s.borrow().done }
    pub fn cancelled(&self) -> bool { self.0.s.borrow().cancelled }
    pub fn error(&self) -> Option<BtError> { self.0.error.borrow().clone() }
    pub fn index(&self) -> usize { self.0.s.borrow().index }
    pub fn set_owns_connection(&self, owns: bool) { self.0.owns_connection.set(owns) }
    pub fn set_on_progress(&self, callback: Option<ProgressFn>) { self.0.on_progress.replace(callback); }

    fn conn(&self) -> Option<Rc<dyn Conn>> {
        self.0.conn.borrow().clone()
    }

    /// Defer the initial work so callers can retain the instance first.
    pub fn start_checked(&self) -> Result<(), &'static str> {
        {
            let mut s = self.0.s.borrow_mut();
            if s.started {
                return Err("This transfer has already been started");
            }
            s.started = true;
        }
        // The idle source keeps the sender alive until it runs (like GLib.idle_add).
        let this = self.clone();
        crate::env::idle_once(move || this.begin());
        Ok(())
    }

    fn finishing_or_done(&self) -> bool {
        let s = self.0.s.borrow();
        s.done || s.finishing
    }

    fn begin(&self) {
        {
            let s = self.0.s.borrow();
            if s.done || s.cancelled {
                return;
            }
        }
        if let Err(error) = self.read_sizes() {
            self.finish(Some(error));
            return;
        }
        self.progress("connecting");
        if self.finishing_or_done() {
            return;
        }
        if self.conn().is_some() {
            self.create_session();
        } else {
            let weak = self.weak();
            (self.0.connector)(Box::new(move |result| {
                if let Some(this) = Self::with(&weak) {
                    this.bus_ready(result);
                } else if let Ok(conn) = result {
                    conn.close(Box::new(|| {}));
                }
            }));
        }
    }

    fn read_sizes(&self) -> Result<(), BtError> {
        if self.0.source.is_empty() || self.0.destination.is_empty() {
            return Err(BtError::runtime("The adapter or device address is unavailable"));
        }
        if self.0.files.is_empty() {
            return Err(BtError::runtime("Select a file to send"));
        }
        for path in &self.0.files {
            if !Path::new(path).is_file() {
                return Err(BtError::runtime(&format!("Not a regular file: {path}")));
            }
            let size = std::fs::File::open(path).and_then(|f| f.metadata())
                .map_err(|e| BtError::runtime(&tessel_core::pyerr::os_error_path(&e, Path::new(path))))?.len();
            self.0.s.borrow_mut().sizes.push(size);
        }
        Ok(())
    }

    fn bus_ready(&self, result: Result<Rc<dyn Conn>, BtError>) {
        let conn = match result {
            Ok(conn) => conn,
            Err(error) => {
                self.finish(Some(error));
                return;
            }
        };
        if self.done() {
            conn.close(Box::new(|| {}));
            return;
        }
        self.0.conn.replace(Some(conn.clone()));
        // A private connection also releases sessions if RemoveSession fails
        // or CreateSession times out without returning the allocated path.
        conn.set_exit_on_close(false);
        let weak = self.weak();
        conn.connect_closed(Box::new(move |error| {
            if let Some(this) = Self::with(&weak) {
                this.connection_closed(error);
            }
        }));
        self.create_session();
    }

    fn connection_closed(&self, error: Option<BtError>) {
        if self.finishing_or_done() {
            return;
        }
        {
            let mut s = self.0.s.borrow_mut();
            s.session = None;
            s.creating = false;
        }
        self.finish(Some(error.unwrap_or_else(|| BtError::runtime("The session bus connection closed"))));
    }

    fn call(&self, path: &str, interface: &str, method: &str, params: Variant, callback: impl FnOnce(&ObexSender, Reply) + 'static) {
        let Some(conn) = self.conn() else { return };
        let this = self.clone();
        conn.call(OBEX, path, interface, method, Some(params), 60000, Box::new(move |reply| callback(&this, reply)));
    }

    fn create_session(&self) {
        if self.done() {
            return;
        }
        let Some(conn) = self.conn() else { return };
        let ids = [
            self.subscribe(&conn, Some(OBEX), Some(PROPERTIES), Some("PropertiesChanged"), None, Some(TRANSFER),
                           |this, s| this.properties_changed(s)),
            self.subscribe(&conn, Some(OBEX), Some(OBJECT_MANAGER), Some("InterfacesRemoved"), None, None,
                           |this, s| this.interfaces_removed(s)),
            self.subscribe(&conn, Some(DBUS), Some(DBUS), Some("NameOwnerChanged"), Some(DBUS_PATH), Some(OBEX),
                           |this, s| this.owner_changed(s)),
        ];
        self.0.signal_ids.replace(ids.to_vec());
        self.0.s.borrow_mut().creating = true;
        let args = vardict(&[("Target", "opp".to_variant()), ("Source", self.0.source.to_variant())]);
        let params = Variant::tuple_from_iter([self.0.destination.to_variant(), args]);
        self.call(ROOT, CLIENT, "CreateSession", params, |this, reply| this.session_ready(reply));
    }

    fn subscribe(&self, conn: &Rc<dyn Conn>, sender: Option<&str>, iface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: fn(&ObexSender, &Signal)) -> u32 {
        let weak = self.weak();
        conn.subscribe(sender, iface, member, path, arg0, Rc::new(move |signal| {
            if let Some(this) = Self::with(&weak) {
                handler(&this, signal);
            }
        }))
    }

    fn session_ready(&self, reply: Reply) {
        self.0.s.borrow_mut().creating = false;
        let result = match reply {
            Err(error) => {
                let cancelled = self.cancelled();
                self.finish(if cancelled { None } else { Some(error) });
                return;
            }
            Ok(result) => result,
        };
        let session = result.child_value(0).str().unwrap_or_default().to_owned();
        let stop = {
            let mut s = self.0.s.borrow_mut();
            s.session = Some(session);
            s.cancelled || s.done
        };
        if stop {
            // A cancellation can arrive while CreateSession is pending.
            self.cleanup();
            return;
        }
        self.send_next();
    }

    fn send_next(&self) {
        let file = {
            let mut s = self.0.s.borrow_mut();
            if s.index == self.0.files.len() {
                None
            } else {
                s.transfer = None;
                s.early_props.clear();
                s.early_removed.clear();
                s.props = Props::new();
                Some(self.0.files[s.index].clone())
            }
        };
        let Some(file) = file else {
            self.finish(None);
            return;
        };
        self.progress("queued");
        if self.finishing_or_done() {
            return;
        }
        let session = self.0.s.borrow().session.clone().unwrap_or_default();
        self.call(&session, PUSH, "SendFile", (file,).to_variant(), |this, reply| this.transfer_ready(reply));
    }

    fn transfer_ready(&self, reply: Reply) {
        if self.finishing_or_done() {
            return;
        }
        let result = match reply {
            Err(error) => {
                self.finish(Some(error));
                return;
            }
            Ok(result) => result,
        };
        let path = result.child_value(0).str().unwrap_or_default().to_owned();
        let mut props = crate::backend::props_of(&result.child_value(1));
        let vanished = {
            let mut s = self.0.s.borrow_mut();
            s.transfer = Some(path.clone());
            // Signals may already have arrived before SendFile's reply: keep
            // the newest values, including a terminal status.
            if let Some(early) = s.early_props.remove(&path) {
                props.extend(early);
            }
            s.early_removed.contains(&path) && props_text(&props, "Status").as_deref() != Some("complete")
        };
        if vanished {
            self.finish(Some(BtError::runtime("The file transfer disappeared")));
            return;
        }
        self.update(props);
    }

    fn properties_changed(&self, signal: &Signal) {
        let session = {
            let s = self.0.s.borrow();
            if s.done || s.finishing {
                return;
            }
            match &s.session {
                Some(session) if !session.is_empty() => session.clone(),
                _ => return,
            }
        };
        let params = &signal.params;
        if params.n_children() < 3 {
            return;
        }
        let iface = params.child_value(0).str().unwrap_or_default().to_owned();
        let props = crate::backend::props_of(&params.child_value(1));
        let invalidated = params.child_value(2).n_children() > 0;
        if iface != TRANSFER || !signal.path.starts_with(&format!("{session}/")) {
            return;
        }
        let transfer = self.0.s.borrow().transfer.clone();
        if transfer.as_deref() == Some(signal.path.as_str()) {
            self.update(props);
            let still = self.0.s.borrow().transfer.as_deref() == Some(signal.path.as_str());
            if invalidated && still && !self.finishing_or_done() {
                let path = signal.path.clone();
                self.call(&signal.path, PROPERTIES, "GetAll", (TRANSFER,).to_variant(), move |this, reply| {
                    if this.0.s.borrow().transfer.as_deref() != Some(path.as_str()) || this.finishing_or_done() {
                        return;
                    }
                    match reply {
                        Err(error) => this.finish(Some(error)),
                        Ok(result) => this.update(first_props(&result)),
                    }
                });
            }
        } else if transfer.is_none() {
            self.0.s.borrow_mut().early_props.entry(signal.path.clone()).or_default().extend(props);
        }
    }

    fn interfaces_removed(&self, signal: &Signal) {
        if self.finishing_or_done() {
            return;
        }
        let (path, _, interfaces) = crate::backend::object_signal(&Signal { member: "InterfacesRemoved".into(), ..signal.clone() });
        let (session, transfer) = {
            let s = self.0.s.borrow();
            (s.session.clone(), s.transfer.clone())
        };
        if session.as_deref() == Some(path.as_str()) {
            self.0.s.borrow_mut().session = None;
            self.finish(Some(BtError::runtime("The Bluetooth file transfer session closed")));
        } else if interfaces.iter().any(|i| i == TRANSFER)
            && session.as_ref().is_some_and(|s| !s.is_empty() && path.starts_with(&format!("{s}/"))) {
            if transfer.as_deref() == Some(path.as_str()) {
                self.finish(Some(BtError::runtime("The file transfer disappeared")));
            } else if transfer.is_none() {
                self.0.s.borrow_mut().early_removed.insert(path);
            }
        }
    }

    fn owner_changed(&self, signal: &Signal) {
        let Some((name, old, new)) = signal.params.get::<(String, String, String)>() else { return };
        if name == OBEX && !old.is_empty() && old != new && !self.done() {
            {
                let mut s = self.0.s.borrow_mut();
                s.session = None;
                s.creating = false;
            }
            self.finish(Some(BtError::runtime("The Bluetooth file transfer service stopped")));
        }
    }

    fn update(&self, props: Props) {
        let status = {
            let mut s = self.0.s.borrow_mut();
            s.props.extend(props);
            props_text(&s.props, "Status").unwrap_or_else(|| "queued".into())
        };
        self.progress(&status);
        if self.finishing_or_done() {
            return;
        }
        if status == "complete" {
            {
                let mut s = self.0.s.borrow_mut();
                let size = s.sizes[s.index];
                s.completed += size;
                s.index += 1;
            }
            self.send_next();
        } else if status == "error" {
            self.finish(Some(BtError::runtime("The device rejected the file or the transfer failed")));
        }
    }

    fn progress(&self, status: &str) {
        let callback = self.0.on_progress.borrow().clone();
        let Some(callback) = callback else { return };
        let info = {
            let s = self.0.s.borrow();
            if s.done || s.finishing || s.index >= self.0.files.len() {
                return;
            }
            Progress {
                filename: self.0.files[s.index].clone(),
                index: s.index + 1,
                count: self.0.files.len(),
                transferred: props_u64(&s.props, "Transferred").unwrap_or(0),
                size: props_u64(&s.props, "Size").unwrap_or(s.sizes[s.index]),
                completed: s.completed,
                total: s.sizes.iter().sum(),
                status: status.to_owned(),
            }
        };
        callback(&info);
    }

    fn finish(&self, error: Option<BtError>) {
        {
            let mut s = self.0.s.borrow_mut();
            if s.done || s.finishing {
                return;
            }
            s.finishing = true;
        }
        self.0.error.replace(error);
        self.cleanup();
    }

    fn cleanup(&self) {
        // RemoveSession also aborts transfers, including one whose SendFile
        // reply has not arrived. Keep the payload alive until this completes.
        let session = {
            let mut s = self.0.s.borrow_mut();
            let session = s.session.take().filter(|s| !s.is_empty());
            if session.is_some() {
                s.finishing = true;
            }
            session
        };
        match session {
            Some(session) => self.call(ROOT, CLIENT, "RemoveSession", object_tuple(&session), |this, reply| {
                if let Err(error) = reply {
                    if this.0.error.borrow().is_none() && !this.cancelled() {
                        this.0.error.replace(Some(error));
                    }
                }
                this.complete();
            }),
            None => self.complete(),
        }
    }

    fn complete(&self) {
        {
            let mut s = self.0.s.borrow_mut();
            if s.done {
                return;
            }
            s.done = true;
        }
        let conn = self.conn();
        if let Some(conn) = &conn {
            for id in self.0.signal_ids.take() {
                conn.unsubscribe(id);
            }
        }
        match conn {
            Some(conn) if self.0.owns_connection.get() && !conn.is_closed() => {
                let this = self.clone();
                conn.close(Box::new(move || this.notify_done()));
            }
            _ => self.notify_done(),
        }
    }

    fn notify_done(&self) {
        let callback = self.0.on_done.take();
        if let Some(callback) = callback {
            let error = self.0.error.borrow().clone();
            callback(error, self.cancelled());
        }
    }
}

impl Sender for ObexSender {
    fn start(&self) {
        let _ = self.start_checked();
    }

    fn cancel(&self) {
        {
            let mut s = self.0.s.borrow_mut();
            if s.done || s.finishing {
                return;
            }
            s.cancelled = true;
            // Never cancel CreateSession's D-Bus request: its late reply is
            // needed to remove the session it may have created meanwhile.
            if s.creating {
                return;
            }
        }
        self.finish(None);
    }
}

// ------------------------------------------------------------- receiving

/// Reduce a remote name to one file name that cannot leave its folder.
pub fn safe_filename(name: &str) -> String {
    let replaced = name.replace('\\', "/");
    let base = replaced.rsplit('/').next().unwrap_or("");
    let filtered: String = base.chars().filter(|&ch| ch >= ' ' && ch != '\x7f').collect();
    let mut name = pyjson::py_strip(&filtered).to_owned();
    if name.is_empty() || name == "." || name == ".." {
        name = "file".into();
    }
    if let Some(rest) = name.strip_prefix('.') {
        // Do not let a sender create hidden files the user will not notice.
        name = format!("_{rest}");
    }
    name.chars().take(200).collect()
}

/// `os.path.splitext` for a bare file name.
pub fn splitext(name: &str) -> (&str, &str) {
    if let Some(dot) = name.rfind('.') {
        if name[..dot].chars().any(|c| c != '.') {
            return (&name[..dot], &name[dot..]);
        }
    }
    (name, "")
}

/// A path in `folder` that does not overwrite an existing file.
pub fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let name = safe_filename(name);
    let (base, ext) = splitext(&name);
    let mut candidate = folder.join(&name);
    let mut number = 1;
    while candidate.symlink_metadata().is_ok() {
        candidate = folder.join(format!("{base} ({number}){ext}"));
        number += 1;
    }
    candidate
}

/// Prompt/notification bookkeeping (`request.user_data`).
#[derive(Default)]
pub struct UserData {
    pub notification: Option<u32>,
    pub percent: Option<i64>,
    pub last_update: Option<i64>,
    /// A fallback dialog, closed by `close()`.
    pub dialog: Option<Rc<dyn PromptHandle>>,
}

pub trait PromptHandle {
    fn alive(&self) -> bool;
    fn destroy(&self);
}

/// One incoming Object Push; `accept`/`reject` answer AuthorizePush.
/// `state`: loading, pending, active, complete, failed, rejected, cancelled.
pub struct ReceiveRequest {
    receiver: Weak<ReceiverInner>,
    invocation: RefCell<Option<Invocation>>,
    pub transfer: String,
    pub name: RefCell<String>,
    pub size: Cell<u64>,
    pub address: RefCell<String>,
    pub mime: RefCell<String>,
    pub path: RefCell<Option<PathBuf>>,
    pub transferred: Cell<u64>,
    pub state: RefCell<String>,
    pub error: RefCell<Option<BtError>>,
    pub user_data: RefCell<UserData>,
}

impl ReceiveRequest {
    /// A request outside any receiver (tests and the UI's fakes).
    pub fn detached(name: &str, size: u64, address: &str) -> Rc<Self> {
        let request = Self::new(Weak::new(), None, "");
        *request.name.borrow_mut() = name.into();
        request.size.set(size);
        *request.address.borrow_mut() = address.into();
        *request.state.borrow_mut() = "pending".into();
        request
    }

    fn new(receiver: Weak<ReceiverInner>, invocation: Option<Invocation>, transfer: &str) -> Rc<Self> {
        Rc::new(Self { receiver, invocation: RefCell::new(invocation), transfer: transfer.into(), name: RefCell::default(),
                       size: Cell::new(0), address: RefCell::default(), mime: RefCell::default(), path: RefCell::new(None),
                       transferred: Cell::new(0), state: RefCell::new("loading".into()), error: RefCell::new(None),
                       user_data: RefCell::default() })
    }

    pub fn state(&self) -> String {
        self.state.borrow().clone()
    }

    pub fn set_state(&self, state: &str) {
        *self.state.borrow_mut() = state.into();
    }

    /// Reserve a unique file in `folder` and answer with its path.
    pub fn accept(&self, folder: &Path) -> Option<PathBuf> {
        if self.state() != "pending" {
            return None;
        }
        let reserved = std::fs::create_dir_all(folder).map_err(|e| tessel_core::pyerr::os_error_path(&e, folder)).and_then(|_| {
            let path = unique_path(folder, &self.name.borrow());
            // Reserve the name now so a second transfer cannot pick it too.
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)
                .map(|_| path.clone()).map_err(|e| tessel_core::pyerr::os_error_path(&e, &path))
        });
        match reserved {
            Err(text) => {
                self.reject();
                self.error.replace(Some(BtError::runtime(&text)));
                None
            }
            Ok(path) => {
                self.path.replace(Some(path.clone()));
                self.set_state("active");
                if let Some(invocation) = self.invocation.take() {
                    invocation.return_value(Some((path.to_string_lossy().into_owned(),).to_variant()));
                }
                Some(path)
            }
        }
    }

    pub fn reject(&self) {
        let state = self.state();
        if state != "pending" && state != "loading" {
            return;
        }
        self.set_state("rejected");
        if let Some(invocation) = self.invocation.take() {
            invocation.return_dbus_error(REJECTED, "Not Authorized");
        }
        if let Some(receiver) = self.receiver.upgrade() {
            receiver.requests.borrow_mut().remove(&self.transfer);
        }
    }

    fn drop_invocation(&self) {
        self.invocation.take();
    }
}

pub type RequestFn = Rc<dyn Fn(&Rc<ReceiveRequest>)>;
pub type StatusFn = Rc<dyn Fn(bool, Option<BtError>)>;

/// Callbacks of [`ObexReceiver`].
#[derive(Clone, Default)]
pub struct ReceiverCallbacks {
    pub on_request: Option<RequestFn>,
    pub on_cancel: Option<RequestFn>,
    pub on_progress: Option<RequestFn>,
    pub on_done: Option<RequestFn>,
    pub on_status: Option<StatusFn>,
}

pub struct ReceiverInner {
    callbacks: ReceiverCallbacks,
    conn: RefCell<Option<Rc<dyn Conn>>>,
    pub path: String,
    pub registered: Cell<bool>,
    pub running: Cell<bool>,
    pub owner: RefCell<Option<String>>,
    registering: Cell<bool>,
    object_id: Cell<u32>,
    signal_ids: RefCell<Vec<u32>>,
    requests: RefCell<HashMap<String, Rc<ReceiveRequest>>>,
    generation: Cell<u64>,
}

/// Anything that serves incoming pushes (tests use fakes).
pub trait Receiver {
    fn start(&self);
    fn stop(&self);
}

/// Session-bus `org.bluez.obex.Agent1` that asks before each push. Only calls
/// from the current `org.bluez.obex` owner are served.
#[derive(Clone)]
pub struct ObexReceiver(pub Rc<ReceiverInner>);

impl ObexReceiver {
    pub fn new(callbacks: ReceiverCallbacks, connection: Option<Rc<dyn Conn>>, path: Option<&str>) -> Self {
        Self(Rc::new(ReceiverInner {
            callbacks, conn: RefCell::new(connection), path: path.unwrap_or(AGENT_PATH).into(), registered: Cell::new(false),
            running: Cell::new(false), owner: RefCell::new(None), registering: Cell::new(false), object_id: Cell::new(0),
            signal_ids: RefCell::default(), requests: RefCell::default(), generation: Cell::new(0),
        }))
    }

    fn weak(&self) -> Weak<ReceiverInner> {
        Rc::downgrade(&self.0)
    }

    fn with(weak: &Weak<ReceiverInner>) -> Option<Self> {
        weak.upgrade().map(Self)
    }

    fn conn(&self) -> Option<Rc<dyn Conn>> {
        self.0.conn.borrow().clone()
    }

    pub fn requests(&self) -> Vec<Rc<ReceiveRequest>> {
        self.0.requests.borrow().values().cloned().collect()
    }

    fn status(&self, registered: bool, error: Option<BtError>) {
        if let Some(callback) = &self.0.callbacks.on_status {
            callback(registered, error);
        }
    }

    fn call(&self, bus: &str, path: &str, interface: &str, method: &str, params: Variant, callback: impl FnOnce(&Self, Reply) + 'static) {
        let Some(conn) = self.conn() else { return };
        let this = self.clone();
        conn.call(bus, path, interface, method, Some(params), 30000, Box::new(move |reply| callback(&this, reply)));
    }

    fn agent_path_variant(&self) -> Variant {
        object_tuple(&self.0.path)
    }

    fn setup(&self) {
        let Some(conn) = self.conn() else { return };
        let weak = self.weak();
        let registered = conn.register_object(&self.0.path, AGENT_XML, Rc::new(move |invocation| {
            match Self::with(&weak) {
                Some(this) => this.method_call(invocation),
                None => invocation.return_dbus_error(REJECTED, "Not Authorized"),
            }
        }));
        match registered {
            Err(error) => {
                self.0.running.set(false);
                self.status(false, Some(error));
                return;
            }
            Ok(id) => self.0.object_id.set(id),
        }
        let (w1, w2) = (self.weak(), self.weak());
        let ids = vec![
            conn.subscribe(Some(OBEX), Some(PROPERTIES), Some("PropertiesChanged"), None, Some(TRANSFER), Rc::new(move |s| {
                if let Some(this) = Self::with(&w1) { this.properties_changed(s) }
            })),
            conn.subscribe(Some(DBUS), Some(DBUS), Some("NameOwnerChanged"), Some(DBUS_PATH), Some(OBEX), Rc::new(move |s| {
                if let Some(this) = Self::with(&w2) { this.owner_changed(s) }
            })),
        ];
        self.0.signal_ids.replace(ids);
        self.register();
    }

    fn register(&self) {
        let generation = self.0.generation.get();
        self.0.registering.set(true);
        // RegisterAgent also D-Bus-activates obexd, which must run to listen.
        self.call(OBEX, ROOT, AGENT_MANAGER, "RegisterAgent", self.agent_path_variant(), move |this, reply| {
            if generation != this.0.generation.get() || !this.0.running.get() {
                if reply.is_ok() && this.0.running.get() {
                    // A restart raced this reply: the same path is registered.
                    this.0.registered.set(true);
                    this.lookup_owner();
                } else if reply.is_ok() && this.conn().is_some() {
                    this.call(OBEX, ROOT, AGENT_MANAGER, "UnregisterAgent", this.agent_path_variant(), |_, _| {});
                }
                return;
            }
            this.0.registering.set(false);
            if let Err(error) = reply {
                if !this.0.registered.get() {
                    this.status(false, Some(error));
                }
                return;
            }
            this.0.registered.set(true);
            this.lookup_owner();
        });
    }

    fn lookup_owner(&self) {
        let generation = self.0.generation.get();
        self.call(DBUS, DBUS_PATH, DBUS, "GetNameOwner", (OBEX,).to_variant(), move |this, reply| {
            if generation != this.0.generation.get() || !this.0.running.get() {
                return;
            }
            let (owner, error) = match reply {
                Ok(result) => (result.child_value(0).str().map(str::to_owned), None),
                Err(error) => (None, Some(error)),
            };
            let have = owner.is_some();
            this.0.owner.replace(owner);
            this.status(this.0.registered.get() && have, error);
        });
    }

    fn method_call(&self, invocation: Invocation) {
        let owner = self.0.owner.borrow().clone();
        if !self.0.running.get() || owner.as_deref() != Some(invocation.sender.as_str()) {
            invocation.return_dbus_error(REJECTED, "Not Authorized");
            return;
        }
        match invocation.method.as_str() {
            "AuthorizePush" => {
                let transfer = invocation.params.child_value(0).str().unwrap_or_default().to_owned();
                self.authorize(invocation, &transfer);
            }
            "Cancel" => {
                invocation.return_value(None);
                for request in self.requests() {
                    let state = request.state();
                    if state == "pending" || state == "loading" {
                        self.cancel_request(&request);
                    }
                }
            }
            "Release" => {
                invocation.return_value(None);
                self.0.registered.set(false);
                self.status(false, None);
            }
            other => {
                let other = other.to_owned();
                invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", &other);
            }
        }
    }

    fn cancel_request(&self, request: &Rc<ReceiveRequest>) {
        request.set_state("cancelled");
        request.drop_invocation();
        self.0.requests.borrow_mut().remove(&request.transfer);
        if let Some(callback) = &self.0.callbacks.on_cancel {
            callback(request);
        }
    }

    fn authorize(&self, invocation: Invocation, transfer: &str) {
        let request = ReceiveRequest::new(self.weak(), Some(invocation), transfer);
        self.0.requests.borrow_mut().insert(transfer.to_owned(), request.clone());
        self.call(OBEX, transfer, PROPERTIES, "GetAll", (TRANSFER,).to_variant(), move |this, reply| {
            if request.state() != "loading" {
                return;
            }
            let result = match reply {
                Err(_) => {
                    request.reject();
                    return;
                }
                Ok(result) => result,
            };
            let props = first_props(&result);
            let name = props.get("Name").filter(|v| pyjson::truthy(v)).or_else(|| props.get("Filename"))
                .filter(|v| pyjson::truthy(v)).map(pyjson::py_str).unwrap_or_default();
            *request.name.borrow_mut() = safe_filename(&name);
            request.size.set(props.get("Size").filter(|v| pyjson::truthy(v)).and_then(|v| pyjson::py_int(Some(v)))
                .map(|n| n.max(0) as u64).unwrap_or(0));
            *request.mime.borrow_mut() = props.get("Type").filter(|v| pyjson::truthy(v)).map(pyjson::py_str).unwrap_or_default();
            let session = props.get("Session").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
            let loaded = move |this: &ObexReceiver, request: &Rc<ReceiveRequest>, reply: Option<Reply>| {
                if request.state() != "loading" {
                    return;
                }
                if let Some(Ok(result)) = reply {
                    let props = first_props(&result);
                    *request.address.borrow_mut() = props.get("Destination").map(pyjson::py_str).unwrap_or_default();
                }
                request.set_state("pending");
                if let Some(callback) = &this.0.callbacks.on_request {
                    callback(request);
                }
            };
            match session {
                Some(session) => {
                    let request = request.clone();
                    this.call(OBEX, &session, PROPERTIES, "GetAll", (SESSION,).to_variant(),
                              move |this, reply| loaded(this, &request, Some(reply)));
                }
                None => loaded(this, &request, None),
            }
        });
    }

    fn properties_changed(&self, signal: &Signal) {
        let request = self.0.requests.borrow().get(&signal.path).cloned();
        let Some(request) = request.filter(|r| r.state() == "active") else { return };
        let params = &signal.params;
        if params.n_children() < 2 || params.child_value(0).str() != Some(TRANSFER) {
            return;
        }
        let props = crate::backend::props_of(&params.child_value(1));
        if props.get("Size").is_some_and(pyjson::truthy) {
            request.size.set(props_u64(&props, "Size").unwrap_or(0));
        }
        if props.contains_key("Transferred") {
            request.transferred.set(props_u64(&props, "Transferred").unwrap_or(0));
        }
        match props_text(&props, "Status").as_deref() {
            Some("complete") => {
                request.transferred.set(request.transferred.get().max(request.size.get()));
                self.finish(&request, None);
            }
            Some("error") => self.finish(&request, Some(BtError::runtime("The transfer failed"))),
            _ => {
                if let Some(callback) = &self.0.callbacks.on_progress {
                    callback(&request);
                }
            }
        }
    }

    fn owner_changed(&self, signal: &Signal) {
        let Some((name, old, new)) = signal.params.get::<(String, String, String)>() else { return };
        if name != OBEX || old == new {
            return;
        }
        self.0.owner.replace((!new.is_empty()).then(|| new.clone()));
        if !old.is_empty() {
            self.0.registered.set(false);
            for request in self.requests() {
                match request.state().as_str() {
                    "active" => self.finish(&request, Some(BtError::runtime("The Bluetooth file transfer service stopped"))),
                    "pending" | "loading" => self.cancel_request(&request),
                    _ => {}
                }
            }
            self.status(false, None);
        }
        if !new.is_empty() && self.0.running.get() && !self.0.registered.get() && !self.0.registering.get() {
            // obexd restarted: the agent registration died with it.
            self.register();
        }
    }

    fn finish(&self, request: &Rc<ReceiveRequest>, error: Option<BtError>) {
        self.0.requests.borrow_mut().remove(&request.transfer);
        request.set_state(if error.is_some() { "failed" } else { "complete" });
        if error.is_some() {
            if let Some(path) = request.path.borrow().as_ref() {
                let _ = std::fs::remove_file(path);
            }
        }
        request.error.replace(error);
        if let Some(callback) = &self.0.callbacks.on_done {
            callback(request);
        }
    }
}

impl Receiver for ObexReceiver {
    fn start(&self) {
        if self.0.running.get() {
            return;
        }
        self.0.running.set(true);
        self.0.generation.set(self.0.generation.get() + 1);
        if self.conn().is_some() {
            self.setup();
            return;
        }
        let generation = self.0.generation.get();
        let weak = self.weak();
        gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |result| {
            let Some(this) = Self::with(&weak) else { return };
            match result {
                Err(error) => this.status(false, Some(BtError::from_glib(&error))),
                Ok(connection) => {
                    if this.0.running.get() && generation == this.0.generation.get() {
                        this.0.conn.replace(Some(crate::dbus::GioConn::new(connection)));
                        this.setup();
                    }
                }
            }
        });
    }

    fn stop(&self) {
        if !self.0.running.get() {
            return;
        }
        self.0.running.set(false);
        self.0.registering.set(false);
        self.0.generation.set(self.0.generation.get() + 1);
        for request in self.requests() {
            let state = request.state();
            if state == "pending" || state == "loading" {
                request.reject();
            }
        }
        let Some(conn) = self.conn() else { return };
        if self.0.registered.get() {
            self.call(OBEX, ROOT, AGENT_MANAGER, "UnregisterAgent", self.agent_path_variant(), |_, _| {});
        }
        self.0.registered.set(false);
        for id in self.0.signal_ids.take() {
            conn.unsubscribe(id);
        }
        let object = self.0.object_id.replace(0);
        if object != 0 {
            conn.unregister_object(object);
        }
    }
}

// ------------------------------------------------------------- notifications

/// One `Notifier.notify(...)` call.
#[derive(Default)]
pub struct Notification {
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub on_action: Option<Rc<dyn Fn(&str)>>,
    pub on_closed: Option<Rc<dyn Fn(u32)>>,
    pub replaces: u32,
    pub value: Option<i64>,
    /// Python default 1.
    pub urgency: u8,
    /// Python default -1.
    pub timeout: i32,
    pub on_shown: Option<Box<dyn FnOnce(u32)>>,
    pub on_error: Option<Box<dyn FnOnce(BtError)>>,
}

impl Notification {
    pub fn new(summary: &str, body: &str) -> Self {
        Self { summary: summary.into(), body: body.into(), urgency: 1, timeout: -1, ..Default::default() }
    }
}

pub trait Notify {
    fn supports_actions(&self) -> bool;
    fn notify(&self, notification: Notification);
    fn close(&self, id: u32);
    fn destroy(&self);
}

type Handlers = (Option<Rc<dyn Fn(&str)>>, Option<Rc<dyn Fn(u32)>>);

/// Small org.freedesktop.Notifications client with action callbacks.
pub struct Notifier {
    conn: Rc<dyn Conn>,
    app_name: String,
    icon: String,
    capabilities: RefCell<Option<HashSet<String>>>,
    handlers: RefCell<HashMap<u32, Handlers>>,
    signal_ids: RefCell<Vec<u32>>,
}

pub const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

impl Notifier {
    pub fn new(conn: Rc<dyn Conn>, app_name: &str, icon: Option<&str>) -> Rc<Self> {
        let notifier = Rc::new(Self { conn: conn.clone(), app_name: app_name.into(), icon: icon.unwrap_or("bluetooth-symbolic").into(),
                                      capabilities: RefCell::new(None), handlers: RefCell::default(), signal_ids: RefCell::default() });
        let mut ids = Vec::new();
        for member in ["ActionInvoked", "NotificationClosed"] {
            let weak = Rc::downgrade(&notifier);
            ids.push(conn.subscribe(Some(NOTIFICATIONS), Some(NOTIFICATIONS), Some(member), Some(NOTIFICATIONS_PATH), None,
                                    Rc::new(move |signal| if let Some(n) = weak.upgrade() { n.signal(signal) })));
        }
        notifier.signal_ids.replace(ids);
        let weak = Rc::downgrade(&notifier);
        notifier.call("GetCapabilities", None, Box::new(move |reply| {
            if let Some(n) = weak.upgrade() {
                let set = match reply {
                    Ok(result) => result.child_value(0).iter().filter_map(|v| v.str().map(str::to_owned)).collect(),
                    Err(_) => HashSet::new(),
                };
                n.capabilities.replace(Some(set));
            }
        }));
        notifier
    }

    fn call(&self, method: &str, params: Option<Variant>, done: Box<dyn FnOnce(Reply)>) {
        self.conn.call(NOTIFICATIONS, NOTIFICATIONS_PATH, NOTIFICATIONS, method, params, 10000, done);
    }

    pub fn capabilities(&self) -> Option<HashSet<String>> {
        self.capabilities.borrow().clone()
    }

    fn signal(&self, signal: &Signal) {
        let params = &signal.params;
        if params.n_children() < 2 {
            return;
        }
        let Some(id) = params.child_value(0).get::<u32>() else { return };
        let handlers = self.handlers.borrow().get(&id).cloned();
        let Some((on_action, on_closed)) = handlers else { return };
        if signal.member == "ActionInvoked" {
            if let (Some(callback), Some(key)) = (on_action, params.child_value(1).str()) {
                callback(key);
            }
        } else {
            self.handlers.borrow_mut().remove(&id);
            if let Some(callback) = on_closed {
                callback(params.child_value(1).get::<u32>().unwrap_or(0));
            }
        }
    }
}

impl Notify for Rc<Notifier> {
    /// Unknown until the server answers: assume the common case.
    fn supports_actions(&self) -> bool {
        self.capabilities.borrow().as_ref().is_none_or(|c| c.contains("actions"))
    }

    fn notify(&self, n: Notification) {
        let flat: Vec<String> = n.actions.iter().flat_map(|(k, l)| [k.clone(), l.clone()]).collect();
        let mut hints = vec![("urgency", n.urgency.to_variant()), ("desktop-entry", "tessel-bluetooth".to_variant())];
        if let Some(value) = n.value {
            hints.push(("value", (value as i32).to_variant()));
        }
        let params = Variant::tuple_from_iter([
            self.app_name.to_variant(), n.replaces.to_variant(), self.icon.to_variant(), n.summary.to_variant(),
            n.body.to_variant(), flat.to_variant(), vardict(&hints), n.timeout.to_variant(),
        ]);
        let weak = Rc::downgrade(self);
        let (on_action, on_closed, on_shown, on_error) = (n.on_action, n.on_closed, n.on_shown, n.on_error);
        self.call("Notify", Some(params), Box::new(move |reply| {
            let ident = match reply {
                Err(error) => {
                    if let Some(callback) = on_error {
                        callback(error);
                    }
                    return;
                }
                Ok(result) => result.child_value(0).get::<u32>().unwrap_or(0),
            };
            if let Some(notifier) = weak.upgrade() {
                if on_action.is_some() || on_closed.is_some() {
                    notifier.handlers.borrow_mut().insert(ident, (on_action, on_closed));
                } else {
                    notifier.handlers.borrow_mut().remove(&ident);
                }
            }
            if let Some(callback) = on_shown {
                callback(ident);
            }
        }));
    }

    fn close(&self, id: u32) {
        if id != 0 {
            self.handlers.borrow_mut().remove(&id);
            self.call("CloseNotification", Some((id,).to_variant()), Box::new(|_| {}));
        }
    }

    fn destroy(&self) {
        for id in self.signal_ids.take() {
            self.conn.unsubscribe(id);
        }
        self.handlers.borrow_mut().clear();
    }
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
