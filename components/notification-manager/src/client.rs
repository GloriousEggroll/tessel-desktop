//! Asynchronous controls for the resident notification service; never
//! auto-starts it (port of notification_client.py).

use gio::prelude::*;
use glib::Variant;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const INTERFACE: &str = "org.waydock.Notifications1";
pub const NOT_RUNNING: &str = "Notifications are not running in this session.";

type Callback = Box<dyn FnOnce(bool, Option<Variant>)>;

struct Inner {
    on_changed: Box<dyn Fn()>,
    state: RefCell<Map<String, Value>>,
    ready: Cell<bool>,
    error: RefCell<String>,
    proxy: RefCell<Option<gio::DBusProxy>>,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    closed: Cell<bool>,
    serial: Cell<u64>,
    cancel: gio::Cancellable,
}

#[derive(Clone)]
pub struct NotificationClient(Rc<Inner>);

impl NotificationClient {
    pub fn new(on_changed: impl Fn() + 'static) -> Self {
        Self(Rc::new(Inner {
            on_changed: Box::new(on_changed), state: RefCell::new(Map::new()), ready: Cell::new(false),
            error: RefCell::new(String::new()), proxy: RefCell::new(None), handlers: RefCell::new(Vec::new()),
            closed: Cell::new(false), serial: Cell::new(0), cancel: gio::Cancellable::new(),
        }))
    }

    pub fn state(&self) -> Map<String, Value> { self.0.state.borrow().clone() }
    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }

    fn changed(&self) {
        (self.0.on_changed)();
    }

    pub fn start(&self) {
        let weak = Rc::downgrade(&self.0);
        gio::DBusProxy::for_bus(gio::BusType::Session,
            gio::DBusProxyFlags::DO_NOT_AUTO_START | gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
            None, NAME, PATH, INTERFACE, Some(&self.0.cancel), move |result| {
                let Some(inner) = weak.upgrade() else { return };
                NotificationClient(inner).connected(result);
            });
    }

    fn connected(&self, result: Result<gio::DBusProxy, glib::Error>) {
        let proxy = match result {
            Ok(proxy) => proxy,
            Err(error) => {
                if !self.0.closed.get() {
                    self.0.error.replace(error.message().to_owned());
                    self.changed();
                }
                return;
            }
        };
        if self.0.closed.get() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let signal = proxy.connect_local("g-signal", false, move |values| {
            let name = values[2].get::<String>().unwrap_or_default();
            let params = values[3].get::<Variant>().ok();
            if let (Some(inner), Some(params)) = (weak.upgrade(), params) {
                let client = NotificationClient(inner);
                if name == "StateChanged" && !client.0.closed.get() {
                    client.apply_state(&params.child_value(0).get::<String>().unwrap_or_default());
                }
            }
            None
        });
        let weak: Weak<Inner> = Rc::downgrade(&self.0);
        let owner = proxy.connect_notify_local(Some("g-name-owner"), move |_, _| {
            if let Some(inner) = weak.upgrade() {
                NotificationClient(inner).owner();
            }
        });
        self.0.proxy.replace(Some(proxy));
        self.0.handlers.replace(vec![signal, owner]);
        self.owner();
    }

    fn owner(&self) {
        self.0.serial.set(self.0.serial.get() + 1);
        self.0.ready.set(false);
        let has_owner = self.0.proxy.borrow().as_ref().and_then(|p| p.name_owner()).is_some();
        if !has_owner {
            self.0.state.replace(Map::new());
            self.0.error.replace(NOT_RUNNING.into());
            self.changed();
        } else {
            let weak = Rc::downgrade(&self.0);
            self.call("GetState", None, Some(Box::new(move |ok, values| {
                if let (true, Some(inner), Some(values)) = (ok, weak.upgrade(), values) {
                    NotificationClient(inner).apply_state(&values.child_value(0).get::<String>().unwrap_or_default());
                }
            })));
        }
    }

    fn apply_state(&self, encoded: &str) {
        match serde_json::from_str::<Value>(encoded) {
            Ok(Value::Object(state)) => {
                let error = state.get("error").map(suite_core::pyjson::py_str_or_empty).unwrap_or_default();
                self.0.state.replace(state);
                self.0.ready.set(true);
                self.0.error.replace(error);
            }
            Ok(_) => {
                self.0.ready.set(false);
                self.0.error.replace("Invalid notification state".into());
            }
            Err(error) => {
                self.0.ready.set(false);
                self.0.error.replace(error.to_string());
            }
        }
        self.changed();
    }

    /// Returns false (and does nothing) without a service owner.
    pub fn call(&self, method: &str, params: Option<Variant>, callback: Option<Callback>) -> bool {
        let proxy = self.0.proxy.borrow().clone();
        let Some(proxy) = proxy.filter(|p| !self.0.closed.get() && p.name_owner().is_some()) else { return false };
        let serial = self.0.serial.get();
        let weak = Rc::downgrade(&self.0);
        proxy.call(method, params.as_ref(), gio::DBusCallFlags::NO_AUTO_START, 5000, Some(&self.0.cancel), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let client = NotificationClient(inner);
            let (ok, error, values) = match result {
                Ok(values) => (true, String::new(), Some(values)),
                Err(mut failure) => {
                    gio::DBusError::strip_remote_error(&mut failure);
                    (false, failure.message().to_owned(), None)
                }
            };
            if client.0.closed.get() || serial != client.0.serial.get() {
                return;
            }
            client.0.error.replace(error);
            if !ok {
                client.changed();
            }
            if let Some(callback) = callback {
                callback(ok, values);
            }
        });
        true
    }

    /// Python `json.dumps(settings)` (default separators).
    pub fn set_settings(&self, settings: &Value) -> bool {
        let text = suite_core::pyfmt::json_dumps(settings, &suite_core::pyfmt::JsonStyle::DEFAULT);
        self.call("SetSettings", Some((text,).to_variant()), None)
    }
    pub fn clear_history(&self) -> bool { self.call("ClearHistory", None, None) }
    pub fn mark_read(&self) -> bool { self.call("MarkRead", None, None) }
    pub fn dismiss(&self, ident: u32) -> bool { self.call("Dismiss", Some((ident,).to_variant()), None) }
    pub fn clear_app(&self, key: &str) -> bool { self.call("ClearApp", Some((key,).to_variant()), None) }
    pub fn activate(&self, ident: u32, key: &str) -> bool { self.call("Activate", Some((ident, key).to_variant()), None) }
    pub fn show_settings(&self) -> bool { self.call("ShowSettings", None, None) }

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

/// Bounded command-line control (3 s), with service activation explicitly disabled.
pub fn request(method: &str, params: Option<Variant>) -> Result<Variant, glib::Error> {
    let bus = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)?;
    bus.call_sync(Some(NAME), PATH, INTERFACE, method, params.as_ref(), None, gio::DBusCallFlags::NO_AUTO_START, 3000,
                  None::<&gio::Cancellable>)
}
