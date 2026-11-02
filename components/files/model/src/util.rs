//! Small Python-stdlib equivalents shared by the modules: `urllib.parse.quote`
//! / `unquote`, `os.path.expanduser`, `shutil.which(name, path=…)`,
//! `shlex.split` (with its errors) and `str.isspace`.

use std::ffi::{CStr, CString};
use std::path::Path;

/// `urllib.parse.quote(text, safe=safe)` (UTF-8, uppercase hex). Letters,
/// digits and `_.-~` are always safe.
pub fn quote(text: &str, safe: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let c = byte as char;
        if byte.is_ascii_alphanumeric() || b"_.-~".contains(&byte) || (byte < 0x80 && safe.contains(c)) {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `urllib.parse.unquote(text)` (UTF-8 with `errors="replace"`).
pub fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &bytes[index + 1..index + 3];
            if hex.iter().all(|b| b.is_ascii_hexdigit()) {
                let hex = std::str::from_utf8(hex).unwrap_or("0");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn passwd_home(name: Option<&str>) -> Option<String> {
    // SAFETY: getpwnam/getpwuid return static storage or NULL; copied immediately.
    unsafe {
        let entry = match name {
            Some(name) => {
                let name = CString::new(name).ok()?;
                libc::getpwnam(name.as_ptr())
            }
            None => libc::getpwuid(libc::getuid()),
        };
        if entry.is_null() || (*entry).pw_dir.is_null() {
            return None;
        }
        Some(CStr::from_ptr((*entry).pw_dir).to_string_lossy().into_owned())
    }
}

/// `os.path.expanduser(path)` (POSIX).
pub fn expanduser(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_owned();
    }
    let end = path[1..].find('/').map(|i| i + 1).unwrap_or(path.len());
    let home = if end == 1 {
        match std::env::var("HOME") {
            Ok(home) => home,
            Err(_) => match passwd_home(None) {
                Some(home) => home,
                None => return path.to_owned(),
            },
        }
    } else {
        match passwd_home(Some(&path[1..end])) {
            Some(home) => home,
            None => return path.to_owned(),
        }
    };
    let joined = format!("{}{}", home.trim_end_matches('/'), &path[end..]);
    if joined.is_empty() {
        "/".into()
    } else {
        joined
    }
}

fn executable_file(path: &Path) -> bool {
    tessel_core::shell::is_executable(path)
}

/// `shutil.which(name, path=path)`: `path=None` uses `$PATH` (else `os.defpath`).
pub fn which(name: &str, path: Option<&str>) -> Option<String> {
    if name.contains('/') {
        return executable_file(Path::new(name)).then(|| name.to_owned());
    }
    let search = match path {
        Some(path) => path.to_owned(),
        None => std::env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".into()),
    };
    if search.is_empty() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for directory in search.split(':') {
        if !seen.insert(directory) {
            continue;
        }
        // os.path.join(directory, name)
        let candidate = if directory.is_empty() || directory.ends_with('/') { format!("{directory}{name}") } else { format!("{directory}/{name}") };
        if executable_file(Path::new(&candidate)) {
            return Some(candidate);
        }
    }
    None
}

/// Python `str.isspace()` for one character.
pub fn is_py_space(c: char) -> bool {
    tessel_core::pyjson::is_py_space(c)
}

/// `shlex.split(text)` (POSIX mode, no comments). `Err` is the `ValueError` text.
pub fn shlex_split(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    let space = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(q) => current.push(q),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(next @ ('"' | '\\')) => current.push(next),
                            Some(next) => {
                                current.push('\\');
                                current.push(next);
                            }
                            None => return Err("No closing quotation".into()),
                        },
                        Some(q) => current.push(q),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(next) => current.push(next),
                    None => return Err("No escaped character".into()),
                }
            }
            c if space(c) => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// `GLib.uri_parse_scheme(text) is not None`.
pub fn has_uri_scheme(text: &str) -> bool {
    glib::Uri::parse_scheme(text).is_some()
}

/// `re.match(r"^[A-Za-z][A-Za-z0-9+.-]*" + suffix, text)`.
pub fn starts_with_scheme(text: &str, suffix: &str) -> bool {
    let mut chars = text.char_indices();
    match chars.next() {
        Some((_, c)) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for (index, c) in chars {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-') {
            continue;
        }
        return text[index..].starts_with(suffix);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_unquote_expanduser_shlex() {
        assert_eq!(quote("My folder #1", ""), "My%20folder%20%231");
        assert_eq!(quote("file:///tmp/a b", ""), "file%3A%2F%2F%2Ftmp%2Fa%20b");
        assert_eq!(quote("a/b", "/"), "a/b");
        assert_eq!(unquote("My%20f"), "My f");
        assert_eq!(unquote("100%"), "100%");
        assert_eq!(unquote("%zz%41"), "%zzA");
        assert_eq!(unquote("%E2%98%83"), "☃");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expanduser("~"), home.trim_end_matches('/'));
        assert_eq!(expanduser("~/Doc"), format!("{}/Doc", home.trim_end_matches('/')));
        assert_eq!(expanduser("~no-such-user-xyz/a"), "~no-such-user-xyz/a");
        assert_eq!(expanduser("a~"), "a~");
        assert_eq!(shlex_split("kitty --class \"Files terminal\"").unwrap(), ["kitty", "--class", "Files terminal"]);
        assert_eq!(shlex_split("a ''").unwrap(), ["a", ""]);
        assert_eq!(shlex_split("'unterminated").unwrap_err(), "No closing quotation");
        assert!(shlex_split("trailing\\").is_err());
        assert!(starts_with_scheme("sftp://x", "://"));
        assert!(!starts_with_scheme("1sftp://x", "://"));
        assert!(starts_with_scheme("not:a", ":"));
        assert!(!starts_with_scheme("not a URI", ":"));
    }
}
