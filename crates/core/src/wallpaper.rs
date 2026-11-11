//! The wallpaper of a screen whose user has not chosen one: Slate, the
//! picture in `branding/wallpaper`, in its dark version.
//!
//! It is the same in every desktop layout. A screen has "not chosen" while the
//! shell's `wallpapers.json` has no entry for it; an entry that names no
//! picture (a plain color) is a choice and is left alone.

use std::path::{Path, PathBuf};

/// How the default picture is placed, and the color behind it.
pub const DEFAULT_MODE: &str = "Fill";
pub const DEFAULT_COLOR: &str = "black";

/// The renders, each with the shape it was made for (width / height).
const PICTURES: [(&str, f64); 3] = [
    ("slate-3840x2160.png", 16.0 / 9.0),
    ("slate-3840x2400.png", 16.0 / 10.0),
    ("slate-7680x2160.png", 32.0 / 9.0),
];

/// Where the package `tessel-wallpapers` installs the pictures.
pub const SYSTEM_DIR: &str = "/usr/share/wallpapers/tessel";

/// The login screen's default picture: the 16:9 render in [`SYSTEM_DIR`].
/// The greeter runs as its own system user and cannot read a home folder, so
/// this is the system path even in a build that is not packaged.
/// [`render_for_screen`] swaps it for the render nearest each screen's shape.
pub const LOGIN_PICTURE: &str = "/usr/share/wallpapers/tessel/slate-3840x2160.png";

/// Where the pictures are installed: by the package, or by `tools/install.py`
/// for one user.
pub fn default_dir() -> PathBuf {
    if crate::paths::PACKAGED {
        PathBuf::from(SYSTEM_DIR)
    } else {
        crate::paths::suite_data_dir().join("wallpapers")
    }
}

/// The default picture for a screen of this size (`None`: 16:9), or `None`
/// when it is not installed.
pub fn default_wallpaper(size: Option<(i32, i32)>) -> Option<PathBuf> {
    default_wallpaper_in(&default_dir(), size)
}

/// [`default_wallpaper`] in a given folder: the installed render whose shape
/// is nearest the screen's.
pub fn default_wallpaper_in(dir: &Path, size: Option<(i32, i32)>) -> Option<PathBuf> {
    let shape = match size {
        Some((width, height)) if width > 0 && height > 0 => f64::from(width) / f64::from(height),
        _ => PICTURES[0].1,
    };
    let distance = |candidate: f64| (candidate / shape).ln().abs();
    PICTURES.iter()
        .map(|(name, made_for)| (dir.join(name), *made_for))
        .filter(|(path, _)| path.is_file())
        .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
        .map(|(path, _)| path)
}

