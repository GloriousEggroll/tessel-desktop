//! Battery UI checks with a fake backend (port of test_power_ui.BatteryUITests),
//! plus the real binaries against a fake UPower. Runs only inside a private
//! nested headless labwc (tests/run_headless.sh); otherwise it skips itself.

mod support;

use tessel_power_core::BatteryModel;
use tessel_battery::ui::{AppHooks, BatteryPopup, BatterySettingsWindow};
use gtk::prelude::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_gtk::gtk;
use support::*;

const SECOND: Duration = Duration::from_secs(1);

fn battery(path: &str, overrides: Value) -> Value {
    let mut device = json!({"path": format!("/org/freedesktop/UPower/devices/{path}"),
        "name": "Sony Wireless Controller", "kind": "Game controller", "type": 12,
        "icon_name": "input-gaming-symbolic", "percentage": 100.0, "level": "",
        "state": "Fully charged", "state_code": 4, "power_supply": false,
        "rechargeable": true, "time_to_empty": null, "time_to_full": null,
        "health": null, "cycles": null, "warning": "", "bluetooth": false,
        "charge_threshold_supported": false, "charge_threshold_enabled": null,
        "charge_start_threshold": null, "charge_end_threshold": null, "busy": false});
    for (key, value) in overrides.as_object().unwrap() {
        device[key] = value.clone();
    }
    device
}

fn laptop_battery() -> Value {
    battery("BAT0", json!({"name": "Laptop battery", "kind": "Battery", "type": 2, "icon_name": "battery-good-symbolic",
        "percentage": 72, "state": "Discharging", "state_code": 2, "power_supply": true, "time_to_empty": 10860,
        "health": 91, "cycles": 164, "charge_threshold_supported": true, "charge_threshold_enabled": true,
        "charge_start_threshold": 60, "charge_end_threshold": 80}))
}

struct FakeBackend {
    power_ready: Cell<bool>,
    power_source_known: Cell<bool>,
    on_battery: Cell<bool>,
    has_battery: Cell<bool>,
    percentage: Cell<Option<f64>>,
    busy: Cell<bool>,
    error: RefCell<String>,
    batteries: RefCell<Vec<Value>>,
    charge_limit: RefCell<Value>,
    battery_history: RefCell<Vec<(String, bool)>>,
    charge_limit_history: RefCell<Vec<i64>>,
    reject: Cell<bool>,
    pending: Cell<Option<i64>>,
}

impl FakeBackend {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            power_ready: Cell::new(true), power_source_known: Cell::new(true), on_battery: Cell::new(false),
            has_battery: Cell::new(false), percentage: Cell::new(None), busy: Cell::new(false), error: RefCell::new(String::new()),
            batteries: RefCell::new(vec![battery("controller", json!({}))]),
            charge_limit: RefCell::new(json!({"supported": false, "maximum": null, "battery_count": 0, "busy": false, "error": "", "reason": ""})),
            battery_history: RefCell::new(vec![]), charge_limit_history: RefCell::new(vec![]), reject: Cell::new(false), pending: Cell::new(None),
        })
    }

    fn with_adjustable_battery(self: &Rc<Self>, maximum: i64) -> Rc<Self> {
        self.has_battery.set(true);
        self.percentage.set(Some(72.0));
        let mut device = laptop_battery();
        device["charge_limit_adjustable"] = json!(true);
        device["charge_end_threshold"] = json!(maximum);
        self.batteries.borrow_mut().insert(0, device);
        let mut limit = self.charge_limit.borrow_mut();
        limit["supported"] = json!(true);
        limit["maximum"] = json!(maximum);
        limit["battery_count"] = json!(1);
        self.clone()
    }

    fn update(&self, index: usize, values: Value) {
        let mut batteries = self.batteries.borrow_mut();
        for (key, value) in values.as_object().unwrap() {
            batteries[index][key] = value.clone();
        }
    }

    fn limit(&self, key: &str, value: Value) {
        self.charge_limit.borrow_mut()[key] = value;
    }

    fn finish_charge_limit(&self, maximum: Option<i64>, error: &str) {
        assert!(self.pending.take().is_some(), "No fake charge-limit request is pending");
        if let Some(maximum) = maximum {
            self.limit("maximum", json!(maximum));
        }
        self.error.replace(error.into());
        self.limit("error", json!(error));
        self.limit("busy", json!(false));
    }
}

