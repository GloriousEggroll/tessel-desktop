//! Wallpaper colours for every GTK4 application (gtk_color_scheme.py).
//!
//! `GtkColorScheme::publish(true, ..)` writes `gtk-4.0/tessel-dock-colors.css`
//! (byte-identical to Python's `color_css`) and appends exactly
//! [`MANAGED_IMPORT`] to the user's `gtk-4.0/gtk.css`, preserving every user
//! byte (BOM, line endings, missing final newline). The first modification
//! backs the original up once to `gtk.css.tessel-dock-original`. Symlinked
//! destinations/directories, special files, files over 2 MiB, an unrelated
//! `tessel-dock-colors.css` and concurrent edits are refused with `error` set.
//! `publish(false, ..)` removes only the managed import.
//!
//! `select_palette` picks the monitor named by way-displays' managed block in
//! `labwc/environment` (parsed with shlex, never executed).

use super::fsx::{self, error_text, PathError};
use serde_json::Value;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tessel_gtk::palette::{Palette, Rgb};

pub const MANAGED_IMPORT: &[u8] = b"@import url(\"tessel-dock-colors.css\"); /* Tessel Dock wallpaper colors */\n";
pub const MANAGED_SEPARATOR: &[u8] = b"\n/* Tessel Dock separator: original stylesheet has no final newline. */\n";
pub const GENERATED_HEADER: &[u8] = b"/* Tessel Dock managed GTK4 wallpaper colors. */\n";
pub const MAX_CSS_BYTES: u64 = 2 * 1024 * 1024;
pub const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
pub const PRIMARY_BEGIN: &str = "# tessel-displays primary output environment begin";
pub const PRIMARY_END: &str = "# tessel-displays primary output environment end";
pub const PRIMARY_KEYS: [&str; 3] = ["SDL_VIDEO_DISPLAY_PRIORITY", "WAYLANDDRV_PRIMARY_MONITOR", "PROTON_WAYLAND_MONITOR"];

/// Errors are Python `OSError` / `ValueError` text.
#[derive(Debug)]
pub enum Failure {
    Os(io::Error),
    Value(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Os(error) => f.write_str(&error_text(error)),
            Failure::Value(text) => f.write_str(text),
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Os(error)
    }
}

type Result<T> = std::result::Result<T, Failure>;

fn rgb(value: Option<&Value>) -> Result<Rgb> {
    let parts = value.and_then(Value::as_array).filter(|a| a.len() == 3)
        .ok_or_else(|| Failure::Value("GTK palette colors must be RGB triples".into()))?;
    let mut color = [0u8; 3];
    for (slot, part) in color.iter_mut().zip(parts) {
        let channel = part.as_i64().filter(|_| part.is_i64() || part.is_u64()).filter(|c| (0..=255).contains(c))
            .ok_or_else(|| Failure::Value("GTK palette channels must be integers from 0 to 255".into()))?;
        *slot = channel as u8;
    }
    Ok(color)
}

/// Validate a `{"background", "foreground", "accent"}` palette like Python's `_rgb`.
pub fn palette_from_value(palette: Option<&Value>) -> Result<Palette> {
    let object = palette.filter(|p| p.is_object()).ok_or_else(|| Failure::Value("A GTK color palette is required".into()))?;
    Ok(Palette { background: rgb(object.get("background"))?, foreground: rgb(object.get("foreground"))?,
                 accent: rgb(object.get("accent"))? })
}

/// `color_css(is_dark, palette, colored_buttons)` as bytes.
pub fn color_css(is_dark: bool, palette: &Palette, colored_buttons: bool) -> Vec<u8> {
    tessel_gtk::palette::color_css(is_dark, palette, colored_buttons).into_bytes()
}

/// `_read_regular`: never follows a destination symlink or blocks on a FIFO.
pub fn read_regular(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let file = match std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PathError::wrap(error, path).into()),
    };
    let info = file.metadata()?;
    if !info.file_type().is_file() {
        return Err(Failure::Value(format!("Not a regular CSS file: {}", path.display())));
    }
    if info.len() > limit {
        return Err(Failure::Value(format!("CSS file is too large: {}", path.display())));
    }
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(Failure::Value(format!("CSS file is too large: {}", path.display())));
    }
    Ok(Some(data))
}

