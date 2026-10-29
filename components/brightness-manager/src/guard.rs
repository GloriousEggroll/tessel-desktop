//! The read-only logind inhibitor/session check used before automatic dimming
//! (the subset of power-manager's `PowerBackend.idle_inhibited` that
//! brightness-manager used; no UPower/PPD subscriptions).

use suite_core::bus::{self, Bus, BusError, Call};
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const LOGIN: &str = "org.freedesktop.login1";
pub const LOGIN_PATH: &str = "/org/freedesktop/login1";
pub const LOGIN_MANAGER: &str = "org.freedesktop.login1.Manager";
pub const LOGIN_SESSION: &str = "org.freedesktop.login1.Session";
pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// What the application needs (fakeable): "unknown means blocked".
pub trait IdleGuard {
    fn start(&self);
    fn close(&self);
    fn idle_inhibited(&self, callback: Box<dyn FnOnce(bool)>);
}

/// Block weak inhibitors too; automatic actions must not bypass user intent.
pub fn inhibited(inhibitors: &Variant, subject: &str) -> Result<bool, String> {
    let invalid = || "Invalid power inhibition response.".to_owned();
    if inhibitors.type_().as_str() != "a(ssssuu)" {
        return Err(invalid());
    }
    for row in inhibitors.iter() {
        let what = row.child_value(0).str().map(String::from).ok_or_else(invalid)?;
        let mode = row.child_value(3).str().map(String::from).ok_or_else(invalid)?;
        if (mode == "block" || mode == "block-weak") && what.split(':').any(|w| w == subject) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub type Connector = Box<dyn Fn(Box<dyn FnOnce(Result<Rc<dyn Bus>, BusError>)>)>;

pub struct Inner {
    connector: Connector,
    session_id: Box<dyn Fn() -> String>,
    bus: RefCell<Option<Rc<dyn Bus>>>,
    started: Cell<bool>,
    closed: Cell<bool>,
}

#[derive(Clone)]
pub struct LogindGuard(pub Rc<Inner>);

impl LogindGuard {
    pub fn system() -> Self {
        Self::new(Box::new(|done| bus::connect(gio::BusType::System, None, done)),
                  Box::new(|| std::env::var("XDG_SESSION_ID").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "auto".into())))
    }

    pub fn new(connector: Connector, session_id: Box<dyn Fn() -> String>) -> Self {
        Self(Rc::new(Inner { connector, session_id, bus: RefCell::new(None), started: Cell::new(false), closed: Cell::new(false) }))
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub fn ready(&self) -> bool {
        self.0.bus.borrow().is_some() && !self.0.closed.get()
    }

    fn call(&self, path: &str, interface: &str, method: &str, params: Option<Variant>, done: Box<dyn FnOnce(Result<Variant, BusError>)>) {
        let bus = self.0.bus.borrow().clone();
        match bus {
            Some(bus) => bus.call(Call::new(LOGIN, path, interface, method, params), done),
            None => done(Err(BusError::local("The login manager is unavailable."))),
        }
    }

    /// Only the foreground local desktop may issue automatic actions.
    fn session_active(&self, callback: Box<dyn FnOnce(bool)>) {
        let weak = self.weak();
        let session = (self.0.session_id)();
        self.call(LOGIN_PATH, LOGIN_MANAGER, "GetSession", Some((session,).into()), Box::new(move |reply| {
            let path = reply.ok().and_then(|v| v.try_child_value(0)).filter(|v| v.type_().as_str() == "o")
                .and_then(|v| v.str().map(String::from));
            let (Some(path), Some(inner)) = (path, weak.upgrade()) else {
                callback(false);
                return;
            };
            LogindGuard(inner).call(&path, PROPERTIES, "GetAll", Some((LOGIN_SESSION,).into()), Box::new(move |reply| {
                let Some(state) = reply.ok().and_then(|v| v.try_child_value(0)).filter(|v| v.type_().as_str() == "a{sv}") else {
                    callback(false);
                    return;
                };
                let dict = glib::VariantDict::new(Some(&state));
                let active = dict.lookup::<bool>("Active").ok().flatten() == Some(true);
                let remote = dict.lookup::<bool>("Remote").ok().flatten() == Some(false);
                let kind = dict.lookup::<String>("Type").ok().flatten();
                callback(active && remote && matches!(kind.as_deref(), Some("wayland" | "x11")));
            }));
        }));
    }
}

impl IdleGuard for LogindGuard {
    fn start(&self) {
        if self.0.started.replace(true) || self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        (self.0.connector)(Box::new(move |result| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.closed.get() {
                return;
            }
            if let Ok(bus) = result {
                inner.bus.replace(Some(bus));
            }
        }));
    }

    fn close(&self) {
        self.0.closed.set(true);
        if let Some(bus) = self.0.bus.take() {
            bus.cancel_reads();
        }
    }

    fn idle_inhibited(&self, callback: Box<dyn FnOnce(bool)>) {
        if !self.ready() {
            callback(true);
            return;
        }
        let weak = self.weak();
        self.call(LOGIN_PATH, LOGIN_MANAGER, "ListInhibitors", None, Box::new(move |reply| {
            let blocked = match reply {
                Err(_) => true,
                Ok(value) => value.try_child_value(0).map(|rows| inhibited(&rows, "idle").unwrap_or(true)).unwrap_or(true),
            };
            match weak.upgrade() {
                Some(inner) if !blocked => LogindGuard(inner).session_active(Box::new(move |active| callback(!active))),
                _ => callback(true),
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use suite_core::bus::FakeBus;
    use glib::prelude::*;

    fn guard(fake: &Rc<FakeBus>) -> LogindGuard {
        let bus: Rc<dyn Bus> = fake.clone();
        let guard = LogindGuard::new(Box::new(move |done| done(Ok(bus.clone()))), Box::new(|| "auto".into()));
        guard.start();
        guard
    }

    fn rows(items: &[(&str, &str)]) -> Variant {
        let rows: Vec<(String, String, String, String, u32, u32)> =
            items.iter().map(|(w, m)| (w.to_string(), "who".into(), "why".into(), m.to_string(), 1000, 1)).collect();
        (rows,).to_variant()
    }

    fn session(active: bool, remote: bool, kind: &str) -> Variant {
        let dict = glib::VariantDict::new(None);
        dict.insert("Active", active);
        dict.insert("Remote", remote);
        dict.insert("Type", kind);
        Variant::tuple_from_iter([dict.end()])
    }

    #[test]
    fn inhibitors_and_session_state_gate_dimming() {
        let fake = FakeBus::new();
        let guard = guard(&fake);
        let result: Rc<RefCell<Vec<bool>>> = Rc::default();
        let r = result.clone();
        guard.idle_inhibited(Box::new(move |b| r.borrow_mut().push(b)));
        fake.reply("ListInhibitors", Ok(rows(&[("sleep:shutdown", "block"), ("idle", "delay")])), None, None, 0);
        let call = fake.reply("GetSession", Ok((glib::variant::ObjectPath::try_from("/s/1").unwrap(),).to_variant()), None, None, 0);
        assert_eq!(call.params.unwrap().child_value(0).str(), Some("auto"));
        let call = fake.reply("GetAll", Ok(session(true, false, "wayland")), None, None, 0);
        assert_eq!((call.path.as_str(), call.interface.as_str()), ("/s/1", PROPERTIES));
        assert_eq!(*result.borrow(), vec![false]);

        for (reply, expect) in [(rows(&[("handle-lid-switch:idle", "block-weak")]), true), (rows(&[("idle", "block")]), true)] {
            let r = result.clone();
            guard.idle_inhibited(Box::new(move |b| r.borrow_mut().push(b)));
            fake.reply("ListInhibitors", Ok(reply), None, None, 0);
            assert_eq!(result.borrow().last(), Some(&expect));
        }
        let r = result.clone();
        guard.idle_inhibited(Box::new(move |b| r.borrow_mut().push(b)));
        fake.reply("ListInhibitors", Ok(rows(&[])), None, None, 0);
        fake.reply("GetSession", Ok((glib::variant::ObjectPath::try_from("/s/1").unwrap(),).to_variant()), None, None, 0);
        fake.reply("GetAll", Ok(session(true, true, "wayland")), None, None, 0);
        assert_eq!(result.borrow().last(), Some(&true), "remote sessions never auto-dim");
        let r = result.clone();
        guard.idle_inhibited(Box::new(move |b| r.borrow_mut().push(b)));
        fake.reply("ListInhibitors", Err(BusError::local("nope")), None, None, 0);
        assert_eq!(result.borrow().last(), Some(&true), "unknown means blocked");
        guard.close();
        let r = result.clone();
        guard.idle_inhibited(Box::new(move |b| r.borrow_mut().push(b)));
        assert_eq!(result.borrow().last(), Some(&true));
        assert_eq!(fake.cancelled.get(), 1);
    }
}
