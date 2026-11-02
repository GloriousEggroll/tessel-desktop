//! Tessel secure session lock.
//! See contracts/tessel-lock.md, in particular §9 Threat model.
pub mod app;
pub mod auth;
pub mod cli;
pub mod fingerprint;
pub mod notify;
pub mod palette;
pub mod session_lock;
pub mod status;
pub mod window;
