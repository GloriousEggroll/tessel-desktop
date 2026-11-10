//! `MonitorLayout(Gtk.Fixed)`: drag-arranged monitor shapes with edge
//! snapping (tessel-displays:1432-2231). Positions live in `LayoutModel`.

use crate::ipc;
use crate::model::{parse_transform, LayoutModel, Meta};
use crate::overlay::MonitorOverlay;
use crate::window::TesselDisplaysApp;
use glib::ControlFlow;
use gtk::prelude::*;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const LAYOUT_CSS: &str = r#"
        .monitor-frame {
            background-color: #333336;
            border: 1px solid #4a90e2;
            border-radius: 10px;
        }
        .overlay-label {
            font-size: 10px;
            color: white;
        }
        .snap-flash {
            border: 2px solid #00ffff;
            border-radius: 4px;
            transition: border 100ms ease-in-out;
        }
        .panel-bar {
            background-color: #4a90e2;
            border: 1px solid #4a90e2;
            border-bottom-left-radius: 10px;
            border-bottom-right-radius: 10px;
            border-top-left-radius: 0;
            border-top-right-radius: 0;
        }
        "#;

/// The widgets of one monitor shape.
#[derive(Clone)]
pub struct Shape {
    pub rotatable: gtk::Box,
    pub label: gtk::Label,
    pub flip_icon_t: gtk::Image,
    pub flip_icon_b: gtk::Image,
    pub panel_bar_top: gtk::Box,
    pub panel_bar_bottom: gtk::Box,
}

fn flip_icons_visible(angle: i64, flipped: bool) -> (bool, bool) {
    if !flipped {
        (false, false)
    } else if angle == 180 || angle == 270 {
        (false, true)
    } else {
        (true, false)
    }
}

pub struct MonitorLayout {
    pub fixed: gtk::Fixed,
    pub model: RefCell<LayoutModel>,
    shapes: RefCell<Vec<Shape>>,
    notebook: gtk::Notebook,
    desc_to_tab_index: BTreeMap<String, u32>,
    pub transform_overrides: Map<String, Value>,
    app: Weak<TesselDisplaysApp>,
    drag: RefCell<Option<gtk::Box>>,
}

