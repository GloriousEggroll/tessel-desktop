#![allow(dead_code)]
//! Fixtures shared by the integration tests. Every home is a fresh directory under
//! `$TMPDIR`; nothing here reads or writes the real `~/.config`.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use tessel_session_startup::session::Source;

/// The Python suite tree (read only): `$DESKTOP_SUITE_SOURCE` or `~/tessel-bluetooth`.
pub fn suite_root() -> Option<PathBuf> {
    let root = std::env::var_os("DESKTOP_SUITE_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join("tessel-bluetooth"));
    root.join("tessel-session-startup/configure_session.py").is_file().then_some(root)
}

pub fn source() -> Source {
    Source::from_root(&suite_root().expect("tessel-bluetooth source tree"))
}

pub fn data(name: &str) -> String {
    fs::read_to_string(suite_root().unwrap().join("tessel-session-startup/testdata").join(name)).unwrap()
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh, empty directory under `$TMPDIR` (never the real home).
pub fn scratch(prefix: &str) -> PathBuf {
    let base = std::env::temp_dir();
    let path = base.join(format!("{prefix}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

pub fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

pub fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// The live files of the suite's development machine with the home replaced
/// (test_configure_session.LiveMigrationTests.setUp).
pub fn live_home(home: &Path) {
    let labwc = home.join(".config/labwc");
    let units = home.join(".config/systemd/user");
    fs::create_dir_all(&labwc).unwrap();
    fs::create_dir_all(&units).unwrap();
    for name in ["rc.xml", "autostart", "environment", "menu.xml"] {
        write(&labwc.join(name), &data(&format!("live-{name}")).replace("@HOME@", &home.display().to_string()));
    }
    chmod(&labwc.join("autostart"), 0o755);
    let home_text = home.display();
    write(
        &units.join("tessel-network.service"),
        &format!("[Unit]\nDescription=Network\n\n[Service]\nExecStart=/usr/bin/python3 {home_text}/.config/waybar/scripts/tessel-network/network_manager.py --background\nRestart=on-failure\nType=simple\n"),
    );
    write(
        &units.join("tessel-power.service"),
        &format!("[Service]\nType=simple\nEnvironmentFile=-%h/.config/labwc/environment\nExecStart=/usr/bin/python3 {home_text}/.config/waybar/scripts/tessel-power/power_manager.py --background\nRestart=on-failure\nRestartSec=2\n"),
    );
    write(&units.join("tessel-lock.service"), "[Service]\nExecStart=/bin/true\n");
    write(
        &units.join("tessel-notifications.service.d/tessel.conf"),
        "[Service]\nEnvironmentFile=-%h/.config/labwc/environment\nExecStart=\nExecStart=/usr/bin/true\n",
    );
}

/// Every regular file below `root`: relative path → (bytes, mode).
pub fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), (fs::read(&path).unwrap(), meta.permissions().mode() & 0o777));
            }
        }
    }
    files
}

/// A directory of logging stand-ins for external commands, put first on `PATH`.
pub fn fake_bin(root: &Path, names: &[&str], log: &Path) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in names {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\necho \"{name} $*\" >> \"{}\"\n", log.display())).unwrap();
        chmod(&path, 0o755);
    }
    bin
}
