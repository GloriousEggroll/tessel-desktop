//! Screenshot & Screen Recorder (`waycapture`, GApplication `org.waydock.Capture`).
//! Rust port of `~/btmanager/capture-manager`; see contracts/capture-manager.md.

pub mod app;
pub mod area;
pub mod args;
pub mod check;
pub mod clipboard;
pub mod compat;
pub mod editor;
pub mod imaging;
pub mod media;
pub mod native;
pub mod notifications;
pub mod popups;
pub mod portal;
pub mod process;
pub mod results;
pub mod settings;
pub mod shortcuts;
pub mod tray;
pub mod ui;
pub mod vulkan;

pub const ID: &str = "org.waydock.Capture";
