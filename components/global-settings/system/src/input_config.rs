//! input_config.py: per-device libinput settings with scoped,
//! conflict-checked labwc XML edits.
use std::collections::BTreeMap;
use std::path::PathBuf;

use tiling_manager::xml::{Element, Node};

use crate::compat::{self, casefold, element_indices, local, tag_end, value, Error, Result};
use crate::workspace_config::{parse_section, session_config, Parsed, Reload, WorkspaceConfig};

pub const CHOICES: &[(&str, &[&str])] = &[
    ("sendEventsMode", &["yes", "no", "disabledOnExternalMouse"]),
    ("accelProfile", &["flat", "adaptive"]),
    ("leftHanded", &["yes", "no"]),
    ("naturalScroll", &["yes", "no"]),
    ("middleEmulation", &["yes", "no"]),
    ("scrollMethod", &["none", "twofinger", "edge", "onbutton"]),
    ("tap", &["yes", "no"]),
    ("tapButtonMap", &["lrm", "lmr"]),
    ("tapAndDrag", &["yes", "no"]),
    ("dragLock", &["no", "yes", "timeout"]),
    ("threeFingerDrag", &["no", "3", "4"]),
    ("disableWhileTyping", &["yes", "no"]),
    ("clickMethod", &["none", "buttonAreas", "clickfinger"]),
];
pub const NUMBERS: &[(&str, (f64, f64))] = &[("pointerSpeed", (-1.0, 1.0)), ("scrollFactor", (0.05, 10.0)), ("scrollButton", (1.0, 767.0))];

pub fn keys() -> Vec<&'static str> { CHOICES.iter().map(|(k, _)| *k).chain(NUMBERS.iter().map(|(k, _)| *k)).collect() }

fn canonical(key: &str) -> Option<&'static str> { keys().into_iter().find(|k| casefold(k) == key) }

/// Python's repr of a float bound (`-1`, `0.05`, `10`).
fn bound(value: f64) -> String { if value.fract() == 0.0 { format!("{}", value as i64) } else { format!("{value}") } }

/// `validate(key, value)`; values are already `str(value)`.
pub fn validate(key: &str, value: Option<&str>) -> Result<Option<String>> {
    if !keys().contains(&key) {
        return compat::value(format!("Unknown input setting: {key}"));
    }
    let Some(item) = value else { return Ok(None) };
    if let Some((_, choices)) = CHOICES.iter().find(|(k, _)| *k == key) {
        if !choices.contains(&item) {
            return compat::value(format!("Invalid value for {key}"));
        }
    } else {
        let number = compat::py_float(item)?;
        let (low, high) = NUMBERS.iter().find(|(k, _)| *k == key).unwrap().1;
        if !number.is_finite() || !(low <= number && number <= high) {
            return compat::value(format!("{key} must be between {} and {}.", bound(low), bound(high)));
        }
        if key == "scrollButton" && number.fract() != 0.0 {
            return compat::value("The scroll button must be an integer event code.");
        }
    }
    Ok(Some(item.to_owned()))
}

fn parse(data: &[u8]) -> Result<Parsed> {
    parse_section(data, "libinput", "XML document types are not supported in input settings.",
                  "Expected one labwc_config with at most one libinput section.")
}

/// `_properties(node)`: attributes then children (casefolded keys), last wins.
fn properties(node: &Element) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    let mut put = |key: String, item: String| {
        if let Some(slot) = result.iter_mut().find(|(k, _)| *k == key) { slot.1 = item } else { result.push((key, item)) }
    };
    for (name, item) in &node.attributes {
        put(casefold(name), item.clone());
    }
    for child in &node.children {
        if let Node::Element(e) = child {
            put(casefold(local(&e.name)), compat::text(e));
        }
    }
    result
}

