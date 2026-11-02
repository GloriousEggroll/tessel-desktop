//! Rust port of ~/tessel-bluetooth/tessel-brightness (see contracts/tessel-brightness.md).

pub mod app;
pub mod args;
pub mod backend;
pub mod client;
pub mod compat;
pub mod core;
pub mod guard;
pub mod gui;
pub mod idle;
pub mod keys;
pub mod launch;
pub mod model;
pub mod scaling;
pub mod status;
pub mod style;
pub mod ui;

#[cfg(test)]
mod backend_tests;
#[cfg(test)]
mod core_tests;
#[cfg(test)]
mod idle_tests;
#[cfg(test)]
mod keys_tests;
#[cfg(test)]
mod scaling_tests;
