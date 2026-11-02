//! Per-monitor wallpaper colours (wallpaper_palette.py).
//!
//! [`PaletteReader`] (worker-owned) reads Tessel Shell's
//! `~/.config/tessel-shell/wallpapers.json`, samples each selected image with
//! gdk-pixbuf (64 px thumbnail, the same decoder Python uses) or a video's
//! representative frame with a bounded `ffmpeg`, and falls back to the
//! configured solid colour. Unchanged files are sampled once.
//! [`WallpaperPalette`] (main thread) runs reads on a worker thread, polls
//! every 3 s, watches the settings file and the selected media's parent
//! directories, ignores superseded results, and calls `on_changed()` on the
//! main loop whenever colours, sources, textures or the error change.

use super::frosted::{self, FrostCache, FrostedWallpaperCache, Image};
use super::fsx;
use gio::prelude::*;
use gtk::gdk_pixbuf::Pixbuf;
use indexmap::IndexMap;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tessel_core::scheduler::{GlibScheduler, Scheduler};
use tessel_gtk::palette::Rgb;

pub const POLL_MS: u32 = 3000;
pub const EVENT_DELAY_MS: u32 = 150;
pub const MAX_CONFIG_BYTES: usize = 65536;
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_VIDEO_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 100_000_000;
pub const VIDEO_SUFFIXES: [&str; 7] = [".mp4", ".m4v", ".mov", ".webm", ".mkv", ".avi", ".gif"];
pub const VIDEO_TIMEOUT: Duration = Duration::from_secs(6);

/// `Path.suffix.lower()`.
pub fn suffix(path: &Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default()
}

pub fn is_video(path: &Path) -> bool {
    VIDEO_SUFFIXES.contains(&suffix(path).as_str())
}

pub fn video_format(path: &Path) -> Option<&'static str> {
    match suffix(path).as_str() {
        ".mp4" | ".m4v" | ".mov" => Some("mov"),
        ".webm" | ".mkv" => Some("matroska"),
        ".avi" => Some("avi"),
        ".gif" => Some("gif"),
        _ => None,
    }
}

/// Tessel Shell's CSS colour → RGB (`Gdk.RGBA.parse`), rejecting invisible ones.
pub fn configured_color(value: Option<&Value>) -> Option<Rgb> {
    let text = value?.as_str()?;
    if text.chars().count() > 128 {
        return None;
    }
    let color = gtk::gdk::RGBA::parse(text).ok()?;
    if color.alpha() <= 0.0 {
        return None;
    }
    Some([color.red(), color.green(), color.blue()].map(|c| fsx::py_round(c as f64 * 255.0) as u8))
}

/// Python's `colorsys.rgb_to_hsv` (saturation, value).
fn saturation_value(r: f64, g: f64, b: f64) -> (f64, f64) {
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    if high == low || high == 0.0 {
        return (0.0, high);
    }
    ((high - low) / high, high)
}

/// Choose a frequent colour, gently favouring colour over black borders.
pub fn dominant_color(width: usize, height: usize, channels: usize, stride: usize, pixels: &[u8]) -> Option<Rgb> {
    let mut buckets: IndexMap<(u8, u8, u8), [f64; 4]> = IndexMap::new();
    for y in 0..height {
        for x in 0..width {
            let offset = y * stride + x * channels;
            let (red, green, blue) = (pixels[offset], pixels[offset + 1], pixels[offset + 2]);
            let alpha = if channels == 4 { pixels[offset + 3] as f64 / 255.0 } else { 1.0 };
            if alpha < 0.1 {
                continue;
            }
            let bucket = buckets.entry((red / 32, green / 32, blue / 32)).or_insert([0.0; 4]);
            bucket[0] += alpha;
            bucket[1] += red as f64 * alpha;
            bucket[2] += green as f64 * alpha;
            bucket[3] += blue as f64 * alpha;
        }
    }
    let score = |b: &[f64; 4]| {
        let (saturation, value) = saturation_value(b[1] / b[0] / 255.0, b[2] / b[0] / 255.0, b[3] / b[0] / 255.0);
        b[0] * (0.25 + saturation) * (0.35 + value)
    };
    let mut winner: Option<&[f64; 4]> = None;
    for bucket in buckets.values() {
        if winner.is_none_or(|w| score(bucket) > score(w)) {
            winner = Some(bucket);
        }
    }
    winner.map(|w| [w[1], w[2], w[3]].map(|c| fsx::py_round(c / w[0]) as u8))
}

fn pixbuf_color(pixbuf: &Pixbuf) -> Option<Rgb> {
    let bytes = pixbuf.read_pixel_bytes();
    dominant_color(pixbuf.width() as usize, pixbuf.height() as usize, pixbuf.n_channels() as usize,
                   pixbuf.rowstride() as usize, &bytes)
}

/// Decode a bounded thumbnail (worker thread only).
pub fn image_thumbnail(path: &Path, size: i32) -> Result<Pixbuf, String> {
    let info = std::fs::metadata(path).map_err(|e| fsx::os_error_path(&e, path))?;
    if !info.is_file() || info.size() > MAX_FILE_BYTES {
        return Err("Wallpaper is not a regular image file or is too large".into());
    }
    let (_format, width, height) = Pixbuf::file_info(path).ok_or("Wallpaper image format is unsupported")?;
    if width <= 0 || height <= 0 || width as u64 * height as u64 > MAX_PIXELS {
        return Err("Wallpaper image dimensions are too large".into());
    }
    Pixbuf::from_file_at_scale(path, size, size, true).map_err(|e| tessel_core::pyerr::python_error_text(&e))
}

pub fn image_color(path: &Path) -> Result<Option<Rgb>, String> {
    Ok(pixbuf_color(&image_thumbnail(path, 64)?))
}

/// Outcome of the bounded ffmpeg run.
pub enum RunOutcome {
    Finished { code: i32, stdout: Vec<u8> },
    TimedOut,
}

pub type FindProgram = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;
pub type RunProgram = Arc<dyn Fn(&[String], Duration) -> Result<RunOutcome, String> + Send + Sync>;

/// `subprocess.run(command, stdin=DEVNULL, stdout=PIPE, stderr=DEVNULL, timeout)`.
pub fn run_program(argv: &[String], timeout: Duration) -> Result<RunOutcome, String> {
    let mut child = std::process::Command::new(&argv[0]).args(&argv[1..])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
        .spawn().map_err(|e| fsx::os_error_path(&e, Path::new(&argv[0])))?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = stdout.read_to_end(&mut data);
        data
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| fsx::os_error_text(&e))? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Ok(RunOutcome::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = reader.join().unwrap_or_default();
    use std::os::unix::process::ExitStatusExt;
    Ok(RunOutcome::Finished { code: status.code().unwrap_or_else(|| -status.signal().unwrap_or(1)), stdout })
}

/// Image/video sampling with injectable `shutil.which` / `subprocess.run`.
#[derive(Clone)]
pub struct MediaSampler {
    pub find_program: FindProgram,
    pub run: RunProgram,
}

impl Default for MediaSampler {
    fn default() -> Self {
        Self { find_program: Arc::new(tessel_core::shell::which), run: Arc::new(run_program) }
    }
}

