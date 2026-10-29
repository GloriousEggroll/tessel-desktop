//! btmanager (Rust port of btmanager.py, btpopup.py, bttransfer.py).
pub mod args;
pub mod backend;
pub mod dbus;
pub mod env;
pub mod i18n;
pub mod manager;
pub mod selftest;
pub mod settings;
#[doc(hidden)]
pub mod testing;
pub mod transfer;
pub mod ui;
pub mod app;
