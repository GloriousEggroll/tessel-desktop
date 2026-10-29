//! shortcuts_model.py: every keyboard shortcut the session knows — labwc
//! defaults, rc.xml and tiling.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gtk::gdk;
use gtk::gdk::prelude::DisplayExtManual;

use crate::compat::{casefold, Result};
use crate::labwc_settings::{self, Action, Done, Reconfigure};

pub const DEFAULTS_FILE: &str = "/usr/share/doc/labwc/rc.xml.all";
pub const GROUPS: &[&str] = &["Windows", "Workspaces", "Tiling", "Launchers", "Media", "System"];
pub const MODIFIERS: &[(&str, &str)] = &[("W", "Super"), ("A", "Alt"), ("C", "Ctrl"), ("S", "Shift"), ("H", "Hyper"), ("M", "Meta")];
pub const MODIFIER_ORDER: &str = "WCAS HM";
/// labwc 0.20 built-in defaults (rc.xml.all), used when the documentation is not installed.
pub const FALLBACK_DEFAULTS: &[(&str, &str, &str)] = &[
    ("A-Tab", "NextWindow", ""), ("A-S-Tab", "PreviousWindow", ""),
    ("W-Return", "Execute", "lab-sensible-terminal"), ("A-F4", "Close", ""),
    ("W-a", "ToggleMaximize", ""), ("W-d", "ToggleShowDesktop", ""),
    ("W-Left", "SnapToEdge", "left"), ("W-Right", "SnapToEdge", "right"),
    ("W-Up", "SnapToEdge", "up"), ("W-Down", "SnapToEdge", "down"),
    ("A-Space", "ShowMenu", "client-menu"),
    ("XF86AudioLowerVolume", "Execute", "pactl set-sink-volume @DEFAULT_SINK@ -5%"),
    ("XF86AudioRaiseVolume", "Execute", "pactl set-sink-volume @DEFAULT_SINK@ +5%"),
    ("XF86AudioMute", "Execute", "pactl set-sink-mute @DEFAULT_SINK@ toggle"),
    ("XF86MonBrightnessUp", "Execute", "brightnessctl set +10%"),
    ("XF86MonBrightnessDown", "Execute", "brightnessctl set 10%-"),
];
pub const WINDOW_ACTIONS: &[(&str, &str)] = &[
    ("NextWindow", "Switch to the next window"), ("PreviousWindow", "Switch to the previous window"),
    ("Close", "Close window"), ("Kill", "Force a window to close"), ("ToggleMaximize", "Maximize or restore window"),
    ("Maximize", "Maximize window"), ("UnMaximize", "Restore window size"), ("Iconify", "Minimize window"),
    ("ToggleFullscreen", "Toggle fullscreen"), ("ToggleShowDesktop", "Show the desktop"),
    ("ToggleAlwaysOnTop", "Keep window on top"), ("Move", "Move window"), ("Resize", "Resize window"),
    ("Focus", "Focus window"), ("Raise", "Raise window"), ("ToggleDecorations", "Toggle window title bar"),
    ("SnapToRegion", "Snap window to a region"), ("MoveToEdge", "Move window to a screen edge"),
    ("GrowToEdge", "Grow window to a screen edge"), ("ShrinkToEdge", "Shrink window from a screen edge"),
    ("ToggleSnapToEdge", "Snap window to a screen edge"),
];
pub const SNAP: &[(&str, &str)] = &[("left", "Snap window to the left"), ("right", "Snap window to the right"),
                                     ("up", "Snap window to the top"), ("down", "Snap window to the bottom")];
