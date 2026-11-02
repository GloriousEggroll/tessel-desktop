//! One-shot source choices bound to the requesting application's portal
//! session (client side of shared/capture_selection.py).
//!
//! The wlroots chooser supplies its exact session handle. There is deliberately
//! no shared "next chooser" file or well-known application name to race against
//! OBS: the request goes to the unique bus name derived from the session path.

use crate::sources::CaptureSource;
use gio::prelude::*;
use std::sync::OnceLock;

pub const PATH: &str = "/org/tessel/CaptureSelection";
pub const INTERFACE: &str = "org.tessel.CaptureSelection";
pub const SELECT_TIMEOUT_MS: i32 = 1200;
pub const SELECTED_TIMEOUT_MS: i32 = 1000;
pub const XML: &str = r#"<node><interface name="org.tessel.CaptureSelection">
<method name="Select"><arg type="o" direction="in"/><arg type="as" direction="in"/>
<arg type="b" direction="out"/><arg type="s" direction="out"/></method>
<method name="Selected"><arg type="o" direction="in"/><arg type="as" direction="in"/></method>
</interface></node>"#;

fn session_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    // Python's `\d` is Unicode-aware for str patterns, like Rust's.
    PATTERN.get_or_init(|| regex::Regex::new(r"^/org/freedesktop/portal/desktop/session/(\d+(?:_\d+)+)/capture_[a-f0-9]+$").unwrap())
}

/// `':' + group1.replace('_', '.')` for a valid xdpw session handle.
pub fn session_owner(session: &str) -> Option<String> {
    let captures = session_pattern().captures(session)?;
    Some(format!(":{}", captures[1].replace('_', ".")))
}

fn session_bus() -> Option<gio::DBusConnection> {
    gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok()
}

/// Return (handled, source). Unknown request owners retain the normal UI.
pub fn requested_selection_on(bus: Option<&gio::DBusConnection>, session: &str, sources: &[CaptureSource]) -> (bool, Option<CaptureSource>) {
    let Some(owner) = session_owner(session) else { return (false, None) };
    let owned;
    let bus = match bus {
        Some(bus) => bus,
        None => match session_bus() {
            Some(bus) => {
                owned = bus;
                &owned
            }
            None => return (false, None),
        },
    };
    let Ok(path) = glib::variant::ObjectPath::try_from(session.to_owned()) else { return (false, None) };
    let tokens: Vec<String> = sources.iter().map(|s| s.token.clone()).collect();
    let parameters = (path, tokens).to_variant();
    let reply = bus.call_sync(Some(&owner), PATH, INTERFACE, "Select", Some(&parameters), Some(glib::VariantTy::new("(bs)").unwrap()),
                              gio::DBusCallFlags::NO_AUTO_START, SELECT_TIMEOUT_MS, None::<&gio::Cancellable>);
    let Ok(reply) = reply else { return (false, None) };
    let Some((handled, token)) = reply.get::<(bool, String)>() else { return (false, None) };
    (handled, sources.iter().find(|s| s.token == token).cloned())
}

pub fn requested_selection(session: &str, sources: &[CaptureSource]) -> (bool, Option<CaptureSource>) {
    requested_selection_on(None, session, sources)
}

/// Attach capture output metadata only to this chooser's requesting session.
pub fn report_selection_on(bus: Option<&gio::DBusConnection>, session: &str, outputs: &[String]) {
    let Some(owner) = session_owner(session) else { return };
    let owned;
    let bus = match bus {
        Some(bus) => bus,
        None => match session_bus() {
            Some(bus) => {
                owned = bus;
                &owned
            }
            None => return,
        },
    };
    let Ok(path) = glib::variant::ObjectPath::try_from(session.to_owned()) else { return };
    let parameters = (path, outputs.to_vec()).to_variant();
    let _ = bus.call_sync(Some(&owner), PATH, INTERFACE, "Selected", Some(&parameters), None,
                          gio::DBusCallFlags::NO_AUTO_START, SELECTED_TIMEOUT_MS, None::<&gio::Cancellable>);
}

pub fn report_selection(session: &str, outputs: &[String]) {
    report_selection_on(None, session, outputs)
}
