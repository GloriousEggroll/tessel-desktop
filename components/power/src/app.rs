//! The resident power policy (PowerApplication of power_manager.py) without
//! GTK: idle events, lid/power button/critical actions, settings, health.
//! Effects (subprocesses, timers, dimming, windows) are injected traits.

use crate::alerts::BatteryAlerts;
use tessel_power_core::backend::PowerSystem;
use crate::config::{self, ConfigError, SLEEP_ACTIONS};
use crate::idle::IdleController;
use crate::logind::SessionSwitches;
use tessel_core::scheduler::Scheduler;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use tessel_core::tr;

pub const APPLICATION_ID: &str = "org.tessel.PowerManager";
pub const INTERFACE: &str = "org.tessel.PowerManager.Controls";
pub const LOCK_SERVICE: [&str; 4] = ["systemctl", "--user", "start", "tessel-lock.service"];
pub const LOCK_TIMEOUT_MS: u32 = 15_000;
pub const BUTTON_DEBOUNCE: f64 = 1.5;
pub const LAUNCH_ENVIRONMENT: [&str; 7] = ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
                                           "DBUS_SESSION_BUS_ADDRESS", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"];

/// How a finished subprocess ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Exit {
    Success,
    /// Non-zero exit; captured stderr (empty when not captured).
    Failed(String),
    /// GLib error while waiting.
    Error(String),
}

/// Subprocess launching (Gio.Subprocess in Python).
pub trait Runner {
    /// Start `argv`; `Err(message)` when it cannot start. stdout is silenced;
    /// stderr is captured when `capture_stderr`, else silenced.
    fn run(&self, argv: &[String], capture_stderr: bool, done: Box<dyn FnOnce(Exit)>) -> Result<(), String>;
    fn which(&self, name: &str) -> Option<PathBuf> {
        tessel_core::shell::which(name)
    }
}

pub trait IdleControl {
    fn ready(&self) -> bool;
    fn token(&self) -> String;
    fn error(&self) -> String;
    fn allows(&self, action: &str, token: &str) -> bool;
    fn start(&self, settings: &Value);
    fn update(&self, settings: &Value);
    fn close(&self);
}

impl IdleControl for IdleController {
    fn ready(&self) -> bool { IdleController::ready(self) }
    fn token(&self) -> String { IdleController::token(self) }
    fn error(&self) -> String { IdleController::error(self) }
    fn allows(&self, action: &str, token: &str) -> bool { IdleController::allows(self, action, token) }
    fn start(&self, settings: &Value) { let _ = IdleController::start(self, settings); }
    fn update(&self, settings: &Value) { let _ = IdleController::update(self, settings); }
    fn close(&self) { IdleController::close(self) }
}

pub trait Dimmer {
    fn dim(&self) -> bool;
    fn restore(&self);
    fn close(&self);
}

/// Windows owned by the GUI layer.
pub trait Surfaces {
    fn refresh(&self);
    fn show_popup(&self, edge: &str, offset: i64);
    fn show_settings(&self);
    fn has_popup(&self) -> bool;
    fn has_settings(&self) -> bool;
    fn close(&self);
}

pub struct Services {
    pub runner: Rc<dyn Runner>,
    pub scheduler: Rc<dyn Scheduler>,
    pub monotonic: Box<dyn Fn() -> f64>,
    pub saver: Box<dyn Fn(&Value) -> Result<(), ConfigError>>,
    /// `~/.config/waybar/scripts` (the Python scripts root, `Path(__file__).parent.parent`).
    pub scripts_root: PathBuf,
    pub python: String,
}

impl Default for Services {
    fn default() -> Self {
        let scripts_root = std::env::var_os("POWER_MANAGER_SCRIPTS_DIR").map(PathBuf::from)
            .unwrap_or_else(|| tessel_core::paths::config_home().join("waybar/scripts"));
        let python = tessel_core::shell::which("python3").map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/usr/bin/python3".into());
        Self { runner: Rc::new(GioRunner), scheduler: Rc::new(tessel_core::scheduler::GlibScheduler::default()),
               monotonic: Box::new(|| glib::monotonic_time() as f64 / 1e6),
               saver: Box::new(|settings| config::save_settings(settings, None)), scripts_root, python }
    }
}

