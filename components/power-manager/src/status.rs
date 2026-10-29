//! Read-only power status for Waybar/Waydock streams (power_status.py).

use suite_power::backend::BackendState;
use serde_json::{json, Value};
use suite_core::tr;

fn profile_name(profile: &str) -> String {
    match profile {
        "power-saver" => tr!("Power Saver"),
        "balanced" => tr!("Balanced"),
        "performance" => tr!("Performance"),
        "" => tr!("Unavailable"),
        other => other.to_owned(),
    }
}

/// The JSON object printed per line. Never mentions batteries.
pub fn status(state: &BackendState) -> Value {
    let profile = if state.profiles_ready { state.active_profile.as_str() } else { "" };
    let mut description = tr!("Power profile: {value1}", value1 = profile_name(profile));
    if !state.performance_degraded.is_empty() {
        description += &tr!("\nPerformance limited: {value1}", value1 = state.performance_degraded.replace('-', " "));
    }
    description += "\n";
    description += &tr!("Click for power profiles · Right-click for settings");
    json!({"text": "\u{f135}", "tooltip": description, "class": [if profile.is_empty() { "unavailable" } else { profile }]})
}

pub fn line(state: &BackendState) -> String {
    suite_core::pyfmt::json_dumps(&status(state), &suite_core::pyfmt::JsonStyle::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_tooltip_keeps_profile_status_separate_from_batteries() {
        let mut state = BackendState { active_profile: "performance".into(), profiles_ready: true, power_source_known: true,
                                       ..Default::default() };
        let result = status(&state);
        assert_eq!(result["text"], "\u{f135}");
        let tooltip = result["tooltip"].as_str().unwrap().to_owned();
        assert!(tooltip.contains("Performance"));
        assert!(!tooltip.contains('%'));
        state.batteries = vec![json!({"name": "Controller", "percentage": 100}), json!({"name": "Mouse", "level": "Low"})];
        assert_eq!(status(&state), result);
        assert!(!tooltip.contains("Plugged in") && !tooltip.contains("Controller") && !tooltip.contains("Mouse"));
        assert_eq!(result["class"], json!(["performance"]));
        state.has_battery = true;
        state.on_battery = true;
        state.percentage = Some(42.0);
        assert_eq!(status(&state), result);
        assert_eq!(line(&state), "{\"text\": \"\\uf135\", \"tooltip\": \"Power profile: Performance\\nClick for power profiles \\u00b7 Right-click for settings\", \"class\": [\"performance\"]}");
    }

    #[test]
    fn unavailable_and_degraded() {
        let state = BackendState { performance_degraded: "high-operating-temperature".into(), ..Default::default() };
        let result = status(&state);
        assert_eq!(result["class"], json!(["unavailable"]));
        assert_eq!(result["tooltip"], "Power profile: Unavailable\nPerformance limited: high operating temperature\nClick for power profiles · Right-click for settings");
    }
}
