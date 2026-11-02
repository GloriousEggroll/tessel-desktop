//! Helpers tessel-core does not have yet (candidates to upstream).

pub mod argparse;

use serde_json::Value;
use std::path::Path;

/// `str(OSError)`: `[Errno 13] Permission denied: '/path'`.
pub fn os_error(error: &std::io::Error, path: Option<&Path>) -> String {
    match error.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a static, NUL-terminated message.
            let message = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
            match path {
                Some(path) => format!("[Errno {code}] {message}: {}", py_repr_str(&path.to_string_lossy())),
                None => format!("[Errno {code}] {message}"),
            }
        }
        None => error.to_string(),
    }
}

/// `repr(str)` for printable text (quotes chosen like Python).
pub fn py_repr_str(text: &str) -> String {
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

/// Python truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|v| v != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(flag) => Some(*flag as i64 as f64),
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

/// Python `==` between JSON values (`1 == 1.0 == True`, dicts unordered).
pub fn py_eq(a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (numeric(a), numeric(b)) {
        return x == y;
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(key, value)| y.get(key).is_some_and(|other| py_eq(value, other)))
        }
        _ => false,
    }
}

/// `json.loads` failure text for the common cases (`Expecting value: line 1 column 1 (char 0)`).
pub fn json_error(data: &[u8], error: &serde_json::Error) -> String {
    if let Err(utf8) = std::str::from_utf8(data) {
        let position = utf8.valid_up_to();
        return format!("'utf-8' codec can't decode byte 0x{:02x} in position {position}: invalid start byte", data[position]);
    }
    let text = String::from_utf8_lossy(data);
    let (line, column) = (error.line().max(1), error.column().max(1));
    let offset: usize = text.split('\n').take(line - 1).map(|l| l.chars().count() + 1).sum::<usize>() + column - 1;
    let message = if error.to_string().starts_with("trailing characters") { "Extra data" } else { "Expecting value" };
    format!("{message}: line {line} column {column} (char {offset})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_equality_and_truthiness() {
        assert!(py_eq(&json!({"a": 1, "b": [true]}), &json!({"b": [1.0], "a": 1.0})));
        assert!(!py_eq(&json!({"a": 1}), &json!({"a": "1"})));
        assert!(!truthy(&json!({})) && truthy(&json!({"x": 0})) && !truthy(&json!(0.0)));
        let error = std::io::Error::from_raw_os_error(13);
        assert_eq!(os_error(&error, Some(Path::new("/x/y"))), "[Errno 13] Permission denied: '/x/y'");
        let bad = serde_json::from_slice::<Value>(b"broken json").unwrap_err();
        assert_eq!(json_error(b"broken json", &bad), "Expecting value: line 1 column 1 (char 0)");
    }
}
