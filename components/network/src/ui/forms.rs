//! `connection_forms.py`: our GTK forms that edit an isolated connection draft.
//!
//! No control contacts NetworkManager or writes to disk. Pages stage input
//! until `apply()`; the owning wizard validates and saves the connection.
//! Error texts never include entered secret values.

use crate::libnm::{self, Connection, IpAddress, IpRoute, Peer, Setting};
use crate::profiles::{setting_schema, PropertySpec};
use crate::ui::style::{accessible_label, expand, icon, label, margins, remove_children, style};
use glib::prelude::*;
use glib::Variant;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::rc::{Rc, Weak};
use tessel_core::pyjson::casefold;
use tessel_core::tr;

pub type FormResult<T> = Result<T, String>;
pub type Operation = Box<dyn FnOnce() -> FormResult<()>>;

/// A page of the editor (`SettingPage`, `IPPage`, …).
pub trait Page {
    fn widget(&self) -> gtk::Widget;
    /// Validate everything first; the returned operations change the draft.
    fn prepare(&self) -> FormResult<Vec<Operation>>;
    fn apply(&self) -> FormResult<()> {
        for operation in self.prepare()? {
            operation()?;
        }
        Ok(())
    }
}

pub fn title(name: &str) -> String {
    match name {
        "connection" => tr!("General"), "802-3-ethernet" => tr!("Ethernet"), "802-11-wireless" => "Wi-Fi".into(),
        "802-11-wireless-security" => "Wi-Fi security".into(), "802-1x" => tr!("Authentication"), "ipv4" => "IPv4".into(),
        "ipv6" => "IPv6".into(), "wireguard" => tr!("WireGuard"), "vpn" => "VPN".into(), "gsm" | "cdma" => tr!("Mobile broadband"),
        "pppoe" => "DSL".into(), "adsl" => "ADSL".into(), "bluetooth" => tr!("Bluetooth"), "infiniband" => tr!("InfiniBand"),
        "bridge" => tr!("Bridge"), "bridge-port" => tr!("Bridge port"), "bond" => tr!("Bond"), "bond-port" => tr!("Bond port"),
        "team" => tr!("Team"), "team-port" => tr!("Team port"), "vlan" => "VLAN".into(), "vxlan" => "VXLAN".into(),
        "macvlan" => "MACVLAN".into(), "tun" => tr!("Tunnel"), "ip-tunnel" => "IP tunnel".into(), "proxy" => tr!("Proxy"),
        "serial" => tr!("Serial"), "ppp" => "PPP".into(), "sriov" => "SR-IOV".into(), "tc" => tr!("Traffic control"),
        "ethtool" => tr!("Link features"), "dcb" => tr!("Data center bridging"), "ovs-bridge" => tr!("Open vSwitch bridge"),
        "ovs-port" => tr!("Open vSwitch port"), "ovs-interface" => tr!("Open vSwitch interface"),
        other => crate::nm::capitalize(&other.replace('-', " ")),
    }
}

pub fn field_name(name: &str) -> String {
    match name {
        "id" => tr!("Connection name"), "interface-name" => tr!("Interface"), "autoconnect" => tr!("Connect automatically"),
        "ssid" => tr!("Network name (SSID)"), "key-mgmt" => tr!("Authentication"), "psk" => tr!("Password"), "mtu" => "MTU".into(),
        "mac-address" => tr!("Device MAC address"), "cloned-mac-address" => "MAC address to use".into(),
        "auto-negotiate" => tr!("Negotiate link speed automatically"), "eap" => tr!("Authentication methods"),
        "identity" => tr!("Identity"), "anonymous-identity" => tr!("Anonymous identity"), "phase2-auth" => tr!("Inner authentication"),
        "ca-cert" => "CA certificate".into(), "client-cert" => tr!("Client certificate"), "private-key" => tr!("Private key"),
        "private-key-password" => tr!("Private key password"), "dns" => "DNS servers".into(), "dns-search" => tr!("Search domains"),
        "ignore-auto-dns" => tr!("Ignore automatic DNS servers"), "ignore-auto-routes" => tr!("Ignore automatic routes"),
        "never-default" => tr!("Use this connection only for its own network"), "may-fail" => tr!("Allow this address family to fail"),
        "route-metric" => tr!("Route priority"), "gateway" => tr!("Gateway"), "listen-port" => tr!("Listen port"),
        "peer-routes" => tr!("Create routes for peer networks"), "wep-key0" => "WEP key 1".into(), "wep-key1" => "WEP key 2".into(),
        "wep-key2" => "WEP key 3".into(), "wep-key3" => "WEP key 4".into(),
        other => crate::nm::capitalize(&other.replace('-', " ")),
    }
}

pub fn basic(setting: &str) -> &'static [&'static str] {
    match setting {
        "connection" => &["id", "interface-name", "autoconnect"],
        "802-3-ethernet" => &["mac-address", "cloned-mac-address", "mtu", "auto-negotiate", "speed", "duplex"],
        "802-11-wireless" => &["ssid", "mode", "hidden", "band", "channel", "mac-address", "cloned-mac-address", "mtu"],
        "802-11-wireless-security" => &["psk", "wep-key0", "wep-tx-keyidx", "leap-username", "leap-password"],
        "802-1x" => &["eap", "identity", "anonymous-identity", "password", "ca-cert", "domain-suffix-match", "client-cert", "private-key",
                      "private-key-password", "phase2-auth"],
        "wireguard" => &["private-key", "listen-port", "mtu", "peer-routes"],
        "gsm" => &["auto-config", "apn", "username", "password", "pin", "home-only"],
        "cdma" => &["number", "username", "password"],
        "pppoe" => &["parent", "username", "password", "service"],
        "adsl" => &["username", "password", "protocol", "encapsulation", "vpi", "vci"],
        "bluetooth" => &["bdaddr", "type"],
        "vlan" => &["parent", "id", "protocol"],
        "bridge" => &["stp", "priority", "forward-delay", "vlan-filtering"],
        "bond" => &["options"],
        "team" => &["config"],
        "proxy" => &["method", "pac-url", "pac-script"],
        "tun" => &["mode", "owner", "group", "pi", "vnet-hdr", "multi-queue"],
        "ip-tunnel" => &["mode", "parent", "local", "remote", "mtu"],
        "vxlan" => &["parent", "id", "local", "remote", "destination-port"],
        "macvlan" => &["parent", "mode", "promiscuous", "tap"],
        "infiniband" => &["mac-address", "transport-mode", "mtu", "p-key", "parent"],
        _ => &[],
    }
}

pub fn string_choices(setting: &str, name: &str) -> Option<Vec<(String, String)>> {
    let pairs: Vec<(&str, String)> = match (setting, name) {
        ("802-11-wireless", "mode") => vec![("infrastructure", tr!("Client")), ("ap", tr!("Hotspot")), ("adhoc", "Ad-hoc".into()),
                                             ("mesh", tr!("Mesh"))],
        ("802-11-wireless", "band") => vec![("", tr!("Automatic")), ("a", "5 GHz".into()), ("bg", "2.4 GHz".into())],
        ("802-3-ethernet", "duplex") => vec![("", tr!("Automatic")), ("full", tr!("Full duplex")), ("half", tr!("Half duplex"))],
        ("802-1x", "phase2-auth") => vec![("", tr!("Automatic")), ("mschapv2", "MSCHAPv2".into()), ("pap", "PAP".into()),
                                          ("chap", "CHAP".into()), ("mschap", "MSCHAP".into()), ("gtc", "GTC".into()), ("tls", "TLS".into())],
        _ => return None,
    };
    Some(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

pub const OPTIONAL: &[&str] = &["802-1x", "802-11-wireless-security", "ipv4", "ipv6", "proxy", "ethtool", "tc", "sriov", "dcb",
    "serial", "ppp", "bridge-port", "bond-port", "team-port", "ovs-external-ids", "ovs-other-config", "link", "match", "user", "hostname"];

pub fn locked(setting: &str) -> &'static [&'static str] {
    match setting {
        "connection" => &["uuid", "type", "timestamp", "read-only"],
        "802-11-wireless" => &["seen-bssids"],
        _ => &[],
    }
}

pub const ETHTOOL_NAMES: &[&str] = &["channels-combined", "channels-other", "channels-rx", "channels-tx", "coalesce-adaptive-rx",
    "coalesce-adaptive-tx", "coalesce-pkt-rate-high", "coalesce-pkt-rate-low", "coalesce-rx-frames", "coalesce-rx-frames-high",
    "coalesce-rx-frames-irq", "coalesce-rx-frames-low", "coalesce-rx-usecs", "coalesce-rx-usecs-high", "coalesce-rx-usecs-irq",
    "coalesce-rx-usecs-low", "coalesce-sample-interval", "coalesce-stats-block-usecs", "coalesce-tx-frames", "coalesce-tx-frames-high",
    "coalesce-tx-frames-irq", "coalesce-tx-frames-low", "coalesce-tx-usecs", "coalesce-tx-usecs-high", "coalesce-tx-usecs-irq",
    "coalesce-tx-usecs-low", "eee-enabled", "feature-esp-hw-offload", "feature-esp-tx-csum-hw-offload", "feature-fcoe-mtu",
    "feature-gro", "feature-gso", "feature-highdma", "feature-hw-tc-offload", "feature-l2-fwd-offload", "feature-loopback",
    "feature-lro", "feature-macsec-hw-offload", "feature-ntuple", "feature-rx", "feature-rxhash", "feature-rxvlan", "feature-rx-all",
    "feature-rx-fcs", "feature-rx-gro-hw", "feature-rx-gro-list", "feature-rx-udp-gro-forwarding", "feature-rx-udp_tunnel-port-offload",
    "feature-rx-vlan-filter", "feature-rx-vlan-stag-filter", "feature-rx-vlan-stag-hw-parse", "feature-sg", "feature-tls-hw-record",
    "feature-tls-hw-rx-offload", "feature-tls-hw-tx-offload", "feature-tso", "feature-tx", "feature-txvlan",
    "feature-tx-checksum-fcoe-crc", "feature-tx-checksum-ipv4", "feature-tx-checksum-ipv6", "feature-tx-checksum-ip-generic",
    "feature-tx-checksum-sctp", "feature-tx-esp-segmentation", "feature-tx-fcoe-segmentation", "feature-tx-gre-csum-segmentation",
    "feature-tx-gre-segmentation", "feature-tx-gso-list", "feature-tx-gso-partial", "feature-tx-gso-robust",
    "feature-tx-ipxip4-segmentation", "feature-tx-ipxip6-segmentation", "feature-tx-nocache-copy", "feature-tx-scatter-gather",
    "feature-tx-scatter-gather-fraglist", "feature-tx-sctp-segmentation", "feature-tx-tcp6-segmentation",
    "feature-tx-tcp-ecn-segmentation", "feature-tx-tcp-mangleid-segmentation", "feature-tx-tcp-segmentation",
    "feature-tx-tunnel-remcsum-segmentation", "feature-tx-udp-segmentation", "feature-tx-udp_tnl-csum-segmentation",
    "feature-tx-udp_tnl-segmentation", "feature-tx-vlan-stag-hw-insert", "fec-mode", "pause-autoneg", "pause-rx", "pause-tx",
    "ring-rx", "ring-rx-jumbo", "ring-rx-mini", "ring-tx"];

pub const ROUTE_ATTRIBUTES: &[&str] = &["advmss", "cwnd", "from", "initcwnd", "initrwnd", "lock-advmss", "lock-cwnd", "lock-initcwnd",
    "lock-initrwnd", "lock-mtu", "lock-window", "mtu", "onlink", "quickack", "rto_min", "scope", "src", "table", "tos", "type",
    "weight", "window"];

fn caption(text: &str) -> gtk::Label {
    let widget = label(text, Some("muted"));
    widget.set_wrap(true);
    widget.set_ellipsize(gtk::pango::EllipsizeMode::None);
    widget.set_max_width_chars(64);
    widget
}

fn entry(text: &str, placeholder: Option<&str>) -> gtk::Entry {
    let widget = gtk::Entry::new();
    widget.set_hexpand(true);
    widget.set_text(text);
    if let Some(placeholder) = placeholder {
        widget.set_placeholder_text(Some(placeholder));
    }
    widget
}

fn text_of(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string()
}

// --- pure value helpers (testable without a display) ---

/// What a control currently holds (`_raw()`).
#[derive(Clone, Debug, PartialEq)]
pub enum Raw {
    None,
    Bool(bool),
    Text(String),
    Rows(Vec<Vec<String>>),
}

/// A parsed value ready for `set_property`.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Bool(bool),
    Int(i64),
    Float(f64),
    Strv(Vec<String>),
    Hash(Vec<(String, String)>),
    Bytes(Option<Vec<u8>>),
    Str(Option<String>),
}

