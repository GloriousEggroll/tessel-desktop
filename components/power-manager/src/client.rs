//! Control the resident power policy without starting another idle
//! controller (port of power_client.py `PowerClient`, used by Global
//! Settings' Power page).

use gio::prelude::*;
use serde_json::{Map, Value};
use std::rc::Rc;
use suite_core::service_settings::SettingsClient;
use suite_core::tr;

pub const BUS_NAME: &str = "org.waybar.PowerManager";
pub const INTERFACE: &str = "org.waybar.PowerManager.Controls";

/// `callback(ok, error)` of [`PowerClient::set_settings`].
pub type Done = Box<dyn FnOnce(bool, String)>;

#[derive(Clone)]
pub struct PowerClient {
    client: SettingsClient,
    on_changed: Rc<dyn Fn()>,
}

impl PowerClient {
    pub fn new(on_changed: impl Fn() + 'static) -> Self {
        let on_changed: Rc<dyn Fn()> = Rc::new(on_changed);
        let notify = on_changed.clone();
        let client = SettingsClient::new(move || notify(), BUS_NAME, INTERFACE,
                                         &tr!("Power management is not running in this session."));
        Self { client, on_changed }
    }

    pub fn start(&self) { self.client.start() }
    pub fn close(&self) { self.client.close() }
    pub fn settings_client(&self) -> &SettingsClient { &self.client }
    pub fn state(&self) -> Map<String, Value> { self.client.state() }
    pub fn ready(&self) -> bool { self.client.ready() }
    pub fn error(&self) -> String { self.client.error() }
    pub fn busy(&self) -> bool { self.client.busy() }

    /// `set_settings(settings, callback)`: `SetSettings(json.dumps(settings))`;
    /// `busy` while the call is in flight. Returns false when not sent (the
    /// callback has then already run with `(false, unavailable)`).
    pub fn set_settings(&self, settings: &Value, callback: Option<Done>) -> bool {
        let text = suite_core::pyfmt::json_dumps(settings, &suite_core::pyfmt::JsonStyle::DEFAULT);
        let (client, notify) = (self.client.clone(), self.on_changed.clone());
        self.client.set_busy(true);
        (self.on_changed)();
        self.client.call("SetSettings", Some(&(text,).to_variant()), Some(Box::new(move |ok, error, _values| {
            client.set_busy(false);
            notify();
            if let Some(callback) = callback {
                callback(ok, error);
            }
        })))
    }
}
