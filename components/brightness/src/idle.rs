//! A separate Wayland idle listener for optional, reversible display dimming
//! (`brightness_idle.py`).
//!
//! This module never changes hardware and never locks, sleeps, or switches
//! outputs off. swayidle supplies compositor idle inhibition. The primary
//! application must also check session/logind inhibition, revoke pending
//! asynchronous dim operations on restore, and restore its saved hardware
//! values on policy changes or failure.

use tessel_core::scheduler::Scheduler;
use tessel_core::secrets::{compare_digest, token_urlsafe};
use tessel_core::shell::shell_join;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const DEFAULT_DELAY: i64 = 120;
pub const MAX_DELAY: i64 = 24 * 60 * 60;

/// `subprocess.Popen` child (fakeable).
pub trait Child {
    /// `poll()`: exit status once finished.
    fn poll(&mut self) -> Option<i32>;
    fn terminate(&mut self) -> Result<(), String>;
    fn kill(&mut self) -> Result<(), String>;
    /// `wait(timeout)`: `Ok(None)` on timeout.
    fn wait(&mut self, timeout: Duration) -> Result<Option<i32>, String>;
}

pub trait Spawner {
    fn which(&self, name: &str) -> Option<PathBuf>;
    fn spawn(&self, argv: &[String]) -> Result<Box<dyn Child>, String>;
}

pub struct RealChild(std::process::Child);

fn status_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().unwrap_or_else(|| -status.signal().unwrap_or(0))
}

impl Child for RealChild {
    fn poll(&mut self) -> Option<i32> {
        self.0.try_wait().ok().flatten().map(status_code)
    }