/// An RGB frame (`width * height * 3` bytes).
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl MediaSampler {
    /// Sample a local animation's representative frame with bounded ffmpeg work.
    pub fn video_thumbnail(&self, path: &Path, size: usize, preserve_aspect: bool) -> Result<Frame, String> {
        let info = std::fs::metadata(path).map_err(|e| fsx::os_error_path(&e, path))?;
        if !info.is_file() || info.size() > MAX_VIDEO_BYTES {
            return Err("Wallpaper video is not a regular file or is too large".into());
        }
        let decoder = (self.find_program)("ffmpeg").ok_or("Video wallpaper color matching requires ffmpeg")?;
        let format = video_format(path).ok_or("Wallpaper video format is unsupported")?;
        let scale = if preserve_aspect { format!("scale={size}:{size}:force_original_aspect_ratio=decrease:flags=area") }
                    else { format!("scale={size}:{size}:flags=area") };
        let mut command: Vec<String> = [decoder.to_string_lossy().as_ref(), "-hide_banner", "-loglevel", "error", "-nostdin",
            "-max_alloc", &(256 * 1024 * 1024).to_string(), "-threads", "1", "-max_pixels", &MAX_PIXELS.to_string(),
            "-protocol_whitelist", "file", "-format_whitelist", format, "-f", format, "-probesize", "1048576",
            "-analyzeduration", "1000000", "-i", path.to_string_lossy().as_ref(), "-map", "0:v:0", "-an", "-sn", "-dn",
            "-vf", &format!("{scale},thumbnail=30"), "-filter_threads", "1", "-frames:v", "1", "-threads", "1",
            "-pix_fmt", "rgb24"].iter().map(|s| s.to_string()).collect();
        let output: &[&str] = if preserve_aspect { &["-c:v", "ppm", "-f", "image2pipe", "pipe:1"] } else { &["-f", "rawvideo", "pipe:1"] };
        command.extend(output.iter().map(|s| s.to_string()));
        let (code, mut pixels) = match (self.run)(&command, VIDEO_TIMEOUT)? {
            RunOutcome::TimedOut => return Err("Video wallpaper color sampling timed out".into()),
            RunOutcome::Finished { code, stdout } => (code, stdout),
        };
        let (mut width, mut height) = (size, size);
        if preserve_aspect {
            let header = regex::bytes::Regex::new(r"^P6\s+(\d+)\s+(\d+)\s+255\s").unwrap();
            let captures = header.captures(&pixels).ok_or("Could not sample the video wallpaper")?;
            let number = |i: usize| std::str::from_utf8(&captures[i]).ok().and_then(|t| t.parse::<usize>().ok()).unwrap_or(0);
            (width, height) = (number(1), number(2));
            let end = captures.get(0).unwrap().end();
            pixels = pixels[end..].to_vec();
        }
        if code != 0 || !(0 < width && width <= size && 0 < height && height <= size) || pixels.len() != width * height * 3 {
            return Err("Could not sample the video wallpaper".into());
        }
        Ok(Frame { width, height, pixels })
    }

    pub fn video_color(&self, path: &Path) -> Result<Option<Rgb>, String> {
        let frame = self.video_thumbnail(path, 64, false)?;
        Ok(dominant_color(frame.width, frame.height, 3, frame.width * 3, &frame.pixels))
    }

    pub fn media_color(&self, path: &Path) -> Result<Option<Rgb>, String> {
        if is_video(path) { self.video_color(path) } else { image_color(path) }
    }

    /// `frost_image`: one bounded image/video still for Frosted textures.
    pub fn frost_image(&self, path: &Path, size: u32) -> Result<Image, String> {
        if is_video(path) {
            let frame = self.video_thumbnail(path, size as usize, true)?;
            let mut image = Image::from_rgb(frame.width as u32, frame.height as u32, &frame.pixels);
            // Tessel Shell's video Center mode uses output-fit.
            image.source_size_unknown = true;
            return Ok(image);
        }
        let mut image = Image::from_pixbuf(&image_thumbnail(path, size as i32)?);
        if let Some((_format, width, height)) = Pixbuf::file_info(path) {
            image.source_size = Some((width as i64, height as i64));
        }
        Ok(image)
    }
}

fn monitor_sizes(sizes: &BTreeMap<String, (i64, i64)>) -> BTreeMap<String, (i64, i64)> {
    let mut valid = BTreeMap::new();
    for (name, size) in sizes {
        if let Some(value) = frosted::normalized_size(Some(*size)) {
            valid.insert(name.clone(), value);
        }
        if valid.len() == 64 {
            break;
        }
    }
    valid
}

/// `_file_stamp`: metadata tuple, or the error's class name.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum FileStamp {
    Ok(u64, u64, u64, i64, i64, u32),
    Err(&'static str),
}

pub fn file_stamp(path: &Path) -> FileStamp {
    match std::fs::metadata(path) {
        Ok(v) => FileStamp::Ok(v.dev(), v.ino(), v.size(), v.mtime() * 1_000_000_000 + v.mtime_nsec(),
                               v.ctime() * 1_000_000_000 + v.ctime_nsec(), v.mode()),
        Err(error) => FileStamp::Err(match error.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            std::io::ErrorKind::NotADirectory => "NotADirectoryError",
            _ => "OSError",
        }),
    }
}

/// One read: colours/sources/textures per output connector (dict order kept,
/// equality order-insensitive like Python dicts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaletteResult {
    pub colors: IndexMap<String, Rgb>,
    pub sources: IndexMap<String, Option<PathBuf>>,
    pub error: Option<String>,
    pub frosts: IndexMap<String, String>,
}

impl PaletteResult {
    pub fn error(text: String) -> Self {
        Self { error: Some(text), ..Default::default() }
    }
}

/// What the controller needs from a reader (tests substitute simple fakes).
pub trait WallpaperReader: Send {
    fn read(&mut self, force: bool) -> PaletteResult;
    fn set_frosted_enabled(&mut self, _enabled: bool) {}
    fn set_monitor_sizes(&mut self, _sizes: &BTreeMap<String, (i64, i64)>) {}
    fn config_path(&self) -> Option<PathBuf> {
        None
    }
    /// Replace the Frosted texture cache (no-op for readers without one).
    fn set_frost_cache(&mut self, _cache: Box<dyn FrostCache>) {}
}

pub type Sample = Box<dyn FnMut(&Path) -> Result<Option<Rgb>, String> + Send>;
type FrostKey = (PathBuf, FileStamp, Option<(i64, i64)>, &'static str, Rgb);

#[derive(PartialEq)]
struct Fingerprint {
    raw: Vec<u8>,
    frosted: bool,
    sizes: Vec<(String, (i64, i64))>,
    entries: Vec<(String, String, Option<FileStamp>)>,
}

/// Worker-owned reader; unchanged image files are sampled only once.
pub struct PaletteReader {
    pub config_path: PathBuf,
    sample: Sample,
    fingerprint: Option<Fingerprint>,
    result: PaletteResult,
    pub frosted_enabled: bool,
    frost_cache: Box<dyn FrostCache>,
    samples: HashMap<(PathBuf, FileStamp), (Option<Rgb>, Option<String>)>,
    frost_samples: HashMap<FrostKey, (Option<String>, Option<String>)>,
    pub monitor_sizes: BTreeMap<String, (i64, i64)>,
}

/// `Path(value).expanduser()` for `~` and `~/…` (`$HOME`).
fn expand_user(value: &str) -> PathBuf {
    if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(tessel_core::paths::home);
        return if value == "~" { home } else { home.join(&value[2..]) };
    }
    PathBuf::from(value)
}

impl PaletteReader {
    pub fn new(config_path: PathBuf, sample: Option<Sample>, frosted: bool, frost_cache: Option<Box<dyn FrostCache>>) -> Self {
        let sampler = MediaSampler::default();
        let frost_cache = frost_cache.unwrap_or_else(|| {
            let loader_sampler = sampler.clone();
            Box::new(FrostedWallpaperCache::new(None, Some(Box::new(move |path, size| loader_sampler.frost_image(path, size))),
                                                frosted::MAX_CACHE_FILES, frosted::MAX_CACHE_BYTES))
        });
        Self {
            config_path, sample: sample.unwrap_or_else(|| Box::new(move |path| sampler.media_color(path))),
            fingerprint: None, result: PaletteResult::default(), frosted_enabled: frosted, frost_cache,
            samples: HashMap::new(), frost_samples: HashMap::new(), monitor_sizes: BTreeMap::new(),
        }
    }

    /// Tessel Shell's default `~/.config/tessel-shell/wallpapers.json`.
    pub fn default_config_path() -> PathBuf {
        tessel_core::paths::home().join(".config/tessel-shell/wallpapers.json")
    }

    pub fn frost_cache_mut(&mut self) -> &mut dyn FrostCache {
        self.frost_cache.as_mut()
    }

    fn read_config(&self) -> Result<(Vec<u8>, serde_json::Map<String, Value>), String> {
        let file = std::fs::File::open(&self.config_path).map_err(|e| fsx::os_error_path(&e, &self.config_path))?;
        let mut raw = Vec::new();
        file.take(MAX_CONFIG_BYTES as u64 + 1).read_to_end(&mut raw).map_err(|e| fsx::os_error_path(&e, &self.config_path))?;
        if raw.len() > MAX_CONFIG_BYTES {
            return Err("Wallpaper settings are too large".into());
        }
        let config: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        match config {
            Value::Object(map) => Ok((raw, map)),
            _ => Err("Wallpaper settings must contain display choices".into()),
        }
    }
}

impl WallpaperReader for PaletteReader {
    fn set_frost_cache(&mut self, cache: Box<dyn FrostCache>) {
        self.frost_cache = cache;
    }

    fn set_frosted_enabled(&mut self, enabled: bool) {
        self.frosted_enabled = enabled;
    }

    fn set_monitor_sizes(&mut self, sizes: &BTreeMap<String, (i64, i64)>) {
        self.monitor_sizes = monitor_sizes(sizes);
    }

