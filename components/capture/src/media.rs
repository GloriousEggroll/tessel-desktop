//! Bounded portal frames, explicit colour conversion and FFmpeg encoding
//! (capture_media.py). Capture uses only a descriptor and node supplied by the
//! ScreenCast portal; there is no unrestricted PipeWire or compositor fallback.

use crate::native::{Feed, FeedError, StreamProps};
use crate::process::{self, RunError};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tessel_core::tr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub transfer: String,
    pub primaries: String,
    pub depth: u32,
    pub pixel_format: String,
}

impl Frame {
    /// Python defaults: sRGB transfer, BT.709, 8-bit, `rgba64le`.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self { width, height, pixels, transfer: "iec61966-2-1".into(), primaries: "bt709".into(), depth: 8, pixel_format: "rgba64le".into() }
    }

    pub fn with(mut self, transfer: &str, primaries: &str, depth: u32) -> Self {
        self.transfer = transfer.into();
        self.primaries = primaries.into();
        self.depth = depth;
        self
    }

    pub fn format(mut self, pixel_format: &str) -> Self {
        self.pixel_format = pixel_format.into();
        self
    }

    pub fn hdr(&self) -> bool {
        matches!(self.transfer.as_str(), "smpte2084" | "arib-std-b67")
    }

    pub fn signature(&self) -> (u32, u32, &str, &str, u32, &str) {
        (self.width, self.height, &self.transfer, &self.primaries, self.depth, &self.pixel_format)
    }
}

pub type Crop = (i64, i64, i64, i64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingOptions {
    pub codec: String,
    pub hdr: bool,
    pub fps: i64,
    pub quality: i64,
    /// Mbit/s; zero selects constant quality.
    pub bitrate: i64,
    pub encoder: String,
    pub container: String,
    pub audio: Vec<String>,
    /// Source peak used for SDR tone mapping, in nits.
    pub peak: i64,
    pub crop: Option<Crop>,
    pub duration: i64,
}

impl Default for RecordingOptions {
    fn default() -> Self {
        Self { codec: "h264".into(), hdr: false, fps: 30, quality: 23, bitrate: 0, encoder: "auto".into(), container: "mkv".into(),
               audio: Vec::new(), peak: 1000, crop: None, duration: 0 }
    }
}

impl RecordingOptions {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.codec.as_str(), "h264" | "h265" | "av1") || !matches!(self.container.as_str(), "mkv" | "mp4") {
            return Err(tr!("Unsupported codec or container."));
        }
        if self.hdr && self.codec == "h264" {
            return Err(tr!("HDR recording requires H.265 or AV1."));
        }
        if !(1..=120).contains(&self.fps) || !(0..=200).contains(&self.bitrate) || !(10..=40).contains(&self.quality) {
            return Err(tr!("Invalid frame rate, bitrate or quality."));
        }
        if !matches!(self.encoder.as_str(), "auto" | "cpu" | "vulkan") || !(100..=10000).contains(&self.peak) {
            return Err(tr!("Invalid encoder or HDR peak brightness."));
        }
        if self.audio.len() > 2 || self.audio.iter().any(|x| x.is_empty() || x.contains('\0')) {
            return Err(tr!("Choose at most two valid audio sources."));
        }
        if !(0..=86400).contains(&self.duration) {
            return Err(tr!("The recording timer must be between 0 and 24 hours."));
        }
        Ok(())
    }
}

pub fn crop_filter(crop: Option<Crop>, frame: &Frame) -> Result<Vec<String>, String> {
    let Some((x, y, width, height)) = crop else { return Ok(Vec::new()) };
    if x.min(y) < 0 || width.min(height) < 2 || x + width > frame.width as i64 || y + height > frame.height as i64 {
        return Err(tr!("The selected area is outside the captured image."));
    }
    Ok(vec![format!("crop={width}:{height}:{x}:{y}")])
}

