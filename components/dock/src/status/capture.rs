//! capture_status.py: event-driven recording indicator shared by all dock
//! outputs. Follows `org.tessel.Capture` (`/org/tessel/Capture/Status`,
//! `org.tessel.Capture.Status`): `GetState() -> (sb)` once per owner and the
//! `Changed(s)` signal; reports `"idle"` whenever the owner vanishes.
//!
//! API: `CaptureStatus::new(changed: Fn(&str))` watches the session bus,
//! `CaptureStatus::on_connection(&conn, changed)` a given connection; `.close()`.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const NAME: &str = "org.tessel.Capture";
pub const PATH: &str = "/org/tessel/Capture/Status";
pub const INTERFACE: &str = "org.tessel.Capture.Status";

struct Inner {
    changed: Box<dyn Fn(&str)>,
    signal: RefCell<Option<gio::SignalSubscription>>,
    closed: Cell<bool>,
    serial: Cell<u64>,
    /// `Gio.bus_unwatch_name` for the current watch (gio's watcher id type is not nameable).
    watch: RefCell<Option<Box<dyn FnOnce()>>>,
}

#[derive(Clone)]
pub struct CaptureStatus(Rc<Inner>);

impl CaptureStatus {
    fn create(changed: impl Fn(&str) + 'static) -> Self {
        Self(Rc::new(Inner {
            changed: Box::new(changed), signal: RefCell::new(None), closed: Cell::new(false),
            serial: Cell::new(0), watch: RefCell::new(None),
        }))
    }

    /// `Gio.bus_watch_name(SESSION, ...)`.
    pub fn new(changed: impl Fn(&str) + 'static) -> Self {
        let status = Self::create(changed);
        let (appeared, vanished) = (Rc::downgrade(&status.0), Rc::downgrade(&status.0));
        let watch = gio::bus_watch_name(gio::BusType::Session, NAME, gio::BusNameWatcherFlags::NONE,
            move |bus, _name, owner| { if let Some(inner) = appeared.upgrade() { CaptureStatus(inner).appeared(bus, owner) } },
            move |_bus, _name| { if let Some(inner) = vanished.upgrade() { CaptureStatus(inner).vanished() } });
        status.0.watch.replace(Some(Box::new(move || gio::bus_unwatch_name(watch))));
        status
    }

    /// Same, on an explicit connection (tests / a shared app connection).
    pub fn on_connection(connection: &gio::DBusConnection, changed: impl Fn(&str) + 'static) -> Self {
        let status = Self::create(changed);
        let (appeared, vanished) = (Rc::downgrade(&status.0), Rc::downgrade(&status.0));
        let watch = gio::bus_watch_name_on_connection(connection, NAME, gio::BusNameWatcherFlags::NONE,
            move |bus, _name, owner| { if let Some(inner) = appeared.upgrade() { CaptureStatus(inner).appeared(bus, owner) } },
            move |_bus, _name| { if let Some(inner) = vanished.upgrade() { CaptureStatus(inner).vanished() } });
        status.0.watch.replace(Some(Box::new(move || gio::bus_unwatch_name(watch))));
        status
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    fn appeared(&self, bus: gio::DBusConnection, owner: &str) {
        self.unsubscribe();
        self.0.serial.set(self.0.serial.get() + 1);
        let serial = self.0.serial.get();
        let weak = self.weak();
        let subscription = bus.subscribe_to_signal(Some(owner), Some(INTERFACE), Some("Changed"), Some(PATH), None,
            gio::DBusSignalFlags::NONE, move |signal| {
                let Some(inner) = weak.upgrade() else { return };
                inner.serial.set(inner.serial.get() + 1);
                if let Some(state) = signal.parameters.try_child_value(0).and_then(|v| v.get::<String>()) {
                    (inner.changed)(&state);
                }
            });
        self.0.signal.replace(Some(subscription));
        let weak = self.weak();
        bus.call(Some(owner), PATH, INTERFACE, "GetState", None, Some(glib::VariantTy::new("(sb)").unwrap()),
                 gio::DBusCallFlags::NO_AUTO_START, 2000, None::<&gio::Cancellable>, move |result| {
            let Ok(reply) = result else { return };
            let Some(inner) = weak.upgrade() else { return };
            if let Some((state, _armed)) = reply.get::<(String, bool)>() {
                if !inner.closed.get() && inner.serial.get() == serial {
                    (inner.changed)(&state);
                }
            }
        });
    }

    fn vanished(&self) {
        self.0.serial.set(self.0.serial.get() + 1);
        self.unsubscribe();
        if !self.0.closed.get() {
            (self.0.changed)("idle");
        }
    }

    fn unsubscribe(&self) {
        // Dropping the subscription unsubscribes it.
        drop(self.0.signal.take());
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        if let Some(unwatch) = self.0.watch.take() {
            unwatch();
        }
        self.unsubscribe();
    }
}
