//! Shared group placement controls for Dock Settings (port of
//! group_layout_settings.py `GroupLayoutControls`, W-g).
//!
//! API (Python names):
//! - [`GroupLayoutControls::new(&config, app_dock)`] → `Rc<GroupLayoutControls>`;
//!   [`widget()`](GroupLayoutControls::widget) is the vertical `gtk::Box` to
//!   place (Dock Settings attaches it across both grid columns).
//! - State: [`layouts()`](GroupLayoutControls::layouts) (`self.layouts`, the
//!   per-mode custom layouts, `{}` until edited), [`set_layout`](GroupLayoutControls::set_layout)
//!   (`self.layouts[mode] = …`), [`mode()`](GroupLayoutControls::mode),
//!   [`current()`](GroupLayoutControls::current) (custom layout or the default
//!   for the mode/display, or the app dock default for `expanded`).
//! - Context: [`set_expanded`](GroupLayoutControls::set_expanded),
//!   [`set_hidden_groups`](GroupLayoutControls::set_hidden_groups),
//!   [`set_context`](GroupLayoutControls::set_context) (each refreshes only on change
//!   except `set_context`, which always refreshes, as in Python).
//! - Edits: [`move_group`](GroupLayoutControls::move_group) (`move`),
//!   [`reorder`](GroupLayoutControls::reorder), [`reset`](GroupLayoutControls::reset),
//!   [`refresh`](GroupLayoutControls::refresh).
//! - Widgets for tests: [`controls(name)`](GroupLayoutControls::controls) =
//!   `(DropDown, up Button, down Button)`, [`row(name)`](GroupLayoutControls::row),
//!   [`names()`](GroupLayoutControls::names) (the groups that have rows, Python
//!   `self.controls` key order), [`caption()`](GroupLayoutControls::caption).
//!
//! CSS classes: `settings-muted` (caption), `settings-section-title` (zone headings).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use suite_core::tr;

use crate::core::config::DockConfig;
use crate::core::group_layout::{default_app_dock_layout, default_layout, Layout, Layouts, APP_DOCK_GROUPS, LAYOUT_GROUPS, POSITIONS};