    fn terminate(&mut self) -> Result<(), String> {
        // SAFETY: an unreaped child's pid cannot be reused.
        if unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    fn kill(&mut self) -> Result<(), String> {
        self.0.kill().map_err(|e| e.to_string())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Option<i32>, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait().map_err(|e| e.to_string())? {
                return Ok(Some(status_code(status)));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

pub struct RealSpawner;

impl Spawner for RealSpawner {
    fn which(&self, name: &str) -> Option<PathBuf> {
        tessel_core::shell::which(name)
    }

    fn spawn(&self, argv: &[String]) -> Result<Box<dyn Child>, String> {
        use std::process::{Command, Stdio};
        // std closes every other descriptor (close_fds) by using CLOEXEC throughout.
        Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).spawn()
            .map(|child| Box::new(RealChild(child)) as Box<dyn Child>)
            .map_err(|e| tessel_core::pyerr::os_error_path(&e, std::path::Path::new(&argv[0])))
    }
}

/// The argv prefix swayidle runs to reach the primary (the Rust binary).
pub fn self_command() -> Vec<String> {
    if let Ok(path) = std::env::var("BRIGHTNESS_MANAGER_EXECUTABLE") {
        if !path.is_empty() {
            return vec![path];
        }
    }
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "tessel-brightness".into());
    vec![exe.strip_suffix(" (deleted)").unwrap_or(&exe).to_owned()]
}

pub struct Inner {
    on_error: Box<dyn Fn(&str)>,
    spawner: Rc<dyn Spawner>,
    scheduler: Rc<dyn Scheduler>,
    self_command: Vec<String>,
    ready: Cell<bool>,
    error: RefCell<String>,
    enabled: Cell<bool>,
    delay: Cell<i64>,
    started: Cell<bool>,
    signature: Cell<Option<(bool, i64)>>,
    token: RefCell<String>,
    process: RefCell<Option<Box<dyn Child>>>,
    watch: Cell<Option<u64>>,
    /// Python patches `_start_watch` away in its unit tests.
    watching: Cell<bool>,
}

#[derive(Clone)]
pub struct IdleController(pub Rc<Inner>);

/// What the application needs from an idle listener (fakeable).
pub trait IdleListener {
    fn ready(&self) -> bool;
    fn token(&self) -> String;
    fn start(&self, enabled: bool, delay: i64) -> Result<(), String>;
    fn update(&self, enabled: bool, delay: i64) -> Result<(), String>;
    fn allows(&self, token: &str) -> bool;
    fn close(&self);
}

impl IdleController {
    pub fn new(on_error: impl Fn(&str) + 'static, spawner: Rc<dyn Spawner>, scheduler: Rc<dyn Scheduler>,
               self_command: Vec<String>) -> Self {
        Self(Rc::new(Inner {
            on_error: Box::new(on_error), spawner, scheduler, self_command, ready: Cell::new(false), error: RefCell::new(String::new()),
            enabled: Cell::new(false), delay: Cell::new(DEFAULT_DELAY), started: Cell::new(false), signature: Cell::new(None),
            token: RefCell::new(String::new()), process: RefCell::new(None), watch: Cell::new(None), watching: Cell::new(true),
        }))
    }

    pub fn set_watching(&self, watching: bool) {
        self.0.watching.set(watching);
    }

    pub fn error(&self) -> String {
        self.0.error.borrow().clone()
    }

    pub fn has_watch(&self) -> bool {
        self.0.watch.get().is_some()
    }

    fn validate(enabled: bool, delay: i64) -> Result<(), String> {
        let _ = enabled;
        if !(1..=MAX_DELAY).contains(&delay) {
            return Err(format!("The dim delay must be a whole number of seconds from 1 to {MAX_DELAY}."));
        }
        Ok(())
    }

    /// The swayidle command line for a policy (`_command`).
    pub fn command(&self, option: &str, detached: bool) -> String {
        let mut words = self.0.self_command.clone();
        words.push(option.into());
        words.push(self.0.token.borrow().clone());
        let command = shell_join(&words);
        // swayidle executes pending resume commands while terminating. Waiting
        // for the primary application's main loop here would deadlock a policy
        // update that is itself waiting for this owned swayidle child to stop.
        if detached { command + " </dev/null >/dev/null 2>&1 &" } else { command }
    }

    /// Tests: set the policy token directly.
    pub fn set_token(&self, token: &str) {
        self.0.token.replace(token.into());
    }

    fn start_watch(&self) {
        if self.0.watch.get().is_some() || !self.0.watching.get() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let id = self.0.scheduler.timeout(1000, Box::new(move || poll(weak)));
        self.0.watch.set(Some(id));
    }

    fn stop_watch(&self) {
        if let Some(id) = self.0.watch.take() {
            self.0.scheduler.cancel(id);
        }
    }

    fn check_child(&self) -> bool {
        let status = match self.0.process.borrow_mut().as_mut() {
            None => return false,
            Some(child) => child.poll(),
        };
        if let Some(status) = status {
            self.0.process.replace(None);
            self.fail(&format!("Automatic dimming stopped unexpectedly (exit status {status})."));
            return false;
        }
        true
    }

    fn fail(&self, message: &str) {
        self.0.ready.set(false);
        self.0.token.replace(String::new());
        self.0.error.replace(message.into());
        (self.0.on_error)(message);
    }

    fn stop_child(&self) -> bool {
        self.0.ready.set(false);
        let Some(mut process) = self.0.process.take() else { return true };
        if process.poll().is_some() {
            return true;
        }
        let outcome = (|| -> Result<(), String> {
            process.terminate()?;
            if process.wait(Duration::from_secs(1))?.is_none() {
                process.kill()?;
                if process.wait(Duration::from_secs(1))?.is_none() {
                    return Err("Timed out waiting for the listener to exit".into());
                }
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            // Keep ownership if termination could not be confirmed; never
            // launch a second listener on top of a surviving child.
            self.0.process.replace(Some(process));
            self.fail(&format!("Could not stop the automatic dimming listener: {error}"));
            return false;
        }
        true
    }
}

fn poll(weak: Weak<Inner>) {
    let Some(inner) = weak.upgrade() else { return };
    let controller = IdleController(inner);
    controller.0.watch.set(None);
    if controller.check_child() {
        controller.start_watch();
    }
}

impl IdleListener for IdleController {
    fn ready(&self) -> bool {
        self.0.ready.get()
    }

    fn token(&self) -> String {
        self.0.token.borrow().clone()
    }

    fn start(&self, enabled: bool, delay: i64) -> Result<(), String> {
        Self::validate(enabled, delay)?;
        self.0.started.set(true);
        self.update(enabled, delay)
    }

    fn update(&self, enabled: bool, delay: i64) -> Result<(), String> {
        Self::validate(enabled, delay)?;
        self.0.enabled.set(enabled);
        self.0.delay.set(delay);
        if !self.0.started.get() {
            return Ok(());
        }
        let signature = (enabled, delay);
        if Some(signature) == self.0.signature.get() {
            if enabled {
                self.check_child();
            }
            return Ok(());
        }
        // The caller restores any dimmed displays when replacing a policy.
        // Old child resume callbacks cannot reactivate obsolete hardware work.
        self.0.token.replace(String::new());
        self.stop_watch();
        if !self.stop_child() {
            return Ok(());
        }
        self.0.signature.set(Some(signature));
        self.0.error.replace(String::new());
        if !enabled {
            self.0.ready.set(true);
            return Ok(());
        }
        let Some(swayidle) = self.0.spawner.which("swayidle") else {
            self.fail("Automatic dimming requires swayidle.");
            return Ok(());
        };
        self.0.token.replace(token_urlsafe(24));
        let dim = self.command("--idle-dim", false);
        let restore = self.command("--idle-restore", true);
        let command: Vec<String> = vec![swayidle.to_string_lossy().into_owned(), "-w".into(), "-C".into(), "/dev/null".into(),
            "timeout".into(), delay.to_string(), dim, "resume".into(), restore.clone(), "before-sleep".into(), restore.clone(),
            "after-resume".into(), restore];
        match self.0.spawner.spawn(&command) {
            Err(error) => {
                self.fail(&format!("Could not start automatic dimming: {error}"));
                return Ok(());
            }
            Ok(child) => {
                self.0.process.replace(Some(child));
            }
        }
        self.0.ready.set(true);
        self.start_watch();
        Ok(())
    }

    /// Validate a callback's policy; the application still checks idleness.
    fn allows(&self, token: &str) -> bool {
        if !self.0.started.get() || !self.0.enabled.get() || !self.0.ready.get() || self.0.token.borrow().is_empty() {
            return false;
        }
        if !compare_digest(token, &self.0.token.borrow()) {
            return false;
        }
        self.check_child()
    }

    fn close(&self) {
        self.0.started.set(false);
        self.0.token.replace(String::new());
        self.stop_watch();
        self.stop_child();
        self.0.signature.set(None);
    }
}
