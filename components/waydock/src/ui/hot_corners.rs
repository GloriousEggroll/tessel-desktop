//! hot_corners.py: per-output pointer dwell triggers for Wayshell Activities.
//!
//! Sensors (`waydock-hot-corner`, TOP layer, keyboard NONE, exclusive −1)
//! occupy only the outermost screen-edge pixels plus a 3×3 corner
//! ([`TRIGGER_SIZE`]) of a [`GUARD_LENGTH`]² window. An activation stays
//! latched across layer-surface occlusion (closing Activities with Escape
//! while the pointer stays in its corner must not reopen it).
//!
//! API: `HotCorners::new(application, activate, blocked, fullscreen)` →
//! `Rc<HotCorners>`; `configure(&config, &[(name, monitor)])`, `refresh()`
//! (cancel dwell when a native fullscreen window is focused — `fullscreen`
//! callback, no X11 probes), `raise_for_monitor(name)` (remap that output's
//! sensors 30 ms after a dock is promoted to TOP), `status()` (the `--check`
//! `hot_corners` JSON), `close()`, `delay_ms()`, `sensor_keys()`,
//! `sensor(name, corner)` (tests). Pure: [`CORNERS`], [`clamp_delay`],
//! [`inside`], [`input_rects`].

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use crate::core::config::DockConfig;

pub const CORNERS: [&str; 4] = ["top_left", "top_right", "bottom_left", "bottom_right"];
pub const TRIGGER_SIZE: i32 = 3;
pub const GUARD_LENGTH: i32 = 48;

/// `max(100, min(2000, int(config.hot_corner_delay_ms)))`.
pub fn clamp_delay(value: i64) -> u32 { value.clamp(100, 2000) as u32 }

/// Whether a pointer at (x, y) of a `width`×`height` sensor is in the trigger square.
pub fn inside(width: f64, height: f64, x: f64, y: f64, bottom: bool, right: bool) -> bool {
    let dx = if right { width - x } else { x };
    let dy = if bottom { height - y } else { y };
    (0.0..=TRIGGER_SIZE as f64).contains(&dx) && (0.0..=TRIGGER_SIZE as f64).contains(&dy)
}

/// The input region: the outermost column and row plus the 3×3 corner.
pub fn input_rects(width: i32, height: i32, bottom: bool, right: bool) -> [(i32, i32, i32, i32); 3] {
    let x = if right { width - 1 } else { 0 };
    let y = if bottom { height - 1 } else { 0 };
    [
        (x, 0, 1, height),
        (0, y, width, 1),
        (if right { width - TRIGGER_SIZE } else { 0 }, if bottom { height - TRIGGER_SIZE } else { 0 }, TRIGGER_SIZE, TRIGGER_SIZE),
    ]
}

pub type Activate = Rc<dyn Fn(&str) -> Result<(), String>>;
pub type Blocked = Rc<dyn Fn() -> Result<bool, String>>;

pub struct HotCorners {
    application: Option<gtk::Application>,
    activate_callback: Activate,
    blocked_callback: Blocked,
    fullscreen: Rc<dyn Fn() -> bool>,
    sensors: RefCell<BTreeMap<(String, String), Rc<Corner>>>,
    delay_ms: Cell<u32>,
    closed: Cell<bool>,
    error: RefCell<String>,
    raise_timers: RefCell<BTreeMap<String, glib::SourceId>>,
    this: Weak<HotCorners>,
}