impl BatteryModel for FakeBackend {
    fn power_ready(&self) -> bool { self.power_ready.get() }
    fn power_source_known(&self) -> bool { self.power_source_known.get() }
    fn on_battery(&self) -> bool { self.on_battery.get() }
    fn has_battery(&self) -> bool { self.has_battery.get() }
    fn percentage(&self) -> Option<f64> { self.percentage.get() }
    fn busy(&self) -> bool { self.busy.get() }
    fn error(&self) -> String { self.error.borrow().clone() }
    fn batteries(&self) -> Vec<Value> { self.batteries.borrow().clone() }
    fn charge_limit(&self) -> Value { self.charge_limit.borrow().clone() }

    fn set_charge_thresholds(&self, path: &str, enabled: bool) -> bool {
        self.battery_history.borrow_mut().push((path.into(), enabled));
        if self.reject.get() {
            self.error.replace("Authorization was cancelled.".into());
            return false;
        }
        for device in self.batteries.borrow_mut().iter_mut() {
            if device["path"] == path {
                device["charge_threshold_enabled"] = json!(enabled);
            }
        }
        true
    }

    fn set_charge_limit(&self, maximum: &Value) -> bool {
        let maximum = maximum.as_i64().unwrap();
        self.charge_limit_history.borrow_mut().push(maximum);
        if self.reject.get() {
            self.error.replace("Authorization was cancelled.".into());
            self.limit("error", json!("Authorization was cancelled."));
            return false;
        }
        let state = self.charge_limit();
        assert!(state["supported"] == true && state["busy"] != true, "The UI attempted an unavailable charge-limit change");
        self.pending.set(Some(maximum));
        self.error.replace(String::new());
        self.limit("error", json!(""));
        // Accepting a request is not a successful hardware readback.
        self.limit("busy", json!(true));
        true
    }
}

struct Hooks {
    error: RefCell<String>,
    bluetooth: Cell<bool>,
    opened: Rc<RefCell<Vec<String>>>,
    on_open: RefCell<Option<Box<dyn Fn()>>>,
}

impl AppHooks for Hooks {
    fn error(&self) -> String { self.error.borrow().clone() }
    fn bluetooth_available(&self) -> bool { self.bluetooth.get() }
    fn open_bluetooth(&self) -> bool {
        match &*self.on_open.borrow() {
            Some(callback) => callback(),
            None => self.opened.borrow_mut().push("bluetooth".into()),
        }
        true
    }
}

struct Case {
    app: gtk::Application,
    backend: Rc<FakeBackend>,
    hooks: Rc<Hooks>,
    opened: Rc<RefCell<Vec<String>>>,
    windows: RefCell<Vec<gtk::Window>>,
}

impl Case {
    fn new(app: &gtk::Application) -> Self {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let hooks = Rc::new(Hooks { error: RefCell::new(String::new()), bluetooth: Cell::new(true), opened: opened.clone(), on_open: RefCell::new(None) });
        Self { app: app.clone(), backend: FakeBackend::new(), hooks, opened, windows: RefCell::new(Vec::new()) }
    }

    fn popup(&self) -> Rc<BatteryPopup> {
        let opened = self.opened.clone();
        let popup = BatteryPopup::new(Some(&self.app), self.hooks.clone(), self.backend.clone(),
                                      Rc::new(move || opened.borrow_mut().push("settings".into())), "bottom", 76);
        self.windows.borrow_mut().push(popup.window.clone().upcast());
        popup.show();
        assert!(wait_until(3 * SECOND, || popup.window.is_visible() && popup.window.width() > 0), "popup shown");
        settle(Duration::from_millis(60));
        popup
    }

    fn settings(&self) -> Rc<BatterySettingsWindow> {
        let window = BatterySettingsWindow::new(Some(&self.app), self.hooks.clone(), self.backend.clone(), None);
        self.windows.borrow_mut().push(window.window.clone().upcast());
        window.window.set_visible(true);
        window.refresh();
        assert!(wait_until(3 * SECOND, || window.window.width() > 0), "settings shown");
        settle(Duration::from_millis(60));
        window
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        for window in self.windows.borrow().iter() {
            window.destroy();
        }
        settle(Duration::from_millis(60));
    }
}

fn first_name(image: &gtk::Image) -> String {
    image.gicon().and_downcast::<gtk::gio::ThemedIcon>().map(|icon| icon.names()[0].to_string()).unwrap_or_default()
}

