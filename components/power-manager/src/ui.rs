//! Power profiles popup and the locally edited Power Settings window
//! (power_ui.py). No direct system writes: profiles go through the backend,
//! policies through `on_save`.
#![allow(deprecated)] // ComboBoxText/ComboBox/ListStore: kept for parity with the GTK4 Python UI.

use suite_power::backend::PowerSystem;
use crate::config::{validate_settings, CRITICAL_LEVELS};
use suite_power::ui::style::{icon, install_style, label, margins, style};
use gtk::prelude::*;
use gtk::{gdk, glib};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::popup::{header_button, popup_header, set_popup_width, HeaderAction, PopupController, POPUP_PADDING, POPUP_WIDTH};

pub fn profiles() -> [(&'static str, String, String, &'static str); 3] {
    [("power-saver", tr!("Power Saver"), tr!("Use less energy."), "power-profile-power-saver-symbolic"),
     ("balanced", tr!("Balanced"), tr!("Balance power and performance."), "power-profile-balanced-symbolic"),
     ("performance", tr!("Performance"), tr!("Prioritize speed and responsiveness."), "power-profile-performance-symbolic")]
}

pub fn sleep_modes() -> [(&'static str, String); 3] {
    [("suspend", tr!("Sleep")), ("hibernate", tr!("Hibernate")), ("suspend-then-hibernate", tr!("Sleep then hibernate"))]
}

pub const DURATIONS: [i64; 14] = [0, 60, 120, 300, 600, 900, 1800, 2700, 3600, 7200, 10800, 21600, 43200, 86400];

pub fn lid_choices() -> Vec<(String, String)> {
    [("suspend", tr!("Sleep")), ("hibernate", tr!("Hibernate")), ("lock", tr!("Lock screen")), ("poweroff", tr!("Power off")),
     ("nothing", tr!("Do nothing"))].into_iter().map(|(a, b)| (a.to_owned(), b)).collect()
}

pub fn button_choices() -> Vec<(String, String)> {
    [("menu", tr!("Show power menu")), ("suspend", tr!("Sleep")), ("hibernate", tr!("Hibernate")), ("poweroff", tr!("Power off")),
     ("nothing", tr!("Do nothing"))].into_iter().map(|(a, b)| (a.to_owned(), b)).collect()
}

pub fn critical_choices() -> Vec<(String, String)> {
    [("auto", tr!("Hibernate, or power off if unavailable")), ("hibernate", tr!("Hibernate")), ("poweroff", tr!("Power off")),
     ("suspend", tr!("Sleep")), ("nothing", tr!("Do nothing"))].into_iter().map(|(a, b)| (a.to_owned(), b)).collect()
}

pub fn critical_level_name(value: i64) -> String {
    if value == 0 { tr!("When the system reports it is almost empty") } else { tr!("At {percent}%", percent = value) }
}

/// Older saved policies gain defaults for options added later.
pub fn complete_settings(settings: &Value) -> Value {
    validate_settings(settings).unwrap_or_else(|_| settings.clone())
}

pub fn profile_name(value: &str) -> String {
    if let Some((_, title, _, _)) = profiles().into_iter().find(|p| p.0 == value) {
        return title;
    }
    if value.is_empty() {
        return tr!("Profile unavailable");
    }
    let text = value.replace('-', " ");
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()).unwrap_or_default()
}

pub fn power_status(backend: &dyn PowerSystem) -> String {
    let s = backend.state();
    if !s.power_ready || !s.power_source_known {
        return tr!("Power source unavailable");
    }
    let mut source = if s.on_battery { tr!("On battery") } else { tr!("Plugged in") };
    if let Some(percentage) = s.percentage.filter(|p| s.has_battery && p.is_finite()) {
        source += &format!(" · {}%", suite_core::pyfmt::format_value(&suite_core::Arg::from(percentage.clamp(0.0, 100.0)), ".0f").unwrap_or_default());
    }
    source
}

/// Map raw errors (paths, errno text) to plain status text; log the details.
pub fn friendly_message(message: &str) -> String {
    let mapping = [
        ("screen lock command is missing", tr!("Automatic screen locking is not set up yet.")),
        ("requires swayidle", tr!("Automatic dimming and sleep need the swayidle and wlopm tools.")),
        ("Idle power management stopped", tr!("Automatic dimming and sleep stopped. They restart when settings change.")),
        ("Could not start idle power management", tr!("Automatic dimming and sleep could not start.")),
    ];
    let pairs: Vec<(&str, &str)> = mapping.iter().map(|(a, b)| (*a, b.as_str())).collect();
    suite_gtk::popup::friendly_status(message, &tr!("Some power settings could not be applied. See the log for details."), &pairs)
}

