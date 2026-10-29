//! Papirus folder colours that follow the wallpaper (papirus_palette.py).
//!
//! The optional `papirus-folders` utility only ever changes private,
//! user-owned mirrors under `$XDG_DATA_HOME/waydock/papirus-icons/theme-<name>`
//! (marker `.waydock-papirus.json`, owner `waydock-papirus-palette-v1`),
//! exposed as `$XDG_DATA_HOME/icons/<name>` symlinks. Installed themes, user
//! themes and symlinked writable directories are never modified; disabling
//! restores the mirror's original colour. Work runs on one debounced worker
//! thread; `on_changed()` is delivered once on the main loop afterwards.

use super::fsx;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use suite_gtk::palette::Rgb;

pub const THEMES: [&str; 3] = ["Papirus", "Papirus-Dark", "Papirus-Light"];
pub const SIZES: [&str; 5] = ["22x22", "24x24", "32x32", "48x48", "64x64"];
pub const MARKER: &str = ".waydock-papirus.json";
pub const OWNER: &str = "waydock-papirus-palette-v1";
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(20);

fn value_error(text: String) -> String {
    text
}

/// `_rgb(palette.get("accent"))`.
pub fn accent_from_value(value: Option<&Value>) -> Result<Rgb, String> {
    let error = || "The folder accent must be an RGB triple".to_owned();
    let parts = value.and_then(Value::as_array).filter(|p| p.len() == 3).ok_or_else(error)?;
    let mut rgb = [0u8; 3];
    for (slot, part) in rgb.iter_mut().zip(parts) {
        *slot = part.as_i64().filter(|_| part.is_i64() || part.is_u64()).filter(|c| (0..=255).contains(c)).ok_or_else(error)? as u8;
    }
    Ok(rgb)
}

fn lab(rgb: Rgb) -> [f64; 3] {
    let linear = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (linear(rgb[0]), linear(rgb[1]), linear(rgb[2]));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).powf(1.0 / 3.0);
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).powf(1.0 / 3.0);
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).powf(1.0 / 3.0);
    [0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
     1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
     0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s]
}

fn float(text: &str) -> Result<f64, String> {
    suite_core::pyjson::py_float(&Value::String(text.into())).ok_or_else(|| format!("could not convert string to float: '{text}'"))
}

/// Read the large opaque front panel, excluding paper and shadow layers.
pub fn folder_rgb(path: &Path) -> Result<Rgb, String> {
    let size = std::fs::metadata(path).map_err(|e| fsx::os_error_path(&e, path))?.len();
    if size > 128 * 1024 {
        return Err("Folder SVG is unexpectedly large".into());
    }
    let data = std::fs::read(path).map_err(|e| fsx::os_error_path(&e, path))?;
    let text = String::from_utf8(data).map_err(|_| "not well-formed (invalid token)".to_owned())?;
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let document = roxmltree::Document::parse_with_options(&text, options).map_err(|e| e.to_string())?;
    let fill_pattern = regex::Regex::new(r"^#[0-9a-fA-F]{6}$").unwrap();
    let mut candidates: Vec<(f64, Rgb)> = Vec::new();
    for node in document.descendants().filter(|n| n.is_element()) {
        let style: Vec<(&str, &str)> = node.attribute("style").unwrap_or("").split(';')
            .filter_map(|part| part.split_once(':')).collect();
        let lookup = |key: &str, default: &str| -> String {
            style.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_owned())
                .unwrap_or_else(|| node.attribute(key).unwrap_or(default).to_owned())
        };
        let fill = suite_core::pyjson::py_strip(&lookup("fill", "")).to_owned();
        if !fill_pattern.is_match(&fill) {
            continue;
        }
        let opacity = float(&lookup("opacity", "1"))? * float(&lookup("fill-opacity", "1"))?;
        if opacity < 0.9 {
            continue;
        }
        let color = suite_gtk::palette::parse_hex(&fill.to_ascii_lowercase()).unwrap();
        let mut area = 0.0;
        if node.tag_name().name() == "rect" {
            if let (Ok(w), Ok(h)) = (float(node.attribute("width").unwrap_or("0")), float(node.attribute("height").unwrap_or("0"))) {
                area = w * h;
            }
        }
        candidates.push((area, color));
    }
    let mut best: Option<(f64, Rgb)> = None;
    for candidate in candidates {
        if best.is_none_or(|b| candidate.0 > b.0) {
            best = Some(candidate);
        }
    }
    best.map(|b| b.1).ok_or_else(|| "No solid folder color found".into())
}

/// `{color: front-panel rgb}` of a theme's `folder-<color>-documents.svg` variants.
pub fn available_colors(theme: &Path) -> BTreeMap<String, Rgb> {
    let places = theme.join("48x48").join("places");
    let mut names: Vec<String> = std::fs::read_dir(&places).map(|entries| entries.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    let valid = regex::Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    let mut colors = BTreeMap::new();
    for name in names {
        if name.starts_with('.') || !name.starts_with("folder-") || !name.ends_with("-documents.svg")
            || name.len() < "folder-".len() + "-documents.svg".len() {
            continue;
        }
        let color = &name["folder-".len()..name.len() - "-documents.svg".len()];
        // Literal argv avoids shell expansion; this also restricts manifest data.
        if !valid.is_match(color) {
            continue;
        }
        if let Ok(rgb) = folder_rgb(&places.join(format!("folder-{color}.svg"))) {
            colors.insert(color.to_owned(), rgb);
        }
    }
    colors
}

pub fn nearest_color(accent: Rgb, colors: &BTreeMap<String, Rgb>) -> Result<String, String> {
    let target = lab(accent);
    let distance = |rgb: &Rgb| {
        let other = lab(*rgb);
        ((target[0] - other[0]).powi(2) + (target[1] - other[1]).powi(2) + (target[2] - other[2]).powi(2)).sqrt()
    };
    let mut best: Option<(&String, f64)> = None;
    for (name, rgb) in colors {
        let d = distance(rgb);
        if best.is_none_or(|b| d < b.1) {
            best = Some((name, d));
        }
    }
    best.map(|b| b.0.clone()).ok_or_else(|| "No Papirus folder variants are installed".into())
}

/// `Path.resolve()` (non-strict): follow symlinks as far as they exist.
pub fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let absolute = if path.is_absolute() { path.to_owned() } else {
        std::env::current_dir().map(|c| c.join(path)).unwrap_or_else(|_| path.to_owned())
    };
    let mut current = absolute;
    for _ in 0..40 {
        match std::fs::read_link(&current) {
            Ok(target) => {
                current = match current.parent() {
                    Some(parent) if target.is_relative() => parent.join(target),
                    _ => target,
                };
                if let Ok(real) = std::fs::canonicalize(&current) {
                    return real;
                }
            }
            Err(_) => break,
        }
    }
    if let (Some(parent), Some(name)) = (current.parent(), current.file_name()) {
        if let Ok(real) = std::fs::canonicalize(parent) {
            return real.join(name);
        }
    }
    current
}

