//! Battery Manager: display-only UPower battery status plus explicit charging
//! controls (port of `~/tessel-bluetooth/tessel-battery`). Owns no idle/sleep policy.
//!
//! * UPower backend (`batteries_only` mode), KDE KAuth charge limits, battery
//!   rows/charge controls and the Power style come from the shared
//!   `tessel-power-core` crate (also used by tessel-power).
//! * [`status`] — the Waybar/Tessel Dock indicator JSON (`battery_status.py`).
//! * [`cli`], [`bluetooth`], [`ui`], [`app`] — `battery_manager.py`/`battery_ui.py`.

pub mod bluetooth;
pub mod cli;
pub mod status;





pub const APPLICATION_ID: &str = "org.tessel.BatteryManager";
pub mod app;
pub mod ui;
