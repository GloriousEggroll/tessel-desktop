//! The Clipboard settings page Global Settings embeds. Every change is
//! written at once; the page follows the file when another writer changes it,
//! and the tessel-clipboard daemon re-reads it.

use crate::{n_, Settings, Store, MAX_MB_LIMIT};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use tessel_core::tr;

pub struct SettingsPage {
    pub page: gtk::Box,
    pub switches: HashMap<&'static str, gtk::Switch>,
    pub max_size: gtk::SpinButton,
    pub store: Store,
    pub error: gtk::Label,
    updating: Cell<bool>,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

/// (key, title, detail)
pub const ROWS: &[(&str, &str, &str)] = &[
    ("keep_clipboard", n_!("Keep copied content after closing apps"),
     n_!("Text and images you copy can still be pasted after the application you copied them from closes.")),
    ("keep_primary", n_!("Keep the middle-click selection"),
     n_!("Also keep the last selected text for middle-click pasting after its application closes.")),
    ("skip_secrets", n_!("Do not keep passwords"),
     n_!("Copies that a password manager marks as secret disappear when it clears or closes them.")),
];

fn row(title: &str, subtitle: &str, control: &gtk::Widget) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("gs-row");
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
    let heading = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    heading.add_css_class("gs-row-title");
    text.append(&heading);
    if !subtitle.is_empty() {
        let detail = gtk::Label::builder().label(subtitle).xalign(0.0).wrap(true).max_width_chars(60).build();
        detail.add_css_class("gs-row-subtitle");
        text.append(&detail);
    }
    row.append(&text);
    control.set_valign(gtk::Align::Center);
    row.append(control);
    control.update_property(&[gtk::accessible::Property::Label(title)]);
    row
}

impl SettingsPage {
    pub fn settings(&self) -> Settings {
        self.store.load()
    }

    /// Show the stored values (after another writer changed the file).
    pub fn refresh(&self) {
        let settings = self.store.load();
        self.updating.set(true);
        for (key, switch) in &self.switches {
            switch.set_active(settings.get_bool(key));
        }
        self.max_size.set_value(f64::from(settings.max_mb));
        self.sync_sensitivity(&settings);
        self.updating.set(false);
    }

    fn sync_sensitivity(&self, settings: &Settings) {
        let any = settings.keep_clipboard || settings.keep_primary;
        self.max_size.set_sensitive(any);
        if let Some(switch) = self.switches.get("skip_secrets") {
            switch.set_sensitive(any);
        }
    }

    fn write(&self, edit: impl FnOnce(&mut Settings)) {
        if self.updating.get() {
            return;
        }
        match self.store.update(edit) {
            Ok(settings) => {
                self.error.set_visible(false);
                self.sync_sensitivity(&settings);
            }
            Err(error) => {
                self.error.set_label(&tr!("Could not save settings: {error}", error = error));
                self.error.set_visible(true);
            }
        }
    }

    pub fn close(&self) {
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            monitor.cancel();
        }
    }
}

/// Call `changed` whenever the settings file is written (by anyone).
fn watch(store: &Store, changed: impl Fn() + 'static) -> Option<gio::FileMonitor> {
    use gio::prelude::*;
    let directory = store.path.parent()?.to_path_buf();
    let _ = std::fs::create_dir_all(&directory);
    let monitor = gio::File::for_path(&directory)
        .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>).ok()?;
    let name = store.path.file_name()?.to_os_string();
    monitor.connect_changed(move |_, file, other, event| {
        let hit = |f: Option<&gio::File>| f.and_then(|f| f.basename()).is_some_and(|b| b.as_os_str() == name);
        let relevant = matches!(event, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created
            | gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedIn | gio::FileMonitorEvent::Renamed);
        if relevant && (hit(Some(file)) || hit(other)) {
            changed();
        }
    });
    Some(monitor)
}

pub fn build_page(path: Option<PathBuf>) -> Rc<SettingsPage> {
    let store = Store::new(path);
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    page.add_css_class("tessel-clipboard-settings");
    let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    error.add_css_class("error");
    page.append(&error);
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("gs-card");
    card.add_css_class("card");
    page.append(&card);
    let mut switches = HashMap::new();
    for (key, title, detail) in ROWS {
        let switch = gtk::Switch::new();
        card.append(&row(&tr!(*title), &tr!(*detail), switch.upcast_ref()));
        switches.insert(*key, switch);
    }
    let max_size = gtk::SpinButton::with_range(1.0, f64::from(MAX_MB_LIMIT), 1.0);
    max_size.set_digits(0);
    card.append(&row(&tr!("Largest copy to keep (MB)"),
                     &tr!("Bigger copies, such as large images, are only available while their application is open."),
                     max_size.upcast_ref()));
    let this = Rc::new(SettingsPage {
        page, switches, max_size, store, error, updating: Cell::new(false), monitor: RefCell::new(None),
    });
    for (key, switch) in &this.switches {
        let (weak, key) = (Rc::downgrade(&this), *key);
        switch.connect_active_notify(move |switch| {
            if let Some(this) = weak.upgrade() {
                let value = switch.is_active();
                this.write(|s| s.set_bool(key, value));
            }
        });
    }
    let weak = Rc::downgrade(&this);
    this.max_size.connect_value_changed(move |spin| {
        if let Some(this) = weak.upgrade() {
            let value = spin.value() as u32;
            this.write(|s| s.max_mb = value);
        }
    });
    this.refresh();
    let weak = Rc::downgrade(&this);
    let monitor = watch(&this.store, move || {
        if let Some(this) = weak.upgrade() {
            this.refresh();
        }
    });
    *this.monitor.borrow_mut() = monitor;
    this
}
