//! Helpers missing from tessel-core/tessel-gtk (upstream candidates):
//! - `lcms`: LittleCMS through dlopen (Pillow's ImageCms: embedded profile → sRGB, sRGB profile bytes),
//! - `Pool`: a bounded worker pool with cancellable, not-yet-started jobs whose results arrive
//!   on the GLib main loop (Python's `ThreadPoolExecutor` + `GLib.idle_add`).

use futures_channel::oneshot;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub mod lcms {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    type OpenMem = unsafe extern "C" fn(*const c_void, u32) -> *mut c_void;
    type CreateSrgb = unsafe extern "C" fn() -> *mut c_void;
    type CreateTransform = unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32, u32, u32) -> *mut c_void;
    type DoTransform = unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void, u32);
    type Delete = unsafe extern "C" fn(*mut c_void);
    type Close = unsafe extern "C" fn(*mut c_void) -> i32;
    type SaveMem = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut u32) -> i32;

    struct Api {
        open: OpenMem,
        srgb: CreateSrgb,
        create: CreateTransform,
        apply: DoTransform,
        delete: Delete,
        close: Close,
        save: SaveMem,
    }

    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    /// `TYPE_RGBA_8`.
    const TYPE_RGBA_8: u32 = (4 << 16) | (1 << 7) | (3 << 3) | 1;
    const INTENT_PERCEPTUAL: u32 = 0;
    const COPY_ALPHA: u32 = 0x0400_0000;

    fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            let library = libc::dlopen(c"liblcms2.so.2".as_ptr(), libc::RTLD_NOW);
            if library.is_null() {
                return None;
            }
            let symbol = |name: &std::ffi::CStr| {
                let pointer = libc::dlsym(library, name.as_ptr());
                (!pointer.is_null()).then_some(pointer)
            };
            Some(Api {
                open: std::mem::transmute::<*mut c_void, OpenMem>(symbol(c"cmsOpenProfileFromMem")?),
                srgb: std::mem::transmute::<*mut c_void, CreateSrgb>(symbol(c"cmsCreate_sRGBProfile")?),
                create: std::mem::transmute::<*mut c_void, CreateTransform>(symbol(c"cmsCreateTransform")?),
                apply: std::mem::transmute::<*mut c_void, DoTransform>(symbol(c"cmsDoTransform")?),
                delete: std::mem::transmute::<*mut c_void, Delete>(symbol(c"cmsDeleteTransform")?),
                close: std::mem::transmute::<*mut c_void, Close>(symbol(c"cmsCloseProfile")?),
                save: std::mem::transmute::<*mut c_void, SaveMem>(symbol(c"cmsSaveProfileToMem")?),
            })
        }).as_ref()
    }

    /// `ImageCmsProfile(createProfile('sRGB')).tobytes()`.
    pub fn srgb_profile() -> Option<Vec<u8>> {
        static BYTES: OnceLock<Option<Vec<u8>>> = OnceLock::new();
        BYTES.get_or_init(|| unsafe {
            let api = api()?;
            let profile = (api.srgb)();
            if profile.is_null() {
                return None;
            }
            let mut size = 0u32;
            let mut bytes = None;
            if (api.save)(profile, std::ptr::null_mut(), &mut size) != 0 && size > 0 {
                let mut buffer = vec![0u8; size as usize];
                if (api.save)(profile, buffer.as_mut_ptr().cast(), &mut size) != 0 {
                    buffer.truncate(size as usize);
                    bytes = Some(buffer);
                }
            }
            (api.close)(profile);
            bytes
        }).clone()
    }

    /// Convert straight-alpha RGBA8 pixels from `profile` to sRGB in place
    /// (`profileToProfile(..., outputMode='RGBA')`, perceptual intent, alpha kept).
    pub fn to_srgb(profile: &[u8], pixels: &mut [u8]) -> Result<(), String> {
        let api = api().ok_or("LittleCMS is not available")?;
        unsafe {
            let source = (api.open)(profile.as_ptr().cast(), profile.len() as u32);
            if source.is_null() {
                return Err("cannot open profile from string".into());
            }
            let target = (api.srgb)();
            let transform = (api.create)(source, TYPE_RGBA_8, target, TYPE_RGBA_8, INTENT_PERCEPTUAL, COPY_ALPHA);
            (api.close)(source);
            (api.close)(target);
            if transform.is_null() {
                return Err("cannot build transform".into());
            }
            for row in pixels.chunks_mut(4 * 4096) {
                let count = (row.len() / 4) as u32;
                let input = row.to_vec();
                (api.apply)(transform, input.as_ptr().cast(), row.as_mut_ptr().cast(), count);
            }
            (api.delete)(transform);
        }
        Ok(())
    }
}

