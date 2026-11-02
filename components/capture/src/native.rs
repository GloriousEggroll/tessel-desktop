//! Bounded native PipeWire reader (capture_native.py); accepts only the portal's
//! restricted FD. `libcapture-pipewire.so` stays C and is loaded with dlopen.

use crate::compat::dl::Library;
use crate::media::Frame;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tessel_core::tr;

/// Portal stream properties used by capture (`a{sv}` from `Start`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamProps {
    pub position: Option<(i64, i64)>,
    pub size: Option<(i64, i64)>,
    pub source_type: Option<u32>,
}

impl StreamProps {
    pub fn from_variant(props: &glib::VariantDict) -> Self {
        let pair = |key: &str| props.lookup_value(key, None).and_then(|v| v.get::<(i32, i32)>()).map(|(a, b)| (a as i64, b as i64));
        Self { position: pair("position"), size: pair("size"), source_type: props.lookup::<u32>("source_type").ok().flatten() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedError {
    /// Python `TimeoutError` ("No new frames arrived from the portal.").
    Timeout(String),
    /// Python `RuntimeError`.
    Failed(String),
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedError::Timeout(t) | FeedError::Failed(t) => f.write_str(t),
        }
    }
}

/// A frame source opened from a portal PipeWire remote.
pub trait Feed: Send + Sync {
    /// Latest frame received at or after `not_before` (CLOCK_MONOTONIC seconds).
    fn frame(&self, timeout: f64, not_before: Option<f64>) -> Result<Frame, FeedError>;
    fn properties(&self) -> &StreamProps;
    fn close(&self);
}

/// Opens a feed from a fresh portal fd and node id (injectable for tests).
pub type FeedFactory = std::sync::Arc<dyn Fn(std::os::fd::OwnedFd, u32, StreamProps) -> Result<std::sync::Arc<dyn Feed>, String> + Send + Sync>;

#[repr(C)]
#[derive(Default)]
struct FrameInfo {
    width: u32,
    height: u32,
    size: u32,
    depth: u32,
    received: f64,
    pixel_format: [c_char; 24],
    transfer: [c_char; 24],
    primaries: [c_char; 24],
}

struct Api {
    open: unsafe extern "C" fn(c_int, u32, u32) -> *mut c_void,
    read: unsafe extern "C" fn(*mut c_void, *mut FrameInfo, *mut *mut c_void, *mut c_char, usize) -> c_int,
    close: unsafe extern "C" fn(*mut c_void),
    release: unsafe extern "C" fn(*mut c_void),
}

pub const LIBRARY: &str = "libcapture-pipewire.so";
pub const INSTALL_DIR: &str = "/usr/local/lib/tessel/tessel-capture";

/// Where the library is looked up: next to the executable, then the Python install dir.
pub fn library_candidates() -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Some(directory) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        result.push(directory.join(LIBRARY));
    }
    let installed = PathBuf::from(INSTALL_DIR).join(LIBRARY);
    if !result.contains(&installed) {
        result.push(installed);
    }
    result
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut last = String::new();
        for path in library_candidates() {
            if !path.exists() {
                if last.is_empty() {
                    last = format!("{}: cannot open shared object file: No such file or directory", path.display());
                }
                continue;
            }
            match Library::open(&path.to_string_lossy()) {
                Ok(library) => unsafe {
                    return Ok(Api {
                        open: library.symbol("capture_open")?,
                        read: library.symbol("capture_read")?,
                        close: library.symbol("capture_close")?,
                        release: library.symbol("capture_release")?,
                    });
                },
                Err(error) => last = error,
            }
        }
        Err(last)
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Python `from capture_native import library` (for `--check`).
pub fn library() -> Result<(), String> {
    api().map(|_| ())
}

fn text(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

struct Handle(*mut c_void);
// SAFETY: the C reader locks internally; access is serialized by the Mutex.
unsafe impl Send for Handle {}

pub struct NativeFeed {
    properties: StreamProps,
    handle: Mutex<Option<Handle>>,
}

impl NativeFeed {
    /// `capture_open(fd, node, fps)`; the library dups `fd`, the caller keeps it.
    pub fn open(fd: std::os::fd::BorrowedFd, node: u32, properties: StreamProps, fps: u32) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        let api = api()?;
        let handle = unsafe { (api.open)(fd.as_raw_fd(), node, fps) };
        if handle.is_null() {
            return Err(tr!("Could not open the portal PipeWire stream."));
        }
        Ok(Self { properties, handle: Mutex::new(Some(Handle(handle))) })
    }

    /// The default factory: open the stream, then close our copy of the fd.
    pub fn factory() -> FeedFactory {
        std::sync::Arc::new(|fd: std::os::fd::OwnedFd, node, props| {
            use std::os::fd::AsFd;
            let feed = NativeFeed::open(fd.as_fd(), node, props, 120)?;
            drop(fd);
            Ok(std::sync::Arc::new(feed) as std::sync::Arc<dyn Feed>)
        })
    }
}

impl Feed for NativeFeed {
    fn frame(&self, timeout: f64, not_before: Option<f64>) -> Result<Frame, FeedError> {
        let api = api().map_err(FeedError::Failed)?;
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        loop {
            let mut info = FrameInfo::default();
            let mut pixels: *mut c_void = std::ptr::null_mut();
            let mut error = [0 as c_char; 256];
            let status = {
                let guard = self.handle.lock().unwrap();
                let Some(handle) = guard.as_ref() else { return Err(FeedError::Failed(tr!("Capture has ended."))) };
                unsafe { (api.read)(handle.0, &mut info, &mut pixels, error.as_mut_ptr(), error.len()) }
            };
            if status < 0 {
                return Err(FeedError::Failed(unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy().into_owned()));
            }
            if status > 0 {
                let fresh = not_before.is_none_or(|limit| info.received >= limit);
                let frame = fresh.then(|| {
                    let data = unsafe { std::slice::from_raw_parts(pixels as *const u8, info.size as usize) }.to_vec();
                    Frame { width: info.width, height: info.height, pixels: data, transfer: text(&info.transfer),
                            primaries: text(&info.primaries), depth: info.depth, pixel_format: text(&info.pixel_format) }
                });
                unsafe { (api.release)(pixels) };
                if let Some(frame) = frame {
                    return Ok(frame);
                }
            }
            if Instant::now() >= deadline {
                return Err(FeedError::Timeout(tr!("No new frames arrived from the portal.")));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn properties(&self) -> &StreamProps {
        &self.properties
    }

    fn close(&self) {
        if let Some(handle) = self.handle.lock().unwrap().take() {
            if let Ok(api) = api() {
                unsafe { (api.close)(handle.0) };
            }
        }
    }
}

impl Drop for NativeFeed {
    fn drop(&mut self) {
        self.close();
    }
}
