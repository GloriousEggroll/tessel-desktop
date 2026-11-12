//! Port of test_brightness_core.py: dim/resume/crash recovery without
//! changing any real monitor (deferred fake hardware queue).

use crate::backend::Hardware;
use crate::backend_tests::temp_root;
use crate::core::{defaults, BrightnessCore};
use crate::model::{self, Callback};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// `Hardware` of the Python tests: every request is queued until `step()`.
#[derive(Default)]
pub struct FakeHardware {
    pub displays: RefCell<Vec<Value>>,
    pub queue: RefCell<Vec<Box<dyn FnOnce()>>>,
    pub writes: RefCell<Vec<(String, String, i64)>>,
    pub fail_read: Cell<bool>,
    pub fail_write: Cell<bool>,
    pub before_write: RefCell<Option<Rc<dyn Fn()>>>,
    pub not_ready: Cell<bool>,
    me: RefCell<std::rc::Weak<FakeHardware>>,
}

impl FakeHardware {
    pub fn new() -> Rc<Self> {
        let feature = json!({"supported": true, "raw": 75, "max": 100, "value": 75});
        let hardware = Rc::new(Self::default());
        hardware.displays.replace(vec![json!({"id": "one", "connector": "DP-1", "enabled": true,
                                              "brightness": feature.clone(), "contrast": feature})]);
        hardware.me.replace(Rc::downgrade(&hardware));
        hardware
    }

    pub fn set(&self, f: impl FnOnce(&mut Vec<Value>)) {
        f(&mut self.displays.borrow_mut());
    }

    pub fn raw(&self, feature: &str) -> Value {
        self.displays.borrow()[0][feature]["raw"].clone()
    }

    pub fn step(&self) {
        let job = self.queue.borrow_mut().remove(0);
        job();
    }

    pub fn finish(&self) {
        for _ in 0..100 {
            if self.queue.borrow().is_empty() {
                return;
            }
            self.step();
        }
        panic!("Unbounded recovery work");
    }

    pub fn written(&self) -> Vec<i64> {
        self.writes.borrow().iter().map(|w| w.2).collect()
    }
}

impl Hardware for FakeHardware {
    fn ready(&self) -> bool { !self.not_ready.get() }
    fn busy(&self) -> bool { false }
    fn error(&self) -> String { String::new() }
    fn displays(&self) -> Vec<Value> { self.displays.borrow().clone() }
    fn start(&self) {}
    fn close(&self) {}
    fn refresh(&self) {}

    fn read_feature(&self, _connector: &str, _feature: &str, callback: Option<Callback>, _expected_id: Option<&str>) -> bool {
        let me = self.me.borrow().clone();
        self.queue.borrow_mut().push(Box::new(move || {
            let fail = me.upgrade().is_some_and(|h| h.fail_read.get());
            model::call(callback, !fail, if fail { "Read failed" } else { "" });
        }));
        true
    }

    fn set_raw(&self, connector: &str, feature: &str, raw: i64, callback: Option<Callback>, expected_id: Option<&str>,
               expected_raw: Option<i64>) -> bool {
        let me = self.me.borrow().clone();
        let (connector, feature, expected_id) = (connector.to_owned(), feature.to_owned(), expected_id.map(String::from));
        self.queue.borrow_mut().push(Box::new(move || {
            let Some(hw) = me.upgrade() else { return };
            let hook = hw.before_write.borrow().clone();
            if let Some(hook) = hook {
                hook();
            }
            let success = {
                let mut displays = hw.displays.borrow_mut();
                let record = displays.iter_mut().find(|d| d["connector"] == connector.as_str()
                    && Some(d["id"].as_str().unwrap_or("")) == expected_id.as_deref());
                match record {
                    Some(record) if !hw.fail_write.get() && expected_raw.is_none_or(|r| record[&feature]["raw"] == r) => {
                        hw.writes.borrow_mut().push((connector.clone(), feature.clone(), raw));
                        let maximum = record[&feature]["max"].as_f64().unwrap();
                        record[&feature]["raw"] = raw.into();
                        record[&feature]["value"] = json!(100.0 * raw as f64 / maximum);
                        true
                    }
                    _ => false,
                }
            };
            model::call(callback, success, if success { "" } else { "Write failed" });
        }));
        true
    }

    fn set_value(&self, connector: &str, feature: &str, percent: f64, callback: Option<Callback>, expected_id: Option<&str>) -> bool {
        let maximum = self.displays.borrow().iter().find(|d| Some(d["id"].as_str().unwrap_or("")) == expected_id)
            .map(|d| d[feature]["max"].as_f64().unwrap()).unwrap_or(100.0);
        self.set_raw(connector, feature, (percent * maximum / 100.0).round_ties_even() as i64, callback, expected_id, None)
    }
}

