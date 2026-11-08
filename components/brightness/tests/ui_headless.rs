#![allow(deprecated)]
//! Port of test_brightness_ui.py (BrightnessUITests, popup cases) with a fake
//! client, plus the keyboard OSD checks of test_brightness_app.py. Runs only
//! inside the private nested headless labwc (tests/run_headless.sh); under a
//! plain `cargo test` it skips itself. No backend, D-Bus or display hardware.

use tessel_brightness::backend::Hardware;
use tessel_brightness::client::UiClient;
use tessel_brightness::core::BrightnessCore;
use tessel_brightness::gui::GtkOsd;
use tessel_brightness::keys::BrightnessKeys;
use tessel_brightness::model::{self, Callback};
use tessel_brightness::style::STYLE_CSS;
use tessel_brightness::ui::{BrightnessPopup, Row};
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::LayerShell;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_gtk::gtk;
use tessel_gtk::osd::AdjustmentOsd;

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while !predicate() && Instant::now() < end {
        settle(0.02);
    }
    predicate()
}

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Value(String, String, f64, Option<String>),
    AutoDim(bool),
    Scale(String, f64, Option<String>),
    AutoScale(bool),
}

struct FakeClient {
    ready: Cell<bool>,
    error: RefCell<String>,
    history: RefCell<Vec<Call>>,
    commit: Cell<bool>,
    state: RefCell<Value>,
}

impl FakeClient {
    fn new() -> Rc<Self> {
        let feature = json!({"supported": true, "value": 75, "raw": 75, "max": 100, "busy": false, "error": ""});
        let unavailable = json!({"supported": false, "value": null, "raw": null, "max": null, "busy": false, "error": ""});
        Rc::new(Self { ready: Cell::new(true), error: RefCell::default(), history: RefCell::default(), commit: Cell::new(true),
            state: RefCell::new(json!({"displays": [
                {"connector": "DP-2", "name": "Dell U2723QE", "id": "dell-id", "enabled": true, "method": "ddc", "error": "",
                 "brightness": feature, "contrast": feature, "scale_id": "dell-scale",
                 "scale": {"supported": true, "value": 1.25, "busy": false, "error": "", "override": false}},
                {"connector": "HDMI-A-1", "name": "UGREEN Display", "id": "ugreen-id", "enabled": true, "method": "unsupported", "error": "",
                 "brightness": unavailable, "contrast": unavailable, "scale_id": "ugreen-scale",
                 "scale": {"supported": true, "value": 1.0, "busy": false, "error": "", "override": false}}],
                "settings": {"auto_dim": false, "dim_after": 120, "dim_percent": 30}, "dimmed": false,
                "scaling": {"ready": true, "busy": false, "error": "", "auto_scale": true}, "busy": false, "error": ""})) })
    }

    fn set(&self, f: impl FnOnce(&mut Value)) {
        f(&mut self.state.borrow_mut());
    }

    fn history(&self) -> Vec<Call> {
        self.history.borrow().clone()
    }
}

impl UiClient for FakeClient {
    fn state(&self) -> Value { self.state.borrow().clone() }
    fn ready(&self) -> bool { self.ready.get() }
    fn error(&self) -> String { self.error.borrow().clone() }

    fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: Option<&str>) -> bool {
        self.history.borrow_mut().push(Call::Value(connector.into(), feature.into(), value, expected_id.map(String::from)));
        if self.commit.get() {
            let mut state = self.state.borrow_mut();
            let display = state["displays"].as_array_mut().unwrap().iter_mut().find(|d| d["connector"] == connector).unwrap();
            display[feature]["value"] = json!(value);
        }
        true
    }

    fn set_auto_dim(&self, enabled: bool) -> bool {
        self.history.borrow_mut().push(Call::AutoDim(enabled));
        if self.commit.get() {
            self.state.borrow_mut()["settings"]["auto_dim"] = enabled.into();
        }
        true
    }

    fn set_scale(&self, connector: &str, value: f64, expected_id: Option<&str>) -> bool {
        self.history.borrow_mut().push(Call::Scale(connector.into(), value, expected_id.map(String::from)));
        if self.commit.get() {
            let mut state = self.state.borrow_mut();
            let display = state["displays"].as_array_mut().unwrap().iter_mut().find(|d| d["connector"] == connector).unwrap();
            display["scale"]["value"] = json!(value);
            display["scale"]["override"] = true.into();
        }
        true
    }

    fn set_auto_scale(&self, enabled: bool) -> bool {
        self.history.borrow_mut().push(Call::AutoScale(enabled));
        if self.commit.get() {
            self.state.borrow_mut()["scaling"]["auto_scale"] = enabled.into();
        }
        true
    }

    fn set_settings(&self, _settings: &Value) -> bool { true }
    fn refresh(&self) -> bool { true }
}