pub fn shared_status(app_error: &str, backend: &dyn PowerSystem) -> String {
    let s = backend.state();
    let mut messages: Vec<String> = [app_error, s.error.as_str()].iter().filter(|v| !v.is_empty()).map(|v| friendly_message(v)).collect();
    if !s.profiles_ready {
        messages.push(tr!("Power profiles are unavailable."));
    }
    if !s.performance_degraded.is_empty() {
        messages.push(tr!("Performance is limited: {value1}.", value1 = s.performance_degraded.replace('-', " ")));
    }
    let mut unique: Vec<String> = Vec::new();
    for message in messages {
        if !unique.contains(&message) {
            unique.push(message);
        }
    }
    unique.join("\n")
}

pub fn duration_name(seconds: i64) -> String {
    if seconds == 0 {
        return tr!("Never");
    }
    if seconds % 3600 == 0 {
        let value = seconds / 3600;
        return if value == 1 { tr!("{count} hour", count = value) } else { tr!("{count} hours", count = value) };
    }
    if seconds % 60 == 0 {
        let value = seconds / 60;
        return if value == 1 { tr!("{count} minute", count = value) } else { tr!("{count} minutes", count = value) };
    }
    if seconds == 1 { tr!("{count} second", count = seconds) } else { tr!("{count} seconds", count = seconds) }
}

pub fn choice(choices: &[(String, String)], value: &str, accessible_name: &str) -> gtk::ComboBoxText {
    let combo = gtk::ComboBoxText::new();
    for (id, title) in choices {
        combo.append(Some(id), title);
    }
    combo.set_active_id(Some(value));
    suite_gtk::utils::accessible(&combo, accessible_name);
    combo
}

pub fn duration_choice(seconds: i64, accessible_name: &str) -> gtk::ComboBoxText {
    let mut values: Vec<i64> = DURATIONS.to_vec();
    if !values.contains(&seconds) {
        values.push(seconds);
    }
    values.sort();
    let choices: Vec<(String, String)> = values.iter().map(|v| (v.to_string(), duration_name(*v))).collect();
    choice(&choices, &seconds.to_string(), accessible_name)
}

pub fn seconds_of(combo: &impl IsA<gtk::ComboBox>) -> Option<i64> {
    combo.active_id().and_then(|id| id.parse().ok())
}

/// Buttons avoid profile writes from programmatic selection refreshes.
pub struct ProfileSelector {
    pub root: gtk::Box,
    pub buttons: BTreeMap<&'static str, gtk::Button>,
    pub checks: BTreeMap<&'static str, gtk::Image>,
    backend: Rc<dyn PowerSystem>,
}

impl ProfileSelector {
    pub fn new(backend: Rc<dyn PowerSystem>, changed: Rc<dyn Fn()>, failed: Rc<dyn Fn(&str)>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let mut buttons = BTreeMap::new();
        let mut checks = BTreeMap::new();
        for (id, title, description, image) in profiles() {
            let button = style(&gtk::Button::new(), &["profile-row"]);
            suite_gtk::utils::accessible(&button, &title);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            suite_gtk::utils::append(&row, &icon(image, 23), false, false);
            let titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
            suite_gtk::utils::append(&titles, &label(&title, Some("row-title"), false), false, false);
            suite_gtk::utils::append(&titles, &label(&description, Some("status"), false), false, false);
            suite_gtk::utils::append(&row, &titles, true, false);
            let check = style(&icon("object-select-symbolic", 18), &["profile-check"]);
            suite_gtk::utils::append(&row, &check, false, false);
            button.set_child(Some(&row));
            suite_gtk::utils::append(&root, &button, false, false);
            buttons.insert(id, button);
            checks.insert(id, check);
        }
        let selector = Rc::new(Self { root, buttons, checks, backend });
        for (id, button) in &selector.buttons {
            let (weak, changed, failed, id) = (Rc::downgrade(&selector), changed.clone(), failed.clone(), *id);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.selected(id, &*changed, &*failed);
                }
            });
        }
        selector
    }

    fn selected(&self, id: &str, changed: &dyn Fn(), failed: &dyn Fn(&str)) {
        {
            let s = self.backend.state();
            if !s.profiles_ready || s.busy || !s.profiles.iter().any(|p| p == id) || id == s.active_profile {
                return;
            }
        }
        failed("");
        let accepted = self.backend.set_profile(id);
        if !accepted && self.backend.state().error.is_empty() {
            failed(&tr!("The power profile could not be changed."));
        }
        changed();
    }

    pub fn refresh(&self) {
        let s = self.backend.state();
        for (id, button) in &self.buttons {
            let selected = s.active_profile == *id;
            if selected { button.add_css_class("selected") } else { button.remove_css_class("selected") }
            self.checks[id].set_visible(selected);
            let available = s.profiles_ready && s.profiles.iter().any(|p| p == id);
            button.set_sensitive(available && !s.busy);
            let tooltip = if available { None } else { Some(tr!("This power profile is unavailable.")) };
            button.set_tooltip_text(tooltip.as_deref());
        }
    }
}

