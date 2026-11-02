//! Calculator: the decimal backend, the GTK4 window and the command line.
//! See contracts/calculator.md.

pub mod app;
pub mod backend;
pub mod cli;
pub mod compat;
pub mod decimal;
pub mod ui;

pub const APPLICATION_ID: &str = "org.tessel.Calculator";

#[cfg(test)]
mod tests;
