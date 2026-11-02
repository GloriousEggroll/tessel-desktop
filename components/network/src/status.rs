//! `network_status.py`: the Waybar/tessel-dock status line.

use crate::backend::State;
use serde_json::{json, Value};
use tessel_core::tr;

pub fn status(backend: &State) -> Value {
    let active: Vec<_> = backend.active.values().filter(|a| a.state == 2 && a.kind != "loopback").collect();
    let vpn: Vec<_> = active.iter().filter(|a| a.vpn || a.kind == "wireguard").collect();
    let primary = active.iter().find(|a| (a.default || a.default6) && !a.vpn).or_else(|| active.iter().find(|a| !a.vpn));
    let (mut text, mut description, mut classes): (String, String, Vec<String>);
    if !backend.ready {
        (text, description, classes) = ("󰤭".into(), tr!("NetworkManager unavailable"), vec!["unavailable".into()]);
    } else if !backend.networking_enabled {
        (text, description, classes) = ("󰤭".into(), tr!("Networking is off"), vec!["disabled".into()]);
    } else if let Some(primary) = primary {
        text = if primary.kind == "wifi" {
            let strength = backend.networks.values().filter(|n| n.active).map(|n| n.strength).max().unwrap_or(0);
            if strength < 25 { "󰤟" } else if strength < 50 { "󰤢" } else if strength < 75 { "󰤥" } else { "󰤨" }
        } else if primary.kind == "mobile" { "󰒧" } else { "󰈀" }.into();
        description = tr!("Connected: {value1}", value1 = primary.name.as_str());
        classes = vec!["connected".into(), primary.kind.clone()];
    } else if backend.active.values().any(|a| a.state == 1) {
        (text, description, classes) = ("󰤡".into(), tr!("Connecting…"), vec!["connecting".into()]);
    } else {
        (text, description, classes) = ("󰤭".into(), tr!("Not connected"), vec!["disconnected".into()]);
    }
    if !vpn.is_empty() {
        text += " 󰌾";
        classes.push("vpn".into());
        description += &format!("\nVPN: {}", vpn.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "));
    }
    if backend.ready && (backend.connectivity == "Limited connectivity" || backend.connectivity == "Sign-in required") {
        classes.push("limited".into());
        description += &format!("\n{}", tessel_core::i18n::translate(&backend.connectivity));
    }
    for device in backend.devices.values() {
        if device.kind != "loopback" && device.kind != "wifi-p2p" {
            description += &format!("\n{}: {}", device.interface, tessel_core::i18n::translate(&device.state_label));
        }
    }
    description += &format!("\n{}", tr!("Click for network controls · Right-click for settings"));
    json!({"text": text, "tooltip": description, "class": classes})
}

/// `json.dumps(status, ensure_ascii=True)`.
pub fn line(backend: &State) -> String {
    tessel_core::pyfmt::json_dumps(&status(backend), &tessel_core::pyfmt::JsonStyle::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn unavailable_disabled_and_connected_lines_match_python() {
        let mut state = State::new();
        assert_eq!(line(&state), "{\"text\": \"\\udb82\\udd2d\", \"tooltip\": \"NetworkManager unavailable\\nClick for network controls \\u00b7 Right-click for settings\", \"class\": [\"unavailable\"]}");
        state.ready = true;
        assert_eq!(status(&state)["class"], serde_json::json!(["disabled"]));
        state.networking_enabled = true;
        let mut wifi = ActiveConnection::new("/a/1", "u", "Home", "wifi");
        wifi.state = 2;
        wifi.default = true;
        state.active.insert("/a/1", wifi);
        let mut vpn = ActiveConnection::new("/a/2", "v", "Work", "vpn");
        vpn.state = 2;
        vpn.vpn = true;
        state.active.insert("/a/2", vpn);
        let mut network = WifiNetwork::new("n", "/d/1", b"Home", "Home", "wpa-personal", 60, "/ap");
        network.active = true;
        state.networks.insert("n", network);
        let mut device = Device::new("/d/1", "wlan0", "Wi-Fi", "wifi");
        device.state_label = "Connected".into();
        state.devices.insert("/d/1", device);
        state.devices.insert("/d/2", Device::new("/d/2", "lo", "lo", "loopback"));
        state.connectivity = "Sign-in required".into();
        let value = status(&state);
        assert_eq!(value["text"], "󰤥 󰌾");
        assert_eq!(value["class"], serde_json::json!(["connected", "wifi", "vpn", "limited"]));
        assert_eq!(value["tooltip"], "Connected: Home\nVPN: Work\nSign-in required\nwlan0: Connected\nClick for network controls · Right-click for settings");
    }

    #[test]
    fn connecting_and_wired_glyphs() {
        let mut state = State::new();
        state.ready = true;
        state.networking_enabled = true;
        let mut pending = ActiveConnection::new("/a/1", "u", "Office", "wired");
        pending.state = 1;
        state.active.insert("/a/1", pending.clone());
        assert_eq!(status(&state)["text"], "󰤡");
        pending.state = 2;
        state.active.insert("/a/1", pending);
        assert_eq!(status(&state)["text"], "󰈀");
        assert_eq!(status(&state)["class"], serde_json::json!(["connected", "wired"]));
    }
}
