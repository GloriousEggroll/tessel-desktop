//! `BrightnessControls`: immediate monitor controls shared with the brightness
//! popup; deliberately independent of the way-displays YAML
//! (tessel-displays:761-997).

use crate::compat::brightness_client::BrightnessModel;
use glib::ControlFlow;
use gtk::prelude::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;
use tessel_core::pyjson::{py_str, truthy};
use tessel_core::tr;

pub const DELAYS: [i64; 8] = [30, 60, 120, 300, 600, 900, 1800, 3600];
pub const FEATURES: [&str; 2] = ["brightness", "contrast"];

/// Why an HDR-locked control is disabled.
pub fn hdr_locked_text(feature: &str) -> String {
    if feature == "contrast" {
        tr!("This display doesn't allow changing contrast while HDR is on. Turn off HDR for this display to adjust it.")
    } else {
        tr!("This display doesn't allow changing brightness while HDR is on. Turn off HDR for this display to adjust it.")
    }
}


/// `_duration(value)` (untranslated in Python).
pub fn duration(value: i64) -> String {
    if value % 60 == 0 {
        let minutes = value / 60;
        format!("{minutes} minute{}", if minutes != 1 { "s" } else { "" })
    } else {
        format!("{value} seconds")
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(b)) => Some(*b as i64 as f64),
        _ => None,
    }
}

