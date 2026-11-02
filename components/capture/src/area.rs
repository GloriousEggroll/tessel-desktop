//! Invisible cursor-monitor probe and drag selection on Wayland (capture_area.py).
//! Layer namespace `tessel-capture-selection`.

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tessel_core::tr;

pub type Rectangle = (f64, f64, f64, f64);
pub type Selection = Option<(gdk::Monitor, Option<Rectangle>)>;

/// Python 3 `round()` (half to even).
pub fn py_round(value: f64) -> i64 {
    let floor = value.floor();
    let difference = value - floor;
    let result = if difference > 0.5 {
        floor + 1.0
    } else if difference < 0.5 {
        floor
    } else if floor % 2.0 == 0.0 {
        floor
    } else {
        floor + 1.0
    };
    result as i64
}

/// Logical selection → native pixel crop `(x, y, w, h)`.
pub fn pixel_rectangle(rectangle: Rectangle, logical_size: (i32, i32), width: u32, height: u32) -> Result<(i64, i64, i64, i64), String> {
    let (x, y, w, h) = rectangle;
    let (lw, lh) = (logical_size.0 as f64, logical_size.1 as f64);
    let (width, height) = (width as f64, height as f64);
    let (mut left, mut top) = (py_round(x * width / lw), py_round(y * height / lh));
    let (mut right, mut bottom) = (py_round((x + w) * width / lw), py_round((y + h) * height / lh));
    left = left.max(0);
    top = top.max(0);
    right = right.min(width as i64);
    bottom = bottom.min(height as i64);
    if right - left < 2 || bottom - top < 2 {
        return Err(tr!("Select a larger area."));
    }
    Ok((left, top, right - left, bottom - top))
}

type Callback = Box<dyn FnOnce(Selection)>;

pub struct Inner {
    callback: RefCell<Option<Callback>>,
    pub area: bool,
    done: Cell<bool>,
    pub windows: RefCell<Vec<(gtk::Window, gtk::DrawingArea, gdk::Monitor)>>,
    origin: Cell<Option<(f64, f64)>>,
    pub rectangle: Cell<Option<Rectangle>>,
    monitor: RefCell<Option<gdk::Monitor>>,
    timer: RefCell<Option<glib::SourceId>>,
    css: gtk::CssProvider,
    display: gdk::Display,
}

#[derive(Clone)]
pub struct ScreenSelection(pub Rc<Inner>);

