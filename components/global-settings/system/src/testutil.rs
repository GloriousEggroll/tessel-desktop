//! Test helpers: private scratch directories and serialized environment edits.
#![cfg(test)]
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("gs-system-{}", std::process::id())).join(name);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// Holds the environment lock and restores the variables on drop.
pub struct EnvGuard {
    saved: Vec<(String, Option<std::ffi::OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub fn set(pairs: &[(&str, &str)]) -> EnvGuard {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut saved = Vec::new();
        for (name, value) in pairs {
            saved.push((name.to_string(), std::env::var_os(name)));
            // SAFETY: every environment mutation in these tests holds ENV_LOCK.
            unsafe { std::env::set_var(name, value) };
        }
        EnvGuard { saved, _lock: lock }
    }

}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            // SAFETY: still holding ENV_LOCK.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(&name, value),
                    None => std::env::remove_var(&name),
                }
            }
        }
    }
}
