//! Deciding what each monitor should be: the part of the daemon with no
//! Wayland in it. Compatible with the way-displays layout rules (MIT,
//! Copyright 2021 Alexander Courtis), so saved layouts come out the same.

use super::config::{Align, Arrange, Cfg, Condition, LidCondition, ModeChoice, Round, VrrMode, SCREEN_SIZE_DPI};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    /// The mode object's protocol id.
    pub id: u32,
    pub width: i32,
    pub height: i32,
    pub mhz: i32,
}

impl Mode {
    pub fn hz(&self) -> i32 { (self.mhz + 500) / 1000 }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub enabled: bool,
    pub mode: Option<u32>,
    /// wl_fixed: 1/256ths.
    pub scale: i32,
    pub x: i32,
    pub y: i32,
    pub transform: i32,
    /// Adaptive sync is on right now. Only read: the compositor switches it
    /// (labwc's `<adaptiveSyncOutputs>`), the daemon never asks for it.
    pub vrr: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Head {
    /// The head object's protocol id.
    pub id: u32,
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub width_mm: i32,
    pub height_mm: i32,
    pub modes: Vec<Mode>,
    pub modes_failed: Vec<Mode>,
    pub preferred: Option<u32>,
    pub cur: State,
    pub des: State,
    pub scaled: (i32, i32),
    /// A manual choice against a conditional DISABLED rule.
    pub override_enabled: Option<bool>,
    pub reapply: bool,
    /// Whether the compositor accepts adaptive sync here: `None` until a test says.
    pub vrr_capable: Option<bool>,
    /// The current mode the last capability answer was for.
    pub vrr_probed: Option<u32>,
}

/// Lid state; `None` when there is no lid.
pub type Lid = Option<bool>;

pub fn matches_exact(head: &Head, name_desc: &str) -> bool {
    head.name == name_desc || head.description == name_desc
}

pub fn matches_regex(head: &Head, name_desc: &str) -> bool {
    let Some(pattern) = name_desc.strip_prefix('!').filter(|pattern| !pattern.is_empty()) else { return false };
    regex::Regex::new(pattern).is_ok_and(|regex| regex.is_match(&head.name) || regex.is_match(&head.description))
}

pub fn matches_fuzzy(head: &Head, name_desc: &str) -> bool {
    if name_desc.starts_with('!') || name_desc.is_empty() {
        return false;
    }
    let needle = name_desc.to_lowercase();
    head.name.to_lowercase().contains(&needle) || head.description.to_lowercase().contains(&needle)
}

pub fn matches(head: &Head, name_desc: &str) -> bool {
    matches_exact(head, name_desc) || matches_regex(head, name_desc) || matches_fuzzy(head, name_desc)
}

/// Whether `text` looks like a connector name: one to three groups that start
/// with a letter and hold only letters and digits, joined by dashes, then a
/// dash and a number of up to five digits (`DP-2`, `HDMI-A-1`, `eDP-1`,
/// `DVI-D-1`, `USB-C-1`, `Virtual-1`, `HEADLESS-1`, `X11-1`).
pub fn is_connector_name(text: &str) -> bool {
    let Some((kind, number)) = text.rsplit_once('-') else { return false };
    let groups: Vec<&str> = kind.split('-').collect();
    (1..=5).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit()) && groups.len() <= 3
        && groups.iter().all(|group| group.bytes().next().is_some_and(|b| b.is_ascii_alphabetic()) && group.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `"<identity> (<connector>)"` split in two, the form of a wlroots output
/// description. `None` for anything else.
pub fn split_description(text: &str) -> Option<(&str, &str)> {
    let (identity, connector) = text.strip_suffix(')')?.rsplit_once(" (")?;
    (!identity.trim().is_empty() && is_connector_name(connector)).then_some((identity, connector))
}

/// What wlroots prints for an output without a make or model (no EDID).
const NO_IDENTITY: [&str; 3] = ["", "Unknown", "(null)"];

/// The monitor part of a head's description: what is left without
/// `" (<connector>)"`. `None` when the description has another form or the
/// monitor has neither a make nor a model, so the text names no monitor.
pub fn head_identity(head: &Head) -> Option<&str> {
    let (identity, connector) = split_description(&head.description)?;
    let known = |field: &str| !NO_IDENTITY.contains(&field.trim());
    (connector == head.name && (known(&head.make) || known(&head.model))).then_some(identity)
}

/// The same monitor on another connector: `name_desc` is a description that
/// differs from the head's only in the connector. Never when another connected
/// head has the same identity (identical monitors), nor when the entry's own
/// description is connected.
pub fn matches_moved(head: &Head, heads: &[Head], name_desc: &str) -> bool {
    let (Some(identity), Some((entry_identity, connector))) = (head_identity(head), split_description(name_desc)) else { return false };
    !name_desc.starts_with('!') && identity == entry_identity && connector != head.name
        && !heads.iter().any(|other| other.description == name_desc
            || (other.name != head.name && split_description(&other.description).is_some_and(|(other_identity, _)| other_identity == identity)))
}

/// The entry that applies to a head: the first one matching by name,
/// description, regex or partial name, else the last one saved for this
/// monitor under another connector.
pub fn entry_for<'a, T>(head: &Head, heads: &[Head], entries: &'a [(String, T)]) -> Option<&'a (String, T)> {
    entries.iter().find(|(name, _)| matches(head, name))
        .or_else(|| entries.iter().rev().find(|(name, _)| matches_moved(head, heads, name)))
}

/// Whether a head is present: matched by any rule, the same monitor on another connector included.
fn plugged(heads: &[Head], name: &str) -> bool {
    heads.iter().any(|head| matches(head, name) || matches_moved(head, heads, name))
}

fn condition_met(condition: &Condition, heads: &[Head], lid: Lid, ignore_closed: bool) -> bool {
    condition.plugged.iter().all(|name| plugged(heads, name))
        && !condition.unplugged.iter().any(|name| plugged(heads, name))
        && match condition.lid {
            Some(LidCondition::Closed) => lid == Some(true) && !ignore_closed,
            Some(LidCondition::Open) => lid == Some(false),
            Some(LidCondition::NotPresent) => lid.is_none(),
            None => true,
        }
}