    fn config_path(&self) -> Option<PathBuf> {
        Some(self.config_path.clone())
    }

    fn read(&mut self, force: bool) -> PaletteResult {
        let (raw, config) = match self.read_config() {
            Ok(value) => value,
            Err(error) => {
                self.fingerprint = None;
                self.result = PaletteResult::error(format!("Wallpaper settings unavailable: {error}"));
                return self.result.clone();
            }
        };
        let mut entries: Vec<(String, &serde_json::Map<String, Value>, Option<PathBuf>)> = Vec::new();
        for (name, entry) in &config {
            let Some(entry) = entry.as_object().filter(|_| !name.starts_with('_')) else { continue };
            if entries.len() == 64 {
                break;
            }
            let mut path = entry.get("file_path").and_then(Value::as_str).filter(|v| !v.is_empty()).map(expand_user)
                .filter(|p| p.is_absolute());
            if entry.get("mode").and_then(Value::as_str) == Some("Color Only") {
                path = None;
            }
            entries.push((name.clone(), entry, path));
        }
        let stamps: HashMap<PathBuf, FileStamp> = entries.iter().filter_map(|(_, _, p)| p.clone()).map(|p| { let s = file_stamp(&p); (p, s) }).collect();
        let options: HashMap<&str, (Option<(i64, i64)>, &'static str, Rgb)> = entries.iter().map(|(name, entry, _)| {
            (name.as_str(), (self.monitor_sizes.get(name).copied(), frosted::normalized_mode(entry.get("mode").and_then(Value::as_str)),
                             frosted::normalized_color(configured_color(entry.get("color")))))
        }).collect();
        let fingerprint = Fingerprint {
            raw, frosted: self.frosted_enabled, sizes: self.monitor_sizes.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            entries: entries.iter().map(|(name, _, path)| (name.clone(),
                path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "None".into()),
                path.as_ref().and_then(|p| stamps.get(p).cloned()))).collect(),
        };
        // Cache cleaners may remove a selected texture independently of its source.
        let missing: HashSet<FrostKey> = self.frost_samples.iter()
            .filter(|(_, (texture, _))| self.frosted_enabled && texture.as_ref().is_some_and(|t| !Path::new(t).is_file()))
            .map(|(key, _)| key.clone()).collect();
        if !force && self.fingerprint.as_ref() == Some(&fingerprint) && missing.is_empty() {
            return self.result.clone();
        }
        // Retain only current sources: a settings save or monitor rename must
        // not decode an unchanged wallpaper again, even on an explicit refresh.
        let keys: HashSet<(PathBuf, FileStamp)> = stamps.iter().map(|(p, s)| (p.clone(), s.clone())).collect();
        let frost_keys: HashSet<FrostKey> = entries.iter().filter_map(|(name, _, path)| path.as_ref().map(|p| {
            let (size, mode, color) = options[name.as_str()];
            (p.clone(), stamps[p].clone(), size, mode, color)
        })).collect();
        self.samples.retain(|key, _| keys.contains(key));
        self.frost_samples.retain(|key, _| frost_keys.contains(key) && !missing.contains(key));
        let mut protected: HashSet<String> = self.frost_samples.values().filter_map(|(t, _)| t.clone()).collect();
        self.frost_cache.protect(&protected);
        let mut result = PaletteResult::default();
        let mut errors = Vec::new();
        for (name, entry, path) in &entries {
            result.sources.insert(name.clone(), path.clone());
            let mut color;
            match path {
                None => color = configured_color(entry.get("color")),
                Some(path) => {
                    let key = (path.clone(), stamps[path].clone());
                    if !self.samples.contains_key(&key) {
                        let sampled = match (self.sample)(path) {
                            Ok(color) => (color, None),
                            Err(error) => (None, Some(error)),
                        };
                        self.samples.insert(key.clone(), sampled);
                    }
                    let (sampled, error) = self.samples[&key].clone();
                    color = sampled;
                    if let Some(error) = error {
                        errors.push(format!("{name}: {error}"));
                        if is_video(path) {
                            color = configured_color(entry.get("color"));
                        }
                    }
                    if self.frosted_enabled {
                        let (size, mode, background) = options[name.as_str()];
                        let frost_key: FrostKey = (path.clone(), stamps[path].clone(), size, mode, background);
                        if !self.frost_samples.contains_key(&frost_key) {
                            let value = match self.frost_cache.texture(path, size, mode, background) {
                                Ok(texture) => (Some(texture), None),
                                Err(error) => (None, Some(error)),
                            };
                            self.frost_samples.insert(frost_key.clone(), value);
                        }
                        let (texture, error) = self.frost_samples[&frost_key].clone();
                        if let Some(texture) = texture {
                            result.frosts.insert(name.clone(), texture.clone());
                            protected.insert(texture);
                            self.frost_cache.protect(&protected);
                        }
                        if let Some(error) = error {
                            errors.push(format!("{name}: Frosted background unavailable: {error}"));
                        }
                    }
                }
            }
            if let Some(color) = color {
                result.colors.insert(name.clone(), color);
            }
        }
        result.error = if errors.is_empty() { None } else { Some(errors.join("; ")) };
        self.fingerprint = Some(fingerprint);
        self.result = result;
        self.result.clone()
    }
}

pub type Work = Box<dyn FnOnce() + Send>;
/// Runs work off the main thread (tests queue it instead).
pub type Worker = Box<dyn Fn(Work)>;
/// Delivers a closure to the main loop (`GLib.idle_add` from the worker).
pub type Poster = Arc<dyn Fn(Work) + Send + Sync>;

/// Construction options; `None` uses real threads, timers and files.
#[derive(Default)]
pub struct WallpaperOptions {
    pub config_path: Option<PathBuf>,
    pub scheduler: Option<Rc<dyn Scheduler>>,
    pub worker: Option<Worker>,
    pub poster: Option<Poster>,
    pub reader: Option<Box<dyn WallpaperReader>>,
}

thread_local! {
    static CONTROLLERS: RefCell<HashMap<u64, Weak<Inner>>> = RefCell::default();
}
static NEXT_CONTROLLER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct Monitor {
    monitor: gio::FileMonitor,
    handler: glib::SignalHandlerId,
}

struct Inner {
    id: u64,
    on_changed: Option<Box<dyn Fn(&WallpaperPalette)>>,
    colors: RefCell<IndexMap<String, Rgb>>,
    sources: RefCell<IndexMap<String, Option<PathBuf>>>,
    frosts: RefCell<IndexMap<String, String>>,
    error: RefCell<Option<String>>,
    enabled: Cell<bool>,
    frosted_enabled: Cell<bool>,
    monitor_sizes: RefCell<BTreeMap<String, (i64, i64)>>,
    scheduler: Rc<dyn Scheduler>,
    worker: Worker,
    poster: Poster,
    reader: Arc<Mutex<Box<dyn WallpaperReader>>>,
    config_path: Option<PathBuf>,
    started: Cell<bool>,
    closed: Cell<bool>,
    busy: Cell<bool>,
    pending: Cell<bool>,
    serial: Cell<u64>,
    timer: Cell<Option<u64>>,
    event_timer: Cell<Option<u64>>,
    monitors: RefCell<HashMap<PathBuf, Monitor>>,
    watch_targets: RefCell<HashSet<PathBuf>>,
}

/// Deliver `on_changed(controller)` on the main thread, including disable.
#[derive(Clone)]
pub struct WallpaperPalette(Rc<Inner>);

fn default_poster() -> Poster {
    let context = glib::MainContext::ref_thread_default();
    Arc::new(move |work: Work| context.invoke(work))
}

impl WallpaperPalette {
    pub fn new(on_changed: Option<Box<dyn Fn(&WallpaperPalette)>>, options: WallpaperOptions) -> Self {
        let reader = options.reader.unwrap_or_else(|| Box::new(PaletteReader::new(
            options.config_path.unwrap_or_else(PaletteReader::default_config_path), None, false, None)));
        let config_path = reader.config_path();
        let id = NEXT_CONTROLLER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let inner = Rc::new(Inner {
            id, on_changed, colors: RefCell::default(), sources: RefCell::default(), frosts: RefCell::default(),
            error: RefCell::default(), enabled: Cell::new(false), frosted_enabled: Cell::new(false),
            monitor_sizes: RefCell::default(),
            scheduler: options.scheduler.unwrap_or_else(|| Rc::new(GlibScheduler::default())),
            worker: options.worker.unwrap_or_else(|| Box::new(|work: Work| {
                let _ = std::thread::Builder::new().name("tessel-dock-wallpaper".into()).spawn(work);
            })),
            poster: options.poster.unwrap_or_else(default_poster),
            reader: Arc::new(Mutex::new(reader)), config_path, started: Cell::new(false), closed: Cell::new(false),
            busy: Cell::new(false), pending: Cell::new(false), serial: Cell::new(0), timer: Cell::new(None),
            event_timer: Cell::new(None), monitors: RefCell::default(), watch_targets: RefCell::default(),
        });
        CONTROLLERS.with(|c| c.borrow_mut().insert(id, Rc::downgrade(&inner)));
        Self(inner)
    }

