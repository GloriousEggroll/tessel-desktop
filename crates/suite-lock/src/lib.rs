//! Shared lock-screen / login-manager code.
//!
//! * [`config`] — `lock-screen/lock_config.py` (used by lock-screen, Waylogin,
//!   the Kasumi screen and Global Settings → Lock Screen).
//! * [`login_model`] — `login-manager/login_model.py` (Waylogin, the root
//!   appearance helper and Global Settings → Login Screen).
//! * [`voice_state`] — `lock-screen/voice_state.py` (plain Rust).
//! * [`palette`] — `lock-screen/lock_palette.py` (plain Rust).
//! * With feature `gtk`: [`visuals`] (lock_visuals.py), [`weather`]
//!   (lock_weather.py), [`media`] (lock_media.py, lock_media_icons.py,
//!   lock_media_art.py — re-exported from media-manager's library),
//!   [`info`] (lock_info.py) and [`dashboard`] (lock_dashboard.py).
//!   See `WIDGETS_READY` for the API summary.
//!
//! `config`/`login_model` moved here from `gs-desktop` (which re-exports them)
//! so the wave-5 components reuse a single implementation.
pub mod config;
pub mod login_model;
pub mod palette;
pub mod voice_state;

#[cfg(feature = "gtk")]
pub mod dashboard;
#[cfg(feature = "gtk")]
pub mod info;
#[cfg(feature = "gtk")]
pub mod media;
#[cfg(feature = "gtk")]
pub mod visuals;
#[cfg(feature = "gtk")]
pub mod weather;

use std::path::{Path, PathBuf};

/// `pathlib.Path.expanduser()`: `~` and `~/…` → `$HOME` (pwd fallback),
/// `~user/…` → that user's home; anything else unchanged.
pub fn expanduser(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix('~') else { return path.to_path_buf() };
    let (user, tail) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index + 1..]),
        None => (rest, ""),
    };
    let home = if user.is_empty() {
        suite_core::paths::home()
    } else {
        let Ok(name) = std::ffi::CString::new(user) else { return path.to_path_buf() };
        // SAFETY: getpwnam returns a pointer into static storage or NULL.
        unsafe {
            let entry = libc::getpwnam(name.as_ptr());
            if entry.is_null() || (*entry).pw_dir.is_null() {
                return path.to_path_buf();
            }
            PathBuf::from(std::ffi::CStr::from_ptr((*entry).pw_dir).to_string_lossy().into_owned())
        }
    };
    if tail.is_empty() { home } else { home.join(tail) }
}
