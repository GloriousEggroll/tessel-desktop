//! login_transition.py: short frame-clock fades, with a deadline even when a
//! surface stops drawing.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

struct Inner {
    widgets: Vec<gtk::Widget>,
    start: f64,
    end: f64,
    duration: i64,
    finished: RefCell<Option<Box<dyn FnOnce()>>>,
    started: Cell<Option<i64>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    timeout: RefCell<Option<glib::SourceId>>,
}

/// `Fade(widgets, start, end, duration_ms, finished)`.
#[derive(Clone)]
pub struct Fade(Rc<Inner>);

impl Fade {
    pub fn new(widgets: Vec<gtk::Widget>, start: f64, end: f64, duration_ms: u32, finished: impl FnOnce() + 'static) -> Fade {
        let fade = Fade(Rc::new(Inner {
            widgets, start, end, duration: (duration_ms.max(1) as i64) * 1000,
            finished: RefCell::new(Some(Box::new(finished))), started: Cell::new(None), tick: RefCell::new(None),
            timeout: RefCell::new(None),
        }));
        for widget in &fade.0.widgets {
            widget.set_opacity(start);
        }
        if let Some(first) = fade.0.widgets.first() {
            let weak = Rc::downgrade(&fade.0);
            let id = first.add_tick_callback(move |_, clock| {
                let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
                Fade(inner).frame(clock.frame_time())
            });
            *fade.0.tick.borrow_mut() = Some(id);
        }
        let weak = Rc::downgrade(&fade.0);
        let timeout = glib::timeout_add_local_once(std::time::Duration::from_millis(duration_ms as u64 + 150), move || {
            if let Some(inner) = weak.upgrade() {
                inner.timeout.borrow_mut().take();
                Fade(inner).complete();
            }
        });
        *fade.0.timeout.borrow_mut() = Some(timeout);
        fade
    }

    fn frame(&self, now: i64) -> glib::ControlFlow {
        let started = *self.0.started.get().get_or_insert(now);
        self.0.started.set(Some(started));
        let fraction = ((now - started) as f64 / self.0.duration as f64).clamp(0.0, 1.0);
        let eased = fraction * fraction * (3.0 - 2.0 * fraction);
        for widget in &self.0.widgets {
            widget.set_opacity(self.0.start + (self.0.end - self.0.start) * eased);
        }
        if fraction < 1.0 {
            return glib::ControlFlow::Continue;
        }
        // Returning Break removes the callback; forget the id first.
        self.0.tick.borrow_mut().take();
        self.complete();
        glib::ControlFlow::Break
    }

    /// Remove the tick callback and the deadline and drop `finished`.
    pub fn cancel(&self) {
        if let Some(id) = self.0.tick.borrow_mut().take() {
            id.remove();
        }
        if let Some(id) = self.0.timeout.borrow_mut().take() {
            id.remove();
        }
        self.0.finished.borrow_mut().take();
    }

    pub fn complete(&self) {
        let callback = self.0.finished.borrow_mut().take();
        self.cancel();
        for widget in &self.0.widgets {
            widget.set_opacity(self.0.end);
        }
        if let Some(callback) = callback {
            callback();
        }
    }

    pub fn is_running(&self) -> bool {
        self.0.finished.borrow().is_some()
    }
}
