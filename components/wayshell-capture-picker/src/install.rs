//! Planning and transactional apply logic of install_capture_picker.py, for
//! the Rust runtime. There is no CLI here: callers inject the command runner
//! (tests use fakes on scratch homes). It never restarts anything by itself.

use crate::portal_config::{configuration_files, Planned};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const PORTAL_UNIT: &str = "xdg-desktop-portal-wlr.service";
pub const PORTAL_BUS: &str = "org.freedesktop.impl.portal.desktop.wlr";
/// Where the Rust binary goes (next to the other Wayshell runtime files; the
/// picker finds `../waydock/waydock-toplevel` from there).
pub const BINARY: &str = ".local/share/wayshell/wayshell-capture-picker";
/// The xdpw `chooser_cmd` target, unchanged from the Python install.
pub const LAUNCHER: &str = ".local/bin/wayshell-capture-picker";

pub fn launcher_script(binary: &Path) -> String {
    format!("#!/bin/sh\nexec {} \"$@\"\n", suite_core::shell::shell_quote(&binary.to_string_lossy()))
}

/// The binary and its `~/.local/bin` launcher (Python: the .py modules + launcher).
pub fn runtime_files(home: &Path, binary: &[u8]) -> Vec<Planned> {
    let target = home.join(BINARY);
    let launcher = launcher_script(&target);
    vec![(target, binary.to_vec(), 0o755), (home.join(LAUNCHER), launcher.into_bytes(), 0o755)]
}

pub fn plan(home: &Path, config_home: Option<&Path>, system_root: &Path, binary: &[u8]) -> Result<Vec<Planned>, String> {
    let config_home = config_home.map(Path::to_path_buf).unwrap_or_else(|| home.join(".config"));
    let mut files = runtime_files(home, binary);
    files.extend(configuration_files(&config_home, &home.join(LAUNCHER), system_root).map_err(|e| e.to_string())?);
    Ok(files)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Snapshot {
    Missing,
    Symlink(PathBuf),
    File { data: Vec<u8>, mode: u32 },
}

impl Snapshot {
    pub fn kind(&self) -> &'static str {
        match self {
            Snapshot::Missing => "missing",
            Snapshot::Symlink(_) => "symlink",
            Snapshot::File { .. } => "file",
        }
    }
}

pub fn snapshot(path: &Path) -> Result<Snapshot, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(Snapshot::Symlink(std::fs::read_link(path).map_err(|e| e.to_string())?)),
        Err(_) => Ok(Snapshot::Missing),
        Ok(meta) if !meta.is_file() => Err(format!("Expected a regular file: {}", path.display())),
        Ok(meta) => Ok(Snapshot::File { data: std::fs::read(path).map_err(|e| e.to_string())?, mode: meta.permissions().mode() & 0o7777 }),
    }
}

/// `waydock/install.atomic_write`: temp file in the target directory, fsync, chmod, rename.
pub fn atomic_write(path: &Path, content: &[u8], mode: u32) -> Result<(), String> {
    let parent = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".waydock-install-{}-{}", std::process::id(),
                                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| e.to_string())?;
        file.write_all(content).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

pub fn restore_file(path: &Path, original: &Snapshot) -> Result<(), String> {
    match original {
        Snapshot::File { data, mode } => atomic_write(path, data, *mode),
        Snapshot::Missing => {
            let _ = std::fs::remove_file(path);
            Ok(())
        }
        Snapshot::Symlink(link) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let _ = std::fs::remove_file(path);
            std::os::unix::fs::symlink(link, path).map_err(|e| e.to_string())
        }
    }
}

/// Files whose content or mode differ, with their current snapshot.
pub fn changes(files: &[Planned]) -> Result<Vec<(Planned, Snapshot)>, String> {
    let mut result = Vec::new();
    for planned in files {
        let (path, data, mode) = planned;
        let unchanged = path.is_file()
            && std::fs::read(path).ok().as_deref() == Some(data.as_slice())
            && std::fs::metadata(path).map(|m| m.permissions().mode() & 0o7777 == *mode).unwrap_or(false);
        if !unchanged {
            result.push((planned.clone(), snapshot(path)?));
        }
    }
    Ok(result)
}

