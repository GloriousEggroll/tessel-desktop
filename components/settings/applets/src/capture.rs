//! `capture_page.py`: Capture's result preferences, embedded without
//! starting Capture, plus the screen picker's remote-control answer.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use gs_base::style::{card, setting_row, PAGE_MARGIN};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tessel_core::remote_control::{self, RemoteControl};
use tessel_core::tr;

use crate::Factories;

/// `page.controller` of the Screenshots page (Python `controls`, with `.card`).
pub struct CapturePage {
    pub controls: tessel_capture::settings::SettingsPage,
    pub card: Option<gtk::Box>,
    pub remote: Rc<RemoteGroup>,
}

/// (connector, label) of each connected screen.
pub type ScreenSource = Rc<dyn Fn() -> Vec<(String, String)>>;

/// GDK's monitors that have a connector name, labelled like the other screen lists.
pub fn connected_screens() -> Vec<(String, String)> {
    let Some(display) = gdk::Display::default() else { return Vec::new() };
    let model = display.monitors();
    (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gdk::Monitor>()).filter_map(|monitor| {
        let connector = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty())?;
        let label = tessel_core::screens::monitor_label(&connector, monitor.description().as_deref().unwrap_or(""),
                                                              monitor.model().as_deref().unwrap_or(""));
        Some((connector, label))
    }).collect()
}

/// "Remote control": let the screen picker answer remote-control requests
/// itself (`~/.config/tessel/remote-control.json`).
pub struct RemoteGroup {
    pub heading: gtk::Label,
    pub card: gtk::Box,
    pub switch: gtk::Switch,
    pub screen: gtk::DropDown,
    pub screen_row: gtk::Box,
    pub error: gtk::Label,
    path: PathBuf,
    screens: ScreenSource,
    /// Connector of each dropdown entry; "" is the first screen.
    connectors: RefCell<Vec<String>>,
    updating: Cell<bool>,
    monitor_watch: RefCell<Option<(gio::ListModel, glib::SignalHandlerId)>>,
}

impl RemoteGroup {
    pub fn new(path: PathBuf, screens: ScreenSource) -> Rc<RemoteGroup> {
        let heading = gtk::Label::builder().label(tr!("Remote control")).xalign(0.0).margin_top(6).build();
        heading.add_css_class("gs-section-title");
        let card = card(0);
        let switch = gtk::Switch::new();
        let unattended = setting_row(&tr!("Allow remote control without asking"),
                                     Some(&tr!("Apps that ask to control this computer get the screen below without the screen picker. Any app on this computer can then see the screen and use the keyboard and pointer.")),
                                     Some(switch.upcast_ref()));
        card.append(&unattended.root);
        let screen = gtk::DropDown::from_strings(&[]);
        let screen_row = setting_row(&tr!("Screen"), None, Some(screen.upcast_ref())).root;
        card.append(&screen_row);
        let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        error.add_css_class("error");
        let this = Rc::new(RemoteGroup {
            heading, card, switch, screen, screen_row, error, path, screens,
            connectors: RefCell::default(), updating: Cell::new(false), monitor_watch: RefCell::default(),
        });
        this.refresh();
        let weak = Rc::downgrade(&this);
        this.switch.connect_active_notify(move |switch| {
            let Some(this) = weak.upgrade() else { return };
            if this.updating.get() {
                return;
            }
            this.screen_row.set_sensitive(switch.is_active());
            let mut stored = RemoteControl::load(&this.path);
            stored.unattended = switch.is_active();
            this.save(&stored);
        });
        let weak = Rc::downgrade(&this);
        this.screen.connect_selected_notify(move |screen| {
            let Some(this) = weak.upgrade() else { return };
            if this.updating.get() {
                return;
            }
            let Some(output) = this.connectors.borrow().get(screen.selected() as usize).cloned() else { return };
            let mut stored = RemoteControl::load(&this.path);
            stored.output = output;
            this.save(&stored);
        });
        if let Some(display) = gdk::Display::default() {
            let model = display.monitors();
            let weak: Weak<RemoteGroup> = Rc::downgrade(&this);
            let id = model.connect_items_changed(move |_, _, _, _| {
                let weak = weak.clone();
                // A new monitor's connector arrives with it; read the list once settled.
                glib::idle_add_local_once(move || {
                    if let Some(this) = weak.upgrade() {
                        this.refresh();
                    }
                });
            });
            this.monitor_watch.replace(Some((model, id)));
        }
        this
    }