impl HotCorners {
    pub fn new(application: Option<gtk::Application>, activate: Activate, blocked: Blocked,
               fullscreen: Rc<dyn Fn() -> bool>) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            application, activate_callback: activate, blocked_callback: blocked, fullscreen,
            sensors: RefCell::default(), delay_ms: Cell::new(150), closed: Cell::new(false),
            error: RefCell::default(), raise_timers: RefCell::default(), this: this.clone(),
        })
    }

    pub fn delay_ms(&self) -> u32 { self.delay_ms.get() }
    pub fn is_closed(&self) -> bool { self.closed.get() }
    pub fn error(&self) -> String { self.error.borrow().clone() }
    pub fn sensor_keys(&self) -> Vec<(String, String)> { self.sensors.borrow().keys().cloned().collect() }
    pub fn sensor(&self, monitor: &str, corner: &str) -> Option<Rc<Corner>> {
        self.sensors.borrow().get(&(monitor.to_owned(), corner.to_owned())).cloned()
    }

    pub fn configure(&self, config: &DockConfig, entries: &[(String, gdk::Monitor)]) {
        if self.closed.get() {
            return;
        }
        let delay = clamp_delay(config.hot_corner_delay_ms);
        let mut desired: Vec<((String, String), gdk::Monitor)> = Vec::new();
        for (name, monitor) in entries {
            for corner in config.hot_corners.get(name).map(|c| c.as_slice()).unwrap_or(&[]) {
                if CORNERS.contains(&corner.as_str()) {
                    let key = (name.clone(), corner.clone());
                    match desired.iter_mut().find(|(k, _)| *k == key) {
                        Some(entry) => entry.1 = monitor.clone(),
                        None => desired.push((key, monitor.clone())),
                    }
                }
            }
        }
        let existing: Vec<((String, String), Rc<Corner>)> =
            self.sensors.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (key, sensor) in existing {
            match desired.iter().find(|(k, _)| *k == key) {
                Some((_, monitor)) if sensor.monitor == *monitor => {
                    if delay != self.delay_ms.get() {
                        sensor.cancel();
                    }
                }
                _ => {
                    sensor.close();
                    self.sensors.borrow_mut().remove(&key);
                }
            }
        }
        self.delay_ms.set(delay);
        for ((name, corner), monitor) in desired {
            let key = (name.clone(), corner.clone());
            if !self.sensors.borrow().contains_key(&key) {
                let sensor = Corner::new(self.this.clone(), self.application.as_ref(), &name, &corner, &monitor);
                self.sensors.borrow_mut().insert(key, sensor);
            }
        }
        self.refresh();
    }

    /// Cancel pending dwell for native fullscreen focus, without X11 probes.
    pub fn refresh(&self) {
        if self.closed.get() {
            return;
        }
        if (self.fullscreen)() {
            for sensor in self.sensors() {
                sensor.cancel();
            }
        }
    }

    fn sensors(&self) -> Vec<Rc<Corner>> { self.sensors.borrow().values().cloned().collect() }

    fn blocked(&self) -> bool {
        match (self.blocked_callback)() {
            Ok(blocked) => blocked,
            Err(error) => {
                self.error.replace(error);
                true
            }
        }
    }

    fn activate(&self, monitor: &str) {
        match (self.activate_callback)(monitor) {
            Ok(()) => self.error.borrow_mut().clear(),
            Err(error) => {
                self.error.replace(error);
            }
        }
    }

    /// `status()` with Python's key order.
    pub fn status(&self) -> Value {
        let sensors = self.sensors.borrow();
        json!({
            "ready": !self.closed.get(),
            "enabled": sensors.len(),
            "delay_ms": self.delay_ms.get(),
            "pending": sensors.values().filter(|s| s.timer.borrow().is_some()).count(),
            "corners": sensors.keys().map(|(m, c)| json!({"monitor": m, "corner": c})).collect::<Vec<_>>(),
            "error": *self.error.borrow(),
        })
    }

    /// Keep sensors above a dock just promoted into the same TOP layer.
    pub fn raise_for_monitor(&self, monitor: &str) {
        if self.closed.get() {
            return;
        }
        if let Some(source) = self.raise_timers.borrow_mut().remove(monitor) {
            source.remove();
        }
        let (this, name) = (self.this.clone(), monitor.to_owned());
        // GTK4 commits the dock's layer change on its next frame; mapping
        // before that commit would let the promoted dock cover the sensor.
        let source = glib::timeout_add_local_once(Duration::from_millis(30), move || {
            let Some(this) = this.upgrade() else { return };
            this.raise_timers.borrow_mut().remove(&name);
            if !this.closed.get() {
                for sensor in this.sensors() {
                    if sensor.monitor_name == name {
                        sensor.window.set_visible(false);
                        sensor.window.set_visible(true);
                    }
                }
            }
        });
        self.raise_timers.borrow_mut().insert(monitor.to_owned(), source);
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        for (_, source) in std::mem::take(&mut *self.raise_timers.borrow_mut()) {
            source.remove();
        }
        for sensor in self.sensors() {
            sensor.close();
        }
        self.sensors.borrow_mut().clear();
    }
}

