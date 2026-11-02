//! Low and critical battery notifications and the configurable critical
//! action (power_alerts.py). Only the resident Power Manager creates this
//! controller, so each warning is sent exactly once per discharge cycle.

use tessel_power_core::backend::{Capabilities, PowerSystem};
use tessel_core::scheduler::Scheduler;
use gio::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::rc::{Rc, Weak};
use tessel_core::tr;

pub const LOW_PERCENT: f64 = 10.0;
pub const CRITICAL_PERCENT: f64 = 5.0;
pub const WARNING_LOW: i64 = 3;
pub const WARNING_CRITICAL: i64 = 4;
pub const WARNING_ACTION: i64 = 5;
pub const COUNTDOWN: u32 = 60;
pub const NOTIFICATIONS: (&str, &str, &str) =
    ("org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications");

pub type Sent = Box<dyn FnOnce(u32)>;

pub trait Notifier {
    #[allow(clippy::too_many_arguments)]
    fn notify(&self, summary: &str, body: &str, urgency: u8, icon: &str, actions: &[(String, String)], replaces: u32,
              callback: Option<Sent>);
    fn close_notification(&self, id: u32);
    fn close(&self);
}

struct Queued {
    summary: String,
    body: String,
    urgency: u8,
    icon: String,
    actions: Vec<(String, String)>,
    replaces: u32,
    callback: Option<Sent>,
}

/// Asynchronous freedesktop notifications; failures are never fatal.
pub struct DBusNotifier {
    connection: RefCell<Option<gio::DBusConnection>>,
    subscription: RefCell<Option<gio::SignalSubscriptionId>>,
    cancel: gio::Cancellable,
    queue: RefCell<Vec<Queued>>,
    closed: Cell<bool>,
}

impl DBusNotifier {
    pub fn new(on_action: impl Fn(u32, &str) + 'static) -> Rc<Self> {
        let notifier = Rc::new(Self { connection: RefCell::new(None), subscription: RefCell::new(None),
                                      cancel: gio::Cancellable::new(), queue: RefCell::new(Vec::new()), closed: Cell::new(false) });
        let weak = Rc::downgrade(&notifier);
        let on_action = Rc::new(on_action);
        gio::bus_get(gio::BusType::Session, Some(&notifier.cancel), move |result| {
            let Ok(connection) = result else { return };
            let Some(this) = weak.upgrade() else { return };
            if this.closed.get() {
                return;
            }
            let action = on_action.clone();
            #[allow(deprecated)]
            let id = connection.signal_subscribe(Some(NOTIFICATIONS.0), Some(NOTIFICATIONS.2), Some("ActionInvoked"),
                Some(NOTIFICATIONS.1), None, gio::DBusSignalFlags::NONE, move |_, _, _, _, _, params| {
                    if let Some((id, key)) = params.get::<(u32, String)>() {
                        action(id, &key);
                    }
                });
            this.subscription.replace(Some(id));
            this.connection.replace(Some(connection));
            let queue = std::mem::take(&mut *this.queue.borrow_mut());
            for q in queue {
                this.notify(&q.summary, &q.body, q.urgency, &q.icon, &q.actions, q.replaces, q.callback);
            }
        });
        notifier
    }
}

/// The `Notify(susssasa{sv}i)` parameters (pure, for tests).
pub fn notify_parameters(summary: &str, body: &str, urgency: u8, icon: &str, actions: &[(String, String)], replaces: u32) -> glib::Variant {
    let flat: Vec<String> = actions.iter().flat_map(|(k, l)| [k.clone(), l.clone()]).collect();
    let hints = glib::VariantDict::new(None);
    hints.insert_value("urgency", &urgency.to_variant());
    hints.insert_value("desktop-entry", &"org.tessel.PowerManager".to_variant());
    hints.insert_value("category", &"device".to_variant());
    if urgency == 2 {
        hints.insert_value("resident", &true.to_variant());
    }
    glib::Variant::tuple_from_iter([tr!("Power").to_variant(), replaces.to_variant(), icon.to_variant(), summary.to_variant(),
                                    body.to_variant(), flat.to_variant(), hints.end(),
                                    (if urgency == 2 { 0i32 } else { -1i32 }).to_variant()])
}