impl ScreenSelection {
    pub fn new(display: &gdk::Display, callback: impl FnOnce(Selection) + 'static, area: bool) -> Result<Self, String> {
        if !gtk4_layer_shell::is_supported() {
            return Err(tr!("Screen selection requires a Wayland layer-shell desktop."));
        }
        let css = tessel_gtk::utils::add_css("window.capture-selection { background: transparent; box-shadow: none; }", Some(display),
                                            tessel_gtk::utils::PRIORITY_USER + 100);
        let this = Self(Rc::new(Inner {
            callback: RefCell::new(Some(Box::new(callback))), area, done: Cell::new(false), windows: RefCell::default(),
            origin: Cell::new(None), rectangle: Cell::new(None), monitor: RefCell::default(), timer: RefCell::default(), css,
            display: display.clone(),
        }));
        if !area {
            this.probe_monitor();
            return Ok(this);
        }
        let monitors = display.monitors();
        for index in 0..monitors.n_items() {
            let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() else { continue };
            let window = gtk::Window::builder().decorated(false).build();
            window.add_css_class("capture-selection");
            window.init_layer_shell();
            window.set_namespace(Some("tessel-capture-selection"));
            window.set_monitor(Some(&monitor));
            window.set_layer(Layer::Overlay);
            window.set_exclusive_zone(-1);
            window.set_keyboard_mode(KeyboardMode::Exclusive);
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
            let view = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
            let weak = Rc::downgrade(&this.0);
            let draw_monitor = monitor.clone();
            view.set_draw_func(move |widget, context, width, height| {
                if let Some(inner) = weak.upgrade() {
                    ScreenSelection(inner).draw(widget, context, width, height, &draw_monitor);
                }
            });
            window.set_child(Some(&view));
            view.set_cursor_from_name(Some("crosshair"));
            let drag = gtk::GestureDrag::builder().button(1).build();
            let (weak, begin_monitor) = (Rc::downgrade(&this.0), monitor.clone());
            drag.connect_drag_begin(move |_, x, y| {
                if let Some(inner) = weak.upgrade() {
                    ScreenSelection(inner).begin(&begin_monitor, x, y);
                }
            });
            let weak = Rc::downgrade(&this.0);
            drag.connect_drag_update(move |_, x, y| {
                if let Some(inner) = weak.upgrade() {
                    ScreenSelection(inner).drag(x, y);
                }
            });
            let weak = Rc::downgrade(&this.0);
            drag.connect_drag_end(move |_, x, y| {
                if let Some(inner) = weak.upgrade() {
                    ScreenSelection(inner).end(x, y);
                }
            });
            view.add_controller(drag);
            let click = gtk::GestureClick::builder().button(3).build();
            let weak = Rc::downgrade(&this.0);
            click.connect_pressed(move |_, _, _, _| {
                if let Some(inner) = weak.upgrade() {
                    ScreenSelection(inner).finish(None, true);
                }
            });
            view.add_controller(click);
            let keys = gtk::EventControllerKey::new();
            let weak = Rc::downgrade(&this.0);
            keys.connect_key_pressed(move |_, key, _, _| match weak.upgrade() {
                Some(inner) if ScreenSelection(inner.clone()).key(key) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            window.add_controller(keys);
            this.0.windows.borrow_mut().push((window, view, monitor));
        }
        for (window, _, _) in this.0.windows.borrow().iter() {
            window.present();
        }
        this.start_timer(120);
        Ok(this)
    }

    fn start_timer(&self, seconds: u32) {
        let weak = Rc::downgrade(&self.0);
        *self.0.timer.borrow_mut() = Some(glib::timeout_add_seconds_local(seconds, move || {
            if let Some(inner) = weak.upgrade() {
                inner.timer.borrow_mut().take();
                ScreenSelection(inner).finish(None, true);
            }
            glib::ControlFlow::Break
        }));
    }

    /// With no output specified, labwc places a new layer surface on the cursor's
    /// monitor. An empty input region avoids depending on a fresh pointer-enter.
    fn probe_monitor(&self) {
        let window = gtk::Window::builder().decorated(false).resizable(false).build();
        window.add_css_class("capture-selection");
        window.set_default_size(1, 1);
        let view = gtk::DrawingArea::builder().content_width(1).content_height(1).build();
        view.set_draw_func(|_, _, _, _| {});
        window.set_child(Some(&view));
        window.init_layer_shell();
        window.set_namespace(Some("tessel-capture-selection"));
        window.set_layer(Layer::Overlay);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        let weak = Rc::downgrade(&self.0);
        window.connect_realize(move |widget| {
            let Some(surface) = widget.surface() else { return };
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
            // get_monitor_at_surface can return the first monitor before the
            // Wayland output-enter event arrives. Use the assigned output.
            let weak = weak.clone();
            surface.connect_enter_monitor(move |_, monitor| {
                let (weak, monitor) = (weak.clone(), monitor.clone());
                glib::idle_add_local_once(move || {
                    if let Some(inner) = weak.upgrade() {
                        ScreenSelection(inner).finish(Some((monitor, None)), true);
                    }
                });
            });
        });
        let monitor = self.0.display.monitors().item(0).and_downcast::<gdk::Monitor>();
        if let Some(monitor) = monitor {
            self.0.windows.borrow_mut().push((window.clone(), view, monitor));
        }
        window.present();
        self.start_timer(3);
    }

    pub fn key(&self, key: gdk::Key) -> bool {
        if key == gdk::Key::Escape {
            self.finish(None, true);
            return true;
        }
        false
    }

    pub fn begin(&self, monitor: &gdk::Monitor, x: f64, y: f64) {
        *self.0.monitor.borrow_mut() = Some(monitor.clone());
        self.0.origin.set(Some((x, y)));
        self.drag(0.0, 0.0);
    }

    pub fn drag(&self, dx: f64, dy: f64) {
        let Some((ax, ay)) = self.0.origin.get() else { return };
        let Some(monitor) = self.0.monitor.borrow().clone() else { return };
        let geometry = monitor.geometry();
        let bx = (ax + dx).min(geometry.width() as f64).max(0.0);
        let by = (ay + dy).min(geometry.height() as f64).max(0.0);
        self.0.rectangle.set(Some((ax.min(bx), ay.min(by), (bx - ax).abs(), (by - ay).abs())));
        for (_, view, _) in self.0.windows.borrow().iter() {
            view.queue_draw();
        }
    }

    pub fn end(&self, dx: f64, dy: f64) {
        self.drag(dx, dy);
        if let Some(rectangle) = self.0.rectangle.get() {
            if rectangle.2.min(rectangle.3) >= 2.0 {
                let monitor = self.0.monitor.borrow().clone();
                if let Some(monitor) = monitor {
                    self.finish(Some((monitor, Some(rectangle))), true);
                }
            }
        }
    }

    #[allow(deprecated)]
    fn draw(&self, widget: &gtk::DrawingArea, context: &gtk::cairo::Context, width: i32, height: i32, monitor: &gdk::Monitor) {
        if !self.0.area {
            return;
        }
        context.set_source_rgba(0.0, 0.0, 0.0, 0.38);
        context.rectangle(0.0, 0.0, width as f64, height as f64);
        let current = self.0.monitor.borrow().clone();
        match self.0.rectangle.get() {
            Some((x, y, w, h)) if current.as_ref() == Some(monitor) => {
                context.rectangle(x, y, w, h);
                context.set_fill_rule(gtk::cairo::FillRule::EvenOdd);
                let _ = context.fill();
                match widget.style_context().lookup_color("theme_selected_bg_color") {
                    Some(accent) => context.set_source_rgba(accent.red() as f64, accent.green() as f64, accent.blue() as f64, 1.0),
                    None => context.set_source_rgb(1.0, 1.0, 1.0),
                }
                context.set_line_width(2.0);
                context.rectangle(x + 1.0, y + 1.0, (w - 2.0).max(0.0), (h - 2.0).max(0.0));
                let _ = context.stroke();
            }
            _ => {
                let _ = context.fill();
            }
        }
    }

    pub fn windows(&self) -> Vec<gtk::Window> {
        self.0.windows.borrow().iter().map(|(w, _, _)| w.clone()).collect()
    }

    pub fn is_done(&self) -> bool {
        self.0.done.get()
    }

    pub fn finish(&self, result: Selection, notify: bool) {
        if self.0.done.replace(true) {
            return;
        }
        if let Some(timer) = self.0.timer.borrow_mut().take() {
            timer.remove();
        }
        for (window, _, _) in self.0.windows.borrow().iter() {
            window.destroy();
        }
        tessel_gtk::utils::remove_css(&self.0.display, &self.0.css);
        let callback = self.0.callback.borrow_mut().take();
        if notify {
            if let Some(callback) = callback {
                callback(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_rectangle_scales_logical_selection() {
        assert_eq!(pixel_rectangle((40.0, 100.0, 60.0, 80.0), (1280, 720), 2560, 1440).unwrap(), (80, 200, 120, 160));
        assert_eq!(pixel_rectangle((-5.0, -5.0, 20.0, 20.0), (100, 100), 100, 100).unwrap(), (0, 0, 15, 15));
        assert!(pixel_rectangle((10.0, 10.0, 0.5, 30.0), (100, 100), 100, 100).is_err());
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(3.5), 4);
        assert_eq!(py_round(-0.5), 0);
    }
}