pub fn current_color(theme: &Path) -> Option<String> {
    let folder = resolve(&theme.join("48x48").join("places").join("folder.svg"));
    let name = folder.file_name()?.to_string_lossy().into_owned();
    let pattern = regex::Regex::new(r"^folder-([a-zA-Z0-9_-]+)\.svg$").unwrap();
    pattern.captures(&name).map(|c| c[1].to_owned())
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
}

/// Package directory/index mtimes catch updated variants without reading icons.
pub fn source_signature(source: &Path) -> Result<Value, String> {
    let mut paths = vec![source.to_owned(), source.join("index.theme")];
    for size in SIZES {
        paths.push(source.join(size).join("places"));
    }
    let mut records = Vec::new();
    for path in paths {
        match std::fs::metadata(&path) {
            Ok(info) => records.push(json!([resolve(&path).to_string_lossy(), mtime_ns(&info), info.len()])),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => records.push(json!([path.to_string_lossy(), null, null])),
            Err(error) => return Err(fsx::os_error_path(&error, &path)),
        }
    }
    Ok(Value::Array(records))
}

/// `subprocess.run(argv, check=True, capture_output=True, timeout=20)`.
pub trait ToolRunner: Send + Sync {
    /// `env` holds variables added to the inherited environment.
    fn run(&self, argv: &[String], env: Option<&[(String, String)]>, timeout: Duration) -> Result<(), String>;
}

fn argv_repr(argv: &[String]) -> String {
    let items: Vec<String> = argv.iter().map(|a| suite_core::pyjson::py_repr_value(&Value::String(a.clone()))).collect();
    format!("[{}]", items.join(", "))
}

pub struct ProcessRunner;

impl ToolRunner for ProcessRunner {
    fn run(&self, argv: &[String], env: Option<&[(String, String)]>, timeout: Duration) -> Result<(), String> {
        let mut command = std::process::Command::new(&argv[0]);
        command.args(&argv[1..]).stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        for (key, value) in env.unwrap_or_default() {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|e| fsx::os_error_path(&e, Path::new(&argv[0])))?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|e| fsx::os_error_text(&e))? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => {
                    use std::os::unix::process::ExitStatusExt;
                    let code = status.code().unwrap_or_else(|| -status.signal().unwrap_or(0));
                    return Err(format!("Command '{}' returned non-zero exit status {code}.", argv_repr(argv)));
                }
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Command '{}' timed out after {} seconds", argv_repr(argv), timeout.as_secs()));
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

/// Result of one worker pass: whether any variant changed (Python's
/// `_revision` moved) and the combined error.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub changed: bool,
    pub error: Option<String>,
}

/// The worker's `_apply` (tests substitute fakes, like Python).
pub trait PapirusApply: Send + Sync {
    fn apply(&self, enabled: bool, accent: Option<Rgb>, closed: &AtomicBool) -> ApplyOutcome;
}

/// The real mirror/recolour implementation.
pub struct Mirrors {
    pub home: PathBuf,
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub icons: PathBuf,
    pub private: PathBuf,
    pub find_program: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub runner: Box<dyn ToolRunner>,
}

fn lexists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::copy(source, destination).map_err(|e| fsx::os_error_path(&e, source))?;
    let info = std::fs::metadata(source).map_err(|e| fsx::os_error_path(&e, source))?;
    let times = std::fs::FileTimes::new().set_accessed(info.accessed().unwrap_or(std::time::SystemTime::now()))
        .set_modified(info.modified().unwrap_or(std::time::SystemTime::now()));
    let file = std::fs::OpenOptions::new().write(true).open(destination).map_err(|e| fsx::os_error_path(&e, destination))?;
    file.set_times(times).map_err(|e| fsx::os_error_path(&e, destination))
}

/// `shutil.copytree(source, destination, symlinks=True)`.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir(destination).map_err(|e| fsx::os_error_path(&e, destination))?;
    let entries = std::fs::read_dir(source).map_err(|e| fsx::os_error_path(&e, source))?;
    for entry in entries {
        let entry = entry.map_err(|e| fsx::os_error_text(&e))?;
        let (from, to) = (entry.path(), destination.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| fsx::os_error_text(&e))?;
        if kind.is_symlink() {
            let target = std::fs::read_link(&from).map_err(|e| fsx::os_error_path(&e, &from))?;
            std::os::unix::fs::symlink(target, &to).map_err(|e| fsx::os_error_path(&e, &to))?;
        } else if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            copy_file(&from, &to)?;
        }
    }
    let permissions = std::fs::metadata(source).map_err(|e| fsx::os_error_path(&e, source))?.permissions();
    let _ = std::fs::set_permissions(destination, permissions);
    Ok(())
}

fn symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| fsx::os_error_path(&e, link))
}

