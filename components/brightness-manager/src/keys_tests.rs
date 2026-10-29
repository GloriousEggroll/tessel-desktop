//! Port of test_brightness_keys.py: keyboard bursts use confirmed hardware
//! and stable output identity.

use crate::backend_tests::temp_root;
use crate::core::{defaults, BrightnessCore};
use crate::core_tests::FakeHardware;
use crate::keys::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default)]
struct FakeOsd {
    shown: RefCell<Vec<(Option<f64>, String)>>,
    resolving: RefCell<Vec<Box<dyn FnOnce(Option<Monitor>)>>>,
    defer: Cell<bool>,
}

impl Osd for FakeOsd {
    fn show(&self, value: Option<f64>, _monitor: Option<&Monitor>, detail: &str) {
        self.shown.borrow_mut().push((value, detail.into()));
    }
    fn resolve_monitor(&self, callback: Box<dyn FnOnce(Option<Monitor>)>) {
        if self.defer.get() {
            self.resolving.borrow_mut().push(callback);
        } else {
            callback(None);
        }
    }
    fn close(&self) {}
}

impl FakeOsd {
    fn last(&self) -> (Option<f64>, String) {
        self.shown.borrow().last().cloned().unwrap()
    }
}

struct Fx {
    directory: std::path::PathBuf,
    backend: Rc<FakeHardware>,
    core: BrightnessCore,
    osd: Rc<FakeOsd>,
    keys: BrightnessKeys,
}

impl Fx {
    fn new() -> Self {
        let directory = temp_root("keys");
        let backend = FakeHardware::new();
        let core = BrightnessCore::new(backend.clone(), || {}, Some(&directory), Some(&directory));
        let osd = Rc::new(FakeOsd::default());
        let keys = BrightnessKeys::new(core.clone(), osd.clone(), None);
        Self { directory, backend, core, osd, keys }
    }

