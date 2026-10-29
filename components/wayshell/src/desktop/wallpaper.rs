//! Wallpaper helpers of DesktopApp/DesktopView: the app-wide still-image
//! texture cache (weak, keyed by realpath/ino/size/mtime_ns), the fit-mode
//! map, video detection and the per-monitor mpvpaper supervisor.

use super::launch::{which_in, Environment};
use gtk::gdk;
use gtk::prelude::*;
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub type TextureKey = (PathBuf, u64, u64, i128);

/// `DesktopApp._wallpaper_textures` (a WeakValueDictionary).
#[derive(Default)]
pub struct TextureCache {
    entries: HashMap<TextureKey, glib::WeakRef<gdk::Texture>>,
}

pub fn texture_key(path: &str) -> std::io::Result<TextureKey> {
    let info = std::fs::metadata(path)?;
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let mtime_ns = info.mtime() as i128 * 1_000_000_000 + info.mtime_nsec() as i128;
    Ok((real, info.ino(), info.size(), mtime_ns))
}

impl TextureCache {
    /// `wallpaper_texture(path)` -> (texture, key). Errors are Python's text.
    pub fn get(&mut self, path: &str) -> Result<(gdk::Texture, TextureKey), String> {
        self.entries.retain(|_, weak| weak.upgrade().is_some());
        let key = texture_key(path).map_err(|e| suite_core::pyerr::os_error_path(&e, std::path::Path::new(path)))?;
        if let Some(texture) = self.entries.get(&key).and_then(|w| w.upgrade()) {
            return Ok((texture, key));
        }
        // Decode explicitly: Picture::set_filename can produce a 200x200
        // fallback for WebP with the sandboxed loader (wayshell:1389).
        let texture = gdk::Texture::from_filename(path).map_err(|e| suite_core::pyerr::python_error_text(&e))?;
        let weak = glib::WeakRef::new();
        weak.set(Some(&texture));
        self.entries.insert(key.clone(), weak);
        Ok((texture, key))
    }

    /// Live entries (`len(app._wallpaper_textures)`).
    pub fn len(&mut self) -> usize {
        self.entries.retain(|_, weak| weak.upgrade().is_some());
        self.entries.len()
    }

    pub fn is_empty(&mut self) -> bool {
        self.len() == 0
    }
}

pub const VIDEO_EXTENSIONS: [&str; 5] = [".mp4", ".m4v", ".mov", ".webm", ".gif"];

/// `_is_video_path` (gif plays through mpvpaper too).
pub fn is_video_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    extension_lower(path).is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

/// `os.path.splitext(path)[1].lower()` (leading dots of the basename don't count).
pub fn extension_lower(path: &str) -> Option<String> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stripped = base.trim_start_matches('.');
    let dot = stripped.rfind('.')?;
    Some(stripped[dot..].to_lowercase())
}

/// Fill→COVER, Fit→CONTAIN, Stretch→FILL, Center→SCALE_DOWN, Tile→FILL,
/// Color Only→FILL; anything else COVER. Second value: centered alignment.
pub fn fit_for_mode(mode: Option<&str>) -> (gtk::ContentFit, bool) {
    let fit = match mode {
        Some("Fill") => gtk::ContentFit::Cover,
        Some("Fit") => gtk::ContentFit::Contain,
        Some("Stretch") | Some("Tile") | Some("Color Only") => gtk::ContentFit::Fill,
        Some("Center") => gtk::ContentFit::ScaleDown,
        _ => gtk::ContentFit::Cover,
    };
    (fit, matches!(mode, Some("Fit") | Some("Center")))
}

/// The `-o` option string of `start_mpvpaper`.
pub fn mpv_options(mode: &str) -> String {
    let mut args = vec!["no-audio", "osc=no", "cursor-autohide=no", "keep-open=yes", "loop-file=inf", "loop-playlist=inf"];
    if mode == "Fit" || mode == "Fill" {
        args.push("keepaspect=yes");
    }
    args.join(" ")
}

pub fn mpvpaper_argv(exe: &str, mode: &str, connector: &str, path: &str) -> Vec<String> {
    vec![exe.into(), "-l".into(), "background".into(), "-o".into(), mpv_options(mode), connector.into(), path.into()]
}

