//! D-Bus transport used by btmanager (`compat`: candidate to upstream into
//! `suite_core::bus`). Unlike `suite_core::bus::Bus` it takes the Python
//! per-call timeouts, exports objects (the OBEX agent) and can close the
//! private session connection that outgoing transfers use. `FakeConn` is the
//! port of the Python tests' `FakeConnection` / `AgentConnection`.

use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub use suite_core::bus::Signal;

/// A failed call, keeping what the Python code inspects of `GLib.Error` /
/// `RuntimeError`: the raw message (`err.message`, remote prefix kept),
/// the GDBus error code and the `_obex_unavailable` marker.
#[derive(Clone, Debug, PartialEq)]
pub struct BtError {
    pub message: String,
    pub dbus_code: Option<gio::DBusError>,
    /// `err._obex_unavailable = True` (no session bus).
    pub unavailable: bool,
    /// A `GLib.Error` (as opposed to RuntimeError/OSError/ValueError).
    pub glib: bool,
}

impl BtError {
    pub fn from_glib(error: &glib::Error) -> Self {
        Self { message: error.message().to_owned(), dbus_code: error.kind::<gio::DBusError>(), unavailable: false, glib: true }
    }

    /// `GLib.Error(message)` (no domain).
    pub fn glib(message: &str) -> Self {
        Self { message: message.into(), dbus_code: None, unavailable: false, glib: true }
    }

    /// `RuntimeError(message)` / `ValueError` / `OSError` text.
    pub fn runtime(message: &str) -> Self {
        Self { message: message.into(), dbus_code: None, unavailable: false, glib: false }
    }

    /// `GLib.Error.new_literal(Gio.dbus_error_quark(), message, code)`.
    pub fn dbus(code: gio::DBusError, message: &str) -> Self {
        Self { message: message.into(), dbus_code: Some(code), unavailable: false, glib: true }
    }

    /// What GDBus produces for a remote error reply `name: message`.
    pub fn remote(name: &str, message: &str) -> Self {
        Self::from_glib(&gio::DBusError::new_for_dbus_error(name, message))
    }

    pub fn into_unavailable(mut self) -> Self {
        self.unavailable = true;
        self
    }
}

impl std::fmt::Display for BtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type Reply = Result<Variant, BtError>;
pub type Done = Box<dyn FnOnce(Reply)>;
pub type SignalHandler = Rc<dyn Fn(&Signal)>;

/// An incoming method call on an exported object.
pub struct Invocation {
    pub sender: String,
    pub path: String,
    pub interface: String,
    pub method: String,
    pub params: Variant,
    reply: InvocationReply,
}

enum InvocationReply {
    Gio(gio::DBusMethodInvocation),
    Fake(Rc<RefCell<Option<Result<Option<Variant>, (String, String)>>>>),
}

impl Invocation {
    pub fn return_value(self, value: Option<Variant>) {
        match self.reply {
            InvocationReply::Gio(invocation) => invocation.return_value(value.as_ref()),
            InvocationReply::Fake(slot) => *slot.borrow_mut() = Some(Ok(value)),
        }
    }

    pub fn return_dbus_error(self, name: &str, message: &str) {
        match self.reply {
            InvocationReply::Gio(invocation) => invocation.return_dbus_error(name, message),
            InvocationReply::Fake(slot) => *slot.borrow_mut() = Some(Err((name.into(), message.into()))),
        }
    }
}

pub type MethodHandler = Rc<dyn Fn(Invocation)>;
pub type InvocationSlot = Rc<RefCell<Option<Result<Option<Variant>, (String, String)>>>>;

pub trait Conn {
    fn call(&self, destination: &str, path: &str, interface: &str, method: &str, params: Option<Variant>, timeout_ms: i32, done: Done);
    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32;
    fn unsubscribe(&self, id: u32);
    fn register_object(&self, path: &str, xml: &str, handler: MethodHandler) -> Result<u32, BtError>;
    fn unregister_object(&self, id: u32);
    fn is_closed(&self) -> bool;
    /// Close asynchronously; `done` runs when finished.
    fn close(&self, done: Box<dyn FnOnce()>);
    /// `connection.connect("closed", …)` with the GError, if any.
    fn connect_closed(&self, handler: Box<dyn Fn(Option<BtError>)>);
    fn set_exit_on_close(&self, _exit: bool) {}
}

/// gio implementation.
pub struct GioConn {
    connection: gio::DBusConnection,
    subscriptions: RefCell<Vec<(u32, gio::SignalSubscriptionId)>>,
    objects: RefCell<Vec<(u32, gio::RegistrationId)>>,
    next: Cell<u32>,
}

