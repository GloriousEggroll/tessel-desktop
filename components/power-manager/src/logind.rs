//! Own logind's lid and power-key handling while Power Manager runs
//! (power_logind.py).
//!
//! Holding "handle-lid-switch"/"handle-power-key" block inhibitors makes
//! logind leave those events to this session; releasing the descriptors
//! (including when the process exits) restores logind's own behavior. The
//! power key inhibitor is only held while the compositor has an XF86PowerOff
//! keybind that forwards presses to `--power-button`.

use suite_power::backend::{LOGIN, UPOWER};
use suite_core::bus::{Bus, BusError, Call, Signal, PROPERTIES};
use suite_core::values;
use gio::prelude::*;
use regex::Regex;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::LazyLock;

static POWER_KEY_BINDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<keybind\b[^>]*\bkey\s*=\s*["'][^"']*XF86PowerOff["']"#).unwrap()
});

pub fn keybind_path() -> PathBuf {
    suite_core::paths::config_home().join("labwc/rc.xml")
}

pub fn binds_power_key(text: &str) -> bool {
    POWER_KEY_BINDING.is_match(text)
}

pub type Connector = Box<dyn Fn(Box<dyn FnOnce(Result<Rc<dyn Bus>, BusError>)>)>;

pub struct Inner {
    on_lid_closed: Box<dyn Fn(bool)>,
    on_error: Box<dyn Fn(&str)>,
    rc_path: PathBuf,
    connector: Connector,
    pub lid_closed: Cell<Option<bool>>,
    pub lid_present: Cell<Option<bool>>,
    pub power_key_bound: Cell<bool>,
    inhibitors: RefCell<BTreeMap<String, OwnedFd>>,
    pending: RefCell<BTreeSet<String>>,
    bus: RefCell<Option<Rc<dyn Bus>>>,
    subscription: Cell<Option<u32>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    cancel: gio::Cancellable,
    closed: Cell<bool>,
}

#[derive(Clone)]
pub struct SessionSwitches(pub Rc<Inner>);

impl SessionSwitches {
    pub fn new(on_lid_closed: impl Fn(bool) + 'static, on_error: impl Fn(&str) + 'static, rc_path: Option<PathBuf>,
               connector: Option<Connector>) -> Self {
        Self(Rc::new(Inner {
            on_lid_closed: Box::new(on_lid_closed), on_error: Box::new(on_error),
            rc_path: rc_path.unwrap_or_else(keybind_path),
            connector: connector.unwrap_or_else(|| Box::new(|done| suite_core::bus::connect(gio::BusType::System, None, done))),
            lid_closed: Cell::new(None), lid_present: Cell::new(None), power_key_bound: Cell::new(false),
            inhibitors: RefCell::new(BTreeMap::new()), pending: RefCell::new(BTreeSet::new()), bus: RefCell::new(None),
            subscription: Cell::new(None), monitor: RefCell::new(None), cancel: gio::Cancellable::new(), closed: Cell::new(false),
        }))
    }

    pub fn inhibitors(&self) -> Vec<String> {
        self.0.inhibitors.borrow().keys().cloned().collect()
    }

    pub fn holds(&self, what: &str) -> bool {
        self.0.inhibitors.borrow().contains_key(what)
    }

    pub fn pending_requests(&self) -> usize {
        self.0.pending.borrow().len()
    }

    /// Tests: Python sets `_connection` directly.
    pub fn attach(&self, bus: Rc<dyn Bus>) {
        self.0.bus.replace(Some(bus));
    }

