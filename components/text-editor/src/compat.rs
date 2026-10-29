//! Helpers suite-core / suite-gtk do not offer yet (upstream candidates).

use std::ffi::CStr;
use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// `os.path.expanduser`: `~` / `~/x` from `$HOME` (else the passwd entry), `~user/x` from passwd.
pub fn expanduser(value: &str) -> String {
    let Some(rest) = value.strip_prefix('~') else { return value.to_owned() };
    let (user, tail) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let home = if user.is_empty() {
        std::env::var("HOME").ok().or_else(|| passwd_home(None))
    } else {
        passwd_home(Some(user))
    };
    match home {
        Some(home) => {
            let home = if home.len() > 1 { home.trim_end_matches('/').to_owned() } else { home };
            let joined = format!("{home}{tail}");
            if joined.is_empty() { "/".into() } else { joined }
        }
        None => value.to_owned(),
    }
}

/// `Path(value).expanduser()` (same rules as `os.path.expanduser` for our uses).
pub fn expanduser_path(value: &str) -> String {
    expanduser(value)
}

fn passwd_home(user: Option<&str>) -> Option<String> {
    // SAFETY: getpwnam/getpwuid return static storage; we copy the field at once.
    unsafe {
        let entry = match user {
            Some(name) => {
                let name = std::ffi::CString::new(name).ok()?;
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

/// `Path.resolve()` (non-strict `realpath`): symlinks in the existing prefix are
/// resolved, the missing remainder is appended lexically.
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    if let Ok(real) = std::fs::canonicalize(&absolute) {
        return real;
    }
    let mut existing = absolute.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match existing.file_name() {
            Some(name) => tail.push(name.to_owned()),
            None => break,
        }
        if !existing.pop() {
            break;
        }
        if let Ok(real) = std::fs::canonicalize(&existing) {
            existing = real;
            break;
        }
    }
    for part in tail.into_iter().rev() {
        match Path::new(&part).components().next() {
            Some(Component::ParentDir) => {
                existing.pop();
            }
            Some(Component::CurDir) => {}
            _ => existing.push(part),
        }
    }
    existing
}

/// `tempfile.NamedTemporaryFile(prefix, suffix, dir, delete=False)`: an
/// exclusively created 0600 file. Errors read like Python's OSError text.
pub fn named_temporary(dir: &Path, prefix: &str, suffix: &str) -> Result<(PathBuf, File), crate::document::DocError> {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut last = None;
    for _ in 0..100 {
        let mut random = [0u8; 8];
        random_bytes(&mut random);
        let name: String = random.iter().map(|b| LETTERS[*b as usize % LETTERS.len()] as char).collect();
        let path = dir.join(format!("{prefix}{name}{suffix}"));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some((error, path)),
            Err(error) => return Err(crate::document::DocError::Os(suite_core::pyerr::os_error_path(&error, &path))),
        }
    }
    let (error, path) = last.expect("100 attempts");
    Err(crate::document::DocError::Exists(suite_core::pyerr::os_error_path(&error, &path)))
}

pub fn random_bytes(buffer: &mut [u8]) {
    // SAFETY: getrandom writes at most buffer.len() bytes into the buffer.
    let filled = unsafe { libc::getrandom(buffer.as_mut_ptr().cast(), buffer.len(), 0) };
    if filled != buffer.len() as isize {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
            ^ (std::process::id() as u128) << 64;
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = (seed >> ((index % 16) * 8)) as u8 ^ index as u8;
        }
    }
}

/// `uuid.uuid4().hex`.
pub fn uuid4_hex() -> String {
    let mut bytes = [0u8; 16];
    random_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `time.time()`.
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `companion_theme.attach_application(app)` with an explicit shared client, so
/// the editor can read `palette.is_dark` (Python `companion_theme._client`).
/// suite-gtk keeps its process-wide client private.
pub fn attach_application(app: &gtk::Application, client: &suite_gtk::theme::PaletteClient) {
    use gtk::prelude::*;
    let client = client.clone();
    let shared = client.clone();
    app.connect_window_added(move |_, window| {
        suite_gtk::theme::attach(window, Some(&shared));
    });
    for window in app.windows() {
        suite_gtk::theme::attach(&window, Some(&client));
    }
}

/// `GtkSource.SearchContext.replace_all()` returning the replacement count.
/// sourceview5 0.11's binding drops the count and asserts `count == 0` iff an
/// error occurred, which panics for "0 replacements" and for any success.
pub fn replace_all(context: &sourceview5::SearchContext, replace: &str) -> Result<u32, glib::Error> {
    use glib::translate::{from_glib_full, ToGlibPtr};
    unsafe {
        let mut error = std::ptr::null_mut();
        let count = sourceview5::ffi::gtk_source_search_context_replace_all(
            context.to_glib_none().0, replace.to_glib_none().0, replace.len() as i32, &mut error);
        if error.is_null() { Ok(count) } else { Err(from_glib_full(error)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanduser_and_resolve() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expanduser("~/notes.txt"), format!("{}/notes.txt", home.trim_end_matches('/')));
        assert_eq!(expanduser("plain/~"), "plain/~");
        let root = std::env::temp_dir().join(format!("text-editor-compat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let real = std::fs::canonicalize(&root).unwrap();
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();
        assert_eq!(resolve(&root.join("link/missing/x.txt")), real.join("missing/x.txt"));
        assert_eq!(uuid4_hex().len(), 32);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
