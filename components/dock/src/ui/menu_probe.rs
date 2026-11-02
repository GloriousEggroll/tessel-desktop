//! The `--menu` output probe (tessel-dock.py `toggle_menu` with several docks):
//! Wayland exposes no global pointer position, so an invisible 1×1 OVERLAY
//! layer surface (`tessel-dock-menu-target`, empty input region, keyboard NONE,
//! exclusive −1) is mapped and the compositor's output choice is read after
//! [`PROBE_DELAY_MS`].
//!
//! API: `MenuProbe::start(application, hotkey, resolved)` maps the surface
//! and after 60 ms calls `resolved(monitor, hotkey)` once (monitor = the
//! output the surface landed on, `None` if it never mapped) after
//! destroying the probe. `cancel()` destroys it without calling back (the
//! second press of a toggle). `hotkey()`, `window()`.

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

pub const PROBE_DELAY_MS: u64 = 60;

pub type Resolved = Box<dyn FnOnce(Option<gdk::Monitor>, bool)>;

pub struct MenuProbe {
    window: gtk::Window,
    hotkey: bool,
    timer: RefCell<Option<glib::SourceId>>,
    resolved: RefCell<Option<Resolved>>,
}

impl MenuProbe {
    pub fn start(application: Option<&gtk::Application>, hotkey: bool, resolved: Resolved) -> Rc<Self> {
        let probe = gtk::Window::new();
        if let Some(application) = application {
            probe.set_application(Some(application));
        }
        probe.set_decorated(false);
        probe.set_resizable(false);
        probe.add_css_class("tessel-dock-surface");
        let catcher = super::host::transparent_area();
        catcher.set_content_width(1);
        catcher.set_content_height(1);
        probe.set_child(Some(&catcher));
        probe.set_default_size(1, 1);
        probe.init_layer_shell();
        probe.set_namespace(Some("tessel-dock-menu-target"));
        probe.set_layer(Layer::Overlay);
        probe.set_exclusive_zone(-1);
        probe.set_keyboard_mode(KeyboardMode::None);
        probe.connect_realize(|window| {
            if let Some(surface) = window.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        probe.present();
        let this = Rc::new(Self { window: probe, hotkey, timer: RefCell::new(None), resolved: RefCell::new(Some(resolved)) });
        let weak = Rc::downgrade(&this);
        let source = glib::timeout_add_local_once(Duration::from_millis(PROBE_DELAY_MS), move || {
            let Some(this) = weak.upgrade() else { return };
            this.timer.borrow_mut().take();
            let monitor = if this.window.is_mapped() {
                this.window.surface().and_then(|surface| WidgetExt::display(&this.window).monitor_at_surface(&surface))
            } else {
                None
            };
            let callback = this.resolved.borrow_mut().take();
            this.cancel();
            if let Some(callback) = callback {
                callback(monitor, this.hotkey);
            }
        });
        this.timer.replace(Some(source));
        this
    }

    pub fn hotkey(&self) -> bool { self.hotkey }
    pub fn window(&self) -> &gtk::Window { &self.window }

    pub fn cancel(&self) {
        if let Some(source) = self.timer.borrow_mut().take() {
            source.remove();
        }
        self.resolved.borrow_mut().take();
        self.window.destroy();
    }
}
