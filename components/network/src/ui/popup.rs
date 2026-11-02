//! `network_popup.py`: Network controls in the compact tray popup.

use crate::app::App;
use crate::backend::NetworkBackend;
use crate::gui::Gui;
use crate::model::*;
use crate::ui::style::{accessible_label, children, expand, icon, label, margins, remove_children, style};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use tessel_core::pyjson::casefold;
use tessel_core::tr;
use tessel_gtk::popup::{header_button, popup_footer, popup_header, set_popup_width, HeaderAction, PopupController, POPUP_PADDING, POPUP_WIDTH};

pub fn strength_icon(strength: i64) -> String {
    let level = if strength >= 75 { "excellent" } else if strength >= 50 { "good" } else if strength >= 25 { "ok" } else { "weak" };
    format!("network-wireless-signal-{level}-symbolic")
}

pub fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "wifi" => "network-wireless-symbolic",
        "vpn" | "wireguard" => "network-vpn-symbolic",
        "wired" => "network-wired-symbolic",
        "mobile" => "network-cellular-symbolic",
        "bluetooth" => "bluetooth-symbolic",
        _ => "network-wired-symbolic",
    }
}

pub fn security_name(security: &str) -> String {
    match security {
        "wpa-personal" => "WPA/WPA2".into(),
        "wpa-enterprise" => tr!("Enterprise"),
        "sae" => "WPA3".into(),
        "owe" => tr!("Enhanced open"),
        "wep" => "WEP".into(),
        "open" => tr!("Open"),
        other => other.into(),
    }
}

pub struct NetworkPopup {
    pub window: gtk::Window,
    gui: Weak<Gui>,
    app: Weak<crate::app::AppInner>,
    backend: NetworkBackend,
    syncing: Cell<bool>,
    destroyed: Cell<bool>,
    signature: RefCell<Option<String>>,
    pub subtitle: gtk::Label,
    pub rows_box: gtk::Box,
    pub scroll: gtk::ScrolledWindow,
    pub settings_button: gtk::Button,
    pub network_enable_button: gtk::Button,
    pub wifi_enable_button: gtk::Button,
    pub portal_button: gtk::Button,
    pub wwan_switch: gtk::Switch,
    pub wwan_row: gtk::Box,
    pub hidden_button: gtk::Button,
    pub hotspot_button: gtk::Button,
    pub status: gtk::Label,
    pub network_rows: RefCell<HashMap<String, gtk::Box>>,
    pub device_rows: RefCell<HashMap<String, gtk::Box>>,
    pub device_switches: RefCell<HashMap<String, gtk::Switch>>,
    pub scan_buttons: RefCell<HashMap<String, gtk::Button>>,
    pub vpn_rows: RefCell<HashMap<String, gtk::Box>>,
    pub vpn_switches: RefCell<HashMap<String, gtk::Switch>>,
    controller: RefCell<Option<PopupController>>,
    css: gtk::CssProvider,
    weak: RefCell<Weak<NetworkPopup>>,
}