impl GioConn {
    pub fn new(connection: gio::DBusConnection) -> Rc<Self> {
        Rc::new(Self { connection, subscriptions: RefCell::default(), objects: RefCell::default(), next: Cell::new(0) })
    }

    pub fn connection(&self) -> &gio::DBusConnection {
        &self.connection
    }

    fn key(&self) -> u32 {
        self.next.set(self.next.get() + 1);
        self.next.get()
    }
}

#[allow(deprecated)]
impl Conn for GioConn {
    fn call(&self, destination: &str, path: &str, interface: &str, method: &str, params: Option<Variant>, timeout_ms: i32, done: Done) {
        let params = params.unwrap_or_else(|| ().to_variant());
        self.connection.call(Some(destination), path, interface, method, Some(&params), None, gio::DBusCallFlags::NONE,
                             timeout_ms, None::<&gio::Cancellable>, move |result| done(result.map_err(|e| BtError::from_glib(&e))));
    }

    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32 {
        let id = self.connection.signal_subscribe(sender, interface, member, path, arg0, gio::DBusSignalFlags::NONE,
            move |_, sender, path, interface, member, params| {
                handler(&Signal { sender: sender.to_owned(), path: path.to_owned(), interface: interface.to_owned(),
                                  member: member.to_owned(), params: params.clone() });
            });
        let key = self.key();
        self.subscriptions.borrow_mut().push((key, id));
        key
    }

    fn unsubscribe(&self, key: u32) {
        let entry = {
            let mut subscriptions = self.subscriptions.borrow_mut();
            subscriptions.iter().position(|e| e.0 == key).map(|i| subscriptions.remove(i))
        };
        if let Some((_, id)) = entry {
            self.connection.signal_unsubscribe(id);
        }
    }

    fn register_object(&self, path: &str, xml: &str, handler: MethodHandler) -> Result<u32, BtError> {
        let info = gio::DBusNodeInfo::for_xml(xml).map_err(|e| BtError::from_glib(&e))?;
        let interface = info.interfaces().first().cloned().ok_or_else(|| BtError::runtime("no interface"))?;
        let id = self.connection.register_object(path, &interface)
            .method_call(move |_, sender, path, interface, method, params, invocation| {
                handler(Invocation { sender: sender.unwrap_or("").to_owned(), path: path.to_owned(),
                                     interface: interface.unwrap_or("").to_owned(), method: method.to_owned(), params,
                                     reply: InvocationReply::Gio(invocation) });
            })
            .build().map_err(|e| BtError::from_glib(&e))?;
        let key = self.key();
        self.objects.borrow_mut().push((key, id));
        Ok(key)
    }

    fn unregister_object(&self, key: u32) {
        let entry = {
            let mut objects = self.objects.borrow_mut();
            objects.iter().position(|e| e.0 == key).map(|i| objects.remove(i))
        };
        if let Some((_, id)) = entry {
            let _ = self.connection.unregister_object(id);
        }
    }

    fn is_closed(&self) -> bool {
        self.connection.is_closed()
    }

    fn close(&self, done: Box<dyn FnOnce()>) {
        self.connection.close(None::<&gio::Cancellable>, move |_| done());
    }

    fn connect_closed(&self, handler: Box<dyn Fn(Option<BtError>)>) {
        self.connection.connect_local("closed", false, move |args| {
            let error = args.get(2).and_then(|v| v.get::<Option<glib::Error>>().ok()).flatten();
            handler(error.as_ref().map(BtError::from_glib));
            None
        });
    }

    fn set_exit_on_close(&self, exit: bool) {
        self.connection.set_exit_on_close(exit);
    }
}

/// A recorded call: `(destination, path, interface, method, params)`.
#[derive(Clone, Debug)]
pub struct CallRecord {
    pub destination: String,
    pub path: String,
    pub interface: String,
    pub method: String,
    pub params: Option<Variant>,
    pub timeout: i32,
}

impl CallRecord {
    /// The first parameter as a string (`params.unpack()[0]`).
    pub fn arg_str(&self, index: usize) -> Option<String> {
        let params = self.params.as_ref()?;
        if index >= params.n_children() {
            return None;
        }
        let child = params.child_value(index);
        child.str().map(str::to_owned)
    }
}

type Responder = Box<dyn Fn(&CallRecord) -> Option<Reply>>;
type FakeSignal = (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, SignalHandler);

