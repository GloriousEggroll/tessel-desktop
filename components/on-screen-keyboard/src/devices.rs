//! Read input metadata, never key events. Udev hotplug drives all updates (osk_devices.py).
use crate::config::Settings;
use regex::Regex;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Device {
    pub name: String,
    pub identity: String,
    pub kind: String,
}

impl Device {
    pub fn new(name: &str, identity: &str, kind: &str) -> Self {
        Self { name: name.into(), identity: identity.into(), kind: kind.into() }
    }
}

fn excluded_names() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(mouse|trackball|touchpad|touchscreen|hotkeys|consumer control|system control)\b").unwrap())
}

/// `classify(properties, sys_path, name, seat=…)` → `"touchscreen"`, `"keyboard"` or `None`.
pub fn classify(properties: &HashMap<String, String>, sys_path: &str, name: &str, seat: &str) -> Option<&'static str> {
    let get = |key: &str| properties.get(key).map(String::as_str);
    if get("ID_SEAT").unwrap_or("seat0") != seat {
        return None;
    }
    // Bluetooth HID devices may live under virtual/misc/uhid; those are real.
    if sys_path.starts_with("/sys/devices/virtual/input/") {
        return None;
    }
    if get("LIBINPUT_IGNORE_DEVICE") == Some("1") {
        return None;
    }
    if get("ID_INPUT_TOUCHSCREEN") == Some("1") {
        return Some("touchscreen");
    }
    if get("ID_INPUT_KEYBOARD") != Some("1") {
        return None;
    }
    if ["ID_INPUT_MOUSE", "ID_INPUT_TOUCHPAD", "ID_INPUT_JOYSTICK"].iter().any(|k| get(k) == Some("1")) {
        return None;
    }
    if excluded_names().is_match(name) {
        return None;
    }
    Some("keyboard")
}

/// Close to Python's `str.casefold()` for sorting names.
pub fn casefold(text: &str) -> String { text.to_lowercase().replace('ß', "ss") }

/// Dedup by (identity, kind), sorted by (kind, casefolded name).
pub fn collect(items: impl IntoIterator<Item = (HashMap<String, String>, String, String)>, seat: &str) -> Vec<Device> {
    let mut result: BTreeMap<(String, String), (usize, Device)> = BTreeMap::new();
    for (index, (properties, sys_path, name)) in items.into_iter().enumerate() {
        let Some(kind) = classify(&properties, &sys_path, &name, seat) else { continue };
        // Stable over replug/reboot, including duplicate event interfaces.
        let serial = properties.get("ID_SERIAL").or_else(|| properties.get("ID_PATH")).cloned().unwrap_or_default();
        let identity = format!("{serial}|{name}");
        let key = (identity.clone(), kind.to_string());
        let order = result.get(&key).map_or(index, |(order, _)| *order);
        result.insert(key, (order, Device { name, identity, kind: kind.into() }));
    }
    let mut ordered: Vec<(usize, Device)> = result.into_values().collect();
    // Python's dict keeps first-insertion order and sorted() is stable.
    ordered.sort_by_key(|(order, _)| *order);
    let mut devices: Vec<Device> = ordered.into_iter().map(|(_, d)| d).collect();
    devices.sort_by(|a, b| (a.kind.as_str(), casefold(&a.name)).cmp(&(b.kind.as_str(), casefold(&b.name))));
    devices
}

pub fn seat() -> String { std::env::var("XDG_SEAT").unwrap_or_else(|_| "seat0".into()) }

/// `discover()`: every udev `input/event*` node.
pub fn discover() -> Vec<Device> {
    let Ok(mut enumerator) = udev::Enumerator::new() else { return Vec::new() };
    if enumerator.match_subsystem("input").is_err() {
        return Vec::new();
    }
    let Ok(list) = enumerator.scan_devices() else { return Vec::new() };
    let items = list.filter_map(|item| {
        let sys_name = item.sysname().to_string_lossy().into_owned();
        if !sys_name.starts_with("event") {
            return None;
        }
        let name = match item.parent_with_subsystem("input") {
            Ok(Some(parent)) => parent.attribute_value("name").and_then(|v| v.to_str()).map(str::to_owned).unwrap_or_else(|| sys_name.clone()),
            _ => sys_name.clone(),
        };
        let properties = item.properties()
            .map(|p| (p.name().to_string_lossy().into_owned(), p.value().to_string_lossy().into_owned())).collect();
        Some((properties, item.syspath().to_string_lossy().into_owned(), name))
    });
    collect(items, &seat())
}

pub fn automatic_available(devices: &[Device], settings: &Settings) -> bool {
    settings.automatic
        && devices.iter().any(|d| d.kind == "touchscreen")
        && !devices.iter().any(|d| d.kind == "keyboard" && !settings.ignore_keyboards.contains(&d.identity))
}

/// Udev netlink monitor (`subsystem=input`), debounced 180 ms, then `changed(discover())`.
pub struct DeviceMonitor(Rc<RefCell<MonitorInner>>);

struct MonitorInner {
    source: Option<glib::SourceId>,
    pending: Option<glib::SourceId>,
}

