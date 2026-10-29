//! D-Bus transport used by the backend: gio on the GLib main loop (no extra
//! executor thread), behind a trait so the policy can be tested with deferred
//! fake replies exactly like the Python `FakeConnection` tests. Same shape as
//! power-manager's `bus.rs` (minus the logind fd call) for later upstreaming.

use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

#[derive(Clone, Debug, PartialEq)]
pub struct BusError {
    /// Remote D-Bus error name (`Gio.DBusError.get_remote_error`), if any.
    pub name: Option<String>,
    /// Message with the remote prefix stripped (`Gio.DBusError.strip_remote_error`).
    pub message: String,
}

impl BusError {
    pub fn local(message: &str) -> Self {
        Self { name: None, message: message.into() }
    }

    pub fn remote(name: &str, message: &str) -> Self {
        Self { name: Some(name.into()), message: message.into() }
    }

    pub fn from_glib(error: &glib::Error) -> Self {
        let name = gio::DBusError::remote_error(error).map(|n| n.to_string());
        let mut stripped = error.clone();
        gio::DBusError::strip_remote_error(&mut stripped);
        Self { name, message: stripped.message().to_owned() }
    }
}

pub type Reply = Result<Variant, BusError>;

#[derive(Clone, Debug)]
pub struct Call {
    pub name: String,
    pub path: String,
    pub interface: String,
    pub method: String,
    pub params: Option<Variant>,
    /// `ALLOW_INTERACTIVE_AUTHORIZATION` (polkit may prompt).
    pub interactive: bool,
    /// Mutations are never cancelled and get a 120 s timeout (reads: 10 s, cancellable).
    pub mutation: bool,
}

impl Call {
    pub fn new(name: &str, path: &str, interface: &str, method: &str, params: Option<Variant>) -> Self {
        Self { name: name.into(), path: path.into(), interface: interface.into(), method: method.into(), params,
               interactive: false, mutation: false }
    }

