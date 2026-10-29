//! Live battery cards shared by the battery popup and settings window
//! (`power_battery_ui.py`). Rows are reused so background battery updates
//! never reset an open control; charge settings change only on explicit input.

use super::style::{icon, label, margins, style};
use crate::power::values::{number, py_eq, text, truthy};
use crate::power::BatteryModel;
use crate::status::whole;
use gtk::glib;
use gtk::prelude::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::gtk;
use suite_gtk::utils::{accessible, append};

pub type Changed = Rc<dyn Fn()>;
pub type Failed = Rc<dyn Fn(&str)>;
pub type Bluetooth = Option<Rc<dyn Fn()>>;

/// Python `round()` (half to even).
fn round_half_even(value: f64) -> i64 {
    let floor = value.floor();
    let diff = value - floor;
    let rounded = if diff > 0.5 || (diff == 0.5 && floor % 2.0 != 0.0) { floor + 1.0 } else { floor };
    rounded as i64
}

pub fn time_label(seconds: Option<&Value>) -> String {
    let Some(seconds) = number(seconds).filter(|s| *s > 0.0) else { return String::new() };
    let minutes = round_half_even(seconds / 60.0).max(1);
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if hours != 0 && minutes != 0 {
        return tr!("{hours} hr {minutes} min", hours = hours, minutes = minutes);
    }
    if hours != 0 { tr!("{hours} hr", hours = hours) } else { tr!("{minutes} min", minutes = minutes) }
}

fn precise(device: &Value) -> Option<f64> {
    number(device.get("percentage")).filter(|v| (0.0..=100.0).contains(v))
}

pub fn level_label(device: &Value) -> String {
    match precise(device) {
        Some(value) => format!("{}%", whole(value)),
        None => {
            let level = text(device.get("level"));
            tr!(if level.is_empty() { "Unknown".to_owned() } else { level })
        }
    }
}

pub fn status_label(device: &Value) -> String {
    let state_text = text(device.get("state"));
    let mut status = tr!(if state_text.is_empty() { "Status unavailable".to_owned() } else { state_text });
    let state = device.get("state_code");
    let charging = py_eq(state, Some(&json!(1)));
    let remaining = time_label(device.get(if charging { "time_to_full" } else { "time_to_empty" }));
    if !remaining.is_empty() && (charging || py_eq(state, Some(&json!(2)))) {
        status = if charging {
            tr!("{status} · {time} until full", status = status.as_str(), time = remaining.as_str())
        } else {
            tr!("{status} · {time} remaining", status = status.as_str(), time = remaining.as_str())
        };
    }
    let warning = text(device.get("warning"));
    if !warning.is_empty() {
        status += &format!(" · {}", tr!(warning));
    }
    status
}

pub struct BatteryRow {
    pub root: gtk::Box,
    pub image: gtk::Image,
    pub name_label: gtk::Label,
    pub state_label: gtk::Label,
    pub level_label: gtk::Label,
    pub level_bar: gtk::LevelBar,
    pub bluetooth_button: gtk::Button,
    pub details_label: gtk::Label,
    pub protection_box: gtk::Box,
    pub protection_hint: gtk::Label,
    pub protection_switch: gtk::Switch,
    pub error_label: gtk::Label,
    device: RefCell<Value>,
    syncing: Cell<bool>,
    backend: Rc<dyn BatteryModel>,
    changed: Changed,
    failed: Failed,
    bluetooth: Bluetooth,
    detailed: bool,
}

