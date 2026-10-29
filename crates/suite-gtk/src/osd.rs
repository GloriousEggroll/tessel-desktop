//! Non-interactive, centered feedback shared by audio and brightness controls
//! (adjustment_osd.py). Never changes hardware, starts services, or requires
//! Waydock to be running.

use crate::placement::pass_input;
use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use suite_core::tr;

pub const CSS: &str = "
window.adjustment-osd {
    background: transparent; border: none; box-shadow: none;
}
.adjustment-osd .popup-surface {
    background-image: none; background-color: @theme_bg_color;
    color: @theme_fg_color; border: 1px solid alpha(@theme_fg_color, .17);
    border-radius: 12px; padding: 16px 20px;
}
.adjustment-osd label, .adjustment-osd image { color: inherit; }
.adjustment-osd .osd-title { font-size: 11pt; font-weight: 600; }
.adjustment-osd .osd-value { font-size: 18pt; font-weight: 600; }
.adjustment-osd .muted { color: alpha(@theme_fg_color, .7); font-size: 9pt; }
.adjustment-osd progressbar trough {
    min-height: 6px; background: alpha(@theme_fg_color, .12);
    border: none; border-radius: 4px; box-shadow: none; margin: 0; padding: 0;
}
.adjustment-osd progressbar progress {
    min-height: 6px; background: @theme_selected_bg_color;
    border: none; border-radius: 4px; box-shadow: none; margin: 0; padding: 0;
}
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adjustment {
    Volume,
    Brightness,
}

struct Widgets {
    window: gtk::Window,
    icon: gtk::Image,
    title: gtk::Label,
    value: gtk::Label,
    progress: gtk::ProgressBar,
    detail: gtk::Label,
    css: gtk::CssProvider,
}

struct Inner {
    on_dismiss: Option<Box<dyn Fn()>>,
    duration: Duration,
    widgets: RefCell<Option<Widgets>>,
    monitor: RefCell<Option<gdk::Monitor>>,
    layer: bool,
    dismiss_timer: RefCell<Option<glib::SourceId>>,
    resolve_timer: RefCell<Option<glib::SourceId>>,
    callbacks: RefCell<Vec<Box<dyn FnOnce(Option<gdk::Monitor>)>>>,
    probe: RefCell<Option<gtk::Window>>,
}

/// The volume/brightness indicator. The caller owns its lifetime: no hidden
/// ApplicationWindow keeps a keyboard-only process running. Python default
/// duration: 1500 ms.
#[derive(Clone)]
pub struct AdjustmentOsd(Rc<Inner>);

/// Icon for an adjustment state.
pub fn icon_name(kind: Adjustment, value: Option<f64>, muted: bool) -> String {
    match (kind, value) {
        (Adjustment::Brightness, _) => "display-brightness-symbolic".into(),
        _ if muted || value == Some(0.0) => "audio-volume-muted-symbolic".into(),
        (_, None) => "audio-card-symbolic".into(),
        (_, Some(v)) => format!("audio-volume-{}-symbolic", if v <= 33.0 { "low" } else if v <= 66.0 { "medium" } else { "high" }),
    }
}

/// Value label text: "Muted", "42%" or "—".
pub fn value_text(kind: Adjustment, value: Option<f64>, muted: bool) -> String {
    if kind == Adjustment::Volume && muted {
        return tr!("Muted");
    }
    value.map_or("—".into(), |v| format!("{:.0}%", v.max(0.0)))
}

impl AdjustmentOsd {
    pub fn new(on_dismiss: Option<Box<dyn Fn()>>, duration_ms: u64) -> Self {
        Self(Rc::new(Inner {
            on_dismiss, duration: Duration::from_millis(duration_ms), widgets: RefCell::new(None), monitor: RefCell::new(None),
            layer: gtk4_layer_shell::is_supported(), dismiss_timer: RefCell::new(None), resolve_timer: RefCell::new(None),
            callbacks: RefCell::new(Vec::new()), probe: RefCell::new(None),
        }))
    }

