//! A FAKE org.freedesktop.timedate1 for a private bus (tests/private_bus.rs and
//! examples/fake_timedate.rs). It only records calls and updates its own
//! in-memory properties: nothing ever reaches the real clock or timedated.
#![allow(dead_code)]

use gio::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::rc::Rc;

pub const XML: &str = r#"<node><interface name="org.freedesktop.timedate1">
  <property name="Timezone" type="s" access="read"/>
  <property name="NTP" type="b" access="read"/>
  <property name="CanNTP" type="b" access="read"/>
  <property name="NTPSynchronized" type="b" access="read"/>
  <method name="SetTime"><arg type="x" direction="in"/><arg type="b" direction="in"/><arg type="b" direction="in"/></method>
  <method name="SetTimezone"><arg type="s" direction="in"/><arg type="b" direction="in"/></method>
  <method name="SetNTP"><arg type="b" direction="in"/><arg type="b" direction="in"/></method>
  <method name="ListTimezones"><arg type="as" direction="out"/></method>
</interface></node>"#;

#[derive(Debug)]
pub struct Timedate {
    pub timezone: String,
    pub ntp: bool,
    pub can_ntp: bool,
    pub synchronized: bool,
    /// (method, params as text, interactive flag)
    pub calls: Vec<(String, String, bool)>,
    /// Reply to the next write with this D-Bus error name.
    pub fail_next: Option<String>,
    pub zones: Vec<String>,
}

impl Default for Timedate {
    fn default() -> Self {
        Self { timezone: "America/Denver".into(), ntp: false, can_ntp: true, synchronized: false, calls: Vec::new(), fail_next: None,
               zones: ["UTC", "America/Denver", "Europe/London", "Asia/Tokyo", "UTC"].map(String::from).to_vec() }
    }
}

pub type Shared = Rc<RefCell<Timedate>>;

pub fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>)
        .expect("private bus")
}

pub fn own(connection: &gio::DBusConnection) {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                         Some(&("org.freedesktop.timedate1", 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                         None::<&gio::Cancellable>).expect("own name");
}

pub fn release(connection: &gio::DBusConnection) {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
                         Some(&("org.freedesktop.timedate1",).to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                         None::<&gio::Cancellable>).expect("release name");
}

fn changed(connection: &gio::DBusConnection, name: &str, value: Variant) {
    let dict = glib::VariantDict::new(None);
    dict.insert_value(name, &value);
    let params = Variant::tuple_from_iter(["org.freedesktop.timedate1".to_variant(), dict.end(), Vec::<String>::new().to_variant()]);
    let _ = connection.emit_signal(None, "/org/freedesktop/timedate1", "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&params));
}

/// Export the fake on its own connection and own the well-known name.
pub fn serve(address: &str, state: &Shared) -> gio::DBusConnection {
    let connection = connect(address);
    let info = gio::DBusNodeInfo::for_xml(XML).unwrap();
    let interface = info.lookup_interface("org.freedesktop.timedate1").unwrap();
    let (s1, s2) = (state.clone(), state.clone());
    connection.register_object("/org/freedesktop/timedate1", &interface)
        .method_call(move |connection, _, _, _, method, params, invocation| {
            let interactive = invocation.message().flags().contains(gio::DBusMessageFlags::ALLOW_INTERACTIVE_AUTHORIZATION);
            s1.borrow_mut().calls.push((method.into(), params.print(false).to_string(), interactive));
            if method == "ListTimezones" {
                let zones = s1.borrow().zones.clone();
                invocation.return_value(Some(&(zones,).to_variant()));
                return;
            }
            if let Some(error) = s1.borrow_mut().fail_next.take() {
                invocation.return_dbus_error(&error, "Refused by the fake service");
                return;
            }
            match method {
                "SetNTP" => {
                    let (enabled, _): (bool, bool) = params.get().unwrap();
                    s1.borrow_mut().ntp = enabled;
                    changed(&connection, "NTP", enabled.to_variant());
                }
                "SetTimezone" => {
                    let (zone, _): (String, bool) = params.get().unwrap();
                    s1.borrow_mut().timezone = zone.clone();
                    changed(&connection, "Timezone", zone.to_variant());
                }
                _ => {}
            }
            invocation.return_value(None);
        })
        .property(move |_, _, _, _, name| {
            let s = s2.borrow();
            match name {
                "Timezone" => s.timezone.to_variant(),
                "NTP" => s.ntp.to_variant(),
                "CanNTP" => s.can_ntp.to_variant(),
                _ => s.synchronized.to_variant(),
            }
        })
        .build().unwrap();
    own(&connection);
    connection
}
