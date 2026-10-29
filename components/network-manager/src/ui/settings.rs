//! `network_settings.py`: saved NetworkManager connections with live state.

use crate::app::App;
use crate::backend::NetworkBackend;
use crate::gui::Gui;
use crate::model::Profile;
use crate::ui::info::local_time;
use crate::ui::style::{accessible_label, expand, icon, install_style, label, margins, remove_style, style};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use suite_core::pyjson::casefold;
use suite_core::tr;

pub fn type_name(kind: &str) -> String {
    match kind {
        "wired" => tr!("Ethernet"), "wifi" => "Wi-Fi".into(), "vpn" => "VPN".into(), "wireguard" => tr!("WireGuard"),
        "mobile" | "gsm" | "cdma" => tr!("Mobile broadband"), "bluetooth" => tr!("Bluetooth"), "bridge" => tr!("Bridge"),
        "bond" => tr!("Bond"), "team" => tr!("Team"), "vlan" => "VLAN".into(), "vxlan" => "VXLAN".into(), "macvlan" => "MACVLAN".into(),
        "tun" => tr!("Tunnel"), "ip-tunnel" => "IP tunnel".into(), "dummy" => tr!("Virtual interface"), "vrf" => "VRF".into(),
        "infiniband" => tr!("InfiniBand"), "pppoe" => "DSL".into(), "wimax" => "WiMAX".into(), "loopback" => tr!("Loopback"),
        "6lowpan" => "6LoWPAN".into(), "ovs-bridge" => tr!("Open vSwitch bridge"), "ovs-interface" => tr!("Open vSwitch interface"),
        "ovs-port" => tr!("Open vSwitch port"),
        "" => tr!("Other"),
        other => crate::nm::capitalize(&other.replace('-', " ")),
    }
}

pub fn type_icon(kind: &str) -> &'static str {
    match kind {
        "wifi" => "network-wireless-symbolic",
        "vpn" | "wireguard" => "network-vpn-symbolic",
        "mobile" | "gsm" | "cdma" => "network-cellular-symbolic",
        "bluetooth" => "bluetooth-symbolic",
        _ => "network-wired-symbolic",
    }
}

pub const VIRTUAL_TYPES: &[&str] = &["bridge", "bond", "team", "vlan", "vxlan", "macvlan", "tun", "ip-tunnel", "dummy", "vrf",
                                     "loopback", "6lowpan", "ovs-bridge", "ovs-interface", "ovs-port"];

pub fn last_used(timestamp: u64) -> String {
    if timestamp == 0 {
        return tr!("Never used");
    }
    match local_time(timestamp, "%b %-d, %Y") {
        Some(text) => tr!("Last used {value1}", value1 = text),
        None => tr!("Last used unknown"),
    }
}

/// `profile_busy(profile)`.
pub fn profile_busy(backend: &NetworkBackend, profile: &Profile) -> bool {
    let busy = &backend.state().busy;
    busy.contains(&format!("activate:{}", profile.id)) || busy.contains(&format!("delete:{}", profile.id))
        || profile.active_ids.iter().any(|a| busy.contains(&format!("deactivate:{a}")))
        || profile.device_ids.iter().any(|d| busy.contains(&format!("disconnect:{d}")))
}

/// Type filter (`_row_visible` without the search text).
pub fn type_matches(filter: &str, kind: &str) -> bool {
    match filter {
        "all" => true,
        "vpn" => kind == "vpn" || kind == "wireguard",
        "mobile" => ["mobile", "gsm", "cdma"].contains(&kind),
        "virtual" => VIRTUAL_TYPES.contains(&kind),
        "other" => !VIRTUAL_TYPES.contains(&kind) && !["wifi", "wired", "vpn", "wireguard", "mobile", "gsm", "cdma"].contains(&kind),
        other => other == kind,
    }
}