/// Byte-based removal of our import (and its no-final-newline separator).
pub fn without_managed_import(data: &[u8]) -> Vec<u8> {
    let bom: &[u8] = if data.starts_with(UTF8_BOM) { UTF8_BOM } else { b"" };
    let body = &data[bom.len()..];
    let separated = [MANAGED_SEPARATOR, MANAGED_IMPORT].concat();
    let body = fsx::replace_bytes(body, &separated, b"");
    let mut out = bom.to_vec();
    for line in fsx::split_bytes_keepends(&body) {
        if line != MANAGED_IMPORT {
            out.extend_from_slice(line);
        }
    }
    out
}

/// Append the import after the user's rules (giving our colours precedence).
pub fn with_managed_import(data: &[u8]) -> Vec<u8> {
    let mut body = without_managed_import(data);
    let content = body.strip_prefix(UTF8_BOM).unwrap_or(&body);
    if !content.is_empty() && !content.ends_with(b"\n") {
        body.extend_from_slice(MANAGED_SEPARATOR);
    }
    body.extend_from_slice(MANAGED_IMPORT);
    body
}

type Stamp = (u64, u64, u64, i64, i64);

pub struct GtkColorScheme {
    pub config_dir: PathBuf,
    pub directory: PathBuf,
    pub path: PathBuf,
    pub colors_path: PathBuf,
    pub backup_path: PathBuf,
    pub primary_path: PathBuf,
    pub error: Option<String>,
    primary_stamp: Option<Option<Stamp>>,
    primary_names: Vec<String>,
    /// Test hooks (Python tests patch `_replace` / `os.replace`).
    pub before_replace: Option<Box<dyn Fn(&Path)>>,
    pub replace_hook: Option<Box<dyn Fn(&Path, &Path) -> io::Result<()>>>,
    /// Counts primary-environment reads (cache test).
    pub primary_reads: std::cell::Cell<u32>,
}

impl GtkColorScheme {
    /// `config_dir` defaults to `$XDG_CONFIG_HOME` (else `~/.config`).
    pub fn new(config_dir: Option<PathBuf>) -> Self {
        let root = config_dir.unwrap_or_else(tessel_core::paths::config_home);
        let config_dir = if root.is_absolute() { root } else {
            std::env::current_dir().map(|cwd| cwd.join(&root)).unwrap_or(root)
        };
        let directory = config_dir.join("gtk-4.0");
        Self {
            path: directory.join("gtk.css"), colors_path: directory.join("tessel-dock-colors.css"),
            backup_path: directory.join("gtk.css.tessel-dock-original"), primary_path: config_dir.join("labwc").join("environment"),
            directory, config_dir, error: None, primary_stamp: None, primary_names: Vec::new(),
            before_replace: None, replace_hook: None, primary_reads: std::cell::Cell::new(0),
        }
    }

