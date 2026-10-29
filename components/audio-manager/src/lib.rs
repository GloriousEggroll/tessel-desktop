//! Audio Settings, the Waybar/dock volume popup, volume-key OSD and the
//! persistent PipeWire loopback service — Rust port of ~/btmanager/audio-manager.

pub mod api;
pub mod app;
pub mod backend;
pub mod cli;
pub mod fake;
pub mod graph;
pub mod level;
pub mod model;
pub mod service;
pub mod store;
pub mod supervisor;
pub mod timer;
pub mod ui;
pub mod volume_keys;
pub mod wp;

#[cfg(test)]
pub(crate) mod testing;
