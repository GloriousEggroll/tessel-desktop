//! Non-GUI foundation of Tessel (ports of tessel-bluetooth/shared).
//!
//! Start every binary with [`i18n::initialize`] before GTK initializes.

pub mod base64;
pub mod bus;
pub mod clock;
pub mod fsutil;
pub mod glib_compat;
pub mod i18n;
pub mod labwc_hdr;
pub mod icon_theme;
pub mod lang;
pub mod paths;
pub mod pyerr;
pub mod pyfmt;
pub mod pyjson;
pub mod scheduler;
pub mod secrets;
pub mod service_settings;
pub mod shell;
pub mod single_instance;
pub mod timer;
pub mod values;

pub use i18n::{translate, translate_with};
pub use pyfmt::Arg;
