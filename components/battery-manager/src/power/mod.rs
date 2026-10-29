//! Battery/power-source logic shared with power-manager (ports of
//! `power-manager/power_backend.py` in `batteries_only=True` mode and
//! `power_charge_limits.py`). Kept free of GTK so the indicator stream binary
//! links only GLib/GIO, and laid out like power-manager's port (gio [`bus::Bus`]
//! trait, injectable [`scheduler::Scheduler`], Python-dict-like `serde_json`
//! values) so it can be upstreamed into a shared crate later.

pub mod backend;
pub mod bus;
pub mod charge_limits;
pub mod devices;
pub mod scheduler;
pub mod values;

pub use backend::{BatteryModel, PowerBackend};
