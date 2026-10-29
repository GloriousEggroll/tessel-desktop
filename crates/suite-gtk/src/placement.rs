//! Pointer output discovery and the dock's live applet placement contract
//! (popup_placement.py).

use crate::utils::{add_css, remove_css};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// Popup placement published by Waydock's `GetPopupPlacement`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// `top`, `bottom`, `left` or `right`.
    pub edge: String,
    /// `start`, `center` or `end`.
    pub align: String,
    /// 0..=4096 px from the edge.
    pub offset: i32,
}

impl Placement {
    /// Validate Waydock's JSON reply exactly as Python does (`offset` must be a JSON int).
    pub fn from_json(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let edge = value.get("edge")?.as_str().filter(|e| ["top", "bottom", "left", "right"].contains(e))?;
        let align = value.get("align")?.as_str().filter(|a| ["start", "center", "end"].contains(a))?;
        let offset = value.get("offset")?.as_i64().filter(|o| (0..=4096).contains(o))?;
        Some(Self { edge: edge.into(), align: align.into(), offset: offset as i32 })
    }
}

pub fn edge_from_name(edge: &str) -> Edge {
    match edge {
        "top" => Edge::Top,
        "left" => Edge::Left,
        "right" => Edge::Right,
        _ => Edge::Bottom,
    }
}

const EDGES: [Edge; 4] = [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right];

/// Anchor a layer popup to `edge` at `offset`, aligned `start`/`center`/`end`
/// along it with a 12px side margin (center: only the primary anchor).
pub fn anchor_popup(window: &impl IsA<gtk::Window>, edge: &str, offset: i32, align: &str) {
    for side in EDGES {
        window.set_anchor(side, false);
        window.set_margin(side, 0);
    }
    let primary = edge_from_name(edge);
    window.set_anchor(primary, true);
    window.set_margin(primary, offset);
    if align != "center" {
        let start = align == "start";
        let secondary = if matches!(edge, "top" | "bottom") {
            if start { Edge::Left } else { Edge::Right }
        } else if start { Edge::Top } else { Edge::Bottom };
        window.set_anchor(secondary, true);
        window.set_margin(secondary, 12);
    }
}

/// Asynchronous, bounded (500 ms) lookup; standalone applets still work without the dock.
pub fn dock_placement(monitor: Option<&gdk::Monitor>, group: &str, callback: impl FnOnce(Option<Placement>) + 'static) {
    let connector = monitor.and_then(|m| m.connector()).map(String::from).unwrap_or_default();
    let group = group.to_owned();
    gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |bus| {
        let Ok(bus) = bus else { return callback(None) };
        bus.call(Some("org.waydock.Waydock"), "/org/waydock/Settings", "org.waydock.Settings", "GetPopupPlacement",
                 Some(&(connector, group).to_variant()), Some(glib::VariantTy::new("(s)").unwrap()),
                 gio::DBusCallFlags::NO_AUTO_START, 500, None::<&gio::Cancellable>, move |reply| {
                     let text = reply.ok().and_then(|v| v.get::<(String,)>()).map(|v| v.0);
                     callback(text.as_deref().and_then(Placement::from_json));
                 });
    });
}

fn transparent_area(width: i32, height: i32) -> gtk::DrawingArea {
    // An empty transparent GTK4 widget attaches no Wayland buffer. A Cairo
    // node creates a real surface.
    let area = gtk::DrawingArea::builder().content_width(width).content_height(height).build();
    area.set_draw_func(|_, cr, _, _| {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
    });
    area
}

/// Give a realized window's surface an empty input region (input passes through).
pub fn pass_input(window: &impl IsA<gtk::Native>) {
    if let Some(surface) = window.surface() {
        surface.set_input_region(Some(&gtk::cairo::Region::create()));
    }
}

struct ProbeInner {
    callback: Option<Box<dyn FnOnce(Option<gdk::Monitor>)>>,
    window: Option<gtk::Window>,
    timer: Option<glib::SourceId>,
    idle: Option<glib::SourceId>,
    css: Option<gtk::CssProvider>,
}

