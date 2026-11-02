//! test_keep_awake.py TimerTests + timer.json format/load rules.

use tessel_keep_awake::timed::{load_timer, timer_text, Inhibit, TimedBackend};
use serde_json::{json, Value};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Default)]
struct Plain {
    requested: Cell<bool>,
    started: Cell<bool>,
    changes: Cell<u32>,
}

impl Inhibit for Plain {
    fn start(&self) { self.started.set(true) }
    fn set_enabled(&self, enabled: bool) -> bool { self.requested.set(enabled); true }
    fn requested(&self) -> bool { self.requested.get() }
    fn on_changed(&self) { self.changes.set(self.changes.get() + 1) }
    fn close(&self) {}
}

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    now: Rc<Cell<f64>>,
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn fixture(name: &str) -> Fixture {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("keep-awake-timer-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // The state dir does not exist yet: save() must create it (mode 0700).
    let path = dir.join("tessel-keep-awake").join("timer.json");
    Fixture { _dir: TempDir(dir), path, now: Rc::new(Cell::new(1000.0)) }
}

impl Fixture {
    fn timed(&self) -> (TimedBackend, Rc<Plain>) {
        let plain = Rc::new(Plain::default());
        let now = self.now.clone();
        (TimedBackend::new(plain.clone(), Some(self.path.clone()), Some(Box::new(move || now.get()))), plain)
    }
    fn file(&self) -> Value { serde_json::from_str(&std::fs::read_to_string(&self.path).unwrap()).unwrap() }
    fn write(&self, value: Value) {
        std::fs::create_dir_all(self.path.parent().unwrap()).unwrap();
        std::fs::write(&self.path, value.to_string()).unwrap();
    }
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn duration_sets_and_persists_end_time_then_releases() {
    let f = fixture("persist");
    let (backend, plain) = f.timed();
    backend.set_duration(30).unwrap();
    backend.set_enabled(true);
    assert_eq!(backend.until(), Some(1000.0 + 1800.0));
    assert_eq!(f.file(), json!({"duration": 30, "until": 2800.0}));
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "{\"duration\": 30, \"until\": 2800.0}\n");
    assert_eq!(mode(f.path.parent().unwrap()), 0o700);
    assert_eq!(mode(&f.path), 0o600);
    assert!(backend.has_timer());
    f.now.set(2800.0);
    backend.check_now();
    assert!(!plain.requested.get());
    assert_eq!(backend.until(), None);
    assert_eq!(f.file()["until"], Value::Null);
    // No temporary files are left behind.
    let names: Vec<_> = std::fs::read_dir(f.path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("timer.json")]);
    backend.close();
}

fn unexpired_timer_resumes_and_expired_one_does_not() {
    let f = fixture("resume");
    f.write(json!({"duration": 60, "until": 1500}));
    let (backend, plain) = f.timed();
    backend.start();
    assert!(plain.started.get());
    assert!(plain.requested.get());
    assert_eq!(backend.remaining(), Some(500.0));
    assert!(backend.has_timer());
    f.write(json!({"duration": 60, "until": 900}));
    let (later, later_plain) = f.timed();
    later.start();
    assert!(!later_plain.requested.get());
    assert_eq!(later.duration(), 60);
    assert_eq!(later.until(), None);
    backend.close();
    later.close();
}

fn until_turned_off_has_no_end_and_rejects_unknown_durations() {
    let f = fixture("forever");
    let (backend, plain) = f.timed();
    backend.set_enabled(true);
    assert_eq!(backend.until(), None);
    backend.set_duration(120).unwrap();
    assert_eq!(backend.until(), Some(1000.0 + 7200.0));
    backend.set_duration(0).unwrap();
    assert_eq!(backend.until(), None);
    assert_eq!(backend.set_duration(45), Err("Unsupported Keep Awake duration.".into()));
    assert_eq!(plain.changes.get(), 2, "set_duration notifies once per valid call");
    backend.close();
}

fn timer_forgotten_when_turned_off_elsewhere() {
    let f = fixture("elsewhere");
    let (backend, plain) = f.timed();
    backend.set_duration(30).unwrap();
    backend.set_enabled(true);
    plain.requested.set(false); // e.g. logind denied the inhibitor
    f.now.set(1100.0);
    backend.check_now();
    assert_eq!(backend.until(), None);
    assert_eq!(f.file(), json!({"duration": 30, "until": null}));
    assert!(!backend.has_timer());
    backend.close();
}

fn real_timeout_expires() {
    let f = fixture("expire");
    f.write(json!({"duration": 30, "until": 1000.5}));
    let (backend, plain) = f.timed();
    backend.start(); // resumes; 0.5 s left -> a 1 s GLib timer
    assert!(plain.requested.get());
    f.now.set(1001.0);
    let context = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while plain.requested.get() && std::time::Instant::now() < end {
        context.iteration(true);
    }
    assert!(!plain.requested.get(), "timer never expired");
    assert_eq!(f.file(), json!({"duration": 30, "until": null}));
    backend.close();
}

fn load_rules() {
    let f = fixture("load");
    let path = &f.path;
    let write = |text: &str| { std::fs::create_dir_all(path.parent().unwrap()).unwrap(); std::fs::write(path, text).unwrap() };
    write("{\"duration\": 45, \"until\": 5000}");
    assert_eq!(load_timer(path, 1000.0), (0, Some(5000.0)));
    write("{\"duration\": 120, \"until\": true}");
    assert_eq!(load_timer(path, 1000.0), (120, None));
    write("{\"duration\": 30, \"until\": \"5000\"}");
    assert_eq!(load_timer(path, 1000.0), (30, None));
    write("[1, 2]");
    assert_eq!(load_timer(path, 1000.0), (0, None));
    write("not json");
    assert_eq!(load_timer(path, 1000.0), (0, None));
    std::fs::remove_file(path).unwrap();
    assert_eq!(load_timer(path, 1000.0), (0, None));
    assert_eq!(timer_text(0, None), "{\"duration\": 0, \"until\": null}\n");
    assert_eq!(timer_text(60, Some(1730000000.25)), "{\"duration\": 60, \"until\": 1730000000.25}\n");
}

#[test]
fn timer() {
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    duration_sets_and_persists_end_time_then_releases();
    unexpired_timer_resumes_and_expired_one_does_not();
    until_turned_off_has_no_end_and_rejects_unknown_durations();
    timer_forgotten_when_turned_off_elsewhere();
    real_timeout_expires();
    load_rules();
}
