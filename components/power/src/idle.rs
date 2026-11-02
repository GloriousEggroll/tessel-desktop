//! Own one swayidle child and invalidate obsolete automatic power actions
//! (power_idle.py).
//!
//! Wayland idle inhibitors are respected by swayidle. Timeout commands return
//! to the primary application, which also checks logind inhibitors and session
//! state before taking an action. Waking displays is deliberately synchronous
//! and does not require that application: swayidle runs resume commands while
//! terminating.

use tessel_power_core::backend::PowerSystem;
use crate::config::{validate_settings, SLEEP_ACTIONS};
use tessel_core::scheduler::Scheduler;
use tessel_core::secrets::{compare_digest, token_urlsafe};
use tessel_core::shell::{is_executable, shell_join};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

pub const DIM_LEAD: i64 = 30;

/// Dim shortly before the first of locking or display-off, never at zero.
pub fn dim_delay(lock_after: i64, display_off: i64, enabled: bool) -> i64 {
    let first = [lock_after, display_off].into_iter().filter(|v| *v != 0).min().unwrap_or(0);
    if !enabled || first == 0 {
        return 0;
    }
    // Very short timers still get a visible warning halfway through.
    if first > 2 * DIM_LEAD { first - DIM_LEAD } else { first / 2 }
}

/// Process operations, injectable for tests (Python patches Popen/which).
pub trait Processes {
    fn which(&self, name: &str) -> Option<PathBuf>;
    fn spawn(&self, argv: &[String]) -> Result<u32, String>;
    /// Reap `pid` from the main loop; `callback(exit_code)` (`os.waitstatus_to_exitcode`).
    fn watch(&self, pid: u32, callback: Box<dyn FnOnce(i32)>);
    fn signal(&self, pid: u32, signal: i32);
}

pub struct RealProcesses;

/// `os.waitstatus_to_exitcode`.
pub fn exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        -libc::WTERMSIG(status)
    } else {
        status
    }
}

use tessel_core::pyerr::os_error_text;

impl Processes for RealProcesses {
    fn which(&self, name: &str) -> Option<PathBuf> {
        tessel_core::shell::which(name)
    }

    fn spawn(&self, argv: &[String]) -> Result<u32, String> {
        use std::process::{Command, Stdio};
        let child = Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).spawn()
            .map_err(|e| os_error_text(&e))?;
        // GLib's child watch reaps it; std's Child is dropped without waiting.
        Ok(child.id())
    }

    fn watch(&self, pid: u32, callback: Box<dyn FnOnce(i32)>) {
        let callback = RefCell::new(Some(callback));
        glib::child_watch_add_local(glib::Pid(pid as i32), move |_, status| {
            if let Some(callback) = callback.borrow_mut().take() {
                callback(exit_code(status));
            }
        });
    }

    fn signal(&self, pid: u32, signal: i32) {
        // SAFETY: an unreaped child's pid cannot be reused.
        unsafe { libc::kill(pid as i32, signal) };
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Signature {
    known: bool,
    battery: bool,
    on_battery: bool,
    source: &'static str,
    lock_after: i64,
    display_off: i64,
    sleep_after: i64,
    sleep_action: String,
    supported: [bool; 3],
    dim_after: i64,
}

struct Policy {
    signature: Signature,
    lock_after: i64,
    display_off: i64,
    sleep_after: i64,
    sleep_action: String,
    dim_after: i64,
}

pub struct IdleController(Rc<Inner>);

pub struct Inner {
    backend: Rc<dyn PowerSystem>,
    on_error: Box<dyn Fn(&str)>,
    processes: Rc<dyn Processes>,
    scheduler: Rc<dyn Scheduler>,
    /// argv prefix that re-enters the primary (the Rust binary itself).
    self_command: Vec<String>,
    ready: Cell<bool>,
    error: RefCell<String>,
    settings: RefCell<Option<Value>>,
    signature: RefCell<Option<Signature>>,
    token: RefCell<String>,
    process: Cell<Option<u32>>,
    watches: RefCell<HashSet<u32>>,
    kill_timers: RefCell<HashMap<u32, u64>>,
    enabled: Cell<bool>,
    lock_script: RefCell<PathBuf>,
}

/// The command prefix swayidle runs to reach the primary.
pub fn self_command() -> Vec<String> {
    if let Ok(path) = std::env::var("POWER_MANAGER_EXECUTABLE") {
        if !path.is_empty() {
            return vec![path];
        }
    }
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "tessel-power".into());
    vec![exe.strip_suffix(" (deleted)").unwrap_or(&exe).to_owned()]
}