impl BatteryRow {
    pub fn new(backend: Rc<dyn BatteryModel>, device: &Value, detailed: bool, changed: Changed, failed: Failed, bluetooth: Bluetooth) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 9);
        style(&root, &["battery-row"]);
        margins(&root, 11);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let image = icon("battery-good-symbolic", 24);
        append(&heading, &image, false, false);
        let name_label = label("", Some("row-title"), false);
        let state_label = label("", Some("status"), false);
        append(&heading, &name_label, true, false);
        let level_label = label("", Some("battery-percentage"), false);
        level_label.set_xalign(1.0);
        let level_bar = gtk::LevelBar::new();
        style(&level_bar, &["battery-level"]);
        level_bar.set_size_request(-1, 6);
        level_bar.set_hexpand(true);
        level_bar.set_halign(gtk::Align::Fill);
        let bluetooth_button = gtk::Button::new();
        style(&bluetooth_button, &["icon-button"]);
        bluetooth_button.set_child(Some(&icon("bluetooth-symbolic", 17)));
        bluetooth_button.set_tooltip_text(Some(&tr!("Bluetooth device settings")));
        append(&heading, &bluetooth_button, false, false);
        append(&heading, &level_label, false, false);
        append(&root, &heading, false, false);
        append(&root, &level_bar, false, false);
        append(&root, &state_label, false, false);
        let details_label = label("", Some("status"), true);
        append(&root, &details_label, false, false);
        let protection_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 3);
        append(&titles, &label(&tr!("Battery protection"), Some("row-title"), false), false, false);
        let protection_hint = label("", Some("status"), true);
        append(&titles, &protection_hint, false, false);
        append(&protection_box, &titles, true, false);
        let protection_switch = gtk::Switch::new();
        protection_switch.set_valign(gtk::Align::Center);
        append(&protection_box, &protection_switch, false, false);
        append(&root, &protection_box, false, false);
        let error_label = label("", Some("error"), true);
        append(&root, &error_label, false, false);
        let row = Rc::new(Self {
            root, image, name_label, state_label, level_label, level_bar, bluetooth_button, details_label, protection_box,
            protection_hint, protection_switch, error_label, device: RefCell::new(device.clone()), syncing: Cell::new(false),
            backend, changed, failed, bluetooth, detailed,
        });
        let weak = Rc::downgrade(&row);
        row.bluetooth_button.connect_clicked(move |_| {
            if let Some(row) = weak.upgrade() {
                row.open_bluetooth();
            }
        });
        let weak = Rc::downgrade(&row);
        row.protection_switch.connect_state_set(move |_, enabled| match weak.upgrade() {
            Some(row) => row.protection(enabled),
            None => glib::Propagation::Stop,
        });
        row.refresh(device);
        row
    }

    fn open_bluetooth(&self) {
        let bluetooth = truthy(self.device.borrow().get("bluetooth"));
        if let (true, Some(open)) = (bluetooth, self.bluetooth.clone()) {
            open();
        }
    }

    fn protection(&self, enabled: bool) -> glib::Propagation {
        if self.syncing.get() {
            return glib::Propagation::Proceed;
        }
        let device = self.device.borrow().clone();
        if !truthy(device.get("charge_threshold_supported")) || truthy(device.get("charge_limit_adjustable"))
            || truthy(device.get("busy")) || !self.backend.power_ready() {
            return glib::Propagation::Stop;
        }
        (self.failed)("");
        let accepted = self.backend.set_charge_thresholds(&text(device.get("path")), enabled);
        if !accepted && self.backend.error().is_empty() {
            (self.failed)(&tr!("Battery protection could not be changed."));
        }
        (self.changed)();
        // The actual device state is authoritative, including authorization failures.
        glib::Propagation::Stop
    }

    pub fn device(&self) -> Value {
        self.device.borrow().clone()
    }

    pub fn refresh(&self, device: &Value) {
        self.device.replace(device.clone());
        let name = match text(device.get("name")) {
            name if name.is_empty() => tr!("Battery"),
            name => name,
        };
        self.name_label.set_text(&name);
        self.name_label.set_tooltip_text(Some(&name));
        let status = status_label(device);
        self.state_label.set_text(&status);
        self.state_label.set_tooltip_text(Some(&status));
        self.level_label.set_text(&level_label(device));
        let icon_name = match text(device.get("icon_name")) {
            name if name.is_empty() => "battery-good-symbolic".to_owned(),
            name => name,
        };
        self.image.set_icon_name(Some(&icon_name));
        self.image.set_pixel_size(24);
        let value = precise(device);
        self.level_bar.set_visible(value.is_some());
        if let Some(value) = value {
            self.level_bar.set_value(value / 100.0);
            accessible(&self.level_bar, &tr!("{value1} battery: {value2:.0f}%", value1 = name.as_str(), value2 = value));
        }
        self.bluetooth_button.set_visible(truthy(device.get("bluetooth")) && self.bluetooth.is_some());
        accessible(&self.bluetooth_button, &tr!("Bluetooth settings for {value1}", value1 = name.as_str()));
        let mut details = Vec::new();
        if self.detailed {
            let kind = text(device.get("kind"));
            if !kind.is_empty() {
                details.push(tr!(kind));
            }
            if let Some(health) = number(device.get("health")).filter(|h| *h > 0.0 && *h <= 100.0) {
                details.push(tr!("Health: {value1:.0f}%", value1 = health));
            }
            if let Some(cycles) = number(device.get("cycles")).filter(|c| *c >= 0.0) {
                details.push(tr!("Charge cycles: {value1:.0f}", value1 = cycles));
            }
        }
        if truthy(device.get("charge_limit_adjustable")) {
            if let Some(maximum) = number(device.get("charge_end_threshold")).filter(|m| *m >= 1.0 && *m < 100.0) {
                details.push(tr!("Charge limit: {value1:.0f}%", value1 = maximum));
            }
        }
        self.details_label.set_text(&details.join(" · "));
        self.details_label.set_visible(!details.is_empty());
        let supported = device.get("charge_threshold_supported") == Some(&Value::Bool(true)) && !truthy(device.get("charge_limit_adjustable"));
        self.protection_box.set_visible(supported);
        let enabled = device.get("charge_threshold_enabled");
        self.syncing.set(true);
        self.protection_switch.set_active(enabled == Some(&Value::Bool(true)));
        self.protection_switch.set_state(enabled == Some(&Value::Bool(true)));
        self.syncing.set(false);
        self.protection_switch.set_sensitive(supported && matches!(enabled, Some(Value::Bool(_)))
                                             && !truthy(device.get("busy")) && self.backend.power_ready());
        accessible(&self.protection_switch, &tr!("Battery protection for {value1}", value1 = name.as_str()));
        let known = |value: Option<f64>| value.filter(|v| *v > 0.0 && *v <= 100.0);
        let (start, stop) = (known(number(device.get("charge_start_threshold"))), known(number(device.get("charge_end_threshold"))));
        let mut threshold = match (start, stop) {
            (Some(start), Some(stop)) => tr!("Resume below {value1:.0f}%; stop at {value2:.0f}%.", value1 = start, value2 = stop),
            (None, Some(stop)) => tr!("Limit charging to {value1:.0f}%.", value1 = stop),
            (Some(start), None) => tr!("Resume charging below {value1:.0f}%.", value1 = start),
            (None, None) => tr!("Use this device’s charge limit."),
        };
        if truthy(device.get("busy")) {
            threshold = tr!("Updating battery protection…");
        }
        self.protection_hint.set_text(&threshold);
        let error = text(device.get("error"));
        self.error_label.set_text(&error);
        self.error_label.set_visible(!error.is_empty());
    }
}