/// When `path` is one of the Slate renders, the installed render beside it
/// whose shape is nearest a screen of this size; any other path, or a Slate
/// render with no installed sibling, is returned unchanged.
pub fn render_for_screen(path: &Path, size: Option<(i32, i32)>) -> PathBuf {
    let is_render = path.file_name().and_then(|name| name.to_str())
        .is_some_and(|name| PICTURES.iter().any(|(render, _)| *render == name));
    match path.parent() {
        Some(dir) if is_render && !dir.as_os_str().is_empty() => default_wallpaper_in(dir, size).unwrap_or_else(|| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// How many folder levels below the opened one the picker looks into.
pub const LIST_DEPTH: usize = 6;
/// At most this many folders are read per listing, so opening a large tree
/// (a home folder, say) stays quick.
const LIST_FOLDERS: usize = 1024;

/// One picture the wallpaper picker offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The file that is applied when this entry is chosen.
    pub path: PathBuf,
    /// Its path relative to the listed folder, or a wallpaper package's name.
    pub label: String,
    /// The dark picture (`contents/images_dark`) of a wallpaper package.
    pub dark: bool,
}

/// The pictures in `root` and its subfolders (down to [`LIST_DEPTH`]) that
/// `wanted` accepts, sorted by label. Nothing is decoded: `wanted` should look
/// at the name only.
///
/// - The same picture in several sizes is offered once, in the size whose
///   shape is nearest a screen of `size` (`None`: 16:9; the largest wins a
///   tie). These are files named `<name>-<W>x<H>.<ext>` (or `<W>x<H>.<ext>`)
///   in one folder, such as Slate's renders in [`SYSTEM_DIR`].
/// - A KDE wallpaper package (`<Name>/contents/images/<W>x<H>.<ext>`) is one
///   entry named from its `metadata.json` or `metadata.desktop`, plus a second,
///   [`Listed::dark`] one when it has `contents/images_dark`. Its
///   `contents/screenshot.*` preview is not offered.
/// - Hidden entries are skipped; a folder reached twice (through a link) is
///   read once; unreadable subfolders are skipped. Only `root` failing to open
///   is an error.
pub fn list_pictures(root: &Path, size: Option<(i32, i32)>, wanted: &dyn Fn(&Path) -> bool) -> std::io::Result<Vec<Listed>> {
    list_pictures_in(root, size, &crate::i18n::language(), wanted)
}

/// [`list_pictures`] with the language that package names are shown in.
pub fn list_pictures_in(root: &Path, size: Option<(i32, i32)>, language: &str, wanted: &dyn Fn(&Path) -> bool) -> std::io::Result<Vec<Listed>> {
    let entries = std::fs::read_dir(root)?;
    let mut walk = Walk { size, language, wanted, seen: std::collections::HashSet::new(), folders: 0, found: Vec::new() };
    if let Ok(canonical) = root.canonicalize() {
        walk.seen.insert(canonical);
    }
    walk.folder(root, "", 0, Some(entries));
    let mut found = walk.found;
    found.sort_by_cached_key(|entry| (crate::pyjson::casefold(&entry.label), entry.label.clone(), entry.dark, entry.path.clone()));
    Ok(found)
}

struct Walk<'a> {
    size: Option<(i32, i32)>,
    language: &'a str,
    wanted: &'a dyn Fn(&Path) -> bool,
    /// Canonical folders already read (links and loops reach them again).
    seen: std::collections::HashSet<PathBuf>,
    folders: usize,
    found: Vec<Listed>,
}

impl Walk<'_> {
    fn folder(&mut self, dir: &Path, prefix: &str, depth: usize, entries: Option<std::fs::ReadDir>) {
        self.folders += 1;
        if self.package(dir, prefix) {
            return;
        }
        let entries = match entries {
            Some(entries) => entries,
            None => match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(_) => return,
            },
        };
        let mut files = Vec::new();
        let mut folders = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
            // Follows links, like the flat listing did.
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => folders.push((link, name, path)),
                Ok(meta) if meta.is_file() && (self.wanted)(&path) => files.push((name, path)),
                _ => {}
            }
        }
        self.files(prefix, files);
        if depth >= LIST_DEPTH {
            return;
        }
        // Real folders first, so a link to one of them (`Default -> F44`) is
        // the copy that is skipped.
        folders.sort();
        for (_, name, path) in folders {
            if self.folders >= LIST_FOLDERS {
                return;
            }
            let Ok(canonical) = path.canonicalize() else { continue };
            if !self.seen.insert(canonical) {
                continue;
            }
            self.folder(&path, &format!("{prefix}{name}/"), depth + 1, None);
        }
    }

    /// The files of one folder, a picture in several sizes counted once.
    fn files(&mut self, prefix: &str, files: Vec<(String, PathBuf)>) {
        let mut groups: Vec<(String, Vec<(String, PathBuf, Option<(u32, u32)>)>)> = Vec::new();
        for (name, path) in files {
            let (key, size) = match sized_name(&name) {
                Some((stem, size)) => (format!("{stem}\0{}", extension(&name)), Some(size)),
                None => (format!("\0{name}"), None),
            };
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push((name, path, size)),
                None => groups.push((key, vec![(name, path, size)])),
            }
        }
        for (_, members) in groups {
            let index = best_size(&members.iter().map(|m| m.2).collect::<Vec<_>>(), self.size);
            let (name, path, _) = members.into_iter().nth(index).expect("a group has a member");
            self.found.push(Listed { path, label: format!("{prefix}{name}"), dark: false });
        }
    }

    /// A KDE wallpaper package at `dir`: its entries, and true.
    fn package(&mut self, dir: &Path, prefix: &str) -> bool {
        let contents = dir.join("contents");
        let images = [(contents.join("images"), false), (contents.join("images_dark"), true)];
        if !images.iter().any(|(folder, _)| folder.is_dir()) {
            return false;
        }
        let name = package_name(dir, self.language)
            .unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        // A package deeper than one level keeps the folders above it in front.
        let parent = prefix.trim_end_matches('/').rsplit_once('/').map(|(parent, _)| parent).unwrap_or("");
        let label = if parent.is_empty() { name } else { format!("{parent}/{name}") };
        for (folder, dark) in images {
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            let mut sizes: Vec<(PathBuf, Option<(u32, u32)>)> = entries.flatten()
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .map(|entry| entry.path())
                .filter(|path| path.is_file() && (self.wanted)(path))
                .map(|path| {
                    let size = path.file_name().and_then(|n| sized_name(&n.to_string_lossy()).map(|(_, size)| size));
                    (path, size)
                })
                .collect();
            if sizes.is_empty() {
                continue;
            }
            sizes.sort();
            let index = best_size(&sizes.iter().map(|s| s.1).collect::<Vec<_>>(), self.size);
            let path = sizes.swap_remove(index).0;
            self.found.push(Listed { path, label: label.clone(), dark });
        }
        true
    }
}