const INT_TYPES: &[&str] = &["gint", "guint", "gint64", "guint64", "glong", "gulong", "gchar", "guchar"];

/// Python `int(text)` / `int(text, 0)` for `0x` prefixes.
pub fn parse_int(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let lower = digits.to_lowercase();
    let value = if let Some(hex) = lower.strip_prefix("0x") {
        i64::from_str_radix(&hex.replace('_', ""), 16).ok()?
    } else {
        if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__")
            || !digits.chars().all(|c| c.is_ascii_digit() || c == '_') {
            return None;
        }
        digits.replace('_', "").parse::<i64>().ok()?
    };
    Some(if negative { -value } else { value })
}

/// `PropertyField._parse(raw)`.
pub fn parse_value(spec: &PropertySpec, numeric: bool, raw: &Raw) -> Result<Parsed, ()> {
    let text = || match raw { Raw::Text(t) => Ok(t.clone()), _ => Err(()) };
    if spec.value_type == "gboolean" {
        return Ok(Parsed::Bool(match raw { Raw::Bool(b) => *b, Raw::Text(t) => !t.is_empty(), _ => false }));
    }
    if numeric || INT_TYPES.contains(&spec.value_type.as_str()) {
        let value = parse_int(&text()?).ok_or(())?;
        if spec.minimum.is_some_and(|m| (value as f64) < m) || spec.maximum.is_some_and(|m| (value as f64) > m) {
            return Err(());
        }
        return Ok(Parsed::Int(value));
    }
    match spec.value_type.as_str() {
        "gfloat" | "gdouble" => {
            let value: f64 = text()?.trim().parse().map_err(|_| ())?;
            if !value.is_finite() {
                return Err(());
            }
            Ok(Parsed::Float(value))
        }
        "GStrv" => Ok(Parsed::Strv(text()?.replace('\n', ",").split(',').map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()).collect())),
        "GHashTable" => {
            let Raw::Rows(rows) = raw else { return Err(()) };
            let mut values: Vec<(String, String)> = Vec::new();
            for row in rows {
                let (key, value) = (row.first().cloned().unwrap_or_default(), row.get(1).cloned().unwrap_or_default());
                if key.is_empty() || values.iter().any(|(k, _)| *k == key) {
                    return Err(());
                }
                values.push((key, value));
            }
            Ok(Parsed::Hash(values))
        }
        "GBytes" => {
            let raw = text()?;
            if spec.name == "ssid" || spec.secret {
                return Ok(Parsed::Bytes(Some(raw.into_bytes())));
            }
            if spec.name.contains("cert") || spec.name.contains("private-key") {
                let text = raw.trim();
                if text.is_empty() {
                    return Ok(Parsed::Bytes(None));
                }
                let mut value = if text.starts_with("file://") || text.starts_with("pkcs11:") { text.to_owned() } else { format!("file://{text}") }
                    .into_bytes();
                value.push(0);
                return Ok(Parsed::Bytes(Some(value)));
            }
            let raw: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
            if raw.len() % 2 != 0 || !raw.is_ascii() {
                return Err(());
            }
            let bytes: Result<Vec<u8>, _> = (0..raw.len()).step_by(2).map(|i| u8::from_str_radix(&raw[i..i + 2], 16)).collect();
            Ok(Parsed::Bytes(Some(bytes.map_err(|_| ())?)))
        }
        "gchararray" => {
            let raw = text()?;
            Ok(Parsed::Str(if raw.is_empty() { None } else { Some(raw) }))
        }
        _ => Err(()),
    }
}

/// `setting.set_property(name, value)`.
pub fn commit(setting: &Setting, name: &str, value: &Parsed) -> Result<(), String> {
    match value {
        Parsed::Bool(b) => setting.set(name, &b.to_value()),
        Parsed::Int(i) => setting.set_int_checked(name, *i),
        Parsed::Float(f) => setting.set(name, &f.to_value()),
        Parsed::Strv(items) => {
            let strv: glib::StrV = items.iter().map(|s| glib::GString::from(s.as_str())).collect();
            setting.set(name, &strv.to_value())
        }
        Parsed::Hash(items) => setting.set_hash(name, items),
        Parsed::Bytes(bytes) => setting.set(name, &bytes.as_ref().map(|b| glib::Bytes::from(b.as_slice())).to_value()),
        Parsed::Str(text) => setting.set(name, &text.to_value()),
    }
}

/// `str(value)` of a property for an entry (`None` → "").
pub fn value_text(value: &glib::Value, value_type: &str) -> String {
    if value_type == "GStrv" {
        return value.get::<Vec<String>>().unwrap_or_default().join(", ");
    }
    if let Some(number) = libnm::value_as_i64(value) {
        return number.to_string();
    }
    if let Ok(Some(text)) = value.get::<Option<String>>() {
        return text;
    }
    if let Ok(f) = value.get::<f64>() {
        return tessel_core::pyfmt::float_repr(f);
    }
    if let Ok(f) = value.get::<f32>() {
        return tessel_core::pyfmt::float_repr(f as f64);
    }
    if let Ok(b) = value.get::<bool>() {
        return if b { "True".into() } else { "False".into() };
    }
    String::new()
}

/// The entry text of a GBytes property (certificate paths only; secrets blank).
pub fn bytes_text(name: &str, secret: bool, raw: &[u8]) -> String {
    if name == "ssid" {
        return String::from_utf8_lossy(raw).into_owned();
    }
    if name.contains("cert") || name.contains("private-key") {
        if raw.starts_with(b"file://") || raw.starts_with(b"pkcs11:") {
            let mut end = raw.len();
            while end > 0 && raw[end - 1] == 0 {
                end -= 1;
            }
            let text = String::from_utf8_lossy(&raw[..end]).into_owned();
            return text.strip_prefix("file://").map(String::from).unwrap_or(text);
        }
        return String::new();
    }
    if secret { String::new() } else { raw.iter().map(|b| format!("{b:02x}")).collect() }
}

// --- RowsEditor ---

pub struct Row {
    pub widget: gtk::Box,
    pub entries: Vec<gtk::Entry>,
    pub original: Option<usize>,
    pub details: Option<Rc<IpAttributes>>,
}

type RowDetails = Box<dyn Fn(Option<usize>) -> Rc<IpAttributes>>;

/// A small editable table; original row objects survive unchanged edits.
pub struct RowsEditor {
    pub root: gtk::Box,
    table: gtk::Box,
    pub add_button: gtk::Button,
    columns: Vec<String>,
    secret_columns: Vec<usize>,
    pub rows: RefCell<Vec<Rc<Row>>>,
    pub dirty: Cell<bool>,
    row_details: RefCell<Option<RowDetails>>,
    on_changed: RefCell<Option<Rc<dyn Fn()>>>,
    weak: RefCell<Weak<RowsEditor>>,
}

impl RowsEditor {
    pub fn new(columns: &[String], rows: &[Vec<String>], add_label: &str, secret_columns: &[usize]) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let table = gtk::Box::new(gtk::Orientation::Vertical, 7);
        root.append(&table);
        let add_button = gtk::Button::with_label(add_label);
        add_button.set_halign(gtk::Align::Start);
        root.append(&add_button);
        let editor = Rc::new(Self { root, table, add_button: add_button.clone(), columns: columns.to_vec(), secret_columns: secret_columns.to_vec(),
                                    rows: RefCell::default(), dirty: Cell::new(false), row_details: RefCell::new(None),
                                    on_changed: RefCell::new(None), weak: RefCell::new(Weak::new()) });
        editor.weak.replace(Rc::downgrade(&editor));
        let w = Rc::downgrade(&editor);
        add_button.connect_clicked(move |_| if let Some(e) = w.upgrade() { e.add_row(None, None, true); });
        for values in rows {
            editor.add_row(Some(values), None, false);
        }
        editor
    }

    pub fn set_row_details(&self, details: impl Fn(Option<usize>) -> Rc<IpAttributes> + 'static) {
        self.row_details.replace(Some(Box::new(details)));
    }

    pub fn set_on_changed(&self, changed: Rc<dyn Fn()>) {
        self.on_changed.replace(Some(changed));
    }

    pub fn add_row(&self, values: Option<&[String]>, original: Option<usize>, changed: bool) -> Rc<Row> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        widget.append(&line);
        let mut entries = Vec::new();
        for (index, column) in self.columns.iter().enumerate() {
            let value = values.and_then(|v| v.get(index)).cloned().unwrap_or_default();
            let e = entry(&value, Some(column));
            e.set_width_chars(if self.columns.len() > 2 { 10 } else { 20 });
            accessible_label(&e, column);
            if self.secret_columns.contains(&index) {
                e.set_visibility(false);
            }
            let w = self.weak.borrow().clone();
            e.connect_changed(move |_| if let Some(r) = w.upgrade() { r.changed() });
            line.append(&expand(&e, &line));
            entries.push(e);
        }
        let remove = style(&gtk::Button::new(), &["icon-button"]);
        remove.set_child(Some(&icon("list-remove-symbolic", 16)));
        remove.set_tooltip_text(Some(&tr!("Remove row")));
        line.append(&remove);
        let details = self.row_details.borrow().as_ref().map(|f| f(original));
        if let Some(details) = &details {
            widget.append(&details.expander);
        }
        let row = Rc::new(Row { widget: widget.clone(), entries, original, details });
        let (w, r) = (self.weak.borrow().clone(), Rc::downgrade(&row));
        remove.connect_clicked(move |_| if let (Some(e), Some(r)) = (w.upgrade(), r.upgrade()) { e.remove_row(&r) });
        self.rows.borrow_mut().push(row.clone());
        self.table.append(&widget);
        widget.set_visible(true);
        if changed {
            self.changed();
        }
        row
    }

    pub fn remove_row(&self, row: &Rc<Row>) {
        self.rows.borrow_mut().retain(|r| !Rc::ptr_eq(r, row));
        self.table.remove(&row.widget);
        self.changed();
    }

    pub fn changed(&self) {
        self.dirty.set(true);
        let callback = self.on_changed.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    pub fn values(&self) -> Vec<(Vec<String>, Option<usize>)> {
        self.rows.borrow().iter().map(|r| (r.entries.iter().map(|e| e.text().trim().to_owned()).collect(), r.original)).collect()
    }
}

