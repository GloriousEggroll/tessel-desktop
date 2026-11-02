//! Tessel media controls (see contracts/tessel-media.md),
//! with the MPRIS client (lock_media + media_session), player icons, album
//! art and Tessel Dock's MediaStatus that tessel-dock/tessel-lock will share.

pub mod app;
pub mod art;
pub mod compat;
pub mod dock_status;
pub mod http;
pub mod icons;
pub mod mpris;
pub mod ui;
