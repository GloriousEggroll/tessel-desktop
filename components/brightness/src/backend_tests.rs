//! Port of test_brightness_backend.py: fake DRM/sysfs, commands and logind
//! only. Nothing here runs ddcutil, wlr-randr or touches a real backlight.

use crate::backend::*;
use tessel_core::scheduler::FakeScheduler;
use crate::model::{Callback, UNAVAILABLE_CONTROL};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

pub fn edid(number: u8) -> Vec<u8> {
    let mut data = b"\x00\xff\xff\xff\xff\xff\xff\x00".to_vec();
    data.extend(std::iter::repeat_n(number, 120));
    data
}

pub fn temp_root(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("brightness-test-{}-{name}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[derive(Default)]
pub struct World {
    pub outputs: Vec<Value>,
    pub values: HashMap<u32, HashMap<String, [i64; 2]>>,
    pub commands: Vec<Vec<String>>,
    pub threads: Vec<ThreadId>,
    pub before_get: Option<Box<dyn FnOnce() + Send>>,
    pub fail_write: bool,
    pub ignore_write: bool,
    pub output_failure: bool,
    pub logind: Vec<(String, u32)>,
    pub backlight: Option<PathBuf>,
}

pub struct FakeSystem {
    pub world: Arc<Mutex<World>>,
    pub root: PathBuf,
}

impl System for FakeSystem {
    fn which(&self, name: &str) -> Option<String> {
        Some(format!("/usr/bin/{name}"))
    }

    fn run(&self, command: &[String], _timeout: Duration) -> Result<CommandOutput, String> {
        let hook = {
            let mut world = self.world.lock().unwrap();
            world.commands.push(command.to_vec());
            world.threads.push(std::thread::current().id());
            if command[0].ends_with("wlr-randr") {
                let failure = world.output_failure;
                return Ok(CommandOutput { status: failure as i32, stdout: Value::from(world.outputs.clone()).to_string(),
                                          stderr: if failure { "Output discovery failed".into() } else { String::new() } });
            }
            if command.iter().any(|w| w == "getvcp") { world.before_get.take() } else { None }
        };
        if let Some(hook) = hook {
            hook();
        }
        let mut world = self.world.lock().unwrap();
        let index = |word: &str| command.iter().position(|w| w == word).unwrap();
        let bus: u32 = command[index("--bus") + 1].parse().unwrap();
        if command.iter().any(|w| w == "getvcp") {
            let requested = &command[index("getvcp") + 1..index("--terse")];
            let available = world.values.get(&bus).cloned().unwrap_or_default();
            let stdout = requested.iter().map(|code| match available.get(code) {
                Some([raw, max]) => format!("VCP {code} C {raw} {max}"),
                None => format!("VCP {code} ERR"),
            }).collect::<Vec<_>>().join("\n");
            let empty = available.is_empty();
            return Ok(CommandOutput { status: empty as i32, stdout,
                                      stderr: if empty { "DDC address 0x37 did not respond".into() } else { String::new() } });
        }
        if command.iter().any(|w| w == "setvcp") {
            let at = index("setvcp");
            let (code, raw) = (command[at + 1].clone(), command[at + 2].parse::<i64>().unwrap());
            let (fail, ignore) = (world.fail_write, world.ignore_write);
            if !fail && !ignore {
                world.values.get_mut(&bus).unwrap().get_mut(&code).unwrap()[0] = raw;
            }
            return Ok(CommandOutput { status: fail as i32, stdout: String::new(),
                                      stderr: if fail { "Write denied".into() } else { String::new() } });
        }
        panic!("unexpected command {command:?}");
    }

    fn drm_root(&self) -> PathBuf {
        self.root.join("drm")
    }

    fn backlight_root(&self) -> PathBuf {
        self.root.join("backlight")
    }

    fn set_backlight(&self, name: &str, raw: u32, guard: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        guard()?;
        let mut world = self.world.lock().unwrap();
        world.logind.push((name.into(), raw));
        if let Some(path) = &world.backlight {
            std::fs::write(path.join("brightness"), raw.to_string()).unwrap();
        }
        Ok(())
    }

    fn sleep(&self, _duration: Duration) {}
}

pub struct FakeMonitors {
    pub changed: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    pub closed: Rc<RefCell<bool>>,
}

impl MonitorWatch for FakeMonitors {
    fn watch(&self, changed: Rc<dyn Fn()>) {
        self.changed.replace(Some(changed));
    }
    fn close(&self) {
        self.changed.replace(None);
        self.closed.replace(true);
    }
}

pub struct Fx {
    pub root: PathBuf,
    pub world: Arc<Mutex<World>>,
    pub backend: DisplayBackend,
    pub scheduler: Rc<FakeScheduler>,
    pub changed: Rc<RefCell<Vec<ThreadId>>>,
    pub errors: Rc<RefCell<Vec<String>>>,
    pub monitor_changed: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    pub monitors_closed: Rc<RefCell<bool>>,
}

impl Fx {
    pub fn new() -> Self {
        let root = temp_root("hw");
        std::fs::create_dir_all(root.join("drm")).unwrap();
        std::fs::create_dir_all(root.join("backlight")).unwrap();
        let world = Arc::new(Mutex::new(World::default()));
        {
            let mut w = world.lock().unwrap();
            w.values.insert(12, HashMap::from([("10".into(), [75, 100]), ("12".into(), [75, 100])]));
            w.values.insert(8, HashMap::new());
        }
        let scheduler = Rc::new(FakeScheduler::default());
        let changed: Rc<RefCell<Vec<ThreadId>>> = Rc::default();
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let monitor_changed: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();
        let monitors_closed: Rc<RefCell<bool>> = Rc::default();
        let (c, e) = (changed.clone(), errors.clone());
        let options = Options {
            system: Arc::new(FakeSystem { world: world.clone(), root: root.clone() }), scheduler: scheduler.clone(),
            monitors: Some(Box::new(FakeMonitors { changed: monitor_changed.clone(), closed: monitors_closed.clone() })),
            spawn_events: false,
        };
        let backend = DisplayBackend::new(move || c.borrow_mut().push(std::thread::current().id()),
                                          move |m| e.borrow_mut().push(m.into()), options);
        let fx = Self { root, world, backend, scheduler, changed, errors, monitor_changed, monitors_closed };
        fx.add_output("DP-2", 1, Some(12), true, 1);
        fx.add_output("HDMI-A-1", 2, Some(8), false, 1);
        fx
    }

    pub fn add_output(&self, connector: &str, identity: u8, bus: Option<u32>, local: bool, card: u32) -> PathBuf {
        self.world.lock().unwrap().outputs.push(json!({"name": connector, "description": format!("Monitor {connector}"), "enabled": true}));
        let path = self.root.join("drm").join(format!("card{card}-{connector}"));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("status"), "connected\n").unwrap();
        std::fs::write(path.join("edid"), edid(identity)).unwrap();
        if let Some(bus) = bus {
            let target = self.root.join(format!("i2c-{}", if local { 7 } else { bus }));
            std::fs::create_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(&target, path.join("ddc")).unwrap();
            if local {
                std::fs::create_dir_all(path.join(format!("i2c-{bus}"))).unwrap();
            }
        }
        path
    }

    pub fn pump(&self, mut predicate: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(3);
        while !predicate() && Instant::now() < end {
            self.backend.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(predicate(), "Asynchronous backend did not complete");
    }

    pub fn start(&self) {
        self.backend.start();
        self.pump(|| self.backend.ready() && !self.backend.busy());
    }

    pub fn display(&self, connector: &str) -> Value {
        self.backend.displays().into_iter().find(|d| d["connector"] == connector).unwrap()
    }

    pub fn writes(&self) -> usize {
        self.world.lock().unwrap().commands.iter().filter(|c| c.iter().any(|w| w == "setvcp")).count()
    }

    pub fn probes(&self, bus: u32) -> usize {
        self.world.lock().unwrap().commands.iter().filter(|c| {
            c.iter().any(|w| w == "getvcp") && c[c.iter().position(|w| w == "--bus").unwrap() + 1] == bus.to_string()
        }).count()
    }

    pub fn getvcp(&self) -> usize {
        self.world.lock().unwrap().commands.iter().filter(|c| c.iter().any(|w| w == "getvcp")).count()
    }

    fn discoveries(&self) -> usize {
        self.world.lock().unwrap().commands.iter().filter(|c| c[0].ends_with("wlr-randr")).count()
    }

    fn set_output(&self, index: usize, f: impl FnOnce(&mut Value)) {
        f(&mut self.world.lock().unwrap().outputs[index]);
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.backend.close();
        self.backend.join_worker();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

type Log = Rc<RefCell<Vec<(bool, String)>>>;

fn recorder() -> (Log, Option<Callback>) {
    let log: Log = Rc::default();
    let l = log.clone();
    (log, Some(Box::new(move |ok, error| l.borrow_mut().push((ok, error)))))
}

#[test]
fn scaled_and_rotated_mode_has_logical_bounds() {
    let output = json!({"position": {"x": -1440, "y": 120}, "scale": 1.5, "modes": [{"width": 3840, "height": 2160, "current": true}]});
    assert_eq!(logical_geometry(&output), Some([-1440, 120, 2560, 1440]));
    for transform in ["90", "270", "flipped-90", "flipped-270"] {
        let mut rotated = output.clone();
        rotated["transform"] = transform.into();
        assert_eq!(logical_geometry(&rotated), Some([-1440, 120, 1440, 2560]));
    }
}

#[test]
fn missing_or_invalid_mode_does_not_invent_output_position() {
    assert_eq!(logical_geometry(&json!({})), None);
    let output = json!({"position": {"x": 0, "y": 0}, "scale": 1, "modes": [{"width": 1920, "height": 1080, "current": true}]});
    assert_eq!(logical_geometry(&output), Some([0, 0, 1920, 1080]));
    for scale in [json!(0), json!(-1), json!(false), json!("1")] {
        let mut bad = output.clone();
        bad["scale"] = scale;
        assert_eq!(logical_geometry(&bad), None);
    }
}

#[test]
fn output_layout_refresh_updates_geometry_without_reprobing_hardware() {
    let fx = Fx::new();
    fx.set_output(0, |o| {
        o["position"] = json!({"x": 0, "y": 0});
        o["scale"] = json!(2);
        o["modes"] = json!([{"width": 3840, "height": 2160, "current": true}]);
    });
    fx.start();
    assert_eq!(fx.display("DP-2")["geometry"], json!([0, 0, 1920, 1080]));
    let probes = fx.getvcp();
    fx.set_output(0, |o| {
        o["position"] = json!({"x": 1920, "y": -120});
        o["transform"] = json!("90");
    });
    fx.backend.queue_refresh(false);
    fx.pump(|| !fx.backend.busy());
    assert_eq!(fx.display("DP-2")["geometry"], json!([1920, -120, 1080, 1920]));
    assert_eq!(fx.getvcp(), probes);
}

#[test]
fn discovery_read_only_and_actual_aux_bus_preferred() {
    let fx = Fx::new();
    fx.start();
    let display = fx.display("DP-2");
    assert_eq!(display["method"], "ddc");
    assert_eq!(display["brightness"]["raw"], 75);
    assert_eq!(display["contrast"]["value"], json!(75.0));
    assert_eq!(display["id"], edid_hash(&edid(1)).unwrap());
    let unavailable = fx.display("HDMI-A-1");
    assert_eq!(unavailable["method"], "unsupported");
    assert_eq!(unavailable["brightness"]["supported"], false);
    assert!(unavailable["diagnostic"].as_str().unwrap().contains("0x37"));
    assert_eq!(unavailable["error"], UNAVAILABLE_CONTROL);
    for feature in ["brightness", "contrast"] {
        assert_eq!(unavailable[feature]["error"], UNAVAILABLE_CONTROL);
        assert!(unavailable[feature]["diagnostic"].as_str().unwrap().contains("0x37"));
    }
    assert_eq!(fx.writes(), 0);
    let world = fx.world.lock().unwrap();
    let buses: std::collections::HashSet<String> = world.commands.iter().filter_map(|c| {
        c.iter().position(|w| w == "--bus").map(|i| c[i + 1].clone())
    }).collect();
    assert_eq!(buses, ["12".to_owned(), "8".to_owned()].into_iter().collect());
    let main = std::thread::current().id();
    assert!(world.threads.iter().all(|t| *t != main), "hardware commands must run off the main thread");
    assert!(fx.changed.borrow().iter().all(|t| *t == main), "change notifications run on the main thread");
}

#[test]
fn percent_converts_actual_maximum_and_readback_precedes_callback() {
    let fx = Fx::new();
    fx.world.lock().unwrap().values.get_mut(&12).unwrap().insert("10".into(), [75, 200]);
    fx.start();
    let observed: Rc<RefCell<Vec<(bool, String, Value)>>> = Rc::default();
    let (o, backend) = (observed.clone(), fx.backend.clone());
    assert!(fx.backend.set_value("DP-2", "brightness", 47.5, Some(Box::new(move |ok, error| {
        let raw = backend.displays()[0]["brightness"]["raw"].clone();
        o.borrow_mut().push((ok, error, raw));
    })), None));
    fx.pump(|| !observed.borrow().is_empty());
    assert_eq!(*observed.borrow(), vec![(true, String::new(), json!(95))]);
    assert_eq!(fx.display("DP-2")["brightness"]["value"], json!(47.5));
    assert_eq!(fx.writes(), 1);
    assert!(!fx.backend.busy());
}

#[test]
fn raw_restore_does_not_round_through_percentage() {
    let fx = Fx::new();
    fx.world.lock().unwrap().values.get_mut(&12).unwrap().insert("10".into(), [40, 255]);
    fx.start();
    let (log, callback) = recorder();
    let id = fx.display("DP-2")["id"].as_str().unwrap().to_owned();
    fx.backend.set_raw("DP-2", "brightness", 191, callback, Some(&id), None);
    fx.pump(|| !log.borrow().is_empty());
    assert_eq!(fx.world.lock().unwrap().values[&12]["10"][0], 191);
    assert_eq!(*log.borrow(), vec![(true, String::new())]);
}

#[test]
fn fresh_read_publishes_external_osd_value_before_callback() {
    let fx = Fx::new();
    fx.start();
    fx.world.lock().unwrap().values.get_mut(&12).unwrap().get_mut("10").unwrap()[0] = 61;
    let seen: Rc<RefCell<Vec<(bool, Value)>>> = Rc::default();
    let (s, backend) = (seen.clone(), fx.backend.clone());
    let id = fx.display("DP-2")["id"].as_str().unwrap().to_owned();
    fx.backend.read_feature("DP-2", "brightness", Some(Box::new(move |ok, _| {
        s.borrow_mut().push((ok, backend.displays()[0]["brightness"]["raw"].clone()));
    })), Some(&id));
    fx.pump(|| !seen.borrow().is_empty());
    assert_eq!(*seen.borrow(), vec![(true, json!(61))]);
    assert_eq!(fx.writes(), 0);
}

#[test]
fn subsequent_changes_queue_and_complete_once_each() {
    let fx = Fx::new();
    fx.start();
    let log: Rc<RefCell<Vec<(i64, bool)>>> = Rc::default();
    for raw in [20, 40] {
        let l = log.clone();
        fx.backend.set_raw("DP-2", "brightness", raw, Some(Box::new(move |ok, _| l.borrow_mut().push((raw, ok)))), None, None);
    }
    fx.pump(|| log.borrow().len() == 2);
    assert_eq!(*log.borrow(), vec![(20, true), (40, true)]);
    assert_eq!(fx.display("DP-2")["brightness"]["raw"], 40);
    assert_eq!(fx.display("DP-2")["brightness"]["busy"], false);
}

#[test]
fn invalid_values_and_identity_reject_without_writes() {
    let fx = Fx::new();
    fx.start();
    for value in [f64::NAN, f64::INFINITY, -1.0, 101.0] {
        let (log, callback) = recorder();
        assert!(!fx.backend.set_value("DP-2", "brightness", value, callback, None));
        assert_eq!(log.borrow().len(), 1);
        assert!(!log.borrow()[0].0);
    }
    let (log, callback) = recorder();
    assert!(!fx.backend.set_raw("DP-2", "brightness", 20, callback, Some("wrong-monitor"), None));
    let l = log.clone();
    assert!(!fx.backend.read_feature("DP-2", "brightness", Some(Box::new(move |ok, e| l.borrow_mut().push((ok, e)))),
                                     Some("wrong-monitor")));
    assert_eq!(log.borrow().len(), 2);
    let (log, callback) = recorder();
    assert!(!fx.backend.set_raw("DP-2", "brightness", -1, callback, None, None));
    assert!(!fx.backend.set_raw("DP-2", "brightness", 20, None, None, Some(-5)));
    assert_eq!(log.borrow()[0].1, "The saved hardware value is invalid.");
    assert_eq!(fx.writes(), 0);
}

#[test]
fn unplugged_monitor_does_not_receive_write() {
    let fx = Fx::new();
    fx.start();
    std::fs::write(fx.root.join("drm/card1-DP-2/status"), "disconnected\n").unwrap();
    let (log, callback) = recorder();
    fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None);
    fx.pump(|| !log.borrow().is_empty());
    assert!(!log.borrow()[0].0);
    assert_eq!(fx.writes(), 0);
}

#[test]
fn edid_swap_after_preread_cancels_before_write() {
    let fx = Fx::new();
    fx.start();
    let path = fx.root.join("drm/card1-DP-2/edid");
    fx.world.lock().unwrap().before_get = Some(Box::new(move || std::fs::write(&path, edid(9)).unwrap()));
    let (log, callback) = recorder();
    fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None);
    fx.pump(|| !log.borrow().is_empty());
    assert!(!log.borrow()[0].0);
    assert!(log.borrow()[0].1.contains("changed"));
    assert_eq!(fx.writes(), 0);
}

#[test]
fn bus_replaced_after_preread_cancels_before_write() {
    let fx = Fx::new();
    fx.start();
    let old = fx.root.join("drm/card1-DP-2/i2c-12");
    fx.world.lock().unwrap().before_get = Some(Box::new(move || std::fs::rename(&old, old.with_file_name("i2c-13")).unwrap()));
    let (log, callback) = recorder();
    fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None);
    fx.pump(|| !log.borrow().is_empty());
    assert!(!log.borrow()[0].0);
    assert_eq!(fx.writes(), 0);
}

#[test]
fn range_change_after_queue_cancels_write() {
    let fx = Fx::new();
    fx.start();
    fx.world.lock().unwrap().values.get_mut(&12).unwrap().get_mut("10").unwrap()[1] = 200;
    let (log, callback) = recorder();
    fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None);
    fx.pump(|| !log.borrow().is_empty());
    assert!(!log.borrow()[0].0);
    assert!(log.borrow()[0].1.contains("range changed"));
    assert_eq!(fx.writes(), 0);
}

