//! D-Bus transport used by the backend: gio on the GLib main loop (no extra
//! executor), behind a trait so the policy can be tested with deferred fake
//! replies exactly like the Python FakeConnection tests.

use gio::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::rc::Rc;

pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

#[derive(Clone, Debug, PartialEq)]
pub struct BusError {
    /// Remote D-Bus error name (`Gio.DBusError.get_remote_error`), if any.
    pub name: Option<String>,
    /// Message with the remote prefix stripped.
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
    pub interactive: bool,
    /// Mutations are never cancelled and get a 120 s timeout (reads: 10 s, cancellable).
    pub mutation: bool,
}

impl Call {
    pub fn new(name: &str, path: &str, interface: &str, method: &str, params: Option<Variant>) -> Self {
        Self { name: name.into(), path: path.into(), interface: interface.into(), method: method.into(), params,
               interactive: false, mutation: false }
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
    /// A call whose reply carries one unix fd (`h`): logind `Inhibit`.
    fn call_with_fd(&self, call: Call, done: Box<dyn FnOnce(Result<(Variant, Option<OwnedFd>), BusError>)>);
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
    next: std::cell::Cell<u32>,
    closed: RefCell<Option<glib::SignalHandlerId>>,
}

impl GioBus {
    pub fn new(connection: gio::DBusConnection) -> Rc<Self> {
        connection.set_exit_on_close(false);
        Rc::new(Self { connection, cancel: RefCell::new(gio::Cancellable::new()), subscriptions: RefCell::new(Vec::new()),
                       next: std::cell::Cell::new(0), closed: RefCell::new(None) })
    }

    pub fn connection(&self) -> &gio::DBusConnection {
        &self.connection
    }

    fn flags(call: &Call) -> gio::DBusCallFlags {
        if call.interactive { gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION } else { gio::DBusCallFlags::NONE }
    }
}

/// Asynchronously get a bus (`Gio.bus_get`) wrapped as a [`Bus`].
pub fn connect(kind: gio::BusType, cancellable: Option<&gio::Cancellable>, done: impl FnOnce(Result<Rc<dyn Bus>, BusError>) + 'static) {
    gio::bus_get(kind, cancellable, move |result| match result {
        Ok(connection) => done(Ok(GioBus::new(connection) as Rc<dyn Bus>)),
        Err(error) => done(Err(BusError::from_glib(&error))),
    });
}

#[allow(deprecated)]
impl Bus for GioBus {
    fn call(&self, call: Call, done: Box<dyn FnOnce(Reply)>) {
        let cancel = if call.mutation { None } else { Some(self.cancel.borrow().clone()) };
        self.connection.call(Some(&call.name), &call.path, &call.interface, &call.method, call.params.as_ref(), None,
                             Self::flags(&call), if call.mutation { 120000 } else { 10000 }, cancel.as_ref(),
                             move |result| done(result.map_err(|e| BusError::from_glib(&e))));
    }

    fn call_with_fd(&self, call: Call, done: Box<dyn FnOnce(Result<(Variant, Option<OwnedFd>), BusError>)>) {
        let cancel = self.cancel.borrow().clone();
        self.connection.call_with_unix_fd_list(
            Some(&call.name), &call.path, &call.interface, &call.method, call.params.as_ref(),
            Some(glib::VariantTy::new("(h)").unwrap()), Self::flags(&call), 10000, None::<&gio::UnixFDList>,
            Some(&cancel), move |result| match result {
                Ok((value, fds)) => {
                    let index = value.child_value(0).get::<glib::variant::Handle>().map(|h| h.0).unwrap_or(-1);
                    let fd = fds.and_then(|list| list.get(index).ok());
                    match fd {
                        Some(fd) => done(Ok((value, Some(fd)))),
                        None => done(Err(BusError::local("The reply did not contain a file descriptor."))),
                    }
                }
                Err(error) => done(Err(BusError::from_glib(&error))),
            });
    }

    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32 {
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
        let mut subscriptions = self.subscriptions.borrow_mut();
        if let Some(index) = subscriptions.iter().position(|entry| entry.0 == key) {
            let (_, id) = subscriptions.remove(index);
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
            let id = self.connection.connect_local("closed", false, move |_| {
                handler();
                None
            });
            self.closed.replace(Some(id));
        }
    }
}

/// Deferred fake replies (port of the Python tests' FakeConnection).
#[derive(Default)]
pub struct FakeBus {
    pub calls: RefCell<Vec<Call>>,
    pub pending: RefCell<Vec<(Call, Option<Box<dyn FnOnce(Reply)>>)>>,
    pub fd_calls: RefCell<Vec<(Call, Option<Box<dyn FnOnce(Result<(Variant, Option<OwnedFd>), BusError>)>>)>>,
    pub signals: RefCell<Vec<(Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, SignalHandler)>>,
    pub unsubscribed: RefCell<Vec<u32>>,
    pub cancelled: std::cell::Cell<u32>,
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
            let (call, callback) = pending.remove(position);
            (call, callback)
        };
        if let Some(callback) = callback {
            callback(result);
        }
        call
    }

    pub fn has_pending(&self, method: &str) -> bool {
        self.pending.borrow().iter().any(|(c, _)| c.method == method)
    }

    pub fn called(&self, method: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.method == method)
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

    fn call_with_fd(&self, call: Call, done: Box<dyn FnOnce(Result<(Variant, Option<OwnedFd>), BusError>)>) {
        self.calls.borrow_mut().push(call.clone());
        self.fd_calls.borrow_mut().push((call, Some(done)));
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
