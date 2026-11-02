//! lock_status.py: battery level and a privacy-safe notification count.
//!
//! Battery comes from UPower's DisplayDevice (system bus). Notifications come
//! from the suite's notification service (`org.tessel.Notifications1`
//! `GetState`/`StateChanged`) without auto-starting it. Only the unread count
//! and, optionally, application names leave this module.
use gio::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tessel_core::tr;
use tessel_gtk::gtk;
use gtk::prelude::*;

pub const UPOWER: &str = "org.freedesktop.UPower";
pub const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
pub const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";
pub const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
pub const NOTIFICATIONS_INTERFACE: &str = "org.tessel.Notifications1";
pub const CHARGING: u32 = 1;
pub const FULL: u32 = 4;
pub const PENDING_CHARGE: u32 = 5;

/// Python `round()` (half to even).
fn round_half_even(value: f64) -> f64 {
    let rounded = value.round();
    if (value - value.trunc()).abs() == 0.5 { 2.0 * (value / 2.0).round() } else { rounded }
}

/// `(icon, text)` for a battery, or None when there is no battery.
pub fn battery_summary(present: bool, kind: u32, percentage: f64, state: u32) -> Option<(String, String)> {
    if !present || kind != 2 {
        return None; // 2 = battery; mains-only PCs show nothing.
    }
    let level = round_half_even(percentage).clamp(0.0, 100.0) as i64;
    let charging = state == CHARGING;
    let detail = if state == FULL { tr!("Fully charged") } else if charging { tr!("Charging") }
                 else if state == PENDING_CHARGE { tr!("Not charging") } else { String::new() };
    let step = if level >= 90 { "full" } else if level >= 50 { "good" } else if level >= 20 { "low" }
               else if level >= 6 { "caution" } else { "empty" };
    let icon = format!("battery-{step}{}-symbolic", if charging || state == FULL { "-charging" } else { "" });
    let text = format!("{level}%") + &if detail.is_empty() { String::new() } else { format!(" · {detail}") };
    Some((icon, text))
}

/// Unread count text (and optional app names), never notification content.
pub fn notification_summary(state: Option<&Value>, show_names: bool) -> Option<String> {
    let state = state?.as_object()?;
    let unread = match state.get("unread") {
        None => 0,
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64))?,
        Some(Value::Bool(b)) => *b as i64,
        Some(Value::String(s)) => s.trim().parse::<i64>().ok()?,
        Some(_) => return None,
    };
    if unread <= 0 {
        return None;
    }
    let mut text = if unread == 1 { tr!("1 new notification") } else { tr!("{count} new notifications", count = unread) };
    if show_names {
        let mut names: Vec<String> = Vec::new();
        for item in state.get("notifications").and_then(Value::as_array).into_iter().flatten() {
            let Some(item) = item.as_object() else { continue };
            let truthy = |key: &str| item.get(key).is_some_and(tessel_core::pyjson::truthy);
            if truthy("read") || truthy("transient") {
                continue;
            }
            let pick = |key: &str| item.get(key).filter(|v| tessel_core::pyjson::truthy(v)).map(python_str);
            let name = pick("app_name").or_else(|| pick("desktop_entry")).unwrap_or_default().trim().to_string();
            if !name.is_empty() && !names.contains(&name) {
                // Python appends name[:64] but compares the untruncated name.
                names.push(name.chars().take(64).collect());
            }
        }
        if !names.is_empty() {
            text += &format!(" · {}{}", names.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
                             if names.len() > 4 { " …" } else { "" });
        }
    }
    Some(text)
}

fn python_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => tessel_core::pyjson::py_str(other),
    }
}

