//! `brightness_client.py`: the GTK-independent asynchronous client protocol
//! (used by `tessel-brightness-status`, and by tessel-displays through the Python
//! file that stays installed), plus the in-process client the popup uses so
//! the service does not proxy to itself over D-Bus.

use crate::model;
use gio::prelude::*;
use glib::Variant;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const BUS_NAME: &str = "org.tessel.BrightnessManager";
pub const OBJECT_PATH: &str = "/org/tessel/BrightnessManager";
pub const INTERFACE: &str = "org.tessel.BrightnessManager.Controls";
pub const UNAVAILABLE: &str = "Brightness controls are unavailable.";

pub const INTERFACE_XML: &str = r#"<node><interface name="org.tessel.BrightnessManager.Controls">
 <method name="GetState"><arg type="s" direction="out"/></method>
 <method name="Refresh"><arg type="b" direction="out"/></method>
 <method name="SetValue"><arg type="s" direction="in" name="connector"/>
  <arg type="s" direction="in" name="feature"/><arg type="d" direction="in" name="value"/>
  <arg type="s" direction="in" name="expected_id"/><arg type="b" direction="out"/></method>
 <method name="SetAutoDim"><arg type="b" direction="in"/><arg type="b" direction="out"/></method>
 <method name="SetScale"><arg type="s" direction="in" name="connector"/>
  <arg type="d" direction="in" name="scale"/><arg type="s" direction="in" name="expected_id"/>
  <arg type="b" direction="out"/></method>
 <method name="SetAutoScale"><arg type="b" direction="in"/><arg type="b" direction="out"/></method>
 <method name="SetSettings"><arg type="s" direction="in"/><arg type="b" direction="out"/></method>
 <signal name="Changed"><arg type="s"/></signal>
</interface></node>"#;

pub fn initial_state() -> Value {
    json!({"ready": false, "displays": [], "settings": {"auto_dim": false, "dim_after": 120, "dim_percent": 30}})
}

/// The view of the service the popup widgets read and write.
pub trait UiClient {
    fn state(&self) -> Value;
    fn ready(&self) -> bool;
    fn error(&self) -> String;
    fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: Option<&str>) -> bool;
    fn set_auto_dim(&self, enabled: bool) -> bool;
    fn set_scale(&self, connector: &str, value: f64, expected_id: Option<&str>) -> bool;
    fn set_auto_scale(&self, enabled: bool) -> bool;
    fn set_settings(&self, settings: &Value) -> bool;
    fn refresh(&self) -> bool;
}

/// A D-Bus request (method + arguments).
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Refresh,
    SetValue(String, String, f64, String),
    SetAutoDim(bool),
    SetScale(String, f64, String),
    SetAutoScale(bool),
    SetSettings(String),
}

impl Request {
    pub fn method(&self) -> &'static str {
        match self {
            Request::Refresh => "Refresh",
            Request::SetValue(..) => "SetValue",
            Request::SetAutoDim(_) => "SetAutoDim",
            Request::SetScale(..) => "SetScale",
            Request::SetAutoScale(_) => "SetAutoScale",
            Request::SetSettings(_) => "SetSettings",
        }
    }

    pub fn parameters(&self) -> Option<Variant> {
        match self {
            Request::Refresh => None,
            Request::SetValue(c, f, v, i) => Some((c.as_str(), f.as_str(), *v, i.as_str()).into()),
            Request::SetAutoDim(b) | Request::SetAutoScale(b) => Some((*b,).into()),
            Request::SetScale(c, v, i) => Some((c.as_str(), *v, i.as_str()).into()),
            Request::SetSettings(s) => Some((s.as_str(),).into()),
        }
    }

    /// Decode D-Bus parameters (`values.unpack()`).
    pub fn from_call(method: &str, values: &Variant) -> Option<Self> {
        Some(match method {
            "Refresh" => Request::Refresh,
            "SetValue" => {
                let (c, f, v, i) = values.get::<(String, String, f64, String)>()?;
                Request::SetValue(c, f, v, i)
            }
            "SetAutoDim" => Request::SetAutoDim(values.get::<(bool,)>()?.0),
            "SetScale" => {
                let (c, v, i) = values.get::<(String, f64, String)>()?;
                Request::SetScale(c, v, i)
            }
            "SetAutoScale" => Request::SetAutoScale(values.get::<(bool,)>()?.0),
            "SetSettings" => Request::SetSettings(values.get::<(String,)>()?.0),
            _ => return None,
        })
    }

    fn settings_call(&self) -> bool {
        matches!(self, Request::SetSettings(_) | Request::SetAutoDim(_))
    }

    fn scaling_call(&self) -> bool {
        matches!(self, Request::SetScale(..) | Request::SetAutoScale(_))
    }
}

