//! Shared record shapes (`feature_state`, display lookups) and Python-like
//! value helpers over `serde_json::Value` (records are the D-Bus JSON).

use serde_json::{json, Value};

pub const FEATURES: [&str; 2] = ["brightness", "contrast"];
pub const UNAVAILABLE_CONTROL: &str = "Hardware controls are unavailable for this display connection.";

pub fn feature_code(feature: &str) -> Option<&'static str> {
    match feature {
        "brightness" => Some("10"),
        "contrast" => Some("12"),
        _ => None,
    }
}

/// `feature_state(supported, raw, maximum, error)`.
pub fn feature_state(supported: Option<bool>, raw: Option<i64>, maximum: Option<i64>, error: &str) -> Value {
    let value = match (raw, maximum) {
        (Some(raw), Some(maximum)) if maximum != 0 => json!(100.0 * raw as f64 / maximum as f64),
        _ => Value::Null,
    };
    json!({"supported": supported, "value": value, "raw": raw, "max": maximum, "busy": false, "error": error})
}

/// `type(x) in (int, float)` and finite (JSON numbers are never bool).
pub fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite())
}

/// `type(x) is int`.
pub fn integer(value: &Value) -> Option<i64> {
    if value.is_i64() || value.is_u64() { value.as_i64() } else { None }
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Python truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(n) => n.as_f64().is_some_and(|v| v != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `record.get(key, {}).get("supported") is True`.
pub fn supported(record: &Value, feature: &str) -> bool {
    record.get(feature).and_then(|f| f.get("supported")) == Some(&Value::Bool(true))
}

/// `record.get("enabled", default)` as Python truthiness.
pub fn enabled(record: &Value, default: bool) -> bool {
    record.get("enabled").map(truthy).unwrap_or(default)
}

/// Python `round()` (half to even) for floats.
pub fn py_round(value: f64) -> f64 {
    value.round_ties_even()
}

pub type Callback = Box<dyn FnOnce(bool, String)>;

pub fn call(callback: Option<Callback>, success: bool, error: &str) {
    if let Some(callback) = callback {
        callback(success, error.to_owned());
    }
}
