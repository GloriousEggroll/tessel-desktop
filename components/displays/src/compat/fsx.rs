//! File helpers with Python semantics: `tempfile.mkstemp`, text-mode reads
//! (universal newlines), `str.splitlines` and `OSError` text.

use std::fs::File;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tessel_core::pyerr::{os_error_path, os_error_text};

/// `tempfile.mkstemp(prefix=prefix, dir=directory)`: a new 0600 file.
pub fn mkstemp(directory: &Path, prefix: &str) -> io::Result<(PathBuf, File)> {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    for attempt in 0..10000u32 {
        let mut seed = glib::random_int() ^ attempt.wrapping_mul(2654435761);
        let mut name = String::from(prefix);
        for _ in 0..8 {
            name.push(CHARS[(seed % CHARS.len() as u32) as usize] as char);
            seed = seed / CHARS.len() as u32 ^ glib::random_int();
        }
        let path = directory.join(name);
        match std::fs::OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from_raw_os_error(libc::EEXIST))
}

pub type Replace<'a> = &'a dyn Fn(&Path, &Path) -> io::Result<()>;

pub fn os_replace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

/// Python `str(OSError)` for a two-path call (`os.replace`).
pub fn replace_error(error: &io::Error, from: &Path, to: &Path) -> String {
    match error.raw_os_error() {
        Some(_) => format!("{}: '{}' -> '{}'", os_error_text(error), from.display(), to.display()),
        None => error.to_string(),
    }
}

/// mkstemp + fchmod(mode) + write + fsync + os.replace; the temporary is
/// removed on any failure. Errors are Python `str(OSError)`.
pub fn atomic_write_with(path: &Path, data: &[u8], mode: u32, prefix: &str, replace: Replace) -> Result<(), String> {
    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory).map_err(|e| os_error_path(&e, directory))?;
    let (temporary, mut file) = mkstemp(directory, prefix).map_err(|e| os_error_path(&e, directory))?;
    let result = (|| {
        file.set_permissions(std::fs::Permissions::from_mode(mode)).map_err(|e| os_error_text(&e))?;
        file.write_all(data).map_err(|e| os_error_text(&e))?;
        file.flush().map_err(|e| os_error_text(&e))?;
        file.sync_all().map_err(|e| os_error_text(&e))?;
        drop(file);
        replace(&temporary, path).map_err(|e| replace_error(&e, &temporary, path))
    })();
    if temporary.exists() && result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Python text-mode read (`open(path, "r", encoding="utf-8").read()`):
/// universal newlines translate `\r\n` and `\r` to `\n`.
pub fn read_text(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(universal_newlines(&text))
}

pub fn universal_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Text error: decode failures are Python `UnicodeDecodeError` (a ValueError),
/// I/O errors are `OSError` text with the path.
pub fn read_error(error: &io::Error, path: &Path) -> String {
    if error.kind() == io::ErrorKind::InvalidData {
        return format!("'utf-8' codec can't decode file '{}'", path.display());
    }
    os_error_path(error, path)
}

/// `str.splitlines()`.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let breaks = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if breaks {
            lines.push(&text[start..index]);
            let mut end = index + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = chars.peek() {
                    chars.next();
                    end += 1;
                }
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// File permission bits (`st_mode & 0o777`).
pub fn mode_of(path: &Path) -> io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\nb\r\nc\rd\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("x\n\ny"), vec!["x", "", "y"]);
        assert_eq!(universal_newlines("a\r\nb\rc"), "a\nb\nc");
    }
}
