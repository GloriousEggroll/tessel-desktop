//! GVariant → Python-like JSON values (what PyGObject's `unpack()` yields; `ay`
//! becomes a list of ints, non-finite doubles become `None`) and the Python
//! `type()`/truthiness checks D-Bus clients apply to optional dict entries.

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
        // Non-finite doubles cannot be JSON; every consumer rejects them anyway
        // (Python's `math.isfinite` checks), so they become "missing".
        VariantClass::Double => variant.get::<f64>().and_then(Number::from_f64).map(Value::Number).unwrap_or(Value::Null),
        VariantClass::String | VariantClass::ObjectPath | VariantClass::Signature => {
            Value::String(variant.str().unwrap_or_default().to_owned())
        }
        VariantClass::Variant => variant.as_variant().map(|inner| to_value(&inner)).unwrap_or(Value::Null),
        VariantClass::Maybe => variant.as_maybe().map(|inner| to_value(&inner)).unwrap_or(Value::Null),
        VariantClass::Array => {
            if variant.type_().element().is_dict_entry() {
                let mut map = Map::new();
                for entry in variant.iter() {
                    let key = match to_value(&entry.child_value(0)) {
                        Value::String(text) => text,
                        other => other.to_string(),
                    };
                    map.insert(key, to_value(&entry.child_value(1)));
                }
                Value::Object(map)
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
    if variant.is_container() && variant.n_children() > 0 {
        to_value(&variant.child_value(0))
    } else {
        Value::Null
    }
}

/// Python `isinstance(value, (int, float)) and not isinstance(value, bool)` (always finite here).
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

/// Python `bool(value)` where a missing key is `None`.
pub fn truthy(value: Option<&Value>) -> bool {
    value.is_some_and(crate::pyjson::truthy)
}

/// Python `str(value or "")`.
pub fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        v if !truthy(v) => String::new(),
        Some(Value::Bool(true)) => "True".into(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Python `==` of two dict values, where `1 == 1.0`, `True == 1` and a missing key is `None`.
pub fn py_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    fn numeric(value: &Value) -> Option<f64> {
        match value {
            Value::Bool(flag) => Some(*flag as i64 as f64),
            Value::Number(n) => n.as_f64(),
            _ => None,
        }
    }
    let a = a.unwrap_or(&Value::Null);
    let b = b.unwrap_or(&Value::Null);
    match (numeric(a), numeric(b)) {
        (Some(p), Some(q)) => p == q,
        _ => a == b,
    }
}

/// `value in (a, b, …)` for integer constants (Python numeric equality).
pub fn int_in(value: Option<&Value>, choices: &[i64]) -> bool {
    choices.iter().any(|choice| py_eq(value, Some(&Value::from(*choice))))
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
        assert!(value["Percentage"].is_f64());
        assert_eq!(value["Type"], 2);
        assert!(value["Bad"].is_null());
        assert!(py_eq(Some(&serde_json::json!(1)), Some(&serde_json::json!(1.0))));
        assert!(py_eq(Some(&Value::Null), None));
        assert!(!py_eq(Some(&Value::Bool(true)), None));
        assert!(int_in(Some(&serde_json::json!(3)), &[2, 3]));
        assert_eq!(int(Some(&serde_json::json!(2.0))), None);
        assert_eq!(number(Some(&Value::Bool(true))), None);
        assert_eq!(text(Some(&serde_json::json!("x"))), "x");
        assert_eq!(text(None), "");
        assert_eq!(text(Some(&Value::Bool(true))), "True");
        assert!(is_false(Some(&Value::Bool(false))) && !is_false(None));
        assert!(!truthy(None) && truthy(Some(&serde_json::json!("x"))));
        let rows = vec![("idle".to_string(), "a".to_string(), "b".to_string(), "block".to_string(), 1u32, 2u32)];
        assert_eq!(first(&(rows,).to_variant()), serde_json::json!([["idle", "a", "b", "block", 1, 2]]));
        assert_eq!(to_value(&glib::Variant::array_from_fixed_array(&[1u8, 2])), serde_json::json!([1, 2]));
    }
}
