//! `status()` JSON (keep_awake.py:43-58): `--status`, `GetState`, `Changed`.

use serde_json::{Map, Number, Value};
use tessel_core::tr;

/// The fields `status()` reads from a backend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub active: bool,
    pub ready: bool,
    pub requested: bool,
    pub error: String,
    pub duration: u32,
    pub until: Option<f64>,
}

pub const ICON_ACTIVE: &str = "\u{f06e}";
pub const ICON_INACTIVE: &str = "\u{f070}";

pub fn status(backend: &Snapshot) -> Value {
    let active = backend.active;
    let pending = backend.requested && !active;
    // "Active" is deliberately untranslated (dock tooltips/tests compare it).
    let state = if active { "Active".to_owned() } else if pending { tr!("Turning on…") } else { tr!("Inactive") };
    let mut tooltip = tr!("Keep Awake: {value1}", value1 = state);
    if !backend.error.is_empty() {
        tooltip += "\n";
        tooltip += &backend.error;
    } else if active {
        tooltip += "\nAutomatic dimming, display off, and sleep are paused.";
    }
    tooltip += "\nClick to toggle · Right-click for controls";
    let until = if backend.requested { backend.until } else { None };
    let class = if active { "active" } else if !backend.ready { "unavailable" } else if pending { "pending" } else { "inactive" };
    let mut map = Map::new();
    map.insert("text".into(), (if active { ICON_ACTIVE } else { ICON_INACTIVE }).into());
    map.insert("tooltip".into(), tooltip.into());
    map.insert("duration".into(), backend.duration.into());
    map.insert("until".into(), until.and_then(Number::from_f64).map(Value::Number).unwrap_or(Value::Null));
    map.insert("class".into(), class.into());
    map.insert("active".into(), active.into());
    map.insert("ready".into(), backend.ready.into());
    map.insert("requested".into(), backend.requested.into());
    map.insert("pending".into(), pending.into());
    map.insert("error".into(), backend.error.clone().into());
    Value::Object(map)
}

/// `json.dumps(status(backend))`.
pub fn status_text(backend: &Snapshot) -> String {
    tessel_core::pyfmt::json_dumps(&status(backend), &tessel_core::pyfmt::JsonStyle::DEFAULT)
}

impl crate::timed::TimedBackend {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot { active: self.active(), ready: self.ready(), requested: self.requested(), error: self.error(),
                   duration: self.duration(), until: self.until() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_truthfully_distinguishes_pending_active_and_unavailable() {
        let mut backend = Snapshot { ready: true, ..Default::default() };
        assert_eq!(status(&backend)["class"], "inactive");
        backend.requested = true;
        assert_eq!(status(&backend)["pending"], true);
        assert_eq!(status(&backend)["active"], false);
        assert_eq!(status(&backend)["class"], "pending");
        backend.active = true;
        assert_eq!(status(&backend)["text"], "\u{f06e}");
        assert_eq!(status(&backend)["class"], "active");
        backend = Snapshot::default();
        assert_eq!(status(&backend)["class"], "unavailable");
        assert_eq!(status(&backend)["text"], "\u{f070}");
    }

    #[test]
    fn json_text_matches_python() {
        let backend = Snapshot { active: true, ready: true, requested: true, duration: 30, until: Some(2800.0),
                                 ..Default::default() };
        assert_eq!(status_text(&backend),
            r#"{"text": "\uf06e", "tooltip": "Keep Awake: Active\nAutomatic dimming, display off, and sleep are paused.\nClick to toggle \u00b7 Right-click for controls", "duration": 30, "until": 2800.0, "class": "active", "active": true, "ready": true, "requested": true, "pending": false, "error": ""}"#);
        let backend = Snapshot { error: "System power management is unavailable.".into(), until: Some(5.0), ..Default::default() };
        assert_eq!(status_text(&backend),
            r#"{"text": "\uf070", "tooltip": "Keep Awake: Inactive\nSystem power management is unavailable.\nClick to toggle \u00b7 Right-click for controls", "duration": 0, "until": null, "class": "unavailable", "active": false, "ready": false, "requested": false, "pending": false, "error": "System power management is unavailable."}"#);
    }
}
