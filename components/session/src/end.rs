//! Graceful end-session (session_end.py): ask every open window to close
//! before leaving. Each window open when the request starts receives one close
//! request; nothing is killed. Windows that remain and logind shutdown
//! inhibitors are reported so the user can cancel or continue.

use crate::bus::{BusCall, GioSystemBus, SystemBus, LOGIN1, LOGIN1_MANAGER, LOGIN1_PATH};
use crate::windows::{Window, WindowFactory, WindowSource};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};
use tessel_core::timer::{self, Timeout};

pub const CLOSE_TIMEOUT: u32 = 10000;
pub const READY_TIMEOUT: u32 = 3000;
pub const RELEVANT_INHIBITORS: [&str; 1] = ["shutdown"];
/// Stored untranslated (Python), translated where it is displayed.
pub const NOTE_UNCHECKED: &str = "Open apps could not be checked.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inhibitor {
    pub who: String,
    pub why: String,
    pub mode: String,
    pub pid: u32,
}

/// One `ListInhibitors` row: (what, who, why, mode, uid, pid).
pub type InhibitorRow = (String, String, String, String, u32, u32);

/// Logind inhibitors that affect `action`.
pub fn parse_inhibitors(rows: &[InhibitorRow], action: &str, uid: Option<u32>) -> Vec<Inhibitor> {
    let uid = uid.unwrap_or_else(|| unsafe { libc::getuid() });
    let mut result = Vec::new();
    for (what, who, why, mode, owner, pid) in rows {
        let relevant = what.split(':').any(|kind| RELEVANT_INHIBITORS.contains(&kind));
        if !matches!(mode.as_str(), "block" | "delay") || !relevant {
            continue;
        }
        // Logging out ends only this user's applications; other users'
        // shutdown inhibitors matter only for restart and power off.
        if action == "logout" && *owner != uid {
            continue;
        }
        result.push(Inhibitor { who: who.clone(), why: why.clone(), mode: mode.clone(), pid: *pid });
    }
    result
}

/// What the application uses of one end-session attempt (Python duck type,
/// faked by the UI tests).
pub trait Ending {
    fn start(&self) {}
    fn done(&self) -> bool;
    fn remaining(&self) -> Vec<Window>;
    fn inhibitors(&self) -> Vec<Inhibitor>;
    fn note(&self) -> String;
    /// True when the user must decide: windows remain, a note, or a `block` inhibitor.
    fn blocking(&self) -> bool {
        !self.remaining().is_empty() || !self.note().is_empty() || self.inhibitors().iter().any(|item| item.mode == "block")
    }
    fn set_on_update(&self, on_update: Option<Rc<dyn Fn()>>);
    fn close(&self);
}

/// `finished(windows, inhibitors, note)`.
pub type Finished = Rc<dyn Fn(Vec<Window>, Vec<Inhibitor>, String)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Waiting,
    Closing,
    Reporting,
}

struct Inner {
    action: String,
    finished: Finished,
    bus: RefCell<Option<Rc<dyn SystemBus>>>,
    close_timeout: u32,
    ready_timeout: u32,
    requested: RefCell<BTreeSet<String>>,
    inhibitors: RefCell<Vec<Inhibitor>>,
    note: RefCell<String>,
    done: Cell<bool>,
    cancelled: Cell<bool>,
    timer: RefCell<Option<Timeout>>,
    phase: Cell<Phase>,
    on_update: RefCell<Option<Rc<dyn Fn()>>>,
    backend: RefCell<Option<Rc<dyn WindowSource>>>,
}

/// One graceful close attempt.
#[derive(Clone)]
pub struct EndSession(Rc<Inner>);

