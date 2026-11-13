//! shortcuts_model.py: every keyboard shortcut the session knows — labwc
//! defaults, rc.xml and tiling.
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use gtk::gdk;
use gtk::gdk::prelude::DisplayExtManual;

use crate::compat::{casefold, Result};
use crate::labwc_settings::{self, Action, Done, KeyboardSection, Reconfigure};

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
    ("tessel-shell-lock", "System", "Lock screen"),
    ("--toggle-recording", "System", "Start or stop screen recording"),
    ("--capture screen", "System", "Take a screenshot"), ("--capture area", "System", "Capture an area"),
    ("--capture window", "System", "Capture a window"), ("--capture workspace", "System", "Capture the workspace"),
    ("--shortcuts", "System", "Show keyboard shortcuts"),
    ("lab-sensible-terminal", "Launchers", "Open a terminal"),
    ("--settings", "Tiling", "Tiling settings"),
];
pub const WORKSPACE_TILING: &[&str] = &["workspace", "send", "send-silent", "workspace-monitor"];

/// `(key, label, description, command)` of tessel-tiling's BINDINGS.
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
    /// The key tessel-configure-session ships for this binding ("" for none).
    pub default_key: String,
}

impl Shortcut {
    pub fn editable(&self) -> bool { self.source == "user" && self.start >= 0 }

    /// A suite binding whose key is no longer the shipped one.
    pub fn changed(&self) -> bool {
        self.editable() && !self.default_key.is_empty() && normalize(&self.key) != normalize(&self.default_key)
    }

    /// `description.format(**values)` (English).
    pub fn formatted(&self) -> String {
        let values: Vec<(&str, tessel_core::pyfmt::Arg)> = self.values.iter()
            .map(|(k, v)| (k.as_str(), tessel_core::pyfmt::Arg::from(v.as_str()))).collect();
        tessel_core::pyfmt::format(&self.description, &values).unwrap_or_else(|_| self.description.clone())
    }

