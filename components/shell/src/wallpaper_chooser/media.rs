//! Worker-side media helpers of wallpaper_chooser.py: listing, `ffprobe`
//! video validation, preview decoding and the default start folder. Nothing
//! here touches GTK widgets; previews cross threads as [`PreviewImage`].

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gtk::gdk_pixbuf::Pixbuf;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tessel_core::tr;

pub const IMAGE_SUFFIXES: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];
/// Match the video formats routed through Tessel Shell's wallpaper player.
pub const VIDEO_SUFFIXES: &[&str] = &["mp4", "m4v", "mov", "webm"];
pub const PREVIEW_WIDTH: i32 = 224;
pub const PREVIEW_HEIGHT: i32 = 126;
pub const CACHE_LIMIT: usize = 64;
pub const PENDING_LIMIT: usize = 8;

pub fn media_suffixes() -> Vec<&'static str> {
    IMAGE_SUFFIXES.iter().chain(VIDEO_SUFFIXES).copied().collect()
}

/// `Path(name).suffix.lower().lstrip(".")`.
pub fn suffix_of(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => name[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

pub fn is_image(path: &Path) -> bool {
    let suffix = suffix_of(path);
    IMAGE_SUFFIXES.contains(&suffix.as_str()) || loader_suffixes().iter().any(|extra| *extra == suffix)
}

/// Newer formats offered only when the installed image loader reads them:
/// distributions ship wallpaper packages as JPEG XL (Fedora's `F44`).
fn loader_suffixes() -> &'static [String] {
    static SUFFIXES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    SUFFIXES.get_or_init(|| {
        let readable: Vec<String> = Pixbuf::formats().iter().flat_map(|format| format.extensions()).map(|e| e.to_lowercase()).collect();
        ["jxl", "avif"].into_iter().filter(|extra| readable.iter().any(|e| e == extra)).map(str::to_owned).collect()
    })
}

pub fn is_video(path: &Path) -> bool {
    VIDEO_SUFFIXES.contains(&suffix_of(path).as_str())
}

pub fn is_media(path: &Path) -> bool {
    is_image(path) || is_video(path)
}

/// The `ffprobe` argv (local files only, first video stream).
pub fn ffprobe_args(path: &Path) -> Vec<std::ffi::OsString> {
    let absolute = super::import::abspath(path);
    let mut args: Vec<std::ffi::OsString> = [
        "-v", "error", "-protocol_whitelist", "file", "-probesize", "1048576", "-analyzeduration", "1000000",
        "-select_streams", "v:0", "-show_entries", "stream=codec_type", "-of", "csv=p=0",
    ]
    .iter()
    .map(Into::into)
    .collect();
    args.push(absolute.into_os_string());
    args
}

/// Probe a local video off the UI thread (4 s timeout); false without ffprobe.
pub fn usable_video(path: &Path) -> bool {
    match tessel_core::shell::which("ffprobe") {
        Some(probe) => usable_video_with(&probe, path),
        None => false,
    }
}

/// [`usable_video`] with an explicit `ffprobe` (tests use a fake one).
pub fn usable_video_with(probe: &Path, path: &Path) -> bool {
    let Ok(mut child) = Command::new(probe)
        .args(ffprobe_args(path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
    match child.wait_with_output() {
        Ok(output) => output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "video",
        Err(_) => false,
    }
}

/// Wallpapers in `folder` and its subfolders, as `(path, label)` sorted by
/// casefolded label: [`tessel_core::wallpaper::list_pictures`] (one entry for a
/// picture shipped in several sizes or as a KDE package), keeping only
/// readable images with a valid header and playable videos. `screen` is the
/// size of the screen the wallpaper is for (`None`: 16:9): of a picture in
/// several sizes, the one nearest that shape is offered.
pub fn list_directory(folder: &Path, screen: Option<(i32, i32)>) -> std::io::Result<Vec<(PathBuf, String)>> {
    let wanted = |path: &Path| is_media(path) && readable(path);
    let listed = tessel_core::wallpaper::list_pictures(folder, screen, &wanted)?;
    let mut entries = Vec::new();
    for entry in listed {
        let path = entry.path;
        if is_image(&path) {
            // Read image headers off the UI thread; a misleading suffix must
            // not produce an unusable generic-image tile.
            match Pixbuf::file_info(&path) {
                Some((_, width, height)) if width > 0 && height > 0 => {}
                _ => continue,
            }
        } else if !usable_video(&path) {
            continue;
        }
        let label = if entry.dark { format!("{} ({})", entry.label, tr!("Dark")) } else { entry.label };
        entries.push((path, label));
    }
    Ok(entries)
}

fn readable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::R_OK) == 0 }
}

