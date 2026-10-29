//! Read kernel charge limits and speak the installed KDE KAuth helper protocol
//! (`power_charge_limits.py`).
//!
//! No privileged helper is installed by this app. KDE's existing helper performs
//! authorized writes and applies them to ALL threshold-capable batteries. This
//! module only reads sysfs; callers explicitly request each mutation over D-Bus.
//!
//! Protocol/source: KDE powerdevil v6.7.4 daemon/chargethresholdhelper_linux.cpp;
//! KAuth src/backends/dbus/DBusHelperProxy.cpp and src/actionreply.cpp.

use crate::devices::basename;
use suite_core::values::{int, is_true, text};
use glib::prelude::*;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const HELPER: &str = "org.kde.powerdevil.chargethresholdhelper";
pub const ENDPOINT: (&str, &str, &str) = (HELPER, "/", "org.kde.kf6auth");
pub const SUPPLIES: &str = "/sys/class/power_supply";

pub fn helper_files() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/libexec/kf6/kauth/chargethresholdhelper"),
        PathBuf::from(format!("/usr/share/dbus-1/system-services/{HELPER}.service")),
        PathBuf::from(format!("/usr/share/polkit-1/actions/{HELPER}.policy")),
    ]
}

/// One battery the global helper would write.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub name: String,
    pub start: Option<i64>,
    pub stop: i64,
    /// (resolved sysfs path, manufacturer, model_name, serial_number)
    pub identity: Vec<String>,
}

/// UPower device path → record.
pub type Records = BTreeMap<String, Record>;

pub const NO_LIMIT: &str = "No adjustable laptop charge limit is available.";

/// The `charge_limit` dict before any device is known.
pub fn initial_state() -> Value {
    json!({"supported": false, "maximum": null, "battery_count": 0, "busy": false, "error": "", "reason": NO_LIMIT})
}

fn read_text(path: &Path) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    std::fs::File::open(path)?.take(4096 * 4).read_to_end(&mut buffer)?;
    let text = String::from_utf8(buffer).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    Ok(text.chars().take(4096).collect::<String>().trim().to_owned())
}

/// Python `int(text)` for decimal text (sign, `_` between digits).
pub fn parse_int(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, digits) = match text.as_bytes().first()? {
        b'+' => (false, &text[1..]),
        b'-' => (true, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let value: i64 = digits.replace('_', "").parse().ok()?;
    Some(if negative { -value } else { value })
}

fn threshold(path: &Path) -> Option<i64> {
    let value = parse_int(&read_text(path).ok()?)?;
    // sysfs is normally root-writable, not writable by this user. Checking
    // access(W_OK) here would incorrectly hide supported controls.
    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    ((0..=100).contains(&value) && mode & 0o200 != 0).then_some(value)
}

fn fail(mut state: Value, reason: &str) -> (Value, Records) {
    state["reason"] = json!(reason);
    (state, Records::new())
}

/// Return UI state plus the exact batteries affected by the global helper.
pub fn inspect_limits(devices: &Map<String, Value>, root: &Path, helper_files: &[PathBuf]) -> (Value, Records) {
    let mut state = initial_state();
    let mut known: BTreeMap<String, String> = BTreeMap::new();
    for (path, props) in devices {
        if int(props.get("Type")) == Some(2) && is_true(props.get("PowerSupply")) && is_true(props.get("IsPresent")) {
            let native = text(props.get("NativePath"));
            if !native.is_empty() {
                known.insert(basename(&native).to_owned(), path.clone());
            }
        }
    }
    if known.is_empty() {
        return (state, Records::new());
    }
    const UNREADABLE: &str = "The hardware charge limit could not be read.";
    let mut records = Records::new();
    let Ok(entries) = std::fs::read_dir(root) else { return fail(state, UNREADABLE) };
    let mut directories = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { return fail(state, UNREADABLE) };
        directories.push(entry.path());
    }
    directories.sort();
    for directory in directories {
        let name = directory.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match read_text(&directory.join("type")) {
            Ok(kind) if kind == "Battery" => {}
            Ok(_) => continue,
            Err(_) => return fail(state, UNREADABLE),
        }
        let start_file = directory.join("charge_control_start_threshold");
        let end_file = directory.join("charge_control_end_threshold");
        if !start_file.exists() && !end_file.exists() {
            continue;
        }
        // Match KDE's full write scope, including start-only batteries. Do
        // not claim this global helper can safely target a subset of them.
        if !known.contains_key(&name) || !end_file.exists() {
            return fail(state, "The available charge-limit helper cannot safely control this battery combination.");
        }
        let optional = |field: &str| -> Result<Option<String>, ()> {
            let path = directory.join(field);
            if path.exists() { read_text(&path).map(Some).map_err(|_| ()) } else { Ok(None) }
        };
        match optional("scope") {
            Err(_) => return fail(state, UNREADABLE),
            Ok(Some(scope)) if scope != "System" => return fail(state, "Charge limits are available only for system batteries."),
            _ => {}
        }
        match optional("present") {
            Err(_) => return fail(state, UNREADABLE),
            Ok(Some(present)) if present != "1" => return fail(state, "A laptop battery is not currently available."),
            _ => {}
        }
        let stop = threshold(&end_file);
        let start = if start_file.exists() { threshold(&start_file) } else { None };
        let Some(stop) = stop else { return fail(state, UNREADABLE) };
        if start_file.exists() && start.is_none() {
            return fail(state, UNREADABLE);
        }
        let Ok(resolved) = std::fs::canonicalize(&directory) else { return fail(state, UNREADABLE) };
        let mut identity = vec![resolved.to_string_lossy().into_owned()];
        for field in ["manufacturer", "model_name", "serial_number"] {
            match optional(field) {
                Ok(value) => identity.push(value.unwrap_or_default()),
                Err(_) => return fail(state, UNREADABLE),
            }
        }
        records.insert(known[&name].clone(), Record { name, start, stop, identity });
    }
    if records.is_empty() {
        return (state, records);
    }
    state["battery_count"] = json!(records.len());
    let stops: std::collections::BTreeSet<i64> = records.values().map(|r| r.stop).collect();
    state["maximum"] = if stops.len() == 1 { json!(stops.iter().next().unwrap()) } else { Value::Null };
    if !helper_files.iter().all(|path| path.is_file()) {
        return fail(state, "Adjusting charge limits requires the KDE charge-limit helper.");
    }
    state["supported"] = json!(true);
    state["reason"] = json!("Applies to all laptop batteries with adjustable charge limits.");
    (state, records)
}

