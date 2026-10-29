//! Kernel charge limits (sysfs, read-only) and the KDE KAuth charge-threshold
//! helper protocol (power_charge_limits.py). No privileged helper is shipped;
//! KDE's helper performs authorized writes for ALL threshold-capable batteries.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const HELPER: &str = "org.kde.powerdevil.chargethresholdhelper";
pub const ENDPOINT: (&str, &str, &str) = (HELPER, "/", "org.kde.kf6auth");
pub const SUPPLIES: &str = "/sys/class/power_supply";

pub fn helper_files() -> Vec<PathBuf> {
    vec![PathBuf::from("/usr/libexec/kf6/kauth/chargethresholdhelper"),
         PathBuf::from(format!("/usr/share/dbus-1/system-services/{HELPER}.service")),
         PathBuf::from(format!("/usr/share/polkit-1/actions/{HELPER}.policy"))]
}

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub name: String,
    pub start: Option<i64>,
    pub stop: i64,
    pub identity: Vec<String>,
}

/// UPower device path → record. BTreeMap keeps identity comparisons stable.
pub type Records = BTreeMap<String, Record>;

fn read_text(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    let bytes = &bytes[..bytes.len().min(4096)];
    Ok(String::from_utf8_lossy(bytes).trim().to_owned())
}

fn threshold(path: &Path) -> Option<i64> {
    let value: i64 = read_text(path).ok()?.parse().ok()?;
    let writable = fs::metadata(path).ok()?.permissions().mode() & 0o200 != 0;
    ((0..=100).contains(&value) && writable).then_some(value)
}

pub fn initial_state() -> Value {
    json!({"supported": false, "maximum": null, "battery_count": 0, "busy": false, "error": "",
           "reason": "No adjustable laptop charge limit is available."})
}

/// UI state plus the exact batteries affected by the global helper.
pub fn inspect_limits(devices: &Map<String, Value>, root: &Path, helper_files: &[PathBuf]) -> (Value, Records) {
    let mut state = initial_state();
    let mut known: BTreeMap<String, String> = BTreeMap::new();
    for (path, props) in devices {
        if props.get("Type").and_then(Value::as_i64) == Some(2) && props.get("Type").is_some_and(|t| !t.is_f64())
            && props.get("PowerSupply") == Some(&Value::Bool(true)) && props.get("IsPresent") == Some(&Value::Bool(true)) {
            let native = crate::values::text(props.get("NativePath"));
            if !native.is_empty() {
                let base = native.rsplit('/').next().unwrap_or("").to_owned();
                known.insert(base, path.clone());
            }
        }
    }
    if known.is_empty() {
        return (state, Records::new());
    }
    let fail = |mut state: Value, reason: &str| {
        state["reason"] = reason.into();
        (state, Records::new())
    };
    let mut records = Records::new();
    let mut entries: Vec<PathBuf> = match fs::read_dir(root) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(_) => return fail(state, "The hardware charge limit could not be read."),
    };
    entries.sort();
    for directory in entries {
        let Ok(kind) = read_text(&directory.join("type")) else {
            return fail(state, "The hardware charge limit could not be read.");
        };
        if kind != "Battery" {
            continue;
        }
        let start_file = directory.join("charge_control_start_threshold");
        let end_file = directory.join("charge_control_end_threshold");
        if !start_file.exists() && !end_file.exists() {
            continue;
        }
        let name = directory.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !known.contains_key(&name) || !end_file.exists() {
            return fail(state, "The available charge-limit helper cannot safely control this battery combination.");
        }
        let field = |file: &str| -> Result<Option<String>, ()> {
            let path = directory.join(file);
            if path.exists() { read_text(&path).map(Some).map_err(|_| ()) } else { Ok(None) }
        };
        match field("scope") {
            Ok(Some(scope)) if scope != "System" => return fail(state, "Charge limits are available only for system batteries."),
            Err(()) => return fail(state, "The hardware charge limit could not be read."),
            _ => {}
        }
        match field("present") {
            Ok(Some(present)) if present != "1" => return fail(state, "A laptop battery is not currently available."),
            Err(()) => return fail(state, "The hardware charge limit could not be read."),
            _ => {}
        }
        let stop = threshold(&end_file);
        let start = if start_file.exists() { threshold(&start_file) } else { None };
        let (Some(stop), true) = (stop, !start_file.exists() || start.is_some()) else {
            return fail(state, "The hardware charge limit could not be read.");
        };
        let resolved = fs::canonicalize(&directory).unwrap_or(directory.clone());
        let mut identity = vec![resolved.to_string_lossy().into_owned()];
        for file in ["manufacturer", "model_name", "serial_number"] {
            match field(file) {
                Ok(value) => identity.push(value.unwrap_or_default()),
                Err(()) => return fail(state, "The hardware charge limit could not be read."),
            }
        }
        records.insert(known[&name].clone(), Record { name, start, stop, identity });
    }
    if records.is_empty() {
        return (state, Records::new());
    }
    state["battery_count"] = records.len().into();
    let stops: std::collections::BTreeSet<i64> = records.values().map(|r| r.stop).collect();
    state["maximum"] = if stops.len() == 1 { (*stops.iter().next().unwrap()).into() } else { Value::Null };
    if !helper_files.iter().all(|path| path.is_file()) {
        state["reason"] = "Adjusting charge limits requires the KDE charge-limit helper.".into();
        return (state, Records::new());
    }
    state["supported"] = true.into();
    state["reason"] = "Applies to all laptop batteries with adjustable charge limits.".into();
    (state, records)
}

