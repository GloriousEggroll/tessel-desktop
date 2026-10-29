//! NetworkManager D-Bus constants, enum tables (libnm value nicks) and the
//! pure model helpers of network_backend.py: security classification, IP
//! detail formatting, and Rust ports of libnm's client-side
//! `nm_device_connection_compatible` / `nm_access_point_connection_valid`
//! (approximations: type, interface name, MAC, SSID/BSSID/mode/band and AP
//! security compatibility).

use glib::Variant;

/// `g_variant_get_variant` only for `v` values (glib-rs does not check the type).
pub fn unbox(value: &Variant) -> Option<Variant> {
    (value.type_() == glib::VariantTy::VARIANT).then(|| value.as_variant()).flatten()
}
use std::collections::{BTreeMap, HashMap};

pub const NM_BUS: &str = "org.freedesktop.NetworkManager";
pub const NM_PATH: &str = "/org/freedesktop/NetworkManager";
pub const NM_IFACE: &str = "org.freedesktop.NetworkManager";
pub const OBJECT_MANAGER_PATH: &str = "/org/freedesktop";
pub const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
pub const SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
pub const CONNECTION_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
pub const DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
pub const WIRED_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wired";
pub const WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
pub const AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
pub const ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
pub const VPN_IFACE: &str = "org.freedesktop.NetworkManager.VPN.Connection";
pub const IP4_IFACE: &str = "org.freedesktop.NetworkManager.IP4Config";
pub const IP6_IFACE: &str = "org.freedesktop.NetworkManager.IP6Config";
pub const AGENT_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager/AgentManager";
pub const AGENT_MANAGER_IFACE: &str = "org.freedesktop.NetworkManager.AgentManager";
pub const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";
pub const AGENT_IFACE: &str = "org.freedesktop.NetworkManager.SecretAgent";

pub const AP_PRIVACY: u32 = 0x1;
pub const KEY_MGMT_PSK: u32 = 0x100;
pub const KEY_MGMT_802_1X: u32 = 0x200;
pub const KEY_MGMT_SAE: u32 = 0x400;
pub const KEY_MGMT_OWE: u32 = 0x800;
pub const KEY_MGMT_OWE_TM: u32 = 0x1000;
pub const KEY_MGMT_EAP_SUITE_B_192: u32 = 0x2000;
pub const SETTINGS_FLAG_EXTERNAL: u32 = 0x8;
pub const WIFI_CAP_WPA: u32 = 0x10;
pub const WIFI_CAP_RSN: u32 = 0x20;
pub const ACTIVATION_FLAG_EXTERNAL: u32 = 0x80;
pub const MANAGED_YES: u32 = 1;
pub const MANAGED_NO: u32 = 0;
pub const MANAGED_RUNTIME: u32 = 1;
pub const MANAGED_SET_ADMIN_STATE: u32 = 16;
pub const ADD2_TO_DISK: u32 = 1;
pub const ADD2_BLOCK_AUTOCONNECT: u32 = 32;
pub const UPDATE2_TO_DISK: u32 = 1;
pub const UPDATE2_NO_REAPPLY: u32 = 64;

pub fn device_state(state: u32) -> &'static str {
    match state {
        0 => "Unknown", 10 => "Unmanaged", 20 => "Unavailable", 30 => "Disconnected", 40 => "Connecting",
        50 => "Configuring", 60 => "Authentication required", 70 => "Obtaining an address", 80 => "Checking connection",
        90 => "Connecting services", 100 => "Connected", 110 => "Disconnecting", 120 => "Connection failed",
        _ => "Unknown",
    }
}

pub fn active_state(state: u32) -> &'static str {
    match state {
        0 => "Unknown", 1 => "Connecting", 2 => "Connected", 3 => "Disconnecting", 4 => "Disconnected",
        _ => "Unknown",
    }
}

pub fn connectivity(value: u32) -> &'static str {
    match value {
        1 => "Offline", 2 => "Sign-in required", 3 => "Limited connectivity", 4 => "Online",
        _ => "Unknown",
    }
}

