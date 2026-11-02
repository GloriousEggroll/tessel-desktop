//! Battery/plug status for Waybar and Tessel Dock (`battery_status.py`), without any
//! automatic power actions. Printed as JSON lines, only when changed.

use tessel_core::values::text;
use tessel_power_core::BatteryModel;
use serde_json::{json, Value};
use tessel_core::pyfmt::{format_value, json_dumps, JsonStyle};
use tessel_core::{tr, Arg};

/// A percentage `0 <= value <= 100` (finite), as Python's validity check.
pub fn valid_percentage(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
}

/// Python `f"{value:.0f}"` (round half to even).
pub fn whole(value: f64) -> String {
    format_value(&Arg::from(value), ".0f").unwrap_or_else(|_| format!("{value:.0}"))
}

/// Python `f"{value:.0f}"` for an int-or-float JSON number.
pub fn whole_value(value: &Value) -> String {
    match value.as_i64() {
        Some(int) if !value.is_f64() => int.to_string(),
        _ => whole(value.as_f64().unwrap_or(0.0)),
    }
}

/// One device line: `"<name>: <NN%|level|Level unavailable> · <state|Status unavailable>"`.
fn device_line(device: &Value) -> String {
    let percent = device.get("percentage").filter(|v| v.is_number());
    let level = match percent {
        Some(value) => format!("{}%", whole_value(value)),
        None => {
            let level = text(device.get("level"));
            let translated = if level.is_empty() { String::new() } else { tr!(level) };
            if translated.is_empty() { tr!("Level unavailable") } else { translated }
        }
    };
    let state = text(device.get("state"));
    let state = tr!(if state.is_empty() { "Status unavailable".to_owned() } else { state });
    format!("{}: {level} · {state}", text(device.get("name")))
}

pub fn status(backend: &dyn BatteryModel) -> Value {
    let known = backend.power_ready() && backend.power_source_known();
    let value = valid_percentage(backend.percentage());
    let (glyph, mut summary, css) = if !known {
        ("\u{f244}", tr!("Power source unavailable"), "unavailable")
    } else if !backend.on_battery() {
        ("\u{f1e6}", tr!("Plugged in"), "plugged")
    } else if let Some(value) = value {
        let index = if value <= 10.0 { 0 } else if value <= 25.0 { 1 } else if value <= 50.0 { 2 } else if value <= 80.0 { 3 } else { 4 };
        let glyph = ["\u{f244}", "\u{f243}", "\u{f242}", "\u{f241}", "\u{f240}"][index];
        let css = if value <= 10.0 { "critical" } else if value <= 25.0 { "low" } else if value >= 95.0 { "full" } else { "battery" };
        (glyph, tr!("On battery · {percent:.0f}%", percent = value), css)
    } else {
        ("\u{f244}", tr!("On battery · Level unavailable"), "unavailable")
    };
    if known && !backend.on_battery() && backend.has_battery() {
        if let Some(value) = value {
            summary += &format!(" · {}%", whole(value));
        }
    }
    let mut lines = vec![summary];
    lines.extend(backend.batteries().iter().map(device_line));
    lines.push(tr!("Click for batteries · Right-click for charging settings"));
    json!({"text": glyph, "tooltip": lines.join("\n"), "class": [css]})
}

