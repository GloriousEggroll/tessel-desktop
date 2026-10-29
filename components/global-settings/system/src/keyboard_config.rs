//! keyboard_config.py: physical keyboard layouts, XKB options and key repeat
//! for labwc.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::compat::{self, casefold, value, Result};
use crate::labwc_settings::{self, Done, KeyboardSection, Reconfigure};

pub const RULES: &[&str] = &["/usr/share/X11/xkb/rules/evdev.lst", "/usr/share/X11/xkb/rules/base.lst"];
static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_()+-]{1,64}$").unwrap());
static OPTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_]+:[A-Za-z0-9_-]+$").unwrap());
/// labwc's own defaults (rc.xml.all).
pub const DEFAULT_RATE: i64 = 25;
pub const DEFAULT_DELAY: i64 = 600;

pub type Pairs = Vec<(String, String)>;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Catalog {
    /// `(code, description)`.
    pub layouts: Pairs,
    /// layout → `((variant, description), …)`.
    pub variants: BTreeMap<String, Pairs>,
    /// group → `((option, description), …)`.
    pub options: BTreeMap<String, Pairs>,
}

fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    // dict(pairs): the last duplicate wins.
    pairs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

impl Catalog {
    pub fn new(layouts: &[(&str, &str)], variants: &[(&str, &[(&str, &str)])], options: &[(&str, &[(&str, &str)])]) -> Catalog {
        let pairs = |items: &[(&str, &str)]| items.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Pairs>();
        Catalog {
            layouts: pairs(layouts),
            variants: variants.iter().map(|(k, v)| (k.to_string(), pairs(v))).collect(),
            options: options.iter().map(|(k, v)| (k.to_string(), pairs(v))).collect(),
        }
    }

    pub fn layout_name(&self, layout: &str, variant: &str) -> String {
        if !variant.is_empty() {
            if let Some(name) = self.variants.get(layout).and_then(|v| lookup(v, variant)).filter(|n| !n.is_empty()) {
                return name.to_owned();
            }
        }
        lookup(&self.layouts, layout).map(str::to_owned)
            .unwrap_or_else(|| if variant.is_empty() { layout.to_owned() } else { format!("{layout} ({variant})") })
    }
}

pub fn parse_rules(text: &str, translate: &dyn Fn(&str) -> String) -> Catalog {
    let mut sections: Vec<(String, Pairs)> = Vec::new();
    let mut current: Option<String> = None;
    for line in compat::splitlines(text, false) {
        if let Some(name) = line.strip_prefix('!') {
            let name = compat::strip(name).to_owned();
            if !sections.iter().any(|(n, _)| *n == name) {
                sections.push((name.clone(), Vec::new()));
            }
            current = Some(name);
            continue;
        }
        let Some(section) = &current else { continue };
        if compat::strip(&line).is_empty() {
            continue;
        }
        let (first, rest) = compat::split_once_whitespace(&line);
        let description = match rest { Some(rest) => translate(compat::strip(&rest)), None => first.clone() };
        sections.iter_mut().find(|(n, _)| n == section).unwrap().1.push((first, description));
    }
    let get = |name: &str| sections.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let mut layouts = get("layout");
    layouts.sort_by_key(|item| casefold(&item.1));
    let mut variants: BTreeMap<String, Pairs> = BTreeMap::new();
    for (variant, description) in get("variant") {
        if let Some((layout, name)) = description.split_once(": ").filter(|(_, name)| !name.is_empty()) {
            variants.entry(layout.to_owned()).or_default().push((variant, name.to_owned()));
        }
    }
    for items in variants.values_mut() {
        items.sort_by_key(|item| casefold(&item.1));
    }
    let mut options: BTreeMap<String, Pairs> = BTreeMap::new();
    for (option, description) in get("option") {
        if let Some((group, _)) = option.split_once(':') {
            options.entry(group.to_owned()).or_default().push((option.clone(), description));
        }
    }
    Catalog { layouts, variants, options }
}

