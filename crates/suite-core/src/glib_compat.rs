//! GLib/GIO entry points that gtk-rs 0.22 no longer exposes from `glib`/`gio`.
//!
//! * Unix signal and fd sources moved to the separate `glib-unix` crate; they
//!   are re-exported here so components need no direct dependency.
//! * `GDesktopAppInfo` moved to the GioUnix namespace and is not wrapped, but
//!   `g_desktop_app_info_new` is still exported by libgio-2.0.

use glib::translate::{from_glib_full, ToGlibPtr};

/// `GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signum, handler)` on the default
/// main context (acquires it; call from the main thread). Returns the source id.
pub use glib_unix::unix_signal_add_local;
/// `GLib.unix_fd_add_full(PRIORITY_DEFAULT, fd, condition, handler)` on the default main context.
pub use glib_unix::unix_fd_add_local;

/// `GLib.source_remove(id)` only if the source still exists on the default
/// context (Python's `find_source_by_id` guard): never warns for a source
/// that already finished.
pub fn source_remove(id: glib::SourceId) {
    unsafe {
        let source = glib::ffi::g_main_context_find_source_by_id(std::ptr::null_mut(), id.as_raw());
        if !source.is_null() {
            glib::ffi::g_source_destroy(source);
        }
    }
}

extern "C" {
    fn g_desktop_app_info_new(desktop_id: *const libc::c_char) -> *mut gio::ffi::GAppInfo;
}

/// `Gio.DesktopAppInfo.new(desktop_id)` (e.g. `"org.gnome.Nautilus.desktop"`),
/// or `None` when no such application is installed.
pub fn desktop_app_info(desktop_id: &str) -> Option<gio::AppInfo> {
    unsafe { from_glib_full(g_desktop_app_info_new(desktop_id.to_glib_none().0)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_desktop_id_is_none_and_stale_source_removal_is_quiet() {
        assert!(desktop_app_info("no-such-application-for-suite-core-tests.desktop").is_none());
        let context = glib::MainContext::default();
        let _guard = context.acquire().unwrap();
        let id = glib::idle_add_local_once(|| {});
        while context.iteration(false) {}
        source_remove(id);
    }
}