// --- PropertyField / SettingPage ---

pub enum Control {
    Switch(gtk::Switch),
    #[allow(deprecated)]
    Combo(gtk::ComboBoxText),
    Rows(Rc<RowsEditor>),
    Text(gtk::TextView),
    Entry(gtk::Entry),
    Caption(gtk::Label),
}

impl Control {
    pub fn widget(&self) -> gtk::Widget {
        match self {
            Control::Switch(w) => w.clone().upcast(),
            Control::Combo(w) => w.clone().upcast(),
            Control::Rows(w) => w.root.clone().upcast(),
            Control::Text(w) => w.clone().upcast(),
            Control::Entry(w) => w.clone().upcast(),
            Control::Caption(w) => w.clone().upcast(),
        }
    }

    /// Set an entry's text (combo with entry, entry or text view).
    #[allow(deprecated)]
    pub fn set_text(&self, text: &str) {
        match self {
            Control::Entry(e) => e.set_text(text),
            Control::Text(v) => v.buffer().set_text(text),
            Control::Combo(c) => {
                c.set_active_id(None);
                if let Some(e) = c.child().and_then(|c| c.downcast::<gtk::Entry>().ok()) {
                    e.set_text(text);
                }
            }
            _ => {}
        }
    }

    pub fn entry(&self) -> Option<gtk::Entry> {
        match self {
            Control::Entry(e) => Some(e.clone()),
            _ => None,
        }
    }
}

pub struct PropertyField {
    pub setting: Setting,
    pub spec: PropertySpec,
    pub name: String,
    pub title: String,
    pub secret: bool,
    pub readonly: bool,
    numeric: bool,
    pub array: Option<crate::ui::structured::Kind>,
    pub original: RefCell<Option<glib::Value>>,
    pub boxed: gtk::Box,
    pub control: Control,
    pub baseline: RefCell<Raw>,
}

impl PropertyField {
    #[allow(deprecated)]
    pub fn new(setting: &Setting, spec: &PropertySpec) -> Rc<Self> {
        let name = spec.name.clone();
        let title = field_name(&name);
        let secret = spec.secret;
        let original = if secret { None } else { setting.get(&name) };
        let boxed = gtk::Box::new(gtk::Orientation::Vertical, 5);
        boxed.append(&label(&title, Some("device-name")));
        let native_type = setting.find_property(&name).map(|p| p.value_type()).unwrap_or(glib::Type::INVALID);
        let is_enum = native_type.is_a(glib::Type::ENUM);
        let numeric = is_enum || native_type.is_a(glib::Type::FLAGS);
        let mut readonly = false;
        let array = if spec.value_type == "GPtrArray" || spec.value_type == "GArray" {
            crate::ui::structured::kind(&name, &spec.dbus_type)
        } else {
            None
        };
        let choices = if is_enum {
            Some(libnm::enum_values(native_type).into_iter()
                 .map(|(v, nick)| (v.to_string(), crate::nm::capitalize(&nick.replace('-', " ")))).collect())
        } else {
            string_choices(&setting.name(), &name)
        };
        let control = if spec.value_type == "gboolean" {
            let w = gtk::Switch::new();
            w.set_halign(gtk::Align::Start);
            w.set_active(original.as_ref().and_then(|v| v.get::<bool>().ok()).unwrap_or(false));
            Control::Switch(w)
        } else if let Some(choices) = choices {
            let w = gtk::ComboBoxText::with_entry();
            for (key, text) in &choices {
                w.append(Some(key), text);
            }
            let current = original.as_ref().map(|v| value_text(v, &spec.value_type)).unwrap_or_default();
            w.set_active_id(Some(&current));
            if w.active_id().is_none() {
                if let Some(e) = w.child().and_then(|c| c.downcast::<gtk::Entry>().ok()) {
                    e.set_text(&current);
                }
            }
            Control::Combo(w)
        } else if spec.value_type == "GHashTable" {
            let rows: Vec<Vec<String>> = if secret { Vec::new() } else { setting.get_hash(&name).into_iter().map(|(k, v)| vec![k, v]).collect() };
            let secret_columns: &[usize] = if secret { &[1] } else { &[] };
            Control::Rows(RowsEditor::new(&[tr!("Option"), tr!("Value")], &rows, &tr!("Add option"), secret_columns))
        } else if spec.value_type == "GPtrArray" || spec.value_type == "GArray" {
            match array {
                Some(kind) => {
                    let view = gtk::TextView::new();
                    view.set_wrap_mode(gtk::WrapMode::WordChar);
                    view.buffer().set_text(&crate::ui::structured::read(setting, &name, kind).join("\n"));
                    view.set_size_request(-1, 90);
                    Control::Text(view)
                }
                None => {
                    readonly = true;
                    Control::Caption(caption(&tr!("Existing structured options are preserved unchanged.")))
                }
            }
        } else if spec.value_type == "GBytes" {
            let raw = if secret { Vec::new() } else { setting.get_bytes(&name).unwrap_or_default() };
            let text = bytes_text(&name, secret, &raw);
            let placeholder = if name == "ssid" { tr!("Network name") }
                else if name.contains("cert") || name.contains("private-key") { tr!("Certificate/key file path or PKCS#11 URI") }
                else if secret { tr!("New value") } else { tr!("Hexadecimal bytes") };
            Control::Entry(entry(&text, Some(&placeholder)))
        } else {
            let text = if secret { String::new() } else { original.as_ref().map(|v| value_text(v, &spec.value_type)).unwrap_or_default() };
            if (name == "config" || name == "pac-script") && !secret {
                let view = gtk::TextView::new();
                view.set_monospace(true);
                view.set_wrap_mode(gtk::WrapMode::WordChar);
                view.set_size_request(-1, 120);
                view.buffer().set_text(&text);
                Control::Text(view)
            } else {
                let placeholder = if secret { Some(tr!("Leave unchanged")) } else { None };
                let e = entry(&text, placeholder.as_deref());
                if secret {
                    e.set_visibility(false);
                    e.set_input_purpose(gtk::InputPurpose::Password);
                    e.set_icon_from_icon_name(gtk::EntryIconPosition::Secondary, Some("view-reveal-symbolic"));
                    e.connect_icon_release(|e, _| e.set_visibility(!gtk::prelude::EntryExt::is_visible(e)));
                }
                Control::Entry(e)
            }
        };
        boxed.append(&control.widget());
        if !spec.description.is_empty() {
            control.widget().set_tooltip_text(Some(&spec.description));
        }
        let field = Rc::new(Self { setting: setting.clone(), spec: spec.clone(), name, title, secret, readonly, numeric, array,
                                   original: RefCell::new(original), boxed, control, baseline: RefCell::new(Raw::None) });
        field.baseline.replace(field.raw());
        field
    }

    #[allow(deprecated)]
    pub fn raw(&self) -> Raw {
        if self.readonly {
            return Raw::None;
        }
        match &self.control {
            Control::Switch(w) => Raw::Bool(w.is_active()),
            Control::Combo(w) => Raw::Text(w.active_id().map(|s| s.to_string()).unwrap_or_else(|| {
                w.child().and_then(|c| c.downcast::<gtk::Entry>().ok()).map(|e| e.text().to_string()).unwrap_or_default()
            })),
            Control::Rows(r) => Raw::Rows(r.values().into_iter().map(|(v, _)| v).collect()),
            Control::Text(v) => Raw::Text(text_of(v)),
            Control::Entry(e) => Raw::Text(e.text().to_string()),
            Control::Caption(_) => Raw::None,
        }
    }

    /// `prepare()`: `None` when unchanged (or an untouched secret).
    pub fn prepare(self: &Rc<Self>) -> FormResult<Option<Operation>> {
        let raw = self.raw();
        if raw == Raw::None || raw == *self.baseline.borrow() || (self.secret && matches!(&raw, Raw::Text(t) if t.is_empty())) {
            return Ok(None);
        }
        if let Some(kind) = self.array {
            let Raw::Text(text) = &raw else { return Ok(None) };
            let lines: Vec<String> = text.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
            crate::ui::structured::validate(kind, &lines).map_err(|_| format!("{}: enter a valid value.", self.title))?;
            let field = self.clone();
            return Ok(Some(Box::new(move || {
                crate::ui::structured::write(&field.setting, &field.name, kind, &lines)
                    .map_err(|_| format!("{}: this value is not supported.", field.title))?;
                field.baseline.replace(raw);
                Ok(())
            })));
        }
        let value = parse_value(&self.spec, self.numeric, &raw).map_err(|_| format!("{}: enter a valid value.", self.title))?;
        let field = self.clone();
        Ok(Some(Box::new(move || {
            commit(&field.setting, &field.name, &value).map_err(|_| format!("{}: this value is not supported.", field.title))?;
            field.baseline.replace(raw);
            Ok(())
        })))
    }
}

pub struct SettingPage {
    pub root: gtk::Box,
    pub setting: Setting,
    pub fields: BTreeMap<String, Rc<PropertyField>>,
}

impl SettingPage {
    pub fn new(setting: &Setting, exclude: &[&str], basic_keys: Option<&[&str]>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let name = setting.name();
        let mut excluded: Vec<&str> = exclude.to_vec();
        excluded.extend(locked(&name));
        excluded.push("name");
        let common = basic_keys.unwrap_or_else(|| basic(&name));
        let mut specs: BTreeMap<String, PropertySpec> = setting_schema(setting).into_iter()
            .filter(|s| !excluded.contains(&s.name.as_str())).map(|s| (s.name.clone(), s)).collect();
        let mut fields = BTreeMap::new();
        for key in common {
            if let Some(spec) = specs.remove(*key) {
                let field = PropertyField::new(setting, &spec);
                root.append(&field.boxed);
                fields.insert(spec.name.clone(), field);
            }
        }
        if !specs.is_empty() {
            let advanced = gtk::Box::new(gtk::Orientation::Vertical, 14);
            advanced.set_margin_top(12);
            for (key, spec) in &specs {
                let field = PropertyField::new(setting, spec);
                advanced.append(&field.boxed);
                fields.insert(key.clone(), field);
            }
            let expander = gtk::Expander::new(Some(&tr!("Advanced options")));
            expander.set_child(Some(&advanced));
            root.append(&expander);
        }
        Rc::new(Self { root, setting: setting.clone(), fields })
    }

