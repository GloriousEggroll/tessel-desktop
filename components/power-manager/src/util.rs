//! Small Python-equivalent helpers: shlex.join, secrets.token_urlsafe,
//! secrets.compare_digest, shutil.which, and an injectable timer queue.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// `shlex.quote`.
pub fn shell_quote(word: &str) -> String {
    if word.is_empty() {
        return "''".into();
    }
    let safe = word.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

/// `shlex.join`.
pub fn shell_join<S: AsRef<str>>(words: &[S]) -> String {
    words.iter().map(|w| shell_quote(w.as_ref())).collect::<Vec<_>>().join(" ")
}

/// `shlex.split` (POSIX mode) for tests and parsing generated commands.
pub fn shell_split(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    current.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => current.push(chars.next().unwrap()),
                        _ => current.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() => {
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
    words
}

/// `secrets.token_urlsafe(n)`: n random bytes, URL-safe base64 without padding.
pub fn token_urlsafe(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let mut filled = 0;
    while filled < bytes {
        // SAFETY: writes at most `bytes - filled` bytes into the owned buffer.
        let result = unsafe { libc::getrandom(buffer[filled..].as_mut_ptr().cast(), bytes - filled, 0) };
        if result > 0 {
            filled += result as usize;
        } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            panic!("getrandom failed");
        }
    }
    base64_urlsafe(&buffer)
}

fn base64_urlsafe(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        let count = chunk.len() + 1;
        for i in 0..count {
            out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Constant-time comparison (`secrets.compare_digest` for str).
pub fn compare_digest(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut difference = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        difference |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    difference == 0
}

/// `shutil.which`: first executable regular file named `name` on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return is_executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    if !metadata.is_file() {
        return false;
    }
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: valid NUL-terminated path.
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

/// Timer/child-watch scheduling, injectable so policy logic is testable
/// without a running main loop (Python tests patch GLib.timeout_add).
pub trait Scheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64;
    fn cancel(&self, id: u64);
}

/// The real GLib main loop.
#[derive(Default)]
pub struct GlibScheduler {
    next: Cell<u64>,
    sources: Rc<RefCell<HashMap<u64, glib::SourceId>>>,
}

impl Scheduler for GlibScheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        let sources = self.sources.clone();
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(milliseconds as u64), move || {
            sources.borrow_mut().remove(&id);
            callback();
        });
        self.sources.borrow_mut().insert(id, source);
        id
    }

    fn cancel(&self, id: u64) {
        if let Some(source) = self.sources.borrow_mut().remove(&id) {
            source.remove();
        }
    }
}

/// A manual timer queue for tests.
#[derive(Default)]
pub struct FakeScheduler {
    next: Cell<u64>,
    pub pending: RefCell<Vec<(u64, u32, Option<Box<dyn FnOnce()>>)>>,
    pub cancelled: RefCell<Vec<u64>>,
}

impl FakeScheduler {
    /// Run the timer `id` (if still pending).
    pub fn fire(&self, id: u64) -> bool {
        let callback = {
            let mut pending = self.pending.borrow_mut();
            let Some(index) = pending.iter().position(|entry| entry.0 == id) else { return false };
            pending.remove(index).2
        };
        if let Some(callback) = callback {
            callback();
        }
        true
    }

    pub fn delays(&self) -> Vec<u32> {
        self.pending.borrow().iter().map(|entry| entry.1).collect()
    }

    /// How many timers were ever created.
    pub fn created(&self) -> u64 {
        self.next.get()
    }

    pub fn ids(&self) -> Vec<u64> {
        self.pending.borrow().iter().map(|entry| entry.0).collect()
    }
}

impl Scheduler for FakeScheduler {
    fn timeout(&self, milliseconds: u32, callback: Box<dyn FnOnce()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        self.pending.borrow_mut().push((id, milliseconds, Some(callback)));
        id
    }

    fn cancel(&self, id: u64) {
        self.cancelled.borrow_mut().push(id);
        self.pending.borrow_mut().retain(|entry| entry.0 != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_matches_shlex() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("/usr/bin/wlopm"), "/usr/bin/wlopm");
        assert_eq!(shell_quote("*"), "'*'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_join(&["/usr/bin/wlopm", "--on", "*"]), "/usr/bin/wlopm --on '*'");
        let tricky = "policy with spaces ' $(false)";
        assert_eq!(shell_split(&shell_join(&["a b", tricky, "x"])), vec!["a b", tricky, "x"]);
    }

    #[test]
    fn tokens_are_urlsafe_and_32_chars() {
        let token = token_urlsafe(24);
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(token, token_urlsafe(24));
        assert_eq!(base64_urlsafe(b"\xfb\xff"), "-_8");
        assert!(compare_digest("abc", "abc"));
        assert!(!compare_digest("abc", "abd"));
        assert!(!compare_digest("abc", "abcd"));
    }
}