impl Notifier for DBusNotifier {
    fn notify(&self, summary: &str, body: &str, urgency: u8, icon: &str, actions: &[(String, String)], replaces: u32,
              callback: Option<Sent>) {
        if self.closed.get() {
            return;
        }
        let Some(connection) = self.connection.borrow().clone() else {
            self.queue.borrow_mut().push(Queued { summary: summary.into(), body: body.into(), urgency, icon: icon.into(),
                                                  actions: actions.to_vec(), replaces, callback });
            return;
        };
        let params = notify_parameters(summary, body, urgency, icon, actions, replaces);
        connection.call(Some(NOTIFICATIONS.0), NOTIFICATIONS.1, NOTIFICATIONS.2, "Notify", Some(&params),
            Some(glib::VariantTy::new("(u)").unwrap()), gio::DBusCallFlags::NONE, 10000, Some(&self.cancel),
            move |result| {
                let id = result.ok().and_then(|v| v.get::<(u32,)>()).map(|v| v.0).unwrap_or(0);
                if let Some(callback) = callback {
                    callback(id);
                }
            });
    }

    fn close_notification(&self, id: u32) {
        let Some(connection) = self.connection.borrow().clone() else { return };
        if id == 0 || self.closed.get() {
            return;
        }
        connection.call(Some(NOTIFICATIONS.0), NOTIFICATIONS.1, NOTIFICATIONS.2, "CloseNotification", Some(&(id,).to_variant()),
                        None, gio::DBusCallFlags::NONE, 10000, Some(&self.cancel), |_| {});
    }

    fn close(&self) {
        self.closed.set(true);
        self.cancel.cancel();
        if let (Some(connection), Some(id)) = (self.connection.take(), self.subscription.take()) {
            #[allow(deprecated)]
            connection.signal_unsubscribe(id);
        }
    }
}

/// 'auto' hibernates when that is possible without a prompt.
pub fn resolve_action(action: &str, capabilities: &Capabilities) -> String {
    if action == "auto" {
        return if capabilities.get("hibernate").map(String::as_str) == Some("yes") { "hibernate" } else { "poweroff" }.into();
    }
    action.into()
}

pub struct Inner {
    backend: Rc<dyn PowerSystem>,
    perform: Box<dyn Fn(&str)>,
    notifier: RefCell<Option<Rc<dyn Notifier>>>,
    scheduler: Rc<dyn Scheduler>,
    settings: RefCell<(String, i64)>,
    sent: RefCell<HashSet<&'static str>>,
    ids: RefCell<BTreeMap<&'static str, u32>>,
    timer: Cell<Option<u64>>,
    closed: Cell<bool>,
}

#[derive(Clone)]
pub struct BatteryAlerts(pub Rc<Inner>);

impl BatteryAlerts {
    /// `notifier: None` creates the session-bus notifier.
    pub fn new(backend: Rc<dyn PowerSystem>, perform: impl Fn(&str) + 'static, notifier: Option<Rc<dyn Notifier>>,
               scheduler: Rc<dyn Scheduler>) -> Self {
        let inner = Rc::new(Inner { backend, perform: Box::new(perform), notifier: RefCell::new(notifier), scheduler,
                                    settings: RefCell::new(("auto".into(), 0)), sent: RefCell::new(HashSet::new()),
                                    ids: RefCell::new(BTreeMap::new()), timer: Cell::new(None), closed: Cell::new(false) });
        if inner.notifier.borrow().is_none() {
            let weak: Weak<Inner> = Rc::downgrade(&inner);
            let notifier = DBusNotifier::new(move |id, key| if let Some(this) = weak.upgrade() { BatteryAlerts(this).action_invoked(id, key) });
            inner.notifier.replace(Some(notifier));
        }
        Self(inner)
    }

    fn notifier(&self) -> Rc<dyn Notifier> {
        self.0.notifier.borrow().clone().expect("notifier")
    }

    pub fn counting_down(&self) -> bool {
        self.0.timer.get().is_some()
    }

    pub fn update(&self, settings: &Value) {
        self.0.settings.replace((settings["critical_action"].as_str().unwrap_or("auto").to_owned(),
                                 settings["critical_level"].as_i64().unwrap_or(0)));
        self.refresh();
    }

    fn discharging(&self) -> bool {
        let s = self.0.backend.state();
        s.power_ready && s.power_source_known && s.has_battery && s.on_battery
    }

