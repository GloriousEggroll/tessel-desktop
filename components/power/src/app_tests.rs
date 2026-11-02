//! Port of test_power_app.py: pending automatic actions cancel on input,
//! policy changes and inhibition. Everything is fake.

use crate::app::*;
use crate::config::{defaults, ConfigError};
use crate::fakes::FakeSystem;
use tessel_core::scheduler::FakeScheduler;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Default)]
pub struct FakeRunner {
    pub runs: RefCell<Vec<(Vec<String>, Option<Box<dyn FnOnce(Exit)>>)>>,
    pub fail: RefCell<Option<String>>,
    pub missing: Cell<bool>,
}

impl FakeRunner {
    fn argv(&self) -> Vec<Vec<String>> {
        self.runs.borrow().iter().map(|r| r.0.clone()).collect()
    }
    fn finish(&self, index: usize, exit: Exit) {
        let done = self.runs.borrow_mut()[index].1.take().unwrap();
        done(exit);
    }
}

impl Runner for FakeRunner {
    fn run(&self, argv: &[String], _capture: bool, done: Box<dyn FnOnce(Exit)>) -> Result<(), String> {
        if let Some(error) = self.fail.borrow().clone() {
            return Err(error);
        }
        self.runs.borrow_mut().push((argv.to_vec(), Some(done)));
        Ok(())
    }
    fn which(&self, name: &str) -> Option<PathBuf> {
        (!self.missing.get()).then(|| PathBuf::from(format!("/usr/bin/{name}")))
    }
}

pub struct FakeIdle {
    pub token: RefCell<String>,
    pub allow: Cell<bool>,
    pub started: RefCell<Vec<Value>>,
    pub updated: RefCell<Vec<Value>>,
}

impl IdleControl for FakeIdle {
    fn ready(&self) -> bool { true }
    fn token(&self) -> String { self.token.borrow().clone() }
    fn error(&self) -> String { String::new() }
    fn allows(&self, _action: &str, token: &str) -> bool { self.allow.get() && token == *self.token.borrow() }
    fn start(&self, settings: &Value) { self.started.borrow_mut().push(settings.clone()); }
    fn update(&self, settings: &Value) { self.updated.borrow_mut().push(settings.clone()); }
    fn close(&self) {}
}

#[derive(Default)]
struct FakeDimmer {
    dims: Cell<usize>,
    restores: Cell<usize>,
}

impl Dimmer for FakeDimmer {
    fn dim(&self) -> bool { self.dims.set(self.dims.get() + 1); true }
    fn restore(&self) { self.restores.set(self.restores.get() + 1); }
    fn close(&self) {}
}

struct Fx {
    app: App,
    backend: Rc<FakeSystem>,
    idle: Rc<FakeIdle>,
    runner: Rc<FakeRunner>,
    scheduler: Rc<FakeScheduler>,
    dimmer: Rc<FakeDimmer>,
    saved: Rc<RefCell<Vec<Value>>>,
    clock: Rc<Cell<f64>>,
    root: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root.parent().unwrap());
    }
}

fn fixture(name: &str) -> Fx {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("power-app-{}-{name}/test suite", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let runner = Rc::new(FakeRunner::default());
    let scheduler = Rc::new(FakeScheduler::default());
    let saved = Rc::new(RefCell::new(Vec::new()));
    let clock = Rc::new(Cell::new(100.0));
    let (s, c) = (saved.clone(), clock.clone());
    let services = Services {
        runner: runner.clone(), scheduler: scheduler.clone(), monotonic: Box::new(move || c.get()),
        saver: Box::new(move |v: &Value| -> Result<(), ConfigError> { s.borrow_mut().push(v.clone()); Ok(()) }),
        scripts_root: root.clone(), python: "/usr/bin/python3".into(),
    };
    let app = App::new(services);
    let backend = FakeSystem::ready();
    backend.set(|s| s.has_battery = true);
    let idle = Rc::new(FakeIdle { token: RefCell::new("current".into()), allow: Cell::new(true), started: RefCell::default(),
                                  updated: RefCell::default() });
    let dimmer = Rc::new(FakeDimmer::default());
    app.0.backend.replace(Some(backend.clone()));
    app.0.idle.replace(Some(idle.clone()));
    app.0.dimmer.replace(Some(dimmer.clone()));
    app.0.config_valid.set(true);
    Fx { app, backend, idle, runner, scheduler, dimmer, saved, clock, root }
}

const LOCK: [&str; 4] = ["systemctl", "--user", "start", "tessel-lock.service"];

#[test]
fn settings_editor_remains_available_without_optional_profiles() {
    let f = fixture("editor");
    f.backend.set(|s| s.profiles_ready = false);
    let state = f.app.settings_state();
    assert_eq!(state["ready"], true);
    assert_eq!(state["profiles_ready"], false);
    assert_eq!(state["settings"], defaults());
    // "ready" keeps its health() position; "settings" comes last.
    let keys: Vec<&String> = state.as_object().unwrap().keys().collect();
    assert_eq!(keys.first().map(|k| k.as_str()), Some("ready"));
    assert_eq!(keys.last().map(|k| k.as_str()), Some("settings"));
}

#[test]
fn health_report_keys_match_python() {
    let f = fixture("health");
    let report = f.app.health();
    let keys: Vec<&str> = report.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["ready", "profiles_ready", "power_ready", "sleep_ready", "idle_ready", "has_battery", "active_profile",
                      "batteries", "charge_limit", "lid_handled", "power_key_handled", "error"]);
    assert_eq!(report["ready"], true);
}