const DEVICE_TYPES: &[&str] = &["unknown", "ethernet", "wifi", "unused1", "unused2", "bt", "olpc-mesh", "wimax", "modem",
    "infiniband", "bond", "vlan", "adsl", "bridge", "generic", "team", "tun", "ip-tunnel", "macvlan", "vxlan", "veth",
    "macsec", "dummy", "ppp", "ovs-interface", "ovs-port", "ovs-bridge", "wpan", "6lowpan", "wireguard", "wifi-p2p",
    "vrf", "loopback", "hsr", "ipvlan", "geneve"];

const DEVICE_REASONS: &[&str] = &["none", "unknown", "now-managed", "now-unmanaged", "config-failed", "ip-config-unavailable",
    "ip-config-expired", "no-secrets", "supplicant-disconnect", "supplicant-config-failed", "supplicant-failed",
    "supplicant-timeout", "ppp-start-failed", "ppp-disconnect", "ppp-failed", "dhcp-start-failed", "dhcp-error",
    "dhcp-failed", "shared-start-failed", "shared-failed", "autoip-start-failed", "autoip-error", "autoip-failed",
    "modem-busy", "modem-no-dial-tone", "modem-no-carrier", "modem-dial-timeout", "modem-dial-failed", "modem-init-failed",
    "gsm-apn-failed", "gsm-registration-not-searching", "gsm-registration-denied", "gsm-registration-timeout",
    "gsm-registration-failed", "gsm-pin-check-failed", "firmware-missing", "removed", "sleeping", "connection-removed",
    "user-requested", "carrier", "connection-assumed", "supplicant-available", "modem-not-found", "bt-failed",
    "gsm-sim-not-inserted", "gsm-sim-pin-required", "gsm-sim-puk-required", "gsm-sim-wrong", "infiniband-mode",
    "dependency-failed", "br2684-failed", "modem-manager-unavailable", "ssid-not-found", "secondary-connection-failed",
    "dcb-fcoe-failed", "teamd-control-failed", "modem-failed", "modem-available", "sim-pin-incorrect", "new-activation",
    "parent-changed", "parent-managed-changed", "ovsdb-failed", "ip-address-duplicate", "ip-method-unsupported",
    "sriov-configuration-failed", "peer-not-found", "device-handler-failed", "unmanaged-by-default",
    "unmanaged-external-down", "unmanaged-link-not-init", "unmanaged-quitting", "unmanaged-sleeping",
    "unmanaged-user-conf", "unmanaged-user-explicit", "unmanaged-user-settings", "unmanaged-user-udev", "networking-off",
    "modem-no-operator-code"];

const ACTIVE_REASONS: &[&str] = &["unknown", "none", "user-disconnected", "device-disconnected", "service-stopped",
    "ip-config-invalid", "connect-timeout", "service-start-timeout", "service-start-failed", "no-secrets", "login-failed",
    "connection-removed", "dependency-failed", "device-realize-failed", "device-removed"];

fn nick(table: &[&str], value: u32) -> String {
    table.get(value as usize).map(|s| s.to_string()).unwrap_or_else(|| value.to_string())
}

/// `enum_name()`: the value nick with `-` → space.
pub fn device_reason_name(value: u32) -> String {
    nick(DEVICE_REASONS, value).replace('-', " ")
}

pub fn active_reason_name(value: u32) -> String {
    nick(ACTIVE_REASONS, value).replace('-', " ")
}

/// NMVpnConnectionStateReason shares the first 12 nicks of the active reasons.
pub fn vpn_reason_name(value: u32) -> String {
    if value <= 11 { active_reason_name(value) } else { value.to_string() }
}

pub fn device_type_nick(value: u32) -> String {
    nick(DEVICE_TYPES, value)
}

/// `DEVICE_KINDS.get(type, nick)`.
pub fn device_kind(value: u32) -> String {
    match value {
        2 => "wifi".into(),
        1 => "wired".into(),
        8 => "mobile".into(),
        5 => "bluetooth".into(),
        other => device_type_nick(other),
    }
}

