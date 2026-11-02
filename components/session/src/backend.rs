//! Explicit session actions through the existing locker, labwc and logind
//! (session_backend.py). Commands and system-bus calls are injectable so the
//! tests never lock, log out, suspend, restart or power off anything.

use crate::bus::*;
use crate::end::{EndSession, Ending, Finished, CLOSE_TIMEOUT, READY_TIMEOUT};
use crate::windows::{helper_factory, WindowFactory};
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::{Rc, Weak};
use tessel_core::tr;

pub const LOCK_COMMAND: [&str; 4] = ["systemctl", "--user", "start", "tessel-lock.service"];
pub const LOGOUT_COMMAND: [&str; 2] = ["labwc", "--exit"];
pub const END_ACTIONS: [&str; 3] = ["logout", "restart", "poweroff"];
pub const DEFAULT_SEAT: &str = "/org/freedesktop/DisplayManager/Seat0";

/// `done(ok, error)`.
pub type Done = Box<dyn FnOnce(bool, String)>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub sleep: bool,
    pub restart: bool,
    pub poweroff: bool,
    pub switch_user: bool,
}

impl Capabilities {
    pub fn all() -> Self { Self { sleep: true, restart: true, poweroff: true, switch_user: false } }
    pub fn get(&self, key: &str) -> bool {
        match key {
            "sleep" => self.sleep,
            "restart" => self.restart,
            "poweroff" => self.poweroff,
            "switch_user" => self.switch_user,
            _ => false,
        }
    }
    pub fn set(&mut self, key: &str, value: bool) {
        match key {
            "sleep" => self.sleep = value,
            "restart" => self.restart = value,
            "poweroff" => self.poweroff = value,
            "switch_user" => self.switch_user = value,
            _ => {}
        }
    }
}

/// What the application needs from a backend (faked by the UI tests).
pub trait Backend {
    fn start(&self);
    fn capabilities(&self) -> Capabilities;
    fn can_logout(&self) -> bool;
    fn error(&self) -> String;
    fn perform(&self, action: &str, done: Done);
    fn end_session(&self, action: &str, finished: Finished) -> Rc<dyn Ending>;
    fn close(&self);
}

/// `Gio.Subprocess(STDOUT_SILENCE | STDERR_PIPE)` + `communicate_utf8_async`:
/// success = exit status 0; the stripped stderr is the error text.
pub trait CommandRunner {
    fn run(&self, argv: &[&str], done: Done);
}

pub struct GioRunner;

impl CommandRunner for GioRunner {
    fn run(&self, argv: &[&str], done: Done) {
        let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
        let child = match gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE) {
            Ok(child) => child,
            Err(error) => return done(false, error.message().to_owned()),
        };
        let process = child.clone();
        child.communicate_utf8_async(None, None::<&gio::Cancellable>, move |result| match result {
            Ok((_, stderr)) => done(process.is_successful(), stderr.as_deref().unwrap_or("").trim().to_owned()),
            Err(error) => done(false, error.message().to_owned()),
        });
    }
}

/// `int(os.environ['LABWC_PID']) > 1 and Path('/proc/<pid>/comm').read_text().strip() == 'labwc'`.
pub fn can_logout_with(labwc_pid: Option<&str>, read: impl Fn(&Path) -> std::io::Result<String>) -> bool {
    let Some(pid) = labwc_pid.and_then(crate::args::python_int) else { return false };
    pid > 1 && read(Path::new(&format!("/proc/{pid}/comm"))).is_ok_and(|comm| tessel_core::pyjson::py_strip(&comm) == "labwc")
}

/// Connects to the system bus (`Gio.bus_get(SYSTEM, cancel, …)`).
pub type Connector = Box<dyn Fn(&gio::Cancellable, Box<dyn FnOnce(Result<Rc<dyn SystemBus>, String>)>)>;

pub fn system_connector() -> Connector {
    Box::new(|cancel, done| {
        gio::bus_get(gio::BusType::System, Some(cancel), move |result| {
            done(result.map(|c| GioSystemBus::new(c) as Rc<dyn SystemBus>).map_err(|e| e.message().to_owned()))
        })
    })
}

struct Inner {
    changed: Rc<dyn Fn()>,
    connector: Connector,
    runner: Rc<dyn CommandRunner>,
    windows: RefCell<Option<Option<WindowFactory>>>,
    bus: RefCell<Option<Rc<dyn SystemBus>>>,
    closed: Cell<bool>,
    capabilities: Cell<Capabilities>,
    greeter: RefCell<Option<String>>,
    error: RefCell<String>,
    cancel: gio::Cancellable,
}

