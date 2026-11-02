//! What the pages need from the resident-service clients (the Python pages
//! duck-typed `start()`, `close()`, `ready`, `state` on whatever the factory
//! returned). Implemented by the real clients here and by test fakes.
use serde_json::Value;

/// `KeepAwakeClient(changed)`: the popup's [`tessel_keep_awake::ui::Controls`] plus
/// its subscription lifecycle.
pub trait KeepAwakeService: tessel_keep_awake::ui::Controls {
    fn start(&self);
    fn close(&self);
}

impl KeepAwakeService for tessel_keep_awake::client::KeepAwakeClient {
    fn start(&self) { tessel_keep_awake::client::KeepAwakeClient::start(self) }
    fn close(&self) { tessel_keep_awake::client::KeepAwakeClient::close(self) }
}

/// `NotificationClient(changed)`.
pub trait NotificationService: tessel_notifications::ui::Client {
    fn start(&self);
    fn close(&self);
}

impl NotificationService for tessel_notifications::client::NotificationClient {
    fn start(&self) { tessel_notifications::client::NotificationClient::start(self) }
    fn close(&self) { tessel_notifications::client::NotificationClient::close(self) }
}

/// `PowerBackend(changed, failed, batteries_only=True)` as the battery page uses it.
pub trait BatterySource: tessel_power_core::BatteryModel {
    fn start(&self);
    fn close(&self);
}

impl BatterySource for tessel_power_core::backend::PowerBackend {
    fn start(&self) { tessel_power_core::backend::PowerSystem::start(self) }
    fn close(&self) { tessel_power_core::backend::PowerSystem::close(self) }
}

/// Reply of [`PowerPolicy::set_settings`]: `(ok, error)`.
pub type SaveReply = Box<dyn FnOnce(bool, String)>;

/// `PowerClient(changed)`: the resident power policy service.
pub trait PowerPolicy {
    fn ready(&self) -> bool;
    fn error(&self) -> String;
    /// `client.state.get("settings")`.
    fn settings(&self) -> Option<Value>;
    /// `set_settings(settings, callback)`; false when the call was not sent.
    fn set_settings(&self, settings: &Value, callback: SaveReply) -> bool;
    fn start(&self);
    fn close(&self);
}

impl PowerPolicy for tessel_power::client::PowerClient {
    fn ready(&self) -> bool { tessel_power::client::PowerClient::ready(self) }
    fn error(&self) -> String { tessel_power::client::PowerClient::error(self) }
    fn settings(&self) -> Option<Value> { self.state().get("settings").cloned() }
    fn set_settings(&self, settings: &Value, callback: SaveReply) -> bool {
        tessel_power::client::PowerClient::set_settings(self, settings, Some(callback))
    }
    fn start(&self) { tessel_power::client::PowerClient::start(self) }
    fn close(&self) { tessel_power::client::PowerClient::close(self) }
}