/// `TYPE_NAMES.get(connection_type, connection_type)`.
pub fn type_kind(connection_type: &str) -> String {
    match connection_type {
        "802-11-wireless" => "wifi",
        "802-3-ethernet" => "wired",
        "gsm" | "cdma" => "mobile",
        "bluetooth" => "bluetooth",
        other => other,
    }.into()
}

/// Python `str.title()`.
pub fn title(text: &str) -> String {
    let mut out = String::new();
    let mut previous_cased = false;
    for c in text.chars() {
        if c.is_alphabetic() {
            if previous_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            previous_cased = true;
        } else {
            out.push(c);
            previous_cased = false;
        }
    }
    out
}

/// Python `str.capitalize()`.
pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars.flat_map(|c| c.to_lowercase())).collect(),
        None => String::new(),
    }
}

/// `ap_security(ap)` (order matters: transition networks are WPA-PSK).
pub fn ap_security(flags: u32, wpa: u32, rsn: u32) -> &'static str {
    let combined = wpa | rsn;
    if combined & (KEY_MGMT_802_1X | KEY_MGMT_EAP_SUITE_B_192) != 0 {
        "wpa-enterprise"
    } else if combined & KEY_MGMT_PSK != 0 {
        "wpa-personal"
    } else if combined & KEY_MGMT_SAE != 0 {
        "sae"
    } else if combined & (KEY_MGMT_OWE | KEY_MGMT_OWE_TM) != 0 {
        "owe"
    } else if flags & AP_PRIVACY != 0 {
        "wep"
    } else {
        "open"
    }
}

/// Serialized connection settings: `{setting: {key: value}}`.
pub type Settings = BTreeMap<String, BTreeMap<String, Variant>>;

pub fn parse_settings(value: &Variant) -> Settings {
    let mut out = Settings::new();
    let value = if value.type_().as_str() == "(a{sa{sv}})" { value.child_value(0) } else { value.clone() };
    for entry in value.iter() {
        let name = entry.child_value(0).str().unwrap_or_default().to_owned();
        let mut props = BTreeMap::new();
        for item in entry.child_value(1).iter() {
            let key = item.child_value(0).str().unwrap_or_default().to_owned();
            if let Some(inner) = crate::nm::unbox(&item.child_value(1)) {
                props.insert(key, inner);
            }
        }
        out.insert(name, props);
    }
    out
}

pub fn settings_variant(settings: &Settings) -> Variant {
    let entries: Vec<Variant> = settings.iter().map(|(name, props)| {
        let dict = glib::VariantDict::new(None);
        for (k, v) in props {
            dict.insert_value(k, v);
        }
        Variant::from_dict_entry(&name.to_variant(), &dict.end())
    }).collect();
    Variant::array_from_iter_with_type(glib::VariantTy::new("{sa{sv}}").unwrap(), entries)
}

use glib::prelude::*;

pub fn get_str(settings: &Settings, setting: &str, key: &str) -> Option<String> {
    settings.get(setting)?.get(key)?.str().map(String::from)
}

pub fn get_bool(settings: &Settings, setting: &str, key: &str) -> Option<bool> {
    settings.get(setting)?.get(key)?.get::<bool>()
}

pub fn get_u64(settings: &Settings, setting: &str, key: &str) -> Option<u64> {
    let value = settings.get(setting)?.get(key)?;
    value.get::<u64>().or_else(|| value.get::<u32>().map(u64::from)).or_else(|| value.get::<i64>().map(|v| v.max(0) as u64))
}

pub fn get_bytes(settings: &Settings, setting: &str, key: &str) -> Option<Vec<u8>> {
    settings.get(setting)?.get(key)?.fixed_array::<u8>().ok().map(|b| b.to_vec())
}

/// `profile_security(connection)` (only called for wireless profiles).
pub fn profile_security(settings: &Settings) -> String {
    let Some(security) = settings.get("802-11-wireless-security") else { return "open".into() };
    let key = security.get("key-mgmt").and_then(|v| v.str().map(String::from)).unwrap_or_default();
    match key.as_str() {
        "none" => "wep", "wpa-psk" => "wpa-personal", "sae" => "sae", "owe" => "owe", "wpa-eap" => "wpa-enterprise",
        "wpa-eap-suite-b-192" => "wpa-enterprise", "ieee8021x" => "wep", other => return other.to_owned(),
    }.into()
}

