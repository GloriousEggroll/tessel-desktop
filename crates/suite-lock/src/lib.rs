//! Shared lock-screen / login-manager code.
//!
//! * [`config`] — `lock-screen/lock_config.py` (used by lock-screen, Waylogin,
//!   the Kasumi screen and Global Settings → Lock Screen).
//! * [`login_model`] — `login-manager/login_model.py` (Waylogin, the root
//!   appearance helper and Global Settings → Login Screen).
//!
//! These moved here from `gs-desktop` (which re-exports them) so the wave-5
//! components reuse a single implementation.
pub mod config;
pub mod login_model;
