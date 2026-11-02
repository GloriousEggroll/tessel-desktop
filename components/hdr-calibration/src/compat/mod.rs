//! Helpers missing from tessel-core / way-displays (upstream candidates):
//! `etree` (ElementTree parse → indent → tostring round trip), `lock`
//! (way-displays' private `color_config::locked`), `resolve` (non-strict
//! `Path.resolve()`), `iso_utc_now`, `time_ns`.
pub mod etree;

use std::path::{Path, PathBuf};

/// Advisory lock shared with color_config.py: `flock(.<name>.color.lock)`.
pub struct Lock(std::fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub fn locked(path: &Path) -> Result<Lock, String> {
    use std::os::fd::AsRawFd;
    let parent = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(parent).map_err(|e| tessel_core::pyerr::os_error_path(&e, parent))?;
    let name = format!(".{}.color.lock", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let lock_path = parent.join(name);
    let file = std::fs::OpenOptions::new().append(true).create(true).open(&lock_path)
        .map_err(|e| tessel_core::pyerr::os_error_path(&e, &lock_path))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(tessel_core::pyerr::os_error_text(&std::io::Error::last_os_error()));
    }
    Ok(Lock(file))
}

/// `Path.resolve()` (non-strict).
pub fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => resolve(parent).join(name),
        _ => absolute,
    }
}

pub fn time_ns() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// `datetime.now(timezone.utc).isoformat()`.
pub fn iso_utc_now() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let Ok(time) = glib::DateTime::from_unix_utc(now.as_secs() as i64) else { return String::new() };
    let base = time.format("%Y-%m-%dT%H:%M:%S").map(|s| s.to_string()).unwrap_or_default();
    let micros = now.subsec_micros();
    if micros == 0 { format!("{base}+00:00") } else { format!("{base}.{micros:06}+00:00") }
}