pub type AppError = Rc<dyn Fn() -> String>;

pub struct PowerPopup {
    pub window: gtk::Window,
    pub settings_button: gtk::Button,
    pub source_icon: gtk::Image,
    pub source_label: gtk::Label,
    pub profile_label: gtk::Label,
    pub selector: Rc<ProfileSelector>,
    pub status_label: gtk::Label,
    pub controller: RefCell<Option<PopupController>>,
    backend: Rc<dyn PowerSystem>,
    app_error: AppError,
    message: RefCell<String>,
    destroyed: Cell<bool>,
    css: gtk::CssProvider,
}

impl PowerPopup {
    pub fn new(backend: Rc<dyn PowerSystem>, app_error: AppError, open_settings: Rc<dyn Fn()>, edge: &str, offset: i32) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_title(Some(&tr!("Power")));
        set_popup_width(&window, POPUP_WIDTH);
        style(&window, &["powermanager"]);
        let css = install_style(&window);
        let surface = style(&gtk::Box::new(gtk::Orientation::Vertical, 0), &["popup-surface"]);
        window.set_child(Some(&surface));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, POPUP_PADDING);
        surface.append(&content);
        let settings_button = header_button("emblem-system-symbolic", &tr!("Power settings"), None, false);
        let heading = popup_header(&["power-profile-balanced-symbolic", "preferences-system-power-symbolic"], &tr!("Power"),
                                   Some(&tr!("Profiles & sleep settings")), vec![HeaderAction::Widget(settings_button.clone().upcast())]);
        suite_gtk::utils::append(&content, &heading.root, false, false);
        // A static heading: the selected row already shows the active profile.
        let profile_label = label(&tr!("Power mode"), Some("section-title"), false);
        suite_gtk::utils::append(&content, &profile_label, false, false);
        let popup = Rc::new_cyclic(|weak: &Weak<PowerPopup>| {
            let (refresh_weak, message_weak) = (weak.clone(), weak.clone());
            let selector = ProfileSelector::new(backend.clone(),
                Rc::new(move || if let Some(p) = refresh_weak.upgrade() { p.refresh() }),
                Rc::new(move |m: &str| if let Some(p) = message_weak.upgrade() { p.message.replace(m.into()); }));
            Self { window: window.clone(), settings_button: settings_button.clone(), source_icon: heading.icon.clone(),
                   source_label: heading.subtitle_label.clone(), profile_label, selector, status_label: label("", Some("status"), true),
                   controller: RefCell::new(None), backend, app_error, message: RefCell::new(String::new()),
                   destroyed: Cell::new(false), css }
        });
        suite_gtk::utils::append(&content, &popup.selector.root, false, false);
        suite_gtk::utils::append(&content, &popup.status_label, false, false);
        let weak = Rc::downgrade(&popup);
        let controller = PopupController::new(&window, move || if let Some(p) = weak.upgrade() { p.refresh() }, edge, offset, "service_apps");
        popup.controller.replace(Some(controller));
        let weak = Rc::downgrade(&popup);
        settings_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.dismiss();
                open_settings();
            }
        });
        let weak = Rc::downgrade(&popup);
        window.connect_unrealize(move |w| {
            if let Some(p) = weak.upgrade() {
                p.destroyed.set(true);
                gtk::style_context_remove_provider_for_display(&WidgetExt::display(w), &p.css);
            }
        });
        popup.refresh();
        popup
    }

    pub fn destroyed(&self) -> bool {
        self.destroyed.get()
    }

    pub fn set_message(&self, message: &str) {
        self.message.replace(message.into());
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        self.source_label.set_text(&tr!("Profiles & sleep settings"));
        self.selector.refresh();
        let message = friendly_message(&self.message.borrow());
        let shared = shared_status(&(self.app_error)(), &*self.backend);
        let mut status = [message, shared].into_iter().filter(|v| !v.is_empty()).collect::<Vec<_>>().join("\n");
        if self.backend.state().busy && status.is_empty() {
            status = tr!("Updating power settings…");
        }
        self.status_label.set_text(&status);
        self.status_label.set_visible(!status.is_empty());
    }

    fn controller(&self) -> Option<PopupController> {
        self.controller.borrow().clone()
    }

    pub fn show(&self) {
        if !self.destroyed.get() {
            if let Some(c) = self.controller() { c.show() }
        }
    }

    pub fn activate(&self) {
        if !self.destroyed.get() {
            if let Some(c) = self.controller() { c.activate() }
        }
    }

    pub fn dismiss(&self) {
        if !self.destroyed.get() {
            if let Some(c) = self.controller() { c.dismiss() }
        }
    }

    pub fn destroy(&self) {
        if !self.destroyed.get() {
            self.window.destroy();
        }
    }
}

