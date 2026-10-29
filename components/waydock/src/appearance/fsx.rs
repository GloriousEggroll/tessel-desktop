//! Small Python-compatible file and text helpers shared by the appearance
//! publishers (`tempfile.mkstemp`, `os.replace`, universal-newline
//! `read_text`, `str.splitlines`, `shlex.split(comments=True)`, `OSError` text).

use std::fs::File;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub use suite_core::pyerr::{os_error_path, os_error_text};

const NAME_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";

fn random_name() -> String {
    (0..8).map(|_| NAME_CHARS[glib::random_int_range(0, NAME_CHARS.len() as i32) as usize] as char).collect()
}

/// `tempfile.mkstemp(prefix, suffix, dir)`: an exclusive 0600 file.
pub fn mkstemp(directory: &Path, prefix: &str, suffix: &str) -> io::Result<(File, PathBuf)> {
    let mut last = None;
    for _ in 0..100 {
        let path = directory.join(format!("{prefix}{}{suffix}", random_name()));
        match std::fs::OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(PathError::wrap(error, &path)),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("No usable temporary file name found")))
}

/// An `io::Error` that remembers Python's `filename` (and `filename2`).
#[derive(Debug)]
pub struct PathError {
    pub source: io::Error,
    pub path: PathBuf,
    pub path2: Option<PathBuf>,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let base = os_error_path(&self.source, &self.path);
        match &self.path2 {
            Some(other) if self.source.raw_os_error().is_some() => write!(f, "{base} -> '{}'", other.display()),
            _ => f.write_str(&base),
        }
    }
}

impl std::error::Error for PathError {}

impl PathError {
    pub fn wrap(source: io::Error, path: &Path) -> io::Error {
        let kind = source.kind();
        io::Error::new(kind, PathError { source, path: path.to_owned(), path2: None })
    }

    pub fn wrap2(source: io::Error, path: &Path, other: &Path) -> io::Error {
        let kind = source.kind();
        io::Error::new(kind, PathError { source, path: path.to_owned(), path2: Some(other.to_owned()) })
    }
}

/// Python's `str(OSError)`: errno text plus the remembered filename(s), or
/// the plain message of a custom error.
pub fn error_text(error: &io::Error) -> String {
    if let Some(inner) = error.get_ref() {
        return inner.to_string();
    }
    os_error_text(error)
}

/// `os.replace` with the Python error text (`'src' -> 'dst'`).
pub fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination).map_err(|e| PathError::wrap2(e, source, destination))
}

/// `Path.read_bytes()` with Python's error text.
pub fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| PathError::wrap(e, path))
}

/// `Path.read_text(encoding="utf-8")`: strict UTF-8 and universal newlines.
pub fn read_text(path: &Path) -> io::Result<String> {
    let data = read_bytes(path)?;
    let text = String::from_utf8(data).map_err(|e| {
        let position = e.utf8_error().valid_up_to();
        let byte = e.as_bytes()[position];
        io::Error::new(io::ErrorKind::InvalidData,
                       format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: invalid start byte"))
    })?;
    Ok(universal_newlines(&text))
}

/// `\r\n` and lone `\r` become `\n` (Python text-mode reads).
pub fn universal_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `str.splitlines()` (no keepends): every Unicode line boundary, `\r\n` as one.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if is_line_break(c) {
            lines.push(&text[start..index]);
            let mut end = index + c.len_utf8();
            if c == '\r' {
                if let Some((_, '\n')) = chars.peek() {
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

/// `bytes.splitlines(keepends=True)`: only `\n`, `\r` and `\r\n` split.
pub fn split_bytes_keepends(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < data.len() {
        match data[index] {
            b'\n' => {
                lines.push(&data[start..=index]);
                start = index + 1;
            }
            b'\r' => {
                let end = if data.get(index + 1) == Some(&b'\n') { index + 2 } else { index + 1 };
                lines.push(&data[start..end]);
                start = end;
                index = end - 1;
            }
            _ => {}
        }
        index += 1;
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

/// `bytes.replace(old, new)` (all non-overlapping occurrences, left to right).
pub fn replace_bytes(data: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    if old.is_empty() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index..].starts_with(old) {
            out.extend_from_slice(new);
            index += old.len();
        } else {
            out.push(data[index]);
            index += 1;
        }
    }
    out
}

/// `bytes.rindex(needle)`.
pub fn rfind_bytes(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > data.len() {
        return None;
    }
    (0..=data.len() - needle.len()).rev().find(|&i| data[i..].starts_with(needle))
}

/// `shlex.split(text, comments=True)` (POSIX mode); errors are shlex's.
pub fn shlex_split(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut token = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    let whitespace = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
    while let Some(c) = chars.next() {
        match c {
            c if whitespace(c) => {
                if in_word {
                    words.push(std::mem::take(&mut token));
                    in_word = false;
                }
            }
            '#' => {
                // A comment runs to the end of the line, also inside a word.
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
                if in_word {
                    words.push(std::mem::take(&mut token));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        None => return Err("No closing quotation".into()),
                        Some('\'') => break,
                        Some(q) => token.push(q),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        None => return Err("No closing quotation".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            None => return Err("No escaped character".into()),
                            Some(q @ ('"' | '\\')) => token.push(q),
                            Some(q) => {
                                token.push('\\');
                                token.push(q);
                            }
                        },
                        Some(q) => token.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    None => return Err("No escaped character".into()),
                    Some(q) => token.push(q),
                }
            }
            c => {
                in_word = true;
                token.push(c);
            }
        }
    }
    if in_word {
        words.push(token);
    }
    Ok(words)
}

/// Python's `round()` (half to even) for display/clamping of numbers.
pub fn py_round(value: f64) -> f64 {
    suite_gtk::palette::round_half_even(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\r\nb\rc\nd\u{2028}e\x0b"), ["a", "b", "c", "d", "e"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("x\n\n"), ["x", ""]);
        assert_eq!(split_bytes_keepends(b"a\r\nb\rc\nd"), [&b"a\r\n"[..], b"b\r", b"c\n", b"d"]);
        assert_eq!(split_bytes_keepends(b"a\x0bb"), [&b"a\x0bb"[..]]);
    }

    #[test]
    fn shlex_matches_python() {
        assert_eq!(shlex_split("'HDMI-A-1'").unwrap(), ["HDMI-A-1"]);
        assert_eq!(shlex_split("DP-1 # note").unwrap(), ["DP-1"]);
        assert_eq!(shlex_split("a#b c").unwrap(), ["a"]);
        assert_eq!(shlex_split("$(touch /never-execute-this)").unwrap(), ["$(touch", "/never-execute-this)"]);
        assert_eq!(shlex_split(r#""a\"b\x""#).unwrap(), [r#"a"b\x"#]);
        assert_eq!(shlex_split("''").unwrap(), [""]);
        assert_eq!(shlex_split("'open").unwrap_err(), "No closing quotation");
        assert_eq!(shlex_split("x\\").unwrap_err(), "No escaped character");
    }

    #[test]
    fn byte_helpers() {
        assert_eq!(replace_bytes(b"aXbXc", b"X", b""), b"abc");
        assert_eq!(rfind_bytes(b"<a></a></a>", b"</a>"), Some(7));
        assert_eq!(rfind_bytes(b"x", b"</a>"), None);
    }
}
