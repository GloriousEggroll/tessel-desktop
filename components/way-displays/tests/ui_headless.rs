#![allow(deprecated)]
//! Port of test_brightness_integration.py (BrightnessIntegrationTests) and
//! test_monitor_overlay.py with a fake brightness client. Runs only inside the
//! private nested headless labwc (tests/run_headless.sh); under plain
//! `cargo test` it skips itself. No DDC, backlight, real D-Bus service or
//! way-displays socket is used: every daemon call is a recording fake.

use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use way_displays::brightness::BrightnessControls;
use way_displays::color_config;
use way_displays::compat::brightness_client::BrightnessModel;
use way_displays::ipc;
use way_displays::labwc::HdrConfig;
use way_displays::overlay::MonitorOverlay;
use way_displays::window::{self, Hooks, WayDisplaysApp};

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

fn wait(mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        settle(0.02);
        if predicate() {
            return true;
        }
    }
    false
}

fn display(connector: &str, value: i64, contrast: bool) -> Value {
    json!({"connector": connector, "id": format!("fixture-{connector}"), "enabled": true, "error": "",
           "brightness": {"supported": true, "value": value, "busy": false, "error": ""},
           "contrast": {"supported": contrast, "value": if contrast { json!(50) } else { Value::Null }, "busy": false, "error": ""}})
}

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Value(String, String, i64),
    Auto(bool),
    Settings(Value),
}

struct FakeClient {
    on_changed: RefCell<Rc<dyn Fn()>>,
    ready: Cell<bool>,
    error: RefCell<String>,
    state: RefCell<Value>,
    calls: RefCell<Vec<Call>>,
    started: Cell<bool>,
    closed: Cell<bool>,
    reject: Cell<bool>,
    /// test_local_policy_value…: SetAutoDim stays in flight (busy).
    pending_auto: Cell<bool>,
}

impl FakeClient {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            on_changed: RefCell::new(Rc::new(|| {})), ready: Cell::new(true), error: RefCell::new(String::new()),
            state: RefCell::new(json!({"displays": [display("DP-1", 70, true), display("HDMI-A-1", 40, false)],
                "settings": {"version": 1, "auto_dim": false, "dim_after": 300, "dim_percent": 30, "preserve_this": "existing setting"}})),
            calls: RefCell::new(Vec::new()), started: Cell::new(false), closed: Cell::new(false), reject: Cell::new(false),
            pending_auto: Cell::new(false),
        })
    }

    fn changed(&self) {
        let callback = self.on_changed.borrow().clone();
        callback();
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    fn set(&self, pointer: &str, value: Value) {
        *self.state.borrow_mut().pointer_mut(pointer).unwrap() = value;
    }
}

impl BrightnessModel for FakeClient {
    fn state(&self) -> Value { self.state.borrow().clone() }
    fn ready(&self) -> bool { self.ready.get() }
    fn error(&self) -> String { self.error.borrow().clone() }

    fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: Option<&Value>) -> Result<bool, String> {
        self.calls.borrow_mut().push(Call::Value(connector.into(), feature.into(), value as i64));
        if self.reject.get() {
            self.error.replace("Monitor control failed.".into());
            self.changed();
            return Ok(false);
        }
        {
            let mut state = self.state.borrow_mut();
            let target = state["displays"].as_array_mut().unwrap().iter_mut().find(|d| d["connector"] == connector).unwrap();
            assert_eq!(expected_id, Some(&target["id"]), "A write did not capture the expected display identity");
            target[feature]["value"] = json!(value as i64);
        }
        self.changed();
        Ok(true)
    }

    fn set_auto_dim(&self, enabled: bool) -> Result<bool, String> {
        self.calls.borrow_mut().push(Call::Auto(enabled));
        if self.pending_auto.get() {
            self.state.borrow_mut()["busy"] = json!(true);
        } else {
            self.state.borrow_mut()["settings"]["auto_dim"] = json!(enabled);
        }
        self.changed();
        Ok(true)
    }

    fn set_settings(&self, settings: &Value) -> Result<bool, String> {
        self.calls.borrow_mut().push(Call::Settings(settings.clone()));
        self.state.borrow_mut()["settings"] = settings.clone();
        self.changed();
        Ok(true)
    }

    fn start(&self) -> Result<(), String> {
        self.started.set(true);
        self.changed();
        Ok(())
    }

    fn close(&self) {
        self.closed.set(true);
    }
}

fn monitor(connector: &str, width: i64) -> Value {
    let mode = json!({"WIDTH": width, "HEIGHT": 1080, "REFRESH_MHZ": 60000});
    json!({"NAME": connector, "DESCRIPTION": format!("Same model {connector}"), "MAKE": "Example", "MODEL": "Display",
           "MODES": [mode.clone()], "CURRENT": {"MODE": mode, "ENABLED": true, "SCALE": 1.0, "VRR": false, "TRANSFORM": "NORMAL"}})
}

