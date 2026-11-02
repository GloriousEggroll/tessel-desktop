//! Local helpers that tessel-core/tessel-gtk lack (candidates to upstream).

use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::os::fd::OwnedFd;
use std::rc::Rc;
use tessel_core::bus::{Bus, BusError, Call, Reply, Signal, SignalHandler};

/// libnm's authorization timeout (`AUTH_TIMEOUT = 2**31 - 1` ms).
pub const AUTH_TIMEOUT: i32 = i32::MAX;

/// A [`Bus`] like `tessel_core::bus::GioBus`, but reads never auto-start a
/// service (`NO_AUTO_START`, as libnm) and interactive mutations wait for
/// polkit indefinitely (`AUTH_TIMEOUT`) instead of 120 s.
/// Upstream candidate: a timeout / flags field on `tessel_core::bus::Call`.
pub struct NmBus {
    connection: gio::DBusConnection,
    cancel: RefCell<gio::Cancellable>,
    subscriptions: RefCell<Vec<(u32, gio::SignalSubscriptionId)>>,
    next: Cell<u32>,
    closed: RefCell<Option<glib::SignalHandlerId>>,
}

impl NmBus {
    pub fn new(connection: gio::DBusConnection) -> Rc<Self> {
        connection.set_exit_on_close(false);
        Rc::new(Self { connection, cancel: RefCell::new(gio::Cancellable::new()), subscriptions: RefCell::new(Vec::new()),
                       next: Cell::new(0), closed: RefCell::new(None) })
    }

    pub fn connection(&self) -> &gio::DBusConnection {
        &self.connection
    }
}

/// `Gio.bus_get` → [`NmBus`] (honours `DBUS_SYSTEM_BUS_ADDRESS`).
pub fn connect(kind: gio::BusType, done: impl FnOnce(Result<(Rc<dyn Bus>, gio::DBusConnection), BusError>) + 'static) {
    gio::bus_get(kind, None::<&gio::Cancellable>, move |result| match result {
        Ok(connection) => done(Ok((NmBus::new(connection.clone()) as Rc<dyn Bus>, connection))),
        Err(error) => done(Err(BusError::from_glib(&error))),
    });
}

#[allow(deprecated)]
impl Bus for NmBus {
    fn call(&self, call: Call, done: Box<dyn FnOnce(Reply)>) {
        let (flags, timeout, cancel) = if call.mutation {
            let flags = if call.interactive { gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION } else { gio::DBusCallFlags::NONE };
            (flags, AUTH_TIMEOUT, None)
        } else {
            (gio::DBusCallFlags::NO_AUTO_START, 25000, Some(self.cancel.borrow().clone()))
        };
        self.connection.call(Some(&call.name), &call.path, &call.interface, &call.method, call.params.as_ref(), None,
                             flags, timeout, cancel.as_ref(), move |result| done(result.map_err(|e| BusError::from_glib(&e))));
    }

    fn call_with_fd(&self, _call: Call, done: Box<dyn FnOnce(Result<(Variant, Option<OwnedFd>), BusError>)>) {
        done(Err(BusError::local("File descriptors are not used by the network service.")));
    }

    fn subscribe(&self, sender: Option<&str>, interface: Option<&str>, member: Option<&str>, path: Option<&str>,
                 arg0: Option<&str>, handler: SignalHandler) -> u32 {
        let id = self.connection.signal_subscribe(sender, interface, member, path, arg0, gio::DBusSignalFlags::NONE,
            move |_, sender, path, interface, member, params| {
                handler(&Signal { sender: sender.to_owned(), path: path.to_owned(), interface: interface.to_owned(),
                                  member: member.to_owned(), params: params.clone() });
            });
        let key = self.next.get() + 1;
        self.next.set(key);
        self.subscriptions.borrow_mut().push((key, id));
        key
    }

    fn unsubscribe(&self, key: u32) {
        let mut subscriptions = self.subscriptions.borrow_mut();
        if let Some(index) = subscriptions.iter().position(|entry| entry.0 == key) {
            let (_, id) = subscriptions.remove(index);
            self.connection.signal_unsubscribe(id);
        }
    }

    fn cancel_reads(&self) {
        let old = self.cancel.replace(gio::Cancellable::new());
        old.cancel();
    }

    fn on_closed(&self, handler: Option<Box<dyn Fn()>>) {
        if let Some(id) = self.closed.take() {
            self.connection.disconnect(id);
        }
        if let Some(handler) = handler {
            let id = self.connection.connect_local("closed", false, move |_| {
                handler();
                None
            });
            self.closed.replace(Some(id));
        }
    }
}

/// Python `float.__format__("g")` for link speeds (`433.0` → `433`, `487.5`).
pub fn format_g(value: f64) -> String {
    tessel_core::pyfmt::format_value(&tessel_core::pyfmt::Arg::Float(value), "g").unwrap_or_else(|_| value.to_string())
}