    /// The indicator window while it exists.
    pub fn window(&self) -> Option<gtk::Window> { self.0.widgets.borrow().as_ref().map(|w| w.window.clone()) }
    pub fn monitor(&self) -> Option<gdk::Monitor> { self.0.monitor.borrow().clone() }

    fn new_window(&self, monitor: Option<&gdk::Monitor>, probe: bool) -> gtk::Window {
        let window = gtk::Window::new();
        window.set_title(Some("Adjustment indicator"));
        window.set_decorated(false);
        window.set_resizable(false);
        window.add_css_class("tray-popup");
        window.add_css_class("adjustment-osd");
        if self.0.layer {
            window.init_layer_shell();
            window.set_namespace(Some(if probe { "desktop-adjustment-osd-monitor" } else { "desktop-adjustment-osd" }));
            window.set_layer(Layer::Overlay);
            window.set_exclusive_zone(-1);
            window.set_keyboard_mode(KeyboardMode::None);
            // No anchors centers the surface in both axes. No monitor lets the
            // compositor choose the output the user is currently interacting with.
            if let Some(monitor) = monitor {
                window.set_monitor(Some(monitor));
            }
        }
        // An empty wl_surface input region passes mouse/touch events through
        // the whole card, including when it overlays a fullscreen game.
        window.connect_realize(|window| pass_input(window));
        window.connect_map(|window| pass_input(window));
        if probe {
            window.set_default_size(1, 1);
            let area = gtk::DrawingArea::new();
            area.set_draw_func(|_, cr, _, _| {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
                let _ = cr.paint();
            });
            window.set_child(Some(&area));
        }
        window
    }

