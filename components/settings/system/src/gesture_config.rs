//! Touchpad gestures: labwc's `<touchpad>` section of rc.xml, edited in place
//! with the rest of the file kept byte for byte.
//!
//! ```xml
//! <touchpad>
//!   <gesture type="swipe" fingers="3" direction="left"><action name="GoToDesktop" to="right" wrap="no"/></gesture>
//! </touchpad>
//! ```
//!
//! A gesture without an element goes to the application under the pointer.
//! The section stays, empty, when the last gesture is set to Do nothing: an rc.xml
//! with a `<touchpad>` section is the user's choice, and `tessel-configure-session`
//! only adds Tessel's gestures to one without it.
use tessel_core::session_keys::{gesture_action, GESTURE_CHOICES, GESTURE_SLOTS};
use tessel_tiling::xml::{Element, Node};

use crate::compat::{self, casefold, element_indices, local_lower, tag_end, value, Result};
use crate::workspace_config::{parse_section, Parsed};

/// Choice id → label (translated by the page). "none" removes the gesture.
pub const CHOICES: [(&str, &str); 11] = [
    ("none", "Do nothing"),
    ("next-workspace", "Next workspace"),
    ("previous-workspace", "Previous workspace"),
    ("overview", "Window overview"),
    ("app-menu", "Application menu"),
    ("show-desktop", "Show the desktop"),
    ("maximize", "Maximize or restore window"),
    ("minimize", "Minimize window"),
    ("fullscreen", "Toggle fullscreen"),
    ("close", "Close window"),
    ("lock", "Lock screen"),
];

/// The page's row key (and search target) of a gesture: `gesture-swipe-3-left`.
pub fn slot_key(kind: &str, fingers: u32, direction: &str) -> String { format!("gesture-{kind}-{fingers}-{direction}") }

/// Every gesture row key, in page order.
pub fn slot_keys() -> Vec<String> { GESTURE_SLOTS.iter().map(|(k, f, d)| slot_key(k, *f, d)).collect() }

/// The row title of a gesture (translated by the page).
pub fn slot_title(key: &str) -> &'static str {
    match key {
        "gesture-swipe-3-left" => "Three-finger swipe left",
        "gesture-swipe-3-right" => "Three-finger swipe right",
        "gesture-swipe-3-up" => "Three-finger swipe up",
        "gesture-swipe-3-down" => "Three-finger swipe down",
        "gesture-swipe-4-left" => "Four-finger swipe left",
        "gesture-swipe-4-right" => "Four-finger swipe right",
        "gesture-swipe-4-up" => "Four-finger swipe up",
        "gesture-swipe-4-down" => "Four-finger swipe down",
        "gesture-pinch-4-in" => "Four-finger pinch in",
        "gesture-pinch-4-out" => "Four-finger pinch out",
        _ => "",
    }
}

/// What one gesture does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    /// One of [`CHOICES`] ("none" when rc.xml has no element for it).
    Choice(String),
    /// Anything else, as rc.xml has it; kept untouched unless replaced.
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GestureState {
    /// rc.xml has a `<touchpad>` section (possibly empty).
    pub present: bool,
    /// Slot key → binding for every gesture the page shows.
    pub bindings: Vec<(String, Binding)>,
}

impl GestureState {
    pub fn get(&self, key: &str) -> Binding {
        self.bindings.iter().find(|(k, _)| k == key).map(|(_, b)| b.clone()).unwrap_or_else(|| Binding::Choice("none".into()))
    }
}

fn parse(data: &[u8]) -> Result<Parsed> {
    parse_section(data, "touchpad", "XML document types are not supported in gesture settings.",
                  "Expected one labwc_config with at most one touchpad section.")
}

fn attribute(node: &Element, name: &str) -> String {
    node.attributes.iter().rev().find(|(k, _)| casefold(k) == name).map(|(_, v)| casefold(compat::strip(v))).unwrap_or_default()
}

