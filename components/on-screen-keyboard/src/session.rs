//! Pause desktop input while the secure lock service owns the session (osk_session.py).
use gio::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub const UNIT_PATH: &str = "/org/freedesktop/systemd1/unit/power_2dmanager_2dlock_2eservice";
pub const SYSTEMD: &str = "org.freedesktop.systemd1";
pub const UNIT_INTERFACE: &str = "org.freedesktop.systemd1.Unit";

/// Initial `ActiveState` that counts as locked.
pub fn locked_initially(state: &str) -> bool { matches!(state, "active" | "activating" | "deactivating") }

/// A failed locker can leave the compositor locked; keep the desktop keyboard
/// suspended until a later successful unlock/service stop.
pub fn locked_on_change(state: &str) -> bool { locked_initially(state) || state == "failed" }

struct Inner {
    changed: Rc<dyn Fn(bool)>,
    closed: bool,
    bus: Option<gio::DBusConnection>,
    subscription: Option<gio::SignalSubscriptionId>,
}

pub struct LockMonitor(Rc<RefCell<Inner>>);

fn unpack_state(value: &glib::Variant) -> Option<String> {
    let value = if value.type_().is_variant() { value.as_variant()? } else { value.clone() };
    value.str().map(str::to_owned)
}

impl LockMonitor {
    pub fn new(changed: impl Fn(bool) + 'static) -> Self {
        let inner = Rc::new(RefCell::new(Inner { changed: Rc::new(changed), closed: false, bus: None, subscription: None }));
        let weak = Rc::downgrade(&inner);
        gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |result| {
            let (Ok(bus), Some(inner)) = (result, weak.upgrade()) else { return };
            if inner.borrow().closed {
                return;
            }
            let signal_weak = Rc::downgrade(&inner);
            #[allow(deprecated)]
            let subscription = bus.signal_subscribe(Some(SYSTEMD), Some("org.freedesktop.DBus.Properties"), Some("PropertiesChanged"),
                Some(UNIT_PATH), None, gio::DBusSignalFlags::NONE, move |_, _, _, _, _, parameters| {
                    let Some(inner) = signal_weak.upgrade() else { return };
                    let Some((interface, changes, _)) = parameters.get::<(String, std::collections::HashMap<String, glib::Variant>, Vec<String>)>() else { return };
                    let (closed, changed) = { let i = inner.borrow(); (i.closed, i.changed.clone()) };
                    if closed || interface != UNIT_INTERFACE {
                        return;
                    }
                    if let Some(state) = changes.get("ActiveState").and_then(unpack_state) {
                        changed(locked_on_change(&state));
                    }
                });
            {
                let mut i = inner.borrow_mut();
                i.subscription = Some(subscription);
                i.bus = Some(bus.clone());
            }
            let weak = Rc::downgrade(&inner);
            bus.call(Some(SYSTEMD), UNIT_PATH, "org.freedesktop.DBus.Properties", "Get",
                     Some(&(UNIT_INTERFACE, "ActiveState").to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 1500,
                     None::<&gio::Cancellable>, move |result| {
                // Unit absent/inactive, including isolated tests: nothing happens.
                let (Ok(reply), Some(inner)) = (result, weak.upgrade()) else { return };
                let Some(state) = reply.try_child_value(0).as_ref().and_then(unpack_state) else { return };
                let (closed, changed) = { let i = inner.borrow(); (i.closed, i.changed.clone()) };
                if !closed {
                    changed(locked_initially(&state));
                }
            });
        });
        Self(inner)
    }

    pub fn close(&self) {
        let mut inner = self.0.borrow_mut();
        inner.closed = true;
        if let (Some(bus), Some(id)) = (inner.bus.clone(), inner.subscription.take()) {
            #[allow(deprecated)]
            bus.signal_unsubscribe(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_states() {
        for state in ["active", "activating", "deactivating"] {
            assert!(locked_initially(state) && locked_on_change(state));
        }
        assert!(!locked_initially("failed"));
        assert!(locked_on_change("failed"));
        assert!(!locked_on_change("inactive"));
    }
}
