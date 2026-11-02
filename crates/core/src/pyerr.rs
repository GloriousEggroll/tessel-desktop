//! Python exception text, for errors the Python components print or show verbatim.

use glib::translate::ToGlibPtr;
use std::path::Path;

/// PyGObject's `str(GLib.Error)`: `"<domain>: <message> (<code>)"`.
pub fn python_error_text(error: &glib::Error) -> String {
    let raw: *const glib::ffi::GError = error.to_glib_none().0;
    let code = unsafe { (*raw).code };
    format!("{}: {} ({code})", error.domain().as_str(), error.message())
}

/// `exc.strerror or str(exc)` of an `OSError` (`"No such file or directory"`).
pub fn strerror(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned(),
        None => error.to_string(),
    }
}

/// Python's `str(OSError)` without a filename: `"[Errno 2] No such file or directory"`.
pub fn os_error_text(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("[Errno {code}] {}", strerror(error)),
        None => error.to_string(),
    }
}

/// Python's `str(OSError)` with a filename: `"[Errno 2] No such file or directory: '/x'"`.
pub fn os_error_path(error: &std::io::Error, path: &Path) -> String {
    match error.raw_os_error() {
        Some(_) => format!("{}: '{}'", os_error_text(error), path.display()),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_and_glib_error_text() {
        let missing = std::io::Error::from_raw_os_error(2);
        assert_eq!(strerror(&missing), "No such file or directory");
        assert_eq!(os_error_text(&missing), "[Errno 2] No such file or directory");
        assert_eq!(os_error_path(&missing, Path::new("/x")), "[Errno 2] No such file or directory: '/x'");
        assert_eq!(os_error_text(&std::io::Error::other("plain")), "plain");
        let error = glib::Error::new(gio::IOErrorEnum::NotFound, "gone");
        assert_eq!(python_error_text(&error), "g-io-error-quark: gone (1)");
    }
}
