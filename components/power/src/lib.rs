//! Rust port of ~/tessel-bluetooth/tessel-power (see contracts/tessel-power.md).
//! The power backend, charge limits and Power style live in `tessel-power-core`;
//! D-Bus transport, scheduler and Python helpers in `tessel-core`.

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