/// Sleep modes with per-row sensitivity (a ComboBox over a ListStore).
pub struct SleepModeChoice {
    pub combo: gtk::ComboBox,
    pub choices: gtk::ListStore,
}

impl SleepModeChoice {
    pub fn new(action: &str, accessible_name: &str) -> Self {
        let choices = gtk::ListStore::new(&[glib::Type::STRING, glib::Type::STRING, glib::Type::BOOL]);
        let combo = gtk::ComboBox::with_model(&choices);
        combo.set_id_column(0);
        let renderer = gtk::CellRendererText::new();
        combo.pack_start(&renderer, true);
        combo.add_attribute(&renderer, "text", 1);
        combo.add_attribute(&renderer, "sensitive", 2);
        for (id, title) in sleep_modes() {
            choices.insert_with_values(None, &[(0, &id), (1, &title), (2, &true)]);
        }
        combo.set_active_id(Some(action));
        suite_gtk::utils::accessible(&combo, accessible_name);
        Self { combo, choices }
    }

    /// `{id: sensitive}` of every row (tests: Python `{row[0]: row[2]}`).
    pub fn rows(&self) -> Vec<(String, String, bool)> {
        let mut rows = Vec::new();
        if let Some(mut iter) = self.choices.iter_first() {
            loop {
                rows.push((self.choices.get::<String>(&iter, 0), self.choices.get::<String>(&iter, 1), self.choices.get::<bool>(&iter, 2)));
                if !self.choices.iter_next(&mut iter) {
                    break;
                }
            }
        }
        rows
    }

    pub fn refresh(&self, backend: &dyn PowerSystem) {
        let s = backend.state();
        let mut any = false;
        if let Some(mut iter) = self.choices.iter_first() {
            for (_, title) in sleep_modes() {
                let id: String = self.choices.get(&iter, 0);
                let capability = if s.sleep_ready { s.capability(&id).unwrap_or("na").to_owned() } else { "pending".into() };
                let enabled = capability == "yes";
                any |= enabled;
                let suffix = match capability.as_str() {
                    "yes" => "",
                    "challenge" => " — authorization required",
                    "pending" => " — checking…",
                    _ => " — unavailable",
                };
                self.choices.set(&iter, &[(1, &format!("{title}{suffix}")), (2, &enabled)]);
                if !self.choices.iter_next(&mut iter) {
                    break;
                }
            }
        }
        self.combo.set_sensitive(s.sleep_ready && any);
    }
}

pub enum Control {
    Duration(gtk::ComboBoxText),
    Sleep(SleepModeChoice),
}

impl Control {
    pub fn combo(&self) -> gtk::ComboBox {
        match self {
            Control::Duration(c) => c.clone().upcast(),
            Control::Sleep(s) => s.combo.clone(),
        }
    }

    pub fn active_id(&self) -> Option<String> {
        self.combo().active_id().map(String::from)
    }

    pub fn set_active_id(&self, id: &str) -> bool {
        self.combo().set_active_id(Some(id))
    }
}

pub type SaveFn = Rc<dyn Fn(Value) -> bool>;

pub struct PowerSettingsWindow {
    pub window: gtk::Window,
    pub source_label: gtk::Label,
    pub selector: Rc<ProfileSelector>,
    pub policy_cards: BTreeMap<&'static str, gtk::Box>,
    pub controls: BTreeMap<&'static str, BTreeMap<&'static str, Control>>,
    pub policy_hints: BTreeMap<&'static str, gtk::Label>,
    pub lock_choice: gtk::ComboBoxText,
    pub dim_switch: gtk::Switch,
    pub extra_controls: Vec<(Vec<&'static str>, gtk::ComboBoxText)>,
    pub lid_rows: Vec<gtk::Widget>,
    pub buttons_hint: gtk::Label,
    pub battery_card: gtk::Box,
    pub battery_hint: gtk::Label,
    pub status_label: gtk::Label,
    pub close_button: gtk::Button,
    pub save_button: gtk::Button,
    pub settings: RefCell<Value>,
    pub saved: RefCell<Value>,
    backend: Rc<dyn PowerSystem>,
    app_error: AppError,
    on_save: SaveFn,
    message: RefCell<String>,
    syncing: Cell<bool>,
    destroyed: Cell<bool>,
    css: gtk::CssProvider,
}

fn card(title: &str, explanation: &str) -> (gtk::Box, gtk::Box) {
    let card = style(&gtk::Box::new(gtk::Orientation::Vertical, 0), &["card"]);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    margins(&body, 16);
    card.append(&body);
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 4);
    suite_gtk::utils::append(&heading, &label(title, Some("section-title"), false), false, false);
    suite_gtk::utils::append(&heading, &label(explanation, Some("status"), true), false, false);
    suite_gtk::utils::append(&body, &heading, false, false);
    (card, body)
}