/// Exact identity of the affected batteries, compared before each write.
pub fn identity(records: &Records) -> Vec<(String, String, Vec<String>)> {
    records.iter().map(|(path, record)| (path.clone(), record.name.clone(), record.identity.clone())).collect()
}

/// Helper arguments for one global maximum (`maximum` is the Python value, type-checked).
pub fn arguments_for(maximum: &Value, records: &Records) -> Result<BTreeMap<String, i64>, String> {
    let Some(maximum) = int(Some(maximum)).filter(|m| (1..=100).contains(m)) else {
        return Err("Choose a maximum charge between 1% and 100%.".into());
    };
    if records.is_empty() {
        return Err(NO_LIMIT.into());
    }
    let mut arguments = BTreeMap::from([("chargeStopThreshold".to_owned(), maximum)]);
    let starts: Vec<Option<i64>> = records.values().map(|r| r.start).collect();
    if starts.iter().any(|start| start.is_some_and(|s| s >= maximum)) {
        if starts.iter().any(Option::is_none) {
            return Err("This battery combination cannot safely lower the recharge threshold. Choose a higher maximum charge.".into());
        }
        // KDE writes start before stop. Lowering start first keeps every
        // battery's start <= stop, including when batteries have different
        // limits. Other start thresholds are preserved unless adjustment is
        // necessary because this helper cannot write per-battery values.
        let lowest = starts.iter().flatten().min().copied().unwrap();
        arguments.insert("chargeStartThreshold".into(), lowest.min(maximum - 1));
    }
    Ok(arguments)
}

/// Qt `QDataStream << QString`: BE u32 byte length + UTF-16BE.
pub fn qstring(text: &str) -> Vec<u8> {
    let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut out = (units.len() as u32).to_be_bytes().to_vec();
    out.extend(units);
    out
}

/// Qt's bounded QVariantMap subset: QString keys and qint32 values (sorted keys).
pub fn encode_arguments(arguments: &BTreeMap<String, i64>) -> Result<Vec<u8>, String> {
    let mut blob = (arguments.len() as u32).to_be_bytes().to_vec();
    for (key, value) in arguments {
        let value = i32::try_from(*value).ok();
        let (true, Some(value)) = (key == "chargeStartThreshold" || key == "chargeStopThreshold", value) else {
            return Err("Invalid charge-limit helper arguments.".into());
        };
        // QVariant: QMetaType::Int=2, not-null flag, signed 32-bit value.
        blob.extend(qstring(key));
        blob.extend(2u32.to_be_bytes());
        blob.push(0);
        blob.extend(value.to_be_bytes());
    }
    Ok(blob)
}

