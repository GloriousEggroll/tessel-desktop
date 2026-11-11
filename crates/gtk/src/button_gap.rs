//! Window buttons as far from the side of the window as from its top.
//!
//! GTK centers the minimize, maximize and close buttons in the height of the
//! header bar, so their distance from the top follows that height (34 px in
//! Global Settings, 42 px in Files), while their distance from the side is the
//! header bar's padding, whatever the height. The two only meet by accident.
//!
//! [`ButtonGap`] measures the title bar of one window as it is laid out and
//! sets the header bar's side padding to what makes the two distances equal.
//! It measures again whenever the window is laid out, so it follows a change
//! of layout (buttons left or right, plain or colored) and of font.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use crate::utils::PRIORITY_USER;

/// No header bar is padded more than this, whatever is measured.
const MAX_PADDING: i32 = 48;
/// A title bar that keeps asking for another padding is left alone after this many.
const MAX_CHANGES: u32 = 12;

/// One side of a header bar, in pixels: the padding it has now, and how far
/// its outermost window button is from the top and from that side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Side {
    pub padding: f32,
    pub top: f32,
    pub gap: f32,
}

/// The side padding that puts the outermost button as far from the side as it
/// is from the top.
pub fn wanted_padding(side: Side) -> i32 {
    ((side.padding + side.top - side.gap).round() as i32).clamp(0, MAX_PADDING)
}

/// The padding of both sides. A side with no buttons takes the other's, so the
/// title bar stays symmetric. `None`: there are no window buttons.
pub fn wanted_paddings(sides: [Option<Side>; 2]) -> Option<(i32, i32)> {
    match (sides[0].map(wanted_padding), sides[1].map(wanted_padding)) {
        (Some(left), Some(right)) => Some((left, right)),
        (Some(both), None) | (None, Some(both)) => Some((both, both)),
        (None, None) => None,
    }
}

/// The rules for one window (`scope` is a class only that window has). The
/// window controls lose the side margins GTK and the user stylesheet give
/// them, so the header bar's padding alone decides where they are.
pub fn gap_css(scope: &str, padding: Option<(i32, i32)>) -> String {
    let window = format!("window.{scope}");
    let mut css = format!("{window} headerbar windowcontrols.start {{ margin-left: 0; }}\n\
                           {window} headerbar windowcontrols.end {{ margin-right: 0; }}\n");
    if let Some((left, right)) = padding {
        css += &format!("{window} headerbar {{ padding-left: {left}px; padding-right: {right}px; }}\n");
    }
    css
}

fn descendants(widget: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
    let mut child = widget.first_child();
    while let Some(current) = child {
        found.push(current.clone());
        descendants(&current, found);
        child = current.next_sibling();
    }
}

/// The header bar that is, or is in, the window's title bar.
fn header_of(window: &gtk::Window) -> Option<gtk::Widget> {
    let bar = window.titlebar()?;
    if bar.is::<gtk::HeaderBar>() {
        return Some(bar);
    }
    let mut inside = Vec::new();
    descendants(&bar, &mut inside);
    inside.into_iter().find(|widget| widget.is::<gtk::HeaderBar>())
}

/// What a laid-out header bar measures: `[left, right]`, `None` for a side
/// with no window button. Distances are from the header bar's outer edge,
/// which is the window's: a widget's own coordinates start inside its padding,
/// so its outer box is asked for.
pub fn measure(header: &gtk::Widget) -> [Option<Side>; 2] {
    let mut sides = [None, None];
    let (Some(outer), Some(inner)) = (header.compute_bounds(header), header.first_child().and_then(|inner| inner.compute_bounds(header)))
        else { return sides };
    if outer.width() <= 0.0 || !header.is_mapped() {
        return sides;
    }
    let (left, right) = (outer.x(), outer.x() + outer.width());
    let paddings = [inner.x() - left, right - inner.x() - inner.width()];
    let mut inside = Vec::new();
    descendants(header, &mut inside);
    for controls in inside.iter().filter(|widget| widget.is::<gtk::WindowControls>() && widget.is_mapped()) {
        let mut child = controls.first_child();
        while let Some(button) = child {
            child = button.next_sibling();
            if !button.is::<gtk::Button>() || !button.is_mapped() {
                continue;       // the window's icon
            }
            // What is seen of a button is its image: the whole button when it
            // is plain, the dot inside it when it is colored.
            let seen = button.first_child().filter(|image| image.is_mapped()).unwrap_or(button);
            let Some(bounds) = seen.compute_bounds(header) else { continue };
            let gaps = [bounds.x() - left, right - bounds.x() - bounds.width()];
            let index = if gaps[0] <= gaps[1] { 0 } else { 1 };
            if sides[index].is_none_or(|side: Side| gaps[index] < side.gap) {
                sides[index] = Some(Side { padding: paddings[index], top: bounds.y() - outer.y(), gap: gaps[index] });
            }
        }
    }
    sides
}

struct GapInner {
    window: glib::WeakRef<gtk::Window>,
    scope: String,
    provider: gtk::CssProvider,
    display: RefCell<Option<gdk::Display>>,
    padding: Cell<Option<(i32, i32)>>,
    changes: Cell<u32>,
    clock: RefCell<Option<(gdk::FrameClock, Vec<glib::SignalHandlerId>)>>,
    signals: RefCell<Vec<glib::SignalHandlerId>>,
    closed: Cell<bool>,
}

/// Keeps one window's buttons as far from the side as from the top.
#[derive(Clone)]
pub struct ButtonGap(Rc<GapInner>);

