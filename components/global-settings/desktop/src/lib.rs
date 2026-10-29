//! Global Settings desktop pages (desktop_pages.py, dock_pages.py,
//! layout_page.py + layout_presets.py, lock_page.py, login_page.py).
//!
//! The shell calls [`build_page`] for each key in [`SECTIONS`]; Welcome uses
//! [`interface_settings`]. Every page module also has a `build_*_page`
//! variant taking Python's keyword arguments (fakes for tests).
pub mod bridge;
pub mod displays;
pub mod dock;
pub mod layout;
pub mod layout_presets;
pub mod lock;
pub mod login;
pub mod theme;
pub mod util;
pub mod workspaces;

#[cfg(test)]
mod tests;

use gs_base::{Host, SettingsPage};

pub use theme::interface_settings;

pub const SECTIONS: &[&str] = &["displays", "theme", "dock", "behavior", "layout", "lock", "login"];

pub fn build_page(section: &str, host: &Host) -> Result<SettingsPage, String> {
    match section {
        "displays" => Ok(displays::build_displays_page(host, displays::DisplaysOptions::default())),
        "theme" => theme::build_theme_page(host, theme::ThemeOptions::default()),
        "dock" | "behavior" => dock::build_dock_page(section, host),
        "layout" => layout::build_layout_page(host, layout::LayoutOptions::default()),
        "lock" => lock::build_lock_page(host, lock::LockOptions::default()),
        "login" => login::build_login_page(host, login::LoginOptions::default()),
        _ => Err(format!("Unknown desktop settings page: {section}")),
    }
}
