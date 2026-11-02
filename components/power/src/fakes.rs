//! Test doubles shared by unit tests and the headless UI test binary.
//! Nothing here touches a real bus, display power or sleep state.

use tessel_power_core::backend::{BackendState, Guard, PowerSystem};
use crate::idle::Processes;
use std::cell::{Cell, Ref, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// `FakeBackend` of the Python UI tests plus recorders for the app tests.
#[derive(Default)]
pub struct FakeSystem {
    pub state: RefCell<BackendState>,
    pub history: RefCell<Vec<String>>,
    pub reject: Cell<bool>,
    pub idle_checks: RefCell<Vec<Box<dyn FnOnce(bool)>>>,
    pub sleeps: RefCell<Vec<(String, bool, Option<Guard>)>>,
    pub explicit: RefCell<Vec<(String, bool)>>,
    pub started: Cell<usize>,
    pub closed: Cell<usize>,
}

impl FakeSystem {
    pub fn ready() -> Rc<Self> {
        let fake = Self::default();
        {
            let mut s = fake.state.borrow_mut();
            s.ready = true;
            s.profiles_ready = true;
            s.power_ready = true;
            s.sleep_ready = true;
            s.profiles = vec!["power-saver".into(), "balanced".into(), "performance".into()];
            s.active_profile = "balanced".into();
            s.power_source_known = true;
            for action in ["suspend", "hibernate", "suspend-then-hibernate"] {
                s.capabilities.insert(action.into(), "yes".into());
            }
        }
        Rc::new(fake)
    }

    pub fn set(&self, f: impl FnOnce(&mut BackendState)) {
        f(&mut self.state.borrow_mut())
    }

    /// Answer the oldest pending `idle_inhibited` check.
    pub fn answer_idle(&self, inhibited: bool) {
        let callback = self.idle_checks.borrow_mut().remove(0);
        callback(inhibited);
    }
}

impl PowerSystem for FakeSystem {
    fn state(&self) -> Ref<'_, BackendState> {
        self.state.borrow()
    }
    fn start(&self) {
        self.started.set(self.started.get() + 1);
    }
    fn close(&self) {
        self.closed.set(self.closed.get() + 1);
    }
    fn set_profile(&self, profile: &str) -> bool {
        self.history.borrow_mut().push(profile.into());
        if self.reject.get() {
            self.state.borrow_mut().error = "Authorization was cancelled.".into();
            return false;
        }
        self.state.borrow_mut().active_profile = profile.into();
        true
    }
    fn idle_inhibited(&self, callback: Box<dyn FnOnce(bool)>) {
        self.idle_checks.borrow_mut().push(callback);
    }
    fn sleep(&self, action: &str, interactive: bool, guard: Option<Guard>) -> bool {
        self.sleeps.borrow_mut().push((action.into(), interactive, guard));
        true
    }
    fn explicit_power(&self, action: &str, callback: Option<Box<dyn FnOnce(bool)>>, interactive: bool) -> bool {
        self.explicit.borrow_mut().push((action.into(), interactive));
        if let Some(callback) = callback {
            callback(true);
        }
        true
    }
}

/// Records spawned commands; children never run.
#[derive(Default)]
pub struct FakeProcesses {
    pub missing: Cell<bool>,
    pub fail_spawn: RefCell<Option<String>>,
    pub commands: RefCell<Vec<Vec<String>>>,
    pub pids: RefCell<Vec<u32>>,
    pub watched: RefCell<Vec<(u32, Option<Box<dyn FnOnce(i32)>>)>>,
    pub signals: RefCell<Vec<(u32, i32)>>,
    next: Cell<u32>,
}

impl FakeProcesses {
    pub fn last(&self) -> Vec<String> {
        self.commands.borrow().last().cloned().unwrap_or_default()
    }

    /// Fire the child watch of `pid`.
    pub fn exit(&self, pid: u32, status: i32) {
        let callback = {
            let mut watched = self.watched.borrow_mut();
            watched.iter_mut().find(|(p, _)| *p == pid).and_then(|(_, c)| c.take())
        };
        if let Some(callback) = callback {
            callback(status);
        }
    }

    pub fn terminated(&self, pid: u32) -> bool {
        self.signals.borrow().iter().any(|&(p, s)| p == pid && s == libc::SIGTERM)
    }

    pub fn killed(&self, pid: u32) -> bool {
        self.signals.borrow().iter().any(|&(p, s)| p == pid && s == libc::SIGKILL)
    }
}

impl Processes for FakeProcesses {
    fn which(&self, name: &str) -> Option<PathBuf> {
        (!self.missing.get()).then(|| PathBuf::from(format!("/usr/bin/{name}")))
    }

    fn spawn(&self, argv: &[String]) -> Result<u32, String> {
        if let Some(error) = self.fail_spawn.borrow().clone() {
            return Err(error);
        }
        let pid = 1000 + self.next.get() + 1;
        self.next.set(self.next.get() + 1);
        self.commands.borrow_mut().push(argv.to_vec());
        self.pids.borrow_mut().push(pid);
        Ok(pid)
    }

    fn watch(&self, pid: u32, callback: Box<dyn FnOnce(i32)>) {
        self.watched.borrow_mut().push((pid, Some(callback)));
    }

    fn signal(&self, pid: u32, signal: i32) {
        self.signals.borrow_mut().push((pid, signal));
    }
}
