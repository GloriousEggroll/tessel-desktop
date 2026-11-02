//! Rust port of tessel-bluetooth/tessel-session: on-demand session controls
//! (lock, switch user, log out, sleep, restart, power off) for the dock's
//! session button and the `--menu` keyboard shortcut / power button.

pub mod app;
pub mod args;
pub mod backend;
pub mod bus;
pub mod end;
pub mod windows;

#[cfg(test)]
mod backend_tests;
#[cfg(test)]
mod end_tests;