fn expected(state: &Value, connector: &str, key: &str) -> String {
    state.get("displays").and_then(Value::as_array).and_then(|d| d.iter().find(|r| model::text(r, "connector") == connector))
        .map(|r| model::text(r, key).to_owned()).unwrap_or_default()
}

/// Shared client bookkeeping: state, ready/error, pending settings calls.
struct Common {
    on_changed: Box<dyn Fn()>,
    state: RefCell<Value>,
    ready: Cell<bool>,
    error: RefCell<String>,
    closed: Cell<bool>,
    settings_pending: Cell<u32>,
    server_busy: Cell<bool>,
}

impl Common {
    fn new(on_changed: Box<dyn Fn()>) -> Self {
        Self { on_changed, state: RefCell::new(initial_state()), ready: Cell::new(false), error: RefCell::new(String::new()),
               closed: Cell::new(false), settings_pending: Cell::new(0), server_busy: Cell::new(false) }
    }

    fn changed(&self) {
        (self.on_changed)();
    }

    fn set_busy(&self) {
        let busy = self.server_busy.get() || self.settings_pending.get() > 0;
        if let Some(state) = self.state.borrow_mut().as_object_mut() {
            state.insert("busy".into(), busy.into());
        }
    }

    /// `_state(encoded)`.
    fn update(&self, encoded: &str) {
        match serde_json::from_str::<Value>(encoded) {
            Ok(state) if state.is_object() && state.get("displays").is_some_and(Value::is_array) => {
                self.server_busy.set(state.get("busy").is_some_and(model::truthy));
                self.ready.set(state.get("ready").is_some_and(model::truthy));
                let error = match state.get("error") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) if model::truthy(other) => other.to_string(),
                    _ => String::new(),
                };
                self.state.replace(state);
                self.set_busy();
                self.error.replace(error);
            }
            Ok(_) => {
                self.ready.set(false);
                self.error.replace("Invalid brightness service response.".into());
            }
            Err(error) => {
                self.ready.set(false);
                self.error.replace(error.to_string());
            }
        }
        self.changed();
    }

    fn begin(&self, request: &Request) {
        if request.settings_call() {
            self.settings_pending.set(self.settings_pending.get() + 1);
            if let Some(state) = self.state.borrow_mut().as_object_mut() {
                state.insert("busy".into(), true.into());
            }
            self.changed();
        }
    }

    /// Reply handling of `_call.done`.
    fn finish(&self, request: &Request, reply: Result<bool, String>) {
        if self.closed.get() {
            return;
        }
        match reply {
            Ok(false) => {
                let scaling = if request.scaling_call() {
                    self.state.borrow().get("scaling").and_then(|s| s.get("error")).and_then(Value::as_str).unwrap_or("").to_owned()
                } else {
                    String::new()
                };
                let state_error = model::text(&self.state.borrow(), "error").to_owned();
                let error = [scaling, state_error].into_iter().find(|e| !e.is_empty())
                    .unwrap_or_else(|| "The change could not be applied.".into());
                self.error.replace(error);
                self.changed();
            }
            Ok(true) => {}
            Err(message) => {
                self.error.replace(message);
                self.changed();
            }
        }
        if request.settings_call() {
            self.settings_pending.set(self.settings_pending.get().saturating_sub(1));
            self.set_busy();
            self.changed();
        }
    }
}

struct ProxyInner {
    common: Common,
    refresh_on_connect: bool,
    proxy: RefCell<Option<gio::DBusProxy>>,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    cancel: gio::Cancellable,
    bus_type: gio::BusType,
}

/// `BrightnessClient(on_changed, refresh_on_connect=False)` over the session bus.
#[derive(Clone)]
pub struct BrightnessClient(Rc<ProxyInner>);

impl BrightnessClient {
    pub fn new(on_changed: impl Fn() + 'static, refresh_on_connect: bool) -> Self {
        Self(Rc::new(ProxyInner { common: Common::new(Box::new(on_changed)), refresh_on_connect, proxy: RefCell::new(None),
                                  handlers: RefCell::new(Vec::new()), cancel: gio::Cancellable::new(), bus_type: gio::BusType::Session }))
    }