#[test]
fn osd_change_after_journal_read_prevents_stale_restore() {
    let fx = Fx::new();
    fx.start();
    fx.world.lock().unwrap().values.get_mut(&12).unwrap().get_mut("10").unwrap()[0] = 43;
    let (log, callback) = recorder();
    let id = fx.display("DP-2")["id"].as_str().unwrap().to_owned();
    fx.backend.set_raw("DP-2", "brightness", 80, callback, Some(&id), Some(75));
    fx.pump(|| !log.borrow().is_empty());
    assert!(!log.borrow()[0].0);
    assert!(log.borrow()[0].1.contains("changed externally"));
    assert_eq!(fx.display("DP-2")["brightness"]["raw"], 43);
    assert_eq!(fx.world.lock().unwrap().values[&12]["10"][0], 43);
    assert_eq!(fx.writes(), 0);
}

#[test]
fn failed_write_preserves_last_confirmed_state() {
    let fx = Fx::new();
    fx.start();
    fx.world.lock().unwrap().fail_write = true;
    let (log, callback) = recorder();
    fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None);
    fx.pump(|| !log.borrow().is_empty());
    assert_eq!(*log.borrow(), vec![(false, "Write denied".to_owned())]);
    let display = fx.display("DP-2");
    assert_eq!(display["brightness"]["raw"], 75);
    assert_eq!(display["brightness"]["supported"], true);
    assert_eq!(display["brightness"]["busy"], false);
    assert_eq!(*fx.errors.borrow().last().unwrap(), "Write denied");
}

