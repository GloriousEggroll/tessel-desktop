//! Pure helpers from monitor_ui.py: value formatting and storage filters.

use serde_json::Value;
use std::cmp::Ordering;

/// Virtual block devices and read-only package images are not storage people manage.
pub const HIDDEN_DEVICE_PREFIXES: &[&str] = &["loop", "ram", "zram"];
pub const HIDDEN_FILESYSTEMS: &[&str] = &["squashfs"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Number(String),
    Text(String),
}

impl Ord for Part {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Part::Number(a), Part::Number(b)) => {
                let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
                a.len().cmp(&b.len()).then_with(|| a.cmp(b))
            }
            (Part::Number(_), Part::Text(_)) => Ordering::Less,
            (Part::Text(_), Part::Number(_)) => Ordering::Greater,
            (Part::Text(a), Part::Text(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Part {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

/// `natural_key`: digit runs compare numerically, text casefolded.
pub fn natural_key(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut digits = false;
    for c in text.chars() {
        let is_digit = c.is_ascii_digit();
        if !current.is_empty() && is_digit != digits {
            parts.push(if digits { Part::Number(std::mem::take(&mut current)) }
                       else { Part::Text(suite_core::pyjson::casefold(&std::mem::take(&mut current))) });
        }
        digits = is_digit;
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(if digits { Part::Number(current) } else { Part::Text(suite_core::pyjson::casefold(&current)) });
    }
    parts
}

fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => suite_core::pyjson::py_str(other),
    }
}

/// Disks shown on the Storage page (no loop/ram/zram), naturally sorted.
pub fn storage_devices(devices: &[Value]) -> Vec<Value> {
    let mut shown: Vec<Value> = devices.iter().filter(|d| {
        let name = text(d.get("name"));
        !HIDDEN_DEVICE_PREFIXES.iter().any(|p| name.starts_with(p))
    }).cloned().collect();
    shown.sort_by_cached_key(|d| natural_key(&text(d.get("name"))));
    shown
}

/// Mounted volumes shown on the Storage page, naturally sorted by mount point.
pub fn storage_volumes(partitions: &[Value]) -> Vec<Value> {
    let hidden = |volume: &Value| {
        let device_path = text(volume.get("device"));
        let device = device_path.rsplit('/').next().unwrap_or("").to_owned();
        let fstype = text(volume.get("fstype"));
        HIDDEN_FILESYSTEMS.contains(&fstype.as_str()) || HIDDEN_DEVICE_PREFIXES.iter().any(|p| device.starts_with(p))
            || text(volume.get("mountpoint")).starts_with("/snap/")
    };
    let mut shown: Vec<Value> = partitions.iter().filter(|v| !hidden(v)).cloned().collect();
    shown.sort_by_cached_key(|v| {
        let mount = text(v.get("mountpoint"));
        natural_key(&if mount.is_empty() { text(v.get("device")) } else { mount })
    });
    shown
}

pub const DASH: &str = "—";

/// `size`: B, KiB … TiB.
pub fn size(value: Option<f64>) -> String {
    let Some(mut value) = value else { return DASH.into() };
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value.abs() < 1024.0 || unit == "TiB" {
            return if unit == "B" { format!("{value:.0} {unit}") } else { format!("{value:.1} {unit}") };
        }
        value /= 1024.0;
    }
    unreachable!()
}

pub fn rate(value: Option<f64>) -> String {
    match value {
        Some(_) => size(value) + "/s",
        None => DASH.into(),
    }
}

pub fn percent(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.1}%"),
        None => DASH.into(),
    }
}

/// Sum of the available values, `None` when none is available.
pub fn total_rates(values: &[Option<f64>]) -> Option<f64> {
    values.iter().any(Option::is_some).then(|| values.iter().flatten().sum())
}

pub fn num(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// `str.capitalize()`.
pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// `str(value)` for display (Python repr rules for numbers/bools/None).
pub fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => suite_core::pyjson::py_str(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// test_storage_hides_virtual_devices_and_sorts_naturally (pure half).
    #[test]
    fn storage_hides_virtual_devices_and_sorts_naturally() {
        let devices: Vec<Value> = ["nvme1n1", "loop3", "sda", "zram0", "nvme0n1", "ram1", "nvme10n1", "dm-0"]
            .iter().map(|n| json!({"name": n})).collect();
        let names: Vec<String> = storage_devices(&devices).iter().map(|d| d["name"].as_str().unwrap().to_owned()).collect();
        assert_eq!(names, ["dm-0", "nvme0n1", "nvme1n1", "nvme10n1", "sda"]);
        let volumes = vec![
            json!({"device": "/dev/loop2", "mountpoint": "/snap/core/1", "fstype": "squashfs"}),
            json!({"device": "/dev/sdb1", "mountpoint": "/mnt/disk10", "fstype": "ext4"}),
            json!({"device": "/dev/sdb2", "mountpoint": "/mnt/disk2", "fstype": "ext4"}),
            json!({"device": "/dev/zram0", "mountpoint": "/tmp", "fstype": "ext2"}),
            json!({"device": "/dev/nvme0n1p2", "mountpoint": "/", "fstype": "btrfs"}),
        ];
        let mounts: Vec<String> = storage_volumes(&volumes).iter().map(|v| v["mountpoint"].as_str().unwrap().to_owned()).collect();
        assert_eq!(mounts, ["/", "/mnt/disk2", "/mnt/disk10"]);
    }

    #[test]
    fn formatting_matches_python() {
        assert_eq!(size(None), "—");
        assert_eq!(size(Some(512.0)), "512 B");
        assert_eq!(size(Some(1536.0)), "1.5 KiB");
        assert_eq!(size(Some(1024f64.powi(4) * 3.0)), "3.0 TiB");
        assert_eq!(size(Some(1024f64.powi(5))), "1024.0 TiB");
        assert_eq!(rate(Some(1024.0 * 1024.0)), "1.0 MiB/s");
        assert_eq!(rate(None), "—");
        assert_eq!(percent(Some(24.0)), "24.0%");
        assert_eq!(total_rates(&[None, None]), None);
        assert_eq!(total_rates(&[Some(1.0), None, Some(2.0)]), Some(3.0));
        assert_eq!(capitalize("working_directory".replace('_', " ").as_str()), "Working directory");
        assert_eq!(display(&json!(10.0)), "10.0");
        assert_eq!(display(&Value::Null), "None");
    }
}
