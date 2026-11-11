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
}