    pub fn start(&self) {
        let inner = &self.0;
        if inner.closed.get() || inner.bus.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(inner);
        (inner.connector)(Box::new(move |result| {
            let Some(this) = weak.upgrade() else { return };
            let this = SessionSwitches(this);
            match result {
                Err(error) => {
                    if !this.0.closed.get() {
                        (this.0.on_error)(&format!("Lid and power button settings are unavailable: {}", error.message));
                    }
                }
                Ok(bus) => this.connected(bus),
            }
        }));
        let file = gio::File::for_path(&inner.rc_path);
        if let Ok(monitor) = file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, Some(&inner.cancel)) {
            let weak = Rc::downgrade(inner);
            monitor.connect_changed(move |_, _, _, _| if let Some(this) = weak.upgrade() { SessionSwitches(this).read_keybinds() });
            inner.monitor.replace(Some(monitor));
        }
    }

    fn connected(&self, bus: Rc<dyn Bus>) {
        if self.0.closed.get() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let id = bus.subscribe(Some(LOGIN.0), Some(PROPERTIES), Some("PropertiesChanged"), Some(LOGIN.1), Some(LOGIN.2),
            Rc::new(move |signal: &Signal| if let Some(this) = weak.upgrade() { SessionSwitches(this).properties_changed(&signal.params) }));
        self.0.subscription.set(Some(id));
        self.0.bus.replace(Some(bus));
        let this = self.clone();
        self.get(LOGIN, "LidClosed", Box::new(move |value| this.lid_state(value.as_ref().and_then(Value::as_bool))));
        let this = self.clone();
        self.get(UPOWER, "LidIsPresent", Box::new(move |value| this.lid_presence(value.as_ref().and_then(Value::as_bool))));
        self.read_keybinds();
    }

    fn get(&self, endpoint: (&str, &str, &str), name: &str, callback: Box<dyn FnOnce(Option<Value>)>) {
        let Some(bus) = self.0.bus.borrow().clone() else { return };
        let weak = Rc::downgrade(&self.0);
        bus.call(Call::new(endpoint.0, endpoint.1, PROPERTIES, "Get", Some((endpoint.2, name).to_variant())), Box::new(move |reply| {
            let value = reply.ok().map(|v| values::first(&v)).filter(|v| !v.is_null());
            if weak.upgrade().is_some_and(|this| !this.closed.get()) {
                callback(value);
            }
        }));
    }

    pub fn lid_state(&self, value: Option<bool>) {
        if let (Some(value), None) = (value, self.0.lid_closed.get()) {
            self.0.lid_closed.set(Some(value));
        }
    }

    pub fn lid_presence(&self, value: Option<bool>) {
        // Unknown presence keeps the inhibitor: it is harmless without a lid.
        self.0.lid_present.set(value);
        self.hold("handle-lid-switch", value != Some(false), "Applying the desktop's lid close settings");
    }

    pub fn read_keybinds(&self) {
        if self.0.closed.get() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        gio::File::for_path(&self.0.rc_path).load_contents_async(Some(&self.0.cancel), move |result| {
            let text = result.map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
            let Some(this) = weak.upgrade() else { return };
            if this.closed.get() {
                return;
            }
            let this = SessionSwitches(this);
            this.set_power_key_bound(binds_power_key(&text));
        });
    }

    pub fn set_power_key_bound(&self, bound: bool) {
        self.0.power_key_bound.set(bound);
        self.hold("handle-power-key", bound, "Applying the desktop's power button settings");
    }

    pub fn hold(&self, what: &str, wanted: bool, why: &str) {
        let inner = &self.0;
        let Some(bus) = inner.bus.borrow().clone().filter(|_| !inner.closed.get()) else { return };
        if !wanted {
            inner.inhibitors.borrow_mut().remove(what);
            return;
        }
        if inner.inhibitors.borrow().contains_key(what) || inner.pending.borrow().contains(what) {
            return;
        }
        inner.pending.borrow_mut().insert(what.into());
        let weak = Rc::downgrade(inner);
        let (what, why) = (what.to_owned(), why.to_owned());
        let call = Call::new(LOGIN.0, LOGIN.1, LOGIN.2, "Inhibit", Some((what.as_str(), "Power Manager", why.as_str(), "block").to_variant()));
        bus.call_with_fd(call, Box::new(move |result| {
            let Some(this) = weak.upgrade() else { return };
            this.pending.borrow_mut().remove(&what);
            let fd = match result {
                Err(error) => {
                    if !this.closed.get() {
                        let subject = what.replace("handle-", "").replace('-', " ");
                        (this.on_error)(&format!("Could not take over {subject} handling: {}", error.message));
                    }
                    return;
                }
                Ok((_, Some(fd))) => fd,
                Ok((_, None)) => return,
            };
            if this.closed.get() || this.inhibitors.borrow().contains_key(&what) {
                return; // dropping `fd` releases it
            }
            this.inhibitors.borrow_mut().insert(what.clone(), fd);
            // The keybind may have been removed while the request was pending.
            if what == "handle-power-key" && !this.power_key_bound.get() {
                SessionSwitches(this).hold(&what, false, &why);
            }
        }));
    }

    pub fn properties_changed(&self, params: &glib::Variant) {
        let params = values::to_value(params);
        if params[0].as_str() != Some(LOGIN.2) {
            return;
        }
        let Some(closed) = params[1].get("LidClosed") else { return };
        let Some(closed) = closed.as_bool() else { return };
        let previous = self.0.lid_closed.replace(Some(closed));
        if closed && previous != Some(true) {
            // logind ignores the lid while docked or with external displays.
            let this = self.clone();
            self.get(LOGIN, "Docked", Box::new(move |docked| {
                if this.0.lid_closed.get() == Some(true) {
                    (this.0.on_lid_closed)(docked.as_ref().and_then(Value::as_bool) == Some(true));
                }
            }));
        }
    }

    pub fn close(&self) {
        let inner = &self.0;
        if inner.closed.get() {
            return;
        }
        inner.closed.set(true);
        inner.cancel.cancel();
        if let Some(monitor) = inner.monitor.take() {
            monitor.cancel();
        }
        if let (Some(bus), Some(id)) = (inner.bus.take(), inner.subscription.take()) {
            bus.unsubscribe(id);
        }
        inner.inhibitors.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use suite_core::bus::FakeBus;

    struct Fx {
        switches: SessionSwitches,
        bus: Rc<FakeBus>,
        closed: Rc<RefCell<Vec<bool>>>,
        write: OwnedFd,
    }

    fn fixture() -> Fx {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let c = closed.clone();
        let switches = SessionSwitches::new(move |d| c.borrow_mut().push(d), |_| {}, Some("/nonexistent".into()), Some(Box::new(|_| {})));
        let bus = FakeBus::new();
        switches.attach(bus.clone());
        let mut fds = [0; 2];
        // SAFETY: pipe() fills two new descriptors we own.
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        // SAFETY: freshly created descriptors.
        unsafe { libc::close(fds[0]) };
        let write = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fds[1]) };
        Fx { switches, bus, closed, write }
    }

    impl Fx {
        fn grant(&self, index: usize) {
            let callback = self.bus.fd_calls.borrow_mut()[index].1.take().unwrap();
            callback(Ok(((glib::variant::Handle(0),).to_variant(), Some(self.write.try_clone().unwrap()))));
        }
        fn inhibit_args(&self, index: usize) -> (String, String) {
            let params = self.bus.fd_calls.borrow()[index].0.params.clone().unwrap();
            (params.child_value(0).get().unwrap(), params.child_value(3).get().unwrap())
        }
    }

    #[test]
    fn lid_inhibitor_is_held_unless_upower_reports_no_lid() {
        let f = fixture();
        f.switches.lid_presence(Some(false));
        assert!(f.bus.fd_calls.borrow().is_empty());
        f.switches.lid_presence(None);
        assert_eq!(f.inhibit_args(0), ("handle-lid-switch".into(), "block".into()));
        f.switches.lid_presence(Some(true));
        assert_eq!(f.bus.fd_calls.borrow().len(), 1, "Pending requests are not duplicated");
        f.grant(0);
        assert!(f.switches.holds("handle-lid-switch"));
        f.switches.close();
        assert!(f.switches.inhibitors().is_empty());
    }

    #[test]
    fn power_key_follows_keybind_and_late_grant_after_unbind_is_released() {
        let f = fixture();
        f.switches.0.power_key_bound.set(true);
        f.switches.hold("handle-power-key", true, "test");
        f.switches.0.power_key_bound.set(false);
        f.switches.hold("handle-power-key", false, "test");
        f.grant(0);
        assert!(!f.switches.holds("handle-power-key"));
    }

    #[test]
    fn lid_close_edge_reports_docked_state() {
        let f = fixture();
        f.switches.lid_state(Some(false));
        let d = glib::VariantDict::new(None);
        d.insert_value("LidClosed", &true.to_variant());
        let changed = glib::Variant::tuple_from_iter([LOGIN.2.to_variant(), d.end(), Vec::<String>::new().to_variant()]);
        f.switches.properties_changed(&changed);
        let call = f.bus.last_pending();
        let params = call.params.clone().unwrap();
        assert_eq!((call.method.as_str(), params.child_value(1).str()), ("Get", Some("Docked")));
        f.bus.reply("Get", Ok((false.to_variant(),).to_variant()), None, None, 0);
        assert_eq!(*f.closed.borrow(), [false]);
        // Repeated "closed" notifications are not new lid closes.
        f.switches.properties_changed(&changed);
        assert_eq!(f.bus.calls.borrow().len(), 1);
    }

    #[test]
    fn power_key_inhibitor_requires_compositor_binding() {
        assert!(binds_power_key(r#"<keyboard><keybind key="XF86PowerOff"><action/></keybind>"#));
        assert!(binds_power_key("<keybind  onRelease='no' key='XF86PowerOff'>"));
        assert!(!binds_power_key(r#"<keybind key="XF86AudioMute">"#));
        assert!(!binds_power_key(""));
    }
}