/// Python `f'{value:g}'` for the small positive peak ratios used here.
fn g(value: f64) -> String {
    let text = format!("{value:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() { "0".into() } else { text.into() }
}

pub fn color_filter(frame: &Frame, hdr: bool, screenshot: bool, peak: i64, crop: Option<Crop>) -> Result<String, String> {
    let mut filters = crop_filter(crop, frame)?;
    if hdr && (!frame.hdr() || frame.depth < 10 || frame.primaries != "bt2020") {
        return Err(tr!("This portal source does not expose 10-bit BT.2020 HDR. Choose an HDR screen, or record in SDR. Window streams may not carry HDR metadata."));
    }
    let mut base = format!("zscale=pin={}:tin={}:min=gbr:rin=full", frame.primaries, frame.transfer);
    if frame.hdr() && !hdr {
        filters.push(format!("{base}:p={}:t=linear:m=gbr:r=full:npl=100", frame.primaries));
        filters.push("format=gbrpf32le".into());
        filters.push("zscale=p=bt709".into());
        filters.push(format!("tonemap=tonemap=mobius:param=0.3:desat=2:peak={}", g(peak as f64 / 100.0)));
        base = "zscale=pin=bt709:tin=linear:min=gbr:rin=full".into();
    }
    if screenshot {
        filters.push(format!("{base}:p=bt709:t=iec61966-2-1:m=gbr:r=full"));
        filters.push("format=rgb24".into());
    } else if hdr {
        filters.push(format!("{base}:p=bt2020:t=smpte2084:m=bt2020nc:r=limited"));
        filters.push("pad=ceil(iw/2)*2:ceil(ih/2)*2".into());
        filters.push("format=yuv420p10le".into());
    } else {
        filters.push(format!("{base}:p=bt709:t=bt709:m=bt709:r=limited"));
        filters.push("pad=ceil(iw/2)*2:ceil(ih/2)*2".into());
        filters.push("format=yuv420p".into());
    }
    Ok(filters.join(","))
}

pub fn raw_input(frame: &Frame, fps: i64) -> Vec<String> {
    ["-f", "rawvideo", "-pixel_format", &frame.pixel_format, "-video_size", &format!("{}x{}", frame.width, frame.height),
     "-framerate", &fps.to_string(), "-i", "pipe:0"].iter().map(|s| s.to_string()).collect()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Python `bytes.decode(errors='replace')[-n:]` (last n characters).
fn tail_chars(data: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(data);
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// One SDR sRGB PNG (with the sRGB ICC profile) of a portal frame.
pub fn screenshot(frame: &Frame, crop: Option<Crop>, peak: i64) -> Result<Vec<u8>, String> {
    let mut args = strings(&["ffmpeg", "-hide_banner", "-loglevel", "error", "-threads", "2", "-filter_threads", "2"]);
    args.extend(raw_input(frame, 1));
    args.push("-vf".into());
    args.push(color_filter(frame, false, true, peak, crop)?);
    args.extend(strings(&["-frames:v", "1", "-c:v", "png", "-threads", "2", "-f", "image2pipe", "pipe:1"]));
    let result = process::run(&args, &frame.pixels, Duration::from_secs(45)).map_err(|e| e.to_string())?;
    if result.status != 0 {
        return Err(tr!("Screenshot conversion failed: {error}", error = tail_chars(&result.stderr, 1500)));
    }
    let image = crate::imaging::decode_rgb(&result.stdout)?;
    crate::imaging::png_rgb_with_icc(&image)
}

/// `open(path, 'xb')` + chmod 0600: never overwrite.
pub fn save_exclusive(path: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    file.write_all(data)?;
    Ok(path.to_path_buf())
}

/// Compose independently tone-mapped outputs in portal logical coordinates.
pub fn compose_screens(items: Vec<(Vec<u8>, StreamProps)>) -> Result<Vec<u8>, String> {
    if items.len() == 1 {
        return Ok(items.into_iter().next().unwrap().0);
    }
    let mut bounds = Vec::new();
    for (_data, properties) in &items {
        let (Some((x, y)), Some((w, h))) = (properties.position, properties.size) else {
            return Err(tr!("The portal did not provide screen positions. Capture the screens separately."));
        };
        bounds.push((x, y, w, h));
    }
    let unique: std::collections::HashSet<_> = bounds.iter().collect();
    if unique.len() != bounds.len() {
        return Err(tr!("The same screen was selected more than once."));
    }
    let left = bounds.iter().map(|b| b.0).min().unwrap_or(0);
    let top = bounds.iter().map(|b| b.1).min().unwrap_or(0);
    let width = bounds.iter().map(|b| b.0 + b.2).max().unwrap_or(0) - left;
    let height = bounds.iter().map(|b| b.1 + b.3).max().unwrap_or(0) - top;
    if width * height > 100_000_000 || width.min(height) < 1 {
        return Err(tr!("The combined workspace is too large to capture."));
    }
    let mut canvas = image::RgbImage::new(width as u32, height as u32);
    for ((data, _), (x, y, w, h)) in items.iter().zip(&bounds) {
        if *w < 1 || *h < 1 {
            return Err("height and width must be > 0".into());
        }
        let picture = crate::imaging::decode_rgb(data)?;
        let resized = crate::imaging::resize_lanczos(&picture, *w as u32, *h as u32);
        // Image.paste clips to the canvas.
        image::imageops::replace(&mut canvas, &resized, x - left, y - top);
    }
    crate::imaging::png_rgb_with_icc(&canvas)
}

pub type Arguments = (Vec<String>, Vec<String>, Vec<String>);

pub fn encoder_arguments(options: &RecordingOptions, kind: &str, device: Option<u32>) -> Result<Arguments, String> {
    let codec = match options.codec.as_str() { "h264" => "h264", "h265" => "hevc", _ => "av1" };
    let (mut before, mut filters) = (Vec::new(), Vec::new());
    let mut args: Vec<String>;
    if kind == "cpu" {
        let encoder = match options.codec.as_str() { "h264" => "libopenh264", "h265" => "libx265", _ => "libsvtav1" };
        args = strings(&["-c:v", encoder]);
        if options.codec == "h264" {
            // OpenH264 has its own rate control; with no bitrate use the chosen constant QP.
            args.extend(strings(&["-profile:v", "high", "-rc_mode", if options.bitrate != 0 { "bitrate" } else { "off" }]));
            if options.bitrate == 0 {
                args.extend(["-qmin".to_string(), options.quality.to_string(), "-qmax".into(), options.quality.to_string()]);
            }
        } else {
            args.extend(strings(&["-preset", if options.codec == "av1" { "8" } else { "fast" }]));
            if options.bitrate == 0 {
                args.extend(["-crf".to_string(), options.quality.to_string()]);
            }
        }
        if options.codec == "h265" {
            args.extend(strings(&["-x265-params", "pools=4:frame-threads=2:log-level=error"]));
        }
        if options.codec == "av1" {
            args.extend(strings(&["-svtav1-params", "lp=4"]));
        }
    } else if kind == "vulkan" {
        before = vec!["-init_hw_device".into(), format!("vulkan=capture{}", device.map(|d| format!(":{d}")).unwrap_or_default()),
                      "-filter_hw_device".into(), "capture".into()];
        filters = vec![if options.hdr { "format=p010le" } else { "format=nv12" }.to_string(), "hwupload".into()];
        args = vec!["-c:v".into(), format!("{codec}_vulkan"), "-usage".into(), "record".into(), "-content".into(), "desktop".into()];
        if options.bitrate == 0 {
            args.extend(["-rc_mode".to_string(), "cqp".into(), "-qp".into(), options.quality.to_string()]);
        }
    } else {
        return Err(tr!("Unknown encoder."));
    }
    if options.bitrate != 0 {
        args.extend(["-b:v".to_string(), format!("{}M", options.bitrate), "-maxrate".into(), format!("{}M", options.bitrate),
                     "-bufsize".into(), format!("{}M", options.bitrate * 2)]);
    }
    Ok((before, filters, args))
}

pub type Encoder = (String, Option<u32>);

pub fn recording_command(frame: &Frame, options: &RecordingOptions, encoder: &Encoder, destination: &str) -> Result<Vec<String>, String> {
    options.validate()?;
    let (before, extra, encoding) = encoder_arguments(options, &encoder.0, encoder.1)?;
    let filters = color_filter(frame, options.hdr, false, options.peak, options.crop)?;
    let mut args = strings(&["ffmpeg", "-hide_banner", "-loglevel", "warning", "-n", "-threads", "4", "-filter_threads", "2"]);
    args.extend(before);
    args.extend(strings(&["-use_wallclock_as_timestamps", "1"]));
    args.extend(raw_input(frame, options.fps));
    for source in &options.audio {
        args.extend(strings(&["-thread_queue_size", "128", "-f", "pulse", "-i", source]));
    }
    args.extend(strings(&["-map", "0:v:0"]));
    if options.audio.len() == 1 {
        args.extend(strings(&["-map", "1:a:0", "-af", "aresample=async=1:first_pts=0"]));
    } else if options.audio.len() == 2 {
        args.extend(strings(&["-filter_complex", "[1:a][2:a]amix=inputs=2:duration=longest,aresample=async=1:first_pts=0[a]", "-map", "[a]"]));
    }
    let mut vf = vec![filters];
    vf.extend(extra);
    args.push("-vf".into());
    args.push(vf.join(","));
    args.extend(encoding);
    args.extend(["-threads".to_string(), "4".into(), "-r".into(), options.fps.to_string(), "-fps_mode".into(), "cfr".into(),
                 "-g".into(), (options.fps * 2).to_string()]);
    if options.hdr {
        args.extend(strings(&["-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc", "-color_range", "tv"]));
    } else {
        args.extend(strings(&["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]));
    }
    if !options.audio.is_empty() {
        args.extend(strings(&["-c:a", if options.container == "mp4" { "aac" } else { "libopus" }, "-b:a", "192k", "-shortest"]));
    }
    if options.codec == "h265" && options.container == "mp4" {
        args.extend(strings(&["-tag:v", "hvc1"]));
    }
    if options.container == "mp4" {
        args.extend(strings(&["-movflags", "+faststart"]));
    }
    if options.duration != 0 {
        args.extend(["-t".to_string(), options.duration.to_string()]);
    }
    args.push(destination.into());
    Ok(args)
}

/// Exercise the real crop, size, FPS, colour conversion and encoder settings
/// (audio and duration disabled for this one-frame test).
pub fn encoder_probe_command(frame: &Frame, options: &RecordingOptions, encoder: &Encoder) -> Result<Vec<String>, String> {
    let mut probe = options.clone();
    probe.audio.clear();
    probe.duration = 0;
    let mut command = recording_command(frame, &probe, encoder, "-")?;
    command.pop();
    command.extend(strings(&["-frames:v", "1", "-f", "null", "-"]));
    Ok(command)
}

/// Encoder probe runner (injectable for tests): args, stdin, timeout.
pub type ProbeRunner<'a> = &'a dyn Fn(&[String], &[u8], Duration) -> Result<process::Output, RunError>;
/// Vulkan encoding device enumeration (injectable for tests).
pub type DeviceLister<'a> = &'a dyn Fn(&str) -> Result<Vec<u32>, String>;

pub fn choose_encoder(options: &RecordingOptions, frame: &Frame) -> Result<Encoder, String> {
    choose_encoder_with(options, frame, &crate::vulkan::encoding_devices, &process::run)
}

/// Test the installed FFmpeg encoders, retaining actionable failure details.
pub fn choose_encoder_with(options: &RecordingOptions, frame: &Frame, devices: DeviceLister, run: ProbeRunner) -> Result<Encoder, String> {
    options.validate()?;
    if tessel_core::shell::which("ffmpeg").is_none() {
        return Err(tr!("FFmpeg is not installed. Install FFmpeg with the requested video encoder."));
    }
    let mut candidates: Vec<Encoder> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    if matches!(options.encoder.as_str(), "auto" | "vulkan") {
        match devices(&options.codec) {
            Ok(list) => {
                if list.is_empty() {
                    let name = match options.codec.as_str() { "h264" => "H.264", "h265" => "H.265", _ => "AV1" };
                    failures.push(tr!("No GPU driver exposes Vulkan Video encoding for {codec}. Vulkan graphics or video decoding support alone is insufficient.", codec = name));
                }
                candidates.extend(list.into_iter().map(|index| ("vulkan".to_string(), Some(index))));
            }
            Err(error) => {
                failures.push(error);
                // Let FFmpeg provide its own diagnostic if enumeration failed.
                candidates.push(("vulkan".into(), None));
            }
        }
    }
    if matches!(options.encoder.as_str(), "auto" | "cpu") {
        candidates.push(("cpu".into(), None));
    }
    for (kind, device) in candidates {
        let encoder = (kind.clone(), device);
        let command = encoder_probe_command(frame, options, &encoder)?;
        let (_, _, arguments) = encoder_arguments(options, &kind, device)?;
        let name = arguments[arguments.iter().position(|a| a == "-c:v").unwrap() + 1].clone();
        let label = name + &device.map(|d| format!(" (GPU {d})")).unwrap_or_default();
        match run(&command, &frame.pixels, Duration::from_secs(15)) {
            Ok(result) if result.status == 0 => return Ok(encoder),
            Ok(result) => {
                // Keep the initial cause, not just FFmpeg's cascading EOF errors.
                let detail = String::from_utf8_lossy(&result.stderr).trim().to_string();
                let detail: String = detail.chars().take(1800).collect();
                failures.push(format!("{label}: {}", if detail.is_empty() {
                    tr!("FFmpeg exited with status {status}.", status = result.status as i64)
                } else { detail }));
            }
            Err(RunError::Timeout(_)) => failures.push(format!("{label}: {}", tr!("FFmpeg encoder test timed out after 15 seconds."))),
            Err(RunError::Os(error)) => failures.push(format!("{label}: {error}")),
        }
    }
    let hint = if options.codec == "h264" && matches!(options.encoder.as_str(), "auto" | "cpu") {
        format!(" {}", tr!("H.264 software recording requires FFmpeg with libopenh264."))
    } else { String::new() };
    Err(format!("{}{hint}\n\n{}", tr!("FFmpeg could not encode these recording settings."), failures.join("\n\n")))
}

/// Recorder events, delivered from the worker thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordEvent {
    /// Encoder kind (`cpu` / `vulkan`).
    Recording(String),
    Saved(String),
    Cancelled,
    Error(String),
}

/// `threading.Event` with `wait(timeout)`.
#[derive(Default)]
pub struct StopFlag {
    set: Mutex<bool>,
    changed: Condvar,
    fast: AtomicBool,
}

impl StopFlag {
    pub fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.fast.store(true, Ordering::SeqCst);
        self.changed.notify_all();
    }

    pub fn is_set(&self) -> bool {
        self.fast.load(Ordering::SeqCst)
    }

    pub fn wait(&self, timeout: Duration) {
        let guard = self.set.lock().unwrap();
        let _ = self.changed.wait_timeout_while(guard, timeout, |set| !*set);
    }
}

/// Worker-owned encoder with bounded buffers and cancellable pipe writes.
pub struct Recorder {
    pub stopped: Arc<StopFlag>,
    pub thread: Option<std::thread::JoinHandle<()>>,
}

type Notify = Box<dyn Fn(RecordEvent) + Send + Sync>;

impl Recorder {
    pub fn new(feed: Arc<dyn Feed>, options: RecordingOptions, destination: PathBuf, notify: Notify) -> Self {
        Self::with_encoder_chooser(feed, options, destination, notify, Box::new(choose_encoder))
    }

    pub fn with_encoder_chooser(feed: Arc<dyn Feed>, options: RecordingOptions, destination: PathBuf, notify: Notify,
                                chooser: Box<dyn Fn(&RecordingOptions, &Frame) -> Result<Encoder, String> + Send>) -> Self {
        let stopped = Arc::new(StopFlag::default());
        let flag = stopped.clone();
        let thread = std::thread::Builder::new().name("screen-recording".into())
            .spawn(move || run_recorder(feed, options, destination, notify, flag, chooser)).expect("recording thread");
        Self { stopped, thread: Some(thread) }
    }

    pub fn stop(&self) {
        self.stopped.set();
    }

    /// Test helper: wait for the worker (Python `thread.join(timeout)`).
    pub fn join(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while let Some(thread) = &self.thread {
            if thread.is_finished() {
                let _ = self.thread.take().unwrap().join();
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

fn feed_error(error: FeedError) -> String {
    match error {
        FeedError::Timeout(text) | FeedError::Failed(text) => text,
    }
}

/// `tempfile.mkstemp(prefix, suffix, dir)` → path (file created 0600).
pub fn mkstemp(directory: &Path, prefix: &str, suffix: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let template = directory.join(format!("{prefix}XXXXXXXX{suffix}"));
    let mut bytes = template.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    let fd = unsafe { libc::mkstemps(bytes.as_mut_ptr() as *mut libc::c_char, suffix.len() as libc::c_int) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe { libc::close(fd) };
    bytes.pop();
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

/// An unlinked temporary file in `tempfile.gettempdir()` (Python TemporaryFile).
fn temporary_file() -> std::io::Result<std::fs::File> {
    let path = mkstemp(&std::env::temp_dir(), "tmp", "")?;
    let file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(file)
}

fn write_some(fd: i32, data: &[u8]) -> std::io::Result<usize> {
    let written = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
    if written < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(0);
        }
        return Err(error);
    }
    Ok(written as usize)
}

fn writable(fd: i32, timeout: Duration) -> bool {
    let mut poll = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as libc::c_int) > 0 }
}

fn run_recorder(feed: Arc<dyn Feed>, options: RecordingOptions, destination: PathBuf, notify: Notify, stopped: Arc<StopFlag>,
                chooser: Box<dyn Fn(&RecordingOptions, &Frame) -> Result<Encoder, String> + Send>) {
    use std::os::unix::process::CommandExt;
    let mut temporary: Option<PathBuf> = None;
    let mut child: Option<std::process::Child> = None;
    let result = (|| -> Result<(), String> {
        let frame = feed.frame(5.0, None).map_err(feed_error)?;
        let encoder = chooser(&options, &frame)?;
        if stopped.is_set() {
            notify(RecordEvent::Cancelled);
            return Ok(());
        }
        let mut frame = feed.frame(5.0, None).map_err(feed_error)?;
        let parent = destination.parent().map(Path::to_path_buf).unwrap_or_default();
        std::fs::create_dir_all(&parent).map_err(|e| tessel_core::pyerr::os_error_path(&e, &parent))?;
        let name = mkstemp(&parent, ".recording-", &format!(".{}", options.container)).map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
        temporary = Some(name.clone());
        // ffmpeg -n creates it; private output mode inherited from the umask below.
        let _ = std::fs::remove_file(&name);
        let mut log = temporary_file().map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
        let command = recording_command(&frame, &options, &encoder, &name.to_string_lossy())?;
        let mut process = std::process::Command::new(&command[0]);
        process.args(&command[1..]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null())
            .stderr(log.try_clone().map_err(|e| tessel_core::pyerr::os_error_text(&e))?);
        unsafe {
            process.pre_exec(|| {
                libc::umask(0o077);
                Ok(())
            });
        }
        let spawned = process.spawn().map_err(|e| process::spawn_error(&e, &command[0]))?;
        child = Some(spawned);
        let process = child.as_mut().unwrap();
        let stdin = process.stdin.take().unwrap();
        let fd = stdin.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let started = Instant::now();
        notify(RecordEvent::Recording(encoder.0.clone()));
        let mut due = started;
        let interval = Duration::from_secs_f64(1.0 / options.fps as f64);
        while !stopped.is_set() {
            if options.duration != 0 && started.elapsed() >= Duration::from_secs(options.duration as u64) {
                break;
            }
            if process.try_wait().map(|s| s.is_some()).unwrap_or(true) {
                break;
            }
            match feed.frame(0.01, None) {
                Ok(new) => {
                    if new.signature() != frame.signature() {
                        return Err(tr!("The source size or color mode changed. Recording was stopped to avoid incorrect colors."));
                    }
                    frame = new;
                }
                // Static sources may not repaint; keep their last authorized frame.
                Err(FeedError::Timeout(_)) => {}
                Err(FeedError::Failed(error)) => return Err(error),
            }
            let mut remaining: &[u8] = &frame.pixels;
            while !remaining.is_empty() && !stopped.is_set() {
                if writable(fd, Duration::from_millis(100)) {
                    let written = write_some(fd, remaining).map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
                    remaining = &remaining[written..];
                }
            }
            if !remaining.is_empty() {
                // Complete this frame before EOF; a partial raw frame is invalid.
                let deadline = Instant::now() + Duration::from_secs(5);
                while !remaining.is_empty() && Instant::now() < deadline {
                    if writable(fd, Duration::from_millis(100)) {
                        let written = write_some(fd, remaining).map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
                        remaining = &remaining[written..];
                    }
                }
            }
            due = (due + interval).max(Instant::now());
            stopped.wait(due.saturating_duration_since(Instant::now()));
        }
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            match process.try_wait() {
                Ok(Some(status)) => break process::code(status),
                Ok(None) if Instant::now() >= deadline => {
                    unsafe { libc::kill(process.id() as i32, libc::SIGTERM) };
                    return Err(tr!("The encoder did not finish. The partial recording was retained."));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => return Err(tessel_core::pyerr::os_error_text(&error)),
            }
        };
        let size = log.seek(SeekFrom::End(0)).unwrap_or(0);
        let _ = log.seek(SeekFrom::Start(size.saturating_sub(2500)));
        let mut tail = Vec::new();
        let _ = log.read_to_end(&mut tail);
        if status != 0 {
            return Err(tr!("The encoder stopped: {error}", error = String::from_utf8_lossy(&tail).into_owned()));
        }
        // Never overwrite an existing recording.
        std::fs::hard_link(&name, &destination).map_err(|e| {
            format!("{}: {} -> {}", tessel_core::pyerr::os_error_text(&e), crate::compat::py_repr(&name.to_string_lossy()),
                    crate::compat::py_repr(&destination.to_string_lossy()))
        })?;
        let _ = std::fs::remove_file(&name);
        temporary = None;
        notify(RecordEvent::Saved(destination.to_string_lossy().into_owned()));
        Ok(())
    })();
    if let Err(error) = result {
        if let Some(process) = child.as_mut() {
            if matches!(process.try_wait(), Ok(None)) {
                unsafe { libc::kill(process.id() as i32, libc::SIGINT) };
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if !matches!(process.try_wait(), Ok(None)) {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let _ = process.kill();
                        let _ = process.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        let mut detail = error;
        if let Some(path) = temporary.as_ref().filter(|p| p.exists()) {
            detail.push_str(&format!("\nPartial recording: {}", path.display()));
        }
        notify(RecordEvent::Error(detail));
    }
    if let Some(mut process) = child {
        let _ = process.try_wait();
    }
}

/// `pactl --format=json list sources` → [(name, description)]; any failure → [].
pub fn audio_sources() -> Vec<(String, String)> {
    let Ok(output) = process::check_output(&["pactl", "--format=json", "list", "sources"], Duration::from_secs(3)) else { return Vec::new() };
    parse_sources(&output)
}

pub fn parse_sources(output: &[u8]) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_slice::<serde_json::Value>(output) else { return Vec::new() };
    let mut result = Vec::new();
    for item in items {
        // Python raises KeyError (uncaught there) for an item without "name"; the list is dropped.
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else { return Vec::new() };
        let description = item.get("description").and_then(|v| v.as_str()).unwrap_or(name);
        result.push((name.to_string(), description.to_string()));
    }
    result
}

pub type Sources = Vec<(String, String)>;

/// Resolve quick presets against current defaults; never silently drop audio.
pub fn recording_audio(preset: Option<&str>, system: &str, microphone: &str) -> Result<(Sources, String, String), String> {
    recording_audio_with(preset, system, microphone, audio_sources(), &|args| {
        process::check_output(args, Duration::from_secs(3)).map(|o| String::from_utf8_lossy(&o).into_owned()).map_err(|_| ())
    })
}

pub fn recording_audio_with(preset: Option<&str>, system: &str, microphone: &str, sources: Sources,
                            command: &dyn Fn(&[&str]) -> Result<String, ()>) -> Result<(Sources, String, String), String> {
    let names: std::collections::HashSet<&str> = sources.iter().map(|(n, _)| n.as_str()).collect();
    let (mut system, mut microphone) = (system.to_string(), microphone.to_string());
    if let Some(preset) = preset {
        system.clear();
        microphone.clear();
        let unavailable = || tr!("The default audio device is unavailable. Check Audio Settings.");
        if matches!(preset, "desktop" | "desktop-mic") {
            let sink = command(&["pactl", "get-default-sink"]).map_err(|_| unavailable())?;
            system = format!("{}.monitor", sink.trim());
        }
        if preset == "desktop-mic" {
            microphone = command(&["pactl", "get-default-source"]).map_err(|_| unavailable())?.trim().to_string();
            if microphone.ends_with(".monitor") {
                return Err(tr!("The default input is desktop audio. Select a microphone as the default input in Audio Settings."));
            }
        }
    }
    for name in [&system, &microphone] {
        if !name.is_empty() && !names.contains(name.as_str()) {
            return Err(tr!("Audio device is unavailable: {name}", name = name.as_str()));
        }
    }
    Ok((sources, system, microphone))
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