    pub fn mutation(mut self) -> Self {
        self.interactive = true;
        self.mutation = true;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Signal {
    pub sender: String,
    pub path: String,
    pub interface: String,
    pub member: String,
    pub params: Variant,
}

pub type SignalHandler = Rc<dyn Fn(&Signal)>;

pub trait Bus {
    fn call(&self, call: Call, done: Box<dyn FnOnce(Reply)>);
    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32;
    fn unsubscribe(&self, id: u32);
    /// Cancel every in-flight read (mutations continue) and arm a fresh cancellable.
    fn cancel_reads(&self);
    /// `connection.connect("closed")`; `None` disconnects.
    fn on_closed(&self, handler: Option<Box<dyn Fn()>>);
}

/// gio implementation.
pub struct GioBus {
    connection: gio::DBusConnection,
    cancel: RefCell<gio::Cancellable>,
    subscriptions: RefCell<Vec<(u32, gio::SignalSubscriptionId)>>,
    next: Cell<u32>,
    closed: RefCell<Option<glib::SignalHandlerId>>,
}

impl GioBus {
    pub fn new(connection: gio::DBusConnection) -> Rc<Self> {
        connection.set_exit_on_close(false);
        Rc::new(Self { connection, cancel: RefCell::new(gio::Cancellable::new()), subscriptions: RefCell::new(Vec::new()),
                       next: Cell::new(0), closed: RefCell::new(None) })
    }

    pub fn connection(&self) -> &gio::DBusConnection {
        &self.connection
    }
}

/// Asynchronously get a bus (`Gio.bus_get`) wrapped as a [`Bus`]. Honours
/// `DBUS_SYSTEM_BUS_ADDRESS`, which the private-bus tests use.
pub fn connect(kind: gio::BusType, cancellable: Option<&gio::Cancellable>, done: impl FnOnce(Result<Rc<dyn Bus>, BusError>) + 'static) {
    gio::bus_get(kind, cancellable, move |result| match result {
        Ok(connection) => done(Ok(GioBus::new(connection) as Rc<dyn Bus>)),
        Err(error) => done(Err(BusError::from_glib(&error))),
    });
}

impl Bus for GioBus {
    fn call(&self, call: Call, done: Box<dyn FnOnce(Reply)>) {
        let cancel = if call.mutation { None } else { Some(self.cancel.borrow().clone()) };
        let flags = if call.interactive { gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION } else { gio::DBusCallFlags::NONE };
        self.connection.call(Some(&call.name), &call.path, &call.interface, &call.method, call.params.as_ref(), None,
                             flags, if call.mutation { 120000 } else { 10000 }, cancel.as_ref(),
                             move |result| done(result.map_err(|e| BusError::from_glib(&e))));
    }

    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32 {
        #[allow(deprecated)]
        let id = self.connection.signal_subscribe(sender, interface, member, path, arg0, gio::DBusSignalFlags::NONE,
            move |_, sender, path, interface, member, params| {
                handler(&Signal { sender: sender.to_owned(), path: path.to_owned(),
                                  interface: interface.to_owned(), member: member.to_owned(), params: params.clone() });
            });
        let key = self.next.get() + 1;
        self.next.set(key);
        self.subscriptions.borrow_mut().push((key, id));
        key
    }

    fn unsubscribe(&self, key: u32) {
        let removed = {
            let mut subscriptions = self.subscriptions.borrow_mut();
            subscriptions.iter().position(|entry| entry.0 == key).map(|index| subscriptions.remove(index).1)
        };
        if let Some(id) = removed {
            #[allow(deprecated)]
            self.connection.signal_unsubscribe(id);
        }
    }

    fn cancel_reads(&self) {
        let old = self.cancel.replace(gio::Cancellable::new());
        old.cancel();
    }

    fn on_closed(&self, handler: Option<Box<dyn Fn()>>) {
        if let Some(id) = self.closed.take() {
            self.connection.disconnect(id);
        }
        if let Some(handler) = handler {
            let handler = glib::thread_guard::ThreadGuard::new(handler);
            let id = self.connection.connect_closed(move |_, _, _| (handler.get_ref())());
            self.closed.replace(Some(id));
        }
    }
}

type Pending = (Call, Option<Box<dyn FnOnce(Reply)>>);
type Subscription = (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, SignalHandler);

/// Deferred fake replies (port of the Python tests' `FakeConnection`).
#[derive(Default)]
pub struct FakeBus {
    pub calls: RefCell<Vec<Call>>,
    pub pending: RefCell<Vec<Pending>>,
    pub signals: RefCell<Vec<Subscription>>,
    pub unsubscribed: RefCell<Vec<u32>>,
    pub cancelled: Cell<u32>,
    pub closed_handler: RefCell<Option<Box<dyn Fn()>>>,
}

impl FakeBus {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Reply to the `index`-th pending call matching `method` (and optional path/name).
    pub fn reply(&self, method: &str, result: Reply, path: Option<&str>, name: Option<&str>, index: usize) -> Call {
        let (call, callback) = {
            let mut pending = self.pending.borrow_mut();
            let matches: Vec<usize> = pending.iter().enumerate()
                .filter(|(_, (c, _))| c.method == method && path.is_none_or(|p| p == c.path) && name.is_none_or(|n| n == c.name))
                .map(|(i, _)| i).collect();
            let position = *matches.get(index).unwrap_or_else(|| panic!("no pending {method} {path:?} {name:?} #{index}"));
            pending.remove(position)
        };
        if let Some(callback) = callback {
            callback(result);
        }
        call
    }

    pub fn called(&self, method: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.method == method)
    }

    pub fn last_call(&self) -> Call {
        self.calls.borrow().last().expect("a call").clone()
    }

    pub fn last_pending(&self) -> Call {
        self.pending.borrow().last().expect("a pending call").0.clone()
    }

    pub fn methods(&self) -> std::collections::BTreeSet<String> {
        self.calls.borrow().iter().map(|c| c.method.clone()).collect()
    }
}

impl Bus for FakeBus {
    fn call(&self, call: Call, done: Box<dyn FnOnce(Reply)>) {
        self.calls.borrow_mut().push(call.clone());
        self.pending.borrow_mut().push((call, Some(done)));
    }

    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32 {
        let own = |v: Option<&str>| v.map(String::from);
        let mut signals = self.signals.borrow_mut();
        signals.push((own(sender), own(interface), own(member), own(path), own(arg0), handler));
        signals.len() as u32
    }

    fn unsubscribe(&self, id: u32) {
        self.unsubscribed.borrow_mut().push(id);
    }

    fn cancel_reads(&self) {
        self.cancelled.set(self.cancelled.get() + 1);
    }

    fn on_closed(&self, handler: Option<Box<dyn Fn()>>) {
        self.closed_handler.replace(handler);
    }
}
