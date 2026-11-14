//! Remote control without asking: the owner's standing answer to RemoteDesktop
//! portal requests. Global Settings writes it; the screen picker
//! (`tessel-capture-picker`) reads it and then answers such requests itself
//! instead of opening. Off unless the file says otherwise.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const VERSION: u64 = 1;

/// `~/.config/tessel/remote-control.json`, written by Global Settings.
pub fn config_path() -> PathBuf {
    crate::paths::config_home().join("tessel").join("remote-control.json")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteControl {
    /// Answer remote-control requests without showing the picker.
    pub unattended: bool,
    /// Connector name of the screen to share (`DP-2`); empty picks the first.
    pub output: String,
}

impl RemoteControl {
    /// Anything unreadable, of another version or malformed is "off".
    pub fn parse(data: &[u8]) -> RemoteControl {
        let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(data) else { return RemoteControl::default() };
        if map.get("version").and_then(Value::as_u64) != Some(VERSION) {
            return RemoteControl::default();
        }
        let Some(Value::Bool(unattended)) = map.get("unattended") else { return RemoteControl::default() };
        let output = match map.get("output") {
            Some(Value::String(name)) if name.len() <= 128 => name.clone(),
            _ => String::new(),
        };
        RemoteControl { unattended: *unattended, output }
    }

    pub fn load(path: &Path) -> RemoteControl {
        std::fs::read(path).map(|data| RemoteControl::parse(&data)).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        let value = json!({ "version": VERSION, "unattended": self.unattended, "output": self.output });
        serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
    }

    /// Write beside the target and rename, so a reader never sees half a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
        std::fs::create_dir_all(&parent).map_err(|e| crate::pyerr::os_error_path(&e, &parent))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let temporary = parent.join(format!(".{name}.{}", std::process::id()));
        let result = std::fs::write(&temporary, self.to_json()).and_then(|_| std::fs::rename(&temporary, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|e| crate::pyerr::os_error_text(&e))
    }
}