fn live_power_source_updates_without_changing_battery_controls(case: &Case) {
    let popup = case.popup();
    let window = case.settings();
    for (label, image) in [(&popup.source_label, &popup.source_icon), (&window.source_label, &window.source_icon)] {
        assert_eq!(label.text(), "Plugged in");
        assert_eq!(first_name(image), "ac-adapter-symbolic");
    }
    case.backend.has_battery.set(true);
    case.backend.on_battery.set(true);
    case.backend.percentage.set(Some(72.3));
    popup.refresh();
    window.refresh();
    for (label, image) in [(&popup.source_label, &popup.source_icon), (&window.source_label, &window.source_icon)] {
        assert_eq!(label.text(), "On battery · 72%");
        assert_eq!(first_name(image), "battery-good-symbolic");
    }
    case.backend.on_battery.set(false);
    popup.refresh();
    assert_eq!(popup.source_label.text(), "Plugged in · 72%");
    case.backend.power_source_known.set(false);
    popup.refresh();
    window.refresh();
    for (label, image) in [(&popup.source_label, &popup.source_icon), (&window.source_label, &window.source_icon)] {
        assert_eq!(label.text(), "Power source unavailable");
        assert_eq!(first_name(image), "battery-missing-symbolic");
    }
    assert!(case.backend.battery_history.borrow().is_empty());
    assert!(case.backend.charge_limit_history.borrow().is_empty());
    // Popup surface classes and header.
    assert!(popup.window.has_css_class("powermanager") && popup.window.has_css_class("batterymanager") && popup.window.has_css_class("tray-popup"));
    assert_eq!(popup.window.title().unwrap(), "Battery");
    assert_eq!(window.window.title().unwrap(), "Battery Settings");
    assert_eq!(popup.settings_button.tooltip_text().unwrap(), "Battery settings");
}

fn battery_gear_dismisses_popup_and_keeps_power_settings_separate(case: &Case) {
    let popup = case.popup();
    popup.settings_button.emit_clicked();
    assert_eq!(*case.opened.borrow(), ["settings"]);
    assert!(!popup.window.is_visible());
    let window = case.settings();
    let controls = window.batteries.charge_controls.clone().unwrap();
    assert!(!controls.root.is_visible());
    assert!(!controls.apply_button.is_sensitive());
    assert!(popup.window.width() <= 430);
}

fn hotplug_removal_and_unavailable_status_do_not_keep_old_rows(case: &Case) {
    let popup = case.popup();
    *case.backend.batteries.borrow_mut() = (0..9).map(|i| battery(&i.to_string(), json!({"name": format!("Headphones {i}")}))).collect();
    popup.refresh();
    settle(Duration::from_millis(100));
    assert_eq!(popup.batteries.rows.borrow().len(), 9);
    let scroll = popup.batteries.scroll.clone().unwrap();
    assert!(scroll.height() <= 230, "scroll height {}", scroll.height());
    case.backend.batteries.borrow_mut().clear();
    popup.refresh();
    assert!(popup.batteries.rows.borrow().is_empty());
    assert!(!scroll.is_visible());
    assert!(popup.batteries.empty_label.text().contains("No battery devices"));
    case.backend.power_ready.set(false);
    popup.refresh();
    assert!(popup.batteries.empty_label.text().contains("unavailable"));
}

fn device_battery_refresh_is_read_only_and_preserves_row_identity(case: &Case) {
    let popup = case.popup();
    let path = case.backend.batteries()[0]["path"].as_str().unwrap().to_owned();
    let row = popup.batteries.rows.borrow()[&path].clone();
    assert_eq!(row.name_label.text(), "Sony Wireless Controller");
    assert_eq!(row.level_label.text(), "100%");
    assert_eq!(row.state_label.text(), "Fully charged");
    assert!(!row.protection_box.is_visible());
    assert!(!row.bluetooth_button.is_visible(), "A wireless name alone does not establish Bluetooth transport");
    case.backend.update(0, json!({"percentage": 19, "state": "Discharging", "state_code": 2, "time_to_empty": 3960}));
    for _ in 0..3 {
        popup.refresh();
    }
    assert!(Rc::ptr_eq(&popup.batteries.rows.borrow()[&path], &row));
    assert_eq!(row.level_label.text(), "19%");
    assert!(row.state_label.text().contains("1 hr 6 min remaining"));
    assert!(case.backend.battery_history.borrow().is_empty());
}

