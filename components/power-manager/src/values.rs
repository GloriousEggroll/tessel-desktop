//! GVariant → Python-like JSON values (what PyGObject's `unpack()` yields).

use glib::{Variant, VariantClass};
use serde_json::{Map, Number, Value};

pub fn to_value(variant: &Variant) -> Value {
    match variant.classify() {
        VariantClass::Boolean => Value::Bool(variant.get::<bool>().unwrap_or(false)),
        VariantClass::Byte => variant.get::<u8>().map(Value::from).unwrap_or(Value::Null),
        VariantClass::Int16 => variant.get::<i16>().map(Value::from).unwrap_or(Value::Null),
        VariantClass::Uint16 => variant.get::<u16>().map(Value::from).unwrap_or(Value::Null),
        VariantClass::Int32 => variant.get::<i32>().map(Value::from).unwrap_or(Value::Null),
        VariantClass::Uint32 => variant.get::<u32>().map(Value::from).unwrap_or(Value::Null),
        VariantClass::Int64 => variant.get::<i64>().map(Value::from).unwrap_or(Value::Null),
        VariantClass::Uint64 => variant.get::<u64>().map(Value::from).unwrap_or(Value::Null),
        VariantClass::Handle => variant.get::<glib::variant::Handle>().map(|h| Value::from(h.0)).unwrap_or(Value::Null),
        // Non-finite doubles cannot be JSON; every consumer rejects them anyway.
        VariantClass::Double => variant.get::<f64>().and_then(Number::from_f64).map(Value::Number).unwrap_or(Value::Null),
        VariantClass::String | VariantClass::ObjectPath | VariantClass::Signature => {
            Value::String(variant.str().unwrap_or_default().to_owned())
        }
        VariantClass::Variant => variant.as_variant().map(|inner| to_value(&inner)).unwrap_or(Value::Null),
        VariantClass::Maybe => variant.as_maybe().map(|inner| to_value(&inner)).unwrap_or(Value::Null),
        VariantClass::Array => {
            let element = variant.type_().element();
            if element.is_dict_entry() {
                let mut map = Map::new();
                for entry in variant.iter() {
                    let key = entry.child_value(0);
                    let key = match to_value(&key) {
                        Value::String(text) => text,
                        other => other.to_string(),
                    };
                    map.insert(key, to_value(&entry.child_value(1)));
                }
                Value::Object(map)
            } else if element.as_str() == "y" {
                // Python unpacks "ay" to bytes; represent as an array of ints.
                Value::Array(variant.iter().map(|b| to_value(&b)).collect())
            } else {
                Value::Array(variant.iter().map(|item| to_value(&item)).collect())
            }
        }
        VariantClass::Tuple | VariantClass::DictEntry => Value::Array(variant.iter().map(|item| to_value(&item)).collect()),
        _ => Value::Null,
    }
}

/// `value[0]` of an unpacked reply tuple.
pub fn first(variant: &Variant) -> Value {
    if variant.n_children() > 0 && (variant.is_container()) {
        to_value(&variant.child_value(0))
    } else {
        Value::Null
    }
}

/// Python `isinstance(value, (int, float)) and not bool` as f64.
pub fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Python `type(value) is int`.
pub fn int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) if !n.is_f64() => n.as_i64().or_else(|| n.as_u64().map(|v| v.min(i64::MAX as u64) as i64)),
        _ => None,
    }
}

/// Python `value is True`.
pub fn is_true(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(true)))
}

/// Python `value is False`.
pub fn is_false(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(false)))
}

/// Python `str(value or "")` for strings.
pub fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None | Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Python equality of two JSON-ish values, where 1 == 1.0 and True == 1.
pub fn py_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    fn numeric(value: &Value) -> Option<f64> {
        match value {
            Value::Bool(flag) => Some(*flag as i64 as f64),
            Value::Number(n) => n.as_f64(),
            _ => None,
        }
    }
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => match (numeric(x), numeric(y)) {
            (Some(p), Some(q)) => p == q,
            _ => x == y,
        },
        // A missing key equals None.
        (Some(Value::Null), None) | (None, Some(Value::Null)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::*;

    #[test]
    fn converts_dbus_shapes() {
        let dict = glib::VariantDict::new(None);
        dict.insert_value("OnBattery", &true.to_variant());
        dict.insert_value("Percentage", &50.0f64.to_variant());
        dict.insert_value("Type", &2u32.to_variant());
        dict.insert_value("Bad", &f64::NAN.to_variant());
        let value = to_value(&dict.end());
        assert_eq!(value["OnBattery"], true);
        assert_eq!(value["Percentage"].as_f64(), Some(50.0));
        assert!(value["Percentage"].is_f64());
        assert_eq!(value["Type"], 2);
        assert!(value["Bad"].is_null());
        let rows = vec![("idle".to_string(), "a".to_string(), "b".to_string(), "block".to_string(), 1u32, 2u32)];
        let reply = (rows,).to_variant();
        assert_eq!(first(&reply), serde_json::json!([["idle", "a", "b", "block", 1, 2]]));
        assert!(py_eq(Some(&serde_json::json!(1)), Some(&serde_json::json!(1.0))));
        assert!(py_eq(Some(&Value::Null), None));
    }
}