fn category(node: &Element) -> String {
    properties(node).into_iter().find(|(k, _)| k == "category").map(|(_, v)| v).unwrap_or_else(|| "default".into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// Canonical key → value, in document (dict) order.
    pub values: Vec<(String, String)>,
    pub xml: String,
}

impl Profile {
    pub fn get(&self, key: &str) -> Option<&str> { self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()) }
    pub fn values_map(&self) -> BTreeMap<String, String> { self.values.iter().cloned().collect() }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct InputState {
    /// Casefolded profile name → profile (insertion order kept).
    pub profiles: Vec<(String, Profile)>,
}

impl InputState {
    pub fn profile(&self, key: &str) -> Option<&Profile> { self.profiles.iter().find(|(k, _)| k == key).map(|(_, p)| p) }

    pub fn contains(&self, key: &str) -> bool { self.profile(key).is_some() }

    pub fn match_profile(&self, name: &str, kind: &str) -> Profile {
        for key in [casefold(name), kind.to_owned(), "default".to_owned()] {
            if let Some(profile) = self.profile(&key) {
                return profile.clone();
            }
        }
        Profile { name: kind.to_owned(), values: Vec::new(), xml: String::new() }
    }
}

pub fn read_state(data: &[u8]) -> Result<InputState> {
    let parsed = parse(data)?;
    let mut state = InputState::default();
    if let Some(index) = parsed.section {
        let section = compat::child_element(&parsed.doc.root, index);
        for i in element_indices(section, "device") {
            let node = compat::child_element(section, i);
            let props = properties(node);
            let name = props.iter().find(|(k, _)| k == "category").map(|(_, v)| v.clone()).unwrap_or_else(|| "default".into());
            let values = props.iter().filter_map(|(k, v)| canonical(k).map(|c| (c.to_owned(), v.clone()))).collect();
            // Matching is case insensitive, and labwc gives the last profile priority.
            let key = casefold(&name);
            let profile = Profile { name, values, xml: node.to_xml() };
            if let Some(slot) = state.profiles.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = profile;
            } else {
                state.profiles.push((key, profile));
            }
        }
    }
    Ok(state)
}

/// One `(name, kind) → changes` entry; `None` removes the device override.
pub type Updates = Vec<((String, String), Option<Vec<(String, Option<String>)>>)>;