impl NetworkPopup {
    pub fn new(gui: &Gui, app: &App, edge: &str, offset: i32, embedded: bool) -> Rc<Self> {
        let window = gtk::Window::new();
        style(&window, &["networkmanager"]);
        window.set_title(Some(&tr!("Network")));
        set_popup_width(&window, POPUP_WIDTH);
        let css = gtk::CssProvider::new();
        css.load_from_string(crate::ui::style::STYLE_CSS);
        gtk::style_context_add_provider_for_display(&WidgetExt::display(&window), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let surface = style(&gtk::Box::new(gtk::Orientation::Vertical, 0), &["popup-surface"]);
        window.set_child(Some(&surface));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, POPUP_PADDING);
        surface.append(&expand(&content, &surface));
        let settings_button = header_button("emblem-system-symbolic", &tr!("Network settings"), None, false);
        let heading = popup_header(&["network-wired-symbolic"], &tr!("Network"), Some(&tr!("Connecting to NetworkManager…")),
                                   vec![HeaderAction::Widget(settings_button.clone().upcast())]);
        content.append(&heading.root);
        let network_enable_button = gtk::Button::with_label(&tr!("Enable networking"));
        let wifi_enable_button = gtk::Button::with_label(&tr!("Enable Wi-Fi"));
        let portal_button = gtk::Button::with_label(&tr!("Sign in to network"));
        portal_button.add_css_class("suggested-action");
        portal_button.set_tooltip_text(Some(&tr!("Open this network's sign-in page in your web browser.")));
        for button in [&network_enable_button, &wifi_enable_button, &portal_button] {
            button.set_visible(false);
            content.append(button);
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_overlay_scrolling(false);
        scroll.set_propagate_natural_height(true);
        scroll.set_min_content_height(80);
        scroll.set_max_content_height(490);
        if embedded {
            scroll.set_propagate_natural_height(false);
            scroll.set_max_content_height(-1);
            scroll.set_vexpand(true);
        }
        let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
        rows_box.set_margin_end(5);
        scroll.set_child(Some(&rows_box));
        content.append(&expand(&scroll, &content));
        let mobile = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let wwan_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        wwan_row.append(&expand(&label(&tr!("Mobile broadband"), Some("section-title")), &wwan_row));
        let wwan_switch = gtk::Switch::new();
        wwan_switch.set_valign(gtk::Align::Center);
        accessible_label(&wwan_switch, &tr!("Mobile broadband"));
        wwan_row.append(&wwan_switch);
        mobile.append(&wwan_row);
        wwan_row.set_visible(false);
        content.append(&mobile);
        let hidden_button = gtk::Button::with_label(&tr!("Hidden Network"));
        let hotspot_button = gtk::Button::with_label(&tr!("Create Network"));
        hotspot_button.set_tooltip_text(Some(&tr!("Create a Wi-Fi hotspot with your own network name and security settings.")));
        content.append(&popup_footer(&[Some(hidden_button.clone().upcast()), Some(hotspot_button.clone().upcast())]));
        let status = label("", Some("muted"));
        status.set_ellipsize(gtk::pango::EllipsizeMode::None);
        status.set_wrap(true);
        status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        status.set_max_width_chars(43);
        status.set_visible(false);
        content.append(&status);
        let popup = Rc::new(Self {
            window: window.clone(), gui: gui.weak(), app: app.downgrade(), backend: app.backend(), syncing: Cell::new(false),
            destroyed: Cell::new(false), signature: RefCell::new(None), subtitle: heading.subtitle_label.clone(), rows_box, scroll,
            settings_button: settings_button.clone(), network_enable_button: network_enable_button.clone(),
            wifi_enable_button: wifi_enable_button.clone(), portal_button: portal_button.clone(), wwan_switch: wwan_switch.clone(),
            wwan_row, hidden_button: hidden_button.clone(), hotspot_button: hotspot_button.clone(), status,
            network_rows: RefCell::default(), device_rows: RefCell::default(), device_switches: RefCell::default(),
            scan_buttons: RefCell::default(), vpn_rows: RefCell::default(), vpn_switches: RefCell::default(),
            controller: RefCell::new(None), css, weak: RefCell::new(Weak::new()),
        });
        popup.weak.replace(Rc::downgrade(&popup));
        let w = popup.weak();
        settings_button.connect_clicked(move |_| if let Some(p) = w.upgrade() { if let Some(g) = p.gui.upgrade() { crate::app::Surfaces::show_settings(&*g) } });
        let w = popup.weak();
        network_enable_button.connect_clicked(move |_| if let Some(p) = w.upgrade() { p.perform(|b| Ok(b.set_networking(true))); });
        let w = popup.weak();
        wifi_enable_button.connect_clicked(move |_| if let Some(p) = w.upgrade() { p.perform(|b| Ok(b.set_wireless(true))); });
        let w = popup.weak();
        portal_button.connect_clicked(move |_| if let Some(p) = w.upgrade() { if let Some(app) = p.app() { app.open_captive_portal() } });
        let w = popup.weak();
        wwan_switch.connect_state_set(move |_, enabled| {
            let Some(p) = w.upgrade() else { return glib::Propagation::Proceed };
            if p.syncing.get() {
                return glib::Propagation::Proceed;
            }
            p.perform(|b| Ok(b.set_wwan(enabled)));
            p.refresh();
            glib::Propagation::Stop
        });
        let w = popup.weak();
        hidden_button.connect_clicked(move |_| if let Some(p) = w.upgrade() { if let Some(g) = p.gui.upgrade() { g.wifi_dialog("hidden", None) } });
        let w = popup.weak();
        hotspot_button.connect_clicked(move |_| if let Some(p) = w.upgrade() { if let Some(g) = p.gui.upgrade() { g.wifi_dialog("hotspot", None) } });
        if !embedded {
            let w = popup.weak();
            let controller = PopupController::new(&window, move || if let Some(p) = w.upgrade() { p.apply_state() }, edge, offset, "service_apps");
            popup.controller.replace(Some(controller));
        }
        let w = popup.weak();
        window.connect_unrealize(move |_| if let Some(p) = w.upgrade() { p.on_destroy() });
        popup.refresh();
        popup
    }

    fn weak(&self) -> Weak<NetworkPopup> {
        self.weak.borrow().clone()
    }

    fn app(&self) -> Option<App> {
        self.app.upgrade().map(App)
    }

    fn perform(&self, action: impl FnOnce(&NetworkBackend) -> Result<bool, String>) -> bool {
        match self.app() {
            Some(app) => app.perform(action),
            None => false,
        }
    }

    fn icon_button(&self, name: &str, tooltip: &str, callback: impl Fn(&NetworkPopup) + 'static) -> gtk::Button {
        let button = style(&gtk::Button::new(), &["icon-button"]);
        button.set_valign(gtk::Align::Center);
        button.set_child(Some(&icon(name, 18)));
        button.set_tooltip_text(Some(tooltip));
        accessible_label(&button, tooltip);
        let w = self.weak();
        button.connect_clicked(move |_| if let Some(p) = w.upgrade() { callback(&p) });
        button
    }

    fn show_info(&self, profile: Option<&str>, active: Option<&str>, device: Option<&str>) {
        if let Some(gui) = self.gui.upgrade() {
            gui.show_info(profile, active, device);
        }
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        let state = self.backend.state().clone();
        let app_error = self.app().map(|a| a.error()).unwrap_or_default();
        let ready = state.ready;
        let busy = !state.busy.is_empty();
        let wifi_devices: Vec<&Device> = state.devices.values().filter(|d| d.kind == "wifi").collect();
        let wireless = ready && state.networking_enabled && !wifi_devices.is_empty() && state.wireless_hardware_enabled;
        self.syncing.set(true);
        self.wwan_switch.set_active(state.wwan_enabled);
        self.wwan_switch.set_state(state.wwan_enabled);
        self.syncing.set(false);
        self.network_enable_button.set_visible(ready && !state.networking_enabled);
        self.network_enable_button.set_sensitive(!busy);
        self.wifi_enable_button.set_visible(ready && state.networking_enabled && !wifi_devices.is_empty() && !state.wireless_enabled);
        self.wifi_enable_button.set_sensitive(state.wireless_hardware_enabled && !busy);
        self.portal_button.set_visible(ready && state.networking_enabled && state.connectivity == "Sign-in required");
        let modems = state.devices.values().any(|d| d.kind == "mobile");
        self.wwan_row.set_visible(modems);
        if modems {
            for child in children(&self.wwan_row) {
                child.set_visible(true);
            }
        }
        self.wwan_switch.set_sensitive(ready && state.networking_enabled && !busy && state.wwan_hardware_enabled);
        for control in [&self.hidden_button, &self.hotspot_button] {
            control.set_sensitive(wireless && state.wireless_enabled && !busy && wifi_devices.iter().any(|d| d.enabled()));
        }
        let active: Vec<&ActiveConnection> = state.active.values().filter(|a| a.kind != "loopback").collect();
        let primary = active.iter().find(|a| a.default || a.default6).or(active.first());
        self.subtitle.set_text(&if !ready { tr!("Connecting to NetworkManager…") } else if !state.networking_enabled { tr!("Networking is off") }
                               else if let Some(p) = primary { p.name.clone() } else { tr!("Not connected") });
        let error = if app_error.is_empty() { state.error.clone() } else { app_error };
        let scanning = state.busy.iter().any(|k| k.starts_with("scan:"));
        let message = if !error.is_empty() { error.clone() }
            else if scanning { tr!("Scanning for nearby networks…") }
            else if busy { tr!("Applying network changes…") }
            else if !wifi_devices.is_empty() && !state.wireless_hardware_enabled { tr!("Wi-Fi is blocked by the hardware switch.") }
            else if state.connectivity == "Sign-in required" { tr!("This network requires you to sign in before you can use the internet.") }
            else if state.connectivity == "Limited connectivity" { tessel_core::i18n::translate(&state.connectivity) }
            else if !wifi_devices.is_empty() && !state.wireless_enabled { tr!("Wi-Fi is off.") }
            else { String::new() };
        self.status.set_text(&message);
        self.status.set_visible(!message.is_empty());
        if error.is_empty() { self.status.remove_css_class("error") } else { self.status.add_css_class("error") }
        // Avoid rebuilding focused controls for unchanged notifications.
        let signature = format!("{:?}", (ready, sorted_busy(&state.busy), state.networking_enabled, state.wireless_enabled,
                                         state.wireless_hardware_enabled, &state.devices, &state.networks, &state.profiles, &state.active));
        if self.signature.borrow().as_deref() != Some(signature.as_str()) {
            self.signature.replace(Some(signature));
            let adjustment = self.scroll.vadjustment();
            let position = adjustment.value();
            self.build_rows();
            self.rows_box.set_visible(true);
            adjustment.set_value(position);
        }
    }

    fn section(&self, title: &str) -> gtk::Box {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 6);
        section.append(&label(title, Some("section-title")));
        self.rows_box.append(&section);
        section
    }