    fn weak(&self) -> Weak<ProxyInner> {
        Rc::downgrade(&self.0)
    }

    pub fn start(&self) {
        let weak = self.weak();
        gio::DBusProxy::for_bus(self.0.bus_type, gio::DBusProxyFlags::DO_NOT_AUTO_START | gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
            None, BUS_NAME, OBJECT_PATH, INTERFACE, Some(&self.0.cancel), move |result| {
                let Some(inner) = weak.upgrade() else { return };
                let client = BrightnessClient(inner);
                match result {
                    Err(error) => {
                        if !client.0.common.closed.get() {
                            client.0.common.error.replace(error.message().to_owned());
                            client.0.common.changed();
                        }
                    }
                    Ok(proxy) => client.connected(proxy),
                }
            });
    }

    fn connected(&self, proxy: gio::DBusProxy) {
        if self.0.common.closed.get() {
            return;
        }
        let weak = self.weak();
        let signal = proxy.connect_local("g-signal", false, move |values| {
            let name = values.get(2).and_then(|v| v.get::<String>().ok());
            let parameters = values.get(3).and_then(|v| v.get::<Variant>().ok());
            if let (Some(inner), Some("Changed"), Some(parameters)) = (weak.upgrade(), name.as_deref(), parameters) {
                if !inner.common.closed.get() {
                    if let Some((encoded,)) = parameters.get::<(String,)>() {
                        inner.common.update(&encoded);
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
        if self.0.common.closed.get() {
            return;
        }
        let owned = self.0.proxy.borrow().as_ref().and_then(|p| p.name_owner()).is_some();
        if owned {
            self.get_state();
            if self.0.refresh_on_connect {
                self.refresh();
            }
        } else {
            self.0.common.ready.set(false);
            self.0.common.error.replace(UNAVAILABLE.into());
            self.0.common.changed();
        }
    }

    fn get_state(&self) {
        let Some(proxy) = self.0.proxy.borrow().clone() else { return };
        let weak = self.weak();
        proxy.call("GetState", None, gio::DBusCallFlags::NONE, 15000, Some(&self.0.cancel), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.common.closed.get() {
                return;
            }
            match result {
                Ok(value) => {
                    if let Some((encoded,)) = value.get::<(String,)>() {
                        inner.common.update(&encoded);
                    }
                }
                Err(mut error) => {
                    gio::DBusError::strip_remote_error(&mut error);
                    inner.common.error.replace(error.message().to_owned());
                    inner.common.changed();
                }
            }
        });
    }

    fn call(&self, request: Request) -> bool {
        let proxy = self.0.proxy.borrow().clone().filter(|p| p.name_owner().is_some());
        let Some(proxy) = proxy.filter(|_| !self.0.common.closed.get()) else {
            self.0.common.error.replace(UNAVAILABLE.into());
            if !self.0.common.closed.get() {
                self.0.common.changed();
            }
            return false;
        };
        self.0.common.begin(&request);
        let weak = self.weak();
        let parameters = request.parameters();
        proxy.call(request.method(), parameters.as_ref(), gio::DBusCallFlags::NONE, 15000, Some(&self.0.cancel), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let reply = match result {
                Ok(value) => Ok(value.get::<(bool,)>().map(|v| v.0).unwrap_or(true)),
                Err(mut error) => {
                    gio::DBusError::strip_remote_error(&mut error);
                    Err(error.message().to_owned())
                }
            };
            inner.common.finish(&request, reply);
        });
        true
    }

    pub fn close(&self) {
        self.0.common.closed.set(true);
        self.0.cancel.cancel();
        if let Some(proxy) = self.0.proxy.take() {
            for handler in self.0.handlers.take() {
                proxy.disconnect(handler);
            }
        }
        self.0.common.ready.set(false);
    }
}

impl UiClient for BrightnessClient {
    fn state(&self) -> Value { self.0.common.state.borrow().clone() }
    fn ready(&self) -> bool { self.0.common.ready.get() }
    fn error(&self) -> String { self.0.common.error.borrow().clone() }

    fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: Option<&str>) -> bool {
        let id = expected_id.map(String::from).unwrap_or_else(|| expected(&self.state(), connector, "id"));
        self.call(Request::SetValue(connector.into(), feature.into(), value, id))
    }

    fn set_auto_dim(&self, enabled: bool) -> bool { self.call(Request::SetAutoDim(enabled)) }

    fn set_scale(&self, connector: &str, value: f64, expected_id: Option<&str>) -> bool {
        let id = expected_id.map(String::from).unwrap_or_else(|| expected(&self.state(), connector, "scale_id"));
        self.call(Request::SetScale(connector.into(), value, id))
    }

    fn set_auto_scale(&self, enabled: bool) -> bool { self.call(Request::SetAutoScale(enabled)) }

    fn set_settings(&self, settings: &Value) -> bool {
        self.call(Request::SetSettings(json_dumps(settings, &JsonStyle::DEFAULT)))
    }

    fn refresh(&self) -> bool { self.call(Request::Refresh) }
}

pub type Dispatch = Rc<dyn Fn(Request) -> Result<bool, String>>;

struct LocalInner {
    common: Common,
    dispatch: RefCell<Option<Dispatch>>,
    notify_source: RefCell<Option<glib::SourceId>>,
}

/// The popup's client inside the service: same reply semantics as the D-Bus
/// client (asynchronous replies, `error` from the last state or failure),
/// without a self-proxy.
#[derive(Clone)]
pub struct LocalClient(Rc<LocalInner>);

impl LocalClient {
    pub fn new(on_changed: impl Fn() + 'static) -> Self {
        Self(Rc::new(LocalInner { common: Common::new(Box::new(on_changed)), dispatch: RefCell::new(None),
                                  notify_source: RefCell::new(None) }))
    }

    pub fn connect(&self, dispatch: Dispatch) {
        self.0.dispatch.replace(Some(dispatch));
    }

    /// The service emitted `Changed(encoded)` (or answered `GetState`).
    pub fn state_changed(&self, encoded: &str) {
        if self.0.common.closed.get() {
            return;
        }
        let encoded = encoded.to_owned();
        let weak = Rc::downgrade(&self.0);
        // Delivered from the main loop like a D-Bus signal, never re-entrantly.
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade() {
                if !inner.common.closed.get() {
                    inner.common.update(&encoded);
                }
            }
        });
    }

