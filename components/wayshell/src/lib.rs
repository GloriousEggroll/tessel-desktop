//! Rust port of `~/btmanager/wayshell` (contracts/wayshell.md).
pub mod app;
pub mod app_catalog;
pub mod cli;
pub mod monitors;
pub mod server;
pub mod switcher;
#[cfg(feature = "desktop")]
pub mod desktop;
#[cfg(feature = "fileops")]
pub mod fileops;
#[cfg(feature = "fileops")]
pub mod wallpaper_chooser;