/// Edit one maximum for all supported laptop batteries, on explicit Apply.
pub struct ChargeLimitControls {
    pub root: gtk::Box,
    pub limit: gtk::SpinButton,
    pub apply_button: gtk::Button,
    pub hint: gtk::Label,
    syncing: Cell<bool>,
    dirty: Cell<bool>,
    submitted: Cell<Option<i32>>,
    backend: Rc<dyn BatteryModel>,
    changed: Changed,
    failed: Failed,
}

impl ChargeLimitControls {
    pub fn new(backend: Rc<dyn BatteryModel>, changed: Changed, failed: Failed) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 9);
        append(&root, &label(&tr!("Maximum charge"), Some("row-title"), false), false, false);
        append(&root, &label(&tr!("Applies to all supported laptop batteries."), Some("status"), true), false, false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let limit = gtk::SpinButton::with_range(1.0, 100.0, 1.0);
        limit.set_value(80.0);
        accessible(&limit, &tr!("Maximum battery charge percent"));
        append(&row, &limit, false, false);
        append(&row, &label("%", None, false), false, false);
        let apply_button = gtk::Button::with_label(&tr!("Apply"));
        style(&apply_button, &["primary"]);
        row.append(&gtk::Box::builder().hexpand(true).build());
        append(&row, &apply_button, false, false);
        append(&root, &row, false, false);
        let hint = label("", Some("status"), true);
        append(&root, &hint, false, false);
        let controls = Rc::new(Self { root, limit, apply_button, hint, syncing: Cell::new(false), dirty: Cell::new(false),
                                      submitted: Cell::new(None), backend, changed, failed });
        let weak = Rc::downgrade(&controls);
        controls.limit.connect_value_changed(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.edited();
            }
        });
        let weak = Rc::downgrade(&controls);
        controls.apply_button.connect_clicked(move |_| {
            if let Some(controls) = weak.upgrade() {
                controls.apply();
            }
        });
        controls.refresh();
        controls
    }

    fn edited(&self) {
        if self.syncing.get() {
            return;
        }
        let current = match self.submitted.get() {
            Some(value) => json!(value),
            None => self.backend.charge_limit().get("maximum").cloned().unwrap_or(Value::Null),
        };
        self.dirty.set(!py_eq(Some(&json!(self.limit.value_as_int())), Some(&current)));
        self.refresh();
    }

    fn apply(&self) {
        let state = self.backend.charge_limit();
        if !self.dirty.get() || !truthy(state.get("supported")) || truthy(state.get("busy"))
            || self.submitted.get().is_some() || self.backend.busy() {
            return;
        }
        (self.failed)("");
        let value = self.limit.value_as_int();
        self.submitted.set(Some(value));
        if !self.backend.set_charge_limit(&json!(value)) {
            self.submitted.set(None);
            if self.backend.error().is_empty() {
                (self.failed)(&tr!("The maximum charge could not be changed."));
            }
        }
        (self.changed)();
    }

    pub fn refresh(&self) {
        let state = self.backend.charge_limit();
        let supported = truthy(state.get("supported"));
        let maximum = state.get("maximum").cloned().unwrap_or(Value::Null);
        let known = number(Some(&maximum)).filter(|m| (1.0..=100.0).contains(m));
        let busy = truthy(state.get("busy")) || self.backend.busy();
        if !supported {
            self.dirty.set(false);
            self.submitted.set(None);
        }
        if let (Some(submitted), false) = (self.submitted.get(), busy) {
            if py_eq(Some(&maximum), Some(&json!(submitted))) {
                self.submitted.set(None);
                self.dirty.set(!py_eq(Some(&json!(self.limit.value_as_int())), Some(&maximum)));
            } else if truthy(state.get("error")) || !self.backend.error().is_empty() {
                self.submitted.set(None);
            }
        }
        if !self.dirty.get() && self.submitted.get().is_none() {
            self.syncing.set(true);
            self.limit.set_value(known.unwrap_or(80.0));
            self.syncing.set(false);
        }
        self.limit.set_sensitive(supported);
        self.apply_button.set_sensitive(supported && !busy && self.dirty.get() && self.submitted.get().is_none());
        let mut message = text(state.get("error"));
        if message.is_empty() {
            message = text(state.get("reason"));
        }
        if message.is_empty() {
            message = if busy {
                tr!("Updating charge limit…")
            } else if let Some(maximum) = known {
                tr!("Current maximum: {value1:.0f}%. Choose 100% to allow a full charge.", value1 = maximum)
            } else {
                tr!("Current limits differ between batteries. Choose a maximum and apply it.")
            };
        }
        self.hint.set_text(&message);
        self.root.set_visible(supported || (truthy(state.get("battery_count")) && truthy(state.get("reason"))));
    }
}