fn mkdtemp(directory: &Path, prefix: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    for _ in 0..100 {
        let name: String = (0..8).map(|_| b"abcdefghijklmnopqrstuvwxyz0123456789_"[glib::random_int_range(0, 37) as usize] as char).collect();
        let path = directory.join(format!("{prefix}{name}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(fsx::os_error_path(&e, &path)),
        }
    }
    Err("No usable temporary directory name found".into())
}

impl Mirrors {
    pub fn new(home: Option<PathBuf>, data_home: Option<PathBuf>, data_dirs: Option<Vec<PathBuf>>) -> Self {
        let home = home.unwrap_or_else(suite_core::paths::home);
        let data_home = data_home.unwrap_or_else(|| match std::env::var("XDG_DATA_HOME") {
            Ok(value) if !value.is_empty() => PathBuf::from(value),
            _ => home.join(".local/share"),
        });
        let data_dirs = data_dirs.unwrap_or_else(|| std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
            .split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect());
        Self {
            icons: data_home.join("icons"), private: data_home.join("waydock").join("papirus-icons"),
            home, data_home, data_dirs,
            find_program: Box::new(|name| suite_core::shell::which(name).map(|p| p.to_string_lossy().into_owned())),
            runner: Box::new(ProcessRunner),
        }
    }

    fn source(&self, name: &str) -> Option<PathBuf> {
        self.data_dirs.iter().map(|d| d.join("icons").join(name))
            .find(|source| source.join("index.theme").is_file() && !resolve(source).starts_with(&self.private))
    }

    pub fn metadata(&self, target: &Path) -> Option<Map<String, Value>> {
        let text = fsx::read_text(&target.join(MARKER)).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        let object = value.as_object()?.clone();
        (object.get("owner").and_then(Value::as_str) == Some(OWNER)).then_some(object)
    }

    fn safe_target(&self, name: &str) -> Result<PathBuf, String> {
        if is_symlink(&self.private) {
            return Err(value_error(format!("Symlinked Papirus working directory left unchanged: {}", self.private.display())));
        }
        let target = self.private.join(format!("theme-{name}"));
        for root in [self.home.join(".icons"), self.icons.clone()] {
            let alias = root.join(name);
            if lexists(&alias) && !(is_symlink(&alias) && resolve(&alias) == target) {
                return Err(format!("Existing custom {name} icon theme left unchanged: {}", alias.display()));
            }
        }
        if lexists(&target) && (is_symlink(&target) || self.metadata(&target).is_none()) {
            return Err(format!("Unmanaged Papirus folder directory left unchanged: {}", target.display()));
        }
        if target.exists() {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata(&target).map_err(|e| fsx::os_error_path(&e, &target))?.uid();
            if uid != unsafe { libc::geteuid() } {
                return Err(format!("Papirus mirror is not owned by this user: {}", target.display()));
            }
            for size in SIZES {
                let directory = target.join(size);
                let places = directory.join("places");
                if is_symlink(&directory) || is_symlink(&places) {
                    return Err(format!("Symlinked writable icon directory left unchanged: {}", places.display()));
                }
            }
        }
        Ok(target)
    }

    fn mirror(&self, name: &str, source: &Path, changed: &mut bool) -> Result<PathBuf, String> {
        let target = self.safe_target(name)?;
        let metadata = self.metadata(&target);
        let signature = source_signature(source)?;
        if metadata.as_ref().is_some_and(|m| m.get("source_signature") == Some(&signature)) {
            return Ok(target);
        }
        std::fs::create_dir_all(&self.private).map_err(|e| fsx::os_error_path(&e, &self.private))?;
        std::fs::create_dir_all(&self.icons).map_err(|e| fsx::os_error_path(&e, &self.icons))?;
        let stage = mkdtemp(&self.private, ".new-papirus-")?;
        let original = match &metadata {
            Some(m) => m.get("original_color").cloned().unwrap_or(Value::Null),
            None => current_color(source).map(Value::String).unwrap_or(Value::Null),
        };
        let mut record = Map::new();
        record.insert("owner".into(), json!(OWNER));
        record.insert("source_signature".into(), signature);
        record.insert("original_color".into(), original);
        let result = (|| -> Result<(), String> {
            copy_file(&source.join("index.theme"), &stage.join("index.theme"))?;
            let entries = std::fs::read_dir(source).map_err(|e| fsx::os_error_path(&e, source))?;
            for item in entries {
                let item = item.map_err(|e| fsx::os_error_text(&e))?;
                let item_name = item.file_name().to_string_lossy().into_owned();
                if item_name == "index.theme" || item_name == "icon-theme.cache" {
                    continue;
                }
                let (path, dest) = (item.path(), stage.join(&item_name));
                if SIZES.contains(&item_name.as_str()) && path.is_dir() {
                    std::fs::create_dir(&dest).map_err(|e| fsx::os_error_path(&e, &dest))?;
                    let contexts = std::fs::read_dir(&path).map_err(|e| fsx::os_error_path(&e, &path))?;
                    for context in contexts {
                        let context = context.map_err(|e| fsx::os_error_text(&e))?;
                        let context_path = context.path();
                        if context.file_name() == "places" && context_path.is_dir() {
                            copy_tree(&context_path, &dest.join("places"))?;
                        } else {
                            symlink(&resolve(&context_path), &dest.join(context.file_name()))?;
                        }
                    }
                } else if let Some(base) = item_name.strip_suffix("@2x").filter(|b| source.join(b).exists()) {
                    symlink(Path::new(base), &dest)?;
                } else {
                    symlink(&resolve(&path), &dest)?;
                }
            }
            let marker = suite_core::pyfmt::json_dumps(&Value::Object(record.clone()), &suite_core::pyfmt::JsonStyle::DEFAULT);
            std::fs::write(stage.join(MARKER), marker).map_err(|e| fsx::os_error_path(&e, &stage))?;
            if target.exists() {
                let previous = self.private.join(format!(".old-{name}"));
                if previous.exists() {
                    if self.metadata(&previous).is_none() {
                        return Err(format!("Unmanaged backup directory left unchanged: {}", previous.display()));
                    }
                    std::fs::remove_dir_all(&previous).map_err(|e| fsx::os_error_path(&e, &previous))?;
                }
                std::fs::rename(&target, &previous).map_err(|e| fsx::error_text(&fsx::PathError::wrap2(e, &target, &previous)))?;
                if let Err(error) = std::fs::rename(&stage, &target) {
                    let _ = std::fs::rename(&previous, &target);
                    return Err(fsx::error_text(&fsx::PathError::wrap2(error, &stage, &target)));
                }
                std::fs::remove_dir_all(&previous).map_err(|e| fsx::os_error_path(&e, &previous))?;
            } else {
                std::fs::rename(&stage, &target).map_err(|e| fsx::error_text(&fsx::PathError::wrap2(e, &stage, &target)))?;
            }
            let alias = self.icons.join(name);
            if !lexists(&alias) {
                symlink(&target, &alias)?;
            }
            *changed = true;
            Ok(())
        })();
        if stage.exists() {
            let _ = std::fs::remove_dir_all(&stage);
        }
        result.map(|()| target)
    }

    fn apply_theme(&self, name: &str, enabled: bool, accent: Option<Rgb>, program: &str, cache_program: Option<&str>,
                   changed: &mut bool) -> Result<(), String> {
        let source = self.source(name);
        let (target, color) = if enabled {
            let Some(source) = source else { return Ok(()) };
            let colors = available_colors(&source);
            let color = nearest_color(accent.expect("enabled publication has an accent"), &colors)?;
            (self.mirror(name, &source, changed)?, Some(color))
        } else {
            let target = self.private.join(format!("theme-{name}"));
            let Some(metadata) = self.metadata(&target) else { return Ok(()) };
            let target = self.safe_target(name)?;
            let color = metadata.get("original_color").and_then(Value::as_str).map(str::to_owned);
            if !color.as_ref().is_some_and(|c| available_colors(&target).contains_key(c)) {
                return Err(format!("Original {name} folder color is unavailable"));
            }
            (target, color)
        };
        if current_color(&target) == color {
            return Ok(());
        }
        let color = color.unwrap();
        let target_text = target.to_string_lossy().into_owned();
        let env = [("DISABLE_UPDATE_ICON_CACHE".to_owned(), "1".to_owned()),
                   ("USER_HOME".to_owned(), self.home.to_string_lossy().into_owned())];
        // Private basenames intentionally aren't Papirus-Dark/Light: those
        // utility branches rebuild sibling caches even with DISABLE set.
        self.runner.run(&[program.to_owned(), "--theme".into(), target_text.clone(), "--color".into(), color, "--once".into()],
                        Some(&env), TOOL_TIMEOUT)?;
        *changed = true;
        if let Some(cache) = cache_program {
            self.runner.run(&[cache.to_owned(), "-qf".into(), target_text.clone()], None, TOOL_TIMEOUT)?;
        }
        let c_path = std::ffi::CString::new(target_text.as_bytes()).map_err(|e| e.to_string())?;
        if unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), std::ptr::null(), 0) } != 0 {
            return Err(fsx::os_error_path(&std::io::Error::last_os_error(), &target));
        }
        Ok(())
    }
}