    fn read_primary_names(&self) -> std::result::Result<Vec<String>, ()> {
        self.primary_reads.set(self.primary_reads.get() + 1);
        let raw = read_regular(&self.primary_path, 65536).map_err(|_| ())?.unwrap_or_default();
        let text = String::from_utf8(raw).map_err(|_| ())?;
        let lines = fsx::splitlines(&text);
        let start = lines.iter().position(|l| *l == PRIMARY_BEGIN).ok_or(())?;
        let end = lines.iter().position(|l| *l == PRIMARY_END).ok_or(())?;
        let mut values: Vec<(&str, String)> = Vec::new();
        if end > start {
            for line in &lines[start + 1..end] {
                let Some((key, value)) = line.split_once('=') else { continue };
                if PRIMARY_KEYS.contains(&key) {
                    let parts = fsx::shlex_split(value).map_err(|_| ())?;
                    if parts.len() == 1 {
                        values.retain(|(k, _)| *k != key);
                        values.push((key, parts.into_iter().next().unwrap()));
                    }
                }
            }
        }
        Ok(PRIMARY_KEYS.iter().filter_map(|key| values.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())).collect())
    }

    /// Use the saved primary display, falling back to the first live one.
    /// File metadata avoids rereading when only window metadata changes.
    pub fn select_palette<'a>(&mut self, monitors: &'a [Value]) -> Option<&'a Value> {
        let first = monitors.first()?;
        let stamp = std::fs::symlink_metadata(&self.primary_path).ok()
            .map(|i| (i.dev(), i.ino(), i.size(), i.mtime() * 1_000_000_000 + i.mtime_nsec(), i.ctime() * 1_000_000_000 + i.ctime_nsec()));
        if self.primary_stamp != Some(stamp) {
            self.primary_stamp = Some(stamp);
            self.primary_names = self.read_primary_names().unwrap_or_default();
        }
        for name in &self.primary_names {
            if let Some(entry) = monitors.iter().find(|e| e.get("name").and_then(Value::as_str) == Some(name)) {
                return Some(entry);
            }
        }
        Some(first)
    }

    fn check_directories(&self) -> Result<()> {
        for path in [&self.config_dir, &self.directory] {
            if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Failure::Value(format!("Refusing to write through a symlink: {}", path.display())));
            }
        }
        Ok(())
    }

    fn write_temporary(&self, prefix: &str, data: &[u8], mode: Option<u32>) -> Result<(File, PathBuf)> {
        let (mut file, name) = fsx::mkstemp(&self.directory, prefix, "")?;
        let result = (|| -> io::Result<()> {
            file.write_all(data)?;
            file.flush()?;
            file.sync_all()?;
            if let Some(mode) = mode {
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&name);
            return Err(error.into());
        }
        Ok((file, name))
    }

    fn backup(&self, original: Option<&[u8]>) -> Result<()> {
        let Some(original) = original else { return Ok(()) };
        if read_regular(&self.backup_path, MAX_CSS_BYTES)?.is_some() {
            return Ok(());
        }
        let (_file, temporary) = self.write_temporary(".tessel-dock-gtk-backup-", original, None)?;
        // An exclusive hard link publishes the complete backup without
        // replacing another process's first backup.
        let result = match std::fs::hard_link(&temporary, &self.backup_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => read_regular(&self.backup_path, MAX_CSS_BYTES).map(|_| ()),
            Err(error) => Err(PathError::wrap2(error, &temporary, &self.backup_path).into()),
        };
        let _ = std::fs::remove_file(&temporary);
        result
    }

    fn replace(&self, path: &Path, data: &[u8], expected: Option<&[u8]>) -> Result<bool> {
        if let Some(hook) = &self.before_replace {
            hook(path);
        }
        if Some(data) == expected {
            return Ok(false);
        }
        let mode = match expected {
            Some(_) => Some(std::fs::symlink_metadata(path).map_err(|e| PathError::wrap(e, path))?.permissions().mode() & 0o7777),
            None => None,
        };
        let (_file, temporary) = self.write_temporary(".tessel-dock-gtk-", data, mode)?;
        let result = (|| -> Result<()> {
            self.check_directories()?;
            if read_regular(path, MAX_CSS_BYTES)?.as_deref() != expected {
                return Err(Failure::Value(format!("CSS file changed during publication: {}", path.display())));
            }
            match &self.replace_hook {
                Some(hook) => hook(&temporary, path)?,
                None => fsx::replace(&temporary, path)?,
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&temporary);
        result.map(|()| true)
    }

    fn try_publish(&self, enabled: bool, is_dark: bool, palette: Option<&Value>, colored: bool) -> Result<bool> {
        self.check_directories()?;
        let original = read_regular(&self.path, MAX_CSS_BYTES)?;
        if !enabled {
            let desired = without_managed_import(original.as_deref().unwrap_or_default());
            let Some(original) = original.as_deref().filter(|o| *o != desired) else { return Ok(false) };
            self.backup(Some(original))?;
            return self.replace(&self.path, &desired, Some(original));
        }
        let generated = color_css(is_dark, &palette_from_value(palette)?, colored);
        let current = read_regular(&self.colors_path, MAX_CSS_BYTES)?;
        if current.as_ref().is_some_and(|c| !c.starts_with(GENERATED_HEADER)) {
            return Err(Failure::Value(format!("Refusing to replace an unrelated CSS file: {}", self.colors_path.display())));
        }
        let desired = with_managed_import(original.as_deref().unwrap_or_default());
        std::fs::create_dir_all(&self.directory).map_err(|e| PathError::wrap(e, &self.directory))?;
        self.check_directories()?;
        if original.as_deref() != Some(&desired[..]) {
            self.backup(original.as_deref())?;
        }
        // Publish the target before the import, avoiding a broken import for
        // applications launched between the two atomic writes.
        let changed = self.replace(&self.colors_path, &generated, current.as_deref())?;
        Ok(self.replace(&self.path, &desired, original.as_deref())? || changed)
    }

    /// Return whether files changed; failures are reported in `error`.
    pub fn publish(&mut self, enabled: bool, is_dark: bool, palette: Option<&Value>, colored_buttons: bool) -> bool {
        match self.try_publish(enabled, is_dark, palette, colored_buttons) {
            Ok(changed) => {
                self.error = None;
                changed
            }
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root(name: &str) -> PathBuf {
        let base = std::env::temp_dir()
            .join(format!("d-gtkcss-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn dark() -> Value {
        json!({"background": [26, 21, 35], "foreground": [242, 239, 248], "accent": [157, 109, 222]})
    }

    fn light() -> Value {
        json!({"background": [242, 236, 249], "foreground": [35, 29, 44], "accent": [117, 71, 182]})
    }

    fn state(name: &str) -> (PathBuf, GtkColorScheme) {
        let root = root(name);
        let state = GtkColorScheme::new(Some(root.clone()));
        (root, state)
    }

    fn write_user_css(state: &GtkColorScheme, data: &[u8]) {
        std::fs::create_dir_all(&state.directory).unwrap();
        std::fs::write(&state.path, data).unwrap();
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn enable_preserves_exact_user_css_and_backs_up_once() {
        let (_root, mut state) = state("enable");
        let original: &[u8] = b"@import url(\"user-theme.css\");\r\n/* custom */\r\nlabel { letter-spacing: 1px; }";
        write_user_css(&state, original);
        std::fs::set_permissions(&state.path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(state.publish(true, true, Some(&dark()), false));
        assert_eq!(read(&state.path), cat(&[original, MANAGED_SEPARATOR, MANAGED_IMPORT]));
        assert_eq!(read(&state.backup_path), original);
        assert_eq!(std::fs::metadata(&state.path).unwrap().permissions().mode() & 0o777, 0o640);
        let updated = cat(&[&read(&state.path), b"\n/* later user edit */\n"]);
        std::fs::write(&state.path, updated).unwrap();
        assert!(state.publish(true, false, Some(&light()), false));
        assert_eq!(read(&state.path), cat(&[original, b"\n/* later user edit */\n", MANAGED_IMPORT]));
        assert_eq!(read(&state.backup_path), original);
    }

    #[test]
    fn disable_removes_only_exact_managed_import_and_retains_new_edits() {
        let (_root, mut state) = state("disable");
        let original: &[u8] = b"@import url(\"tessel-dock-colors.css\"); /* my separate import */\nwindow { padding: 2px; }\n";
        write_user_css(&state, original);
        state.publish(true, true, Some(&dark()), false);
        let edited = cat(&[&read(&state.path), b"/* user edit after enabling */\n"]);
        std::fs::write(&state.path, edited).unwrap();
        assert!(state.publish(false, true, None, false));
        assert_eq!(read(&state.path), cat(&[original, b"/* user edit after enabling */\n"]));
        assert!(state.colors_path.exists());
        assert!(!state.publish(false, true, None, false));
    }

    #[test]
    fn no_user_css_and_disabled_does_not_create_directory() {
        let (_root, mut state) = state("nocss");
        assert!(!state.publish(false, true, None, false));
        assert!(!state.directory.exists());
        assert!(state.publish(true, true, Some(&dark()), false));
        assert_eq!(read(&state.path), MANAGED_IMPORT);
        assert!(!state.backup_path.exists());
        assert!(state.publish(false, true, None, false));
        assert_eq!(read(&state.path), b"");
    }

    #[test]
    fn bom_and_duplicates_are_handled_without_changing_user_bytes() {
        let (_root, mut state) = state("bom");
        let user = cat(&[UTF8_BOM, b"/* UTF-8 user theme */\n"]);
        write_user_css(&state, &user);
        state.publish(true, true, Some(&dark()), false);
        let expected = cat(&[&user, MANAGED_IMPORT]);
        assert_eq!(read(&state.path), expected);
        std::fs::write(&state.path, cat(&[&expected, MANAGED_IMPORT])).unwrap();
        assert!(state.publish(true, true, Some(&dark()), false));
        assert_eq!(read(&state.path), expected);
        state.publish(false, true, None, false);
        assert_eq!(read(&state.path), user);
    }

    #[test]
    fn original_line_endings_and_missing_final_newline_round_trip() {
        let (_root, mut state) = state("endings");
        let bom_text = cat(&[UTF8_BOM, b"/* BOM without newline */"]);
        for original in [&b""[..], b"/* no final newline */", b"/* LF */\n", b"/* CRLF */\r\n", UTF8_BOM, &bom_text] {
            write_user_css(&state, original);
            assert!(state.publish(true, true, Some(&dark()), false), "{original:?}");
            assert!(read(&state.path).starts_with(original));
            assert!(!state.publish(true, true, Some(&dark()), false));
            assert!(state.publish(false, true, None, false));
            assert_eq!(read(&state.path), original);
        }
    }

    #[test]
    fn previously_prepended_import_moves_after_user_color_settings() {
        let (_root, mut state) = state("prepended");
        let original: &[u8] = b"@define-color accent_bg_color #ff0000;\nbutton { padding: 8px; }";
        write_user_css(&state, &cat(&[MANAGED_IMPORT, original]));
        assert!(state.publish(true, true, Some(&dark()), false));
        assert_eq!(read(&state.path), cat(&[original, MANAGED_SEPARATOR, MANAGED_IMPORT]));
        assert!(state.publish(false, true, None, false));
        assert_eq!(read(&state.path), original);
    }

    #[test]
    fn repeated_publication_and_restart_do_not_rewrite() {
        let (root, mut state) = state("repeat");
        state.publish(true, true, Some(&dark()), false);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = calls.clone();
        state.replace_hook = Some(Box::new(move |_, _| { counter.set(counter.get() + 1); Ok(()) }));
        assert!(!state.publish(true, true, Some(&dark()), false));
        let mut restarted = GtkColorScheme::new(Some(root));
        let counter = calls.clone();
        restarted.replace_hook = Some(Box::new(move |_, _| { counter.set(counter.get() + 1); Ok(()) }));
        assert!(!restarted.publish(true, true, Some(&dark()), false));
        assert_eq!(calls.get(), 0);
        assert!(state.error.is_none());
    }

    #[test]
    fn helper_collision_is_preserved_without_editing_user_css() {
        let (_root, mut state) = state("collision");
        let original: &[u8] = b"/* user's original CSS */\n";
        write_user_css(&state, original);
        std::fs::write(&state.colors_path, "/* unrelated existing file */").unwrap();
        assert!(!state.publish(true, true, Some(&dark()), false));
        assert!(state.error.as_deref().unwrap().contains("unrelated"));
        assert_eq!(read(&state.path), original);
        assert_eq!(read(&state.colors_path), b"/* unrelated existing file */");
        assert!(!state.backup_path.exists());
    }

    #[test]
    fn symlink_destinations_are_not_followed_or_replaced() {
        let (root, mut state) = state("symlinks");
        std::fs::create_dir_all(&state.directory).unwrap();
        let target = root.join("untouched");
        std::fs::write(&target, b"private original").unwrap();
        for path in [state.path.clone(), state.colors_path.clone(), state.backup_path.clone()] {
            if path != state.path {
                write_user_css(&state, b"/* CSS */\n");
            }
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(!state.publish(true, true, Some(&dark()), false), "{path:?}");
            assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
            assert_eq!(read(&target), b"private original");
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn symlink_gtk_directory_is_refused() {
        let (root, mut state) = state("symlinkdir");
        let target = root.join("other");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &state.directory).unwrap();
        assert!(!state.publish(true, true, Some(&dark()), false));
        assert!(state.error.as_deref().unwrap().contains("symlink"));
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn oversized_css_and_bad_palette_leave_user_file_unchanged() {
        let (_root, mut state) = state("oversized");
        let big = vec![b'x'; MAX_CSS_BYTES as usize + 1];
        let mut bad = dark();
        bad["accent"] = json!("red; bad CSS");
        for (original, palette) in [(&big[..], dark()), (&b"/* okay */"[..], bad)] {
            write_user_css(&state, original);
            assert!(!state.publish(true, true, Some(&palette), false));
            assert!(state.error.is_some());
            assert_eq!(read(&state.path), original);
        }
        assert_eq!(state.error.as_deref(), Some("GTK palette colors must be RGB triples"));
    }

    #[test]
    fn failed_atomic_write_is_recoverable_and_cleans_temporary_files() {
        let (_root, mut state) = state("atomic");
        let original: &[u8] = b"/* original */\n";
        write_user_css(&state, original);
        state.replace_hook = Some(Box::new(|_, _| Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only config"))));
        assert!(!state.publish(true, true, Some(&dark()), false));
        assert_eq!(read(&state.path), original);
        assert!(state.error.as_deref().unwrap().contains("read-only"));
        assert!(!std::fs::read_dir(&state.directory).unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".tessel-dock-")));
        state.replace_hook = None;
        assert!(state.publish(true, true, Some(&dark()), false));
        assert!(state.error.is_none());
    }

    #[test]
    fn concurrent_css_edit_is_retained() {
        let (_root, mut state) = state("concurrent");
        write_user_css(&state, b"/* original */\n");
        let target = state.path.clone();
        state.before_replace = Some(Box::new(move |path| {
            if path == target {
                std::fs::write(path, b"/* concurrent user change */\n").unwrap();
            }
        }));
        assert!(!state.publish(true, true, Some(&dark()), false));
        assert!(state.error.as_deref().unwrap().contains("changed during"));
        assert_eq!(read(&state.path), b"/* concurrent user change */\n");
    }

    #[test]
    fn primary_monitor_selection_only_uses_managed_block_and_refreshes() {
        let (_root, mut state) = state("primary");
        let mut first = dark();
        first["name"] = json!("DP-1");
        let mut primary = light();
        primary["name"] = json!("HDMI-A-1");
        std::fs::create_dir_all(state.primary_path.parent().unwrap()).unwrap();
        std::fs::write(&state.primary_path, format!(
            "SDL_VIDEO_DISPLAY_PRIORITY=DP-1\n{PRIMARY_BEGIN}\nSDL_VIDEO_DISPLAY_PRIORITY='HDMI-A-1'\nWAYLANDDRV_PRIMARY_MONITOR=DP-1\n{PRIMARY_END}\n")).unwrap();
        let both = [first.clone(), primary.clone()];
        assert!(std::ptr::eq(state.select_palette(&both).unwrap(), &both[1]));
        let reads = state.primary_reads.get();
        assert!(std::ptr::eq(state.select_palette(&both).unwrap(), &both[1]));
        assert_eq!(state.primary_reads.get(), reads, "should be cached");
        let only = [first.clone()];
        assert!(std::ptr::eq(state.select_palette(&only).unwrap(), &only[0]));
        std::fs::write(&state.primary_path, format!("{PRIMARY_BEGIN}\nWAYLANDDRV_PRIMARY_MONITOR=DP-1\n{PRIMARY_END}\n")).unwrap();
        let swapped = [primary.clone(), first.clone()];
        assert!(std::ptr::eq(state.select_palette(&swapped).unwrap(), &swapped[1]));
        std::fs::remove_file(&state.primary_path).unwrap();
        assert!(std::ptr::eq(state.select_palette(&swapped).unwrap(), &swapped[0]));
    }

    #[test]
    fn bad_primary_config_falls_back_without_executing_shell() {
        let (_root, mut state) = state("badprimary");
        let mut first = dark();
        first["name"] = json!("DP-1");
        std::fs::create_dir_all(state.primary_path.parent().unwrap()).unwrap();
        std::fs::write(&state.primary_path, format!(
            "{PRIMARY_BEGIN}\nSDL_VIDEO_DISPLAY_PRIORITY=$(touch /never-execute-this)\n{PRIMARY_END}\n")).unwrap();
        let only = [first];
        assert!(std::ptr::eq(state.select_palette(&only).unwrap(), &only[0]));
        assert!(state.select_palette(&[]).is_none());
        std::fs::write(&state.primary_path, b"\xffnot utf8").unwrap();
        assert!(std::ptr::eq(state.select_palette(&only).unwrap(), &only[0]));
        assert!(!Path::new("/never-execute-this").exists());
    }

    #[test]
    fn colors_include_gtk_and_adwaita_names_without_widget_overrides() {
        for (dark_mode, palette) in [(true, dark()), (false, light())] {
            let css = String::from_utf8(color_css(dark_mode, &palette_from_value(Some(&palette)).unwrap(), false)).unwrap();
            assert!(css.as_bytes().starts_with(GENERATED_HEADER));
            assert!(css.contains("@define-color theme_bg_color"));
            assert!(css.contains("@define-color accent_bg_color"));
            for role in ["window", "view", "headerbar", "sidebar", "card", "dialog", "popover", "accent"] {
                assert!(css.contains(&format!("--{role}-bg-color:")));
                assert!(css.contains(&format!("--{role}-fg-color:")));
            }
            let rules: Vec<&str> = css.lines().filter(|l| l.contains('{') && !l.starts_with(":root")).collect();
            assert!(!rules.is_empty());
            assert!(rules.iter().all(|l| { let s = l.split('{').next().unwrap(); s.contains("windowcontrols") || s.contains(".csd") }));
            let colors = css.split("window windowcontrols").next().unwrap();
            assert!(!colors.contains("opacity:") && !colors.contains("border-radius") && !colors.contains("background-color:"));
            let definition = |name: &str| -> Rgb {
                let marker = format!("@define-color {name} #");
                let start = css.find(&marker).unwrap() + marker.len();
                std::array::from_fn(|i| u8::from_str_radix(&css[start + i * 2..start + i * 2 + 2], 16).unwrap())
            };
            use tessel_gtk::palette::contrast;
            for role in ["window", "view", "headerbar", "sidebar", "card", "dialog", "popover", "accent"] {
                assert!(contrast(definition(&format!("{role}_bg_color")), definition(&format!("{role}_fg_color"))) >= 4.5);
            }
            assert!(contrast(definition("window_bg_color"), definition("accent_color")) >= 4.5);
        }
    }

    #[test]
    fn gtk_resolves_appended_palette_after_existing_accent_colors_and_rules() {
        // Python checks this through GTK's own CssProvider; GTK needs a
        // display here, so the import order is checked structurally instead
        // (the generated sheet is imported last, after every user rule).
        let (_root, mut state) = state("order");
        let original: &[u8] = b"@define-color accent_color #ff0000;\n:root { --accent-bg-color: #ff0000; }\nbutton { color: @accent_bg_color; }";
        write_user_css(&state, original);
        state.publish(true, true, Some(&dark()), false);
        let data = read(&state.path);
        assert!(data.ends_with(MANAGED_IMPORT));
        assert!(read(&state.colors_path).windows(b"accent_bg_color #9d6dde;".len()).any(|w| w == b"accent_bg_color #9d6dde;"));
        state.publish(false, true, None, false);
        assert_eq!(read(&state.path), original);
    }
}