pub const SYSTEM_ACTIONS: &[(&str, &str)] = &[
    ("Exit", "Sign out"), ("Reconfigure", "Reload the desktop configuration"), ("None", "Disabled"),
    ("Debug", "Print debugging information"), ("ToggleKeybinds", "Pause or resume shortcuts"),
    ("EnableTabletMouseEmulation", "Tablet mouse emulation"),
];
/// Suite helpers that rc.xml runs, matched on the command text.
pub const SUITE_ACTIONS: &[(&str, &str, &str)] = &[
    ("session-menu", "System", "Open the session menu"), ("terminal", "Launchers", "Open a terminal"),
    ("shortcuts", "System", "Show keyboard shortcuts"), ("power-button", "System", "Power button"),
    ("lock", "System", "Lock screen"),
    ("play-pause", "Media", "Play or pause media"), ("pause", "Media", "Pause media"),
    ("next", "Media", "Next track"), ("previous", "Media", "Previous track"), ("stop", "Media", "Stop media"),
    ("volume-up", "Media", "Raise volume"), ("volume-down", "Media", "Lower volume"),
    ("toggle-mute", "Media", "Mute or unmute audio"),
    ("brightness-up", "Media", "Increase brightness"), ("brightness-down", "Media", "Decrease brightness"),
];
pub const COMMANDS: &[(&str, &str, &str)] = &[
    ("switch-window --reverse", "Windows", "Switch to the previous window"),
    ("switch-window", "Windows", "Switch windows"),
    ("--volume-up", "Media", "Raise volume"), ("--volume-down", "Media", "Lower volume"),
    ("--toggle-mute", "Media", "Mute or unmute audio"),
    ("set-sink-volume @DEFAULT_SINK@ +", "Media", "Raise volume"),
    ("set-sink-volume @DEFAULT_SINK@ -", "Media", "Lower volume"),
    ("set-sink-mute", "Media", "Mute or unmute audio"),
    ("--brightness-up", "Media", "Increase brightness"), ("--brightness-down", "Media", "Decrease brightness"),
    ("brightnessctl set +", "Media", "Increase brightness"), ("brightnessctl set ", "Media", "Decrease brightness"),
    ("playerctl play-pause", "Media", "Play or pause media"), ("playerctl next", "Media", "Next track"),
    ("playerctl previous", "Media", "Previous track"),
    ("--menu-hotkey", "Launchers", "Open the application menu"),
    ("wayshell-lock", "System", "Lock screen"),
    ("--toggle-recording", "System", "Start or stop screen recording"),
    ("--capture screen", "System", "Take a screenshot"), ("--capture area", "System", "Capture an area"),
    ("--capture window", "System", "Capture a window"), ("--capture workspace", "System", "Capture the workspace"),
    ("--shortcuts", "System", "Show keyboard shortcuts"),
    ("lab-sensible-terminal", "Launchers", "Open a terminal"),
    ("--settings", "Tiling", "Tiling settings"),
];
pub const WORKSPACE_TILING: &[&str] = &["workspace", "send", "send-silent", "workspace-monitor"];

/// `(key, label, description, command)` of tiling-manager's BINDINGS.
pub type TilingBinding = (String, String, String, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub key: String,
    /// English; translate when shown.
    pub description: String,
    /// Format values for description, e.g. `[("value1", "3")]`.
    pub values: Vec<(String, String)>,
    pub group: String,
    /// "default", "user" or "tiling".
    pub source: String,
    pub active: bool,
    /// Byte offset of a user keybind in rc.xml (-1 otherwise).
    pub start: i64,
    /// Description of a default this user binding replaces.
    pub overrides: String,
}

impl Shortcut {
    pub fn editable(&self) -> bool { self.source == "user" && self.start >= 0 }

    /// `description.format(**values)` (English).
    pub fn formatted(&self) -> String {
        let values: Vec<(&str, suite_core::pyfmt::Arg)> = self.values.iter()
            .map(|(k, v)| (k.as_str(), suite_core::pyfmt::Arg::from(v.as_str()))).collect();
        suite_core::pyfmt::format(&self.description, &values).unwrap_or_else(|_| self.description.clone())
    }

    /// `tr(description, **values)`.
    pub fn translated(&self) -> String {
        let values: Vec<(&str, suite_core::pyfmt::Arg)> = self.values.iter()
            .map(|(k, v)| (k.as_str(), suite_core::pyfmt::Arg::from(v.as_str()))).collect();
        suite_core::i18n::translate_with(&self.description, &values)
    }
}

fn modifier(part: &str) -> Option<&'static str> {
    let upper = part.to_uppercase();
    MODIFIERS.iter().find(|(k, _)| *k == upper).map(|(_, v)| *v)
}