impl EndSession {
    /// `factory = None` means the window helper is unavailable.
    pub fn new(action: &str, finished: Finished, bus: Option<Rc<dyn SystemBus>>, factory: Option<WindowFactory>,
               close_timeout: u32, ready_timeout: u32) -> Self {
        let this = Self(Rc::new(Inner {
            action: action.into(), finished, bus: RefCell::new(bus), close_timeout, ready_timeout,
            requested: RefCell::default(), inhibitors: RefCell::default(), note: RefCell::default(),
            done: Cell::new(false), cancelled: Cell::new(false), timer: RefCell::new(None),
            phase: Cell::new(Phase::Waiting), on_update: RefCell::new(None), backend: RefCell::new(None),
        }));
        if let Some(factory) = factory {
            let weak = this.weak();
            let changed: Rc<dyn Fn()> = Rc::new(move || if let Some(this) = upgrade(&weak) { this.changed() });
            let weak = this.weak();
            let error: Rc<dyn Fn(String)> = Rc::new(move |_message| if let Some(this) = upgrade(&weak) { this.error() });
            this.0.backend.replace(Some(factory(changed, error)));
        }
        this
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    fn backend(&self) -> Option<Rc<dyn WindowSource>> { self.0.backend.borrow().clone() }

    /// Test hook (Python sets `ending.backend = None`).
    pub fn drop_backend(&self) { self.0.backend.replace(None); }

    pub fn is_cancelled(&self) -> bool { self.0.cancelled.get() }

    fn begin(&self) {
        let Some(backend) = self.backend() else {
            self.0.note.replace(NOTE_UNCHECKED.into());
            self.0.phase.set(Phase::Closing);
            self.finish_closing();
            return;
        };
        backend.start();
        let weak = self.weak();
        let timer = timer::once(self.0.ready_timeout, move || if let Some(this) = upgrade(&weak) { this.ready_timed_out() });
        self.0.timer.replace(Some(timer));
    }

    fn error(&self) {
        let ready = self.backend().is_some_and(|backend| backend.ready());
        if self.0.phase.get() == Phase::Waiting && !ready {
            self.stop_timer();
            self.0.note.replace(NOTE_UNCHECKED.into());
            self.0.phase.set(Phase::Closing);
            self.finish_closing();
        }
    }

    fn changed(&self) {
        if self.0.done.get() || self.0.cancelled.get() {
            if self.0.phase.get() == Phase::Reporting && !self.0.cancelled.get() {
                let update = self.0.on_update.borrow().clone();
                if let Some(update) = update {
                    update();
                }
            }
            return;
        }
        let Some(backend) = self.backend() else { return };
        if self.0.phase.get() == Phase::Waiting && backend.ready() {
            self.stop_timer();
            self.0.phase.set(Phase::Closing);
            // wlr-foreign-toplevel sends the existing windows before "done";
            // give the helper one main-loop turn to publish them.
            let weak = self.weak();
            timer::once(150, move || if let Some(this) = upgrade(&weak) { this.request_close() });
        } else if self.0.phase.get() == Phase::Closing && !self.0.requested.borrow().is_empty() {
            let open = backend.windows();
            let any_left = open.iter().any(|window| self.0.requested.borrow().contains(&window.id));
            if !any_left {
                self.finish_closing();
            }
        }
    }

    fn ready_timed_out(&self) {
        self.0.timer.replace(None);
        if self.0.phase.get() == Phase::Waiting {
            self.0.note.replace(NOTE_UNCHECKED.into());
            self.0.phase.set(Phase::Closing);
            self.finish_closing();
        }
    }

    fn request_close(&self) {
        if self.0.done.get() || self.0.cancelled.get() {
            return;
        }
        let Some(backend) = self.backend() else { return };
        let ids: Vec<String> = backend.windows().into_iter().map(|window| window.id).collect();
        self.0.requested.replace(ids.iter().cloned().collect());
        if ids.is_empty() {
            self.finish_closing();
            return;
        }
        for id in &ids {
            backend.close_window(id);
        }
        let weak = self.weak();
        let timer = timer::once(self.0.close_timeout, move || {
            if let Some(this) = upgrade(&weak) {
                this.0.timer.replace(None);
                this.finish_closing();
            }
        });
        self.0.timer.replace(Some(timer));
    }

    fn finish_closing(&self) {
        if self.0.done.get() || self.0.cancelled.get() {
            return;
        }
        self.stop_timer();
        self.0.done.set(true);
        self.0.phase.set(Phase::Reporting);
        self.load_inhibitors();
    }

    fn load_inhibitors(&self) {
        if self.0.bus.borrow().is_none() {
            match GioSystemBus::system_sync() {
                Some(bus) => { self.0.bus.replace(Some(bus)); }
                None => {
                    self.report();
                    return;
                }
            }
        }
        let bus = self.0.bus.borrow().clone().expect("bus");
        let weak = self.weak();
        let action = self.0.action.clone();
        bus.call(BusCall::new(LOGIN1, LOGIN1_PATH, LOGIN1_MANAGER, "ListInhibitors", None,
                              gio::DBusCallFlags::NO_AUTO_START, 3000, None),
                 Box::new(move |reply| {
                     let Some(this) = upgrade(&weak) else { return };
                     let rows = reply.ok().and_then(|value| value.try_child_value(0))
                         .and_then(|rows| rows.get::<Vec<InhibitorRow>>());
                     this.0.inhibitors.replace(rows.map(|rows| parse_inhibitors(&rows, &action, None)).unwrap_or_default());
                     this.report();
                 }));
    }

    fn report(&self) {
        if !self.0.cancelled.get() {
            let windows = self.remaining();
            let inhibitors = self.0.inhibitors.borrow().clone();
            let note = self.0.note.borrow().clone();
            (self.0.finished)(windows, inhibitors, note);
        }
    }

    fn stop_timer(&self) {
        if let Some(timer) = self.0.timer.take() {
            timer.cancel();
        }
    }
}

impl Ending for EndSession {
    fn start(&self) { self.begin() }

    fn done(&self) -> bool { self.0.done.get() }

    fn remaining(&self) -> Vec<Window> {
        match self.backend() {
            Some(backend) if backend.ready() => backend.windows(),
            _ => Vec::new(),
        }
    }

    fn inhibitors(&self) -> Vec<Inhibitor> { self.0.inhibitors.borrow().clone() }

    fn note(&self) -> String { self.0.note.borrow().clone() }

    fn set_on_update(&self, on_update: Option<Rc<dyn Fn()>>) { self.0.on_update.replace(on_update); }

    fn close(&self) {
        self.0.cancelled.set(true);
        self.stop_timer();
        if let Some(backend) = self.backend() {
            backend.close();
        }
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<EndSession> { weak.upgrade().map(EndSession) }
