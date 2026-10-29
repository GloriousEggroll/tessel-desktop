//! Python value semantics over `serde_json` values (JSON-decoded Python data):
//! `int()` / `float()` / `str()` / `repr()` / truthiness / `==` rules.

use serde_json::Value;

/// `bool(value)`.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|f| f != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `value or other or …`: the first truthy value.
pub fn first_truthy<'a>(values: &[Option<&'a Value>]) -> Option<&'a Value> {
    values.iter().flatten().copied().find(|v| truthy(v))
}

/// Python `int()` text parsing: surrounding whitespace, sign, single `_` between digits.
fn parse_int_text(text: &str) -> Option<i64> {
    let text = py_strip(text);
    let (negative, digits) = match text.as_bytes().first()? {
        b'+' => (false, &text[1..]),
        b'-' => (true, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut result: i64 = 0;
    for ch in digits.chars() {
        if ch == '_' {
            continue;
        }
        let digit = ch.to_digit(10)? as i64;
        result = result.checked_mul(10)?.checked_add(digit)?;
    }
    Some(if negative { -result } else { result })
}

/// `_int(value)` of audio_backend.py: `int(value)` or None on TypeError/ValueError.
pub fn py_int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Bool(flag) => Some(*flag as i64),
        Value::Number(number) => number.as_i64().or_else(|| number.as_u64().map(|u| u.min(i64::MAX as u64) as i64)).or_else(|| {
            let float = number.as_f64()?;
            float.is_finite().then(|| float.trunc() as i64)
        }),
        Value::String(text) => parse_int_text(text),
        _ => None,
    }
}

/// `float(value)` or None.
pub fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(flag) => Some(*flag as i64 as f64),
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let text = py_strip(text).replace('_', "");
            let lower = text.to_ascii_lowercase();
            let unsigned = lower.trim_start_matches(['+', '-']);
            if matches!(unsigned, "inf" | "infinity" | "nan") {
                let negative = lower.starts_with('-');
                return Some(match (unsigned, negative) {
                    ("nan", _) => f64::NAN,
                    (_, true) => f64::NEG_INFINITY,
                    _ => f64::INFINITY,
                });
            }
            text.parse::<f64>().ok()
        }
        _ => None,
    }
}

fn repr_string(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 32 || c as u32 == 127 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::String(text) => repr_string(text),
        other => py_str(other),
    }
}

/// `str(value)` for JSON-decoded Python values.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => match (number.as_i64(), number.as_u64()) {
            (Some(int), _) => int.to_string(),
            (_, Some(int)) => int.to_string(),
            _ => crate::pyfmt::float_repr(number.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(text) => text.clone(),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", repr_string(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str(value or "")`: falsy values (`None`, `0`, `""`, `[]`, …) give `""`.
pub fn py_str_or_empty(value: &Value) -> String {
    if truthy(value) { py_str(value) } else { String::new() }
}

/// `repr(value)` for JSON-decoded Python values.
pub fn py_repr_value(value: &Value) -> String {
    py_repr(value)
}

/// `str(d.get(key))` where a missing key is `None`.
pub fn py_str_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".into(), py_str)
}

/// Python `==` between a JSON value and an `int | None`.
pub fn eq_int(value: &Value, other: Option<i64>) -> bool {
    match (value, other) {
        (Value::Null, None) => true,
        (_, None) | (Value::Null, Some(_)) => false,
        (Value::Bool(flag), Some(int)) => *flag as i64 == int,
        (Value::Number(number), Some(int)) => match number.as_i64() {
            Some(own) => own == int,
            None => number.as_f64() == Some(int as f64),
        },
        _ => false,
    }
}

/// `other in value` for a list value (TypeError for non-containers → false).
pub fn contains_int(value: &Value, other: Option<i64>) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|item| eq_int(item, other)),
        _ => false,
    }
}

/// `value in (True, "true", 1)`.
pub fn is_true_value(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(true)) => true,
        Some(Value::String(text)) => text == "true",
        Some(Value::Number(number)) => number.as_f64() == Some(1.0),
        _ => false,
    }
}

/// `str.isspace()` characters stripped by `str.strip()`.
pub fn is_py_space(ch: char) -> bool {
    ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch)
}

/// `str.strip()`.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

/// `str.casefold()` (full case folding for the common special cases).
pub fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ς' => out.push('σ'),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬀ' => out.push_str("ff"),
            'ſ' => out.push('s'),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_int_float_and_str() {
        assert_eq!(py_int(Some(&json!("210"))), Some(210));
        assert_eq!(py_int(Some(&json!(" -1_0 "))), Some(-10));
        assert_eq!(py_int(Some(&json!("1.5"))), None);
        assert_eq!(py_int(Some(&json!(7.9))), Some(7));
        assert_eq!(py_int(Some(&json!(true))), Some(1));
        assert_eq!(py_int(Some(&Value::Null)), None);
        assert_eq!(py_float(&json!("inf")), Some(f64::INFINITY));
        assert_eq!(py_str(&json!(1.0)), "1.0");
        assert_eq!(py_str(&Value::Null), "None");
        assert_eq!(py_str(&json!(["a", 1])), "['a', 1]");
        assert!(is_true_value(Some(&json!(1))) && is_true_value(Some(&json!("true"))));
        assert!(!is_true_value(Some(&json!("false"))) && !is_true_value(Some(&json!(0))));
        assert!(contains_int(&json!([7, 8]), Some(7)) && !contains_int(&json!([7]), None));
        assert_eq!(casefold("Straße"), "strasse");
        assert_eq!(py_strip("\u{1c} x \u{a0}"), "x");
    }

    #[test]
    fn python_value_semantics() {
        assert_eq!(py_str_or_empty(&json!(null)), "");
        assert_eq!(py_str_or_empty(&json!(0)), "");
        assert_eq!(py_str_or_empty(&json!(5)), "5");
        assert_eq!(py_str_or_empty(&json!(true)), "True");
        assert_eq!(py_str_or_empty(&json!("x")), "x");
        assert_eq!(py_str(&json!(0)), "0");
        assert_eq!(py_str(&json!({"a": "it's"})), "{'a': \"it's\"}");
        assert_eq!(py_repr_value(&json!("x")), "'x'");
        assert_eq!(py_float(&json!("1.5")), Some(1.5));
        assert_eq!(py_float(&json!("abc")), None);
        assert_eq!(py_float(&json!(null)), None);
        assert!(py_float(&json!("nan")).unwrap().is_nan());
        assert!(truthy(&json!([1])) && !truthy(&json!({})));
    }
}