    /// Show the stored value; a saved screen that is not connected stays listed.
    pub fn refresh(&self) {
        let stored = RemoteControl::load(&self.path);
        let mut connectors = vec![String::new()];
        let mut labels = vec![tr!("First available screen")];
        for (connector, label) in (self.screens)() {
            if !connectors.contains(&connector) {
                connectors.push(connector);
                labels.push(label);
            }
        }
        if !connectors.contains(&stored.output) {
            connectors.push(stored.output.clone());
            labels.push(stored.output.clone());
        }
        let selected = connectors.iter().position(|c| *c == stored.output).unwrap_or(0) as u32;
        self.updating.set(true);
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.screen.set_model(Some(&gtk::StringList::new(&refs)));
        self.screen.set_selected(selected);
        self.switch.set_active(stored.unattended);
        self.screen_row.set_sensitive(stored.unattended);
        self.connectors.replace(connectors);
        self.updating.set(false);
    }

    pub fn connectors(&self) -> Vec<String> { self.connectors.borrow().clone() }

    fn save(&self, value: &RemoteControl) {
        match value.save(&self.path) {
            Ok(()) => self.error.set_visible(false),
            Err(error) => {
                self.error.set_label(&tr!("Could not save settings: {error}", error = error));
                self.error.set_visible(true);
                // Back to what is stored, after this change notification.
                self.refresh();
            }
        }
    }

    pub fn close(&self) {
        if let Some((model, id)) = self.monitor_watch.take() {
            model.disconnect(id);
        }
    }
}

/// Capture's switch rows go in the shared card, with the shared row styles.
pub fn shared_card(controls: &tessel_capture::settings::SettingsPage) -> Option<gtk::Box> {
    let page = &controls.page;
    page.set_margin_top(PAGE_MARGIN);
    page.set_margin_bottom(PAGE_MARGIN);
    page.set_margin_start(PAGE_MARGIN);
    page.set_margin_end(PAGE_MARGIN);
    // Python iterates `controls.switches.values()` (insertion = KEYS order).
    let rows: Vec<gtk::Widget> = tessel_capture::settings::KEYS.iter()
        .filter_map(|key| controls.switches.get(key))
        .filter_map(|switch| switch.parent())
        .collect();
    let first = rows.first()?;
    let b = card(0);
    page.insert_child_after(&b, first.prev_sibling().as_ref());
    for row in &rows {
        page.remove(row);
        row.add_css_class("gs-row");
        if let Some(labels) = row.first_child().and_downcast::<gtk::Box>() {
            labels.set_valign(gtk::Align::Center);
            if let Some(title) = labels.first_child().and_downcast::<gtk::Label>() {
                title.add_css_class("gs-row-title");
                if let Some(hint) = title.next_sibling().and_downcast::<gtk::Label>() {
                    for name in ["subtitle", "dim-label"] {
                        hint.remove_css_class(name);
                    }
                    hint.add_css_class("gs-row-subtitle");
                }
            }
        }
        b.append(row);
    }
    Some(b)
}

pub(crate) fn build(_host: &Host, factories: &Factories) -> Result<SettingsPage, String> {
    let controls = tessel_capture::settings::build_page(factories.capture_path.clone());
    let card = shared_card(&controls);
    let screens = factories.remote_control_screens.clone().unwrap_or_else(|| Rc::new(connected_screens));
    let remote = RemoteGroup::new(factories.remote_control_path.clone().unwrap_or_else(remote_control::config_path), screens);
    controls.page.append(&remote.heading);
    controls.page.append(&remote.card);
    controls.page.append(&remote.error);
    let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&controls.page));
    let page = SettingsPage::new(scroll);
    for key in ["copy_to_clipboard", "open_editor"] {
        if let Some(switch) = controls.switches.get(key) {
            page.add_target(key, switch);
        }
    }
    page.add_target("remote_unattended", &remote.switch);
    page.add_target("remote_screen", &remote.screen);
    let closing = remote.clone();
    page.set_close(move || closing.close());
    page.set_controller(Rc::new(CapturePage { controls, card, remote }));
    Ok(page)
}