fn path_value<'a>(settings: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(settings, |value, key| &value[*key])
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl PowerSettingsWindow {
    pub fn new(backend: Rc<dyn PowerSystem>, app_error: AppError, settings: &Value, on_save: SaveFn) -> Rc<Self> {
        let settings = complete_settings(settings);
        let window = gtk::Window::new();
        window.set_title(Some(&tr!("Power Settings")));
        window.set_default_size(580, 760);
        window.set_size_request(500, 460);
        style(&window, &["powermanager"]);
        let css = install_style(&window);
        window.set_titlebar(Some(&suite_gtk::utils::headerbar(&tr!("Power Settings"))));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 15);
        margins(&root, 22);
        window.set_child(Some(&root));
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 4);
        suite_gtk::utils::append(&heading, &label(&tr!("Power & Sleep"), Some("title"), false), false, false);
        let source_label = label("", Some("subtitle"), false);
        suite_gtk::utils::append(&heading, &source_label, false, false);
        suite_gtk::utils::append(&root, &heading, false, false);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_margin_end(6);
        scroll.set_child(Some(&content));
        suite_gtk::utils::append(&root, &scroll, true, false);

        let this = Rc::new_cyclic(|weak: &Weak<PowerSettingsWindow>| {
            let (profile_card, profile_content) = card(&tr!("Power profile"), &tr!("Changes take effect immediately."));
            let (w1, w2) = (weak.clone(), weak.clone());
            let selector = ProfileSelector::new(backend.clone(), Rc::new(move || if let Some(t) = w1.upgrade() { t.refresh() }),
                                                Rc::new(move |m: &str| if let Some(t) = w2.upgrade() { t.message.replace(m.into()); }));
            suite_gtk::utils::append(&profile_content, &selector.root, false, false);
            suite_gtk::utils::append(&content, &profile_card, false, false);
            let mut policy_cards = BTreeMap::new();
            let mut controls = BTreeMap::new();
            let mut policy_hints = BTreeMap::new();
            for (key, title, explanation) in [("ac", tr!("Plugged in"), tr!("While connected to external power.")),
                                              ("battery", "On battery".to_owned(), tr!("While using battery power."))] {
                let (card_box, body) = card(&title, &explanation);
                let grid = gtk::Grid::builder().column_spacing(18).row_spacing(13).build();
                let mut fields = BTreeMap::new();
                for (row, (field, text)) in [("display_off", tr!("Turn off display after")), ("sleep_after", tr!("Sleep after")),
                                             ("sleep_action", tr!("Sleep mode"))].into_iter().enumerate() {
                    let control = if field == "sleep_action" {
                        Control::Sleep(SleepModeChoice::new(settings[key][field].as_str().unwrap_or("suspend"), &format!("{title} sleep mode")))
                    } else {
                        Control::Duration(duration_choice(settings[key][field].as_i64().unwrap_or(0),
                                                          &format!("{title} {}", text.to_lowercase())))
                    };
                    let combo = control.combo();
                    combo.set_hexpand(false);
                    grid.attach(&label(&text, None, false), 0, row as i32, 1, 1);
                    grid.attach(&combo, 1, row as i32, 1, 1);
                    let weak = weak.clone();
                    combo.connect_changed(move |_| if let Some(t) = weak.upgrade() { t.policy_changed(key, field) });
                    fields.insert(field, control);
                }
                if let Some(first) = grid.child_at(0, 0) {
                    first.set_hexpand(true);
                }
                suite_gtk::utils::append(&body, &grid, false, false);
                let hint = label("", Some("status"), true);
                suite_gtk::utils::append(&body, &hint, false, false);
                suite_gtk::utils::append(&content, &card_box, false, false);
                body.set_visible(true);
                policy_cards.insert(key, card_box);
                controls.insert(key, fields);
                policy_hints.insert(key, hint);
            }
            let (lock_card, lock_body) = card(&tr!("Screen lock"), &tr!("Lock the session after inactivity on either power source."));
            let lock_row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
            suite_gtk::utils::append(&lock_row, &label(&tr!("Lock after"), None, false), true, false);
            let lock_choice = duration_choice(settings["lock_after"].as_i64().unwrap_or(0), &tr!("Lock screen after"));
            let w = weak.clone();
            lock_choice.connect_changed(move |_| if let Some(t) = w.upgrade() { t.lock_changed() });
            suite_gtk::utils::append(&lock_row, &lock_choice, false, false);
            suite_gtk::utils::append(&lock_body, &lock_row, false, false);
            let dim_row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
            let dim_titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
            suite_gtk::utils::append(&dim_titles, &label(&tr!("Dim screen before locking"), None, false), false, false);
            suite_gtk::utils::append(&dim_titles, &label(&tr!("Dims the display 30 seconds before it locks or turns off."), Some("status"), true),
                                     false, false);
            suite_gtk::utils::append(&dim_row, &dim_titles, true, false);
            let dim_switch = gtk::Switch::new();
            dim_switch.set_valign(gtk::Align::Center);
            dim_switch.set_active(settings["dim_before_lock"].as_bool().unwrap_or(true));
            suite_gtk::utils::accessible(&dim_switch, &tr!("Dim screen before locking"));
            let w = weak.clone();
            dim_switch.connect_active_notify(move |s| if let Some(t) = w.upgrade() { t.dim_changed(s.is_active()) });
            suite_gtk::utils::append(&dim_row, &dim_switch, false, false);
            suite_gtk::utils::append(&lock_body, &dim_row, false, false);
            suite_gtk::utils::append(&content, &lock_card, false, false);

            let mut extra_controls = Vec::new();
            let (buttons_card, buttons_body) = card(&tr!("Lid and power button"),
                                                    &tr!("Choose what happens when you close the lid or press the power button."));
            let grid = gtk::Grid::builder().column_spacing(18).row_spacing(13).build();
            let mut lid_rows: Vec<gtk::Widget> = Vec::new();
            let rows: [(Vec<&'static str>, String, Vec<(String, String)>); 3] = [
                (vec!["lid_close", "battery"], tr!("Lid closed on battery"), lid_choices()),
                (vec!["lid_close", "ac"], tr!("Lid closed when plugged in"), lid_choices()),
                (vec!["power_button"], tr!("Power button"), button_choices())];
            for (row, (path, text, choices)) in rows.into_iter().enumerate() {
                let widget = choice(&choices, &value_text(path_value(&settings, &path)), &text);
                let title = label(&text, None, false);
                title.set_hexpand(true);
                grid.attach(&title, 0, row as i32, 1, 1);
                grid.attach(&widget, 1, row as i32, 1, 1);
                let (w, p) = (weak.clone(), path.clone());
                widget.connect_changed(move |c| if let Some(t) = w.upgrade() { t.extra_changed(c, &p) });
                if path[0] == "lid_close" {
                    lid_rows.push(title.upcast());
                    lid_rows.push(widget.clone().upcast());
                }
                extra_controls.push((path, widget));
            }
            suite_gtk::utils::append(&buttons_body, &grid, false, false);
            let buttons_hint = label("", Some("status"), true);
            suite_gtk::utils::append(&buttons_body, &buttons_hint, false, false);
            suite_gtk::utils::append(&content, &buttons_card, false, false);
            let (battery_card, battery_body) = card(&tr!("Low battery"),
                &tr!("You are notified at 10% and 5%. When the battery is almost empty, a 60-second warning lets you cancel the action."));
            let grid = gtk::Grid::builder().column_spacing(18).row_spacing(13).build();
            let levels: Vec<(String, String)> = CRITICAL_LEVELS.iter().map(|v| (v.to_string(), critical_level_name(*v))).collect();
            let rows: [(Vec<&'static str>, String, Vec<(String, String)>); 2] = [
                (vec!["critical_action"], tr!("Critical battery action"), critical_choices()),
                (vec!["critical_level"], tr!("Take action"), levels)];
            for (row, (path, text, choices)) in rows.into_iter().enumerate() {
                let widget = choice(&choices, &value_text(path_value(&settings, &path)), &text);
                let title = label(&text, None, false);
                title.set_hexpand(true);
                grid.attach(&title, 0, row as i32, 1, 1);
                grid.attach(&widget, 1, row as i32, 1, 1);
                let (w, p) = (weak.clone(), path.clone());
                widget.connect_changed(move |c| if let Some(t) = w.upgrade() { t.extra_changed(c, &p) });
                extra_controls.push((path, widget));
            }
            suite_gtk::utils::append(&battery_body, &grid, false, false);
            let battery_hint = label("", Some("status"), true);
            suite_gtk::utils::append(&battery_body, &battery_hint, false, false);
            suite_gtk::utils::append(&content, &battery_card, false, false);
            let status_label = label("", Some("status"), true);
            suite_gtk::utils::append(&root, &status_label, false, false);
            let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let close_button = gtk::Button::with_label(&tr!("Close"));
            let save_button = style(&gtk::Button::with_label(&tr!("Save Changes")), &["primary"]);
            footer.set_halign(gtk::Align::End);
            suite_gtk::utils::append(&footer, &close_button, false, false);
            suite_gtk::utils::append(&footer, &save_button, false, false);
            suite_gtk::utils::append(&root, &footer, false, false);
            Self { window: window.clone(), source_label: source_label.clone(), selector, policy_cards, controls, policy_hints,
                   lock_choice, dim_switch, extra_controls, lid_rows, buttons_hint, battery_card, battery_hint, status_label,
                   close_button, save_button, saved: RefCell::new(settings.clone()), settings: RefCell::new(settings.clone()),
                   backend, app_error, on_save, message: RefCell::new(String::new()), syncing: Cell::new(false),
                   destroyed: Cell::new(false), css }
        });
        let w = window.clone();
        this.close_button.connect_clicked(move |_| w.destroy());
        let weak = Rc::downgrade(&this);
        this.save_button.connect_clicked(move |_| if let Some(t) = weak.upgrade() { t.save() });
        let w = window.clone();
        suite_gtk::utils::key_controller(&window, false, move |_, key, _, _| {
            if key == gdk::Key::Escape {
                w.destroy();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(&this);
        window.connect_unrealize(move |w| {
            if let Some(t) = weak.upgrade() {
                t.destroyed.set(true);
                gtk::style_context_remove_provider_for_display(&WidgetExt::display(w), &t.css);
            }
        });
        this.refresh();
        this
    }

    pub fn destroyed(&self) -> bool {
        self.destroyed.get()
    }

    pub fn extra(&self, path: &[&str]) -> &gtk::ComboBoxText {
        &self.extra_controls.iter().find(|(p, _)| p.as_slice() == path).expect("extra control").1
    }

    fn policy_changed(&self, key: &'static str, field: &'static str) {
        let control = &self.controls[key][field];
        let Some(id) = control.active_id().filter(|_| !self.syncing.get()) else { return };
        let value: Value = if field == "sleep_action" { id.clone().into() } else { id.parse::<i64>().unwrap_or(0).into() };
        if field == "sleep_action" {
            let allowed = {
                let s = self.backend.state();
                s.sleep_ready && s.capability(&id) == Some("yes")
            };
            if !allowed {
                self.syncing.set(true);
                let current = value_text(&self.settings.borrow()[key][field]);
                control.set_active_id(&current);
                self.syncing.set(false);
                return;
            }
        }
        self.settings.borrow_mut()[key][field] = value;
        self.message.replace(String::new());
        self.refresh();
    }

    fn extra_changed(&self, widget: &gtk::ComboBoxText, path: &[&str]) {
        let Some(id) = widget.active_id().filter(|_| !self.syncing.get()) else { return };
        let value: Value = if path == ["critical_level"] { id.parse::<i64>().unwrap_or(0).into() } else { id.to_string().into() };
        {
            let mut settings = self.settings.borrow_mut();
            let target = path[..path.len() - 1].iter().fold(&mut *settings, |v, k| &mut v[*k]);
            target[path[path.len() - 1]] = value;
        }
        self.message.replace(String::new());
        self.refresh();
    }

    fn dim_changed(&self, active: bool) {
        if !self.syncing.get() {
            self.settings.borrow_mut()["dim_before_lock"] = active.into();
            self.message.replace(String::new());
            self.refresh();
        }
    }

    fn lock_changed(&self) {
        if self.syncing.get() {
            return;
        }
        if let Some(seconds) = seconds_of(&self.lock_choice) {
            self.settings.borrow_mut()["lock_after"] = seconds.into();
            self.message.replace(String::new());
            self.refresh();
        }
    }

    /// Show options added after the original idle policy (remote reloads).
    pub fn sync_extra_controls(&self) {
        let syncing = self.syncing.replace(true);
        let settings = self.settings.borrow().clone();
        self.dim_switch.set_active(settings["dim_before_lock"].as_bool().unwrap_or(true));
        for (path, widget) in &self.extra_controls {
            widget.set_active_id(Some(&value_text(path_value(&settings, path))));
        }
        self.syncing.set(syncing);
    }

    fn action_hint(&self, actions: &[String]) -> String {
        let s = self.backend.state();
        if !s.sleep_ready {
            return String::new();
        }
        let mut notes = Vec::new();
        let mut seen: Vec<&String> = Vec::new();
        for action in actions {
            if seen.contains(&action) {
                continue;
            }
            seen.push(action);
            if (action == "suspend" || action == "hibernate") && s.capability(action) != Some("yes") {
                notes.push(if action == "hibernate" { tr!("Hibernate is not available on this system.") }
                           else { tr!("Sleep is not available on this system.") });
            }
        }
        notes.join(" ")
    }

    pub fn validation_error(&self) -> String {
        let s = self.backend.state();
        let settings = self.settings.borrow();
        let saved = self.saved.borrow();
        let keys: &[&str] = if s.has_battery { &["ac", "battery"] } else { &["ac"] };
        for key in keys {
            let policy = &settings[*key];
            let changed = ["sleep_after", "sleep_action"].iter().any(|f| policy[*f] != saved[*key][*f]);
            let action = policy["sleep_action"].as_str().unwrap_or("");
            if policy["sleep_after"].as_i64().unwrap_or(0) != 0 && changed && (!s.sleep_ready || s.capability(action) != Some("yes")) {
                return tr!("Choose an available sleep mode, or set Sleep after to Never.");
            }
        }
        String::new()
    }

    pub fn refresh(&self) {
        if self.destroyed.get() {
            return;
        }
        self.source_label.set_text(&tr!("Profiles, display-off and sleep behavior"));
        self.selector.refresh();
        self.syncing.set(true);
        let settings = self.settings.borrow().clone();
        let (has_battery, sleep_ready, capabilities) = {
            let s = self.backend.state();
            (s.has_battery, s.sleep_ready, s.capabilities.clone())
        };
        for (key, controls) in &self.controls {
            self.policy_cards[key].set_visible(*key == "ac" || has_battery);
            if let Control::Sleep(choice) = &controls["sleep_action"] {
                choice.refresh(&*self.backend);
            }
            let action = settings[*key]["sleep_action"].as_str().unwrap_or("");
            let capability = if sleep_ready { capabilities.get(action).cloned().unwrap_or_else(|| "na".into()) } else { "pending".into() };
            let mut note = match capability.as_str() {
                "challenge" => tr!("This mode needs authorization and cannot run automatically."),
                "pending" => tr!("Sleep support is unavailable while connecting to the system service."),
                "yes" => String::new(),
                _ => tr!("The saved sleep mode is unavailable on this system."),
            };
            if action == "suspend-then-hibernate" {
                let lead = if note.is_empty() { "" } else { " " };
                note += &tr!("{value1}The system controls how long to sleep before hibernating.", value1 = lead);
            }
            self.policy_hints[key].set_text(&note);
            self.policy_hints[key].set_visible(!note.is_empty());
        }
        for widget in &self.lid_rows {
            widget.set_visible(has_battery);
        }
        self.battery_card.set_visible(has_battery);
        let mut actions: Vec<String> = Vec::new();
        if has_battery {
            for source in ["ac", "battery"] {
                actions.push(value_text(&settings["lid_close"][source]));
            }
        }
        actions.push(value_text(&settings["power_button"]));
        let hint = self.action_hint(&actions);
        self.buttons_hint.set_text(&hint);
        self.buttons_hint.set_visible(!hint.is_empty());
        let critical = value_text(&settings["critical_action"]);
        let mut hint = if critical == "auto" { String::new() } else { self.action_hint(std::slice::from_ref(&critical)) };
        if critical == "nothing" {
            hint = tr!("The system may still shut down by itself when the battery is empty.");
        }
        self.battery_hint.set_text(&hint);
        self.battery_hint.set_visible(!hint.is_empty());
        self.syncing.set(false);
        let invalid = self.validation_error();
        self.save_button.set_sensitive(settings != *self.saved.borrow() && invalid.is_empty());
        let shared = shared_status(&(self.app_error)(), &*self.backend);
        let message = [self.message.borrow().clone(), invalid, shared].into_iter().filter(|v| !v.is_empty()).collect::<Vec<_>>().join("\n");
        self.status_label.set_text(&message);
        self.status_label.set_visible(!message.is_empty());
    }

    pub fn save(&self) {
        let settings = self.settings.borrow().clone();
        if settings == *self.saved.borrow() || !self.validation_error().is_empty() {
            return;
        }
        let saved = (self.on_save)(settings.clone());
        if saved {
            self.saved.replace(settings);
            self.message.replace(tr!("Settings saved."));
        } else {
            self.message.replace(tr!("Could not save settings."));
        }
        self.refresh();
    }
}
