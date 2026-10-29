//! `GioUnix.DesktopAppInfo` lookups. gio-rs 0.22 no longer wraps
//! GDesktopAppInfo (it moved to the GioUnix namespace), but the symbol is
//! still exported by libgio-2.0, so a two-function FFI is enough.

use gio::prelude::*;
use glib::translate::{from_glib_full, ToGlibPtr};

extern "C" {
    fn g_desktop_app_info_new(desktop_id: *const libc::c_char) -> *mut gio::ffi::GAppInfo;
}

/// The icon of `<name>` (a `.desktop` id), or `None` when not installed.
pub fn desktop_app_icon(name: &str) -> Option<gio::Icon> {
    let info: Option<gio::AppInfo> = unsafe { from_glib_full(g_desktop_app_info_new(name.to_glib_none().0)) };
    info?.icon()
}

// glib-rs 0.22 moved `unix_signal_add` out of `glib` (into glib-unix); the
// C function is still part of libglib-2.0.
extern "C" {
    fn g_unix_signal_add_full(priority: libc::c_int, signum: libc::c_int, handler: glib::ffi::GSourceFunc,
                              user_data: glib::ffi::gpointer, notify: glib::ffi::GDestroyNotify) -> libc::c_uint;
}

type SignalHandler = Box<dyn FnMut() -> glib::ControlFlow>;

unsafe extern "C" fn signal_trampoline(data: glib::ffi::gpointer) -> glib::ffi::gboolean {
    let handler = &mut *(data as *mut SignalHandler);
    match handler() {
        glib::ControlFlow::Continue => glib::ffi::GTRUE,
        glib::ControlFlow::Break => glib::ffi::GFALSE,
    }
}

unsafe extern "C" fn signal_destroy(data: glib::ffi::gpointer) {
    drop(Box::from_raw(data as *mut SignalHandler));
}

/// `GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signum, handler)` on the default main context.
pub fn unix_signal_add_local(signum: i32, handler: impl FnMut() -> glib::ControlFlow + 'static) -> glib::SourceId {
    let boxed: Box<SignalHandler> = Box::new(Box::new(handler));
    unsafe {
        let id = g_unix_signal_add_full(glib::ffi::G_PRIORITY_DEFAULT, signum, Some(signal_trampoline),
                                        Box::into_raw(boxed) as glib::ffi::gpointer, Some(signal_destroy));
        glib::translate::from_glib(id)
    }
}
