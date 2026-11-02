//! Global Settings pages that host the desktop applets' own controls
//! (applet_pages.py, weather_page.py, language_page.py, capture_page.py), plus the
//! Rust-only Archive Manager, Clipboard, Passwords and System Information pages.
//!
//! Each page owns only its display subscriptions: persistent power policy,
//! idle inhibition, notifications and Bluetooth stay in their resident
//! services, even after Global Settings closes. Every page is built from the
//! embedded component's own Rust crate.
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gs_base::{Host, SettingsPage};

pub mod embed;
pub mod services;

mod archive;
mod audio;
mod battery;
mod bluetooth;
mod capture;
mod clipboard;
mod datetime;
mod keepawake;
mod language;
mod network;
mod notifications;
mod passwords;
mod power;
mod system_info;
mod weather;

pub use embed::{embed, Embed};
pub use services::*;

pub use archive::ArchivePage;
pub use audio::AudioPage;
pub use battery::BatteryPage;
pub use bluetooth::BluetoothPage;
pub use capture::CapturePage;
pub use clipboard::ClipboardPage;
pub use datetime::DatePage;
pub use keepawake::KeepAwakePage;
pub use language::LanguagePage;
pub use network::{NetworkPage, FACADE_ID as NETWORK_FACADE_ID};
pub use notifications::NotificationsPage;
pub use passwords::PasswordsPage;
pub use power::PowerPage;
pub use system_info::{SystemInfo, SystemInfoPage, VERSION as TESSEL_VERSION};
pub use weather::WeatherPage;

/// Sections built here, in the order of `applet_pages.BUILDERS` followed by
/// the stand-alone page modules.
pub const SECTIONS: &[&str] = &["audio", "bluetooth", "network", "power", "battery", "keepawake", "notifications",
                                 "datetime", "weather", "language", "capture", "archive", "clipboard", "passwords",
                                 "systeminfo"];

/// `applet_pages.BUILDERS` keys (the sections `applet_pages.build_page` accepts).
pub const APPLET_SECTIONS: &[&str] = &["audio", "bluetooth", "network", "power", "battery", "datetime", "keepawake",
                                       "notifications"];

pub type Changed = Rc<dyn Fn()>;
pub type Failed = Rc<dyn Fn(&str)>;

/// Python's `factories` dict: dependency injection for GUI checks. `None`
/// means the real service client / backend.
#[derive(Default)]
pub struct Factories {
    /// `factories["audio"](changed, failed)`.
    pub audio: Option<Box<dyn Fn(Changed, Failed) -> Rc<dyn tessel_audio::api::AudioApi>>>,
    /// `factories["datetime"](changed, failed)`.
    pub datetime: Option<Box<dyn Fn(Changed, Failed) -> Rc<dyn tessel_calendar::backend::DateSystem>>>,
    /// `factories["battery"](changed, failed)` (default `PowerBackend(…, batteries_only=True)`).
    pub battery: Option<Box<dyn Fn(Changed, Failed) -> Rc<dyn BatterySource>>>,
    /// `factories["bluetooth"]()`: the BlueZ model (default: the system bus).
    pub bluetooth: Option<Box<dyn Fn() -> Result<Rc<tessel_bluetooth::backend::Bt>, String>>>,
    /// Services handed to the embedded Bluetooth UI (receive-mode storage…).
    pub bluetooth_services: Option<tessel_bluetooth::manager::Services>,
    /// `factories["network"](changed, error)`.
    pub network: Option<Box<dyn Fn(Changed, Failed) -> tessel_network::backend::NetworkBackend>>,
    /// `factories["keepawake"](changed)`.
    pub keepawake: Option<Box<dyn Fn(Changed) -> Rc<dyn KeepAwakeService>>>,
    /// `factories["power_backend"](changed, failed)`.
    pub power_backend: Option<Box<dyn Fn(Changed, Failed) -> Rc<dyn tessel_power_core::backend::PowerSystem>>>,
    /// `factories["power"](changed)` (default `PowerClient`).
    pub power: Option<Box<dyn Fn(Changed) -> Rc<dyn PowerPolicy>>>,
    /// `factories["notifications"](changed)`.
    pub notifications: Option<Box<dyn Fn(Changed) -> Rc<dyn NotificationService>>>,
    /// Weather store and services (default: the user's weather config).
    pub weather_store: Option<Arc<tessel_weather::backend::ConfigStore>>,
    pub weather_services: Option<tessel_weather::settings::Services>,
    /// `language_page.LanguageBackend` (tests patch it).
    pub language: Option<Box<dyn Fn() -> Arc<dyn tessel_language::ui::WizardBackend>>>,
    /// `capture_page.build_page(section, host, path)`.
    pub capture_path: Option<PathBuf>,
    /// The Archive Manager settings file (default `~/.config/tessel/tessel-archive.json`).
    pub archive_path: Option<PathBuf>,
    /// The clipboard settings file (default `~/.config/tessel/clipboard.json`).
    pub clipboard_path: Option<PathBuf>,
    /// The filesystem System Information reads `/etc`, `/proc` and `/sys` from (default `/`).
    pub system_root: Option<PathBuf>,
    /// How System Information starts System Monitor (default a gio subprocess).
    pub system_launch: Option<system_info::Launch>,
    /// Restart Desktop Shell runner (tests pass a fake).
    pub system_runner: Option<system_info::Runner>,
}