#[test]
fn unconfirmed_write_publishes_real_value_and_fails() {
    let fx = Fx::new();
    fx.start();
    fx.world.lock().unwrap().ignore_write = true;
    let (log, callback) = recorder();
    fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None);
    fx.pump(|| !log.borrow().is_empty());
    assert!(!log.borrow()[0].0);
    assert!(log.borrow()[0].1.contains("reported 75"));
    assert_eq!(fx.display("DP-2")["brightness"]["raw"], 75);
}

#[test]
fn identical_edids_are_connector_scoped() {
    let fx = Fx::new();
    std::fs::write(fx.root.join("drm/card1-HDMI-A-1/edid"), edid(1)).unwrap();
    fx.start();
    let hash = edid_hash(&edid(1)).unwrap();
    assert_eq!(fx.display("DP-2")["id"], format!("{hash}:DP-2"));
    assert_eq!(fx.display("HDMI-A-1")["id"], format!("{hash}:HDMI-A-1"));
}

#[test]
fn multiple_gpus_with_same_connector_disable_ambiguous_mapping() {
    let fx = Fx::new();
    fx.add_output("DP-2", 3, Some(18), false, 2);
    fx.world.lock().unwrap().outputs.pop(); // One compositor output, two connected DRM candidates.
    fx.start();
    let display = fx.display("DP-2");
    assert_eq!(display["method"], "unsupported");
    assert!(display["error"].as_str().unwrap().contains("uniquely"));
    assert_eq!(display["brightness"]["supported"], false);
}

