//! Port of `~/btmanager/wayshell/wayshell_theme.py`: apply Waydock's monitor
//! palette to Wayshell's GTK 4 controls, never the wallpaper.
//!
//! Public API used by wayshell and wayshell-prompt:
//! `WayshellTheme::new(path: Option<PathBuf>) -> Rc<WayshellTheme>`, `.refresh()`, `.close()`,
//! plus the pure functions (`palette_path`, `read_palette`, `monitor_palette`,
//! `frost_texture_path`, `frost_background`, `on_accent`, `palette_css`).
//!
//! Python's per-window `window.monitor` attribute is [`set_window_monitor`].

pub mod css;
pub mod palette;

pub use css::{frost_background, frost_texture_path, on_accent, palette_css, Colors, CssOptions, ValueError};
pub use palette::{monitor_palette, palette_path, parse_palette, read_palette, MonitorPalette, Palette};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use suite_gtk::blur::{self, BlurBinding};

/// Marker classes that select the CSS variant.
pub const DESKTOP_CLASS: &str = "wayshell-desktop";
pub const SWITCHER_CLASS: &str = "wayshell-switcher";
pub const BACKDROP_CLASS: &str = "wayshell-switcher-backdrop";
/// `Gtk.STYLE_PROVIDER_PRIORITY_USER + 2`.
pub const PROVIDER_PRIORITY: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER + 2;

const MONITOR_KEY: &str = "wayshell-theme-monitor";

/// Python `window.monitor = monitor`: the monitor a (layer) window belongs to.
/// `None` clears it, so the surface's monitor is used again.
pub fn set_window_monitor(window: &impl IsA<gtk::Window>, monitor: Option<&gdk::Monitor>) {
    let object = window.upcast_ref::<gtk::Window>().upcast_ref::<glib::Object>();
    // SAFETY: this key only ever stores a gdk::Monitor.
    unsafe {
        match monitor {
            Some(monitor) => object.set_data(MONITOR_KEY, monitor.clone()),
            None => drop(object.steal_data::<gdk::Monitor>(MONITOR_KEY)),
        }
    }
}

/// `getattr(window, "monitor", None)`.
pub fn window_monitor(window: &impl IsA<gtk::Window>) -> Option<gdk::Monitor> {
    // SAFETY: see set_window_monitor.
    unsafe { window.upcast_ref::<gtk::Window>().upcast_ref::<glib::Object>().data::<gdk::Monitor>(MONITOR_KEY).map(|m| m.as_ref().clone()) }
}

/// `(st_ino, st_size, st_mtime_ns)`; `Unset` is Python's initial `object()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stamp {
    Unset,
    Missing,
    File(u64, u64, i128),
}

#[derive(Clone, PartialEq, Eq)]
struct Signature {
    stamp: Stamp,
    name: String,
    desktop: bool,
    switcher: bool,
    backdrop: bool,
}

struct WindowState {
    css_class: String,
    provider: gtk::CssProvider,
    signature: Signature,
}

/// Follow atomic palette changes and monitor moves using GTK 4 toplevels.
///
/// GTK file/color dialogs created in this process are picked up automatically.
/// DesktopView marks itself `wayshell-desktop` before the next idle refresh.
pub struct WayshellTheme {
    pub path: PathBuf,
    snapshot: RefCell<Option<Rc<Palette>>>,
    stamp: Cell<Stamp>,
    windows: RefCell<HashMap<gtk::Window, WindowState>>,
    blur: RefCell<HashMap<gtk::Window, BlurBinding>>,
    serial: Cell<u64>,
    closed: Cell<bool>,
    idle: RefCell<Option<glib::SourceId>>,
    watched: RefCell<HashMap<gtk::Window, Vec<(glib::Object, glib::SignalHandlerId)>>>,
    markers: RefCell<HashMap<gtk::Window, (bool, bool, bool)>>,
    monitor_handlers: RefCell<Vec<(gdk::Monitor, glib::SignalHandlerId)>>,
    toplevels: gio::ListModel,
    handler: RefCell<Option<glib::SignalHandlerId>>,
    file_monitor: RefCell<Option<(gio::FileMonitor, glib::SignalHandlerId)>>,
    monitors: Option<gio::ListModel>,
    monitors_handler: RefCell<Option<glib::SignalHandlerId>>,
    this: RefCell<Weak<WayshellTheme>>,
}

