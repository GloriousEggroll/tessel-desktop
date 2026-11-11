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

/// True when a distribution package built this program (`TESSEL_PACKAGED` set
/// at build time, see distribution/fedora). Paths that other programs read
/// from generated configuration then point at `/usr` instead of the
/// `tools/install.py` locations.
pub const PACKAGED: bool = option_env!("TESSEL_PACKAGED").is_some();

/// The fixed, root-owned path of a Tessel program, for callers that must not
/// search the user's directories (the login screen, pkexec): `/usr/lib/tessel`
/// in a package, otherwise `/usr/local/lib/tessel`.
#[macro_export]
macro_rules! system_program {
    ($name:literal) => {
        if $crate::paths::PACKAGED { concat!("/usr/lib/tessel/", $name) } else { concat!("/usr/local/lib/tessel/", $name) }
    };
}

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
/// beside the running executable, then in [`program_dirs`], else the bare
/// name for a `PATH` lookup. When `$TESSEL_PROGRAM_DIR` is set (tests), only
/// that directory is searched, and a program missing there is the bare name.
pub fn program(name: &str) -> PathBuf {
    // Tests point this at a directory of fakes, instead of everything else, so
    // installed programs neither replace a fake nor hide a missing one.
    if let Some(dir) = env::var_os("TESSEL_PROGRAM_DIR").filter(|dir| !dir.is_empty()) {
        let path = PathBuf::from(dir).join(name);
        return if is_executable(&path) { path } else { PathBuf::from(name) };
    }
    let beside = env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(name)));
    beside.into_iter().chain(program_dirs().into_iter().map(|dir| dir.join(name)))
        .find(|path| is_executable(path))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// This program's own file, for running it again (helper modes). Once an
/// update replaces the file, Linux reports the running binary as
/// "<path> (deleted)"; the path without that suffix is the updated program.
/// Falls back to [`program`]`(name)` when neither exists.
pub fn own_program(name: &str) -> PathBuf {
    let exe = env::current_exe().ok().map(|exe| undeleted(&exe));
    exe.filter(|path| is_executable(path)).unwrap_or_else(|| program(name))
}

/// `path` without the kernel's " (deleted)" suffix.
pub fn undeleted(path: &std::path::Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod own_program_tests {
    use super::*;

    #[test]
    fn a_replaced_binary_runs_the_updated_file() {
        assert_eq!(undeleted(std::path::Path::new("/a/tessel-files (deleted)")), PathBuf::from("/a/tessel-files"));
        assert_eq!(undeleted(std::path::Path::new("/a/tessel-files")), PathBuf::from("/a/tessel-files"));
        // The test binary itself exists, so it is its own program.
        assert_eq!(own_program("nothing-by-this-name"), env::current_exe().unwrap());
    }
}

#[cfg(test)]
mod program_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_test_override_is_the_only_place_searched() {
        let dir = env::temp_dir().join(format!("tessel-programs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tessel-fake"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(dir.join("tessel-fake"), std::fs::Permissions::from_mode(0o755)).unwrap();
        // The test binary's own directory holds this binary; with the override
        // set, even a program that is there is not found beside the executable.
        let own = env::current_exe().unwrap();
        let own_name = own.file_name().unwrap().to_str().unwrap().to_owned();
        let saved = env::var_os("TESSEL_PROGRAM_DIR");
        env::set_var("TESSEL_PROGRAM_DIR", &dir);
        let found = program("tessel-fake");
        let missing = program(&own_name);
        match saved {
            Some(value) => env::set_var("TESSEL_PROGRAM_DIR", value),
            None => env::remove_var("TESSEL_PROGRAM_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, dir.join("tessel-fake"), "a fake in the override directory is found");
        assert_eq!(missing, PathBuf::from(&own_name), "a program missing there is the bare name, though it exists beside the executable");
    }
}