/// `updates` maps (device name or category, kind) to changed keys only.
pub fn with_profiles(data: &[u8], updates: &Updates, expected: &InputState) -> Result<Vec<u8>> {
    let current = read_state(data)?;
    for ((name, kind), values) in updates {
        let folded = casefold(name);
        if values.is_none() && ["default", "non-touch", "touchpad", "touch"].contains(&folded.as_str()) {
            return value("Only individual device overrides can be removed.");
        }
        for (key, item) in values.iter().flatten() {
            validate(key, item.as_deref())?;
        }
        let send = values.iter().flatten().find(|(k, _)| k == "sendEventsMode").and_then(|(_, v)| v.clone());
        if ["default", "non-touch"].contains(&folded.as_str()) && send.as_deref().is_some_and(|v| v != "yes") {
            return value("Disable an individual mouse instead; this category also includes keyboards.");
        }
        if current.match_profile(name, kind) != expected.match_profile(name, kind) {
            return compat::runtime("Input settings changed elsewhere. Reload before applying.");
        }
    }
    let parsed = parse(data)?;
    let mut section = match parsed.section {
        Some(index) => compat::child_element(&parsed.doc.root, index).clone(),
        None => compat::new_element("libinput"),
    };
    // Defaults first: a new device override must clone its original profile,
    // not a category modified in the same transaction.
    for ((name, kind), values) in updates {
        let folded = casefold(name);
        let nodes: Vec<usize> = element_indices(&section, "device").into_iter()
            .filter(|i| casefold(&category(compat::child_element(&section, *i))) == folded).collect();
        let Some(values) = values else {
            for index in nodes.iter().rev() {
                section.children.remove(*index);
            }
            continue;
        };
        if values.is_empty() {
            continue;
        }
        let index = match nodes.last() {
            Some(index) => *index,
            None => {
                let original = expected.match_profile(name, kind);
                let mut node = if original.xml.is_empty() {
                    compat::new_element("device")
                } else {
                    let mut fragment = compat::parse_xml(original.xml.as_bytes(), "").map_err(|e| Error::Xml(e.to_string()))?.root;
                    compat::minidom_order(&mut fragment);
                    fragment
                };
                node.attributes.retain(|(attr, _)| casefold(attr) != "category");
                node.children.retain(|n| !matches!(n, Node::Element(e) if compat::local_lower(&e.name) == "category"));
                compat::set_attribute(&mut node, "category", name);
                section.children.push(Node::Text("\n    ".into()));
                section.children.push(Node::Element(node));
                section.children.push(Node::Text("\n  ".into()));
                section.self_closing = false;
                section.children.len() - 2
            }
        };
        let node = compat::child_element_mut(&mut section, index);
        for (key, item) in values {
            let folded_key = casefold(key);
            node.attributes.retain(|(attr, _)| casefold(attr) != folded_key);
            node.children.retain(|n| !matches!(n, Node::Element(e) if compat::local_lower(&e.name) == folded_key));
            if let Some(item) = item {
                node.children.push(Node::Element(compat::text_element(key, item)));
                node.self_closing = false;
            }
        }
    }
    let replacement = section.to_xml().into_bytes();
    if let Some((begin, end)) = parsed.span {
        return Ok([&data[..begin], &replacement[..], &data[end..]].concat());
    }
    // Find the actual root opening (not XML declarations or comments).
    let opening = tag_end(data, parsed.doc.root.start)?;
    if compat::bytes_rstrip(&data[..opening]).ends_with(b"/>") {
        let name = parsed.doc.root.name.as_bytes();
        return Ok([&data[..opening - 2], b">\n  ", &replacement[..], b"\n</", name, b">", &data[opening..]].concat());
    }
    let end = parsed.root_end;
    Ok([&data[..end], b"  ", &replacement[..], b"\n", &data[end..]].concat())
}

/// The workspace store's file handling with input backups and messages.
#[derive(Clone)]
pub struct InputConfig(pub WorkspaceConfig);

impl InputConfig {
    /// `InputConfig(path=None, **kwargs)`.
    pub fn new(path: Option<PathBuf>, fallback: Option<PathBuf>, reload: Option<Reload>, backup_dir: Option<PathBuf>) -> Result<Self> {
        let (path, fallback) = match path {
            Some(path) => (path, fallback),
            None => session_config().map_err(|error| match error {
                Error::Runtime(message) => Error::Runtime(message.replace("Workspace count editing", "Input editing")
                    .replace("Workspace settings", "Input settings")),
                other => other,
            }).map(|(p, f)| (p, fallback.or(f)))?,
        };
        let backup = backup_dir.unwrap_or_else(|| crate::labwc_settings::state_home().join("desktop-suite/backups/input"));
        Ok(InputConfig(WorkspaceConfig::with_backup(path, fallback, reload, backup)))
    }

    pub fn at(path: impl Into<PathBuf>, backup_dir: impl Into<PathBuf>, reload: Reload) -> Self {
        InputConfig(WorkspaceConfig::at(path, backup_dir, reload))
    }

    pub fn path(&self) -> &std::path::Path { &self.0.path }
    pub fn backup_dir(&self) -> &std::path::Path { &self.0.backup_dir }

    pub fn read(&self) -> Result<Vec<u8>> { self.0.read() }

    pub fn load(&self) -> Result<InputState> { read_state(&self.0.read()?) }

    pub fn save(&self, updates: &Updates, expected: &InputState) -> Result<InputState> {
        let original = self.0.read()?;
        if updates.is_empty() {
            return read_state(&original);
        }
        let updated = with_profiles(&original, updates, expected)?;
        self.0.commit(&original, &updated, "Could not reload labwc; the previous input settings were restored.")?;
        read_state(&updated)
    }
}

