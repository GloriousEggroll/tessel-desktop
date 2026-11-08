//! Port of overview.py: preview matching, the Activities layout, the
//! in-process capture (replacing the `tessel-overview` helper process: same
//! frame order — desktop, identities, then windows — and the same bounds:
//! ≤ 1200 ms per frame, 10 s for windows, 12 s overall, desktop ≤ 1600×1000,
//! windows ≤ 640×400, NORMAL transform only) and the animated `Stage`.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, gsk};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use tessel_wayland::client::CaptureId;
use tessel_wayland::model::HandleKey;
use tessel_wayland::{Client, Event, Frame, GlibClient, Protocols};

pub type Key = (String, String);

pub fn preview_key(app_id: &str, title: &str) -> Key {
    (app_id.to_string(), title.to_string())
}

/// `unique_preview_target(frame, items, identities)`; `items` are (id, app_id, title).
pub fn unique_preview_target<'a>(frame: &Key, items: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>, identities: &HashMap<Key, usize>) -> Option<String> {
    if identities.get(frame).copied() != Some(1) {
        return None;
    }
    let matches: Vec<&str> = items.into_iter().filter(|(_, a, t)| *a == frame.0 && *t == frame.1).map(|(id, _, _)| id).collect();
    (matches.len() == 1).then(|| matches[0].to_string())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub columns: i32,
    pub content_height: i32,
    pub preview_height: i32,
}