/// `xkeyboard-config` gettext translation of rule descriptions.
fn xkb_translation(language: &str) -> Box<dyn Fn(&str) -> String> {
    let base = language.split(['_', '.', '@']).next().unwrap_or("").to_owned();
    let mut catalogs = Vec::new();
    for code in [language.to_owned(), base] {
        let path = PathBuf::from("/usr/share/locale").join(&code).join("LC_MESSAGES/xkeyboard-config.mo");
        if let Ok(data) = std::fs::read(&path) {
            catalogs.push(crate::compat_mo::parse(&data));
            break;
        }
    }
    Box::new(move |text: &str| catalogs.iter().find_map(|c| c.get(text).cloned()).unwrap_or_else(|| text.to_owned()))
}

/// Read the XKB rules list (call from a worker thread).
pub fn load_catalog(language: &str) -> Catalog {
    load_catalog_from(language, &RULES.iter().map(PathBuf::from).collect::<Vec<_>>(), &|| {
        let mut command = std::process::Command::new("localectl");
        command.arg("list-x11-keymap-layouts");
        crate::input_devices::run_timeout(command, 10).filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    })
}

pub fn load_catalog_from(language: &str, rules: &[PathBuf], localectl: &dyn Fn() -> Option<String>) -> Catalog {
    let translate = xkb_translation(language);
    for path in rules {
        if let Ok(text) = std::fs::read_to_string(path) {
            return parse_rules(&text, &*translate);
        }
    }
    let output = localectl().unwrap_or_else(|| "us\n".into());
    Catalog {
        layouts: compat::split_whitespace(&output).into_iter().filter(|c| CODE.is_match(c)).map(|c| (c.to_owned(), c.to_owned())).collect(),
        ..Default::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardState {
    /// `(layout, variant)`.
    pub layouts: Vec<(String, String)>,
    pub options: Vec<String>,
    pub repeat_rate: i64,
    pub repeat_delay: i64,
}

impl Default for KeyboardState {
    fn default() -> Self {
        KeyboardState { layouts: vec![("us".into(), String::new())], options: Vec::new(), repeat_rate: DEFAULT_RATE, repeat_delay: DEFAULT_DELAY }
    }
}

impl KeyboardState {
    pub fn option(&self, group: &str) -> String {
        let prefix = format!("{group}:");
        self.options.iter().find(|o| o.starts_with(&prefix)).cloned().unwrap_or_default()
    }

    pub fn with_option(&self, group: &str, item: &str) -> KeyboardState {
        let prefix = format!("{group}:");
        let mut options: Vec<String> = self.options.iter().filter(|o| !o.starts_with(&prefix)).cloned().collect();
        if !item.is_empty() {
            options.push(item.to_owned());
        }
        KeyboardState { options, ..self.clone() }
    }
}

fn split(item: Option<&String>) -> Vec<String> {
    item.map(String::as_str).unwrap_or("").split(',').map(|s| compat::strip(s).to_owned()).collect()
}

pub fn state_from(environment: &std::collections::HashMap<String, String>, keyboard: &KeyboardSection) -> KeyboardState {
    let mut layouts: Vec<String> = split(environment.get("XKB_DEFAULT_LAYOUT")).into_iter().filter(|s| !s.is_empty()).collect();
    if layouts.is_empty() {
        layouts = vec!["us".into()];
    }
    let variants = split(environment.get("XKB_DEFAULT_VARIANT"));
    let pairs = layouts.into_iter().enumerate().map(|(i, l)| (l, variants.get(i).cloned().unwrap_or_default())).collect();
    let options = split(environment.get("XKB_DEFAULT_OPTIONS")).into_iter().filter(|s| !s.is_empty()).collect();
    let number = |name: &str, default: i64, low: i64, high: i64| {
        let text = keyboard.values.get(name).map(|v| v.0.clone()).unwrap_or_default();
        match compat::py_int(&text) {
            Ok(v) if low <= v && v <= high => v,
            _ => default,
        }
    };
    KeyboardState { layouts: pairs, options, repeat_rate: number("repeatrate", DEFAULT_RATE, 1, 100),
                    repeat_delay: number("repeatdelay", DEFAULT_DELAY, 100, 3000) }
}

pub fn validate(state: &KeyboardState) -> Result<&KeyboardState> {
    if !(1..=4).contains(&state.layouts.len()) {
        return value("Choose between one and four keyboard layouts.");
    }
    for (layout, variant) in &state.layouts {
        if !CODE.is_match(layout) || (!variant.is_empty() && !CODE.is_match(variant)) {
            return value("Invalid keyboard layout.");
        }
    }
    if state.options.iter().any(|o| !OPTION.is_match(o)) {
        return value("Invalid keyboard option.");
    }
    if !(1..=100).contains(&state.repeat_rate) || !(100..=3000).contains(&state.repeat_delay) {
        return value("Invalid key repeat setting.");
    }
    Ok(state)
}

pub fn environment_values(state: &KeyboardState) -> labwc_settings::EnvValues {
    let variants: Vec<&str> = state.layouts.iter().map(|(_, v)| v.as_str()).collect();
    vec![
        ("XKB_DEFAULT_LAYOUT".into(), Some(state.layouts.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(","))),
        ("XKB_DEFAULT_VARIANT".into(), variants.iter().any(|v| !v.is_empty()).then(|| variants.join(","))),
        ("XKB_DEFAULT_OPTIONS".into(), (!state.options.is_empty()).then(|| state.options.join(","))),
    ]
}

pub struct KeyboardConfig {
    pub rc: PathBuf,
    pub environment: PathBuf,
    pub reload: Reconfigure,
}

impl KeyboardConfig {
    pub fn new(rc: Option<&Path>, environment: Option<&Path>, reload: Option<Reconfigure>) -> KeyboardConfig {
        let rc = rc.map(Path::to_path_buf).unwrap_or_else(labwc_settings::rc_path);
        let environment = environment.map(Path::to_path_buf).unwrap_or_else(|| labwc_settings::environment_path(Some(&rc)));
        KeyboardConfig { rc, environment, reload: reload.unwrap_or_else(labwc_settings::default_reconfigure) }
    }

    pub fn load(&self) -> Result<KeyboardState> {
        let environment = labwc_settings::read_environment(Some(&self.environment))?;
        let keyboard = labwc_settings::read_keyboard(&labwc_settings::read_bytes(&self.rc)?)?;
        Ok(state_from(&environment, &keyboard))
    }

    /// Write both files; returns `(environment_changed, rc_changed)`.
    pub fn save(&self, state: &KeyboardState, done: Option<Done>) -> Result<(bool, bool)> {
        validate(state)?;
        let environment = labwc_settings::update_environment(&environment_values(state), Some(&self.environment))?;
        let current = self.load()?;
        let mut values = Vec::new();
        if current.repeat_rate != state.repeat_rate {
            values.push(("repeatRate".to_owned(), Some(state.repeat_rate.to_string())));
        }
        if current.repeat_delay != state.repeat_delay {
            values.push(("repeatDelay".to_owned(), Some(state.repeat_delay.to_string())));
        }
        let rc = !values.is_empty()
            && labwc_settings::update_rc(|data| labwc_settings::set_keyboard_values(data, &values), Some(&self.rc))?;
        if environment || rc {
            (self.reload)(done);
        } else if let Some(done) = done {
            done(true, String::new());
        }
        Ok((environment, rc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labwc_settings::tests::RC;
    use crate::testutil::{scratch, EnvGuard};
    use std::cell::RefCell;
    use std::rc::Rc;

    const RULES_TEXT: &str = "! model
  pc105           Generic 105-key PC
! layout
  us              English (US)
  de              German
! variant
  nodeadkeys      de: German (no dead keys)
  dvorak          us: English (Dvorak)
! option
  grp             Switching to another layout
  grp:alt_shift_toggle Alt+Shift
  compose:ralt    Right Alt
";

    fn s(a: &str, b: &str) -> (String, String) { (a.into(), b.into()) }

    #[test]
    fn rules_catalog() {
        let catalog = parse_rules(RULES_TEXT, &|v| v.to_owned());
        assert_eq!(catalog.layouts, vec![s("us", "English (US)"), s("de", "German")]);
        assert_eq!(catalog.variants["de"], vec![s("nodeadkeys", "German (no dead keys)")]);
        assert_eq!(catalog.options["grp"], vec![s("grp:alt_shift_toggle", "Alt+Shift")]);
        assert_eq!(catalog.layout_name("us", "dvorak"), "English (Dvorak)");
        assert_eq!(catalog.layout_name("xx", "y"), "xx (y)");
        let fallback = load_catalog_from("en", &[PathBuf::from("/nonexistent/rules")], &|| Some("us de bad;x\n".into()));
        assert_eq!(fallback.layouts, vec![s("us", "us"), s("de", "de")]);
        assert_eq!(load_catalog_from("en", &[], &|| None).layouts, vec![s("us", "us")]);
    }

    #[test]
    fn save_writes_environment_and_repeat_then_reloads_once() {
        let root = scratch("kbd-save");
        let _env = EnvGuard::set(&[("XDG_STATE_HOME", root.join("state").to_str().unwrap())]);
        let (rc, env) = (root.join("rc.xml"), root.join("environment"));
        std::fs::write(&rc, RC).unwrap();
        std::fs::write(&env, "XKB_DEFAULT_LAYOUT=us\nXKB_DEFAULT_OPTIONS=grp:alt_shift_toggle\nOTHER=1\n").unwrap();
        let reloads = Rc::new(RefCell::new(0));
        let counter = reloads.clone();
        let config = KeyboardConfig::new(Some(&rc), Some(&env), Some(Rc::new(move |_done| *counter.borrow_mut() += 1)));
        let state = config.load().unwrap();
        assert_eq!(state, KeyboardState { layouts: vec![s("us", "")], options: vec!["grp:alt_shift_toggle".into()], repeat_rate: 30, repeat_delay: 600 });
        let state = KeyboardState { layouts: vec![s("us", ""), s("de", "nodeadkeys")],
                                    options: vec!["grp:alt_shift_toggle".into(), "compose:ralt".into()], repeat_rate: 30, repeat_delay: 400 };
        config.save(&state, None).unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "XKB_DEFAULT_LAYOUT=us,de\n\
            XKB_DEFAULT_OPTIONS=grp:alt_shift_toggle,compose:ralt\nOTHER=1\nXKB_DEFAULT_VARIANT=,nodeadkeys\n");
        assert!(String::from_utf8(std::fs::read(&rc).unwrap()).unwrap().contains("<repeatDelay>400</repeatDelay>"));
        assert_eq!(config.load().unwrap(), state);
        assert_eq!(*reloads.borrow(), 1);
        config.save(&state, None).unwrap();
        assert_eq!(*reloads.borrow(), 1);
        let bad = KeyboardState { layouts: vec![s("us;rm", "")], ..Default::default() };
        assert!(config.save(&bad, None).unwrap_err().is_value());
        let empty = KeyboardState { layouts: vec![], ..Default::default() };
        assert!(config.save(&empty, None).unwrap_err().is_value());
    }

    #[test]
    fn options_keep_unmanaged_groups() {
        let state = KeyboardState { options: vec!["caps:escape".into(), "grp:win_space_toggle".into()], ..Default::default() };
        assert_eq!(state.with_option("grp", "").options, ["caps:escape"]);
        assert_eq!(state.with_option("compose", "compose:lwin-altgr").option("compose"), "compose:lwin-altgr");
        validate(&state.with_option("compose", "compose:lwin-altgr")).unwrap();
    }
}
