//! Crash-recovery drafts for unsaved editor buffers (recovery.py).
//!
//! Each window owns one draft file named by a random identifier. Drafts are
//! written atomically with private permissions after editing pauses, and removed
//! when the document is saved or deliberately discarded. Drafts left behind by a
//! process which is no longer running are offered for restoration.

use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::pyjson::py_float;

pub const VERSION: i64 = 1;
pub const DELAY_SECONDS: f64 = 30.0;

/// `$XDG_STATE_HOME/text-editor/recovery` (default `~/.local/state/...`).
pub fn recovery_dir() -> PathBuf {
    suite_core::paths::state_home().join("text-editor/recovery")
}

pub fn new_identifier() -> String {
    crate::compat::uuid4_hex()
}

fn path(identifier: &str, folder: Option<&Path>) -> Result<PathBuf, String> {
    if identifier.is_empty() || !identifier.chars().all(|c| "0123456789abcdef".contains(c)) {
        return Err("Invalid recovery identifier.".into());
    }
    Ok(folder.map(Path::to_owned).unwrap_or_else(recovery_dir).join(format!("{identifier}.json")))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub identifier: String,
    pub text: String,
    pub location: Option<String>,
    pub encoding: String,
    pub newline: String,
    pub saved: f64,
}

/// The JSON payload, in Python `json.dump` form (`", "`/`": "`, ASCII escapes).
pub fn payload(pid: u32, saved: f64, text: &str, location: Option<&str>, encoding: &str, newline: &str) -> String {
    let value = json!({"version": VERSION, "pid": pid, "saved": saved, "location": location,
                       "encoding": encoding, "newline": newline, "text": text});
    json_dumps(&value, &JsonStyle::DEFAULT)
}

/// Atomically store one draft (`<id>.json.<pid>.tmp` 0600 + fsync + rename). Off the GTK thread.
pub fn write(identifier: &str, text: &str, location: Option<&str>, encoding: &str, newline: &str, folder: Option<&Path>)
             -> std::io::Result<PathBuf> {
    let path = path(identifier, folder).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    if let Some(parent) = path.parent() {
        if !parent.is_dir() {
            if let Some(grand) = parent.parent() {
                std::fs::create_dir_all(grand)?;
            }
            match std::fs::DirBuilder::new().mode(0o700).create(parent) {
                Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
                _ => {}
            }
        }
    }
    let pid = std::process::id();
    let data = payload(pid, crate::compat::now(), text, location, encoding, newline);
    let temporary = path.with_file_name(format!("{}.{pid}.tmp", path.file_name().unwrap().to_string_lossy()));
    let mut stream = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    stream.write_all(data.as_bytes())?;
    stream.flush()?;
    stream.sync_all()?;
    drop(stream);
    std::fs::rename(&temporary, &path)?;
    Ok(path)
}

pub fn remove(identifier: &str, folder: Option<&Path>) {
    if let Ok(path) = path(identifier, folder) {
        let _ = std::fs::remove_file(path);
    }
}

fn alive(pid: Option<&Value>) -> bool {
    // isinstance(pid, int): JSON integers (and booleans, a Python int subclass).
    let pid = match pid {
        Some(Value::Bool(b)) => *b as i64,
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(-1),
        _ => return false,
    };
    if pid <= 0 || pid == std::process::id() as i64 {
        return false;
    }
    let Ok(command) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    let has = |needle: &[u8]| command.windows(needle.len()).any(|w| w == needle);
    has(b"text_editor") || has(b"text-editor") || has(b"editor_ui")
}

/// `value == 1` in Python (1, 1.0 and True).
fn is_version(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() == Some(VERSION as f64),
        _ => false,
    }
}

/// Drafts whose editor process is gone, newest first.
pub fn pending(folder: Option<&Path>) -> Vec<Draft> {
    let folder = folder.map(Path::to_owned).unwrap_or_else(recovery_dir);
    let mut drafts = Vec::new();
    let Ok(entries) = std::fs::read_dir(&folder) else { return drafts };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // glob("*.json") skips dot files.
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else { continue };
        let Ok(text) = String::from_utf8(data) else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(object) = value.as_object() else { continue };
        let Some(body) = object.get("text").and_then(Value::as_str) else { continue };
        if !is_version(object.get("version")) || alive(object.get("pid")) {
            continue;
        }
        let saved = match object.get("saved") {
            None => 0.0,
            Some(value) if !suite_core::pyjson::truthy(value) => 0.0,
            Some(value) => match py_float(value) {
                Some(saved) => saved,
                None => continue,
            },
        };
        let text_of = |key: &str, default: &str| object.get(key).and_then(Value::as_str).unwrap_or(default).to_owned();
        drafts.push(Draft {
            identifier: name.trim_end_matches(".json").to_owned(), text: body.to_owned(),
            location: object.get("location").and_then(Value::as_str).map(str::to_owned),
            encoding: text_of("encoding", "utf-8"), newline: text_of("newline", "LF"), saved,
        });
    }
    // sort(reverse=True) is stable for equal keys.
    drafts.sort_by(|a, b| b.saved.partial_cmp(&a.saved).unwrap_or(std::cmp::Ordering::Equal));
    drafts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn write_pending_remove_round_trip() {
        let folder = std::env::temp_dir().join(format!("text-editor-recovery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let first = new_identifier();
        let path = write(&first, "draft é", Some("/x/y.txt"), "utf-8", "CRLF", Some(&folder)).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with(&format!("{{\"version\": 1, \"pid\": {}, \"saved\": ", std::process::id())), "{raw}");
        assert!(raw.ends_with("\"location\": \"/x/y.txt\", \"encoding\": \"utf-8\", \"newline\": \"CRLF\", \"text\": \"draft \\u00e9\"}"), "{raw}");
        // Our own pid counts as "not running" (Python _alive()).
        let drafts = pending(Some(&folder));
        assert_eq!(drafts.len(), 1);
        assert_eq!((drafts[0].identifier.as_str(), drafts[0].text.as_str(), drafts[0].location.as_deref()), (first.as_str(), "draft é", Some("/x/y.txt")));
        assert_eq!(drafts[0].newline, "CRLF");
        // Newest first; invalid entries skipped.
        std::fs::write(folder.join("aa.json"), r#"{"version": 1, "pid": 0, "saved": 1e12, "text": "new", "location": 3}"#).unwrap();
        std::fs::write(folder.join("bb.json"), r#"{"version": 2, "pid": 0, "text": "old version"}"#).unwrap();
        std::fs::write(folder.join("cc.json"), r#"{"version": 1, "pid": 0, "saved": "soon", "text": "bad saved"}"#).unwrap();
        std::fs::write(folder.join("dd.json"), r#"[1]"#).unwrap();
        let drafts = pending(Some(&folder));
        assert_eq!(drafts.iter().map(|d| d.identifier.as_str()).collect::<Vec<_>>(), vec!["aa", first.as_str()]);
        assert_eq!((drafts[0].location.clone(), drafts[0].encoding.as_str()), (None, "utf-8"));
        remove(&first, Some(&folder));
        remove("aa", Some(&folder));
        remove("../evil", Some(&folder));
        assert!(write("NOT-HEX", "", None, "utf-8", "LF", Some(&folder)).is_err());
        assert!(pending(Some(&folder)).is_empty());
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