#[test]
fn no_action_from_stale_policy() {
    let f = fixture("stale");
    f.app.idle_action("suspend", "stale");
    assert!(f.backend.idle_checks.borrow().is_empty());
}

#[test]
fn inhibited_display_remains_on() {
    let f = fixture("inhibited");
    f.app.idle_action("display-off", "current");
    f.backend.answer_idle(true);
    assert!(f.runner.runs.borrow().is_empty());
}

#[test]
fn resume_cancels_pending_sleep() {
    let f = fixture("resume-sleep");
    f.app.idle_action("suspend", "current");
    f.app.idle_resume("suspend", "current");
    f.backend.answer_idle(false);
    assert!(f.backend.sleeps.borrow().is_empty());
}

#[test]
fn resume_revokes_final_backend_guard() {
    let f = fixture("guard");
    f.app.idle_action("hibernate", "current");
    f.backend.answer_idle(false);
    let guard = f.backend.sleeps.borrow()[0].2.clone().unwrap();
    assert_eq!(f.backend.sleeps.borrow()[0].1, false, "automatic sleep is never interactive");
    assert!(guard());
    f.app.idle_resume("hibernate", "current");
    assert!(!guard());
}

#[test]
fn source_switch_cancels_pending_display_off() {
    let f = fixture("switch");
    f.app.idle_action("display-off", "current");
    f.idle.allow.set(false);
    f.backend.answer_idle(false);
    assert!(f.runner.runs.borrow().is_empty());
}

#[test]
fn resume_wakes_display_and_ignores_old_child() {
    let f = fixture("wake");
    f.app.idle_resume("display-off", "stale");
    assert!(f.runner.runs.borrow().is_empty());
    f.app.idle_resume("display-off", "current");
    assert_eq!(f.runner.argv(), [vec!["wlopm", "--on", "*"]]);
}

#[test]
fn display_wake_waits_for_inflight_display_off() {
    let f = fixture("inflight");
    f.app.idle_action("display-off", "current");
    f.backend.answer_idle(false);
    assert_eq!(f.app.commands(), ["display-off"]);
    f.app.request_wake();
    assert_eq!(f.runner.runs.borrow().len(), 1, "wake waits for the in-flight display-off");
    f.runner.finish(0, Exit::Success);
    assert_eq!(f.runner.argv()[1], ["wlopm", "--on", "*"]);
    assert!(!f.app.wake_requested());
}

#[test]
fn display_off_completion_requeues_wake_after_resume() {
    let f = fixture("requeue");
    f.app.idle_action("display-off", "current");
    f.backend.answer_idle(false);
    assert_eq!(f.runner.argv()[0], ["wlopm", "--off", "*"]);
    // Input arrived while wlopm --off was running: the guard now fails.
    f.app.idle_resume("lock", "current");
    f.idle.allow.set(false);
    f.runner.finish(0, Exit::Success);
    assert!(f.app.commands().contains(&"display-on".to_owned()));
    assert_eq!(f.runner.argv()[1], ["wlopm", "--on", "*"]);
}

#[test]
fn screen_lock_uses_independent_service() {
    let f = fixture("lock");
    f.app.idle_action("lock", "current");
    f.backend.answer_idle(false);
    assert_eq!(f.runner.argv(), [LOCK.to_vec()]);
}

#[test]
fn keep_awake_or_idle_inhibitor_blocks_automatic_lock() {
    let f = fixture("tessel-keep-awake");
    f.app.idle_action("lock", "current");
    f.backend.answer_idle(true);
    assert!(f.runner.runs.borrow().is_empty());
}