pub struct GioRunner;

impl Runner for GioRunner {
    fn run(&self, argv: &[String], capture_stderr: bool, done: Box<dyn FnOnce(Exit)>) -> Result<(), String> {

        let words: Vec<&std::ffi::OsStr> = argv.iter().map(|w| w.as_ref()).collect();
        let flags = gio::SubprocessFlags::STDOUT_SILENCE
            | if capture_stderr { gio::SubprocessFlags::STDERR_PIPE } else { gio::SubprocessFlags::STDERR_SILENCE };
        let process = gio::Subprocess::newv(&words, flags).map_err(|e| e.message().to_owned())?;
        let child = process.clone();
        if capture_stderr {
            process.communicate_utf8_async(None, None::<&gio::Cancellable>, move |result| match result {
                Err(error) => done(Exit::Error(error.message().to_owned())),
                Ok((_, stderr)) if child.is_successful() => { let _ = stderr; done(Exit::Success) }
                Ok((_, stderr)) => done(Exit::Failed(stderr.map(|s| s.to_string()).unwrap_or_default())),
            });
        } else {
            process.wait_async(None::<&gio::Cancellable>, move |result| match result {
                Err(error) => done(Exit::Error(error.message().to_owned())),
                Ok(()) if child.is_successful() => done(Exit::Success),
                Ok(()) => done(Exit::Failed(String::new())),
            });
        }
        Ok(())
    }
}

pub struct AppInner {
    pub backend: RefCell<Option<Rc<dyn PowerSystem>>>,
    pub idle: RefCell<Option<Rc<dyn IdleControl>>>,
    pub alerts: RefCell<Option<BatteryAlerts>>,
    pub switches: RefCell<Option<SessionSwitches>>,
    pub dimmer: RefCell<Option<Rc<dyn Dimmer>>>,
    pub surfaces: RefCell<Option<Rc<dyn Surfaces>>>,
    /// `SettingsService.changed`.
    pub notify_service: RefCell<Option<Box<dyn Fn()>>>,
    pub services: Services,
    pub settings_config: RefCell<Value>,
    pub error: RefCell<String>,
    pub stopping: Cell<bool>,
    pub config_valid: Cell<bool>,
    last_button: Cell<Option<f64>>,
    commands: RefCell<HashSet<String>>,
    wake_requested: Cell<bool>,
    action_generation: RefCell<BTreeMap<String, u64>>,
}

#[derive(Clone)]
pub struct App(pub Rc<AppInner>);

impl App {
    pub fn new(services: Services) -> Self {
        Self(Rc::new(AppInner {
            backend: RefCell::new(None), idle: RefCell::new(None), alerts: RefCell::new(None), switches: RefCell::new(None),
            dimmer: RefCell::new(None), surfaces: RefCell::new(None), notify_service: RefCell::new(None), services,
            settings_config: RefCell::new(config::defaults()), error: RefCell::new(String::new()), stopping: Cell::new(false),
            config_valid: Cell::new(false), last_button: Cell::new(None), commands: RefCell::new(HashSet::new()),
            wake_requested: Cell::new(false),
            action_generation: RefCell::new(crate::args::ACTIONS.iter().map(|a| (a.to_string(), 0)).collect()),
        }))
    }

    pub fn downgrade(&self) -> Weak<AppInner> {
        Rc::downgrade(&self.0)
    }

