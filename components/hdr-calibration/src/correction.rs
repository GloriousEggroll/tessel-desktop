//! correction.py: strict, monitor-bound HDR output correction import. No
//! inferred measurements.

use crate::calibration::CalError;
use serde_json::{json, Map, Value};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::Path;

pub const MARKER: &str = "# labwc-hdr-pq-correction-v1";
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

fn value(message: &str) -> CalError {
    CalError::Value(message.into())
}

/// Python `float(text)` for cube samples (finite or not; the caller checks).
fn py_float(text: &str) -> Option<f64> {
    let cleaned = text.trim();
    if cleaned.is_empty() || cleaned.starts_with('_') || cleaned.ends_with('_') || cleaned.contains("__") {
        return None;
    }
    cleaned.replace('_', "").parse::<f64>().ok()
}

pub fn sha256_hex(data: &[u8]) -> String {
    glib::compute_checksum_for_data(glib::ChecksumType::Sha256, data).map(|s| s.to_string()).unwrap_or_default()
}

/// `read_cube(path, identity, mode) -> (data, sha256, size)`.
pub fn read_cube(path: &Path, identity: &str, mode: &str) -> Result<(Vec<u8>, String, u32), CalError> {
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)
        .map_err(|e| CalError::Os(tessel_core::pyerr::os_error_path(&e, path)))?;
    let info = file.metadata().map_err(|e| CalError::Os(tessel_core::pyerr::os_error_text(&e)))?;
    let file_type = info.file_type();
    if !file_type.is_file() || file_type.is_fifo() || !(128..=MAX_BYTES).contains(&info.len()) {
        return Err(value("Choose a regular HDR correction file smaller than 16 MiB."));
    }
    let mut data = Vec::new();
    (&mut file).take(MAX_BYTES + 1).read_to_end(&mut data).map_err(|e| CalError::Os(tessel_core::pyerr::os_error_text(&e)))?;
    if data.len() as u64 > MAX_BYTES || data.contains(&0) {
        return Err(value("Invalid HDR correction file."));
    }
    let text = std::str::from_utf8(&data).map_err(|_| value("The correction must be a UTF-8 .cube file."))?;
    let (mut dim, mut count) = (0u32, 0u64);
    let mut marker = false;
    let mut found: Vec<String> = Vec::new();
    for raw in way_displays::compat::fsx::splitlines(text) {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line == MARKER {
            marker = true;
            continue;
        }
        if line.starts_with("# display-id: ") || line.starts_with("# picture-mode: ") {
            let (key, found_value) = line[2..].split_once(": ").unwrap();
            let expected = if key == "display-id" { identity } else { mode };
            if found.iter().any(|f| f == key) || found_value != expected {
                return Err(value("The correction belongs to a different monitor or HDR picture mode."));
            }
            found.push(key.to_owned());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("TITLE ") && count == 0 {
            continue;
        }
        if line.starts_with("LUT_3D_SIZE ") {
            let digits = &line["LUT_3D_SIZE ".len()..];
            if dim != 0 || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return Err(value("Invalid or duplicate LUT size."));
            }
            dim = digits.parse().unwrap_or(0);
            if ![2, 17, 33, 65].contains(&dim) {
                return Err(value("The correction cube must have 2, 17, 33 or 65 samples per axis."));
            }
            continue;
        }
        let domain = ["DOMAIN_MIN", "DOMAIN_MAX"].into_iter().find(|key| line.starts_with(&format!("{key} ")));
        let body = match domain { Some(key) => &line[key.len()..], None => line };
        let values: Option<Vec<f64>> = body.split_whitespace().map(py_float).collect();
        let Some(values) = values else {
            return Err(value("Unsupported correction directive or invalid RGB sample."));
        };
        if values.len() != 3 || !values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return Err(value("Correction samples must be finite RGB triplets in [0, 1]."));
        }
        if let Some(key) = domain {
            let expected = if key == "DOMAIN_MIN" { 0.0 } else { 1.0 };
            if count != 0 || found.iter().any(|f| f == key) || values.iter().any(|v| *v != expected) {
                return Err(value("Only the complete PQ [0, 1] domain is supported."));
            }
            found.push(key.to_owned());
        } else {
            count += 1;
            if dim == 0 || count > (dim as u64).pow(3) {
                return Err(value("Unexpected number of correction samples."));
            }
        }
    }
    let has = |key: &str| found.iter().any(|f| f == key);
    if !marker || !has("display-id") || !has("picture-mode") || dim == 0 || count != (dim as u64).pow(3) {
        return Err(value("Choose a complete monitor-bound HDR PQ correction, not an SDR or creative LUT."));
    }
    let sha = sha256_hex(&data);
    Ok((data, sha, dim))
}

/// Copy a validated cube to `$XDG_CONFIG_HOME/tessel-hdr-calibration/corrections/<sha>.cube` (0400).
pub fn import_cube(path: &Path, identity: &str, mode: &str) -> Result<Map<String, Value>, CalError> {
    let (data, sha, dim) = read_cube(path, identity, mode)?;
    let target = crate::calibration::config_dir().join("corrections").join(format!("{sha}.cube"));
    way_displays::color_config::atomic_write(&target, &data, 0o400)?;
    Ok(json!({"path": target.to_string_lossy(), "sha256": sha, "encoding": "BT2020-PQ", "size": dim}).as_object().unwrap().clone())
}