#[test]
fn internal_backlight_preferred_and_logind_is_only_writer() {
    let fx = Fx::new();
    fx.world.lock().unwrap().outputs.clear();
    let connector = fx.add_output("eDP-1", 3, None, false, 1);
    let backlight = fx.root.join("backlight/panel");
    std::fs::create_dir_all(&backlight).unwrap();
    std::os::unix::fs::symlink(&connector, backlight.join("device")).unwrap();
    std::fs::write(backlight.join("type"), "raw\n").unwrap();
    std::fs::write(backlight.join("brightness"), "57\n").unwrap();
    std::fs::write(backlight.join("max_brightness"), "255\n").unwrap();
    fx.world.lock().unwrap().backlight = Some(backlight);
    fx.start();
    let display = fx.display("eDP-1");
    assert_eq!(display["method"], "backlight");
    assert_eq!(display["contrast"]["supported"], false);
    assert_eq!(display["contrast"]["error"], "Internal display contrast is not exposed by its backlight.");
    assert_eq!(display["brightness"]["raw"], 57);
    let (log, callback) = recorder();
    fx.backend.set_value("eDP-1", "brightness", 60.0, callback, None);
    fx.pump(|| !log.borrow().is_empty());
    assert_eq!(fx.world.lock().unwrap().logind, vec![("panel".to_owned(), 153)]);
    assert_eq!(*log.borrow(), vec![(true, String::new())]);
    assert_eq!(fx.writes(), 0);
}