    pub fn field(&self, name: &str) -> Option<Rc<PropertyField>> {
        self.fields.get(name).cloned()
    }
}

impl Page for SettingPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn prepare(&self) -> FormResult<Vec<Operation>> {
        let mut out = Vec::new();
        for field in self.fields.values() {
            if let Some(op) = field.prepare()? {
                out.push(op);
            }
        }
        Ok(out)
    }
}

// --- IP pages ---

/// `IPAttributes.render(value)`.
pub fn render_attribute(value: &Variant) -> String {
    match value.type_().as_str() {
        "b" => if value.get::<bool>().unwrap_or(false) { "true".into() } else { "false".into() },
        "s" => value.str().unwrap_or_default().to_owned(),
        "y" => value.get::<u8>().map(|v| v.to_string()).unwrap_or_default(),
        "u" => value.get::<u32>().map(|v| v.to_string()).unwrap_or_default(),
        "i" => value.get::<i32>().map(|v| v.to_string()).unwrap_or_default(),
        "t" => value.get::<u64>().map(|v| v.to_string()).unwrap_or_default(),
        "x" => value.get::<i64>().map(|v| v.to_string()).unwrap_or_default(),
        _ => value.print(false).to_string(),
    }
}

/// Infer and parse one additional address/route option (`IPAttributes.prepare`).
pub fn parse_attribute(name: &str, text: &str, previous: Option<&Variant>, route: bool, family: i32) -> Result<Variant, ()> {
    let signature = match previous {
        Some(v) => v.type_().as_str().to_owned(),
        None if name.starts_with("lock-") || name == "onlink" || name == "quickack" => "b".into(),
        None if name == "scope" || name == "tos" => "y".into(),
        None if route && ROUTE_ATTRIBUTES.contains(&name) && !["from", "src", "type"].contains(&name) => "u".into(),
        None => "s".into(),
    };
    let int = || parse_int(text).ok_or(());
    let value = match signature.as_str() {
        "b" => {
            let lower = text.to_lowercase();
            if !["true", "false", "yes", "no", "1", "0"].contains(&lower.as_str()) {
                return Err(());
            }
            ["true", "yes", "1"].contains(&lower.as_str()).to_variant()
        }
        "y" => u8::try_from(int()?).map_err(|_| ())?.to_variant(),
        "u" => u32::try_from(int()?).map_err(|_| ())?.to_variant(),
        "i" => i32::try_from(int()?).map_err(|_| ())?.to_variant(),
        "t" => u64::try_from(int()?).map_err(|_| ())?.to_variant(),
        "x" => int()?.to_variant(),
        "s" => text.to_variant(),
        other => Variant::parse(Some(glib::VariantTy::new(other).map_err(|_| ())?), text).map_err(|_| ())?,
    };
    if route && !libnm::route_attribute_valid(name, &value, family) {
        return Err(());
    }
    Ok(value)
}

pub struct IpAttributes {
    pub expander: gtk::Expander,
    pub table: Rc<RowsEditor>,
    original: BTreeMap<String, Variant>,
    route: bool,
    family: i32,
}

impl IpAttributes {
    pub fn new(original: Vec<(String, Variant)>, family: i32, route: bool, changed: Option<Rc<dyn Fn()>>) -> Rc<Self> {
        let expander = gtk::Expander::new(Some(&if route { tr!("Additional route options") } else { tr!("Address options") }));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(8);
        let examples = if route {
            "Common options: table (routing table number), onlink (true/false), src (preferred source address), mtu.".to_owned()
        } else {
            tr!("Use label to give this address an interface label. Existing options retain their original types.")
        };
        content.append(&caption(&examples));
        let table = RowsEditor::new(&[tr!("Option"), tr!("Value")], &[], &tr!("Add option"), &[]);
        if let Some(changed) = changed {
            table.set_on_changed(changed);
        }
        let original: BTreeMap<String, Variant> = original.into_iter().collect();
        for (name, value) in &original {
            table.add_row(Some(&[name.clone(), render_attribute(value)]), None, false);
        }
        content.append(&table.root);
        expander.set_child(Some(&content));
        Rc::new(Self { expander, table, original, route, family })
    }

    pub fn prepare(&self) -> FormResult<Vec<(String, Variant)>> {
        let mut result: Vec<(String, Variant)> = Vec::new();
        for (fields, _) in self.table.values() {
            let (name, text) = (fields[0].clone(), fields[1].clone());
            if name.is_empty() || result.iter().any(|(n, _)| *n == name) {
                return Err(tr!("Additional IP options need unique, nonempty option names."));
            }
            let previous = self.original.get(&name);
            if let Some(previous) = previous {
                if text == render_attribute(previous) {
                    result.push((name, previous.clone()));
                    continue;
                }
            }
            let value = parse_attribute(&name, &text, previous, self.route, self.family)
                .map_err(|_| tr!("Additional IP option {value1}: enter a valid value.", value1 = name.as_str()))?;
            result.push((name, value));
        }
        Ok(result)
    }
}

/// `ipaddress.ip_network(text, strict=False)` → (network address, prefix).
pub fn parse_network(text: &str) -> Option<(IpAddr, u32)> {
    let (address, prefix) = match text.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (text, None),
    };
    let address: IpAddr = address.trim().parse().ok()?;
    let max = if address.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(p) => p.trim().parse::<u32>().ok().filter(|p| *p <= max)?,
        None => max,
    };
    let network = match address {
        IpAddr::V4(v4) => {
            let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
            IpAddr::V4((u32::from(v4) & mask).into())
        }
        IpAddr::V6(v6) => {
            let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
            IpAddr::V6((u128::from(v6) & mask).into())
        }
    };
    Some((network, prefix))
}

/// Validate one address row (version and prefix range).
pub fn parse_address(address: &str, prefix: &str, version: u8) -> Option<(String, u32)> {
    let address: IpAddr = address.parse().ok()?;
    let prefix: u32 = prefix.trim().parse().ok()?;
    let (is_version, max) = if version == 4 { (address.is_ipv4(), 32) } else { (address.is_ipv6(), 128) };
    (is_version && prefix <= max).then(|| (address.to_string(), prefix))
}

/// Validate one route row `(destination, gateway, metric)`.
pub fn parse_route(destination: &str, gateway: &str, metric: &str, version: u8) -> Option<(String, u32, Option<String>, i64)> {
    let (network, prefix) = parse_network(destination)?;
    if (version == 4) != network.is_ipv4() {
        return None;
    }
    let gateway = if gateway.is_empty() {
        None
    } else {
        let g: IpAddr = gateway.parse().ok()?;
        if g.is_ipv4() != (version == 4) {
            return None;
        }
        Some(g.to_string())
    };
    let metric = if metric.is_empty() { -1 } else { metric.trim().parse::<i64>().ok()? };
    if metric < -1 {
        return None;
    }
    Some((network.to_string(), prefix, gateway, metric))
}

pub struct IpPage {
    pub root: gtk::Box,
    pub setting: Setting,
    family: i32,
    version: u8,
    #[allow(deprecated)]
    pub method: gtk::ComboBoxText,
    method_before: Rc<RefCell<String>>,
    pub addresses: Rc<RowsEditor>,
    address_originals: Vec<IpAddress>,
    pub options: Rc<SettingPage>,
    pub routes: Rc<RowsEditor>,
    route_originals: Vec<IpRoute>,
    pub rules: gtk::TextView,
    rules_before: Rc<RefCell<String>>,
}

impl IpPage {
    #[allow(deprecated)]
    pub fn new(setting: &Setting) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let version: u8 = if setting.name() == "ipv4" { 4 } else { 6 };
        let family = if version == 4 { libnm::AF_INET } else { libnm::AF_INET6 };
        let method = gtk::ComboBoxText::new();
        let mut methods = vec![("auto".to_owned(), if version == 4 { tr!("Automatic (DHCP)") } else { tr!("Automatic") }),
                               ("manual".into(), tr!("Manual")), ("link-local".into(), tr!("Link-local only")),
                               ("shared".into(), tr!("Share with other computers")), ("disabled".into(), tr!("Disabled"))];
        if version == 6 {
            methods.push(("dhcp".into(), tr!("Automatic, DHCP only")));
            methods.push(("ignore".into(), tr!("Ignore")));
        }
        let current = setting.ip_method().filter(|m| !m.is_empty()).unwrap_or_else(|| "auto".into());
        if !methods.iter().any(|(k, _)| *k == current) {
            methods.push((current.clone(), current.clone()));
        }
        for (key, text) in &methods {
            method.append(Some(key), text);
        }
        method.set_active_id(Some(&current));
        root.append(&label(&tr!("Address assignment"), Some("device-name")));
        root.append(&method);
        root.append(&label(&tr!("Addresses"), Some("section-title")));
        root.append(&caption(&tr!("Enter an address and its prefix length, such as 24 for a typical IPv4 network.")));
        let address_originals = setting.addresses();
        let address_attrs: Vec<Vec<(String, Variant)>> = address_originals.iter().map(|a| a.attributes()).collect();
        let addresses = RowsEditor::new(&[tr!("Address"), tr!("Prefix")], &[], &tr!("Add address"), &[]);
        {
            let w = Rc::downgrade(&addresses);
            addresses.set_row_details(move |original| {
                let w2 = w.clone();
                let changed: Rc<dyn Fn()> = Rc::new(move || if let Some(a) = w2.upgrade() { a.changed() });
                IpAttributes::new(original.map(|i| address_attrs[i].clone()).unwrap_or_default(), family, false, Some(changed))
            });
        }
        for (index, address) in address_originals.iter().enumerate() {
            addresses.add_row(Some(&[address.address(), address.prefix().to_string()]), Some(index), false);
        }
        root.append(&addresses.root);
        let options = SettingPage::new(setting, &["method", "addresses", "routes"],
                                       Some(&["gateway", "dns", "dns-search", "ignore-auto-dns", "never-default"]));
        root.append(&options.root);
        root.append(&label(&tr!("Routes"), Some("section-title")));
        root.append(&caption(&tr!("Use 0.0.0.0/0 or ::/0 for a default route. Leave gateway empty for a direct route and priority empty for automatic.")));
        let route_originals = setting.routes();
        let route_attrs: Vec<Vec<(String, Variant)>> = route_originals.iter().map(|r| r.attributes()).collect();
        let routes = RowsEditor::new(&["Destination/prefix".into(), tr!("Gateway"), tr!("Priority")], &[], &tr!("Add route"), &[]);
        {
            let w = Rc::downgrade(&routes);
            routes.set_row_details(move |original| {
                let w2 = w.clone();
                let changed: Rc<dyn Fn()> = Rc::new(move || if let Some(a) = w2.upgrade() { a.changed() });
                IpAttributes::new(original.map(|i| route_attrs[i].clone()).unwrap_or_default(), family, true, Some(changed))
            });
        }
        for (index, route) in route_originals.iter().enumerate() {
            let metric = if route.metric() == -1 { String::new() } else { route.metric().to_string() };
            routes.add_row(Some(&[format!("{}/{}", route.dest(), route.prefix()), route.next_hop().unwrap_or_default(), metric]),
                           Some(index), false);
        }
        root.append(&routes.root);
        let rules = gtk::TextView::new();
        rules.set_wrap_mode(gtk::WrapMode::WordChar);
        rules.set_size_request(-1, 80);
        let rules_before = setting.routing_rule_strings().join("\n");
        rules.buffer().set_text(&rules_before);
        let rules_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        rules_box.set_margin_top(8);
        rules_box.append(&caption(&tr!("One policy routing rule per line, for example: priority 100 from 192.0.2.0/24 table 100")));
        rules_box.append(&rules);
        let expander = gtk::Expander::new(Some(&tr!("Policy routing")));
        expander.set_child(Some(&rules_box));
        root.append(&expander);
        Rc::new(Self { root, setting: setting.clone(), family, version, method, method_before: Rc::new(RefCell::new(current)), addresses,
                       address_originals, options, routes, route_originals, rules, rules_before: Rc::new(RefCell::new(rules_before)) })
    }

    fn prepare_attributes(row: &Row) -> FormResult<Vec<(String, Variant)>> {
        match &row.details {
            Some(details) => details.prepare(),
            None => Ok(Vec::new()),
        }
    }
}