pub struct CommandResult {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `command(argv, check)`: `Err` when `check` and the command failed.
pub trait Commands {
    fn run(&mut self, argv: &[&str], check: bool) -> Result<CommandResult, String>;
}

/// `main()` after argument parsing. `out` receives the Python messages.
pub fn install(home: &Path, files: &[Planned], apply: bool, runtime_only: bool, commands: &mut dyn Commands, out: &mut dyn Write)
               -> Result<(), String> {
    let changes = changes(files)?;
    for ((path, _, _), _) in &changes {
        let _ = writeln!(out, "{}", path.display());
    }
    let _ = writeln!(out, "{} files prepared. KDE portal preferences and service enablement are unchanged.", changes.len());
    if !apply || changes.is_empty() {
        return Ok(());
    }
    let active = commands.run(&["systemctl", "--user", "is-active", "--quiet", PORTAL_UNIT], false)?.returncode == 0;
    let enabled = commands.run(&["systemctl", "--user", "is-enabled", PORTAL_UNIT], false)?.stdout;
    if active && !runtime_only {
        let paths = commands.run(&["busctl", "--user", "--list", "tree", PORTAL_BUS], true)?.stdout;
        if paths.lines().any(|path| path.contains("/session/")) {
            return Err("A screen-sharing session is active. Finish it before applying this portal update.".into());
        }
    }
    let directory = home.join(".local/state/desktop-suite/backups");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let backup = make_temporary_directory(&directory, "capture-picker-")?;
    for ((path, _, _), before) in &changes {
        match before {
            Snapshot::Symlink(_) => return Err(format!("Refusing to replace a linked file: {}", path.display())),
            Snapshot::File { data, mode } => {
                let relative = path.strip_prefix(home).map_err(|_| format!("{} is not inside {}", path.display(), home.display()))?;
                atomic_write(&backup.join(relative), data, *mode)?;
            }
            Snapshot::Missing => {}
        }
    }
    let manifest: Vec<Value> = changes.iter().map(|((path, _, _), before)| json!({"path": path.to_string_lossy(), "before": before.kind()})).collect();
    let style = suite_core::pyfmt::JsonStyle { indent: Some(2), item_separator: ",", ..suite_core::pyfmt::JsonStyle::DEFAULT };
    std::fs::write(backup.join("manifest.json"), suite_core::pyfmt::json_dumps(&Value::Array(manifest), &style) + "\n")
        .map_err(|e| e.to_string())?;
    let mut written: Vec<(PathBuf, Snapshot)> = Vec::new();
    let result = (|| -> Result<(), String> {
        for ((path, data, mode), before) in &changes {
            if &snapshot(path)? != before {
                return Err(format!("An installed file changed during update: {}", path.display()));
            }
            atomic_write(path, data, *mode)?;
            written.push((path.clone(), before.clone()));
        }
        if active && !runtime_only {
            commands.run(&["systemctl", "--user", "restart", PORTAL_UNIT], true)?;
            commands.run(&["busctl", "--user", "get-property", PORTAL_BUS, "/org/freedesktop/portal/desktop",
                           "org.freedesktop.impl.portal.ScreenCast", "AvailableSourceTypes"], true)?;
        }
        for (path, data, _) in files {
            if std::fs::read(path).ok().as_deref() != Some(data.as_slice()) {
                return Err(format!("{}", path.display()));
            }
        }
        if commands.run(&["systemctl", "--user", "is-enabled", PORTAL_UNIT], false)?.stdout != enabled {
            return Err("service enablement changed".into());
        }
        Ok(())
    })();
    if let Err(error) = result {
        for (path, before) in written.iter().rev() {
            let _ = restore_file(path, before);
        }
        if active && !runtime_only {
            let _ = commands.run(&["systemctl", "--user", "restart", PORTAL_UNIT], false);
        }
        return Err(error);
    }
    let suffix = if runtime_only { " It will load on the next capture request." }
                 else if active { " Refreshed the wlroots portal." } else { " It will load when the portal starts." };
    let _ = writeln!(out, "Installed Wayshell capture picker.{suffix}");
    let _ = writeln!(out, "Backup: {}", backup.display());
    Ok(())
}

fn make_temporary_directory(parent: &Path, prefix: &str) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut template = parent.join(format!("{prefix}XXXXXXXX")).as_os_str().as_bytes().to_vec();
    template.push(0);
    let result = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    if result.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    template.pop();
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&template)))
}
