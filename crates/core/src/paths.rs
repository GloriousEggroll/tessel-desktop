//! XDG base directories (a non-empty environment variable wins, otherwise
//! `$HOME/<default>`), Tessel's own directories under them, and where Tessel's
//! programs are installed.

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

/// `~/.config/tessel`: shared Tessel preferences (language.json, clock.json, …).
pub fn suite_config_dir() -> PathBuf { config_home().join(SUITE) }
/// `~/.local/share/tessel`: shared installed data (locale/, session/).
pub fn suite_data_dir() -> PathBuf { data_home().join(SUITE) }
/// `~/.local/state/tessel`: shared state (backups/, …).
pub fn suite_state_dir() -> PathBuf { state_home().join(SUITE) }
/// `~/.cache/tessel`.
pub fn suite_cache_dir() -> PathBuf { cache_home().join(SUITE) }
/// `~/.config/tessel/<part>`: one part's own configuration directory (`dock`, `shell`, …).
pub fn component_config_dir(part: &str) -> PathBuf { suite_config_dir().join(part_name(part)) }
/// `~/.cache/tessel/<part>`.
pub fn component_cache_dir(part: &str) -> PathBuf { suite_cache_dir().join(part_name(part)) }
/// `~/.local/state/tessel/<part>`.
pub fn component_state_dir(part: &str) -> PathBuf { suite_state_dir().join(part_name(part)) }

/// The directory name every Tessel path uses.
pub const SUITE: &str = "tessel";

/// `tessel-dock` and `dock` both name the part `dock`.
fn part_name(part: &str) -> &str { part.strip_prefix("tessel-").unwrap_or(part) }

/// Where Tessel's programs are installed, most specific first. The running
/// program's own directory comes before these (see [`program`]).
pub fn program_dirs() -> Vec<PathBuf> {
    vec![
        home().join(".local/lib").join(SUITE),
        PathBuf::from("/usr/local/lib").join(SUITE),
        PathBuf::from("/usr/lib").join(SUITE),
        PathBuf::from("/usr/libexec").join(SUITE),
    ]
}

/// Find another Tessel program (`tessel-power-status`, `tessel-dock-toplevel`):
/// beside the running executable, then in [`program_dirs`], else the bare name
/// for a `PATH` lookup.
pub fn program(name: &str) -> PathBuf {
    let beside = env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(name)));
    beside.into_iter().chain(program_dirs().into_iter().map(|dir| dir.join(name)))
        .find(|path| is_executable(path))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}