struct Fx {
    client: Rc<FakeClient>,
    opened: Rc<RefCell<Vec<String>>>,
    windows: RefCell<Vec<gtk::Window>>,
}

impl Fx {
    fn new() -> Self {
        Self { client: FakeClient::new(), opened: Rc::default(), windows: RefCell::default() }
    }

    fn popup(&self) -> Rc<BrightnessPopup> {
        let opened = self.opened.clone();
        let popup = BrightnessPopup::new(self.client.clone(), Rc::new(String::new),
                                         Rc::new(move || opened.borrow_mut().push("settings".into())), "bottom", 76);
        self.windows.borrow_mut().push(popup.window.clone());
        popup.show();
        settle(0.06);
        popup
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for window in self.windows.borrow().iter() {
            window.destroy();
        }
        settle(0.06);
    }
}

fn row(popup: &BrightnessPopup, feature: &str) -> Row {
    popup.controls.row("DP-2", "dell-id", feature).expect("row")
}

fn value(connector: &str, feature: &str, v: f64, id: &str) -> Call {
    Call::Value(connector.into(), feature.into(), v, Some(id.into()))
}

fn refresh_is_read_only_and_shows_actual_values_and_unsupported_controls() {
    let f = Fx::new();
    let popup = f.popup();
    assert!(wait_until(|| popup.window.is_mapped(), 2.0), "popup maps");
    for _ in 0..4 {
        popup.refresh();
    }
    assert!(f.client.history().is_empty());
    let brightness = row(&popup, "brightness");
    assert_eq!(brightness.value_label().text(), "75%");
    assert!(brightness.scale().is_sensitive());
    let unavailable = popup.controls.row("HDMI-A-1", "ugreen-id", "brightness").unwrap();
    assert_eq!(unavailable.value_label().text(), "—");
    assert!(!unavailable.scale().is_sensitive());
    assert!(unavailable.hint().text().contains("Not supported"));
    unavailable.scale().set_value(20.0);
    settle(0.25);
    assert!(f.client.history().is_empty());
    assert!(popup.window.width() <= 430);
    assert!(popup.window.has_css_class("brightnessmanager") && popup.window.has_css_class("tray-popup"));
    assert_eq!(gtk4_layer_shell::LayerShell::namespace(&popup.window).as_deref(), Some("desktop-applet"));
    println!("ok refresh_is_read_only_and_shows_actual_values_and_unsupported_controls");
}

fn hdr_locked_control_is_disabled_with_its_value_and_a_note() {
    let f = Fx::new();
    f.client.set(|state| {
        state["displays"][0]["contrast"]["hdr_locked"] = json!(true);
        state["displays"][0]["contrast"]["error"] = json!("This display does not allow changing this while HDR is on.");
    });
    let popup = f.popup();
    let contrast = row(&popup, "contrast");
    assert!(!contrast.scale().is_sensitive());
    assert_eq!(contrast.value_label().text(), "75%");
    assert_eq!(contrast.scale().value(), 75.0);
    assert!(contrast.hint().text().contains("Turn off HDR"), "{}", contrast.hint().text());
    assert!(contrast.hint().text().contains("contrast"));
    contrast.scale().set_value(20.0);
    settle(0.25);
    assert!(f.client.history().is_empty());
    assert!(row(&popup, "brightness").scale().is_sensitive(), "only the locked control is disabled");
    println!("ok hdr_locked_control_is_disabled_with_its_value_and_a_note");
}