/// The head's adaptive sync choice: in VRR_OFF is Off (also when it is in
/// VRR_ALWAYS too), in VRR_ALWAYS is Always, in neither is Automatic. Entries
/// saved for this monitor under another connector count only when no entry
/// in either list matches it otherwise.
pub fn vrr_mode(cfg: &Cfg, head: &Head, heads: &[Head]) -> VrrMode {
    let pick = |test: &dyn Fn(&str) -> bool| {
        if cfg.vrr_off.iter().any(|name| test(name)) {
            Some(VrrMode::Off)
        } else if cfg.vrr_always.iter().any(|name| test(name)) {
            Some(VrrMode::Always)
        } else {
            None
        }
    };
    pick(&|name| matches(head, name)).or_else(|| pick(&|name| matches_moved(head, heads, name))).unwrap_or(VrrMode::Auto)
}

/// Connector name and adaptive sync choice of every connected head, for labwc.
pub fn vrr_modes(cfg: &Cfg, heads: &[Head]) -> std::collections::BTreeMap<String, VrrMode> {
    heads.iter().filter(|head| !head.name.is_empty()).map(|head| (head.name.clone(), vrr_mode(cfg, head, heads))).collect()
}

/// The next head to test adaptive sync on: enabled, with a current mode that
/// has no answer yet. A head with adaptive sync on right now needs no test.
pub fn vrr_probe_candidate(heads: &[Head]) -> Option<usize> {
    heads.iter().position(|head| head.cur.enabled && head.cur.mode.is_some() && head.vrr_probed != head.cur.mode)
}

/// Whether DISABLED turns this head off. A closed lid is ignored when the
/// laptop panel is the only display, so it can still sleep and wake smoothly.
pub fn disabled_applies(cfg: &Cfg, head: &Head, heads: &[Head], lid: Lid) -> bool {
    disabled_entries(cfg, head, heads).iter().any(|(_, conditions)| {
        conditions.is_empty() || conditions.iter().any(|condition| condition_met(condition, heads, lid, heads.len() == 1))
    })
}

/// The DISABLED entries for a head: all that match it by name, description,
/// regex or partial name, else the last one saved for this monitor under
/// another connector.
fn disabled_entries<'a>(cfg: &'a Cfg, head: &Head, heads: &[Head]) -> Vec<&'a (String, Vec<Condition>)> {
    let direct: Vec<_> = cfg.disabled.iter().filter(|(name, _)| matches(head, name)).collect();
    if !direct.is_empty() {
        return direct;
    }
    cfg.disabled.iter().rev().find(|(name, _)| matches_moved(head, heads, name)).into_iter().collect()
}

fn fixed_scale(cfg: &Cfg, scale: f64) -> i32 {
    let base = f64::from(cfg.round_base);
    let fixed = (scale * 256.0).round();
    let scaled = fixed / 256.0 * base;
    let rounded = match cfg.round {
        Round::Nearest => scaled.round(),
        Round::Up => scaled.ceil(),
        Round::Down => scaled.floor(),
    };
    (rounded * (256.0 / base)) as i32
}

/// Pixel density that looks right at scale 1, by how far away a screen is used.
const CLOSE_SCREEN_DPI: f64 = 135.0;
const FAR_SCREEN_DPI: f64 = 110.0;
const CLOSE_SCREEN_MAX_INCHES: f64 = 20.0;
/// The highest automatic scale without an AUTO_SCALE_MAX, and the top of the Displays slider.
const AUTO_SCALE_CEILING: f64 = 2.5;

fn auto_scale(cfg: &Cfg, head: &Head, mode: &Mode) -> i32 {
    if head.width_mm == 0 || head.height_mm == 0 {
        return fixed_scale(cfg, 1.0);
    }
    let (width_mm, height_mm) = (f64::from(head.width_mm), f64::from(head.height_mm));
    let horizontal = f64::from(mode.width) / width_mm * 25.4;
    let vertical = f64::from(mode.height) / height_mm * 25.4;
    let dpi = (horizontal + vertical) / 2.0;
    let inches = width_mm.hypot(height_mm) / 25.4;
    let target = if cfg.dpi != SCREEN_SIZE_DPI {
        f64::from(cfg.dpi)
    } else if inches < CLOSE_SCREEN_MAX_INCHES {
        CLOSE_SCREEN_DPI
    } else {
        FAR_SCREEN_DPI
    };
    let base = f64::from(cfg.round_base);
    let min = ((cfg.auto_min * base).ceil() / base).max(1.0 / base);
    let max = if cfg.auto_max > 0.0 { (cfg.auto_max * base).floor() / base } else { AUTO_SCALE_CEILING };
    let scale = dpi / target;
    let clamped = if scale < min {
        min
    } else if min <= max && scale > max {
        max
    } else {
        scale
    };
    fixed_scale(cfg, clamped)
}

fn greatest_first(modes: &[Mode]) -> Vec<Mode> {
    let mut sorted = modes.to_vec();
    sorted.sort_by_key(|mode| std::cmp::Reverse((mode.width, mode.height, mode.mhz)));
    sorted
}

/// The mode a head should use: its MODE setting if a mode satisfies it, else
/// the preferred mode, else the largest. `None` when it has no modes at all.
pub fn find_mode(cfg: &Cfg, head: &Head, heads: &[Head]) -> Option<u32> {
    let preferred = head.preferred.and_then(|id| head.modes.iter().find(|mode| mode.id == id));
    let sorted = greatest_first(&head.modes);
    let chosen = entry_for(head, heads, &cfg.modes).and_then(|(_, choice)| match choice {
        ModeChoice::Max => sorted.first().copied(),
        ModeChoice::MaxPreferredRefresh => preferred.and_then(|p| sorted.iter().find(|m| m.width == p.width && m.height == p.height).copied()),
        ModeChoice::Size { width, height, mhz } => {
            let same = |mode: &&Mode| mode.width == *width && mode.height == *height;
            let exact = mhz.and_then(|mhz| sorted.iter().filter(same).find(|mode| mode.mhz == mhz).copied());
            exact.or_else(|| sorted.iter().filter(same).find(|mode| match mhz {
                Some(mhz) => mode.hz() == (mhz + 500) / 1000,
                None => true,
            }).copied())
        }
    });
    chosen.or(preferred.copied()).or_else(|| {
        // Largest area, then the highest refresh at that size.
        head.modes.iter().copied().max_by_key(|mode| (i64::from(mode.width) * i64::from(mode.height), mode.mhz))
    }).map(|mode| mode.id)
}

fn scaled_length(cfg: &Cfg, scale: i32, length: i32) -> i32 {
    let base = f64::from(cfg.round_base);
    let factor = (f64::from(scale) / 256.0 * base + 0.5).floor();
    (f64::from(length) * base / factor).floor() as i32
}