#[test]
fn lightweight_poll_does_not_repeat_ddc_probes() {
    let fx = Fx::new();
    fx.start();
    let reads = fx.getvcp();
    fx.backend.queue_refresh(false);
    fx.pump(|| !fx.backend.busy());
    assert_eq!(fx.getvcp(), reads);
    assert_eq!(fx.display("DP-2")["brightness"]["supported"], true);
}

#[test]
fn no_background_polling_of_ddc_hardware() {
    let fx = Fx::new();
    fx.start();
    assert_eq!(fx.backend.poll_source(), None);
    assert!(fx.scheduler.pending.borrow().is_empty());
    assert_eq!(fx.probes(8), 1);
}

#[test]
fn unsupported_ddc_display_is_remembered_until_reconnected() {
    let fx = Fx::new();
    fx.start();
    assert_eq!(fx.display("HDMI-A-1")["brightness"]["supported"], false);
    let supported = fx.probes(12);
    for _ in 0..3 {
        fx.backend.refresh();
        fx.pump(|| !fx.backend.busy());
    }
    assert_eq!(fx.probes(8), 1);
    assert_eq!(fx.probes(12), supported + 3);
    assert_eq!(fx.display("HDMI-A-1")["method"], "unsupported");
    // Disabling and re-enabling the same output is not a reconnect.
    fx.set_output(1, |o| o["enabled"] = false.into());
    fx.backend.queue_refresh(false);
    fx.pump(|| !fx.backend.busy());
    fx.set_output(1, |o| o["enabled"] = true.into());
    fx.backend.queue_refresh(false);
    fx.pump(|| !fx.backend.busy());
    assert_eq!(fx.probes(8), 1);
    // A real unplug and replug gets exactly one fresh probe.
    let unplugged = fx.world.lock().unwrap().outputs.remove(1);
    fx.backend.queue_refresh(false);
    fx.pump(|| !fx.backend.busy());
    {
        let mut world = fx.world.lock().unwrap();
        world.outputs.push(unplugged);
        world.values.insert(8, HashMap::from([("10".into(), [40, 100])]));
    }
    fx.backend.queue_refresh(false);
    fx.pump(|| !fx.backend.busy());
    assert_eq!(fx.probes(8), 2);
    assert_eq!(fx.display("HDMI-A-1")["brightness"]["supported"], true);
}