    /// First available colour (summaries only).
    pub fn color(&self) -> Option<Rgb> {
        self.0.colors.borrow().values().next().copied()
    }

    pub fn source(&self) -> Option<Option<PathBuf>> {
        self.0.sources.borrow().values().next().cloned()
    }

    /// Unknown outputs return `None` so they keep the system theme.
    pub fn color_for(&self, output: &str) -> Option<Rgb> {
        self.0.colors.borrow().get(output).copied()
    }

    pub fn frost_for(&self, output: &str) -> Option<String> {
        self.0.frosts.borrow().get(output).cloned()
    }

    pub fn colors(&self) -> IndexMap<String, Rgb> {
        self.0.colors.borrow().clone()
    }

    pub fn sources(&self) -> IndexMap<String, Option<PathBuf>> {
        self.0.sources.borrow().clone()
    }

    pub fn frosts(&self) -> IndexMap<String, String> {
        self.0.frosts.borrow().clone()
    }

    pub fn error(&self) -> Option<String> {
        self.0.error.borrow().clone()
    }

    pub fn enabled(&self) -> bool {
        self.0.enabled.get()
    }

    pub fn watched_directories(&self) -> Vec<PathBuf> {
        self.0.monitors.borrow().keys().cloned().collect()
    }

    pub fn event_timer_pending(&self) -> bool {
        self.0.event_timer.get().is_some()
    }

    /// Access the reader (tests swap the frost cache).
    pub fn with_reader<R>(&self, f: impl FnOnce(&mut dyn WallpaperReader) -> R) -> R {
        f(self.0.reader.lock().unwrap().as_mut())
    }

    pub fn set_frosted_enabled(&self, enabled: bool) {
        if self.0.closed.get() || enabled == self.0.frosted_enabled.get() {
            return;
        }
        self.0.frosted_enabled.set(enabled);
        self.bump();
        if !enabled {
            let result = PaletteResult { colors: self.colors(), sources: self.sources(), error: self.error(), frosts: IndexMap::new() };
            self.publish(result);
        }
        self.refresh();
    }

    pub fn set_monitor_sizes(&self, sizes: &BTreeMap<String, (i64, i64)>) {
        let sizes = monitor_sizes(sizes);
        if self.0.closed.get() || *self.0.monitor_sizes.borrow() == sizes {
            return;
        }
        *self.0.monitor_sizes.borrow_mut() = sizes;
        self.bump();
        self.refresh();
    }

    pub fn start(&self) {
        if self.0.closed.get() || self.0.started.get() {
            return;
        }
        self.0.started.set(true);
        if self.0.enabled.get() {
            self.schedule();
            self.refresh();
        }
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.bump();
        self.cancel_timer();
        self.cancel_monitors();
    }

    pub fn set_enabled(&self, enabled: bool) {
        if self.0.closed.get() || enabled == self.0.enabled.get() {
            return;
        }
        self.0.enabled.set(enabled);
        self.bump();
        if enabled && self.0.started.get() {
            self.schedule();
            self.refresh();
        } else if !enabled {
            self.cancel_timer();
            self.cancel_monitors();
            self.0.pending.set(false);
            self.publish(PaletteResult::default());
        }
    }

    pub fn refresh(&self) {
        if self.0.closed.get() || !self.0.started.get() || !self.0.enabled.get() {
            return;
        }
        self.bump();
        if self.0.busy.get() {
            self.0.pending.set(true);
        } else {
            self.launch(true);
        }
    }

    /// Stop the 3 s poll (tests prove notifications work without it).
    pub fn cancel_timer(&self) {
        if let Some(timer) = self.0.timer.take() {
            self.0.scheduler.cancel(timer);
        }
    }

    fn bump(&self) {
        self.0.serial.set(self.0.serial.get() + 1);
    }

    fn schedule(&self) {
        self.sync_monitors();
        if self.0.timer.get().is_none() {
            self.arm_poll();
        }
    }