    pub fn backend(&self) -> Option<Rc<dyn PowerSystem>> { self.0.backend.borrow().clone() }
    pub fn idle(&self) -> Option<Rc<dyn IdleControl>> { self.0.idle.borrow().clone() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn set_error(&self, error: &str) { self.0.error.replace(error.into()); }
    pub fn settings(&self) -> Value { self.0.settings_config.borrow().clone() }
    pub fn commands(&self) -> Vec<String> { self.0.commands.borrow().iter().cloned().collect() }
    pub fn wake_requested(&self) -> bool { self.0.wake_requested.get() }

    /// do_startup's settings load.
    pub fn load_settings(&self, path: Option<&std::path::Path>) {
        match config::load_settings(path) {
            Ok(settings) => {
                self.0.settings_config.replace(settings);
                self.0.config_valid.set(true);
            }
            Err(error) => {
                self.0.settings_config.replace(config::defaults());
                self.0.error.replace(tr!("Could not load power settings: {value1}", value1 = error.to_string()));
            }
        }
    }

    pub fn health(&self) -> Value {
        let backend = self.backend();
        let idle_ready = self.idle().is_some_and(|i| i.ready());
        let switches = self.0.switches.borrow().clone();
        let handled = |what: &str| switches.as_ref().is_some_and(|s| s.holds(what));
        let error = self.error();
        match backend {
            None => json!({"ready": false, "profiles_ready": false, "power_ready": false, "sleep_ready": false,
                "idle_ready": idle_ready, "has_battery": false, "active_profile": "", "batteries": [], "charge_limit": {},
                "lid_handled": handled("handle-lid-switch"), "power_key_handled": handled("handle-power-key"), "error": error}),
            Some(backend) => {
                let s = backend.state();
                json!({
                    "ready": s.profiles_ready && s.power_ready && s.sleep_ready && idle_ready && self.0.config_valid.get(),
                    "profiles_ready": s.profiles_ready, "power_ready": s.power_ready, "sleep_ready": s.sleep_ready,
                    "idle_ready": idle_ready, "has_battery": s.has_battery, "active_profile": s.active_profile,
                    "batteries": s.batteries, "charge_limit": s.charge_limit,
                    "lid_handled": handled("handle-lid-switch"), "power_key_handled": handled("handle-power-key"),
                    "error": if error.is_empty() { s.error.clone() } else { error },
                })
            }
        }
    }

    /// GetState: transport readiness, not the health of optional backends.
    pub fn settings_state(&self) -> Value {
        let mut state = self.health();
        state["settings"] = self.settings();
        state["ready"] = (!self.0.stopping.get() && self.backend().is_some() && self.idle().is_some()).into();
        state
    }

    pub fn remote_save(&self, encoded: &str) -> Result<(bool, String), String> {
        let settings: Value = serde_json::from_str(encoded).map_err(|e| e.to_string())?;
        if !settings.is_object() {
            return Err("Invalid power settings.".into());
        }
        let ok = self.save(&settings);
        Ok((ok, self.error()))
    }

    pub fn changed(&self) {
        if self.0.stopping.get() {
            return;
        }
        if self.0.config_valid.get() {
            if let Some(idle) = self.idle() {
                idle.update(&self.settings());
            }
        }
        if let Some(alerts) = self.0.alerts.borrow().clone() {
            alerts.refresh();
        }
        self.refresh_views();
    }

    fn refresh_views(&self) {
        if let Some(surfaces) = self.0.surfaces.borrow().clone() {
            surfaces.refresh();
        }
        if let Some(notify) = self.0.notify_service.borrow().as_ref() {
            notify();
        }
    }

    pub fn idle_error(&self, message: &str) {
        self.0.error.replace(message.into());
        self.refresh_views();
    }

    pub fn save(&self, settings: &Value) -> bool {
        let result = (|| -> Result<Value, String> {
            let updated = config::validate_settings(settings)?;
            // Preserve unsupported saved modes, but do not allow enabling a new
            // unattended action which this machine cannot currently use.
            let previous = self.settings();
            for source in ["ac", "battery"] {
                let policy = &updated[source];
                let changed = ["sleep_after", "sleep_action"].iter().any(|k| policy[*k] != previous[source][*k]);
                let action = policy["sleep_action"].as_str().unwrap_or("");
                let capable = self.backend().is_some_and(|b| b.state().capability(action) == Some("yes"));
                if policy["sleep_after"].as_i64().unwrap_or(0) != 0 && changed && !capable {
                    return Err(tr!("That automatic sleep mode is not available on this system."));
                }
            }
            (self.0.services.saver)(&updated).map_err(|e| e.to_string())?;
            Ok(updated)
        })();
        let updated = match result {
            Err(error) => {
                self.0.error.replace(error);
                self.changed();
                return false;
            }
            Ok(updated) => updated,
        };
        let was_valid = self.0.config_valid.get();
        self.0.settings_config.replace(updated.clone());
        self.0.config_valid.set(true);
        self.0.error.replace(String::new());
        if let Some(idle) = self.idle() {
            if was_valid { idle.update(&updated) } else { idle.start(&updated) }
        }
        if let Some(alerts) = self.0.alerts.borrow().clone() {
            alerts.update(&updated);
        }
        self.changed();
        true
    }

    fn generation(&self, action: &str) -> u64 {
        self.0.action_generation.borrow().get(action).copied().unwrap_or(0)
    }

    fn bump(&self, action: &str) -> u64 {
        let mut generations = self.0.action_generation.borrow_mut();
        let entry = generations.entry(action.to_owned()).or_insert(0);
        *entry += 1;
        *entry
    }

    pub fn idle_action(&self, action: &str, token: &str) {
        let Some(idle) = self.idle() else { return };
        if !idle.allows(action, token) {
            return;
        }
        let generation = self.bump(action);
        let weak = self.downgrade();
        let (action_owned, token_owned) = (action.to_owned(), token.to_owned());
        let still_idle: Rc<dyn Fn() -> bool> = Rc::new(move || {
            let Some(this) = weak.upgrade().map(App) else { return false };
            !this.0.stopping.get() && this.generation(&action_owned) == generation
                && this.idle().is_some_and(|i| i.allows(&action_owned, &token_owned))
        });
        let Some(backend) = self.backend() else { return };
        let weak = self.downgrade();
        let action = action.to_owned();
        backend.idle_inhibited(Box::new(move |inhibited| {
            let Some(this) = weak.upgrade().map(App) else { return };
            // Mains/battery state or settings may have changed during D-Bus I/O.
            if inhibited || !still_idle() {
                return;
            }
            match action.as_str() {
                "suspend" | "hibernate" | "suspend-then-hibernate" => {
                    if let Some(backend) = this.backend() {
                        backend.sleep(&action, false, Some(still_idle.clone()));
                    }
                }
                "dim" => {
                    if let Some(dimmer) = this.0.dimmer.borrow().clone() {
                        dimmer.dim();
                    }
                }
                "lock" => this.run(&LOCK_SERVICE.map(String::from), "lock", None),
                "display-off" | "display-on" => {
                    let flag = if action == "display-off" { "--off" } else { "--on" };
                    this.run(&["wlopm".into(), flag.into(), "*".into()], &action, Some(still_idle.clone()));
                }
                _ => {}
            }
        }));
    }

    pub fn idle_resume(&self, action: &str, token: &str) {
        if action == "dim" {
            // Restoring is always safe, including for a replaced swayidle
            // child whose token has already been revoked.
            self.bump(action);
            if let Some(dimmer) = self.0.dimmer.borrow().clone() {
                dimmer.restore();
            }
            return;
        }
        let Some(idle) = self.idle() else { return };
        if token.is_empty() || token != idle.token() {
            return;
        }
        self.bump(action);
        if action == "display-off" {
            // Also wake here in case an already queued display-off callback ran
            // between swayidle's direct wake command and this notification.
            self.request_wake();
        }
    }

    pub fn source(&self) -> &'static str {
        let on_battery = self.backend().is_some_and(|b| {
            let s = b.state();
            s.power_source_known && s.has_battery && s.on_battery
        });
        if on_battery { "battery" } else { "ac" }
    }

