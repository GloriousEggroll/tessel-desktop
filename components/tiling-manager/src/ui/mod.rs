//! GTK surfaces: the shortcut popup (tiling_ui.py) and the shared
//! immediate-apply controls (tiling_settings.py). GTK is initialised only here.

pub mod popup;
pub mod settings;

pub use popup::run;
