//! Python `shlex.quote` / `shlex.join` / `shlex.split` (POSIX) and `shutil.which`.

use std::path::{Path, PathBuf};

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
    fn which_finds_executables_only() {
        assert!(which("sh").is_some_and(|p| p.is_absolute()));
        assert!(which("/bin/sh").is_some());
        assert!(which("no-such-program-for-tessel-core").is_none());
        assert!(!is_executable(Path::new("/")));
    }
}
