//! Waydock status line (osk_status.py) and the daemon's state file.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `$XDG_RUNTIME_DIR/waykeys/state.json`.
pub fn state_path() -> PathBuf { suite_core::paths::runtime_dir().join("waykeys/state.json") }

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// One status object. A missing/invalid file or a dead `pid` reads as "Off".
pub fn status(path: Option<&Path>) -> Value {
    let path = path.map(Path::to_path_buf).unwrap_or_else(state_path);
    let data = std::fs::read_to_string(&path).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value { Value::Object(map) => Some(map), _ => None })
        .filter(|map| match map.get("pid") {
            // type(pid) is int (bool excluded) and pid > 0 and kill(pid, 0) succeeds.
            Some(Value::Number(n)) => n.as_i64().filter(|p| *p > 0 && *p <= i32::MAX as i64)
                .is_some_and(|pid| unsafe { libc::kill(pid as libc::pid_t, 0) } == 0),
            _ => false,
        })
        .unwrap_or_default();
    let visible = data.get("visible") == Some(&Value::Bool(true));
    let detail = match data.get("error") {
        error if truthy(error) => match error.unwrap() {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        _ if truthy(data.get("automatic_available")) => "Automatic: touchscreen without a keyboard".into(),
        _ => "Click to show · Right-click for settings".into(),
    };
    json!({"text": "\u{f11c}", "class": if visible { "active" } else { "inactive" },
           "tooltip": format!("On-screen Keyboard: {}\n{detail}", if visible { "On" } else { "Off" })})
}

#[cfg(test)]
mod tests {
    use super::*;
    use suite_core::pyfmt::{json_dumps, JsonStyle};

    #[test]
    fn status_lines() {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!("waykeys-status-{}.json", std::process::id()));
        let missing = status(Some(&path));
        assert_eq!(json_dumps(&missing, &JsonStyle::DEFAULT),
                   r#"{"text": "\uf11c", "class": "inactive", "tooltip": "On-screen Keyboard: Off\nClick to show \u00b7 Right-click for settings"}"#);
        let pid = std::process::id();
        std::fs::write(&path, format!(r#"{{"pid": {pid}, "visible": true, "automatic_available": true, "ready": true, "error": ""}}"#)).unwrap();
        let value = status(Some(&path));
        assert_eq!(value["class"], "active");
        assert_eq!(value["tooltip"], "On-screen Keyboard: On\nAutomatic: touchscreen without a keyboard");
        std::fs::write(&path, format!(r#"{{"pid": {pid}, "visible": false, "error": "Broken"}}"#)).unwrap();
        assert_eq!(status(Some(&path))["tooltip"], "On-screen Keyboard: Off\nBroken");
        for bad in [r#"{"pid": true, "visible": true}"#, r#"{"pid": 0, "visible": true}"#, r#"{"pid": 2147483647, "visible": true}"#, "[1]", "{"] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(status(Some(&path))["class"], "inactive", "{bad}");
        }
        std::fs::remove_file(&path).unwrap();
    }
}
