//! `MonitorOverlay`: click-through identify label at the output's top-left
//! corner, above panels and fullscreen windows (tessel-displays:600-738).

use crate::style::style_window;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tessel_gtk::theme::WindowTheme;

pub const OVERLAY_CSS: &str = r#"
        window#overlay-window {
            background-color: rgba(0, 0, 0, 0.0);  /* fully transparent */
        }

        .overlay-box {
            background-color: rgba(0, 0, 0, 0.85);
            border-radius: 8px;
            padding: 16px;
            border: 1px solid white;
        }

        .connector-label {
            color: white;
            font-size: 40pt;
            font-weight: bold;
        }
        .resolution-label {
            color: white;
            font-size: 30pt;
            font-weight: bold;
        }
        .device-label {
            color: white;
            font-size: 16pt;
            font-weight: bold;
        }
        "#;

pub struct MonitorOverlay {
    pub window: gtk::Window,
    pub palette: WindowTheme,
    pub overlay_box: gtk::Box,
}

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::Word);
    label.set_max_width_chars(20);
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    label.set_hexpand(false);
    label.set_vexpand(false);
    label
}

/// `(device)` text: the description without a trailing ` (<connector>)`.
pub fn device_text(device: &str, label_text: &str) -> String {
    format!("({})", device.replace(&format!(" ({label_text})"), ""))
}

pub fn resolution_text(width: i64, height: i64, angle: i64) -> String {
    if angle == 90 || angle == 270 { format!("{height}x{width}") } else { format!("{width}x{height}") }
}

impl MonitorOverlay {
    pub fn new(monitor: &gtk::gdk::Monitor, label_text: &str, device: &str, width: i64, height: i64, angle: i64) -> Self {
        let window = gtk::Window::new();
        window.set_title(Some("MonitorOverlay"));
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_focusable(false);
        window.init_layer_shell();
        // Identify physical screens above panels and fullscreen applications.
        // Ignore reserved workspace strips so this remains at output (0, 0).
        window.set_layer(Layer::Overlay);
        window.set_exclusive_zone(-1);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_monitor(Some(monitor));
        window.set_widget_name("overlay-window");
        let palette = style_window(&window, true);
        let display = WidgetExt::display(&window);
        let provider = gtk::CssProvider::new();
        provider.load_from_string(OVERLAY_CSS);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

        let overlay = gtk::Overlay::new();
        window.set_child(Some(&overlay));
        let overlay_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        overlay_box.add_css_class("overlay-box");
        overlay_box.set_valign(gtk::Align::Center);
        overlay_box.set_halign(gtk::Align::Center);
        overlay_box.set_vexpand(false);
        overlay_box.set_hexpand(false);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.set_valign(gtk::Align::Center);
        content.set_halign(gtk::Align::Fill);
        content.set_vexpand(true);
        content.set_hexpand(true);
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
        labels.set_valign(gtk::Align::Center);
        labels.set_halign(gtk::Align::Fill);
        labels.set_vexpand(false);
        labels.set_hexpand(true);
        labels.append(&label(label_text, "connector-label"));
        labels.append(&label(&resolution_text(width, height, angle), "resolution-label"));
        labels.append(&label(&device_text(device, label_text), "device-label"));
        content.append(&labels);
        overlay_box.append(&content);
        overlay.set_child(Some(&overlay_box));
        // Dynamically calculate 15% size of monitor.
        overlay_box.set_size_request(1.max((width as f64 * 0.15) as i32), 1.max((height as f64 * 0.15) as i32));
        Self { window, palette, overlay_box }
    }

    /// `present()` + empty input region (click-through).
    pub fn present(&self) {
        self.window.present();
        if let Some(surface) = self.window.surface() {
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
        }
    }

    pub fn destroy(&self) {
        self.palette.close();
        self.window.destroy();
    }
}
