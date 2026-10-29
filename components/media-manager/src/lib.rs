//! Rust port of ~/btmanager/media-manager (see contracts/media-manager.md),
//! with the MPRIS client (lock_media + media_session), player icons, album
//! art and Waydock's MediaStatus that waydock/lock-screen will share.

pub mod app;
pub mod art;
pub mod compat;
pub mod dock_status;
pub mod http;
pub mod icons;
pub mod mpris;
pub mod ui;
