//! Python `os.path` / `pathlib` / `urllib` semantics the ported modules rely on.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// `os.path.normpath` for POSIX paths (keeps exactly two leading slashes).
pub fn normpath(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return b".".to_vec();
    }
    let initial = if path.starts_with(b"/") {
        if path.starts_with(b"//") && !path.starts_with(b"///") { 2 } else { 1 }
    } else {
        0
    };
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in path.split(|&b| b == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        if part == b".." {
            if initial == 0 && (parts.is_empty() || parts.last() == Some(&&b".."[..])) {
                parts.push(part);
            } else if !parts.is_empty() {
                parts.pop();
            }
            continue;
        }
        parts.push(part);
    }
    let mut out = vec![b'/'; initial];
    out.extend(parts.join(&b'/'));
    if out.is_empty() { b".".to_vec() } else { out }
}

fn cwd() -> Vec<u8> {
    std::env::current_dir().map(|p| p.into_os_string().into_vec()).unwrap_or_else(|_| b"/".to_vec())
}

/// `os.path.abspath`.
pub fn abspath(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let joined = if bytes.starts_with(b"/") {
        bytes.to_vec()
    } else {
        let mut base = cwd();
        base.push(b'/');
        base.extend_from_slice(bytes);
        base
    };
    PathBuf::from(OsString::from_vec(normpath(&joined)))
}