    fn create(&self, monitor: Option<&gdk::Monitor>) {
        let window = self.new_window(monitor, false);
        let surface = gtk::Box::new(gtk::Orientation::Vertical, 12);
        surface.add_css_class("popup-surface");
        surface.set_size_request(310, -1);
        window.set_child(Some(&surface));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let icon = gtk::Image::new();
        icon.set_pixel_size(30);
        row.append(&icon);
        let title = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        title.add_css_class("osd-title");
        row.append(&title);
        let value = gtk::Label::builder().xalign(1.0).build();
        value.add_css_class("osd-value");
        row.append(&value);
        surface.append(&row);
        let progress = gtk::ProgressBar::new();
        surface.append(&progress);
        let detail = gtk::Label::builder().xalign(0.0).build();
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        detail.set_max_width_chars(36);
        detail.add_css_class("muted");
        surface.append(&detail);
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &css, crate::utils::PRIORITY_APPLICATION);
        crate::theme::attach(&window, None);
        self.0.monitor.replace(monitor.cloned());
        self.0.widgets.replace(Some(Widgets { window, icon, title, value, progress, detail, css }));
    }

    /// Show (or update) the indicator for `duration`. Non-finite values show "—".
    pub fn show(&self, kind: Adjustment, value: Option<f64>, muted: bool, monitor: Option<&gdk::Monitor>, detail: &str) {
        let value = value.filter(|v| v.is_finite());
        if self.0.widgets.borrow().is_none() {
            self.create(monitor);
        } else if let (Some(monitor), true) = (monitor, self.0.layer) {
            let window = self.window().unwrap();
            if Some(monitor) != self.0.monitor.borrow().as_ref() {
                window.set_visible(false);
            }
            window.set_monitor(Some(monitor));
            self.0.monitor.replace(Some(monitor.clone()));
        }
        {
            let widgets = self.0.widgets.borrow();
            let w = widgets.as_ref().unwrap();
            w.title.set_text(&if kind == Adjustment::Volume { tr!("Volume") } else { tr!("Brightness") });
            w.value.set_text(&value_text(kind, value, muted));
            w.icon.set_icon_name(Some(&icon_name(kind, value, muted)));
            w.progress.set_fraction(match value { Some(v) if !muted => (v / 100.0).clamp(0.0, 1.0), _ => 0.0 });
            // The detail line always keeps its height so the card never jumps,
            // for example when the volume is muted and there is no device name.
            let detail = if !detail.is_empty() { detail.to_owned() }
                         else if kind == Adjustment::Volume && muted { tr!("Muted") } else { String::new() };
            w.detail.set_text(if detail.is_empty() { " " } else { &detail });
            w.detail.set_visible(true);
            w.window.set_visible(true);
        }
        if let Some(id) = self.0.dismiss_timer.take() {
            id.remove();
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(self.0.duration, move || {
            if let Some(inner) = weak.upgrade() {
                inner.dismiss_timer.replace(None);
                AdjustmentOsd(inner).close();
            }
        });
        self.0.dismiss_timer.replace(Some(id));
    }

    /// Resolve the compositor-selected output without focusing a window.
    pub fn resolve_monitor(&self, callback: impl FnOnce(Option<gdk::Monitor>) + 'static) {
        self.0.callbacks.borrow_mut().push(Box::new(callback));
        if self.0.widgets.borrow().is_none() {
            self.show(Adjustment::Brightness, None, false, None, &tr!("Adjusting brightness…"));
        }
        if self.0.probe.borrow().is_none() {
            // A mapped layer surface retains its assigned output. A fresh tiny,
            // invisible surface resolves current interaction even if the user
            // moved to another monitor before the previous indicator expired.
            let probe = self.new_window(None, true);
            probe.set_visible(true);
            self.0.probe.replace(Some(probe));
        }
        if self.0.resolve_timer.borrow().is_none() {
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_local_once(Duration::from_millis(50), move || {
                let Some(inner) = weak.upgrade() else { return };
                inner.resolve_timer.replace(None);
                if let Some(probe) = inner.probe.take() {
                    let monitor = probe.surface().and_then(|s| WidgetExt::display(&probe).monitor_at_surface(&s));
                    inner.monitor.replace(monitor);
                    probe.destroy();
                }
                let callbacks = inner.callbacks.take();
                let monitor = inner.monitor.borrow().clone();
                for callback in callbacks {
                    callback(monitor.clone());
                }
            });
            self.0.resolve_timer.replace(Some(id));
        }
    }

    pub fn close(&self) {
        for timer in [self.0.dismiss_timer.take(), self.0.resolve_timer.take()].into_iter().flatten() {
            timer.remove();
        }
        self.0.callbacks.borrow_mut().clear();
        if let Some(probe) = self.0.probe.take() {
            probe.destroy();
        }
        if let Some(widgets) = self.0.widgets.take() {
            gtk::style_context_remove_provider_for_display(&WidgetExt::display(&widgets.window), &widgets.css);
            widgets.window.destroy();
            self.0.monitor.replace(None);
            if let Some(on_dismiss) = &self.0.on_dismiss {
                on_dismiss();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_and_value_text() {
        assert_eq!(icon_name(Adjustment::Brightness, Some(0.0), true), "display-brightness-symbolic");
        assert_eq!(icon_name(Adjustment::Volume, Some(0.0), false), "audio-volume-muted-symbolic");
        assert_eq!(icon_name(Adjustment::Volume, Some(50.0), true), "audio-volume-muted-symbolic");
        assert_eq!(icon_name(Adjustment::Volume, None, false), "audio-card-symbolic");
        assert_eq!(icon_name(Adjustment::Volume, Some(33.0), false), "audio-volume-low-symbolic");
        assert_eq!(icon_name(Adjustment::Volume, Some(66.0), false), "audio-volume-medium-symbolic");
        assert_eq!(icon_name(Adjustment::Volume, Some(67.0), false), "audio-volume-high-symbolic");
        assert_eq!(value_text(Adjustment::Brightness, Some(42.5), false), "42%");
        assert_eq!(value_text(Adjustment::Brightness, Some(-3.0), false), "0%");
        assert_eq!(value_text(Adjustment::Brightness, None, false), "—");
    }
}