/// `slate-3840x2160.png` → `("slate", (3840, 2160))`; `1920x1080.jxl` → `("", (1920, 1080))`.
fn sized_name(name: &str) -> Option<(&str, (u32, u32))> {
    let stem = match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    };
    let (stem, dimensions) = match stem.rfind('-') {
        Some(dash) => (&stem[..dash], &stem[dash + 1..]),
        None => ("", stem),
    };
    let (width, height) = dimensions.split_once('x')?;
    let digits = |text: &str| !text.is_empty() && text.len() <= 6 && text.bytes().all(|b| b.is_ascii_digit());
    if !digits(width) || !digits(height) {
        return None;
    }
    let size = (width.parse().ok()?, height.parse().ok()?);
    (size.0 > 0 && size.1 > 0).then_some((stem, size))
}

fn extension(name: &str) -> String {
    name.rfind('.').filter(|dot| *dot > 0).map(|dot| name[dot + 1..].to_lowercase()).unwrap_or_default()
}

/// The index of the size whose shape is nearest a screen of `screen`
/// (`None`: 16:9, as [`default_wallpaper_in`]); the larger wins a tie and an
/// unknown size loses to any known one.
fn best_size(sizes: &[Option<(u32, u32)>], screen: Option<(i32, i32)>) -> usize {
    let shape = match screen {
        Some((width, height)) if width > 0 && height > 0 => f64::from(width) / f64::from(height),
        _ => PICTURES[0].1,
    };
    let score = |size: &Option<(u32, u32)>| match size {
        Some((width, height)) => ((f64::from(*width) / f64::from(*height) / shape).ln().abs(), u64::from(*width) * u64::from(*height)),
        None => (f64::INFINITY, 0),
    };
    let mut best = 0;
    for (index, size) in sizes.iter().enumerate().skip(1) {
        let (distance, area) = score(size);
        let (best_distance, best_area) = score(&sizes[best]);
        if distance < best_distance - 1e-9 || ((distance - best_distance).abs() <= 1e-9 && area > best_area) {
            best = index;
        }
    }
    best
}