/// D-Bus object cache: `{path: {interface: {property: value}}}`.
pub type Objects = HashMap<String, HashMap<String, BTreeMap<String, Variant>>>;

pub fn prop<'a>(objects: &'a Objects, path: &str, iface: &str, name: &str) -> Option<&'a Variant> {
    objects.get(path)?.get(iface)?.get(name)
}

pub fn prop_str(objects: &Objects, path: &str, iface: &str, name: &str) -> String {
    prop(objects, path, iface, name).and_then(|v| v.str().map(String::from)).unwrap_or_default()
}

pub fn prop_u32(objects: &Objects, path: &str, iface: &str, name: &str) -> u32 {
    prop(objects, path, iface, name).and_then(|v| v.get::<u32>().or_else(|| v.get::<u8>().map(u32::from))).unwrap_or(0)
}

pub fn prop_i64(objects: &Objects, path: &str, iface: &str, name: &str) -> i64 {
    prop(objects, path, iface, name).and_then(|v| v.get::<i64>().or_else(|| v.get::<u64>().map(|x| x as i64))).unwrap_or(0)
}

pub fn prop_u64(objects: &Objects, path: &str, iface: &str, name: &str) -> u64 {
    prop(objects, path, iface, name).and_then(|v| v.get::<u64>()).unwrap_or(0)
}

pub fn prop_bool(objects: &Objects, path: &str, iface: &str, name: &str) -> bool {
    prop(objects, path, iface, name).and_then(|v| v.get::<bool>()).unwrap_or(false)
}

/// An object-path property; `"/"` (none) → `None`.
pub fn prop_path(objects: &Objects, path: &str, iface: &str, name: &str) -> Option<String> {
    prop(objects, path, iface, name).and_then(|v| v.str().map(String::from)).filter(|p| p != "/" && !p.is_empty())
}

pub fn prop_paths(objects: &Objects, path: &str, iface: &str, name: &str) -> Vec<String> {
    prop(objects, path, iface, name).map(|v| v.iter().filter_map(|p| p.str().map(String::from)).collect()).unwrap_or_default()
}

pub fn prop_bytes(objects: &Objects, path: &str, iface: &str, name: &str) -> Vec<u8> {
    prop(objects, path, iface, name).and_then(|v| v.fixed_array::<u8>().ok().map(|b| b.to_vec())).unwrap_or_default()
}

fn ipv4_text(value: u32) -> String {
    std::net::Ipv4Addr::from(u32::from_be(value)).to_string()
}

fn ipv6_text(bytes: &[u8]) -> Option<String> {
    let array: [u8; 16] = bytes.try_into().ok()?;
    Some(std::net::Ipv6Addr::from(array).to_string())
}

fn entry_value(entry: &Variant, key: &str) -> Option<Variant> {
    entry.iter().find(|e| e.child_value(0).str() == Some(key)).map(|e| { let v = e.child_value(1); crate::nm::unbox(&v).unwrap_or(v) })
}

fn dict_str(entry: &Variant, key: &str) -> Option<String> {
    entry_value(entry, key).and_then(|v| v.str().map(String::from))
}

fn dict_u32(entry: &Variant, key: &str) -> Option<u32> {
    entry_value(entry, key).and_then(|v| v.get::<u32>())
}

