//! The Archive Manager settings page (Ark's Configure dialog: General,
//! Extraction, Plugins, Previews). The app's Settings window and Global
//! Settings both embed it; every change is written at once and the page
//! follows the file when another instance writes it.

use crate::plugins::{Access, PLUGINS};
use crate::{n_, Settings, Store};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use suite_core::tr;

pub struct SettingsPage {
    pub page: gtk::Box,
    pub switches: HashMap<&'static str, gtk::Switch>,
    pub plugin_switches: HashMap<&'static str, gtk::Switch>,
    pub max_size: gtk::SpinButton,
    pub store: Store,
    pub error: gtk::Label,
    updating: Cell<bool>,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

/// (key, title, detail, section)
const ROWS: &[(&str, &str, &str, &str)] = &[
    ("show_info_panel", n_!("Show information panel"), n_!("Show details about the selected entry beside the archive contents."), "general"),
    ("open_destination", n_!("Open destination folder after extracting"), n_!("Show the extracted files in the file manager."), "extraction"),
    ("close_after_extraction", n_!("Close Archive Manager after extracting"), n_!("Close the window once an extraction started from it finishes."), "extraction"),
    ("preserve_paths", n_!("Preserve paths when extracting"), n_!("Recreate the folders stored in the archive."), "extraction"),
    ("extract_subfolder", n_!("Extract into a subfolder"), n_!("Create a folder named after the archive unless it holds a single folder."), "extraction"),
    ("internal_preview", n_!("Use the internal previewer"), n_!("Preview text and pictures inside Archive Manager; other files open in their application."), "previews"),
    ("limit_preview_size", n_!("Limit preview file size"), n_!("Do not preview entries larger than the size below."), "previews"),
];

pub fn row(title: &str, subtitle: Option<&str>, control: Option<&gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("gs-row");
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
    let heading = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    heading.add_css_class("gs-row-title");
    text.append(&heading);
    if let Some(subtitle) = subtitle.filter(|s| !s.is_empty()) {
        let detail = gtk::Label::builder().label(subtitle).xalign(0.0).wrap(true).max_width_chars(60).build();
        detail.add_css_class("gs-row-subtitle");
        text.append(&detail);
    }
    row.append(&text);
    if let Some(control) = control {
        control.set_valign(gtk::Align::Center);
        row.append(control);
        control.update_property(&[gtk::accessible::Property::Label(title)]);
    }
    row
}

fn section(page: &gtk::Box, title: &str) -> gtk::Box {
    let heading = gtk::Label::builder().label(title).xalign(0.0).margin_top(6).build();
    heading.add_css_class("gs-section-title");
    page.append(&heading);
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("gs-card");
    card.add_css_class("card");
    page.append(&card);
    card
}

/// "tar.gz, tgz (read and write)" style summary of a plugin's formats.
pub fn plugin_formats(plugin: &crate::plugins::Plugin) -> String {
    let mut read_write = Vec::new();
    let mut read_only = Vec::new();
    for (kind, access, _) in plugin.formats {
        let name = kind.extension();
        match access {
            Access::ReadWrite => read_write.push(name),
            Access::ReadOnly => read_only.push(name),
        }
    }
    let mut parts = Vec::new();
    if !read_write.is_empty() {
        parts.push(tr!("Read and write: {formats}", formats = read_write.join(", ")));
    }
    if !read_only.is_empty() {
        parts.push(tr!("Read only: {formats}", formats = read_only.join(", ")));
    }
    parts.join("\n")
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
        for (id, switch) in &self.plugin_switches {
            switch.set_active(settings.plugin_enabled(id));
        }
        self.max_size.set_value(f64::from(settings.preview_max_mb));
        self.max_size.set_sensitive(settings.limit_preview_size);
        self.updating.set(false);
    }

    fn write(&self, edit: impl FnOnce(&mut Settings)) {
        if self.updating.get() {
            return;
        }
        match self.store.update(edit) {
            Ok(settings) => {
                self.error.set_visible(false);
                self.max_size.set_sensitive(settings.limit_preview_size);
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

pub fn build_page(path: Option<PathBuf>) -> Rc<SettingsPage> {
    let store = Store::new(path);
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    page.add_css_class("archive-settings");
    let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    error.add_css_class("error");
    page.append(&error);
    let general = section(&page, &tr!("General"));
    let extraction = section(&page, &tr!("Extraction"));
    let mut switches = HashMap::new();
    let mut preview_rows = Vec::new();
    for (key, title, detail, which) in ROWS {
        let switch = gtk::Switch::new();
        let r = row(&tr!(*title), Some(&tr!(*detail)), Some(switch.upcast_ref()));
        match *which {
            "general" => general.append(&r),
            "extraction" => extraction.append(&r),
            _ => preview_rows.push(r),
        }
        switches.insert(*key, switch);
    }
    let plugins_card = section(&page, &tr!("Plugins"));
    let mut plugin_switches = HashMap::new();
    for plugin in PLUGINS {
        let switch = gtk::Switch::new();
        let available = plugin.available();
        let mut detail = format!("{}\n{}", tr!(plugin.description), plugin_formats(plugin));
        if !available {
            detail.push('\n');
            detail.push_str(&tr!("Not available: install {tools}.", tools = plugin.tools.join(", ")));
        }
        let r = row(&tr!(plugin.name), Some(&detail), Some(switch.upcast_ref()));
        r.set_widget_name(&format!("plugin-{}", plugin.id));
        plugins_card.append(&r);
        plugin_switches.insert(plugin.id, switch);
    }
    let previews = section(&page, &tr!("Previews"));
    for r in preview_rows {
        previews.append(&r);
    }
    let max_size = gtk::SpinButton::with_range(1.0, 10_000.0, 1.0);
    max_size.set_digits(0);
    previews.append(&row(&tr!("Maximum preview size (MB)"), None, Some(max_size.upcast_ref())));
    let this = Rc::new(SettingsPage {
        page, switches, plugin_switches, max_size, store, error, updating: Cell::new(false), monitor: RefCell::new(None),
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
    for (id, switch) in &this.plugin_switches {
        let (weak, id) = (Rc::downgrade(&this), *id);
        switch.connect_active_notify(move |switch| {
            if let Some(this) = weak.upgrade() {
                let value = switch.is_active();
                this.write(|s| s.set_plugin_enabled(id, value));
            }
        });
    }
    let weak = Rc::downgrade(&this);
    this.max_size.connect_value_changed(move |spin| {
        if let Some(this) = weak.upgrade() {
            let value = spin.value() as u32;
            this.write(|s| s.preview_max_mb = value);
        }
    });
    this.refresh();
    let weak = Rc::downgrade(&this);
    let monitor = this.store.watch(move || {
        if let Some(this) = weak.upgrade() {
            this.refresh();
        }
    });
    *this.monitor.borrow_mut() = monitor;
    this
}
