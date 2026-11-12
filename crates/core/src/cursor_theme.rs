//! Pointer (Xcursor) themes for Global Settings: discovery of installed
//! themes, cursor lookup through aliases and `Inherits=`, a strict Xcursor
//! image reader, and the files a choice is written to (GTK `settings.ini`,
//! `icons/default/index.theme`, the recorded choice).
//!
//! A usable cursor theme is a directory in an icon search path
//! ([`crate::icon_theme::search_dirs`]) whose `cursors/` has a default arrow.
//! Everything here takes explicit paths so tests use fake directories.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::icon_theme::{ini_get, ini_set};

/// The theme selected when nothing installed is configured (GTK's and
/// wlroots' fallback through `default`).
pub const DEFAULT: &str = "Adwaita";
/// `org.gnome.desktop.interface` keys.
pub const GSETTINGS_KEY: &str = "cursor-theme";
pub const GSETTINGS_SIZE_KEY: &str = "cursor-size";
/// GTK `settings.ini` key in `[Settings]`.
pub const INI_KEY: &str = "gtk-cursor-theme-name";
pub const INI_SECTION: &str = "Settings";
/// labwc `environment` variable.
pub const ENV_KEY: &str = "XCURSOR_THEME";
/// The theme XWayland and toolkits fall back to; its index.theme names the choice.
pub const DEFAULT_THEME_DIR: &str = "default";
/// The default arrow; a theme without one is not offered.
pub const ARROW: [&str; 2] = ["left_ptr", "default"];
/// Preview cursors with their usual aliases: arrow, pointing hand, text
/// beam, move, horizontal resize, busy.
pub const SAMPLES: [&[&str]; 6] = [&["left_ptr", "default", "arrow"], &["hand2", "pointer", "hand1"], &["xterm", "text", "ibeam"],
                                   &["fleur", "move", "all-scroll", "grabbing"], &["sb_h_double_arrow", "ew-resize", "h_double_arrow", "col-resize"],
                                   &["watch", "wait", "progress", "left_ptr_watch"]];
/// Inherits= levels followed (libXcursor follows a similar small number).
pub const MAX_INHERIT_DEPTH: usize = 8;
/// Themes visited per lookup, whatever the Inherits= graph.
pub const MAX_THEMES: usize = 32;
/// Largest Xcursor file read.
pub const MAX_FILE_BYTES: u64 = 32 << 20;
/// Largest image side accepted.
pub const MAX_IMAGE_SIZE: u32 = 512;
/// Largest table of contents accepted (libXcursor's limit).
pub const MAX_TOC: u32 = 0x10000;
/// Largest index.theme read.
const MAX_INDEX_BYTES: u64 = 64 << 10;
const MAGIC: &[u8; 4] = b"Xcur";
const FILE_HEADER: u32 = 16;
const IMAGE_TYPE: u32 = 0xfffd_0002;
const IMAGE_HEADER: u32 = 36;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorThemeInfo {
    /// Directory name (the value GTK, gsettings and XCURSOR_THEME store).
    pub id: String,
    /// `Name=` from index.theme (the id when missing).
    pub name: String,
    pub dir: PathBuf,
}

/// One Xcursor image: premultiplied ARGB32 little endian, i.e. B, G, R, A
/// bytes per pixel, rows of `width * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorImage {
    pub nominal: u32,
    pub width: u32,
    pub height: u32,
    pub xhot: u32,
    pub yhot: u32,
    pub delay: u32,
    pub pixels: Vec<u8>,
}

/// A directory name usable as a theme id in paths, ini files and the environment.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && !id.contains('/') && !id.chars().any(char::is_control) && id.trim() == id
}

/// Read at most `limit` bytes of a regular file (FIFOs and devices are refused
/// without blocking); `None` when it is missing, unreadable or larger.
fn read_limited(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY).open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data).ok()?;
    (data.len() as u64 <= limit).then_some(data)
}

fn index_text(dir: &Path) -> Option<String> {
    read_limited(&dir.join("index.theme"), MAX_INDEX_BYTES).map(|data| String::from_utf8_lossy(&data).into_owned())
}

fn has_arrow(dir: &Path) -> bool {
    ARROW.iter().any(|name| dir.join("cursors").join(name).is_file())
}

