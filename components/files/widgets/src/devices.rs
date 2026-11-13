//! Port of files_devices.DiskUsage: an asynchronous capacity indicator for a
//! Places row (the disk discovery/capacity data lives in `tessel_files_model::devices`).

use crate::compat::{capacity, format_size};
use gtk::pango;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub const CAPACITY_QUERY: &str = "filesystem::size,filesystem::free,filesystem::used";

/// `(size, free, used)` in bytes.
pub type Capacity = (u64, u64, u64);

thread_local! {
    // Last known capacity per location URI, for the life of the process, so a rebuilt row starts with numbers.
    static LAST_CAPACITY: RefCell<HashMap<String, Capacity>> = RefCell::new(HashMap::new());
}

/// The last capacity a reply reported for `location`, if any.
pub fn last_capacity(location: &gio::File) -> Option<Capacity> {
    LAST_CAPACITY.with(|cache| cache.borrow().get(location.uri().as_str()).copied())
}

/// Forget every remembered capacity (tests start from "Checking space…").
pub fn forget_last_capacities() {
    LAST_CAPACITY.with(|cache| cache.borrow_mut().clear());
}

/// A one-line message plus an empty second line, as tall as "X free\nY total".
fn two_lines(text: &str) -> String {
    format!("{text}\n")
}

pub struct DiskUsageInner {
    pub root: gtk::Box,
    pub location: gio::File,
    pub bar: gtk::ProgressBar,
    pub detail: gtk::Label,
    cancel: RefCell<Option<gio::Cancellable>>,
    closed: Cell<bool>,
}

/// `.files-disk-usage`: a 5 px bar (`.low-space` under 10 % free) and "X free\nY total".
/// The bar always takes its space (transparent until a capacity is known) and the
/// text is always two lines, so a reply never changes the row's size.
#[derive(Clone)]
pub struct DiskUsage(Rc<DiskUsageInner>);

impl std::ops::Deref for DiskUsage {
    type Target = DiskUsageInner;
    fn deref(&self) -> &DiskUsageInner {
        &self.0
    }
}

impl DiskUsage {
    /// Starts with the last known capacity of `location` (see [`last_capacity`]).
    pub fn new(location: &gio::File) -> Self {
        Self::seeded(location, last_capacity(location))
    }

    /// Shows `known` at once (or "Checking space…" for `None`), then queries.
    pub fn seeded(location: &gio::File, known: Option<Capacity>) -> Self {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
        root.add_css_class("files-disk-usage");
        let bar = gtk::ProgressBar::builder().opacity(0.0).build();
        let detail = gtk::Label::builder().label(two_lines("Checking space…")).xalign(0.0).ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(25).build();
        detail.add_css_class("muted");
        root.append(&bar);
        root.append(&detail);
        let usage = Self(Rc::new(DiskUsageInner {
            root, location: location.clone(), bar, detail, cancel: RefCell::new(None), closed: Cell::new(false),
        }));
        if known.is_some() {
            usage.show(known);
        }
        usage.refresh();
        usage
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    /// The pending query's cancellable (`usage.cancel`); `None` when idle.
    pub fn pending(&self) -> Option<gio::Cancellable> {
        self.0.cancel.borrow().clone()
    }

    pub fn closed(&self) -> bool {
        self.0.closed.get()
    }

    /// Start one query unless closed or one is already running (never queues).
    pub fn refresh(&self) {
        if self.0.closed.get() || self.0.cancel.borrow().is_some() {
            return;
        }
        let cancel = gio::Cancellable::new();
        self.0.cancel.replace(Some(cancel.clone()));
        let weak = Rc::downgrade(&self.0);
        self.0.location.query_filesystem_info_async(CAPACITY_QUERY, glib::Priority::LOW, Some(&cancel.clone()), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            let value = result.ok().and_then(|info| capacity(&info));
            DiskUsage(inner).finished(&cancel, value);
        });
    }

    /// The query reply (`finished` in Python); stale or post-close replies are ignored.
    pub fn finished(&self, token: &gio::Cancellable, value: Option<Capacity>) {
        if self.0.closed.get() || self.0.cancel.borrow().as_ref() != Some(token) {
            return;
        }
        self.0.cancel.replace(None);
        if let Some(value) = value {
            LAST_CAPACITY.with(|cache| cache.borrow_mut().insert(self.location.uri().to_string(), value));
        }
        self.show(value);
    }

    /// Only what differs is set: an unchanged text is not touched.
    fn show(&self, value: Option<Capacity>) {
        let set_text = |text: &str| {
            if self.detail.text() != text {
                self.detail.set_text(text);
            }
        };
        self.bar.set_opacity(if value.is_some() { 1.0 } else { 0.0 });
        let parse_name = self.location.parse_name();
        let Some((size, free, used)) = value else {
            set_text(&two_lines("Space unavailable"));
            self.root.set_tooltip_text(Some(&parse_name));
            return;
        };
        let fraction = used as f64 / size as f64;
        if self.bar.fraction() != fraction {
            self.bar.set_fraction(fraction);
        }
        set_text(&format!("{} free\n{} total", format_size(free), format_size(size)));
        self.root.set_tooltip_text(Some(&format!("{parse_name}\n{} used · {} free · {} total",
                                                 format_size(used), format_size(free), format_size(size))));
        if (free as f64 / size as f64) < 0.1 {
            self.bar.add_css_class("low-space");
        } else {
            self.bar.remove_css_class("low-space");
        }
    }

    /// True while the bar shows a capacity (it keeps its space either way).
    pub fn showing_capacity(&self) -> bool {
        self.bar.opacity() > 0.0
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        if let Some(cancel) = self.0.cancel.take() {
            cancel.cancel();
        }
    }
}