fn full_battery_gauge_spans_row_with_status_underneath(case: &Case) {
    let path = case.backend.batteries()[0]["path"].as_str().unwrap().to_owned();
    let popup = case.popup();
    let settings = case.settings();
    for row in [popup.batteries.rows.borrow()[&path].clone(), settings.batteries.rows.borrow()[&path].clone()] {
        assert!(row.level_bar.is_visible());
        assert_eq!(row.level_bar.value(), 1.0);
        let (width, height) = (row.level_bar.width(), row.level_bar.height());
        assert!(width as f64 >= row.root.width() as f64 * 0.85, "gauge {width} of {}", row.root.width());
        assert!(height > 0 && height <= 12);
        let gauge = row.level_bar.compute_point(&row.root, &gtk::graphene::Point::new(0.0, 0.0)).unwrap();
        let level = row.level_label.compute_point(&row.root, &gtk::graphene::Point::new(0.0, 0.0)).unwrap();
        let status = row.state_label.compute_point(&row.root, &gtk::graphene::Point::new(0.0, 0.0)).unwrap();
        assert!(gauge.y() >= level.y() + row.level_label.height() as f32);
        assert!(status.y() >= gauge.y() + height as f32);
        assert!((status.x() - gauge.x()).abs() <= 2.0);
        assert_eq!(row.state_label.text(), "Fully charged");
    }
}

fn coarse_and_unknown_battery_levels_do_not_invent_percentages(case: &Case) {
    *case.backend.batteries.borrow_mut() = vec![battery("controller", json!({"percentage": null, "level": "Low", "time_to_full": 7200}))];
    let popup = case.popup();
    let row = popup.batteries.rows.borrow().values().next().unwrap().clone();
    assert_eq!(row.level_label.text(), "Low");
    assert!(!row.level_bar.is_visible());
    assert_eq!(row.state_label.text(), "Fully charged", "Do not show charge estimates for another state");
    case.backend.update(0, json!({"level": "", "state": "Charging", "state_code": 1}));
    popup.refresh();
    assert_eq!(row.level_label.text(), "Unknown");
    assert!(row.state_label.text().contains("2 hr until full"));
}

fn battery_protection_requires_support_and_handles_rejection(case: &Case) {
    let device = laptop_battery();
    let path = device["path"].as_str().unwrap().to_owned();
    *case.backend.batteries.borrow_mut() = vec![device];
    let popup = case.popup();
    let row = popup.batteries.rows.borrow()[&path].clone();
    assert!(row.protection_box.is_visible());
    assert!(row.protection_switch.is_active());
    assert!(row.protection_hint.text().contains("80%"));
    assert!(case.backend.battery_history.borrow().is_empty());
    case.backend.reject.set(true);
    row.protection_switch.set_active(false);
    assert_eq!(*case.backend.battery_history.borrow(), [(path.clone(), false)]);
    assert!(row.protection_switch.is_active());
    assert!(popup.status_label.text().contains("cancelled"));
    case.backend.reject.set(false);
    case.backend.error.replace(String::new());
    row.protection_switch.set_active(false);
    assert!(!row.protection_switch.is_active());
    case.backend.update(0, json!({"busy": true}));
    popup.refresh();
    assert!(!row.protection_switch.is_sensitive());
    row.protection_switch.emit_by_name::<bool>("state-set", &[&true]);
    assert_eq!(case.backend.battery_history.borrow().len(), 2);
    case.backend.update(0, json!({"busy": false, "charge_threshold_supported": false}));
    popup.refresh();
    assert!(!row.protection_box.is_visible());
    row.protection_switch.emit_by_name::<bool>("state-set", &[&true]);
    assert_eq!(case.backend.battery_history.borrow().len(), 2);
}

fn battery_details_show_health_and_cycles(case: &Case) {
    let device = laptop_battery();
    let path = device["path"].as_str().unwrap().to_owned();
    case.backend.has_battery.set(true);
    *case.backend.batteries.borrow_mut() = vec![device];
    let window = case.settings();
    let row = window.batteries.rows.borrow()[&path].clone();
    assert!(row.details_label.text().contains("Health: 91%"));
    assert!(row.details_label.text().contains("Charge cycles: 164"));
    assert!(row.details_label.text().starts_with("Battery"));
    assert!(row.state_label.text().contains("3 hr 1 min remaining"));
    let popup = case.popup();
    assert!(!popup.batteries.rows.borrow()[&path].details_label.is_visible(), "the popup is compact");
}

