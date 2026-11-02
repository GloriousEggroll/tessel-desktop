//! Minimal FFI to gtk4-layer-shell's `GtkSessionLockInstance`
//! (ext-session-lock-v1), which the gtk4-layer-shell 0.8 crate does not bind.
//!
//! Upstream (wmww/gtk4-layer-shell `src/gtk4-session-lock.c`, read 2026-10):
//! the instance class has no dispose/finalize override, so dropping it never
//! unlocks; lock surfaces hold a ref to the instance; the only unlock paths are
//! `gtk_session_lock_instance_unlock()` and the compositor's `finished` event.
//! As defence in depth, [`SessionLock::new`] leaks one strong reference so the
//! instance can never be finalized, and this wrapper has no `Drop` impl.
//!
//! `unlock()` is crate-private and must only be called from the PAM-success
//! completions in `app.rs` (contracts/tessel-lock.md §2, §9).
use glib::translate::{from_glib, from_glib_full, ToGlibPtr};
use gtk::prelude::*;
use tessel_gtk::gtk;

// Resolved from libgtk4-layer-shell.so.0, which tessel-gtk links first.
extern "C" {
    fn gtk_session_lock_is_supported() -> glib::ffi::gboolean;
    fn gtk_session_lock_instance_new() -> *mut glib::gobject_ffi::GObject;
    fn gtk_session_lock_instance_lock(instance: *mut glib::gobject_ffi::GObject) -> glib::ffi::gboolean;
    fn gtk_session_lock_instance_unlock(instance: *mut glib::gobject_ffi::GObject);
    fn gtk_session_lock_instance_is_locked(instance: *mut glib::gobject_ffi::GObject) -> glib::ffi::gboolean;
    fn gtk_session_lock_instance_assign_window_to_monitor(instance: *mut glib::gobject_ffi::GObject,
                                                          window: *mut gtk::ffi::GtkWindow,
                                                          monitor: *mut gtk::gdk::ffi::GdkMonitor);
}

/// `Gtk4SessionLock.is_supported()` (needs an initialised GDK Wayland display).
pub fn is_supported() -> bool {
    // SAFETY: no arguments; reads the bound registry.
    unsafe { from_glib(gtk_session_lock_is_supported()) }
}

#[derive(Clone)]
pub struct SessionLock(glib::Object);

impl SessionLock {
    pub fn new() -> Self {
        // SAFETY: returns a new (full) reference to a GtkSessionLockInstance.
        let object: glib::Object = unsafe { from_glib_full(gtk_session_lock_instance_new()) };
        // Never finalize the instance (see module docs).
        std::mem::forget(object.clone());
        Self(object)
    }

    fn ptr(&self) -> *mut glib::gobject_ffi::GObject { self.0.as_ptr() }

    pub fn connect_monitor(&self, f: impl Fn(&Self, gtk::gdk::Monitor) + 'static) {
        let this = self.clone();
        self.0.connect_local("monitor", false, move |values| {
            if let Some(monitor) = values.get(1).and_then(|v| v.get::<gtk::gdk::Monitor>().ok()) {
                f(&this, monitor);
            }
            None
        });
    }

    pub fn connect_locked(&self, f: impl Fn() + 'static) { self.connect_plain("locked", f) }
    pub fn connect_failed(&self, f: impl Fn() + 'static) { self.connect_plain("failed", f) }
    pub fn connect_unlocked(&self, f: impl Fn() + 'static) { self.connect_plain("unlocked", f) }

    fn connect_plain(&self, signal: &str, f: impl Fn() + 'static) {
        self.0.connect_local(signal, false, move |_| { f(); None });
    }

    /// Request the lock; false on immediate failure (the `failed` signal is emitted too).
    pub fn lock(&self) -> bool {
        // SAFETY: valid instance.
        unsafe { from_glib(gtk_session_lock_instance_lock(self.ptr())) }
    }

    pub fn is_locked(&self) -> bool {
        // SAFETY: valid instance.
        unsafe { from_glib(gtk_session_lock_instance_is_locked(self.ptr())) }
    }

    pub fn assign_window_to_monitor(&self, window: &impl IsA<gtk::Window>, monitor: &gtk::gdk::Monitor) {
        let window: &gtk::Window = window.upcast_ref();
        // SAFETY: valid instance, window and monitor; the library takes its own refs.
        unsafe {
            gtk_session_lock_instance_assign_window_to_monitor(self.ptr(), window.to_glib_none().0,
                                                               monitor.to_glib_none().0);
        }
    }

    /// Release the compositor lock. ONLY for verified PAM success.
    pub(crate) fn unlock(&self) {
        // SAFETY: valid instance; a no-op unless locked.
        unsafe { gtk_session_lock_instance_unlock(self.ptr()) }
    }
}

impl Default for SessionLock {
    fn default() -> Self { Self::new() }
}
