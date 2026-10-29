//! Shared GTK4 helpers of the desktop suite (ports of btmanager/shared GTK
//! modules and the Waydock palette theme). Re-exports `gtk4` as `gtk`.

pub use gtk;
pub use gtk4_layer_shell as layer_shell;

pub mod app_menu;
pub mod blur;
pub mod osd;
pub mod palette;
pub mod placement;
pub mod popup;
pub mod theme;
pub mod utils;
