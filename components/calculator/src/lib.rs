//! Rust port of ~/btmanager/calculator (calculator.py, calculator_backend.py,
//! calculator_ui.py). See contracts/calculator.md.

pub mod app;
pub mod backend;
pub mod cli;
pub mod compat;
pub mod decimal;
pub mod ui;

pub const APPLICATION_ID: &str = "org.waydock.Calculator";

#[cfg(test)]
mod tests;