/// Build one page lazily with the real services.
pub fn build_page(section: &str, host: &Host) -> Result<SettingsPage, String> {
    build_page_with(section, host, &Factories::default())
}

/// `build_page(section, host_window, factories=…)`.
pub fn build_page_with(section: &str, host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    match section {
        "audio" => audio::build(host, factories),
        "bluetooth" => bluetooth::build(host, factories),
        "network" => network::build(host, factories),
        "power" => power::build(host, factories),
        "battery" => battery::build(host, factories),
        "datetime" => datetime::build(host, factories),
        "keepawake" => keepawake::build(host, factories),
        "notifications" => notifications::build(host, factories),
        "weather" => weather::build(host, factories),
        "language" => language::build(host, factories),
        "capture" => capture::build(host, factories),
        "archive" => archive::build(host, factories),
        "clipboard" => clipboard::build(host, factories),
        "passwords" => passwords::build(host, factories),
        "systeminfo" => system_info::build(host, factories),
        other => Err(format!("Unknown settings section: {other}")),
    }
}

/// The host's application (Python `host.get_application()`).
pub(crate) fn application(host: &Host) -> Result<gtk::Application, String> {
    host.application().ok_or_else(|| "Global Settings has no application".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn sections_cover_every_builder_once() {
        assert_eq!(SECTIONS, &["audio", "bluetooth", "network", "power", "battery", "keepawake", "notifications",
                               "datetime", "weather", "language", "capture", "archive", "clipboard", "passwords",
                               "systeminfo"]);
        let mut unique = SECTIONS.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), SECTIONS.len());
        assert!(APPLET_SECTIONS.iter().all(|s| SECTIONS.contains(s)));
        assert_eq!(APPLET_SECTIONS.len(), 8);
    }

    #[test]
    fn facade_and_timer_contract() {
        assert_eq!(NETWORK_FACADE_ID, "org.tessel.GlobalSettings.NetworkDialogs");
        assert_eq!(datetime::TICK_SECONDS, 15);
    }

    #[test]
    fn power_client_reports_unavailable_without_the_service() {
        // Never started: no proxy, so nothing is sent on any bus.
        let changes = Rc::new(RefCell::new(0));
        let counter = changes.clone();
        let client = tessel_power::client::PowerClient::new(move || *counter.borrow_mut() += 1);
        let replies: Rc<RefCell<Vec<(bool, String)>>> = Rc::default();
        let sink = replies.clone();
        let sent = PowerPolicy::set_settings(&client, &serde_json::json!({"lock_after": 600}),
                                             Box::new(move |ok, error| sink.borrow_mut().push((ok, error))));
        assert!(!sent);
        assert!(!client.busy());
        let replies = replies.borrow();
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].0);
        assert_eq!(replies[0].1, "Power management is not running in this session.");
        assert!(*changes.borrow() >= 2, "busy and idle both notify");
        assert!(!PowerPolicy::ready(&client));
        assert_eq!(PowerPolicy::settings(&client), None);
    }
}
