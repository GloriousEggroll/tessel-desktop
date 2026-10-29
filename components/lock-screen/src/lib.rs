//! Wayshell secure session lock (Rust port of ~/btmanager/lock-screen).
//! See contracts/lock-screen.md, in particular §9 Threat model.
pub mod app;
pub mod auth;
pub mod cli;
pub mod fingerprint;
pub mod notify;
pub mod palette;
pub mod session_lock;
pub mod status;
pub mod window;