fn charge_limit_edits_require_apply_and_readback_before_success(case: &Case) {
    case.backend.with_adjustable_battery(100);
    let window = case.settings();
    let controls = window.batteries.charge_controls.clone().unwrap();
    assert!(controls.root.is_visible());
    assert_eq!(controls.limit.value_as_int(), 100);
    assert!(!controls.apply_button.is_sensitive());
    controls.limit.set_value(80.0);
    for _ in 0..3 {
        window.refresh();
    }
    assert_eq!(controls.limit.value_as_int(), 80);
    assert!(controls.hint.text().contains("100%"));
    assert!(case.backend.charge_limit_history.borrow().is_empty());
    assert!(controls.apply_button.is_sensitive());
    controls.apply_button.emit_clicked();
    assert_eq!(*case.backend.charge_limit_history.borrow(), [80]);
    assert_eq!(case.backend.charge_limit()["maximum"], 100);
    assert!(!controls.apply_button.is_sensitive());
    assert!(controls.hint.text().contains("Updating"));
    controls.apply_button.emit_clicked();
    assert_eq!(*case.backend.charge_limit_history.borrow(), [80], "Do not submit a duplicate while pending");
    case.backend.finish_charge_limit(Some(80), "");
    window.refresh();
    assert!(controls.hint.text().contains("Current maximum: 80%"));
    assert!(!controls.apply_button.is_sensitive());
}

fn pending_charge_request_preserves_newer_edit(case: &Case) {
    case.backend.with_adjustable_battery(100);
    let window = case.settings();
    let controls = window.batteries.charge_controls.clone().unwrap();
    controls.limit.set_value(80.0);
    controls.apply_button.emit_clicked();
    controls.limit.set_value(85.0);
    window.refresh();
    assert_eq!(controls.limit.value_as_int(), 85);
    assert!(!controls.apply_button.is_sensitive());
    case.backend.finish_charge_limit(Some(80), "");
    window.refresh();
    assert_eq!(controls.limit.value_as_int(), 85);
    assert!(controls.hint.text().contains("Current maximum: 80%"));
    assert!(controls.apply_button.is_sensitive());
    controls.apply_button.emit_clicked();
    assert_eq!(*case.backend.charge_limit_history.borrow(), [80, 85]);
}

fn denied_charge_limit_retains_actual_value_and_can_retry(case: &Case) {
    case.backend.with_adjustable_battery(100);
    let window = case.settings();
    let controls = window.batteries.charge_controls.clone().unwrap();
    controls.limit.set_value(80.0);
    case.backend.reject.set(true);
    controls.apply_button.emit_clicked();
    assert_eq!(case.backend.charge_limit()["maximum"], 100);
    assert_eq!(controls.limit.value_as_int(), 80);
    assert!(controls.hint.text().contains("cancelled"));
    assert!(controls.apply_button.is_sensitive());
    case.backend.reject.set(false);
    controls.apply_button.emit_clicked();
    case.backend.finish_charge_limit(None, "The device rejected the requested maximum.");
    window.refresh();
    assert_eq!(case.backend.charge_limit()["maximum"], 100);
    assert_eq!(controls.limit.value_as_int(), 80);
    assert!(controls.hint.text().contains("rejected"));
    assert!(controls.apply_button.is_sensitive());
    assert_eq!(*case.backend.charge_limit_history.borrow(), [80, 80]);
}

fn edit_back_to_previous_maximum_survives_pending_readback(case: &Case) {
    case.backend.with_adjustable_battery(100);
    let window = case.settings();
    let controls = window.batteries.charge_controls.clone().unwrap();
    controls.limit.set_value(80.0);
    controls.apply_button.emit_clicked();
    controls.limit.set_value(100.0);
    window.refresh();
    case.backend.finish_charge_limit(Some(80), "");
    window.refresh();
    assert_eq!(controls.limit.value_as_int(), 100);
    assert!(controls.apply_button.is_sensitive());
    assert!(controls.hint.text().contains("Current maximum: 80%"));
    assert_eq!(*case.backend.charge_limit_history.borrow(), [80]);
}

