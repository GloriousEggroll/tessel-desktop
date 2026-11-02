//! HDR Calibration (see contracts/tessel-hdr-calibration.md). The native `hdr-pattern`
//! test-pattern client is a separate C helper, found with `tessel_core::paths::program`.
pub mod app;
pub mod args;
pub mod calibration;
pub mod compat;
pub mod correction;
pub mod ui;

#[cfg(test)]
mod tests;