pub struct ConnectionRow {
    pub row: gtk::ListBoxRow,
    pub profile_id: String,
    image: gtk::Image,
    name_label: gtk::Label,
    active_badge: gtk::Label,
    detail_label: gtk::Label,
    pub connect_button: gtk::Button,
    pub info_button: gtk::Button,
    pub edit_button: gtk::Button,
    pub remove_button: gtk::Button,
}

pub struct NetworkSettingsWindow {
    pub window: gtk::Window,
    gui: Weak<Gui>,
    backend: NetworkBackend,
    pub rows: RefCell<HashMap<String, Rc<ConnectionRow>>>,
    destroyed: Cell<bool>,
    error: RefCell<String>,
    dialogs: RefCell<Vec<gtk::Window>>,
    order: RefCell<Vec<String>>,
    pub add_button: gtk::Button,
    pub search_entry: gtk::SearchEntry,
    #[allow(deprecated)]
    pub type_combo: gtk::ComboBoxText,
    pub count_label: gtk::Label,
    stack: gtk::Stack,
    pub listbox: gtk::ListBox,
    empty_label: gtk::Label,
    pub status_label: gtk::Label,
    css: gtk::CssProvider,
    weak: RefCell<Weak<NetworkSettingsWindow>>,
    /// `_settings_host`: the Global Settings window embedding these controls.
    pub settings_host: RefCell<Option<gtk::Window>>,
}

