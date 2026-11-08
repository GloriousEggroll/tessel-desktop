//! Displays: the `tessel-displays` window, its colour helpers and the daemon
//! (`tessel-displays --daemon`) that applies the layout.
pub mod brightness;
pub mod color_config;
pub mod color_controls;
pub mod compat;
pub mod daemon;
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
