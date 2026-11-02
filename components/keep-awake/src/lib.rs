//! Keep Awake: a resident manual logind `idle:sleep` inhibitor with a small
//! layer-shell popup (Rust port of tessel-bluetooth/tessel-keep-awake; contract in
//! contracts/tessel-keep-awake.md).

pub mod app;
pub mod args;
pub mod backend;
pub mod client;
pub mod status;
pub mod timed;
pub mod ui;