fn debounce_coalesces_gesture_and_other_feature_refresh_does_not_reset_it() {
    let f = Fx::new();
    let popup = f.popup();
    let brightness = row(&popup, "brightness");
    brightness.scale().set_value(40.0);
    brightness.scale().set_value(51.0);
    f.client.set(|s| s["displays"][0]["contrast"]["value"] = 80.into());
    popup.refresh();
    assert_eq!(brightness.scale().value(), 51.0);
    assert_eq!(row(&popup, "contrast").scale().value(), 80.0);
    assert!(f.client.history().is_empty());
    settle(0.25);
    assert_eq!(f.client.history(), vec![value("DP-2", "brightness", 51.0, "dell-id")]);
    println!("ok debounce_coalesces_gesture_and_other_feature_refresh_does_not_reset_it");
}

fn busy_feature_keeps_latest_pending_value_and_remains_interactive() {
    let f = Fx::new();
    let popup = f.popup();
    let brightness = row(&popup, "brightness");
    f.client.set(|s| s["displays"][0]["brightness"]["busy"] = true.into());
    brightness.scale().set_value(40.0);
    popup.refresh();
    assert!(brightness.scale().is_sensitive());
    settle(0.25);
    assert!(f.client.history().is_empty());
    brightness.scale().set_value(45.0);
    f.client.set(|s| s["displays"][0]["brightness"]["busy"] = false.into());
    popup.refresh();
    assert_eq!(brightness.scale().value(), 45.0);
    settle(0.25);
    assert_eq!(f.client.history(), vec![value("DP-2", "brightness", 45.0, "dell-id")]);
    println!("ok busy_feature_keeps_latest_pending_value_and_remains_interactive");
}

fn submitted_value_survives_unrelated_refresh_until_hardware_reply() {
    let f = Fx::new();
    f.client.commit.set(false);
    let popup = f.popup();
    let brightness = row(&popup, "brightness");
    brightness.scale().set_value(42.0);
    settle(0.25);
    popup.refresh();
    assert_eq!(brightness.scale().value(), 42.0);
    assert_eq!(brightness.value_label().text(), "42%");
    f.client.set(|s| s["displays"][0]["brightness"]["busy"] = true.into());
    popup.refresh();
    f.client.set(|s| { s["displays"][0]["brightness"]["busy"] = false.into(); s["displays"][0]["brightness"]["value"] = 41.into(); });
    popup.refresh();
    assert_eq!(brightness.scale().value(), 41.0);
    println!("ok submitted_value_survives_unrelated_refresh_until_hardware_reply");
}

fn replaced_display_cannot_receive_old_debounced_command_even_without_refresh() {
    let f = Fx::new();
    let popup = f.popup();
    row(&popup, "brightness").scale().set_value(20.0);
    f.client.set(|s| s["displays"][0]["id"] = "replacement-id".into());
    settle(0.25);
    assert!(f.client.history().is_empty());
    popup.refresh();
    assert!(popup.controls.row("DP-2", "dell-id", "brightness").is_none());
    assert!(popup.controls.row("DP-2", "replacement-id", "brightness").is_some());
    println!("ok replaced_display_cannot_receive_old_debounced_command_even_without_refresh");
}

fn dismiss_destroy_and_gear_cancel_pending_slider_changes() {
    let f = Fx::new();
    let popup = f.popup();
    row(&popup, "brightness").scale().set_value(20.0);
    popup.dismiss();
    settle(0.25);
    assert!(f.client.history().is_empty());
    popup.show();
    assert!(wait_until(|| popup.window.is_visible(), 2.0), "Popup must reappear after show()");
    row(&popup, "brightness").scale().set_value(30.0);
    popup.settings_button.emit_clicked();
    settle(0.25);
    assert_eq!(*f.opened.borrow(), vec!["settings".to_owned()]);
    assert!(!popup.window.is_visible());
    assert!(f.client.history().is_empty());
    assert_eq!(popup.settings_button.tooltip_text().as_deref(), Some("Display settings"));
    println!("ok dismiss_destroy_and_gear_cancel_pending_slider_changes");
}

fn dismiss_racing_asynchronous_show_still_cancels_pending_slider() {
    let f = Fx::new();
    let popup = f.popup();
    popup.dismiss();
    settle(0.06);
    popup.show();
    row(&popup, "brightness").scale().set_value(25.0);
    popup.dismiss();
    settle(0.25);
    assert!(f.client.history().is_empty());
    println!("ok dismiss_racing_asynchronous_show_still_cancels_pending_slider");
}