/// A queued job's cancellation handle (`Future.cancel()`: only before it starts).
#[derive(Clone, Default)]
pub struct Ticket {
    cancelled: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
}

impl Ticket {
    pub fn cancel(&self) -> bool {
        if self.started.load(Ordering::SeqCst) {
            return false;
        }
        self.cancelled.store(true, Ordering::SeqCst);
        true
    }
    pub fn cancelled(&self) -> bool { self.cancelled.load(Ordering::SeqCst) }
}

type Job = Box<dyn FnOnce() + Send>;

struct State {
    jobs: VecDeque<Job>,
    threads: usize,
    idle: usize,
    shutdown: bool,
}

struct PoolInner {
    name: String,
    max: usize,
    state: Mutex<State>,
    ready: Condvar,
}

/// Threads are started lazily, like `ThreadPoolExecutor`.
#[derive(Clone)]
pub struct Pool(Arc<PoolInner>);

impl Pool {
    pub fn new(name: &str, max: usize) -> Self {
        Self(Arc::new(PoolInner {
            name: name.into(), max: max.max(1),
            state: Mutex::new(State { jobs: VecDeque::new(), threads: 0, idle: 0, shutdown: false }), ready: Condvar::new(),
        }))
    }

    /// Queue `work`; the receiver yields its result, or `Canceled` if it never ran.
    pub fn submit<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static) -> (Ticket, oneshot::Receiver<T>) {
        let (sender, receiver) = oneshot::channel();
        let ticket = Ticket::default();
        let job_ticket = ticket.clone();
        let job: Job = Box::new(move || {
            if job_ticket.cancelled.load(Ordering::SeqCst) {
                return;
            }
            job_ticket.started.store(true, Ordering::SeqCst);
            let _ = sender.send(work());
        });
        let mut state = self.0.state.lock().unwrap();
        if state.shutdown {
            ticket.cancelled.store(true, Ordering::SeqCst);
            return (ticket, receiver);
        }
        state.jobs.push_back(job);
        if state.idle == 0 && state.threads < self.0.max {
            state.threads += 1;
            let inner = self.0.clone();
            let _ = std::thread::Builder::new().name(format!("{}_{}", inner.name, state.threads - 1)).spawn(move || worker(inner));
        } else {
            self.0.ready.notify_one();
        }
        (ticket, receiver)
    }

    /// `shutdown(wait=False, cancel_futures=True)`.
    pub fn shutdown(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.shutdown = true;
        state.jobs.clear();
        self.0.ready.notify_all();
    }
}

fn worker(inner: Arc<PoolInner>) {
    loop {
        let job = {
            let mut state = inner.state.lock().unwrap();
            loop {
                if let Some(job) = state.jobs.pop_front() {
                    break Some(job);
                }
                if state.shutdown {
                    break None;
                }
                state.idle += 1;
                state = inner.ready.wait(state).unwrap();
                state.idle -= 1;
            }
        };
        match job {
            Some(job) => job(),
            None => {
                inner.state.lock().unwrap().threads -= 1;
                return;
            }
        }
    }
}

/// Python float repr for EXIF rationals (`str(IFDRational)`).
pub fn float_repr(value: f64) -> String { tessel_core::pyfmt::float_repr(value) }

/// Python `bytes` repr (`b'…'`).
pub fn bytes_repr(bytes: &[u8]) -> String {
    let mut out = String::from("b'");
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push('\'');
    out
}

