//! Python-exception-shaped errors.
//!
//! The Python helpers raise `ValueError`, `FileExistsError`, `OSError`,
//! `PermissionError`, `RuntimeError`, `TypeError`, `KeyError`, `CancelledError`
//! or `GLib.Error`; callers show `str(error)`. [`Error`] keeps that class as
//! [`ErrorKind`] and its `Display` is Python's `str(exception)`.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Python exception class of an [`Error`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// `ValueError` (also `UnicodeDecodeError`, `zipfile.BadZipFile`-like input errors).
    Value,
    /// `FileExistsError` (an `OSError`).
    FileExists,
    /// `FileNotFoundError` (an `OSError`).
    NotFound,
    /// `PermissionError` (an `OSError`).
    Permission,
    /// Any other `OSError`.
    Os,
    /// `RuntimeError`.
    Runtime,
    /// `TypeError`.
    Type,
    /// `KeyError` (`str()` is the repr of its message).
    Key,
    /// `file_tools.CancelledError`.
    Cancelled,
    /// `subprocess.TimeoutExpired`.
    Timeout,
    /// `GLib.Error` (`str()` is `"<domain>: <message> (<code>)"`).
    Glib,
}

/// A Python-like exception: class, message, optional errno/filename or GLib error.
#[derive(Clone, Debug)]
pub struct Error {
    pub kind: ErrorKind,
    /// The exception's message (`args[0]`, or `strerror` for errno errors).
    pub message: String,
    /// `OSError.errno`, when the error came from the OS.
    pub errno: Option<i32>,
    /// `OSError.filename`.
    pub filename: Option<PathBuf>,
    /// The original `GLib.Error`, for [`ErrorKind::Glib`].
    pub glib: Option<glib::Error>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error { kind, message: message.into(), errno: None, filename: None, glib: None }
    }
    pub fn value(message: impl Into<String>) -> Self { Self::new(ErrorKind::Value, message) }
    pub fn runtime(message: impl Into<String>) -> Self { Self::new(ErrorKind::Runtime, message) }
    pub fn os(message: impl Into<String>) -> Self { Self::new(ErrorKind::Os, message) }
    pub fn file_exists(message: impl Into<String>) -> Self { Self::new(ErrorKind::FileExists, message) }
    pub fn type_error(message: impl Into<String>) -> Self { Self::new(ErrorKind::Type, message) }
    pub fn key(message: impl Into<String>) -> Self { Self::new(ErrorKind::Key, message) }
    /// `CancelledError("Operation cancelled")`.
    pub fn cancelled() -> Self { Self::new(ErrorKind::Cancelled, "Operation cancelled") }

    /// An `OSError` subclass chosen from the errno, like CPython.
    pub fn from_io(error: &io::Error, filename: Option<&Path>) -> Self {
        let errno = error.raw_os_error();
        let kind = match errno {
            Some(libc::EEXIST) => ErrorKind::FileExists,
            Some(libc::ENOENT) => ErrorKind::NotFound,
            Some(libc::EACCES) | Some(libc::EPERM) => ErrorKind::Permission,
            _ => ErrorKind::Os,
        };
        let message = match errno {
            Some(_) => tessel_core::pyerr::strerror(error),
            None => error.to_string(),
        };
        Error { kind, message, errno, filename: filename.map(Path::to_path_buf), glib: None }
    }

    /// An `OSError` for a raw errno (`OSError(errno, strerror(errno), filename)`).
    pub fn from_errno(errno: i32, filename: Option<&Path>) -> Self {
        Self::from_io(&io::Error::from_raw_os_error(errno), filename)
    }

    /// `OSError` from the current `errno` (after a failed libc call).
    pub fn last_os(filename: Option<&Path>) -> Self {
        Self::from_io(&io::Error::last_os_error(), filename)
    }

    pub fn from_glib(error: glib::Error) -> Self {
        Error { kind: ErrorKind::Glib, message: error.message().to_string(), errno: None, filename: None,
                glib: Some(error) }
    }

    /// `isinstance(error, OSError)` (FileExistsError, PermissionError, … included).
    pub fn is_os_error(&self) -> bool {
        matches!(self.kind, ErrorKind::FileExists | ErrorKind::NotFound | ErrorKind::Permission | ErrorKind::Os)
    }

    /// `isinstance(error, ValueError)`.
    pub fn is_value_error(&self) -> bool { self.kind == ErrorKind::Value }

    /// A GLib error with this GIO code.
    pub fn matches_io(&self, code: gio::IOErrorEnum) -> bool {
        self.glib.as_ref().is_some_and(|error| error.matches(code))
    }
}

impl fmt::Display for Error {
    /// Python's `str(exception)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::Glib => match &self.glib {
                Some(error) => f.write_str(&tessel_core::pyerr::python_error_text(error)),
                None => f.write_str(&self.message),
            },
            ErrorKind::Key => f.write_str(&python_repr(&self.message)),
            _ => match self.errno {
                Some(errno) => {
                    write!(f, "[Errno {errno}] {}", self.message)?;
                    if let Some(name) = &self.filename {
                        write!(f, ": {}", python_repr(&name.to_string_lossy()))?;
                    }
                    Ok(())
                }
                None => f.write_str(&self.message),
            },
        }
    }
}

impl std::error::Error for Error {}

impl From<glib::Error> for Error {
    fn from(error: glib::Error) -> Self { Error::from_glib(error) }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self { Error::from_io(&error, None) }
}

/// Python `repr(str)` (quote choice and escapes of control characters).
pub fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => { out.push('\\'); out.push(c); }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

pub type Result<T> = std::result::Result<T, Error>;
