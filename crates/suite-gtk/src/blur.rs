//! Opt-in compositor blur for GTK4 surfaces using GTK's Wayland connection
//! (background_blur.py; the C bridge is compiled into this crate).
//!
//! No wallpaper sampling or background image fallback is used. On compositors
//! without ext-background-effect-v1, the regular transparent tint remains intact.

use gtk::glib;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void, CString};
use std::rc::{Rc, Weak};
use std::time::Duration;

extern "C" {
    fn waydock_blur_new() -> *mut c_void;
    fn waydock_blur_update(handle: *mut c_void, root: *mut gtk::ffi::GtkWidget, target_class: *const c_char,
                           enabled: c_int, include_root: c_int, include_popovers: c_int, corners: c_int, strength: c_int) -> c_int;
    fn waydock_blur_clear(handle: *mut c_void);
    fn waydock_blur_free(handle: *mut c_void);
    fn waydock_blur_geometry(handle: *mut c_void, index: u32, values: *mut c_int) -> c_int;
}

struct Inner {
    widget: glib::WeakRef<gtk::Widget>,
    target_class: RefCell<Option<String>>,
    include_root: Cell<bool>,
    include_popovers: bool,
    enabled: Cell<bool>,
    strength: Cell<u8>,
    rounded: Cell<bool>,
    closed: Cell<bool>,
    surface_count: Cell<i32>,
    handle: Cell<*mut c_void>,
    signals: RefCell<Vec<glib::SignalHandlerId>>,
}

impl Drop for Inner {
    // Freeing the native group prevents a discarded binding from leaving
    // surface/protocol references behind.
    fn drop(&mut self) {
        let handle = self.handle.replace(std::ptr::null_mut());
        if !handle.is_null() {
            unsafe { waydock_blur_free(handle) };
        }
    }
}

thread_local! {
    static BINDINGS: RefCell<Vec<Weak<Inner>>> = const { RefCell::new(Vec::new()) };
    static TIMER: Cell<bool> = const { Cell::new(false) };
}

/// One blur request group for a GTK native tree.
#[derive(Clone)]
pub struct BlurBinding(Rc<Inner>);

