//! Owned D-Bus surfaces (W-c; contract §2, §3).
//!
//! - `settings_api` — `org.tessel.Settings` at `/org/tessel/Settings` (settings_api.py),
//!   driven through the `SettingsHost` trait the application implements.
//! - `sni`          — StatusNotifier watcher + host backend (status_tray.py `StatusTrayBackend`).
//! - `dbusmenu`     — `com.canonical.dbusmenu` client → `gio::Menu` + `gio::SimpleActionGroup`
//!   (dbusmenu.py `RemoteMenu`, model side; the PopoverMenu is the widget's job).
//! - `readiness`    — sd_notify `READY=1` (service_readiness.py).

pub mod dbusmenu;
pub mod readiness;
pub mod settings_api;
pub mod sni;