    pub fn lid_closed(&self, docked: bool) {
        if self.0.stopping.get() || docked {
            return;
        }
        let action = self.settings()["lid_close"][self.source()].as_str().unwrap_or("nothing").to_owned();
        self.explicit(&action, "lid", false);
    }

    pub fn power_button(&self) {
        if self.0.stopping.get() {
            return;
        }
        let now = (self.0.services.monotonic)();
        if self.0.last_button.get().is_some_and(|last| now - last < BUTTON_DEBOUNCE) {
            return;
        }
        self.0.last_button.set(Some(now));
        let action = self.settings()["power_button"].as_str().unwrap_or("menu").to_owned();
        if action == "menu" {
            self.open_session_menu();
        } else {
            self.explicit(&action, "button", true);
        }
    }

    pub fn reset_button_debounce(&self) {
        self.0.last_button.set(None);
    }

    pub fn critical_action(&self, action: &str) {
        self.explicit(action, "battery", false);
    }

    /// Lid, power button and critical battery actions chosen by the user.
    pub fn explicit(&self, action: &str, reason: &str, interactive: bool) {
        let Some(backend) = self.backend() else { return };
        if action == "nothing" || self.0.stopping.get() {
            return;
        }
        if action == "lock" {
            return self.run(&LOCK_SERVICE.map(String::from), "lock", None);
        }
        if action == "poweroff" {
            backend.explicit_power("poweroff", None, interactive);
            return;
        }
        if !SLEEP_ACTIONS.contains(&action) {
            return;
        }
        let weak = self.downgrade();
        let (action, battery) = (action.to_owned(), reason == "battery");
        self.lock_then(Box::new(move |ok| {
            let Some(this) = weak.upgrade().map(App) else { return };
            // An empty battery must sleep even if the locker failed; otherwise a
            // failed lock cancels sleep, matching the session menu.
            if ok || battery {
                if let Some(backend) = this.backend() {
                    backend.explicit_power(&action, None, interactive);
                }
            } else {
                this.idle_error(&tr!("The screen could not be locked, so the computer was not put to sleep."));
            }
        }));
    }

