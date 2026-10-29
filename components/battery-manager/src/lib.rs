//! Battery Manager: display-only UPower battery status plus explicit charging
//! controls (port of `~/btmanager/battery-manager`). Owns no idle/sleep policy.
//!
//! * [`power`] — UPower backend + KDE KAuth charge-limit protocol, shared with
//!   power-manager (no GTK; candidate for a shared crate).
//! * [`power_ui`] — battery rows/charge controls and the Power style, shared
//!   with power-manager's settings window.
//! * [`status`] — the Waybar/Waydock indicator JSON (`battery_status.py`).
//! * [`cli`], [`bluetooth`], [`ui`], [`app`] — `battery_manager.py`/`battery_ui.py`.

pub mod bluetooth;
pub mod cli;
pub mod power;
pub mod status;





pub const APPLICATION_ID: &str = "org.waybar.BatteryManager";
pub mod app;
pub mod power_ui;
pub mod ui;