/// `pathlib.Path(path).absolute()`: joined with the cwd, `.` parts and repeated
/// or trailing slashes removed, `..` kept.
pub fn pure_absolute(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let joined = if bytes.starts_with(b"/") {
        bytes.to_vec()
    } else {
        let mut base = cwd();
        base.push(b'/');
        base.extend_from_slice(bytes);
        base
    };
    let initial = if joined.starts_with(b"//") && !joined.starts_with(b"///") { 2 } else { 1 };
    let parts: Vec<&[u8]> = joined.split(|&b| b == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let mut out = vec![b'/'; initial];
    out.extend(parts.join(&b'/'));
    PathBuf::from(OsString::from_vec(out))
}

/// `os.path.expanduser` for a leading `~` (only the current user's `~`).
pub fn expanduser(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    if bytes == b"~" || bytes.starts_with(b"~/") {
        let home = suite_core::paths::home();
        let mut out = home.into_os_string().into_vec();
        while out.len() > 1 && out.ends_with(b"/") {
            out.pop();
        }
        out.extend_from_slice(&bytes[1..]);
        if out.is_empty() {
            out.push(b'/');
        }
        return PathBuf::from(OsString::from_vec(out));
    }
    path.to_path_buf()
}

/// `os.path.realpath` (non-strict): resolve symlinks as far as components exist.
pub fn realpath(path: &Path) -> PathBuf {
    let absolute = abspath(path);
    let mut resolved: Vec<u8> = Vec::new();
    let mut pending: Vec<Vec<u8>> = absolute.as_os_str().as_bytes().split(|&b| b == b'/')
        .filter(|p| !p.is_empty()).map(<[u8]>::to_vec).rev().collect();
    let mut hops = 0;
    while let Some(part) = pending.pop() {
        if part == b"." {
            continue;
        }
        if part == b".." {
            if let Some(index) = resolved.iter().rposition(|&b| b == b'/') {
                resolved.truncate(index);
            }
            continue;
        }
        let mut candidate = resolved.clone();
        candidate.push(b'/');
        candidate.extend_from_slice(&part);
        let candidate_path = PathBuf::from(OsString::from_vec(candidate.clone()));
        match std::fs::read_link(&candidate_path) {
            Ok(target) if hops < 40 => {
                hops += 1;
                let target = target.into_os_string().into_vec();
                if target.starts_with(b"/") {
                    resolved.clear();
                }
                for piece in target.split(|&b| b == b'/').filter(|p| !p.is_empty()).rev() {
                    pending.push(piece.to_vec());
                }
            }
            _ => resolved = candidate,
        }
    }
    if resolved.is_empty() {
        resolved.push(b'/');
    }
    PathBuf::from(OsString::from_vec(resolved))
}

/// `os.path.commonpath([a, b])` for two absolute, normalized paths.
pub fn commonpath(a: &Path, b: &Path) -> PathBuf {
    let left: Vec<&[u8]> = a.as_os_str().as_bytes().split(|&c| c == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let right: Vec<&[u8]> = b.as_os_str().as_bytes().split(|&c| c == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let common: Vec<&[u8]> = left.iter().zip(right.iter()).take_while(|(x, y)| x == y).map(|(x, _)| *x).collect();
    let mut out = b"/".to_vec();
    out.extend(common.join(&b'/'));
    PathBuf::from(OsString::from_vec(out))
}

/// `os.path.dirname`.
pub fn dirname(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let index = bytes.iter().rposition(|&b| b == b'/').map_or(0, |i| i + 1);
    let mut head = &bytes[..index];
    if !head.is_empty() && head.iter().any(|&b| b != b'/') {
        while head.ends_with(b"/") {
            head = &head[..head.len() - 1];
        }
    }
    PathBuf::from(OsStr::from_bytes(head))
}

/// `os.path.basename`.
pub fn basename(path: &Path) -> &OsStr {
    let bytes = path.as_os_str().as_bytes();
    let index = bytes.iter().rposition(|&b| b == b'/').map_or(0, |i| i + 1);
    OsStr::from_bytes(&bytes[index..])
}

/// `os.path.splitext` of a basename.
#[allow(dead_code)]
pub fn splitext(name: &str) -> (&str, &str) {
    if let Some(dot) = name.rfind('.') {
        if name[..dot].chars().any(|c| c != '.') {
            return (&name[..dot], &name[dot..]);
        }
    }
    (name, "")
}

/// Python 3.14 `PurePath.suffix` (`"x."` → `"."`, `".bashrc"` → `""`).
pub fn path_suffix(name: &OsStr) -> OsString {
    let bytes = name.as_bytes();
    let stripped = &bytes[bytes.iter().position(|&b| b != b'.').unwrap_or(bytes.len())..];
    match stripped.iter().rposition(|&b| b == b'.') {
        Some(index) => OsStr::from_bytes(&stripped[index..]).to_os_string(),
        None => OsString::new(),
    }
}

/// `urllib.parse.quote_from_bytes(bytes, safe="/")`.
pub fn quote(bytes: &[u8], safe: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) || safe.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `pathlib.Path(path).as_uri()` for an absolute path.
pub fn as_uri(path: &Path) -> String {
    let bytes = path.as_os_str().as_bytes();
    if bytes.starts_with(b"//") && !bytes.starts_with(b"///") {
        // Python keeps a POSIX double-slash root as "file:////…".
        return format!("file://{}", quote(bytes, b"/"));
    }
    format!("file://{}", quote(bytes, b"/"))
}

/// `urllib.parse.unquote(text, errors="strict")`; `None` for invalid UTF-8.
pub fn unquote_strict(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).ok()
}

/// `str.splitlines()` (no keepends).
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let breaks = matches!(ch, '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}'
            | '\u{85}' | '\u{2028}' | '\u{2029}');
        if breaks {
            lines.push(&text[start..index]);
            let mut end = index + ch.len_utf8();
            if ch == '\r' {
                if let Some((next, '\n')) = chars.peek().copied() {
                    chars.next();
                    end = next + 1;
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

/// First `count` code points (`text[:count]`).
pub fn prefix_chars(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// Matches `^[A-Za-z][A-Za-z0-9+.-]*://`.
pub fn has_uri_scheme_slashes(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.first().is_none_or(|b| !b.is_ascii_alphabetic()) {
        return false;
    }
    let mut index = 1;
    while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || b"+.-".contains(&bytes[index])) {
        index += 1;
    }
    bytes[index..].starts_with(b"://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_path_helpers() {
        assert_eq!(normpath(b"//a/../b/./c//"), b"//b/c");
        assert_eq!(normpath(b"///a"), b"/a");
        assert_eq!(normpath(b"a/../../b"), b"../b");
        assert_eq!(splitext("x."), ("x", "."));
        assert_eq!(splitext(".bashrc"), (".bashrc", ""));
        assert_eq!(splitext("..foo"), ("..foo", ""));
        assert_eq!(splitext("a.tar.gz"), ("a.tar", ".gz"));
        assert_eq!(splitext(".a.b"), (".a", ".b"));
        assert_eq!(path_suffix(OsStr::new("x.")), ".");
        assert_eq!(path_suffix(OsStr::new("...")), "");
        assert_eq!(path_suffix(OsStr::new(".a.b")), ".b");
        assert_eq!(as_uri(Path::new("/a/$(x); one~_.-é+,=@!*'.txt")),
                   "file:///a/%24%28x%29%3B%20one~_.-%C3%A9%2B%2C%3D%40%21%2A%27.txt");
        assert_eq!(as_uri(Path::new("//a/b")), "file:////a/b");
        assert_eq!(splitlines("a\r\nb\rc\n\nd"), vec!["a", "b", "c", "", "d"]);
        assert_eq!(splitlines("a\n"), vec!["a"]);
        assert_eq!(unquote_strict("a%20b%C3%A9").as_deref(), Some("a bé"));
        assert_eq!(unquote_strict("%ff"), None);
        assert_eq!(unquote_strict("100%"), Some("100%".into()));
        assert_eq!(commonpath(Path::new("/a/b"), Path::new("/a/bc")), PathBuf::from("/a"));
        assert_eq!(dirname(Path::new("/a/b")), PathBuf::from("/a"));
        assert_eq!(dirname(Path::new("/a")), PathBuf::from("/"));
        assert!(has_uri_scheme_slashes("sftp://x"));
        assert!(!has_uri_scheme_slashes("/tmp/a://"));
    }
}