fn poll() -> glib::ControlFlow {
    let bindings: Vec<_> = BINDINGS.with(|b| {
        let mut b = b.borrow_mut();
        b.retain(|weak| weak.strong_count() > 0);
        b.iter().filter_map(Weak::upgrade).collect()
    });
    let mut active = false;
    for inner in bindings {
        let binding = BlurBinding(inner);
        if !binding.0.closed.get() && binding.0.enabled.get() && binding.0.widget.upgrade().is_some_and(|w| w.is_mapped()) {
            binding.refresh();
            active = true;
        }
    }
    if !active {
        TIMER.with(|t| t.set(false));
    }
    if active { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
}

impl BlurBinding {
    fn new(widget: &gtk::Widget, target_class: Option<&str>, include_root: bool, include_popovers: bool) -> Self {
        let inner = Rc::new(Inner {
            widget: widget.downgrade(), target_class: RefCell::new(target_class.map(str::to_owned)),
            include_root: Cell::new(include_root), include_popovers, enabled: Cell::new(false), strength: Cell::new(50),
            rounded: Cell::new(true), closed: Cell::new(false), surface_count: Cell::new(0),
            handle: Cell::new(unsafe { waydock_blur_new() }), signals: RefCell::new(Vec::new()),
        });
        let weak = Rc::downgrade(&inner);
        let mapped = widget.connect_map(move |_| {
            if let Some(inner) = weak.upgrade() {
                let binding = BlurBinding(inner);
                let (enabled, strength, rounded) = (binding.0.enabled.get(), binding.0.strength.get(), binding.0.rounded.get());
                binding.configure(enabled, strength, rounded, None, None);
            }
        });
        let unmapped = |weak: Weak<Inner>| move |_: &gtk::Widget| {
            if let Some(inner) = weak.upgrade() {
                BlurBinding(inner).clear();
            }
        };
        let signals = vec![mapped, widget.connect_unmap(unmapped(Rc::downgrade(&inner))),
                           widget.connect_unrealize(unmapped(Rc::downgrade(&inner)))];
        inner.signals.replace(signals);
        BINDINGS.with(|b| b.borrow_mut().push(Rc::downgrade(&inner)));
        Self(inner)
    }

    fn clear(&self) {
        let handle = self.0.handle.get();
        if !handle.is_null() {
            unsafe { waydock_blur_clear(handle) };
        }
        self.0.surface_count.set(0);
    }

    /// `strength` 0..=100 maps to 0..=40 px; `rounded` gives 12px corners.
    /// `target_class` (a panel CSS class, `Some("")` for none) and `include_root`
    /// replace the stored values when given.
    pub fn configure(&self, enabled: bool, strength: u8, rounded: bool, target_class: Option<&str>, include_root: Option<bool>) {
        if self.0.closed.get() {
            return;
        }
        self.0.enabled.set(enabled);
        self.0.strength.set(strength.min(100));
        self.0.rounded.set(rounded);
        if let Some(target) = target_class {
            self.0.target_class.replace(Some(target.to_owned()));
        }
        if let Some(include_root) = include_root {
            self.0.include_root.set(include_root);
        }
        self.refresh();
        if enabled && !TIMER.with(|t| t.get()) {
            // One timer for all surfaces, no perpetual frame callback. Native
            // tree traversal and changed-region detection happen in C.
            TIMER.with(|t| t.set(true));
            glib::timeout_add_local(Duration::from_millis(200), poll);
        }
    }

    pub fn refresh(&self) {
        let handle = self.0.handle.get();
        let Some(widget) = self.0.widget.upgrade() else { return };
        if self.0.closed.get() || handle.is_null() {
            return;
        }
        let target = self.0.target_class.borrow().clone().filter(|t| !t.is_empty()).and_then(|t| CString::new(t).ok());
        let pointer: *mut gtk::ffi::GtkWidget = widget.to_glib_none().0;
        let strength = crate::palette::round_half_even(self.0.strength.get() as f64 * 0.4) as c_int;
        let count = unsafe {
            waydock_blur_update(handle, pointer, target.as_ref().map_or(std::ptr::null(), |t| t.as_ptr()),
                                self.0.enabled.get() as c_int, self.0.include_root.get() as c_int,
                                self.0.include_popovers as c_int, if self.0.rounded.get() { 12 } else { 0 }, strength)
        };
        self.0.surface_count.set(count);
    }

    /// Number of surfaces currently given a blur region.
    pub fn surface_count(&self) -> i32 { self.0.surface_count.get() }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }

    /// Diagnostics: `[x, y, width, height, corners, strength]` of surface `index`.
    pub fn geometry(&self, index: u32) -> Option<[i32; 6]> {
        let mut values = [0 as c_int; 6];
        let handle = self.0.handle.get();
        (!handle.is_null() && unsafe { waydock_blur_geometry(handle, index, values.as_mut_ptr()) } != 0).then_some(values)
    }

    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        if let Some(widget) = self.0.widget.upgrade() {
            for signal in self.0.signals.take() {
                widget.disconnect(signal);
            }
        }
        let handle = self.0.handle.replace(std::ptr::null_mut());
        if !handle.is_null() {
            unsafe { waydock_blur_free(handle) };
        }
        self.0.surface_count.set(0);
        BINDINGS.with(|b| b.borrow_mut().retain(|weak| !std::ptr::eq(weak.as_ptr(), Rc::as_ptr(&self.0))));
    }
}

const KEY: &str = "waydock-background-blur";

/// Share one binding per GTK native tree to avoid duplicate protocol objects.
pub fn attach(widget: &impl IsA<gtk::Widget>, target_class: Option<&str>, include_root: bool, include_popovers: bool) -> BlurBinding {
    let widget = widget.upcast_ref::<gtk::Widget>();
    // SAFETY: the key is only ever used with this type.
    if let Some(existing) = unsafe { widget.data::<BlurBinding>(KEY) } {
        let existing = unsafe { existing.as_ref() }.clone();
        if !existing.is_closed() {
            return existing;
        }
    }
    let binding = BlurBinding::new(widget, target_class, include_root, include_popovers);
    unsafe { widget.set_data(KEY, binding.clone()) };
    binding
}
