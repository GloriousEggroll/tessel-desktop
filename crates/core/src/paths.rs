//! XDG base directories exactly as the Python components compute them:
//! a non-empty environment variable wins, otherwise `$HOME/<default>`.

use std::env;
use std::path::PathBuf;

/// `Path.home()`: `$HOME`, falling back to the password database.
pub fn home() -> PathBuf {
    match env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => glib::home_dir(),
    }
}

fn xdg(variable: &str, default: &str) -> PathBuf {
    match env::var_os(variable) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home().join(default),
    }
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home() -> PathBuf { xdg("XDG_CONFIG_HOME", ".config") }
/// `$XDG_CACHE_HOME` or `~/.cache`.
pub fn cache_home() -> PathBuf { xdg("XDG_CACHE_HOME", ".cache") }
/// `$XDG_DATA_HOME` or `~/.local/share`.
pub fn data_home() -> PathBuf { xdg("XDG_DATA_HOME", ".local/share") }
/// `$XDG_STATE_HOME` or `~/.local/state`.
pub fn state_home() -> PathBuf { xdg("XDG_STATE_HOME", ".local/state") }

/// `$XDG_RUNTIME_DIR`, else GLib's fallback (`g_get_user_runtime_dir`).
pub fn runtime_dir() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => glib::user_runtime_dir(),
    }
}

/// `~/.config/tessel`: shared suite preferences (language.json, clock.json, …).
pub fn suite_config_dir() -> PathBuf { config_home().join("tessel") }
/// `~/.local/share/tessel`: shared installed data (locale/, session/).
pub fn suite_data_dir() -> PathBuf { data_home().join("tessel") }
/// `~/.config/<component>`: a component's own configuration directory.
pub fn component_config_dir(component: &str) -> PathBuf { config_home().join(component) }
/// `~/.cache/<component>`.
pub fn component_cache_dir(component: &str) -> PathBuf { cache_home().join(component) }
/// `~/.local/state/<component>`.
pub fn component_state_dir(component: &str) -> PathBuf { state_home().join(component) }