/// The cursor theme in `dir` (named after the directory) when it has a default arrow.
pub fn theme_at(dir: &Path) -> Option<CursorThemeInfo> {
    let id = dir.file_name()?.to_str()?.to_owned();
    if !valid_id(&id) || id == DEFAULT_THEME_DIR || !has_arrow(dir) {
        return None;
    }
    let name = index_text(dir).and_then(|text| ini_get(&text, "Icon Theme", "Name")).filter(|n| !n.is_empty());
    Some(CursorThemeInfo { name: name.unwrap_or_else(|| id.clone()), id, dir: dir.to_path_buf() })
}

/// Whether `id` is an installed cursor theme in any of `dirs`.
pub fn is_installed(dirs: &[PathBuf], id: &str) -> bool {
    valid_id(id) && dirs.iter().any(|d| theme_at(&d.join(id)).is_some())
}

/// The themes a user can choose: one per id (earlier search dirs win), only
/// themes with their own cursors, never `default`; sorted by name.
pub fn discover(dirs: &[PathBuf]) -> Vec<CursorThemeInfo> {
    let mut themes: Vec<CursorThemeInfo> = Vec::new();
    for root in dirs {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            let Some(theme) = theme_at(&path) else { continue };
            if !themes.iter().any(|t| t.id == theme.id) {
                themes.push(theme);
            }
        }
    }
    themes.sort_by(|a, b| crate::pyjson::casefold(&a.name).cmp(&crate::pyjson::casefold(&b.name)).then(a.id.cmp(&b.id)));
    themes
}

/// `Inherits=` of the first index.theme of `theme` in `dirs` (libXcursor's order).
pub fn inherits(dirs: &[PathBuf], theme: &str) -> Vec<String> {
    let Some(text) = dirs.iter().find_map(|d| index_text(&d.join(theme))) else { return Vec::new() };
    ini_get(&text, "Icon Theme", "Inherits").map(|value| {
        value.split([',', ';']).map(str::trim).filter(|t| valid_id(t)).map(str::to_owned).collect()
    }).unwrap_or_default()
}

