//! dock_ui.Magnifier: draws its single child scaled from the dock's edge,
//! leaving the layout untouched (scaling only happens in the snapshot, so
//! magnified icons never resize the dock; the dock reserves transparent
//! headroom for them instead).
//!
//! API: `Magnifier::new(&child, edge)` (`edge` = "bottom" | "top" | "left" |
//! "right"; the scale origin is that edge's midpoint), `.child()`,
//! `.scale()`, `.target()`, `.set_target(t)` (animates `scale += (target −
//! scale)·0.45` per frame until within 0.004), [`origin`].

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// `Magnifier.ORIGINS[edge]` (fractions of width/height).
pub fn origin(edge: &str) -> (f32, f32) {
    match edge {
        "top" => (0.5, 0.0),
        "left" => (0.0, 0.5),
        "right" => (1.0, 0.5),
        _ => (0.5, 1.0),
    }
}

/// One animation step; returns (new scale, finished).
pub fn step(scale: f64, target: f64) -> (f64, bool) {
    let scale = scale + (target - scale) * 0.45;
    if (scale - target).abs() < 0.004 { (target, true) } else { (scale, false) }
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Magnifier {
        pub child: RefCell<Option<gtk::Widget>>,
        pub origin: Cell<(f32, f32)>,
        pub scale: Cell<f64>,
        pub target: Cell<f64>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Magnifier {
        const NAME: &'static str = "TesselDockMagnifier";
        type Type = super::Magnifier;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Magnifier {
        fn dispose(&self) {
            if let Some(tick) = self.tick.borrow_mut().take() {
                tick.remove();
            }
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Magnifier {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match self.child.borrow().as_ref() {
                Some(child) => child.measure(orientation, for_size),
                None => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(child) = self.child.borrow().clone() else { return };
            let widget = self.obj();
            let scale = self.scale.get();
            if scale == 1.0 {
                widget.snapshot_child(&child, snapshot);
                return;
            }
            let (ox, oy) = self.origin.get();
            let (x, y) = (widget.width() as f32 * ox, widget.height() as f32 * oy);
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(x, y));
            snapshot.scale(scale as f32, scale as f32);
            snapshot.translate(&gtk::graphene::Point::new(-x, -y));
            widget.snapshot_child(&child, snapshot);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct Magnifier(ObjectSubclass<imp::Magnifier>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Magnifier {
    pub fn new(child: &impl IsA<gtk::Widget>, edge: &str) -> Self {
        let magnifier: Self = glib::Object::new();
        let imp = magnifier.imp();
        imp.origin.set(origin(edge));
        imp.scale.set(1.0);
        imp.target.set(1.0);
        child.set_parent(&magnifier);
        imp.child.replace(Some(child.clone().upcast()));
        magnifier
    }

    pub fn child(&self) -> Option<gtk::Widget> { self.imp().child.borrow().clone() }
    pub fn scale(&self) -> f64 { self.imp().scale.get() }
    pub fn target(&self) -> f64 { self.imp().target.get() }

    pub fn set_target(&self, target: f64) {
        let imp = self.imp();
        imp.target.set(target);
        if imp.tick.borrow().is_none() && (imp.scale.get() - target).abs() > 0.001 {
            let id = self.add_tick_callback(|widget, _clock| {
                let imp = widget.imp();
                let (scale, done) = step(imp.scale.get(), imp.target.get());
                imp.scale.set(scale);
                widget.queue_draw();
                if done {
                    imp.tick.borrow_mut().take();
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
            imp.tick.replace(Some(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_animation_steps() {
        assert_eq!(origin("bottom"), (0.5, 1.0));
        assert_eq!(origin("top"), (0.5, 0.0));
        assert_eq!(origin("left"), (0.0, 0.5));
        assert_eq!(origin("right"), (1.0, 0.5));
        let (mut scale, mut done) = (1.0, false);
        let mut frames = 0;
        while !done {
            (scale, done) = step(scale, 1.5);
            frames += 1;
        }
        assert_eq!(scale, 1.5);
        assert!(frames < 12);
        assert_eq!(step(1.0, 1.0), (1.0, true));
    }
}