    pub fn refresh(&self) {
        if self.0.closed.get() {
            return;
        }
        if !self.discharging() {
            // Charging starts a new discharge cycle and cancels any countdown.
            if self.0.backend.state().power_source_known {
                self.reset();
            }
            return;
        }
        let (warning, percent) = {
            let s = self.0.backend.state();
            (s.warning_level, s.percentage.filter(|p| p.is_finite()))
        };
        let below = |limit: f64| percent.is_some_and(|p| p <= limit);
        let level = self.0.settings.borrow().1;
        let due = if level == 0 { warning >= WARNING_ACTION } else { below(level as f64) };
        let low = warning >= WARNING_LOW || below(LOW_PERCENT);
        let critical = warning >= WARNING_CRITICAL || below(CRITICAL_PERCENT);
        let shown = percent.map(|p| tessel_core::pyfmt::format_value(&tessel_core::Arg::from(p), ".0f").unwrap_or_default() + "%")
            .unwrap_or_default();
        let sent = |key: &str| self.0.sent.borrow().contains(key);
        if critical && !sent("critical") {
            self.0.sent.borrow_mut().extend(["low", "critical"]);
            let body = if shown.is_empty() { tr!("Plug in now to avoid losing work.") }
                       else { tr!("{percent} remaining. Plug in now to avoid losing work.", percent = shown.as_str()) };
            self.send("critical", &tr!("Battery critically low"), &body, 2, "battery-caution-symbolic", &[]);
        } else if low && !sent("low") {
            self.0.sent.borrow_mut().insert("low");
            let body = if shown.is_empty() { tr!("Consider plugging in.") }
                       else { tr!("{percent} remaining. Consider plugging in.", percent = shown.as_str()) };
            self.send("low", &tr!("Battery low"), &body, 1, "battery-low-symbolic", &[]);
        }
        if due && !sent("action") {
            self.0.sent.borrow_mut().insert("action");
            let action = self.resolved();
            if action != "nothing" {
                self.start_countdown(&action);
            }
        }
    }

    fn resolved(&self) -> String {
        let action = self.0.settings.borrow().0.clone();
        resolve_action(&action, &self.0.backend.state().capabilities)
    }

    fn send(&self, key: &'static str, summary: &str, body: &str, urgency: u8, icon: &str, actions: &[(String, String)]) {
        let weak = Rc::downgrade(&self.0);
        let replaces = self.0.ids.borrow().get(key).copied().unwrap_or(0);
        self.notifier().notify(summary, body, urgency, icon, actions, replaces, Some(Box::new(move |id| {
            if let Some(this) = weak.upgrade() {
                this.ids.borrow_mut().insert(key, id);
            }
        })));
    }

    fn start_countdown(&self, action: &str) {
        let body = match action {
            "hibernate" => tr!("The computer will hibernate in {seconds} seconds unless it is plugged in.", seconds = COUNTDOWN as i64),
            "poweroff" => tr!("The computer will power off in {seconds} seconds unless it is plugged in.", seconds = COUNTDOWN as i64),
            "suspend" => tr!("The computer will sleep in {seconds} seconds unless it is plugged in.", seconds = COUNTDOWN as i64),
            // Python raises KeyError for anything else; never act on it.
            _ => return,
        };
        self.send("action", &tr!("Battery almost empty"), &body, 2, "battery-empty-symbolic", &[("cancel".into(), tr!("Cancel"))]);
        let weak = Rc::downgrade(&self.0);
        let id = self.0.scheduler.timeout(COUNTDOWN * 1000, Box::new(move || {
            if let Some(this) = weak.upgrade() {
                BatteryAlerts(this).expired();
            }
        }));
        self.0.timer.set(Some(id));
    }

    fn expired(&self) {
        self.0.timer.set(None);
        self.close_key("action");
        if !self.0.closed.get() && self.discharging() {
            let action = self.resolved();
            (self.0.perform)(&action);
        }
    }

    pub fn action_invoked(&self, id: u32, key: &str) {
        if key == "cancel" && id != 0 && Some(id) == self.0.ids.borrow().get("action").copied() {
            self.cancel();
        }
    }

    /// The user keeps working; no further action this discharge cycle.
    pub fn cancel(&self) {
        if let Some(timer) = self.0.timer.take() {
            self.0.scheduler.cancel(timer);
            self.close_key("action");
        }
    }

    fn close_key(&self, key: &str) {
        let id = self.0.ids.borrow_mut().remove(key).unwrap_or(0);
        if id != 0 {
            self.notifier().close_notification(id);
        }
    }

    pub fn reset(&self) {
        self.cancel();
        let keys: Vec<&'static str> = self.0.ids.borrow().keys().copied().collect();
        for key in keys {
            self.close_key(key);
        }
        self.0.sent.borrow_mut().clear();
    }