/// The file of the first of `names` in `theme`, else in what it inherits
/// (depth first, as libXcursor does). Symlinks are followed.
pub fn find_cursor(dirs: &[PathBuf], theme: &str, names: &[&str]) -> Option<PathBuf> {
    fn walk(dirs: &[PathBuf], theme: &str, names: &[&str], depth: usize, seen: &mut Vec<String>) -> Option<PathBuf> {
        if depth > MAX_INHERIT_DEPTH || seen.len() >= MAX_THEMES || !valid_id(theme) || seen.iter().any(|t| t == theme) {
            return None;
        }
        seen.push(theme.to_owned());
        for name in names {
            for dir in dirs {
                let path = dir.join(theme).join("cursors").join(name);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        inherits(dirs, theme).iter().find_map(|parent| walk(dirs, parent, names, depth + 1, seen))
    }
    walk(dirs, theme, names, 0, &mut Vec::new())
}

fn word(data: &[u8], offset: u64) -> Option<u32> {
    let start = usize::try_from(offset).ok()?;
    let bytes = data.get(start..start.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn parse_image(data: &[u8], position: u32, nominal: u32) -> Option<CursorImage> {
    let base = u64::from(position);
    let field = |index: u64| word(data, base + 4 * index);
    if field(0)? < IMAGE_HEADER || field(1)? != IMAGE_TYPE || field(2)? != nominal {
        return None;
    }
    let (width, height, xhot, yhot, delay) = (field(4)?, field(5)?, field(6)?, field(7)?, field(8)?);
    if width == 0 || height == 0 || width > MAX_IMAGE_SIZE || height > MAX_IMAGE_SIZE || xhot > width || yhot > height {
        return None;
    }
    let start = usize::try_from(base + u64::from(IMAGE_HEADER)).ok()?;
    let length = (width as usize) * (height as usize) * 4;
    let mut pixels = data.get(start..start.checked_add(length)?)?.to_vec();
    // Premultiplied: no channel above alpha, whatever the file says.
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3];
        for channel in &mut pixel[..3] {
            *channel = (*channel).min(alpha);
        }
    }
    Some(CursorImage { nominal, width, height, xhot, yhot, delay, pixels })
}

/// The first image of the nominal size closest to `size` in Xcursor `data`
/// (the first such size in the table wins a tie, as in libXcursor).
/// `None` for anything malformed, truncated or oversized.
pub fn parse_xcursor(data: &[u8], size: u32) -> Option<CursorImage> {
    if data.get(..4)? != MAGIC {
        return None;
    }
    let header = word(data, 4)?;
    let count = word(data, 12)?;
    if header < FILE_HEADER || count == 0 || count > MAX_TOC {
        return None;
    }
    let toc_end = u64::from(header) + u64::from(count) * 12;
    if toc_end > data.len() as u64 {
        return None;
    }
    let entries: Vec<(u32, u32)> = (0..u64::from(count)).filter_map(|i| {
        let at = u64::from(header) + i * 12;
        (word(data, at)? == IMAGE_TYPE).then_some((word(data, at + 4)?, word(data, at + 8)?))
    }).collect();
    let distance = |nominal: u32| nominal.abs_diff(size);
    let mut best: Option<u32> = None;
    for (nominal, _) in &entries {
        if best.is_none_or(|b| distance(*nominal) < distance(b)) {
            best = Some(*nominal);
        }
    }
    let best = best?;
    entries.iter().filter(|(nominal, _)| *nominal == best).find_map(|(nominal, position)| parse_image(data, *position, *nominal))
}

/// [`parse_xcursor`] of a file (regular files up to [`MAX_FILE_BYTES`]).
pub fn load_cursor(path: &Path, size: u32) -> Option<CursorImage> {
    parse_xcursor(&read_limited(path, MAX_FILE_BYTES)?, size)
}

/// The preview images of `theme`: one per [`SAMPLES`] entry it (or what it
/// inherits) has, with the name of the sample; missing cursors are skipped.
pub fn sample_images(dirs: &[PathBuf], theme: &str, size: u32) -> Vec<(&'static str, CursorImage)> {
    SAMPLES.iter().filter_map(|names| {
        let path = find_cursor(dirs, theme, names)?;
        Some((names[0], load_cursor(&path, size)?))
    }).collect()
}

// ---- files a choice is written to -------------------------------------

/// Set `key=value` in `[section]` of one ini file (created when missing),
/// replacing it atomically and keeping its mode; a symlinked file is
/// written through to its target. True when the file changed.
pub fn write_ini_value(path: &Path, section: &str, key: &str, value: &str) -> std::io::Result<bool> {
    let target = if path.is_symlink() { std::fs::canonicalize(path)? } else { path.to_path_buf() };
    let current = match std::fs::read_to_string(&target) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let text = ini_set(current.as_deref().unwrap_or(""), section, key, value);
    if current.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mode = std::fs::metadata(&target).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644);
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&temporary, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map(|()| true)
}

/// The cursor theme a GTK `settings.ini` names.
pub fn read_gtk_ini(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    ini_get(&text, INI_SECTION, INI_KEY).filter(|v| !v.is_empty())
}

/// Write `gtk-cursor-theme-name` to both GTK `settings.ini` files under `config_home`.
pub fn write_gtk_inis(config_home: &Path, theme: &str) -> Result<(), String> {
    let mut errors = Vec::new();
    for path in crate::icon_theme::gtk_ini_paths(config_home) {
        if let Err(error) = write_ini_value(&path, INI_SECTION, INI_KEY, theme) {
            errors.push(crate::pyerr::os_error_path(&error, &path));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

/// The `default` theme directories: `$XDG_DATA_HOME/icons/default` (created
/// when missing) and `~/.icons/default` (only when it exists).
pub fn default_theme_dirs(data_home: &Path, home: &Path) -> [(PathBuf, bool); 2] {
    [(data_home.join("icons").join(DEFAULT_THEME_DIR), true), (home.join(".icons").join(DEFAULT_THEME_DIR), false)]
}

fn inside(path: &Path, home: &Path) -> bool {
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    std::fs::canonicalize(path).is_ok_and(|p| p.starts_with(home))
}

/// Point `dir/index.theme` at `theme` with `[Icon Theme]` `Inherits=`: an
/// existing file only gets its Inherits= line changed. Skipped without an
/// error (false) when `dir` is missing and not to be created, when it or the
/// file is a symlink out of `home` (a system directory), or when it is not
/// writable. True when the file changed.
pub fn write_default_index(dir: &Path, create: bool, home: &Path, theme: &str) -> Result<bool, String> {
    if !valid_id(theme) {
        return Err(format!("Invalid pointer theme: {theme}"));
    }
    let file = dir.join("index.theme");
    if !create && !dir.is_dir() {
        return Ok(false);
    }
    if (dir.is_symlink() && !inside(dir, home)) || (file.is_symlink() && !inside(&file, home)) {
        return Ok(false);
    }
    match write_ini_value(&file, "Icon Theme", "Inherits", theme) {
        Ok(changed) => Ok(changed),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem) => Ok(false),
        Err(error) => Err(crate::pyerr::os_error_path(&error, &file)),
    }
}

/// [`write_default_index`] for both [`default_theme_dirs`].
pub fn write_default_indexes(data_home: &Path, home: &Path, theme: &str) -> Result<(), String> {
    let errors: Vec<String> = default_theme_dirs(data_home, home).iter()
        .filter_map(|(dir, create)| write_default_index(dir, *create, home, theme).err()).collect();
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

/// Where the theme chosen in Global Settings is recorded.
pub fn choice_path(config_home: &Path) -> PathBuf {
    config_home.join("tessel/cursor-theme")
}

/// The pointer theme chosen in Global Settings, when one was.
pub fn read_choice(config_home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(choice_path(config_home)).ok()?;
    text.lines().next().map(str::trim).filter(|id| valid_id(id)).map(str::to_owned)
}

/// Record the pointer theme chosen in Global Settings.
pub fn write_choice(config_home: &Path, theme: &str) -> Result<(), String> {
    let path = choice_path(config_home);
    if read_choice(config_home).as_deref() == Some(theme) {
        return Ok(());
    }
    let write = || {
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".cursor-theme.{}.tmp", std::process::id()));
        std::fs::write(&temporary, format!("{theme}\n"))?;
        std::fs::rename(&temporary, &path).inspect_err(|_| { let _ = std::fs::remove_file(&temporary); })
    };
    write().map_err(|error: std::io::Error| crate::pyerr::os_error_path(&error, &path))
}

/// The shared `org.gnome.desktop.interface` settings when the schema has `cursor-theme`.
pub fn interface_settings() -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.interface", true)?;
    schema.has_key(GSETTINGS_KEY).then(|| gio::Settings::new("org.gnome.desktop.interface"))
}

/// An Xcursor file of `images` (`(nominal, width, height, fill BGRA)`), for tests.
pub fn encode_xcursor(images: &[(u32, u32, u32, [u8; 4])]) -> Vec<u8> {
    let mut data = Vec::new();
    let put = |data: &mut Vec<u8>, value: u32| data.extend_from_slice(&value.to_le_bytes());
    data.extend_from_slice(MAGIC);
    put(&mut data, FILE_HEADER);
    put(&mut data, 0x1_0000);
    put(&mut data, images.len() as u32);
    let mut position = FILE_HEADER + 12 * images.len() as u32;
    for (nominal, width, height, _) in images {
        put(&mut data, IMAGE_TYPE);
        put(&mut data, *nominal);
        put(&mut data, position);
        position += IMAGE_HEADER + width * height * 4;
    }
    for (nominal, width, height, fill) in images {
        for value in [IMAGE_HEADER, IMAGE_TYPE, *nominal, 1, *width, *height, width / 2, height / 2, 50] {
            put(&mut data, value);
        }
        for _ in 0..width * height {
            data.extend_from_slice(fill);
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("suite-cursor-theme-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cursors(root: &Path, id: &str, index: Option<&str>, names: &[&str]) {
        let dir = root.join(id);
        std::fs::create_dir_all(dir.join("cursors")).unwrap();
        if let Some(index) = index {
            std::fs::write(dir.join("index.theme"), index).unwrap();
        }
        for name in names {
            std::fs::write(dir.join("cursors").join(name), encode_xcursor(&[(24, 24, 24, [1, 2, 3, 255])])).unwrap();
        }
    }

    #[test]
    fn discovery_lists_themes_with_an_arrow_once_by_name() {
        let dir = scratch("discover");
        let (user, legacy, system) = (dir.join("data/icons"), dir.join("home/.icons"), dir.join("usr/icons"));
        cursors(&system, "Adwaita", Some("[Icon Theme]\nName=Adwaita\nInherits=hicolor\nDirectories=a\n"), &["default"]);
        cursors(&system, "breeze_cursors", Some("[Icon Theme]\nName=Breeze\n"), &["left_ptr", "xterm"]);
        cursors(&system, "bibata", Some("[Icon Theme]\nName=bibata Modern\n"), &["left_ptr"]);
        cursors(&system, "Nameless", None, &["left_ptr"]);
        cursors(&system, "EmptyName", Some("[Icon Theme]\nName=\n"), &["left_ptr"]);
        cursors(&system, "noarrow", Some("[Icon Theme]\nName=No Arrow\n"), &["xterm"]);
        cursors(&system, ".hidden", None, &["left_ptr"]);
        cursors(&system, "default", Some("[Icon Theme]\nInherits=Adwaita\n"), &["left_ptr"]);
        // Inherit-only themes (no cursors/) and icon-only themes are not cursor themes.
        std::fs::create_dir_all(system.join("inherit-only")).unwrap();
        std::fs::write(system.join("inherit-only/index.theme"), "[Icon Theme]\nInherits=Adwaita\n").unwrap();
        std::fs::create_dir_all(system.join("Papirus/places")).unwrap();
        std::fs::write(system.join("Papirus/index.theme"), "[Icon Theme]\nName=Papirus\nDirectories=places\n").unwrap();
        std::fs::write(system.join("a-file"), "x").unwrap();
        // An arrow that is a symlink counts; a dangling one does not.
        cursors(&legacy, "Linked", Some("[Icon Theme]\nName=Linked\n"), &["default"]);
        std::os::unix::fs::symlink("default", legacy.join("Linked/cursors/left_ptr")).unwrap();
        cursors(&legacy, "Dangling", None, &[]);
        std::os::unix::fs::symlink("nowhere", legacy.join("Dangling/cursors/left_ptr")).unwrap();
        // The first search dir wins an id.
        cursors(&user, "Adwaita", Some("[Icon Theme]\nName=My Adwaita\n"), &["left_ptr"]);
        // A user dir with the id but no cursors does not hide the system theme.
        std::fs::create_dir_all(user.join("breeze_cursors")).unwrap();
        let dirs = crate::icon_theme::search_dirs_for(&dir.join("data"), &dir.join("home"), &[dir.join("usr")]);
        assert_eq!(dirs, [user.clone(), legacy.clone(), system.clone()]);
        let found = discover(&dirs);
        let summary: Vec<(&str, &str)> = found.iter().map(|t| (t.id.as_str(), t.name.as_str())).collect();
        assert_eq!(summary, [("bibata", "bibata Modern"), ("breeze_cursors", "Breeze"), ("EmptyName", "EmptyName"),
                             ("Linked", "Linked"), ("Adwaita", "My Adwaita"), ("Nameless", "Nameless")]);
        assert_eq!(found[1].dir, system.join("breeze_cursors"));
        assert_eq!(found[4].dir, user.join("Adwaita"));
        assert!(is_installed(&dirs, "Adwaita"));
        assert!(!is_installed(&dirs, "default"));
        assert!(!is_installed(&dirs, "noarrow"));
        assert!(!is_installed(&dirs, "../usr/icons/Adwaita"));
        assert!(!is_installed(&dirs, ""));
        assert!(!valid_id("a\nb") && !valid_id(" a") && !valid_id(".x") && valid_id("Bibata-Modern Ice"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cursors_resolve_through_aliases_symlinks_and_inherits() {
        let dir = scratch("resolve");
        let (user, system) = (dir.join("user"), dir.join("system"));
        cursors(&system, "base", Some("[Icon Theme]\nName=Base\n"), &["left_ptr", "pointer", "xterm", "watch", "fleur"]);
        cursors(&system, "middle", Some("[Icon Theme]\nInherits = missing ; base\n"), &["default", "wait"]);
        cursors(&user, "top", Some("[Icon Theme]\nInherits=middle,base\n"), &["hand2"]);
        std::os::unix::fs::symlink("hand2", user.join("top/cursors/text")).unwrap();
        let dirs = [user.clone(), system.clone()];
        assert_eq!(inherits(&dirs, "top"), ["middle", "base"]);
        assert_eq!(inherits(&dirs, "middle"), ["missing", "base"]);
        assert_eq!(find_cursor(&dirs, "top", SAMPLES[0]), Some(system.join("middle/cursors/default")));
        assert_eq!(find_cursor(&dirs, "top", SAMPLES[1]), Some(user.join("top/cursors/hand2")));
        // The theme's own alias wins over the parent's first name.
        assert_eq!(find_cursor(&dirs, "top", SAMPLES[2]), Some(user.join("top/cursors/text")));
        assert_eq!(find_cursor(&dirs, "top", SAMPLES[5]), Some(system.join("middle/cursors/wait")));
        assert_eq!(find_cursor(&dirs, "top", SAMPLES[3]), Some(system.join("base/cursors/fleur")));
        assert_eq!(find_cursor(&dirs, "top", SAMPLES[4]), None);
        let samples: Vec<&str> = sample_images(&dirs, "top", 32).iter().map(|(name, _)| *name).collect();
        assert_eq!(samples, ["left_ptr", "hand2", "xterm", "fleur", "watch"]);
        // Loops and deep chains end.
        cursors(&system, "loop-a", Some("[Icon Theme]\nInherits=loop-b\n"), &[]);
        cursors(&system, "loop-b", Some("[Icon Theme]\nInherits=loop-a,../base\n"), &[]);
        assert_eq!(find_cursor(&dirs, "loop-a", SAMPLES[0]), None);
        for level in 0..20 {
            cursors(&system, &format!("deep{level}"), Some(&format!("[Icon Theme]\nInherits=deep{}\n", level + 1)), &[]);
        }
        cursors(&system, "deep20", None, &["left_ptr"]);
        assert_eq!(find_cursor(&dirs, "deep0", SAMPLES[0]), None);
        assert_eq!(find_cursor(&dirs, "deep15", SAMPLES[0]), Some(system.join("deep20/cursors/left_ptr")));
        // A FIFO named like a cursor is not read (and does not block).
        cursors(&system, "fifo", None, &[]);
        let fifo = std::ffi::CString::new(system.join("fifo/cursors/left_ptr").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(load_cursor(&system.join("fifo/cursors/left_ptr"), 32), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xcursor_picks_the_nominal_size_closest_to_the_request() {
        let data = encode_xcursor(&[(24, 24, 24, [10, 20, 30, 255]), (32, 32, 30, [1, 1, 1, 128]), (48, 48, 48, [5, 5, 5, 5]),
                                    (32, 32, 32, [9, 9, 9, 9])]);
        let image = parse_xcursor(&data, 32).unwrap();
        assert_eq!((image.nominal, image.width, image.height, image.xhot, image.yhot, image.delay), (32, 32, 30, 16, 15, 50));
        assert_eq!(image.pixels.len(), 32 * 30 * 4);
        assert_eq!(&image.pixels[..4], &[1, 1, 1, 128]);
        assert_eq!(parse_xcursor(&data, 64).unwrap().nominal, 48);
        assert_eq!(parse_xcursor(&data, 1).unwrap().nominal, 24);
        // 28 is as close to 24 as to 32: the first in the table wins.
        assert_eq!(parse_xcursor(&data, 28).unwrap().nominal, 24);
        // Channels above alpha are clamped (premultiplied data).
        let image = parse_xcursor(&encode_xcursor(&[(24, 2, 2, [200, 100, 50, 60])]), 24).unwrap();
        assert_eq!(&image.pixels[..4], &[60, 60, 50, 60]);
        // Non-image chunks (comments) are ignored.
        let mut with_comment = encode_xcursor(&[(24, 2, 2, [0, 0, 0, 255])]);
        with_comment[12] = 2;
        with_comment.splice(28..28, [0x01u8, 0x00, 0xfe, 0xff, 1, 0, 0, 0, 0, 0, 0, 0]);
        let at = u32::from_le_bytes(with_comment[24..28].try_into().unwrap()) + 12;
        with_comment[24..28].copy_from_slice(&at.to_le_bytes());
        assert_eq!(parse_xcursor(&with_comment, 24).unwrap().width, 2);
    }

    #[test]
    fn malformed_truncated_oversized_and_hostile_files_are_refused() {
        let good = encode_xcursor(&[(32, 32, 32, [1, 2, 3, 255])]);
        assert!(parse_xcursor(&good, 32).is_some());
        // Every truncation fails cleanly.
        for length in 0..good.len() {
            assert_eq!(parse_xcursor(&good[..length], 32), None, "truncated to {length}");
        }
        let patched = |offset: usize, value: u32| {
            let mut data = good.clone();
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            data
        };
        let image = 16 + 12; // first chunk
        for (offset, value) in [
            (0, 0x72756359),               // bad magic
            (4, 4),                        // header shorter than 16
            (4, u32::MAX),                 // header past the end
            (12, 0),                       // no entries
            (12, MAX_TOC + 1),             // too many entries
            (12, u32::MAX),                // entry count overflow
            (24, u32::MAX),                // chunk position past the end
            (24, u32::MAX - 3),            // position overflow
            (image, 8),                    // chunk header too short
            (image + 4, 0xfffe_0001),      // chunk is not an image
            (image + 8, 24),               // chunk size differs from the table
            (image + 16, 0),               // zero width
            (image + 16, MAX_IMAGE_SIZE + 1),
            (image + 20, u32::MAX),        // huge height
            (image + 24, 33),              // hotspot outside
            (image + 28, 40),
        ] {
            assert_eq!(parse_xcursor(&patched(offset, value), 32), None, "offset {offset} = {value:#x}");
        }
        // A broken first frame of the best size falls back to the next one of that size.
        let two = encode_xcursor(&[(32, 4, 4, [1, 1, 1, 1]), (32, 2, 2, [2, 2, 2, 2])]);
        let mut broken = two.clone();
        broken[16 + 24 + 16..16 + 24 + 20].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_xcursor(&broken, 32).unwrap().width, 2);
        // Sizes too large are refused before reading; random bytes never panic.
        let dir = scratch("hostile");
        let path = dir.join("big");
        std::fs::File::create(&path).unwrap().set_len(MAX_FILE_BYTES + 1).unwrap();
        assert_eq!(load_cursor(&path, 32), None);
        std::fs::write(&path, &good).unwrap();
        assert!(load_cursor(&path, 32).is_some());
        assert_eq!(load_cursor(&dir.join("missing"), 32), None);
        assert_eq!(load_cursor(&dir, 32), None);
        let mut seed = 0x2545_f491_u32;
        for _ in 0..2000 {
            let mut data = good.clone();
            for _ in 0..4 {
                seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
                let at = seed as usize % data.len();
                data[at] = (seed >> 8) as u8;
            }
            let _ = parse_xcursor(&data, 32);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gtk_ini_keeps_every_other_line() {
        let dir = scratch("ini");
        let [gtk3, gtk4] = crate::icon_theme::gtk_ini_paths(&dir);
        std::fs::create_dir_all(gtk3.parent().unwrap()).unwrap();
        let text = "# mine\n[Settings]\ngtk-icon-theme-name=Papirus\n; note\ngtk-cursor-theme-name = breeze_cursors\ngtk-cursor-theme-size=24\n";
        std::fs::write(&gtk3, text).unwrap();
        write_gtk_inis(&dir, "Adwaita").unwrap();
        assert_eq!(std::fs::read_to_string(&gtk3).unwrap(), text.replace("gtk-cursor-theme-name = breeze_cursors", "gtk-cursor-theme-name=Adwaita"));
        assert_eq!(std::fs::read_to_string(&gtk4).unwrap(), "[Settings]\ngtk-cursor-theme-name=Adwaita\n");
        assert_eq!(read_gtk_ini(&gtk4).as_deref(), Some("Adwaita"));
        assert!(!write_ini_value(&gtk4, INI_SECTION, INI_KEY, "Adwaita").unwrap(), "unchanged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_theme_index_follows_the_choice_only_where_it_may() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("default");
        let (data, home) = (dir.join("home/.local/share"), dir.join("home"));
        // Created under XDG_DATA_HOME; ~/.icons is never created.
        write_default_indexes(&data, &home, "Bibata").unwrap();
        assert_eq!(std::fs::read_to_string(data.join("icons/default/index.theme")).unwrap(), "[Icon Theme]\nInherits=Bibata\n");
        assert!(!home.join(".icons").exists());
        // An existing file only gets its Inherits= line changed; ~/.icons/default is followed when it exists.
        let user = data.join("icons/default/index.theme");
        std::fs::write(&user, "# keep\n[Icon Theme]\nName=Default\nComment=x\nInherits=Adwaita\n\n[Other]\nInherits=no\n").unwrap();
        std::fs::create_dir_all(home.join(".icons/default")).unwrap();
        write_default_indexes(&data, &home, "breeze_cursors").unwrap();
        assert_eq!(std::fs::read_to_string(&user).unwrap(),
                   "# keep\n[Icon Theme]\nName=Default\nComment=x\nInherits=breeze_cursors\n\n[Other]\nInherits=no\n");
        assert_eq!(std::fs::read_to_string(home.join(".icons/default/index.theme")).unwrap(), "[Icon Theme]\nInherits=breeze_cursors\n");
        let file = std::fs::read_to_string(home.join(".icons/default/index.theme")).unwrap();
        std::fs::write(home.join(".icons/default/index.theme"), format!("{file}Name=Mine\n")).unwrap();
        assert!(!write_default_index(&home.join(".icons/default"), false, &home, "breeze_cursors").unwrap());
        // A symlink into a system directory is skipped without an error; one inside home is written through.
        let system = dir.join("usr/share/icons/default");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(system.join("index.theme"), "[Icon Theme]\nInherits=Adwaita\n").unwrap();
        std::fs::remove_dir_all(data.join("icons/default")).unwrap();
        std::os::unix::fs::symlink(&system, data.join("icons/default")).unwrap();
        std::fs::remove_file(home.join(".icons/default/index.theme")).unwrap();
        std::os::unix::fs::symlink(system.join("index.theme"), home.join(".icons/default/index.theme")).unwrap();
        write_default_indexes(&data, &home, "Bibata").unwrap();
        assert_eq!(std::fs::read_to_string(system.join("index.theme")).unwrap(), "[Icon Theme]\nInherits=Adwaita\n");
        let dotfiles = home.join("dotfiles/index.theme");
        std::fs::create_dir_all(dotfiles.parent().unwrap()).unwrap();
        std::fs::write(&dotfiles, "[Icon Theme]\nInherits=x\n").unwrap();
        std::fs::remove_file(home.join(".icons/default/index.theme")).unwrap();
        std::os::unix::fs::symlink(&dotfiles, home.join(".icons/default/index.theme")).unwrap();
        write_default_indexes(&data, &home, "Bibata").unwrap();
        assert_eq!(std::fs::read_to_string(&dotfiles).unwrap(), "[Icon Theme]\nInherits=Bibata\n");
        assert!(home.join(".icons/default/index.theme").is_symlink());
        // A directory that is not writable is skipped without an error.
        std::fs::remove_file(home.join(".icons/default/index.theme")).unwrap();
        std::fs::set_permissions(home.join(".icons/default"), std::fs::Permissions::from_mode(0o500)).unwrap();
        let writable = std::fs::write(home.join(".icons/default/probe"), "").is_ok();
        if !writable {
            assert_eq!(write_default_index(&home.join(".icons/default"), false, &home, "Bibata"), Ok(false));
        }
        std::fs::set_permissions(home.join(".icons/default"), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(write_default_index(&home.join(".icons/default"), false, &home, "bad\nid").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_theme_chosen_in_global_settings_is_recorded() {
        let config = scratch("choice");
        assert_eq!(read_choice(&config), None);
        write_choice(&config, "Bibata").unwrap();
        assert_eq!(std::fs::read_to_string(config.join("tessel/cursor-theme")).unwrap(), "Bibata\n");
        assert_eq!(read_choice(&config).as_deref(), Some("Bibata"));
        std::fs::write(choice_path(&config), "../etc\n").unwrap();
        assert_eq!(read_choice(&config), None);
        let _ = std::fs::remove_dir_all(&config);
    }
}