    /// Wait for the secure locker's readiness before sleeping.
    pub fn lock_then(&self, callback: Box<dyn FnOnce(bool)>) {
        struct State {
            callback: Option<Box<dyn FnOnce(bool)>>,
            timer: Option<u64>,
        }
        let state = Rc::new(RefCell::new(State { callback: Some(callback), timer: None }));
        let finish = {
            let (state, weak, scheduler) = (state.clone(), self.downgrade(), self.0.services.scheduler.clone());
            Rc::new(move |ok: bool| {
                let (callback, timer) = {
                    let mut s = state.borrow_mut();
                    (s.callback.take(), s.timer.take())
                };
                let Some(callback) = callback else { return };
                if let Some(timer) = timer {
                    scheduler.cancel(timer);
                }
                if weak.upgrade().is_some_and(|this| !this.stopping.get()) {
                    callback(ok);
                }
            })
        };
        let f = finish.clone();
        let started = self.0.services.runner.run(&LOCK_SERVICE.map(String::from), false, Box::new(move |exit| f(exit == Exit::Success)));
        if started.is_err() {
            return finish(false);
        }
        let f = finish.clone();
        let id = self.0.services.scheduler.timeout(LOCK_TIMEOUT_MS, Box::new(move || {
            f(false);
        }));
        state.borrow_mut().timer = Some(id);
    }

    fn launch_prefix(&self, launcher: &std::path::Path) -> Vec<String> {
        let mut command: Vec<String> = [launcher.to_string_lossy().as_ref(), "--user", "--collect", "--quiet", "--service-type=exec",
                                        "--slice=app.slice", "--expand-environment=no"].map(String::from).to_vec();
        command.extend(LAUNCH_ENVIRONMENT.iter().filter(|n| std::env::var_os(n).is_some()).map(|n| format!("--setenv={n}")));
        command.push("--".into());
        command.push(self.0.services.python.clone());
        command
    }

    pub fn session_menu_script(&self) -> Option<PathBuf> {
        let path = self.0.services.scripts_root.join("tessel-session/session_manager.py");
        path.is_file().then_some(path)
    }

    pub fn open_session_menu(&self) -> bool {
        let script = self.session_menu_script();
        let launcher = self.0.services.runner.which("systemd-run");
        let (Some(script), Some(launcher)) = (script, launcher) else {
            self.idle_error(&tr!("The power menu is not installed."));
            return false;
        };
        let mut command = self.launch_prefix(&launcher);
        command.extend([script.to_string_lossy().into_owned(), "--popup".into()]);
        if let Err(error) = self.0.services.runner.run(&command, false, Box::new(|_| {})) {
            self.idle_error(&tr!("Could not open the power menu: {value1}", value1 = error));
            return false;
        }
        true
    }