    fn build_rows(&self) {
        remove_children(&self.rows_box);
        self.network_rows.borrow_mut().clear();
        self.device_rows.borrow_mut().clear();
        self.device_switches.borrow_mut().clear();
        self.scan_buttons.borrow_mut().clear();
        self.vpn_rows.borrow_mut().clear();
        self.vpn_switches.borrow_mut().clear();
        let state = self.backend.state().clone();
        let enabled = state.ready && state.networking_enabled && state.busy.is_empty();
        if !state.ready {
            self.rows_box.append(&label(&tr!("Waiting for the network service…"), Some("muted")));
            return;
        }
        let mut devices: Vec<Device> = state.devices.values().cloned().collect();
        devices.sort_by(|a, b| (a.active_ids.is_empty(), a.interface.clone()).cmp(&(b.active_ids.is_empty(), b.interface.clone())));
        for (kind, title) in [("wired", tr!("Ethernet/LAN")), ("wifi", tr!("Wi-Fi"))] {
            let adapters: Vec<&Device> = devices.iter().filter(|d| d.kind == kind).collect();
            if adapters.is_empty() {
                continue;
            }
            let section = self.section(&title);
            for device in adapters {
                let row = self.adapter_card(device);
                self.device_rows.borrow_mut().insert(device.id.clone(), row.clone());
                section.append(&row);
            }
        }
        let vpn = vpn_entries(&state.devices, &state.profiles, &state.active);
        let vpn_devices: Vec<&String> = vpn.iter().flat_map(|e| e.device_ids.iter()).collect();
        let others: Vec<&Device> = devices.iter().filter(|d| !vpn_devices.contains(&&d.id)
            && !["wired", "wifi", "loopback", "wifi-p2p"].contains(&d.kind.as_str())
            && (!["tun", "wireguard", "bridge", "veth"].contains(&d.kind.as_str()) || !d.active_ids.is_empty())).collect();
        if !vpn.is_empty() {
            let section = self.section("VPN");
            let mut sorted = vpn.clone();
            sorted.sort_by(|a, b| (!a.enabled, casefold(&a.name), a.id.clone()).cmp(&(!b.enabled, casefold(&b.name), b.id.clone())));
            for entry in &sorted {
                let row = self.vpn_card(entry);
                self.vpn_rows.borrow_mut().insert(entry.id.clone(), row.clone());
                section.append(&row);
            }
        }
        if !others.is_empty() {
            let section = self.section(&tr!("Other connections"));
            for device in others {
                let row = self.device_row(device, enabled);
                self.device_rows.borrow_mut().insert(device.id.clone(), row.clone());
                section.append(&row);
            }
        }
        if self.device_rows.borrow().is_empty() && vpn.is_empty() {
            self.rows_box.append(&label(&tr!("No network adapters found."), Some("muted")));
        }
    }

