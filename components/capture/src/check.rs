//! `tessel-capture --check` (capture.py `check`).

use serde_json::{json, Map, Value};

pub fn check() -> Map<String, Value> {
    let mut result = Map::new();
    result.insert("ready".into(), json!(false));
    if let Err(error) = crate::native::library() {
        result.insert("error".into(), json!(error));
        return result;
    }
    result.insert("ffmpeg".into(), json!(tessel_core::shell::which("ffmpeg").is_some()));
    result.insert("pipewire".into(), json!(true));
    result.insert("video".into(), json!(tessel_core::shell::which("ffprobe").is_some()));
    // PNG/JPEG encoding is compiled in (Python: Pillow importable).
    result.insert("images".into(), json!(true));
    let ready = ["ffmpeg", "pipewire", "video", "images"].iter().all(|k| result[*k] == json!(true));
    result.insert("ready".into(), json!(ready));
    result
}

pub fn text(result: &Map<String, Value>) -> String {
    tessel_core::pyfmt::json_dumps(&Value::Object(result.clone()), &tessel_core::pyfmt::JsonStyle::DEFAULT)
}