/// One `(monitor, corner)` sensor (`_Corner`).
pub struct Corner {
    owner: Weak<HotCorners>,
    pub monitor_name: String,
    pub corner: String,
    pub monitor: gdk::Monitor,
    bottom: bool,
    right: bool,
    timer: RefCell<Option<glib::SourceId>>,
    inside: Cell<bool>,
    on_surface: Cell<bool>,
    fired: Cell<bool>,
    closed: Cell<bool>,
    reentered: Cell<bool>,
    pub window: gtk::Window,
}

impl Corner {
    fn new(owner: Weak<HotCorners>, application: Option<&gtk::Application>, monitor_name: &str, corner: &str,
           monitor: &gdk::Monitor) -> Rc<Self> {
        let window = gtk::Window::new();
        if let Some(application) = application {
            window.set_application(Some(application));
        }
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_size_request(GUARD_LENGTH, GUARD_LENGTH);
        let style = gtk::CssProvider::new();
        style.load_from_string("window {background: transparent; border: 0; padding: 0; margin: 0;}");
        #[allow(deprecated)]
        window.style_context().add_provider(&style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 5);
        window.init_layer_shell();
        window.set_namespace(Some("waydock-hot-corner"));
        window.set_monitor(Some(monitor));
        window.set_layer(Layer::Top);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_exclusive_zone(-1);
        let (bottom, right) = (corner.starts_with("bottom"), corner.ends_with("right"));
        window.set_anchor(if bottom { Edge::Bottom } else { Edge::Top }, true);
        window.set_anchor(if right { Edge::Right } else { Edge::Left }, true);
        let sensor = Rc::new(Self {
            owner, monitor_name: monitor_name.to_owned(), corner: corner.to_owned(), monitor: monitor.clone(), bottom, right,
            timer: RefCell::new(None), inside: Cell::new(false), on_surface: Cell::new(false), fired: Cell::new(false),
            closed: Cell::new(false), reentered: Cell::new(false), window: window.clone(),
        });
        let controller = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&sensor);
        controller.connect_enter(move |_, x, y| if let Some(s) = weak.upgrade() { s.enter(x, y) });
        let weak = Rc::downgrade(&sensor);
        controller.connect_leave(move |_| if let Some(s) = weak.upgrade() { s.leave() });
        let weak = Rc::downgrade(&sensor);
        controller.connect_motion(move |_, x, y| if let Some(s) = weak.upgrade() { s.motion(x, y) });
        window.add_controller(controller);
        let weak = Rc::downgrade(&sensor);
        window.connect_realize(move |_| if let Some(s) = weak.upgrade() { s.input_region() });
        let area = gtk::DrawingArea::new();
        area.set_content_width(GUARD_LENGTH);
        area.set_content_height(GUARD_LENGTH);
        area.set_draw_func(|_, context, _, _| {
            context.set_operator(gtk::cairo::Operator::Source);
            context.set_source_rgba(0.0, 0.0, 0.0, 0.0);
            let _ = context.paint();
        });
        let weak = Rc::downgrade(&sensor);
        area.connect_resize(move |_, _, _| if let Some(s) = weak.upgrade() { s.input_region() });
        window.set_child(Some(&area));
        window.present();
        sensor
    }

    pub fn fired(&self) -> bool { self.fired.get() }
    pub fn pending(&self) -> bool { self.timer.borrow().is_some() }

    fn input_region(&self) {
        let Some(native) = self.window.surface() else { return };
        let (width, height) = (self.window.width(), self.window.height());
        let region = gtk::cairo::Region::create();
        for (x, y, w, h) in input_rects(width, height, self.bottom, self.right) {
            let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(x, y, w, h));
        }
        native.set_input_region(Some(&region));
    }

    pub fn enter(self: &Rc<Self>, x: f64, y: f64) {
        self.reentered.set(self.fired.get());
        self.on_surface.set(true);
        self.sample(x, y);
    }

    pub fn motion(self: &Rc<Self>, x: f64, y: f64) {
        self.on_surface.set(true);
        // A fresh physical motion after re-entry distinguishes a new approach
        // from an overlay uncovering a stationary pointer.
        if self.fired.get() && self.reentered.get() {
            self.fired.set(false);
            self.reentered.set(false);
        }
        self.sample(x, y);
    }

    pub fn leave(&self) {
        self.on_surface.set(false);
        self.inside.set(false);
        self.cancel();
    }

    fn sample(self: &Rc<Self>, x: f64, y: f64) {
        let (width, height) = (self.window.width() as f64, self.window.height() as f64);
        let inside = inside(width, height, x, y, self.bottom, self.right);
        self.inside.set(inside);
        if !inside {
            self.fired.set(false);
            self.reentered.set(false);
            self.cancel();
        } else if !self.fired.get() && self.timer.borrow().is_none() && !self.closed.get() {
            let Some(owner) = self.owner.upgrade() else { return };
            if owner.blocked() {
                self.fired.set(true);
            } else {
                let weak = Rc::downgrade(self);
                let source = glib::timeout_add_local_once(Duration::from_millis(owner.delay_ms() as u64), move || {
                    if let Some(sensor) = weak.upgrade() {
                        sensor.trigger();
                    }
                });
                self.timer.replace(Some(source));
            }
        }
    }

    fn trigger(&self) {
        self.timer.borrow_mut().take();
        if self.closed.get() || !self.on_surface.get() || !self.inside.get() || self.fired.get() {
            return;
        }
        self.fired.set(true);
        if let Some(owner) = self.owner.upgrade() {
            if !owner.blocked() {
                owner.activate(&self.monitor_name);
            }
        }
    }

    pub fn cancel(&self) {
        if let Some(source) = self.timer.borrow_mut().take() {
            source.remove();
        }
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.cancel();
        self.window.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_clamped_like_python() {
        assert_eq!(clamp_delay(5), 100);
        assert_eq!(clamp_delay(150), 150);
        assert_eq!(clamp_delay(99999), 2000);
    }

    #[test]
    fn trigger_square_per_corner() {
        assert!(inside(48.0, 48.0, 0.0, 0.0, false, false));
        assert!(inside(48.0, 48.0, 3.0, 3.0, false, false));
        assert!(!inside(48.0, 48.0, 4.0, 0.0, false, false));
        assert!(inside(48.0, 48.0, 47.0, 47.0, true, true));
        assert!(!inside(48.0, 48.0, 0.0, 47.0, true, true));
        assert!(inside(48.0, 48.0, 46.0, 1.0, false, true));
    }

    #[test]
    fn input_region_is_edge_strips_plus_corner() {
        assert_eq!(input_rects(48, 48, false, false), [(0, 0, 1, 48), (0, 0, 48, 1), (0, 0, 3, 3)]);
        assert_eq!(input_rects(48, 48, true, true), [(47, 0, 1, 48), (0, 47, 48, 1), (45, 45, 3, 3)]);
    }
}