impl ButtonGap {
    /// `scope` is a CSS class that only this window has.
    pub fn new(window: &gtk::Window, scope: &str) -> Self {
        let gap = Self(Rc::new(GapInner {
            window: window.downgrade(), scope: scope.to_owned(), provider: gtk::CssProvider::new(), display: RefCell::new(None),
            padding: Cell::new(None), changes: Cell::new(0), clock: RefCell::new(None), signals: RefCell::new(Vec::new()),
            closed: Cell::new(false),
        }));
        gap.0.provider.load_from_string(&gap_css(scope, None));
        let display = WidgetExt::display(window);
        gtk::style_context_add_provider_for_display(&display, &gap.0.provider, PRIORITY_USER + 3);
        gap.0.display.replace(Some(display));
        let weak = Rc::downgrade(&gap.0);
        let realized = window.connect_realize(move |_| if let Some(inner) = weak.upgrade() { ButtonGap(inner).watch() });
        let weak = Rc::downgrade(&gap.0);
        let unrealized = window.connect_unrealize(move |_| if let Some(inner) = weak.upgrade() { ButtonGap(inner).unwatch() });
        gap.0.signals.replace(vec![realized, unrealized]);
        if window.is_realized() {
            gap.watch();
        }
        gap
    }

    /// The side padding in use: `(left, right)`, `None` until a title bar with
    /// window buttons has been laid out.
    pub fn padding(&self) -> Option<(i32, i32)> {
        self.0.padding.get()
    }

    /// Measure after each layout of the window. A change made there is laid
    /// out again before the frame is painted, so the buttons never jump; the
    /// look after painting catches a layout this handler ran ahead of.
    fn watch(&self) {
        self.unwatch();
        let Some(clock) = self.0.window.upgrade().and_then(|window| window.frame_clock()) else { return };
        let handler = |weak: Weak<GapInner>| move |_: &gdk::FrameClock| if let Some(inner) = weak.upgrade() { ButtonGap(inner).update() };
        let handlers = vec![clock.connect_layout(handler(Rc::downgrade(&self.0))), clock.connect_after_paint(handler(Rc::downgrade(&self.0)))];
        self.0.clock.replace(Some((clock, handlers)));
    }

    fn unwatch(&self) {
        if let Some((clock, handlers)) = self.0.clock.take() {
            for handler in handlers {
                clock.disconnect(handler);
            }
        }
    }

    /// Measure the title bar and correct its side padding.
    pub fn update(&self) {
        if self.0.closed.get() {
            return;
        }
        let Some(header) = self.0.window.upgrade().and_then(|window| header_of(&window)) else { return };
        let Some(wanted) = wanted_paddings(measure(&header)) else { return };
        if Some(wanted) == self.0.padding.get() {
            self.0.changes.set(0);
            return;
        }
        // Each change is laid out and measured again. A title bar whose
        // buttons do not follow the padding would never settle.
        if self.0.changes.get() >= MAX_CHANGES {
            return;
        }
        self.0.changes.set(self.0.changes.get() + 1);
        self.0.padding.set(Some(wanted));
        self.0.provider.load_from_string(&gap_css(&self.0.scope, Some(wanted)));
    }

    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        self.unwatch();
        if let Some(display) = self.0.display.take() {
            gtk::style_context_remove_provider_for_display(&display, &self.0.provider);
        }
        if let Some(window) = self.0.window.upgrade() {
            for handler in self.0.signals.take() {
                window.disconnect(handler);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_padding_makes_the_side_gap_the_top_gap() {
        // Files: a 42 px header bar, buttons 9 px from the top and against the padding.
        assert_eq!(wanted_padding(Side { padding: 6.0, top: 9.0, gap: 6.0 }), 9);
        // Global Settings: 34 px, with 10 px of padding of its own.
        assert_eq!(wanted_padding(Side { padding: 10.0, top: 5.0, gap: 10.0 }), 5);
        // Colored dots have 2 px of the button around them.
        assert_eq!(wanted_padding(Side { padding: 6.0, top: 13.0, gap: 8.0 }), 11);
        // Settled: measuring again asks for the same.
        assert_eq!(wanted_padding(Side { padding: 9.0, top: 9.0, gap: 9.0 }), 9);
        assert_eq!(wanted_padding(Side { padding: 11.0, top: 13.0, gap: 13.0 }), 11);
        assert_eq!(wanted_padding(Side { padding: 0.0, top: 0.0, gap: 30.0 }), 0);
        assert_eq!(wanted_padding(Side { padding: 6.0, top: 400.0, gap: 6.0 }), MAX_PADDING);
    }

    #[test]
    fn a_side_without_buttons_mirrors_the_other() {
        let side = Some(Side { padding: 6.0, top: 9.0, gap: 6.0 });
        let other = Some(Side { padding: 6.0, top: 13.0, gap: 8.0 });
        assert_eq!(wanted_paddings([None, side]), Some((9, 9)));
        assert_eq!(wanted_paddings([side, None]), Some((9, 9)));
        assert_eq!(wanted_paddings([other, side]), Some((11, 9)));
        assert_eq!(wanted_paddings([None, None]), None);
    }

    #[test]
    fn the_rules_reach_only_their_window() {
        let css = gap_css("scope-7", Some((9, 11)));
        assert!(css.contains("window.scope-7 headerbar { padding-left: 9px; padding-right: 11px; }"));
        assert!(css.contains("window.scope-7 headerbar windowcontrols.end { margin-right: 0; }"));
        assert!(css.lines().all(|line| line.starts_with("window.scope-7 ")));
        assert!(!gap_css("scope-7", None).contains("padding"));
    }
}
