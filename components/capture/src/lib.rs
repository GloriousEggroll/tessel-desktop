//! Screenshot & Screen Recorder (`tessel-capture`, GApplication `org.tessel.Capture`).
//! Rust port of `~/tessel-bluetooth/tessel-capture`; see contracts/tessel-capture.md.

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

pub const ID: &str = "org.tessel.Capture";