#[derive(Clone)]
pub struct SessionBackend(Rc<Inner>);

impl SessionBackend {
    pub fn new(changed: Rc<dyn Fn()>) -> Self {
        Self::with(changed, system_connector(), Rc::new(GioRunner), None)
    }

    /// `windows = None` → the installed Tessel Dock helper (looked up on first use).
    pub fn with(changed: Rc<dyn Fn()>, connector: Connector, runner: Rc<dyn CommandRunner>,
                windows: Option<Option<WindowFactory>>) -> Self {
        Self(Rc::new(Inner {
            changed, connector, runner, windows: RefCell::new(windows), bus: RefCell::new(None),
            closed: Cell::new(false), capabilities: Cell::new(Capabilities::default()), greeter: RefCell::new(None),
            error: RefCell::default(), cancel: gio::Cancellable::new(),
        }))
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    pub fn bus(&self) -> Option<Rc<dyn SystemBus>> { self.0.bus.borrow().clone() }
    pub fn set_bus(&self, bus: Option<Rc<dyn SystemBus>>) { self.0.bus.replace(bus); }
    pub fn set_capabilities(&self, capabilities: Capabilities) { self.0.capabilities.set(capabilities); }
    pub fn greeter(&self) -> Option<String> { self.0.greeter.borrow().clone() }
    pub fn set_greeter(&self, greeter: Option<&str>) { self.0.greeter.replace(greeter.map(String::from)); }

    fn notify(&self) {
        if !self.0.closed.get() {
            (self.0.changed)();
        }
    }

    /// `_connected`: query capabilities and the display manager's seats.
    pub fn connected(&self, result: Result<Rc<dyn SystemBus>, String>) {
        let bus = match result {
            Ok(bus) => bus,
            Err(message) => {
                if !self.0.closed.get() {
                    self.0.error.replace(message);
                    self.notify();
                }
                return;
            }
        };
        self.0.bus.replace(Some(bus.clone()));
        if self.0.closed.get() {
            return;
        }
        for (key, method) in [("sleep", "CanSuspend"), ("restart", "CanReboot"), ("poweroff", "CanPowerOff")] {
            let weak = self.weak();
            bus.call(BusCall::new(LOGIN1, LOGIN1_PATH, LOGIN1_MANAGER, method, None, gio::DBusCallFlags::NO_AUTO_START,
                                  3000, Some(&self.0.cancel)),
                     Box::new(move |reply| if let Some(inner) = weak.upgrade() { SessionBackend(inner).capability(key, reply) }));
        }
        // Same display-manager interface the lock screen's Switch User uses.
        let weak = self.weak();
        bus.call(BusCall::new(DISPLAY_MANAGER, DISPLAY_MANAGER_PATH, "org.freedesktop.DBus.Properties", "Get",
                              Some(Variant::from((DISPLAY_MANAGER, "Seats"))), gio::DBusCallFlags::NO_AUTO_START, 3000,
                              Some(&self.0.cancel)),
                 Box::new(move |reply| if let Some(inner) = weak.upgrade() { SessionBackend(inner).seats(reply) }));
    }

    fn seats(&self, reply: Reply) {
        if let Ok(value) = reply {
            let mut seats = value.try_child_value(0);
            if let Some(inner) = seats.as_ref().and_then(|v| v.as_variant()) {
                seats = Some(inner);
            }
            let paths: Vec<String> = match seats {
                Some(seats) if seats.is_container() && !seats.is_type(glib::VariantTy::VARIANT) => {
                    seats.iter().filter_map(|item| item.str().map(String::from)).collect()
                }
                _ => Vec::new(),
            };
            // Only the current local seat; never send another seat away.
            let expected = std::env::var("XDG_SEAT_PATH").unwrap_or_else(|_| DEFAULT_SEAT.into());
            if paths.contains(&expected) {
                self.0.greeter.replace(Some(expected));
                let mut capabilities = self.0.capabilities.get();
                capabilities.switch_user = true;
                self.0.capabilities.set(capabilities);
            }
        }
        self.notify();
    }

    fn capability(&self, key: &str, reply: Reply) {
        match reply {
            Ok(value) => {
                let answer = value.try_child_value(0).and_then(|v| v.str().map(String::from)).unwrap_or_default();
                let mut capabilities = self.0.capabilities.get();
                capabilities.set(key, matches!(answer.as_str(), "yes" | "challenge"));
                self.0.capabilities.set(capabilities);
            }
            Err(message) => {
                self.0.error.replace(message);
            }
        }
        self.notify();
    }

    fn command(&self, argv: &[&str], done: Done) { self.0.runner.run(argv, done) }

    fn switch(&self, done: Done) {
        let (Some(bus), Some(greeter)) = (self.bus(), self.greeter()) else {
            return done(false, tr!("Switching users is unavailable."));
        };
        bus.call(BusCall::new(DISPLAY_MANAGER, &greeter, SEAT_INTERFACE, "SwitchToGreeter", None,
                              gio::DBusCallFlags::NO_AUTO_START, 5000, None),
                 Box::new(move |reply| match reply {
                     Ok(_) => done(true, String::new()),
                     Err(message) => done(false, message),
                 }));
    }

    /// logind Suspend/Reboot/PowerOff(b interactive=true): normal authorization
    /// and inhibitor handling; never forced.
    fn power(&self, method: &str, done: Done) {
        let key = match method {
            "Suspend" => "sleep",
            "Reboot" => "restart",
            "PowerOff" => "poweroff",
            _ => "",
        };
        let bus = self.bus();
        let Some(bus) = bus.filter(|_| self.0.capabilities.get().get(key)) else {
            return done(false, tr!("That action is unavailable."));
        };
        bus.call(BusCall::new(LOGIN1, LOGIN1_PATH, LOGIN1_MANAGER, method, Some(Variant::from((true,))),
                              gio::DBusCallFlags::NONE, 120000, None),
                 Box::new(move |reply| match reply {
                     Ok(_) => done(true, String::new()),
                     Err(message) => done(false, message),
                 }));
    }
}

impl Backend for SessionBackend {
    fn start(&self) {
        let weak = self.weak();
        (self.0.connector)(&self.0.cancel, Box::new(move |result| {
            if let Some(inner) = weak.upgrade() {
                SessionBackend(inner).connected(result);
            }
        }));
    }