impl NetworkSettingsWindow {
    #[allow(deprecated)]
    pub fn new(gui: &Gui, backend: &NetworkBackend) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_title(Some(&tr!("Network Settings")));
        window.set_default_size(820, 620);
        window.set_size_request(700, 420);
        style(&window, &["networkmanager"]);
        let css = install_style(&window);
        let header = gtk::HeaderBar::new();
        header.set_show_title_buttons(true);
        header.set_title_widget(Some(&gtk::Label::new(Some(&tr!("Network Settings")))));
        let preferences = style(&gtk::Button::new(), &["icon-button"]);
        preferences.set_child(Some(&icon("preferences-system-notifications-symbolic", 18)));
        preferences.set_tooltip_text(Some(&tr!("Notification preferences")));
        accessible_label(&preferences, &tr!("Notification preferences"));
        header.pack_start(&preferences);
        window.set_titlebar(Some(&header));
        let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
        margins(&page, 24);
        window.set_child(Some(&page));
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 6);
        titles.append(&label(&tr!("Network"), Some("title")));
        titles.append(&label(&tr!("Manage saved connections and network settings"), Some("subtitle")));
        heading.append(&expand(&titles, &heading));
        let add_button = style(&gtk::Button::with_label(&tr!("Add connection")), &["primary"]);
        add_button.set_valign(gtk::Align::Center);
        heading.append(&add_button);
        page.append(&heading);
        let filters = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some(&tr!("Search connections")));
        search_entry.set_hexpand(true);
        filters.append(&expand(&search_entry, &filters));
        let type_combo = gtk::ComboBoxText::new();
        for (key, name) in [("all", tr!("All types")), ("wired", tr!("Ethernet")), ("wifi", "Wi-Fi".into()), ("vpn", "VPN".into()),
                            ("mobile", tr!("Mobile broadband")), ("virtual", tr!("Virtual networks")), ("other", tr!("Other"))] {
            type_combo.append(Some(key), &name);
        }
        type_combo.set_active_id(Some("all"));
        accessible_label(&type_combo, &tr!("Connection type"));
        filters.append(&type_combo);
        page.append(&filters);
        let count_label = label(&tr!("Saved connections"), Some("section-title"));
        page.append(&count_label);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let listbox = style(&gtk::ListBox::new(), &["device-list"]);
        listbox.set_selection_mode(gtk::SelectionMode::None);
        listbox.set_valign(gtk::Align::Start);
        scroller.set_child(Some(&listbox));
        stack.add_named(&scroller, Some("list"));
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty.set_valign(gtk::Align::Center);
        empty.append(&style(&icon("network-workgroup-symbolic", 48), &["empty-icon"]));
        let empty_label = style(&gtk::Label::new(None), &["empty"]);
        empty_label.set_justify(gtk::Justification::Center);
        empty_label.set_wrap(true);
        empty.append(&empty_label);
        stack.add_named(&empty, Some("empty"));
        page.append(&expand(&stack, &page));
        let status_label = label("", Some("status"));
        status_label.set_visible(false);
        status_label.set_wrap(true);
        status_label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        page.append(&status_label);
        let settings = Rc::new(Self {
            window: window.clone(), gui: gui.weak(), backend: backend.clone(), rows: RefCell::default(), destroyed: Cell::new(false),
            error: RefCell::default(), dialogs: RefCell::default(), order: RefCell::default(), add_button: add_button.clone(),
            search_entry: search_entry.clone(), type_combo: type_combo.clone(), count_label, stack, listbox: listbox.clone(),
            empty_label, status_label, css, weak: RefCell::new(Weak::new()), settings_host: RefCell::new(None),
        });
        settings.weak.replace(Rc::downgrade(&settings));
        let g = gui.weak();
        preferences.connect_clicked(move |_| if let Some(g) = g.upgrade() { g.show_preferences() });
        let w = settings.weak();
        add_button.connect_clicked(move |_| if let Some(s) = w.upgrade() { s.launch_editor(None) });
        let w = settings.weak();
        search_entry.connect_search_changed(move |_| if let Some(s) = w.upgrade() { s.filter_rows() });
        let w = settings.weak();
        type_combo.connect_changed(move |_| if let Some(s) = w.upgrade() { s.filter_rows() });
        let w = settings.weak();
        listbox.set_filter_func(move |row| w.upgrade().is_some_and(|s| s.row_visible_for(row)));
        let w = settings.weak();
        suite_gtk::utils::key_controller(&window, false, move |_, key, _, state| {
            let Some(s) = w.upgrade() else { return glib::Propagation::Proceed };
            if key == gtk::gdk::Key::Escape {
                s.window.close();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::f && state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                s.search_entry.grab_focus();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        let w = settings.weak();
        window.connect_unrealize(move |_| if let Some(s) = w.upgrade() { s.on_destroy() });
        let w = settings.weak();
        window.connect_destroy(move |_| {
            if let Some(s) = w.upgrade() {
                s.on_destroy();
                if let Some(gui) = s.gui.upgrade() {
                    gui.settings_destroyed();
                }
            }
        });
        settings.refresh();
        settings
    }

    fn weak(&self) -> Weak<NetworkSettingsWindow> {
        self.weak.borrow().clone()
    }

    fn app(&self) -> Option<App> {
        self.gui.upgrade().and_then(|g| g.app())
    }

    fn connection_row(&self, profile: &Profile) -> Rc<ConnectionRow> {
        let row = gtk::ListBoxRow::new();
        row.set_selectable(false);
        row.set_activatable(false);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        margins(&body, 14);
        row.set_child(Some(&body));
        let image = style(&icon(type_icon(&profile.kind), 23), &["device-icon"]);
        image.set_valign(gtk::Align::Center);
        body.append(&image);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 5);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name_label = label("", Some("device-name"));
        name_label.set_hexpand(true);
        name_label.set_max_width_chars(34);
        heading.append(&expand(&name_label, &heading));
        let active_badge = style(&gtk::Label::new(None), &["badge", "connected"]);
        active_badge.set_visible(false);
        active_badge.set_valign(gtk::Align::Center);
        heading.append(&active_badge);
        names.append(&heading);
        let detail_label = label("", Some("device-detail"));
        detail_label.set_max_width_chars(55);
        names.append(&detail_label);
        body.append(&expand(&names, &body));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        actions.set_valign(gtk::Align::Center);
        let connect_button = style(&gtk::Button::new(), &["connect-action"]);
        actions.append(&connect_button);
        let action = |image: &str, tooltip: &str| {
            let button = style(&gtk::Button::new(), &["icon-button"]);
            button.set_child(Some(&icon(image, 16)));
            button.set_tooltip_text(Some(tooltip));
            accessible_label(&button, tooltip);
            actions.append(&button);
            button
        };
        let info_button = action("dialog-information-symbolic", &tr!("Connection information"));
        let edit_button = action("emblem-system-symbolic", &tr!("Edit connection"));
        let remove_button = action("user-trash-symbolic", &tr!("Remove saved connection"));
        body.append(&actions);
        let id = profile.id.clone();
        let w = self.weak();
        connect_button.connect_clicked({ let id = id.clone(); let w = w.clone(); move |_| if let Some(s) = w.upgrade() { s.connect_clicked(&id) } });
        info_button.connect_clicked({ let id = id.clone(); let w = w.clone(); move |_| if let Some(s) = w.upgrade() { s.info_clicked(&id) } });
        edit_button.connect_clicked({ let id = id.clone(); let w = w.clone(); move |_| if let Some(s) = w.upgrade() {
            if s.current(&id).is_some() { s.launch_editor(Some(&id)) } } });
        remove_button.connect_clicked({ let w = w.clone(); move |_| if let Some(s) = w.upgrade() {
            if let Some(profile) = s.current(&id) { s.confirm_remove(&profile); } } });
        unsafe { row.set_data("profile-id", profile.id.clone()) };
        Rc::new(ConnectionRow { row, profile_id: profile.id.clone(), image, name_label, active_badge, detail_label, connect_button,
                                info_button, edit_button, remove_button })
    }

    fn refresh_row(&self, row: &ConnectionRow, profile: &Profile) {
        let state = self.backend.state().clone();
        let active: Vec<_> = profile.active_ids.iter().filter_map(|a| state.active.get(a)).filter(|a| a.state != 3 && a.state != 4).collect();
        row.name_label.set_text(&profile.name);
        row.name_label.set_tooltip_text(Some(&profile.name));
        let mut parts = vec![type_name(&profile.kind)];
        let mut interfaces: Vec<String> = Vec::new();
        for connection in &active {
            for id in &connection.device_ids {
                if let Some(device) = state.devices.get(id) {
                    if !interfaces.contains(&device.interface) {
                        interfaces.push(device.interface.clone());
                    }
                }
            }
        }
        let interface = if interfaces.is_empty() { profile.interface.clone() } else { interfaces.join(", ") };
        if !interface.is_empty() {
            parts.push(interface);
        }
        parts.push(if active.iter().any(|a| a.state == 2) { tr!("Connected") } else { last_used(profile.timestamp) });
        let text = parts.join(" · ");
        row.detail_label.set_text(&text);
        row.detail_label.set_tooltip_text(Some(&format!("{text}\n{}", profile.id)));
        row.image.set_icon_name(Some(type_icon(&profile.kind)));
        row.active_badge.set_visible(!active.is_empty());
        row.active_badge.set_text(&if !active.is_empty() && active.iter().all(|a| a.state == 1) { tr!("Connecting") }
                                   else if active.len() > 1 { tr!("{value1} active", value1 = active.len()) } else { tr!("Active") });
        let busy = profile_busy(&self.backend, profile);
        row.connect_button.set_label(&if busy { tr!("Working…") } else if !active.is_empty() { tr!("Disconnect") } else { tr!("Connect") });
        row.connect_button.set_sensitive(state.ready && !busy);
        row.edit_button.set_sensitive(state.ready && !busy);
        row.remove_button.set_sensitive(state.ready && !busy);
        row.info_button.set_sensitive(state.ready);
    }

    fn current(&self, uuid: &str) -> Option<Profile> {
        let profile = self.backend.state().profiles.get(uuid).cloned();
        if profile.is_none() {
            self.set_error(&tr!("This saved connection is no longer available."));
        }
        profile
    }

    fn connect_clicked(&self, uuid: &str) {
        let Some(profile) = self.current(uuid) else { return };
        if profile_busy(&self.backend, &profile) {
            return;
        }
        let active: Vec<String> = profile.active_ids.iter()
            .filter(|a| self.backend.state().active.get(*a).is_some_and(|x| x.state != 3 && x.state != 4)).cloned().collect();
        if !active.is_empty() {
            for item in active {
                self.perform(move |b| Ok(b.deactivate(&item)));
            }
        } else {
            let id = profile.id.clone();
            self.perform(move |b| Ok(b.activate_profile(&id, None)));
        }
    }

    fn info_clicked(&self, uuid: &str) {
        let Some(profile) = self.current(uuid) else { return };
        let state = self.backend.state().clone();
        let active = profile.active_ids.iter().find(|a| state.active.contains_key(*a)).cloned();
        let device = profile.device_ids.iter().find(|d| state.devices.contains_key(*d)).cloned();
        if let Some(gui) = self.gui.upgrade() {
            gui.show_info(Some(&profile.id), active.as_deref(), device.as_deref());
        }
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        let state = self.backend.state().clone();
        let stale: Vec<String> = self.rows.borrow().keys().filter(|k| !state.profiles.contains_key(k.as_str())).cloned().collect();
        for uuid in stale {
            if let Some(row) = self.rows.borrow_mut().remove(&uuid) {
                self.listbox.remove(&row.row);
            }
        }
        for (uuid, profile) in state.profiles.iter() {
            let existing = self.rows.borrow().get(uuid).cloned();
            let row = existing.unwrap_or_else(|| {
                let row = self.connection_row(profile);
                self.listbox.append(&row.row);
                row.row.set_visible(true);
                self.rows.borrow_mut().insert(uuid.clone(), row.clone());
                row
            });
            self.refresh_row(&row, profile);
        }
        let mut order: Vec<String> = state.profiles.keys().cloned().collect();
        order.sort_by_key(|uuid| {
            let p = state.profiles.get(uuid).unwrap();
            (p.active_ids.is_empty(), casefold(&type_name(&p.kind)), casefold(&p.name), uuid.clone())
        });
        if *self.order.borrow() != order {
            self.order.replace(order.clone());
            let positions: HashMap<String, usize> = order.iter().enumerate().map(|(i, u)| (u.clone(), i)).collect();
            let total = positions.len();
            self.listbox.set_sort_func(move |a, b| {
                let key = |row: &gtk::ListBoxRow| unsafe { row.data::<String>("profile-id").map(|p| p.as_ref().clone()) }
                    .and_then(|id| positions.get(&id).copied()).unwrap_or(total);
                key(a).cmp(&key(b)).into()
            });
            self.listbox.invalidate_sort();
        }
        self.add_button.set_sensitive(state.ready);
        self.filter_rows();
        let error = self.error.borrow().clone();
        let mut message = if error.is_empty() { state.error.clone() } else { error.clone() };
        if message.is_empty() && !state.ready {
            message = tr!("Connecting to NetworkManager…");
        }
        self.status_label.set_text(&message);
        self.status_label.set_visible(!message.is_empty());
        if !error.is_empty() || !state.error.is_empty() {
            self.status_label.add_css_class("error");
        } else {
            self.status_label.remove_css_class("error");
        }
    }

    fn row_visible_for(&self, row: &gtk::ListBoxRow) -> bool {
        let Some(id) = (unsafe { row.data::<String>("profile-id").map(|p| p.as_ref().clone()) }) else { return false };
        self.row_visible(&id)
    }

    pub fn row_visible(&self, uuid: &str) -> bool {
        let state = self.backend.state();
        let Some(profile) = state.profiles.get(uuid) else { return false };
        let query = casefold(self.search_entry.text().trim());
        let device_names: Vec<String> = profile.device_ids.iter().filter_map(|d| state.devices.get(d))
            .map(|d| format!("{} {}", d.interface, d.name)).collect();
        let text = [profile.name.clone(), type_name(&profile.kind), profile.interface.clone(), device_names.join(" "), profile.id.clone()].join(" ");
        if !query.is_empty() && !casefold(&text).contains(&query) {
            return false;
        }
        #[allow(deprecated)]
        let kind = self.type_combo.active_id().map(|s| s.to_string()).unwrap_or_else(|| "all".into());
        type_matches(&kind, &profile.kind)
    }

    pub fn filter_rows(&self) {
        if self.destroyed.get() {
            return;
        }
        self.listbox.invalidate_filter();
        let ids: Vec<String> = self.rows.borrow().keys().cloned().collect();
        let count = ids.iter().filter(|id| self.row_visible(id)).count();
        let total = self.backend.state().profiles.len();
        self.count_label.set_text(&if count == total { tr!("Saved connections · {value1}", value1 = count) }
                                   else { tr!("Saved connections · {value1} of {value2}", value1 = count, value2 = total) });
        self.stack.set_visible_child_name(if count > 0 { "list" } else { "empty" });
        self.empty_label.set_text(&if total > 0 { tr!("No matching connections") }
                                   else if self.backend.ready() { tr!("No saved connections\nAdd a connection to get started.") }
                                   else { tr!("Loading saved connections…") });
    }

    pub fn perform(&self, action: impl FnOnce(&NetworkBackend) -> Result<bool, String>) -> bool {
        if self.destroyed.get() || !self.backend.ready() {
            return false;
        }
        self.error.replace(String::new());
        let result = self.app().is_some_and(|app| app.perform(action));
        self.refresh();
        result
    }

    pub fn launch_editor(&self, uuid: Option<&str>) {
        if self.destroyed.get() || !self.backend.ready() {
            return;
        }
        self.error.replace(String::new());
        if let Some(gui) = self.gui.upgrade() {
            gui.launch_editor(uuid, None);
        }
    }

    #[allow(deprecated)]
    pub fn confirm_remove(&self, profile: &Profile) -> Option<gtk::MessageDialog> {
        if !self.backend.ready() || profile_busy(&self.backend, profile) {
            return None;
        }
        let parent = self.settings_host.borrow().clone().unwrap_or_else(|| self.window.clone());
        let dialog = gtk::MessageDialog::new(Some(&parent), gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
                                             gtk::MessageType::Question, gtk::ButtonsType::None, &tr!("Remove saved connection?"));
        dialog.set_secondary_text(Some(&format!("“{}” and its saved settings will be removed.", profile.name)));
        dialog.add_button(&tr!("Cancel"), gtk::ResponseType::Cancel);
        let remove = dialog.add_button(&tr!("Remove"), gtk::ResponseType::Accept);
        style(&dialog, &["networkmanager"]);
        style(&remove, &["destructive-action"]);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        self.dialogs.borrow_mut().push(dialog.clone().upcast());
        let (w, uuid) = (self.weak(), profile.id.clone());
        dialog.connect_response(move |window, response| {
            let Some(s) = w.upgrade() else { return window.destroy() };
            s.dialogs.borrow_mut().retain(|d| d != window.upcast_ref::<gtk::Window>());
            window.destroy();
            if response == gtk::ResponseType::Accept && !s.destroyed.get() {
                // Check the live profile again before deleting.
                if s.backend.state().profiles.contains_key(&uuid) {
                    let id = uuid.clone();
                    s.perform(move |b| Ok(b.delete_profile(&id)));
                } else {
                    s.set_error(&tr!("This saved connection is no longer available."));
                }
            }
        });
        dialog.set_visible(true);
        Some(dialog)
    }

    pub fn set_error(&self, message: &str) {
        self.error.replace(message.to_owned());
        self.refresh();
    }

    /// `_on_destroy(window)` (also called by an embedding page).
    pub fn on_destroy(&self) {
        if self.destroyed.get() {
            return;
        }
        self.destroyed.set(true);
        for dialog in self.dialogs.take() {
            dialog.destroy();
        }
        remove_style(&self.window, &self.css);
    }
}