fn loading_state_never_displays_fake_sensitive_zero_values() {
    let f = Fx::new();
    f.client.ready.set(false);
    let popup = f.popup();
    let brightness = row(&popup, "brightness");
    assert!(!brightness.scale().is_sensitive());
    assert_eq!(brightness.value_label().text(), "—");
    assert_eq!(brightness.hint().text(), "Loading display controls…");
    assert!(!popup.auto_dim.toggle.is_sensitive());
    assert!(f.client.history().is_empty());
    println!("ok loading_state_never_displays_fake_sensitive_zero_values");
}

fn display_scaling_is_read_only_on_open_and_independent_of_hardware_controls() {
    let f = Fx::new();
    let popup = f.popup();
    assert_eq!(popup.window.title().as_deref(), Some("Displays"));
    let scale = row(&popup, "scale");
    let unsupported_hardware = popup.controls.row("HDMI-A-1", "ugreen-id", "scale").unwrap();
    for _ in 0..3 {
        popup.refresh();
    }
    assert!(f.client.history().is_empty());
    assert_eq!(scale.value_label().text(), "125%");
    assert!(unsupported_hardware.scale().is_sensitive());
    let adjustment = scale.scale().adjustment();
    assert_eq!((adjustment.lower(), adjustment.upper(), adjustment.step_increment()), (1.0, 2.5, 0.125));
    unsupported_hardware.scale().set_value(1.125);
    settle(0.25);
    assert_eq!(f.client.history(), vec![Call::Scale("HDMI-A-1".into(), 1.125, Some("ugreen-scale".into()))]);
    assert_eq!(f.client.state()["scaling"]["auto_scale"], true, "A manual monitor override keeps global automatic scaling");
    assert_eq!(unsupported_hardware.value_label().text(), "112.5%");
    popup.auto_scale.toggle.set_active(false);
    assert_eq!(f.client.history().last(), Some(&Call::AutoScale(false)));
    println!("ok display_scaling_is_read_only_on_open_and_independent_of_hardware_controls");
}

fn scaling_service_unavailable_busy_and_hotplug_cannot_write_stale_display() {
    let f = Fx::new();
    let popup = f.popup();
    let scale = row(&popup, "scale");
    f.client.set(|s| { s["scaling"]["ready"] = false.into(); s["scaling"]["error"] = "tessel-displays is unavailable".into(); });
    popup.refresh();
    assert!(!scale.scale().is_sensitive());
    assert!(!popup.auto_scale.toggle.is_sensitive());
    assert!(scale.hint().text().contains("tessel-displays"));
    assert!(row(&popup, "brightness").scale().is_sensitive());
    scale.scale().set_value(1.5);
    settle(0.25);
    assert!(f.client.history().is_empty());
    f.client.set(|s| { s["scaling"]["ready"] = true.into(); s["scaling"]["busy"] = true.into(); s["scaling"]["error"] = "".into(); });
    popup.refresh();
    scale.scale().set_value(1.5);
    settle(0.25);
    assert!(f.client.history().is_empty());
    f.client.set(|s| s["scaling"]["busy"] = false.into());
    popup.refresh();
    scale.scale().set_value(1.5);
    f.client.set(|s| s["displays"][0]["scale_id"] = "replacement-scale-id".into());
    settle(0.25);
    assert!(f.client.history().is_empty(), "A stale row must not apply its old choice to a replacement display");
    f.client.set(|s| s["displays"][0]["enabled"] = false.into());
    popup.refresh();
    assert!(!scale.scale().is_sensitive());
    println!("ok scaling_service_unavailable_busy_and_hotplug_cannot_write_stale_display");
}