/// Python `round(x)` (half to even).
pub fn py_round(value: f64) -> i64 {
    let rounded = value.round();
    if (value - value.trunc()).abs() == 0.5 && rounded as i64 % 2 != 0 {
        (rounded - value.signum()) as i64
    } else {
        rounded as i64
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// `(source, value, expected_id)`; `source == None` is Python's `0`.
type Pending = (Option<glib::SourceId>, i64, Value);

pub struct BrightnessControls {
    pub root: gtk::Box,
    pub connector: String,
    pub client: Option<Rc<dyn BrightnessModel>>,
    pub client_error: RefCell<String>,
    syncing: Cell<bool>,
    closed: Cell<bool>,
    pending: RefCell<BTreeMap<&'static str, Pending>>,
    pending_policy: RefCell<Option<serde_json::Map<String, Value>>>,
    local_error: RefCell<String>,
    delay_values: RefCell<Vec<i64>>,
    pub scales: BTreeMap<&'static str, gtk::Scale>,
    pub value_labels: BTreeMap<&'static str, gtk::Label>,
    pub feature_rows: BTreeMap<&'static str, gtk::Box>,
    pub auto_switch: gtk::Switch,
    pub dim_row: gtk::Box,
    pub delay_choice: gtk::DropDown,
    pub dim_level: gtk::SpinButton,
    pub status_label: gtk::Label,
    me: RefCell<std::rc::Weak<Self>>,
}

impl BrightnessControls {
    pub fn new(connector: &str, client: Option<Rc<dyn BrightnessModel>>, client_error: &str) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("brightness-controls");
        root.set_margin_top(8);
        root.set_margin_bottom(12);
        let title = gtk::Label::builder().label(tr!("Brightness & contrast")).xalign(0.0).build();
        title.add_css_class("heading");
        root.append(&title);
        let hint = gtk::Label::builder().label(tr!("Changes apply immediately and are shared with the brightness popup.")).xalign(0.0).build();
        hint.set_wrap(true);
        hint.add_css_class("dim-label");
        root.append(&hint);

        let mut scales = BTreeMap::new();
        let mut value_labels = BTreeMap::new();
        let mut feature_rows = BTreeMap::new();
        for (feature, title) in [("brightness", tr!("Brightness")), ("contrast", tr!("Contrast"))] {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let name = gtk::Label::builder().label(title.as_str()).xalign(0.0).build();
            name.set_width_chars(11);
            row.append(&name);
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
            scale.set_draw_value(false);
            scale.set_hexpand(true);
            scale.set_size_request(180, -1);
            scale.set_tooltip_text(Some(&tr!("{value1} for {value2}", value1 = title.as_str(), value2 = connector)));
            row.append(&scale);
            let value = gtk::Label::builder().label(tr!("Unavailable")).xalign(1.0).build();
            value.set_width_chars(12);
            row.append(&value);
            scales.insert(feature, scale);
            value_labels.insert(feature, value);
            feature_rows.insert(feature, row.clone());
            root.append(&row);
        }

        let auto_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        auto_row.append(&gtk::Label::builder().label(tr!("Automatically dim when idle")).xalign(0.0).hexpand(true).build());
        let auto_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        auto_row.append(&auto_switch);
        root.append(&auto_row);

        let dim_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        dim_row.append(&gtk::Label::builder().label(tr!("Dim after")).xalign(0.0).build());
        let labels: Vec<String> = DELAYS.iter().map(|v| duration(*v)).collect();
        let delay_choice = gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        dim_row.append(&delay_choice);
        dim_row.append(&gtk::Label::builder().label(tr!("to")).xalign(0.0).build());
        let dim_level = gtk::SpinButton::with_range(1.0, 90.0, 1.0);
        dim_row.append(&dim_level);
        dim_row.append(&gtk::Label::new(Some(&tr!("% brightness"))));
        root.append(&dim_row);
        let shared = gtk::Label::builder().label(tr!("Automatic dimming applies to all supported displays. Activity restores brightness.")).xalign(0.0).build();
        shared.set_wrap(true);
        shared.add_css_class("dim-label");
        root.append(&shared);
        let status_label = gtk::Label::builder().xalign(0.0).build();
        status_label.set_wrap(true);
        status_label.add_css_class("dim-label");
        root.append(&status_label);

        let this = Rc::new(Self {
            root, connector: connector.to_owned(), client, client_error: RefCell::new(client_error.to_owned()),
            syncing: Cell::new(false), closed: Cell::new(false), pending: RefCell::new(BTreeMap::new()),
            pending_policy: RefCell::new(None), local_error: RefCell::new(String::new()),
            delay_values: RefCell::new(DELAYS.to_vec()), scales, value_labels, feature_rows, auto_switch, dim_row,
            delay_choice, dim_level, status_label, me: RefCell::new(std::rc::Weak::new()),
        });
        this.me.replace(Rc::downgrade(&this));
        for feature in FEATURES {
            let weak = Rc::downgrade(&this);
            this.scales[feature].connect_value_changed(move |scale| {
                if let Some(this) = weak.upgrade() {
                    this.value_changed(scale, feature);
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.auto_switch.connect_active_notify(move |switch| {
            if let Some(this) = weak.upgrade() {
                this.auto_changed(switch);
            }
        });
        let weak = Rc::downgrade(&this);
        this.delay_choice.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.settings_changed();
            }
        });
        let weak = Rc::downgrade(&this);
        this.dim_level.connect_value_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.settings_changed();
            }
        });
        this.refresh();
        this
    }

    fn state(&self) -> Value {
        self.client.as_ref().map(|c| c.state()).filter(|s| truthy(s)).unwrap_or_else(|| json!({}))
    }

    fn display(&self) -> Value {
        self.state().get("displays").and_then(Value::as_array)
            .and_then(|d| d.iter().find(|r| r.get("connector").and_then(Value::as_str) == Some(self.connector.as_str())).cloned())
            .unwrap_or_else(|| json!({}))
    }

    fn ready(&self) -> bool {
        !self.closed.get() && self.client.as_ref().is_some_and(|c| c.ready())
    }

    fn feature_data(display: &Value, feature: &str) -> Value {
        display.get(feature).filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}))
    }

    fn feature_ready(&self, feature: &str) -> bool {
        let display = self.display();
        let data = Self::feature_data(&display, feature);
        self.ready() && display.get("enabled").is_none_or(truthy)
            && display.get("id").is_some_and(truthy)
            && data.get("supported").is_some_and(truthy)
            && number(data.get("value")).is_some_and(f64::is_finite)
            && !data.get("busy").is_some_and(truthy)
            && !data.get("hdr_locked").is_some_and(truthy)
    }

    fn cancel_value(&self, feature: &str) {
        let pending = self.pending.borrow_mut().remove(feature);
        if let Some((Some(source), _, _)) = pending {
            tessel_core::glib_compat::source_remove(source);
        }
    }

    fn value_changed(&self, scale: &gtk::Scale, feature: &'static str) {
        if self.syncing.get() || self.closed.get() {
            return;
        }
        if !self.feature_ready(feature) {
            self.refresh();
            return;
        }
        self.cancel_value(feature);
        let value = py_round(scale.value());
        let expected_id = self.display().get("id").cloned().unwrap_or(Value::Null);
        self.value_labels[feature].set_text(&format!("{value}%"));
        let source = self.schedule(180, feature, value, expected_id.clone());
        self.pending.borrow_mut().insert(feature, (Some(source), value, expected_id));
    }

    fn schedule(&self, delay: u64, feature: &'static str, value: i64, expected_id: Value) -> glib::SourceId {
        let weak = self.me.borrow().clone();
        glib::timeout_add_local(Duration::from_millis(delay), move || {
            if let Some(this) = weak.upgrade() {
                this.send_value(feature, value, expected_id.clone());
            }
            ControlFlow::Break
        })
    }

    fn send_value(&self, feature: &'static str, value: i64, expected_id: Value) {
        self.pending.borrow_mut().remove(feature);
        let display = self.display();
        let data = Self::feature_data(&display, feature);
        let same = display.get("id").cloned().unwrap_or(Value::Null) == expected_id;
        if self.ready() && same && data.get("supported").is_some_and(truthy) {
            if data.get("busy").is_some_and(truthy) {
                self.pending.borrow_mut().insert(feature, (None, value, expected_id));
            } else if self.feature_ready(feature) {
                let client = self.client.clone().unwrap();
                let connector = self.connector.clone();
                self.request(move || client.set_value(&connector, feature, value as f64, Some(&expected_id)));
            } else {
                self.refresh();
            }
        } else {
            self.refresh();
        }
    }

    fn request(&self, action: impl FnOnce() -> Result<bool, String>) {
        self.local_error.replace(String::new());
        match action() {
            Ok(false) => {
                let error = self.client.as_ref().map(|c| c.error()).filter(|e| !e.is_empty())
                    .unwrap_or_else(|| tr!("The brightness setting could not be changed."));
                self.local_error.replace(error);
            }
            Ok(true) => {}
            Err(error) => {
                self.local_error.replace(error);
            }
        }
        self.refresh();
    }

    fn auto_changed(&self, switch: &gtk::Switch) {
        if self.syncing.get() || self.closed.get() {
            return;
        }
        if self.ready() && !self.state().get("busy").is_some_and(truthy) {
            let enabled = switch.is_active();
            let mut policy = serde_json::Map::new();
            policy.insert("auto_dim".into(), enabled.into());
            self.pending_policy.replace(Some(policy));
            let client = self.client.clone().unwrap();
            self.request(move || client.set_auto_dim(enabled));
        } else {
            self.refresh();
        }
    }

    fn settings_changed(&self) {
        if self.syncing.get() || self.closed.get() {
            return;
        }
        if !self.ready() || self.state().get("busy").is_some_and(truthy) {
            self.refresh();
            return;
        }
        let index = self.delay_choice.selected() as usize;
        let Some(delay) = self.delay_values.borrow().get(index).copied() else { return };
        let mut settings = self.state().get("settings").cloned().unwrap_or_else(|| json!({}));
        if let Some(map) = settings.as_object_mut() {
            map.insert("dim_after".into(), delay.into());
            map.insert("dim_percent".into(), self.dim_level.value_as_int().into());
        }
        let mut policy = serde_json::Map::new();
        for key in ["dim_after", "dim_percent"] {
            policy.insert(key.into(), settings[key].clone());
        }
        self.pending_policy.replace(Some(policy));
        let client = self.client.clone().unwrap();
        self.request(move || client.set_settings(&settings));
    }

    pub fn refresh(&self) {
        if self.closed.get() {
            return;
        }
        self.syncing.set(true);
        self.refresh_inner();
        self.syncing.set(false);
    }

    fn refresh_inner(&self) {
        let state = self.state();
        let display = self.display();
        let ready = self.ready();
        let has_display = display.as_object().is_some_and(|m| !m.is_empty());
        let mut errors: Vec<Value> = vec![
            Value::String(self.client_error.borrow().clone()),
            Value::String(self.client.as_ref().map(|c| c.error()).unwrap_or_default()),
            state.get("error").cloned().unwrap_or(json!("")),
            display.get("error").cloned().unwrap_or(json!("")),
            Value::String(self.local_error.borrow().clone()),
        ];
        for feature in FEATURES {
            let data = Self::feature_data(&display, feature);
            let supported = data.get("supported").is_some_and(truthy);
            let checking = ready && has_display && data.get("supported").is_none_or(Value::is_null);
            let value = number(data.get("value"));
            let readable = supported && value.is_some_and(f64::is_finite);
            let enabled = self.feature_ready(feature);
            self.scales[feature].set_sensitive(enabled);
            let pending = self.pending.borrow().get(feature).map(|p| (p.0.is_some(), p.1, p.2.clone()));
            let display_id = display.get("id").cloned().unwrap_or(Value::Null);
            if !ready || !supported || !display.get("enabled").is_none_or(truthy)
                || pending.as_ref().is_some_and(|p| p.2 != display_id) {
                self.cancel_value(feature);
            } else if let Some((false, value, id)) = pending.clone() {
                if enabled {
                    let source = self.schedule(1, feature, value, id.clone());
                    self.pending.borrow_mut().insert(feature, (Some(source), value, id));
                }
            }
            if !self.pending.borrow().contains_key(feature) {
                if readable {
                    self.scales[feature].set_value(value.unwrap().clamp(0.0, 100.0));
                }
                let text = if readable {
                    format!("{}%", py_round(value.unwrap()))
                } else if checking {
                    tr!("Checking…")
                } else {
                    tr!("Unavailable")
                };
                self.value_labels[feature].set_text(&text);
            }
            let locked = data.get("hdr_locked").is_some_and(truthy);
            let tooltip = match data.get("error").filter(|e| truthy(e)) {
                _ if locked => hdr_locked_text(feature),
                Some(error) => py_str(error),
                None if supported => tr!("{value1} for {value2}", value1 = capitalize(feature), value2 = self.connector.as_str()),
                None => tr!("This monitor does not expose {value1} control.", value1 = feature),
            };
            self.scales[feature].set_tooltip_text(Some(&tooltip));
            errors.push(if locked { Value::String(hdr_locked_text(feature)) } else { data.get("error").cloned().unwrap_or(json!("")) });
        }
        let mut settings = state.get("settings").filter(|v| truthy(v)).and_then(Value::as_object).cloned().unwrap_or_default();
        let busy = state.get("busy").is_some_and(truthy);
        let policy = self.pending_policy.borrow().clone();
        match policy {
            Some(policy) if ready && busy => settings.extend(policy),
            _ => {
                self.pending_policy.replace(None);
            }
        }
        let auto = settings.get("auto_dim").is_some_and(truthy);
        self.auto_switch.set_active(auto);
        let policy_ready = ready && !busy;
        self.auto_switch.set_sensitive(policy_ready);
        let delay = settings.get("dim_after").cloned().unwrap_or(json!(120));
        if let Some(delay) = delay.as_i64().filter(|d| *d > 0) {
            if !self.delay_values.borrow().contains(&delay) {
                let mut values = self.delay_values.borrow().clone();
                values.push(delay);
                values.sort();
                let labels: Vec<String> = values.iter().map(|v| duration(*v)).collect();
                self.delay_values.replace(values);
                self.delay_choice.set_model(Some(&gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>())));
            }
        }
        let delay_number = number(Some(&delay));
        let index = self.delay_values.borrow().iter().position(|v| Some(*v as f64) == delay_number).unwrap_or(0);
        self.delay_choice.set_selected(index as u32);
        self.dim_level.set_value(number(settings.get("dim_percent")).unwrap_or(30.0));
        self.dim_row.set_sensitive(policy_ready);
        if state.get("dimmed").is_some_and(truthy) {
            errors.push(Value::String(tr!("Displays are temporarily dimmed. Activity restores brightness.")));
        }
        if !ready && !errors.iter().any(truthy) {
            errors.push(Value::String(tr!("Brightness service is unavailable.")));
        } else if ready && !has_display {
            errors.push(Value::String("No brightness controls were found for this display.".into()));
        }
        let mut lines: Vec<String> = Vec::new();
        for error in errors.iter().filter(|e| truthy(e)) {
            let text = py_str(error);
            if !lines.contains(&text) {
                lines.push(text);
            }
        }
        let text = lines.join("\n");
        self.status_label.set_text(&text);
        self.status_label.set_visible(!text.is_empty());
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub fn delay_values(&self) -> Vec<i64> {
        self.delay_values.borrow().clone()
    }

    pub fn close(&self) {
        self.closed.set(true);
        let features: Vec<&str> = self.pending.borrow().keys().copied().collect();
        for feature in features {
            self.cancel_value(feature);
        }
    }
}