/// `ip_details(config, family)`: addresses, gateway, DNS, search domains, routes.
pub fn ip_details(objects: &Objects, path: &str, family: u8) -> Vec<(String, String)> {
    let iface = if family == 4 { IP4_IFACE } else { IP6_IFACE };
    let Some(config) = objects.get(path).and_then(|o| o.get(iface)) else { return Vec::new() };
    let mut details = Vec::new();
    let data = |name: &str| config.get(name).map(|v| v.iter().map(|x| crate::nm::unbox(&x).unwrap_or(x)).collect::<Vec<Variant>>()).unwrap_or_default();
    let addresses: Vec<String> = data("AddressData").iter().filter_map(|a| {
        Some(format!("{}/{}", dict_str(a, "address")?, dict_u32(a, "prefix").unwrap_or(0)))
    }).collect();
    if !addresses.is_empty() {
        details.push((format!("IPv{family} addresses"), addresses.join("\n")));
    }
    if let Some(gateway) = config.get("Gateway").and_then(|v| v.str().map(String::from)).filter(|g| !g.is_empty()) {
        details.push((format!("IPv{family} gateway"), gateway));
    }
    let mut nameservers: Vec<String> = data("NameserverData").iter().filter_map(|n| dict_str(n, "address")).collect();
    if nameservers.is_empty() {
        if let Some(value) = config.get("Nameservers") {
            for item in value.iter() {
                if let Some(v4) = item.get::<u32>() {
                    nameservers.push(ipv4_text(v4));
                } else if let Ok(bytes) = item.fixed_array::<u8>() {
                    if let Some(text) = ipv6_text(bytes) {
                        nameservers.push(text);
                    }
                }
            }
        }
    }
    if !nameservers.is_empty() {
        details.push((format!("IPv{family} DNS"), nameservers.join(", ")));
    }
    let domains: Vec<String> = config.get("Domains").map(|v| v.iter().filter_map(|d| d.str().map(String::from)).collect()).unwrap_or_default();
    if !domains.is_empty() {
        details.push((format!("IPv{family} search domains"), domains.join(", ")));
    }
    let routes: Vec<String> = data("RouteData").iter().filter_map(|r| {
        let mut text = format!("{}/{}", dict_str(r, "dest")?, dict_u32(r, "prefix").unwrap_or(0));
        if let Some(hop) = dict_str(r, "next-hop").filter(|h| !h.is_empty()) {
            text += &format!(" via {hop}");
        }
        if let Some(metric) = dict_u32(r, "metric") {
            text += &format!(" (metric {metric})");
        }
        Some(text)
    }).collect();
    if !routes.is_empty() {
        details.push((format!("IPv{family} routes"), routes.join("\n")));
    }
    details
}

