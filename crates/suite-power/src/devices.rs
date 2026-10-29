//! UPower device normalization (`power_backend.py:28-142`). Devices are Python
//! dicts: `serde_json` objects keyed by UPower property names.

use suite_core::values::{int, int_in, is_true, number, text};
use serde_json::{json, Map, Value};

pub const UPOWER: (&str, &str, &str) = ("org.freedesktop.UPower", "/org/freedesktop/UPower", "org.freedesktop.UPower");
pub const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";
pub const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

pub fn device_kind(kind: i64) -> Option<&'static str> {
    Some(match kind {
        2 => "Battery", 3 => "UPS", 4 => "Monitor", 5 => "Mouse", 6 => "Keyboard",
        7 => "PDA", 8 => "Phone", 9 => "Media player", 10 => "Tablet", 11 => "Computer",
        12 => "Game controller", 13 => "Pen", 14 => "Touchpad", 15 => "Modem",
        16 => "Network device", 17 => "Headset", 18 => "Speakers", 19 => "Headphones",
        20 => "Video device", 21 => "Audio device", 22 => "Remote control", 23 => "Printer",
        24 => "Scanner", 25 => "Camera", 26 => "Wearable", 27 => "Toy", 28 => "Bluetooth device",
        _ => return None,
    })
}

pub fn battery_state(state: i64) -> Option<&'static str> {
    Some(match state {
        0 => "Unknown", 1 => "Charging", 2 => "Discharging", 3 => "Empty",
        4 => "Fully charged", 5 => "Waiting to charge", 6 => "Waiting to discharge",
        _ => return None,
    })
}

pub fn battery_level(level: i64) -> Option<&'static str> {
    Some(match level {
        0 => "Unknown", 3 => "Low", 4 => "Critical", 6 => "Normal", 7 => "High", 8 => "Full",
        _ => return None,
    })
}

pub fn battery_warning(level: i64) -> Option<&'static str> {
    Some(match level {
        3 => "Low battery", 4 => "Critical battery", 5 => "Battery almost empty",
        _ => return None,
    })
}

/// `_number(value, minimum, maximum)`: the value itself, or `None`.
pub fn bounded(value: Option<&Value>, minimum: f64, maximum: Option<f64>) -> Value {
    match number(value) {
        Some(n) if n >= minimum && maximum.is_none_or(|m| n <= m) => value.cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// `_device_identity`: the properties that identify one physical battery.
pub fn device_identity(properties: &Value) -> Vec<Value> {
    ["NativePath", "Vendor", "Model", "Serial", "Type", "PowerSupply"].iter()
        .map(|key| properties.get(*key).cloned().unwrap_or(Value::Null)).collect()
}

/// Python `os.path.basename`.
pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

/// Ignore headset/controller/mouse batteries, even if they report charge.
pub fn is_system_battery(device: &Value) -> bool {
    int_in(device.get("Type"), &[2, 3]) && is_true(device.get("PowerSupply")) && is_true(device.get("IsPresent"))
}

pub fn system_batteries<'a>(devices: impl IntoIterator<Item = &'a Value>) -> Vec<&'a Value> {
    devices.into_iter().filter(|device| is_system_battery(device)).collect()
}

/// Energy-weighted charge over system supplies, else the mean Percentage; clamped 0–100.
pub fn battery_percentage(batteries: &[&Value]) -> Option<f64> {
    if batteries.is_empty() {
        return None;
    }
    let energies: Vec<(Option<f64>, Option<f64>)> = batteries.iter()
        .map(|item| (number(item.get("Energy")), number(item.get("EnergyFull")))).collect();
    let value = if energies.iter().all(|(energy, full)| energy.is_some_and(|e| e >= 0.0) && full.is_some_and(|f| f > 0.0)) {
        let energy: f64 = energies.iter().map(|e| e.0.unwrap()).sum();
        let full: f64 = energies.iter().map(|e| e.1.unwrap()).sum();
        100.0 * energy / full
    } else {
        let values: Vec<Option<f64>> = batteries.iter().map(|item| number(item.get("Percentage"))).collect();
        if !values.iter().all(|v| v.is_some_and(|v| (0.0..=100.0).contains(&v))) {
            return None;
        }
        values.iter().map(|v| v.unwrap()).sum::<f64>() / values.len() as f64
    };
    Some(value.clamp(0.0, 100.0))
}

