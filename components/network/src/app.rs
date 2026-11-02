//! `NetworkApplication` (network_manager.py) without GTK: backend + agent
//! ownership, `--check`, notifications (connected/disconnected/VPN and the
//! captive portal), `perform()` error handling and the popup's action
//! decisions. Windows live behind [`Surfaces`] (`gui.rs`), created lazily.

use crate::agent::AgentService;
use crate::backend::NetworkBackend;
use crate::model::friendly_text;
use crate::preferences::NotificationPreferences;
use gio::prelude::*;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use tessel_core::tr;

pub const APPLICATION_ID: &str = "org.tessel.NetworkManager";
/// Plain HTTP so the hotspot can intercept it and redirect to its sign-in page.
pub const FALLBACK_PORTAL_URI: &str = "http://nmcheck.gnome.org/check_network_status.txt";

/// The URI a captive portal redirects; NetworkManager's own check first.
pub fn portal_uri(connectivity_uri: &str) -> String {
    let uri = connectivity_uri.trim();
    if uri.starts_with("http://") || uri.starts_with("https://") { uri.to_owned() } else { FALLBACK_PORTAL_URI.to_owned() }
}

/// Windows owned by the GUI layer (`gui.rs`); all calls happen on the main loop.
pub trait Surfaces {
    fn refresh(&self);
    fn set_settings_error(&self, message: &str);
    fn show_popup(&self, edge: &str, offset: i64);
    fn show_settings(&self);
    fn dismiss_popup(&self);
    fn show_wifi_dialog(&self, mode: &str, network_id: Option<&str>);
    fn close(&self);
}

pub struct AppInner {
    pub gapp: gio::Application,
    pub backend: RefCell<Option<NetworkBackend>>,
    pub agent: RefCell<Option<Rc<AgentService>>>,
    pub preferences: RefCell<Option<NotificationPreferences>>,
    pub surfaces: RefCell<Option<Rc<dyn Surfaces>>>,
    pub error: RefCell<String>,
    pub stopping: Cell<bool>,
    last_connections: RefCell<Option<BTreeMap<String, (String, bool)>>>,
    last_connectivity: RefCell<Option<String>>,
    pub notifications: RefCell<Vec<(String, Option<String>)>>,
    /// Embedded facade (Global Settings' `NetworkPageApplication`): changes
    /// refresh the windows but never send connection/portal notifications,
    /// which stay with the resident network service.
    pub quiet: Cell<bool>,
}

#[derive(Clone)]
pub struct App(pub Rc<AppInner>);

impl App {
    pub fn new(gapp: &gio::Application) -> Self {
        Self(Rc::new(AppInner {
            gapp: gapp.clone(), backend: RefCell::new(None), agent: RefCell::new(None), preferences: RefCell::new(None),
            surfaces: RefCell::new(None), error: RefCell::default(), stopping: Cell::new(false),
            last_connections: RefCell::new(None), last_connectivity: RefCell::new(None), notifications: RefCell::default(),
            quiet: Cell::new(false),
        }))
    }

    pub fn downgrade(&self) -> Weak<AppInner> {
        Rc::downgrade(&self.0)
    }

    pub fn backend(&self) -> NetworkBackend {
        self.0.backend.borrow().clone().expect("backend")
    }

    pub fn surfaces(&self) -> Option<Rc<dyn Surfaces>> {
        self.0.surfaces.borrow().clone()
    }

    pub fn error(&self) -> String {
        self.0.error.borrow().clone()
    }

    /// `--check`: `{"ready","network_ready","agent_ready","devices","profiles"}`.
    pub fn health(&self) -> serde_json::Value {
        let backend = self.backend();
        let state = backend.state();
        let agent_ready = self.0.agent.borrow().as_ref().is_some_and(|a| a.ready());
        json!({"ready": state.ready && agent_ready, "network_ready": state.ready, "agent_ready": agent_ready,
               "devices": state.devices.len(), "profiles": state.profiles.len()})
    }

    pub fn changed(&self) {
        if self.0.stopping.get() {
            return;
        }
        if let Some(surfaces) = self.surfaces() {
            surfaces.refresh();
        }
        if self.0.quiet.get() {
            return;
        }
        self.notify_connections();
        self.notify_portal();
    }

    /// `_error(message)`: translated, shown in the settings window, then refresh.
    pub fn report_error(&self, message: &str) {
        let text = tessel_core::i18n::translate(message);
        self.0.error.replace(text.clone());
        if let Some(surfaces) = self.surfaces() {
            surfaces.set_settings_error(&text);
        }
        self.changed();
    }

    /// `perform(function, *args)`: clear the error, run, report raised errors.
    pub fn perform(&self, action: impl FnOnce(&NetworkBackend) -> Result<bool, String>) -> bool {
        self.0.error.replace(String::new());
        if let Some(surfaces) = self.surfaces() {
            surfaces.set_settings_error("");
        }
        match action(&self.backend()) {
            Ok(result) => {
                self.changed();
                result
            }
            Err(message) => {
                self.report_error(&friendly_text(&message, false));
                false
            }
        }
    }

    pub fn notifications_enabled(&self, event: &str) -> bool {
        match self.0.preferences.borrow().as_ref() {
            None => true,
            Some(preferences) => !preferences.get_boolean(&format!("disable-{event}-notifications")),
        }
    }

