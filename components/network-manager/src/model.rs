//! `network_model.py`: plain data shown by the popup and settings window,
//! `vpn_entries` (joining VPN profiles, activations and tunnel devices) and
//! `friendly_error`.

use std::collections::{BTreeMap, HashSet};
use suite_core::bus::BusError;
use suite_core::pyjson::casefold;

/// Insertion-ordered maps keyed like the Python dicts (D-Bus path / uuid / network id).
pub type Map<V> = indexmap_lite::IndexMap<V>;

/// A tiny insertion-ordered string map (Python dict semantics) without a new dependency.
pub mod indexmap_lite {
    #[derive(Clone, Debug, PartialEq)]
    pub struct IndexMap<V> {
        keys: Vec<String>,
        values: std::collections::HashMap<String, V>,
    }

    impl<V> Default for IndexMap<V> {
        fn default() -> Self {
            Self { keys: Vec::new(), values: std::collections::HashMap::new() }
        }
    }

    impl<V> IndexMap<V> {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn insert(&mut self, key: impl Into<String>, value: V) {
            let key = key.into();
            if !self.values.contains_key(&key) {
                self.keys.push(key.clone());
            }
            self.values.insert(key, value);
        }
        pub fn get(&self, key: &str) -> Option<&V> {
            self.values.get(key)
        }
        pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
            self.values.get_mut(key)
        }
        pub fn contains_key(&self, key: &str) -> bool {
            self.values.contains_key(key)
        }
        pub fn remove(&mut self, key: &str) -> Option<V> {
            let value = self.values.remove(key)?;
            self.keys.retain(|k| k != key);
            Some(value)
        }
        pub fn len(&self) -> usize {
            self.keys.len()
        }
        pub fn is_empty(&self) -> bool {
            self.keys.is_empty()
        }
        pub fn keys(&self) -> impl Iterator<Item = &String> {
            self.keys.iter()
        }
        pub fn values(&self) -> impl Iterator<Item = &V> {
            self.keys.iter().map(move |k| &self.values[k])
        }
        pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
            self.keys.iter().map(move |k| (k, &self.values[k]))
        }
        pub fn clear(&mut self) {
            self.keys.clear();
            self.values.clear();
        }
    }

    impl<V> FromIterator<(String, V)> for IndexMap<V> {
        fn from_iter<T: IntoIterator<Item = (String, V)>>(iter: T) -> Self {
            let mut map = Self::new();
            for (k, v) in iter {
                map.insert(k, v);
            }
            map
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub interface: String,
    pub name: String,
    pub kind: String,
    pub state: u32,
    pub state_label: String,
    pub managed: bool,
    pub active_ids: Vec<String>,
    pub details: Vec<(String, String)>,
    pub carrier: bool,
}

impl Device {
    pub fn new(id: &str, interface: &str, name: &str, kind: &str) -> Self {
        Self { id: id.into(), interface: interface.into(), name: name.into(), kind: kind.into(), state: 0,
               state_label: "Unknown".into(), managed: true, active_ids: Vec::new(), details: Vec::new(), carrier: false }
    }

    /// Administrative availability (NetworkManager's per-device Managed flag).
    pub fn enabled(&self) -> bool {
        self.managed
    }

    pub fn detail(&self, key: &str) -> Option<&str> {
        self.details.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WifiNetwork {
    pub id: String,
    pub device_id: String,
    pub ssid: Vec<u8>,
    pub label: String,
    pub security: String,
    pub strength: i64,
    pub ap_path: String,
    pub profile_ids: Vec<String>,
    pub active: bool,
    pub frequency: u32,
    pub bssid: String,
}

impl WifiNetwork {
    pub fn new(id: &str, device_id: &str, ssid: &[u8], label: &str, security: &str, strength: i64, ap_path: &str) -> Self {
        Self { id: id.into(), device_id: device_id.into(), ssid: ssid.into(), label: label.into(), security: security.into(),
               strength, ap_path: ap_path.into(), profile_ids: Vec::new(), active: false, frequency: 0, bssid: String::new() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub interface: String,
    pub device_ids: Vec<String>,
    pub active_ids: Vec<String>,
    pub ssid: Vec<u8>,
    pub security: String,
    pub timestamp: u64,
    pub autoconnect: bool,
    pub external: bool,
}

impl Profile {
    pub fn new(id: &str, name: &str, kind: &str) -> Self {
        Self { id: id.into(), name: name.into(), kind: kind.into(), interface: String::new(), device_ids: Vec::new(),
               active_ids: Vec::new(), ssid: Vec::new(), security: String::new(), timestamp: 0, autoconnect: true, external: false }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveConnection {
    pub id: String,
    pub profile_id: String,
    pub name: String,
    pub kind: String,
    pub device_ids: Vec<String>,
    pub state: u32,
    pub state_label: String,
    pub default: bool,
    pub default6: bool,
    pub vpn: bool,
    pub details: Vec<(String, String)>,
    pub external: bool,
}

impl ActiveConnection {
    pub fn new(id: &str, profile_id: &str, name: &str, kind: &str) -> Self {
        Self { id: id.into(), profile_id: profile_id.into(), name: name.into(), kind: kind.into(), device_ids: Vec::new(),
               state: 0, state_label: "Unknown".into(), default: false, default6: false, vpn: false, details: Vec::new(), external: false }
    }
}

/// One VPN configuration and its dedicated interfaces, for a popup card.
#[derive(Clone, Debug, PartialEq)]
pub struct VpnEntry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub profile_id: String,
    pub device_ids: Vec<String>,
    pub active_ids: Vec<String>,
    pub interface: String,
    pub state_label: String,
    pub enabled: bool,
    pub external: bool,
    pub controllable: bool,
}

impl VpnEntry {
    fn new(id: &str, name: &str, kind: &str) -> Self {
        Self { id: id.into(), name: name.into(), kind: kind.into(), profile_id: String::new(), device_ids: Vec::new(),
               active_ids: Vec::new(), interface: String::new(), state_label: "Disconnected".into(), enabled: false,
               external: false, controllable: false }
    }
}

/// Join VPN profiles, activations and dedicated tunnel devices (see the
/// Python docstring: only WireGuard devices or an explicit VPN profile /
/// activation make a VPN; compatible devices are never ownership).
pub fn vpn_entries(devices: &Map<Device>, profiles: &Map<Profile>, active: &Map<ActiveConnection>) -> Vec<VpnEntry> {
    let vpn_kinds = ["vpn", "wireguard"];
    let tunnel_kinds = ["wireguard", "tun", "ip-tunnel"];
    let mut entries: Map<VpnEntry> = Map::new();
    let mut claimed: HashSet<String> = HashSet::new();
    for profile in profiles.values() {
        if vpn_kinds.contains(&profile.kind.as_str()) {
            let mut entry = VpnEntry::new(&profile.id, &profile.name, &profile.kind);
            entry.profile_id = profile.id.clone();
            entry.interface = profile.interface.clone();
            entry.external = profile.external;
            entries.insert(profile.id.clone(), entry);
        }
    }
    for connection in active.values() {
        if !(connection.vpn || vpn_kinds.contains(&connection.kind.as_str()) || entries.contains_key(&connection.profile_id)) {
            continue;
        }
        let key = if connection.profile_id.is_empty() { connection.id.clone() } else { connection.profile_id.clone() };
        if !entries.contains_key(&key) {
            let mut entry = VpnEntry::new(&key, &connection.name, &connection.kind);
            entry.external = connection.external;
            entries.insert(key.clone(), entry);
        }
        let entry = entries.get_mut(&key).unwrap();
        entry.active_ids.push(connection.id.clone());
        entry.external = entry.external || connection.external;
        for device_id in &connection.device_ids {
            if let Some(device) = devices.get(device_id) {
                if tunnel_kinds.contains(&device.kind.as_str()) {
                    if !entry.device_ids.contains(device_id) {
                        entry.device_ids.push(device_id.clone());
                    }
                    claimed.insert(device_id.clone());
                }
            }
        }
    }
    let keys: Vec<String> = entries.keys().cloned().collect();
    for key in &keys {
        let (interface, profile_id) = {
            let entry = entries.get(key).unwrap();
            (entry.interface.clone(), entry.profile_id.clone())
        };
        if interface.is_empty() {
            continue;
        }
        for device in devices.values() {
            if claimed.contains(&device.id) || !tunnel_kinds.contains(&device.kind.as_str()) || device.interface != interface {
                continue;
            }
            let conflict = active.values()
                .filter(|c| c.device_ids.contains(&device.id) && [1, 2, 3].contains(&c.state))
                .any(|c| c.profile_id != profile_id);
            if conflict {
                continue;
            }
            entries.get_mut(key).unwrap().device_ids.push(device.id.clone());
            claimed.insert(device.id.clone());
        }
    }
    for device in devices.values() {
        if device.kind == "wireguard" && !claimed.contains(&device.id) {
            let name = if device.name.is_empty() { &device.interface } else { &device.name };
            let mut entry = VpnEntry::new(&device.id, name, "wireguard");
            entry.device_ids = vec![device.id.clone()];
            entry.interface = device.interface.clone();
            entry.state_label = device.state_label.clone();
            entry.enabled = device.state == 100;
            entry.external = true;
            entries.insert(device.id.clone(), entry);
        }
    }
    let keys: Vec<String> = entries.keys().cloned().collect();
    for key in keys {
        let entry = entries.get_mut(&key).unwrap();
        let connections: Vec<&ActiveConnection> = entry.active_ids.iter().filter_map(|k| active.get(k)).collect();
        if !connections.is_empty() {
            let priority = |state: u32| match state { 2 => 0, 1 => 1, 3 => 2, 4 => 3, _ => 4 };
            // min() keeps the first minimum, like Python.
            let mut current = connections[0];
            for c in &connections[1..] {
                if priority(c.state) < priority(current.state) {
                    current = c;
                }
            }
            entry.state_label = current.state_label.clone();
            entry.enabled = connections.iter().any(|c| c.state == 1 || c.state == 2);
        }
        let mut interfaces: Vec<String> = Vec::new();
        for id in &entry.device_ids {
            if let Some(device) = devices.get(id) {
                if !device.interface.is_empty() && !interfaces.contains(&device.interface) {
                    interfaces.push(device.interface.clone());
                }
            }
        }
        if !interfaces.is_empty() {
            entry.interface = interfaces.join(", ");
        }
        entry.controllable = (!entry.profile_id.is_empty() || entry.enabled) && !entry.external;
    }
    let mut result: Vec<VpnEntry> = entries.values().cloned().collect();
    result.sort_by(|a, b| (!a.enabled, casefold(&a.name), a.id.clone()).cmp(&(!b.enabled, casefold(&b.name), b.id.clone())));
    result
}

const FRIENDLY: &[(&[&str], &str)] = &[
    (&["AccessDenied", "PermissionDenied", "NotAuthorized", "not authorized", "Not authorized"],
     "You are not allowed to change this network setting."),
    (&["ServiceUnknown", "NameHasNoOwner", "UnknownObject"], "NetworkManager is not running."),
    (&["Timeout", "TimedOut", "timed out"], "The network service did not respond in time."),
    (&["UnknownConnection", "UnknownDevice", "NotFound"], "That connection or adapter is no longer available."),
];

/// Plain, user-facing text for an error; empty for user cancellation.
/// `text` is Python's `str(error.message)`, `cancelled` = `G_IO_ERROR_CANCELLED`.
pub fn friendly_text(text: &str, cancelled: bool) -> String {
    let text = text.trim();
    let lower = text.to_lowercase();
    if cancelled || lower == "operation was cancelled" || lower == "cancelled" {
        return String::new();
    }
    for (needles, message) in FRIENDLY {
        if needles.iter().any(|n| text.contains(n)) {
            return (*message).into();
        }
    }
    let mut rest = strip_remote_prefix(text);
    // re.sub(r"\s*\(\d+\)$", "", text)
    if rest.ends_with(')') {
        if let Some(open) = rest.rfind('(') {
            let digits = &rest[open + 1..rest.len() - 1];
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                rest = rest[..open].trim_end().to_owned();
            }
        }
    }
    let rest = rest.trim().to_owned();
    if rest.is_empty() { "The network change could not be completed.".into() } else { rest }
}

/// `^(?:[\w.-]+-quark:\s*)?(?:GDBus\.Error:[\w.]+:\s*)?`
fn strip_remote_prefix(text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '.' || c == '-';
    let mut rest = text;
    if let Some(index) = rest.find("-quark:") {
        let head = &rest[..index];
        if !head.is_empty() && head.chars().all(word) {
            rest = rest[index + 7..].trim_start();
        }
    }
    if let Some(after) = rest.strip_prefix("GDBus.Error:") {
        if let Some(colon) = after.find(':') {
            let name = &after[..colon];
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                rest = after[colon + 1..].trim_start();
            }
        }
    }
    rest.to_owned()
}

pub fn friendly_glib(error: &glib::Error) -> String {
    friendly_text(error.message(), error.matches(gio::IOErrorEnum::Cancelled))
}

/// D-Bus errors as PyGObject shows them (`GDBus.Error:<name>: <message>`).
pub fn friendly_bus(error: &BusError) -> String {
    match &error.name {
        Some(name) => friendly_text(&format!("GDBus.Error:{name}: {}", error.message), false),
        None => friendly_text(&error.message, error.message == "Operation was cancelled"),
    }
}

pub fn sorted_busy(busy: &HashSet<String>) -> Vec<String> {
    let mut keys: Vec<String> = busy.iter().cloned().collect();
    keys.sort();
    keys
}

/// Ordered `dict.fromkeys` style union.
pub fn union(first: &[String], second: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in first.iter().chain(second) {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

pub type Details = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    fn map<V: Clone>(items: &[(&str, V)]) -> Map<V> {
        items.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn dev(id: &str, iface: &str, name: &str, kind: &str) -> Device {
        Device::new(id, iface, name, kind)
    }

    #[test]
    fn external_wireguard_profile_activation_and_device_are_one_card() {
        let mut device = dev("/wg", "wg-example", "WireGuard", "wireguard");
        device.state = 100;
        let mut profile = Profile::new("wg-uuid", "Example VPN", "wireguard");
        profile.interface = "wg-example".into();
        profile.active_ids = vec!["/active/wg".into()];
        profile.external = true;
        let mut active = ActiveConnection::new("/active/wg", "wg-uuid", "Example VPN", "wireguard");
        active.device_ids = vec!["/wg".into()];
        active.state = 2;
        active.state_label = "Connected".into();
        active.external = true;
        let entries = vpn_entries(&map(&[("/wg", device)]), &map(&[("wg-uuid", profile)]), &map(&[("/active/wg", active)]));
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!((e.id.as_str(), e.profile_id.as_str(), e.device_ids.clone(), e.active_ids.clone()),
                   ("wg-uuid", "wg-uuid", vec!["/wg".to_string()], vec!["/active/wg".to_string()]));
        assert_eq!((e.interface.as_str(), e.state_label.as_str()), ("wg-example", "Connected"));
        assert!(e.enabled && e.external && !e.controllable);
    }

    #[test]
    fn native_vpn_underlay_remains_outside_tunnel_device_ids() {
        let eth = dev("/eth", "enp0s1", "Ethernet", "wired");
        let wifi = dev("/wifi", "wlan0", "Wi-Fi", "wifi");
        let mut profile = Profile::new("vpn", "Office", "vpn");
        profile.device_ids = vec!["/eth".into(), "/wifi".into()];
        let mut active = ActiveConnection::new("/active/vpn", "vpn", "Office", "vpn");
        active.device_ids = vec!["/eth".into(), "/wifi".into()];
        active.state = 2;
        active.vpn = true;
        let entries = vpn_entries(&map(&[("/eth", eth), ("/wifi", wifi)]), &map(&[("vpn", profile)]), &map(&[("/active/vpn", active)]));
        assert_eq!(entries.len(), 1);
        assert!(entries[0].device_ids.is_empty());
        assert!(entries[0].controllable);
    }

    #[test]
    fn arbitrary_virtual_devices_and_external_tun_are_not_vpn() {
        let devices: Map<Device> = ["tun", "bridge", "veth", "loopback", "dummy", "ip-tunnel"].iter()
            .map(|k| (k.to_string(), dev(k, &format!("mullvad-{k}"), "Virtual device", k))).collect();
        let mut profile = Profile::new("tun-profile", "Mullvad named virtual device", "tun");
        profile.interface = "mullvad-tun".into();
        profile.external = true;
        let mut active = ActiveConnection::new("/active/tun", "tun-profile", "x", "tun");
        active.device_ids = vec!["tun".into()];
        active.state = 2;
        active.external = true;
        assert!(vpn_entries(&devices, &map(&[("tun-profile", profile)]), &map(&[("/active/tun", active)])).is_empty());
    }

    #[test]
    fn explicit_vpn_claims_tun_and_orphan_vpn_remains_controllable() {
        let tun = dev("/tun", "tun0", "Tunnel", "tun");
        let mut active = ActiveConnection::new("/active/vpn", "unavailable-profile", "Office", "vpn");
        active.device_ids = vec!["/tun".into()];
        active.state = 1;
        active.state_label = "Connecting".into();
        active.vpn = true;
        let entries = vpn_entries(&map(&[("/tun", tun)]), &Map::new(), &map(&[("/active/vpn", active)]));
        let e = &entries[0];
        assert_eq!(e.profile_id, "");
        assert_eq!(e.device_ids, ["/tun"]);
        assert_eq!(e.active_ids, ["/active/vpn"]);
        assert!(e.enabled && e.controllable);
        assert_eq!(e.state_label, "Connecting");
    }

    #[test]
    fn orphan_wireguard_device_is_visible_without_connect_action() {
        let mut device = dev("/wg", "wg-example", "WireGuard", "wireguard");
        device.state = 30;
        device.state_label = "Disconnected".into();
        let entries = vpn_entries(&map(&[("/wg", device)]), &Map::new(), &Map::new());
        let e = &entries[0];
        assert_eq!((e.name.as_str(), e.interface.as_str(), e.state_label.as_str()), ("WireGuard", "wg-example", "Disconnected"));
        assert_eq!(e.device_ids, ["/wg"]);
        assert!(!e.enabled && e.external && !e.controllable);
    }

    #[test]
    fn orphan_activations_share_uuid_and_cannot_reconnect() {
        let mut a = ActiveConnection::new("/active/1", "unavailable-profile", "Office", "vpn");
        a.state = 4;
        a.state_label = "Disconnected".into();
        a.vpn = true;
        let mut b = ActiveConnection::new("/active/2", "unavailable-profile", "Office", "vpn");
        b.state = 3;
        b.state_label = "Disconnecting".into();
        b.vpn = true;
        let entries = vpn_entries(&Map::new(), &Map::new(), &map(&[("/active/1", a), ("/active/2", b)]));
        let e = &entries[0];
        assert_eq!(e.active_ids, ["/active/1", "/active/2"]);
        assert_eq!(e.profile_id, "");
        assert!(!e.controllable && !e.enabled);
        assert_eq!(e.state_label, "Disconnecting");
    }

    #[test]
    fn compatible_devices_are_not_owned_by_inactive_profile() {
        let device = dev("/wg", "wg-external", "External interface", "wireguard");
        let mut saved = Profile::new("saved", "Home", "wireguard");
        saved.device_ids = vec!["/wg".into()];
        let mut external = Profile::new("external", "Other app", "wireguard");
        external.interface = "wg-external".into();
        external.external = true;
        let mut active = ActiveConnection::new("/active", "external", "Other app", "wireguard");
        active.device_ids = vec!["/wg".into()];
        active.state = 2;
        active.external = true;
        let entries = vpn_entries(&map(&[("/wg", device)]), &map(&[("saved", saved), ("external", external)]), &map(&[("/active", active)]));
        assert_eq!(entries.len(), 2);
        let saved = entries.iter().find(|e| e.id == "saved").unwrap();
        assert!(saved.device_ids.is_empty() && saved.controllable && !saved.enabled);
        assert_eq!(entries.iter().find(|e| e.id == "external").unwrap().device_ids, ["/wg"]);
    }

    #[test]
    fn disconnecting_status_is_preserved_and_external_flag_wins() {
        let profile = Profile::new("saved", "Home", "wireguard");
        let mut active = ActiveConnection::new("/active", "saved", "Home", "wireguard");
        active.state = 3;
        active.state_label = "Disconnecting".into();
        active.external = true;
        let entries = vpn_entries(&Map::new(), &map(&[("saved", profile)]), &map(&[("/active", active)]));
        let e = &entries[0];
        assert_eq!(e.state_label, "Disconnecting");
        assert!(!e.enabled && e.external && !e.controllable);
    }

    #[test]
    fn friendly_errors_match_python() {
        assert_eq!(friendly_text("Operation was cancelled", false), "");
        assert_eq!(friendly_text("GDBus.Error:org.freedesktop.NetworkManager.PermissionDenied: Not authorized to control networking.", false),
                   "You are not allowed to change this network setting.");
        assert_eq!(friendly_bus(&BusError::remote("org.freedesktop.DBus.Error.ServiceUnknown", "x")), "NetworkManager is not running.");
        assert_eq!(friendly_text("g-io-error-quark: Timeout was reached (24)", false), "The network service did not respond in time.");
        assert_eq!(friendly_text("g-dbus-error-quark: GDBus.Error:org.x.Failed: Something broke (36)", false), "Something broke");
        assert_eq!(friendly_text("Permission denied", false), "Permission denied");
        assert_eq!(friendly_text("  ", false), "The network change could not be completed.");
        assert_eq!(friendly_bus(&BusError::remote("org.freedesktop.NetworkManager.Device.NotAllowed", "SetManaged is unavailable")),
                   "SetManaged is unavailable");
    }
}