    fn vpn_busy(&self, entry: &VpnEntry) -> bool {
        let state = self.backend.state();
        let mut targets: Vec<&String> = vec![&entry.profile_id];
        targets.extend(entry.active_ids.iter());
        targets.extend(entry.device_ids.iter());
        targets.retain(|t| !t.is_empty());
        let transitioning = entry.active_ids.iter().any(|p| state.active.get(p).is_some_and(|a| a.state == 3));
        transitioning || state.busy.iter().any(|key| {
            let (action, target) = key.split_once(':').unwrap_or((key.as_str(), ""));
            action == "radio" || action == "networking" || targets.iter().any(|t| *t == target)
        })
    }

    fn vpn_card(&self, entry: &VpnEntry) -> gtk::Box {
        let card = style(&gtk::Box::new(gtk::Orientation::Vertical, 0), &["card", "adapter-card"]);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margins(&header, 12);
        card.append(&expand(&header, &card));
        let (profile, active, device) = (entry.profile_id.clone(), entry.active_ids.first().cloned(), entry.device_ids.first().cloned());
        header.append(&self.icon_button("network-vpn-symbolic", &tr!("Connection information for {value1}", value1 = entry.name.as_str()),
            move |p| p.show_info(Some(&profile).filter(|s| !s.is_empty()).map(|s| s.as_str()), active.as_deref(), device.as_deref())));
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let name = label(&entry.name, Some("device-name"));
        name.set_max_width_chars(27);
        name.set_tooltip_text(Some(&entry.name));
        titles.append(&name);
        let kind = if entry.kind == "wireguard" { tr!("WireGuard") } else { "VPN".into() };
        let state_label = tessel_core::i18n::translate(&entry.state_label);
        let detail = [kind.as_str(), entry.interface.as_str(), state_label.as_str()].iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
        let status = label(&detail, Some("device-detail"));
        status.set_max_width_chars(35);
        status.set_tooltip_text(Some(&detail));
        titles.append(&status);
        if entry.external || !entry.controllable {
            let owner = label(&if entry.external { tr!("Managed externally") } else { tr!("Status only") }, Some("muted"));
            owner.set_tooltip_text(Some(&tr!("Use the application managing this VPN to connect or disconnect it.")));
            titles.append(&owner);
        }
        header.append(&expand(&titles, &header));
        let toggle = gtk::Switch::new();
        toggle.set_valign(gtk::Align::Center);
        toggle.set_active(entry.enabled);
        toggle.set_state(entry.enabled);
        toggle.set_sensitive(entry.controllable && self.backend.state().networking_enabled && !self.vpn_busy(entry));
        toggle.set_tooltip_text(Some(&if entry.controllable {
            format!("{} {}", if entry.enabled { tr!("Disconnect") } else { tr!("Connect") }, entry.name)
        } else { tr!("Use the application managing this VPN to connect or disconnect it.") }));
        accessible_label(&toggle, &entry.name);
        let (w, id) = (self.weak(), entry.id.clone());
        toggle.connect_state_set(move |switch, enabled| {
            let Some(p) = w.upgrade() else { return glib::Propagation::Proceed };
            if p.syncing.get() {
                return glib::Propagation::Proceed;
            }
            switch.set_sensitive(false);
            let (w, id) = (p.weak(), id.clone());
            glib::idle_add_local_once(move || if let Some(p) = w.upgrade() { p.set_vpn_enabled(&id, enabled) });
            glib::Propagation::Stop
        });
        self.vpn_switches.borrow_mut().insert(entry.id.clone(), toggle.clone());
        header.append(&toggle);
        card
    }