/// `nm_device_connection_compatible`: type, interface name and MAC address checks.
pub fn connection_compatible(objects: &Objects, device: &str, settings: &Settings) -> bool {
    let connection_type = get_str(settings, "connection", "type").unwrap_or_default();
    let device_type = prop_u32(objects, device, DEVICE_IFACE, "DeviceType");
    let accepted: &[&str] = match device_type {
        1 => &["802-3-ethernet", "pppoe"],
        2 => &["802-11-wireless"],
        5 => &["bluetooth"],
        6 => &["802-11-olpc-mesh"],
        7 => &["wimax"],
        8 => &["gsm", "cdma"],
        9 => &["infiniband"],
        10 => &["bond"],
        11 => &["vlan"],
        12 => &["adsl"],
        13 => &["bridge"],
        14 => &["generic"],
        15 => &["team"],
        16 => &["tun"],
        17 => &["ip-tunnel"],
        18 => &["macvlan"],
        19 => &["vxlan"],
        20 => &["veth"],
        21 => &["macsec"],
        22 => &["dummy"],
        23 => &["pppoe"],
        24 => &["ovs-interface"],
        25 => &["ovs-port"],
        26 => &["ovs-bridge"],
        27 => &["wpan"],
        28 => &["6lowpan"],
        29 => &["wireguard"],
        30 => &["wifi-p2p"],
        31 => &["vrf"],
        32 => &["loopback"],
        33 => &["hsr"],
        34 => &["ipvlan"],
        35 => &["geneve"],
        _ => &[],
    };
    if !accepted.contains(&connection_type.as_str()) {
        return false;
    }
    let interface = prop_str(objects, device, DEVICE_IFACE, "Interface");
    if let Some(name) = get_str(settings, "connection", "interface-name").filter(|n| !n.is_empty()) {
        if name != interface {
            return false;
        }
    }
    let mac_setting = match device_type {
        1 => Some(("802-3-ethernet", WIRED_IFACE)),
        2 => Some(("802-11-wireless", WIRELESS_IFACE)),
        _ => None,
    };
    if let Some((setting, iface)) = mac_setting {
        if let Some(mac) = get_bytes(settings, setting, "mac-address").filter(|m| !m.is_empty()) {
            let mut hardware = prop_str(objects, device, iface, "PermHwAddress");
            if hardware.is_empty() {
                hardware = prop_str(objects, device, DEVICE_IFACE, "HwAddress");
            }
            if !hardware.is_empty() && !hardware.eq_ignore_ascii_case(&mac_text(&mac)) {
                return false;
            }
        }
    }
    if device_type == 2 {
        // libnm: WPA/RSN key management needs a WPA- or RSN-capable card.
        let key = get_str(settings, "802-11-wireless-security", "key-mgmt").unwrap_or_default();
        if ["wpa-psk", "wpa-eap", "sae", "owe", "wpa-eap-suite-b-192"].contains(&key.as_str()) {
            let caps = prop_u32(objects, device, WIRELESS_IFACE, "WirelessCapabilities");
            if caps & (WIFI_CAP_WPA | WIFI_CAP_RSN) == 0 {
                return false;
            }
            let protos: Vec<String> = settings.get("802-11-wireless-security").and_then(|s| s.get("proto"))
                .and_then(|v| v.get::<Vec<String>>()).unwrap_or_default();
            if protos.iter().any(|p| p == "rsn") && !protos.iter().any(|p| p == "wpa") && caps & WIFI_CAP_RSN == 0 {
                return false;
            }
        }
    }
    if device_type == 5 {
        if let Some(address) = get_bytes(settings, "bluetooth", "bdaddr").filter(|m| !m.is_empty()) {
            let hardware = prop_str(objects, device, DEVICE_IFACE, "HwAddress");
            if !hardware.is_empty() && !hardware.eq_ignore_ascii_case(&mac_text(&address)) {
                return false;
            }
        }
    }
    true
}