impl IdleController {
    pub fn new(backend: Rc<dyn PowerSystem>, on_error: impl Fn(&str) + 'static, processes: Rc<dyn Processes>,
               scheduler: Rc<dyn Scheduler>, self_command: Vec<String>) -> Self {
        Self(Rc::new(Inner {
            backend, on_error: Box::new(on_error), processes, scheduler, self_command, ready: Cell::new(false),
            error: RefCell::new(String::new()), settings: RefCell::new(None), signature: RefCell::new(None),
            token: RefCell::new(String::new()), process: Cell::new(None), watches: RefCell::new(HashSet::new()),
            kill_timers: RefCell::new(HashMap::new()), enabled: Cell::new(false),
            lock_script: RefCell::new(tessel_core::paths::config_home().join("waybar/scripts/screenlock.sh")),
        }))
    }

    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn token(&self) -> String { self.0.token.borrow().clone() }
    pub fn process(&self) -> Option<u32> { self.0.process.get() }
    /// `$XDG_CONFIG_HOME/waybar/scripts/screenlock.sh` (exists/executable check only).
    pub fn set_lock_script(&self, path: PathBuf) { self.0.lock_script.replace(path); }
    #[cfg(test)]
    pub fn set_token(&self, token: &str) { self.0.token.replace(token.into()); }

    pub fn start(&self, settings: &Value) -> Result<(), String> {
        let validated = validate_settings(settings)?;
        self.0.enabled.set(true);
        self.0.signature.replace(None);
        self.update(&validated)
    }

    pub fn update(&self, settings: &Value) -> Result<(), String> {
        self.0.settings.replace(Some(validate_settings(settings)?));
        self.refresh();
        Ok(())
    }

    pub fn refresh(&self) {
        Inner::refresh(&self.0)
    }

    /// Reject queued callbacks from any superseded policy or power source.
    pub fn allows(&self, action: &str, token: &str) -> bool {
        let inner = &self.0;
        let current = inner.token.borrow().clone();
        if !inner.enabled.get() || !inner.ready.get() || current.is_empty() {
            return false;
        }
        if !compare_digest(&current, token) {
            return false;
        }
        if inner.process.get().is_none() {
            return false;
        }
        let Some(policy) = inner.policy() else { return false };
        if Some(&policy.signature) != inner.signature.borrow().as_ref() {
            return false;
        }
        match action {
            "dim" => policy.dim_after > 0,
            "lock" => policy.lock_after > 0,
            "display-off" | "display-on" => policy.display_off > 0,
            _ => action == policy.sleep_action && policy.sleep_after > 0,
        }
    }

    pub fn close(&self) {
        let inner = &self.0;
        inner.enabled.set(false);
        inner.token.replace(String::new());
        inner.stop_child();
        inner.signature.replace(None);
    }

    pub fn resume_command(&self, action: &str, wake: Option<&str>) -> String {
        self.0.resume_command(action, wake)
    }

    /// Test hook: the child watch fired for `pid` with an exit code.
    pub fn child_exited(&self, pid: u32, status: i32) {
        Inner::child_exited(&self.0, pid, status)
    }
}

impl Inner {
    fn policy(&self) -> Option<Policy> {
        let settings = self.settings.borrow();
        let settings = settings.as_ref()?;
        let state = self.backend.state();
        let (known, battery, on_battery) = (state.power_source_known, state.has_battery, state.on_battery);
        let source = if known && battery && on_battery { "battery" } else { "ac" };
        let policy = &settings[source];
        // Interactive authorization is not suitable for unattended timers.
        let supported = SLEEP_ACTIONS.map(|action| state.capability(action) == Some("yes"));
        let sleep_action = policy["sleep_action"].as_str().unwrap_or("suspend").to_owned();
        let index = SLEEP_ACTIONS.iter().position(|a| *a == sleep_action).unwrap_or(0);
        let raw_sleep = policy["sleep_after"].as_i64().unwrap_or(0);
        let sleep_after = if known && supported[index] { raw_sleep } else { 0 };
        let lock_after = settings["lock_after"].as_i64().unwrap_or(0);
        let display_off = policy["display_off"].as_i64().unwrap_or(0);
        let dim_after = dim_delay(lock_after, display_off, settings["dim_before_lock"].as_bool().unwrap_or(true));
        Some(Policy {
            signature: Signature { known, battery, on_battery, source, lock_after, display_off, sleep_after: raw_sleep,
                                   sleep_action: sleep_action.clone(), supported, dim_after },
            lock_after, display_off, sleep_after, sleep_action, dim_after,
        })
    }