fn floordiv(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// Python `round()` (half to even).
fn py_round(value: f64) -> i64 {
    let r = value.round();
    if (value - value.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 {
        (r - value.signum()) as i64
    } else {
        r as i64
    }
}

pub fn overview_layout(width: i32, height: i32, count: usize) -> Layout {
    let width = (width as i64 - 80).max(1);
    let height = (height as i64 - 80).max(1);
    let count = count as i64;
    let desired = py_round(((count.max(1) * width) as f64 / height.max(1) as f64 / 1.8).sqrt());
    let columns = count.max(1).min(desired.max(1)).min(floordiv(width, 300).max(1)).min(4);
    let cell = floordiv(width - 40 - 20 * (columns - 1), columns).max(100);
    let rows = (count.max(1) + columns - 1) / columns;
    let height_per_row = floordiv(height - 110 - 20 * (rows - 1), rows) - 76;
    let preview = ((cell as f64 * 0.60) as i64).min(height_per_row).max(90).min(320);
    Layout {
        width: width as i32, height: height.min(rows * (preview + 86) + 100) as i32, columns: columns as i32,
        content_height: (height - 100).max(80) as i32, preview_height: preview as i32,
    }
}

/// `layout_limits` (tessel_shell_switcher_ui.py:20) for keyboard mode.
pub fn layout_limits(width: i32, height: i32, count: usize) -> Layout {
    let available_width = (width as i64 - 48).max(1);
    let available_height = (height as i64 - 48).max(1);
    let count = count as i64;
    let columns = count.max(1).min(4).min(floordiv(available_width - 36, 176).max(1));
    let panel_width = available_width.min(800).min((columns * 176 + 36).max(360));
    let content_height = (available_height - 118).max(32).min(444);
    let natural_height = ((count.max(1) + columns - 1) / columns) * 128 + 118;
    Layout {
        width: panel_width as i32, height: available_height.min(natural_height).min(content_height + 118) as i32,
        columns: columns as i32, content_height: content_height as i32, preview_height: -1,
    }
}

// ------------------------------------------------------------------ capture

pub enum CaptureFrame {
    Desktop(gdk::Texture),
    Identities(Vec<Key>),
    Window { key: Key, texture: gdk::Texture },
}

/// Scale a frame with cairo (bilinear) into a texture no larger than `limit`.
pub fn thumbnail(frame: &Frame, limit: (f64, f64)) -> Option<gdk::Texture> {
    use gtk::cairo;
    if frame.transform != 0 || frame.width == 0 || frame.height == 0 {
        return None;
    }
    let ratio = (limit.0 / frame.width as f64).min(limit.1 / frame.height as f64).min(1.0);
    let width = ((frame.width as f64 * ratio).round() as i32).max(1);
    let height = ((frame.height as f64 * ratio).round() as i32).max(1);
    if ratio >= 1.0 {
        return Some(frame.to_texture());
    }
    let format = if frame.has_alpha() { cairo::Format::ARgb32 } else { cairo::Format::Rgb24 };
    let source = cairo::ImageSurface::create_for_data(frame.data.clone(), format, frame.width as i32, frame.height as i32, frame.stride as i32).ok()?;
    let mut small = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    {
        let cr = cairo::Context::new(&small).ok()?;
        cr.scale(width as f64 / frame.width as f64, height as f64 / frame.height as f64);
        cr.set_source_surface(&source, 0., 0.).ok()?;
        cr.source().set_filter(cairo::Filter::Bilinear);
        cr.paint().ok()?;
    }
    small.flush();
    let stride = small.stride() as usize;
    let data = small.data().ok()?.to_vec();
    let bytes = glib::Bytes::from_owned(data);
    Some(gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, stride).upcast())
}

enum Step {
    Desktop,
    Windows,
}

struct CaptureInner {
    client: RefCell<Option<GlibClient>>,
    callback: RefCell<Option<Box<dyn Fn(CaptureFrame)>>>,
    finished: RefCell<Option<Box<dyn FnOnce()>>>,
    pending: RefCell<Option<(CaptureId, Option<HandleKey>, glib::SourceId)>>,
    queue: RefCell<Vec<HandleKey>>,
    step: RefCell<Step>,
    window_deadline: Cell<Option<Instant>>,
    overall: RefCell<Option<glib::SourceId>>,
    closed: Cell<bool>,
}

/// `OverviewCapture(callback, finished)`: `start(connector)`, `close()`.
#[derive(Clone)]
pub struct OverviewCapture(Rc<CaptureInner>);

impl OverviewCapture {
    pub fn new(callback: impl Fn(CaptureFrame) + 'static, finished: impl FnOnce() + 'static) -> Self {
        Self(Rc::new(CaptureInner {
            client: RefCell::new(None), callback: RefCell::new(Some(Box::new(callback))),
            finished: RefCell::new(Some(Box::new(finished))), pending: RefCell::new(None), queue: RefCell::new(Vec::new()),
            step: RefCell::new(Step::Desktop), window_deadline: Cell::new(None), overall: RefCell::new(None), closed: Cell::new(false),
        }))
    }

    pub fn is_closed(&self) -> bool {
        self.0.closed.get()
    }

    fn finish(&self) {
        self.close();
        if let Some(finished) = self.0.finished.borrow_mut().take() {
            finished();
        }
    }

    fn emit(&self, frame: CaptureFrame) {
        let callback = self.0.callback.borrow();
        if let Some(callback) = callback.as_ref() {
            callback(frame);
        }
    }

    pub fn start(&self, connector: &str) {
        let Ok(mut client) = Client::connect(Protocols::CAPTURE) else {
            self.finish();
            return;
        };
        client.keep_closed_listed = true;
        client.listed_limit = Some(128);
        if client.roundtrip().is_err() || client.roundtrip().is_err() || !client.can_capture_toplevels() {
            self.finish();
            return;
        }
        let _ = client.take_events();
        let weak = Rc::downgrade(&self.0);
        let glib_client = GlibClient::attach(client, None, move |_, event| {
            if let Some(inner) = weak.upgrade() {
                OverviewCapture(inner).event(event);
            }
        });
        *self.0.client.borrow_mut() = Some(glib_client.clone());
        let weak = Rc::downgrade(&self.0);
        *self.0.overall.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_secs(12), move || {
            if let Some(inner) = weak.upgrade() {
                inner.overall.borrow_mut().take();
                OverviewCapture(inner).finish();
            }
        }));
        let desktop = glib_client.with(|c| {
            if !connector.is_empty() && c.can_capture_outputs() && c.model.output_by_name(connector).is_some() {
                c.capture_output(connector, false).ok()
            } else {
                None
            }
        });
        match desktop {
            Some(id) => self.wait_for(id, None),
            None => self.begin_windows(),
        }
    }

    fn wait_for(&self, id: CaptureId, key: Option<HandleKey>) {
        let weak = Rc::downgrade(&self.0);
        let timer = glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            if let Some(inner) = weak.upgrade() {
                let this = OverviewCapture(inner);
                if let Some((id, _, _)) = this.0.pending.borrow_mut().take() {
                    if let Some(c) = this.0.client.borrow().as_ref() {
                        c.with(|c| c.cancel_capture(id));
                    }
                }
                this.next();
            }
        });
        *self.0.pending.borrow_mut() = Some((id, key, timer));
    }

    fn begin_windows(&self) {
        if self.0.closed.get() {
            return;
        }
        *self.0.step.borrow_mut() = Step::Windows;
        let client = self.0.client.borrow().clone();
        let Some(client) = client else { return };
        let (identities, keys) = {
            let c = client.borrow();
            let listed: Vec<_> = c.model.listed().iter().filter(|t| !t.closed).collect();
            (listed.iter().map(|t| preview_key(t.app_id.as_deref().unwrap_or(""), t.title.as_deref().unwrap_or(""))).collect::<Vec<_>>(),
             c.model.listed().iter().map(|t| t.key).collect::<Vec<_>>())
        };
        self.emit(CaptureFrame::Identities(identities));
        let mut queue = keys;
        queue.reverse();
        *self.0.queue.borrow_mut() = queue;
        self.0.window_deadline.set(Some(Instant::now() + Duration::from_secs(10)));
        self.next_window();
    }

    fn next(&self) {
        if self.0.closed.get() {
            return;
        }
        let step = matches!(*self.0.step.borrow(), Step::Desktop);
        if step {
            self.begin_windows();
        } else {
            self.next_window();
        }
    }

    fn next_window(&self) {
        loop {
            if self.0.closed.get() {
                return;
            }
            if self.0.window_deadline.get().is_some_and(|d| Instant::now() >= d) {
                self.finish();
                return;
            }
            let Some(key) = self.0.queue.borrow_mut().pop() else {
                self.finish();
                return;
            };
            let client = self.0.client.borrow().clone();
            let Some(client) = client else { return };
            let id = client.with(|c| {
                if c.model.listed_toplevel(key).is_some_and(|t| !t.closed) { c.capture_listed(key, false).ok() } else { None }
            });
            if let Some(id) = id {
                self.wait_for(id, Some(key));
                return;
            }
        }
    }

    fn event(&self, event: Event) {
        if self.0.closed.get() {
            return;
        }
        match event {
            Event::Captured(id, result) => {
                let pending = self.0.pending.borrow_mut().take();
                let Some((pending_id, key, timer)) = pending else { return };
                if pending_id != id {
                    *self.0.pending.borrow_mut() = Some((pending_id, key, timer));
                    return;
                }
                timer.remove();
                let client = self.0.client.borrow().clone();
                if let Some(client) = &client {
                    client.with(|c| c.cancel_capture(id));
                }
                if let (Ok(frame), Some(client)) = (result, client) {
                    match key {
                        None => {
                            if let Some(texture) = thumbnail(&frame, (1600., 1000.)) {
                                self.emit(CaptureFrame::Desktop(texture));
                            }
                        }
                        Some(key) => {
                            let listed = client.borrow().model.listed_toplevel(key).filter(|t| !t.closed).cloned();
                            if let Some(listed) = listed {
                                if let Some(texture) = thumbnail(&frame, (640., 400.)) {
                                    let k = preview_key(listed.app_id.as_deref().unwrap_or(""), listed.title.as_deref().unwrap_or(""));
                                    self.emit(CaptureFrame::Window { key: k, texture });
                                }
                            }
                        }
                    }
                }
                self.next();
            }
            Event::Changed(tessel_wayland::model::Event::ListedClosed(key)) => {
                // A window closing mid-capture never produces a (stale) image.
                let matches = self.0.pending.borrow().as_ref().is_some_and(|(_, k, _)| *k == Some(key));
                if matches {
                    if let Some((id, _, timer)) = self.0.pending.borrow_mut().take() {
                        timer.remove();
                        if let Some(c) = self.0.client.borrow().as_ref() {
                            c.with(|c| c.cancel_capture(id));
                        }
                    }
                    self.next();
                }
            }
            Event::Disconnected(_) => self.finish(),
            _ => {}
        }
    }

    /// Stop capturing and drop the connection (Python killed the helper).
    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        if let Some((_, _, timer)) = self.0.pending.borrow_mut().take() {
            timer.remove();
        }
        if let Some(source) = self.0.overall.borrow_mut().take() {
            source.remove();
        }
        if let Some(client) = self.0.client.borrow_mut().take() {
            client.close();
        }
        self.0.queue.borrow_mut().clear();
        self.0.callback.borrow_mut().take();
    }
}