struct Inner {
    changed: Box<dyn Fn()>,
    battery: RefCell<Option<(String, String)>>,
    notifications: RefCell<Value>,
    closed: Cell<bool>,
    cancel: gio::Cancellable,
    upower: RefCell<Option<gio::DBusProxy>>,
    notify: RefCell<Option<gio::DBusProxy>>,
    upower_pending: Cell<bool>,
    notify_pending: Cell<bool>,
    handlers: RefCell<Vec<(gio::DBusProxy, glib::SignalHandlerId)>>,
}

/// Watches UPower and the notification service; `changed()` on updates.
#[derive(Clone)]
pub struct LockStatus(Rc<Inner>);

impl LockStatus {
    pub fn new(changed: impl Fn() + 'static) -> Self {
        Self(Rc::new(Inner { changed: Box::new(changed), battery: RefCell::new(None),
            notifications: RefCell::new(Value::Object(Default::default())), closed: false.into(),
            cancel: gio::Cancellable::new(), upower: RefCell::new(None), notify: RefCell::new(None),
            upower_pending: false.into(), notify_pending: false.into(), handlers: RefCell::new(Vec::new()) }))
    }

    pub fn battery(&self) -> Option<(String, String)> { self.0.battery.borrow().clone() }
    pub fn set_battery(&self, battery: Option<(String, String)>) { *self.0.battery.borrow_mut() = battery }
    pub fn notifications(&self) -> Value { self.0.notifications.borrow().clone() }
    pub fn set_notifications(&self, state: Value) { *self.0.notifications.borrow_mut() = state }

    pub fn start(&self, battery: bool, notifications: bool) {
        if self.0.closed.get() {
            return;
        }
        // Python checks `self._upower is None`; a pending creation is not repeated here.
        if battery && self.0.upower.borrow().is_none() && !self.0.upower_pending.get() {
            self.0.upower_pending.set(true);
            let this = self.clone();
            gio::DBusProxy::for_bus(gio::BusType::System, gio::DBusProxyFlags::DO_NOT_AUTO_START, None, UPOWER,
                DISPLAY_DEVICE, UPOWER_DEVICE, Some(&self.0.cancel), move |result| {
                    this.0.upower_pending.set(false);
                    if let Ok(proxy) = result { this.upower_ready(proxy) }
                });
        }
        if notifications && self.0.notify.borrow().is_none() && !self.0.notify_pending.get() {
            self.0.notify_pending.set(true);
            let this = self.clone();
            gio::DBusProxy::for_bus(gio::BusType::Session,
                gio::DBusProxyFlags::DO_NOT_AUTO_START | gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES, None, NOTIFICATIONS,
                NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, Some(&self.0.cancel), move |result| {
                    this.0.notify_pending.set(false);
                    if let Ok(proxy) = result { this.notify_ready(proxy) }
                });
        }
    }

    fn upower_ready(&self, proxy: gio::DBusProxy) {
        if self.0.closed.get() {
            return;
        }
        *self.0.upower.borrow_mut() = Some(proxy.clone());
        let weak = Rc::downgrade(&self.0);
        let id = proxy.connect_local("g-properties-changed", false, move |_| {
            if let Some(inner) = weak.upgrade() { LockStatus(inner).read_battery() }
            None
        });
        self.0.handlers.borrow_mut().push((proxy, id));
        self.read_battery();
    }

    fn read_battery(&self) {
        let Some(proxy) = self.0.upower.borrow().clone() else { return };
        let values: Vec<Option<glib::Variant>> = ["IsPresent", "Type", "Percentage", "State"].iter()
            .map(|name| proxy.cached_property(name)).collect();
        let battery = match values.as_slice() {
            [Some(present), Some(kind), Some(percentage), Some(state)] => battery_summary(
                present.get::<bool>().unwrap_or(false), kind.get::<u32>().unwrap_or(0),
                percentage.get::<f64>().unwrap_or(0.0), state.get::<u32>().unwrap_or(0)),
            _ => None,
        };
        *self.0.battery.borrow_mut() = battery;
        if !self.0.closed.get() {
            (self.0.changed)();
        }
    }