impl Page for IpPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    #[allow(deprecated)]
    fn prepare(&self) -> FormResult<Vec<Operation>> {
        let mut operations = self.options.prepare()?;
        let method = self.method.active_id().map(|s| s.to_string()).unwrap_or_default();
        if method != *self.method_before.borrow() {
            let (setting, before) = (self.setting.clone(), self.method_before.clone());
            operations.push(Box::new(move || {
                setting.set_str("method", Some(&method));
                before.replace(method);
                Ok(())
            }));
        }
        let v = self.version;
        if self.addresses.dirty.get() {
            let mut values = Vec::new();
            let rows = self.addresses.rows.borrow().clone();
            for (index, row) in rows.iter().enumerate() {
                let fields: Vec<String> = row.entries.iter().map(|e| e.text().trim().to_owned()).collect();
                let bad = || format!("IPv{v} address {}: enter a valid address and prefix.", index + 1);
                let (address, prefix) = parse_address(&fields[0], &fields[1], v).ok_or_else(bad)?;
                let value = match row.original {
                    Some(i) => self.address_originals[i].duplicate(),
                    None => IpAddress::new(self.family, &address, prefix).map_err(|_| bad())?,
                };
                value.set_address(&address);
                value.set_prefix(prefix);
                let attributes = Self::prepare_attributes(row).map_err(|_| bad())?;
                for (name, _) in value.attributes() {
                    if !attributes.iter().any(|(n, _)| *n == name) {
                        value.set_attribute(&name, None);
                    }
                }
                for (name, attribute) in &attributes {
                    value.set_attribute(name, Some(attribute));
                }
                values.push(value);
            }
            let (setting, editor) = (self.setting.clone(), self.addresses.clone());
            operations.push(Box::new(move || {
                setting.set_addresses(&values);
                editor.dirty.set(false);
                Ok(())
            }));
        }
        if self.routes.dirty.get() {
            let mut routes = Vec::new();
            let rows = self.routes.rows.borrow().clone();
            for (index, row) in rows.iter().enumerate() {
                let fields: Vec<String> = row.entries.iter().map(|e| e.text().trim().to_owned()).collect();
                let bad = || format!("IPv{v} route {}: check the destination, gateway and priority.", index + 1);
                let (dest, prefix, gateway, metric) = parse_route(&fields[0], &fields[1], &fields[2], v).ok_or_else(bad)?;
                let route = match row.original {
                    Some(i) => self.route_originals[i].duplicate(),
                    None => IpRoute::new(self.family, &dest, prefix, gateway.as_deref(), metric).map_err(|_| bad())?,
                };
                route.set_dest(&dest);
                route.set_prefix(prefix);
                route.set_next_hop(gateway.as_deref());
                route.set_metric(metric);
                let attributes = Self::prepare_attributes(row)?;
                for (name, _) in route.attributes() {
                    if !attributes.iter().any(|(n, _)| *n == name) {
                        route.set_attribute(&name, None);
                    }
                }
                for (name, attribute) in &attributes {
                    route.set_attribute(name, Some(attribute));
                }
                routes.push(route);
            }
            let (setting, editor) = (self.setting.clone(), self.routes.clone());
            operations.push(Box::new(move || {
                setting.set_routes(&routes);
                editor.dirty.set(false);
                Ok(())
            }));
        }
        let text = text_of(&self.rules);
        if text != *self.rules_before.borrow() {
            let lines: Vec<String> = text.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect();
            let message = format!("IPv{v}: enter valid policy routing rules for this address family.");
            // Validate on a scratch setting; the draft changes only on commit.
            let scratch = Setting::new(&self.setting.name()).ok_or_else(|| message.clone())?;
            scratch.set_routing_rules(&lines, self.family).map_err(|_| message.clone())?;
            let (setting, family, before) = (self.setting.clone(), self.family, self.rules_before.clone());
            operations.push(Box::new(move || {
                setting.set_routing_rules(&lines, family).map_err(|_| message)?;
                before.replace(text);
                Ok(())
            }));
        }
        Ok(operations)
    }
}

// --- WireGuard ---

/// Values of one peer editor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeerValues {
    pub public_key: String,
    pub endpoint: String,
    pub allowed_ips: String,
    pub keepalive: String,
    pub preshared_key: String,
}

/// `WireGuardPeerEditor.prepare()`: the original peer's unshown secret survives.
pub fn prepare_peer(original: Option<&Peer>, values: &PeerValues) -> FormResult<Peer> {
    let mut peer = match original { Some(p) => p.duplicate(true), None => Peer::new() };
    if !peer.set_public_key(&values.public_key) {
        return Err(tr!("WireGuard peer: enter a valid public key."));
    }
    if !values.endpoint.is_empty() {
        if !peer.set_endpoint(&values.endpoint) {
            return Err(tr!("WireGuard peer: enter a valid endpoint, including its port."));
        }
    } else if let Some(original) = original.filter(|o| !o.endpoint().is_empty()) {
        // Rebuild for an explicit clear, preserving the opaque peer credential.
        peer = Peer::new();
        peer.set_public_key(&values.public_key);
        if let Some(secret) = original.preshared_key() {
            peer.set_preshared_key(&secret);
        }
        peer.set_preshared_key_flags(original.preshared_key_flags());
    }
    peer.clear_allowed_ips();
    for network in values.allowed_ips.replace('\n', ",").split(',') {
        let network = network.trim();
        if !network.is_empty() && !peer.append_allowed_ip(network) {
            return Err(tr!("WireGuard peer: enter valid allowed network addresses and prefixes."));
        }
    }
    let keepalive = if values.keepalive.is_empty() { "0" } else { values.keepalive.as_str() };
    let interval = parse_int(keepalive).filter(|i| (0..=65535).contains(i))
        .ok_or_else(|| tr!("WireGuard peer: keepalive must be from 0 to 65535 seconds."))?;
    peer.set_persistent_keepalive(interval as u16);
    if !values.preshared_key.is_empty() && !peer.set_preshared_key(&values.preshared_key) {
        return Err(tr!("WireGuard peer: enter a valid preshared key."));
    }
    Ok(peer)
}

pub fn peer_values(peer: Option<&Peer>) -> PeerValues {
    match peer {
        Some(p) => PeerValues { public_key: p.public_key(), endpoint: p.endpoint(), allowed_ips: p.allowed_ips().join(", "),
                                keepalive: p.persistent_keepalive().to_string(), preshared_key: String::new() },
        None => PeerValues { keepalive: "0".into(), ..Default::default() },
    }
}

pub struct WireGuardPeerEditor {
    pub root: gtk::Box,
    pub original: RefCell<Option<Peer>>,
    pub entries: BTreeMap<&'static str, gtk::Entry>,
    before: RefCell<PeerValues>,
}

type RemovePeer = Rc<dyn Fn(&Rc<WireGuardPeerEditor>)>;

impl WireGuardPeerEditor {
    pub fn new(peer: Option<Peer>, remove: RemovePeer) -> Rc<Self> {
        let root = style(&gtk::Box::new(gtk::Orientation::Vertical, 9), &["card"]);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 9);
        margins(&content, 14);
        root.append(&content);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        heading.append(&expand(&label(&tr!("Peer"), Some("section-title")), &heading));
        let button = style(&gtk::Button::new(), &["icon-button"]);
        button.set_child(Some(&icon("user-trash-symbolic", 16)));
        button.set_tooltip_text(Some(&tr!("Remove peer")));
        heading.append(&button);
        content.append(&heading);
        let values = peer_values(peer.as_ref());
        let mut entries = BTreeMap::new();
        for (name, title, value) in [("public-key", tr!("Public key"), values.public_key.clone()),
                                     ("endpoint", tr!("Endpoint (host:port)"), values.endpoint.clone()),
                                     ("allowed-ips", tr!("Allowed networks"), values.allowed_ips.clone()),
                                     ("persistent-keepalive", tr!("Keepalive interval (seconds)"), values.keepalive.clone()),
                                     ("preshared-key", tr!("Preshared key"), String::new())] {
            content.append(&label(&title, Some("device-name")));
            let placeholder = if name == "preshared-key" { Some(tr!("Leave unchanged")) } else { None };
            let e = entry(&value, placeholder.as_deref());
            if name == "preshared-key" {
                e.set_visibility(false);
                e.set_input_purpose(gtk::InputPurpose::Password);
            }
            content.append(&e);
            entries.insert(name, e);
        }
        let editor = Rc::new(Self { root, original: RefCell::new(peer), entries, before: RefCell::new(values) });
        let w = Rc::downgrade(&editor);
        button.connect_clicked(move |_| if let Some(e) = w.upgrade() { remove(&e) });
        editor
    }

    pub fn current(&self) -> PeerValues {
        let text = |k: &str| self.entries[k].text().trim().to_owned();
        PeerValues { public_key: text("public-key"), endpoint: text("endpoint"), allowed_ips: text("allowed-ips"),
                     keepalive: text("persistent-keepalive"), preshared_key: text("preshared-key") }
    }

    pub fn changed(&self) -> bool {
        self.original.borrow().is_none() || self.current() != *self.before.borrow()
    }

    pub fn prepare(&self) -> FormResult<Peer> {
        prepare_peer(self.original.borrow().as_ref(), &self.current())
    }
}