pub fn identity(records: &Records) -> Vec<(String, String, Vec<String>)> {
    records.iter().map(|(path, r)| (path.clone(), r.name.clone(), r.identity.clone())).collect()
}

/// Helper arguments (sorted keys, like Python's `sorted(arguments.items())`).
pub fn arguments_for(maximum: &Value, records: &Records) -> Result<BTreeMap<String, i64>, String> {
    let maximum = match crate::values::int(Some(maximum)) {
        Some(v) if (1..=100).contains(&v) => v,
        _ => return Err("Choose a maximum charge between 1% and 100%.".into()),
    };
    if records.is_empty() {
        return Err("No adjustable laptop charge limit is available.".into());
    }
    let mut arguments = BTreeMap::from([("chargeStopThreshold".to_owned(), maximum)]);
    let starts: Vec<Option<i64>> = records.values().map(|r| r.start).collect();
    if starts.iter().any(|s| s.is_some_and(|s| s >= maximum)) {
        if starts.iter().any(Option::is_none) {
            return Err("This battery combination cannot safely lower the recharge threshold. Choose a higher maximum charge.".into());
        }
        let lowest = starts.iter().flatten().min().copied().unwrap_or(0);
        arguments.insert("chargeStartThreshold".into(), lowest.min(maximum - 1));
    }
    Ok(arguments)
}

fn qstring(text: &str) -> Vec<u8> {
    let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut out = (utf16.len() as u32).to_be_bytes().to_vec();
    out.extend(utf16);
    out
}

/// Qt's bounded QVariantMap subset: QString keys and qint32 values.
pub fn encode_arguments(arguments: &BTreeMap<String, i64>) -> Result<Vec<u8>, String> {
    let mut blob = (arguments.len() as u32).to_be_bytes().to_vec();
    for (key, value) in arguments {
        if key != "chargeStartThreshold" && key != "chargeStopThreshold" {
            return Err("Invalid charge-limit helper arguments.".into());
        }
        blob.extend(qstring(key));
        blob.extend(2u32.to_be_bytes());
        blob.push(0);
        blob.extend((*value as i32).to_be_bytes());
    }
    Ok(blob)
}

/// `performAction(saya{sv}aya{sh})` parameters.
pub fn parameters(action: &str, arguments: Option<&BTreeMap<String, i64>>) -> Result<glib::Variant, String> {
    use glib::prelude::*;
    if action != "getthreshold" && action != "setthreshold" {
        return Err("Invalid charge-limit action.".into());
    }
    let empty = BTreeMap::new();
    let blob = encode_arguments(arguments.unwrap_or(&empty))?;
    let details: std::collections::HashMap<String, glib::Variant> = std::collections::HashMap::new();
    let fds: std::collections::HashMap<String, glib::variant::Handle> = std::collections::HashMap::new();
    Ok((format!("{HELPER}.{action}"), Vec::<u8>::new(), details, blob, fds).to_variant())
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8], String> {
        if self.offset + size > self.data.len() {
            return Err("Incomplete response from the charge-limit helper.".into());
        }
        let slice = &self.data[self.offset..self.offset + size];
        self.offset += size;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String, String> {
        let invalid = || "Invalid response from the charge-limit helper.".to_owned();
        let size = self.u32()?;
        if size == 0xffff_ffff {
            return Ok(String::new());
        }
        if size > 8192 || size % 2 != 0 || self.offset + size as usize > self.data.len() {
            return Err(invalid());
        }
        let bytes = self.take(size as usize)?;
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).map_err(|_| invalid())
    }
}

