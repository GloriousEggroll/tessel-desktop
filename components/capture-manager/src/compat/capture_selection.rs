//! One-shot source choices bound to the requesting application's portal session
//! (shared/capture_selection.py). The wlroots chooser supplies its exact session
//! handle; there is deliberately no shared "next chooser" file or well-known name.
//!
//! Server side: [`broker`] registers `/org/waydock/CaptureSelection` once per
//! connection (keyed by its unique name). Client side: [`requested_selection`] /
//! [`report_selection`] (also ported in wayshell-capture-picker).

use gio::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

pub const PATH: &str = "/org/waydock/CaptureSelection";
pub const INTERFACE: &str = "org.waydock.CaptureSelection";
pub const XML: &str = r#"<node><interface name="org.waydock.CaptureSelection">
<method name="Select"><arg type="o" direction="in"/><arg type="as" direction="in"/>
<arg type="b" direction="out"/><arg type="s" direction="out"/></method>
<method name="Selected"><arg type="o" direction="in"/><arg type="as" direction="in"/></method>
</interface></node>"#;

type Observer = Box<dyn FnOnce(Vec<String>)>;

pub struct SelectionBroker {
    /// session handle → requested output connector (popped by `Select`).
    pub targets: RefCell<HashMap<String, String>>,
    /// session handle → callback receiving the chooser's outputs (popped by `Selected`).
    pub observers: RefCell<HashMap<String, Observer>>,
    pub registration: RefCell<Option<gio::RegistrationId>>,
}

/// `Select`'s answer for a popped target: `(target is not None, chosen token or "")`.
pub fn choose(target: Option<&str>, tokens: &[String]) -> (bool, String) {
    let mut chosen = String::new();
    if let Some(target) = target.filter(|t| !t.is_empty()) {
        if tokens.len() <= 8192 {
            for token in tokens {
                if let Some(rest) = token.strip_prefix("Monitor: ") {
                    if rest.split(' ').next().unwrap_or("") == target {
                        chosen = token.clone();
                        break;
                    }
                }
            }
        }
    }
    // A vanished output cancels this request; never silently select another.
    (target.is_some(), chosen)
}

/// `Selected` argument check: at most 16 outputs of at most 128 characters.
pub fn valid_outputs(outputs: &[String]) -> bool {
    outputs.len() <= 16 && outputs.iter().all(|name| name.chars().count() <= 128)
}

impl SelectionBroker {
    fn call(&self, method: &str, parameters: &glib::Variant, invocation: gio::DBusMethodInvocation) {
        if method == "Selected" {
            let Some((session, outputs)) = parameters.get::<(glib::variant::ObjectPath, Vec<String>)>() else {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "Invalid capture outputs");
                return;
            };
            if !valid_outputs(&outputs) {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "Invalid capture outputs");
                return;
            }
            let callback = self.observers.borrow_mut().remove(session.as_str());
            if let Some(callback) = callback {
                callback(outputs);
            }
            invocation.return_value(None);
            return;
        }
        let Some((session, tokens)) = parameters.get::<(glib::variant::ObjectPath, Vec<String>)>() else {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "Invalid arguments");
            return;
        };
        let target = self.targets.borrow_mut().remove(session.as_str());
        let (handled, chosen) = choose(target.as_deref(), &tokens);
        // Python: `if target and …` — an empty target string still reports handled.
        invocation.return_value(Some(&(handled, chosen).to_variant()));
    }
}

thread_local! {
    static BROKERS: RefCell<HashMap<String, Rc<SelectionBroker>>> = RefCell::new(HashMap::new());
}