/// The slot key of a `<gesture>` element, when it is one the page shows.
fn slot_of(node: &Element) -> Option<String> {
    let (kind, fingers, direction) = (attribute(node, "type"), attribute(node, "fingers"), attribute(node, "direction"));
    GESTURE_SLOTS.iter().find(|(k, f, d)| *k == kind && f.to_string() == fingers && *d == direction).map(|(k, f, d)| slot_key(k, *f, d))
}

fn elements(node: &Element) -> Vec<&Element> {
    node.children.iter().filter_map(|n| if let Node::Element(e) = n { Some(e) } else { None }).collect()
}

/// An action's arguments: attributes, then child elements (casefolded names; last wins).
fn arguments(action: &Element) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();
    let mut put = |key: String, item: String| {
        if let Some(slot) = result.iter_mut().find(|(k, _)| *k == key) { slot.1 = item } else { result.push((key, item)) }
    };
    for (name, item) in &action.attributes {
        put(casefold(name), item.clone());
    }
    for child in elements(action) {
        put(local_lower(&child.name), compat::text(child));
    }
    result
}

/// Same program (by file name, so `/usr/bin/…`, `~/.local/bin/…` and a bare name
/// all count) with the same arguments.
fn same_command(found: &str, wanted: &str) -> bool {
    let (found, wanted) = (compat::split_whitespace(found), compat::split_whitespace(wanted));
    let name = |word: &str| word.rsplit('/').next().unwrap_or(word).to_owned();
    !found.is_empty() && found.len() == wanted.len() && name(found[0]) == name(wanted[0]) && found[1..] == wanted[1..]
}

/// The choice a `<gesture>` element is, if it is exactly one of them.
fn choice_of(node: &Element) -> Option<&'static str> {
    let children = elements(node);
    let [action] = children.as_slice() else { return None };
    if local_lower(&action.name) != "action" {
        return None;
    }
    let mut found = arguments(action);
    let name = found.iter().position(|(k, _)| k == "name").map(|i| found.remove(i).1)?;
    GESTURE_CHOICES.iter().copied().find(|id| {
        let Some((wanted_name, wanted)) = gesture_action(id) else { return false };
        casefold(compat::strip(&name)) == casefold(wanted_name) && found.len() == wanted.len()
            && wanted.iter().all(|(key, item)| found.iter().any(|(k, v)| k.as_str() == *key && if *key == "command" {
                same_command(v, item)
            } else {
                casefold(compat::strip(v)) == casefold(item)
            }))
    })
}

pub fn read_state(data: &[u8]) -> Result<GestureState> {
    let parsed = parse(data)?;
    let mut state = GestureState::default();
    let Some(index) = parsed.section else { return Ok(state) };
    state.present = true;
    let section = compat::child_element(&parsed.doc.root, index);
    for i in element_indices(section, "gesture") {
        let node = compat::child_element(section, i);
        let Some(key) = slot_of(node) else { continue };
        let binding = match choice_of(node) {
            Some(id) => Binding::Choice(id.to_owned()),
            None => Binding::Custom(node.to_xml()),
        };
        // A later duplicate replaces an earlier one, as with labwc's keybinds.
        if let Some(slot) = state.bindings.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = binding;
        } else {
            state.bindings.push((key, binding));
        }
    }
    Ok(state)
}

/// `(slot key, choice id)` for every changed gesture.
pub type GestureUpdates = Vec<(String, String)>;