#[test]
fn monitor_hotplug_signals_trigger_one_settled_refresh() {
    let fx = Fx::new();
    fx.start();
    assert!(fx.backend.has_monitor_watch());
    let before = fx.discoveries();
    let changed = fx.monitor_changed.borrow().clone().expect("watching GDK monitors");
    changed(); // items-changed
    changed(); // notify::geometry
    assert_eq!(fx.scheduler.delays(), vec![HOTPLUG_SETTLE_MS]);
    assert_eq!(fx.scheduler.cancelled.borrow().len(), 1);
    let id = fx.scheduler.ids()[0];
    fx.scheduler.fire(id);
    fx.pump(|| !fx.backend.busy() && fx.backend.poll_source().is_none());
    assert_eq!(fx.discoveries(), before + 1);
    assert_eq!(fx.probes(8), 1);
    fx.backend.close();
    assert!(*fx.monitors_closed.borrow());
    assert!(fx.monitor_changed.borrow().is_none());
}

#[test]
fn failed_discovery_disables_stale_controls() {
    let fx = Fx::new();
    fx.start();
    fx.world.lock().unwrap().output_failure = true;
    fx.backend.refresh();
    fx.pump(|| !fx.backend.busy());
    assert!(!fx.backend.ready());
    assert_eq!(fx.display("DP-2")["enabled"], false);
    assert_eq!(fx.display("DP-2")["error"], "Output discovery failed");
    let (log, callback) = recorder();
    assert!(!fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None));
    assert_eq!(log.borrow().len(), 1);
    assert_eq!(fx.writes(), 0);
}

