//! Tessel Keyboard (tessel-keyboard): the automatic touch keyboard for the labwc session.
//!
//! The GTK process never binds virtual-keyboard / input-method protocols; the
//! unchanged native helper `tessel-keyboard-input` (native/input.c) does, over a line
//! protocol on its stdin/stdout ([`backend`]).

pub mod backend;
pub mod config;
pub mod devices;
pub mod keyboard;
pub mod lock;
pub mod manager;
pub mod session;
pub mod settings;
pub mod state;
pub mod status;

/// Path of the helper built from native/input.c by this crate's build script.
pub const BUILT_HELPER: &str = env!("TESSEL_KEYBOARD_INPUT_BUILT");
