//! Rust port of ~/btmanager/calendar-manager (see contracts/calendar-manager.md).

pub mod civil;
pub mod events;
pub mod http;
pub mod preferences;
pub mod py;
pub mod store;
pub mod tz;
pub mod backend;
pub mod fake;
#[cfg(test)]
mod backend_tests;
pub mod app;
pub mod args;
pub mod popup;
pub mod settings;
pub mod view;