/// A fresh input-transparent surface selects labwc's cursor output.
///
/// Waits for wl_surface.enter, rather than guessing from keyboard focus or
/// querying a surface before its output events have arrived. Calls back once
/// with the entered monitor, or `None` after 1 s / without layer shell.
#[derive(Clone)]
pub struct PointerMonitor(Rc<RefCell<ProbeInner>>);

impl PointerMonitor {
    pub fn new(callback: impl FnOnce(Option<gdk::Monitor>) + 'static) -> Self {
        let probe = Self(Rc::new(RefCell::new(ProbeInner {
            callback: Some(Box::new(callback)), window: None, timer: None, idle: None, css: None })));
        if !gtk4_layer_shell::is_supported() {
            let weak = Rc::downgrade(&probe.0);
            let id = glib::idle_add_local_once(move || if let Some(inner) = weak.upgrade() {
                inner.borrow_mut().idle = None;
                PointerMonitor(inner).finish(None)
            });
            probe.0.borrow_mut().idle = Some(id);
            return probe;
        }
        let window = gtk::Window::builder().decorated(false).resizable(false).build();
        window.add_css_class("suite-output-probe");
        let css = add_css("window.suite-output-probe { background: transparent; box-shadow: none; border: none; min-width: 0; min-height: 0; }",
                          None, crate::utils::PRIORITY_APPLICATION);
        window.set_default_size(1, 1);
        window.set_child(Some(&transparent_area(1, 1)));
        window.init_layer_shell();
        window.set_namespace(Some("suite-output-probe"));
        window.set_layer(Layer::Overlay);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        let weak = Rc::downgrade(&probe.0);
        window.connect_realize(move |window| {
            pass_input(window);
            let weak = weak.clone();
            if let Some(surface) = window.surface() {
                surface.connect_enter_monitor(move |_, monitor| {
                    let Some(inner) = weak.upgrade() else { return };
                    if inner.borrow().idle.is_none() {
                        let (weak, monitor) = (Rc::downgrade(&inner), monitor.clone());
                        let id = glib::idle_add_local_once(move || {
                            if let Some(inner) = weak.upgrade() {
                                inner.borrow_mut().idle = None;
                                PointerMonitor(inner).finish(Some(monitor))
                            }
                        });
                        inner.borrow_mut().idle = Some(id);
                    }
                });
            }
        });
        window.present();
        let weak = Rc::downgrade(&probe.0);
        let timer = glib::timeout_add_local_once(Duration::from_millis(1000), move || {
            if let Some(inner) = weak.upgrade() {
                inner.borrow_mut().timer = None;
                PointerMonitor(inner).finish(None);
            }
        });
        let mut inner = probe.0.borrow_mut();
        inner.window = Some(window);
        inner.timer = Some(timer);
        inner.css = Some(css);
        drop(inner);
        probe
    }

    fn finish(&self, monitor: Option<gdk::Monitor>) {
        let callback = self.0.borrow_mut().callback.take();
        self.cancel();
        if let Some(callback) = callback {
            callback(monitor);
        }
    }

    /// Drop the callback, timers, probe window and its stylesheet.
    pub fn cancel(&self) {
        let (timer, idle, window, css) = {
            let mut inner = self.0.borrow_mut();
            inner.callback = None;
            (inner.timer.take(), inner.idle.take(), inner.window.take(), inner.css.take())
        };
        for source in [timer, idle].into_iter().flatten() {
            source.remove();
        }
        if let Some(window) = window {
            window.destroy();
        }
        if let (Some(css), Some(display)) = (css, gdk::Display::default()) {
            remove_css(&display, &css);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_reply_validation() {
        assert_eq!(Placement::from_json(r#"{"edge": "top", "align": "start", "offset": 48}"#),
                   Some(Placement { edge: "top".into(), align: "start".into(), offset: 48 }));
        for invalid in [r#"{"edge": "middle", "align": "start", "offset": 1}"#, r#"{"edge": "top", "align": "left", "offset": 1}"#,
                        r#"{"edge": "top", "align": "end", "offset": 4097}"#, r#"{"edge": "top", "align": "end", "offset": -1}"#,
                        r#"{"edge": "top", "align": "end", "offset": 1.0}"#, r#"{"edge": "top", "align": "end", "offset": true}"#,
                        "[]", "nope"] {
            assert_eq!(Placement::from_json(invalid), None, "{invalid}");
        }
    }
}