    fn set_vpn_enabled(&self, entry_id: &str, enabled: bool) {
        if self.destroyed.get() {
            return;
        }
        let weak = self.weak();
        let id = entry_id.to_owned();
        self.perform(move |backend| {
            let Some(p) = weak.upgrade() else { return Ok(false) };
            p.change_vpn(backend, &id, enabled)
        });
        self.signature.replace(None);
        self.refresh();
    }

    /// `_change_vpn`: apply the requested state even if a notification raced the click.
    pub fn change_vpn(&self, backend: &NetworkBackend, entry_id: &str, enabled: bool) -> Result<bool, String> {
        let state = backend.state().clone();
        let entry = vpn_entries(&state.devices, &state.profiles, &state.active).into_iter().find(|e| e.id == entry_id)
            .ok_or_else(|| tr!("That VPN connection is no longer available."))?;
        if !entry.controllable {
            return Err(tr!("Use the application managing this VPN to change its connection."));
        }
        if !state.ready || !state.networking_enabled || self.vpn_busy(&entry) {
            return Ok(false);
        }
        let active_ids: Vec<String> = entry.active_ids.iter().filter(|p| state.active.get(*p).is_some_and(|a| a.state != 4)).cloned().collect();
        if enabled && active_ids.is_empty() {
            backend.activate_profile(&entry.profile_id, None);
        } else if !enabled {
            for path in active_ids {
                backend.deactivate(&path);
            }
        }
        Ok(true)
    }

