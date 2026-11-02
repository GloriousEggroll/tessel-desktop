//! Locked, atomic file writes shared by preferences and the metadata fallback
//! (Python `_locked`/`_store_lock`, `_atomic_write`/`_atomic_json`).

use crate::error::{Error, Result};
use std::ffi::{CString, OsString};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

pub fn cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::value("embedded null byte"))
}

/// `path.parent.mkdir(parents=True, exist_ok=True)`.
pub fn make_parents(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| Error::from_io(&e, Some(parent)))?;
        }
    }
    Ok(())
}

/// Exclusive `flock` on `<path>.lock` (0600) around `body`.
pub fn locked<T>(path: &Path, body: impl FnOnce() -> Result<T>) -> Result<T> {
    make_parents(path)?;
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    let lock = PathBuf::from(lock);
    let name = cstring(&lock)?;
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC, 0o600) };
    if fd < 0 {
        return Err(Error::last_os(Some(&lock)));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(Error::last_os(Some(&lock)));
    }
    let result = body();
    unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_UN) };
    result
}

/// `tempfile.mkstemp(prefix=…, dir=…)`: a new 0600 file with a random suffix.
pub fn mkstemp(directory: &Path, prefix: &str) -> Result<(std::fs::File, PathBuf)> {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    for _ in 0..100 {
        let mut random = [0u8; 8];
        getrandom(&mut random);
        let suffix: String = random.iter().map(|b| LETTERS[*b as usize % LETTERS.len()] as char).collect();
        let mut name = OsString::from(prefix);
        name.push(&suffix);
        let candidate = directory.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true)
            .custom_flags_compat(0o600).open(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Error::from_io(&error, Some(&candidate))),
        }
    }
    Err(Error::file_exists("No usable temporary file name found"))
}

trait OpenMode {
    fn custom_flags_compat(&mut self, mode: u32) -> &mut Self;
}
impl OpenMode for std::fs::OpenOptions {
    fn custom_flags_compat(&mut self, mode: u32) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(mode).custom_flags(libc::O_CLOEXEC)
    }
}

pub fn getrandom(buffer: &mut [u8]) {
    let mut filled = 0;
    while filled < buffer.len() {
        let count = unsafe { libc::getrandom(buffer[filled..].as_mut_ptr().cast(), buffer.len() - filled, 0) };
        if count <= 0 {
            // Extremely unlikely; fall back to time-derived bytes.
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos()).unwrap_or(0) as u64 ^ (std::process::id() as u64) << 32;
            for (i, byte) in buffer[filled..].iter_mut().enumerate() {
                *byte = (nanos >> ((i % 8) * 8)) as u8;
            }
            return;
        }
        filled += count as usize;
    }
}

/// Random lowercase hex (`uuid.uuid4().hex`-like, 32 characters).
pub fn uuid_hex() -> String {
    let mut random = [0u8; 16];
    getrandom(&mut random);
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    random.iter().map(|b| format!("{b:02x}")).collect()
}

/// mkstemp(prefix=".<name>.") beside `path`, write, fsync, `os.replace`; the
/// temporary is removed on failure. `resolve` follows a symlinked `path` first.
pub fn atomic_write(path: &Path, data: &[u8], resolve: bool) -> Result<()> {
    let path = if resolve { crate::pyos::realpath(path) } else { path.to_path_buf() };
    if resolve {
        make_parents(&path)?;
    }
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let name = path.file_name().map(|n| n.to_os_string().into_vec()).unwrap_or_default();
    let prefix = format!(".{}.", String::from_utf8_lossy(&name));
    let (mut file, temporary) = mkstemp(&parent, &prefix)?;
    let outcome = (|| {
        file.write_all(data).map_err(|e| Error::from_io(&e, Some(&temporary)))?;
        file.flush().map_err(|e| Error::from_io(&e, Some(&temporary)))?;
        file.sync_all().map_err(|e| Error::from_io(&e, Some(&temporary)))?;
        drop(file);
        std::fs::rename(&temporary, &path).map_err(|e| Error::from_io(&e, Some(&temporary)))
    })();
    let _ = std::fs::remove_file(&temporary).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) });
    outcome
}