    pub fn close(&self) {
        self.cancel();
        self.0.closed.set(true);
        if let Some(notifier) = self.0.notifier.borrow().clone() {
            notifier.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults;
    use crate::fakes::FakeSystem;
    use tessel_core::scheduler::FakeScheduler;

    #[derive(Default)]
    struct FakeNotifier {
        sent: RefCell<Vec<(String, String, u8, Vec<(String, String)>, u32)>>,
        closed: RefCell<Vec<u32>>,
        next: Cell<u32>,
    }

    impl Notifier for FakeNotifier {
        fn notify(&self, summary: &str, body: &str, urgency: u8, _icon: &str, actions: &[(String, String)], replaces: u32,
                  callback: Option<Sent>) {
            self.next.set(self.next.get() + 1);
            let id = if replaces != 0 { replaces } else { self.next.get() };
            self.sent.borrow_mut().push((summary.into(), body.into(), urgency, actions.to_vec(), id));
            if let Some(callback) = callback {
                callback(id);
            }
        }
        fn close_notification(&self, id: u32) {
            self.closed.borrow_mut().push(id);
        }
        fn close(&self) {}
    }

    struct Fx {
        backend: Rc<FakeSystem>,
        performed: Rc<RefCell<Vec<String>>>,
        notifier: Rc<FakeNotifier>,
        scheduler: Rc<FakeScheduler>,
        alerts: BatteryAlerts,
        settings: Value,
    }

    fn fixture() -> Fx {
        let backend = FakeSystem::ready();
        backend.set(|s| {
            s.has_battery = true;
            s.on_battery = true;
            s.percentage = Some(50.0);
            s.warning_level = 1;
            s.capabilities.insert("hibernate".into(), "yes".into());
        });
        let performed = Rc::new(RefCell::new(Vec::new()));
        let p = performed.clone();
        let notifier = Rc::new(FakeNotifier::default());
        let scheduler = Rc::new(FakeScheduler::default());
        let alerts = BatteryAlerts::new(backend.clone(), move |a| p.borrow_mut().push(a.into()), Some(notifier.clone()), scheduler.clone());
        let settings = defaults();
        alerts.update(&settings);
        Fx { backend, performed, notifier, scheduler, alerts, settings }
    }

    impl Fx {
        fn level(&self, percentage: Option<f64>, warning: i64) {
            self.backend.set(|s| { s.percentage = percentage; s.warning_level = warning; });
            self.alerts.refresh();
        }
        fn summaries(&self) -> Vec<String> {
            self.notifier.sent.borrow().iter().map(|s| s.0.clone()).collect()
        }
        fn timers(&self) -> u64 {
            self.scheduler.created()
        }
    }

    #[test]
    fn low_then_critical_once_per_discharge_cycle() {
        let f = fixture();
        f.level(Some(11.0), 1);
        assert!(f.summaries().is_empty());
        f.level(Some(10.0), 1);
        f.level(Some(9.0), 1);
        assert_eq!(f.summaries(), ["Battery low"]);
        assert_eq!(f.notifier.sent.borrow()[0].2, 1);
        assert_eq!(f.notifier.sent.borrow()[0].1, "10% remaining. Consider plugging in.");
        f.level(Some(5.0), 1);
        f.level(Some(4.0), 1);
        assert_eq!(f.summaries(), ["Battery low", "Battery critically low"]);
        assert_eq!(f.notifier.sent.borrow()[1].2, 2);
        f.backend.set(|s| s.on_battery = false);
        f.alerts.refresh();
        let mut closed = f.notifier.closed.borrow().clone();
        closed.sort();
        assert_eq!(closed, [1, 2]);
        f.backend.set(|s| s.on_battery = true);
        f.level(Some(9.0), 1);
        assert_eq!(f.notifier.sent.borrow().len(), 3);
    }

    #[test]
    fn upower_warning_levels_trigger_without_percentage() {
        let f = fixture();
        f.level(None, 3);
        f.level(None, 4);
        let urgencies: Vec<u8> = f.notifier.sent.borrow().iter().map(|s| s.2).collect();
        assert_eq!(urgencies, [1, 2]);
        assert_eq!(f.notifier.sent.borrow()[1].1, "Plug in now to avoid losing work.");
        assert_eq!(f.timers(), 0);
    }

    #[test]
    fn action_level_starts_cancellable_countdown_then_acts() {
        let f = fixture();
        f.level(Some(2.0), 5);
        let countdown = f.notifier.sent.borrow().last().unwrap().clone();
        assert_eq!(countdown.2, 2);
        assert_eq!(countdown.3[0].0, "cancel");
        assert!(countdown.1.contains("hibernate"));
        assert_eq!(f.scheduler.delays(), [COUNTDOWN * 1000]);
        f.level(Some(1.0), 5);
        assert_eq!(f.timers(), 1, "The countdown starts once");
        let id = f.scheduler.ids()[0];
        f.scheduler.fire(id);
        assert_eq!(*f.performed.borrow(), ["hibernate"]);
        assert!(f.notifier.closed.borrow().contains(&countdown.4));
    }

    #[test]
    fn cancel_action_and_plugging_in_stop_the_countdown() {
        let f = fixture();
        f.level(Some(2.0), 5);
        let id = f.notifier.sent.borrow().last().unwrap().4;
        f.alerts.action_invoked(id + 100, "cancel");
        assert!(f.alerts.counting_down());
        f.alerts.action_invoked(id, "cancel");
        assert!(!f.alerts.counting_down());
        assert_eq!(f.scheduler.cancelled.borrow().len(), 1);
        f.level(Some(1.0), 5);
        assert_eq!(f.timers(), 1, "A cancelled action does not restart this cycle");
        f.backend.set(|s| s.on_battery = false);
        f.alerts.refresh();
        f.backend.set(|s| s.on_battery = true);
        f.level(Some(2.0), 5);
        assert_eq!(f.timers(), 2);
        f.backend.set(|s| s.on_battery = false);
        f.alerts.refresh();
        assert_eq!(f.scheduler.cancelled.borrow().len(), 2);
        assert!(f.performed.borrow().is_empty(), "Expiry after plugging in must not act");
    }

    #[test]
    fn configured_percentage_and_nothing_action() {
        let mut f = fixture();
        f.settings["critical_level"] = 7.into();
        f.alerts.update(&f.settings);
        f.level(Some(8.0), 1);
        assert_eq!(f.timers(), 0);
        f.level(Some(7.0), 1);
        assert_eq!(f.timers(), 1);
        f.alerts.reset();
        f.settings["critical_action"] = "nothing".into();
        f.alerts.update(&f.settings);
        f.level(Some(3.0), 5);
        assert_eq!(f.timers(), 1);
    }

    #[test]
    fn unknown_source_does_not_reset_or_warn() {
        let f = fixture();
        f.level(Some(9.0), 1);
        f.backend.set(|s| s.power_source_known = false);
        f.alerts.refresh();
        f.backend.set(|s| s.power_source_known = true);
        f.alerts.refresh();
        assert_eq!(f.notifier.sent.borrow().len(), 1);
        assert!(f.notifier.closed.borrow().is_empty());
    }

    #[test]
    fn auto_action_prefers_unattended_hibernate() {
        let caps = |v: &str| Capabilities::from([("hibernate".to_owned(), v.to_owned())]);
        assert_eq!(resolve_action("auto", &caps("yes")), "hibernate");
        assert_eq!(resolve_action("auto", &caps("challenge")), "poweroff");
        assert_eq!(resolve_action("suspend", &Capabilities::new()), "suspend");
    }

    #[test]
    fn notification_hints_match_python() {
        let params = notify_parameters("Battery low", "body", 2, "battery-low-symbolic", &[("cancel".into(), "Cancel".into())], 7);
        assert_eq!(params.type_().as_str(), "(susssasa{sv}i)");
        assert_eq!(params.child_value(1).get::<u32>(), Some(7));
        assert_eq!(params.child_value(5).get::<Vec<String>>().unwrap(), ["cancel", "Cancel"]);
        let hints = glib::VariantDict::new(Some(&params.child_value(6)));
        assert_eq!(hints.lookup::<u8>("urgency").unwrap(), Some(2));
        assert_eq!(hints.lookup::<String>("desktop-entry").unwrap().as_deref(), Some("org.tessel.PowerManager"));
        assert_eq!(hints.lookup::<String>("category").unwrap().as_deref(), Some("device"));
        assert_eq!(hints.lookup::<bool>("resident").unwrap(), Some(true));
        assert_eq!(params.child_value(7).get::<i32>(), Some(0));
        let normal = notify_parameters("a", "b", 1, "i", &[], 0);
        assert_eq!(normal.child_value(7).get::<i32>(), Some(-1));
        assert!(!glib::VariantDict::new(Some(&normal.child_value(6))).contains("resident"));
    }
}