pub fn mac_text(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// `nm_access_point_connection_valid`: SSID, BSSID, mode, band and security.
pub fn ap_connection_valid(objects: &Objects, ap: &str, settings: &Settings) -> bool {
    if get_str(settings, "connection", "type").as_deref() != Some("802-11-wireless") || !settings.contains_key("802-11-wireless") {
        return false;
    }
    let ssid = prop_bytes(objects, ap, AP_IFACE, "Ssid");
    let wanted = get_bytes(settings, "802-11-wireless", "ssid").unwrap_or_default();
    if !ssid.is_empty() && wanted != ssid {
        return false;
    }
    if ssid.is_empty() && wanted.is_empty() {
        return false;
    }
    if let Some(bssid) = get_bytes(settings, "802-11-wireless", "bssid").filter(|b| !b.is_empty()) {
        let actual = prop_str(objects, ap, AP_IFACE, "HwAddress");
        if !actual.eq_ignore_ascii_case(&mac_text(&bssid)) {
            return false;
        }
    }
    let ap_mode = prop_u32(objects, ap, AP_IFACE, "Mode");
    let mode = get_str(settings, "802-11-wireless", "mode").unwrap_or_else(|| "infrastructure".into());
    let expected = match mode.as_str() {
        "infrastructure" => 2,
        "adhoc" => 1,
        "mesh" => 4,
        _ => return false,
    };
    if ap_mode != 0 && ap_mode != expected {
        return false;
    }
    let frequency = prop_u32(objects, ap, AP_IFACE, "Frequency");
    if let Some(band) = get_str(settings, "802-11-wireless", "band").filter(|b| !b.is_empty()) {
        let ok = match band.as_str() {
            "a" => (4915..=5825).contains(&frequency),
            "bg" => (2412..=2484).contains(&frequency),
            _ => true,
        };
        if frequency != 0 && !ok {
            return false;
        }
    }
    let flags = prop_u32(objects, ap, AP_IFACE, "Flags");
    let wpa = prop_u32(objects, ap, AP_IFACE, "WpaFlags");
    let rsn = prop_u32(objects, ap, AP_IFACE, "RsnFlags");
    security_compatible(settings, flags, wpa, rsn)
}

/// `nm_setting_wireless_ap_security_compatible` (key management level).
pub fn security_compatible(settings: &Settings, flags: u32, wpa: u32, rsn: u32) -> bool {
    let both = wpa | rsn;
    let Some(security) = settings.get("802-11-wireless-security") else {
        return flags & AP_PRIVACY == 0 && wpa == 0 && rsn == 0;
    };
    let key = security.get("key-mgmt").and_then(|v| v.str().map(String::from)).unwrap_or_default();
    match key.as_str() {
        "none" => flags & AP_PRIVACY != 0 && wpa == 0 && rsn == 0,
        "ieee8021x" => flags & AP_PRIVACY != 0 && (both == 0 || both & KEY_MGMT_802_1X != 0),
        "wpa-psk" => both & KEY_MGMT_PSK != 0,
        "sae" => both & KEY_MGMT_SAE != 0,
        "owe" => both & (KEY_MGMT_OWE | KEY_MGMT_OWE_TM) != 0,
        "wpa-eap" => both & KEY_MGMT_802_1X != 0,
        "wpa-eap-suite-b-192" => both & KEY_MGMT_EAP_SUITE_B_192 != 0,
        _ => false,
    }
}

// --- udev-based device description (libnm `nm_device_get_description`) ---

const VENDOR_PHRASES: &[&str] = &["Access Systems", "Business Mobile Networks BV", "Communications & Multimedia",
    "Company of Japan", "Computer Co.", "Computer Corp.", "Computer Corporation", "Computer Inc.", "Computer, Inc.",
    "Information and Communication Products", "Macao Commercial Offshore", "Mobile Phones", "(M) Son",
    "Multimedia Internet Technology", "Technology Group Ltd.", "Wireless Networks", "Wireless Solutions"];
const VENDOR_WORDS: &[&str] = &["AB", "AG", "A/S", "ASA", "B.V.", "Chips", "Co.", "Co", "Communications", "Components",
    "Computers", "Computertechnik", "corp.", "Corp.", "Corp", "Corporation", "Devices", "Electronics", "Electronic",
    "Elektronik", "GmbH", "Inc.", "Inc", "INC.", "Integrated", "Interface", "Limited", "Ltd.", "Ltd", "Microsystems",
    "Multimedia", "Network", "Networks", "of", "Semiconductor", "Semiconductors", "Systems", "Technologies",
    "Technology", "Telecommunications", "Wireless"];
const PRODUCT_PHRASES: &[&str] = &["100/10 MBit", "10/100 Mbps", "1.0 GbE", "10 GbE", "10 Gigabit", "10 Mbps",
    "1 Gigabit", "1 GbE", "100 Mbps", "2.5 GbE", "2.5GbE", "2.5 Gigabit", "40 GbE", "5 GbE", "802.11 Wireless Network Adapter",
    "Dual Band Wireless AC", "Dual Band Wireless", "Ethernet Controller", "Gigabit Ethernet Controller",
    "Network Connection", "Wireless LAN Controller", "Wireless Network Adapter", "Wireless LAN Adapter",
    "Wireless Adapter", "Wi-Fi Adapter", "Multiprotocol MAC/baseband processor", "Fast Ethernet"];
const PRODUCT_WORDS: &[&str] = &["1000BaseT", "100BaseT", "1000BaseTX", "100BaseTX", "Adapter", "Adaptor", "ADSL",
    "Basic", "CAN", "Converter", "Controller", "Cradle", "Desktop", "Device", "Dongle", "Ethernet", "Fast", "Gigabit",
    "GbE", "Integrated", "Interface", "LAN", "Mbps", "Mini", "Mobile", "Modem", "Network", "PC", "PCI", "PCIe", "Port",
    "Series", "USB", "Wi-Fi", "WiFi", "Wireless", "WLAN"];

fn fixup(text: &str, phrases: &[&str], words: &[&str], strip_brackets: bool) -> String {
    let mut desc = text.replace('_', " ");
    if strip_brackets {
        for (open, close) in [('[', ']'), ('(', ')')] {
            while let (Some(a), Some(b)) = (desc.find(open), desc.find(close)) {
                if b < a {
                    break;
                }
                desc.replace_range(a..=b, " ");
            }
        }
    }
    for phrase in phrases {
        while let Some(index) = desc.find(phrase) {
            let end = index + phrase.len();
            let before_ok = index == 0 || desc[..index].ends_with(' ');
            let after_ok = end == desc.len() || desc[end..].starts_with(' ');
            if !(before_ok && after_ok) {
                break;
            }
            desc.replace_range(index..end, " ");
        }
    }
    let kept: Vec<&str> = desc.split_whitespace().filter(|w| !words.contains(w)).collect();
    kept.join(" ").trim_end_matches(',').trim().to_owned()
}

/// Vendor/product from the udev database (`/run/udev/data/n<ifindex>`).
pub fn udev_description(ifindex: u32, interface: &str) -> String {
    if ifindex == 0 {
        return interface.to_owned();
    }
    let root = std::env::var("NETWORK_MANAGER_UDEV_DATA").unwrap_or_else(|_| "/run/udev/data".into());
    let Ok(text) = std::fs::read_to_string(format!("{root}/n{ifindex}")) else { return interface.to_owned() };
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(&format!("E:{key}=")).map(String::from));
    let vendor = value("ID_VENDOR_FROM_DATABASE").or_else(|| value("ID_VENDOR")).unwrap_or_default();
    let product = value("ID_MODEL_FROM_DATABASE").or_else(|| value("ID_MODEL")).unwrap_or_default();
    description(&vendor, &product, interface)
}