/// `DesktopApp._mpvpaper` (mon_key -> process).
#[derive(Default)]
pub struct MpvSupervisor {
    processes: HashMap<String, Child>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StartError {
    /// mpvpaper is not on the launch PATH.
    Missing,
    Spawn(String),
}

impl MpvSupervisor {
    /// `start_mpvpaper`: stops the previous player first.
    pub fn start(&mut self, mon_key: &str, connector: &str, path: &str, mode: &str, environment: &Environment) -> Result<(), StartError> {
        self.stop(mon_key);
        let exe = which_in("mpvpaper", environment.get("PATH").map(String::as_str)).ok_or(StartError::Missing)?;
        let argv = mpvpaper_argv(&exe.to_string_lossy(), mode, connector, path);
        let child = Command::new(&argv[0]).args(&argv[1..]).env_clear().envs(environment).spawn().map_err(|e| StartError::Spawn(suite_core::pyerr::os_error_text(&e)))?;
        self.processes.insert(mon_key.to_owned(), child);
        Ok(())
    }

    /// `stop_mpvpaper`: terminate, wait up to 2 s, then kill.
    pub fn stop(&mut self, mon_key: &str) {
        let Some(mut child) = self.processes.remove(mon_key) else { return };
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        // SAFETY: plain kill(2) on our own child.
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    pub fn running(&mut self, mon_key: &str) -> Option<u32> {
        let child = self.processes.get_mut(mon_key)?;
        matches!(child.try_wait(), Ok(None)).then(|| child.id())
    }

    pub fn keys(&self) -> Vec<String> {
        self.processes.keys().cloned().collect()
    }

    pub fn stop_all(&mut self) {
        for key in self.keys() {
            self.stop(&key);
        }
    }
}

/// Apply the fit/alignment to the background picture.
pub fn apply_fit(picture: &gtk::Picture, mode: Option<&str>) {
    let (fit, centered) = fit_for_mode(mode);
    picture.set_can_shrink(true);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_content_fit(fit);
    let align = if centered { gtk::Align::Center } else { gtk::Align::Fill };
    picture.set_halign(align);
    picture.set_valign(align);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpvpaper_argv_is_exact() {
        assert_eq!(mpvpaper_argv("/bin/mpvpaper", "Fill", "HDMI-A-1", "/v.mp4"), ["/bin/mpvpaper", "-l", "background", "-o", "no-audio osc=no cursor-autohide=no keep-open=yes loop-file=inf loop-playlist=inf keepaspect=yes", "HDMI-A-1", "/v.mp4"]);
        assert_eq!(mpv_options("Fit"), "no-audio osc=no cursor-autohide=no keep-open=yes loop-file=inf loop-playlist=inf keepaspect=yes");
        for mode in ["Center", "Stretch", "Tile", "Color Only"] {
            assert_eq!(mpv_options(mode), "no-audio osc=no cursor-autohide=no keep-open=yes loop-file=inf loop-playlist=inf");
        }
    }

    #[test]
    fn fit_map_and_video_detection() {
        assert_eq!(fit_for_mode(Some("Fill")), (gtk::ContentFit::Cover, false));
        assert_eq!(fit_for_mode(Some("Fit")), (gtk::ContentFit::Contain, true));
        assert_eq!(fit_for_mode(Some("Stretch")), (gtk::ContentFit::Fill, false));
        assert_eq!(fit_for_mode(Some("Center")), (gtk::ContentFit::ScaleDown, true));
        assert_eq!(fit_for_mode(Some("Tile")), (gtk::ContentFit::Fill, false));
        assert_eq!(fit_for_mode(None), (gtk::ContentFit::Cover, false));
        assert!(is_video_path("/a/B.MP4") && is_video_path("x.gif") && is_video_path("a.webm"));
        assert!(!is_video_path("") && !is_video_path("/a/.mp4") && !is_video_path("a.png"));
        assert_eq!(extension_lower("/x/archive.tar.GZ").as_deref(), Some(".gz"));
    }
}