impl DeviceMonitor {
    pub fn new(changed: impl Fn(Vec<Device>) + 'static) -> Self {
        let inner = Rc::new(RefCell::new(MonitorInner { source: None, pending: None }));
        let socket = udev::MonitorBuilder::new().and_then(|b| b.match_subsystem("input")).and_then(|b| b.listen());
        match socket {
            Ok(socket) => {
                let fd = socket.as_raw_fd();
                let changed = Rc::new(changed);
                let weak = Rc::downgrade(&inner);
                let id = glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN, move |_, _| {
                    while socket.iter().next().is_some() {}
                    let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
                    if inner.borrow().pending.is_none() {
                        let (weak, changed) = (Rc::downgrade(&inner), changed.clone());
                        let id = glib::timeout_add_local_once(Duration::from_millis(180), move || {
                            if let Some(inner) = weak.upgrade() {
                                inner.borrow_mut().pending = None;
                            }
                            changed(discover());
                        });
                        inner.borrow_mut().pending = Some(id);
                    }
                    glib::ControlFlow::Continue
                });
                inner.borrow_mut().source = Some(id);
            }
            Err(error) => eprintln!("waykeys: udev monitor unavailable: {error}"),
        }
        Self(inner)
    }

    pub fn close(&self) {
        let (source, pending) = {
            let mut inner = self.0.borrow_mut();
            (inner.source.take(), inner.pending.take())
        };
        for id in [source, pending].into_iter().flatten() {
            id.remove();
        }
    }
}

impl Drop for DeviceMonitor {
    fn drop(&mut self) { self.close() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::validate;
    use serde_json::json;

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // test_policy.py: test_hardware_combinations_and_explicit_receiver_override
    #[test]
    fn hardware_combinations_and_explicit_receiver_override() {
        let touch = Device::new("Touch", "touch", "touchscreen");
        let keyboard = Device::new("Keys", "keys", "keyboard");
        let mut settings = validate(&json!({})).unwrap();
        for (devices, wanted) in [(vec![], false), (vec![keyboard.clone()], false), (vec![touch.clone()], true),
                                  (vec![touch.clone(), keyboard.clone()], false)] {
            assert_eq!(automatic_available(&devices, &settings), wanted);
        }
        settings.ignore_keyboards = vec!["keys".into()];
        assert!(automatic_available(&[touch.clone(), keyboard], &settings));
        settings.automatic = false;
        assert!(!automatic_available(&[touch], &settings));
    }

    // test_policy.py: test_mouse_hotkeys_virtual_inputs_and_other_seats_do_not_block
    #[test]
    fn mouse_hotkeys_virtual_inputs_and_other_seats_do_not_block() {
        let keyboard = [("ID_INPUT_KEYBOARD", "1")];
        let with = |extra: (&str, &str)| props(&[keyboard[0], extra]);
        for (p, path, name) in [(props(&keyboard), "/sys/devices/virtual/input/input3", "Virtual Keyboard"),
                                (props(&keyboard), "/sys/devices/pci/x", "Mouse Receiver"),
                                (with(("ID_INPUT_MOUSE", "1")), "/sys/devices/pci/x", "USB"),
                                (with(("ID_INPUT_JOYSTICK", "1")), "/sys/devices/pci/x", "Pad"),
                                (with(("ID_SEAT", "seat1")), "/sys/devices/pci/x", "Keyboard")] {
            assert_eq!(classify(&p, path, name, "seat0"), None, "{name}");
        }
        assert_eq!(classify(&props(&keyboard), "/sys/devices/virtual/misc/uhid/input1", "Bluetooth Keyboard", "seat0"), Some("keyboard"));
        assert_eq!(classify(&props(&[("ID_INPUT_TOUCHSCREEN", "1")]), "/sys/devices/pci/y", "Touch", "seat0"), Some("touchscreen"));
        assert_eq!(classify(&props(&[("ID_INPUT_KEYBOARD", "1"), ("LIBINPUT_IGNORE_DEVICE", "1")]), "/sys/x", "K", "seat0"), None);
        assert_eq!(classify(&props(&keyboard), "/sys/x", "Logitech Consumer Control", "seat0"), None);
        assert_eq!(classify(&props(&keyboard), "/sys/x", "Mousepad Keyboard", "seat0"), Some("keyboard"));
        assert_eq!(classify(&props(&[("ID_INPUT_KEYBOARD", "1"), ("ID_SEAT", "seat1")]), "/sys/x", "K", "seat1"), Some("keyboard"));
    }

    #[test]
    fn identity_dedup_and_sorting() {
        let kb = |serial: Option<&str>, path: Option<&str>| {
            let mut p = props(&[("ID_INPUT_KEYBOARD", "1")]);
            if let Some(s) = serial { p.insert("ID_SERIAL".into(), s.into()); }
            if let Some(s) = path { p.insert("ID_PATH".into(), s.into()); }
            p
        };
        let devices = collect([
            (kb(Some("usb-1"), Some("pci-1")), "/sys/a".into(), "beta".into()),
            (kb(Some("usb-1"), None), "/sys/b".into(), "beta".into()),
            (kb(None, Some("pci-2")), "/sys/c".into(), "Alpha".into()),
            (kb(None, None), "/sys/d".into(), "gamma".into()),
            (props(&[("ID_INPUT_TOUCHSCREEN", "1")]), "/sys/e".into(), "Panel".into()),
        ], "seat0");
        let names: Vec<_> = devices.iter().map(|d| (d.kind.as_str(), d.identity.as_str())).collect();
        assert_eq!(names, [("keyboard", "pci-2|Alpha"), ("keyboard", "usb-1|beta"), ("keyboard", "|gamma"), ("touchscreen", "|Panel")]);
    }
}
