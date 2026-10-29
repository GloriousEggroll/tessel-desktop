//! Rust port of ~/btmanager/weather-manager (see contracts/weather-manager.md).

pub mod app;
pub mod backend;
pub mod compat;
pub mod dates;
pub mod http;
pub mod location;
pub mod settings;
pub mod status;
pub mod tray;
pub mod ui;
pub mod worker;

#[cfg(test)]
mod backend_tests;