    fn device_busy(&self, device: &Device) -> bool {
        let state = self.backend.state();
        for key in &state.busy {
            let (action, target) = match key.split_once(':') {
                Some((a, t)) => (a, t),
                None => (key.as_str(), ""),
            };
            if action == "radio" || action == "networking" || target.is_empty() {
                return true;
            }
            if target == device.id || device.active_ids.iter().any(|a| a == target) {
                return true;
            }
            if action == "wifi" && state.networks.get(target).is_some_and(|n| n.device_id == device.id) {
                return true;
            }
            if (action == "activate" || action == "delete") && state.profiles.get(target).is_some_and(|p| p.device_ids.contains(&device.id)) {
                return true;
            }
        }
        false
    }

    fn adapter_card(&self, device: &Device) -> gtk::Box {
        let state = self.backend.state().clone();
        let card = style(&gtk::Box::new(gtk::Orientation::Vertical, 10), &["card", "adapter-card"]);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
        margins(&body, 12);
        card.append(&expand(&body, &card));
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let id = device.id.clone();
        header.append(&self.icon_button(kind_icon(&device.kind), &tr!("Connection information for {value1}", value1 = device.interface.as_str()),
                                        move |p| p.show_info(None, None, Some(&id))));
        let names = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let product_name = if device.name.is_empty() { &device.interface } else { &device.name };
        let product = label(product_name, Some("device-name"));
        product.set_max_width_chars(25);
        product.set_tooltip_text(Some(product_name));
        names.append(&product);
        let mut enabled = state.networking_enabled && device.enabled();
        if device.kind == "wifi" {
            enabled = enabled && state.wireless_enabled && state.wireless_hardware_enabled;
        }
        let status = if !state.networking_enabled { tr!("Networking is off") }
            else if device.kind == "wifi" && !state.wireless_hardware_enabled { tr!("Hardware blocked") }
            else if device.kind == "wifi" && !state.wireless_enabled { tr!("Wi-Fi is off") }
            else if !device.enabled() { tr!("Disabled") }
            else if device.kind == "wired" && !device.carrier { tr!("Cable unplugged") }
            else { tessel_core::i18n::translate(&device.state_label) };
        let text = format!("{} · {status}", device.interface);
        let details = label(&text, Some("device-detail"));
        details.set_max_width_chars(35);
        details.set_tooltip_text(Some(&text));
        names.append(&details);
        header.append(&expand(&names, &header));
        let busy = self.device_busy(device);
        if device.kind == "wifi" {
            let id = device.id.clone();
            let scan = self.icon_button("view-refresh-symbolic", &tr!("Scan for networks on {value1}", value1 = device.interface.as_str()),
                                        move |p| { p.perform(|b| Ok(b.scan(Some(&id)))); });
            scan.set_sensitive(enabled && !busy);
            header.append(&scan);
            self.scan_buttons.borrow_mut().insert(device.id.clone(), scan);
        }
        let toggle = gtk::Switch::new();
        toggle.set_valign(gtk::Align::Center);
        toggle.set_active(enabled);
        toggle.set_state(enabled);
        toggle.set_sensitive(state.networking_enabled && !busy
                             && (device.kind != "wifi" || (state.wireless_enabled && state.wireless_hardware_enabled)));
        toggle.set_tooltip_text(Some(&format!("{} {}", if enabled { tr!("Disable") } else { tr!("Enable") }, device.interface)));
        accessible_label(&toggle, &format!("{} ({})", device.name, device.interface));
        let (w, id) = (self.weak(), device.id.clone());
        toggle.connect_state_set(move |switch, enabled| {
            let Some(p) = w.upgrade() else { return glib::Propagation::Proceed };
            if p.syncing.get() {
                return glib::Propagation::Proceed;
            }
            // Finish the switch event before an async request refreshes its card.
            switch.set_sensitive(false);
            let (w, id) = (p.weak(), id.clone());
            glib::idle_add_local_once(move || {
                if let Some(p) = w.upgrade() {
                    if p.destroyed.get() {
                        return;
                    }
                    p.perform(|b| Ok(b.set_device_enabled(&id, enabled)));
                    // A preflight rejection may leave the backend unchanged:
                    // still restore the switch from the confirmed state.
                    p.signature.replace(None);
                    p.refresh();
                }
            });
            glib::Propagation::Stop
        });
        self.device_switches.borrow_mut().insert(device.id.clone(), toggle.clone());
        header.append(&toggle);
        body.append(&header);
        if device.kind == "wifi" {
            self.wifi_networks(&body, device, enabled && !busy);
        } else if enabled {
            self.device_connections(&body, device, !busy, false);
        }
        card
    }

