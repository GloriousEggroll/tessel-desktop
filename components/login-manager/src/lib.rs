//! Waylogin (login-manager): greetd greeter, its IPC client, transitions and
//! process hardening. The root appearance helper (`login-settings-helper`) is
//! a separate binary that does not link this library.
pub mod desktop_fade;
pub mod hardening;
pub mod protocol;
pub mod transition;
pub mod ui;
