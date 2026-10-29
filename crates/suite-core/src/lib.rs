//! Non-GUI foundation of the desktop suite (ports of btmanager/shared).
//!
//! Start every binary with [`i18n::initialize`] before GTK initializes.

pub mod clock;
pub mod i18n;
pub mod lang;
pub mod paths;
pub mod pyfmt;
pub mod service_settings;
pub mod single_instance;

pub use i18n::{translate, translate_with};
pub use pyfmt::Arg;
