//! `brightness-manager/brightness_client.py` (`BrightnessClient(on_changed,
//! refresh_on_connect=True)`): the asynchronous D-Bus client of
//! `org.waybar.BrightnessManager`. A copy of the client in the
//! brightness-manager crate, reduced to what the Displays window uses.

use gio::prelude::*;
use glib::Variant;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::pyjson::{py_str, truthy};

pub const BUS_NAME: &str = "org.waybar.BrightnessManager";
pub const OBJECT_PATH: &str = "/org/waybar/BrightnessManager";
pub const INTERFACE: &str = "org.waybar.BrightnessManager.Controls";
pub const UNAVAILABLE: &str = "Brightness controls are unavailable.";

/// What the brightness widgets read and write (the Python duck type used by
/// `BrightnessControls`, so tests can substitute a fake).
pub trait BrightnessModel {
    fn state(&self) -> Value;
    fn ready(&self) -> bool;
    fn error(&self) -> String;
    /// `Ok(false)` = the call was refused (Python `False`), `Err` = raised.
    fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: Option<&Value>) -> Result<bool, String>;
    fn set_auto_dim(&self, enabled: bool) -> Result<bool, String>;
    fn set_settings(&self, settings: &Value) -> Result<bool, String>;
    fn start(&self) -> Result<(), String>;
    fn close(&self);
}

pub fn initial_state() -> Value {
    json!({"ready": false, "displays": [], "settings": {"auto_dim": false, "dim_after": 120, "dim_percent": 30}})
}

struct Inner {
    on_changed: Box<dyn Fn()>,
    refresh_on_connect: bool,
    state: RefCell<Value>,
    ready: Cell<bool>,
    error: RefCell<String>,
    closed: Cell<bool>,
    settings_pending: Cell<u32>,
    server_busy: Cell<bool>,
    proxy: RefCell<Option<gio::DBusProxy>>,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    cancel: gio::Cancellable,
}

#[derive(Clone)]
pub struct BrightnessClient(Rc<Inner>);

impl BrightnessClient {
    pub fn new(on_changed: impl Fn() + 'static, refresh_on_connect: bool) -> Self {
        Self(Rc::new(Inner {
            on_changed: Box::new(on_changed), refresh_on_connect, state: RefCell::new(initial_state()),
            ready: Cell::new(false), error: RefCell::new(String::new()), closed: Cell::new(false),
            settings_pending: Cell::new(0), server_busy: Cell::new(false), proxy: RefCell::new(None),
            handlers: RefCell::new(Vec::new()), cancel: gio::Cancellable::new(),
        }))
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn changed(&self) {
        (self.0.on_changed)();
    }

    fn connected(&self, proxy: gio::DBusProxy) {
        if self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        let signal = proxy.connect_local("g-signal", false, move |values| {
            let name = values.get(2).and_then(|v| v.get::<String>().ok());
            let parameters = values.get(3).and_then(|v| v.get::<Variant>().ok());
            if let (Some(inner), Some("Changed"), Some(parameters)) = (weak.upgrade(), name.as_deref(), parameters) {
                if !inner.closed.get() {
                    if let Some((encoded,)) = parameters.get::<(String,)>() {
                        BrightnessClient(inner).update(&encoded);
                    }
                }
            }
            None
        });
        let weak = self.weak();
        let owner = proxy.connect_notify_local(Some("g-name-owner"), move |_, _| {
            if let Some(inner) = weak.upgrade() {
                BrightnessClient(inner).owner();
            }
        });
        self.0.handlers.replace(vec![signal, owner]);
        self.0.proxy.replace(Some(proxy));
        self.owner();
    }

    fn owner(&self) {
        if self.0.closed.get() {
            return;
        }
        let owned = self.0.proxy.borrow().as_ref().and_then(|p| p.name_owner()).is_some();
        if owned {
            self.get_state();
            if self.0.refresh_on_connect {
                self.call("Refresh", None, false);
            }
        } else {
            self.0.ready.set(false);
            self.0.error.replace(UNAVAILABLE.into());
            self.changed();
        }
    }

    fn set_busy(&self) {
        let busy = self.0.server_busy.get() || self.0.settings_pending.get() > 0;
        if let Some(state) = self.0.state.borrow_mut().as_object_mut() {
            state.insert("busy".into(), busy.into());
        }
    }

    /// `_state(encoded)`.
    fn update(&self, encoded: &str) {
        match serde_json::from_str::<Value>(encoded) {
            Ok(state) if state.is_object() && state.get("displays").is_some_and(Value::is_array) => {
                self.0.server_busy.set(state.get("busy").is_some_and(truthy));
                self.0.ready.set(state.get("ready").is_some_and(truthy));
                let error = match state.get("error") {
                    Some(value) if truthy(value) => py_str(value),
                    _ => String::new(),
                };
                self.0.state.replace(state);
                self.set_busy();
                self.0.error.replace(error);
            }
            Ok(_) => {
                self.0.ready.set(false);
                self.0.error.replace("Invalid brightness service response.".into());
            }
            Err(error) => {
                self.0.ready.set(false);
                self.0.error.replace(error.to_string());
            }
        }
        self.changed();
    }

    fn get_state(&self) {
        let Some(proxy) = self.0.proxy.borrow().clone() else { return };
        let weak = self.weak();
        proxy.call("GetState", None, gio::DBusCallFlags::NONE, 15000, Some(&self.0.cancel), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.closed.get() {
                return;
            }
            let client = BrightnessClient(inner);
            match result {
                Ok(value) => {
                    if let Some((encoded,)) = value.get::<(String,)>() {
                        client.update(&encoded);
                    }
                }
                Err(mut error) => {
                    gio::DBusError::strip_remote_error(&mut error);
                    client.0.error.replace(error.message().to_owned());
                    client.changed();
                }
            }
        });
    }