fn submitted_scale_waits_for_reply_and_failure_restores_observed_value() {
    let f = Fx::new();
    let popup = f.popup();
    let scale = row(&popup, "scale");
    f.client.commit.set(false);
    scale.scale().set_value(1.5);
    settle(0.25);
    popup.refresh();
    assert_eq!(scale.scale().value(), 1.5);
    assert!(!scale.scale().is_sensitive());
    f.client.set(|s| s["scaling"]["busy"] = true.into());
    popup.refresh();
    f.client.set(|s| { s["scaling"]["busy"] = false.into(); s["scaling"]["error"] = "The scale could not be saved".into(); });
    popup.refresh();
    assert_eq!(scale.scale().value(), 1.25);
    assert!(scale.scale().is_sensitive());
    assert!(scale.hint().text().contains("could not be saved"));
    f.client.set(|s| { s["scaling"]["error"] = "".into(); s["displays"][0]["scale"]["value"] = json!(1.333); });
    popup.refresh();
    assert_eq!(scale.value_label().text(), "133.3%");
    assert!((scale.scale().value() - 1.333).abs() < 1e-9);
    f.client.set(|s| s["displays"][0]["scale"]["value"] = json!(3.0));
    popup.refresh();
    assert_eq!(scale.value_label().text(), "300%");
    assert_eq!(scale.scale().value(), 3.0);
    assert_eq!(f.client.history(), vec![Call::Scale("DP-2".into(), 1.5, Some("dell-scale".into()))]);
    println!("ok submitted_scale_waits_for_reply_and_failure_restores_observed_value");
}

fn scaling_drag_waits_for_release_and_keyboard_changes_are_coalesced() {
    let f = Fx::new();
    let popup = f.popup();
    let scale = row(&popup, "scale");
    scale.pressed();
    scale.scale().set_value(1.51);
    assert_eq!(scale.value_label().text(), "150%");
    settle(0.25);
    scale.scale().set_value(1.89);
    popup.refresh();
    settle(0.25);
    assert!(f.client.history().is_empty(), "Dragging must not rescale the display mid-gesture");
    assert_eq!(scale.value_label().text(), "187.5%");
    scale.released();
    settle(0.25);
    assert_eq!(f.client.history(), vec![Call::Scale("DP-2".into(), 1.875, Some("dell-scale".into()))]);
    for v in [2.0, 2.125, 2.25] {
        scale.scale().set_value(v);
    }
    settle(0.25);
    assert_eq!(f.client.history().last(), Some(&Call::Scale("DP-2".into(), 2.25, Some("dell-scale".into()))));
    assert_eq!(f.client.history().len(), 2);
    println!("ok scaling_drag_waits_for_release_and_keyboard_changes_are_coalesced");
}

fn dismiss_and_busy_cancel_unsent_scaling_changes() {
    let f = Fx::new();
    let popup = f.popup();
    let scale = row(&popup, "scale");
    scale.scale().set_value(1.5);
    popup.dismiss();
    settle(0.25);
    assert!(f.client.history().is_empty());
    popup.show();
    scale.scale().set_value(1.5);
    f.client.set(|s| s["scaling"]["busy"] = true.into());
    settle(0.25);
    assert!(f.client.history().is_empty());
    assert_eq!(scale.scale().value(), 1.25);
    assert!(!scale.scale().is_sensitive());
    println!("ok dismiss_and_busy_cancel_unsent_scaling_changes");
}

fn auto_dim_switch_is_immediate_and_shows_dimmed_state() {
    let f = Fx::new();
    let popup = f.popup();
    popup.auto_dim.toggle.set_active(true);
    assert_eq!(f.client.history(), vec![Call::AutoDim(true)]);
    f.client.set(|s| s["dimmed"] = true.into());
    popup.refresh();
    assert_eq!(popup.auto_dim.hint.text(), "Displays are dimmed. Move the pointer to restore.");
    f.client.error.replace("[Errno 13] Permission denied: '/x/y'".into());
    popup.refresh();
    assert_eq!(popup.status_label.text(), "Some display settings are unavailable. See the log for details.");
    println!("ok auto_dim_switch_is_immediate_and_shows_dimmed_state");
}

fn css_parses_without_warnings() {
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(Cell::new(0));
    let e = errors.clone();
    provider.connect_parsing_error(move |_, _, error| { eprintln!("css: {error}"); e.set(e.get() + 1); });
    provider.load_from_string(STYLE_CSS);
    assert_eq!(errors.get(), 0);
    println!("ok css_parses_without_warnings");
}

/// Synchronous fake hardware for the keyboard/OSD check (one display).
struct Synthetic {
    displays: RefCell<Vec<Value>>,
}