/// A KDE package's name in `language` (`Name[de]`, then `Name[pt]` for
/// `pt_BR`, then `Name`) from `metadata.json` or `metadata.desktop`.
fn package_name(dir: &Path, language: &str) -> Option<String> {
    let mut keys = Vec::new();
    if !language.is_empty() {
        keys.push(format!("Name[{language}]"));
        if let Some((base, _)) = language.split_once(['_', '-', '@']) {
            keys.push(format!("Name[{base}]"));
        }
    }
    keys.push("Name".to_owned());
    let pick = |lookup: &dyn Fn(&str) -> Option<String>| keys.iter().find_map(|key| lookup(key).filter(|name| !name.trim().is_empty()));
    if let Ok(text) = std::fs::read_to_string(dir.join("metadata.json")) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            let plugin = value.get("KPlugin").unwrap_or(&value);
            if let Some(name) = pick(&|key| plugin.get(key).and_then(|v| v.as_str()).map(|s| s.trim().to_owned())) {
                return Some(name);
            }
        }
    }
    let text = std::fs::read_to_string(dir.join("metadata.desktop")).ok()?;
    let mut section = "";
    let mut values = std::collections::HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[Desktop Entry]" {
            if let Some((key, value)) = line.split_once('=') {
                values.entry(key.trim().to_owned()).or_insert_with(|| value.trim().to_owned());
            }
        }
    }
    pick(&|key| values.get(key).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tessel-core-wallpaper-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), b"png").unwrap();
        }
        dir
    }

    #[test]
    fn a_screen_gets_the_render_made_for_its_shape() {
        let dir = folder("shape", &["slate-3840x2160.png", "slate-3840x2400.png", "slate-7680x2160.png", "slate-light-3840x2160.png"]);
        let name = |size| default_wallpaper_in(&dir, size).unwrap().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name(Some((1920, 1080))), "slate-3840x2160.png");
        assert_eq!(name(Some((2560, 1600))), "slate-3840x2400.png");
        assert_eq!(name(Some((5120, 1440))), "slate-7680x2160.png");
        assert_eq!(name(Some((3440, 1440))), "slate-3840x2160.png", "21:9 is nearer 16:9 than 32:9");
        assert_eq!(name(Some((1080, 1920))), "slate-3840x2400.png", "a screen on its side takes the least wide");
        assert_eq!(name(None), "slate-3840x2160.png");
        assert_eq!(name(Some((0, 0))), "slate-3840x2160.png");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_installed_renders_are_offered() {
        let dir = folder("partial", &["slate-3840x2160.png"]);
        assert_eq!(default_wallpaper_in(&dir, Some((5120, 1440))).unwrap(), dir.join("slate-3840x2160.png"));
        std::fs::remove_file(dir.join("slate-3840x2160.png")).unwrap();
        assert_eq!(default_wallpaper_in(&dir, Some((1920, 1080))), None);
        assert_eq!(default_wallpaper_in(&dir.join("missing"), None), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_login_picture_is_the_wide_render_in_the_system_folder() {
        assert_eq!(Path::new(LOGIN_PICTURE), Path::new(SYSTEM_DIR).join(PICTURES[0].0));
    }

    #[test]
    fn a_slate_render_is_swapped_for_its_sibling_made_for_the_screen() {
        let dir = folder("sibling", &["slate-3840x2160.png", "slate-7680x2160.png", "other.png"]);
        let wide = dir.join("slate-3840x2160.png");
        assert_eq!(render_for_screen(&wide, Some((5120, 1440))), dir.join("slate-7680x2160.png"));
        assert_eq!(render_for_screen(&dir.join("slate-7680x2160.png"), Some((1920, 1080))), wide);
        assert_eq!(render_for_screen(&wide, Some((2560, 1600))), wide, "16:10 is not installed; 16:9 is nearest");
        assert_eq!(render_for_screen(&wide, None), wide);
        assert_eq!(render_for_screen(&dir.join("other.png"), Some((5120, 1440))), dir.join("other.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_or_foreign_picture_is_left_alone() {
        let missing = Path::new("/nonexistent-tessel-test/slate-3840x2160.png");
        assert_eq!(render_for_screen(missing, Some((5120, 1440))), missing);
        let dir = folder("lonely", &["slate-7680x2160.png"]);
        // The 16:9 file is gone, but its 32:9 sibling is the only render there.
        assert_eq!(render_for_screen(&dir.join("slate-3840x2160.png"), Some((1920, 1080))), dir.join("slate-7680x2160.png"));
        assert_eq!(render_for_screen(Path::new("slate-3840x2160.png"), Some((5120, 1440))), Path::new("slate-3840x2160.png"));
        assert_eq!(render_for_screen(Path::new("/usr/share/backgrounds/x.jpg"), None), Path::new("/usr/share/backgrounds/x.jpg"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn touch(root: &Path, files: &[&str]) {
        for file in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"img").unwrap();
        }
    }

    fn images(path: &Path) -> bool {
        matches!(extension(&path.file_name().unwrap().to_string_lossy()).as_str(), "png" | "jpg" | "jxl")
    }

    fn listed(root: &Path, size: Option<(i32, i32)>) -> Vec<(String, String, bool)> {
        list_pictures_in(root, size, "de", &images).unwrap().into_iter()
            .map(|entry| (entry.label, entry.path.strip_prefix(root).unwrap().to_string_lossy().into_owned(), entry.dark))
            .collect()
    }

    fn entry(label: &str, path: &str, dark: bool) -> (String, String, bool) {
        (label.to_owned(), path.to_owned(), dark)
    }

    #[test]
    fn the_listing_looks_into_subfolders() {
        let root = folder("nested", &[]);
        touch(&root, &["b.png", "A.jpg", "notes.txt", ".hidden.png", "space/earth.jpg", "space/deep/Mars.png",
                       ".secret/x.png", "empty.png/inner.txt"]);
        assert_eq!(listed(&root, None), vec![
            entry("A.jpg", "A.jpg", false),
            entry("b.png", "b.png", false),
            entry("space/deep/Mars.png", "space/deep/Mars.png", false),
            entry("space/earth.jpg", "space/earth.jpg", false),
        ]);
        assert!(list_pictures_in(&root.join("missing"), None, "de", &images).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_listing_stops_at_its_depth() {
        let root = folder("depth", &[]);
        let deep = (1..=LIST_DEPTH).map(|n| format!("d{n}")).collect::<Vec<_>>().join("/");
        touch(&root, &[&format!("{deep}/in.png"), &format!("{deep}/d{}/out.png", LIST_DEPTH + 1)]);
        assert_eq!(listed(&root, None), vec![entry(&format!("{deep}/in.png"), &format!("{deep}/in.png"), false)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_kde_package_is_one_picture_and_one_dark_picture() {
        let root = folder("kde", &[]);
        touch(&root, &["Next/contents/images/1440x2960.png", "Next/contents/images/5120x2880.png", "Next/contents/images/7680x2160.png",
                       "Next/contents/images_dark/5120x2880.png", "Next/contents/images_dark/7680x2160.png",
                       "Next/contents/screenshot.png", "Next/contents/images/notes.txt",
                       "F44/contents/images/1920x1080.jxl", "F44/contents/images/3840x2160.jxl", "F44/contents/images/1080x1920.jxl",
                       "Plain/contents/images/800x600.jpg", "more/Old/contents/images/1024x768.png"]);
        std::fs::write(root.join("Next/metadata.json"), r#"{"KPlugin": {"Id": "Next", "Name": "Waterfall", "Name[de]": "Wasserfall"}}"#).unwrap();
        std::fs::write(root.join("F44/metadata.json"), r#"{"KPlugin": {"Id": "F44", "Name": "Fedora Forty-Four"}}"#).unwrap();
        std::fs::write(root.join("more/Old/metadata.desktop"), "[Desktop Entry]\nName=Old Times\nName[de]=Alte Zeiten\n").unwrap();
        assert_eq!(listed(&root, None), vec![
            entry("Fedora Forty-Four", "F44/contents/images/3840x2160.jxl", false),
            entry("more/Alte Zeiten", "more/Old/contents/images/1024x768.png", false),
            entry("Plain", "Plain/contents/images/800x600.jpg", false),
            entry("Wasserfall", "Next/contents/images/5120x2880.png", false),
            entry("Wasserfall", "Next/contents/images_dark/5120x2880.png", true),
        ]);
        let wide = listed(&root, Some((5120, 1440)));
        assert!(wide.contains(&entry("Wasserfall", "Next/contents/images_dark/7680x2160.png", true)));
        let names: Vec<String> = list_pictures_in(&root, None, "pt_BR", &images).unwrap().into_iter().map(|e| e.label).collect();
        assert!(names.contains(&"Waterfall".to_owned()) && names.contains(&"more/Old Times".to_owned()));
        // Opening the package itself, or its sizes folder, still offers it once.
        assert_eq!(listed(&root.join("Next"), None).len(), 2);
        assert_eq!(listed(&root.join("Next/contents/images"), None), vec![entry("5120x2880.png", "5120x2880.png", false)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn links_and_loops_are_read_once() {
        let root = folder("links", &[]);
        touch(&root, &["F44/contents/images/1920x1080.jxl", "real/sea.png"]);
        std::os::unix::fs::symlink("F44", root.join("Default")).unwrap();
        std::os::unix::fs::symlink("..", root.join("real/up")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("real/loop")).unwrap();
        std::os::unix::fs::symlink("missing", root.join("dangling")).unwrap();
        std::os::unix::fs::symlink("real/sea.png", root.join("sea-link.png")).unwrap();
        assert_eq!(listed(&root, None), vec![
            entry("F44", "F44/contents/images/1920x1080.jxl", false),
            entry("real/sea.png", "real/sea.png", false),
            entry("sea-link.png", "sea-link.png", false),
        ]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unreadable_folder_is_skipped() {
        let root = folder("unreadable", &[]);
        touch(&root, &["open/a.png", "closed/b.png"]);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("closed"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = listed(&root, None);
        std::fs::set_permissions(root.join("closed"), std::fs::Permissions::from_mode(0o755)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            // Root reads every folder.
            assert_eq!(got, vec![entry("open/a.png", "open/a.png", false)]);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn slate_is_offered_once_dark_and_once_light() {
        let root = folder("slate", &[]);
        let renders = ["slate-3840x2160.png", "slate-3840x2400.png", "slate-7680x2160.png",
                       "slate-light-3840x2160.png", "slate-light-3840x2400.png", "slate-light-7680x2160.png"];
        touch(&root.join("tessel"), &renders);
        touch(&root, &["tessellate-3840x2160.png", "pragser-3840x2160-12263.jpg"]);
        assert_eq!(listed(&root, None), vec![
            entry("pragser-3840x2160-12263.jpg", "pragser-3840x2160-12263.jpg", false),
            entry("tessel/slate-3840x2160.png", "tessel/slate-3840x2160.png", false),
            entry("tessel/slate-light-3840x2160.png", "tessel/slate-light-3840x2160.png", false),
            entry("tessellate-3840x2160.png", "tessellate-3840x2160.png", false),
        ]);
        // The dark one is the default picture, chosen the same way per screen.
        let tessel = root.join("tessel");
        for size in [None, Some((1920, 1080)), Some((2560, 1600)), Some((5120, 1440))] {
            let got = list_pictures_in(&tessel, size, "de", &images).unwrap();
            assert_eq!(got.len(), 2);
            assert_eq!(Some(got[0].path.clone()), default_wallpaper_in(&tessel, size));
        }
        assert_eq!(listed(&tessel, Some((5120, 1440)))[1], entry("slate-light-7680x2160.png", "slate-light-7680x2160.png", false));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sizes_are_read_from_names() {
        assert_eq!(sized_name("slate-light-3840x2160.png"), Some(("slate-light", (3840, 2160))));
        assert_eq!(sized_name("1920x1080.jxl"), Some(("", (1920, 1080))));
        assert_eq!(sized_name("pragser-3840x2160-12263.jpg"), None);
        assert_eq!(sized_name("0x0.png"), None);
        assert_eq!(sized_name("a-x1080.png"), None);
        assert_eq!(best_size(&[Some((1440, 2960)), Some((3840, 2160)), Some((5120, 2880)), None], None), 2);
        assert_eq!(best_size(&[None, Some((800, 600))], None), 1);
        assert_eq!(best_size(&[Some((1080, 1920)), Some((1920, 1080))], Some((1080, 1920))), 0);
    }
}