/// `performAction(s action, ay callerID, a{sv} details, ay arguments, a{sh} fds)`.
pub fn parameters(action: &str, arguments: Option<&BTreeMap<String, i64>>) -> Result<glib::Variant, String> {
    if action != "getthreshold" && action != "setthreshold" {
        return Err("Invalid charge-limit action.".into());
    }
    let blob = encode_arguments(arguments.unwrap_or(&BTreeMap::new()))?;
    Ok(glib::Variant::tuple_from_iter([
        format!("{HELPER}.{action}").to_variant(),
        glib::Variant::array_from_fixed_array::<u8>(&[]),
        glib::VariantDict::new(None).end(),
        glib::Variant::array_from_fixed_array(&blob),
        glib::Variant::array_from_iter_with_type(glib::VariantTy::new("{sh}").unwrap(), std::iter::empty::<glib::Variant>()),
    ]))
}

/// The `ay` of a `performAction` reply (`value[0]`).
pub fn reply_blob(reply: &glib::Variant) -> Result<Vec<u8>, String> {
    if reply.is_container() && reply.n_children() == 1 {
        let child = reply.child_value(0);
        if child.type_().as_str() == "ay" {
            return Ok(child.fixed_array::<u8>().map(<[u8]>::to_vec).unwrap_or_default());
        }
    }
    Err("Invalid response from the charge-limit helper.".into())
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

const INVALID: &str = "Invalid response from the charge-limit helper.";

impl<'a> Reader<'a> {
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
        let size = self.u32()?;
        if size == 0xffff_ffff {
            return Ok(String::new());
        }
        if size > 8192 || size % 2 != 0 || self.offset + size as usize > self.data.len() {
            return Err(INVALID.into());
        }
        let units: Vec<u16> = self.take(size as usize)?.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).map_err(|_| INVALID.into())
    }
}

