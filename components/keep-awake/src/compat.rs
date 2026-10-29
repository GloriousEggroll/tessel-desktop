//! Helpers missing from suite-core / glib 0.22 (candidates to upstream).

use glib::ffi::{gboolean, gpointer, GDestroyNotify, GSourceFunc};
use glib::translate::ToGlibPtr;
use std::os::raw::{c_int, c_uint};

unsafe extern "C" {
    fn g_unix_signal_add_full(priority: c_int, signum: c_int, handler: GSourceFunc, user_data: gpointer,
                              notify: GDestroyNotify) -> c_uint;
}

type Handler = Box<dyn FnMut() -> glib::ControlFlow + 'static>;

unsafe extern "C" fn trampoline(data: gpointer) -> gboolean {
    let handler = unsafe { &mut *(data as *mut Handler) };
    match handler() {
        glib::ControlFlow::Continue => glib::ffi::GTRUE,
        glib::ControlFlow::Break => glib::ffi::GFALSE,
    }
}

unsafe extern "C" fn destroy(data: gpointer) {
    drop(unsafe { Box::from_raw(data as *mut Handler) });
}

/// `GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signum, handler)` on the default
/// main context (glib 0.22 moved this to the separate glib-unix crate).
/// Returns the raw source id; remove it with [`source_remove`].
pub fn unix_signal_add_local(signum: i32, handler: impl FnMut() -> glib::ControlFlow + 'static) -> u32 {
    let boxed: Box<Handler> = Box::new(Box::new(handler));
    unsafe {
        g_unix_signal_add_full(glib::ffi::G_PRIORITY_DEFAULT, signum, Some(trampoline),
                               Box::into_raw(boxed) as gpointer, Some(destroy))
    }
}

/// `GLib.source_remove(id)` only if the source still exists (Python's
/// `find_source_by_id` guard).
pub fn source_remove(id: u32) {
    unsafe {
        let source = glib::ffi::g_main_context_find_source_by_id(std::ptr::null_mut(), id);
        if !source.is_null() {
            glib::ffi::g_source_destroy(source);
        }
    }
}

/// PyGObject's `str(GLib.Error)`: `"<domain>: <message> (<code>)"`.
pub fn python_error_text(error: &glib::Error) -> String {
    let raw: *const glib::ffi::GError = error.to_glib_none().0;
    let code = unsafe { (*raw).code };
    format!("{}: {} ({code})", error.domain().as_str(), error.message())
}

/// Python's `str(OSError)`: `"[Errno N] Description"`.
pub fn python_os_error_text(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let text = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
            format!("[Errno {code}] {text}")
        }
        None => error.to_string(),
    }
}

/// `time.time()`.
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
