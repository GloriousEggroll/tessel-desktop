//! Helpers missing from suite-core/suite-gtk (candidates to upstream; see PARITY.md).

pub mod fsutil;
pub mod yaml;

/// `Gio.DesktopAppInfo.get_boolean(key)` for an app from
/// `suite_core::glib_compat::desktop_app_info` (a GDesktopAppInfo).
pub fn desktop_boolean(info: &gio::AppInfo, key: &str) -> bool {
    use glib::prelude::*;
    use glib::translate::ToGlibPtr;
    extern "C" {
        fn g_desktop_app_info_get_boolean(info: *mut gio::ffi::GAppInfo, key: *const libc::c_char) -> glib::ffi::gboolean;
    }
    let name: &str = info.type_().name();
    if name != "GDesktopAppInfo" {
        return false;
    }
    // SAFETY: `info` is a GDesktopAppInfo (checked above); key is NUL-terminated.
    unsafe { g_desktop_app_info_get_boolean(info.to_glib_none().0, key.to_glib_none().0) != 0 }
}