pub struct Fx {
    pub directory: PathBuf,
    pub backend: Rc<FakeHardware>,
    pub core: RefCell<BrightnessCore>,
    pub notifications: Rc<Cell<u32>>,
}

impl Fx {
    pub fn new() -> Self {
        let directory = temp_root("core");
        let backend = FakeHardware::new();
        let notifications: Rc<Cell<u32>> = Rc::default();
        let core = Self::make(&backend, &directory, &notifications);
        Self { directory, backend, core: RefCell::new(core), notifications }
    }

    fn make(backend: &Rc<FakeHardware>, directory: &std::path::Path, notifications: &Rc<Cell<u32>>) -> BrightnessCore {
        let n = notifications.clone();
        BrightnessCore::new(backend.clone(), move || n.set(n.get() + 1), Some(directory), Some(directory))
    }

    pub fn make_core(&self) -> BrightnessCore {
        Self::make(&self.backend, &self.directory, &self.notifications)
    }

    pub fn core(&self) -> BrightnessCore {
        self.core.borrow().clone()
    }

    pub fn enable(&self) {
        let mut settings = defaults();
        settings["auto_dim"] = true.into();
        assert!(self.core().set_settings(&settings));
    }

    pub fn dim(&self) {
        self.enable();
        self.core().dim(Rc::new(|| true));
        self.backend.finish();
        assert_eq!(self.backend.raw("brightness"), 30);
    }

    pub fn journal(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.core().0.journal_path).unwrap()).unwrap()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn always() -> Rc<dyn Fn() -> bool> {
    Rc::new(|| true)
}

#[test]
fn defaults_do_not_write_hardware() {
    let fx = Fx::new();
    fx.core().dim(always());
    fx.backend.finish();
    assert!(fx.backend.writes.borrow().is_empty());
    assert!(!fx.core().auto_dim());
}

#[test]
fn durable_original_before_dim_and_exact_resume() {
    let fx = Fx::new();
    fx.enable();
    let path = fx.core().0.journal_path.clone();
    let checked = Rc::new(Cell::new(false));
    let c = checked.clone();
    fx.backend.before_write.replace(Some(Rc::new(move || {
        if !c.get() {
            let journal: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(journal["one"]["raw"], 75);
            c.set(true);
        }
    })));
    fx.core().dim(always());
    fx.backend.finish();
    assert!(checked.get());
    fx.core().restore_all(None);
    fx.backend.finish();
    assert_eq!(*fx.backend.writes.borrow(), vec![("DP-1".into(), "brightness".into(), 30), ("DP-1".into(), "brightness".into(), 75)]);
    assert_eq!(fx.journal(), json!({}));
    assert_eq!(std::fs::read_to_string(&fx.core().0.journal_path).unwrap(), "{}\n");
}

#[test]
fn already_darker_display_never_brightens() {
    let fx = Fx::new();
    fx.backend.set(|d| { d[0]["brightness"]["raw"] = 15.into(); d[0]["brightness"]["value"] = 15.into(); });
    fx.enable();
    fx.core().dim(always());
    fx.backend.finish();
    assert!(fx.backend.writes.borrow().is_empty());
}

#[test]
fn actual_hardware_range_is_restored_without_rounding() {
    let fx = Fx::new();
    fx.backend.set(|d| { d[0]["brightness"]["raw"] = 193.into(); d[0]["brightness"]["max"] = 255.into(); });
    fx.enable();
    fx.core().dim(always());
    fx.backend.finish();
    fx.core().restore_all(None);
    fx.backend.finish();
    assert_eq!(fx.backend.written(), vec![76, 193]);
}

#[test]
fn resume_cancels_pending_fresh_read() {
    let fx = Fx::new();
    fx.enable();
    fx.core().dim(always());
    fx.core().restore_all(None);
    fx.backend.finish();
    assert!(fx.backend.writes.borrow().is_empty());
    assert!(!fx.core().busy());
}

#[test]
fn resume_during_dim_write_restores_after_write() {
    let fx = Fx::new();
    fx.enable();
    fx.core().dim(always());
    fx.backend.step();
    let stopped = Rc::new(Cell::new(0));
    let s = stopped.clone();
    fx.core().restore_all(Some(Box::new(move || s.set(s.get() + 1))));
    assert_eq!(stopped.get(), 0);
    fx.backend.finish();
    assert_eq!(fx.backend.raw("brightness"), 75);
    assert_eq!(stopped.get(), 1);
}

#[test]
fn disable_restores_and_persists_across_restart() {
    let fx = Fx::new();
    fx.dim();
    fx.core().set_settings(&defaults());
    fx.backend.finish();
    assert_eq!(fx.backend.raw("brightness"), 75);
    assert!(!fx.make_core().auto_dim());
}

