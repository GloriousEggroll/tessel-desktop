//! Rust port of `~/tessel-bluetooth/tessel-hdr-calibration` (see contracts/tessel-hdr-calibration.md).
//! The native `hdr-pattern` test-pattern client stays the C helper (hdr-pattern.c).
pub mod app;
pub mod args;
pub mod calibration;
pub mod compat;
pub mod correction;
pub mod ui;

#[cfg(test)]
mod tests;
