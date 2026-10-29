//! Controls for the existing session inhibitor, independent of a settings
//! window (port of keep_awake_client.py `KeepAwakeClient`).

use crate::ui::Controls;
use gio::prelude::*;
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;
use suite_core::service_settings::{truthy, SettingsClient};
use suite_core::tr;

pub const BUS_NAME: &str = "org.waybar.KeepAwake";
pub const OBJECT_PATH: &str = "/org/waybar/KeepAwake";
pub const INTERFACE: &str = "org.waybar.KeepAwake.Controls";
pub const DURATION_INTERFACE: &str = "org.waybar.KeepAwake.Duration";

#[derive(Clone)]
pub struct KeepAwakeClient {
    client: SettingsClient,
    on_changed: Rc<dyn Fn()>,
    // Python assigns `self.error` from SetDuration failures until the next state.
    error: Rc<RefCell<Option<String>>>,
    unavailable: String,
}

impl KeepAwakeClient {
    pub fn new(on_changed: impl Fn() + 'static) -> Self {
        let on_changed: Rc<dyn Fn()> = Rc::new(on_changed);
        let error: Rc<RefCell<Option<String>>> = Rc::default();
        let unavailable = tr!("Keep Awake is not running in this session.");
        let (notify, reset) = (on_changed.clone(), error.clone());
        let client = SettingsClient::new(move || { reset.replace(None); notify() }, BUS_NAME, INTERFACE, &unavailable);
        Self { client, on_changed, error, unavailable }
    }

    pub fn start(&self) { self.client.start() }
    pub fn close(&self) { self.client.close() }
    pub fn settings(&self) -> &SettingsClient { &self.client }
    pub fn pending(&self) -> bool { self.flag("pending") }

    fn flag(&self, key: &str) -> bool { self.client.state().get(key).is_some_and(truthy) }

    /// Raw `SetDuration(u)` on the Duration interface of the settings proxy's
    /// object (`NO_AUTO_START`, 15 s), exactly like the Python client.
    pub fn request_duration(&self, minutes: u32) -> bool {
        let Some(proxy) = self.client.proxy().filter(|proxy| proxy.name_owner().is_some()) else {
            self.error.replace(Some(self.unavailable.clone()));
            (self.on_changed)();
            return false;
        };
        let (error, notify) = (self.error.clone(), self.on_changed.clone());
        let name = proxy.name().map(|name| name.to_string()).unwrap_or_else(|| BUS_NAME.into());
        proxy.connection().call(Some(name.as_str()), &proxy.object_path(), DURATION_INTERFACE, "SetDuration", Some(&(minutes,).to_variant()),
                        None, gio::DBusCallFlags::NO_AUTO_START, 15000, None::<&gio::Cancellable>, move |result| {
            if let Err(mut failure) = result {
                gio::DBusError::strip_remote_error(&mut failure);
                error.replace(Some(failure.message().to_owned()));
                notify();
            }
        });
        true
    }
}

impl Controls for KeepAwakeClient {
    fn ready(&self) -> bool { self.client.ready() }
    fn active(&self) -> bool { self.flag("active") }
    fn requested(&self) -> bool { self.flag("requested") }
    fn error(&self) -> String { self.error.borrow().clone().unwrap_or_else(|| self.client.error()) }
    fn duration(&self) -> u32 {
        match self.client.state().get("duration") {
            Some(Value::Number(n)) if n.is_u64() => n.as_u64().unwrap_or(0) as u32,
            _ => 0,
        }
    }
    fn until(&self) -> Option<f64> {
        match self.client.state().get("until") {
            Some(Value::Number(n)) => n.as_f64(),
            _ => None,
        }
    }
    fn set_enabled(&self, enabled: bool) { self.client.call("SetEnabled", Some(&(enabled,).to_variant()), None); }
    fn set_duration(&self, minutes: u32) { self.request_duration(minutes); }
}
