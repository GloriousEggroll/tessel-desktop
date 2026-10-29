//! Indicator sources (W-c; contract §7 "Status sources").
//!
//! - `backend`  — status_backend.py: `StatusBackend` (stream scripts, wpctl audio
//!   + scroll coalescing, BlueZ, keep-awake via `SettingsClient`, discovery via
//!   `network_discovery::backend::DiscoveryBackend`, capture, media via
//!   `media_manager::dock_status::MediaStatus`, show desktop) and the pure
//!   helpers `indicator`, `plain_text`, `audio_status`, `bluetooth_status`.
//! - `process`  — status_backend.ProcessJob (`StatusJob` trait + `JobFactory`).
//! - `capture`  — capture_status.py (`CaptureStatus`).
//! - `html`     — Python `html.unescape` + tag stripping (shared with dbus::sni).

pub mod backend;
pub mod capture;
pub mod html;
mod html_entities;
pub mod process;