    fn call(&self, method: &'static str, parameters: Option<Variant>, settings_call: bool) -> bool {
        let proxy = self.0.proxy.borrow().clone().filter(|p| p.name_owner().is_some());
        let Some(proxy) = proxy.filter(|_| !self.0.closed.get()) else {
            self.0.error.replace(UNAVAILABLE.into());
            if !self.0.closed.get() {
                self.changed();
            }
            return false;
        };
        if settings_call {
            self.0.settings_pending.set(self.0.settings_pending.get() + 1);
            if let Some(state) = self.0.state.borrow_mut().as_object_mut() {
                state.insert("busy".into(), true.into());
            }
            self.changed();
        }
        let weak = self.weak();
        proxy.call(method, parameters.as_ref(), gio::DBusCallFlags::NONE, 15000, Some(&self.0.cancel), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.closed.get() {
                return;
            }
            let client = BrightnessClient(inner);
            match result {
                Ok(value) => {
                    if value.get::<(bool,)>().is_some_and(|v| !v.0) {
                        let error = client.0.state.borrow().get("error").filter(|v| truthy(v)).map(py_str)
                            .unwrap_or_else(|| "The change could not be applied.".to_owned());
                        client.0.error.replace(error);
                        client.changed();
                    }
                }
                Err(mut error) => {
                    gio::DBusError::strip_remote_error(&mut error);
                    client.0.error.replace(error.message().to_owned());
                    client.changed();
                }
            }
            if settings_call {
                client.0.settings_pending.set(client.0.settings_pending.get().saturating_sub(1));
                client.set_busy();
                client.changed();
            }
        });
        true
    }
}

impl BrightnessModel for BrightnessClient {
    fn state(&self) -> Value { self.0.state.borrow().clone() }
    fn ready(&self) -> bool { self.0.ready.get() }
    fn error(&self) -> String { self.0.error.borrow().clone() }

    fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: Option<&Value>) -> Result<bool, String> {
        let id = match expected_id {
            Some(id) => py_str(id),
            None => {
                let state = self.state();
                state.get("displays").and_then(Value::as_array)
                    .and_then(|d| d.iter().find(|r| r.get("connector").and_then(Value::as_str) == Some(connector)))
                    .and_then(|r| r.get("id")).map(py_str).unwrap_or_default()
            }
        };
        Ok(self.call("SetValue", Some((connector, feature, value, id.as_str()).into()), false))
    }

    fn set_auto_dim(&self, enabled: bool) -> Result<bool, String> {
        Ok(self.call("SetAutoDim", Some((enabled,).into()), true))
    }

    fn set_settings(&self, settings: &Value) -> Result<bool, String> {
        let encoded = json_dumps(settings, &JsonStyle::DEFAULT);
        Ok(self.call("SetSettings", Some((encoded.as_str(),).into()), true))
    }

    fn start(&self) -> Result<(), String> {
        let weak = self.weak();
        gio::DBusProxy::for_bus(gio::BusType::Session,
            gio::DBusProxyFlags::DO_NOT_AUTO_START | gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
            None, BUS_NAME, OBJECT_PATH, INTERFACE, Some(&self.0.cancel), move |result| {
                let Some(inner) = weak.upgrade() else { return };
                let client = BrightnessClient(inner);
                match result {
                    Err(error) => {
                        if !client.0.closed.get() {
                            client.0.error.replace(error.message().to_owned());
                            client.changed();
                        }
                    }
                    Ok(proxy) => client.connected(proxy),
                }
            });
        Ok(())
    }

    fn close(&self) {
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
