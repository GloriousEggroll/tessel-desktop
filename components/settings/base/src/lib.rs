//! Shared building blocks of the Global Settings pages.
//!
//! * [`page`] — `page_api.SettingsPage`, the page host interface and search
//!   `Setting` metadata.
//! * [`style`] — `global_style.py` (page header, cards, rows, footer,
//!   empty state, Toast, choose/confirm_leave, unify_header).
//! * [`widgets`] — `page_widgets.py` (`Page`, `label`, `dropdown`, `spawn`,
//!   `status_text`, background work).
//! * [`tags`] — Python-style attributes on GTK objects (`row.page_name`).
pub mod page;
pub mod style;
pub mod tags;
pub mod widgets;

pub use page::{Host, PageHost, Setting, SettingsPage};
pub use tessel_core::tr;