/// Strictly decode KAuth's tiny integer result map; reject unknown types.
pub fn decode_reply(data: &[u8]) -> Result<BTreeMap<String, i32>, String> {
    let invalid = || "Invalid response from the charge-limit helper.".to_owned();
    if data.len() > 16384 {
        return Err(invalid());
    }
    let mut cursor = Cursor { data, offset: 0 };
    let count = cursor.u32()?;
    if count > 8 {
        return Err(invalid());
    }
    let mut result = BTreeMap::new();
    for _ in 0..count {
        let key = cursor.string()?;
        let kind = cursor.u32()?;
        let null = cursor.take(1)?[0];
        if kind != 2 || null != 0 {
            return Err("Unsupported response from the charge-limit helper.".into());
        }
        result.insert(key, i32::from_be_bytes(cursor.take(4)?.try_into().unwrap()));
    }
    let error_code = cursor.u32()?;
    let reply_type = cursor.u32()?;
    let description = cursor.string()?;
    if cursor.offset != data.len() {
        return Err(invalid());
    }
    if reply_type != 2 {
        let known = match error_code {
            4 | 5 => "Authorization was canceled or denied.",
            6 => "The battery charge-limit helper is busy. Try again.",
            _ => "The charge-limit helper could not apply this setting.",
        };
        return Err(if description.is_empty() { known.to_owned() } else { description });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(code: u32, kind: u32, description: &str) -> Vec<u8> {
        let mut blob = encode_arguments(&BTreeMap::new()).unwrap();
        blob.extend(code.to_be_bytes());
        blob.extend(kind.to_be_bytes());
        blob.extend(qstring(description));
        blob
    }

    #[test]
    fn arguments_match_qt6_qdatastream() {
        let expected = "000000010000002600630068006100720067006500530074006f0070005400680072006500730068006f006c0064000000020000000050";
        let blob = encode_arguments(&BTreeMap::from([("chargeStopThreshold".into(), 80)])).unwrap();
        let hex: String = blob.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, expected);
    }

    #[test]
    fn success_denial_busy_and_truncated_replies() {
        assert_eq!(decode_reply(&reply(0, 2, "")).unwrap(), BTreeMap::new());
        for (code, message) in [(4, "denied"), (5, "denied"), (6, "busy")] {
            assert!(decode_reply(&reply(code, 0, "")).unwrap_err().contains(message));
        }
        assert!(decode_reply(&reply(0xffff_ffff, 1, "Charge limits not supported")).unwrap_err().contains("not supported"));
        for blob in [vec![], vec![0u8; 10], vec![0xff; 32], vec![0; 20000]] {
            assert!(decode_reply(&blob).is_err());
        }
    }

    #[test]
    fn set_parameters_are_scoped_to_known_actions_and_integer_thresholds() {
        let args = BTreeMap::from([("chargeStopThreshold".into(), 80)]);
        let values = parameters("setthreshold", Some(&args)).unwrap();
        assert_eq!(values.type_().as_str(), "(saya{sv}aya{sh})");
        assert_eq!(values.child_value(0).get::<String>().unwrap(), format!("{HELPER}.setthreshold"));
        assert!(parameters("anythingelse", None).is_err());
        assert!(encode_arguments(&BTreeMap::from([("path".into(), 1)])).is_err());
    }

    fn record(start: Option<i64>) -> Record {
        Record { name: "x".into(), start, stop: 80, identity: vec![] }
    }

    #[test]
    fn lowering_limit_preserves_or_safely_adjusts_start() {
        let mut records = Records::from([("a".into(), record(Some(75))), ("b".into(), record(Some(60)))]);
        assert_eq!(arguments_for(&json!(80), &records).unwrap(), BTreeMap::from([("chargeStopThreshold".into(), 80)]));
        assert_eq!(arguments_for(&json!(50), &records).unwrap(),
                   BTreeMap::from([("chargeStopThreshold".into(), 50), ("chargeStartThreshold".into(), 49)]));
        assert_eq!(arguments_for(&json!(1), &records).unwrap()["chargeStartThreshold"], 0);
        assert_eq!(arguments_for(&json!(75), &records).unwrap()["chargeStartThreshold"], 60);
        records.get_mut("b").unwrap().start = None;
        assert!(arguments_for(&json!(50), &records).unwrap_err().contains("combination"));
        for value in [json!(true), json!(false), json!(0), json!(101), json!(50.5), json!("80"), Value::Null] {
            assert!(arguments_for(&value, &records).is_err());
        }
    }

    struct Probe {
        root: PathBuf,
        helper: PathBuf,
        supplies: PathBuf,
        devices: Map<String, Value>,
    }

    impl Probe {
        fn new(name: &str) -> Self {
            let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let root = base.join(format!("power-limits-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("supplies")).unwrap();
            let helper = root.join("helper");
            fs::write(&helper, "fixture").unwrap();
            Self { supplies: root.join("supplies"), root, helper, devices: Map::new() }
        }

        fn add(&mut self, name: &str, stop: Option<i64>, start: Option<i64>) -> PathBuf {
            let directory = self.supplies.join(name);
            fs::create_dir(&directory).unwrap();
            for (key, value) in [("type", "Battery"), ("scope", "System"), ("present", "1"), ("model_name", "Laptop battery"),
                                 ("serial_number", name)] {
                fs::write(directory.join(key), value).unwrap();
            }
            if let Some(stop) = stop {
                fs::write(directory.join("charge_control_end_threshold"), stop.to_string()).unwrap();
            }
            if let Some(start) = start {
                fs::write(directory.join("charge_control_start_threshold"), start.to_string()).unwrap();
            }
            self.devices.insert(format!("/{name}"), json!({"NativePath": name, "Type": 2, "PowerSupply": true, "IsPresent": true}));
            directory
        }

        fn probe(&self) -> (Value, Records) {
            inspect_limits(&self.devices, &self.supplies, std::slice::from_ref(&self.helper))
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn present_system_batteries_report_actual_limits_and_mixed_state() {
        let mut p = Probe::new("present");
        p.add("BAT0", Some(80), Some(60));
        let (state, records) = p.probe();
        assert_eq!(state["supported"], true);
        assert_eq!((state["maximum"].clone(), state["battery_count"].clone()), (json!(80), json!(1)));
        assert_eq!(records["/BAT0"].start, Some(60));
        p.add("BAT1", Some(90), Some(70));
        let (state, _) = p.probe();
        assert_eq!(state["supported"], true);
        assert!(state["maximum"].is_null());
        assert_eq!(state["battery_count"], 2);
    }

    #[test]
    fn missing_helper_does_not_claim_adjustable() {
        let mut p = Probe::new("helper");
        p.add("BAT0", Some(80), None);
        fs::remove_file(&p.helper).unwrap();
        let (state, records) = p.probe();
        assert_eq!(state["supported"], false);
        assert!(records.is_empty());
        assert!(state["reason"].as_str().unwrap().contains("helper"));
    }

    #[test]
    fn peripherals_absent_read_only_and_unmapped_batteries_fail_closed() {
        let mut p = Probe::new("closed");
        let directory = p.add("BAT0", Some(80), None);
        for (name, value) in [("PowerSupply", json!(false)), ("IsPresent", json!(false)), ("Type", json!(12))] {
            let original = p.devices["/BAT0"][name].clone();
            p.devices["/BAT0"][name] = value;
            assert_eq!(p.probe().0["supported"], false);
            p.devices["/BAT0"][name] = original;
        }
        let end = directory.join("charge_control_end_threshold");
        fs::set_permissions(&end, fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(p.probe().0["supported"], false);
        fs::set_permissions(&end, fs::Permissions::from_mode(0o644)).unwrap();
        p.add("BAT1", Some(80), None);
        p.devices.remove("/BAT1");
        assert_eq!(p.probe().0["supported"], false);
    }

    #[test]
    fn start_only_companion_prevents_partial_global_write() {
        let mut p = Probe::new("start-only");
        p.add("BAT0", Some(80), None);
        p.add("BAT1", None, Some(60));
        assert_eq!(p.probe().0["supported"], false);
    }
}
