//! Keep Awake: a resident manual logind `idle:sleep` inhibitor with a small
//! layer-shell popup (Rust port of btmanager/keep-awake; contract in
//! contracts/keep-awake.md).

pub mod app;
pub mod args;
pub mod backend;
pub mod client;
pub mod compat;
pub mod status;
pub mod timed;
pub mod ui;