/// The broker for this connection, registered on first use (Python `broker(bus)`).
pub fn broker(bus: &gio::DBusConnection) -> Rc<SelectionBroker> {
    let key = bus.unique_name().map(|n| n.to_string()).unwrap_or_default();
    if let Some(existing) = BROKERS.with(|map| map.borrow().get(&key).cloned()) {
        return existing;
    }
    let broker = Rc::new(SelectionBroker { targets: RefCell::default(), observers: RefCell::default(), registration: RefCell::default() });
    let info = gio::DBusNodeInfo::for_xml(XML).expect("CaptureSelection XML");
    let interface = info.lookup_interface(INTERFACE).expect("CaptureSelection interface");
    let weak = Rc::downgrade(&broker);
    let registration = bus
        .register_object(PATH, &interface)
        .method_call(move |_, _, _, _, method, parameters, invocation| match weak.upgrade() {
            Some(broker) => broker.call(method, &parameters, invocation),
            None => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownObject", "Capture selection ended"),
        })
        .build();
    match registration {
        Ok(id) => *broker.registration.borrow_mut() = Some(id),
        Err(error) => eprintln!("waycapture: could not register {PATH}: {}", error.message()),
    }
    BROKERS.with(|map| map.borrow_mut().insert(key, broker.clone()));
    broker
}

fn session_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"^/org/freedesktop/portal/desktop/session/(\d+(?:_\d+)+)/capture_[a-f0-9]+$").unwrap())
}

/// `':' + group1.replace('_', '.')` for a valid xdpw session handle.
pub fn session_owner(session: &str) -> Option<String> {
    let captures = session_pattern().captures(session)?;
    Some(format!(":{}", captures[1].replace('_', ".")))
}

/// Return (handled, token). Unknown request owners retain the normal UI.
pub fn requested_selection(bus: &gio::DBusConnection, session: &str, tokens: &[String]) -> (bool, Option<String>) {
    let Some(owner) = session_owner(session) else { return (false, None) };
    let Ok(path) = glib::variant::ObjectPath::try_from(session.to_owned()) else { return (false, None) };
    let reply = bus.call_sync(Some(&owner), PATH, INTERFACE, "Select", Some(&(path, tokens.to_vec()).to_variant()),
        Some(glib::VariantTy::new("(bs)").unwrap()), gio::DBusCallFlags::NO_AUTO_START, 1200, None::<&gio::Cancellable>);
    match reply.ok().and_then(|r| r.get::<(bool, String)>()) {
        Some((handled, token)) => (handled, tokens.iter().find(|t| **t == token).cloned()),
        None => (false, None),
    }
}

/// Attach capture output metadata only to this chooser's requesting session.
pub fn report_selection(bus: &gio::DBusConnection, session: &str, outputs: &[String]) {
    let Some(owner) = session_owner(session) else { return };
    let Ok(path) = glib::variant::ObjectPath::try_from(session.to_owned()) else { return };
    let _ = bus.call_sync(Some(&owner), PATH, INTERFACE, "Selected", Some(&(path, outputs.to_vec()).to_variant()), None,
        gio::DBusCallFlags::NO_AUTO_START, 1000, None::<&gio::Cancellable>);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_matches_only_the_exact_monitor_and_vanished_outputs_cancel() {
        let list = tokens(&["Window: 1 firefox", "Monitor: HDMI-A-1 Dell", "Monitor: DP-1 LG", "Monitor: DP-10 x"]);
        assert_eq!(choose(Some("DP-1"), &list), (true, "Monitor: DP-1 LG".into()));
        assert_eq!(choose(Some("eDP-1"), &list), (true, String::new()));
        assert_eq!(choose(None, &list), (false, String::new()));
        let many: Vec<String> = (0..8193).map(|i| format!("Monitor: X-{i}")).collect();
        assert_eq!(choose(Some("X-1"), &many), (true, String::new()));
        assert!(valid_outputs(&tokens(&["DP-1"])));
        assert!(!valid_outputs(&vec!["a".to_string(); 17]));
        assert!(!valid_outputs(&["x".repeat(129)]));
    }

    #[test]
    fn session_owner_rules() {
        assert_eq!(session_owner("/org/freedesktop/portal/desktop/session/1_42/capture_0a1b").as_deref(), Some(":1.42"));
        assert_eq!(session_owner("/org/freedesktop/portal/desktop/session/1_42/obs_1"), None);
        assert_eq!(session_owner("/org/freedesktop/portal/desktop/session/42/capture_ab"), None);
    }
}
