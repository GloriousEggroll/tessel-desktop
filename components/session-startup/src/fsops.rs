//! File and process helpers with the Python installers' semantics and error texts.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use tessel_core::pyerr::os_error_path;

use crate::pystr;

pub type Snapshot = Option<(Vec<u8>, u32)>;

fn os_error(error: &std::io::Error, path: &Path) -> String {
    os_error_path(error, path)
}

/// `Path.read_bytes()`.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| os_error(&error, path))
}

/// `Path.read_text()`: UTF-8, universal newlines.
pub fn read_text(path: &Path) -> Result<String, String> {
    let data = read_bytes(path)?;
    Ok(pystr::universal_newlines(&pystr::decode(&data)?))
}

pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// `path.stat().st_mode & 0o777`.
pub fn mode(path: &Path) -> Result<u32, String> {
    fs::metadata(path).map(|m| m.permissions().mode() & 0o777).map_err(|error| os_error(&error, path))
}

/// `mode(path, default)` of `configure_session.plan`.
pub fn mode_or(path: &Path, default: u32) -> Result<u32, String> {
    if path.exists() {
        mode(path)
    } else {
        Ok(default)
    }
}

/// `snapshot(path)`: refuses symlinks, `(bytes, mode)` of an existing file, else `None`.
pub fn snapshot(path: &Path) -> Result<Snapshot, String> {
    if is_symlink(path) {
        return Err(format!("Refusing to replace a symlink: {}", path.display()));
    }
    if path.exists() {
        return Ok(Some((read_bytes(path)?, mode(path)?)));
    }
    Ok(None)
}

fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut bytes = [0u8; 8];
    if unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) } != bytes.len() as isize {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        bytes = (nanos as u64 ^ std::process::id() as u64).to_le_bytes();
    }
    bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

/// `configure.atomic_write`: `mkdir -p` the parent, `NamedTemporaryFile(prefix=".labwc-services-")`
/// (0600), write, chmod, `replace`; the temporary is removed on failure.
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(parent).map_err(|error| os_error(&error, parent))?;
    let (temporary, mut stream) = loop {
        let candidate = parent.join(format!(".labwc-services-{}", random_suffix()));
        match fs::OpenOptions::new().write(true).create_new(true).mode_0600().open(&candidate) {
            Ok(stream) => break (candidate, stream),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(os_error(&error, &candidate)),
        }
    };
    let result = (|| {
        stream.write_all(data).map_err(|error| os_error(&error, &temporary))?;
        drop(stream);
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).map_err(|error| os_error(&error, &temporary))?;
        fs::rename(&temporary, path).map_err(|error| {
            format!("{} -> '{}'", os_error(&error, &temporary), path.display())
        })
    })();
    let _ = fs::remove_file(&temporary);
    result
}

trait CreateMode {
    fn mode_0600(&mut self) -> &mut Self;
}

impl CreateMode for fs::OpenOptions {
    fn mode_0600(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, 0o600)
    }
}

/// `path.unlink(missing_ok=True)`.
pub fn unlink_missing_ok(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(os_error(&error, path)),
    }
}

/// `datetime.now().strftime(prefix + "%Y%m%dT%H%M%S%f")`.
pub fn timestamp(prefix: &str) -> String {
    let now = glib::DateTime::now_local().expect("local time");
    format!(
        "{prefix}{}{:06}",
        now.format("%Y%m%dT%H%M%S").expect("format"),
        now.microsecond()
    )
}

/// `path.relative_to(home)`.
pub fn relative_to(path: &Path, home: &Path) -> Result<PathBuf, String> {
    path.strip_prefix(home)
        .map(Path::to_path_buf)
        .map_err(|_| format!("{} is not in the subpath of {}", pystr::py_repr(&path.display().to_string()), pystr::py_repr(&home.display().to_string())))
}

/// Outcome of `subprocess.run` (stdio inherited).
pub struct Run {
    pub status: ExitStatus,
}

fn signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "SIGHUP", 2 => "SIGINT", 3 => "SIGQUIT", 4 => "SIGILL", 6 => "SIGABRT", 8 => "SIGFPE",
        9 => "SIGKILL", 11 => "SIGSEGV", 13 => "SIGPIPE", 14 => "SIGALRM", 15 => "SIGTERM",
        _ => return format!("unknown signal {signal}"),
    };
    format!("<Signals.{name}: {signal}>")
}

/// `subprocess.run(argv, check=…, timeout=…)` with inherited stdio and Python's exception texts
/// (`FileNotFoundError`, `TimeoutExpired`, `CalledProcessError`).
pub fn run(argv: &[&str], check: bool, timeout: Option<Duration>) -> Result<Run, String> {
    let _ = std::io::stdout().flush();
    let mut child = Command::new(argv[0]).args(&argv[1..]).spawn().map_err(|error| {
        let mut text = tessel_core::pyerr::os_error_text(&error);
        if error.raw_os_error().is_some() {
            text = format!("{text}: {}", pystr::py_repr(argv[0]));
        }
        text
    })?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| tessel_core::pyerr::os_error_text(&error))? {
            break status;
        }
        if let Some(limit) = timeout {
            if started.elapsed() >= limit {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "Command '{}' timed out after {} seconds",
                    pystr::py_repr_list(argv),
                    limit.as_secs()
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if check && !status.success() {
        use std::os::unix::process::ExitStatusExt;
        let detail = match (status.code(), status.signal()) {
            (_, Some(signal)) => format!("died with {}.", signal_name(signal)),
            (Some(code), _) => format!("returned non-zero exit status {code}."),
            _ => "failed.".into(),
        };
        return Err(format!("Command '{}' {detail}", pystr::py_repr_list(argv)));
    }
    Ok(Run { status })
}

/// `pathlib.PurePosixPath(text)` normalization: repeated and trailing slashes and `.`
/// components dropped, exactly two leading slashes kept (`Path.home()` of `$HOME`).
pub fn py_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.is_empty() {
        return PathBuf::from(".");
    }
    let root = if text.starts_with("//") && !text.starts_with("///") {
        "//"
    } else if text.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = text.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
    let joined = format!("{root}{}", parts.join("/"));
    PathBuf::from(if joined.is_empty() { ".".to_owned() } else { joined })
}

/// `Path.home()`.
pub fn home() -> PathBuf {
    py_path(&tessel_core::paths::home())
}
