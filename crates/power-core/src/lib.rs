//! Power and battery logic shared by tessel-power and tessel-battery
//! (ports of `tessel-power/power_backend.py`, `power_charge_limits.py`, and,
//! with the `gtk` feature, `power_style.py` + `power_battery_ui.py`).
//!
//! * [`backend::PowerBackend`] — one state machine; tessel-power runs
//!   [`backend::Mode::Full`] (profiles, UPower, logind), tessel-battery
//!   [`backend::Mode::BatteriesOnly`] (`batteries_only=True`, UPower only).
//! * [`devices`] — UPower device normalization; [`charge_limits`] — sysfs
//!   limits + the KDE KAuth charge-threshold helper protocol.

pub mod backend;
pub mod charge_limits;
pub mod devices;
#[cfg(feature = "gtk")]
pub mod ui;

pub use backend::{BatteryModel, Mode, Options, PowerBackend, PowerSystem};