/// Normalize real UPower devices without treating peripherals as supplies.
///
/// BatteryLevel takes precedence over approximate Percentage values. Threshold
/// percentages are read-only; EnableChargeThreshold is the supported control.
pub fn device_batteries(devices: &Map<String, Value>, busy_path: Option<&str>) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for (path, properties) in devices {
        let kind_value = properties.get("Type").cloned().unwrap_or(json!(0));
        let kind = int(Some(&kind_value)).unwrap_or(-1);
        if path == DISPLAY_DEVICE || int_in(Some(&kind_value), &[0, 1])
            || matches!(properties.get("IsPresent"), Some(Value::Bool(false))) {
            continue;
        }
        let native = text(properties.get("NativePath"));
        let vendor = text(properties.get("Vendor")).trim().to_owned();
        let model = text(properties.get("Model")).trim().to_owned();
        let mut name = if !model.is_empty() { model.clone() } else if !vendor.is_empty() { vendor.clone() }
                       else if kind == 2 { basename(&native).to_owned() } else { String::new() };
        if !vendor.is_empty() && !model.is_empty() && !model.to_lowercase().starts_with(&vendor.to_lowercase()) {
            name = format!("{vendor} {model}");
        }
        let level_code = match properties.get("BatteryLevel") { None => Some(1), Some(v) => int(Some(v)) };
        let coarse = level_code != Some(1);
        let percentage = if coarse { Value::Null } else { bounded(properties.get("Percentage"), 0.0, Some(100.0)) };
        let supply = is_system_battery(properties);
        let thresholds = supply && kind == 2 && is_true(properties.get("ChargeThresholdSupported"));
        let threshold_enabled = match properties.get("ChargeThresholdEnabled") { Some(Value::Bool(b)) => Value::Bool(*b), _ => Value::Null };
        let mut start = bounded(properties.get("ChargeStartThreshold"), 0.0, Some(100.0));
        let mut end = bounded(properties.get("ChargeEndThreshold"), 0.0, Some(100.0));
        // Newer UPower explicitly identifies thresholds versus firmware-managed
        // optimized charging. Avoid describing dummy percentages as a real limit.
        if let Some(settings) = int(properties.get("ChargeThresholdSettingsSupported")) {
            if settings & 1 == 0 {
                start = Value::Null;
            }
            if settings & 2 == 0 {
                end = Value::Null;
            }
        }
        let kind_name = device_kind(kind).unwrap_or("Battery device");
        let state_value = properties.get("State").cloned().unwrap_or(json!(0));
        let icon = text(properties.get("IconName"));
        let tail = path.rsplit('/').next().unwrap_or("");
        result.push(json!({
            "path": path,
            "name": if name.is_empty() { kind_name.to_owned() } else { name },
            "kind": kind_name, "type": kind_value,
            "icon_name": if icon.is_empty() { "battery-symbolic".to_owned() } else { icon },
            "percentage": percentage,
            "level": if coarse { level_code.and_then(battery_level).unwrap_or("Unknown") } else { "" },
            "state": int(Some(&state_value)).and_then(battery_state).unwrap_or("Unknown"),
            "state_code": state_value, "power_supply": supply,
            "rechargeable": is_true(properties.get("IsRechargeable")),
            "time_to_empty": bounded(properties.get("TimeToEmpty"), 1.0, None),
            "time_to_full": bounded(properties.get("TimeToFull"), 1.0, None),
            "health": bounded(properties.get("Capacity"), 0.01, Some(100.0)),
            "cycles": bounded(properties.get("ChargeCycles"), 0.0, None),
            "warning": int(properties.get("WarningLevel")).and_then(battery_warning).unwrap_or(""),
            "bluetooth": kind == 28 || native.starts_with("/org/bluez/") || tail.starts_with("bluez_"),
            "charge_threshold_supported": thresholds,
            "charge_threshold_enabled": if thresholds { threshold_enabled } else { Value::Null },
            "charge_start_threshold": if thresholds { start } else { Value::Null },
            "charge_end_threshold": if thresholds { end } else { Value::Null },
            "busy": busy_path == Some(path.as_str()),
        }));
    }
    result.sort_by(|a, b| {
        let key = |item: &Value| (!is_true(item.get("power_supply")), text(item.get("name")).to_lowercase(), text(item.get("path")));
        key(a).cmp(&key(b))
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn one(properties: Value) -> Value {
        device_batteries(&map(json!({"/device": properties})), None).remove(0)
    }

    #[test]
    fn devices_filter_synthetic_supply_and_absent_batteries() {
        let devices = map(json!({
            "/org/freedesktop/UPower/devices/DisplayDevice": {"Type": 2, "IsPresent": true},
            "/ac": {"Type": 1, "Online": true},
            "/absent": {"Type": 2, "IsPresent": false},
            "/controller": {"Type": 12, "IsPresent": true, "Model": "Controller",
                            "Percentage": 100.0, "BatteryLevel": 1, "State": 4},
            "/battery": {"Type": 2, "IsPresent": true, "PowerSupply": true, "NativePath": "BAT0", "Percentage": 50.0},
        }));
        let result = device_batteries(&devices, None);
        let paths: Vec<_> = result.iter().map(|d| d["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["/battery", "/controller"]);
        assert_eq!(result[0]["name"], "BAT0");
        assert_eq!(result[1]["state"], "Fully charged");
        assert_eq!(result[1]["percentage"], 100.0);
        assert!(result[1]["health"].is_null());
        assert!(result[1]["cycles"].is_null());
        assert_eq!(result[1]["charge_threshold_supported"], false);
    }

    #[test]
    fn coarse_unknown_and_invalid_percentages_are_not_fabricated() {
        for (level, expected) in [(0, "Unknown"), (3, "Low"), (4, "Critical"), (8, "Full")] {
            let result = one(json!({"Type": 5, "BatteryLevel": level, "Percentage": 50.0}));
            assert_eq!(result["level"], expected);
            assert!(result["percentage"].is_null());
        }
        // NaN/inf arrive as missing values (see values::to_value).
        for percentage in [Value::Null, json!(-1), json!(101), json!(true), json!("50")] {
            let result = one(json!({"Type": 5, "Percentage": percentage}));
            assert!(result["percentage"].is_null());
        }
    }

    #[test]
    fn unknown_health_duration_and_cycle_sentinels_are_hidden() {
        let result = one(json!({"Type": 12, "Capacity": 0.0, "TimeToEmpty": 0, "TimeToFull": -1, "ChargeCycles": -1}));
        for key in ["health", "time_to_empty", "time_to_full", "cycles"] {
            assert!(result[key].is_null(), "{key}");
        }
        let result = one(json!({"Type": 2, "Capacity": 93.0, "TimeToEmpty": 7200, "TimeToFull": 3600, "ChargeCycles": 123}));
        assert_eq!((result["health"].as_f64(), result["time_to_empty"].as_i64(), result["time_to_full"].as_i64(),
                    result["cycles"].as_i64()), (Some(93.0), Some(7200), Some(3600), Some(123)));
    }

    #[test]
    fn firmware_charge_control_does_not_display_dummy_percentages() {
        let mut values = json!({"Type": 2, "IsPresent": true, "PowerSupply": true,
            "ChargeThresholdSupported": true, "ChargeThresholdEnabled": true,
            "ChargeThresholdSettingsSupported": 4, "ChargeStartThreshold": 0, "ChargeEndThreshold": 100});
        let result = one(values.clone());
        assert_eq!(result["charge_threshold_supported"], true);
        assert!(result["charge_start_threshold"].is_null());
        assert!(result["charge_end_threshold"].is_null());
        let object = values.as_object_mut().unwrap();
        object.remove("ChargeThresholdSettingsSupported"); // Older UPower supported limits.
        object.insert("ChargeStartThreshold".into(), json!(4294967295u64)); // Unsupported sentinel.
        object.insert("ChargeEndThreshold".into(), json!(80));
        let result = one(values);
        assert!(result["charge_start_threshold"].is_null());
        assert_eq!(result["charge_end_threshold"], 80);
    }

    #[test]
    fn bluetooth_requires_known_transport_not_wireless_product_name() {
        for (native, kind, expected) in [("/org/bluez/hci0/dev_01", 17, true), ("", 28, true),
                                         ("ps-controller-battery-aa:bb:cc:dd:ee:ff", 12, false)] {
            let result = one(json!({"Type": kind, "NativePath": native, "Model": "Wireless controller"}));
            assert_eq!(result["bluetooth"], expected);
        }
        let result = device_batteries(&map(json!({"/org/freedesktop/UPower/devices/headset_bluez_00": {"Type": 17}})), None);
        assert_eq!(result[0]["bluetooth"], false);
        let result = device_batteries(&map(json!({"/org/freedesktop/UPower/devices/bluez_00": {"Type": 17}})), None);
        assert_eq!(result[0]["bluetooth"], true);
    }

    #[test]
    fn names_kinds_and_sort_order() {
        let devices = map(json!({
            "/b": {"Type": 5, "Vendor": "Logitech", "Model": "MX Master"},
            "/a": {"Type": 6, "Vendor": "Logitech", "Model": "logitech K380"},
            "/c": {"Type": 99},
            "/d": {"Type": 2, "IsPresent": true, "PowerSupply": true, "NativePath": "/sys/x/BAT1", "State": 2, "WarningLevel": 3},
        }));
        let result = device_batteries(&devices, Some("/b"));
        let names: Vec<_> = result.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["BAT1", "Battery device", "logitech K380", "Logitech MX Master"]);
        assert_eq!(result[0]["state"], "Discharging");
        assert_eq!(result[0]["warning"], "Low battery");
        assert_eq!(result[1]["kind"], "Battery device");
        assert_eq!(result[3]["busy"], true);
        assert_eq!(result[3]["icon_name"], "battery-symbolic");
    }

    #[test]
    fn invalid_battery_percentages_are_not_displayed() {
        assert_eq!(battery_percentage(&[&json!({"Percentage": null})]), None);
        assert_eq!(battery_percentage(&[&json!({"Percentage": 101})]), None);
        assert_eq!(battery_percentage(&[&json!({"Percentage": 80}), &json!({"Percentage": 20})]), Some(50.0));
        assert!(system_batteries([&json!({"Type": 5, "PowerSupply": true, "IsPresent": true})]).is_empty());
        let weighted = [json!({"Energy": 10.0, "EnergyFull": 20.0, "Percentage": 50.0}),
                        json!({"Energy": 60.0, "EnergyFull": 80.0, "Percentage": 75.0})];
        assert_eq!(battery_percentage(&weighted.iter().collect::<Vec<_>>()), Some(70.0));
    }
}
