//! Opt-in compositor blur for GTK4 surfaces on GTK's own wl_display
//! (port of shared/background_blur.c + background_blur.py; `tessel_gtk::blur`
//! re-exports this module).
//!
//! Uses ext_background_effect_v1 (blur region) and the private
//! waydock_blur_manager_v1 (per-surface radius 0..40). GTK owns the display,
//! dispatch of its own queue, surfaces and commits: this only adds
//! double-buffered protocol state, never roundtrips and never commits — the
//! state lands with GTK's next frame (`queue_render`). Our globals live on a
//! private event queue on GDK's connection, dispatched by a GSource.
//! No wallpaper sampling or image fallback: on compositors without
//! ext-background-effect-v1 the regular transparent tint remains.

use crate::source::{raw_source, Pump, Wake};
use gdk_wayland::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;
use wayland_client::protocol::{wl_compositor, wl_region, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1 as effect_manager, ext_background_effect_surface_v1 as effect_surface,
};

/// Generated from protocols/waydock-blur-v1.xml (labwc's private extension).
#[allow(non_upper_case_globals, non_camel_case_types, unused_imports, missing_docs, clippy::all)]
pub mod waydock_blur_v1 {
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/waydock-blur-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/waydock-blur-v1.xml");
}
use waydock_blur_v1::{waydock_blur_manager_v1 as radius_manager, waydock_blur_surface_v1 as radius_surface};

const BLUR_CAPABILITY: u32 = 1;
const MANAGER_KEY: &str = "tessel-wayland-blur-manager";
const OWNER_KEY: &str = "tessel-dock-background-blur-owner";

