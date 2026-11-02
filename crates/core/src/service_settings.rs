//! Small asynchronous settings transport for already-running session services
//! (port of shared/service_settings.py).
//!
//! Clients never auto-start a service or own its policy/inhibitor lifetime.
//! The service exports `GetState() -> s` (JSON snapshot), one setter method
//! `<method>(<signature>) -> (b ok, s error)` and a `Changed(s)` signal on the
//! GApplication's own object path.

use crate::pyfmt::{json_dumps, JsonStyle};
use gio::prelude::*;
use glib::Variant;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub fn interface_xml(interface: &str, method: &str, signature: &str) -> String {
    format!(r#"<node><interface name="{interface}">
      <method name="GetState"><arg type="s" direction="out"/></method>
      <method name="{method}"><arg type="{signature}" direction="in"/>
        <arg type="b" direction="out"/><arg type="s" direction="out"/></method>
      <signal name="Changed"><arg type="s"/></signal>
    </interface></node>"#)
}

/// What a setter returns: `Ok((ok, error_text))` is replied as `(bs)`; `Err`
/// becomes the D-Bus error `<interface>.Error` (Python's raised exceptions).
pub type ApplyResult = Result<(bool, String), String>;

struct ServiceInner {
    connection: gio::DBusConnection,
    path: String,
    interface: String,
    snapshot: Box<dyn Fn() -> Value>,
    registration: RefCell<Option<gio::RegistrationId>>,
}

/// Exports the settings interface; call [`SettingsService::changed`] after any state change.
#[derive(Clone)]
pub struct SettingsService(Rc<ServiceInner>);

impl SettingsService {
    /// `application` must be registered (e.g. inside `startup`).
    pub fn new(application: &impl IsA<gio::Application>, interface: &str, method: &str, signature: &str,
               snapshot: impl Fn() -> Value + 'static, apply: impl Fn(&Variant) -> ApplyResult + 'static)
               -> Result<Self, glib::Error> {
        let application = application.as_ref();
        let connection = application.dbus_connection()
            .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::NotConnected, "No D-Bus connection"))?;
        let path = application.dbus_object_path().map(String::from).unwrap_or_default();
        let info = gio::DBusNodeInfo::for_xml(&interface_xml(interface, method, signature))?;
        let interface_info = info.lookup_interface(interface).expect("interface in generated XML");
        let inner = Rc::new(ServiceInner { connection: connection.clone(), path: path.clone(), interface: interface.into(),
                                           snapshot: Box::new(snapshot), registration: RefCell::new(None) });
        let weak: Weak<ServiceInner> = Rc::downgrade(&inner);
        let setter = method.to_owned();
        let id = connection.register_object(&path, &interface_info)
            .method_call(move |_, _, _, _, name, params, invocation| {
                let Some(inner) = weak.upgrade() else { return };
                let service = SettingsService(inner);
                if name == "GetState" {
                    invocation.return_value(Some(&(service.encoded(),).to_variant()));
                } else if name == setter {
                    match apply(&params.child_value(0)) {
                        Ok((ok, error)) => {
                            invocation.return_value(Some(&(ok, error).to_variant()));
                            service.changed();
                        }
                        Err(error) => invocation.return_dbus_error(&format!("{}.Error", service.0.interface), &error),
                    }
                } else {
                    invocation.return_dbus_error(&format!("{}.Error", service.0.interface), "Unknown settings method.");
                }
            })
            .build()?;
        inner.registration.replace(Some(id));
        Ok(Self(inner))
    }

    fn encoded(&self) -> String {
        json_dumps(&(self.0.snapshot)(), &JsonStyle::DEFAULT)
    }

    /// Emit `Changed(snapshot)`; a no-op after `close`.
    pub fn changed(&self) {
        if self.0.registration.borrow().is_some() {
            let _ = self.0.connection.emit_signal(None, &self.0.path, &self.0.interface, "Changed",
                                                  Some(&(self.encoded(),).to_variant()));
        }
    }

    pub fn close(&self) {
        if let Some(id) = self.0.registration.take() {
            let _ = self.0.connection.unregister_object(id);
        }
    }
}

