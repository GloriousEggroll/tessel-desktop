//! tessel-power/power_client.py: control the resident power policy without
//! starting another idle controller (only what Lock Screen needs).
use std::rc::Rc;

use serde_json::{Map, Value};
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::service_settings::SettingsClient;
use tessel_core::tr;

pub const NAME: &str = "org.tessel.PowerManager";
pub const INTERFACE: &str = "org.tessel.PowerManager.Controls";

/// `done(ok, error)` of `set_settings`.
pub type Done = Box<dyn FnOnce(bool, String)>;

/// What LockSettings uses of `PowerClient` (tests substitute a fake).
pub trait LockPower {
    fn ready(&self) -> bool;
    fn state(&self) -> Map<String, Value>;
    fn error(&self) -> String;
    fn start(&self);
    fn close(&self);
    fn set_settings(&self, settings: &Map<String, Value>, done: Done);
}

/// `PowerClient(on_changed)`.
pub struct PowerClient {
    client: SettingsClient,
    on_changed: Rc<dyn Fn()>,
}

impl PowerClient {
    pub fn new(on_changed: Rc<dyn Fn()>) -> Self {
        let notify = on_changed.clone();
        let client = SettingsClient::new(move || notify(), NAME, INTERFACE,
                                         &tr!("Power management is not running in this session."));
        Self { client, on_changed }
    }
}

impl LockPower for PowerClient {
    fn ready(&self) -> bool { self.client.ready() }
    fn state(&self) -> Map<String, Value> { self.client.state() }
    fn error(&self) -> String { self.client.error() }
    fn start(&self) { self.client.start() }
    fn close(&self) { self.client.close() }
    fn set_settings(&self, settings: &Map<String, Value>, done: Done) {
        let client = self.client.clone();
        let on_changed = self.on_changed.clone();
        self.client.set_busy(true);
        (self.on_changed)();
        let payload = json_dumps(&Value::Object(settings.clone()), &JsonStyle::DEFAULT);
        use glib::prelude::ToVariant;
        self.client.call("SetSettings", Some(&(payload,).to_variant()), Some(Box::new(move |ok, error, _values| {
            client.set_busy(false);
            on_changed();
            done(ok, error);
        })));
    }
}

pub type PowerFactory = Rc<dyn Fn(Rc<dyn Fn()>) -> Rc<dyn LockPower>>;

pub fn default_factory() -> PowerFactory {
    Rc::new(|changed| Rc::new(PowerClient::new(changed)) as Rc<dyn LockPower>)
}
