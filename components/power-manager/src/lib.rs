//! Rust port of ~/btmanager/power-manager (see contracts/power-manager.md).

pub mod alerts;
pub mod app;
pub mod args;
pub mod backend;
pub mod bus;
pub mod charge_limits;
pub mod config;
pub mod dim;
pub mod fakes;
pub mod gui;
pub mod idle;
pub mod logind;
pub mod status;
pub mod style;
pub mod ui;
pub mod util;
pub mod values;

#[cfg(test)]
mod backend_tests;
#[cfg(test)]
mod idle_tests;
#[cfg(test)]
mod app_tests;