/// Deferred replies, signal emission and exported objects on no bus at all.
#[derive(Default)]
pub struct FakeConn {
    pub calls: RefCell<Vec<CallRecord>>,
    pub pending: RefCell<Vec<(CallRecord, Option<Done>)>>,
    pub signals: RefCell<Vec<(u32, FakeSignal)>>,
    pub objects: RefCell<Vec<(u32, String, MethodHandler)>>,
    next: Cell<u32>,
    /// Answer some calls synchronously (Python `side_effect` fakes).
    pub responder: RefCell<Option<Responder>>,
    pub closed: Cell<bool>,
    pub close_requests: Cell<u32>,
    pub pending_close: RefCell<Option<Box<dyn FnOnce()>>>,
    pub closed_handlers: RefCell<Vec<Box<dyn Fn(Option<BtError>)>>>,
}

impl FakeConn {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn with_responder(responder: impl Fn(&CallRecord) -> Option<Reply> + 'static) -> Rc<Self> {
        let conn = Self::new();
        conn.responder.replace(Some(Box::new(responder)));
        conn
    }

    fn key(&self) -> u32 {
        self.next.set(self.next.get() + 1);
        self.next.get()
    }

    /// Reply to the first pending call named `method`.
    pub fn reply(&self, method: &str, result: Reply) -> CallRecord {
        self.reply_where(|c| c.method == method, result).unwrap_or_else(|| panic!("No pending {method} call"))
    }

    pub fn reply_where(&self, matches: impl Fn(&CallRecord) -> bool, result: Reply) -> Option<CallRecord> {
        let entry = {
            let mut pending = self.pending.borrow_mut();
            pending.iter().position(|(c, _)| matches(c)).map(|i| pending.remove(i))
        };
        let (call, done) = entry?;
        if let Some(done) = done {
            done(result);
        }
        Some(call)
    }

    /// Complete the oldest pending call.
    pub fn complete_first(&self, result: Reply) -> CallRecord {
        let (call, done) = self.pending.borrow_mut().remove(0);
        if let Some(done) = done {
            done(result);
        }
        call
    }

    pub fn has_pending(&self, method: &str) -> bool {
        self.pending.borrow().iter().any(|(c, _)| c.method == method)
    }

    pub fn pending_methods(&self) -> Vec<(String, String)> {
        self.pending.borrow().iter().map(|(c, _)| (c.method.clone(), c.path.clone())).collect()
    }

    pub fn methods(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c.method.clone()).collect()
    }

    pub fn count(&self, method: &str) -> usize {
        self.calls.borrow().iter().filter(|c| c.method == method).count()
    }

    pub fn last_call(&self) -> CallRecord {
        self.calls.borrow().last().cloned().expect("a call")
    }

    pub fn subscription_count(&self) -> usize {
        self.signals.borrow().len()
    }

    /// Deliver a signal to every matching subscription (sender is not checked,
    /// like the Python fake).
    pub fn emit(&self, sender: &str, interface: &str, member: &str, path: &str, params: Variant) {
        let arg0 = if params.n_children() > 0 { params.child_value(0).str().map(str::to_owned) } else { None };
        let handlers: Vec<SignalHandler> = self.signals.borrow().iter().filter(|(_, (_, i, m, p, a, _))| {
            i.as_deref().is_none_or(|i| i == interface) && m.as_deref().is_none_or(|m| m == member)
                && p.as_deref().is_none_or(|p| p == path) && a.as_deref().is_none_or(|a| Some(a) == arg0.as_deref())
        }).map(|(_, s)| s.5.clone()).collect();
        let signal = Signal { sender: sender.into(), path: path.into(), interface: interface.into(), member: member.into(), params };
        for handler in handlers {
            handler(&signal);
        }
    }

    /// Call the single exported object's method; returns the reply so far.
    pub fn invoke(&self, method: &str, params: Variant, sender: &str) -> Option<Result<Option<Variant>, (String, String)>> {
        self.invoke_slot(method, params, sender).take()
    }

    /// Like [`invoke`](Self::invoke), keeping the slot a deferred reply fills.
    pub fn invoke_slot(&self, method: &str, params: Variant, sender: &str) -> InvocationSlot {
        let (path, handler) = {
            let objects = self.objects.borrow();
            assert_eq!(objects.len(), 1, "exactly one exported object");
            (objects[0].1.clone(), objects[0].2.clone())
        };
        let slot = Rc::new(RefCell::new(None));
        handler(Invocation { sender: sender.into(), path, interface: String::new(), method: method.into(), params,
                             reply: InvocationReply::Fake(slot.clone()) });
        slot
    }

    pub fn fire_closed(&self, error: Option<BtError>) {
        self.closed.set(true);
        for handler in self.closed_handlers.borrow().iter() {
            handler(error.clone());
        }
    }

    pub fn finish_close(&self) {
        if let Some(done) = self.pending_close.take() {
            done();
        }
    }
}

