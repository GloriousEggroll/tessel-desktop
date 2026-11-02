//! System Monitor: processes, performance, storage, network and graphics (see contracts/tessel-system-monitor.md).
pub mod app;
pub mod args;
pub mod backend;
pub mod format;
pub mod procfs;
pub mod source;
pub mod ui;

#[cfg(test)]
mod tests;
