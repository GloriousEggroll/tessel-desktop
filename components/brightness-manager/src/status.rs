//! `brightness_status.py`: the read-only Waybar/Waydock indicator object.
//! Never probes or changes monitor hardware.

use crate::model::{self, number};
use serde_json::{json, Value};
use suite_core::pyfmt::{format_value, json_dumps, Arg, JsonStyle};
use suite_core::tr;

pub fn status(state: &Value, ready: bool) -> Value {
    let mut text = vec![tr!("Displays")];
    let empty = Vec::new();
    for display in state.get("displays").and_then(Value::as_array).unwrap_or(&empty) {
        let brightness = display.get("brightness").cloned().unwrap_or(json!({}));
        let mut details = Vec::new();
        if let Some(value) = brightness.get("value").and_then(number).filter(|_| model::truthy(brightness.get("supported").unwrap_or(&Value::Null))) {
            details.push(tr!("Brightness: {value}%", value = format_value(&Arg::Float(value), ".0f").unwrap_or_default()));
        }
        let scale = display.get("scale").cloned().unwrap_or(json!({}));
        if let Some(value) = scale.get("value").and_then(number).filter(|_| model::truthy(scale.get("supported").unwrap_or(&Value::Null))) {
            details.push(tr!("Scale: {value}%", value = format_value(&Arg::Float(value * 100.0), "g").unwrap_or_default()));
        }
        let connector = model::text(display, "connector");
        text.push(if details.is_empty() { connector.to_owned() } else { format!("{connector}: {}", details.join(" · ")) });
    }
    let enabled = state.get("settings").and_then(|s| s.get("auto_dim")).map(model::truthy).unwrap_or(false);
    text.push(tr!("Auto-dim: {value1}", value1 = if enabled { tr!("On") } else { tr!("Off") }));
    text.push(tr!("Click for display controls · Right-click for display settings"));
    let class = if !ready {
        "unavailable"
    } else if state.get("dimmed").is_some_and(model::truthy) {
        "dimmed"
    } else if enabled {
        "auto"
    } else {
        "manual"
    };
    json!({"text": "\u{f108}", "tooltip": text.join("\n"), "class": class})
}

pub fn line(state: &Value, ready: bool) -> String {
    json_dumps(&status(state, ready), &JsonStyle::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_and_classes_match_python() {
        let initial = json!({"ready": false, "displays": [], "settings": {"auto_dim": false, "dim_after": 120, "dim_percent": 30}});
        assert_eq!(line(&initial, false),
                   "{\"text\": \"\\uf108\", \"tooltip\": \"Displays\\nAuto-dim: Off\\nClick for display controls \\u00b7 Right-click for display settings\", \"class\": \"unavailable\"}");
        let state = json!({"displays": [
            {"connector": "DP-2", "brightness": {"supported": true, "value": 74.6}, "scale": {"supported": true, "value": 1.125}},
            {"connector": "HDMI-A-1", "brightness": {"supported": false, "value": null}, "scale": {"supported": false, "value": 1.0}}],
            "settings": {"auto_dim": true}, "dimmed": false});
        let result = status(&state, true);
        assert_eq!(result["tooltip"], "Displays\nDP-2: Brightness: 75% · Scale: 112.5%\nHDMI-A-1\nAuto-dim: On\nClick for display controls · Right-click for display settings");
        assert_eq!(result["class"], "auto");
        let mut dimmed = state.clone();
        dimmed["dimmed"] = true.into();
        assert_eq!(status(&dimmed, true)["class"], "dimmed");
        dimmed["settings"]["auto_dim"] = false.into();
        dimmed["dimmed"] = false.into();
        assert_eq!(status(&dimmed, true)["class"], "manual");
    }
}
