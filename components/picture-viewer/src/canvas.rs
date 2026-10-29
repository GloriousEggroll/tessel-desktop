//! Port of picture_canvas.py: zoomable image canvas with panning, crop selection
//! and annotation gestures.

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use image::RgbaImage;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Zoom {
    /// `"fit"`, `"fill"` or `"actual"`.
    Mode(&'static str),
    Value(f64),
}

pub type Rectangle = [f64; 4];

pub struct Inner {
    pub area: gtk::DrawingArea,
    pub scroll: gtk::ScrolledWindow,
    surface: RefCell<Option<cairo::ImageSurface>>,
    pub image_size: Cell<(u32, u32)>,
    zoom_mode: Cell<&'static str>,
    zoom: Cell<f64>,
    tool: RefCell<String>,
    pub ratio: Cell<f64>,
    pub rectangle: Cell<Option<Rectangle>>,
    pub points: RefCell<Vec<(f64, f64)>>,
    origin: Cell<(f64, f64)>,
    start_point: Cell<(f64, f64)>,
    adjustments: Cell<(f64, f64)>,
    offset: Cell<(f64, f64)>,
    scale: Cell<f64>,
    changed: RefCell<Rc<dyn Fn(Rectangle)>>,
    annotate: RefCell<Rc<dyn Fn(&str, Vec<(f64, f64)>)>>,
    fullscreen: RefCell<Rc<dyn Fn()>>,
}

/// `PictureCanvas(scroll, changed, annotate, fullscreen)`.
#[derive(Clone)]
pub struct PictureCanvas(pub Rc<Inner>);

impl std::ops::Deref for PictureCanvas {
    type Target = Inner;
    fn deref(&self) -> &Inner { &self.0 }
}

/// Pillow `convert('RGBa').tobytes('raw', 'BGRa')`: premultiplied native-endian ARGB32.
pub fn premultiplied_bgra(image: &RgbaImage) -> Vec<u8> {
    let mut data = Vec::with_capacity(image.as_raw().len());
    for p in image.pixels() {
        let a = p[3] as u32;
        let mul = |v: u8| {
            let t = v as u32 * a + 128;
            ((t + (t >> 8)) >> 8) as u8
        };
        data.extend_from_slice(&[mul(p[2]), mul(p[1]), mul(p[0]), p[3]]);
    }
    data
}

impl PictureCanvas {
    pub fn new(scroll: &gtk::ScrolledWindow) -> Self {
        let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
        let canvas = Self(Rc::new(Inner {
            area: area.clone(), scroll: scroll.clone(), surface: RefCell::new(None), image_size: Cell::new((1, 1)),
            zoom_mode: Cell::new("fit"), zoom: Cell::new(1.0), tool: RefCell::new("pan".into()), ratio: Cell::new(0.0),
            rectangle: Cell::new(None), points: RefCell::new(Vec::new()), origin: Cell::new((0.0, 0.0)),
            start_point: Cell::new((0.0, 0.0)), adjustments: Cell::new((0.0, 0.0)), offset: Cell::new((0.0, 0.0)),
            scale: Cell::new(1.0), changed: RefCell::new(Rc::new(|_| {})), annotate: RefCell::new(Rc::new(|_, _| {})),
            fullscreen: RefCell::new(Rc::new(|| {})),
        }));
        let weak = Rc::downgrade(&canvas.0);
        area.set_draw_func(move |_, context, width, height| {
            if let Some(inner) = weak.upgrade() {
                PictureCanvas(inner).draw(context, width as f64, height as f64);
            }
        });
        area.set_focusable(true);
        let gesture = gtk::GestureDrag::builder().button(1).build();
        let weak = Rc::downgrade(&canvas.0);
        gesture.connect_drag_begin(move |_, x, y| if let Some(i) = weak.upgrade() { PictureCanvas(i).begin(x, y) });
        let weak = Rc::downgrade(&canvas.0);
        gesture.connect_drag_update(move |_, dx, dy| if let Some(i) = weak.upgrade() { PictureCanvas(i).drag(dx, dy) });
        let weak = Rc::downgrade(&canvas.0);
        gesture.connect_drag_end(move |_, dx, dy| if let Some(i) = weak.upgrade() { PictureCanvas(i).end(dx, dy) });
        area.add_controller(gesture);
        let click = gtk::GestureClick::builder().button(1).build();
        let weak = Rc::downgrade(&canvas.0);
        click.connect_pressed(move |_, count, x, y| if let Some(i) = weak.upgrade() { PictureCanvas(i).pressed(count, x, y) });
        area.add_controller(click);
        let zoom = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(&canvas.0);
        zoom.connect_scroll(move |controller, _, dy| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            if controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                let canvas = PictureCanvas(inner);
                canvas.set_zoom(Zoom::Value(canvas.scale() * if dy < 0.0 { 1.2 } else { 1.0 / 1.2 }));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        area.add_controller(zoom);
        canvas
    }

    pub fn connect(&self, changed: Rc<dyn Fn(Rectangle)>, annotate: Rc<dyn Fn(&str, Vec<(f64, f64)>)>, fullscreen: Rc<dyn Fn()>) {
        self.changed.replace(changed);
        self.annotate.replace(annotate);
        self.fullscreen.replace(fullscreen);
    }

    pub fn zoom_mode(&self) -> &'static str { self.zoom_mode.get() }
    pub fn scale(&self) -> f64 { self.scale.get() }
    pub fn tool(&self) -> String { self.tool.borrow().clone() }
    pub fn surface(&self) -> Option<cairo::ImageSurface> { self.surface.borrow().clone() }
    pub fn has_image(&self) -> bool { self.surface.borrow().is_some() }

    pub fn set_image(&self, image: &RgbaImage) {
        let (width, height) = image.dimensions();
        let surface = cairo::ImageSurface::create_for_data(premultiplied_bgra(image), cairo::Format::ARgb32, width as i32,
                                                           height as i32, width as i32 * 4).ok();
        self.surface.replace(surface);
        self.image_size.set((width, height));
        self.rectangle.set(None);
        self.points.borrow_mut().clear();
        let mode = self.zoom_mode.get();
        self.set_zoom(if mode != "custom" { Zoom::Mode(mode) } else { Zoom::Value(self.zoom.get()) });
    }

    pub fn clear(&self) {
        self.surface.replace(None);
        self.rectangle.set(None);
        self.points.borrow_mut().clear();
        self.area.set_content_width(1);
        self.area.set_content_height(1);
        self.area.queue_draw();
    }

    pub fn set_zoom(&self, value: Zoom) {
        match value {
            Zoom::Mode(mode) => {
                self.zoom_mode.set(mode);
                if mode == "actual" {
                    self.zoom.set(1.0);
                }
            }
            Zoom::Value(zoom) => {
                self.zoom_mode.set("custom");
                self.zoom.set(zoom.clamp(0.02, 16.0));
            }
        }
        if matches!(self.zoom_mode.get(), "custom" | "actual") {
            let (w, h) = self.image_size.get();
            self.area.set_content_width((w as f64 * self.zoom.get()).round_ties_even() as i32);
            self.area.set_content_height((h as f64 * self.zoom.get()).round_ties_even() as i32);
        } else {
            self.area.set_content_width(1);
            self.area.set_content_height(1);
        }
        self.area.queue_draw();
    }

    pub fn set_tool(&self, tool: &str) {
        self.tool.replace(tool.into());
        self.rectangle.set(None);
        self.points.borrow_mut().clear();
        self.area.set_cursor_from_name(Some(if tool == "pan" { "grab" } else { "crosshair" }));
        self.area.queue_draw();
    }

    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        let (w, h) = self.image_size.get();
        let (ox, oy) = self.offset.get();
        let scale = self.scale.get();
        (((x - ox) / scale).clamp(0.0, w as f64), ((y - oy) / scale).clamp(0.0, h as f64))
    }

    fn begin(&self, x: f64, y: f64) {
        self.area.grab_focus();
        self.origin.set((x, y));
        let start = self.point(x, y);
        self.start_point.set(start);
        self.points.replace(vec![start]);
        self.adjustments.set((self.scroll.hadjustment().value(), self.scroll.vadjustment().value()));
    }

    fn drag(&self, dx: f64, dy: f64) {
        if self.surface.borrow().is_none() {
            return;
        }
        let tool = self.tool();
        if tool == "pan" {
            let (h, v) = self.adjustments.get();
            self.scroll.hadjustment().set_value(h - dx);
            self.scroll.vadjustment().set_value(v - dy);
            return;
        }
        let (ax, ay) = self.start_point.get();
        let (ox, oy) = self.origin.get();
        let (mut bx, mut by) = self.point(ox + dx, oy + dy);
        if tool == "pen" {
            self.points.borrow_mut().push((bx, by));
        } else {
            let ratio = self.ratio.get();
            if ratio != 0.0 && tool == "crop" {
                let width = (bx - ax).abs().min((by - ay).abs() * ratio);
                bx = ax + width * if bx >= ax { 1.0 } else { -1.0 };
                by = ay + width / ratio * if by >= ay { 1.0 } else { -1.0 };
            }
            self.points.replace(vec![(ax, ay), (bx, by)]);
        }
        let rectangle = [ax.min(bx), ay.min(by), (bx - ax).abs(), (by - ay).abs()];
        self.rectangle.set(Some(rectangle));
        let changed = self.changed.borrow().clone();
        changed(rectangle);
        self.area.queue_draw();
    }

    fn end(&self, dx: f64, dy: f64) {
        self.drag(dx, dy);
        let tool = self.tool();
        let points = self.points.borrow().clone();
        if !matches!(tool.as_str(), "pan" | "crop" | "red-eye" | "text") && points.len() > 1 {
            let annotate = self.annotate.borrow().clone();
            annotate(&tool, points);
        }
    }

    fn pressed(&self, count: i32, x: f64, y: f64) {
        let tool = self.tool();
        if tool == "text" && count == 1 {
            let point = self.point(x, y);
            let annotate = self.annotate.borrow().clone();
            annotate("text", vec![point, point]);
        } else if count == 2 && tool == "pan" {
            let fullscreen = self.fullscreen.borrow().clone();
            fullscreen();
        }
    }

    fn draw(&self, context: &cairo::Context, width: f64, height: f64) {
        // A neutral viewing surface keeps the wallpaper accent from tinting
        // photos; surrounding controls still use the suite palette and blur.
        context.set_source_rgb(0.095, 0.095, 0.10);
        let _ = context.paint();
        let Some(surface) = self.surface.borrow().clone() else { return };
        let (iw, ih) = self.image_size.get();
        let (iw, ih) = (iw as f64, ih as f64);
        let fit = (width / iw).min(height / ih);
        let scale = match self.zoom_mode.get() {
            "fit" => fit,
            "fill" => (width / iw).max(height / ih),
            _ => self.zoom.get(),
        };
        self.scale.set(scale);
        let offset = ((width - iw * scale) / 2.0, (height - ih * scale) / 2.0);
        self.offset.set(offset);
        let _ = context.save();
        context.translate(offset.0, offset.1);
        context.scale(scale, scale);
        // Checkerboard appears only through transparent image pixels.
        let cell = (12.0 / scale).max(1.0);
        context.set_source_rgb(0.24, 0.24, 0.25);
        context.rectangle(0.0, 0.0, iw, ih);
        let _ = context.fill();
        context.set_source_rgb(0.3, 0.3, 0.31);
        // Clip loops to the visible viewport, even at high zoom.
        let (left, top, right, bottom) = context.clip_extents().unwrap_or((0.0, 0.0, iw, ih));
        let rows = ((top / cell).max(0.0) as i64, ((ih / cell) as i64 + 1).min((bottom / cell) as i64 + 1));
        let columns = ((left / cell).max(0.0) as i64, ((iw / cell) as i64 + 1).min((right / cell) as i64 + 1));
        for y in rows.0..rows.1 {
            for x in columns.0..columns.1 {
                if (x + y) % 2 == 1 {
                    let (cx, cy) = (x as f64 * cell, y as f64 * cell);
                    context.rectangle(cx, cy, cell.min(iw - cx), cell.min(ih - cy));
                }
            }
        }
        let _ = context.fill();
        let _ = context.set_source_surface(&surface, 0.0, 0.0);
        let _ = context.paint();
        let tool = self.tool();
        if let (Some([x, y, w, h]), true) = (self.rectangle.get(), tool != "pan") {
            if tool == "crop" || tool == "red-eye" {
                context.set_fill_rule(cairo::FillRule::EvenOdd);
                context.rectangle(0.0, 0.0, iw, ih);
                context.rectangle(x, y, w, h);
                context.set_source_rgba(0.0, 0.0, 0.0, 0.45);
                let _ = context.fill();
            }
            context.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            context.set_line_width(1.5 / scale);
            if matches!(tool.as_str(), "line" | "arrow" | "pen") {
                let points = self.points.borrow();
                if let Some((px, py)) = points.first() {
                    context.move_to(*px, *py);
                    for (px, py) in points.iter().skip(1) {
                        context.line_to(*px, *py);
                    }
                }
            } else {
                context.rectangle(x, y, w, h);
            }
            let _ = context.stroke();
        }
        let _ = context.restore();
    }
}