pub struct WireGuardPage {
    pub root: gtk::Box,
    pub setting: Setting,
    pub options: Rc<SettingPage>,
    peers_box: gtk::Box,
    pub peers: RefCell<Vec<Rc<WireGuardPeerEditor>>>,
    structure_changed: Cell<bool>,
    weak: RefCell<Weak<WireGuardPage>>,
}

impl WireGuardPage {
    pub fn new(setting: &Setting) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let options = SettingPage::new(setting, &[], None);
        root.append(&options.root);
        root.append(&label(&tr!("Peers"), Some("section-title")));
        let peers_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.append(&peers_box);
        let page = Rc::new(Self { root: root.clone(), setting: setting.clone(), options, peers_box, peers: RefCell::default(),
                                  structure_changed: Cell::new(false), weak: RefCell::new(Weak::new()) });
        page.weak.replace(Rc::downgrade(&page));
        for peer in setting.peers() {
            page.add_peer(Some(peer), false);
        }
        let add = gtk::Button::with_label(&tr!("Add peer"));
        add.set_halign(gtk::Align::Start);
        let w = Rc::downgrade(&page);
        add.connect_clicked(move |_| if let Some(p) = w.upgrade() { p.add_peer(None, true); });
        root.append(&add);
        page
    }

    pub fn add_peer(&self, peer: Option<Peer>, changed: bool) -> Rc<WireGuardPeerEditor> {
        let w = self.weak.borrow().clone();
        let editor = WireGuardPeerEditor::new(peer, Rc::new(move |e| if let Some(p) = w.upgrade() { p.remove_peer(e) }));
        self.peers.borrow_mut().push(editor.clone());
        self.peers_box.append(&editor.root);
        editor.root.set_visible(true);
        if changed {
            self.structure_changed.set(true);
        }
        editor
    }

    pub fn remove_peer(&self, editor: &Rc<WireGuardPeerEditor>) {
        self.peers.borrow_mut().retain(|e| !Rc::ptr_eq(e, editor));
        self.peers_box.remove(&editor.root);
        self.structure_changed.set(true);
    }
}

impl Page for WireGuardPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn prepare(&self) -> FormResult<Vec<Operation>> {
        self.options.prepare()
    }
    fn apply(&self) -> FormResult<()> {
        let operations = self.options.prepare()?;
        let editors = self.peers.borrow().clone();
        let changed = self.structure_changed.get() || editors.iter().any(|e| e.changed());
        let peers = if changed { Some(editors.iter().map(|e| e.prepare()).collect::<FormResult<Vec<Peer>>>()?) } else { None };
        for op in operations {
            op()?;
        }
        if let Some(peers) = peers {
            self.setting.clear_peers();
            for peer in &peers {
                self.setting.append_peer(peer);
            }
            for (editor, peer) in editors.iter().zip(peers) {
                editor.original.replace(Some(peer));
                editor.before.replace(editor.current());
            }
            self.structure_changed.set(false);
        }
        Ok(())
    }
}

// --- ethtool ---

/// Parse one ethtool option (`None` = back to the default).
pub fn parse_ethtool(signature: &str, raw: &str) -> Result<Option<Variant>, ()> {
    if raw.is_empty() || raw == "default" {
        return Ok(None);
    }
    Ok(Some(match signature {
        "b" => (raw == "true").to_variant(),
        "u" => u32::try_from(parse_int(raw).ok_or(())?).map_err(|_| ())?.to_variant(),
        "i" => i32::try_from(parse_int(raw).ok_or(())?).map_err(|_| ())?.to_variant(),
        "t" => u64::try_from(parse_int(raw).ok_or(())?).map_err(|_| ())?.to_variant(),
        "x" => parse_int(raw).ok_or(())?.to_variant(),
        "s" => raw.to_variant(),
        other => Variant::parse(Some(glib::VariantTy::new(other).map_err(|_| ())?), raw).map_err(|_| ())?,
    }))
}

pub fn ethtool_group(group: &str) -> String {
    match group {
        "feature" => tr!("Offload features"), "coalesce" => tr!("Interrupt coalescing"), "ring" => tr!("Ring buffers"),
        "channels" => tr!("Channels"), "pause" => tr!("Pause frames"), "eee" => tr!("Energy-efficient Ethernet"),
        "fec" => tr!("Forward error correction"), _ => tr!("Other link options"),
    }
}

pub enum EthtoolControl {
    #[allow(deprecated)]
    Choice(gtk::ComboBoxText),
    Entry(gtk::Entry),
}

pub struct EthtoolPage {
    pub root: gtk::Box,
    pub setting: Setting,
    pub options: BTreeMap<String, EthtoolControl>,
    types: BTreeMap<String, String>,
    before: RefCell<BTreeMap<String, String>>,
}

impl EthtoolPage {
    #[allow(deprecated)]
    pub fn new(setting: &Setting) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
        root.append(&caption(&tr!("Default leaves the device's existing behavior unchanged. These settings take effect when the connection is activated.")));
        let mut names: std::collections::BTreeSet<String> = ETHTOOL_NAMES.iter().map(|s| s.to_string()).collect();
        names.extend(setting.option_names());
        let mut groups: BTreeMap<String, gtk::Box> = BTreeMap::new();
        let mut options = BTreeMap::new();
        let mut types = BTreeMap::new();
        for name in &names {
            let value = setting.option(name);
            let signature = value.as_ref().map(|v| v.type_().as_str().to_owned())
                .unwrap_or_else(|| if libnm::ethtool_option_is_bool(name) { "b".into() } else { "u".into() });
            let group = name.split('-').next().unwrap_or("").to_owned();
            let section = groups.entry(group.clone()).or_insert_with(|| {
                let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
                section.set_margin_top(8);
                let expander = gtk::Expander::new(Some(&ethtool_group(&group)));
                expander.set_child(Some(&section));
                root.append(&expander);
                section
            }).clone();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let text = name.strip_prefix(&format!("{group}-")).unwrap_or(name).replace('-', " ");
            let caption = label(&text, Some("device-name"));
            caption.set_tooltip_text(Some(name));
            row.append(&expand(&caption, &row));
            let control = if signature == "b" {
                let w = gtk::ComboBoxText::new();
                for (key, title) in [("default", tr!("Default")), ("true", tr!("Enabled")), ("false", tr!("Disabled"))] {
                    w.append(Some(key), &title);
                }
                let active = match &value { None => "default", Some(v) if v.get::<bool>() == Some(true) => "true", _ => "false" };
                w.set_active_id(Some(active));
                row.append(&w);
                EthtoolControl::Choice(w)
            } else {
                let text = value.as_ref().map(render_attribute).unwrap_or_default();
                let w = entry(&text, Some(&tr!("Default")));
                w.set_width_chars(14);
                row.append(&w);
                EthtoolControl::Entry(w)
            };
            section.append(&row);
            options.insert(name.clone(), control);
            types.insert(name.clone(), signature);
        }
        let page = Rc::new(Self { root, setting: setting.clone(), options, types, before: RefCell::default() });
        let before: BTreeMap<String, String> = page.options.keys().map(|k| (k.clone(), page.raw(k))).collect();
        page.before.replace(before);
        page
    }

    #[allow(deprecated)]
    pub fn raw(&self, name: &str) -> String {
        match &self.options[name] {
            EthtoolControl::Choice(c) => c.active_id().map(|s| s.to_string()).unwrap_or_default(),
            EthtoolControl::Entry(e) => e.text().trim().to_owned(),
        }
    }
}

impl Page for EthtoolPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn prepare(&self) -> FormResult<Vec<Operation>> {
        Ok(Vec::new())
    }
    fn apply(&self) -> FormResult<()> {
        let mut updates: Vec<(String, Option<Variant>)> = Vec::new();
        for name in self.options.keys() {
            let raw = self.raw(name);
            if self.before.borrow().get(name) == Some(&raw) {
                continue;
            }
            let error = || tr!("Link option {value1}: enter a supported value.", value1 = name.as_str());
            let value = parse_ethtool(&self.types[name], &raw).map_err(|_| error())?;
            if let Some(value) = &value {
                // Native validation uses a temporary setting, not the NIC.
                let validation = Setting::new("ethtool").ok_or_else(error)?;
                validation.set_option(name, Some(value));
                validation.verify().map_err(|_| error())?;
            }
            updates.push((name.clone(), value));
        }
        for (name, value) in updates {
            self.setting.set_option(&name, value.as_ref());
            let raw = self.raw(&name);
            self.before.borrow_mut().insert(name, raw);
        }
        Ok(())
    }
}

// --- 802.1X ---

pub const TLS_METHODS: &[&str] = &["tls", "peap", "ttls", "fast", "teap"];

pub fn needs_certificate(eap: &str) -> bool {
    eap.to_lowercase().replace('\n', ",").split(',').any(|m| TLS_METHODS.contains(&m.trim()))
}

pub struct AuthenticationPage {
    pub root: gtk::Box,
    pub setting: Setting,
    pub options: Rc<SettingPage>,
    #[allow(deprecated)]
    pub ca_mode: gtk::ComboBoxText,
    pub skip_ack: gtk::CheckButton,
}

impl AuthenticationPage {
    #[allow(deprecated)]
    pub fn new(setting: &Setting, new: bool) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let options = SettingPage::new(setting, &["system-ca-certs"], None);
        root.append(&label(&tr!("Verify the authentication server"), Some("section-title")));
        let ca_mode = gtk::ComboBoxText::new();
        for (key, title) in [("system", tr!("Use system trusted certificates")), ("custom", tr!("Use a specific CA certificate")),
                             ("skip", "Do not verify the server certificate".to_owned())] {
            ca_mode.append(Some(key), &title);
        }
        let custom = setting.get_bytes("ca-cert").is_some() || setting.get_string("ca-path").is_some_and(|p| !p.is_empty());
        let initial = if setting.get_bool("system-ca-certs") { "system" } else if custom { "custom" } else if new { "system" } else { "skip" };
        ca_mode.set_active_id(Some(initial));
        root.append(&ca_mode);
        let skip_ack = gtk::CheckButton::new();
        let acknowledgment = gtk::Label::builder().label(tr!("I understand that the server's certificate will not be verified"))
            .xalign(0.0).wrap(true).build();
        skip_ack.set_child(Some(&acknowledgment));
        skip_ack.set_visible(false);
        root.append(&skip_ack);
        root.append(&options.root);
        let page = Rc::new(Self { root, setting: setting.clone(), options, ca_mode: ca_mode.clone(), skip_ack });
        if let Some(ca) = page.options.field("ca-cert") {
            ca.boxed.set_visible(false);
        }
        let w = Rc::downgrade(&page);
        ca_mode.connect_changed(move |_| if let Some(p) = w.upgrade() { p.sync_mode() });
        if let Some(e) = page.options.field("eap").and_then(|f| f.control.entry()) {
            let w = Rc::downgrade(&page);
            e.connect_changed(move |_| if let Some(p) = w.upgrade() { p.sync_mode() });
        }
        page.sync_mode();
        page
    }

    fn needs_certificate(&self) -> bool {
        match self.options.field("eap").map(|f| f.raw()) {
            Some(Raw::Text(t)) => needs_certificate(&t),
            _ => false,
        }
    }

    #[allow(deprecated)]
    fn mode(&self) -> String {
        self.ca_mode.active_id().map(|s| s.to_string()).unwrap_or_default()
    }

    fn sync_mode(&self) {
        let mode = self.mode();
        self.skip_ack.set_visible(mode == "skip" && self.needs_certificate());
        if let Some(field) = self.options.field("ca-cert") {
            field.boxed.set_visible(mode == "custom");
        }
    }
}

