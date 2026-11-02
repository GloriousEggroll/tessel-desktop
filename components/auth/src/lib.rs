//! polkitman: Tessel's Polkit authentication agent (GTK4), which
//! also hosts the BlueZ pairing agent. Rust port of
//! ~/tessel-bluetooth/tessel-auth (see contracts/tessel-auth.md).

pub mod agent;
pub mod app;
pub mod bluetooth;
pub mod ffi;
pub mod identity;
pub mod listener;
pub mod secret;
pub mod session;
pub mod window;