#[test]
fn close_cancels_queued_requests_exactly_once() {
    let fx = Fx::new();
    fx.start();
    let (log, callback) = recorder();
    fx.backend.set_raw("DP-2", "brightness", 20, callback, None, None);
    fx.backend.close();
    fx.pump(|| !log.borrow().is_empty());
    assert_eq!(log.borrow().len(), 1);
    assert!(!log.borrow()[0].0);
    assert_eq!(log.borrow()[0].1, "Display controls were closed before the request completed.");
    fx.backend.join_worker();
    fx.backend.pump();
    assert_eq!(log.borrow().len(), 1);
}

#[test]
fn continuous_vcp_only_valid_values() {
    let parsed = parse_vcp("VCP 10 C 75 100\nVCP 12 C 48 255\n");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed["10"], (75, 100));
    assert_eq!(parsed["12"], (48, 255));
    assert!(parse_vcp("VCP 10 ERR\nVCP 12 NC 0x01\nVCP 10 C 0 0\nVCP 12 C 101 100").is_empty());
    assert_eq!(parse_vcp("  VCP 0x10 C 5 10  ")["10"], (5, 10));
}

#[test]
fn invalid_edid_cannot_supply_stable_identity() {
    assert!(edid_hash(b"not a monitor").is_err());
}

#[test]
fn bounded_subprocess_timeout_reports_error() {
    let error = run_command(&["/bin/sleep".into(), "5".into()], Duration::from_millis(50)).err().unwrap();
    assert!(error.contains("timeout"), "{error}");
    let error = run_command(&["/nonexistent/ddcutil".into()], Duration::from_secs(1)).err().unwrap();
    assert!(error.starts_with("Could not run ddcutil: [Errno 2]"), "{error}");
    let output = run_command(&["/bin/sh".into(), "-c".into(), "echo $LC_ALL; echo err >&2; exit 3".into()], Duration::from_secs(2)).unwrap();
    assert_eq!((output.status, output.stdout.as_str(), output.stderr.as_str()), (3, "C\n", "err\n"));
}

