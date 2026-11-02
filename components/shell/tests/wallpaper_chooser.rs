//! Pure parts of ~/tessel-bluetooth/shared/test_wallpaper_chooser.py (listing,
//! default folder, ffprobe validation, previews). Widget tests live in
//! wallpaper_chooser_ui.rs (private headless labwc only).
#![cfg(feature = "fileops")]

use std::fs;
use std::path::{Path, PathBuf};

use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
use tessel_shell::wallpaper_chooser::media;

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp() -> Temp {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tmp = std::env::temp_dir();
    assert!(!tmp.starts_with("/tmp"), "set TMPDIR to a scratch dir");
    let dir = tmp.join(format!("wallpaper-chooser-test-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Temp(dir)
}

pub fn write_png(path: &Path, width: i32, height: i32) {
    let pixbuf = Pixbuf::new(Colorspace::Rgb, true, 8, width, height).unwrap();
    pixbuf.fill(0x334c99ff);
    pixbuf.savev(path, "png", &[]).unwrap();
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn media_listing_filters_and_sorts_supported_files() {
    let t = temp();
    let folder = &t.0;
    write_png(&folder.join("Wallpaper.png"), 64, 36);
    for name in ["clip.MP4", "broken.JPEG", "notes.txt", ".hidden.png", "unsupported.mkv"] {
        fs::write(folder.join(name), b"fixture").unwrap();
    }
    write_png(&folder.join("picture.PNG"), 64, 36);
    std::os::unix::fs::symlink(folder.join("absent.png"), folder.join("missing.png")).unwrap();
    fs::create_dir(folder.join("Another Folder.png")).unwrap();
    let names: Vec<String> = media::list_directory(folder).unwrap().into_iter().map(|(_, n)| n).collect();
    assert_eq!(names, vec!["picture.PNG", "Wallpaper.png"]);
    assert!(media::list_directory(&folder.join("nope")).is_err());
}

#[test]
fn video_validation_uses_local_only_ffprobe() {
    let t = temp();
    let video = t.0.join("Movie.mp4");
    fs::write(&video, b"fixture").unwrap();
    let log = t.0.join("argv");
    let ok = script(&t.0, "ffprobe-ok", &format!("printf '%s\\n' \"$@\" > '{}'\necho video", log.display()));
    assert!(media::usable_video_with(&ok, &video));
    let argv = fs::read_to_string(&log).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        argv,
        vec![
            "-v", "error", "-protocol_whitelist", "file", "-probesize", "1048576", "-analyzeduration", "1000000", "-select_streams", "v:0",
            "-show_entries", "stream=codec_type", "-of", "csv=p=0", video.to_str().unwrap()
        ]
    );
    assert!(!media::usable_video_with(&script(&t.0, "ffprobe-audio", "echo audio"), &video));
    assert!(!media::usable_video_with(&script(&t.0, "ffprobe-fail", "echo video; exit 1"), &video));
    assert!(!media::usable_video_with(&t.0.join("missing-ffprobe"), &video));
}

#[test]
fn video_validation_keeps_playable_video_without_cached_thumbnail() {
    let (Some(ffmpeg), Some(_)) = (tessel_core::shell::which("ffmpeg"), tessel_core::shell::which("ffprobe")) else {
        eprintln!("ffmpeg tools are unavailable");
        return;
    };
    let t = temp();
    let video = t.0.join("Movie.mp4");
    let status = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-f", "lavfi", "-i", "color=c=blue:s=32x32:r=1", "-t", "1", "-c:v", "mpeg4"])
        .arg(&video)
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(media::usable_video(&video));
    assert!(media::list_directory(&t.0).unwrap().iter().any(|(_, n)| n == "Movie.mp4"));
    assert!(media::preview(&video).unwrap().is_none(), "No cached video thumbnail is required");
    assert!(media::validate_media(&video).is_ok());
}

#[test]
fn default_folder_prefers_system_collection_with_user_fallbacks() {
    let candidates = ["/usr/share/applications/wallpapers", "/usr/share/wallpapers", "/fixture/Pictures", "/fixture/Home"];
    for offset in 0..candidates.len() {
        let remaining: Vec<&str> = candidates[offset..].to_vec();
        let got = media::default_wallpaper_folder_with(Some(Path::new(candidates[2])), Path::new(candidates[3]), |p| {
            remaining.iter().any(|r| Path::new(r) == p)
        });
        assert_eq!(got, PathBuf::from(candidates[offset]));
    }
    // Nothing exists: home.
    assert_eq!(media::default_wallpaper_folder_with(None, Path::new("/fixture/Home"), |_| false), PathBuf::from("/fixture/Home"));
}

#[test]
fn image_previews_fit_224x126_and_validation_rejects_damage() {
    let t = temp();
    let wide = t.0.join("Wide.png");
    write_png(&wide, 800, 100);
    let image = media::preview(&wide).unwrap().unwrap();
    assert!(image.width <= media::PREVIEW_WIDTH && image.height <= media::PREVIEW_HEIGHT);
    assert_eq!(image.width, media::PREVIEW_WIDTH);
    let broken = t.0.join("Broken.png");
    fs::write(&broken, b"not an image").unwrap();
    assert!(media::preview(&broken).is_err());
    assert_eq!(media::validate_media(&broken).unwrap_err(), "This image cannot be used as a wallpaper.");
    assert_eq!(media::validate_media(&t.0.join("notes.txt")).unwrap_err(), "Choose a supported image or video.");
    assert!(media::validate_media(&wide).is_ok());
    assert_eq!((media::CACHE_LIMIT, media::PENDING_LIMIT), (64, 8));
}