impl PapirusApply for Mirrors {
    fn apply(&self, enabled: bool, accent: Option<Rgb>, closed: &AtomicBool) -> ApplyOutcome {
        let mut outcome = ApplyOutcome::default();
        let Some(program) = (self.find_program)("papirus-folders") else { return outcome };
        let cache_program = (self.find_program)("gtk-update-icon-cache");
        let mut errors = Vec::new();
        for name in THEMES {
            if closed.load(Ordering::SeqCst) {
                return outcome;
            }
            if let Err(error) = self.apply_theme(name, enabled, accent, &program, cache_program.as_deref(), &mut outcome.changed) {
                errors.push(format!("{name}: {error}"));
            }
        }
        if !errors.is_empty() {
            outcome.error = Some(errors.join("; "));
        }
        outcome
    }
}

/// Queue a callback on the main loop from the worker (`GLib.idle_add`).
pub trait IdleQueue: Send + Sync {
    fn idle_add(&self, callback: Box<dyn FnOnce() + Send>) -> u64;
    fn source_remove(&self, id: u64);
}

/// Idle sources on the main context that created the publisher.
pub struct GlibIdle {
    context: glib::MainContext,
    next: AtomicU64,
    sources: Arc<Mutex<std::collections::HashMap<u64, glib::Source>>>,
}

impl GlibIdle {
    pub fn new() -> Self {
        Self { context: glib::MainContext::ref_thread_default(), next: AtomicU64::new(1), sources: Arc::default() }
    }
}

impl Default for GlibIdle {
    fn default() -> Self {
        Self::new()
    }
}

impl IdleQueue for GlibIdle {
    fn idle_add(&self, callback: Box<dyn FnOnce() + Send>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let sources = self.sources.clone();
        let mut callback = Some(callback);
        let source = glib::idle_source_new(None, glib::Priority::DEFAULT_IDLE, move || {
            sources.lock().unwrap().remove(&id);
            if let Some(callback) = callback.take() {
                callback();
            }
            glib::ControlFlow::Break
        });
        self.sources.lock().unwrap().insert(id, source.clone());
        source.attach(Some(&self.context));
        id
    }

    fn source_remove(&self, id: u64) {
        if let Some(source) = self.sources.lock().unwrap().remove(&id) {
            source.destroy();
        }
    }
}

thread_local! {
    static CALLBACKS: std::cell::RefCell<std::collections::HashMap<u64, std::rc::Rc<dyn Fn()>>> = Default::default();
}
static NEXT_PUBLISHER: AtomicU64 = AtomicU64::new(1);

type Key = (bool, Option<Rgb>);

#[derive(Default)]
struct State {
    pending: Option<(Key, Instant)>,
    last: Option<Key>,
    last_check: Option<Instant>,
    thread: bool,
    refresh_source: u64,
    error: Option<String>,
}

struct Shared {
    state: Mutex<State>,
    condition: Condvar,
    closed: AtomicBool,
    idle: Box<dyn IdleQueue>,
    apply: Box<dyn PapirusApply>,
    debounce: Mutex<Duration>,
    has_callback: bool,
    id: u64,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// Construction options (Python keyword arguments).
pub struct PapirusOptions {
    pub debounce: Duration,
    pub apply: Option<Box<dyn PapirusApply>>,
    pub idle: Option<Box<dyn IdleQueue>>,
}

impl Default for PapirusOptions {
    fn default() -> Self {
        Self { debounce: Duration::from_millis(400), apply: None, idle: None }
    }
}

pub struct PapirusPalette(Arc<Shared>);

impl PapirusPalette {
    /// `on_changed()` runs on the creating thread's main context.
    pub fn new(on_changed: Option<Box<dyn Fn()>>, options: PapirusOptions) -> Self {
        let id = NEXT_PUBLISHER.fetch_add(1, Ordering::SeqCst);
        let has_callback = on_changed.is_some();
        if let Some(callback) = on_changed {
            CALLBACKS.with(|c| c.borrow_mut().insert(id, std::rc::Rc::from(callback)));
        }
        Self(Arc::new(Shared {
            state: Mutex::default(), condition: Condvar::new(), closed: AtomicBool::new(false),
            idle: options.idle.unwrap_or_else(|| Box::new(GlibIdle::new())),
            apply: options.apply.unwrap_or_else(|| Box::new(Mirrors::new(None, None, None))),
            debounce: Mutex::new(options.debounce), has_callback, id, thread: Mutex::new(None),
        }))
    }

