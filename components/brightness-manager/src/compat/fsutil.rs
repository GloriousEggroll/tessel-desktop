//! `brightness_core.write_json` / `read_json`: atomic temp file (prefix
//! `.brightness-`), fsync, rename, then fsync of the directory.

use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use suite_core::pyfmt::{json_dumps, JsonStyle};

/// `json.dump(value, stream, indent=2)` + newline.
pub fn pretty(value: &Value) -> String {
    let style = JsonStyle { indent: Some(2), item_separator: ",", ..JsonStyle::DEFAULT };
    json_dumps(value, &style) + "\n"
}

fn temporary(directory: &Path, prefix: &str) -> io::Result<(fs::File, PathBuf)> {
    use std::os::unix::fs::OpenOptionsExt;
    for attempt in 0..100u32 {
        let random = glib::random_int() ^ attempt.wrapping_mul(2654435761);
        let path = directory.join(format!("{prefix}{random:08x}"));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "No usable temporary file name found"))
}

/// Durable replace: the data and the directory entry are on disk on return.
pub fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let (mut file, temporary) = temporary(parent, ".brightness-")?;
    let result = (|| {
        file.write_all(pretty(value).as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    let _ = fs::remove_file(&temporary);
    result
}

/// Outcome of `read_json`: missing ⇒ `default`; >128 KiB ⇒ error; invalid JSON ⇒ error.
pub fn read_json(path: &Path, default: Value) -> Result<Value, String> {
    let text = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(default),
        Err(error) => return Err(suite_core::pyerr::os_error_path(&error, path)),
    };
    let text = String::from_utf8(text).map_err(|_| "'utf-8' codec can't decode the file".to_owned())?;
    if text.chars().count() > 131072 {
        return Err("The brightness settings file is too large.".into());
    }
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pretty_matches_python_indent() {
        assert_eq!(pretty(&json!({})), "{}\n");
        assert_eq!(pretty(&json!({"auto_dim": false, "dim_after": 120})), "{\n  \"auto_dim\": false,\n  \"dim_after\": 120\n}\n");
        assert_eq!(pretty(&json!({"one": {"connector": "DP-1", "raw": 75}})),
                   "{\n  \"one\": {\n    \"connector\": \"DP-1\",\n    \"raw\": 75\n  }\n}\n");
    }
}