/// Canonical comparable form: modifiers in a fixed order, keysym casefolded.
pub fn normalize(key: &str) -> String {
    let mut parts: Vec<String> = crate::compat::strip(key).split('-').map(str::to_owned).collect();
    if parts.len() > 1 && parts.last().map(String::is_empty).unwrap_or(false) {
        parts.truncate(parts.len().saturating_sub(2));
        parts.push("-".into()); // a literal minus key
    }
    let (symbol, modifiers) = match parts.split_last() { Some((s, m)) => (s.clone(), m.to_vec()), None => (String::new(), vec![]) };
    let mut found: Vec<char> = modifiers.iter().map(|p| p.to_uppercase())
        .filter(|p| MODIFIERS.iter().any(|(k, _)| k == p))
        .filter_map(|p| p.chars().next()).collect();
    found.sort_by_key(|c| MODIFIER_ORDER.find(*c).unwrap_or(99));
    found.dedup();
    let mut out: Vec<String> = found.into_iter().map(String::from).collect();
    out.push(casefold(&symbol));
    out.join("-")
}

const PRETTY: &[(&str, &str)] = &[
    ("Super_L", "Super"), ("Super_R", "Super"), ("Return", "Enter"), ("grave", "`"), ("minus", "−"),
    ("equal", "="), ("slash", "/"), ("Print", "Print Screen"), ("space", "Space"), ("Space", "Space"),
    ("XF86AudioRaiseVolume", "Volume Up"), ("XF86AudioLowerVolume", "Volume Down"),
    ("XF86AudioMute", "Mute"), ("XF86MonBrightnessUp", "Brightness Up"),
    ("XF86MonBrightnessDown", "Brightness Down"), ("XF86AudioPlay", "Play"), ("XF86AudioNext", "Next Track"),
    ("XF86AudioPrev", "Previous Track"), ("XF86AudioPause", "Pause"), ("XF86AudioStop", "Stop"),
    ("XF86PowerOff", "Power"),
];

/// `display_key(key)` (identity translation, the Python default).
pub fn display_key(key: &str) -> String { display_key_with(key, &|v| v.to_owned()) }

/// `display_key(key, translate)`.
pub fn display_key_with(key: &str, translate: &dyn Fn(&str) -> String) -> String {
    let parts: Vec<&str> = key.split('-').collect();
    let mut names: Vec<String> = parts[..parts.len() - 1].iter().map(|p| translate(modifier(p).unwrap_or(p))).collect();
    let symbol = parts.last().copied().unwrap_or(key);
    let pretty = PRETTY.iter().find(|(k, _)| *k == symbol).map(|(_, v)| *v);
    let translated = pretty.is_some_and(|p| p.chars().count() > 1);
    let mut symbol = pretty.unwrap_or(symbol).to_owned();
    if symbol.chars().count() == 1 {
        symbol = symbol.to_uppercase();
    }
    names.push(if translated { translate(&symbol) } else { symbol });
    names.join(" + ")
}

/// tiling-manager's BINDINGS as `(key, label, description, command)`.
pub fn tiling_bindings() -> Vec<TilingBinding> {
    tiling_manager::bindings::BINDINGS.iter()
        .map(|b| (b.key.clone(), b.label.clone(), b.description.clone(), b.command.clone())).collect()
}

fn group_for(command: &str) -> &'static str {
    if WORKSPACE_TILING.contains(&command.split(' ').next().unwrap_or("")) { "Workspaces" } else { "Tiling" }
}

fn file_name(path: &str) -> String { Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }

/// `(group, English description, values)` for a labwc action.
pub fn describe(action: &str, attributes: &[(String, String)], tiling: &[TilingBinding]) -> (String, String, Vec<(String, String)>) {
    let mut get: HashMap<String, String> = HashMap::new();
    for (name, value) in attributes {
        get.insert(casefold(name), value.clone());
    }
    let get = |k: &str| get.get(k).cloned().unwrap_or_default();
    let plain = |g: &str, d: &str| (g.to_owned(), d.to_owned(), Vec::new());
    let lookup = |table: &[(&str, &str)], k: &str| table.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string());
    match action {
        "Tiling" => {
            let command = get("command");
            for (_key, _label, description, tiling_command) in tiling {
                if *tiling_command == command {
                    return (group_for(&command).into(), description.clone(), Vec::new());
                }
            }
            ("Tiling".into(), "Tiling: {value1}".into(), vec![("value1".into(), command)])
        }
        "Execute" => {
            let command = get("command");
            let words = crate::compat::split_whitespace(&command);
            if words.len() >= 2 && file_name(words[0]) == "desktop-suite-action" {
                for (name, group, description) in SUITE_ACTIONS {
                    if words[1] == *name {
                        return plain(group, description);
                    }
                }
            }
            for (needle, group, description) in COMMANDS {
                if command.contains(needle) {
                    return plain(group, description);
                }
            }
            let first = file_name(command.split(' ').next().unwrap_or(""));
            ("Launchers".into(), "Run {value1}".into(), vec![("value1".into(), if first.is_empty() { command } else { first })])
        }
        "SnapToEdge" => plain("Windows", &lookup(SNAP, &get("direction")).unwrap_or_else(|| "Snap window to a screen edge".into())),
        "ShowMenu" => {
            let menu = get("menu");
            if menu == "client-menu" {
                return plain("Windows", "Show the window menu");
            }
            ("Launchers".into(), "Show the {value1} menu".into(), vec![("value1".into(), menu)])
        }
        "GoToDesktop" | "SendToDesktop" => {
            let target = get("to");
            let relative = match target.to_lowercase().as_str() { "left" => Some("previous"), "right" => Some("next"), _ => None };
            if let Some(relative) = relative {
                let text = match (action, relative) {
                    ("GoToDesktop", "previous") => "Switch to the previous workspace",
                    ("GoToDesktop", _) => "Switch to the next workspace",
                    (_, "previous") => "Move window to the previous workspace",
                    _ => "Move window to the next workspace",
                };
                return plain("Workspaces", text);
            }
            let text = if action == "GoToDesktop" { "Switch to workspace {value1}" } else { "Move window to workspace {value1}" };
            ("Workspaces".into(), text.into(), vec![("value1".into(), target)])
        }
        _ => {
            if let Some(text) = lookup(WINDOW_ACTIONS, action) {
                return plain("Windows", &text);
            }
            if let Some(text) = lookup(SYSTEM_ACTIONS, action) {
                return plain("System", &text);
            }
            plain("System", if action.is_empty() { "Disabled" } else { action })
        }
    }
}

pub type Keybinds = Vec<(String, Vec<Action>)>;

pub fn default_keybinds_from(path: &Path) -> Keybinds {
    if let Ok(data) = std::fs::read(path) {
        if let Ok(section) = labwc_settings::read_keyboard(&data) {
            let binds: Keybinds = section.keybinds.into_iter().map(|b| (b.key, b.actions)).collect();
            if !binds.is_empty() {
                return binds;
            }
        }
    }
    FALLBACK_DEFAULTS.iter().map(|(key, action, value)| {
        let name = match *action { "Execute" => Some("command"), "SnapToEdge" => Some("direction"), "ShowMenu" => Some("menu"), _ => None };
        let attributes = name.map(|n| vec![(n.to_owned(), value.to_string())]).unwrap_or_default();
        (key.to_string(), vec![Action { name: action.to_string(), attributes }])
    }).collect()
}

pub fn default_keybinds() -> Keybinds { default_keybinds_from(Path::new(DEFAULTS_FILE)) }

fn natural(text: &str) -> Vec<(u8, u128, String)> {
    // re.split(r'(\d+)'): digits compare as ints (Python would compare int with
    // str only if the shapes differ, which the split keeps aligned).
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut digits = false;
    let flush = |current: &mut String, digits: bool, parts: &mut Vec<(u8, u128, String)>| {
        if digits {
            parts.push((1, current.parse().unwrap_or(u128::MAX), String::new()));
        } else {
            parts.push((0, 0, casefold(current)));
        }
        current.clear();
    };
    for c in text.chars() {
        let is_digit = c.is_ascii_digit();
        if is_digit != digits && (!current.is_empty() || is_digit) {
            flush(&mut current, digits, &mut parts);
        }
        digits = is_digit;
        current.push(c);
    }
    flush(&mut current, digits, &mut parts);
    // Python's split alternates text/number/text…; compare position-wise.
    parts.into_iter().map(|(kind, number, text)| if kind == 1 { (0, number, String::new()) } else { (0, 0, text) }).collect()
}