fn charge_limit_capability_loss_and_global_busy_cannot_issue_writes(case: &Case) {
    case.backend.with_adjustable_battery(100);
    let window = case.settings();
    let controls = window.batteries.charge_controls.clone().unwrap();
    controls.limit.set_value(80.0);
    case.backend.busy.set(true);
    window.refresh();
    assert!(!controls.apply_button.is_sensitive());
    controls.apply_button.emit_clicked();
    assert!(case.backend.charge_limit_history.borrow().is_empty());
    case.backend.busy.set(false);
    case.backend.limit("supported", json!(false));
    case.backend.limit("reason", json!("Charge limits are unavailable."));
    window.refresh();
    assert!(controls.root.is_visible(), "Explain unavailable laptop controls");
    assert!(!controls.limit.is_sensitive());
    assert!(!controls.apply_button.is_sensitive());
    assert!(controls.hint.text().contains("unavailable"));
    controls.apply_button.emit_clicked();
    assert!(case.backend.charge_limit_history.borrow().is_empty());
    case.backend.limit("battery_count", json!(0));
    case.backend.limit("reason", json!(""));
    window.refresh();
    assert!(!controls.root.is_visible());
}

fn numeric_charge_limits_hide_preset_protection_and_follow_external_readback(case: &Case) {
    case.backend.with_adjustable_battery(80);
    let popup = case.popup();
    let window = case.settings();
    let path = case.backend.batteries()[0]["path"].as_str().unwrap().to_owned();
    for section in [&popup.batteries, &window.batteries] {
        let row = section.rows.borrow()[&path].clone();
        assert!(!row.protection_box.is_visible());
        row.protection_switch.emit_by_name::<bool>("state-set", &[&false]);
    }
    assert!(case.backend.battery_history.borrow().is_empty());
    assert!(window.batteries.rows.borrow()[&path].details_label.text().contains("Charge limit: 80%"));
    let controls = window.batteries.charge_controls.clone().unwrap();
    case.backend.limit("maximum", json!(90));
    window.refresh();
    assert_eq!(controls.limit.value_as_int(), 90);
    assert!(!controls.apply_button.is_sensitive());
    assert!(case.backend.charge_limit_history.borrow().is_empty());
}

fn bluetooth_shortcut_is_gated_and_dismisses_popup_before_opening(case: &Case) {
    *case.backend.batteries.borrow_mut() = vec![battery("controller", json!({"bluetooth": true}))];
    let popup = case.popup();
    let row = popup.batteries.rows.borrow().values().next().unwrap().clone();
    assert!(row.bluetooth_button.is_visible());
    let (opened, window) = (case.opened.clone(), popup.window.clone());
    case.hooks.on_open.replace(Some(Box::new(move || opened.borrow_mut().push(format!("bluetooth:{}", window.is_visible())))));
    row.bluetooth_button.emit_clicked();
    assert_eq!(*case.opened.borrow(), ["bluetooth:false"]);
    case.hooks.bluetooth.set(false);
    let unavailable = case.popup();
    assert!(!unavailable.batteries.rows.borrow().values().next().unwrap().bluetooth_button.is_visible());
}

fn status_messages_are_friendly_and_deduplicated(case: &Case) {
    let popup = case.popup();
    case.hooks.error.replace("Authorization was canceled or denied.".into());
    case.backend.error.replace("Authorization was canceled or denied.".into());
    popup.refresh();
    assert_eq!(popup.status_label.text(), "Authorization was canceled or denied.");
    case.backend.error.replace("GDBus.Error:org.freedesktop.DBus.Error.Failed: boom".into());
    popup.refresh();
    assert!(popup.status_label.text().ends_with("Some battery details could not be read. See the log for details."));
    case.hooks.error.replace(String::new());
    case.backend.error.replace(String::new());
    case.backend.busy.set(true);
    popup.refresh();
    assert_eq!(popup.status_label.text(), "Updating battery settings…");
    case.backend.busy.set(false);
    popup.refresh();
    assert!(!popup.status_label.is_visible());
}

fn style_css_parses() {
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = errors.clone();
    provider.connect_parsing_error(move |_, _, error| sink.borrow_mut().push(error.to_string()));
    provider.load_from_string(tessel_power_core::ui::style::STYLE_CSS);
    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
}

/// "VmRSS KiB (RssAnon KiB anon)" from /proc.
fn rss_kb(pid: u32) -> String {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let field = |name: &str| -> u64 {
        status.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0)
    };
    format!("{} KiB ({} KiB anon)", field("VmRSS:"), field("RssAnon:"))
}