pub struct BatterySection {
    pub root: gtk::Box,
    pub empty_label: gtk::Label,
    pub list_box: gtk::Box,
    pub scroll: Option<gtk::ScrolledWindow>,
    pub charge_controls: Option<Rc<ChargeLimitControls>>,
    pub rows: RefCell<HashMap<String, Rc<BatteryRow>>>,
    backend: Rc<dyn BatteryModel>,
    changed: Changed,
    failed: Failed,
    bluetooth: Bluetooth,
    detailed: bool,
}

impl BatterySection {
    pub fn new(backend: Rc<dyn BatteryModel>, changed: Changed, failed: Failed, bluetooth: Bluetooth, detailed: bool) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 9);
        append(&root, &label(&tr!("Batteries"), Some("section-title"), false), false, false);
        let empty_label = label("", Some("status"), true);
        append(&root, &empty_label, false, false);
        let list_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let scroll = if detailed {
            append(&root, &list_box, false, false);
            None
        } else {
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroll.set_propagate_natural_height(true);
            scroll.set_max_content_height(224);
            scroll.set_child(Some(&list_box));
            append(&root, &scroll, false, false);
            Some(scroll)
        };
        let charge_controls = detailed.then(|| {
            let controls = ChargeLimitControls::new(backend.clone(), changed.clone(), failed.clone());
            append(&root, &controls.root, false, false);
            controls
        });
        let section = Rc::new(Self { root, empty_label, list_box, scroll, charge_controls, rows: RefCell::new(HashMap::new()),
                                     backend, changed, failed, bluetooth, detailed });
        section.refresh();
        section
    }

    pub fn refresh(&self) {
        let devices = self.backend.batteries();
        let paths: Vec<String> = devices.iter().map(|d| text(d.get("path"))).collect();
        let stale: Vec<Rc<BatteryRow>> = {
            let mut rows = self.rows.borrow_mut();
            let gone: Vec<String> = rows.keys().filter(|path| !paths.contains(path)).cloned().collect();
            gone.iter().filter_map(|path| rows.remove(path)).collect()
        };
        for row in stale {
            self.list_box.remove(&row.root);
        }
        let mut previous: Option<gtk::Widget> = None;
        for (device, path) in devices.iter().zip(&paths) {
            let existing = self.rows.borrow().get(path).cloned();
            let row = match existing {
                Some(row) => {
                    row.refresh(device);
                    row
                }
                None => {
                    let row = BatteryRow::new(self.backend.clone(), device, self.detailed, self.changed.clone(),
                                              self.failed.clone(), self.bluetooth.clone());
                    append(&self.list_box, &row.root, false, false);
                    self.rows.borrow_mut().insert(path.clone(), row.clone());
                    row
                }
            };
            self.list_box.reorder_child_after(&row.root, previous.as_ref());
            previous = Some(row.root.clone().upcast());
        }
        self.empty_label.set_text(&if self.backend.power_ready() { tr!("No battery devices detected.") }
                                   else { tr!("Battery information is unavailable.") });
        self.empty_label.set_visible(devices.is_empty());
        if let Some(scroll) = &self.scroll {
            scroll.set_visible(!devices.is_empty());
        }
        if let Some(controls) = &self.charge_controls {
            controls.refresh();
        }
    }
}