#[test]
fn input_cancels_a_pending_automatic_lock() {
    let f = fixture("input");
    f.app.idle_action("lock", "current");
    f.app.idle_resume("lock", "current");
    f.backend.answer_idle(false);
    assert!(f.runner.runs.borrow().is_empty());
}

#[test]
fn duplicate_commands_are_deduplicated_and_failures_reported() {
    let f = fixture("dedupe");
    f.app.run(&LOCK.map(String::from), "lock", None);
    f.app.run(&LOCK.map(String::from), "lock", None);
    assert_eq!(f.runner.runs.borrow().len(), 1);
    f.runner.finish(0, Exit::Failed("  unit failed \n".into()));
    assert_eq!(f.app.error(), "Could not lock: unit failed");
    f.app.run(&LOCK.map(String::from), "lock", None);
    f.runner.finish(1, Exit::Failed(String::new()));
    assert_eq!(f.app.error(), "Could not lock: command failed");
    f.runner.fail.replace(Some("Failed to execute child process".into()));
    f.app.run(&["wlopm".into()], "display-on", None);
    assert_eq!(f.app.error(), "Could not display-on: Failed to execute child process");
}

#[test]
fn unavailable_automatic_sleep_is_not_saved() {
    let f = fixture("unavailable");
    f.backend.set(|s| { s.capabilities.insert("hibernate".into(), "challenge".into()); });
    let mut proposed = defaults();
    proposed["ac"]["sleep_after"] = 1200.into();
    proposed["ac"]["sleep_action"] = "hibernate".into();
    assert!(!f.app.save(&proposed));
    assert!(f.saved.borrow().is_empty());
    assert_eq!(f.app.settings(), defaults());
    assert_eq!(f.app.error(), "That automatic sleep mode is not available on this system.");
}

#[test]
fn repairing_invalid_config_starts_idle_controller() {
    let f = fixture("repair");
    f.app.0.config_valid.set(false);
    f.app.set_error("Invalid settings");
    assert!(f.app.save(&defaults()));
    assert_eq!(*f.idle.started.borrow(), [defaults()]);
    assert!(f.app.0.config_valid.get());
    assert_eq!(f.app.error(), "");
}

#[test]
fn display_edit_preserves_existing_unsupported_sleep_choice() {
    let f = fixture("preserve");
    let mut existing = defaults();
    existing["ac"]["sleep_after"] = 3600.into();
    existing["ac"]["sleep_action"] = "hibernate".into();
    f.app.0.settings_config.replace(existing.clone());
    f.backend.set(|s| { s.capabilities.insert("hibernate".into(), "no".into()); });
    let mut proposed = existing;
    proposed["ac"]["display_off"] = 900.into();
    assert!(f.app.save(&proposed));
    assert_eq!(*f.saved.borrow(), [proposed]);
}

#[test]
fn dim_respects_inhibitors_and_restores_even_with_revoked_token() {
    let f = fixture("dim");
    f.app.idle_action("dim", "current");
    f.backend.answer_idle(true);
    assert_eq!(f.dimmer.dims.get(), 0);
    f.app.idle_action("dim", "current");
    f.backend.answer_idle(false);
    assert_eq!(f.dimmer.dims.get(), 1);
    f.app.idle_resume("dim", "revoked");
    assert_eq!(f.dimmer.restores.get(), 1);
}

#[test]
fn lid_close_uses_power_source_policy_and_locks_before_sleep() {
    let f = fixture("lid");
    f.backend.set(|s| s.on_battery = true);
    let mut settings = defaults();
    settings["lid_close"] = json!({"ac": "nothing", "battery": "hibernate"});
    f.app.0.settings_config.replace(settings.clone());
    f.app.lid_closed(true);
    assert!(f.runner.runs.borrow().is_empty());
    f.app.lid_closed(false);
    assert!(f.backend.explicit.borrow().is_empty());
    assert_eq!(f.runner.argv(), [LOCK.to_vec()]);
    assert_eq!(f.scheduler.delays(), [LOCK_TIMEOUT_MS]);
    f.runner.finish(0, Exit::Success);
    assert_eq!(*f.backend.explicit.borrow(), [("hibernate".to_owned(), false)]);
    assert!(f.scheduler.delays().is_empty(), "the lock timeout is cancelled");
    f.backend.set(|s| s.on_battery = false);
    f.app.lid_closed(false);
    assert_eq!(f.runner.runs.borrow().len(), 1);
    settings["lid_close"]["ac"] = "lock".into();
    f.app.0.settings_config.replace(settings);
    f.app.lid_closed(false);
    assert_eq!(f.runner.argv()[1], LOCK);
    assert_eq!(f.app.commands(), ["lock"]);
}

