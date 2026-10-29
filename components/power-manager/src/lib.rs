//! Rust port of ~/btmanager/power-manager (see contracts/power-manager.md).
//! The power backend, charge limits and Power style live in `suite-power`;
//! D-Bus transport, scheduler and Python helpers in `suite-core`.

pub mod alerts;
pub mod app;
pub mod args;
pub mod client;
pub mod config;
pub mod dim;
pub mod fakes;
pub mod gui;
pub mod idle;
pub mod logind;
pub mod status;
pub mod ui;

#[cfg(test)]
mod idle_tests;
#[cfg(test)]
mod app_tests;