impl Page for AuthenticationPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn prepare(&self) -> FormResult<Vec<Operation>> {
        let mode = self.mode();
        let needs = self.needs_certificate();
        if mode == "skip" && needs && !self.skip_ack.is_active() {
            return Err(tr!("Choose a trusted CA certificate or explicitly acknowledge skipping server certificate verification."));
        }
        let ca = self.options.field("ca-cert");
        let ca_path = self.options.field("ca-path");
        let nonempty = |f: &Rc<PropertyField>| matches!(f.raw(), Raw::Text(t) if !t.trim().is_empty());
        let has_ca = ca.as_ref().is_some_and(|f| nonempty(f) || (f.raw() == *f.baseline.borrow() && self.setting.get_bytes("ca-cert").is_some()))
            || ca_path.as_ref().is_some_and(nonempty);
        if mode == "custom" && needs && !has_ca {
            return Err(tr!("Choose the CA certificate or CA directory for this network."));
        }
        let mut operations = self.options.prepare()?;
        if needs {
            let setting = self.setting.clone();
            operations.push(Box::new(move || {
                setting.set_bool("system-ca-certs", mode == "system");
                if mode == "skip" {
                    setting.set_bytes("ca-cert", None);
                    setting.set_str("ca-path", None);
                    // The draft and controls agree after an explicit skip.
                    for field in [ca, ca_path].into_iter().flatten() {
                        field.control.set_text("");
                        field.baseline.replace(Raw::Text(String::new()));
                        field.original.replace(None);
                    }
                }
                Ok(())
            }));
        }
        Ok(operations)
    }
}

// --- Wi-Fi ---

pub fn wireless_mode(security: Option<&Setting>) -> String {
    let Some(setting) = security else { return "open".into() };
    let method = setting.get_string("key-mgmt").filter(|m| !m.is_empty()).unwrap_or_else(|| "open".into());
    match method.as_str() {
        "none" => "wep".into(),
        "ieee8021x" => if setting.get_string("auth-alg").as_deref() == Some("leap") { "leap".into() } else { "dynamic-wep".into() },
        _ => method,
    }
}

pub struct WirelessForm {
    pub root: gtk::Box,
    connection: Connection,
    pub network: Rc<SettingPage>,
    original_security: Option<Setting>,
    original_auth: Option<Setting>,
    original_mode: String,
    before_mode: RefCell<String>,
    #[allow(deprecated)]
    pub security_combo: gtk::ComboBoxText,
    security_box: gtk::Box,
    pub security_setting: RefCell<Option<Setting>>,
    pub security_form: RefCell<Option<Rc<SettingPage>>>,
    pub authentication_form: RefCell<Option<Rc<AuthenticationPage>>>,
}

impl WirelessForm {
    #[allow(deprecated)]
    pub fn new(connection: &Connection, setting: &Setting) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let network = SettingPage::new(setting, &[], Some(&["ssid"]));
        root.append(&network.root);
        let original_security = connection.setting("802-11-wireless-security");
        let original_auth = connection.setting("802-1x");
        let original_mode = wireless_mode(original_security.as_ref());
        root.append(&label(&tr!("Security"), Some("section-title")));
        let security_combo = gtk::ComboBoxText::new();
        let choices = [("open", tr!("Open (no password)")), ("wpa-psk", "WPA/WPA2 Personal".to_owned()), ("sae", tr!("WPA3 Personal")),
                       ("wpa-eap", "WPA/WPA2 Enterprise".to_owned()), ("wpa-eap-suite-b-192", tr!("WPA3 Enterprise, 192-bit")),
                       ("owe", tr!("Enhanced Open")), ("wep", "WEP".to_owned()), ("dynamic-wep", tr!("Dynamic WEP (802.1X)")),
                       ("leap", "LEAP".to_owned())];
        for (key, title) in &choices {
            security_combo.append(Some(key), title);
        }
        if !choices.iter().any(|(k, _)| *k == original_mode) {
            security_combo.append(Some(&original_mode), &original_mode);
        }
        security_combo.set_active_id(Some(&original_mode));
        root.append(&security_combo);
        let security_box = gtk::Box::new(gtk::Orientation::Vertical, 14);
        root.append(&security_box);
        let form = Rc::new(Self { root, connection: connection.clone(), network, original_security, original_auth,
                                  original_mode: original_mode.clone(), before_mode: RefCell::new(original_mode),
                                  security_combo: security_combo.clone(), security_box, security_setting: RefCell::new(None),
                                  security_form: RefCell::new(None), authentication_form: RefCell::new(None) });
        let w = Rc::downgrade(&form);
        security_combo.connect_changed(move |_| if let Some(f) = w.upgrade() { f.security_changed() });
        form.security_changed();
        form
    }

    #[allow(deprecated)]
    fn mode(&self) -> String {
        self.security_combo.active_id().map(|s| s.to_string()).unwrap_or_default()
    }

    fn security_changed(&self) {
        remove_children(&self.security_box);
        let mode = self.mode();
        self.security_form.replace(None);
        self.authentication_form.replace(None);
        if mode == "open" {
            self.security_box.append(&caption(&tr!("This network does not require authentication.")));
            self.security_box.set_visible(true);
            return;
        }
        let setting = match (&self.original_security, mode == self.original_mode) {
            (Some(original), true) => original.clone(),
            _ => {
                let setting = Setting::new("802-11-wireless-security").unwrap();
                let method = match mode.as_str() { "wep" => "none", "dynamic-wep" | "leap" => "ieee8021x", other => other };
                setting.set_str("key-mgmt", Some(method));
                if mode == "leap" {
                    setting.set_str("auth-alg", Some("leap"));
                }
                if ["sae", "owe", "wpa-eap-suite-b-192"].contains(&mode.as_str()) {
                    setting.set_int("pmf", 3);
                }
                setting
            }
        };
        let basic_keys: &[&str] = match mode.as_str() {
            "wpa-psk" | "sae" => &["psk"],
            "wep" => &["wep-key0", "wep-key1", "wep-key2", "wep-key3", "wep-tx-keyidx", "wep-key-type", "auth-alg"],
            "leap" => &["leap-username", "leap-password"],
            _ => &[],
        };
        let form = SettingPage::new(&setting, &["key-mgmt"], Some(basic_keys));
        self.security_box.append(&form.root);
        self.security_form.replace(Some(form));
        self.security_setting.replace(Some(setting));
        if ["wpa-eap", "wpa-eap-suite-b-192", "dynamic-wep"].contains(&mode.as_str()) {
            let authentication = self.original_auth.clone().unwrap_or_else(|| {
                let auth = Setting::new("802-1x").unwrap();
                auth.set_strv("eap", &[if mode == "wpa-eap-suite-b-192" { "tls" } else { "peap" }.to_owned()]);
                if mode != "wpa-eap-suite-b-192" {
                    auth.set_str("phase2-auth", Some("mschapv2"));
                }
                auth
            });
            self.security_box.append(&label(&tr!("Enterprise authentication"), Some("section-title")));
            let page = AuthenticationPage::new(&authentication, self.original_auth.is_none());
            self.security_box.append(&page.root);
            self.authentication_form.replace(Some(page));
        }
        self.security_box.set_visible(true);
    }

    fn put(&self, setting: &Setting) {
        if self.connection.setting(&setting.name()).as_ref() != Some(setting) {
            self.connection.add_setting(setting);
        }
    }
}

impl Page for WirelessForm {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn prepare(&self) -> FormResult<Vec<Operation>> {
        let mut operations = self.network.prepare()?;
        if let Some(form) = self.security_form.borrow().as_ref() {
            operations.extend(form.prepare()?);
        }
        if let Some(form) = self.authentication_form.borrow().as_ref() {
            operations.extend(form.prepare()?);
        }
        Ok(operations)
    }
    fn apply(&self) -> FormResult<()> {
        let mode = self.mode();
        for op in self.prepare()? {
            op()?;
        }
        if mode == "open" {
            if *self.before_mode.borrow() != "open" {
                self.connection.remove_setting("802-11-wireless-security");
                self.connection.remove_setting("802-1x");
            }
        } else {
            if let Some(setting) = self.security_setting.borrow().as_ref() {
                self.put(setting);
            }
            if let Some(form) = self.authentication_form.borrow().as_ref() {
                self.put(&form.setting);
            } else if ["wpa-eap", "wpa-eap-suite-b-192", "dynamic-wep"].contains(&self.before_mode.borrow().as_str()) {
                self.connection.remove_setting("802-1x");
            }
        }
        self.before_mode.replace(mode);
        Ok(())
    }
}

// --- page assembly ---

/// Optional settings that can be added to this draft, sorted by title.
pub fn available_optional_settings(connection: &Connection) -> Vec<(String, String)> {
    let wireless = connection.setting("802-11-wireless").is_some();
    let mut names: Vec<(String, String)> = OPTIONAL.iter()
        .filter(|n| libnm::setting_exists(n))
        .filter(|n| !(wireless && (**n == "802-1x" || **n == "802-11-wireless-security")))
        .filter(|n| connection.setting(n).is_none())
        .map(|n| (n.to_string(), title(n))).collect();
    names.sort_by_key(|(_, t)| casefold(t));
    names
}

pub fn add_optional_setting(connection: &Connection, name: &str) -> FormResult<Setting> {
    let unavailable = || tr!("That option is already present or is unavailable for this connection.");
    if !available_optional_settings(connection).iter().any(|(n, _)| n == name) {
        return Err(unavailable());
    }
    let setting = Setting::new(name).ok_or_else(unavailable)?;
    if name == "ipv4" || name == "ipv6" {
        setting.set_str("method", Some("auto"));
    }
    connection.add_setting(&setting);
    Ok(setting)
}

/// The order in which `build_pages` shows settings.
pub fn page_order(connection: &Connection) -> Vec<String> {
    let names: Vec<String> = connection.settings().iter().map(|s| s.name()).collect();
    let primary = connection.connection_type();
    let order = ["connection".to_owned(), primary, "802-1x".into(), "ipv4".into(), "ipv6".into(), "proxy".into()];
    let mut sorted = names.clone();
    sorted.sort_by_key(|n| (order.iter().position(|o| o == n).unwrap_or(order.len()), title(n)));
    let wireless = names.iter().any(|n| n == "802-11-wireless");
    sorted.into_iter().filter(|n| n != "vpn" && !(wireless && (n == "802-11-wireless-security" || n == "802-1x"))).collect()
}

