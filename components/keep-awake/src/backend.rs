//! Own a logind idle/sleep inhibitor for this desktop session
//! (port of keep_awake_backend.py `InhibitorBackend`).

use suite_core::pyerr::{os_error_text as python_os_error_text, python_error_text};
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, OwnedFd};
use std::rc::{Rc, Weak};

pub const LOGIN_NAME: &str = "org.freedesktop.login1";
pub const LOGIN_PATH: &str = "/org/freedesktop/login1";
pub const LOGIN_INTERFACE: &str = "org.freedesktop.login1.Manager";
/// `Inhibit(ssss)` arguments, exactly as the Python backend sends them.
pub const INHIBIT_ARGS: (&str, &str, &str, &str) =
    ("idle:sleep", "Keep Awake", "Keep this desktop awake until turned off", "block");
pub const UNAVAILABLE: &str = "System power management is unavailable.";
pub const NO_HANDLE: &str = "The system did not return an inhibitor handle.";

/// The reply of one `Inhibit` call: the received descriptor, or the error text.
pub type InhibitReply = Result<OwnedFd, String>;

/// What the backend sends `Inhibit` through. The real one is a
/// `gio::DBusConnection`; tests substitute a recorder.
pub trait LoginConnection {
    /// Start the call; `done` runs later on the main context. `Err` is an
    /// immediate failure (Python's synchronous raise).
    fn inhibit(&self, owner: &str, cancellable: &gio::Cancellable, done: Box<dyn FnOnce(InhibitReply)>)
               -> Result<(), String>;
}

impl LoginConnection for gio::DBusConnection {
    fn inhibit(&self, owner: &str, cancellable: &gio::Cancellable, done: Box<dyn FnOnce(InhibitReply)>)
               -> Result<(), String> {
        self.call_with_unix_fd_list(
            Some(owner), LOGIN_PATH, LOGIN_INTERFACE, "Inhibit", Some(&INHIBIT_ARGS.to_variant()),
            Some(glib::VariantTy::new("(h)").unwrap()), gio::DBusCallFlags::NO_AUTO_START, 10000,
            None::<&gio::UnixFDList>, Some(cancellable), move |result| {
                done(match result {
                    Err(error) => Err(python_error_text(&error)),
                    Ok((_, None)) => Err(NO_HANDLE.into()),
                    Ok((reply, Some(descriptors))) => {
                        let index = reply.get::<(glib::variant::Handle,)>().map(|(h,)| h.0).unwrap_or(-1);
                        descriptors.get(index).map_err(|e| python_error_text(&e))
                        // `descriptors` drops here, closing its own copy.
                    }
                })
            });
        Ok(())
    }
}

/// Which bus logind lives on. `Address` is for private-bus tests.
#[derive(Clone, Debug)]
pub enum BusSource {
    System,
    Address(String),
}

struct Inner {
    on_changed: Rc<dyn Fn()>,
    source: BusSource,
    ready: Cell<bool>,
    requested: Cell<bool>,
    pending: Cell<bool>,
    error: RefCell<String>,
    connection: RefCell<Option<Rc<dyn LoginConnection>>>,
    bus: RefCell<Option<gio::DBusConnection>>,
    owner: RefCell<Option<String>>,
    fd: RefCell<Option<OwnedFd>>,
    generation: Cell<u64>,
    request: RefCell<Option<gio::Cancellable>>,
    watch: RefCell<Option<Box<dyn FnOnce()>>>,
    closed_handler: RefCell<Option<glib::SignalHandlerId>>,
    reconnect: RefCell<Option<glib::SourceId>>,
    closed: Cell<bool>,
    bus_cancel: gio::Cancellable,
}

/// Holds the logind block FD while requested; see the contract §2.2 / §4.
#[derive(Clone)]
pub struct InhibitorBackend(Rc<Inner>);

impl InhibitorBackend {
    pub fn new(on_changed: Rc<dyn Fn()>) -> Self { Self::with_bus(on_changed, BusSource::System) }