#[derive(Default)]
struct Globals {
    registry: Option<wl_registry::WlRegistry>,
    effects: Option<(u32, effect_manager::ExtBackgroundEffectManagerV1)>,
    radii: Option<(u32, radius_manager::WaydockBlurManagerV1)>,
    capabilities: u32,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, .. } => {
                if interface == effect_manager::ExtBackgroundEffectManagerV1::interface().name && state.effects.is_none() {
                    state.effects = Some((name, registry.bind(name, 1, qh, ())));
                } else if interface == radius_manager::WaydockBlurManagerV1::interface().name && state.radii.is_none() {
                    state.radii = Some((name, registry.bind(name, 1, qh, ())));
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                if state.effects.as_ref().is_some_and(|e| e.0 == name) {
                    if let Some((_, manager)) = state.effects.take() {
                        manager.destroy();
                    }
                    state.capabilities = 0;
                }
                if state.radii.as_ref().is_some_and(|r| r.0 == name) {
                    if let Some((_, manager)) = state.radii.take() {
                        manager.destroy();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<effect_manager::ExtBackgroundEffectManagerV1, ()> for Globals {
    fn event(state: &mut Self, _: &effect_manager::ExtBackgroundEffectManagerV1, event: effect_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let effect_manager::Event::Capabilities { flags } = event {
            state.capabilities = match flags {
                WEnum::Value(v) => v.bits(),
                WEnum::Unknown(raw) => raw,
            };
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Globals {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(effect_surface::ExtBackgroundEffectSurfaceV1, radius_manager::WaydockBlurManagerV1,
           radius_surface::WaydockBlurSurfaceV1, wl_region::WlRegion, wl_compositor::WlCompositor);

/// Per-GdkDisplay protocol state (C `Manager`).
struct Manager {
    display: glib::WeakRef<gdk::Display>,
    conn: Connection,
    compositor: Option<wl_compositor::WlCompositor>,
    queue: RefCell<EventQueue<Globals>>,
    globals: RefCell<Globals>,
    wake: Arc<Wake>,
    source: RefCell<Option<glib::Source>>,
}

impl Drop for Manager {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.destroy();
        }
        // GDK has already disconnected its Wayland display when closed.
        if self.display.upgrade().is_some_and(|d| !d.is_closed()) {
            let mut globals = self.globals.borrow_mut();
            if let Some((_, m)) = globals.effects.take() {
                m.destroy();
            }
            if let Some((_, m)) = globals.radii.take() {
                m.destroy();
            }
            globals.registry.take();
            let _ = self.conn.flush();
        }
    }
}

struct ManagerPump(Weak<Manager>);

impl ManagerPump {
    fn pump(manager: &Manager) {
        let (Ok(mut queue), Ok(mut globals)) = (manager.queue.try_borrow_mut(), manager.globals.try_borrow_mut()) else { return };
        let waker = std::task::Waker::from(manager.wake.clone());
        let mut cx = std::task::Context::from_waker(&waker);
        let _ = queue.poll_dispatch_pending(&mut cx, &mut globals);
    }
}

impl Pump for ManagerPump {
    fn prepare(&self) -> bool {
        let Some(manager) = self.0.upgrade() else { return true };
        if manager.display.upgrade().is_none_or(|d| d.is_closed()) {
            return true;
        }
        manager.wake.take();
        // Our handlers only record globals, so dispatching here is enough.
        Self::pump(&manager);
        false
    }
    fn check(&self, revents: u16) -> bool { self.0.upgrade().is_none() || revents & (glib::ffi::G_IO_IN as u16) != 0 }
    fn dispatch(&self) -> bool {
        let Some(manager) = self.0.upgrade() else { return false };
        if manager.display.upgrade().is_none_or(|d| d.is_closed()) {
            return false;
        }
        if let Ok(queue) = manager.queue.try_borrow() {
            if let Some(guard) = queue.prepare_read() {
                let _ = guard.read();
            }
        }
        Self::pump(&manager);
        true
    }
}

fn manager_for(display: &gdk::Display) -> Option<Rc<Manager>> {
    let wayland = display.downcast_ref::<gdk_wayland::WaylandDisplay>()?;
    if display.is_closed() {
        return None;
    }
    // SAFETY: the key is only ever used with this type.
    if let Some(existing) = unsafe { display.data::<Rc<Manager>>(MANAGER_KEY) } {
        return Some(unsafe { existing.as_ref() }.clone());
    }
    let conn = crate::gtk::gdk_connection(display)?;
    let queue = conn.new_event_queue::<Globals>();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let context = glib::MainContext::default();
    let manager = Rc::new(Manager {
        display: display.downgrade(), compositor: wayland.wl_compositor(), conn: conn.clone(), queue: RefCell::new(queue),
        globals: RefCell::new(Globals { registry: Some(registry), ..Default::default() }), wake: Wake::new(&context),
        source: RefCell::new(None),
    });
    let fd = conn.backend().poll_fd().as_raw_fd_compat();
    manager.source.replace(Some(raw_source(fd, Box::new(ManagerPump(Rc::downgrade(&manager))), &context, "tessel-wayland-blur")));
    let _ = conn.flush();
    unsafe { display.set_data(MANAGER_KEY, manager.clone()) };
    Some(manager)
}

trait RawFdCompat {
    fn as_raw_fd_compat(&self) -> i32;
}
impl RawFdCompat for std::os::fd::BorrowedFd<'_> {
    fn as_raw_fd_compat(&self) -> i32 { std::os::fd::AsRawFd::as_raw_fd(self) }
}

/// One blurred native surface (C `Surface`).
struct BlurSurface {
    surface: gdk::Surface,
    manager: Rc<Manager>,
    effect: effect_surface::ExtBackgroundEffectSurfaceV1,
    radius: Option<radius_surface::WaydockBlurSurfaceV1>,
    geometry: [i32; 6],
    seen: bool,
    token: usize,
}

impl Drop for BlurSurface {
    fn drop(&mut self) {
        // SAFETY: OWNER_KEY only ever holds a usize token.
        unsafe {
            if self.surface.data::<usize>(OWNER_KEY).is_some_and(|t| *t.as_ref() == self.token) {
                self.surface.steal_data::<usize>(OWNER_KEY);
            }
        }
        if !self.surface.display().is_closed() {
            self.effect.destroy();
            if let Some(radius) = self.radius.take() {
                radius.destroy();
            }
            let _ = self.manager.conn.flush();
            if !self.surface.is_destroyed() {
                self.surface.queue_render();
            }
        }
    }
}

thread_local! {
    static NEXT_TOKEN: Cell<usize> = const { Cell::new(1) };
}

/// The blur objects of one GTK native tree (C `Group`).
#[derive(Default)]
struct Group {
    surfaces: Vec<BlurSurface>,
}

fn find_panel(widget: &gtk::Widget, class_name: Option<&str>) -> Option<gtk::Widget> {
    if class_name.is_some_and(|c| !c.is_empty() && widget.has_css_class(c)) {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if !current.is::<gtk::Native>() {
            if let Some(found) = find_panel(&current, class_name) {
                return Some(found);
            }
        }
        child = current.next_sibling();
    }
    None
}

/// Pixel-centre scanlines keep the effect inside the CSS rounded outline; the
/// full middle rectangle bounds the cost by 2·radius rows, not panel size.
pub fn rounded_region_rects(x: i32, y: i32, width: i32, height: i32, radius: i32) -> Vec<[i32; 4]> {
    let radius = radius.min(width.min(height) / 2);
    if radius <= 0 {
        return vec![[x, y, width, height]];
    }
    let mut rects = vec![[x, y + radius, width, height - 2 * radius]];
    for row in 0..radius {
        let dy = radius as f64 - row as f64 - 0.5;
        let r = radius as f64;
        let inset = (r - (r * r - dy * dy).sqrt()).ceil() as i32;
        if width - inset * 2 > 0 {
            rects.push([x + inset, y + row, width - inset * 2, 1]);
            rects.push([x + inset, y + height - row - 1, width - inset * 2, 1]);
        }
    }
    rects
}

impl Group {
    fn apply_native(&mut self, widget: &gtk::Widget, panel: &gtk::Widget, corners: i32, strength: i32) {
        let Some(native) = widget.dynamic_cast_ref::<gtk::Native>() else { return };
        let Some(surface) = native.surface() else { return };
        let Some(wayland_surface) = surface.downcast_ref::<gdk_wayland::WaylandSurface>() else { return };
        if surface.is_destroyed() {
            return;
        }
        let Some(manager) = manager_for(&surface.display()) else { return };
        let (effects, radii, capabilities) = {
            let globals = manager.globals.borrow();
            (globals.effects.as_ref().map(|e| e.1.clone()), globals.radii.as_ref().map(|r| r.1.clone()), globals.capabilities)
        };
        let Some(effects) = effects.filter(|_| capabilities & BLUR_CAPABILITY != 0) else { return };
        let Some(compositor) = manager.compositor.clone() else { return };
        let Some(bounds) = panel.compute_bounds(widget) else { return };
        // GTK adds this translation when snapshotting the native widget into
        // its surface (CSD shadows put the window at a positive offset).
        let (tx, ty) = native.surface_transform();
        let (left, top) = (bounds.x() as f64 + tx, bounds.y() as f64 + ty);
        let (x, y) = (left.ceil() as i32, top.ceil() as i32);
        let width = (left + bounds.width() as f64).floor() as i32 - x;
        let height = (top + bounds.height() as f64).floor() as i32 - y;
        if width <= 0 || height <= 0 {
            return;
        }
        let index = match self.surfaces.iter().position(|s| s.surface == surface) {
            Some(index) => index,
            None => {
                // Only one extension object is permitted per wl_surface; the
                // current owner wins until it releases that surface.
                if unsafe { surface.data::<usize>(OWNER_KEY) }.is_some() {
                    return;
                }
                let Some(wl_surface) = wayland_surface.wl_surface() else { return };
                let qh = manager.queue.borrow().handle();
                let effect = effects.get_background_effect(&wl_surface, &qh, ());
                let radius = radii.map(|r| r.get_surface(&wl_surface, &qh, ()));
                let token = NEXT_TOKEN.with(|t| t.replace(t.get() + 1));
                unsafe { surface.set_data(OWNER_KEY, token) };
                self.surfaces.push(BlurSurface { surface: surface.clone(), manager: manager.clone(), effect, radius,
                                                 geometry: [0, 0, 0, 0, 0, -1], seen: false, token });
                self.surfaces.len() - 1
            }
        };
        let record = &mut self.surfaces[index];
        record.seen = true;
        let geometry = [x, y, width, height, corners, strength];
        if record.geometry == geometry {
            return;
        }
        let qh = manager.queue.borrow().handle();
        let region = compositor.create_region(&qh, ());
        for [rx, ry, rw, rh] in rounded_region_rects(x, y, width, height, corners) {
            region.add(rx, ry, rw, rh);
        }
        record.effect.set_blur_region(Some(&region));
        region.destroy();
        if let Some(radius) = &record.radius {
            radius.set_radius(strength as u32);
        }
        record.geometry = geometry;
        let _ = manager.conn.flush();
        widget.queue_draw();
        surface.queue_render();
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(&mut self, widget: &gtk::Widget, root: &gtk::Widget, target_class: Option<&str>, include_root: bool,
             include_popovers: bool, corners: i32, strength: i32) {
        if !widget.is_mapped() || (widget.is::<gtk::Popover>() && !include_popovers) {
            return;
        }
        if widget.is::<gtk::Native>() {
            let mut panel = None;
            if widget == root && include_root {
                panel = if target_class.is_some_and(|c| !c.is_empty()) { find_panel(widget, target_class) } else { Some(widget.clone()) };
            } else if widget.is::<gtk::Popover>() {
                // GtkPopoverContent owns the background, not the transparent
                // popup margin, shadow, or positioning arrow.
                let mut child = widget.first_child();
                while let Some(current) = child {
                    if current.css_name() == "contents" {
                        panel = Some(current);
                        break;
                    }
                    child = current.next_sibling();
                }
            }
            if let Some(panel) = panel {
                self.apply_native(widget, &panel, corners, strength);
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            self.visit(&current, root, target_class, include_root, include_popovers, corners, strength);
            child = current.next_sibling();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update(&mut self, root: &gtk::Widget, target_class: Option<&str>, enabled: bool, include_root: bool,
              include_popovers: bool, corners: i32, strength: i32) -> i32 {
        for surface in &mut self.surfaces {
            surface.seen = false;
        }
        if enabled && strength > 0 {
            self.visit(root, root, target_class, include_root, include_popovers, corners.clamp(0, 64), strength.clamp(0, 40));
        }
        self.surfaces.retain(|s| s.seen);
        self.surfaces.len() as i32
    }
}

// ---- BlurBinding (background_blur.py API) ----------------------------------------------

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
    group: RefCell<Group>,
    signals: RefCell<Vec<glib::SignalHandlerId>>,
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
            group: RefCell::new(Group::default()), signals: RefCell::new(Vec::new()),
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
        self.0.group.borrow_mut().surfaces.clear();
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
            // One timer for all surfaces, no perpetual frame callback.
            TIMER.with(|t| t.set(true));
            glib::timeout_add_local(Duration::from_millis(200), poll);
        }
    }

    pub fn refresh(&self) {
        let Some(widget) = self.0.widget.upgrade() else { return };
        if self.0.closed.get() {
            return;
        }
        let target = self.0.target_class.borrow().clone().filter(|t| !t.is_empty());
        let strength = (self.0.strength.get() as f64 * 0.4).round_ties_even() as i32;
        let count = self.0.group.borrow_mut().update(&widget, target.as_deref(), self.0.enabled.get(), self.0.include_root.get(),
                                                     self.0.include_popovers, if self.0.rounded.get() { 12 } else { 0 }, strength);
        self.0.surface_count.set(count);
    }

    /// Number of surfaces currently given a blur region.
    pub fn surface_count(&self) -> i32 { self.0.surface_count.get() }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }

    /// Diagnostics: `[x, y, width, height, corners, strength]` of surface `index`
    /// (requested logical geometry, never pixels from other applications).
    pub fn geometry(&self, index: u32) -> Option<[i32; 6]> {
        self.0.group.borrow().surfaces.get(index as usize).map(|s| s.geometry)
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
        self.clear();
        BINDINGS.with(|b| b.borrow_mut().retain(|weak| !std::ptr::eq(weak.as_ptr(), Rc::as_ptr(&self.0))));
    }
}

/// Whether the compositor advertised blur for this display (diagnostics; the
/// answer arrives asynchronously after the first `attach`).
pub fn compositor_supports_blur(display: &gdk::Display) -> Option<bool> {
    let manager = manager_for(display)?;
    let globals = manager.globals.borrow();
    Some(globals.effects.is_some() && globals.capabilities & BLUR_CAPABILITY != 0)
}

/// Whether the private radius extension (waydock_blur_manager_v1) is bound.
pub fn compositor_supports_radius(display: &gdk::Display) -> Option<bool> {
    Some(manager_for(display)?.globals.borrow().radii.is_some())
}

const KEY: &str = "tessel-dock-background-blur";

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