#[test]
fn new_process_recovers_dimmed_hardware() {
    let fx = Fx::new();
    fx.dim();
    let restarted = fx.make_core();
    restarted.pump();
    fx.backend.finish();
    assert!(restarted.entries().is_empty());
    assert_eq!(fx.backend.raw("brightness"), 75);
}

#[test]
fn external_osd_change_preserved() {
    let fx = Fx::new();
    fx.dim();
    fx.backend.set(|d| d[0]["brightness"]["raw"] = 51.into());
    fx.core().restore_all(None);
    fx.backend.finish();
    assert_eq!(fx.backend.writes.borrow().len(), 1);
    assert!(fx.core().entries().is_empty());
}

#[test]
fn hotplug_different_monitor_cannot_receive_restoration() {
    let fx = Fx::new();
    fx.dim();
    let old = fx.backend.displays();
    fx.backend.set(|d| d[0]["id"] = "two".into());
    fx.core().restore_all(None);
    fx.backend.finish();
    assert_eq!(fx.backend.writes.borrow().len(), 1);
    assert!(fx.core().entries().contains_key("one"));
    fx.backend.displays.replace(old);
    fx.core().pump();
    fx.backend.finish();
    assert_eq!(fx.backend.raw("brightness"), 75);
}

#[test]
fn failure_retains_journal_and_retries_without_looping() {
    let fx = Fx::new();
    fx.dim();
    fx.backend.fail_write.set(true);
    let stopped = Rc::new(Cell::new(0));
    let s = stopped.clone();
    fx.core().restore_all(Some(Box::new(move || s.set(s.get() + 1))));
    fx.backend.finish();
    assert_eq!(stopped.get(), 1);
    assert!(fx.core().entries().contains_key("one"));
    assert!(fx.core().error().starts_with("Brightness restoration is pending: "));
    fx.backend.fail_write.set(false);
    fx.core().restore_all(None);
    fx.backend.finish();
    assert!(fx.core().entries().is_empty());
    assert_eq!(fx.core().error(), "");
}

#[test]
fn failed_journal_write_prevents_dimming() {
    let fx = Fx::new();
    fx.enable();
    fx.core().set_writer(Box::new(|_, _| Err(std::io::Error::other("Disk full"))));
    fx.core().dim(always());
    fx.backend.finish();
    assert!(fx.backend.writes.borrow().is_empty());
    assert!(fx.core().error().starts_with("Could not save the original brightness: "));
}

#[test]
fn manual_change_restores_then_sets_and_cannot_be_undone_by_recovery() {
    let fx = Fx::new();
    fx.dim();
    fx.core().set_value("DP-1", "brightness", 60.0, "one", None).unwrap();
    fx.backend.finish();
    assert_eq!(fx.backend.written(), vec![30, 75, 60]);
    assert!(fx.core().entries().is_empty());
}

#[test]
fn multiple_pending_manual_writes_keep_busy_until_all_complete() {
    let fx = Fx::new();
    fx.dim();
    fx.core().set_value("DP-1", "brightness", 61.0, "one", None).unwrap();
    fx.core().set_value("DP-1", "contrast", 62.0, "one", None).unwrap();
    fx.backend.step(); // restoration read
    fx.backend.step(); // restoration write; first manual request starts
    fx.backend.step(); // first manual write
    assert!(fx.core().busy());
    fx.backend.finish();
    assert!(!fx.core().busy());
    assert_eq!(fx.backend.raw("brightness"), 61);
    assert_eq!(fx.backend.raw("contrast"), 62);
}

#[test]
fn slider_values_arriving_during_a_slow_write_keep_only_the_newest() {
    let fx = Fx::new();
    let done = Rc::new(RefCell::new(Vec::new()));
    let callback = |name: &'static str| -> Option<Callback> {
        let done = done.clone();
        Some(Box::new(move |ok, _| done.borrow_mut().push((name, ok))))
    };
    fx.core().set_value("DP-1", "brightness", 60.0, "one", callback("60")).unwrap();
    // The 60 write is in flight; a drag sends more values meanwhile.
    for value in [55.0, 50.0, 45.0] {
        fx.core().set_value("DP-1", "brightness", value, "one", None).unwrap();
    }
    fx.core().set_value("DP-1", "contrast", 40.0, "one", callback("contrast")).unwrap();
    fx.core().set_value("DP-1", "brightness", 42.0, "one", callback("42")).unwrap();
    assert!(fx.core().busy());
    fx.backend.finish();
    assert_eq!(*fx.backend.writes.borrow(), vec![("DP-1".into(), "brightness".into(), 60), ("DP-1".into(), "contrast".into(), 40),
                                                 ("DP-1".into(), "brightness".into(), 42)]);
    assert_eq!(*done.borrow(), vec![("60", true), ("contrast", true), ("42", true)]);
    assert!(!fx.core().busy());
    assert_eq!(fx.core().error(), "");
}

