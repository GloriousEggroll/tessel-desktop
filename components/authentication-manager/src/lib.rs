//! polkitman: the desktop suite's Polkit authentication agent (GTK4), which
//! also hosts the BlueZ pairing agent. Rust port of
//! ~/btmanager/authentication-manager (see contracts/authentication-manager.md).

pub mod agent;
pub mod app;
pub mod bluetooth;
pub mod ffi;
pub mod identity;
pub mod listener;
pub mod secret;
pub mod session;
pub mod window;