    fn notify_ready(&self, proxy: gio::DBusProxy) {
        if self.0.closed.get() {
            return;
        }
        *self.0.notify.borrow_mut() = Some(proxy.clone());
        let weak = Rc::downgrade(&self.0);
        let id = proxy.connect_local("g-signal", false, move |values| {
            let name = values.get(2).and_then(|v| v.get::<String>().ok());
            let parameters = values.get(3).and_then(|v| v.get::<glib::Variant>().ok());
            if let (Some(inner), Some("StateChanged"), Some(parameters)) = (weak.upgrade(), name.as_deref(), parameters) {
                if let Some(text) = parameters.try_child_value(0).and_then(|v| v.str().map(str::to_owned)) {
                    LockStatus(inner).notify_state(&text);
                }
            }
            None
        });
        let weak = Rc::downgrade(&self.0);
        let owner = proxy.connect_notify_local(Some("g-name-owner"), move |_, _| {
            if let Some(inner) = weak.upgrade() { LockStatus(inner).load_notifications() }
        });
        self.0.handlers.borrow_mut().push((proxy.clone(), id));
        self.0.handlers.borrow_mut().push((proxy, owner));
        self.load_notifications();
    }

    fn load_notifications(&self) {
        let Some(proxy) = self.0.notify.borrow().clone() else { return };
        if proxy.name_owner().is_none() {
            *self.0.notifications.borrow_mut() = Value::Object(Default::default());
            (self.0.changed)();
            return;
        }
        let this = self.clone();
        proxy.call("GetState", None, gio::DBusCallFlags::NO_AUTO_START, 3000, Some(&self.0.cancel), move |result| {
            if let Some(text) = result.ok().and_then(|v| v.try_child_value(0)).and_then(|v| v.str().map(str::to_owned)) {
                this.notify_state(&text);
            }
        });
    }

    fn notify_state(&self, encoded: &str) {
        if self.0.closed.get() {
            return;
        }
        let Ok(state) = serde_json::from_str::<Value>(encoded) else { return };
        *self.0.notifications.borrow_mut() = if state.is_object() { state } else { Value::Object(Default::default()) };
        (self.0.changed)();
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.cancel.cancel();
        for (proxy, handler) in self.0.handlers.borrow_mut().drain(..) {
            proxy.disconnect(handler);
        }
        self.0.upower.borrow_mut().take();
        self.0.notify.borrow_mut().take();
    }
}

/// Centered battery and notification indicators under the date.
#[derive(Clone)]
pub struct StatusRow {
    pub widget: gtk::Box,
    pub battery: gtk::Box,
    pub battery_icon: gtk::Image,
    pub battery_label: gtk::Label,
    pub notifications: gtk::Box,
    pub notifications_label: gtk::Label,
    pub fingerprint: gtk::Box,
    pub fingerprint_label: gtk::Label,
}

impl Default for StatusRow {
    fn default() -> Self { Self::new() }
}

impl StatusRow {
    pub fn new() -> Self {
        use tessel_gtk::utils::image;
        let widget = gtk::Box::builder().spacing(28).halign(gtk::Align::Center).visible(false).build();
        widget.add_css_class("lock-information");
        widget.add_css_class("lock-status");
        let battery = gtk::Box::builder().spacing(8).visible(false).build();
        let battery_icon = image("battery-good-symbolic", 22);
        let battery_label = gtk::Label::new(None);
        battery.append(&battery_icon);
        battery.append(&battery_label);
        widget.append(&battery);
        let notifications = gtk::Box::builder().spacing(8).visible(false).build();
        notifications.append(&image("preferences-system-notifications-symbolic", 22));
        let notifications_label = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(48).build();
        notifications.append(&notifications_label);
        widget.append(&notifications);
        let fingerprint = gtk::Box::builder().spacing(8).visible(false).build();
        fingerprint.append(&image("auth-fingerprint-symbolic", 22));
        let fingerprint_label = gtk::Label::builder().wrap(true).max_width_chars(40).build();
        fingerprint.append(&fingerprint_label);
        widget.append(&fingerprint);
        Self { widget, battery, battery_icon, battery_label, notifications, notifications_label, fingerprint, fingerprint_label }
    }

