//! Rust port of `~/tessel-bluetooth/way-displays` (the "Displays" GUI
//! `tessel-displays` and its colour helpers). The upstream way-displays
//! daemon is not part of this crate.
pub mod brightness;
pub mod color_config;
pub mod color_controls;
pub mod compat;
pub mod ipc;
pub mod labwc;
pub mod layout;
pub mod model;
pub mod monitor_widget;
pub mod overlay;
pub mod primary;
pub mod style;
pub mod window;

#[cfg(test)]
mod tests;