    fn arm_poll(&self) {
        let weak = Rc::downgrade(&self.0);
        let id = self.0.scheduler.timeout(POLL_MS, Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                WallpaperPalette(inner).poll();
            }
        }));
        self.0.timer.set(Some(id));
    }

    /// Watch parent directories so atomic replacements remain observable.
    fn sync_monitors(&self) {
        if self.0.closed.get() || !self.0.enabled.get() {
            return;
        }
        let mut targets: HashSet<PathBuf> = self.0.sources.borrow().values().flatten().cloned().collect();
        if let Some(config) = &self.0.config_path {
            targets.insert(config.clone());
        }
        let mut directories = HashSet::new();
        for target in &targets {
            let mut directory = target.parent().map(Path::to_owned).unwrap_or_else(|| target.clone());
            // Also discover a settings/media directory created after startup.
            while !directory.is_dir() {
                match directory.parent() {
                    Some(parent) if parent != directory => directory = parent.to_owned(),
                    _ => break,
                }
            }
            directories.insert(directory);
        }
        *self.0.watch_targets.borrow_mut() = targets;
        let stale: Vec<PathBuf> = self.0.monitors.borrow().keys().filter(|d| !directories.contains(*d)).cloned().collect();
        for directory in stale {
            if let Some(entry) = self.0.monitors.borrow_mut().remove(&directory) {
                entry.monitor.disconnect(entry.handler);
                entry.monitor.cancel();
            }
        }
        for directory in directories {
            if self.0.monitors.borrow().contains_key(&directory) {
                continue;
            }
            let Ok(monitor) = gio::File::for_path(&directory).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>)
            else { continue }; // Periodic stat checks remain for unwatchable mounts.
            let weak = Rc::downgrade(&self.0);
            let handler = monitor.connect_changed(move |_, file, other, _| {
                if let Some(inner) = weak.upgrade() {
                    let paths: Vec<PathBuf> = [Some(file), other].into_iter().flatten().filter_map(|f| f.path()).collect();
                    WallpaperPalette(inner).file_changed(&paths);
                }
            });
            self.0.monitors.borrow_mut().insert(directory, Monitor { monitor, handler });
        }
    }

    fn cancel_monitors(&self) {
        if let Some(timer) = self.0.event_timer.take() {
            self.0.scheduler.cancel(timer);
        }
        for (_, entry) in self.0.monitors.borrow_mut().drain() {
            entry.monitor.disconnect(entry.handler);
            entry.monitor.cancel();
        }
        self.0.watch_targets.borrow_mut().clear();
    }

    /// A watched directory reported `paths` (file and other file).
    pub fn file_changed(&self, paths: &[PathBuf]) {
        if self.0.closed.get() || !self.0.enabled.get() {
            return;
        }
        let relevant = paths.iter().any(|path| self.0.watch_targets.borrow().iter().any(|target| target.starts_with(path)));
        if !relevant {
            return;
        }
        // Do not publish an image that finished decoding after its selection
        // changed; coalesce multi-write settings saves before loading.
        self.bump();
        if let Some(timer) = self.0.event_timer.take() {
            self.0.scheduler.cancel(timer);
        }
        let weak = Rc::downgrade(&self.0);
        self.0.event_timer.set(Some(self.0.scheduler.timeout(EVENT_DELAY_MS, Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.event_timer.set(None);
                let palette = WallpaperPalette(inner);
                palette.sync_monitors();
                palette.refresh();
            }
        }))));
    }

    fn poll(&self) {
        self.0.timer.set(None);
        if self.0.closed.get() || !self.0.enabled.get() {
            return;
        }
        if !self.0.busy.get() {
            self.launch(false);
        }
        self.arm_poll();
    }

    fn launch(&self, force: bool) {
        self.0.busy.set(true);
        let serial = self.0.serial.get();
        let frosted = self.0.frosted_enabled.get();
        let sizes = self.0.monitor_sizes.borrow().clone();
        let reader = self.0.reader.clone();
        let poster = self.0.poster.clone();
        let id = self.0.id;
        (self.0.worker)(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut reader = reader.lock().unwrap_or_else(|e| e.into_inner());
                reader.set_frosted_enabled(frosted);
                reader.set_monitor_sizes(&sizes);
                reader.read(force)
            })).unwrap_or_else(|panic| {
                let text = panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                PaletteResult::error(format!("Wallpaper color unavailable: {text}"))
            });
            poster(Box::new(move || {
                let inner = CONTROLLERS.with(|c| c.borrow().get(&id).and_then(Weak::upgrade));
                if let Some(inner) = inner {
                    WallpaperPalette(inner).completed(serial, result);
                }
            }));
        }));
    }

    fn completed(&self, serial: u64, result: PaletteResult) {
        self.0.busy.set(false);
        if self.0.closed.get() {
            return;
        }
        if self.0.enabled.get() && serial == self.0.serial.get() {
            self.publish(result);
        }
        if self.0.pending.get() && self.0.enabled.get() {
            self.0.pending.set(false);
            self.launch(true);
        }
    }

    fn publish(&self, result: PaletteResult) {
        let same = *self.0.colors.borrow() == result.colors && *self.0.sources.borrow() == result.sources
            && *self.0.frosts.borrow() == result.frosts && *self.0.error.borrow() == result.error;
        if same {
            return;
        }
        *self.0.colors.borrow_mut() = result.colors;
        *self.0.sources.borrow_mut() = result.sources;
        *self.0.frosts.borrow_mut() = result.frosts;
        *self.0.error.borrow_mut() = result.error;
        self.sync_monitors();
        if let Some(callback) = &self.0.on_changed {
            callback(self);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let id = self.id;
        let _ = CONTROLLERS.try_with(|c| c.borrow_mut().remove(&id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tessel_core::scheduler::FakeScheduler;

    fn root(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("d-wall-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn write_config(path: &Path, config: &Value) {
        std::fs::write(path, serde_json::to_vec(config).unwrap()).unwrap();
    }

    fn image(root: &Path, name: &str, color: u32) -> PathBuf {
        let path = root.join(name);
        let pixbuf = Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 120, 80).unwrap();
        pixbuf.fill(color);
        pixbuf.savev(&path, "png", &[]).unwrap();
        path
    }

    fn reader(config: &Path) -> PaletteReader {
        PaletteReader::new(config.to_owned(), None, false, None)
    }

    #[test]
    fn image_sampling_and_css_colors() {
        let root = root("sampling");
        assert_eq!(image_color(&image(&root, "purple.png", 0x8958C8FF)).unwrap(), Some([137, 88, 200]));
        assert_eq!(configured_color(Some(&json!("rgb(18,36,54)"))), Some([18, 36, 54]));
        assert_eq!(configured_color(Some(&json!("#ff0055"))), Some([255, 0, 85]));
        assert_eq!(configured_color(Some(&json!("invalid"))), None);
        assert_eq!(configured_color(Some(&json!("rgba(0,0,0,0)"))), None);
        assert_eq!(configured_color(Some(&json!(12))), None);
    }

    #[test]
    fn transparent_image_does_not_replace_theme() {
        let root = root("transparent");
        assert_eq!(image_color(&image(&root, "transparent.png", 0xA040D000)).unwrap(), None);
    }

    #[test]
    fn oversized_image_is_rejected_before_decoding() {
        let root = root("large");
        let path = root.join("large.png");
        std::fs::File::create(&path).unwrap().set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(image_color(&path).unwrap_err().contains("too large"));
    }

    #[test]
    fn most_frequent_color_wins_over_tiny_bright_accent() {
        let root = root("dominant");
        let pixbuf = Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 64, 64).unwrap();
        pixbuf.fill(0x8040A0FF);
        pixbuf.new_subpixbuf(0, 0, 1, 1).fill(0xFFFFFFFF);
        let path = root.join("dominant.png");
        pixbuf.savev(&path, "png", &[]).unwrap();
        assert_eq!(image_color(&path).unwrap(), Some([128, 64, 160]));
    }

    #[test]
    fn outputs_solid_colors_and_video_backgrounds() {
        let root = root("outputs");
        let path = image(&root, "red.png", 0xC05050FF);
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({
            "DP-1": {"file_path": path, "mode": "Fill"},
            "DP-2": {"file_path": path, "mode": "Color Only", "color": "#204060"},
            "HDMI-A-1": {"file_path": "/video/movie.mp4", "color": "rgb(50,40,30)"},
            "_view_prefs": {"hide_icons": true},
        }));
        let result = reader(&config).read(false);
        let expected: IndexMap<String, Rgb> = [("DP-1", [192, 80, 80]), ("DP-2", [32, 64, 96]), ("HDMI-A-1", [50, 40, 30])]
            .into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        assert_eq!(result.colors, expected);
        assert_eq!(result.sources["DP-1"], Some(path));
        assert_eq!(result.sources["DP-2"], None);
        assert!(result.error.unwrap().contains("HDMI-A-1:"));
    }

    #[test]
    fn video_and_animation_sample_actual_media_instead_of_background_color() {
        let Some(ffmpeg) = tessel_core::shell::which("ffmpeg") else { return };
        let root = root("video");
        for extension in ["mkv", "gif"] {
            let path = root.join(format!("animation.{extension}"));
            let mut command = std::process::Command::new(&ffmpeg);
            command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "color=c=red:s=128x64:r=10", "-t", "0.4"]);
            if extension == "mkv" {
                command.args(["-c:v", "ffv1"]);
            }
            assert!(command.arg(&path).stdin(std::process::Stdio::null()).status().unwrap().success());
            let config = root.join("wallpapers.json");
            write_config(&config, &json!({"DP-2": {"file_path": path, "color": "#0000ff"}}));
            let result = reader(&config).read(false);
            assert!(result.error.is_none(), "{:?}", result.error);
            let [red, green, blue] = result.colors["DP-2"];
            assert!(red > 240 && green < 15 && blue < 15);
            assert_eq!(result.sources["DP-2"], Some(path.clone()));
            let sampler = MediaSampler::default();
            let loader_sampler = sampler.clone();
            let cache = FrostedWallpaperCache::new(Some(root.join("frosted")), Some(Box::new(move |p, s| loader_sampler.frost_image(p, s))),
                                                   frosted::MAX_CACHE_FILES, frosted::MAX_CACHE_BYTES);
            let mut frosted_reader = PaletteReader::new(config.clone(), None, true, Some(Box::new(cache)));
            let texture = PathBuf::from(frosted_reader.read(false).frosts["DP-2"].clone());
            assert!(texture.is_file());
            assert!(std::fs::metadata(&texture).unwrap().len() < (frosted::MAX_DIMENSION * frosted::MAX_DIMENSION * 3) as u64);
            let pixbuf = Pixbuf::from_file(&texture).unwrap();
            assert_eq!(pixbuf.width(), 2 * pixbuf.height());
            assert!(pixbuf.width().max(pixbuf.height()) as u32 <= frosted::MAX_DIMENSION);
            assert_eq!(frosted_reader.read(false).frosts["DP-2"], texture.to_string_lossy());
        }
    }

    fn fake_sampler(outcome: impl Fn() -> Result<RunOutcome, String> + Send + Sync + 'static,
                    calls: Arc<Mutex<Vec<Vec<String>>>>) -> MediaSampler {
        MediaSampler {
            find_program: Arc::new(|_| Some(PathBuf::from("/usr/bin/ffmpeg"))),
            run: Arc::new(move |argv, timeout| {
                assert_eq!(timeout, Duration::from_secs(6));
                calls.lock().unwrap().push(argv.to_vec());
                outcome()
            }),
        }
    }

    #[test]
    fn video_timeout_falls_back_and_retains_selected_source() {
        let root = root("timeout");
        let path = root.join("video.mp4");
        std::fs::write(&path, b"a local video").unwrap();
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-2": {"file_path": path, "color": "#8040c0"}}));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sampler = fake_sampler(|| Ok(RunOutcome::TimedOut), calls.clone());
        let result = PaletteReader::new(config, Some(Box::new(move |p| sampler.media_color(p))), false, None).read(false);
        assert_eq!(result.colors.get("DP-2"), Some(&[128, 64, 192]));
        assert_eq!(result.sources.get("DP-2"), Some(&Some(path)));
        assert!(result.error.unwrap().contains("timed out"));
        let argv = calls.lock().unwrap()[0].clone();
        assert_eq!(argv[argv.iter().position(|a| a == "-protocol_whitelist").unwrap() + 1], "file");
        assert_eq!(argv[argv.iter().position(|a| a == "-f").unwrap() + 1], "mov");
    }

    #[test]
    fn failed_or_incomplete_video_decode_rejected() {
        let root = root("failed");
        let path = root.join("video.webm");
        std::fs::write(&path, b"not a video").unwrap();
        for (code, output) in [(1, b"".to_vec()), (0, b"partial".to_vec())] {
            let sampler = fake_sampler(move || Ok(RunOutcome::Finished { code, stdout: output.clone() }), Arc::default());
            assert!(sampler.video_color(&path).unwrap_err().contains("Could not sample"));
        }
        let sampler = MediaSampler { find_program: Arc::new(|_| None), run: Arc::new(|_, _| panic!("never runs")) };
        assert!(sampler.video_color(&path).unwrap_err().contains("requires ffmpeg"));
    }

    #[test]
    fn unchanged_files_cached_but_replacements_and_config_changes_detected() {
        let root = root("cached");
        let path = image(&root, "wall.png", 0xFF0000FF);
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-2": {"file_path": path}}));
        let sampled = Arc::new(Mutex::new(0));
        let counter = sampled.clone();
        let mut reader = PaletteReader::new(config.clone(), Some(Box::new(move |p| { *counter.lock().unwrap() += 1; image_color(p) })), false, None);
        assert_eq!(reader.read(false).colors["DP-2"], [255, 0, 0]);
        reader.read(false);
        assert_eq!(*sampled.lock().unwrap(), 1);
        std::fs::rename(image(&root, "replacement.png", 0x0000FFFF), &path).unwrap();
        assert_eq!(reader.read(false).colors["DP-2"], [0, 0, 255]);
        assert_eq!(*sampled.lock().unwrap(), 2);
        write_config(&config, &json!({"HDMI-A-1": {"file_path": path}}));
        let colors = reader.read(false).colors;
        assert_eq!(colors.len(), 1);
        assert_eq!(colors["HDMI-A-1"], [0, 0, 255]);
    }

    #[test]
    fn clones_share_one_image_sample() {
        let root = root("clones");
        let path = image(&root, "shared.png", 0x123456FF);
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-2": {"file_path": path}, "HDMI-A-1": {"file_path": path}}));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let seen = samples.clone();
        let mut reader = PaletteReader::new(config, Some(Box::new(move |p| { seen.lock().unwrap().push(p.to_owned()); Ok(Some([18, 52, 86])) })), false, None);
        assert_eq!(reader.read(false).colors.len(), 2);
        assert_eq!(*samples.lock().unwrap(), [path]);
    }

    struct CountingCache(FrostedWallpaperCache, Arc<Mutex<Vec<(PathBuf, Option<(i64, i64)>, String, Rgb)>>>);

    impl FrostCache for CountingCache {
        fn texture(&mut self, path: &Path, size: Option<(i64, i64)>, mode: &str, color: Rgb) -> Result<String, String> {
            self.1.lock().unwrap().push((path.to_owned(), size, mode.to_owned(), color));
            self.0.texture(path, size, mode, color)
        }
        fn protect(&mut self, paths: &HashSet<String>) {
            self.0.protect(paths)
        }
    }

    struct FailingCache(Arc<Mutex<usize>>);

    impl FrostCache for FailingCache {
        fn texture(&mut self, _: &Path, _: Option<(i64, i64)>, _: &str, _: Rgb) -> Result<String, String> {
            *self.0.lock().unwrap() += 1;
            Err("Pillow is unavailable".into())
        }
        fn protect(&mut self, _: &HashSet<String>) {}
    }

    fn counting_sampler() -> (Sample, Arc<Mutex<usize>>) {
        let count = Arc::new(Mutex::new(0));
        let counter = count.clone();
        (Box::new(move |p: &Path| { *counter.lock().unwrap() += 1; image_color(p) }), count)
    }

    #[test]
    fn frosted_outputs_share_texture_and_follow_live_file_changes() {
        let root = root("frostshare");
        let red = image(&root, "red.png", 0xFF0000FF);
        let blue = image(&root, "blue.png", 0x0000FFFF);
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-1": {"file_path": red}, "DP-2": {"file_path": blue}, "DP-3": {"file_path": red},
                                      "DP-4": {"file_path": red, "mode": "Color Only", "color": "#333333"}}));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cache = CountingCache(FrostedWallpaperCache::with_directory(root.join("frosted")), calls.clone());
        let (sample, samples) = counting_sampler();
        let mut reader = PaletteReader::new(config, Some(sample), false, Some(Box::new(cache)));
        assert!(reader.read(false).frosts.is_empty());
        assert!(calls.lock().unwrap().is_empty());
        reader.set_frosted_enabled(true);
        let result = reader.read(false);
        assert_eq!(result.frosts.keys().cloned().collect::<HashSet<_>>(), ["DP-1", "DP-2", "DP-3"].map(String::from).into_iter().collect());
        assert_eq!(result.frosts["DP-1"], result.frosts["DP-3"]);
        assert_ne!(result.frosts["DP-1"], result.frosts["DP-2"]);
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(*samples.lock().unwrap(), 2);
        reader.read(true);
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(*samples.lock().unwrap(), 2);
        std::fs::rename(image(&root, "replacement.png", 0x00FF00FF), &red).unwrap();
        let updated = reader.read(false);
        assert_ne!(updated.frosts["DP-1"], result.frosts["DP-1"]);
        assert_eq!(updated.frosts["DP-2"], result.frosts["DP-2"]);
        assert_eq!(calls.lock().unwrap().len(), 3);
        reader.set_frosted_enabled(false);
        assert!(reader.read(false).frosts.is_empty());
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn frosted_failure_falls_back_without_discarding_color_or_retrying_forever() {
        let root = root("frostfail");
        let path = image(&root, "red.png", 0xFF0000FF);
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-1": {"file_path": path}, "DP-2": {"file_path": root.join("missing.png")}}));
        let count = Arc::new(Mutex::new(0));
        let mut reader = PaletteReader::new(config, None, true, Some(Box::new(FailingCache(count.clone()))));
        let result = reader.read(false);
        assert_eq!(result.colors.len(), 1);
        assert_eq!(result.colors["DP-1"], [255, 0, 0]);
        assert!(result.frosts.is_empty());
        assert!(result.error.unwrap().contains("Frosted background unavailable"));
        reader.read(true);
        assert_eq!(*count.lock().unwrap(), 2);
    }

    struct FixedCache(String, Arc<Mutex<Vec<(PathBuf, Option<(i64, i64)>, String, Rgb)>>>);

    impl FrostCache for FixedCache {
        fn texture(&mut self, path: &Path, size: Option<(i64, i64)>, mode: &str, color: Rgb) -> Result<String, String> {
            self.1.lock().unwrap().push((path.to_owned(), size, mode.to_owned(), color));
            Ok(self.0.clone())
        }
        fn protect(&mut self, _: &HashSet<String>) {}
    }

    #[test]
    fn config_only_change_reuses_existing_color_and_frost() {
        let root = root("configonly");
        let path = image(&root, "wall.png", 0xFF0000FF);
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-1": {"file_path": path}}));
        let texture = image(&root, "frost.png", 0xFF0000FF);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let seen = samples.clone();
        let mut reader = PaletteReader::new(config.clone(), Some(Box::new(move |p| { seen.lock().unwrap().push(p.to_owned()); Ok(Some([255, 0, 0])) })),
                                            true, Some(Box::new(FixedCache(texture.to_string_lossy().into_owned(), calls.clone()))));
        let first = reader.read(false);
        write_config(&config, &json!({"DP-2": {"file_path": path}, "_view_prefs": {"hide_icons": true}}));
        let result = reader.read(true);
        assert_eq!(result.frosts.len(), 1);
        assert_eq!(result.frosts["DP-2"], first.frosts["DP-1"]);
        assert_eq!(*samples.lock().unwrap(), [path.clone()]);
        assert_eq!(*calls.lock().unwrap(), [(path, None, "Fill".to_owned(), [112, 112, 112])]);
    }

    #[test]
    fn removed_cache_file_or_directory_regenerates_unchanged_wallpaper() {
        let root = root("removedcache");
        let path = image(&root, "wall.png", 0xFF0000FF);
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-1": {"file_path": path}}));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cache = CountingCache(FrostedWallpaperCache::with_directory(root.join("frosted")), calls.clone());
        let (sample, samples) = counting_sampler();
        let mut reader = PaletteReader::new(config, Some(sample), true, Some(Box::new(cache)));
        let texture = PathBuf::from(reader.read(false).frosts["DP-1"].clone());
        std::fs::remove_file(&texture).unwrap();
        assert_eq!(reader.read(false).frosts["DP-1"], texture.to_string_lossy());
        assert!(texture.is_file());
        std::fs::remove_dir_all(root.join("frosted")).unwrap();
        assert_eq!(reader.read(false).frosts["DP-1"], texture.to_string_lossy());
        assert!(texture.is_file());
        assert_eq!(calls.lock().unwrap().len(), 3);
        assert_eq!(*samples.lock().unwrap(), 1);
    }

    #[test]
    fn monitor_geometry_and_fit_changes_update_only_affected_frost_texture() {
        let root = root("geometry");
        let path = image(&root, "wall.png", 0xFF0000FF);
        let config = root.join("wallpapers.json");
        let mut choices = json!({"DP-1": {"file_path": path, "mode": "Fit", "color": "#000000"},
                                 "DP-2": {"file_path": path, "mode": "Fit", "color": "#000000"}});
        write_config(&config, &choices);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cache = CountingCache(FrostedWallpaperCache::with_directory(root.join("frosted")), calls.clone());
        let (sample, samples) = counting_sampler();
        let mut reader = PaletteReader::new(config.clone(), Some(sample), true, Some(Box::new(cache)));
        reader.set_monitor_sizes(&[("DP-1".to_owned(), (1920, 1080)), ("DP-2".to_owned(), (1080, 1920))].into_iter().collect());
        let first = reader.read(false);
        let size = |p: &str| { let b = Pixbuf::from_file(p).unwrap(); (b.width(), b.height()) };
        assert_eq!(size(&first.frosts["DP-1"]), (1024, 576));
        assert_eq!(size(&first.frosts["DP-2"]), (576, 1024));
        reader.set_monitor_sizes(&[("DP-1".to_owned(), (1920, 1080)), ("DP-2".to_owned(), (1600, 1200))].into_iter().collect());
        let changed = reader.read(false);
        assert_eq!(changed.frosts["DP-1"], first.frosts["DP-1"]);
        assert_ne!(changed.frosts["DP-2"], first.frosts["DP-2"]);
        assert_eq!(calls.lock().unwrap().len(), 3);
        assert_eq!(*samples.lock().unwrap(), 1);
        choices["DP-1"]["mode"] = json!("Fill");
        write_config(&config, &choices);
        assert_ne!(reader.read(false).frosts["DP-1"], changed.frosts["DP-1"]);
        assert_eq!(calls.lock().unwrap().len(), 4);
    }

    #[test]
    fn missing_or_invalid_images_fall_back_per_output() {
        let root = root("invalid");
        let invalid = root.join("invalid.png");
        std::fs::write(&invalid, "this is not an image").unwrap();
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-1": {"file_path": invalid}, "DP-2": {"file_path": root.join("missing.png")},
                                      "DP-3": {"color": "#abcdef"}}));
        let result = reader(&config).read(false);
        assert_eq!(result.colors.len(), 1);
        assert_eq!(result.colors["DP-3"], [171, 205, 239]);
        assert_eq!(result.sources["DP-1"], Some(invalid));
        assert_eq!(result.sources["DP-2"], Some(root.join("missing.png")));
        let error = result.error.unwrap();
        assert!(error.contains("DP-1:") && error.contains("DP-2:"));
    }

    #[test]
    fn config_errors_clear_previous_color_and_are_bounded() {
        let root = root("configerrors");
        let config = root.join("wallpapers.json");
        write_config(&config, &json!({"DP-2": {"color": "red"}}));
        let mut reader = reader(&config);
        assert!(!reader.read(false).colors.is_empty());
        for content in ["broken".to_owned(), "[]".to_owned(), " ".repeat(MAX_CONFIG_BYTES + 1)] {
            std::fs::write(&config, content).unwrap();
            let result = reader.read(false);
            assert!(result.colors.is_empty());
            assert!(result.error.is_some());
        }
        std::fs::remove_file(&config).unwrap();
        assert!(reader.read(false).colors.is_empty());
    }

    type Queue = Arc<Mutex<Vec<Work>>>;

    struct Controller {
        palette: WallpaperPalette,
        scheduler: Rc<FakeScheduler>,
        work: Rc<RefCell<Vec<Work>>>,
        idles: Queue,
        changes: Rc<Cell<usize>>,
        config: PathBuf,
        root: PathBuf,
    }

    impl Controller {
        fn new(name: &str) -> Self {
            let root = root(name);
            let config = root.join("wallpapers.json");
            write_config(&config, &json!({"DP-2": {"color": "#8040c0"}}));
            let scheduler = Rc::new(FakeScheduler::default());
            let work: Rc<RefCell<Vec<Work>>> = Rc::default();
            let idles: Queue = Arc::default();
            let changes = Rc::new(Cell::new(0));
            let (queue, posted, counter) = (work.clone(), idles.clone(), changes.clone());
            let palette = WallpaperPalette::new(Some(Box::new(move |_| counter.set(counter.get() + 1))), WallpaperOptions {
                scheduler: Some(scheduler.clone()), worker: Some(Box::new(move |w| queue.borrow_mut().push(w))),
                poster: Some(Arc::new(move |w| posted.lock().unwrap().push(w))),
                reader: Some(Box::new(PaletteReader::new(config.clone(), None, false, None))), config_path: None,
            });
            Self { palette, scheduler, work, idles, changes, config, root }
        }

        fn finish_work(&self) {
            let work = self.work.borrow_mut().remove(0);
            work();
            loop {
                let next = { let mut idles = self.idles.lock().unwrap(); if idles.is_empty() { None } else { Some(idles.remove(0)) } };
                match next {
                    Some(idle) => idle(),
                    None => break,
                }
            }
        }

        fn use_frost_cache(&self) {
            let directory = self.root.join("frosted");
            self.palette.with_reader(|reader| reader.set_frost_cache(Box::new(FrostedWallpaperCache::with_directory(directory))));
        }
    }

    #[test]
    fn disabled_does_no_work_then_publishes_per_output_on_main_callback() {
        let c = Controller::new("ctl-disabled");
        c.palette.start();
        assert!(c.work.borrow().is_empty());
        assert!(c.scheduler.ids().is_empty());
        c.palette.set_enabled(true);
        assert_eq!(c.work.borrow().len(), 1);
        assert_eq!(c.palette.color(), None);
        c.finish_work();
        assert_eq!(c.palette.color_for("DP-2"), Some([128, 64, 192]));
        assert_eq!(c.palette.color_for("unknown"), None);
        assert_eq!(c.changes.get(), 1);
        c.palette.set_enabled(false);
        assert_eq!(c.palette.color(), None);
        assert!(c.scheduler.ids().is_empty());
        c.palette.close();
    }

    #[test]
    fn rapid_toggle_cannot_publish_old_worker_result() {
        let c = Controller::new("ctl-toggle");
        c.palette.start();
        c.palette.set_enabled(true);
        c.palette.set_enabled(false);
        c.palette.set_enabled(true);
        c.finish_work();
        assert_eq!(c.palette.color(), None);
        assert_eq!(c.work.borrow().len(), 1);
        write_config(&c.config, &json!({"DP-2": {"color": "#00ff00"}}));
        c.finish_work();
        assert_eq!(c.palette.color(), Some([0, 255, 0]));
        assert_eq!(c.changes.get(), 1);
        c.palette.close();
    }

    #[test]
    fn refresh_supersedes_running_work_and_close_ignores_completion() {
        let c = Controller::new("ctl-refresh");
        c.palette.set_enabled(true);
        c.palette.start();
        c.palette.refresh();
        c.finish_work();
        assert_eq!(c.palette.color(), None);
        c.palette.close();
        c.finish_work();
        assert_eq!(c.changes.get(), 0);
        assert!(c.scheduler.ids().is_empty());
    }

    #[test]
    fn frosted_toggle_drops_old_worker_and_clears_disabled_texture_immediately() {
        let c = Controller::new("ctl-frost");
        let path = image(&c.root, "wall.png", 0xFF0000FF);
        write_config(&c.config, &json!({"DP-2": {"file_path": path}}));
        c.use_frost_cache();
        c.palette.set_frosted_enabled(true);
        c.palette.start();
        assert!(c.work.borrow().is_empty());
        c.palette.set_enabled(true);
        c.palette.set_frosted_enabled(false);
        c.finish_work();
        assert_eq!(c.palette.frost_for("DP-2"), None);
        c.finish_work();
        assert_eq!(c.palette.color_for("DP-2"), Some([255, 0, 0]));
        c.palette.set_frosted_enabled(true);
        c.finish_work();
        assert!(Path::new(&c.palette.frost_for("DP-2").unwrap()).is_file());
        assert_eq!(c.palette.frost_for("unknown"), None);
        c.palette.set_frosted_enabled(false);
        assert_eq!(c.palette.frost_for("DP-2"), None);
        assert_eq!(c.palette.color_for("DP-2"), Some([255, 0, 0]));
        c.palette.set_enabled(false);
        c.finish_work();
        assert!(c.palette.frosts().is_empty());
        assert!(c.palette.colors().is_empty());
        c.palette.close();
    }

    #[test]
    fn monitor_geometry_change_supersedes_inflight_texture() {
        let c = Controller::new("ctl-geometry");
        let path = image(&c.root, "wall.png", 0xFF0000FF);
        write_config(&c.config, &json!({"DP-2": {"file_path": path}}));
        c.use_frost_cache();
        c.palette.set_monitor_sizes(&[("DP-2".to_owned(), (1920, 1080))].into_iter().collect());
        c.palette.set_frosted_enabled(true);
        c.palette.set_enabled(true);
        c.palette.start();
        c.palette.set_monitor_sizes(&[("DP-2".to_owned(), (1080, 1920))].into_iter().collect());
        c.finish_work();
        assert_eq!(c.palette.frost_for("DP-2"), None);
        c.finish_work();
        let texture = Pixbuf::from_file(c.palette.frost_for("DP-2").unwrap()).unwrap();
        assert_eq!((texture.width(), texture.height()), (576, 1024));
        c.palette.close();
    }

    struct FixedReader;
    impl WallpaperReader for FixedReader {
        fn read(&mut self, _force: bool) -> PaletteResult {
            let mut result = PaletteResult::default();
            result.colors.insert("DP-2".into(), [40, 50, 60]);
            result.sources.insert("DP-2".into(), None);
            result.error = Some(format!("{:?}", std::thread::current().id()));
            result
        }
    }

    #[test]
    fn real_worker_runs_off_main_thread_and_callback_returns_to_main() {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let main = std::thread::current().id();
            let callbacks = Rc::new(RefCell::new(Vec::new()));
            let seen = callbacks.clone();
            let palette = WallpaperPalette::new(Some(Box::new(move |_| seen.borrow_mut().push(std::thread::current().id()))),
                                                WallpaperOptions { reader: Some(Box::new(FixedReader)), ..Default::default() });
            palette.set_enabled(true);
            palette.start();
            assert!(tessel_core::timer::wait_for(&context, Duration::from_secs(2), || !callbacks.borrow().is_empty()));
            assert_eq!(*callbacks.borrow(), [main]);
            assert_ne!(palette.error().unwrap(), format!("{main:?}"), "read ran on the worker thread");
            palette.close();
        }).unwrap();
    }

    struct JsonReader(PaletteReader);
    impl WallpaperReader for JsonReader {
        fn read(&mut self, force: bool) -> PaletteResult { self.0.read(force) }
        fn config_path(&self) -> Option<PathBuf> { Some(self.0.config_path.clone()) }
    }

    fn json_sample() -> Sample {
        Box::new(|path: &Path| {
            let text = std::fs::read_to_string(path).map_err(|e| fsx::os_error_path(&e, path))?;
            let value: Vec<u8> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            Ok(Some([value[0], value[1], value[2]]))
        })
    }

    fn watching(name: &str, test: impl FnOnce(&glib::MainContext, &WallpaperPalette, &Path, &Path, &Rc<Cell<usize>>)) {
        let root = root(name);
        let config = root.join("tessel-shell/wallpapers.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let changes = Rc::new(Cell::new(0));
            let counter = changes.clone();
            let reader = JsonReader(PaletteReader::new(config.clone(), Some(json_sample()), false, None));
            let palette = WallpaperPalette::new(Some(Box::new(move |_| counter.set(counter.get() + 1))),
                                                WallpaperOptions { reader: Some(Box::new(reader)), ..Default::default() });
            test(&context, &palette, &root, &config, &changes);
            palette.close();
        }).unwrap();
    }

    fn write_choices(config: &Path, choices: &Value) {
        let temporary = config.with_extension("new");
        std::fs::write(&temporary, serde_json::to_vec(choices).unwrap()).unwrap();
        std::fs::rename(temporary, config).unwrap();
    }

    fn start(context: &glib::MainContext, palette: &WallpaperPalette, changes: &Rc<Cell<usize>>) {
        palette.set_enabled(true);
        palette.start();
        assert!(tessel_core::timer::wait_for(context, Duration::from_millis(1500), || changes.get() > 0));
        // Disable polling to prove updates come from filesystem notifications.
        palette.cancel_timer();
    }

    fn wait(context: &glib::MainContext, predicate: impl FnMut() -> bool) -> bool {
        tessel_core::timer::wait_for(context, Duration::from_millis(1500), predicate)
    }

    #[test]
    fn tessel_shell_atomic_saves_follow_each_connector_without_polling() {
        watching("watch-atomic", |context, palette, _root, config, changes| {
            write_choices(config, &json!({"DP-2": {"color": "red"}, "HDMI-A-1": {"color": "blue"}, "DP-old": {"color": "green"}}));
            start(context, palette, changes);
            assert_eq!(palette.color_for("DP-2"), Some([255, 0, 0]));
            assert_eq!(palette.color_for("HDMI-A-1"), Some([0, 0, 255]));
            assert_eq!(palette.color_for("unknown"), None);
            for (color, expected) in [("#654321", [101, 67, 33]), ("#bbaa33", [187, 170, 51])] {
                write_choices(config, &json!({"DP-2": {"color": color}, "HDMI-A-1": {"color": "blue"}}));
                assert!(wait(context, || palette.color_for("DP-2") == Some(expected)));
                assert_eq!(palette.color_for("HDMI-A-1"), Some([0, 0, 255]));
            }
        });
    }

    #[test]
    fn selected_media_replacement_changes_color_without_polling() {
        watching("watch-media", |context, palette, root, config, changes| {
            let media = root.join("pictures/wallpaper.png");
            std::fs::create_dir_all(media.parent().unwrap()).unwrap();
            std::fs::write(&media, "[120, 30, 80]").unwrap();
            write_choices(config, &json!({"DP-2": {"file_path": media}}));
            start(context, palette, changes);
            assert_eq!(palette.color_for("DP-2"), Some([120, 30, 80]));
            let replacement = media.with_extension("new");
            std::fs::write(&replacement, "[30, 160, 80]").unwrap();
            std::fs::rename(&replacement, &media).unwrap();
            assert!(wait(context, || palette.color_for("DP-2") == Some([30, 160, 80])));
            std::thread::sleep(Duration::from_millis(20));
            std::fs::write(&media, "[80, 40, 190]").unwrap();
            assert!(wait(context, || palette.color_for("DP-2") == Some([80, 40, 190])));
            std::fs::remove_file(&media).unwrap();
            assert!(wait(context, || palette.color_for("DP-2").is_none()));
            assert_eq!(palette.sources()["DP-2"], Some(media.clone()));
            std::fs::write(&media, "[12, 140, 70]").unwrap();
            assert!(wait(context, || palette.color_for("DP-2") == Some([12, 140, 70])));
        });
    }

    #[test]
    fn new_wallpaper_in_another_directory_replaces_old_watch() {
        watching("watch-move", |context, palette, root, config, changes| {
            let (first, second) = (root.join("one/wallpaper.png"), root.join("two/wallpaper.png"));
            for (path, color) in [(&first, "[11, 22, 33]"), (&second, "[44, 55, 66]")] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, color).unwrap();
            }
            write_choices(config, &json!({"DP-2": {"file_path": first}}));
            start(context, palette, changes);
            write_choices(config, &json!({"DP-2": {"file_path": second}}));
            assert!(wait(context, || palette.color_for("DP-2") == Some([44, 55, 66])));
            assert!(!palette.watched_directories().contains(&first.parent().unwrap().to_owned()));
            std::thread::sleep(Duration::from_millis(20));
            std::fs::write(&second, "[77, 88, 99]").unwrap();
            assert!(wait(context, || palette.color_for("DP-2") == Some([77, 88, 99])));
        });
    }

    #[test]
    fn disable_and_close_cancel_watches_and_queued_refresh() {
        watching("watch-close", |context, palette, _root, config, changes| {
            write_choices(config, &json!({"DP-2": {"color": "red"}}));
            start(context, palette, changes);
            assert!(!palette.watched_directories().is_empty());
            write_choices(config, &json!({"DP-2": {"color": "blue"}}));
            assert!(wait(context, || palette.event_timer_pending()));
            palette.set_enabled(false);
            assert!(palette.watched_directories().is_empty());
            assert!(!palette.event_timer_pending());
            assert_eq!(palette.color_for("DP-2"), None);
            palette.set_enabled(true);
            assert!(wait(context, || palette.color_for("DP-2") == Some([0, 0, 255])));
            palette.close();
            assert!(palette.watched_directories().is_empty());
            assert!(!palette.event_timer_pending());
        });
    }
}