impl WayshellTheme {
    /// `WayshellTheme(path=None)`; `None` follows [`palette_path`].
    pub fn new(path: Option<PathBuf>) -> Rc<Self> {
        let path = path.unwrap_or_else(palette_path);
        let display = gdk::Display::default();
        let theme = Rc::new(Self {
            path, snapshot: RefCell::new(None), stamp: Cell::new(Stamp::Unset),
            windows: RefCell::default(), blur: RefCell::default(), serial: Cell::new(0), closed: Cell::new(false),
            idle: RefCell::new(None), watched: RefCell::default(), markers: RefCell::default(),
            monitor_handlers: RefCell::default(), toplevels: gtk::Window::toplevels(), handler: RefCell::new(None),
            file_monitor: RefCell::new(None), monitors: display.as_ref().map(|d| d.monitors()),
            monitors_handler: RefCell::new(None), this: RefCell::new(Weak::new()),
        });
        theme.this.replace(Rc::downgrade(&theme));
        let weak = Rc::downgrade(&theme);
        let handler = theme.toplevels.connect_items_changed(move |_, _, _, _| queue(&weak));
        theme.handler.replace(Some(handler));
        // Palette writes are atomic renames into place. GLib's file monitor
        // also notices a cache directory created after this process starts.
        match gio::File::for_path(&theme.path).monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            Ok(monitor) => {
                monitor.set_rate_limit(100);
                let weak = Rc::downgrade(&theme);
                let id = monitor.connect_changed(move |_, _, _, _| queue(&weak));
                theme.file_monitor.replace(Some((monitor, id)));
            }
            Err(error) => eprintln!("Palette monitoring unavailable: {}", error.message()),
        }
        // Output moves and monitor geometry changes replace the old polling.
        if let Some(monitors) = &theme.monitors {
            let weak = Rc::downgrade(&theme);
            let id = monitors.connect_items_changed(move |_, _, _, _| {
                if let Some(theme) = weak.upgrade() {
                    theme.monitors_changed();
                }
            });
            theme.monitors_handler.replace(Some(id));
        }
        theme.monitors_changed();
        theme.refresh_now();
        theme
    }

    fn weak(&self) -> Weak<Self> {
        self.this.borrow().clone()
    }

    fn monitors_changed(&self) {
        for (monitor, handler) in self.monitor_handlers.take() {
            monitor.disconnect(handler);
        }
        if let Some(monitors) = &self.monitors {
            let mut handlers = Vec::new();
            for monitor in monitors.iter::<glib::Object>().flatten().filter_map(|o| o.downcast::<gdk::Monitor>().ok()) {
                let weak = self.weak();
                let id = monitor.connect_geometry_notify(move |_| queue(&weak));
                handlers.push((monitor, id));
            }
            self.monitor_handlers.replace(handlers);
        }
        self.queue_refresh();
    }

    /// Refresh when a window maps onto, or moves to, another output.
    fn watch(&self, window: &gtk::Window) {
        let fresh = !self.watched.borrow().contains_key(window);
        if fresh {
            let weak = self.weak();
            let realize = window.connect_realize(move |_| queue(&weak));
            let weak = self.weak();
            let classes = window.connect_notify_local(Some("css-classes"), move |window, _| {
                if let (Some(theme), Ok(window)) = (weak.upgrade(), window.clone().downcast::<gtk::Window>()) {
                    theme.classes_changed(&window);
                }
            });
            let object = window.clone().upcast::<glib::Object>();
            self.watched.borrow_mut().insert(window.clone(), vec![(object.clone(), realize), (object, classes)]);
        }
        if let Some(surface) = window.surface() {
            let object = surface.clone().upcast::<glib::Object>();
            let known = self.watched.borrow().get(window).is_some_and(|handlers| handlers.iter().any(|(o, _)| *o == object));
            if !known {
                let weak = self.weak();
                let id = surface.connect_enter_monitor(move |_, _| queue(&weak));
                if let Some(handlers) = self.watched.borrow_mut().get_mut(window) {
                    handlers.push((object, id));
                }
            }
        }
    }

    fn unwatch(&self, window: &gtk::Window) {
        let handlers = self.watched.borrow_mut().remove(window);
        for (object, handler) in handlers.unwrap_or_default() {
            object.disconnect(handler);
        }
    }

    /// Only the marker classes change the CSS; ignore our own palette class.
    fn classes_changed(&self, window: &gtk::Window) {
        let markers = (window.has_css_class(DESKTOP_CLASS), window.has_css_class(SWITCHER_CLASS),
                       window.has_css_class(BACKDROP_CLASS));
        let previous = self.markers.borrow_mut().insert(window.clone(), markers);
        if previous != Some(markers) {
            self.queue_refresh();
        }
    }

    fn queue_refresh(&self) {
        if self.idle.borrow().is_some() || self.closed.get() {
            return;
        }
        let weak = self.weak();
        let id = glib::idle_add_local_once(move || {
            if let Some(theme) = weak.upgrade() {
                theme.idle.replace(None);
                theme.refresh_now();
            }
        });
        self.idle.replace(Some(id));
    }

    fn monitor_for(window: &gtk::Window) -> (Option<String>, Option<[i64; 4]>) {
        let mut monitor = window_monitor(window);
        if monitor.is_none() {
            let mut surface = window.surface();
            if surface.is_none() {
                if let Some(parent) = window.transient_for() {
                    surface = parent.surface();
                }
            }
            if let Some(surface) = surface {
                monitor = WidgetExt::display(window).monitor_at_surface(&surface);
            }
        }
        let Some(monitor) = monitor else { return (None, None) };
        let geometry = monitor.geometry();
        (monitor.connector().map(|c| c.to_string()),
         Some([geometry.x() as i64, geometry.y() as i64, geometry.width() as i64, geometry.height() as i64]))
    }

    fn remove(&self, window: &gtk::Window) {
        let Some(state) = self.windows.borrow_mut().remove(window) else { return };
        let binding = self.blur.borrow_mut().remove(window);
        if let Some(binding) = binding {
            binding.close();
        }
        window.remove_css_class(&state.css_class);
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(window), &state.provider);
    }

    /// Windows currently themed (Python `theme._windows`, for tests/diagnostics).
    pub fn themed_windows(&self) -> Vec<gtk::Window> {
        self.windows.borrow().keys().cloned().collect()
    }

    /// The palette class and provider of a themed window.
    pub fn window_provider(&self, window: &impl IsA<gtk::Window>) -> Option<(String, gtk::CssProvider)> {
        self.windows.borrow().get(window.upcast_ref()).map(|state| (state.css_class.clone(), state.provider.clone()))
    }

    /// The blur binding of a window (Python `theme._blur`).
    pub fn window_blur(&self, window: &impl IsA<gtk::Window>) -> Option<BlurBinding> {
        self.blur.borrow().get(window.upcast_ref()).cloned()
    }

    /// The validated snapshot in use (`theme.snapshot`).
    pub fn snapshot(&self) -> Option<Rc<Palette>> {
        self.snapshot.borrow().clone()
    }

    /// Re-read the cache if it changed and restyle every toplevel.
    pub fn refresh(&self) {
        self.refresh_now();
    }

    /// `refresh()` with Python's return value (`false` once closed).
    pub fn refresh_now(&self) -> bool {
        if self.closed.get() {
            return false;
        }
        let stamp = match std::fs::metadata(&self.path) {
            Ok(stat) => Stamp::File(stat.ino(), stat.size(), stat.mtime() as i128 * 1_000_000_000 + stat.mtime_nsec() as i128),
            Err(_) => Stamp::Missing,
        };
        if stamp != self.stamp.get() {
            self.stamp.set(stamp);
            let snapshot = if stamp == Stamp::Missing { None } else { read_palette(&self.path).map(Rc::new) };
            self.snapshot.replace(snapshot);
        }
        let windows: Vec<gtk::Window> = self.toplevels.iter::<glib::Object>().flatten().filter_map(|o| o.downcast::<gtk::Window>().ok()).collect();
        let gone: Vec<gtk::Window> = self.watched.borrow().keys().filter(|w| !windows.contains(w)).cloned().collect();
        for window in &gone {
            self.unwatch(window);
            self.markers.borrow_mut().remove(window);
        }
        for window in &windows {
            self.watch(window);
        }
        let gone: Vec<gtk::Window> = self.windows.borrow().keys().filter(|w| !windows.contains(w)).cloned().collect();
        for window in &gone {
            self.remove(window);
        }
        let Some(snapshot) = self.snapshot() else {
            for window in self.themed_windows() {
                self.remove(&window);
            }
            return true;
        };
        for window in &windows {
            let (name, geometry) = Self::monitor_for(window);
            let colors = monitor_palette(&snapshot, name.as_deref(), geometry);
            let desktop = window.has_css_class(DESKTOP_CLASS);
            let switcher = window.has_css_class(SWITCHER_CLASS);
            let backdrop = window.has_css_class(BACKDROP_CLASS);
            let existing = self.blur.borrow().get(window).cloned();
            let binding = existing.unwrap_or_else(|| {
                let binding = blur::attach(window, None, true, true);
                self.blur.borrow_mut().insert(window.clone(), binding.clone());
                binding
            });
            binding.configure(snapshot.apps_frosted && !backdrop, snapshot.apps_frost_strength as u8,
                              snapshot.apps_rounded_corners, Some(if switcher { "switcher-surface" } else { "" }),
                              Some(!(desktop || backdrop)));
            let signature = Signature { stamp: self.stamp.get(), name: colors.name.clone(), desktop, switcher, backdrop };
            let current = self.windows.borrow().get(window).map(|state| (state.css_class.clone(), state.provider.clone(),
                                                                          state.signature == signature));
            let (css_class, provider) = match current {
                Some((_, _, true)) => continue,
                Some((css_class, provider, false)) => (css_class, provider),
                None => {
                    self.serial.set(self.serial.get() + 1);
                    let css_class = format!("wayshell-palette-{}", self.serial.get());
                    let provider = gtk::CssProvider::new();
                    gtk::style_context_add_provider_for_display(&WidgetExt::display(window), &provider, PROVIDER_PRIORITY);
                    window.add_css_class(&css_class);
                    (css_class, provider)
                }
            };
            let transparency = if snapshot.apps_transparency_enabled { snapshot.apps_transparency } else { 0 };
            let options = CssOptions {
                desktop, switcher, backdrop, frosted: snapshot.apps_frosted,
                frost_strength: snapshot.apps_frost_strength, rounded_corners: snapshot.apps_rounded_corners,
            };
            // Validated snapshot values are always in range.
            let text = palette_css(&css_class, &colors.colors, transparency, &options).unwrap_or_default();
            provider.load_from_string(&text);
            self.windows.borrow_mut().insert(window.clone(), WindowState { css_class, provider, signature });
        }
        true
    }

    /// Stop following the palette and remove every provider/class/blur.
    pub fn close(&self) {
        self.closed.set(true);
        if let Some((monitor, handler)) = self.file_monitor.take() {
            monitor.disconnect(handler);
            monitor.cancel();
        }
        for (monitor, handler) in self.monitor_handlers.take() {
            monitor.disconnect(handler);
        }
        if let (Some(monitors), Some(handler)) = (&self.monitors, self.monitors_handler.take()) {
            monitors.disconnect(handler);
        }
        let watched: Vec<gtk::Window> = self.watched.borrow().keys().cloned().collect();
        for window in &watched {
            self.unwatch(window);
        }
        if let Some(id) = self.idle.take() {
            suite_core::glib_compat::source_remove(id);
        }
        if let Some(handler) = self.handler.take() {
            self.toplevels.disconnect(handler);
        }
        for window in self.themed_windows() {
            self.remove(&window);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }
}

fn queue(weak: &Weak<WayshellTheme>) {
    if let Some(theme) = weak.upgrade() {
        theme.queue_refresh();
    }
}