    fn send(&self, id: &str, notification: Option<gio::Notification>) {
        self.0.notifications.borrow_mut().push((id.to_owned(), notification.as_ref().map(|_| id.to_owned())));
        match notification {
            Some(n) => self.0.gapp.send_notification(Some(id), &n),
            None => self.0.gapp.withdraw_notification(id),
        }
    }

    fn notify_portal(&self) {
        let backend = self.backend();
        let state = { let s = backend.state(); if s.ready { Some(s.connectivity.clone()) } else { None } };
        let previous = self.0.last_connectivity.replace(state.clone());
        if previous == state {
            return;
        }
        if state.as_deref() != Some("Sign-in required") {
            self.send("network-captive-portal", None);
            return;
        }
        if self.notifications_enabled("portal") {
            let note = gio::Notification::new(&tr!("Sign in to network"));
            note.set_body(Some(&tr!("This network requires you to sign in before you can use the internet.")));
            note.set_icon(&gio::ThemedIcon::new("network-wireless-hotspot-symbolic"));
            note.set_default_action("app.captive-portal");
            note.add_button(&tr!("Sign in"), "app.captive-portal");
            self.send("network-captive-portal", Some(note));
        }
    }

    fn notify_connections(&self) {
        let backend = self.backend();
        let state = backend.state();
        if !state.ready {
            self.0.last_connections.replace(None);
            return;
        }
        let current: BTreeMap<String, (String, bool)> = state.active.iter()
            .filter(|(_, a)| a.state == 2 && a.kind != "loopback")
            .map(|(path, a)| (path.clone(), (a.name.clone(), a.vpn || a.kind == "wireguard"))).collect();
        drop(state);
        let previous = self.0.last_connections.replace(Some(current.clone()));
        let Some(previous) = previous else { return };
        let id = |path: &str| format!("network-{}", path.rsplit('/').next().unwrap_or(path));
        for (path, (name, vpn)) in &current {
            if !previous.contains_key(path) && self.notifications_enabled(if *vpn { "vpn" } else { "connected" }) {
                let note = gio::Notification::new(&tr!("Network connected"));
                note.set_body(Some(name));
                self.send(&id(path), Some(note));
            }
        }
        for (path, (name, vpn)) in &previous {
            if !current.contains_key(path) && self.notifications_enabled(if *vpn { "vpn" } else { "disconnected" }) {
                let note = gio::Notification::new(&tr!("Network disconnected"));
                note.set_body(Some(name));
                self.send(&id(path), Some(note));
            }
        }
    }

    /// Open the hotspot sign-in page in the default browser, without blocking.
    pub fn open_captive_portal(&self) {
        if let Some(surfaces) = self.surfaces() {
            surfaces.dismiss_popup();
        }
        self.send("network-captive-portal", None);
        let uri = portal_uri(&self.backend().state().connectivity_uri);
        let weak = self.downgrade();
        let context = crate::gui::launch_context();
        gio::AppInfo::launch_default_for_uri_async(&uri, context.as_ref(), None::<&gio::Cancellable>, move |result| {
            if let (Err(error), Some(app)) = (result, weak.upgrade()) {
                let message = crate::model::friendly_glib(&error);
                if !message.is_empty() {
                    App(app).report_error(&tr!("Could not open the sign-in page: {value1}", value1 = message));
                }
            }
        });
    }

    pub fn toggle_profile(&self, uuid: &str) {
        let profile = self.backend().state().profiles.get(uuid).cloned();
        let Some(profile) = profile else { return self.report_error(&tr!("That connection is no longer available.")) };
        if !profile.active_ids.is_empty() {
            for path in profile.active_ids {
                self.perform(|b| Ok(b.deactivate(&path)));
            }
        } else {
            let uuid = uuid.to_owned();
            self.perform(move |b| Ok(b.activate_profile(&uuid, None)));
        }
    }

    /// Popup row: disconnect an active network; connect saved/open/OWE
    /// directly; otherwise open our Wi-Fi form.
    pub fn connect_network(&self, network_id: &str) {
        let network = self.backend().state().networks.get(network_id).cloned();
        let Some(network) = network else { return self.report_error(&tr!("That wireless network is no longer available. Scan again.")) };
        if network.active {
            self.perform(|b| Ok(b.disconnect_device(&network.device_id)));
        } else if !network.profile_ids.is_empty() || network.security == "open" || network.security == "owe" {
            let id = network_id.to_owned();
            self.perform(move |b| Ok(b.connect_wifi(&id, None)));
        } else if let Some(surfaces) = self.surfaces() {
            surfaces.show_wifi_dialog("network", Some(network_id));
        }
    }

    /// Shutdown order: agent → backend → popup/children → hold release.
    pub fn shutdown(&self) {
        self.0.stopping.set(true);
        if let Some(agent) = self.0.agent.borrow().clone() {
            agent.close();
        }
        if let Some(backend) = self.0.backend.borrow().clone() {
            backend.close();
        }
        if let Some(surfaces) = self.0.surfaces.take() {
            surfaces.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_uri_prefers_networkmanager_check() {
        assert_eq!(portal_uri(" http://check.invalid/ "), "http://check.invalid/");
        assert_eq!(portal_uri("https://x"), "https://x");
        assert_eq!(portal_uri("ftp://x"), FALLBACK_PORTAL_URI);
        assert_eq!(portal_uri(""), FALLBACK_PORTAL_URI);
    }
}