/// `(setting name, title, page)` for every setting of the draft except VPN.
pub fn build_pages(connection: &Connection) -> Vec<(String, String, Rc<dyn Page>)> {
    page_order(connection).into_iter().filter_map(|name| {
        let setting = connection.setting(&name)?;
        let page: Rc<dyn Page> = match name.as_str() {
            "ipv4" | "ipv6" => IpPage::new(&setting),
            "wireguard" => WireGuardPage::new(&setting),
            "802-11-wireless" => WirelessForm::new(connection, &setting),
            "ethtool" => EthtoolPage::new(&setting),
            "802-1x" => AuthenticationPage::new(&setting, false),
            _ => SettingPage::new(&setting, &[], None),
        };
        let t = title(&name);
        Some((name, t, page))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::new_draft;

    fn spec(name: &str, value_type: &str) -> PropertySpec {
        PropertySpec { name: name.into(), label: String::new(), description: String::new(), value_type: value_type.into(),
                       dbus_type: String::new(), secret: false, required: false, minimum: None, maximum: None }
    }

    #[test]
    fn property_values_parse_like_python() {
        let t = |s: &str| Raw::Text(s.into());
        assert_eq!(parse_value(&spec("mtu", "guint"), false, &t(" 1500 ")), Ok(Parsed::Int(1500)));
        assert_eq!(parse_value(&spec("mtu", "guint"), false, &t("0x10")), Ok(Parsed::Int(16)));
        let mut bounded = spec("mtu", "guint");
        bounded.maximum = Some(100.0);
        assert!(parse_value(&bounded, false, &t("101")).is_err());
        assert!(parse_value(&spec("x", "gdouble"), false, &t("inf")).is_err());
        assert_eq!(parse_value(&spec("dns", "GStrv"), false, &t("1.1.1.1, 2.2.2.2\n3.3.3.3,")),
                   Ok(Parsed::Strv(vec!["1.1.1.1".into(), "2.2.2.2".into(), "3.3.3.3".into()])));
        let duplicate = Raw::Rows(vec![vec!["a".into(), "1".into()], vec!["a".into(), "2".into()]]);
        assert!(parse_value(&spec("options", "GHashTable"), false, &duplicate).is_err());
        assert!(parse_value(&spec("options", "GHashTable"), false, &Raw::Rows(vec![vec!["".into(), "1".into()]])).is_err());
        assert_eq!(parse_value(&spec("ca-cert", "GBytes"), false, &t("/etc/ca.pem")), Ok(Parsed::Bytes(Some(b"file:///etc/ca.pem\0".to_vec()))));
        assert_eq!(parse_value(&spec("ca-cert", "GBytes"), false, &t("  ")), Ok(Parsed::Bytes(None)));
        assert_eq!(parse_value(&spec("ssid", "GBytes"), false, &t("Home")), Ok(Parsed::Bytes(Some(b"Home".to_vec()))));
        assert_eq!(parse_value(&spec("mac-address", "GBytes"), false, &t("0aff")), Ok(Parsed::Bytes(Some(vec![10, 255]))));
        assert_eq!(parse_value(&spec("id", "gchararray"), false, &t("")), Ok(Parsed::Str(None)));
        assert_eq!(bytes_text("ca-cert", false, b"file:///tmp/ca.pem\0"), "/tmp/ca.pem");
        assert_eq!(bytes_text("ca-cert", false, b"\x30\x03"), "", "embedded data is never shown");
        assert_eq!(bytes_text("psk", true, b"secret"), "");
    }

    #[test]
    fn commit_and_display_use_native_property_types() {
        let wireless = Setting::new("802-11-wireless").unwrap();
        commit(&wireless, "mtu", &Parsed::Int(1400)).unwrap();
        assert_eq!(value_text(&wireless.get("mtu").unwrap(), "guint"), "1400");
        commit(&wireless, "hidden", &Parsed::Bool(true)).unwrap();
        assert!(wireless.get_bool("hidden"));
        let security = Setting::new("802-11-wireless-security").unwrap();
        commit(&security, "psk-flags", &Parsed::Int(1)).unwrap();
        assert_eq!(security.get_u32("psk-flags"), 1);
        let bond = Setting::new("bond").unwrap();
        commit(&bond, "options", &Parsed::Hash(vec![("mode".into(), "active-backup".into())])).unwrap();
        assert!(bond.get_hash("options").contains(&("mode".to_string(), "active-backup".to_string())));
        let ipv4 = Setting::new("ipv4").unwrap();
        commit(&ipv4, "dns", &Parsed::Strv(vec!["192.0.2.53".into(), "192.0.2.54".into()])).unwrap();
        assert_eq!(value_text(&ipv4.get("dns").unwrap(), "GStrv"), "192.0.2.53, 192.0.2.54");
        let proxy = Setting::new("proxy").unwrap();
        commit(&proxy, "pac-script", &Parsed::Str(Some("function FindProxyForURL(url, host) {\n  return 'DIRECT';\n}".into()))).unwrap();
        assert!(proxy.get_string("pac-script").unwrap().contains('\n'));
    }

    #[test]
    fn ip_rows_validate_family_prefix_and_metric() {
        assert_eq!(parse_address("192.0.2.10", "24", 4), Some(("192.0.2.10".into(), 24)));
        assert!(parse_address("192.0.2.10", "33", 4).is_none());
        assert!(parse_address("invalid address", "24", 4).is_none());
        assert!(parse_address("2001:db8::1", "64", 4).is_none());
        assert_eq!(parse_route("198.51.100.7/24", "192.0.2.1", "20", 4),
                   Some(("198.51.100.0".into(), 24, Some("192.0.2.1".into()), 20)));
        assert_eq!(parse_route("0.0.0.0/0", "", "", 4), Some(("0.0.0.0".into(), 0, None, -1)));
        assert!(parse_route("198.51.100.0/24", "2001:db8::1", "", 4).is_none());
        assert!(parse_route("198.51.100.0/24", "", "-2", 4).is_none());
    }

    #[test]
    fn ip_attributes_keep_types_and_infer_new_ones() {
        let table = parse_attribute("table", "100", None, true, libnm::AF_INET).unwrap();
        assert_eq!((table.type_().as_str(), table.get::<u32>()), ("u", Some(100)));
        let onlink = parse_attribute("onlink", "true", None, true, libnm::AF_INET).unwrap();
        assert_eq!(onlink.get::<bool>(), Some(true));
        assert!(parse_attribute("onlink", "maybe", None, true, libnm::AF_INET).is_err());
        let label_value = parse_attribute("label", "eth0:1", None, false, libnm::AF_INET).unwrap();
        assert_eq!(label_value.str(), Some("eth0:1"));
        assert_eq!(render_attribute(&100u32.to_variant()), "100");
        assert_eq!(render_attribute(&true.to_variant()), "true");
        assert_eq!(render_attribute(&"x".to_variant()), "x");
        let route = IpRoute::new(libnm::AF_INET, "198.51.100.0", 24, Some("192.0.2.1"), 10).unwrap();
        route.set_attribute("table", Some(&table));
        assert_eq!(route.attributes()[0].1.get::<u32>(), Some(100));
    }

    #[test]
    fn routing_rules_validate_on_a_scratch_setting() {
        let ipv4 = Setting::new("ipv4").unwrap();
        assert!(ipv4.set_routing_rules(&["priority 100 table 100".into()], libnm::AF_INET).is_ok());
        assert_eq!(ipv4.routing_rule_strings().len(), 1);
        assert!(ipv4.set_routing_rules(&["nonsense".into()], libnm::AF_INET).is_err());
        assert_eq!(ipv4.routing_rule_strings().len(), 1, "a failed parse never partially replaces rules");
    }

    #[test]
    fn wireguard_peer_edit_preserves_unshown_secret_and_allows_empty_endpoint() {
        let b64 = |range: std::ops::Range<u8>| tessel_core::base64::encode(&range.collect::<Vec<u8>>());
        let (public, secret) = (b64(0..32), b64(32..64));
        let peer = Peer::new();
        assert!(peer.set_public_key(&public) && peer.set_preshared_key(&secret) && peer.set_endpoint("example.test:51820"));
        assert!(peer.append_allowed_ip("10.0.0.0/8"));
        let mut values = peer_values(Some(&peer));
        assert_eq!(values.preshared_key, "");
        values.keepalive = "25".into();
        values.endpoint = String::new();
        let updated = prepare_peer(Some(&peer), &values).unwrap();
        assert_eq!(updated.endpoint(), "");
        assert_eq!(updated.preshared_key(), Some(secret));
        assert_eq!(updated.persistent_keepalive(), 25);
        assert_eq!(updated.allowed_ips(), ["10.0.0.0/8"]);
        values.keepalive = "70000".into();
        assert!(prepare_peer(Some(&peer), &values).err().unwrap().contains("keepalive"));
        values.keepalive = "0".into();
        values.public_key = "not a key".into();
        assert!(prepare_peer(Some(&peer), &values).is_err());
    }

    #[test]
    fn ethtool_values_use_native_types_and_reject_negative_rings() {
        assert_eq!(parse_ethtool("b", "false").unwrap().unwrap().get::<bool>(), Some(false));
        assert_eq!(parse_ethtool("u", "512").unwrap().unwrap().get::<u32>(), Some(512));
        assert!(parse_ethtool("u", "-1").is_err());
        assert_eq!(parse_ethtool("b", "default"), Ok(None));
        assert!(libnm::ethtool_option_is_bool("feature-rx"));
        assert!(!libnm::ethtool_option_is_bool("ring-rx"));
        let setting = Setting::new("ethtool").unwrap();
        setting.set_option("ring-rx", Some(&512u32.to_variant()));
        setting.verify().unwrap();
        assert_eq!(setting.option("ring-rx").unwrap().get::<u32>(), Some(512));
    }

    #[test]
    fn optional_settings_and_page_order() {
        let draft = new_draft("802-3-ethernet", None).unwrap();
        let options = available_optional_settings(&draft.connection);
        assert!(options.iter().any(|(n, _)| n == "ethtool"));
        assert!(!options.iter().any(|(n, _)| n == "ipv4"), "already present");
        add_optional_setting(&draft.connection, "proxy").unwrap();
        assert!(add_optional_setting(&draft.connection, "proxy").is_err());
        assert_eq!(page_order(&draft.connection)[..2], ["connection".to_string(), "802-3-ethernet".to_string()]);
        let wifi = new_draft("802-11-wireless", None).unwrap();
        assert!(!available_optional_settings(&wifi.connection).iter().any(|(n, _)| n == "802-1x"));
        assert!(needs_certificate("PEAP"));
        assert!(!needs_certificate("md5"));
        assert_eq!(wireless_mode(None), "open");
    }
}