/// Test/UI convenience: `{(name, kind): {key: value}}`.
pub fn update(name: &str, kind: &str, values: &[(&str, Option<&str>)]) -> ((String, String), Option<Vec<(String, Option<String>)>>) {
    ((name.into(), kind.into()), Some(values.iter().map(|(k, v)| (k.to_string(), v.map(str::to_owned))).collect()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::input_devices::parse_devices;
    use crate::testutil::scratch;
    use std::sync::{Arc, Mutex};

    pub const CONFIG: &[u8] = br#"<?xml version="1.0"?>
<!-- Keep the rest of this file byte for byte. -->
<labwc_config>
  <mouse><default/><context name="Root"><mousebind button="Right" action="Press"/></context></mouse>
  <libinput>
    <!-- Device names and unknown options must survive. -->
    <device category="default"><leftHanded>no</leftHanded></device>
    <device category="non-touch" pointerSpeed="0.2"><accelProfile>flat</accelProfile><futureOption>yes</futureOption></device>
    <device category="touchpad"><tap>yes</tap><scrollMethod>twofinger</scrollMethod></device>
  </libinput>
  <hdrOutputs><output name="DP-2"/></hdrOutputs>
</labwc_config>
"#;

    pub const LAPTOP: &str = "Device:                  SNSL002D:00 2C2F:002D Touchpad
Kernel:                  /dev/input/event6
Capabilities:            pointer gesture
Tap-to-click:            disabled
Tap-and-drag:            enabled
Tap button map:          left/right/middle
Tap drag lock:           disabled
Left-handed:             disabled
Nat.scrolling:           disabled
Middle emulation:        disabled
Scroll methods:          *two-finger edge
Scroll button:           n/a
Click methods:           *button-areas clickfinger
Disable-w-typing:        enabled
Accel profiles:          flat *adaptive custom

Device:                  TPPS/2 Elan TrackPoint
Kernel:                  /dev/input/event4
Capabilities:            pointer
Tap-to-click:            n/a
Scroll methods:          *button
Scroll button:           BTN_MIDDLE
Click methods:           none
Accel profiles:          flat *adaptive custom

Device:                  AT Translated Set 2 keyboard
Kernel:                  /dev/input/event3
Capabilities:            keyboard
Tap-to-click:            n/a
";

    fn values(state: &InputState, name: &str, kind: &str) -> Vec<(String, String)> { state.match_profile(name, kind).values }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> { items.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }

    fn has(data: &[u8], needle: &str) -> bool { data.windows(needle.len()).any(|w| w == needle.as_bytes()) }

    fn split<'a>(data: &'a [u8], marker: &str, after: bool) -> &'a [u8] {
        let at = data.windows(marker.len()).position(|w| w == marker.as_bytes()).unwrap();
        if after { &data[at + marker.len()..] } else { &data[..at] }
    }

    #[test]
    fn device_matching_has_no_property_inheritance() {
        let state = read_state(CONFIG).unwrap();
        assert_eq!(values(&state, "mouse", "non-touch"), pairs(&[("pointerSpeed", "0.2"), ("accelProfile", "flat")]));
        let updated = with_profiles(CONFIG, &vec![update("Mouse", "non-touch", &[("pointerSpeed", Some("0.5"))])], &state).unwrap();
        let selected = read_state(&updated).unwrap().match_profile("mOuSe", "non-touch");
        assert_eq!(selected.values_map(), pairs(&[("pointerSpeed", "0.5"), ("accelProfile", "flat")]).into_iter().collect());
        assert!(selected.xml.contains("futureOption"));
        assert_eq!(read_state(&updated).unwrap().match_profile("another", "non-touch").get("pointerSpeed"), Some("0.2"));
    }

    #[test]
    fn only_input_section_changes_and_unknown_values_survive() {
        let updated = with_profiles(CONFIG, &vec![update("non-touch", "non-touch", &[("pointerSpeed", Some("-0.3")), ("naturalScroll", Some("yes"))])],
                                    &read_state(CONFIG).unwrap()).unwrap();
        assert_eq!(split(&updated, "<libinput>", false), split(CONFIG, "<libinput>", false));
        assert_eq!(split(&updated, "</libinput>", true), split(CONFIG, "</libinput>", true));
        assert!(has(&updated, "futureOption"));
        assert!(has(&updated, "<!-- Device names"));
        assert!(!has(&updated, "pointerSpeed=\"0.2\""));
    }

    #[test]
    fn attributes_children_case_and_duplicate_profiles() {
        let data = br#"<labwc_config><libinput><device category="Mouse" tap="no"><Tap>yes</Tap></device><device><category>mouse</category><naturalScroll>yes</naturalScroll></device></libinput></labwc_config>"#;
        let state = read_state(data).unwrap();
        assert_eq!(values(&state, "Mouse", "non-touch"), pairs(&[("naturalScroll", "yes")]));
        let updated = with_profiles(data, &vec![update("MOUSE", "non-touch", &[("naturalScroll", None), ("leftHanded", Some("yes"))])], &state).unwrap();
        assert_eq!(values(&read_state(&updated).unwrap(), "mouse", "non-touch"), pairs(&[("leftHanded", "yes")]));
    }

    #[test]
    fn simultaneous_new_override_clones_original_defaults() {
        let updated = with_profiles(CONFIG, &vec![update("non-touch", "non-touch", &[("pointerSpeed", Some("-1"))]),
                                                  update("A & B", "non-touch", &[("leftHanded", Some("yes"))])],
                                    &read_state(CONFIG).unwrap()).unwrap();
        let state = read_state(&updated).unwrap();
        assert_eq!(state.match_profile("A & B", "non-touch").get("pointerSpeed"), Some("0.2"));
        assert_eq!(state.match_profile("unlisted", "non-touch").get("pointerSpeed"), Some("-1"));
    }

    #[test]
    fn remove_override_returns_to_shared_defaults_including_duplicates() {
        let data = String::from_utf8_lossy(CONFIG).replace("</libinput>",
            "<device category=\"Mouse\"><pointerSpeed>1</pointerSpeed></device><device category=\"mouse\"><pointerSpeed>-1</pointerSpeed></device></libinput>").into_bytes();
        let removal: Updates = vec![(("Mouse".into(), "non-touch".into()), None)];
        let updated = with_profiles(&data, &removal, &read_state(&data).unwrap()).unwrap();
        assert!(!read_state(&updated).unwrap().contains("mouse"));
        assert_eq!(read_state(&updated).unwrap().match_profile("Mouse", "non-touch").get("pointerSpeed"), Some("0.2"));
        let bad: Updates = vec![(("non-touch".into(), "non-touch".into()), None)];
        assert!(with_profiles(&data, &bad, &read_state(&data).unwrap()).unwrap_err().is_value());
    }

    #[test]
    fn mouse_defaults_cannot_disable_keyboards() {
        let error = with_profiles(CONFIG, &vec![update("non-touch", "non-touch", &[("sendEventsMode", Some("no"))])], &read_state(CONFIG).unwrap()).unwrap_err();
        assert!(error.is_value() && error.message().contains("keyboards"));
        with_profiles(CONFIG, &vec![update("Mouse", "non-touch", &[("sendEventsMode", Some("no"))])], &read_state(CONFIG).unwrap()).unwrap();
    }

    #[test]
    fn empty_namespaced_and_missing_section() {
        for data in [&b"<labwc_config/>"[..], b"<!-- <labwc_config is a comment --><labwc_config/>",
                     b"<labwc_config xmlns=\"http://openbox.org/3.4/rc\"><mouse/></labwc_config>"] {
            let updated = with_profiles(data, &vec![update("touchpad", "touchpad", &[("clickMethod", Some("buttonAreas"))])], &read_state(data).unwrap()).unwrap();
            assert_eq!(read_state(&updated).unwrap().match_profile("touchpad", "touchpad").get("clickMethod"), Some("buttonAreas"));
        }
    }

    #[test]
    fn invalid_options_rejected_before_writing() {
        for (key, item) in [("pointerSpeed", "nan"), ("pointerSpeed", "2"), ("scrollFactor", "0"), ("scrollButton", "2.5"),
                            ("accelProfile", "custom"), ("anything", "yes")] {
            assert!(with_profiles(CONFIG, &vec![update("non-touch", "non-touch", &[(key, Some(item))])], &read_state(CONFIG).unwrap())
                .unwrap_err().is_value(), "{key}={item}");
        }
        for data in [&b"<!DOCTYPE labwc_config><labwc_config/>"[..], b"<oops/>", b"<labwc_config><libinput/><libinput/></labwc_config>"] {
            assert!(read_state(data).unwrap_err().is_value());
        }
        assert_eq!(validate("pointerSpeed", Some("2")).unwrap_err().message(), "pointerSpeed must be between -1 and 1.");
        assert_eq!(validate("scrollFactor", Some("0")).unwrap_err().message(), "scrollFactor must be between 0.05 and 10.");
    }

    #[test]
    fn conflicts_only_with_changed_profile_and_merges_other_edits() {
        let state = read_state(CONFIG).unwrap();
        let other = String::from_utf8_lossy(CONFIG).replace("<tap>yes</tap>", "<tap>no</tap>").into_bytes();
        with_profiles(&other, &vec![update("non-touch", "non-touch", &[("leftHanded", Some("yes"))])], &state).unwrap();
        let error = with_profiles(&other, &vec![update("touchpad", "touchpad", &[("leftHanded", Some("yes"))])], &state).unwrap_err();
        assert!(error.is_runtime() && error.message().contains("changed elsewhere"));
    }

    #[test]
    fn persistence_backup_and_reload_rollback() {
        let folder = scratch("input-persist");
        let path = folder.join("rc.xml");
        std::fs::write(&path, CONFIG).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let counter = calls.clone();
        let mut config = InputConfig::at(&path, folder.join("backups"), Arc::new(move || {
            *counter.lock().unwrap() += 1;
            Ok(())
        }));
        let state = config.load().unwrap();
        config.save(&vec![], &state).unwrap();
        assert_eq!(*calls.lock().unwrap(), 0);
        config.save(&vec![update("touchpad", "touchpad", &[("tap", Some("no"))])], &state).unwrap();
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!(config.load().unwrap().match_profile("touchpad", "touchpad").get("tap"), Some("no"));
        let backup = std::fs::read_dir(config.backup_dir()).unwrap().next().unwrap().unwrap().path();
        assert_eq!(std::fs::read(backup).unwrap(), CONFIG);
        let original = std::fs::read(&path).unwrap();
        config.0.reload = Arc::new(|| Err("reload failed".into()));
        let error = config.save(&vec![update("touchpad", "touchpad", &[("tap", Some("yes"))])], &config.load().unwrap()).unwrap_err();
        assert!(error.message().contains("restored"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn laptop_capabilities_not_mistaken_for_current_settings() {
        let devices = parse_devices(LAPTOP);
        assert_eq!(devices.len(), 2);
        let (pad, point) = (&devices[0], &devices[1]);
        assert_eq!(pad.kind, "touchpad");
        assert!(pad.supports("tap"));
        assert!(pad.allowed("scrollMethod", Some("edge")));
        assert!(!pad.allowed("scrollMethod", Some("onbutton")));
        assert!(pad.allowed("clickMethod", Some("buttonAreas")));
        assert!(pad.allowed("clickMethod", Some("clickfinger")));
        assert!(!point.supports("tap"));
        assert!(point.allowed("scrollMethod", Some("onbutton")));
        assert_eq!(read_state(CONFIG).unwrap().match_profile(&pad.name, &pad.kind).get("tap"), Some("yes"));
    }
}
