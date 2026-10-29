//! The system-bus calls session-manager makes, behind a trait so tests use a
//! fake (Python tests replaced `bus` with a `Mock`). Each call carries its own
//! flags and timeout, exactly as session_backend.py / session_end.py pass them
//! (3000 ms NO_AUTO_START reads, 5000 ms greeter switch, 120000 ms power calls).

use glib::Variant;
use std::cell::RefCell;
use std::rc::Rc;

pub const LOGIN1: &str = "org.freedesktop.login1";
pub const LOGIN1_PATH: &str = "/org/freedesktop/login1";
pub const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
pub const DISPLAY_MANAGER: &str = "org.freedesktop.DisplayManager";
pub const DISPLAY_MANAGER_PATH: &str = "/org/freedesktop/DisplayManager";
pub const SEAT_INTERFACE: &str = "org.freedesktop.DisplayManager.Seat";

#[derive(Clone, Debug)]
pub struct BusCall {
    pub name: String,
    pub path: String,
    pub interface: String,
    pub method: String,
    pub params: Option<Variant>,
    pub flags: gio::DBusCallFlags,
    pub timeout: i32,
    pub cancellable: Option<gio::Cancellable>,
}

impl BusCall {
    pub fn new(name: &str, path: &str, interface: &str, method: &str, params: Option<Variant>,
               flags: gio::DBusCallFlags, timeout: i32, cancellable: Option<&gio::Cancellable>) -> Self {
        Self { name: name.into(), path: path.into(), interface: interface.into(), method: method.into(), params,
               flags, timeout, cancellable: cancellable.cloned() }
    }
}

/// `Err` holds `GLib.Error.message` (remote errors keep their `GDBus.Error:` prefix).
pub type Reply = Result<Variant, String>;
pub type ReplyFn = Box<dyn FnOnce(Reply)>;

pub trait SystemBus {
    fn call(&self, call: BusCall, done: ReplyFn);
}

pub struct GioSystemBus(pub gio::DBusConnection);

impl GioSystemBus {
    pub fn new(connection: gio::DBusConnection) -> Rc<Self> { Rc::new(Self(connection)) }

    /// `Gio.bus_get_sync(SYSTEM)` (honours `DBUS_SYSTEM_BUS_ADDRESS`).
    pub fn system_sync() -> Option<Rc<dyn SystemBus>> {
        gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>).ok().map(|c| Self::new(c) as Rc<dyn SystemBus>)
    }
}

impl SystemBus for GioSystemBus {
    fn call(&self, call: BusCall, done: ReplyFn) {
        self.0.call(Some(&call.name), &call.path, &call.interface, &call.method, call.params.as_ref(), None,
                    call.flags, call.timeout, call.cancellable.as_ref(),
                    move |result| done(result.map_err(|error| error.message().to_owned())));
    }
}

/// Records calls; replies are delivered explicitly with [`FakeSystemBus::reply`].
#[derive(Default)]
pub struct FakeSystemBus {
    pub calls: RefCell<Vec<BusCall>>,
    pub pending: RefCell<Vec<(BusCall, Option<ReplyFn>)>>,
}

impl FakeSystemBus {
    pub fn new() -> Rc<Self> { Rc::new(Self::default()) }

    pub fn methods(&self) -> Vec<String> { self.calls.borrow().iter().map(|c| c.method.clone()).collect() }

    pub fn called(&self, method: &str) -> bool { self.calls.borrow().iter().any(|c| c.method == method) }

    pub fn last(&self) -> Option<BusCall> { self.calls.borrow().last().cloned() }

    /// Answer the oldest unanswered call to `method`; returns false if none.
    pub fn reply(&self, method: &str, reply: Reply) -> bool {
        let done = {
            let mut pending = self.pending.borrow_mut();
            let Some(entry) = pending.iter_mut().find(|(call, done)| call.method == method && done.is_some()) else {
                return false;
            };
            entry.1.take()
        };
        if let Some(done) = done {
            done(reply);
        }
        true
    }
}

impl SystemBus for FakeSystemBus {
    fn call(&self, call: BusCall, done: ReplyFn) {
        self.calls.borrow_mut().push(call.clone());
        self.pending.borrow_mut().push((call, Some(done)));
    }
}

impl SystemBus for Rc<FakeSystemBus> {
    fn call(&self, call: BusCall, done: ReplyFn) { (**self).call(call, done) }
}
