//! Running the CLI tools: no terminal, no stdin (a tool must never wait for
//! a password prompt), cancellable (the child is killed), with an output
//! callback for progress parsing.

use crate::archive::{Ctx, Error, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn text(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
    /// The most useful error line of a failed run.
    pub fn error(&self, tool: &str) -> String {
        let text = self.text();
        let line = text.lines().map(str::trim)
            .find(|l| l.starts_with("ERROR") || l.contains("rror:") || l.contains("annot ") || l.contains("failed"))
            .or_else(|| text.lines().map(str::trim).rfind(|l| !l.is_empty()))
            .unwrap_or("");
        if line.is_empty() {
            format!("{tool} failed (exit status {})", self.status)
        } else {
            format!("{tool}: {line}")
        }
    }
}

/// Run `program args` in `cwd`; `on_output` sees stdout chunks as they arrive.
pub fn run(program: &str, args: &[String], cwd: Option<&Path>, ctx: &Ctx, on_output: Option<&(dyn Fn(&str) + Sync)>) -> Result<Output> {
    ctx.check()?;
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .env("LC_ALL", "C.UTF-8").env("LANG", "C.UTF-8").env_remove("LANGUAGE");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    // Never let a tool open the controlling terminal for a password prompt.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| Error::Failed(format!("{program}: {e}")))?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    std::thread::scope(|scope| -> Result<Output> {
        let out_buf = stdout_buf.clone();
        let reader = stdout_pipe.map(|mut pipe| {
            scope.spawn(move || {
                let mut chunk = [0u8; 8192];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Some(callback) = on_output {
                                callback(&String::from_utf8_lossy(&chunk[..n]));
                            }
                            out_buf.lock().unwrap().extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            })
        });
        let err_buf = stderr_buf.clone();
        let err_reader = stderr_pipe.map(|mut pipe| {
            scope.spawn(move || {
                let mut all = Vec::new();
                let _ = pipe.read_to_end(&mut all);
                err_buf.lock().unwrap().extend_from_slice(&all);
            })
        });
        let status = loop {
            if ctx.cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(r) = reader {
                    let _ = r.join();
                }
                if let Some(r) = err_reader {
                    let _ = r.join();
                }
                return Err(Error::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(15)),
                Err(e) => return Err(Error::Failed(e.to_string())),
            }
        };
        if let Some(r) = reader {
            let _ = r.join();
        }
        if let Some(r) = err_reader {
            let _ = r.join();
        }
        Ok(Output {
            status: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout_buf.lock().unwrap()).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_buf.lock().unwrap()).into_owned(),
        })
    })
}

/// Run with `input` on stdin (short-lived tools such as `zipnote -w`).
pub fn run_input(program: &str, args: &[String], ctx: &Ctx, input: &[u8]) -> Result<Output> {
    use std::io::Write;
    ctx.check()?;
    let mut child = Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .env("LC_ALL", "C.UTF-8").env("LANG", "C.UTF-8").spawn().map_err(|e| Error::Failed(format!("{program}: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let output = child.wait_with_output().map_err(|e| Error::Failed(format!("{program}: {e}")))?;
    let _ = writer.join();
    Ok(Output {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// A private scratch directory (0700) removed on drop.
pub struct TempDir(pub std::path::PathBuf);

impl TempDir {
    /// Below `parent` (same filesystem as the target, for cheap renames).
    pub fn new_in(parent: &Path, prefix: &str) -> Result<TempDir> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::create_dir_all(parent)?;
        for attempt in 0..1000u32 {
            let name = format!("{prefix}{}-{}-{attempt}", std::process::id(), random_suffix());
            let path = parent.join(name);
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(TempDir(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(Error::Failed(format!("{}: {e}", path.display()))),
            }
        }
        Err(Error::Failed("could not create a temporary folder".into()))
    }

    /// Below the private runtime dir (`$XDG_RUNTIME_DIR/wayarchive`), else the system temp dir.
    pub fn private(prefix: &str) -> Result<TempDir> {
        TempDir::new_in(&private_root(), prefix)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Keep the directory (the caller takes ownership of cleaning it up).
    pub fn keep(self) -> std::path::PathBuf {
        let path = self.0.clone();
        std::mem::forget(self);
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = make_writable_and_remove(&self.0);
    }
}

fn make_writable_and_remove(path: &Path) -> std::io::Result<()> {
    // Extracted read-only folders would otherwise block removal.
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.is_dir() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let _ = make_writable_and_remove(&entry.path());
                }
            }
            return std::fs::remove_dir(path);
        }
        return std::fs::remove_file(path);
    }
    Ok(())
}

pub fn remove_tree(path: &Path) {
    let _ = make_writable_and_remove(path);
}

pub fn private_root() -> std::path::PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).filter(|p| p.is_absolute() && p.is_dir());
    match runtime {
        Some(dir) => dir.join("wayarchive"),
        None => std::env::temp_dir().join(format!("wayarchive-{}", unsafe { libc::getuid() })),
    }
}

pub fn random_suffix() -> String {
    let mut bytes = [0u8; 6];
    let n = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if n != bytes.len() as isize {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        bytes.copy_from_slice(&t.to_le_bytes()[..6]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Write a list file (one name per line, UTF-8) for tools that read `@file`.
pub fn list_file(dir: &Path, names: &[String]) -> Result<std::path::PathBuf> {
    let path = dir.join("list.txt");
    let mut text = String::new();
    for name in names {
        if name.contains('\n') {
            return Err(Error::Unsupported("file names with line breaks".into()));
        }
        text.push_str(name);
        text.push('\n');
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

/// The last "NN%" in a chunk of tool output.
pub fn last_percent(chunk: &str) -> Option<f64> {
    let bytes = chunk.as_bytes();
    let mut found = None;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'%' {
            let mut start = i;
            while start > 0 && bytes[start - 1].is_ascii_digit() {
                start -= 1;
            }
            if start < i {
                if let Ok(v) = chunk[start..i].parse::<u32>() {
                    if v <= 100 {
                        found = Some(f64::from(v) / 100.0);
                    }
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_parsing() {
        assert_eq!(last_percent("  5% 1 - a\u{8}\u{8} 42% 3 - b"), Some(0.42));
        assert_eq!(last_percent("100%"), Some(1.0));
        assert_eq!(last_percent("no progress"), None);
    }

    #[test]
    fn run_collects_output_and_cancels() {
        let ctx = Ctx::new();
        let out = run("sh", &["-c".into(), "echo hi; echo err >&2; exit 3".into()], None, &ctx, None).unwrap();
        assert_eq!(out.status, 3);
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "err");
        let ctx = Ctx::new();
        let flag = ctx.cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        assert_eq!(run("sleep", &["10".into()], None, &ctx, None).err(), Some(Error::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(5));
        t.join().unwrap();
    }

    #[test]
    fn temp_dirs_are_private_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("wayarchive-tmp-test-{}", std::process::id()));
        let dir = TempDir::new_in(&base, ".t-").unwrap();
        let path = dir.path().to_path_buf();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::create_dir(path.join("ro")).unwrap();
        std::fs::write(path.join("ro/f"), "x").unwrap();
        std::fs::set_permissions(path.join("ro"), std::fs::Permissions::from_mode(0o500)).unwrap();
        drop(dir);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(base);
    }
}