    /// `tr(description, **values)`.
    pub fn translated(&self) -> String {
        let values: Vec<(&str, tessel_core::pyfmt::Arg)> = self.values.iter()
            .map(|(k, v)| (k.as_str(), tessel_core::pyfmt::Arg::from(v.as_str()))).collect();
        tessel_core::i18n::translate_with(&self.description, &values)
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

/// tessel-tiling's BINDINGS as `(key, label, description, command)`.
pub fn tiling_bindings() -> Vec<TilingBinding> {
    tessel_tiling::bindings::BINDINGS.iter()
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
            if words.len() >= 2 && file_name(words[0]) == "tessel-action" {
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

/// tessel-configure-session's keybinding block (`tessel_core::session_keys`).
pub fn suite_keybinds() -> &'static Keybinds {
    static TABLE: LazyLock<Keybinds> = LazyLock::new(|| {
        tessel_core::session_keys::keybindings().into_iter()
            .map(|(key, name, attribute, value)| (key, vec![Action::new(name, &[(attribute, &value)])])).collect()
    });
    &TABLE
}

/// Actions compared by name and attributes, ignoring case of names and attribute order.
fn action_key(actions: &[Action]) -> Vec<(String, Vec<(String, String)>)> {
    actions.iter().map(|action| {
        let mut attributes: Vec<(String, String)> = action.attributes.iter().map(|(n, v)| (casefold(n), v.clone())).collect();
        attributes.sort();
        (casefold(&action.name), attributes)
    }).collect()
}

/// Shipped key of each keybind in the suite block, by start offset. As
/// tessel-configure-session keeps rebound keys, the nth block keybind with an
/// action pairs with the nth table row with that action.
pub fn suite_defaults(section: &KeyboardSection, table: &Keybinds) -> HashMap<usize, String> {
    let mut rows: HashMap<_, VecDeque<&str>> = HashMap::new();
    for (key, actions) in table {
        rows.entry(action_key(actions)).or_default().push_back(key);
    }
    let mut result = HashMap::new();
    for bind in section.keybinds.iter().filter(|b| b.suite && !b.tiling) {
        if let Some(key) = rows.get_mut(&action_key(&bind.actions)).and_then(VecDeque::pop_front) {
            result.insert(bind.start, key.to_owned());
        }
    }
    result
}

/// `(start, key, default)` of every changed suite keybind, and how many are
/// kept because a keybind of the user's own outside the block uses the default.
pub fn reset_plan(section: &KeyboardSection, table: &Keybinds) -> (Vec<(usize, String, String)>, usize) {
    let defaults = suite_defaults(section, table);
    let own: HashSet<String> = section.keybinds.iter()
        .filter(|b| !b.suite && !b.tiling && b.actions.first().is_some_and(|a| a.name != "None"))
        .map(|b| normalize(&b.key)).collect();
    let (mut changes, mut kept) = (Vec::new(), 0);
    for bind in &section.keybinds {
        let Some(default) = defaults.get(&bind.start) else { continue };
        if normalize(&bind.key) == normalize(default) {
            continue;
        }
        if own.contains(&normalize(default)) {
            kept += 1;
        } else {
            changes.push((bind.start, bind.key.clone(), default.clone()));
        }
    }
    (changes, kept)
}

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
    let suite = suite_defaults(&section, suite_keybinds());
    let mut result: Vec<(String, Shortcut)> = Vec::new();
    let add = |result: &mut Vec<(String, Shortcut)>, key: &str, actions: &[Action], source: &str, start: i64, overrides: String,
               default_key: String| {
        let normal = normalize(key);
        let Some(first) = actions.first() else {
            result.retain(|(k, _)| *k != normal); // an empty keybind removes an earlier one
            return;
        };
        let (group, description, values) = describe(&first.name, &first.attributes, tiling);
        let item = Shortcut { key: key.into(), description, values, group, source: source.into(), active: first.name != "None", start, overrides,
                               default_key };
        if let Some(slot) = result.iter_mut().find(|(k, _)| *k == normal) { slot.1 = item } else { result.push((normal, item)) }
    };
    if load_defaults {
        for (key, actions) in default_binds {
            add(&mut result, key, actions, "default", -1, String::new(), String::new());
        }
    }
    for bind in &section.keybinds {
        let normal = normalize(&bind.key);
        let overrides = result.iter().find(|(k, _)| *k == normal).filter(|(_, p)| p.source == "default")
            .map(|(_, p)| p.description.clone()).unwrap_or_default();
        let default_key = suite.get(&bind.start).cloned().unwrap_or_default();
        add(&mut result, &bind.key, &bind.actions, if bind.tiling { "tiling" } else { "user" }, bind.start as i64, overrides, default_key);
    }
    if !section.keybinds.iter().any(|b| b.tiling) {
        // Tiling is off: list its shortcuts as available when it is turned on.
        for (key, _label, description, command) in tiling {
            let normal = normalize(key);
            if !result.iter().any(|(k, _)| *k == normal) {
                result.push((normal, Shortcut { key: key.clone(), description: description.clone(), values: Vec::new(),
                    group: group_for(command).into(), source: "tiling".into(), active: false, start: -1, overrides: String::new(),
                    default_key: String::new() }));
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

/// The active shortcut of the user's own (or another suite binding) that holds
/// `shortcut`'s default key. labwc and tiling shortcuts there are the shipped state.
pub fn reset_blocker<'a>(shortcuts: &'a [Shortcut], shortcut: &Shortcut) -> Option<&'a Shortcut> {
    if shortcut.default_key.is_empty() {
        return None;
    }
    conflicts(shortcuts, &shortcut.default_key, Some(shortcut)).into_iter().find(|i| i.active && i.source == "user")
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

/// What [`Shortcuts::reset_all`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResetSummary {
    pub reset: usize,
    /// Left as they are: a shortcut of the user's own uses the default key.
    pub kept: usize,
}

/// `done(okay, summary)` of [`Shortcuts::reset_all`].
pub type ResetDone = Box<dyn FnOnce(bool, ResetSummary)>;

/// `ShortcutStore(path, reload)` and what the page needs of it.
pub trait Shortcuts {
    fn load(&self) -> Result<Vec<Shortcut>>;
    fn rebind(&self, shortcut: &Shortcut, new_key: &str, done: Option<Done>) -> Result<()>;
    /// Put the shipped key back. Returns the shortcut that holds it instead
    /// (nothing is written then).
    fn reset(&self, shortcut: &Shortcut, done: Option<Done>) -> Result<Option<Shortcut>>;
    /// Put every changed suite key back in one write and one reload.
    fn reset_all(&self, done: Option<ResetDone>) -> Result<()>;
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

    fn reset(&self, shortcut: &Shortcut, done: Option<Done>) -> Result<Option<Shortcut>> {
        if !shortcut.editable() || shortcut.default_key.is_empty() {
            return crate::compat::value("Only your own shortcuts can be changed here.");
        }
        if let Some(blocker) = reset_blocker(&self.load()?, shortcut) {
            return Ok(Some(blocker.clone()));
        }
        self.rebind(shortcut, &shortcut.default_key, done)?;
        Ok(None)
    }

    fn reset_all(&self, done: Option<ResetDone>) -> Result<()> {
        let summary = Cell::new(ResetSummary::default());
        let changed = labwc_settings::update_rc(|data| {
            let (changes, kept) = reset_plan(&labwc_settings::read_keyboard(data)?, suite_keybinds());
            summary.set(ResetSummary { reset: changes.len(), kept });
            if changes.is_empty() { Ok(data.to_vec()) } else { labwc_settings::rebind_many(data, &changes) }
        }, Some(&self.path))?;
        let summary = summary.get();
        if changed {
            (self.reload)(done.map(|done| -> Done { Box::new(move |okay, _| done(okay, summary)) }));
        } else if let Some(done) = done {
            done(true, ResetSummary { reset: 0, ..summary });
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
        let data = String::from_utf8_lossy(RC).replace("<default/>", "").replace("<!-- Tessel Tiling keyboard begin -->", "")
            .replace("<!-- Tessel Tiling keyboard end -->", "");
        let items = run(data.as_bytes());
        assert!(!items.contains_key("W-a"));
        assert_eq!(items["W-1"].group, "Workspaces");
        assert!(!items["W-1"].active);
    }

    #[test]
    fn suite_keybinding_block_is_described_and_editable() {
        let block = br#"<labwc_config><keyboard><default/><!-- Tessel keybindings begin: x --><keybind key="W-slash"><action name="Execute" command="~/.local/bin/tessel-action shortcuts"/></keybind><keybind key="C-A-Right"><action name="GoToDesktop" to="right"/></keybind><keybind key="W-S-2"><action name="SendToDesktop" to="2"/></keybind><!-- Tessel keybindings end --></keyboard></labwc_config>"#;
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

    /// The block in the shape tessel-configure-session writes it.
    pub fn suite_block() -> String {
        let mut lines = vec![format!("{}: generated by tessel-configure-session.", tessel_core::session_keys::BLOCK_BEGIN),
                             "     Keys changed in Global Settings are kept on reinstall. -->".to_owned()];
        for (key, name, attribute, value) in tessel_core::session_keys::keybindings() {
            lines.push(format!("<keybind key=\"{key}\"><action name=\"{name}\" {attribute}={}/></keybind>", labwc_settings::quoteattr(&value)));
        }
        lines.push(tessel_core::session_keys::BLOCK_END.to_owned());
        lines.join("\n    ")
    }

    /// An rc.xml as tessel-configure-session writes it, with a keybind of the
    /// user's own and the tiling block (which overrides W-C-Left).
    pub fn suite_rc() -> String {
        format!(r#"<?xml version="1.0"?>
<labwc_config>
  <keyboard>
    <default/>
    {}
    <keybind key="W-e"><action name="Execute" command="foo"/></keybind>
    <!-- Tessel Tiling keyboard begin -->
    <keybind key="W-t"><action name="Tiling" command="float"/></keybind>
    <keybind key="W-C-Left"><action name="Tiling" command="group prev"/></keybind>
    <!-- Tessel Tiling keyboard end -->
  </keyboard>
</labwc_config>
"#, suite_block())
    }

    /// Change the key of the nth suite row with `key`.
    pub fn rekey(text: &str, key: &str, new_key: &str) -> String {
        text.replacen(&format!("<keybind key=\"{key}\">"), &format!("<keybind key=\"{new_key}\">"), 1)
    }

    fn suite_defaults_for(text: &str) -> Vec<(String, String, String)> {
        let section = labwc_settings::read_keyboard(text.as_bytes()).unwrap();
        let defaults = suite_defaults(&section, suite_keybinds());
        section.keybinds.iter().map(|b| (b.key.clone(), defaults.get(&b.start).cloned().unwrap_or_default(),
                                         b.actions.first().map(|a| a.get("to", "")).unwrap_or_default())).collect()
    }

    fn store_on(text: &str, name: &str) -> (PathBuf, ShortcutStore, Rc<RefCell<i32>>, EnvGuard) {
        let folder = scratch(name);
        let env = EnvGuard::set(&[("XDG_STATE_HOME", folder.join("state").to_str().unwrap())]);
        let path = folder.join("rc.xml");
        std::fs::write(&path, text).unwrap();
        let reloads = Rc::new(RefCell::new(0));
        let counter = reloads.clone();
        let mut store = ShortcutStore::new(Some(&path), Some(Rc::new(move |done: Option<Done>| {
            *counter.borrow_mut() += 1;
            if let Some(done) = done { done(true, String::new()) }
        })));
        store.tiling = Some(tiling());
        let mut labwc = defaults();
        labwc.push(("W-Return".into(), vec![Action::new("Execute", &[("command", "lab-sensible-terminal")])]));
        store.defaults = Some(labwc);
        (path, store, reloads, env)
    }

    #[test]
    fn suite_table_is_parsed_and_markers_match_session_startup() {
        assert_eq!(suite_keybinds().len(), tessel_core::session_keys::keybindings().len());
        assert_eq!(suite_keybinds()[2].1[0].get("command", ""), format!("{} terminal", tessel_core::session_keys::ACTION));
        assert_eq!(tessel_core::session_keys::BLOCK_BEGIN, format!("<!-- {}", labwc_settings::TESSEL_KEYBINDINGS_BEGIN));
        assert_eq!(tessel_core::session_keys::BLOCK_END, format!("<!-- {} -->", labwc_settings::TESSEL_KEYBINDINGS_END));
    }

    #[test]
    fn suite_rows_pair_with_their_shipped_keys_by_action() {
        let text = suite_rc();
        // Unchanged: every block row's default is its own key.
        let rows = suite_defaults_for(&text);
        assert_eq!(rows.iter().filter(|(_, d, _)| !d.is_empty()).count(), suite_keybinds().len());
        assert!(rows.iter().all(|(key, d, _)| d.is_empty() || key == d));
        // A changed key, and duplicate actions swapped: paired first to first.
        let changed = rekey(&rekey(&rekey(&rekey(&text, "W-Return", "C-A-t"), "W-C-Left", "Swap"), "C-A-Left", "W-C-Left"), "Swap", "C-A-Left");
        let changed = changed.replacen("<keybind key=\"C-A-Left\"><action name=\"GoToDesktop\" to=\"left\"/>",
                                       "<keybind key=\"C-A-Left\"><action To=\"left\" name=\"GoToDesktop\"/>", 1);
        let rows = suite_defaults_for(&changed);
        let find = |key: &str| rows.iter().filter(|(k, _, _)| k == key).map(|(_, d, _)| d.clone()).collect::<Vec<_>>();
        assert_eq!(find("C-A-t"), ["W-Return"]);
        // Block order is now C-A-Left (first left row), then W-C-Left; tiling's W-C-Left has none.
        let left: Vec<_> = rows.iter().filter(|(_, _, to)| to == "left").map(|(k, d, _)| (k.as_str(), d.as_str())).collect();
        assert_eq!(left, [("C-A-Left", "W-C-Left"), ("W-C-Left", "C-A-Left")]);
        assert_eq!(find("W-e"), [""]);
        assert_eq!(find("W-t"), [""]);
        let items = collect(changed.as_bytes(), &tiling(), Some(&defaults())).unwrap();
        let get = |key: &str| items.iter().find(|i| i.key == key).unwrap().clone();
        assert!(get("C-A-t").changed());
        assert_eq!(get("C-A-t").default_key, "W-Return");
        assert_eq!(get("W-slash").default_key, "W-slash");
        assert!(!get("W-slash").changed());
        assert_eq!((get("W-e").default_key.as_str(), get("W-a").default_key.as_str(), get("W-t").default_key.as_str()), ("", "", ""));
        assert_eq!((get("W-a").source.as_str(), get("W-t").source.as_str()), ("default", "tiling"));
        // Tiling's W-C-Left overrides the block row there.
        assert_eq!(get("W-C-Left").source, "tiling");
        assert!(get("C-A-Left").changed());
    }

    #[test]
    fn reset_writes_the_default_key_and_reloads_once() {
        let (path, store, reloads, _env) = store_on(&rekey(&suite_rc(), "W-Return", "C-A-t"), "shortcut-reset");
        let terminal = store.load().unwrap().into_iter().find(|i| i.key == "C-A-t").unwrap();
        // The labwc default on W-Return is the shipped state: it does not block.
        assert!(store.load().unwrap().iter().any(|i| i.key == "W-Return" && i.source == "default"));
        let message = Rc::new(RefCell::new(None));
        let m = message.clone();
        assert_eq!(store.reset(&terminal, Some(Box::new(move |okay, _| *m.borrow_mut() = Some(okay)))).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), suite_rc());
        assert_eq!((*reloads.borrow(), *message.borrow()), (1, Some(true)));
        // Stale: the file no longer has C-A-t there.
        assert!(store.reset(&terminal, None).unwrap_err().is_runtime());
        // A tiling shortcut on the default key does not block either.
        std::fs::write(&path, rekey(&suite_rc(), "W-C-Left", "W-S-Left")).unwrap();
        let left = store.load().unwrap().into_iter().find(|i| i.key == "W-S-Left").unwrap();
        assert_eq!(store.reset(&left, None).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), suite_rc());
        assert_eq!(*reloads.borrow(), 2);
        let own = store.load().unwrap().into_iter().find(|i| i.key == "W-e").unwrap();
        assert!(store.reset(&own, None).unwrap_err().is_value());
    }

    #[test]
    fn reset_is_refused_while_a_user_shortcut_holds_the_default_key() {
        // The terminal and shortcut-sheet rows moved: W-Return now opens the sheet.
        let text = rekey(&rekey(&suite_rc(), "W-Return", "C-A-t"), "W-slash", "W-Return");
        let (path, store, reloads, _env) = store_on(&text, "shortcut-reset-clash");
        let items = store.load().unwrap();
        let terminal = items.iter().find(|i| i.key == "C-A-t").unwrap();
        let blocker = store.reset(terminal, None).unwrap().unwrap();
        assert_eq!((blocker.key.as_str(), blocker.description.as_str()), ("W-Return", "Show keyboard shortcuts"));
        assert_eq!(reset_blocker(&items, terminal).map(|b| b.key.as_str()), Some("W-Return"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(*reloads.borrow(), 0);
        // A keybind of the user's own outside the block blocks too.
        let own = rekey(&suite_rc(), "W-Return", "C-A-t").replace("<keybind key=\"W-e\">", "<keybind key=\"W-Return\">");
        std::fs::write(&path, &own).unwrap();
        let terminal = store.load().unwrap().into_iter().find(|i| i.key == "C-A-t").unwrap();
        assert_eq!(store.reset(&terminal, None).unwrap().unwrap().description, "Run {value1}");
        assert_eq!(*reloads.borrow(), 0);
    }

    #[test]
    fn reset_all_restores_every_suite_key_in_one_write() {
        let mut text = suite_rc();
        for (key, new_key) in [("W-Return", "C-A-t"), ("W-l", "W-C-S-l"), ("W-S-1", "F9"), ("W-C-Right", "C-A-S-Right"),
                               ("W-C-Left", "Swap"), ("C-A-Left", "W-C-Left"), ("Swap", "C-A-Left"), ("W-2", "W-F2")] {
            text = rekey(&text, key, new_key);
        }
        // The user's own keybind on W-2 keeps that row where it is.
        text = text.replace("<keybind key=\"W-e\">", "<keybind key=\"W-2\">");
        let (path, store, reloads, _env) = store_on(&text, "shortcut-reset-all");
        let summary = Rc::new(RefCell::new(None));
        let s = summary.clone();
        store.reset_all(Some(Box::new(move |okay, result| *s.borrow_mut() = Some((okay, result))))).unwrap();
        assert_eq!(*summary.borrow(), Some((true, ResetSummary { reset: 6, kept: 1 })));
        assert_eq!(*reloads.borrow(), 1);
        let expected = rekey(&suite_rc(), "W-2", "W-F2").replace("<keybind key=\"W-e\">", "<keybind key=\"W-2\">");
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, expected);
        labwc_settings::read_keyboard(written.as_bytes()).unwrap();
        assert!(path.parent().unwrap().join("state/tessel/backups/labwc-rc/rc.xml.orig").is_file());
        // Again: only the kept row differs, nothing is written or reloaded.
        let s = summary.clone();
        store.reset_all(Some(Box::new(move |okay, result| *s.borrow_mut() = Some((okay, result))))).unwrap();
        assert_eq!(*summary.borrow(), Some((true, ResetSummary { reset: 0, kept: 1 })));
        assert_eq!(*reloads.borrow(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn reset_all_with_nothing_changed_writes_nothing() {
        let (path, store, reloads, _env) = store_on(&suite_rc(), "shortcut-reset-none");
        assert!(!store.load().unwrap().iter().any(Shortcut::changed));
        let summary = Rc::new(RefCell::new(None));
        let s = summary.clone();
        store.reset_all(Some(Box::new(move |okay, result| *s.borrow_mut() = Some((okay, result))))).unwrap();
        assert_eq!(*summary.borrow(), Some((true, ResetSummary::default())));
        assert_eq!(*reloads.borrow(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), suite_rc());
        assert!(!path.parent().unwrap().join("state/tessel/backups/labwc-rc/rc.xml.orig").exists());
    }
}