// ------------------------------------------------------------------- Stage

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Stage {
        pub progress: Cell<f64>,
        pub desktop: RefCell<Option<gdk::Texture>>,
        pub wallpaper: RefCell<Option<gdk::Paintable>>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub start_time: Cell<i64>,
        pub child: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Stage {
        const NAME: &'static str = "TesselShellStage";
        type Type = super::Stage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for Stage {
        fn constructed(&self) {
            self.parent_constructed();
            self.progress.set(1.0);
        }
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Stage {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (width, height) = (widget.width() as f32, widget.height() as f32);
            if width <= 0. || height <= 0. {
                return;
            }
            let progress = self.progress.get();
            let eased = (1. - (1. - progress).powi(3)) as f32;
            if let Some(wallpaper) = self.wallpaper.borrow().as_ref() {
                let (w, h) = (wallpaper.intrinsic_width() as f32, wallpaper.intrinsic_height() as f32);
                let ratio = (width / w.max(1.)).max(height / h.max(1.));
                snapshot.save();
                snapshot.translate(&graphene::Point::new((width - w * ratio) / 2., (height - h * ratio) / 2.));
                wallpaper.snapshot(snapshot, (w * ratio) as f64, (h * ratio) as f64);
                snapshot.restore();
            }
            snapshot.save();
            let scale = 1. + 0.10 * (1. - eased);
            snapshot.translate(&graphene::Point::new(width * (1. - scale) / 2., height * (1. - scale) / 2.));
            snapshot.scale(scale, scale);
            snapshot.push_opacity(eased as f64);
            if let Some(child) = self.child.borrow().as_ref() {
                widget.snapshot_child(child, snapshot);
            }
            snapshot.pop();
            snapshot.restore();
            if let Some(desktop) = self.desktop.borrow().as_ref() {
                if eased < 1. {
                    snapshot.save();
                    let scale = 1. - 0.16 * eased;
                    snapshot.translate(&graphene::Point::new(width * (1. - scale) / 2., height * (1. - scale) / 2.));
                    snapshot.push_opacity((1. - eased) as f64);
                    desktop.snapshot(snapshot, (width * scale) as f64, (height * scale) as f64);
                    snapshot.pop();
                    snapshot.restore();
                }
            }
            let _ = gsk::ScalingFilter::Linear;
        }
    }
}

glib::wrapper! {
    pub struct Stage(ObjectSubclass<imp::Stage>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Stage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Stage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_child(&self, child: &impl IsA<gtk::Widget>) {
        if let Some(old) = self.imp().child.borrow_mut().take() {
            old.unparent();
        }
        child.set_parent(self);
        *self.imp().child.borrow_mut() = Some(child.clone().upcast());
    }

    pub fn progress(&self) -> f64 {
        self.imp().progress.get()
    }
    pub fn tick_active(&self) -> bool {
        self.imp().tick.borrow().is_some()
    }
    pub fn has_desktop(&self) -> bool {
        self.imp().desktop.borrow().is_some()
    }
    pub fn set_wallpaper(&self, paintable: Option<gdk::Paintable>) {
        *self.imp().wallpaper.borrow_mut() = paintable;
        self.queue_draw();
    }
    pub fn has_wallpaper(&self) -> bool {
        self.imp().wallpaper.borrow().is_some()
    }

    pub fn animate(&self, desktop: Option<gdk::Texture>) {
        self.stop();
        let imp = self.imp();
        *imp.desktop.borrow_mut() = desktop;
        if !self.settings().is_gtk_enable_animations() {
            imp.desktop.borrow_mut().take();
            return;
        }
        imp.progress.set(0.);
        imp.start_time.set(0);
        let weak: Weak<()> = Weak::new();
        let _ = weak;
        let id = self.add_tick_callback(|stage, clock| {
            let imp = stage.imp();
            let now = clock.frame_time();
            if imp.start_time.get() == 0 {
                imp.start_time.set(now);
            }
            let progress = ((now - imp.start_time.get()) as f64 / 280000.).min(1.);
            imp.progress.set(progress);
            stage.queue_draw();
            if progress >= 1. {
                imp.desktop.borrow_mut().take();
                imp.tick.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *imp.tick.borrow_mut() = Some(id);
    }

    pub fn stop(&self) {
        let imp = self.imp();
        if let Some(tick) = imp.tick.borrow_mut().take() {
            tick.remove();
        }
        imp.progress.set(1.);
        imp.desktop.borrow_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_window_images_are_not_assigned_to_close_targets() {
        let frame = preview_key("test", "Untitled");
        let identities: HashMap<Key, usize> = [(frame.clone(), 1)].into();
        assert_eq!(unique_preview_target(&frame, [("a", "test", "Untitled")], &identities), Some("a".into()));
        assert_eq!(unique_preview_target(&frame, [("a", "test", "Untitled"), ("b", "test", "Untitled")], &identities), None);
        let twice: HashMap<Key, usize> = [(frame.clone(), 2)].into();
        assert_eq!(unique_preview_target(&frame, [("a", "test", "Untitled")], &twice), None);
        assert_eq!(unique_preview_target(&frame, [("a", "other", "Untitled")], &identities), None);
    }

    #[test]
    fn layout_keeps_previews_readable_on_small_and_large_monitors() {
        for (width, height) in [(640, 480), (1280, 720), (3840, 2160)] {
            for count in [0, 1, 4, 20, 100] {
                let layout = overview_layout(width, height, count);
                assert!(layout.width <= width);
                assert!(layout.height <= height);
                assert!(layout.preview_height >= 90);
            }
        }
    }

    #[test]
    fn layouts_match_python_values() {
        // Values from overview.overview_layout / tessel_shell_switcher_ui.layout_limits.
        assert_eq!(overview_layout(1280, 720, 4), Layout { width: 1200, height: 630, columns: 2, content_height: 540, preview_height: 179 });
        assert_eq!(overview_layout(3840, 2160, 20), Layout { width: 3760, height: 2040, columns: 4, content_height: 1980, preview_height: 302 });
        assert_eq!(overview_layout(640, 480, 0), Layout { width: 560, height: 400, columns: 1, content_height: 300, preview_height: 214 });
        assert_eq!(layout_limits(1280, 720, 3), Layout { width: 564, height: 246, columns: 3, content_height: 444, preview_height: -1 });
        assert_eq!(layout_limits(1280, 720, 40), Layout { width: 740, height: 562, columns: 4, content_height: 444, preview_height: -1 });
        assert_eq!(layout_limits(320, 240, 5), Layout { width: 272, height: 192, columns: 1, content_height: 74, preview_height: -1 });
        assert_eq!(layout_limits(1024, 768, 0), Layout { width: 360, height: 246, columns: 1, content_height: 444, preview_height: -1 });
    }

    #[test]
    fn python_round_is_half_even() {
        assert_eq!(py_round(0.5), 0);
        assert_eq!(py_round(1.5), 2);
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(2.6), 3);
    }
}