fn test_hooks() -> Hooks {
    Hooks {
        ipc_operation: Rc::new(|op, _| panic!("No display daemon calls in UI tests ({op})")),
        ipc_config_operation: Rc::new(|op, _| panic!("No display daemon calls in UI tests ({op})")),
        get_config_data: Rc::new(|| panic!("No display daemon calls in UI tests (GET)")),
        get_xwayland_primary_output: Rc::new(|| None),
        apply_xwayland_primary_output: Rc::new(|_| (true, None)),
        read_labwc_primary_environment: Rc::new(|_| None),
        write_labwc_primary_environment: Rc::new(|_, _| panic!("unexpected environment write")),
        write_labwc_primary_xinitrc: Rc::new(|_, _| panic!("unexpected xinitrc write")),
        read_labwc_hdr_config: Rc::new(|_| HdrConfig { global: false, overrides: BTreeMap::new(), managed: BTreeMap::new(), text: String::new() }),
        write_labwc_hdr_overrides: Rc::new(|_, _| panic!("unexpected rc.xml write")),
        reconfigure_labwc: Rc::new(|| (true, None)),
    }
}

/// Record daemon calls; `on` sees each `(operation, cfg)`.
fn recording(hooks: &mut Hooks, operations: Rc<RefCell<Vec<(String, Value)>>>, writes: Rc<RefCell<Vec<String>>>) {
    let ops = operations.clone();
    hooks.ipc_config_operation = Rc::new(move |op, cfg| {
        ops.borrow_mut().push((op.to_owned(), cfg.clone()));
        Ok(String::new())
    });
    hooks.ipc_operation = Rc::new(move |op, _| {
        writes.borrow_mut().push(op.to_owned());
        Ok(String::new())
    });
    hooks.get_config_data = Rc::new(|| false);
    hooks.write_labwc_primary_environment = Rc::new(|_, _| (true, None, false));
    hooks.write_labwc_primary_xinitrc = Rc::new(|_, _| (true, None, false));
}

struct Fx {
    app: gtk::Application,
    client: Rc<FakeClient>,
    widgets: Rc<RefCell<Vec<Rc<BrightnessControls>>>>,
    windows: RefCell<Vec<Rc<WayDisplaysApp>>>,
}

impl Fx {
    fn new(app: &gtk::Application) -> Self {
        window::set_default_hooks(Some(test_hooks()));
        color_config::set_status_override(None);
        let client = FakeClient::new();
        let widgets: Rc<RefCell<Vec<Rc<BrightnessControls>>>> = Rc::default();
        let list = widgets.clone();
        client.on_changed.replace(Rc::new(move || {
            let widgets = list.borrow().clone();
            for widget in widgets {
                widget.refresh();
            }
        }));
        Self { app: app.clone(), client, widgets, windows: RefCell::new(Vec::new()) }
    }

    fn refresh(&self) {
        self.client.changed();
    }

    fn controls(&self, connector: &str) -> Rc<BrightnessControls> {
        let controls = BrightnessControls::new(connector, Some(self.client.clone() as Rc<dyn BrightnessModel>), "");
        self.widgets.borrow_mut().push(controls.clone());
        controls
    }

    fn window(&self) -> Rc<WayDisplaysApp> {
        self.window_with(json!({"CFG": {"AUTO_SCALE": false, "CUSTOM_KEEP": {"untouched": true}},
                                "STATE": {"HEADS": [monitor("DP-1", 1920), monitor("HDMI-A-1", 1600)]}}))
    }

    fn window_with(&self, data: Value) -> Rc<WayDisplaysApp> {
        ipc::set_config_data(data);
        let client = self.client.clone();
        let factory: window::ClientFactory = Box::new(move |on_changed| {
            client.on_changed.replace(on_changed);
            Ok(client as Rc<dyn BrightnessModel>)
        });
        let window = WayDisplaysApp::new(&self.app, Some(factory), false);
        self.windows.borrow_mut().push(window.clone());
        window.window.present();
        settle(0.06);
        window
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for widget in self.widgets.borrow().iter() {
            widget.close();
        }
        for window in self.windows.borrow().iter() {
            window.close_brightness();
            window.window.destroy();
        }
        settle(0.06);
        color_config::set_status_override(None);
    }
}

fn connector_matching_refresh_and_unsupported_contrast(app: &gtk::Application) {
    let f = Fx::new(app);
    let (primary, secondary) = (f.controls("DP-1"), f.controls("HDMI-A-1"));
    for _ in 0..3 {
        f.refresh();
    }
    assert_eq!(primary.scales["brightness"].value(), 70.0);
    assert_eq!(secondary.scales["brightness"].value(), 40.0);
    assert!(primary.scales["contrast"].is_sensitive());
    assert!(!secondary.scales["contrast"].is_sensitive());
    assert_eq!(secondary.value_labels["contrast"].text(), "Unavailable");
    assert!(f.client.calls().is_empty());
    println!("ok connector_matching_refresh_and_unsupported_contrast");
}

