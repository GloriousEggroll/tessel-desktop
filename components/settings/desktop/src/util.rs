//! Small helpers shared by the desktop pages (Python module-level `_label`,
//! `round()`, `run_in_thread`).
use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

/// `_label(text, css=None)`: `Gtk.Label(label=text, xalign=0, wrap=True)`.
pub fn label(text: &str, css: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::builder().label(text).xalign(0.0).wrap(true).build();
    if let Some(css) = css.filter(|c| !c.is_empty()) {
        widget.add_css_class(css);
    }
    widget
}

/// Python `round()` (ties to even).
pub fn py_round(value: f64) -> i64 {
    value.round_ties_even() as i64
}

pub fn accessible_label(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

/// Where page work runs: a worker thread whose result is delivered on the
/// GTK thread (Python `threading.Thread` + `GLib.idle_add`), or inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Runner {
    #[default]
    Thread,
    Inline,
}

impl Runner {
    pub fn run<T: Send + 'static>(self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(Option<T>) + 'static) {
        match self {
            Runner::Inline => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).ok();
                done(result)
            }
            Runner::Thread => {
                glib::MainContext::default().spawn_local(async move {
                    done(gio::spawn_blocking(work).await.ok());
                });
            }
        }
    }
}

/// A shared `closed` flag checked by late callbacks.
pub type Flag = Rc<Cell<bool>>;

/// Remove every child of a box.
pub fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// The host window's display (Python `host.get_display()`).
pub fn host_display(host: &gs_base::Host) -> gtk::gdk::Display {
    WidgetExt::display(&host.window())
}