impl MonitorLayout {
    #[allow(clippy::too_many_arguments)]
    pub fn new(monitors: &[Value], notebook: &gtk::Notebook, desc_to_tab_index: BTreeMap<String, u32>, align: Option<&str>,
               arrange: Option<&str>, order: Option<&[String]>, transform_overrides: Option<Map<String, Value>>,
               app: Weak<TesselDisplaysApp>) -> Rc<Self> {
        let fixed = gtk::Fixed::new();
        fixed.set_halign(gtk::Align::Center);
        let provider = gtk::CssProvider::new();
        provider.load_from_string(LAYOUT_CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        let transform_overrides = transform_overrides.unwrap_or_default();
        let model = LayoutModel::new(monitors, &ipc::cfg(), align, arrange, order, &transform_overrides);
        let this = Rc::new(Self {
            fixed, model: RefCell::new(LayoutModel::default()), shapes: RefCell::new(Vec::new()), notebook: notebook.clone(),
            desc_to_tab_index, transform_overrides, app, drag: RefCell::new(None),
        });
        let (width, height) = model.size_request;
        let mut shapes = Vec::new();
        for meta in &model.entries {
            let (angle, flipped) = parse_transform(&meta.transform.to_uppercase());
            let shape = this.build_shape(&meta.name, &meta.desc, meta.width, meta.height, angle, flipped);
            if meta.placed {
                this.fixed.put(&shape.rotatable, meta.x as f64, meta.y as f64);
            }
            shapes.push(shape);
        }
        if model.entries.iter().any(|m| m.placed) {
            this.fixed.set_size_request(width as i32, height as i32);
        }
        this.shapes.replace(shapes);
        this.model.replace(model);
        this.sync();
        this
    }

    fn build_shape(self: &Rc<Self>, name: &str, desc: &str, w: i64, h: i64, angle: i64, flipped: bool) -> Shape {
        let label = gtk::Label::new(Some(name));
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::Word);
        label.set_max_width_chars(20);
        label.set_halign(gtk::Align::Center);
        label.set_valign(gtk::Align::Center);
        label.set_hexpand(true);
        label.set_vexpand(true);
        let flip = |valign| {
            let icon = gtk::Image::from_icon_name("object-flip-horizontal-symbolic");
            icon.set_halign(gtk::Align::Center);
            icon.set_valign(valign);
            icon.set_widget_name("flip-indicator");
            icon.add_css_class("flip-indicator");
            icon.set_visible(false);
            icon
        };
        let (flip_icon_t, flip_icon_b) = (flip(gtk::Align::Start), flip(gtk::Align::End));
        let (top, bottom) = flip_icons_visible(angle, flipped);
        flip_icon_t.set_visible(top);
        flip_icon_b.set_visible(bottom);
        let bar = |valign| {
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bar.set_size_request(-1, 8);
            bar.set_halign(gtk::Align::Fill);
            bar.set_valign(valign);
            bar.set_hexpand(true);
            bar.set_vexpand(true);
            bar.add_css_class("panel-bar");
            bar.set_visible(false);
            bar
        };
        let (panel_bar_top, panel_bar_bottom) = (bar(gtk::Align::Start), bar(gtk::Align::End));
        let upside_down = angle == 180 || angle == 270;
        panel_bar_top.set_visible(upside_down);
        panel_bar_bottom.set_visible(!upside_down);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_halign(gtk::Align::Fill);
        content.set_valign(gtk::Align::Fill);
        content.set_hexpand(true);
        content.set_vexpand(true);
        content.append(&panel_bar_top);
        content.append(&panel_bar_bottom);
        let overlay = gtk::Overlay::new();
        overlay.set_halign(gtk::Align::Fill);
        overlay.set_valign(gtk::Align::Fill);
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        overlay.set_child(Some(&content));
        overlay.add_overlay(&label);
        overlay.add_overlay(&flip_icon_t);
        overlay.add_overlay(&flip_icon_b);
        let rotatable = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        rotatable.set_halign(gtk::Align::Start);
        rotatable.set_valign(gtk::Align::Start);
        rotatable.add_css_class("monitor-frame");
        rotatable.set_size_request(w as i32, h as i32);
        rotatable.append(&overlay);

        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        let target = desc.to_owned();
        click.connect_pressed(move |_, _, _, _| {
            if let Some(this) = weak.upgrade() {
                if let Some(index) = this.desc_to_tab_index.get(&target) {
                    this.notebook.set_current_page(Some(*index));
                }
            }
        });
        rotatable.add_controller(click);
        let drag = gtk::GestureDrag::new();
        let (weak, widget) = (Rc::downgrade(self), rotatable.clone());
        drag.connect_drag_begin(move |_, _, _| {
            if let Some(this) = weak.upgrade() {
                if this.index(&widget).is_some() {
                    this.drag.replace(Some(widget.clone()));
                }
            }
        });
        let (weak, widget) = (Rc::downgrade(self), rotatable.clone());
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(this) = weak.upgrade() {
                this.drag_update(&widget, dx, dy);
            }
        });
        let (weak, widget) = (Rc::downgrade(self), rotatable.clone());
        drag.connect_drag_end(move |_, _, _| {
            if let Some(this) = weak.upgrade() {
                this.drag_end(&widget);
            }
        });
        rotatable.add_controller(drag);
        Shape { rotatable, label, flip_icon_t, flip_icon_b, panel_bar_top, panel_bar_bottom }
    }

    fn index(&self, widget: &gtk::Box) -> Option<usize> {
        self.shapes.borrow().iter().position(|s| s.rotatable == *widget)
    }

    fn is_child(&self, widget: &gtk::Box) -> bool {
        widget.parent().is_some_and(|p| p == *self.fixed.upcast_ref::<gtk::Widget>())
    }

    /// Move every placed shape to its model position.
    fn sync(&self) {
        let model = self.model.borrow();
        for (meta, shape) in model.entries.iter().zip(self.shapes.borrow().iter()) {
            if self.is_child(&shape.rotatable) {
                self.fixed.move_(&shape.rotatable, meta.x as f64, meta.y as f64);
            }
        }
    }

    pub fn entries(&self) -> Vec<Meta> {
        self.model.borrow().entries.clone()
    }

    pub fn shape(&self, desc: &str) -> Option<Shape> {
        let index = self.model.borrow().index_of(desc)?;
        self.shapes.borrow().get(index).cloned()
    }

    pub fn with_meta(&self, desc: &str, f: impl FnOnce(&mut Meta)) {
        let mut model = self.model.borrow_mut();
        if let Some(index) = model.index_of(desc) {
            f(&mut model.entries[index]);
        }
    }

    fn drag_update(&self, widget: &gtk::Box, delta_x: f64, delta_y: f64) {
        let Some(index) = self.index(widget) else { return };
        let (current_x, current_y) = match widget.parent() {
            Some(parent) => widget.compute_point(&parent, &gtk::graphene::Point::new(0.0, 0.0))
                .map(|p| (p.x() as f64, p.y() as f64)).unwrap_or((0.0, 0.0)),
            None => (0.0, 0.0),
        };
        let (width, height) = {
            let model = self.model.borrow();
            (model.entries[index].width, model.entries[index].height)
        };
        let new_x = ((current_x + delta_x) as i64).min(self.fixed.width() as i64 - width).max(0);
        let new_y = ((current_y + delta_y) as i64).min(self.fixed.height() as i64 - height).max(0);
        if self.is_child(widget) {
            self.fixed.move_(widget, new_x as f64, new_y as f64);
        }
        let mut model = self.model.borrow_mut();
        model.entries[index].x = new_x;
        model.entries[index].y = new_y;
    }

    fn drag_end(&self, widget: &gtk::Box) {
        let Some(index) = self.index(widget) else { return };
        let (x, y, snapped) = {
            let model = self.model.borrow();
            let info = &model.entries[index];
            let others: Vec<_> = model.entries.iter().enumerate().filter(|(i, _)| *i != index)
                .map(|(_, d)| (d.x, d.y, d.width, d.height)).collect();
            (info.x, info.y, crate::model::snap_to_edges(info.x, info.y, &others, info.width, info.height))
        };
        if self.is_child(widget) {
            self.fixed.move_(widget, snapped.0 as f64, snapped.1 as f64);
        }
        {
            let mut model = self.model.borrow_mut();
            model.entries[index].x = snapped.0;
            model.entries[index].y = snapped.1;
        }
        if (x, y) != snapped {
            self.flash_widget_border(widget);
        }
        if let Some(app) = self.app.upgrade() {
            app.on_monitor_toggle_changed();
        }
    }

    /// Show `desc` with `transform` and the mode it has: `width`×`height` pixels
    /// (`None` keeps the mode it had).
    pub fn update_widget_transform(&self, desc: &str, transform: &str, mode: Option<(i64, i64)>) {
        self.with_meta(desc, |meta| {
            let (width, height) = mode.unwrap_or((meta.orig_width, meta.orig_height));
            meta.set_mode(width, height, transform);
        });
        self.apply_transform_to_shape(desc, transform);
    }

    /// `_apply_transform_to_shape(rotatable, transform)`: the shape takes its
    /// display's turned size, then every shape is placed again.
    pub fn apply_transform_to_shape(&self, desc: &str, transform: &str) {
        let Some(index) = self.model.borrow().index_of(desc) else { return };
        let shape = self.shapes.borrow()[index].clone();
        let (width, height) = {
            let model = self.model.borrow();
            (model.entries[index].width, model.entries[index].height)
        };
        let (angle, flipped) = parse_transform(transform);
        let vertical = angle == 90 || angle == 270;
        shape.rotatable.remove_css_class(if vertical { "horizontal" } else { "vertical" });
        shape.rotatable.add_css_class(if vertical { "vertical" } else { "horizontal" });
        shape.rotatable.set_size_request(width as i32, height as i32);
        let upside_down = angle == 180 || angle == 270;
        shape.panel_bar_top.set_visible(upside_down);
        shape.panel_bar_bottom.set_visible(!upside_down);
        let (top, bottom) = flip_icons_visible(angle, flipped);
        shape.flip_icon_t.set_visible(top);
        shape.flip_icon_b.set_visible(bottom);
        self.recenter_all_monitors();
        self.snap_monitors_to_edges();
    }

    pub fn recenter_all_monitors(&self) {
        let (width, height) = (self.fixed.width() as i64, self.fixed.height() as i64);
        self.model.borrow_mut().recenter(width, height, &ipc::cfg());
        self.sync();
    }

    pub fn snap_monitors_to_edges(&self) {
        self.model.borrow_mut().snap_monitors_to_edges();
        self.sync();
    }

    /// `add_monitor(monitor)`.
    pub fn add_monitor(self: &Rc<Self>, monitor: &Value) {
        let desc = tessel_core::pyjson::py_str(&monitor["DESCRIPTION"]);
        let duplicate = {
            let model = self.model.borrow();
            model.entries.iter().zip(self.shapes.borrow().iter())
                .any(|(meta, shape)| meta.desc == desc && self.is_child(&shape.rotatable))
        };
        if duplicate {
            return;
        }
        let meta = self.model.borrow().add_meta(monitor, &self.transform_overrides);
        let (angle, flipped) = parse_transform(&meta.transform.to_uppercase());
        let shape = self.build_shape(&meta.name, &meta.desc, meta.width, meta.height, angle, flipped);
        self.fixed.put(&shape.rotatable, meta.x as f64, meta.y as f64);
        self.shapes.borrow_mut().push(shape);
        self.model.borrow_mut().entries.push(meta);
    }

    pub fn remove_monitor(&self, desc: &str) {
        let Some(index) = self.model.borrow().index_of(desc) else { return };
        let shape = self.shapes.borrow_mut().remove(index);
        if self.is_child(&shape.rotatable) {
            self.fixed.remove(&shape.rotatable);
        }
        self.model.borrow_mut().entries.remove(index);
    }

    pub fn flash_widget_border(&self, widget: &gtk::Box) {
        widget.add_css_class("snap-flash");
        let widget = widget.clone();
        glib::timeout_add_local(Duration::from_millis(150), move || {
            widget.remove_css_class("snap-flash");
            ControlFlow::Break
        });
    }

    /// `show_overlay(desc, …, name)`: identify label on the matching output.
    pub fn show_overlay(&self, desc: &str, orig_width: i64, orig_height: i64, name: &str) -> Option<MonitorOverlay> {
        let cfg = ipc::cfg();
        let transform = cfg.get("TRANSFORM").filter(|v| tessel_core::pyjson::truthy(v)).and_then(|t| t.get(desc))
            .cloned().unwrap_or(Value::from(0));
        let (angle, _) = parse_transform(&crate::model::transform_text(Some(&transform)));
        let display = gtk::gdk::Display::default()?;
        let monitors = display.monitors();
        let monitor = (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast::<gtk::gdk::Monitor>())
            .find(|m| m.description().is_some_and(|d| d.contains(desc)))?;
        let overlay = MonitorOverlay::new(&monitor, name, desc, orig_width, orig_height, angle);
        overlay.present();
        Some(overlay)
    }
}
