//! NetworkManager D-Bus constants, enum tables (libnm value nicks) and the
//! pure model helpers of network_backend.py: security classification and IP
//! detail formatting. Device/profile compatibility, AP matching and device
//! descriptions are libnm's own (`backend::Matcher`, `libnm::Client`).

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

}