    pub fn error(&self) -> Option<String> {
        self.0.state.lock().unwrap().error.clone()
    }

    pub fn set_debounce(&self, debounce: Duration) {
        *self.0.debounce.lock().unwrap() = debounce;
    }

    pub fn has_worker(&self) -> bool {
        self.0.state.lock().unwrap().thread
    }

    pub fn refresh_source(&self) -> u64 {
        self.0.state.lock().unwrap().refresh_source
    }

    /// Wait for the worker thread to exit (after `close`).
    pub fn join(&self, timeout: Duration) -> bool {
        let handle = self.0.thread.lock().unwrap().take();
        let Some(handle) = handle else { return true };
        let deadline = Instant::now() + timeout;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let finished = handle.is_finished();
        if finished {
            let _ = handle.join();
        } else {
            *self.0.thread.lock().unwrap() = Some(handle);
        }
        finished
    }

    /// Publish the folder colour for `palette` (`None` while disabled).
    pub fn publish(&self, enabled: bool, palette: Option<&Value>) -> bool {
        if enabled && palette.is_none() {
            return false;
        }
        let accent = if enabled {
            let palette = palette.unwrap();
            let result = match palette.as_object() {
                Some(object) => accent_from_value(object.get("accent")),
                None => Err(format!("'{}' object has no attribute 'get'", python_type(palette))),
            };
            match result {
                Ok(accent) => Some(accent),
                Err(error) => {
                    self.0.state.lock().unwrap().error = Some(error);
                    return false;
                }
            }
        } else {
            None
        };
        self.publish_key((enabled, accent))
    }

    /// `publish` with an already validated accent.
    pub fn publish_accent(&self, enabled: bool, accent: Option<Rgb>) -> bool {
        if enabled && accent.is_none() {
            return false;
        }
        self.publish_key((enabled, if enabled { accent } else { None }))
    }

    fn publish_key(&self, key: Key) -> bool {
        let mut state = self.0.state.lock().unwrap();
        if self.0.closed.load(Ordering::SeqCst) {
            return false;
        }
        let now = Instant::now();
        if state.last == Some(key) && state.last_check.is_some_and(|t| now.duration_since(t) < Duration::from_secs(300)) {
            return false;
        }
        state.last = Some(key);
        state.last_check = Some(now);
        state.pending = Some((key, now + *self.0.debounce.lock().unwrap()));
        if !state.thread {
            state.thread = true;
            let shared = self.0.clone();
            let handle = std::thread::Builder::new().name("papirus-palette".into()).spawn(move || worker(shared)).ok();
            *self.0.thread.lock().unwrap() = handle;
        }
        self.0.condition.notify_all();
        true
    }

    pub fn close(&self) {
        let mut state = self.0.state.lock().unwrap();
        self.0.closed.store(true, Ordering::SeqCst);
        state.pending = None;
        if state.refresh_source != 0 {
            self.0.idle.source_remove(state.refresh_source);
            state.refresh_source = 0;
        }
        self.0.condition.notify_all();
    }

    /// `_queue_refresh` (exposed for tests).
    pub fn queue_refresh(&self) {
        queue_refresh(&self.0);
    }
}

impl Drop for PapirusPalette {
    fn drop(&mut self) {
        self.close();
        CALLBACKS.with(|c| c.borrow_mut().remove(&self.0.id));
    }
}

fn python_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn queue_refresh(shared: &Arc<Shared>) {
    // Hold the lock until the source id is recorded so a fast main loop
    // cannot dispatch it before registration.
    let mut state = shared.state.lock().unwrap();
    if shared.closed.load(Ordering::SeqCst) || !shared.has_callback || state.refresh_source != 0 {
        return;
    }
    let target = Arc::downgrade(shared);
    state.refresh_source = shared.idle.idle_add(Box::new(move || {
        let Some(shared) = target.upgrade() else { return };
        {
            let mut state = shared.state.lock().unwrap();
            state.refresh_source = 0;
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
        }
        let callback = CALLBACKS.with(|c| c.borrow().get(&shared.id).cloned());
        if let Some(callback) = callback {
            callback();
        }
    }));
}

fn worker(shared: Arc<Shared>) {
    loop {
        let key = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if shared.closed.load(Ordering::SeqCst) {
                    return;
                }
                let Some((key, deadline)) = state.pending else {
                    state = shared.condition.wait(state).unwrap();
                    continue;
                };
                let now = Instant::now();
                if deadline > now {
                    state = shared.condition.wait_timeout(state, deadline - now).unwrap().0;
                    continue;
                }
                state.pending = None;
                break key;
            }
        };
        let outcome = shared.apply.apply(key.0, key.1, &shared.closed);
        shared.state.lock().unwrap().error = outcome.error.clone();
        // Also refresh successful changes if another installed variant
        // failed. Unchanged colours and missing optional tools do nothing.
        if outcome.changed {
            queue_refresh(&shared);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn svg(color: &str) -> String {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><path style=\"fill:#303030\"/>\
                 <rect width=\"36\" height=\"16\" fill=\"#eeeeee\"/><rect width=\"40\" height=\"26\" style=\"fill:{color}\"/>\
                 <path style=\"opacity:0.1;fill:#ffffff\"/></svg>")
    }

    #[derive(Clone, Default)]
    struct Calls(Arc<Mutex<Vec<(Vec<String>, Option<Vec<(String, String)>>, Duration)>>>);

    struct FakeRunner {
        calls: Calls,
        fail_cache: bool,
    }