    fn refresh(self: &Rc<Self>) {
        if !self.enabled.get() {
            return;
        }
        let Some(policy) = self.policy() else { return };
        if Some(&policy.signature) == self.signature.borrow().as_ref() {
            return;
        }
        // Revoke first: callbacks already queued from the old child may arrive
        // after its termination. Display waking bypasses this token safely.
        self.token.replace(String::new());
        self.stop_child();
        self.signature.replace(Some(policy.signature.clone()));
        self.error.replace(String::new());
        let (swayidle, wlopm, systemctl) = (self.processes.which("swayidle"), self.processes.which("wlopm"),
                                            self.processes.which("systemctl"));
        let (Some(swayidle), Some(wlopm), Some(systemctl)) = (swayidle, wlopm, systemctl) else {
            return self.fail("Idle power management requires swayidle and wlopm, plus a systemd user session.");
        };
        let lock_script = self.lock_script.borrow().clone();
        if !is_executable(&lock_script) {
            return self.fail(&format!("The screen lock command is missing or not executable: {}", lock_script.display()));
        }
        self.token.replace(token_urlsafe(24));
        let wlopm = wlopm.to_string_lossy().into_owned();
        let wake = shell_join(&[wlopm.as_str(), "--on", "*"]);
        let mut command: Vec<String> = vec![swayidle.to_string_lossy().into_owned(), "-w".into(), "-C".into(), "/dev/null".into()];
        let mut timer = |seconds: i64, action: &str, wake: Option<&str>| {
            command.extend([
                "timeout".to_owned(), seconds.to_string(), self.action_command(action), "resume".to_owned(),
                self.resume_command(action, wake),
            ]);
        };
        if policy.dim_after != 0 {
            // Restoring is always safe, so it is not bound to the policy token:
            // swayidle runs this resume command when it is replaced as well.
            timer(policy.dim_after, "dim", None);
        }
        if policy.lock_after != 0 {
            timer(policy.lock_after, "lock", None);
        }
        if policy.display_off != 0 {
            timer(policy.display_off, "display-off", Some(&wake));
        }
        if policy.sleep_after != 0 {
            timer(policy.sleep_after, &policy.sleep_action, None);
        }
        // An independent service owns the locker. Its Type=notify readiness is
        // sent only after the compositor confirms the session is locked.
        let lock = shell_join(&[systemctl.to_string_lossy().as_ref(), "--user", "start", "tessel-lock.service"]);
        command.extend(["lock".to_owned(), lock.clone(), "before-sleep".to_owned(), lock, "after-resume".to_owned(), wake]);
        match self.processes.spawn(&command) {
            Err(error) => self.fail(&format!("Could not start idle power management: {error}")),
            Ok(pid) => {
                self.process.set(Some(pid));
                self.ready.set(true);
                self.watches.borrow_mut().insert(pid);
                let weak: Weak<Inner> = Rc::downgrade(self);
                self.processes.watch(pid, Box::new(move |status| {
                    if let Some(this) = weak.upgrade() {
                        Inner::child_exited(&this, pid, status);
                    }
                }));
            }
        }
    }

    fn action_command(&self, action: &str) -> String {
        let token = self.token.borrow().clone();
        let mut words = self.self_command.clone();
        words.extend(["--idle-action".into(), action.into(), "--policy-token".into(), token]);
        shell_join(&words)
    }

    fn resume_command(&self, action: &str, wake: Option<&str>) -> String {
        // Never wait for the primary's main loop: it may itself be waiting
        // for swayidle to terminate/restart.
        let token = self.token.borrow().clone();
        let mut words = self.self_command.clone();
        words.extend(["--idle-resume".into(), action.into(), "--policy-token".into(), token]);
        let command = shell_join(&words) + " </dev/null >/dev/null 2>&1 &";
        match wake {
            Some(wake) => format!("{command} {wake}"),
            None => command,
        }
    }

    fn child_exited(self: &Rc<Self>, pid: u32, status: i32) {
        self.watches.borrow_mut().remove(&pid);
        if let Some(timer) = self.kill_timers.borrow_mut().remove(&pid) {
            self.scheduler.cancel(timer);
        }
        if self.process.get() != Some(pid) {
            return;
        }
        self.process.set(None);
        self.fail(&format!("Idle power management stopped unexpectedly (exit status {status})."));
    }

    fn fail(&self, message: &str) {
        self.ready.set(false);
        self.token.replace(String::new());
        self.error.replace(message.into());
        (self.on_error)(message);
    }

    /// Ask the old child to exit without blocking the main loop. Its child
    /// watch reaps it; a stuck child is killed after one second.
    fn stop_child(self: &Rc<Self>) {
        self.ready.set(false);
        let Some(pid) = self.process.take() else { return };
        if !self.watches.borrow().contains(&pid) {
            return;
        }
        self.processes.signal(pid, libc::SIGTERM);
        let weak = Rc::downgrade(self);
        let id = self.scheduler.timeout(1000, Box::new(move || {
            let Some(inner) = weak.upgrade() else { return };
            inner.kill_timers.borrow_mut().remove(&pid);
            if inner.watches.borrow().contains(&pid) {
                inner.processes.signal(pid, libc::SIGKILL);
            }
        }));
        self.kill_timers.borrow_mut().insert(pid, id);
    }
}