/// Effective shortcuts, ordered by group then description.
pub fn collect(rc_data: &[u8], tiling: &[TilingBinding], defaults: Option<&Keybinds>) -> Result<Vec<Shortcut>> {
    let section = labwc_settings::read_keyboard(rc_data)?;
    let loaded;
    let default_binds = match defaults {
        Some(d) => d,
        None => {
            loaded = default_keybinds();
            &loaded
        }
    };
    let load_defaults = section.has_default || section.keybinds.is_empty();
    let mut result: Vec<(String, Shortcut)> = Vec::new();
    let add = |result: &mut Vec<(String, Shortcut)>, key: &str, actions: &[Action], source: &str, start: i64, overrides: String| {
        let normal = normalize(key);
        let Some(first) = actions.first() else {
            result.retain(|(k, _)| *k != normal); // an empty keybind removes an earlier one
            return;
        };
        let (group, description, values) = describe(&first.name, &first.attributes, tiling);
        let item = Shortcut { key: key.into(), description, values, group, source: source.into(), active: first.name != "None", start, overrides };
        if let Some(slot) = result.iter_mut().find(|(k, _)| *k == normal) { slot.1 = item } else { result.push((normal, item)) }
    };
    if load_defaults {
        for (key, actions) in default_binds {
            add(&mut result, key, actions, "default", -1, String::new());
        }
    }
    for bind in &section.keybinds {
        let normal = normalize(&bind.key);
        let overrides = result.iter().find(|(k, _)| *k == normal).filter(|(_, p)| p.source == "default")
            .map(|(_, p)| p.description.clone()).unwrap_or_default();
        add(&mut result, &bind.key, &bind.actions, if bind.tiling { "tiling" } else { "user" }, bind.start as i64, overrides);
    }
    if !section.keybinds.iter().any(|b| b.tiling) {
        // Tiling is off: list its shortcuts as available when it is turned on.
        for (key, _label, description, command) in tiling {
            let normal = normalize(key);
            if !result.iter().any(|(k, _)| *k == normal) {
                result.push((normal, Shortcut { key: key.clone(), description: description.clone(), values: Vec::new(),
                    group: group_for(command).into(), source: "tiling".into(), active: false, start: -1, overrides: String::new() }));
            }
        }
    }
    let order = |group: &str| GROUPS.iter().position(|g| *g == group).unwrap_or(99);
    let mut items: Vec<Shortcut> = result.into_iter().map(|(_, s)| s).collect();
    items.sort_by(|a, b| (order(&a.group), natural(&a.formatted()), natural(&a.key))
        .cmp(&(order(&b.group), natural(&b.formatted()), natural(&b.key))));
    Ok(items)
}

/// Shortcuts already using `key`, other than `ignore`.
pub fn conflicts<'a>(shortcuts: &'a [Shortcut], key: &str, ignore: Option<&Shortcut>) -> Vec<&'a Shortcut> {
    let wanted = normalize(key);
    shortcuts.iter().filter(|item| {
        normalize(&item.key) == wanted
            && !ignore.is_some_and(|i| std::ptr::eq(*item, i) || (item.start == i.start && item.source == i.source))
    }).collect()
}

/// labwc key string for a GTK key press, or "" for a lone modifier.
pub fn key_from_event(keyval: gdk::Key, state: gdk::ModifierType, display: Option<&gdk::Display>, keycode: Option<u32>) -> String {
    let mut name = keyval.name().map(|n| n.to_string()).unwrap_or_default();
    if ["Shift_", "Control_", "Alt_", "Meta_", "Super_", "Hyper_", "ISO_Level3", "Caps_Lock"].iter().any(|p| name.starts_with(p)) {
        return String::new();
    }
    if let (Some(display), Some(keycode)) = (display, keycode) {
        if let Some((base, ..)) = display.translate_key(keycode, gdk::ModifierType::empty(), 0) {
            if let Some(base_name) = base.name() {
                name = base_name.to_string();
            }
        }
    }
    key_string(&name, state)
}

/// The modifier prefix + (lowercased single-letter) key name.
pub fn key_string(name: &str, state: gdk::ModifierType) -> String {
    let name = if name.chars().count() == 1 { name.to_lowercase() } else { name.to_owned() };
    let mut prefix = String::new();
    for (mask, letter) in [(gdk::ModifierType::SUPER_MASK, "W"), (gdk::ModifierType::CONTROL_MASK, "C"),
                           (gdk::ModifierType::ALT_MASK, "A"), (gdk::ModifierType::SHIFT_MASK, "S")] {
        if state.contains(mask) {
            prefix.push_str(letter);
            prefix.push('-');
        }
    }
    prefix + &name
}

