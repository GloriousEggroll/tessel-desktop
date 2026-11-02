//! Port of files_devices.DiskUsage: an asynchronous capacity indicator for a
//! Places row (the disk discovery/capacity data lives in `tessel_files_model::devices`).

use crate::compat::{capacity, format_size};
use gtk::pango;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const CAPACITY_QUERY: &str = "filesystem::size,filesystem::free,filesystem::used";

pub struct DiskUsageInner {
    pub root: gtk::Box,
    pub location: gio::File,
    pub bar: gtk::ProgressBar,
    pub detail: gtk::Label,
    cancel: RefCell<Option<gio::Cancellable>>,
    closed: Cell<bool>,
}

/// `.files-disk-usage`: a 5 px bar (`.low-space` under 10 % free) and "X free\nY total".
#[derive(Clone)]
pub struct DiskUsage(Rc<DiskUsageInner>);

impl std::ops::Deref for DiskUsage {
    type Target = DiskUsageInner;
    fn deref(&self) -> &DiskUsageInner {
        &self.0
    }
}

impl DiskUsage {
    pub fn new(location: &gio::File) -> Self {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
        root.add_css_class("files-disk-usage");
        let bar = gtk::ProgressBar::builder().visible(false).build();
        let detail = gtk::Label::builder().label("Checking space…").xalign(0.0).ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(25).build();
        detail.add_css_class("muted");
        root.append(&bar);
        root.append(&detail);
        let usage = Self(Rc::new(DiskUsageInner {
            root, location: location.clone(), bar, detail, cancel: RefCell::new(None), closed: Cell::new(false),
        }));
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
    pub fn finished(&self, token: &gio::Cancellable, value: Option<(u64, u64, u64)>) {
        if self.0.closed.get() || self.0.cancel.borrow().as_ref() != Some(token) {
            return;
        }
        self.0.cancel.replace(None);
        self.bar.set_visible(value.is_some());
        let parse_name = self.location.parse_name();
        let Some((size, free, used)) = value else {
            self.detail.set_text("Space unavailable");
            self.root.set_tooltip_text(Some(&parse_name));
            return;
        };
        self.bar.set_fraction(used as f64 / size as f64);
        self.detail.set_text(&format!("{} free\n{} total", format_size(free), format_size(size)));
        self.root.set_tooltip_text(Some(&format!("{parse_name}\n{} used · {} free · {} total",
                                                 format_size(used), format_size(free), format_size(size))));
        if (free as f64 / size as f64) < 0.1 {
            self.bar.add_css_class("low-space");
        } else {
            self.bar.remove_css_class("low-space");
        }
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        if let Some(cancel) = self.0.cancel.take() {
            cancel.cancel();
        }
    }
}