fn gesture_element(key: &str, id: &str) -> Result<Element> {
    let Some((kind, fingers, direction)) = GESTURE_SLOTS.iter().find(|(k, f, d)| slot_key(k, *f, d) == key) else {
        return value(format!("Unknown touchpad gesture: {key}"));
    };
    let Some((name, attributes)) = gesture_action(id) else { return value(format!("Unknown gesture action: {id}")) };
    let mut action = compat::new_element("action");
    compat::set_attribute(&mut action, "name", name);
    for (attribute, item) in attributes {
        compat::set_attribute(&mut action, attribute, &item);
    }
    let mut gesture = compat::new_element("gesture");
    compat::set_attribute(&mut gesture, "type", kind);
    compat::set_attribute(&mut gesture, "fingers", &fingers.to_string());
    compat::set_attribute(&mut gesture, "direction", direction);
    gesture.children.push(Node::Element(action));
    gesture.self_closing = false;
    Ok(gesture)
}

fn blank(node: &Node) -> bool { matches!(node, Node::Text(t) if compat::strip(t).is_empty()) }

/// Remove a child with the indentation before it.
fn remove(section: &mut Element, index: usize) {
    section.children.remove(index);
    if index > 0 && blank(&section.children[index - 1]) {
        section.children.remove(index - 1);
    }
}

/// Append a child on its own line before the closing tag.
fn append(section: &mut Element, node: Element) {
    if section.children.iter().all(blank) {
        section.children = vec![Node::Text("\n    ".into()), Node::Element(node), Node::Text("\n  ".into())];
    } else if section.children.last().is_some_and(blank) {
        let at = section.children.len() - 1;
        section.children.insert(at, Node::Element(node));
        section.children.insert(at, Node::Text("\n    ".into()));
    } else {
        section.children.extend([Node::Text("\n    ".into()), Node::Element(node), Node::Text("\n  ".into())]);
    }
    section.self_closing = false;
}