/// `ShortcutStore(path, reload)` and what the page needs of it.
pub trait Shortcuts {
    fn load(&self) -> Result<Vec<Shortcut>>;
    fn rebind(&self, shortcut: &Shortcut, new_key: &str, done: Option<Done>) -> Result<()>;
}

pub struct ShortcutStore {
    pub path: PathBuf,
    pub reload: Reconfigure,
    /// `_tiling_bindings()` (patched in tests).
    pub tiling: Option<Vec<TilingBinding>>,
    pub defaults: Option<Keybinds>,
}

impl ShortcutStore {
    pub fn new(path: Option<&Path>, reload: Option<Reconfigure>) -> ShortcutStore {
        ShortcutStore { path: path.map(Path::to_path_buf).unwrap_or_else(labwc_settings::rc_path),
                        reload: reload.unwrap_or_else(labwc_settings::default_reconfigure), tiling: None, defaults: None }
    }
}

impl Shortcuts for ShortcutStore {
    fn load(&self) -> Result<Vec<Shortcut>> {
        let tiling = self.tiling.clone().unwrap_or_else(tiling_bindings);
        collect(&labwc_settings::read_bytes(&self.path)?, &tiling, self.defaults.as_ref())
    }

    fn rebind(&self, shortcut: &Shortcut, new_key: &str, done: Option<Done>) -> Result<()> {
        if !shortcut.editable() {
            return crate::compat::value("Only your own shortcuts can be changed here.");
        }
        let changed = labwc_settings::update_rc(|data| labwc_settings::rebind(data, shortcut.start as usize, &shortcut.key, new_key),
                                                Some(&self.path))?;
        if changed {
            (self.reload)(done);
        } else if let Some(done) = done {
            done(true, String::new());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::labwc_settings::tests::RC;
    use crate::testutil::{scratch, EnvGuard};
    use std::cell::RefCell;
    use std::rc::Rc;

    pub fn tiling() -> Vec<TilingBinding> {
        vec![("W-t".into(), "Super + T".into(), "Toggle this window between tiled and floating".into(), "float".into()),
             ("W-1".into(), "Super + 1".into(), "Switch to workspace 1".into(), "workspace 1".into())]
    }

    pub fn defaults() -> Keybinds {
        vec![("A-F4".into(), vec![Action::new("Close", &[])]), ("W-d".into(), vec![Action::new("ToggleShowDesktop", &[])]),
             ("W-a".into(), vec![Action::new("ToggleMaximize", &[])])]
    }

    fn run(data: &[u8]) -> HashMap<String, Shortcut> {
        collect(data, &tiling(), Some(&defaults())).unwrap().into_iter().map(|s| (s.key.clone(), s)).collect()
    }

    #[test]
    fn defaults_user_overrides_removals_and_tiling_are_merged() {
        let items = run(RC);
        assert!(!items.contains_key("A-F4"));
        assert_eq!(items["W-a"].source, "default");
        assert_eq!((items["W-d"].source.as_str(), items["W-d"].overrides.as_str()), ("user", "Show the desktop"));
        assert_eq!((items["W-l"].group.as_str(), items["W-l"].description.as_str(), items["W-l"].editable()), ("System", "Lock screen", true));
        assert_eq!((items["W-t"].source.as_str(), items["W-t"].group.as_str(), items["W-t"].editable()), ("tiling", "Tiling", false));
        assert!(!items.contains_key("W-1"));
        let groups: Vec<usize> = collect(RC, &tiling(), Some(&defaults())).unwrap().iter()
            .map(|i| GROUPS.iter().position(|g| *g == i.group).unwrap()).collect();
        let mut sorted = groups.clone();
        sorted.sort();
        assert_eq!(groups, sorted);
    }

    #[test]
    fn inactive_tiling_shortcuts_are_listed_without_defaults_when_disabled() {
        let data = String::from_utf8_lossy(RC).replace("<default/>", "").replace("<!-- Waytile keyboard begin -->", "")
            .replace("<!-- Waytile keyboard end -->", "");
        let items = run(data.as_bytes());
        assert!(!items.contains_key("W-a"));
        assert_eq!(items["W-1"].group, "Workspaces");
        assert!(!items["W-1"].active);
    }

    #[test]
    fn suite_keybinding_block_is_described_and_editable() {
        let block = br#"<labwc_config><keyboard><default/><!-- Desktop suite keybindings begin: x --><keybind key="W-slash"><action name="Execute" command="~/.local/bin/desktop-suite-action shortcuts"/></keybind><keybind key="C-A-Right"><action name="GoToDesktop" to="right"/></keybind><keybind key="W-S-2"><action name="SendToDesktop" to="2"/></keybind><!-- Desktop suite keybindings end --></keyboard></labwc_config>"#;
        let items = run(block);
        assert_eq!((items["W-slash"].description.as_str(), items["W-slash"].group.as_str(), items["W-slash"].editable()),
                   ("Show keyboard shortcuts", "System", true));
        assert_eq!(items["C-A-Right"].description, "Switch to the next workspace");
        assert_eq!(items["W-S-2"].description, "Move window to workspace {value1}");
        assert_eq!(items["W-S-2"].values, vec![("value1".to_string(), "2".to_string())]);
    }

    #[test]
    fn conflicts_and_normalization() {
        let items = collect(RC, &tiling(), Some(&defaults())).unwrap();
        assert_eq!(normalize("S-W-D"), normalize("W-S-d"));
        let lock = items.iter().find(|i| i.key == "W-l").unwrap();
        assert_eq!(conflicts(&items, "w-D", Some(lock)).iter().map(|i| i.key.as_str()).collect::<Vec<_>>(), ["W-d"]);
        assert!(conflicts(&items, "W-l", Some(lock)).is_empty());
        assert_eq!(display_key("W-S-Return"), "Super + Shift + Enter");
        assert_eq!(display_key("XF86AudioMute"), "Mute");
        assert_eq!(display_key("W-grave"), "Super + `");
        assert_eq!(display_key("W-x"), "Super + X");
        assert_eq!(display_key_with("W-x", &|s| format!("<{s}>")), "<Super> + X");
        assert_eq!(normalize("C--"), "C--");
        assert_eq!(key_string("E", gdk::ModifierType::SUPER_MASK | gdk::ModifierType::SHIFT_MASK), "W-S-e");
    }

    #[test]
    fn natural_sort_orders_numbers_numerically() {
        let mut items = vec!["Switch to workspace 10", "Switch to workspace 2", "Switch to workspace 1"];
        items.sort_by_key(|s| natural(s));
        assert_eq!(items, ["Switch to workspace 1", "Switch to workspace 2", "Switch to workspace 10"]);
    }

    #[test]
    fn fallback_defaults_when_documentation_missing() {
        let binds = default_keybinds_from(Path::new("/nonexistent/rc.xml.all"));
        assert_eq!(binds.len(), FALLBACK_DEFAULTS.len());
        assert_eq!(binds[2].1[0].get("command", ""), "lab-sensible-terminal");
        assert!(!tiling_bindings().is_empty());
    }

    #[test]
    fn store_rebinds_through_rc_and_reloads() {
        let folder = scratch("shortcut-store");
        let _env = EnvGuard::set(&[("XDG_STATE_HOME", folder.to_str().unwrap())]);
        let path = folder.join("rc.xml");
        std::fs::write(&path, RC).unwrap();
        let reloads = Rc::new(RefCell::new(0));
        let counter = reloads.clone();
        let mut store = ShortcutStore::new(Some(&path), Some(Rc::new(move |_| *counter.borrow_mut() += 1)));
        store.tiling = Some(tiling());
        let lock = store.load().unwrap().into_iter().find(|i| i.key == "W-l").unwrap();
        store.rebind(&lock, "W-C-l", None).unwrap();
        assert!(String::from_utf8(std::fs::read(&path).unwrap()).unwrap().contains("key=\"W-C-l\""));
        assert_eq!(*reloads.borrow(), 1);
        let tiling = store.load().unwrap().into_iter().find(|i| i.key == "W-t").unwrap();
        assert!(store.rebind(&tiling, "W-y", None).unwrap_err().is_value());
    }
}