/// Decoded preview pixels (`Send`); turned into a texture on the UI thread.
#[derive(Clone, Debug)]
pub struct PreviewImage {
    pub bytes: glib::Bytes,
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    pub has_alpha: bool,
}

impl PreviewImage {
    fn from_pixbuf(pixbuf: &Pixbuf) -> Self {
        Self {
            bytes: pixbuf.read_pixel_bytes(),
            width: pixbuf.width(),
            height: pixbuf.height(),
            stride: pixbuf.rowstride() as usize,
            has_alpha: pixbuf.has_alpha(),
        }
    }

    pub fn to_texture(&self) -> gdk::Texture {
        let format = if self.has_alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
        gdk::MemoryTexture::new(self.width, self.height, format, &self.bytes, self.stride).upcast()
    }
}

/// Runs on a worker. Videos reuse freedesktop thumbnails (never decoded);
/// `Ok(None)` when none matches; images error when undecodable.
pub fn preview(path: &Path) -> Result<Option<PreviewImage>, String> {
    if is_video(path) {
        let uri = gio::File::for_path(path).uri().to_string();
        let digest = glib::compute_checksum_for_string(glib::ChecksumType::Md5, &uri).map(|d| d.to_string()).unwrap_or_default();
        let base = tessel_core::paths::cache_home().join("thumbnails");
        let mtime = std::fs::metadata(path)
            .map_err(|e| e.to_string())?
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for size in ["large", "x-large", "xx-large", "normal"] {
            let thumbnail = base.join(size).join(format!("{digest}.png"));
            if !thumbnail.is_file() {
                continue;
            }
            if let Ok(pixbuf) = Pixbuf::from_file_at_scale(&thumbnail, PREVIEW_WIDTH, PREVIEW_HEIGHT, true) {
                if pixbuf.option("tEXt::Thumb::URI").as_deref() == Some(uri.as_str())
                    && pixbuf.option("tEXt::Thumb::MTime").as_deref() == Some(mtime.to_string().as_str())
                {
                    return Ok(Some(PreviewImage::from_pixbuf(&pixbuf)));
                }
            }
        }
        return Ok(None);
    }
    let pixbuf = Pixbuf::from_file_at_scale(path, PREVIEW_WIDTH, PREVIEW_HEIGHT, true).map_err(|e| e.message().to_string())?;
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    Ok(Some(PreviewImage::from_pixbuf(&pixbuf)))
}

/// `_validate_media`: supported suffix, decodable image or probed video.
pub fn validate_media(source: &Path) -> Result<(), String> {
    if !is_media(source) {
        return Err(tr!("Choose a supported image or video."));
    }
    if is_image(source) {
        if preview(source).is_err() {
            return Err(tr!("This image cannot be used as a wallpaper."));
        }
    } else if !usable_video(source) {
        return Err(tr!("This video cannot be used as a wallpaper."));
    }
    Ok(())
}

/// First existing of the system collections, Pictures, home.
pub fn default_wallpaper_folder() -> PathBuf {
    let pictures = glib::user_special_dir(glib::UserDirectory::Pictures);
    default_wallpaper_folder_with(pictures.as_deref(), &tessel_core::paths::home(), |p| p.is_dir())
}

/// [`default_wallpaper_folder`] with injectable inputs (tests).
pub fn default_wallpaper_folder_with(pictures: Option<&Path>, home: &Path, is_dir: impl Fn(&Path) -> bool) -> PathBuf {
    let mut candidates: Vec<PathBuf> = vec!["/usr/share/applications/wallpapers".into(), "/usr/share/wallpapers".into()];
    if let Some(pictures) = pictures {
        if !pictures.as_os_str().is_empty() {
            candidates.push(pictures.to_path_buf());
        }
    }
    candidates.push(home.to_path_buf());
    candidates.into_iter().find(|p| is_dir(p)).unwrap_or_else(|| home.to_path_buf())
}