/// A request naming a head under a conditional DISABLED rule is a manual choice
/// for now, not a change to the rule. It lasts until the rule agrees. A request
/// naming the rule itself is about the rule: Global Settings sends every saved
/// rule back on each Apply, and that must not switch the head.
pub fn apply_disabled_overrides(cfg: &Cfg, heads: &mut [Head], command: &str, requested: &mut Vec<(String, Vec<Condition>)>) {
    let is_rule = |name: &str| cfg.disabled.iter().any(|(rule, _)| rule == name);
    let mut handled = Vec::new();
    let snapshot: Vec<Head> = heads.to_vec();
    for head in heads {
        let conditional = disabled_entries(cfg, head, &snapshot).iter().any(|(_, conditions)| !conditions.is_empty());
        if !conditional {
            continue;
        }
        let named: Vec<String> = requested.iter().filter(|(name, _)| !is_rule(name) && (matches(head, name) || matches_moved(head, &snapshot, name)))
            .map(|(name, _)| name.clone()).collect();
        for name in named {
            handled.push(name);
            let toggle = match command {
                "CFG_TOGGLE" => true,
                "CFG_DEL" => !head.cur.enabled,
                "CFG_SET" => head.cur.enabled,
                _ => false,
            };
            if toggle {
                head.override_enabled = match head.override_enabled {
                    None => Some(!head.cur.enabled),
                    Some(_) => None,
                };
            }
        }
    }
    requested.retain(|(name, _)| !handled.contains(name));
}

/// Fill in `des` and `scaled` for every head.
pub fn desire(cfg: &Cfg, heads: &mut [Head], lid: Lid) {
    let snapshot: Vec<Head> = heads.to_vec();
    for head in heads.iter_mut() {
        head.des = head.cur;
        let enabled = !disabled_applies(cfg, head, &snapshot, lid);
        if head.override_enabled == Some(enabled) {
            head.override_enabled = None;
        }
        head.des.enabled = head.override_enabled.unwrap_or(enabled);
        if head.des.enabled {
            match find_mode(cfg, head, &snapshot) {
                Some(mode) => head.des.mode = Some(mode),
                None => head.des.enabled = false,
            }
        }
        if head.des.enabled {
            let mode = head.des.mode.and_then(|id| head.modes.iter().find(|mode| mode.id == id)).copied();
            head.des.scale = if !cfg.scaling {
                fixed_scale(cfg, 1.0)
            } else if let Some((_, scale)) = entry_for(head, &snapshot, &cfg.scales) {
                fixed_scale(cfg, *scale)
            } else if let (true, Some(mode)) = (cfg.auto_scale, mode) {
                auto_scale(cfg, head, &mode)
            } else {
                fixed_scale(cfg, 1.0)
            };
            head.des.transform = entry_for(head, &snapshot, &cfg.transforms).map_or(0, |(_, transform)| *transform);
            if let (Some(mode), true) = (mode, head.des.scale != 0) {
                let (width, height) = if head.des.transform % 2 == 0 { (mode.width, mode.height) } else { (mode.height, mode.width) };
                head.scaled = (scaled_length(cfg, head.des.scale, width), scaled_length(cfg, head.des.scale, height));
            }
        }
        if head.reapply {
            head.des.enabled = false;
        }
    }
    let order = ordered(cfg, heads);
    positions(cfg, heads, &order);
}

/// Head indices in layout order: ORDER entries take exact matches first, then
/// regular expressions, then partial names, then the same monitor on another
/// connector; anything unmatched comes last.
pub fn ordered(cfg: &Cfg, heads: &[Head]) -> Vec<usize> {
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); cfg.order.len()];
    let mut left: Vec<usize> = (0..heads.len()).collect();
    let moved = |head: &Head, name: &str| matches_moved(head, heads, name);
    let tests: [&dyn Fn(&Head, &str) -> bool; 4] = [&matches_exact, &matches_regex, &matches_fuzzy, &moved];
    for test in tests {
        for (bucket, name) in buckets.iter_mut().zip(&cfg.order) {
            left.retain(|&index| {
                let found = test(&heads[index], name);
                if found {
                    bucket.push(index);
                }
                !found
            });
        }
    }
    buckets.into_iter().flatten().chain(left).collect()
}

fn positions(cfg: &Cfg, heads: &mut [Head], order: &[usize]) {
    let shown = |head: &Head| head.des.enabled && head.des.mode.is_some();
    let tallest = heads.iter().filter(|head| shown(head)).map(|head| head.scaled.1).max().unwrap_or(0);
    let widest = heads.iter().filter(|head| shown(head)).map(|head| head.scaled.0).max().unwrap_or(0);
    let (mut x, mut y) = (0, 0);
    for &index in order {
        let head = &mut heads[index];
        if !shown(head) {
            continue;
        }
        let (width, height) = head.scaled;
        match cfg.arrange {
            Arrange::Column => {
                head.des.y = y;
                y += height;
                head.des.x = match cfg.align {
                    Align::Right => widest - width,
                    Align::Middle => (f64::from(widest - width) / 2.0 + 0.5) as i32,
                    _ => 0,
                };
            }
            Arrange::Row => {
                head.des.x = x;
                x += width;
                head.des.y = match cfg.align {
                    Align::Bottom => tallest - height,
                    Align::Middle => (f64::from(tallest - height) / 2.0 + 0.5) as i32,
                    _ => 0,
                };
            }
        }
    }
}

/// The next change to send. Reapplying and a mode change each travel alone,
/// because a compositor can refuse those one by one. Adaptive sync is never
/// part of a change: labwc switches it from rc.xml.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    Reapply(usize),
    Mode(usize),
    All(Vec<usize>),
}