    fn wifi_networks(&self, body: &gtk::Box, device: &Device, enabled: bool) {
        let state = self.backend.state().clone();
        if !device.enabled() || !state.networking_enabled || !state.wireless_enabled || !state.wireless_hardware_enabled {
            return;
        }
        let mut networks: Vec<&WifiNetwork> = state.networks.values().filter(|n| n.device_id == device.id).collect();
        networks.sort_by(|a, b| (!a.active, -a.strength, casefold(&a.label)).cmp(&(!b.active, -b.strength, casefold(&b.label))));
        let connected: Vec<&WifiNetwork> = networks.iter().filter(|n| n.active).cloned().collect();
        let available: Vec<&WifiNetwork> = networks.iter().filter(|n| !n.active).cloned().collect();
        for network in &connected {
            let row = self.network_row(network, enabled);
            self.network_rows.borrow_mut().insert(network.id.clone(), row.clone());
            body.append(&row);
        }
        if !device.active_ids.is_empty() && connected.is_empty() {
            self.device_connections(body, device, enabled, true);
        }
        let established = device.state == 100 || device.active_ids.iter()
            .any(|p| state.active.get(p).is_some_and(|a| a.kind == "wifi" && a.state == 2));
        if established {
            return;
        }
        body.append(&label(&tr!("Available networks ({value1})", value1 = available.len()), Some("muted")));
        let listing = gtk::Box::new(gtk::Orientation::Vertical, 9);
        for network in &available {
            let row = self.network_row(network, enabled);
            self.network_rows.borrow_mut().insert(network.id.clone(), row.clone());
            listing.append(&row);
        }
        if available.is_empty() {
            let text = if state.busy.contains(&format!("scan:{}", device.id)) { tr!("Scanning…") } else { tr!("No networks found. Try scanning.") };
            listing.append(&label(&text, Some("muted")));
        }
        body.append(&listing);
    }

    fn network_row(&self, network: &WifiNetwork, enabled: bool) -> gtk::Box {
        let row = style(&gtk::Box::new(gtk::Orientation::Horizontal, 9), &["network-row"]);
        row.append(&icon(&strength_icon(network.strength), 20));
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let name = label(&network.label, None);
        name.set_max_width_chars(24);
        name.set_tooltip_text(Some(&network.label));
        titles.append(&name);
        let device = self.backend.state().devices.get(&network.device_id).cloned();
        let status = if network.active {
            device.map(|d| tessel_core::i18n::translate(&d.state_label)).unwrap_or_else(|| tr!("Connecting"))
        } else if !network.profile_ids.is_empty() { tr!("Saved") } else { security_name(&network.security) };
        titles.append(&label(&format!("{status} · {}%", network.strength), Some("muted")));
        row.append(&expand(&titles, &row));
        let button = gtk::Button::with_label(&if network.active { tr!("Disconnect") } else { tr!("Connect") });
        button.set_sensitive(enabled);
        let (app, id) = (self.app.clone(), network.id.clone());
        button.connect_clicked(move |_| if let Some(app) = app.upgrade() { App(app).connect_network(&id) });
        row.append(&button);
        row
    }

