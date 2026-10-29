//! A click-through overlay that dims every output shortly before locking
//! (power_dim.py). Purely visual: it never writes backlight or monitor
//! settings, so restoring cannot fail or leave hardware changed.

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;

pub const OPACITY: f64 = 0.55;
pub const CSS: &str = "window.power-dim-overlay { background: transparent; }";

#[derive(Default)]
pub struct DimOverlay {
    pub windows: RefCell<Vec<gtk::Window>>,
    css: RefCell<Option<gtk::CssProvider>>,
}

impl DimOverlay {
    /// Requires an initialized GTK.
    pub fn available(&self) -> bool {
        gtk::is_initialized_main_thread() && gtk4_layer_shell::is_supported()
    }

    pub fn dimmed(&self) -> bool {
        !self.windows.borrow().is_empty()
    }

    pub fn dim(&self) -> bool {
        if !self.windows.borrow().is_empty() || !self.available() {
            return false;
        }
        let Some(display) = gdk::Display::default() else { return false };
        if self.css.borrow().is_none() {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(CSS);
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            self.css.replace(Some(provider));
        }
        let monitors = display.monitors();
        let windows: Vec<gtk::Window> = (0..monitors.n_items())
            .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>()).map(|m| window(&m)).collect();
        let dimmed = !windows.is_empty();
        self.windows.replace(windows);
        dimmed
    }

    pub fn restore(&self) {
        for window in self.windows.take() {
            window.destroy();
        }
    }

    pub fn close(&self) {
        self.restore();
        if let Some(provider) = self.css.take() {
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_remove_provider_for_display(&display, &provider);
            }
        }
    }
}

fn window(monitor: &gdk::Monitor) -> gtk::Window {
    let window = gtk::Window::new();
    window.set_title(Some("Dim screen"));
    window.set_decorated(false);
    window.add_css_class("power-dim-overlay");
    window.init_layer_shell();
    window.set_namespace(Some("power-manager-dim"));
    window.set_layer(Layer::Overlay);
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_monitor(Some(monitor));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    let area = gtk::DrawingArea::new();
    area.set_draw_func(|_, cr, _, _| {
        cr.set_source_rgba(0.0, 0.0, 0.0, OPACITY);
        let _ = cr.paint();
    });
    window.set_child(Some(&area));
    window.connect_realize(|w| pass_input(w));
    window.connect_map(|w| pass_input(w));
    window.present();
    window
}

fn pass_input(window: &gtk::Window) {
    if let Some(surface) = window.surface() {
        surface.set_input_region(Some(&gtk::cairo::Region::create()));
    }
    let _ = glib::ControlFlow::Break;
}