/// rc.xml with `updates` applied. Only the `<touchpad>` section changes; a gesture
/// changed elsewhere since `expected` was read is a conflict.
pub fn with_gestures(data: &[u8], updates: &GestureUpdates, expected: &GestureState) -> Result<Vec<u8>> {
    let current = read_state(data)?;
    for (key, id) in updates {
        if !slot_keys().contains(key) {
            return value(format!("Unknown touchpad gesture: {key}"));
        }
        if id != "none" && gesture_action(id).is_none() {
            return value(format!("Unknown gesture action: {id}"));
        }
        if current.get(key) != expected.get(key) {
            return compat::runtime("Gesture settings changed elsewhere. Reload before applying.");
        }
    }
    let parsed = parse(data)?;
    let mut section = match parsed.section {
        Some(index) => compat::child_element(&parsed.doc.root, index).clone(),
        None => compat::new_element("touchpad"),
    };
    for (key, id) in updates {
        let found: Vec<usize> = element_indices(&section, "gesture").into_iter()
            .filter(|i| slot_of(compat::child_element(&section, *i)).as_deref() == Some(key.as_str())).collect();
        if id == "none" {
            for index in found.iter().rev() {
                remove(&mut section, *index);
            }
            continue;
        }
        let node = gesture_element(key, id)?;
        match found.split_first() {
            Some((first, rest)) => {
                for index in rest.iter().rev() {
                    remove(&mut section, *index);
                }
                section.children[*first] = Node::Element(node);
            }
            None => append(&mut section, node),
        }
    }
    // Keep the (now empty) section: it records that gestures were chosen.
    if section.children.iter().all(blank) {
        section.children.clear();
        section.self_closing = true;
    }
    let replacement = section.to_xml().into_bytes();
    if let Some((begin, end)) = parsed.span {
        return Ok([&data[..begin], &replacement[..], &data[end..]].concat());
    }
    let opening = tag_end(data, parsed.doc.root.start)?;
    if compat::bytes_rstrip(&data[..opening]).ends_with(b"/>") {
        let name = parsed.doc.root.name.as_bytes();
        return Ok([&data[..opening - 2], b">\n  ", &replacement[..], b"\n</", name, b">", &data[opening..]].concat());
    }
    let end = parsed.root_end;
    Ok([&data[..end], b"  ", &replacement[..], b"\n", &data[end..]].concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_core::session_keys::{DOCK, LOCK, SHELL_CLI};

    const CONFIG: &str = r#"<?xml version="1.0"?>
<!-- Keep the rest of this file byte for byte. -->
<labwc_config>
  <libinput><device category="touchpad"><tap>yes</tap></device></libinput>
  <touchpad>
    <!-- my gestures -->
    <gesture type="swipe" fingers="3" direction="left"><action name="GoToDesktop" to="right" wrap="no"/></gesture>
    <gesture type="swipe" fingers="3" direction="up"><action name="Execute" command="/usr/bin/tessel-shell-cli show-activities"/></gesture>
    <gesture type="pinch" fingers="4" direction="in"><action name="Execute" command="my-zoom --in"/></gesture>
    <gesture type="swipe" fingers="5" direction="up"><action name="Close"/></gesture>
  </touchpad>
  <hdrOutputs><output name="DP-2"/></hdrOutputs>
</labwc_config>
"#;

    fn choice(id: &str) -> Binding { Binding::Choice(id.into()) }

    fn edits(items: &[(&str, &str)]) -> GestureUpdates { items.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }

    fn apply(data: &str, items: &[(&str, &str)]) -> String {
        let data = data.as_bytes();
        String::from_utf8(with_gestures(data, &edits(items), &read_state(data).unwrap()).unwrap()).unwrap()
    }

    fn split<'a>(text: &'a str, marker: &str, after: bool) -> &'a str {
        let at = text.find(marker).unwrap();
        if after { &text[at + marker.len()..] } else { &text[..at] }
    }

    #[test]
    fn choices_custom_and_unset_gestures_are_read() {
        let state = read_state(CONFIG.as_bytes()).unwrap();
        assert!(state.present);
        assert_eq!(state.get("gesture-swipe-3-left"), choice("next-workspace"));
        assert_eq!(state.get("gesture-swipe-3-up"), choice("overview"));
        assert_eq!(state.get("gesture-swipe-3-down"), choice("none"));
        let Binding::Custom(xml) = state.get("gesture-pinch-4-in") else { panic!("custom") };
        assert!(xml.contains("my-zoom --in"));
        assert_eq!(state.bindings.len(), 3, "five fingers is not a row of the page");
        let missing = read_state(b"<labwc_config/>").unwrap();
        assert!(!missing.present && missing.bindings.is_empty());
        assert!(read_state(b"<labwc_config><touchpad/></labwc_config>").unwrap().present);
    }

    #[test]
    fn every_choice_round_trips_with_either_install_path() {
        for (index, (id, _)) in CHOICES.iter().enumerate().skip(1) {
            let key = &slot_keys()[index % 10];
            let text = apply("<labwc_config/>", &[(key, id)]);
            assert_eq!(read_state(text.as_bytes()).unwrap().get(key), choice(id), "{text}");
            for (packaged, local) in [("/usr/bin/", "~/.local/bin/"), ("~/.local/bin/", "/usr/bin/")] {
                let other = text.replace(packaged, local);
                assert_eq!(read_state(other.as_bytes()).unwrap().get(key), choice(id), "{other}");
            }
        }
        let text = apply("<labwc_config/>", &[("gesture-swipe-4-up", "overview"), ("gesture-swipe-4-down", "lock"),
                                              ("gesture-pinch-4-out", "app-menu")]);
        assert!(text.contains(&format!("<gesture type=\"swipe\" fingers=\"4\" direction=\"up\"><action name=\"Execute\" command=\"{SHELL_CLI} show-activities\"/></gesture>")), "{text}");
        assert!(text.contains(&format!("command=\"{LOCK}\"")) && text.contains(&format!("command=\"{DOCK} --menu-hotkey\"")));
        assert_eq!(text, format!("<labwc_config>\n  <touchpad>\n    {}\n    {}\n    {}\n  </touchpad>\n</labwc_config>",
            tessel_core::session_keys::gesture_xml("swipe", 4, "up", "overview").unwrap(),
            tessel_core::session_keys::gesture_xml("swipe", 4, "down", "lock").unwrap(),
            tessel_core::session_keys::gesture_xml("pinch", 4, "out", "app-menu").unwrap()));
    }

    #[test]
    fn near_misses_are_custom_and_case_or_child_forms_are_choices() {
        for gesture in [r#"<action name="GoToDesktop" to="right"/>"#, r#"<action name="Execute" command="tessel-shell-cli show-activities --monitor DP-1"/>"#,
                        r#"<action name="Close"/><action name="Close"/>"#, r#"<action name="ToggleMaximize" direction="vertical"/>"#,
                        r#"<action name="Execute" command="other-lock"/>"#, ""] {
            let text = format!(r#"<labwc_config><touchpad><gesture type="swipe" fingers="3" direction="down">{gesture}</gesture></touchpad></labwc_config>"#);
            assert!(matches!(read_state(text.as_bytes()).unwrap().get("gesture-swipe-3-down"), Binding::Custom(_)), "{gesture}");
        }
        let text = r#"<labwc_config><touchpad><gesture Type="Swipe" fingers=" 3 " direction="DOWN"><action name="execute"><command>tessel-shell-lock</command></action></gesture><gesture type="pinch" fingers="4" direction="out"><action name="gotodesktop" to="Left" wrap="No"/></gesture></touchpad></labwc_config>"#;
        let state = read_state(text.as_bytes()).unwrap();
        assert_eq!(state.get("gesture-swipe-3-down"), choice("lock"));
        assert_eq!(state.get("gesture-pinch-4-out"), choice("previous-workspace"));
    }

    #[test]
    fn only_the_touchpad_section_changes_and_custom_gestures_survive() {
        let text = apply(CONFIG, &[("gesture-swipe-3-left", "previous-workspace"), ("gesture-swipe-3-down", "show-desktop"),
                                   ("gesture-swipe-3-up", "none")]);
        assert_eq!(split(&text, "<touchpad>", false), split(CONFIG, "<touchpad>", false));
        assert_eq!(split(&text, "</touchpad>", true), split(CONFIG, "</touchpad>", true));
        assert!(text.contains("<!-- my gestures -->") && text.contains("my-zoom --in") && text.contains("fingers=\"5\""), "{text}");
        let state = read_state(text.as_bytes()).unwrap();
        assert_eq!(state.get("gesture-swipe-3-left"), choice("previous-workspace"));
        assert_eq!(state.get("gesture-swipe-3-down"), choice("show-desktop"));
        assert_eq!(state.get("gesture-swipe-3-up"), choice("none"));
        assert!(!text.contains("show-activities"));
        // The changed gesture keeps its place; the new one goes last, each on its own line.
        assert!(text.contains("<!-- my gestures -->\n    <gesture type=\"swipe\" fingers=\"3\" direction=\"left\"><action name=\"GoToDesktop\" to=\"left\" wrap=\"no\"/></gesture>\n    <gesture type=\"pinch\""), "{text}");
        assert!(text.contains("<action name=\"ToggleShowDesktop\"/></gesture>\n  </touchpad>"), "{text}");
        // A custom gesture is replaced only when the user picks something else.
        let replaced = apply(CONFIG, &[("gesture-pinch-4-in", "fullscreen")]);
        assert!(!replaced.contains("my-zoom") && replaced.contains("ToggleFullscreen"));
        assert_eq!(apply(CONFIG, &[]), CONFIG);
    }

    #[test]
    fn removing_the_last_gesture_leaves_an_empty_section() {
        let one = apply("<labwc_config>\n  <touchpad>\n    <gesture type=\"swipe\" fingers=\"3\" direction=\"left\"><action name=\"Close\"/></gesture>\n  </touchpad>\n</labwc_config>\n",
                        &[("gesture-swipe-3-left", "none")]);
        assert_eq!(one, "<labwc_config>\n  <touchpad/>\n</labwc_config>\n");
        assert!(read_state(one.as_bytes()).unwrap().present);
        // Duplicates of one gesture all go; setting one keeps a single element.
        let twice = "<labwc_config><touchpad><gesture type=\"swipe\" fingers=\"3\" direction=\"up\"><action name=\"Close\"/></gesture><gesture type=\"swipe\" fingers=\"3\" direction=\"up\"><action name=\"Iconify\"/></gesture></touchpad></labwc_config>";
        assert_eq!(read_state(twice.as_bytes()).unwrap().get("gesture-swipe-3-up"), choice("minimize"), "the last one counts");
        assert_eq!(apply(twice, &[("gesture-swipe-3-up", "none")]), "<labwc_config><touchpad/></labwc_config>");
        let set = apply(twice, &[("gesture-swipe-3-up", "maximize")]);
        assert_eq!(set.matches("<gesture").count(), 1, "{set}");
        // Choosing nothing where there was no section still records the choice.
        assert_eq!(apply("<labwc_config>\n</labwc_config>\n", &[("gesture-swipe-3-up", "none")]), "<labwc_config>\n  <touchpad/>\n</labwc_config>\n");
        // A comment in an otherwise empty section stays.
        let commented = apply("<labwc_config><touchpad><!-- c --><gesture type=\"pinch\" fingers=\"4\" direction=\"in\"><action name=\"Close\"/></gesture></touchpad></labwc_config>",
                              &[("gesture-pinch-4-in", "none")]);
        assert_eq!(commented, "<labwc_config><touchpad><!-- c --></touchpad></labwc_config>");
    }

    #[test]
    fn empty_namespaced_and_missing_sections() {
        for data in ["<labwc_config/>", "<!-- <labwc_config is a comment --><labwc_config/>",
                     "<labwc_config xmlns=\"http://openbox.org/3.4/rc\"><mouse/></labwc_config>", "<labwc_config><touchpad/></labwc_config>"] {
            let text = apply(data, &[("gesture-swipe-4-left", "next-workspace")]);
            assert_eq!(read_state(text.as_bytes()).unwrap().get("gesture-swipe-4-left"), choice("next-workspace"), "{text}");
        }
    }

    #[test]
    fn malformed_input_and_unknown_values_are_rejected() {
        for data in [&b"<!DOCTYPE labwc_config><labwc_config/>"[..], b"<oops/>", b"<labwc_config><touchpad/><touchpad/></labwc_config>"] {
            assert!(read_state(data).unwrap_err().is_value(), "{}", String::from_utf8_lossy(data));
            assert!(with_gestures(data, &edits(&[("gesture-swipe-3-up", "close")]), &GestureState::default()).unwrap_err().is_value());
        }
        assert!(read_state(b"<labwc_config><touchpad>").is_err());
        let state = read_state(CONFIG.as_bytes()).unwrap();
        for (key, id) in [("gesture-swipe-5-up", "close"), ("gesture-swipe-3-up", "reboot"), ("gesture-swipe-3-up", "")] {
            assert!(with_gestures(CONFIG.as_bytes(), &edits(&[(key, id)]), &state).unwrap_err().is_value(), "{key}={id}");
        }
    }

    #[test]
    fn conflicts_only_with_changed_gestures() {
        let state = read_state(CONFIG.as_bytes()).unwrap();
        let other = CONFIG.replace("my-zoom --in", "my-zoom --other");
        with_gestures(other.as_bytes(), &edits(&[("gesture-swipe-3-left", "close")]), &state).unwrap();
        let error = with_gestures(other.as_bytes(), &edits(&[("gesture-pinch-4-in", "close")]), &state).unwrap_err();
        assert!(error.is_runtime() && error.message().contains("changed elsewhere"));
    }
}