#[test]
fn stop_waits_for_earlier_manual_requests() {
    let fx = Fx::new();
    fx.dim();
    fx.core().set_value("DP-1", "brightness", 61.0, "one", None).unwrap();
    fx.core().set_value("DP-1", "contrast", 62.0, "one", None).unwrap();
    let stopped = Rc::new(Cell::new(0));
    let s = stopped.clone();
    fx.core().restore_all(Some(Box::new(move || s.set(s.get() + 1))));
    fx.backend.step(); // recovery read
    fx.backend.step(); // recovery write, starts first manual request
    assert_eq!(stopped.get(), 0);
    fx.backend.step(); // first manual request, starts second
    assert_eq!(stopped.get(), 0);
    fx.backend.finish();
    assert_eq!(stopped.get(), 1);
    assert_eq!(fx.backend.raw("contrast"), 62);
}

#[test]
fn guard_rechecked_after_slow_hardware_read() {
    let fx = Fx::new();
    let allowed = Rc::new(Cell::new(true));
    fx.enable();
    let a = allowed.clone();
    fx.core().dim(Rc::new(move || a.get()));
    allowed.set(false);
    fx.backend.finish();
    assert!(fx.backend.writes.borrow().is_empty());
}

#[test]
fn osd_change_between_dim_read_and_write_is_preserved() {
    let fx = Fx::new();
    fx.enable();
    fx.core().dim(always());
    fx.backend.step();
    fx.backend.set(|d| d[0]["brightness"]["raw"] = 41.into());
    fx.backend.finish();
    assert!(fx.backend.writes.borrow().is_empty());
    assert!(fx.core().entries().is_empty());
    assert_eq!(fx.backend.raw("brightness"), 41);
}

#[test]
fn osd_change_between_resume_read_and_write_is_preserved() {
    let fx = Fx::new();
    fx.dim();
    fx.core().restore_all(None);
    fx.backend.step();
    fx.backend.set(|d| d[0]["brightness"]["raw"] = 51.into());
    fx.backend.finish();
    assert_eq!(fx.backend.writes.borrow().len(), 1);
    fx.core().restore_all(None);
    fx.backend.finish();
    assert!(fx.core().entries().is_empty());
    assert_eq!(fx.backend.raw("brightness"), 51);
}

#[test]
fn corrupt_journal_does_not_write_hardware() {
    let fx = Fx::new();
    let path = fx.core().0.journal_path.clone();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{invalid").unwrap();
    fx.core.replace(fx.make_core());
    assert!(fx.core().error().starts_with("Could not load brightness recovery data: "));
    fx.enable();
    fx.core().dim(always());
    fx.backend.finish();
    assert!(fx.backend.writes.borrow().is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{invalid");
    for invalid in [r#"[]"#, r#"{"one": {"connector": "DP-1", "raw": 75, "target": 75, "max": 100}}"#,
                    r#"{"one": {"connector": "DP-1", "raw": 75, "target": 30, "max": 70}}"#,
                    r#"{"one": {"connector": "DP-1", "raw": 75.0, "target": 30, "max": 100}}"#] {
        std::fs::write(&path, invalid).unwrap();
        assert!(fx.make_core().error().starts_with("Could not load brightness recovery data: "), "{invalid}");
    }
}

#[test]
fn invalid_settings_do_not_replace_saved_preferences() {
    let fx = Fx::new();
    fx.enable();
    let mut bad = defaults();
    bad["dim_after"] = (-1).into();
    assert!(!fx.core().set_settings(&bad));
    assert_eq!(fx.core().error(), "The dimming delay must be between one second and 24 hours.");
    assert!(fx.make_core().auto_dim());
    for (value, message) in [(json!({"auto_dim": true}), "Choose valid automatic dimming settings."),
                             (json!({"auto_dim": 1, "dim_after": 120, "dim_percent": 30}), "Auto-dim must be on or off."),
                             (json!({"auto_dim": true, "dim_after": 120, "dim_percent": 91}), "Choose a dimmed brightness from 1% to 90%."),
                             (json!({"auto_dim": true, "dim_after": 120.0, "dim_percent": 30}),
                              "The dimming delay must be between one second and 24 hours.")] {
        assert!(!fx.core().set_settings(&value));
        assert_eq!(fx.core().error(), message);
    }
    let text = std::fs::read_to_string(&fx.core().0.config_path).unwrap();
    assert_eq!(text, "{\n  \"auto_dim\": true,\n  \"dim_after\": 120,\n  \"dim_percent\": 30\n}\n");
}
