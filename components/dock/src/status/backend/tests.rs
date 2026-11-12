//! Port of test_status_backend.py (fake jobs, fake scheduler, fake services;
//! no real commands, no session/system bus).

use super::*;
use crate::status::process::{ProcessJob, StatusJob};
use tessel_core::scheduler::FakeScheduler;

struct FakeJob {
    command: Vec<String>,
    on_line: LineHandler,
    on_done: DoneHandler,
    options: JobOptions,
    started: Cell<bool>,
    closed: Cell<bool>,
}

impl StatusJob for FakeJob {
    fn start(&self) { self.started.set(true) }
    fn close(&self) { self.closed.set(true) }
}

impl FakeJob {
    fn line(&self, value: Value) { (self.on_line)(&value.to_string()).unwrap() }
    fn finish(&self, error: Option<&str>) { (self.on_done)(error.map(String::from)) }
}

#[derive(Default)]
struct FakeClient {
    state: RefCell<Map<String, Value>>,
    starts: Cell<u32>,
    closes: Cell<u32>,
}
impl KeepAwakeSource for FakeClient {
    fn start(&self) { self.starts.set(self.starts.get() + 1) }
    fn state(&self) -> Map<String, Value> { self.state.borrow().clone() }
    fn close(&self) { self.closes.set(self.closes.get() + 1) }
}
impl DiscoverySource for FakeClient {
    fn start(&self) { self.starts.set(self.starts.get() + 1) }
    fn snapshot(&self) -> Map<String, Value> { self.state.borrow().clone() }
    fn close(&self) { self.closes.set(self.closes.get() + 1) }
}
impl CaptureSource for FakeClient {
    fn close(&self) { self.closes.set(self.closes.get() + 1) }
}
impl MediaSource for FakeClient {
    fn start(&self) { self.starts.set(self.starts.get() + 1) }
    fn action(&self, _command: &str) -> bool { true }
    fn icon(&self) -> Option<gio::Icon> { None }
    fn close(&self) { self.closes.set(self.closes.get() + 1) }
}
struct NoBluetooth;
impl BluetoothReader for NoBluetooth {
    fn read(&self, _done: Box<dyn FnOnce(Result<Value, String>)>) {}
    fn cancel(&self) {}
}

struct Fixture {
    backend: StatusBackend,
    scheduler: Rc<FakeScheduler>,
    created: Rc<RefCell<Vec<Rc<FakeJob>>>>,
    errors: Rc<RefCell<Vec<String>>>,
    changed: Rc<Cell<u32>>,
    keep_awake: Rc<FakeClient>,
    discovery: Rc<FakeClient>,
    discovery_changed: Rc<RefCell<Option<Notify>>>,
    /// Every media status service created, with the callback it publishes through.
    media: Rc<RefCell<Vec<(Rc<FakeClient>, Rc<dyn Fn(Status)>)>>>,
}

impl Fixture {
    fn new() -> Self { Self::with_media(false) }

    fn with_media(media_enabled: bool) -> Self {
        let scheduler = Rc::new(FakeScheduler::default());
        let created: Rc<RefCell<Vec<Rc<FakeJob>>>> = Rc::default();
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let changed = Rc::new(Cell::new(0));
        let keep_awake = Rc::new(FakeClient::default());
        keep_awake.state.replace(json!({"text": "eye", "class": "inactive"}).as_object().unwrap().clone());
        let discovery = Rc::new(FakeClient::default());
        let discovery_changed: Rc<RefCell<Option<Notify>>> = Rc::default();
        let jobs = created.clone();
        let factory: JobFactory = Rc::new(move |command, on_line, on_done, options| {
            let job = Rc::new(FakeJob { command, on_line, on_done, options, started: Cell::new(false), closed: Cell::new(false) });
            jobs.borrow_mut().push(job.clone());
            job as Rc<dyn StatusJob>
        });
        let media: Rc<RefCell<Vec<(Rc<FakeClient>, Rc<dyn Fn(Status)>)>>> = Rc::default();
        let (k, d, dc, m) = (keep_awake.clone(), discovery.clone(), discovery_changed.clone(), media.clone());
        let services = Services {
            keep_awake: Rc::new(move |_changed| k.clone() as Rc<dyn KeepAwakeSource>),
            discovery: Rc::new(move |changed| { dc.replace(Some(changed)); d.clone() as Rc<dyn DiscoverySource> }),
            capture: Rc::new(|_changed| Rc::new(FakeClient::default()) as Rc<dyn CaptureSource>),
            media: Rc::new(move |changed| {
                let client = Rc::new(FakeClient::default());
                m.borrow_mut().push((client.clone(), changed));
                client as Rc<dyn MediaSource>
            }),
            bluetooth: Rc::new(NoBluetooth),
            tessel_capture_exists: Rc::new(|| false),
        };
        let (e, c) = (errors.clone(), changed.clone());
        let backend = StatusBackend::new(move || c.set(c.get() + 1), Some(Box::new(move |error: &str| e.borrow_mut().push(error.into()))),
            StatusOptions { program: Rc::new(|name| std::path::Path::new("/test bin").join(name)), job_factory: factory, scheduler: scheduler.clone(), services, on_show_desktop: None,
                           media: media_enabled });
        Self { backend, scheduler, created, errors, changed, keep_awake, discovery, discovery_changed, media }
    }

