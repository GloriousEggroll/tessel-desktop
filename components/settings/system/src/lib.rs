//! Global Settings system pages (Python: input_page, keyboard_page,
//! shortcuts_page, accessibility_page, default_apps_page, night_light_page,
//! users_page, about_page) and the shared labwc configuration editors
//! (labwc_settings, workspace_config, input_config, keyboard_config).
use gs_base::{Host, SettingsPage};

pub mod compat;
pub mod labwc_settings;
pub mod workspace_config;
pub mod workspace_settings;
pub mod input_config;
pub mod input_devices;
pub mod mouse_polling;
pub mod keyboard_config;
mod compat_mo;
pub mod shortcuts_model;
pub mod night_light;
pub mod users_backend;
pub mod about_page;
pub mod users_page;
pub mod night_light_page;
pub mod default_apps_page;
pub mod accessibility_page;
pub mod keyboard_page;
pub mod shortcuts_page;
pub mod mouse_polling_ui;
pub mod input_page;
mod testutil;

pub const SECTIONS: &[&str] = &["input", "keyboard", "shortcuts", "accessibility", "defaultapps", "nightlight", "users"];

/// `<module>.build_page(section, host_window)` for every system section.
/// Each page also has a `build_page_with(…)` taking the test injections the
/// Python constructors took (fake runners, devices, backends).
pub fn build_page(section: &str, host: &Host) -> Result<SettingsPage, String> {
    Ok(match section {
        "input" => input_page::build_page_with(host, None, None, None),
        "keyboard" => keyboard_page::build_page_with(None, None, None),
        "shortcuts" => shortcuts_page::build_page_with(None),
        "accessibility" => accessibility_page::build_page_with(None),
        "defaultapps" => default_apps_page::build_page_with(None),
        "nightlight" => night_light_page::build_page_with(None, None, None, None),
        "users" => users_page::build_page_with(None, None),
        other => return Err(other.to_owned()),
    })
}