/// `json.dumps(status(backend))` (ASCII escapes, `", "` separators).
pub fn encode(backend: &dyn BatteryModel) -> String {
    json_dumps(&status(backend), &JsonStyle::DEFAULT)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::cell::RefCell;

    /// The Python tests' `SimpleNamespace` backend.
    pub struct Fake {
        pub power_ready: bool,
        pub power_source_known: bool,
        pub percentage: Option<f64>,
        pub has_battery: bool,
        pub on_battery: bool,
        pub batteries: Vec<Value>,
        pub error: String,
        pub charge_limit: Value,
        pub calls: RefCell<Vec<String>>,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self { power_ready: true, power_source_known: true, percentage: Some(72.0), has_battery: true, on_battery: true,
                   batteries: vec![], error: String::new(), charge_limit: json!({"supported": false, "maximum": null}),
                   calls: RefCell::new(vec![]) }
        }
    }

    impl BatteryModel for Fake {
        fn power_ready(&self) -> bool { self.power_ready }
        fn power_source_known(&self) -> bool { self.power_source_known }
        fn on_battery(&self) -> bool { self.on_battery }
        fn has_battery(&self) -> bool { self.has_battery }
        fn percentage(&self) -> Option<f64> { self.percentage }
        fn busy(&self) -> bool { false }
        fn error(&self) -> String { self.error.clone() }
        fn batteries(&self) -> Vec<Value> { self.batteries.clone() }
        fn charge_limit(&self) -> Value { self.charge_limit.clone() }
        fn set_charge_thresholds(&self, path: &str, _: bool) -> bool { self.calls.borrow_mut().push(path.into()); false }
        fn set_charge_limit(&self, _: &Value) -> bool { self.calls.borrow_mut().push("limit".into()); false }
    }

    fn tooltip(result: &Value) -> &str {
        result["tooltip"].as_str().unwrap()
    }

    #[test]
    fn ac_power_has_plug_icon_and_retains_laptop_charge() {
        let result = status(&Fake { on_battery: false, ..Fake::default() });
        assert_eq!(result["text"], "\u{f1e6}");
        assert_eq!(result["class"], json!(["plugged"]));
        assert!(tooltip(&result).starts_with("Plugged in · 72%\n"));
        assert!(tooltip(&result).contains("Right-click for charging settings"));
    }

    #[test]
    fn battery_charge_ranges_select_icons_and_warning_classes() {
        let cases = [(100, "\u{f240}", "full"), (95, "\u{f240}", "full"), (94, "\u{f240}", "battery"), (80, "\u{f241}", "battery"),
                     (72, "\u{f241}", "battery"), (50, "\u{f242}", "battery"), (26, "\u{f242}", "battery"), (25, "\u{f243}", "low"),
                     (11, "\u{f243}", "low"), (10, "\u{f244}", "critical"), (0, "\u{f244}", "critical")];
        for (percent, glyph, css) in cases {
            let result = status(&Fake { percentage: Some(percent as f64), ..Fake::default() });
            assert_eq!(result["text"], glyph, "{percent}");
            assert_eq!(result["class"], json!([css]));
            assert!(tooltip(&result).starts_with(&format!("On battery · {percent}%\n")));
        }
    }

    #[test]
    fn unknown_and_invalid_charge_never_invent_a_percentage() {
        // Python's False / "72" cases cannot be represented by Option<f64>.
        for percent in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-1.0), Some(101.0)] {
            let result = status(&Fake { percentage: percent, ..Fake::default() });
            assert_eq!(result["text"], "\u{f244}");
            assert_eq!(result["class"], json!(["unavailable"]));
            assert!(tooltip(&result).contains("On battery · Level unavailable"));
            assert!(!tooltip(&result).contains('%'));
            let result = status(&Fake { percentage: percent, on_battery: false, ..Fake::default() });
            assert_eq!(result["text"], "\u{f1e6}");
            assert_eq!(result["class"], json!(["plugged"]));
            assert!(tooltip(&result).starts_with("Plugged in\n"));
        }
    }

    #[test]
    fn unknown_source_does_not_reuse_stale_ac_or_battery_state() {
        for (ready, known) in [(false, true), (true, false)] {
            for on_battery in [false, true] {
                let result = status(&Fake { power_ready: ready, power_source_known: known, on_battery, ..Fake::default() });
                assert_eq!(result["class"], json!(["unavailable"]));
                let text = tooltip(&result);
                assert!(text.contains("Power source unavailable"));
                assert!(!text.contains("Plugged in") && !text.contains("On battery") && !text.contains('%'));
            }
        }
    }

    #[test]
    fn peripheral_only_desktop_keeps_plug_icon_and_device_levels() {
        let result = status(&Fake {
            has_battery: false, on_battery: false, percentage: None,
            batteries: vec![json!({"name": "Controller", "percentage": 100, "state": "Fully charged"}),
                            json!({"name": "Mouse", "percentage": null, "level": "Low", "state": "Discharging"}),
                            json!({"name": "Headphones", "percentage": null})],
            ..Fake::default()
        });
        assert_eq!(result["text"], "\u{f1e6}");
        assert_eq!(result["class"], json!(["plugged"]));
        let text = tooltip(&result);
        assert!(text.starts_with("Plugged in\n"));
        assert!(text.contains("Controller: 100% · Fully charged"));
        assert!(text.contains("Mouse: Low · Discharging"));
        assert!(text.contains("Headphones: Level unavailable · Status unavailable"));
        assert!(!text.contains("On battery"));
    }

    #[test]
    fn encoding_matches_python_json_dumps() {
        let fake = Fake { on_battery: false, percentage: Some(72.5), batteries: vec![json!({"name": "Mouse", "percentage": 41.6, "state": "Charging"})], ..Fake::default() };
        assert_eq!(encode(&fake), "{\"text\": \"\\uf1e6\", \"tooltip\": \"Plugged in \\u00b7 72%\\nMouse: 42% \\u00b7 Charging\\nClick for batteries \\u00b7 Right-click for charging settings\", \"class\": [\"plugged\"]}");
    }
}