    fn job(&self, name: &str) -> Rc<FakeJob> {
        let jobs = self.backend.0.jobs.borrow();
        let (_, job) = jobs.get(name).unwrap_or_else(|| panic!("no job {name}"));
        let pointer = Rc::as_ptr(job) as *const ();
        self.created.borrow().iter().find(|candidate| Rc::as_ptr(candidate) as *const () == pointer).unwrap().clone()
    }

    fn status(&self, name: &str) -> Status { self.backend.get(name).unwrap() }

    fn delay(&self, id: u64) -> u32 {
        self.scheduler.pending.borrow().iter().find(|entry| entry.0 == id).map(|entry| entry.1).unwrap()
    }
}

fn classes(status: &Status) -> Vec<String> {
    status["classes"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect()
}

#[test]
fn discovery_observes_once_without_launching_another_status_process() {
    let f = Fixture::new();
    f.backend.refresh(Some("discovery"));
    f.backend.refresh(Some("discovery"));
    assert_eq!(f.discovery.starts.get(), 1);
    assert!(f.created.borrow().is_empty());
    f.discovery.state.replace(json!({"text": "devices", "tooltip": "Discover Network Devices: Enabled",
                                     "class": "active", "installed": true, "ready": true}).as_object().unwrap().clone());
    let notify = f.discovery_changed.borrow().clone().unwrap();
    notify();
    assert_eq!(f.status("discovery")["visible"], true);
    f.discovery.state.borrow_mut().insert("installed".into(), false.into());
    notify();
    assert_eq!(f.status("discovery")["visible"], false);
    f.backend.close();
    assert_eq!(f.discovery.closes.get(), 1);
}

#[test]
fn start_has_one_shared_fixed_monitor_per_status_and_does_not_launch_apps() {
    let f = Fixture::new();
    f.backend.start();
    f.backend.start();
    assert_eq!(f.created.borrow().len(), 10);
    assert_eq!(f.backend.job_names().len(), 10);
    assert_eq!(f.job("notifications").command, ["/test bin/tessel-notifications-status"]);
    assert_eq!(f.job("network").command, ["/test bin/tessel-network-status"]);
    assert_eq!(f.job("capslock").command, ["/test bin/tessel-capslock-status"]);
    assert_eq!(f.job("keyboard").command, ["/test bin/tessel-keyboard-status"]);
    assert_eq!(f.job("weather").command, ["/test bin/tessel-weather-status", "--watch"]);
    assert!(f.job("weather").options.stream);
    assert!(f.job("audio").options == JobOptions { stream: false, timeout: 10000 });
    assert!(!f.created.borrow().iter().any(|job| job.command.iter().any(|a| a == "set-volume")));
    assert!(f.created.borrow().iter().all(|job| job.started.get()));
}

#[test]
fn updates_are_deduplicated_and_callbacks_coalesced() {
    let f = Fixture::new();
    f.backend.start();
    let job = f.job("battery");
    let value = json!({"text": "full", "tooltip": "Fully charged", "class": ["full", "plugged"]});
    // start() itself queued "changed" (capture idle, show desktop); flush it first.
    if let Some(id) = f.backend.source("changed") { f.scheduler.fire(id); }
    let before = f.changed.get();
    job.line(value.clone());
    let source = f.backend.source("changed").unwrap();
    job.line(value.clone());
    assert_eq!(f.backend.source("changed"), Some(source));
    f.scheduler.fire(source);
    assert_eq!(f.changed.get(), before + 1);
    job.line(value);
    assert_eq!(f.backend.source("changed"), None);
}

#[test]
fn capslock_hidden_when_empty_and_script_markup_is_plain() {
    let f = Fixture::new();
    f.backend.start();
    let caps = f.job("capslock");
    caps.line(json!({"text": "", "tooltip": "Caps Lock: off"}));
    assert_eq!(f.status("capslock")["visible"], false);
    caps.line(json!({"text": "⇪", "class": "locked"}));
    assert_eq!(f.status("capslock")["visible"], true);
    let value = indicator(&json!({"text": "<span alpha='50%'>eye</span>", "class": "INACTIVE"}), false).unwrap();
    assert_eq!(value["text"], "eye");
    assert_eq!(classes(&value), ["inactive"]);
}

#[test]
fn protocol_rejects_bad_types() {
    for value in [json!([]), json!({}), json!({"text": 1}), json!({"text": "x", "tooltip": {}}), json!({"text": "x", "class": [1]})] {
        assert!(indicator(&value, false).is_err(), "{value}");
    }
    assert_eq!(indicator(&json!({}), false).unwrap_err(), "expected indicator JSON with text");
    assert_eq!(indicator(&json!({"text": "x", "tooltip": 1}), false).unwrap_err(), "invalid indicator tooltip");
    assert_eq!(indicator(&json!({"text": "x", "class": {}}), false).unwrap_err(), "invalid indicator classes");
    // Invalid class names are dropped, not rejected; key order is Python's dict order.
    let value = indicator(&json!({"text": "a &amp; b", "class": ["ok", "bad name", ""]}), false).unwrap();
    assert_eq!(serde_json::to_string(&value).unwrap(),
               r#"{"text":"a & b","tooltip":"","classes":["ok"],"class":["ok"],"visible":true}"#);
}

#[test]
fn audio_volume_and_muted_icons() {
    assert_eq!(audio_status("Volume: 0.42").unwrap()["tooltip"], "Volume: 42%");
    assert_eq!(classes(&audio_status("Volume: 0.42 [MUTED]").unwrap()), ["muted"]);
    assert_eq!(audio_status("Volume: 0.42 [MUTED]").unwrap()["tooltip"], "Volume: 42% (muted)");
    assert_ne!(audio_status("Volume: 0.00").unwrap()["text"], audio_status("Volume: 0.99").unwrap()["text"]);
    for invalid in ["broken", "Volume: nan", "Volume: -1", "Volume: 3000"] {
        assert!(audio_status(invalid).is_err(), "{invalid}");
    }
    assert_eq!(audio_status("Volume: 3000").unwrap_err(), "invalid PipeWire volume");
    assert_eq!(audio_status("broken").unwrap_err(), "invalid PipeWire volume response");
    // round() is banker's rounding like Python's.
    assert_eq!(audio_status("Volume: 0.125").unwrap()["tooltip"], "Volume: 12%");
    assert_eq!(audio_status("Volume: 1.00  ").unwrap()["text"], "\u{f028}");
}

#[test]
fn bluetooth_multi_adapter_and_device_status() {
    let mut objects = json!({"/a": {"org.bluez.Adapter1": {"Powered": false}},
                             "/b": {"org.bluez.Adapter1": {"Powered": true}},
                             "/b/d": {"org.bluez.Device1": {"Connected": true, "Alias": "Headphones"}}});
    let value = bluetooth_status(&objects);
    assert_eq!(classes(&value), ["connected"]);
    assert!(value["tooltip"].as_str().unwrap().contains("Headphones"));
    assert_eq!(value["tooltip"], "Bluetooth: connected\nHeadphones");
    objects.as_object_mut().unwrap().remove("/b/d");
    assert_eq!(classes(&bluetooth_status(&objects)), ["on"]);
    assert_eq!(classes(&bluetooth_status(&json!({}))), ["unavailable"]);
    assert_eq!(bluetooth_status(&json!({}))["tooltip"], "Bluetooth: no adapter");
}

#[test]
fn stream_exit_marks_unavailable_then_restarts() {
    let f = Fixture::new();
    f.backend.start();
    let job = f.job("power");
    job.line(json!({"text": "power"}));
    job.finish(None);
    assert_eq!(classes(&f.status("power")), ["unavailable"]);
    assert_eq!(f.status("power")["tooltip"], "Power: status monitor stopped");
    assert_eq!(*f.errors.borrow(), ["Power: status monitor stopped"]);
    let retry = f.backend.source("power").unwrap();
    assert_eq!(f.delay(retry), 3000);
    f.scheduler.fire(retry);
    assert!(!Rc::ptr_eq(&f.job("power"), &job));
    job.line(json!({"text": "stale"}));
    assert_ne!(f.status("power")["text"], "stale");
    // The same error again is not reported twice.
    f.job("power").finish(None);
    assert_eq!(f.errors.borrow().len(), 1);
}

#[test]
fn one_shot_errors_retry_at_interval_and_no_status_is_reported() {
    let f = Fixture::new();
    f.backend.refresh(Some("audio"));
    let job = f.job("audio");
    assert_eq!(job.command, ["/usr/bin/wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"]);
    job.finish(None);
    assert_eq!(f.status("audio")["tooltip"], "Audio: no status returned");
    assert_eq!(f.delay(f.backend.source("audio").unwrap()), 2000);
    f.scheduler.fire(f.backend.source("audio").unwrap());
    let job = f.job("audio");
    assert!((job.on_line)("broken").is_err());
    (job.on_line)("Volume: 0.50").unwrap();
    job.finish(None);
    assert_eq!(f.status("audio")["tooltip"], "Volume: 50%");
    assert_eq!(f.delay(f.backend.source("audio").unwrap()), 2000);
}

#[test]
fn weather_stream_preserves_units_and_updates_without_respawning() {
    let f = Fixture::new();
    f.backend.refresh(Some("weather"));
    let job = f.job("weather");
    job.line(json!({"text": "64°F", "tooltip": "Sunny\nTomorrow: clear"}));
    assert_eq!(f.status("weather")["text"], "64°F");
    assert!(f.status("weather")["tooltip"].as_str().unwrap().contains("Tomorrow"));
    job.line(json!({"text": "18°C", "tooltip": "Changed location and units"}));
    assert_eq!(f.status("weather")["text"], "18°C");
    assert!(Rc::ptr_eq(&f.job("weather"), &job));
    assert_eq!(f.backend.source("weather"), None);
}

#[test]
fn actions_only_allow_fixed_scripts_and_refresh_after_completion() {
    let f = Fixture::new();
    assert!(!f.backend.action("$(bad-command)"));
    assert!(f.created.borrow().is_empty());
    assert!(f.backend.action("idle_inhibitor"));
    let job = f.job("action-idle_inhibitor");
    assert_eq!(job.command, ["/test bin/tessel-keep-awake", "--toggle"]);
    assert!(!f.backend.action("idle_inhibitor"));
    job.finish(None);
    assert!(!f.backend.job_names().contains(&"idle_inhibitor".to_owned()));
    assert!(!f.backend.job_names().contains(&"action-idle_inhibitor".to_owned()));
    assert_eq!(f.keep_awake.starts.get(), 1);
    // Action failures are reported with the title.
    assert!(f.backend.action("idle_inhibitor"));
    f.job("action-idle_inhibitor").finish(Some("boom"));
    assert_eq!(*f.errors.borrow(), ["Idle Inhibitor: boom"]);
}

#[test]
fn keep_awake_changes_do_not_spawn_status_processes_or_poll() {
    let f = Fixture::new();
    f.backend.refresh(Some("idle_inhibitor"));
    for active in [true, false, true] {
        f.keep_awake.state.replace(json!({"text": "eye", "class": if active { "active" } else { "inactive" }}).as_object().unwrap().clone());
        f.backend.keep_awake_changed();
        assert_eq!(classes(&f.status("idle_inhibitor")), [if active { "active" } else { "inactive" }]);
    }
    f.keep_awake.state.replace(json!({"ready": false, "error": "Service stopped"}).as_object().unwrap().clone());
    f.backend.keep_awake_changed();
    assert!(classes(&f.status("idle_inhibitor")).contains(&"unavailable".to_owned()));
    assert_eq!(f.status("idle_inhibitor")["tooltip"], "Service stopped");
    f.backend.refresh(Some("idle_inhibitor"));
    assert_eq!(f.keep_awake.starts.get(), 1);
    assert!(f.created.borrow().is_empty());
    assert_eq!(f.backend.source("idle_inhibitor"), None);
    f.backend.close();
    assert_eq!(f.keep_awake.closes.get(), 1);
}

#[test]
fn show_desktop_calls_native_owner_and_keeps_state_without_script_polling() {
    let f = Fixture::new();
    assert!(!f.backend.action("showdesktop"));
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    f.backend.set_on_show_desktop(Some(Rc::new(move || { counter.set(counter.get() + 1); true })));
    assert!(f.backend.action("showdesktop"));
    assert_eq!(calls.get(), 1);
    f.backend.set_show_desktop(true);
    f.backend.refresh(Some("showdesktop"));
    assert!(f.created.borrow().is_empty());
    assert_eq!(f.status("showdesktop")["tooltip"], "Restore windows");
    assert!(classes(&f.status("showdesktop")).contains(&"active".to_owned()));
    assert!(classes(&f.status("showdesktop")).contains(&"solid".to_owned()));
    f.backend.set_show_desktop(false);
    assert_eq!(f.status("showdesktop")["tooltip"], "Show desktop");
    assert!(!classes(&f.status("showdesktop")).contains(&"solid".to_owned()));
}

#[test]
fn scroll_serializes_and_coalesces_burst_without_shell() {
    let f = Fixture::new();
    f.backend.audio_scroll(1.0);
    let first = f.job("action-audio");
    assert_eq!(first.command.last().unwrap(), "5%+");
    assert!(first.command.iter().any(|a| a == "1.0"));
    assert_eq!(first.command, ["/usr/bin/wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"]);
    f.backend.audio_scroll(1.0);
    f.backend.audio_scroll(1.0);
    assert!(Rc::ptr_eq(&f.job("action-audio"), &first));
    first.finish(None);
    let second = f.job("action-audio");
    assert_eq!(second.command.last().unwrap(), "10%+");
    second.finish(None);
    f.backend.audio_scroll(-1.0);
    assert_eq!(f.job("action-audio").command.last().unwrap(), "5%-");
    // Bursts are clamped to ±20 steps.
    f.backend.audio_scroll(100.0);
    f.job("action-audio").finish(None);
    assert_eq!(f.job("action-audio").command.last().unwrap(), "100%+");
}

#[test]
fn close_cancels_all_monitors_timers_and_ignores_late_results() {
    let f = Fixture::new();
    f.backend.start();
    let job = f.job("battery");
    job.line(json!({"text": "before"}));
    f.backend.close();
    assert!(f.created.borrow().iter().all(|job| job.closed.get()));
    assert!(f.scheduler.pending.borrow().is_empty());
    job.line(json!({"text": "late"}));
    job.finish(Some("late error"));
    f.backend.refresh(None);
    assert_eq!(f.status("battery")["text"], "before");
    assert!(f.errors.borrow().is_empty());
    assert_eq!(f.created.borrow().len(), 10);
}

#[test]
fn initial_status_and_capture_indicator() {
    let f = Fixture::new();
    assert_eq!(f.status("power")["tooltip"], "Power: waiting for status");
    assert_eq!(f.status("brightness")["tooltip"], "Displays: waiting for status");
    assert_eq!(f.status("discovery")["tooltip"], "Discover Network Devices: waiting for status");
    assert_eq!(f.status("capslock")["visible"], false);
    assert_eq!(f.status("idle_inhibitor")["tooltip"], "Idle Inhibitor: waiting for status");
    f.backend.start();
    let record = f.status("screen_record");
    assert_eq!(record["state"], "idle");
    assert_eq!(record["visible"], false);
    assert_eq!(record["tooltip"], "Screen Record · click for presets · right-click for settings");
    f.backend.capture_changed("recording");
    let record = f.status("screen_record");
    assert_eq!((record["text"].as_str().unwrap(), classes(&record)), ("\u{f04d}", vec!["recording".to_owned()]));
    assert_eq!(record["tooltip"], "Recording · click to stop and save");
}

#[test]
fn json_errors_read_like_python() {
    let error = serde_json::from_str::<Value>("not JSON").unwrap_err();
    assert_eq!(python_json_error("not JSON", &error), "Expecting value: line 1 column 1 (char 0)");
    let error = serde_json::from_str::<Value>("").unwrap_err();
    assert_eq!(python_json_error("", &error), "Expecting value: line 1 column 1 (char 0)");
    let error = serde_json::from_str::<Value>("{} x").unwrap_err();
    assert_eq!(python_json_error("{} x", &error), "Extra data: line 1 column 4 (char 3)");
}

// ProcessTests: real child processes (sh -c), never a status program.

fn run_job(code: &str, timeout: u32, json_lines: bool) -> (Vec<String>, Option<String>, bool) {
    let context = glib::MainContext::new();
    context.with_thread_default(|| {
        let lines: Rc<RefCell<Vec<String>>> = Rc::default();
        let results: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let (l, r) = (lines.clone(), results.clone());
        let on_line: LineHandler = Rc::new(move |text: &str| {
            if json_lines {
                serde_json::from_str::<Value>(text).map_err(|e| python_json_error(text, &e))?;
            }
            l.borrow_mut().push(text.to_owned());
            Ok(())
        });
        let job = ProcessJob::new(vec!["/bin/sh".into(), "-c".into(), code.into()], on_line,
                                  Rc::new(move |error| r.borrow_mut().push(error)), JobOptions { stream: false, timeout });
        job.start();
        assert!(tessel_core::timer::wait_for(&context, std::time::Duration::from_secs(3), || !results.borrow().is_empty()),
                "process did not finish within the test deadline");
        assert_eq!(results.borrow().len(), 1);
        let closed = job.closed();
        job.close();
        let lines = lines.borrow().clone();
        let result = results.borrow()[0].clone();
        (lines, result, closed)
    }).unwrap()
}

#[test]
fn process_unicode_and_final_unterminated_line_are_drained_before_exit() {
    let (lines, error, closed) = run_job("printf '☀\\nlast line'", 1000, false);
    assert_eq!(lines, ["☀", "last line"]);
    assert_eq!(error, None);
    assert!(closed);
}

#[test]
fn process_timeout_terminates_a_silent_child() {
    let (lines, error, closed) = run_job("exec sleep 10", 50, false);
    assert!(lines.is_empty());
    assert!(error.unwrap().contains("timed out"));
    assert!(closed);
}

#[test]
fn process_oversized_unterminated_line_is_rejected() {
    let (lines, error, _) = run_job("head -c 70000 /dev/zero | tr '\\0' x", 1000, false);
    assert!(lines.is_empty());
    assert!(error.unwrap().contains("oversized"));
}

#[test]
fn process_invalid_json_finishes_cleanly_for_restart() {
    let (_, error, _) = run_job("echo 'not JSON'", 1000, true);
    assert_eq!(error.as_deref(), Some("Expecting value: line 1 column 1 (char 0)"));
}

#[test]
fn process_failure_and_invalid_utf8() {
    let (_, error, _) = run_job("exit 3", 1000, false);
    assert_eq!(error.as_deref(), Some("status command failed"));
    let (_, error, _) = run_job("printf '\\377\\n'", 1000, false);
    assert_eq!(error.as_deref(), Some("'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"));
}

#[test]
fn errors_keep_python_dict_insertion_order() {
    // Python: `_set` pops `errors[name]`, `_failed` then re-inserts it, so a
    // repeated failure moves to the end and a good status removes it.
    let f = Fixture::new();
    assert!(f.backend.errors().is_empty());
    f.backend.failed("power", "status monitor stopped");
    f.backend.failed("audio", "no status returned");
    f.backend.failed("bluetooth", "no bus");
    let names = |f: &Fixture| f.backend.errors().into_iter().map(|(name, _)| name).collect::<Vec<_>>();
    assert_eq!(names(&f), ["power", "audio", "bluetooth"]);
    f.backend.failed("power", "status monitor stopped");
    assert_eq!(names(&f), ["audio", "bluetooth", "power"]);
    assert_eq!(f.backend.errors()[2], ("power".to_owned(), "Power: status monitor stopped".to_owned()));
    // The same error again is still reported once.
    assert_eq!(f.errors.borrow().iter().filter(|e| e.starts_with("Power:")).count(), 1);
    f.backend.set("audio", indicator(&json!({"text": "vol"}), false).unwrap());
    assert_eq!(names(&f), ["bluetooth", "power"]);
    // Real flow: a stream that exits is recorded after the earlier failures.
    f.backend.start();
    let job = f.job("network");
    job.finish(None);
    assert_eq!(names(&f).last().map(String::as_str), Some("network"));
}

fn playing(title: &str) -> Status {
    json!({"text": "\u{f001}", "tooltip": format!("Media\n{title}"), "visible": true, "title": title}).as_object().unwrap().clone()
}

#[test]
fn media_is_off_by_default_and_never_watched() {
    assert!(!StatusOptions::default().media);
    let f = Fixture::new();
    f.backend.start();
    assert!(f.media.borrow().is_empty(), "no media service while the switch is off");
    assert!(!f.backend.media_enabled() && !f.backend.media_running());
    assert_eq!(f.status("media")["classes"], json!(["unavailable"]));
    assert!(!f.backend.action("media-toggle"));
    assert!(f.backend.media_icon().is_none());
}

#[test]
fn media_on_at_start_starts_one_service_and_close_stops_it() {
    let f = Fixture::with_media(true);
    f.backend.start();
    assert_eq!(f.media.borrow().len(), 1);
    let (client, changed) = f.media.borrow()[0].clone();
    assert_eq!(client.starts.get(), 1);
    changed(playing("Song"));
    assert_eq!(f.status("media")["title"], "Song");
    assert!(f.backend.action("media-toggle"));
    f.backend.close();
    assert_eq!(client.closes.get(), 1);
}

#[test]
fn media_switch_starts_and_stops_the_service_live_and_repeatedly() {
    let f = Fixture::new();
    // Before start() the switch is only remembered.
    f.backend.set_media_enabled(true);
    assert!(f.media.borrow().is_empty());
    f.backend.set_media_enabled(false);
    f.backend.start();
    assert!(f.media.borrow().is_empty());
    let mut previous: Option<Rc<dyn Fn(Status)>> = None;
    for round in 0..3 {
        f.backend.set_media_enabled(true);
        f.backend.set_media_enabled(true);
        assert_eq!(f.media.borrow().len(), round + 1, "one fresh service per switch-on");
        let (client, changed) = f.media.borrow()[round].clone();
        assert_eq!((client.starts.get(), client.closes.get()), (1, 0));
        assert!(f.backend.media_enabled() && f.backend.media_running());
        if let Some(stale) = previous.take() {
            // A previous, closed service cannot publish into the new one.
            stale(playing("Stale"));
            assert!(f.status("media").get("title").is_none());
        }
        changed(playing("Song"));
        assert_eq!(f.status("media")["title"], "Song");
        assert!(f.backend.action("media-toggle"));
        f.backend.set_media_enabled(false);
        f.backend.set_media_enabled(false);
        assert_eq!(client.closes.get(), 1, "switching off closes the service once");
        assert!(!f.backend.media_running() && f.backend.media_icon().is_none());
        // The published status is cleared and a late callback cannot bring it back.
        assert!(f.status("media").get("title").is_none());
        assert_eq!(f.status("media")["classes"], json!(["unavailable"]));
        changed(playing("Late"));
        assert!(f.status("media").get("title").is_none());
        assert!(!f.backend.action("media-next"));
        previous = Some(changed);
    }
    assert!(f.media.borrow().iter().all(|(client, _)| client.starts.get() == 1 && client.closes.get() == 1));
    f.backend.close();
    f.backend.set_media_enabled(true);
    assert_eq!(f.media.borrow().len(), 3, "a closed backend never starts media again");
}
