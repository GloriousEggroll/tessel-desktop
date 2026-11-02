//! Test helpers: unique scratch directories under `$TMPDIR` and the Python
//! parity runner (`python3 -I`, tessel-dock sources read-only on `sys.path`).
#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Self {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!(
            "tessel-dock-core-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            glib::random_int()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub const PYTHON_SOURCES: &str = concat!(env!("HOME"), "/tessel-bluetooth/tessel-dock");

/// Run `python3 -I -c <code>` with HOME/XDG pointed into `home`. Returns
/// stdout, or `None` when python3 or the sources are unavailable.
pub fn python(home: &Path, code: &str, extra_env: &[(&str, &str)]) -> Option<String> {
    if !Path::new(PYTHON_SOURCES).join("dock_config.py").exists() {
        return None;
    }
    let prelude = format!("import sys\nsys.path.insert(0, {:?})\n", PYTHON_SOURCES);
    let mut command = std::process::Command::new("python3");
    command
        .arg("-I")
        .arg("-B")
        .arg("-c")
        .arg(prelude + code)
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PATH", "/usr/bin:/bin");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let output = command.output().ok()?;
    assert!(output.status.success(), "python failed: {}", String::from_utf8_lossy(&output.stderr));
    Some(String::from_utf8(output.stdout).unwrap())
}