/// `--background`, then `--check` until ready; returns (rss KiB, ms to ready).
fn measure_service(program: &str, args: &[&str], check: &dyn Fn() -> std::process::Output) -> (std::process::Child, String, u128) {
    let start = Instant::now();
    let child = Command::new(program).args(args).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let mut ready = None;
    wait_until(20 * SECOND, || {
        let output = check();
        if output.status.success() {
            ready = Some(start.elapsed().as_millis());
        }
        ready.is_some()
    });
    let ready = ready.expect("service ready");
    settle(Duration::from_millis(1500));
    let rss = rss_kb(child_id(&child));
    (child, rss, ready)
}

fn child_id(child: &std::process::Child) -> u32 {
    child.id()
}

fn stop(mut child: std::process::Child) -> Option<i32> {
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let mut status = None;
    wait_until(5 * SECOND, || { status = child.try_wait().unwrap(); status.is_some() });
    if status.is_none() {
        let _ = child.kill();
    }
    status.and_then(|s| s.code())
}

/// The real binaries against a fake UPower on this private session bus (doubling as the system bus).
fn application_and_measurements() {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
    // SAFETY: single-threaded test main; children inherit it.
    unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };
    let connection = gtk::gio::DBusConnection::for_address_sync(&address,
        gtk::gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gtk::gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None, None::<&gtk::gio::Cancellable>).unwrap();
    let upower = support::FakeUPower::from_connection(connection, false);
    upower.add_device("/org/freedesktop/UPower/devices/battery_BAT0", &[("Type", 2u32.to_variant()), ("PowerSupply", true.to_variant()),
        ("NativePath", "BAT0".to_variant()), ("Model", "Laptop battery".to_variant()), ("Percentage", 64.0f64.to_variant()),
        ("State", 1u32.to_variant())], false);
    let binary = env!("CARGO_BIN_EXE_tessel-battery");
    let check = || Command::new(binary).arg("--check").output().unwrap();
    let (child, rss, ready_ms) = measure_service(binary, &["--background"], &check);
    let output = check();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.ends_with("}\n"));
    let state: Value = serde_json::from_str(&text).unwrap();
    let keys: Vec<_> = state.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["ready", "has_battery", "on_battery", "percentage", "batteries", "charge_limit", "error"]);
    assert_eq!((state["ready"].clone(), state["has_battery"].clone(), state["percentage"].clone()), (json!(true), json!(true), json!(64.0)));
    assert!(text.starts_with("{\"ready\": true, \"has_battery\": true"), "Python json.dumps separators: {text}");
    for args in [&["--popup", "--popup-edge", "top", "--popup-offset", "30"][..], &["--popup"], &["--settings"], &["--background"], &[]] {
        let status = Command::new(binary).args(args).status().unwrap();
        assert_eq!(status.code(), Some(0), "{args:?}");
        settle(Duration::from_millis(300));
    }
    let bad = Command::new(binary).args(["--popup-edge", "diagonal"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(child_alive(&child), "primary survives popup/settings requests");
    let rss_after_windows = rss_kb(child.id());
    assert_eq!(stop(child), Some(0), "SIGTERM quits cleanly");

    // Indicator stream.
    let stream = env!("CARGO_BIN_EXE_tessel-battery-status");
    let start = Instant::now();
    let mut status_child = Command::new(stream).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let stdout = status_child.stdout.take().unwrap();
    std::thread::spawn(move || for line in BufReader::new(stdout).lines().map_while(Result::ok) { let _ = sender.send(line); });
    let mut lines = Vec::new();
    wait_until(10 * SECOND, || { while let Ok(line) = receiver.try_recv() { lines.push(line) } lines.len() >= 2 });
    let stream_ready = start.elapsed().as_millis();
    assert!(lines[1].contains("Plugged in \\u00b7 64%"), "{lines:?}");
    settle(Duration::from_millis(1000));
    let stream_rss = rss_kb(status_child.id());
    stop(status_child);
    println!("measure rust tessel-battery --background: ready {ready_ms} ms, RSS {rss} (after popup+settings {rss_after_windows})");
    println!("measure rust tessel-battery-status: first device line {stream_ready} ms, RSS {stream_rss}");

    // Python twins, run the same way (read-only, private buses), if requested.
    if let Some(source) = std::env::var("BATTERY_PYTHON_SOURCE").ok().filter(|source| !source.is_empty()) {
        let script = format!("{source}/battery_manager.py");
        let check = || Command::new("python3").args([script.as_str(), "--check"]).output().unwrap();
        let (child, rss, ready_ms) = measure_service("python3", &[script.as_str(), "--background"], &check);
        for args in [&["--popup"][..], &["--settings"]] {
            Command::new("python3").arg(&script).args(args).status().unwrap();
            settle(Duration::from_millis(500));
        }
        let rss_after = rss_kb(child.id());
        stop(child);
        let start = Instant::now();
        let mut python_stream = Command::new("python3").args(["-u", &format!("{source}/battery_status.py")])
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let stdout = python_stream.stdout.take().unwrap();
        std::thread::spawn(move || for line in BufReader::new(stdout).lines().map_while(Result::ok) { let _ = sender.send(line); });
        let mut lines = Vec::new();
        wait_until(20 * SECOND, || { while let Ok(line) = receiver.try_recv() { lines.push(line) } lines.len() >= 2 });
        let python_ready = start.elapsed().as_millis();
        settle(Duration::from_millis(1000));
        let python_rss = rss_kb(python_stream.id());
        stop(python_stream);
        println!("measure python battery_manager.py --background: ready {ready_ms} ms, RSS {rss} (after popup+settings {rss_after})");
        println!("measure python battery_status.py: first device line {python_ready} ms, RSS {python_rss}");
        if lines.len() >= 2 {
            println!("python stream line: {}", lines[1]);
        }
    }
    println!("ok application_and_measurements");
}

fn child_alive(child: &std::process::Child) -> bool {
    std::path::Path::new(&format!("/proc/{}", child.id())).exists()
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if std::env::var("BATTERY_UI_HEADLESS").is_err() || runtime == "/run/user/1000" || display.is_empty() {
        println!("skipped: run tests/run_headless.sh (private headless labwc)");
        return;
    }
    gtk::init().expect("GTK");
    let app = gtk::Application::new(Some("org.batterymanager.IsolatedTests"), gtk::gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let _hold = app.hold();
    style_css_parses();
    let cases: &[(&str, fn(&Case))] = &[
        ("live_power_source_updates_without_changing_battery_controls", live_power_source_updates_without_changing_battery_controls),
        ("battery_gear_dismisses_popup_and_keeps_power_settings_separate", battery_gear_dismisses_popup_and_keeps_power_settings_separate),
        ("hotplug_removal_and_unavailable_status_do_not_keep_old_rows", hotplug_removal_and_unavailable_status_do_not_keep_old_rows),
        ("device_battery_refresh_is_read_only_and_preserves_row_identity", device_battery_refresh_is_read_only_and_preserves_row_identity),
        ("full_battery_gauge_spans_row_with_status_underneath", full_battery_gauge_spans_row_with_status_underneath),
        ("coarse_and_unknown_battery_levels_do_not_invent_percentages", coarse_and_unknown_battery_levels_do_not_invent_percentages),
        ("battery_protection_requires_support_and_handles_rejection", battery_protection_requires_support_and_handles_rejection),
        ("battery_details_show_health_and_cycles", battery_details_show_health_and_cycles),
        ("charge_limit_edits_require_apply_and_readback_before_success", charge_limit_edits_require_apply_and_readback_before_success),
        ("pending_charge_request_preserves_newer_edit", pending_charge_request_preserves_newer_edit),
        ("denied_charge_limit_retains_actual_value_and_can_retry", denied_charge_limit_retains_actual_value_and_can_retry),
        ("edit_back_to_previous_maximum_survives_pending_readback", edit_back_to_previous_maximum_survives_pending_readback),
        ("charge_limit_capability_loss_and_global_busy_cannot_issue_writes", charge_limit_capability_loss_and_global_busy_cannot_issue_writes),
        ("numeric_charge_limits_hide_preset_protection_and_follow_external_readback", numeric_charge_limits_hide_preset_protection_and_follow_external_readback),
        ("bluetooth_shortcut_is_gated_and_dismisses_popup_before_opening", bluetooth_shortcut_is_gated_and_dismisses_popup_before_opening),
        ("status_messages_are_friendly_and_deduplicated", status_messages_are_friendly_and_deduplicated),
    ];
    for (name, case) in cases {
        let fixture = Case::new(&app);
        case(&fixture);
        drop(fixture);
        println!("ok {name}");
    }
    application_and_measurements();
    println!("test result: ok. {} passed", cases.len() + 2);
}
