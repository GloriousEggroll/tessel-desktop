//! What the pages need from the resident-service clients (the Python pages
//! duck-typed `start()`, `close()`, `ready`, `state` on whatever the factory
//! returned). Implemented by the real clients here and by test fakes.
use serde_json::Value;

/// `KeepAwakeClient(changed)`: the popup's [`keep_awake::ui::Controls`] plus
/// its subscription lifecycle.
pub trait KeepAwakeService: keep_awake::ui::Controls {
    fn start(&self);
    fn close(&self);
}

impl KeepAwakeService for keep_awake::client::KeepAwakeClient {
    fn start(&self) { keep_awake::client::KeepAwakeClient::start(self) }
    fn close(&self) { keep_awake::client::KeepAwakeClient::close(self) }
}

/// `NotificationClient(changed)`.
pub trait NotificationService: notification_manager::ui::Client {
    fn start(&self);
    fn close(&self);
}

impl NotificationService for notification_manager::client::NotificationClient {
    fn start(&self) { notification_manager::client::NotificationClient::start(self) }
    fn close(&self) { notification_manager::client::NotificationClient::close(self) }
}

/// `PowerBackend(changed, failed, batteries_only=True)` as the battery page uses it.
pub trait BatterySource: suite_power::BatteryModel {
    fn start(&self);
    fn close(&self);
}

impl BatterySource for suite_power::backend::PowerBackend {
    fn start(&self) { suite_power::backend::PowerSystem::start(self) }
    fn close(&self) { suite_power::backend::PowerSystem::close(self) }
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

impl PowerPolicy for power_manager::client::PowerClient {
    fn ready(&self) -> bool { power_manager::client::PowerClient::ready(self) }
    fn error(&self) -> String { power_manager::client::PowerClient::error(self) }
    fn settings(&self) -> Option<Value> { self.state().get("settings").cloned() }
    fn set_settings(&self, settings: &Value, callback: SaveReply) -> bool {
        power_manager::client::PowerClient::set_settings(self, settings, Some(callback))
    }
    fn start(&self) { power_manager::client::PowerClient::start(self) }
    fn close(&self) { power_manager::client::PowerClient::close(self) }
}