fn accessible_name(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

struct Row {
    name: String,
    row: gtk::Box,
    selector: gtk::DropDown,
    up: gtk::Button,
    down: gtk::Button,
}

pub struct GroupLayoutControls {
    root: gtk::Box,
    app_dock: bool,
    expanded: Cell<bool>,
    hidden_groups: RefCell<BTreeSet<String>>,
    layouts: RefCell<Layouts>,
    mode: RefCell<String>,
    display: RefCell<String>,
    rows: Vec<Row>,
    zones: Vec<(String, gtk::Box)>,
    caption: gtk::Label,
    updating: Cell<bool>,
}

impl GroupLayoutControls {
    pub fn new(config: &DockConfig, app_dock: bool) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let (layouts, mode, display) = if app_dock {
            let mut layouts = Layouts::new();
            if !config.app_dock_group_layout.is_empty() {
                layouts.insert("dock", config.app_dock_group_layout.clone());
            }
            (layouts, "dock".to_owned(), "icons".to_owned())
        } else {
            (config.group_layouts.clone(), config.mode.clone(), config.application_display.clone())
        };
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let caption = gtk::Label::new(None);
        caption.set_xalign(0.0);
        caption.set_hexpand(true);
        caption.set_wrap(true);
        caption.add_css_class("settings-muted");
        header.append(&caption);
        let reset = gtk::Button::with_label(&tr!("Reset layout"));
        reset.set_valign(gtk::Align::Start);
        reset.set_tooltip_text(Some(&tr!("Restore the default group positions for this mode")));
        header.append(&reset);
        root.append(&header);
        let mut zones = Vec::new();
        for (position, label) in POSITIONS {
            let zone = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let heading = gtk::Label::new(Some(&tr!(label)));
            heading.set_xalign(0.0);
            heading.add_css_class("settings-section-title");
            zone.append(&heading);
            root.append(&zone);
            zones.push((position.to_string(), zone));
        }
        let mut rows = Vec::new();
        let position_labels: Vec<String> = POSITIONS.iter().map(|(_, label)| tr!(label)).collect();
        for (name, title) in LAYOUT_GROUPS {
            if app_dock && !APP_DOCK_GROUPS.contains(name) {
                continue;
            }
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let label = gtk::Label::new(Some(&tr!(title)));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(26);
            row.append(&label);
            let strings: Vec<&str> = position_labels.iter().map(String::as_str).collect();
            let selector = gtk::DropDown::from_strings(&strings);
            accessible_name(&selector, &tr!("Position of {value1}", value1 = tr!(title)));
            row.append(&selector);
            let mut arrows = Vec::new();
            for (icon, direction) in [("go-up-symbolic", "Move up"), ("go-down-symbolic", "Move down")] {
                let button = gtk::Button::from_icon_name(icon);
                button.set_tooltip_text(Some(&tr!(direction)));
                accessible_name(&button, &format!("{}: {}", tr!(direction), tr!(title)));
                row.append(&button);
                arrows.push(button);
            }
            let down = arrows.pop().unwrap();
            let up = arrows.pop().unwrap();
            rows.push(Row { name: name.to_string(), row, selector, up, down });
        }
        let controls = Rc::new(Self {
            root,
            app_dock,
            expanded: Cell::new(config.app_dock_expand),
            hidden_groups: RefCell::new(BTreeSet::new()),
            layouts: RefCell::new(layouts),
            mode: RefCell::new(mode),
            display: RefCell::new(display),
            rows,
            zones,
            caption,
            updating: Cell::new(false),
        });
        let weak = Rc::downgrade(&controls);
        reset.connect_clicked(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.reset();
            }
        });
        for row in &controls.rows {
            let weak: Weak<Self> = Rc::downgrade(&controls);
            let group = row.name.clone();
            row.selector.connect_selected_notify(move |widget| {
                let Some(controls) = weak.upgrade() else { return };
                if controls.updating.get() {
                    return;
                }
                if let Some((position, _)) = POSITIONS.get(widget.selected() as usize) {
                    controls.move_group(&group, position);
                }
            });
            for (button, delta) in [(&row.up, -1), (&row.down, 1)] {
                let weak: Weak<Self> = Rc::downgrade(&controls);
                let group = row.name.clone();
                button.connect_clicked(move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.reorder(&group, delta);
                    }
                });
            }
        }
        controls.refresh();
        controls
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn caption(&self) -> &gtk::Label {
        &self.caption
    }

    pub fn is_app_dock(&self) -> bool {
        self.app_dock
    }

    pub fn layouts(&self) -> Layouts {
        self.layouts.borrow().clone()
    }

    pub fn set_layout(&self, mode: &str, layout: Layout) {
        self.layouts.borrow_mut().insert(mode, layout);
    }

    pub fn mode(&self) -> String {
        self.mode.borrow().clone()
    }

    pub fn names(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.name.clone()).collect()
    }

    pub fn controls(&self, name: &str) -> Option<(gtk::DropDown, gtk::Button, gtk::Button)> {
        self.rows.iter().find(|row| row.name == name).map(|row| (row.selector.clone(), row.up.clone(), row.down.clone()))
    }

    pub fn row(&self, name: &str) -> Option<gtk::Box> {
        self.rows.iter().find(|row| row.name == name).map(|row| row.row.clone())
    }

    pub fn current(&self) -> Layout {
        let mode = self.mode.borrow().clone();
        if let Some(layout) = self.layouts.borrow().get(&mode) {
            return layout.clone();
        }
        if self.app_dock { default_app_dock_layout(self.expanded.get()) } else { default_layout(&mode, &self.display.borrow()) }
    }

    pub fn set_expanded(&self, expanded: bool) {
        if self.expanded.get() != expanded {
            self.expanded.set(expanded);
            self.refresh();
        }
    }

    pub fn set_hidden_groups<S: AsRef<str>>(&self, names: &[S]) {
        let hidden: BTreeSet<String> = names.iter().map(|n| n.as_ref().to_owned()).collect();
        if hidden != *self.hidden_groups.borrow() {
            *self.hidden_groups.borrow_mut() = hidden;
            self.refresh();
        }
    }

    pub fn set_context(&self, mode: &str, display: &str) {
        *self.mode.borrow_mut() = mode.to_owned();
        *self.display.borrow_mut() = display.to_owned();
        self.refresh();
    }

    pub fn refresh(&self) {
        self.updating.set(true);
        let target = if self.app_dock {
            "Separate app dock"
        } else if *self.mode.borrow() == "bar" {
            "Bar"
        } else {
            "Dock"
        };
        self.caption.set_text(&tr!(
            "Group positions for {value1}. Up/down changes order within a position. Hidden groups keep their places.",
            value1 = tr!(target)
        ));
        for row in &self.rows {
            if let Some(parent) = row.row.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) {
                parent.remove(&row.row);
            }
        }
        let current = self.current();
        let hidden = self.hidden_groups.borrow().clone();
        for (index, (position, zone)) in self.zones.iter().enumerate() {
            let names: Vec<&String> = current
                .get(position)
                .map(|names| names.iter().filter(|name| !hidden.contains(*name)).collect())
                .unwrap_or_default();
            for (order, name) in names.iter().enumerate() {
                // Python raises KeyError for a group without a row; skip it.
                let Some(row) = self.rows.iter().find(|row| &row.name == *name) else { continue };
                row.selector.set_selected(index as u32);
                row.up.set_sensitive(order > 0);
                row.down.set_sensitive(order + 1 < names.len());
                zone.append(&row.row);
                let hint = if name.as_str() == "running" {
                    Some(tr!("Shown in Application icons mode"))
                } else if name.as_str() == "activities" || name.as_str() == "focused_title" {
                    Some(tr!("Shown in Focused window title mode"))
                } else {
                    None
                };
                row.row.set_tooltip_text(hint.as_deref());
            }
        }
        self.updating.set(false);
    }

    /// Python `move(name, position)`.
    pub fn move_group(&self, name: &str, position: &str) {
        let mut layout = self.current();
        if layout.get(position).is_some_and(|names| names.iter().any(|n| n == name)) {
            return;
        }
        for (_key, names) in layout.iter_mut() {
            names.retain(|n| n != name);
        }
        if let Some(names) = layout.get_mut(position) {
            names.push(name.to_owned());
        }
        let mode = self.mode.borrow().clone();
        self.layouts.borrow_mut().insert(mode, layout);
        self.refresh();
    }

    pub fn reorder(&self, name: &str, step: i32) {
        let mut layout = self.current();
        let hidden = self.hidden_groups.borrow().clone();
        let Some((_key, names)) = layout.iter_mut().find(|(_k, names)| names.iter().any(|n| n == name)) else { return };
        let visible: Vec<String> = names.iter().filter(|g| !hidden.contains(*g)).cloned().collect();
        let Some(index) = visible.iter().position(|g| g == name) else { return };
        let target = index as i64 + step as i64;
        if target < 0 || target >= visible.len() as i64 {
            return;
        }
        let a = names.iter().position(|g| g == name).unwrap();
        let b = names.iter().position(|g| *g == visible[target as usize]).unwrap();
        names.swap(a, b);
        let mode = self.mode.borrow().clone();
        self.layouts.borrow_mut().insert(mode, layout);
        self.refresh();
    }

    pub fn reset(&self) {
        let mode = self.mode.borrow().clone();
        self.layouts.borrow_mut().remove(&mode);
        self.refresh();
    }
}