    fn capabilities(&self) -> Capabilities { self.0.capabilities.get() }

    fn can_logout(&self) -> bool {
        can_logout_with(std::env::var("LABWC_PID").ok().as_deref(), |path| std::fs::read_to_string(path))
    }

    fn error(&self) -> String { self.0.error.borrow().clone() }

    fn perform(&self, action: &str, done: Done) {
        match action {
            "lock" => self.command(&LOCK_COMMAND, done),
            "switch_user" => {
                if !self.0.capabilities.get().switch_user || self.bus().is_none() || self.greeter().is_none() {
                    return done(false, tr!("Switching users is unavailable."));
                }
                let this = self.clone();
                // Lock first so this session is protected while the greeter runs.
                self.command(&LOCK_COMMAND, Box::new(move |ok, error| {
                    if ok {
                        this.switch(done);
                    } else {
                        done(false, tr!("The screen could not be locked. Switch User was cancelled. ") + &error);
                    }
                }));
            }
            "logout" => {
                if !self.can_logout() {
                    done(false, tr!("The labwc session is unavailable."));
                } else {
                    self.command(&LOGOUT_COMMAND, done);
                }
            }
            "sleep" => {
                if !self.0.capabilities.get().sleep {
                    return done(false, tr!("Sleep is unavailable."));
                }
                let this = self.clone();
                self.command(&LOCK_COMMAND, Box::new(move |ok, error| {
                    if ok {
                        this.power("Suspend", done);
                    } else {
                        done(false, tr!("The screen could not be locked. Sleep was cancelled. ") + &error);
                    }
                }));
            }
            "poweroff" => self.power("PowerOff", done),
            "restart" => self.power("Reboot", done),
            _ => done(false, tr!("Unknown session action.")),
        }
    }

    fn end_session(&self, action: &str, finished: Finished) -> Rc<dyn Ending> {
        let factory = self.0.windows.borrow_mut().get_or_insert_with(helper_factory).clone();
        let ending = EndSession::new(action, finished, self.bus(), factory, CLOSE_TIMEOUT, READY_TIMEOUT);
        ending.start();
        Rc::new(ending)
    }

    fn close(&self) {
        self.0.closed.set(true);
        self.0.cancel.cancel();
    }
}