/// Reply callback: `(ok, error, reply values)`.
pub type Reply = Box<dyn FnOnce(bool, String, Option<Variant>)>;

struct ClientInner {
    on_changed: Box<dyn Fn()>,
    name: String,
    interface: String,
    unavailable: String,
    state: RefCell<Map<String, Value>>,
    ready: Cell<bool>,
    error: RefCell<String>,
    proxy: RefCell<Option<gio::DBusProxy>>,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    closed: Cell<bool>,
    cancel: gio::Cancellable,
    busy: Cell<bool>,
    serial: Cell<u64>,
}

/// Follows one service's state; never starts it.
#[derive(Clone)]
pub struct SettingsClient(Rc<ClientInner>);

impl SettingsClient {
    pub fn new(on_changed: impl Fn() + 'static, name: &str, interface: &str, unavailable: &str) -> Self {
        let mut state = Map::new();
        state.insert("ready".into(), false.into());
        state.insert("error".into(), "".into());
        Self(Rc::new(ClientInner {
            on_changed: Box::new(on_changed), name: name.into(), interface: interface.into(),
            unavailable: unavailable.into(), state: RefCell::new(state), ready: Cell::new(false),
            error: RefCell::new(String::new()), proxy: RefCell::new(None), handlers: RefCell::new(Vec::new()),
            closed: Cell::new(false), cancel: gio::Cancellable::new(), busy: Cell::new(false), serial: Cell::new(0),
        }))
    }

    pub fn state(&self) -> Map<String, Value> { self.0.state.borrow().clone() }
    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    /// Owner-managed "request in flight" flag; reset whenever the service owner changes.
    pub fn busy(&self) -> bool { self.0.busy.get() }
    pub fn set_busy(&self, busy: bool) { self.0.busy.set(busy) }
    /// The underlying proxy once connected (Python `self._proxy`), e.g. for a raw
    /// call on another interface of the same object via `proxy.connection()`.
    pub fn proxy(&self) -> Option<gio::DBusProxy> { self.0.proxy.borrow().clone() }

    fn notify(&self) { (self.0.on_changed)() }

    pub fn start(&self) {
        let weak = Rc::downgrade(&self.0);
        let path = format!("/{}", self.0.name.replace('.', "/"));
        gio::DBusProxy::for_bus(gio::BusType::Session,
            gio::DBusProxyFlags::DO_NOT_AUTO_START | gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
            None, &self.0.name, &path, &self.0.interface, Some(&self.0.cancel), move |result| {
                let Some(inner) = weak.upgrade() else { return };
                SettingsClient(inner).connected(result);
            });
    }

    fn connected(&self, result: Result<gio::DBusProxy, glib::Error>) {
        let proxy = match result {
            Ok(proxy) => proxy,
            Err(error) => {
                if !self.0.closed.get() {
                    self.0.error.replace(error.message().into());
                    self.notify();
                }
                return;
            }
        };
        if self.0.closed.get() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let signal = proxy.connect_local("g-signal", false, move |values| {
            let name: String = values[2].get().ok()?;
            let params: Variant = values[3].get().ok()?;
            if let Some(inner) = weak.upgrade() {
                let client = SettingsClient(inner);
                if name == "Changed" && !client.0.closed.get() {
                    if let Some(encoded) = params.try_child_value(0).and_then(|v| v.get::<String>()) {
                        client.apply_state(&encoded);
                    }
                }
            }
            None
        });
        let weak = Rc::downgrade(&self.0);
        let owner = proxy.connect_notify_local(Some("g-name-owner"), move |_, _| {
            if let Some(inner) = weak.upgrade() {
                SettingsClient(inner).owner_changed();
            }
        });
        self.0.proxy.replace(Some(proxy));
        self.0.handlers.replace(vec![signal, owner]);
        self.owner_changed();
    }

