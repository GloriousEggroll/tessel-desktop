//! Rust port of ~/tessel-bluetooth/calculator (calculator.py, calculator_backend.py,
//! calculator_ui.py). See contracts/calculator.md.

pub mod app;
pub mod backend;
pub mod cli;
pub mod compat;
pub mod decimal;
pub mod ui;

pub const APPLICATION_ID: &str = "org.tessel.Calculator";

#[cfg(test)]
mod tests;