    impl ToolRunner for FakeRunner {
        fn run(&self, argv: &[String], env: Option<&[(String, String)]>, timeout: Duration) -> Result<(), String> {
            self.calls.0.lock().unwrap().push((argv.to_vec(), env.map(|e| e.to_vec()), timeout));
            if argv[0].ends_with("papirus-folders") {
                let theme = PathBuf::from(&argv[argv.iter().position(|a| a == "--theme").unwrap() + 1]);
                let color = &argv[argv.iter().position(|a| a == "--color").unwrap() + 1];
                for size in SIZES {
                    let places = theme.join(size).join("places");
                    for entry in std::fs::read_dir(&places).unwrap() {
                        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                        if name.starts_with(&format!("folder-{color}")) && name.ends_with(".svg") {
                            let dest = places.join(name.replacen(&format!("-{color}"), "", 1));
                            let _ = std::fs::remove_file(&dest);
                            std::os::unix::fs::symlink(&name, &dest).unwrap();
                        }
                    }
                }
            } else {
                if self.fail_cache {
                    return Err(format!("Command '{}' returned non-zero exit status 1.", argv_repr(argv)));
                }
                std::fs::write(PathBuf::from(argv.last().unwrap()).join("icon-theme.cache"), b"test").unwrap();
            }
            Ok(())
        }
    }

    struct Fixture {
        home: PathBuf,
        data: PathBuf,
        system: PathBuf,
        calls: Calls,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("d-papirus-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let (home, system) = (root.join("home"), root.join("system"));
            for theme_name in THEMES {
                let theme = system.join("icons").join(theme_name);
                std::fs::create_dir_all(&theme).unwrap();
                std::fs::write(theme.join("index.theme"), format!("[Icon Theme]\nName={theme_name}\n")).unwrap();
                for size in SIZES {
                    let places = theme.join(size).join("places");
                    std::fs::create_dir_all(&places).unwrap();
                    for (color, value) in [("blue", "#5294e2"), ("red", "#ff2222"), ("violet", "#7e57c2")] {
                        std::fs::write(places.join(format!("folder-{color}.svg")), svg(value)).unwrap();
                        std::fs::write(places.join(format!("folder-{color}-documents.svg")), svg(value)).unwrap();
                    }
                    std::os::unix::fs::symlink("folder-blue.svg", places.join("folder.svg")).unwrap();
                    std::os::unix::fs::symlink("folder-blue-documents.svg", places.join("folder-documents.svg")).unwrap();
                    let apps = theme.join(size).join("apps");
                    std::fs::create_dir(&apps).unwrap();
                    std::fs::write(apps.join("example.svg"), svg("#ffffff")).unwrap();
                }
                std::os::unix::fs::symlink("48x48", theme.join("48x48@2x")).unwrap();
            }
            Self { data: home.join(".local/share"), home, system, calls: Calls::default() }
        }

        fn mirrors(&self, fail_cache: bool) -> Mirrors {
            let mut mirrors = Mirrors::new(Some(self.home.clone()), Some(self.data.clone()), Some(vec![self.system.clone()]));
            mirrors.find_program = Box::new(|name| Some(format!("/bin/{name}")));
            mirrors.runner = Box::new(FakeRunner { calls: self.calls.clone(), fail_cache });
            mirrors
        }

        fn calls(&self) -> usize {
            self.calls.0.lock().unwrap().len()
        }
    }

    fn apply(mirrors: &Mirrors, enabled: bool, accent: Option<Rgb>) -> ApplyOutcome {
        mirrors.apply(enabled, accent, &AtomicBool::new(false))
    }

    #[test]
    fn matches_actual_front_panel_colors() {
        let fixture = Fixture::new("colors");
        let colors = available_colors(&fixture.system.join("icons/Papirus"));
        assert_eq!(colors["violet"], [126, 87, 194]);
        assert_eq!(nearest_color([125, 88, 192], &colors).unwrap(), "violet");
        assert_eq!(nearest_color([250, 32, 34], &colors).unwrap(), "red");
        assert!(accent_from_value(Some(&json!([300, 1, 2]))).is_err());
        assert!(nearest_color([1, 2, 3], &BTreeMap::new()).is_err());
    }

    #[test]
    fn mirrors_only_mutable_places_and_never_changes_sources() {
        let fixture = Fixture::new("mirror");
        let mirrors = fixture.mirrors(false);
        assert_eq!(apply(&mirrors, true, Some([255, 34, 34])), ApplyOutcome { changed: true, error: None });
        for name in THEMES {
            let source = fixture.system.join("icons").join(name);
            let alias = fixture.data.join("icons").join(name);
            let target = resolve(&alias);
            assert!(is_symlink(&alias));
            assert_eq!(target.file_name().unwrap().to_string_lossy(), format!("theme-{name}"));
            assert_eq!(current_color(&target).as_deref(), Some("red"));
            assert_eq!(current_color(&source).as_deref(), Some("blue"));
            assert!(!is_symlink(&target.join("48x48/places")));
            assert!(is_symlink(&target.join("48x48/apps")));
            assert_eq!(resolve(&target.join("48x48@2x/places/folder.svg")), target.join("48x48/places/folder-red.svg"));
            let metadata: Value = serde_json::from_str(&std::fs::read_to_string(target.join(MARKER)).unwrap()).unwrap();
            assert_eq!(metadata["original_color"], "blue");
            assert_eq!(metadata["owner"], OWNER);
        }
        let calls = fixture.calls.0.lock().unwrap();
        assert_eq!(calls.len(), 6);
        for (argv, env, timeout) in calls.iter() {
            assert_eq!(*timeout, Duration::from_secs(20));
            assert!(!argv.contains(&"sudo".to_owned()));
            if argv[0].ends_with("papirus-folders") {
                assert!(argv.contains(&"--once".to_owned()));
                let env = env.as_ref().unwrap();
                assert!(env.contains(&("DISABLE_UPDATE_ICON_CACHE".into(), "1".into())));
                assert!(env.contains(&("USER_HOME".into(), fixture.home.to_string_lossy().into_owned())));
            }
        }
    }