pub fn plan(heads: &[Head]) -> Plan {
    let changing: Vec<usize> = (0..heads.len()).filter(|&i| heads[i].reapply || heads[i].des != heads[i].cur).collect();
    if changing.is_empty() {
        return Plan::Nothing;
    }
    if let Some(&index) = changing.iter().find(|&&i| heads[i].reapply) {
        return Plan::Reapply(index);
    }
    if let Some(&index) = changing.iter().find(|&&i| heads[i].des.mode != heads[i].cur.mode) {
        return Plan::Mode(index);
    }
    Plan::All(changing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::config::Cfg;

    fn mode(id: u32, width: i32, height: i32, mhz: i32) -> Mode { Mode { id, width, height, mhz } }

    fn head(id: u32, name: &str, description: &str, modes: Vec<Mode>, preferred: u32) -> Head {
        Head { id, name: name.into(), description: description.into(), modes, preferred: Some(preferred), width_mm: 600, height_mm: 340,
               cur: State { enabled: true, mode: Some(preferred), scale: 256, ..Default::default() }, ..Default::default() }
    }

    fn pair() -> Vec<Head> {
        vec![
            head(1, "HDMI-A-1", "UGU UGREEN-UHD 0x01000E00 (HDMI-A-1)", vec![mode(10, 1920, 1080, 60000), mode(11, 3840, 2160, 60000)], 11),
            head(2, "DP-2", "Dell Inc. AW3225QF 2Q49YZ3 (DP-2)", vec![mode(20, 3840, 2160, 239990), mode(21, 3840, 2160, 60000)], 20),
        ]
    }

    #[test]
    fn name_matching_is_exact_then_regex_then_partial() {
        let heads = pair();
        assert!(matches(&heads[1], "DP-2") && matches(&heads[1], "Dell Inc. AW3225QF 2Q49YZ3 (DP-2)"));
        assert!(matches(&heads[1], "!^DP-[0-9]$") && !matches(&heads[0], "!^DP-[0-9]$"));
        assert!(matches(&heads[1], "aw3225qf"), "partial names ignore case");
        assert!(!matches(&heads[1], "!not(("), "a bad regular expression matches nothing");
    }

    #[test]
    fn order_follows_order_and_the_rest_come_last() {
        let heads = pair();
        let mut cfg = Cfg::default();
        assert_eq!(ordered(&cfg, &heads), [0, 1]);
        cfg.order = vec!["Dell".into(), "UGU".into()];
        assert_eq!(ordered(&cfg, &heads), [1, 0]);
        cfg.order = vec!["DP-2".into()];
        assert_eq!(ordered(&cfg, &heads), [1, 0]);
    }

    #[test]
    fn a_row_places_monitors_side_by_side_at_their_scale() {
        let mut heads = pair();
        let mut cfg = Cfg { order: vec!["DP-2".into(), "HDMI-A-1".into()], ..Default::default() };
        cfg.scales = vec![("DP-2".into(), 2.0)];
        desire(&cfg, &mut heads, None);
        assert_eq!((heads[1].des.x, heads[1].des.y, heads[1].des.scale), (0, 0, 512));
        assert_eq!(heads[1].scaled, (1920, 1080));
        assert_eq!((heads[0].des.x, heads[0].des.y), (1920, 0));
        cfg.align = Align::Bottom;
        desire(&cfg, &mut heads, None);
        assert_eq!((heads[1].des.y, heads[0].des.y), (1080, 0), "the shorter monitor sits at the bottom");
        cfg.align = Align::Middle;
        desire(&cfg, &mut heads, None);
        assert_eq!(heads[1].des.y, 540);
    }

    #[test]
    fn a_column_stacks_monitors() {
        let mut heads = pair();
        let cfg = Cfg { arrange: Arrange::Column, align: Align::Right, scales: vec![("DP-2".into(), 2.0)], order: vec!["HDMI".into()], ..Default::default() };
        desire(&cfg, &mut heads, None);
        assert_eq!((heads[0].des.x, heads[0].des.y), (0, 0));
        assert_eq!((heads[1].des.x, heads[1].des.y), (1920, 2160));
    }

    #[test]
    fn rotation_swaps_the_scaled_size() {
        let mut heads = pair();
        let cfg = Cfg { transforms: vec![("HDMI".into(), 1)], order: vec!["HDMI".into(), "DP-2".into()], ..Default::default() };
        desire(&cfg, &mut heads, None);
        assert_eq!(heads[0].scaled, (2160, 3840));
        assert_eq!(heads[0].des.transform, 1);
        assert_eq!(heads[1].des.x, 2160);
    }

    #[test]
    fn scale_rounds_to_the_configured_step() {
        let cfg = Cfg::default();
        assert_eq!(fixed_scale(&cfg, 1.3), 320, "1.25 is the nearest eighth");
        assert_eq!(fixed_scale(&Cfg { round: Round::Up, ..Default::default() }, 1.3), 352, "1.375");
        assert_eq!(fixed_scale(&Cfg { round: Round::Down, ..Default::default() }, 1.3), 320);
        assert_eq!(fixed_scale(&Cfg { round_base: 2, ..Default::default() }, 1.3), 384, "1.5 at half steps");
    }

    #[test]
    fn auto_scale_follows_dpi_within_its_limits() {
        // 3840 wide on 600 mm and 2160 on 340 mm: about 162 dpi on a 27 inch screen.
        let heads = pair();
        let mut cfg = Cfg { auto_scale: true, ..Default::default() };
        let mode = heads[0].modes[1];
        assert_eq!(auto_scale(&cfg, &heads[0], &mode), 384, "162 / 110 rounds to 1.5");
        cfg.auto_max = 1.25;
        assert_eq!(auto_scale(&cfg, &heads[0], &mode), 320);
        cfg.auto_max = -1.0;
        cfg.auto_min = 2.0;
        assert_eq!(auto_scale(&cfg, &heads[0], &mode), 512);
        let mut unknown = heads[0].clone();
        unknown.width_mm = 0;
        assert_eq!(auto_scale(&cfg, &unknown, &mode), 256, "no physical size means scale 1");
    }

    #[test]
    fn auto_scale_suits_the_size_of_the_screen() {
        let cfg = Cfg { auto_scale: true, ..Default::default() };
        let scale = |width, height, width_mm, height_mm| {
            let mut screen = pair()[0].clone();
            (screen.width_mm, screen.height_mm) = (width_mm, height_mm);
            auto_scale(&cfg, &screen, &mode(1, width, height, 60000))
        };
        assert_eq!(scale(2880, 1920, 280, 190), 480, "a 13 inch laptop that way-displays' formula put at 2.75 is 1.875");
        assert_eq!(scale(1920, 1080, 344, 194), 256, "15.6 inch 1080p");
        assert_eq!(scale(1920, 1080, 531, 299), 256, "24 inch 1080p never goes below 1");
        assert_eq!(scale(5120, 2880, 600, 340), 512, "27 inch 5K");
        assert_eq!(scale(7680, 4320, 300, 170), 640, "stays within the Displays slider");
    }

    #[test]
    fn a_chosen_auto_scale_dpi_is_used_for_every_screen() {
        let cfg = Cfg { auto_scale: true, dpi: 120, auto_max: 3.0, ..Default::default() };
        let mut laptop = pair()[0].clone();
        (laptop.width_mm, laptop.height_mm) = (280, 190);
        assert_eq!(auto_scale(&cfg, &laptop, &mode(1, 2880, 1920, 60000)), 544, "259 / 120 rounds to 2.125");
    }

    #[test]
    fn a_saved_mode_wins_then_the_preferred_one() {
        let heads = pair();
        let mut cfg = Cfg::default();
        assert_eq!(find_mode(&cfg, &heads[1], &[]), Some(20), "preferred");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: Some(60000) })];
        assert_eq!(find_mode(&cfg, &heads[1], &[]), Some(21), "exact refresh");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: Some(239_900) })];
        assert_eq!(find_mode(&cfg, &heads[1], &[]), Some(20), "same rounded refresh");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: None })];
        assert_eq!(find_mode(&cfg, &heads[1], &[]), Some(20), "highest refresh at that size");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 1, height: 1, mhz: None })];
        assert_eq!(find_mode(&cfg, &heads[1], &[]), Some(20), "a missing mode falls back to preferred");
        cfg.modes = vec![("HDMI".into(), ModeChoice::Max)];
        assert_eq!(find_mode(&cfg, &heads[0], &[]), Some(11));
        let mut none = heads[0].clone();
        none.preferred = None;
        assert_eq!(find_mode(&Cfg::default(), &none, &[]), Some(11), "largest without a preferred mode");
        none.modes.clear();
        assert_eq!(find_mode(&Cfg::default(), &none, &[]), None);
    }

    #[test]
    fn saving_the_lid_rule_back_leaves_the_panel_on() {
        let cfg = Cfg::default();
        let mut heads = vec![head(3, "eDP-1", "Internal panel", vec![mode(30, 1920, 1200, 60000)], 30)];
        desire(&cfg, &mut heads, Some(false));
        heads[0].cur = heads[0].des;
        assert!(heads[0].cur.enabled);
        let mut requested = cfg.disabled.clone();
        apply_disabled_overrides(&cfg, &mut heads, "CFG_SET", &mut requested);
        assert_eq!(heads[0].override_enabled, None, "the rule itself is not a request to turn the panel off");
        assert_eq!(requested, cfg.disabled, "the rule stays in the request");
        desire(&cfg, &mut heads, Some(false));
        assert!(heads[0].des.enabled);
    }

    #[test]
    fn naming_a_panel_under_the_lid_rule_is_a_manual_choice() {
        let cfg = Cfg::default();
        let mut heads = vec![head(3, "eDP-1", "Internal panel", vec![mode(30, 1920, 1200, 60000)], 30)];
        desire(&cfg, &mut heads, Some(false));
        heads[0].cur = heads[0].des;
        let mut requested = vec![("eDP-1".to_owned(), Vec::new())];
        apply_disabled_overrides(&cfg, &mut heads, "CFG_SET", &mut requested);
        assert_eq!(heads[0].override_enabled, Some(false));
        assert!(requested.is_empty(), "a manual choice is not saved as a rule");
    }

    #[test]
    fn disabled_rules_and_the_lid() {
        let mut heads = pair();
        heads.push(head(3, "eDP-1", "Internal panel", vec![mode(30, 1920, 1200, 60000)], 30));
        let cfg = Cfg::default();
        desire(&cfg, &mut heads, Some(true));
        assert!(!heads[2].des.enabled, "closed lid with other displays");
        desire(&cfg, &mut heads, Some(false));
        assert!(heads[2].des.enabled);
        let mut alone = vec![heads[2].clone()];
        desire(&cfg, &mut alone, Some(true));
        assert!(alone[0].des.enabled, "the only display stays on with the lid closed");
        let plain = Cfg { disabled: vec![("DP-2".into(), vec![])], ..Default::default() };
        desire(&plain, &mut heads, None);
        assert!(!heads[1].des.enabled);
        assert_eq!(heads[0].des.x, 0, "a disabled monitor takes no space");
        let conditional = Cfg { disabled: vec![("DP-2".into(), vec![Condition { plugged: vec!["HDMI".into()], ..Default::default() }])], ..Default::default() };
        desire(&conditional, &mut heads, None);
        assert!(!heads[1].des.enabled);
        heads[1].override_enabled = Some(true);
        desire(&conditional, &mut heads, None);
        assert!(heads[1].des.enabled, "a manual choice beats the condition");
    }

    #[test]
    fn vrr_mode_comes_from_the_two_lists_and_off_wins() {
        let heads = pair();
        let mut cfg = Cfg::default();
        assert_eq!((vrr_mode(&cfg, &heads[0], &heads), vrr_mode(&cfg, &heads[1], &heads)), (VrrMode::Auto, VrrMode::Auto), "in neither list");
        cfg.vrr_off = vec!["UGU UGREEN-UHD 0x01000E00 (HDMI-A-1)".into()];
        cfg.vrr_always = vec!["!^DP-".into()];
        assert_eq!((vrr_mode(&cfg, &heads[0], &heads), vrr_mode(&cfg, &heads[1], &heads)), (VrrMode::Off, VrrMode::Always), "by description and regex");
        cfg.vrr_always.push("HDMI-A-1".into());
        assert_eq!(vrr_mode(&cfg, &heads[0], &heads), VrrMode::Off, "in both lists is Off");
        let modes = vrr_modes(&cfg, &heads);
        assert_eq!(modes.into_iter().collect::<Vec<_>>(), [("DP-2".to_owned(), VrrMode::Always), ("HDMI-A-1".to_owned(), VrrMode::Off)]);
        let mut unnamed = heads.clone();
        unnamed[0].name.clear();
        assert_eq!(vrr_modes(&cfg, &unnamed).len(), 1, "a head without a name yet is left out");
    }

    #[test]
    fn the_daemon_never_asks_for_adaptive_sync() {
        let mut heads = pair();
        heads[0].cur.vrr = true;
        let cfg = Cfg { vrr_off: vec!["HDMI".into()], vrr_always: vec!["DP-2".into()], ..Default::default() };
        desire(&cfg, &mut heads, None);
        assert!(heads[0].des.vrr && !heads[1].des.vrr, "the desired state keeps what the compositor has");
        for head in &mut heads {
            head.cur = State { vrr: head.cur.vrr, ..head.des };
        }
        heads[1].cur.vrr = true;
        desire(&cfg, &mut heads, None);
        assert_eq!(plan(&heads), Plan::Nothing, "a screen labwc switched is no change to make");
    }

    #[test]
    fn each_enabled_head_is_tested_once_per_mode() {
        let mut heads = pair();
        assert_eq!(vrr_probe_candidate(&heads), Some(0));
        heads[0].vrr_probed = heads[0].cur.mode;
        assert_eq!(vrr_probe_candidate(&heads), Some(1));
        heads[1].vrr_probed = heads[1].cur.mode;
        assert_eq!(vrr_probe_candidate(&heads), None);
        heads[1].cur.mode = Some(21);
        assert_eq!(vrr_probe_candidate(&heads), Some(1), "a new mode is tested again");
        heads[1].cur.enabled = false;
        assert_eq!(vrr_probe_candidate(&heads), None, "a disabled head is not tested");
        heads[1].cur.enabled = true;
        heads[1].cur.mode = None;
        assert_eq!(vrr_probe_candidate(&heads), None, "nor one without a current mode");
    }

    #[test]
    fn modes_change_one_at_a_time_before_the_rest() {
        let mut heads = pair();
        let cfg = Cfg { modes: vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: Some(60000) })], scales: vec![("HDMI".into(), 2.0)], ..Default::default() };
        desire(&cfg, &mut heads, None);
        assert_eq!(plan(&heads), Plan::Mode(1));
        heads[1].cur.mode = heads[1].des.mode;
        match plan(&heads) {
            Plan::All(indices) => assert!(indices.contains(&0)),
            other => panic!("{other:?}"),
        }
        for head in &mut heads { head.cur = head.des; }
        assert_eq!(plan(&heads), Plan::Nothing);
        heads[0].reapply = true;
        assert_eq!(plan(&heads), Plan::Reapply(0));
    }

    // ------------------------------------------- the same monitor on another connector

    /// The configuration of GitHub issue #9: one monitor pair seen on two sets of connectors.
    const MOVED_FILE: &str = "ORDER:
- UGD Artist 12     0x20210910 (HDMI-A-2)
- Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-2)
MODE:
  UGD Artist 12     0x20210910 (HDMI-A-3): {WIDTH: 1920, HEIGHT: 1080, HZ: 60.003}
  Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-4): {WIDTH: 2560, HEIGHT: 1440, HZ: 239.97}
  UGD Artist 12     0x20210910 (HDMI-A-2): {WIDTH: 1920, HEIGHT: 1080, HZ: 60.003}
  Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-2): {WIDTH: 2560, HEIGHT: 1440, HZ: 239.97}
VRR_OFF:
- Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101 (DP-2)
";
    const MSI: &str = "Micro-Star Int'l Co., Ltd. MAG 274Q X24 0x01010101";
    const UGD: &str = "UGD Artist 12     0x20210910";

    fn monitor(id: u32, make: &str, model: &str, serial: &str, connector: &str) -> Head {
        let identity = [make, model, serial].join(" ");
        let modes = vec![mode(id * 10, 2560, 1440, 143_999), mode(id * 10 + 1, 2560, 1440, 239_970), mode(id * 10 + 2, 1920, 1080, 60_003)];
        Head { make: make.into(), model: model.into(), serial: serial.into(), ..head(id, connector, &format!("{identity} ({connector})"), modes, id * 10) }
    }

    fn msi(connector: &str) -> Head { monitor(1, "Micro-Star Int'l Co., Ltd.", "MAG 274Q X24", "0x01010101", connector) }
    fn ugd(connector: &str) -> Head { monitor(2, "UGD", "Artist 12    ", "0x20210910", connector) }
    fn moved_cfg() -> Cfg { Cfg::load(MOVED_FILE).unwrap() }
    fn at(msi_connector: &str, ugd_connector: &str) -> Vec<Head> { vec![msi(msi_connector), ugd(ugd_connector)] }
    /// The issue's two sets of connectors and one neither was saved under.
    const BOOTS: [(&str, &str); 3] = [("DP-2", "HDMI-A-2"), ("DP-4", "HDMI-A-3"), ("DP-7", "HDMI-A-7")];

    #[test]
    fn connector_names_are_recognised() {
        for name in ["DP-2", "HDMI-A-1", "eDP-1", "DVI-D-1", "USB-C-1", "Virtual-1", "HEADLESS-1", "DP-12", "X11-1", "WL-1", "Unknown-3"] {
            assert!(is_connector_name(name), "{name}");
        }
        for name in ["", "DP", "DP-", "-1", "DP-2a", "DP 2", "2DP-1", "DP--2", "A-B-C-D-1", "DP-123456", "(null)", "0x01010101", "DP-2 via HDMI"] {
            assert!(!is_connector_name(name), "{name}");
        }
        assert_eq!(split_description(&format!("{MSI} (DP-2)")), Some((MSI, "DP-2")));
        assert_eq!(split_description("Foo (rev 2) (HDMI-A-1)"), Some(("Foo (rev 2)", "HDMI-A-1")), "the last parentheses");
        assert_eq!(split_description("(null) (null) (DP-2)"), Some(("(null) (null)", "DP-2")));
        for text in ["Headless output 1", "Foo (DP-2 via HDMI)", " (DP-2)", "(DP-2)", "Foo (DP-2) ", "Foo DP-2", "DP-2"] {
            assert_eq!(split_description(text), None, "{text}");
        }
    }

    #[test]
    fn only_a_monitor_with_a_make_or_model_has_an_identity() {
        assert_eq!(head_identity(&msi("DP-2")), Some(MSI));
        assert_eq!(head_identity(&ugd("HDMI-A-2")), Some(UGD), "spaces kept as they are");
        let mut no_edid = head(3, "DP-2", "(null) (null) (DP-2)", vec![], 0);
        assert_eq!(head_identity(&no_edid), None, "no EDID: wlroots prints (null) and sends no make or model");
        (no_edid.make, no_edid.model, no_edid.description) = ("Unknown".into(), "Unknown".into(), "Unknown Unknown (DP-2)".into());
        assert_eq!(head_identity(&no_edid), None);
        no_edid.model = "Panel".into();
        assert_eq!(head_identity(&no_edid), Some("Unknown Unknown"), "a model alone is enough");
        let mut other_name = msi("DP-2");
        other_name.name = "DP-3".into();
        assert_eq!(head_identity(&other_name), None, "the description must end with the head's own connector");
        let mut headless = msi("HEADLESS-1");
        headless.description = "Headless output 1".into();
        assert_eq!(head_identity(&headless), None);
        let no_identity = head(4, "DP-7", "(null) (null) (DP-7)", vec![], 0);
        assert!(!matches_moved(&no_identity, &[no_identity.clone()], "(null) (null) (DP-2)"), "monitors without EDID never follow");
    }

    #[test]
    fn the_same_monitor_on_another_connector_matches() {
        let heads = at("DP-7", "HDMI-A-7");
        assert!(matches_moved(&heads[0], &heads, &format!("{MSI} (DP-2)")));
        assert!(matches_moved(&heads[1], &heads, &format!("{UGD} (HDMI-A-3)")), "double spaces included");
        assert!(!matches_moved(&heads[1], &heads, "UGD Artist 12 0x20210910 (HDMI-A-3)"), "the identity is compared byte for byte");
        assert!(!matches_moved(&heads[0], &heads, &format!("{} (DP-2)", MSI.to_lowercase())), "and with case");
        assert!(!matches_moved(&heads[0], &heads, &format!("{MSI} (DP-7)")), "its own connector is an exact match, not a move");
        assert!(!matches_moved(&heads[0], &heads, &format!("{MSI} 2 (DP-2)")), "another serial is another monitor");
        assert!(!matches_moved(&heads[0], &heads, &format!("{MSI} (not a connector)")));
        assert!(!matches_moved(&heads[0], &heads, &format!("!{MSI} (DP-2)")), "a regular expression is not a description");
        for name in ["DP-2", "!^DP-2$", "MAG 274Q X24 0x01010101 (DP-2)", "(DP-2)"] {
            assert!(!matches_moved(&heads[0], &heads, name) && !matches(&heads[0], name), "{name}: short names, regexes and parts stay as they were");
        }
        assert!(matches(&heads[0], "mag 274q") && matches(&heads[0], "!^DP-7$") && matches(&heads[0], "DP-7"));
    }

    #[test]
    fn identical_monitors_never_follow_a_connector() {
        let twin = |id, connector: &str| monitor(id, "Dell Inc.", "U2720Q", "", connector);
        let saved = "Dell Inc. U2720Q  (DP-2)";
        let heads = vec![twin(1, "DP-1"), twin(2, "DP-3")];
        assert_eq!(heads[0].description, "Dell Inc. U2720Q  (DP-1)");
        assert!(!matches_moved(&heads[0], &heads, saved) && !matches_moved(&heads[1], &heads, saved), "two connected: neither");
        assert!(matches_moved(&heads[0], &heads[..1], saved), "one connected: it follows");
        let heads = vec![twin(1, "DP-1"), twin(2, "DP-2")];
        assert!(!matches_moved(&heads[0], &heads, saved), "the entry's own connector is connected with that description");
        assert!(matches(&heads[1], saved));
        let cfg = Cfg { scales: vec![(saved.into(), 2.0)], ..Default::default() };
        let mut heads = heads;
        desire(&cfg, &mut heads, None);
        assert_eq!((heads[0].des.scale, heads[1].des.scale), (256, 512), "today's behaviour for twins");
        // Another monitor on the entry's connector does not stop the move.
        let heads = vec![msi("DP-7"), monitor(3, "Other", "Screen", "", "DP-2")];
        assert!(matches_moved(&heads[0], &heads, &format!("{MSI} (DP-2)")));
    }

    #[test]
    fn an_entry_matching_otherwise_wins_over_a_moved_one() {
        let heads = at("DP-2", "HDMI-A-2");
        let (moved, exact) = (format!("{MSI} (DP-4)"), format!("{MSI} (DP-2)"));
        for scales in [vec![(moved.clone(), 2.0), (exact.clone(), 1.5)], vec![(exact.clone(), 1.5), (moved.clone(), 2.0)]] {
            assert_eq!(entry_for(&heads[0], &heads, &scales).map(|e| e.1), Some(1.5), "{scales:?}");
        }
        for name in ["DP-2", "274Q", "!^DP"] {
            let scales = vec![(moved.clone(), 2.0), (name.to_owned(), 1.25)];
            assert_eq!(entry_for(&heads[0], &heads, &scales).map(|e| e.1), Some(1.25), "{name}");
        }
        // Several moved entries: the last one in the file.
        let heads = at("DP-7", "HDMI-A-7");
        let scales = vec![(moved.clone(), 2.0), (exact.clone(), 1.5)];
        assert_eq!(entry_for(&heads[0], &heads, &scales).map(|e| e.1), Some(1.5));
        let scales = vec![(exact, 1.5), (moved, 2.0)];
        assert_eq!(entry_for(&heads[0], &heads, &scales).map(|e| e.1), Some(2.0));
        assert_eq!(entry_for(&heads[1], &heads, &scales), None, "another monitor's entries");
    }

    #[test]
    fn the_issue_file_keeps_the_mode_on_every_connector() {
        let cfg = moved_cfg();
        for (dp, hdmi) in BOOTS {
            let heads = at(dp, hdmi);
            assert_eq!(find_mode(&cfg, &heads[0], &heads), Some(11), "{dp}: 239.97 Hz, not the preferred 144 Hz");
            assert_eq!(find_mode(&cfg, &heads[1], &heads), Some(22), "{hdmi}: 1080p");
            assert_eq!(find_mode(&Cfg::default(), &heads[0], &heads), Some(10));
        }
        let mut cfg = cfg;
        cfg.modes[3].1 = ModeChoice::Size { width: 2560, height: 1440, mhz: Some(143_999) };
        let heads = at("DP-2", "HDMI-A-2");
        assert_eq!(find_mode(&cfg, &heads[0], &heads), Some(10), "on DP-2 the DP-2 entry is used");
        let heads = at("DP-4", "HDMI-A-3");
        assert_eq!(find_mode(&cfg, &heads[0], &heads), Some(11), "on DP-4 the DP-4 entry");
        let heads = at("DP-7", "HDMI-A-7");
        assert_eq!(find_mode(&cfg, &heads[0], &heads), Some(10), "elsewhere the last of the monitor's entries");
    }

    #[test]
    fn the_issue_file_keeps_adaptive_sync_off_and_labwc_gets_the_new_connector() {
        let cfg = moved_cfg();
        for (dp, hdmi) in BOOTS {
            let heads = at(dp, hdmi);
            assert_eq!((vrr_mode(&cfg, &heads[0], &heads), vrr_mode(&cfg, &heads[1], &heads)), (VrrMode::Off, VrrMode::Auto), "{dp}");
            let modes: Vec<_> = vrr_modes(&cfg, &heads).into_iter().collect();
            assert_eq!(modes, [(dp.to_owned(), VrrMode::Off), (hdmi.to_owned(), VrrMode::Auto)], "the adaptive sync block names {dp}");
        }
        let mut cfg = cfg;
        cfg.vrr_always = vec![format!("{MSI} (DP-4)")];
        let mode = |cfg: &Cfg, dp| { let heads = at(dp, "HDMI-A-2"); vrr_mode(cfg, &heads[0], &heads) };
        assert_eq!((mode(&cfg, "DP-2"), mode(&cfg, "DP-4"), mode(&cfg, "DP-7")), (VrrMode::Off, VrrMode::Always, VrrMode::Off),
                   "an entry for the connector wins over a moved one in the other list; between moved ones Off wins");
        cfg.vrr_off.clear();
        assert_eq!((mode(&cfg, "DP-2"), mode(&cfg, "DP-7")), (VrrMode::Always, VrrMode::Always));
        cfg.vrr_off = vec!["HDMI".into()];
        assert_eq!(mode(&cfg, "DP-7"), VrrMode::Always, "an entry for another monitor changes nothing");
    }

    #[test]
    fn the_issue_file_keeps_the_order_on_every_connector() {
        let cfg = moved_cfg();
        for (dp, hdmi) in BOOTS {
            let heads = at(dp, hdmi);
            assert_eq!(ordered(&cfg, &heads), [1, 0], "{dp}: the tablet first");
            assert_eq!(ordered(&Cfg::default(), &heads), [0, 1]);
        }
        let heads = at("DP-7", "HDMI-A-7");
        let cfg = Cfg { order: vec![format!("{UGD} (HDMI-A-2)"), "DP-7".into()], ..Default::default() };
        assert_eq!(ordered(&cfg, &heads), [1, 0]);
        let cfg = Cfg { order: vec![format!("{MSI} (DP-2)"), "HDMI-A-7".into(), format!("{MSI} (DP-4)")], ..Default::default() };
        assert_eq!(ordered(&cfg, &heads), [0, 1]);
        let cfg = Cfg { order: vec![format!("{UGD} (HDMI-A-2)"), "MAG 274Q".into()], ..Default::default() };
        let heads = at("DP-7", "HDMI-A-7");
        assert_eq!(ordered(&cfg, &heads), [1, 0]);
        let cfg = Cfg { order: vec![format!("{MSI} (DP-2)"), "Artist".into()], ..Default::default() };
        assert_eq!(ordered(&cfg, &heads), [0, 1], "a partial name and a moved entry each place their own monitor");
    }

    #[test]
    fn scale_rotation_and_position_follow_the_monitor() {
        let mut cfg = moved_cfg();
        cfg.scales = vec![(format!("{MSI} (DP-4)"), 1.25), (format!("{MSI} (DP-2)"), 1.5)];
        cfg.transforms = vec![(format!("{UGD} (HDMI-A-3)"), 1), (format!("{UGD} (HDMI-A-2)"), 3)];
        let expect = [("DP-2", "HDMI-A-2", 384, 3), ("DP-4", "HDMI-A-3", 320, 1), ("DP-7", "HDMI-A-7", 384, 3)];
        for (dp, hdmi, scale, transform) in expect {
            let mut heads = at(dp, hdmi);
            desire(&cfg, &mut heads, None);
            assert_eq!((heads[0].des.scale, heads[1].des.transform), (scale, transform), "{dp}");
            assert_eq!((heads[1].des.x, heads[0].des.x), (0, 1080), "{dp}: the turned tablet sits left of the monitor");
            assert_eq!(heads[0].des.mode, Some(11));
        }
    }

    #[test]
    fn disabled_and_its_conditions_follow_the_monitor() {
        let off = |name: String| Cfg { disabled: vec![(name, vec![])], ..Default::default() };
        let cfg = off(format!("{UGD} (HDMI-A-3)"));
        for (dp, hdmi) in BOOTS {
            let heads = at(dp, hdmi);
            assert!(disabled_applies(&cfg, &heads[1], &heads, None) && !disabled_applies(&cfg, &heads[0], &heads, None), "{hdmi}");
        }
        // The connector's own conditional entry wins; its condition does not hold.
        let mut cfg = cfg;
        cfg.disabled.push((format!("{UGD} (HDMI-A-2)"), vec![Condition { unplugged: vec![format!("{MSI} (DP-2)")], ..Default::default() }]));
        let heads = at("DP-2", "HDMI-A-2");
        assert!(!disabled_applies(&cfg, &heads[1], &heads, None), "the HDMI-A-2 entry, MSI plugged");
        let heads = at("DP-4", "HDMI-A-3");
        assert!(disabled_applies(&cfg, &heads[1], &heads, None), "the HDMI-A-3 entry");
        let heads = at("DP-7", "HDMI-A-7");
        assert!(!disabled_applies(&cfg, &heads[1], &heads, None), "the last entry; the MSI on DP-7 counts as plugged");
        assert!(disabled_applies(&cfg, &heads[1], &heads[1..], None), "and unplugged when it is not connected");
        let cfg = Cfg { disabled: vec![("HDMI-A-7".into(), vec![Condition { plugged: vec![format!("{MSI} (DP-2)")], ..Default::default() }])], ..Default::default() };
        assert!(disabled_applies(&cfg, &heads[1], &heads, None), "PLUGGED names the monitor under its old connector");
        // A manual choice against the moved conditional rule is a manual choice.
        let cfg = Cfg { disabled: vec![(format!("{UGD} (HDMI-A-2)"), vec![Condition { plugged: vec!["DP-7".into()], ..Default::default() }])], ..Default::default() };
        let mut heads = at("DP-7", "HDMI-A-7");
        desire(&cfg, &mut heads, None);
        assert!(!heads[1].des.enabled);
        heads[1].cur = heads[1].des;
        let mut requested = vec![(heads[1].description.clone(), Vec::new())];
        apply_disabled_overrides(&cfg, &mut heads, "CFG_DEL", &mut requested);
        assert_eq!((heads[1].override_enabled, requested.len()), (Some(true), 0));
        let mut requested = cfg.disabled.clone();
        apply_disabled_overrides(&cfg, &mut heads, "CFG_DEL", &mut requested);
        assert_eq!(requested, cfg.disabled, "naming the rule itself is about the rule");
    }
}
