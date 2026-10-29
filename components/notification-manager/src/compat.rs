//! Small helpers this component needed that suite-core does not provide
//! (candidates to upstream): Python value semantics for JSON, strict base64,
//! and a private (0600, mkstemp-style) atomic writer.

use serde_json::Value;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// Python truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|v| v != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(list) => !list.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) if !number.is_f64() => number.to_string(),
        Value::Number(number) => suite_core::pyfmt::float_repr(number.as_f64().unwrap_or(0.0)),
        Value::String(text) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(list) => format!("[{}]", list.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!("{{{}}}", map.iter()
            .map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v))).collect::<Vec<_>>().join(", ")),
    }
}

/// `str(value or "")`.
pub fn py_str(value: &Value) -> String {
    if !truthy(value) {
        return String::new();
    }
    match value {
        Value::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// `str(value)` (no truthiness shortcut: `0` → `"0"`).
pub fn py_str_any(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// [`py_float`] taking an optional value.
pub fn py_float_opt(value: &Value) -> Option<f64> {
    py_float(value)
}

/// Python `float(value)`; `None` where Python raises TypeError/ValueError.
pub fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let cleaned = text.trim().replace('_', "");
            let lower = cleaned.to_ascii_lowercase();
            let unsigned = lower.trim_start_matches(['+', '-']);
            if matches!(unsigned, "nan" | "inf" | "infinity") {
                let negative = lower.starts_with('-');
                return Some(match unsigned {
                    "nan" => f64::NAN,
                    _ if negative => f64::NEG_INFINITY,
                    _ => f64::INFINITY,
                });
            }
            if cleaned.is_empty() || !cleaned.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c)) {
                return None;
            }
            cleaned.parse::<f64>().ok()
        }
        _ => None,
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `base64.b64decode(text, validate=True)`.
pub fn base64_decode_strict(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let mut n = 0u32;
        let mut padding = 0;
        for (position, &byte) in chunk.iter().enumerate() {
            let value = if byte == b'=' {
                if !last || position < 2 {
                    return None;
                }
                padding += 1;
                0
            } else {
                if padding > 0 {
                    return None;
                }
                ALPHABET.iter().position(|&c| c == byte)? as u32
            };
            n = n << 6 | value;
        }
        out.push((n >> 16) as u8);
        if padding < 2 {
            out.push((n >> 8) as u8);
        }
        if padding < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Python's `str(OSError)` text: `[Errno N] message: 'path'`.
pub fn os_error(error: &std::io::Error, path: &Path) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let message = error.to_string();
            let message = message.split(" (os error").next().unwrap_or(&message).to_owned();
            format!("[Errno {code}] {message}: '{}'", path.display())
        }
        None => error.to_string(),
    }
}

/// Write `directory/name` like `tempfile.mkstemp(prefix=…)` + `os.replace`:
/// the directory is created 0700, the file is 0600, the temporary is removed
/// on failure. Errors are Python `OSError` strings.
pub fn write_private(directory: &Path, name: &str, prefix: &str, data: &[u8]) -> Result<(), String> {
    if !directory.is_dir() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(directory)
            .map_err(|e| os_error(&e, directory))?;
    }
    let mut temporary = None;
    let mut last_error = None;
    for attempt in 0..100u32 {
        let suffix: String = {
            let seed = glib::random_int() ^ attempt.wrapping_mul(2654435761);
            (0..8).map(|i| b"abcdefghijklmnopqrstuvwxyz0123456789_"[((seed >> (i * 4)) as usize + i as usize * 7) % 37] as char).collect()
        };
        let path = directory.join(format!("{prefix}{suffix}"));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => {
                temporary = Some((path, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                last_error = Some(os_error(&error, &path));
                break;
            }
        }
    }
    let Some((path, mut file)) = temporary else {
        return Err(last_error.unwrap_or_else(|| "No usable temporary file name found".into()));
    };
    let target = directory.join(name);
    let result = file.write_all(data).map_err(|e| os_error(&e, &path))
        .and_then(|()| std::fs::rename(&path, &target).map_err(|e| os_error(&e, &path)));
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64_round_trip_and_strictness() {
        for data in [&b""[..], b"r", b"rg", b"rgb", b"\xff\x00\x10\x80"] {
            assert_eq!(base64_decode_strict(&base64_encode(data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"rgb"), "cmdi");
        assert!(base64_decode_strict("broken").is_none());
        assert!(base64_decode_strict("cm=i").is_none());
        assert!(base64_decode_strict("c$di").is_none());
    }

    #[test]
    fn python_value_semantics() {
        assert_eq!(py_str(&json!(null)), "");
        assert_eq!(py_str(&json!(0)), "");
        assert_eq!(py_str(&json!(5)), "5");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_float(&json!("1.5")), Some(1.5));
        assert_eq!(py_float(&json!("abc")), None);
        assert_eq!(py_float(&json!(null)), None);
        assert!(py_float(&json!("nan")).unwrap().is_nan());
        assert!(truthy(&json!([1])) && !truthy(&json!({})));
    }
}