    fn writes(&self) -> Vec<i64> {
        self.backend.written()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn repeat_burst_sends_one_write_then_only_the_latest_target() {
    let fx = Fx::new();
    for _ in 0..20 {
        assert!(fx.keys.adjust(10.0));
    }
    assert_eq!(fx.backend.queue.borrow().len(), 1);
    assert_eq!(fx.osd.last().0, Some(75.0), "Pending requests must not report unconfirmed brightness");
    fx.backend.step();
    assert_eq!(fx.writes(), vec![85]);
    assert_eq!(fx.backend.queue.borrow().len(), 1);
    fx.backend.finish();
    assert_eq!(fx.writes(), vec![85, 100]);
    assert_eq!(fx.osd.last().0, Some(100.0));
}

#[test]
fn direction_change_after_limit_is_applied_to_requested_level() {
    let fx = Fx::new();
    fx.backend.set(|d| { d[0]["brightness"]["raw"] = 95.into(); d[0]["brightness"]["value"] = 95.into(); });
    fx.keys.adjust(10.0);
    fx.keys.adjust(-10.0);
    fx.backend.finish();
    assert_eq!(fx.writes(), vec![100, 90]);
    assert_eq!(fx.osd.last().0, Some(90.0));
}

#[test]
fn already_at_limit_does_not_write_hardware() {
    let fx = Fx::new();
    fx.backend.set(|d| { d[0]["brightness"]["raw"] = 100.into(); d[0]["brightness"]["value"] = 100.into(); });
    fx.keys.adjust(10.0);
    assert!(fx.backend.queue.borrow().is_empty());
    assert_eq!(fx.osd.last().0, Some(100.0));
}

#[test]
fn auto_dim_restores_before_relative_keyboard_adjustment() {
    let fx = Fx::new();
    let mut settings = defaults();
    settings["auto_dim"] = true.into();
    fx.core.set_settings(&settings);
    fx.core.dim(Rc::new(|| true));
    fx.backend.finish();
    assert_eq!(fx.backend.raw("brightness"), 30);
    fx.keys.adjust(-10.0);
    fx.backend.finish();
    assert_eq!(fx.writes(), vec![30, 75, 65]);
    assert!(fx.core.entries().is_empty());
    assert_eq!(fx.osd.last().0, Some(65.0));
}

#[test]
fn hardware_failure_discards_queued_repeats_and_shows_actual_value() {
    let fx = Fx::new();
    fx.backend.fail_write.set(true);
    fx.keys.adjust(10.0);
    fx.keys.adjust(10.0);
    fx.backend.finish();
    assert!(fx.writes().is_empty());
    let (value, detail) = fx.osd.last();
    assert_eq!(value, Some(75.0));
    assert!(detail.to_lowercase().contains("failed"));
    assert!(fx.backend.queue.borrow().is_empty());
}

#[test]
fn replaced_monitor_is_never_adjusted_by_a_pending_burst() {
    let fx = Fx::new();
    fx.keys.adjust(10.0);
    fx.keys.adjust(10.0);
    fx.backend.set(|d| d[0]["id"] = "replacement".into());
    fx.backend.finish();
    assert!(fx.writes().is_empty());
    assert_eq!(fx.osd.last().0, None);
    assert_eq!(fx.backend.raw("brightness"), 75);
}

#[test]
fn slider_override_cancels_queued_keyboard_target() {
    let fx = Fx::new();
    fx.keys.adjust(10.0);
    fx.keys.adjust(10.0);
    fx.keys.cancel();
    fx.core.set_value("DP-1", "brightness", 55.0, "one", None).unwrap();
    fx.backend.finish();
    assert_eq!(fx.writes(), vec![85, 55]);
}

#[test]
fn repeats_during_monitor_resolution_accumulate_without_extra_resolutions() {
    let fx = Fx::new();
    fx.osd.defer.set(true);
    for step in [10.0, 10.0, -10.0] {
        fx.keys.adjust(step);
    }
    assert_eq!(fx.osd.resolving.borrow().len(), 1);
    assert!(fx.backend.queue.borrow().is_empty());
    let callback = fx.osd.resolving.borrow_mut().pop().unwrap();
    callback(None);
    fx.backend.finish();
    assert_eq!(fx.writes(), vec![85]);
}

#[test]
fn shutdown_invalidates_pending_monitor_resolution_and_writes() {
    let fx = Fx::new();
    fx.osd.defer.set(true);
    fx.keys.adjust(10.0);
    fx.keys.close();
    let callback = fx.osd.resolving.borrow_mut().pop().unwrap();
    callback(None);
    assert!(!fx.keys.adjust(10.0));
    assert!(fx.backend.queue.borrow().is_empty());
    assert!(fx.osd.shown.borrow().is_empty());
}

#[test]
fn unsupported_and_unknown_controls_never_receive_requests() {
    let fx = Fx::new();
    for feature in [json!({"supported": false, "value": 75}), json!({"supported": null, "value": 75}),
                    json!({"supported": true, "value": "nan"})] {
        fx.backend.set(|d| d[0]["brightness"] = feature.clone());
        fx.keys.adjust(10.0);
        assert!(fx.backend.queue.borrow().is_empty());
        assert_eq!(fx.osd.last().0, None);
    }
    assert_eq!(fx.osd.last().1, "No display supports hardware brightness control.");
}

#[test]
fn not_ready_backend_reports_without_resolving() {
    let fx = Fx::new();
    fx.backend.not_ready.set(true);
    assert!(!fx.keys.adjust(10.0));
    assert_eq!(fx.osd.last(), (None, "Display controls are not ready.".to_owned()));
}

fn selection() -> (Vec<Value>, Monitor) {
    let mut displays = FakeHardware::new().displays.borrow().clone();
    displays[0]["geometry"] = json!([0, 0, 1920, 1080]);
    displays[0]["method"] = json!("ddc");
    let mut second = displays[0].clone();
    second["connector"] = json!("DP-2");
    second["id"] = json!("two");
    second["geometry"] = json!([1920, 0, 1920, 1080]);
    displays.push(second);
    (displays, Monitor { geometry: [1920, 0, 1920, 1080], model: Some("Generic panel".into()), handle: None })
}

fn nowhere() -> Option<&'static std::path::Path> {
    Some(std::path::Path::new("/no/theme/cache"))
}

#[test]
fn native_geometry_selects_correct_display_without_waydock() {
    let (displays, monitor) = selection();
    assert_eq!(select_display(&displays, Some(&monitor), nowhere()), Some(displays[1].clone()));
}

#[test]
fn unique_position_handles_fractional_scale_rounding() {
    let (mut displays, monitor) = selection();
    displays[1]["geometry"] = json!([1920, 0, 1919, 1079]);
    assert_eq!(select_display(&displays, Some(&monitor), nowhere()), Some(displays[1].clone()));
}

#[test]
fn internal_panel_is_fallback_but_never_overrides_a_matching_external() {
    let (mut displays, monitor) = selection();
    displays[0]["method"] = json!("backlight");
    assert_eq!(select_display(&displays, None, None), Some(displays[0].clone()));
    assert_eq!(select_display(&displays, Some(&monitor), None), Some(displays[1].clone()));
}

#[test]
fn unsupported_output_skipped_and_ambiguous_external_outputs_unchanged() {
    let (mut displays, monitor) = selection();
    assert_eq!(select_display(&displays, None, None), None);
    displays[1]["brightness"]["supported"] = json!(false);
    assert_eq!(select_display(&displays, Some(&monitor), nowhere()), None,
               "An unsupported matched output must not redirect keys to another monitor");
    assert_eq!(select_display(&displays, None, None), Some(displays[0].clone()));
    displays[0]["enabled"] = json!(false);
    assert_eq!(select_display(&displays, Some(&monitor), nowhere()), None);
}

#[test]
fn optional_palette_fallback_only_for_missing_native_geometry() {
    let folder = temp_root("palette");
    let cache = folder.join("palette.json");
    std::fs::write(&cache, json!({"version": 1, "monitors": [{"name": "DP-2", "geometry": [1920, 0, 1920, 1080]}]}).to_string()).unwrap();
    let (mut displays, monitor) = selection();
    for display in &mut displays {
        display.as_object_mut().unwrap().remove("geometry");
    }
    assert_eq!(select_display(&displays, Some(&monitor), Some(&cache)), Some(displays[1].clone()));
    std::fs::write(&cache, "invalid").unwrap();
    assert_eq!(select_display(&displays, Some(&monitor), Some(&cache)), None);
    let mut connector_model = monitor.clone();
    connector_model.model = Some("DP-2".into());
    assert_eq!(select_display(&displays, Some(&connector_model), Some(&cache)), Some(displays[1].clone()));
    let _ = std::fs::remove_dir_all(&folder);
}