fn apply_runs_primary_command_and_updates_startup_script(app: &gtk::Application) {
    let f = Fx::new(app);
    let window = f.window();
    let folder = PathBuf::from(std::env::var("TMPDIR").unwrap()).join(format!("apply-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let applied = Rc::new(RefCell::new(Vec::<Option<String>>::new()));
    let writes = Rc::new(RefCell::new(Vec::new()));
    {
        let mut hooks = window.hooks.borrow_mut();
        recording(&mut hooks, Rc::default(), writes.clone());
        hooks.get_config_data = Rc::new(|| true);
        hooks.write_labwc_primary_environment = Rc::new(way_displays::primary::write_labwc_primary_environment);
        hooks.write_labwc_primary_xinitrc = Rc::new(way_displays::primary::write_labwc_primary_xinitrc);
        let log = applied.clone();
        hooks.apply_xwayland_primary_output = Rc::new(move |o| {
            log.borrow_mut().push(o.map(String::from));
            (true, None)
        });
    }
    window.labwc_environment_path.replace(folder.join("environment"));
    let path = folder.join("xinitrc");
    std::fs::write(&path, "#!/bin/sh\nxrandr --output OLD --primary\ntrue\n").unwrap();
    let target = window.monitor_widgets.borrow().iter().find(|w| w.name().as_deref() == Some("HDMI-A-1")).unwrap().clone();
    target.primary_switch.set_active(true);
    window.save_btn.emit_clicked();
    assert_eq!(applied.borrow().last().cloned().flatten().as_deref(), Some("HDMI-A-1"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("xrandr --output HDMI-A-1 --primary"));
    assert!(!text.contains("--output OLD"));
    assert!(text.ends_with("true\n"));
    assert_eq!(writes.borrow().last().map(String::as_str), Some("CFG_WRITE"));
    let count = applied.borrow().len();
    settle(0.65); // Drain the normal layout refresh and XRandR retry.
    assert_eq!(applied.borrow().len(), count + 1);
    assert_eq!(applied.borrow().last().cloned().flatten().as_deref(), Some("HDMI-A-1"));
    let _ = std::fs::remove_dir_all(&folder);
    println!("ok apply_runs_primary_command_and_updates_startup_script");
}

fn slider_coalesces_explicit_input_on_exact_display(app: &gtk::Application) {
    let f = Fx::new(app);
    let (first, second) = (f.controls("DP-1"), f.controls("HDMI-A-1"));
    first.scales["brightness"].set_value(60.0);
    first.scales["brightness"].set_value(61.0);
    f.refresh();
    assert_eq!(first.scales["brightness"].value(), 61.0);
    assert!(f.client.calls().is_empty());
    settle(0.23);
    assert_eq!(f.client.calls(), [Call::Value("DP-1".into(), "brightness".into(), 61)]);
    assert_eq!(second.scales["brightness"].value(), 40.0);
    println!("ok slider_coalesces_explicit_input_on_exact_display");
}

fn pending_slider_is_canceled_on_service_loss_busy_or_close(app: &gtk::Application) {
    let f = Fx::new(app);
    let controls = f.controls("DP-1");
    controls.scales["brightness"].set_value(61.0);
    f.client.ready.set(false);
    f.refresh();
    settle(0.23);
    assert!(f.client.calls().is_empty());
    assert!(controls.status_label.text().contains("unavailable"));
    f.client.ready.set(true);
    f.client.set("/displays/0/brightness/busy", json!(true));
    f.refresh();
    controls.scales["brightness"].set_value(62.0);
    settle(0.23);
    assert!(f.client.calls().is_empty());
    f.client.set("/displays/0/brightness/busy", json!(false));
    f.refresh();
    controls.scales["brightness"].set_value(63.0);
    controls.close();
    settle(0.23);
    assert!(f.client.calls().is_empty());
    println!("ok pending_slider_is_canceled_on_service_loss_busy_or_close");
}

fn missing_monitor_and_write_failure_do_not_show_fake_success(app: &gtk::Application) {
    let f = Fx::new(app);
    let missing = f.controls("DP-9");
    assert!(!missing.scales["brightness"].is_sensitive());
    assert!(missing.status_label.text().contains("No brightness controls"));
    let controls = f.controls("DP-1");
    f.client.reject.set(true);
    controls.scales["brightness"].set_value(65.0);
    settle(0.23);
    assert_eq!(controls.scales["brightness"].value(), 70.0);
    assert!(controls.status_label.text().contains("Monitor control failed"));
    println!("ok missing_monitor_and_write_failure_do_not_show_fake_success");
}

fn hotplug_identity_change_cancels_value_for_previous_display(app: &gtk::Application) {
    let f = Fx::new(app);
    let controls = f.controls("DP-1");
    controls.scales["brightness"].set_value(65.0);
    f.client.set("/displays/0/id", json!("replacement-same-connector"));
    f.refresh();
    settle(0.23);
    assert!(f.client.calls().is_empty());
    assert_eq!(controls.scales["brightness"].value(), 70.0);
    println!("ok hotplug_identity_change_cancels_value_for_previous_display");
}

fn busy_feature_defers_existing_pending_value_without_losing_it(app: &gtk::Application) {
    let f = Fx::new(app);
    let controls = f.controls("DP-1");
    controls.scales["brightness"].set_value(65.0);
    f.client.set("/displays/0/brightness/busy", json!(true));
    f.refresh();
    settle(0.23);
    assert!(f.client.calls().is_empty());
    assert_eq!(controls.scales["brightness"].value(), 65.0);
    f.client.set("/displays/0/brightness/busy", json!(false));
    f.refresh();
    settle(0.06);
    assert_eq!(f.client.calls(), [Call::Value("DP-1".into(), "brightness".into(), 65)]);
    println!("ok busy_feature_defers_existing_pending_value_without_losing_it");
}

fn loading_feature_is_distinct_from_unsupported_feature(app: &gtk::Application) {
    let f = Fx::new(app);
    f.client.set("/displays/0/brightness/supported", Value::Null);
    f.client.set("/displays/0/brightness/value", Value::Null);
    let controls = f.controls("DP-1");
    assert_eq!(controls.value_labels["brightness"].text(), "Checking…");
    assert!(!controls.scales["brightness"].is_sensitive());
    println!("ok loading_feature_is_distinct_from_unsupported_feature");
}

fn shared_auto_dim_settings_refresh_all_tabs_without_extra_writes(app: &gtk::Application) {
    let f = Fx::new(app);
    let (first, second) = (f.controls("DP-1"), f.controls("HDMI-A-1"));
    first.auto_switch.set_active(true);
    assert!(second.auto_switch.is_active());
    assert_eq!(f.client.calls(), [Call::Auto(true)]);
    let index = second.delay_values().iter().position(|v| *v == 600).unwrap();
    second.delay_choice.set_selected(index as u32);
    assert_eq!(first.delay_values()[first.delay_choice.selected() as usize], 600);
    assert_eq!(f.client.state()["settings"]["preserve_this"], json!("existing setting"));
    first.dim_level.set_value(25.0);
    assert_eq!(second.dim_level.value_as_int(), 25);
    assert_eq!(f.client.calls().len(), 3);
    f.refresh();
    assert_eq!(f.client.calls().len(), 3);
    println!("ok shared_auto_dim_settings_refresh_all_tabs_without_extra_writes");
}

fn busy_policy_does_not_overwrite_in_flight_settings(app: &gtk::Application) {
    let f = Fx::new(app);
    let controls = f.controls("DP-1");
    f.client.state.borrow_mut()["busy"] = json!(true);
    f.refresh();
    assert!(!controls.auto_switch.is_sensitive());
    assert!(!controls.dim_row.is_sensitive());
    controls.auto_switch.set_active(true);
    controls.dim_level.set_value(25.0);
    assert!(f.client.calls().is_empty());
    assert!(!controls.auto_switch.is_active());
    assert_eq!(controls.dim_level.value_as_int(), 30);
    println!("ok busy_policy_does_not_overwrite_in_flight_settings");
}

fn local_policy_value_waits_for_async_reply_and_reverts_on_failure(app: &gtk::Application) {
    let f = Fx::new(app);
    let controls = f.controls("DP-1");
    f.client.pending_auto.set(true);
    controls.auto_switch.set_active(true);
    assert!(controls.auto_switch.is_active());
    assert!(!controls.auto_switch.is_sensitive());
    assert_eq!(f.client.state()["settings"]["auto_dim"], json!(false));
    f.client.error.replace("Settings were not saved.".into());
    f.client.state.borrow_mut()["busy"] = json!(false);
    f.refresh();
    assert!(!controls.auto_switch.is_active());
    assert!(controls.status_label.text().contains("Settings were not saved"));
    println!("ok local_policy_value_waits_for_async_reply_and_reverts_on_failure");
}

fn status(entries: &[(&str, &[(&str, &str)])]) -> color_config::Status {
    entries.iter().map(|(section, values)| (section.to_string(),
        values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())).collect()
}

fn color_changes_are_pending_and_shared_with_the_embedded_editor(app: &gtk::Application) {
    let f = Fx::new(app);
    let available = status(&[("compositor", &[("icc_supported", "true")]),
                             ("DP-1", &[("icc_active", "false"), ("bit_depth", "8"), ("hdr", "false")])]);
    color_config::set_status_override(Some(Rc::new(move || available.clone())));
    let window = f.window();
    let controls = window.monitor_widgets.borrow()[0].color_controls.clone();
    assert!(controls.depth.is_sensitive());
    assert!(!controls.changed());
    assert!(!window.save_btn.is_sensitive());
    controls.depth.set_selected(1);
    assert!(controls.changed());
    assert_eq!(controls.get_state().sdr_bit_depth, 10);
    assert!(window.save_btn.is_sensitive());
    assert!(controls.status.text().contains("pending until Apply"));
    assert!(controls.status.text().contains("Active output depth: 8-bit"));
    assert!(f.client.calls().is_empty());
    println!("ok color_changes_are_pending_and_shared_with_the_embedded_editor");
}

fn color_controls_explain_an_unsupported_compositor(app: &gtk::Application) {
    let f = Fx::new(app);
    color_config::set_status_override(Some(Rc::new(BTreeMap::new)));
    let window = f.window();
    let controls = window.monitor_widgets.borrow()[0].color_controls.clone();
    assert!(!controls.browse.is_sensitive());
    assert!(!controls.depth.is_sensitive());
    assert!(controls.status.text().contains("updated labwc"));
    assert!(!window.save_btn.is_sensitive());
    println!("ok color_controls_explain_an_unsupported_compositor");
}

fn hdr_report_distinguishes_metadata_white_target_and_measurement(app: &gtk::Application) {
    let f = Fx::new(app);
    let active: Rc<RefCell<BTreeMap<String, String>>> = Rc::new(RefCell::new(
        [("hdr", "true"), ("enabled", "true"), ("hdr_supported", "true"), ("hdr_peak_nits", "426.9"),
         ("hdr_max_frame_average_nits", "265"), ("hdr_reference_white_nits", "203")]
            .iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()));
    let source = active.clone();
    color_config::set_status_override(Some(Rc::new(move || {
        let mut status = status(&[("compositor", &[("icc_supported", "true")])]);
        status.insert("DP-1".into(), source.borrow().clone());
        status
    })));
    let window = f.window();
    let controls = window.monitor_widgets.borrow()[0].color_controls.clone();
    assert!(controls.hdr_box.is_visible());
    let rows: BTreeMap<_, _> = controls.hdr_rows().into_iter().collect();
    assert_eq!(rows["Peak brightness"], "426.9 nits");
    assert_eq!(rows["Frame-average limit"], "265 nits");
    assert_eq!(rows["Reference white"], "203 nits");
    assert_eq!(rows["Output correction"], "None");
    active.borrow_mut().insert("hdr_peak_nits".into(), "nan".into());
    active.borrow_mut().insert("hdr_reference_white_nits".into(), "inf".into());
    controls.refresh();
    let rows: BTreeMap<_, _> = controls.hdr_rows().into_iter().collect();
    assert_eq!((rows["Peak brightness"].as_str(), rows["Reference white"].as_str()), ("Unavailable", "Unavailable"));
    active.borrow_mut().insert("hdr".into(), "false".into());
    active.borrow_mut().insert("hdr_reference_white_nits".into(), "203".into());
    controls.refresh();
    assert!(!controls.hdr_rows().iter().any(|(k, _)| k == "Reference white"));
    assert!(controls.hdr_box.is_visible());
    println!("ok hdr_report_distinguishes_metadata_white_target_and_measurement");
}

fn vrr_changes_in_two_stale_editors_preserve_the_other_monitor(app: &gtk::Application) {
    let f = Fx::new(app);
    let (first, second) = (f.window(), f.window());
    let descriptions: Vec<String> = first.monitor_widgets.borrow().iter().map(|w| w.desc()).collect();
    let disabled: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(descriptions.clone()));
    // Both windows opened before either monitor's VRR was enabled.
    ipc::with_config_data(|data| data["CFG"]["VRR_OFF"] = json!(descriptions));
    let operations: Rc<RefCell<Vec<(String, Value)>>> = Rc::default();
    for window in [&first, &second] {
        let mut hooks = window.hooks.borrow_mut();
        recording(&mut hooks, operations.clone(), Rc::default());
        let (ops, set) = (operations.clone(), disabled.clone());
        hooks.ipc_config_operation = Rc::new(move |op, cfg| {
            ops.borrow_mut().push((op.to_owned(), cfg.clone()));
            let items: Vec<String> = cfg.get("VRR_OFF").and_then(Value::as_array).map(|a| a.iter().map(|v| v.as_str().unwrap().to_owned()).collect()).unwrap_or_default();
            let mut set = set.borrow_mut();
            if op == "CFG_DEL" {
                set.retain(|d| !items.contains(d));
            } else {
                for item in items {
                    if !set.contains(&item) {
                        set.push(item);
                    }
                }
            }
            Ok(String::new())
        });
    }
    first.monitor_widgets.borrow()[0].vrr_switch.set_active(true);
    first.save_config();
    assert_eq!(*disabled.borrow(), [descriptions[1].clone()]);
    second.monitor_widgets.borrow()[1].vrr_switch.set_active(true);
    second.save_config();
    assert!(disabled.borrow().is_empty());
    // A subsequent unrelated Apply must not re-send either VRR setting.
    operations.borrow_mut().clear();
    first.save_config();
    assert!(operations.borrow().iter().all(|(_, cfg)| cfg.get("VRR_OFF").is_none()));
    second.monitor_widgets.borrow()[1].vrr_switch.set_active(false);
    second.save_config();
    assert_eq!(*disabled.borrow(), [descriptions[1].clone()]);
    settle(0.65);
    println!("ok vrr_changes_in_two_stale_editors_preserve_the_other_monitor");
}

fn automatic_scaling_removes_only_that_displays_scale(app: &gtk::Application) {
    let f = Fx::new(app);
    let window = f.window();
    let (first_desc, second_desc) = ("Same model DP-1", "Same model HDMI-A-1");
    ipc::with_config_data(|data| {
        data["CFG"]["AUTO_SCALE"] = json!(true);
        data["CFG"]["SCALE"] = json!({first_desc: 1.5, second_desc: 1.25});
    });
    window.load_monitor_data();
    let (first, second) = {
        let widgets = window.monitor_widgets.borrow();
        (widgets[0].clone(), widgets[1].clone())
    };
    assert!(!first.scale_auto_switch.is_active());
    assert!(first.scale_slider.is_sensitive());
    let _ = second;
    let operations: Rc<RefCell<Vec<(String, Value)>>> = Rc::default();
    recording(&mut window.hooks.borrow_mut(), operations.clone(), Rc::default());
    first.scale_auto_switch.set_active(true);
    assert!(!first.scale_slider.is_sensitive());
    window.save_config();
    settle(0.65);
    let ops = operations.borrow();
    let deleted: Vec<&Value> = ops.iter().filter(|(o, _)| o == "CFG_DEL").map(|(_, c)| c).collect();
    let saved: Vec<&Value> = ops.iter().filter(|(o, _)| o == "CFG_SET").map(|(_, c)| c).collect();
    assert_eq!(deleted[0]["SCALE"], json!({first_desc: 1.5}));
    assert_eq!(saved[0]["AUTO_SCALE"], json!(true));
    assert_eq!(saved[0]["SCALE"], json!({second_desc: 1.25}));
    println!("ok automatic_scaling_removes_only_that_displays_scale");
}

fn vrr_uses_desired_state_when_runtime_is_temporarily_off(app: &gtk::Application) {
    let f = Fx::new(app);
    let window = f.window();
    ipc::with_config_data(|data| data["STATE"]["HEADS"][0]["DESIRED"] = json!({"VRR": true}));
    window.load_monitor_data();
    let widget = window.monitor_widgets.borrow()[0].clone();
    assert!(widget.vrr_switch.is_active());
    assert!(!widget.get_desired_state().unwrap().vrr_changed);
    println!("ok vrr_uses_desired_state_when_runtime_is_temporarily_off");
}

fn full_window_brightness_changes_preserve_apply_and_monitor_configuration(app: &gtk::Application) {
    let f = Fx::new(app);
    let window = f.window();
    let initial = ipc::config_data();
    let original: Vec<_> = window.monitor_widgets.borrow().iter().map(|w| w.get_desired_state()).collect();
    assert!(f.client.started.get());
    assert!(!window.save_btn.is_sensitive());
    let controls = window.monitor_widgets.borrow()[0].brightness_controls.clone();
    controls.scales["brightness"].set_value(61.0);
    controls.auto_switch.set_active(true);
    settle(0.23);
    assert!(!window.save_btn.is_sensitive());
    let now: Vec<_> = window.monitor_widgets.borrow().iter().map(|w| w.get_desired_state()).collect();
    assert_eq!(now, original);
    assert_eq!(ipc::config_data(), initial);
    window.monitor_widgets.borrow()[0].vrr_switch.set_active(true);
    assert!(window.save_btn.is_sensitive());
    f.client.set("/displays/0/brightness/value", json!(55));
    f.client.changed();
    assert!(window.monitor_widgets.borrow()[0].vrr_switch.is_active());
    assert!(window.save_btn.is_sensitive());
    println!("ok full_window_brightness_changes_preserve_apply_and_monitor_configuration");
}

fn window_follows_live_dock_palette_without_changing_display_settings(app: &gtk::Application) {
    use suite_gtk::theme::{attach, PaletteClient};
    let f = Fx::new(app);
    let window = f.window();
    let original = ipc::config_data();
    let display = WidgetExt::display(&window.window);
    let output = display.monitor_at_surface(&window.window.surface().unwrap()).unwrap();
    let area = output.geometry();
    let directory = PathBuf::from(std::env::var("TMPDIR").unwrap()).join(format!("palette-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("palette.json");
    let mut data = json!({"version": 1, "is_dark": true, "monitors": [
        {"name": output.connector().unwrap().to_string(), "geometry": [area.x(), area.y(), area.width(), area.height()],
         "background": [40, 25, 60], "foreground": [201, 211, 221], "accent": [105, 167, 255]}]});
    std::fs::write(&path, data.to_string()).unwrap();
    if let Some(palette) = window.palette.take() {
        palette.close();
    }
    let client = PaletteClient::new(Some(path.clone()));
    let binding = attach(&window.window, Some(&client));
    window.palette.replace(Some(binding.clone()));
    settle(0.06);
    assert!(window.window.has_css_class("way-displays"));
    assert_eq!(binding.monitor_name().as_deref(), Some(output.connector().unwrap().as_str()));
    let rgb = |c: gtk::gdk::RGBA| ((c.red() * 255.0).round() as i32, (c.green() * 255.0).round() as i32, (c.blue() * 255.0).round() as i32);
    assert_eq!(rgb(window.window.color()), (201, 211, 221));
    data["monitors"][0]["foreground"] = json!([30, 40, 50]);
    let replacement = path.with_extension("new");
    std::fs::write(&replacement, data.to_string()).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    settle(0.25);
    assert_eq!(rgb(window.window.color()), (30, 40, 50));
    assert_eq!(ipc::config_data(), original);
    assert!(f.client.calls().is_empty());
    binding.close();
    client.close();
    let _ = std::fs::remove_dir_all(&directory);
    println!("ok window_follows_live_dock_palette_without_changing_display_settings");
}

fn rebuilt_tabs_and_window_close_cancel_old_debounce_and_client(app: &gtk::Application) {
    let f = Fx::new(app);
    let window = f.window();
    let old = window.monitor_widgets.borrow()[0].brightness_controls.clone();
    old.scales["brightness"].set_value(61.0);
    window.load_monitor_data();
    settle(0.23);
    assert!(old.is_closed());
    assert!(f.client.calls().is_empty());
    let new = window.monitor_widgets.borrow()[0].brightness_controls.clone();
    new.scales["brightness"].set_value(62.0);
    window.window.close();
    settle(0.23);
    assert!(f.client.closed.get());
    assert!(f.client.calls().is_empty());
    println!("ok rebuilt_tabs_and_window_close_cancel_old_debounce_and_client");
}

fn window_contract(app: &gtk::Application) {
    let f = Fx::new(app);
    // The first head describes the first headless output, so it gets an
    // identify overlay (matched by `desc in GdkMonitor.description`).
    let output = gtk::gdk::Display::default().unwrap().monitors().item(0).and_downcast::<gtk::gdk::Monitor>().unwrap();
    let mut first = monitor("DP-1", 1920);
    first["DESCRIPTION"] = json!(output.description().map(|d| d.to_string()).unwrap_or_default());
    let window = f.window_with(json!({"CFG": {}, "STATE": {"HEADS": [first, monitor("HDMI-A-1", 1600)]}}));
    assert_eq!(window.window.title().as_deref(), Some("Displays"));
    assert!(window.window.has_css_class("way-displays"));
    assert_eq!(window.monitors_notebook.n_pages(), 2);
    let tab = window.monitors_notebook.tab_label(&window.monitors_notebook.nth_page(Some(0)).unwrap()).and_downcast::<gtk::Label>().unwrap();
    assert_eq!(tab.text(), "Example Display (DP-1)");
    assert!(window.save_btn.has_css_class("primary"));
    assert_eq!(window.save_btn.label().as_deref(), Some("Apply"));
    let layout = window.layout_area.borrow().clone().unwrap();
    let positions: Vec<_> = layout.entries().iter().map(|e| (e.desc.clone(), e.x, e.y)).collect();
    assert_eq!(positions.len(), 2);
    assert!(window.layout_frame.child().is_some());
    // Each output got its identify overlay (non-embedded windows).
    assert!(wait(|| window.overlay_count() == 1), "one overlay per matching headless output");
    // Rotation updates the shape and marks Apply.
    let widget = window.monitor_widgets.borrow()[0].clone();
    widget.rotation_buttons[1].1.set_active(true);
    assert!(!widget.rotation_buttons[0].1.is_active());
    assert_eq!(widget.get_transform().as_deref(), Some("90"));
    assert!(window.save_btn.is_sensitive());
    widget.flip_switch.set_active(true);
    assert_eq!(widget.get_transform().as_deref(), Some("FLIPPED-90"));
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = errors.clone();
    provider.connect_parsing_error(move |_, _, e| sink.borrow_mut().push(e.to_string()));
    provider.load_from_string(way_displays::style::CSS);
    provider.load_from_string(way_displays::overlay::OVERLAY_CSS);
    provider.load_from_string(way_displays::layout::LAYOUT_CSS);
    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    println!("ok window_contract");
}

// ------------------------------------------------- test_monitor_overlay.py

fn paint(window: &gtk::Window, color: (f64, f64, f64)) {
    let area = gtk::DrawingArea::new();
    area.set_draw_func(move |_, cr, _, _| {
        cr.set_source_rgb(color.0, color.1, color.2);
        let _ = cr.paint();
    });
    window.set_child(Some(&area));
}

fn reserved_panel(app: &gtk::Application, monitor: &gtk::gdk::Monitor, top: bool, thickness: i32) -> gtk::Window {
    let panel = gtk::Window::new();
    panel.set_application(Some(app));
    panel.init_layer_shell();
    panel.set_monitor(Some(monitor));
    panel.set_layer(Layer::Top);
    panel.set_keyboard_mode(KeyboardMode::None);
    panel.set_exclusive_zone(thickness);
    let anchors = if top { [Edge::Top, Edge::Left, Edge::Right] } else { [Edge::Top, Edge::Bottom, Edge::Left] };
    for edge in anchors {
        panel.set_anchor(edge, true);
    }
    if top { panel.set_size_request(1, thickness) } else { panel.set_size_request(thickness, 1) }
    paint(&panel, (0.0, 1.0, 0.0));
    panel.present();
    panel
}

/// `grim -t ppm -o <output> -` → (width, height, rgb bytes).
fn capture(connector: &str) -> (usize, usize, Vec<u8>) {
    let out = std::process::Command::new("grim").args(["-t", "ppm", "-o", connector, "-"]).output().expect("grim");
    assert!(out.status.success(), "grim failed: {}", String::from_utf8_lossy(&out.stderr));
    let data = out.stdout;
    let mut fields = Vec::new();
    let mut index = 0;
    while fields.len() < 4 {
        while data[index].is_ascii_whitespace() {
            index += 1;
        }
        let start = index;
        while !data[index].is_ascii_whitespace() {
            index += 1;
        }
        fields.push(String::from_utf8_lossy(&data[start..index]).into_owned());
    }
    index += 1;
    let (width, height) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    (width, height, data[index..].to_vec())
}

fn assert_indicator_at_corner(connector: &str) {
    let (width, height, pixels) = capture(connector);
    let at = |x: usize, y: usize| (pixels[(y * width + x) * 3], pixels[(y * width + x) * 3 + 1], pixels[(y * width + x) * 3 + 2]);
    let red: Vec<(usize, usize)> = (0..height).flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|(x, y)| { let (r, g, b) = at(*x, *y); r > 240 && g < 15 && b < 15 }).collect();
    assert!(!red.is_empty(), "Indicator disappeared behind another surface");
    assert_eq!((red.iter().map(|p| p.0).min().unwrap(), red.iter().map(|p| p.1).min().unwrap()), (0, 0));
    assert_eq!(at(8, 8), (255, 0, 0));
}

fn output_corner_ignores_reserved_top_and_left_and_stays_above_fullscreen(app: &gtk::Application) {
    let monitors = gtk::gdk::Display::default().unwrap().monitors();
    assert_eq!(monitors.n_items(), 2);
    let mut windows: Vec<gtk::Window> = Vec::new();
    let mut overlays = Vec::new();
    for index in 0..monitors.n_items() {
        let monitor = monitors.item(index).and_downcast::<gtk::gdk::Monitor>().unwrap();
        let geometry = monitor.geometry();
        let connector = monitor.connector().unwrap().to_string();
        windows.push(reserved_panel(app, &monitor, true, 72));
        windows.push(reserved_panel(app, &monitor, false, 64));
        let window = gtk::ApplicationWindow::new(app);
        window.set_title(Some("Private overlay fixture"));
        window.set_decorated(false);
        paint(window.upcast_ref(), (0.0, 0.0, 1.0));
        // Fullscreen first places the fixture explicitly on this output.
        window.fullscreen_on_monitor(&monitor);
        window.present();
        assert!(wait(|| window.is_fullscreen()));
        window.unfullscreen();
        window.maximize();
        assert!(wait(|| window.is_maximized() && window.width() == geometry.width() - 64 && window.height() == geometry.height() - 72));
        let label = MonitorOverlay::new(&monitor, &connector, "Test display", geometry.width() as i64, geometry.height() as i64, 0);
        paint(&label.window, (1.0, 0.0, 0.0));
        label.window.set_size_request(160, 120);
        label.present();
        settle(0.15);
        assert_indicator_at_corner(&connector);
        window.fullscreen_on_monitor(&monitor);
        window.present();
        assert!(wait(|| window.is_fullscreen()));
        settle(0.15);
        assert_indicator_at_corner(&connector);
        windows.push(window.upcast());
        overlays.push(label);
    }
    for overlay in overlays {
        overlay.destroy();
    }
    for window in windows.into_iter().rev() {
        window.destroy();
    }
    settle(0.15);
    println!("ok output_corner_ignores_reserved_top_and_left_and_stays_above_fullscreen");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none()
        || std::env::var_os("WAY_DISPLAYS_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/way-displays/tests/run_headless.sh for the private compositor)");
        return;
    }
    suite_core::i18n::initialize();
    gtk::init().expect("display");
    gtk::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
    let app = gtk::Application::new(Some("org.waydisplays.BrightnessTests"), gio::ApplicationFlags::NON_UNIQUE);
    app.register(gio::Cancellable::NONE).unwrap();
    connector_matching_refresh_and_unsupported_contrast(&app);
    apply_runs_primary_command_and_updates_startup_script(&app);
    slider_coalesces_explicit_input_on_exact_display(&app);
    pending_slider_is_canceled_on_service_loss_busy_or_close(&app);
    missing_monitor_and_write_failure_do_not_show_fake_success(&app);
    hotplug_identity_change_cancels_value_for_previous_display(&app);
    busy_feature_defers_existing_pending_value_without_losing_it(&app);
    loading_feature_is_distinct_from_unsupported_feature(&app);
    shared_auto_dim_settings_refresh_all_tabs_without_extra_writes(&app);
    busy_policy_does_not_overwrite_in_flight_settings(&app);
    local_policy_value_waits_for_async_reply_and_reverts_on_failure(&app);
    color_changes_are_pending_and_shared_with_the_embedded_editor(&app);
    color_controls_explain_an_unsupported_compositor(&app);
    hdr_report_distinguishes_metadata_white_target_and_measurement(&app);
    vrr_changes_in_two_stale_editors_preserve_the_other_monitor(&app);
    automatic_scaling_removes_only_that_displays_scale(&app);
    vrr_uses_desired_state_when_runtime_is_temporarily_off(&app);
    full_window_brightness_changes_preserve_apply_and_monitor_configuration(&app);
    window_follows_live_dock_palette_without_changing_display_settings(&app);
    rebuilt_tabs_and_window_close_cancel_old_debounce_and_client(&app);
    window_contract(&app);
    output_corner_ignores_reserved_top_and_left_and_stays_above_fullscreen(&app);
    println!("ui_headless: all checks passed");
}