    pub fn bluetooth_script(&self) -> Option<PathBuf> {
        let root = &self.0.services.scripts_root;
        [root.join("tessel-bluetooth/tessel-bluetooth.py"), root.join("tessel-bluetooth.py")].into_iter().find(|p| p.is_file())
    }

    pub fn bluetooth_available(&self) -> bool {
        self.bluetooth_script().is_some() && self.0.services.runner.which("systemd-run").is_some()
    }

    pub fn open_bluetooth(&self) -> bool {
        let script = self.bluetooth_script();
        let launcher = self.0.services.runner.which("systemd-run");
        let (Some(script), Some(launcher)) = (script, launcher) else {
            self.0.error.replace(tr!("Bluetooth settings are not installed."));
            self.changed();
            return false;
        };
        let mut command = self.launch_prefix(&launcher);
        command.push(script.to_string_lossy().into_owned());
        let weak = self.downgrade();
        let started = self.0.services.runner.run(&command, false, Box::new(move |exit| {
            if exit != Exit::Success {
                if let Some(this) = weak.upgrade().map(App) {
                    this.0.error.replace(tr!("Could not open Bluetooth settings. Try the Bluetooth icon in Waybar."));
                    this.changed();
                }
            }
        }));
        if let Err(error) = started {
            self.0.error.replace(tr!("Could not open Bluetooth settings: {value1}", value1 = error));
            self.changed();
            return false;
        }
        true
    }

    pub fn request_wake(&self) {
        self.0.wake_requested.set(true);
        self.drain_wake();
    }

    pub fn drain_wake(&self) {
        let busy = {
            let commands = self.0.commands.borrow();
            commands.contains("display-off") || commands.contains("display-on")
        };
        if self.0.wake_requested.get() && !busy {
            self.0.wake_requested.set(false);
            self.run(&["wlopm".into(), "--on".into(), "*".into()], "display-on", None);
        }
    }

    pub fn run(&self, argv: &[String], action: &str, guard: Option<Rc<dyn Fn() -> bool>>) {
        if self.0.commands.borrow().contains(action) {
            return;
        }
        let weak = self.downgrade();
        let owned = action.to_owned();
        let started = self.0.services.runner.run(argv, true, Box::new(move |exit| {
            if let Some(this) = weak.upgrade().map(App) {
                this.command_done(&owned, guard, exit);
            }
        }));
        match started {
            Ok(()) => { self.0.commands.borrow_mut().insert(action.to_owned()); }
            Err(error) => self.idle_error(&tr!("Could not {value1}: {value2}", value1 = action, value2 = error)),
        }
    }

    pub fn command_done(&self, action: &str, guard: Option<Rc<dyn Fn() -> bool>>, exit: Exit) {
        self.0.commands.borrow_mut().remove(action);
        match exit {
            Exit::Success => {}
            Exit::Failed(stderr) => {
                let detail = if stderr.trim().is_empty() { "command failed".to_owned() } else { stderr.trim().to_owned() };
                self.idle_error(&tr!("Could not {value1}: {value2}", value1 = action, value2 = detail));
            }
            Exit::Error(message) => {
                if !self.0.stopping.get() {
                    self.idle_error(&tr!("Could not {value1}: {value2}", value1 = action, value2 = message));
                }
            }
        }
        if action == "display-off" && guard.is_some_and(|g| !g()) {
            self.0.wake_requested.set(true);
        }
        self.drain_wake();
    }

    /// do_shutdown.
    pub fn shutdown(&self) {
        self.0.stopping.set(true);
        if let Some(idle) = self.0.idle.borrow().clone() {
            idle.close();
        }
        if let Some(alerts) = self.0.alerts.borrow().clone() {
            alerts.close();
        }
        if let Some(switches) = self.0.switches.borrow().clone() {
            switches.close();
        }
        if let Some(dimmer) = self.0.dimmer.borrow().clone() {
            dimmer.close();
        }
        if let Some(backend) = self.backend() {
            backend.close();
        }
        if let Some(surfaces) = self.0.surfaces.borrow().clone() {
            surfaces.close();
        }
    }
}