impl Hardware for Synthetic {
    fn ready(&self) -> bool { true }
    fn busy(&self) -> bool { false }
    fn error(&self) -> String { String::new() }
    fn displays(&self) -> Vec<Value> { self.displays.borrow().clone() }
    fn start(&self) {}
    fn close(&self) {}
    fn refresh(&self) {}
    fn read_feature(&self, _: &str, _: &str, callback: Option<Callback>, _: Option<&str>) -> bool {
        glib::idle_add_local_once(move || model::call(callback, true, ""));
        true
    }
    fn set_raw(&self, _: &str, feature: &str, raw: i64, callback: Option<Callback>, _: Option<&str>, _: Option<i64>) -> bool {
        {
            let mut displays = self.displays.borrow_mut();
            displays[0][feature]["raw"] = raw.into();
            displays[0][feature]["value"] = json!(raw as f64);
        }
        glib::idle_add_local_once(move || model::call(callback, true, ""));
        true
    }
    fn set_value(&self, connector: &str, feature: &str, percent: f64, callback: Option<Callback>, id: Option<&str>) -> bool {
        self.set_raw(connector, feature, percent.round() as i64, callback, id, None)
    }
}

fn find_label(widget: &gtk::Widget, text: &str) -> bool {
    if widget.downcast_ref::<gtk::Label>().is_some_and(|l| l.text() == text) {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if find_label(&current, text) {
            return true;
        }
        child = current.next_sibling();
    }
    false
}

fn keyboard_brightness_uses_the_shared_osd_without_focus() {
    let directory = std::env::temp_dir().join(format!("bm-ui-{}", std::process::id()));
    let feature = json!({"supported": true, "raw": 75, "max": 100, "value": 75.0, "busy": false, "error": ""});
    let hardware = Rc::new(Synthetic { displays: RefCell::new(vec![json!({"connector": "DP-TEST", "id": "synthetic", "enabled": true,
        "name": "Synthetic display", "method": "ddc", "brightness": feature, "contrast": feature})]) });
    let core = BrightnessCore::new(hardware.clone(), || {}, Some(&directory), Some(&directory));
    let osd = AdjustmentOsd::new(None, 1500);
    let keys = BrightnessKeys::new(core, Rc::new(GtkOsd(osd.clone())), None);
    assert!(keys.adjust(10.0));
    assert!(wait_until(|| hardware.displays()[0]["brightness"]["raw"] == 85 && osd.window().is_some_and(|w| w.is_mapped()), 3.0),
            "keyboard brightness and centered indicator");
    let window = osd.window().unwrap();
    assert!(wait_until(|| find_label(window.upcast_ref(), "85%"), 2.0));
    assert_eq!(window.keyboard_mode(), gtk4_layer_shell::KeyboardMode::None, "The indicator must not steal keyboard focus");
    assert_eq!(window.namespace().as_deref(), Some("desktop-adjustment-osd"));
    assert!(keys.adjust(-10.0));
    assert!(wait_until(|| hardware.displays()[0]["brightness"]["raw"] == 75 && find_label(window.upcast_ref(), "75%"), 3.0));
    keys.close();
    osd.close();
    let _ = std::fs::remove_dir_all(&directory);
    println!("ok keyboard_brightness_uses_the_shared_osd_without_focus");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var_os("BRIGHTNESS_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/brightness/tests/run_headless.sh for the private compositor)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("display");
    gtk::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
    refresh_is_read_only_and_shows_actual_values_and_unsupported_controls();
    hdr_locked_control_is_disabled_with_its_value_and_a_note();
    debounce_coalesces_gesture_and_other_feature_refresh_does_not_reset_it();
    busy_feature_keeps_latest_pending_value_and_remains_interactive();
    submitted_value_survives_unrelated_refresh_until_hardware_reply();
    replaced_display_cannot_receive_old_debounced_command_even_without_refresh();
    dismiss_destroy_and_gear_cancel_pending_slider_changes();
    dismiss_racing_asynchronous_show_still_cancels_pending_slider();
    loading_state_never_displays_fake_sensitive_zero_values();
    display_scaling_is_read_only_on_open_and_independent_of_hardware_controls();
    scaling_service_unavailable_busy_and_hotplug_cannot_write_stale_display();
    submitted_scale_waits_for_reply_and_failure_restores_observed_value();
    scaling_drag_waits_for_release_and_keyboard_changes_are_coalesced();
    dismiss_and_busy_cancel_unsent_scaling_changes();
    auto_dim_switch_is_immediate_and_shows_dimmed_state();
    css_parses_without_warnings();
    keyboard_brightness_uses_the_shared_osd_without_focus();
    println!("ui_headless: all checks passed");
}
