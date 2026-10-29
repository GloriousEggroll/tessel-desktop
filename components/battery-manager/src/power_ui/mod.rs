//! GTK parts shared with power-manager: `power_style.py` and
//! `power_battery_ui.py` (battery rows, UPower protection switch, the global
//! KAuth "Maximum charge" control). Candidate for a shared crate.

pub mod battery_ui;
pub mod style;