    pub fn update(&self, battery: Option<&(String, String)>, notifications: Option<&str>, fingerprint: &str) {
        self.battery.set_visible(battery.is_some());
        if let Some((icon, text)) = battery {
            self.battery_icon.set_icon_name(Some(icon));
            self.battery_label.set_text(text);
            self.battery.update_property(&[gtk::accessible::Property::Label(&format!("{} {text}", tr!("Battery")))]);
        }
        let notifications = notifications.unwrap_or("");
        self.notifications.set_visible(!notifications.is_empty());
        self.notifications_label.set_text(notifications);
        self.fingerprint.set_visible(!fingerprint.is_empty());
        self.fingerprint_label.set_text(fingerprint);
        self.widget.set_visible(self.battery.get_visible() || self.notifications.get_visible() || self.fingerprint.get_visible());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary(present: bool, kind: u32, pct: f64, state: u32) -> Option<(String, String)> {
        battery_summary(present, kind, pct, state)
    }

    #[test]
    fn battery_summary_matches_python() {
        assert_eq!(summary(false, 2, 50.0, 2), None);
        assert_eq!(summary(true, 1, 100.0, 0), None); // Line power only.
        assert_eq!(summary(true, 2, 82.4, 1), Some(("battery-good-charging-symbolic".into(), "82% · Charging".into())));
        assert_eq!(summary(true, 2, 40.0, 2), Some(("battery-low-symbolic".into(), "40%".into())));
        assert_eq!(summary(true, 2, 100.0, 4).unwrap().1, "100% · Fully charged");
        assert_eq!(summary(true, 2, 100.0, 4).unwrap().0, "battery-full-charging-symbolic");
        assert_eq!(summary(true, 2, 70.0, 5).unwrap().1, "70% · Not charging");
        assert_eq!(summary(true, 2, 5.5, 2).unwrap(), ("battery-caution-symbolic".into(), "6%".into()));
        assert_eq!(summary(true, 2, 4.5, 2).unwrap(), ("battery-empty-symbolic".into(), "4%".into()));
        assert_eq!(summary(true, 2, 120.0, 2).unwrap().1, "100%");
    }

    #[test]
    fn notification_summary_never_includes_content() {
        let state = json!({"unread": 3, "notifications": [
            {"app_name": "Mail", "summary": "Secret subject", "body": "Private", "read": false},
            {"app_name": "Chat", "summary": "Hi", "read": false},
            {"app_name": "Mail", "summary": "Again", "read": false},
            {"app_name": "Old", "summary": "Seen", "read": true}]});
        assert_eq!(notification_summary(Some(&state), false).unwrap(), "3 new notifications");
        let named = notification_summary(Some(&state), true).unwrap();
        assert_eq!(named, "3 new notifications · Mail, Chat");
        for secret in ["Secret", "Private", "Hi", "Old"] {
            assert!(!named.contains(secret));
        }
        assert_eq!(notification_summary(Some(&json!({"unread": 1})), false).unwrap(), "1 new notification");
        for empty in [json!({"unread": 0}), json!({}), json!({"unread": "x"}), json!([])] {
            assert_eq!(notification_summary(Some(&empty), false), None);
        }
        assert_eq!(notification_summary(None, false), None);
        let many = json!({"unread": 9, "notifications": [
            {"app_name": "A"}, {"app_name": "B"}, {"desktop_entry": "c.desktop"}, {"app_name": "D"}, {"app_name": "E"},
            {"app_name": "T", "transient": true}]});
        assert_eq!(notification_summary(Some(&many), true).unwrap(), "9 new notifications · A, B, c.desktop, D …");
    }
}