/// Strictly decode KAuth's tiny integer result map; reject unknown types.
pub fn decode_reply(data: &[u8]) -> Result<BTreeMap<String, i32>, String> {
    if data.len() > 16384 {
        return Err(INVALID.into());
    }
    let mut reader = Reader { data, offset: 0 };
    let count = reader.u32()?;
    if count > 8 {
        return Err(INVALID.into());
    }
    let mut result = BTreeMap::new();
    for _ in 0..count {
        let key = reader.string()?;
        let kind = reader.u32()?;
        let null = reader.take(1)?[0];
        if kind != 2 || null != 0 {
            return Err("Unsupported response from the charge-limit helper.".into());
        }
        result.insert(key, i32::from_be_bytes(reader.take(4)?.try_into().unwrap()));
    }
    let error_code = reader.u32()?;
    let reply_type = reader.u32()?;
    let description = reader.string()?;
    if reader.offset != data.len() {
        return Err(INVALID.into());
    }
    if reply_type != 2 {
        if !description.is_empty() {
            return Err(description);
        }
        return Err(match error_code {
            4 | 5 => "Authorization was canceled or denied.",
            6 => "The battery charge-limit helper is busy. Try again.",
            _ => "The charge-limit helper could not apply this setting.",
        }.into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn reply(code: u32, kind: u32, description: &str, data: &BTreeMap<String, i64>) -> Vec<u8> {
        let mut blob = encode_arguments(data).unwrap();
        blob.extend(code.to_be_bytes());
        blob.extend(kind.to_be_bytes());
        blob.extend(qstring(description));
        blob
    }

    fn ok() -> Vec<u8> {
        reply(0, 2, "", &BTreeMap::new())
    }

    fn args(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn arguments_match_qt6_qdatastream() {
        // Independently produced using Qt6 QDataStream.writeQString and
        // writeQVariant(80), not generated by the implementation under test.
        let expected = "000000010000002600630068006100720067006500530074006f0070005400680072006500730068006f006c0064000000020000000050";
        let blob = encode_arguments(&args(&[("chargeStopThreshold", 80)])).unwrap();
        let hex: String = blob.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, expected);
    }

    #[test]
    fn success_denial_busy_and_truncated_replies() {
        assert_eq!(decode_reply(&ok()).unwrap(), BTreeMap::new());
        let data = args(&[("chargeStopThreshold", 80)]);
        assert_eq!(decode_reply(&reply(0, 2, "", &data)).unwrap()["chargeStopThreshold"], 80);
        for (code, message) in [(4, "denied"), (5, "denied"), (6, "busy")] {
            assert!(decode_reply(&reply(code, 0, "", &BTreeMap::new())).unwrap_err().contains(message));
        }
        assert!(decode_reply(&reply(0xffff_ffff, 1, "Charge limits not supported", &BTreeMap::new())).unwrap_err().contains("not supported"));
        for blob in [vec![], vec![0u8; 10], vec![0xffu8; 32], vec![0u8; 20000]] {
            assert!(decode_reply(&blob).is_err());
        }
        let mut trailing = ok();
        trailing.push(0);
        assert!(decode_reply(&trailing).is_err());
        // Non-int QVariant (type 10 = QString) is rejected.
        let mut bad = 1u32.to_be_bytes().to_vec();
        bad.extend(qstring("x"));
        bad.extend(10u32.to_be_bytes());
        bad.push(0);
        assert!(decode_reply(&bad).unwrap_err().contains("Unsupported"));
    }

    #[test]
    fn set_parameters_are_scoped_to_known_actions_and_integer_thresholds() {
        let values = parameters("setthreshold", Some(&args(&[("chargeStopThreshold", 80)]))).unwrap();
        assert_eq!(values.type_().as_str(), "(saya{sv}aya{sh})");
        assert_eq!(values.child_value(0).str(), Some("org.kde.powerdevil.chargethresholdhelper.setthreshold"));
        assert_eq!(values.child_value(1).n_children(), 0);
        assert_eq!(values.child_value(2).n_children(), 0);
        assert_eq!(values.child_value(4).n_children(), 0);
        assert_eq!(values.child_value(3).fixed_array::<u8>().unwrap(), encode_arguments(&args(&[("chargeStopThreshold", 80)])).unwrap());
        assert!(parameters("anythingelse", None).is_err());
        assert!(encode_arguments(&args(&[("path", 1)])).is_err());
        assert!(encode_arguments(&args(&[("chargeStopThreshold", 1 << 40)])).is_err());
        let get = parameters("getthreshold", None).unwrap();
        assert_eq!(get.child_value(3).fixed_array::<u8>().unwrap(), [0, 0, 0, 0]);
    }

    fn record(start: Option<i64>) -> Record {
        Record { name: "BAT".into(), start, stop: 90, identity: vec![] }
    }

    #[test]
    fn lowering_limit_preserves_or_safely_adjusts_start() {
        let mut records = Records::from([("a".into(), record(Some(75))), ("b".into(), record(Some(60)))]);
        assert_eq!(arguments_for(&json!(80), &records).unwrap(), args(&[("chargeStopThreshold", 80)]));
        assert_eq!(arguments_for(&json!(50), &records).unwrap(), args(&[("chargeStopThreshold", 50), ("chargeStartThreshold", 49)]));
        assert_eq!(arguments_for(&json!(1), &records).unwrap()["chargeStartThreshold"], 0);
        assert_eq!(arguments_for(&json!(75), &records).unwrap()["chargeStartThreshold"], 60);
        records.get_mut("b").unwrap().start = None;
        assert!(arguments_for(&json!(50), &records).unwrap_err().contains("combination"));
        for value in [json!(true), json!(false), json!(0), json!(101), json!(50.5), json!("80"), Value::Null] {
            assert!(arguments_for(&value, &records).is_err(), "{value}");
        }
        assert_eq!(arguments_for(&json!(80), &Records::new()).unwrap_err(), NO_LIMIT);
        assert_eq!(parse_int(" +1_0 "), Some(10));
        assert_eq!(parse_int("1e3"), None);
    }

    struct Probe {
        root: PathBuf,
        helper: PathBuf,
        supplies: PathBuf,
        devices: Map<String, Value>,
    }

    impl Probe {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!("battery-limits-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let supplies = base.join("supplies");
            fs::create_dir_all(&supplies).unwrap();
            let helper = base.join("helper");
            fs::write(&helper, "fixture").unwrap();
            Self { root: base, helper, supplies, devices: Map::new() }
        }

        fn add(&mut self, name: &str, stop: Option<i64>, start: Option<i64>) -> PathBuf {
            let directory = self.supplies.join(name);
            fs::create_dir(&directory).unwrap();
            for (key, value) in [("type", "Battery"), ("scope", "System"), ("present", "1"),
                                 ("model_name", "Laptop battery"), ("serial_number", name)] {
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
        let mut probe = Probe::new("mixed");
        probe.add("BAT0", Some(80), Some(60));
        let (state, records) = probe.probe();
        assert_eq!(state["supported"], true);
        assert_eq!((state["maximum"].as_i64(), state["battery_count"].as_i64()), (Some(80), Some(1)));
        assert_eq!(records["/BAT0"].start, Some(60));
        assert_eq!(records["/BAT0"].identity[1..], ["".to_owned(), "Laptop battery".into(), "BAT0".into()]);
        probe.add("BAT1", Some(90), Some(70));
        let (state, _) = probe.probe();
        assert_eq!(state["supported"], true);
        assert!(state["maximum"].is_null());
        assert_eq!(state["battery_count"], 2);
        let keys: Vec<_> = state.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["supported", "maximum", "battery_count", "busy", "error", "reason"]);
    }

    #[test]
    fn missing_helper_does_not_claim_adjustable() {
        let mut probe = Probe::new("helper");
        probe.add("BAT0", Some(80), None);
        fs::remove_file(&probe.helper).unwrap();
        let (state, records) = probe.probe();
        assert_eq!(state["supported"], false);
        assert!(records.is_empty());
        assert!(state["reason"].as_str().unwrap().contains("helper"));
    }

    #[test]
    fn peripherals_absent_read_only_and_unmapped_batteries_fail_closed() {
        let mut probe = Probe::new("closed");
        let directory = probe.add("BAT0", Some(80), None);
        assert_eq!(probe.probe().0["supported"], true);
        for (property, value) in [("PowerSupply", json!(false)), ("IsPresent", json!(false)), ("Type", json!(12))] {
            let original = probe.devices["/BAT0"][property].clone();
            probe.devices["/BAT0"][property] = value;
            assert_eq!(probe.probe().0["supported"], false, "{property}");
            probe.devices["/BAT0"][property] = original;
        }
        let end = directory.join("charge_control_end_threshold");
        fs::set_permissions(&end, fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(probe.probe().0["supported"], false);
        fs::set_permissions(&end, fs::Permissions::from_mode(0o644)).unwrap();
        probe.add("BAT1", Some(80), None);
        probe.devices.remove("/BAT1");
        let (state, _) = probe.probe();
        assert_eq!(state["supported"], false);
        assert!(state["reason"].as_str().unwrap().contains("combination"));
    }

    #[test]
    fn scope_presence_and_garbage_fail_closed() {
        let mut probe = Probe::new("scope");
        let directory = probe.add("BAT0", Some(80), None);
        fs::write(directory.join("scope"), "Device").unwrap();
        assert!(probe.probe().0["reason"].as_str().unwrap().contains("system batteries"));
        fs::write(directory.join("scope"), "System\n").unwrap();
        fs::write(directory.join("present"), "0").unwrap();
        assert!(probe.probe().0["reason"].as_str().unwrap().contains("not currently available"));
        fs::write(directory.join("present"), "1").unwrap();
        fs::write(directory.join("charge_control_end_threshold"), "101").unwrap();
        assert_eq!(probe.probe().0["reason"], "The hardware charge limit could not be read.");
        fs::write(directory.join("charge_control_end_threshold"), "80\n").unwrap();
        fs::write(directory.join("charge_control_start_threshold"), "x").unwrap();
        assert_eq!(probe.probe().0["reason"], "The hardware charge limit could not be read.");
        fs::remove_file(directory.join("charge_control_start_threshold")).unwrap();
        // Non-batteries are ignored.
        let mains = probe.supplies.join("AC");
        fs::create_dir(&mains).unwrap();
        fs::write(mains.join("type"), "Mains").unwrap();
        assert_eq!(probe.probe().0["supported"], true);
        // No devices known: no sysfs scan at all.
        assert_eq!(inspect_limits(&Map::new(), Path::new("/nonexistent"), &[]).0, initial_state());
    }

    #[test]
    fn start_only_companion_prevents_partial_global_write() {
        let mut probe = Probe::new("start-only");
        probe.add("BAT0", Some(80), None);
        probe.add("BAT1", None, Some(60));
        assert_eq!(probe.probe().0["supported"], false);
    }
}