    pub fn with_bus(on_changed: Rc<dyn Fn()>, source: BusSource) -> Self {
        Self(Rc::new(Inner {
            on_changed, source, ready: Cell::new(false), requested: Cell::new(false), pending: Cell::new(false),
            error: RefCell::new(String::new()), connection: RefCell::new(None), bus: RefCell::new(None),
            owner: RefCell::new(None), fd: RefCell::new(None), generation: Cell::new(0),
            request: RefCell::new(None), watch: RefCell::new(None), closed_handler: RefCell::new(None),
            reconnect: RefCell::new(None), closed: Cell::new(false), bus_cancel: gio::Cancellable::new(),
        }))
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    fn changed(&self) { (self.0.on_changed)() }

    pub fn active(&self) -> bool { self.0.fd.borrow().is_some() }
    pub fn ready(&self) -> bool { self.0.ready.get() }
    pub fn requested(&self) -> bool { self.0.requested.get() }
    pub fn pending(&self) -> bool { self.0.pending.get() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }
    pub fn owner(&self) -> Option<String> { self.0.owner.borrow().clone() }
    /// The held descriptor number (tests).
    pub fn descriptor(&self) -> Option<i32> { self.0.fd.borrow().as_ref().map(AsRawFd::as_raw_fd) }

    fn set_error(&self, text: impl Into<String>) { self.0.error.replace(text.into()); }

    /// Test hook: behave as if connected to `connection` with logind owned by `owner`.
    pub fn attach(&self, connection: Rc<dyn LoginConnection>, owner: &str, ready: bool) {
        self.0.connection.replace(Some(connection));
        self.0.owner.replace(Some(owner.into()));
        self.0.ready.set(ready);
    }

    pub fn start(&self) {
        if self.0.closed.get() || self.0.connection.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let ready = move |result: Result<gio::DBusConnection, glib::Error>| {
            if let Some(inner) = weak.upgrade() {
                InhibitorBackend(inner).bus_ready(result);
            }
        };
        match &self.0.source {
            BusSource::System => gio::bus_get(gio::BusType::System, Some(&self.0.bus_cancel), ready),
            BusSource::Address(address) => gio::DBusConnection::for_address(
                address, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None::<&gio::DBusAuthObserver>, Some(&self.0.bus_cancel), ready),
        }
    }

    fn bus_ready(&self, result: Result<gio::DBusConnection, glib::Error>) {
        let connection = match result {
            Ok(connection) => connection,
            Err(error) => {
                if !self.0.closed.get() {
                    self.set_error(format!("System power management is unavailable: {}", error.message()));
                    self.changed();
                    self.retry();
                }
                return;
            }
        };
        if self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        // connect_local: the handler need not be Send (it only runs on this context).
        let handler = connection.connect_local("closed", false, move |_| {
            if let Some(inner) = weak.upgrade() {
                InhibitorBackend(inner).disconnected();
            }
            None
        });
        self.0.closed_handler.replace(Some(handler));
        self.0.connection.replace(Some(Rc::new(connection.clone())));
        self.0.bus.replace(Some(connection.clone()));
        let (appeared, vanished) = (self.weak(), self.weak());
        let watch = gio::bus_watch_name_on_connection(&connection, LOGIN_NAME, gio::BusNameWatcherFlags::NONE,
            move |_, _, owner| {
                if let Some(inner) = appeared.upgrade() {
                    InhibitorBackend(inner).appeared(owner);
                }
            },
            move |_, _| {
                if let Some(inner) = vanished.upgrade() {
                    InhibitorBackend(inner).vanished();
                }
            });
        self.0.watch.replace(Some(Box::new(move || gio::bus_unwatch_name(watch))));
    }

    /// logind appeared (or changed owner).
    pub fn appeared(&self, owner: &str) {
        if self.0.closed.get() {
            return;
        }
        if self.0.owner.borrow().as_deref() != Some(owner) {
            self.cancel_request();
            self.release();
        }
        self.0.owner.replace(Some(owner.into()));
        self.0.ready.set(true);
        self.set_error("");
        if self.requested() && !self.active() {
            self.acquire();
        } else {
            self.changed();
        }
    }

    /// logind vanished.
    pub fn vanished(&self) {
        if self.0.closed.get() {
            return;
        }
        self.cancel_request();
        self.release();
        self.0.owner.replace(None);
        self.0.ready.set(false);
        self.set_error(UNAVAILABLE);
        self.changed();
    }

    fn disconnected(&self) {
        self.vanished();
        if self.0.closed.get() {
            return;
        }
        self.detach_bus();
        self.retry();
    }

    fn retry(&self) {
        if self.0.reconnect.borrow().is_some() || self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        let id = glib::timeout_add_seconds_local(2, move || {
            if let Some(inner) = weak.upgrade() {
                inner.reconnect.replace(None);
                InhibitorBackend(inner).start();
            }
            glib::ControlFlow::Break
        });
        self.0.reconnect.replace(Some(id));
    }

    pub fn set_enabled(&self, enabled: bool) -> bool {
        if self.0.closed.get() {
            return false;
        }
        self.0.requested.set(enabled);
        if !enabled {
            self.cancel_request();
            self.release();
            if self.ready() {
                self.set_error("");
            }
            self.changed();
        } else if self.ready() && !self.active() && !self.pending() {
            self.acquire();
        } else {
            self.changed();
        }
        true
    }

    pub fn toggle(&self) -> bool { self.set_enabled(!self.requested()) }

    fn acquire(&self) {
        if self.0.closed.get() || !self.ready() || !self.requested() || self.pending() {
            return;
        }
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        let request = gio::Cancellable::new();
        self.0.request.replace(Some(request.clone()));
        self.0.pending.set(true);
        self.set_error("");
        self.changed();
        let connection = self.0.connection.borrow().clone();
        let owner = self.owner().unwrap_or_default();
        let weak = self.weak();
        let started = match connection {
            None => Err("'NoneType' object has no attribute 'call_with_unix_fd_list'".to_owned()),
            Some(connection) => connection.inhibit(&owner, &request, Box::new(move |reply| {
                match weak.upgrade() {
                    Some(inner) => InhibitorBackend(inner).acquired(generation, reply),
                    None => drop(reply), // closes a late descriptor
                }
            })),
        };
        if let Err(error) = started {
            self.0.pending.set(false);
            self.0.requested.set(false);
            self.0.request.replace(None);
            self.set_error(format!("Could not keep the desktop awake: {error}"));
            self.changed();
        }
    }

    fn acquired(&self, generation: u64, reply: InhibitReply) {
        let current = !self.0.closed.get() && generation == self.0.generation.get();
        // Always finish a stale call so its received FD is released; never keep
        // an inhibitor requested before Disable or disconnect.
        if current && self.requested() {
            match reply.and_then(|fd| set_cloexec(&fd).map(|()| fd)) {
                Ok(fd) => {
                    self.0.fd.replace(Some(fd));
                    self.set_error("");
                }
                Err(error) => {
                    self.0.requested.set(false);
                    self.set_error(format!("Could not keep the desktop awake: {error}"));
                }
            }
        } else {
            drop(reply);
        }
        if current {
            self.0.pending.set(false);
            self.0.request.replace(None);
            self.changed();
        }
    }

    fn cancel_request(&self) {
        self.0.generation.set(self.0.generation.get() + 1);
        if let Some(request) = self.0.request.take() {
            request.cancel();
        }
        self.0.pending.set(false);
    }

    fn release(&self) {
        // Dropping the OwnedFd closes it, which releases the logind inhibitor.
        let descriptor = self.0.fd.take();
        drop(descriptor);
    }

    fn detach_bus(&self) {
        if let Some(unwatch) = self.0.watch.take() {
            unwatch();
        }
        if let (Some(bus), Some(handler)) = (self.0.bus.take(), self.0.closed_handler.take()) {
            bus.disconnect(handler);
        }
        self.0.connection.replace(None);
    }

    pub fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.requested.set(false);
        self.0.ready.set(false);
        self.0.bus_cancel.cancel();
        self.cancel_request();
        self.release();
        self.detach_bus();
        if let Some(id) = self.0.reconnect.take() {
            id.remove();
        }
    }
}

// os.set_inheritable(fd, False)
fn set_cloexec(fd: &OwnedFd) -> Result<(), String> {
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(python_os_error_text(&std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Whether `fd` is close-on-exec (tests: `os.get_inheritable` is false).
pub fn is_cloexec(fd: i32) -> bool {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags >= 0 && flags & libc::FD_CLOEXEC != 0
}
