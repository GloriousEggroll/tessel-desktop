//! Indicator sources (W-c; contract §7 "Status sources").
//!
//! - `backend`  — status_backend.py: `StatusBackend` (status stream programs, wpctl audio
//!   + scroll coalescing, BlueZ, tessel-keep-awake via `SettingsClient`, discovery via
//!   `tessel_network_discovery::backend::DiscoveryBackend`, capture, media via
//!   `tessel_media::dock_status::MediaStatus`, show desktop) and the pure
//!   helpers `indicator`, `plain_text`, `audio_status`, `bluetooth_status`.
//! - `process`  — status_backend.ProcessJob (`StatusJob` trait + `JobFactory`).
//! - `capture`  — capture_status.py (`CaptureStatus`).
//! - `capslock` — the `tessel-capslock-status` stream.
//! - `html`     — Python `html.unescape` + tag stripping (shared with dbus::sni).

pub mod backend;
pub mod capslock;
pub mod capture;
pub mod html;
mod html_entities;
pub mod process;