pub fn description(vendor: &str, product: &str, interface: &str) -> String {
    let vendor = fixup(vendor, VENDOR_PHRASES, VENDOR_WORDS, false);
    let product = fixup(product, PRODUCT_PHRASES, PRODUCT_WORDS, true);
    match (vendor.is_empty(), product.is_empty()) {
        (true, true) => interface.to_owned(),
        (true, false) => product,
        (false, true) => vendor,
        (false, false) if product.starts_with(&vendor) => product,
        (false, false) => format!("{vendor} {product}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_classifies_personal_enterprise_transition_and_owe() {
        for (flags, expected) in [(0, "open"), (KEY_MGMT_PSK, "wpa-personal"), (KEY_MGMT_SAE, "sae"),
                                  (KEY_MGMT_SAE | KEY_MGMT_PSK, "wpa-personal"), (KEY_MGMT_802_1X, "wpa-enterprise"),
                                  (KEY_MGMT_OWE, "owe")] {
            let privacy = if flags != 0 { AP_PRIVACY } else { 0 };
            assert_eq!(ap_security(privacy, 0, flags), expected);
        }
        assert_eq!(ap_security(AP_PRIVACY, 0, 0), "wep");
    }

    #[test]
    fn titles_and_reasons_follow_python() {
        assert_eq!(title("wifi-p2p"), "Wifi-P2P");
        assert_eq!(title("wired"), "Wired");
        assert_eq!(capitalize("no secrets"), "No secrets");
        assert_eq!(device_reason_name(7), "no secrets");
        assert_eq!(vpn_reason_name(10), "login failed");
        assert_eq!(device_kind(30), "wifi-p2p");
        assert_eq!(device_kind(32), "loopback");
        assert_eq!(device_kind(1), "wired");
    }

    #[test]
    fn descriptions_shorten_udev_names_like_libnm() {
        assert_eq!(description("Intel Corporation", "Ethernet Controller I226-V", "enp8s0"), "Intel I226-V");
        assert_eq!(description("Intel Corporation", "Wi-Fi 6E(802.11ax) AX210/AX1675* 2x2 [Typhoon Peak] (Wi-Fi 6 AX210 160MHz)", "wlp9s0"),
                   "Intel 6E AX210/AX1675* 2x2");
        assert_eq!(description("", "", "lo"), "lo");
    }
}