#[test]
fn failed_lock_cancels_lid_sleep_but_not_critical_battery_action() {
    let f = fixture("failed-lock");
    f.app.lid_closed(false);
    f.runner.finish(0, Exit::Failed(String::new()));
    assert!(f.backend.explicit.borrow().is_empty());
    assert_eq!(f.app.error(), "The screen could not be locked, so the computer was not put to sleep.");
    f.app.critical_action("suspend");
    // A locker that never becomes ready times out after 15 s.
    let timer = f.scheduler.ids()[0];
    f.scheduler.fire(timer);
    assert_eq!(*f.backend.explicit.borrow(), [("suspend".to_owned(), false)]);
    f.runner.finish(1, Exit::Success);
    assert_eq!(f.backend.explicit.borrow().len(), 1, "late lock completion is ignored");
    f.app.critical_action("poweroff");
    assert_eq!(f.backend.explicit.borrow().last().unwrap(), &("poweroff".to_owned(), false));
    assert_eq!(f.runner.runs.borrow().len(), 2);
}

#[test]
fn power_button_shows_menu_or_runs_action_with_debounce() {
    let f = fixture("button");
    std::fs::create_dir_all(f.root.join("tessel-session")).unwrap();
    std::fs::write(f.root.join("tessel-session/session_manager.py"), "").unwrap();
    f.app.power_button();
    f.clock.set(101.0);
    f.app.power_button();
    assert_eq!(f.runner.runs.borrow().len(), 1);
    let menu = f.runner.argv()[0].clone();
    assert_eq!(menu[..7], ["/usr/bin/systemd-run", "--user", "--collect", "--quiet", "--service-type=exec", "--slice=app.slice",
                           "--expand-environment=no"]);
    assert_eq!(menu[menu.len() - 3..], ["/usr/bin/python3".to_owned(), f.root.join("tessel-session/session_manager.py").to_string_lossy().into_owned(),
                                        "--popup".to_owned()]);
    f.clock.set(101.6);
    let mut settings = defaults();
    settings["power_button"] = "poweroff".into();
    f.app.0.settings_config.replace(settings.clone());
    f.app.power_button();
    assert_eq!(*f.backend.explicit.borrow(), [("poweroff".to_owned(), true)]);
    f.app.reset_button_debounce();
    settings["power_button"] = "nothing".into();
    f.app.0.settings_config.replace(settings);
    f.app.power_button();
    assert_eq!(f.backend.explicit.borrow().len(), 1);
}

#[test]
fn missing_power_menu_reports_error() {
    let f = fixture("menu-missing");
    assert!(!f.app.open_session_menu());
    assert_eq!(f.app.error(), "The power menu is not installed.");
}

#[test]
fn bluetooth_shortcut_launches_independently_without_shell_parsing() {
    let f = fixture("bluetooth");
    let script = f.root.join("tessel-bluetooth.py");
    std::fs::write(&script, "").unwrap();
    assert!(f.app.bluetooth_available());
    assert!(f.app.open_bluetooth());
    let command = f.runner.argv()[0].clone();
    assert!(command.contains(&"--service-type=exec".to_owned()));
    assert!(!command.contains(&"--scope".to_owned()) && !command.contains(&"--wait".to_owned()));
    assert_eq!(command.last().unwrap(), &script.to_string_lossy().into_owned());
    f.runner.finish(0, Exit::Failed(String::new()));
    assert!(f.app.error().contains("Try the Bluetooth icon"));
}

#[test]
fn missing_bluetooth_shortcut_reports_unavailable_without_launch() {
    let f = fixture("bluetooth-missing");
    assert!(!f.app.open_bluetooth());
    assert!(f.runner.runs.borrow().is_empty());
    assert!(f.app.error().contains("not installed"));
}

#[test]
fn remote_save_rejects_non_objects() {
    let f = fixture("remote");
    assert_eq!(f.app.remote_save("[]"), Err("Invalid power settings.".into()));
    assert!(f.app.remote_save("{broken").is_err());
    assert_eq!(f.app.remote_save(r#"{"lock_after": 600}"#), Ok((true, String::new())));
    assert_eq!(f.saved.borrow()[0]["lock_after"], 600);
    assert_eq!(f.app.remote_save(r#"{"lock_after": -1}"#),
               Ok((false, "Screen lock delay must be a whole number of seconds from 0 to 86400.".into())));
}
