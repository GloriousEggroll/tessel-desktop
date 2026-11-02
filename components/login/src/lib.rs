//! Tessel Login (tessel-login): greetd greeter, its IPC client, transitions and
//! process hardening. The root appearance helper (`tessel-login-helper`) is
//! a separate binary that does not link this library.
pub mod desktop_fade;
pub mod hardening;
pub mod protocol;
pub mod transition;
pub mod ui;