    fn call(&self, request: Request) -> bool {
        let dispatch = self.0.dispatch.borrow().clone();
        let Some(dispatch) = dispatch.filter(|_| !self.0.common.closed.get()) else {
            self.0.common.error.replace(UNAVAILABLE.into());
            if !self.0.common.closed.get() {
                self.0.common.changed();
            }
            return false;
        };
        self.0.common.begin(&request);
        let weak = Rc::downgrade(&self.0);
        glib::idle_add_local_once(move || {
            let Some(inner) = weak.upgrade() else { return };
            let reply = dispatch(request.clone());
            inner.common.finish(&request, reply);
        });
        true
    }

    pub fn close(&self) {
        self.0.common.closed.set(true);
        if let Some(source) = self.0.notify_source.take() {
            source.remove();
        }
        self.0.dispatch.replace(None);
        self.0.common.ready.set(false);
    }
}

impl UiClient for LocalClient {
    fn state(&self) -> Value { self.0.common.state.borrow().clone() }
    fn ready(&self) -> bool { self.0.common.ready.get() }
    fn error(&self) -> String { self.0.common.error.borrow().clone() }

    fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: Option<&str>) -> bool {
        let id = expected_id.map(String::from).unwrap_or_else(|| expected(&self.state(), connector, "id"));
        self.call(Request::SetValue(connector.into(), feature.into(), value, id))
    }

    fn set_auto_dim(&self, enabled: bool) -> bool { self.call(Request::SetAutoDim(enabled)) }

    fn set_scale(&self, connector: &str, value: f64, expected_id: Option<&str>) -> bool {
        let id = expected_id.map(String::from).unwrap_or_else(|| expected(&self.state(), connector, "scale_id"));
        self.call(Request::SetScale(connector.into(), value, id))
    }

    fn set_auto_scale(&self, enabled: bool) -> bool { self.call(Request::SetAutoScale(enabled)) }

    fn set_settings(&self, settings: &Value) -> bool {
        self.call(Request::SetSettings(json_dumps(settings, &JsonStyle::DEFAULT)))
    }

    fn refresh(&self) -> bool { self.call(Request::Refresh) }
}