    #[test]
    fn repeated_color_does_not_rerun_tools_and_disable_restores() {
        let fixture = Fixture::new("repeat");
        let mirrors = fixture.mirrors(false);
        apply(&mirrors, true, Some([255, 34, 34]));
        apply(&mirrors, true, Some([254, 35, 35]));
        assert_eq!(fixture.calls(), 6);
        apply(&mirrors, false, None);
        assert_eq!(fixture.calls(), 12);
        for name in THEMES {
            assert_eq!(current_color(&fixture.data.join("icons").join(name)).as_deref(), Some("blue"));
        }
        assert_eq!(apply(&mirrors, false, None), ApplyOutcome::default());
        assert_eq!(fixture.calls(), 12);
    }

    #[test]
    fn restore_survives_process_restart() {
        let fixture = Fixture::new("restart");
        apply(&fixture.mirrors(false), true, Some([255, 34, 34]));
        let second = fixture.mirrors(false);
        apply(&second, true, Some([126, 87, 194]));
        apply(&second, false, None);
        assert_eq!(current_color(&fixture.data.join("icons/Papirus")).as_deref(), Some("blue"));
    }

    #[test]
    fn existing_user_theme_is_preserved() {
        let fixture = Fixture::new("custom");
        let custom = fixture.home.join(".icons/Papirus");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("index.theme"), "custom theme").unwrap();
        let outcome = apply(&fixture.mirrors(false), true, Some([255, 34, 34]));
        assert!(outcome.error.unwrap().contains("Existing custom Papirus"));
        assert_eq!(std::fs::read_to_string(custom.join("index.theme")).unwrap(), "custom theme");
        assert!(!fixture.data.join("icons/Papirus").exists());
        assert_eq!(current_color(&fixture.system.join("icons/Papirus")).as_deref(), Some("blue"));
    }

    #[test]
    fn symlinked_places_cannot_modify_system_themes() {
        let fixture = Fixture::new("symlinked");
        let mirrors = fixture.mirrors(false);
        apply(&mirrors, true, Some([255, 34, 34]));
        let target = resolve(&fixture.data.join("icons/Papirus"));
        let places = target.join("48x48/places");
        std::fs::rename(&places, target.join("48x48/original-places")).unwrap();
        std::os::unix::fs::symlink(fixture.system.join("icons/Papirus/48x48/places"), &places).unwrap();
        fixture.calls.0.lock().unwrap().clear();
        let outcome = apply(&mirrors, true, Some([126, 87, 194]));
        assert!(outcome.error.unwrap().contains("Symlinked writable icon directory"));
        assert_eq!(current_color(&fixture.system.join("icons/Papirus")).as_deref(), Some("blue"));
        let text = target.to_string_lossy().into_owned();
        assert!(!fixture.calls.0.lock().unwrap().iter().any(|(argv, ..)| argv.contains(&text)));
    }

    #[test]
    fn source_update_refreshes_owned_mirror() {
        let fixture = Fixture::new("update");
        let mirrors = fixture.mirrors(false);
        apply(&mirrors, true, Some([255, 34, 34]));
        let source = fixture.system.join("icons/Papirus");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(source.join("index.theme"), "[Icon Theme]\nName=Updated\n").unwrap();
        for size in SIZES {
            std::fs::write(source.join(size).join("places/new-icon.svg"), svg("#ffff00")).unwrap();
        }
        apply(&mirrors, true, Some([126, 87, 194]));
        let target = resolve(&fixture.data.join("icons/Papirus"));
        assert!(target.join("48x48/places/new-icon.svg").is_file());
        assert!(std::fs::read_to_string(target.join("index.theme")).unwrap().contains("Updated"));
        assert_eq!(current_color(&target).as_deref(), Some("violet"));
        let metadata: Value = serde_json::from_str(&std::fs::read_to_string(target.join(MARKER)).unwrap()).unwrap();
        assert_eq!(metadata["original_color"], "blue");
    }

    struct Recording {
        sender: Mutex<mpsc::Sender<(bool, Option<Rgb>)>>,
        gate: Option<Arc<(Mutex<bool>, Condvar)>>,
        changed: bool,
    }

    impl PapirusApply for Recording {
        fn apply(&self, enabled: bool, accent: Option<Rgb>, _closed: &AtomicBool) -> ApplyOutcome {
            self.sender.lock().unwrap().send((enabled, accent)).unwrap();
            if let Some(gate) = &self.gate {
                let (lock, condition) = &**gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = condition.wait_timeout(open, Duration::from_secs(2)).unwrap().0;
                }
            }
            ApplyOutcome { changed: self.changed, error: None }
        }
    }

    #[derive(Clone, Default)]
    struct FakeIdle(Arc<Mutex<(Vec<Option<Box<dyn FnOnce() + Send>>>, Vec<u64>)>>);

    impl IdleQueue for FakeIdle {
        fn idle_add(&self, callback: Box<dyn FnOnce() + Send>) -> u64 {
            let mut inner = self.0.lock().unwrap();
            inner.0.push(Some(callback));
            inner.0.len() as u64
        }
        fn source_remove(&self, id: u64) {
            self.0.lock().unwrap().1.push(id);
        }
    }

    impl FakeIdle {
        fn take(&self, id: u64) -> Box<dyn FnOnce() + Send> {
            self.0.lock().unwrap().0[id as usize - 1].take().unwrap()
        }
        fn count(&self) -> usize {
            self.0.lock().unwrap().0.len()
        }
        fn cancelled(&self) -> Vec<u64> {
            self.0.lock().unwrap().1.clone()
        }
    }

    fn recording(changed: bool, gate: Option<Arc<(Mutex<bool>, Condvar)>>) -> (Box<Recording>, mpsc::Receiver<(bool, Option<Rgb>)>) {
        let (sender, receiver) = mpsc::channel();
        (Box::new(Recording { sender: Mutex::new(sender), gate, changed }), receiver)
    }

    #[test]
    fn missing_optional_utility_and_pending_palette_do_nothing() {
        let fixture = Fixture::new("missing");
        let mut mirrors = fixture.mirrors(false);
        mirrors.find_program = Box::new(|_| None);
        assert_eq!(apply(&mirrors, true, Some([255, 34, 34])), ApplyOutcome::default());
        assert!(!mirrors.private.exists());
        let publisher = PapirusPalette::new(None, PapirusOptions { apply: Some(Box::new(mirrors)), idle: Some(Box::new(FakeIdle::default())), ..Default::default() });
        assert!(!publisher.publish(true, None));
        assert!(!publisher.has_worker());
    }

    #[test]
    fn single_worker_coalesces_and_deduplicates() {
        let (apply, received) = recording(false, None);
        let publisher = PapirusPalette::new(None, PapirusOptions { debounce: Duration::from_millis(30), apply: Some(apply),
                                                                   idle: Some(Box::new(FakeIdle::default())) });
        assert!(publisher.publish(true, Some(&json!({"accent": [255, 0, 0]}))));
        assert!(publisher.publish(true, Some(&json!({"accent": [0, 255, 0]}))));
        assert!(!publisher.publish(true, Some(&json!({"accent": [0, 255, 0]}))));
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), (true, Some([0, 255, 0])));
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
        publisher.close();
        assert!(publisher.join(Duration::from_secs(2)));
        assert!(!publisher.publish(false, None));
    }

    #[test]
    fn busy_worker_runs_only_latest_queued_color() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (apply, received) = recording(false, Some(gate.clone()));
        let publisher = PapirusPalette::new(None, PapirusOptions { debounce: Duration::ZERO, apply: Some(apply),
                                                                   idle: Some(Box::new(FakeIdle::default())) });
        publisher.publish(true, Some(&json!({"accent": [255, 0, 0]})));
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap().1, Some([255, 0, 0]));
        publisher.publish(true, Some(&json!({"accent": [0, 255, 0]})));
        publisher.publish(true, Some(&json!({"accent": [0, 0, 255]})));
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap().1, Some([0, 0, 255]));
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
        publisher.close();
        publisher.join(Duration::from_secs(2));
    }

    #[test]
    fn worker_completion_queues_one_main_loop_refresh() {
        let fixture = Fixture::new("refresh");
        let idle = FakeIdle::default();
        let delivered = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = delivered.clone();
        let publisher = PapirusPalette::new(Some(Box::new(move || counter.set(counter.get() + 1))), PapirusOptions {
            debounce: Duration::ZERO, apply: Some(Box::new(fixture.mirrors(false))), idle: Some(Box::new(idle.clone())) });
        publisher.publish(true, Some(&json!({"accent": [255, 34, 34]})));
        let deadline = Instant::now() + Duration::from_secs(2);
        while idle.count() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let source = publisher.refresh_source();
        assert_eq!(fixture.calls(), 6, "All three variants and caches finish before notification");
        assert_eq!(delivered.get(), 0, "Worker must never call GTK directly");
        idle.take(source)();
        assert_eq!(delivered.get(), 1);
        assert!(idle.cancelled().is_empty());
        assert_eq!(publisher.refresh_source(), 0);
        publisher.close();
    }

    #[test]
    fn refreshes_coalesce_and_pending_delivery_is_ignored_after_close() {
        let idle = FakeIdle::default();
        let delivered = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = delivered.clone();
        let (apply, _received) = recording(false, None);
        let publisher = PapirusPalette::new(Some(Box::new(move || counter.set(counter.get() + 1))), PapirusOptions {
            debounce: Duration::ZERO, apply: Some(apply), idle: Some(Box::new(idle.clone())) });
        publisher.queue_refresh();
        publisher.queue_refresh();
        assert_eq!(idle.count(), 1);
        publisher.close();
        assert_eq!(idle.cancelled(), [1]);
        idle.take(1)();
        publisher.queue_refresh();
        assert_eq!(idle.count(), 1);
        assert_eq!(delivered.get(), 0);
    }

    #[test]
    fn unchanged_color_does_not_queue_refresh() {
        let fixture = Fixture::new("unchanged");
        apply(&fixture.mirrors(false), true, Some([255, 34, 34]));
        let idle = FakeIdle::default();
        let publisher = PapirusPalette::new(Some(Box::new(|| panic!("no refresh"))), PapirusOptions {
            debounce: Duration::ZERO, apply: Some(Box::new(fixture.mirrors(false))), idle: Some(Box::new(idle.clone())) });
        publisher.publish(true, Some(&json!({"accent": [255, 34, 34]})));
        std::thread::sleep(Duration::from_millis(200));
        publisher.close();
        publisher.join(Duration::from_secs(2));
        assert_eq!(idle.count(), 0);
        assert_eq!(fixture.calls(), 6);
    }

    #[test]
    fn close_during_worker_prevents_late_refresh() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (apply, received) = recording(true, Some(gate.clone()));
        let idle = FakeIdle::default();
        let publisher = PapirusPalette::new(Some(Box::new(|| panic!("late refresh"))), PapirusOptions {
            debounce: Duration::ZERO, apply: Some(apply), idle: Some(Box::new(idle.clone())) });
        publisher.publish(true, Some(&json!({"accent": [255, 34, 34]})));
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        publisher.close();
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(publisher.join(Duration::from_secs(2)));
        assert_eq!(idle.count(), 0);
    }

    #[test]
    fn partial_success_still_refreshes_successful_variants() {
        let fixture = Fixture::new("partial");
        let idle = FakeIdle::default();
        let delivered = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = delivered.clone();
        let publisher = PapirusPalette::new(Some(Box::new(move || counter.set(counter.get() + 1))), PapirusOptions {
            debounce: Duration::ZERO, apply: Some(Box::new(fixture.mirrors(true))), idle: Some(Box::new(idle.clone())) });
        publisher.publish(true, Some(&json!({"accent": [255, 34, 34]})));
        let deadline = Instant::now() + Duration::from_secs(2);
        while (idle.count() == 0 || publisher.error().is_none()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(publisher.error().is_some());
        idle.take(publisher.refresh_source())();
        assert_eq!(delivered.get(), 1);
        publisher.close();
    }

    #[test]
    fn glib_idle_delivers_on_main_context() {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let idle = GlibIdle::new();
            let (sender, receiver) = mpsc::channel();
            let id = idle.idle_add(Box::new(move || sender.send(std::thread::current().id()).unwrap()));
            let other = idle.idle_add(Box::new(|| panic!("removed")));
            idle.source_remove(other);
            assert!(id != other);
            let main = std::thread::current().id();
            assert!(suite_core::timer::wait_for(&context, Duration::from_secs(1), || receiver.try_recv().is_ok_and(|t| t == main)));
            suite_core::timer::settle(&context, Duration::from_millis(50));
        }).unwrap();
    }
}