    fn device_row(&self, device: &Device, enabled: bool) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.append(&icon(kind_icon(&device.kind), 20));
        let status = label(&format!("{} · {}", device.interface, tessel_core::i18n::translate(&device.state_label)), Some("muted"));
        status.set_max_width_chars(30);
        top.append(&expand(&status, &top));
        let id = device.id.clone();
        top.append(&self.icon_button("dialog-information-symbolic", &tr!("Connection information"), move |p| p.show_info(None, None, Some(&id))));
        row.append(&top);
        self.device_connections(&row, device, enabled, true);
        row
    }

    fn device_connections(&self, row: &gtk::Box, device: &Device, enabled: bool, disconnect: bool) {
        let state = self.backend.state().clone();
        if !device.active_ids.is_empty() {
            for path in &device.active_ids {
                let Some(active) = state.active.get(path) else { continue };
                if active.vpn {
                    continue;
                }
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let name = label(&active.name, None);
                name.set_max_width_chars(26);
                line.append(&expand(&name, &line));
                if disconnect {
                    let button = gtk::Button::with_label(&tr!("Disconnect"));
                    button.set_sensitive(enabled && device.managed);
                    let (w, path) = (self.weak(), path.clone());
                    button.connect_clicked(move |_| if let Some(p) = w.upgrade() { p.perform(|b| Ok(b.deactivate(&path))); });
                    line.append(&button);
                }
                row.append(&line);
            }
        } else {
            let mut profiles: Vec<&Profile> = state.profiles.values()
                .filter(|p| p.device_ids.contains(&device.id) && p.kind != "vpn" && p.kind != "wireguard").collect();
            if !profiles.is_empty() {
                profiles.sort_by_key(|p| casefold(&p.name));
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                #[allow(deprecated)]
                let combo = gtk::ComboBoxText::new();
                #[allow(deprecated)]
                {
                    for profile in &profiles {
                        combo.append(Some(&profile.id), &profile.name);
                    }
                    combo.set_active(Some(0));
                }
                combo.set_hexpand(true);
                line.append(&expand(&combo, &line));
                let button = gtk::Button::with_label(&tr!("Connect"));
                button.set_sensitive(enabled && device.managed);
                let (w, id) = (self.weak(), device.id.clone());
                #[allow(deprecated)]
                button.connect_clicked(move |_| {
                    if let (Some(p), Some(profile)) = (w.upgrade(), combo.active_id()) {
                        p.perform(|b| Ok(b.activate_profile(&profile, Some(&id))));
                    }
                });
                line.append(&button);
                row.append(&line);
            } else if device.managed {
                let button = gtk::Button::with_label(&tr!("Add Connection"));
                let (w, kind) = (self.weak(), device.kind.clone());
                button.connect_clicked(move |_| if let Some(p) = w.upgrade() { if let Some(g) = p.gui.upgrade() { g.launch_editor(None, Some(&kind)) } });
                row.append(&button);
            }
        }
    }

    fn apply_state(&self) {
        let Some(controller) = self.controller.borrow().clone() else {
            self.refresh();
            return;
        };
        let monitor = gtk4_layer_shell::LayerShell::monitor(&self.window).or_else(|| {
            gtk::gdk::Display::default().and_then(|d| d.monitors().item(0)).and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok())
        });
        if let Some(monitor) = monitor {
            // Leave room for the header, actions and dock offset on short displays.
            let height = monitor.geometry().height() - controller.offset() - 240;
            self.scroll.set_max_content_height(height.clamp(80, 490));
        }
        self.refresh();
    }

    pub fn show(&self) {
        if let Some(c) = self.controller.borrow().clone() {
            c.show();
        }
    }

    pub fn activate(&self) {
        if let Some(c) = self.controller.borrow().clone() {
            c.activate();
        }
    }

    pub fn dismiss(&self) {
        if let Some(c) = self.controller.borrow().clone() {
            c.dismiss();
        }
    }

    pub fn destroyed(&self) -> bool {
        self.destroyed.get()
    }

    /// `popup.controller` (`None` when embedded).
    pub fn controller(&self) -> Option<PopupController> {
        self.controller.borrow().clone()
    }

    pub fn destroy(&self) {
        if self.destroyed.get() {
            return;
        }
        if let Some(c) = self.controller.take() {
            c.close();
        }
        self.on_destroy();
        self.window.destroy();
        if let Some(gui) = self.gui.upgrade() {
            gui.popup_destroyed(self);
        }
    }

    fn on_destroy(&self) {
        if self.destroyed.get() {
            return;
        }
        self.destroyed.set(true);
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(&self.window), &self.css);
    }
}