    fn has_owner(&self) -> bool {
        self.0.proxy.borrow().as_ref().is_some_and(|proxy| proxy.name_owner().is_some())
    }

    fn owner_changed(&self) {
        self.0.serial.set(self.0.serial.get() + 1);
        self.0.busy.set(false);
        if !self.has_owner() {
            self.0.ready.set(false);
            let mut state = Map::new();
            state.insert("ready".into(), false.into());
            state.insert("error".into(), self.0.unavailable.clone().into());
            self.0.state.replace(state);
            self.0.error.replace(self.0.unavailable.clone());
            self.notify();
        } else {
            let weak = Rc::downgrade(&self.0);
            self.call("GetState", None, Some(Box::new(move |ok, _, values| {
                let encoded = values.and_then(|v| v.try_child_value(0)).and_then(|v| v.get::<String>());
                if let (true, Some(inner), Some(encoded)) = (ok, weak.upgrade(), encoded) {
                    SettingsClient(inner).apply_state(&encoded);
                }
            })));
        }
    }

    fn apply_state(&self, encoded: &str) {
        match serde_json::from_str::<Value>(encoded) {
            Ok(Value::Object(state)) => {
                self.0.ready.set(state.get("ready").is_some_and(truthy));
                self.0.error.replace(state.get("error").map(py_str_or_empty).unwrap_or_default());
                self.0.state.replace(state);
            }
            Ok(_) => {
                self.0.ready.set(false);
                self.0.error.replace("Invalid settings response.".into());
            }
            Err(error) => {
                self.0.ready.set(false);
                self.0.error.replace(error.to_string());
            }
        }
        self.notify();
    }

    /// Call a service method (e.g. `"SetSettings"`, `Some(&("{...}",).to_variant())`).
    /// Returns false (and replies `(false, unavailable)`) when the service is not running.
    /// A reply that arrives after the owner changed is reported as unavailable.
    pub fn call(&self, method: &str, params: Option<&Variant>, callback: Option<Reply>) -> bool {
        if self.0.closed.get() || !self.has_owner() {
            self.0.error.replace(self.0.unavailable.clone());
            if !self.0.closed.get() {
                self.notify();
            }
            if let Some(callback) = callback {
                callback(false, self.error(), None);
            }
            return false;
        }
        let serial = self.0.serial.get();
        let weak = Rc::downgrade(&self.0);
        let get_state = method == "GetState";
        let proxy = self.0.proxy.borrow().clone().unwrap();
        proxy.call(method, params, gio::DBusCallFlags::NO_AUTO_START, 15000, Some(&self.0.cancel), move |result| {
            let (ok, error, values) = match result {
                Ok(values) if get_state => (true, String::new(), Some(values)),
                Ok(values) => {
                    let ok = values.try_child_value(0).and_then(|v| v.get::<bool>()).unwrap_or(false);
                    let error = values.try_child_value(1).and_then(|v| v.get::<String>()).unwrap_or_default();
                    (ok, error, Some(values))
                }
                Err(mut failure) => {
                    gio::DBusError::strip_remote_error(&mut failure);
                    (false, failure.message().to_owned(), None)
                }
            };
            let Some(inner) = weak.upgrade() else { return };
            let client = SettingsClient(inner);
            if client.0.closed.get() {
                return;
            }
            if serial != client.0.serial.get() {
                if let Some(callback) = callback {
                    callback(false, client.0.unavailable.clone(), None);
                }
                return;
            }
            if !error.is_empty() {
                client.0.error.replace(error.clone());
                client.notify();
            }
            if let Some(callback) = callback {
                callback(ok, error, values);
            }
        });
        true
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.cancel.cancel();
        if let Some(proxy) = self.0.proxy.take() {
            for handler in self.0.handlers.take() {
                proxy.disconnect(handler);
            }
        }
        self.0.ready.set(false);
    }
}

/// Python truthiness of a JSON value (re-exported for existing callers).
pub use crate::pyjson::truthy;
use crate::pyjson::py_str_or_empty;