impl Conn for FakeConn {
    fn call(&self, destination: &str, path: &str, interface: &str, method: &str, params: Option<Variant>, timeout_ms: i32, done: Done) {
        let call = CallRecord { destination: destination.into(), path: path.into(), interface: interface.into(),
                                method: method.into(), params, timeout: timeout_ms };
        self.calls.borrow_mut().push(call.clone());
        let immediate = self.responder.borrow().as_ref().and_then(|r| r(&call));
        match immediate {
            Some(result) => done(result),
            None => self.pending.borrow_mut().push((call, Some(done))),
        }
    }

    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32 {
        let own = |v: Option<&str>| v.map(String::from);
        let key = self.key();
        self.signals.borrow_mut().push((key, (own(sender), own(interface), own(member), own(path), own(arg0), handler)));
        key
    }

    fn unsubscribe(&self, id: u32) {
        self.signals.borrow_mut().retain(|(key, _)| *key != id);
    }

    fn register_object(&self, path: &str, _xml: &str, handler: MethodHandler) -> Result<u32, BtError> {
        let key = self.key();
        self.objects.borrow_mut().push((key, path.into(), handler));
        Ok(key)
    }

    fn unregister_object(&self, id: u32) {
        self.objects.borrow_mut().retain(|(key, _, _)| *key != id);
    }

    fn is_closed(&self) -> bool {
        self.closed.get()
    }

    fn close(&self, done: Box<dyn FnOnce()>) {
        self.close_requests.set(self.close_requests.get() + 1);
        self.pending_close.replace(Some(done));
    }

    fn connect_closed(&self, handler: Box<dyn Fn(Option<BtError>)>) {
        self.closed_handlers.borrow_mut().push(handler);
    }
}

/// An object path variant (`o`), or `s` if the text is not a valid path.
pub fn object_path(path: &str) -> Variant {
    match glib::variant::ObjectPath::try_from(path) {
        Ok(object) => object.to_variant(),
        Err(_) => path.to_variant(),
    }
}

/// `GLib.Variant("(o)", (path,))`.
pub fn object_tuple(path: &str) -> Variant {
    Variant::tuple_from_iter([object_path(path)])
}

/// `(a{sv})` reply from `(name, variant)` pairs.
pub fn props_reply(props: &[(&str, Variant)]) -> Variant {
    let dict = glib::VariantDict::new(None);
    for (key, value) in props {
        dict.insert_value(key, value);
    }
    Variant::tuple_from_iter([dict.end()])
}

/// An `a{sv}` from pairs.
pub fn vardict(props: &[(&str, Variant)]) -> Variant {
    let dict = glib::VariantDict::new(None);
    for (key, value) in props {
        dict.insert_value(key, value);
    }
    dict.end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_errors_keep_the_gdbus_prefix_and_code() {
        let error = BtError::remote("org.freedesktop.DBus.Error.ServiceUnknown", "gone");
        assert_eq!(error.message, "GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: gone");
        assert_eq!(error.dbus_code, Some(gio::DBusError::ServiceUnknown));
        let bluez = BtError::remote("org.bluez.Error.Failed", "br-connection-page-timeout");
        assert_eq!(bluez.message, "GDBus.Error:org.bluez.Error.Failed: br-connection-page-timeout");
    }

    #[test]
    fn fake_conn_defers_or_answers_and_emits() {
        let conn = FakeConn::with_responder(|c| (c.method == "Now").then(|| Ok(().to_variant())));
        let got = Rc::new(Cell::new(0));
        for method in ["Now", "Later"] {
            let g = got.clone();
            conn.call("a", "/", "i", method, None, 1, Box::new(move |r| { assert!(r.is_ok()); g.set(g.get() + 1) }));
        }
        assert_eq!(got.get(), 1);
        conn.reply("Later", Ok(().to_variant()));
        assert_eq!(got.get(), 2);
        let seen = Rc::new(Cell::new(0));
        let s = seen.clone();
        let id = conn.subscribe(None, Some("i"), Some("S"), None, Some("x"), Rc::new(move |_| s.set(s.get() + 1)));
        conn.emit("a", "i", "S", "/", ("x",).to_variant());
        conn.emit("a", "i", "S", "/", ("y",).to_variant());
        conn.unsubscribe(id);
        conn.emit("a", "i", "S", "/", ("x",).to_variant());
        assert_eq!(seen.get(), 1);
    }
}