/// Weak "call this owner's method" closure helper.
pub fn weak_fn<T: 'static>(weak: &Weak<T>, f: impl Fn(&T) + 'static) -> Rc<dyn Fn()> {
    let weak = weak.clone();
    Rc::new(move || {
        if let Some(owner) = weak.upgrade() {
            f(&owner);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_and_level_labels() {
        assert_eq!(time_label(Some(&json!(3960))), "1 hr 6 min");
        assert_eq!(time_label(Some(&json!(7200))), "2 hr");
        assert_eq!(time_label(Some(&json!(20))), "1 min");
        assert_eq!(time_label(Some(&json!(90))), "2 min");
        assert_eq!(time_label(Some(&json!(150))), "2 min", "round half to even");
        assert_eq!(time_label(Some(&json!(0))), "");
        assert_eq!(time_label(None), "");
        assert_eq!(level_label(&json!({"percentage": 72.5})), "72%");
        assert_eq!(level_label(&json!({"percentage": null, "level": "Low"})), "Low");
        assert_eq!(level_label(&json!({"percentage": null, "level": ""})), "Unknown");
        assert_eq!(level_label(&json!({"percentage": 101})), "Unknown");
    }

    #[test]
    fn status_labels_only_estimate_for_matching_state() {
        let device = json!({"state": "Fully charged", "state_code": 4, "time_to_full": 7200});
        assert_eq!(status_label(&device), "Fully charged");
        let device = json!({"state": "Charging", "state_code": 1, "time_to_full": 7200});
        assert_eq!(status_label(&device), "Charging · 2 hr until full");
        let device = json!({"state": "Discharging", "state_code": 2, "time_to_empty": 3960, "warning": "Low battery"});
        assert_eq!(status_label(&device), "Discharging · 1 hr 6 min remaining · Low battery");
        assert_eq!(status_label(&json!({})), "Status unavailable");
    }
}
