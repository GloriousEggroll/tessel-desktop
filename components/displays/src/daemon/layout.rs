//! Deciding what each monitor should be: the part of the daemon with no
//! Wayland in it. Compatible with the way-displays layout rules (MIT,
//! Copyright 2021 Alexander Courtis), so saved layouts come out the same.

use super::config::{Align, Arrange, Cfg, Condition, LidCondition, ModeChoice, Round};

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
    pub vrr_failed: bool,
    pub reapply: bool,
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

fn condition_met(condition: &Condition, heads: &[Head], lid: Lid, ignore_closed: bool) -> bool {
    condition.plugged.iter().all(|name| heads.iter().any(|head| matches(head, name)))
        && !condition.unplugged.iter().any(|name| heads.iter().any(|head| matches(head, name)))
        && match condition.lid {
            Some(LidCondition::Closed) => lid == Some(true) && !ignore_closed,
            Some(LidCondition::Open) => lid == Some(false),
            Some(LidCondition::NotPresent) => lid.is_none(),
            None => true,
        }
}

/// Whether DISABLED turns this head off. A closed lid is ignored when the
/// laptop panel is the only display, so it can still sleep and wake smoothly.
pub fn disabled_applies(cfg: &Cfg, head: &Head, heads: &[Head], lid: Lid) -> bool {
    cfg.disabled.iter().any(|(name, conditions)| {
        matches(head, name)
            && (conditions.is_empty() || conditions.iter().any(|condition| condition_met(condition, heads, lid, heads.len() == 1)))
    })
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

fn auto_scale(cfg: &Cfg, head: &Head, mode: &Mode) -> i32 {
    if head.width_mm == 0 || head.height_mm == 0 {
        return fixed_scale(cfg, 1.0);
    }
    let horizontal = f64::from(mode.width) / f64::from(head.width_mm) * 25.4;
    let vertical = f64::from(mode.height) / f64::from(head.height_mm) * 25.4;
    let dpi = (horizontal + vertical) / 2.0;
    let base = cfg.round_base as i64;
    let dpi_base = i64::from(cfg.dpi);
    let dpi_min = (dpi_base / base * (cfg.auto_min * base as f64).ceil() as i64).max(dpi_base / base) as f64;
    let dpi_max = (dpi_base / base * (cfg.auto_max * base as f64) as i64) as f64;
    let mut clamped = dpi;
    if clamped < dpi_min {
        clamped = dpi_min;
    } else if dpi_min <= dpi_max && clamped > dpi_max {
        clamped = dpi_max;
    }
    fixed_scale(cfg, clamped / dpi_base as f64)
}

fn greatest_first(modes: &[Mode]) -> Vec<Mode> {
    let mut sorted = modes.to_vec();
    sorted.sort_by_key(|mode| std::cmp::Reverse((mode.width, mode.height, mode.mhz)));
    sorted
}

/// The mode a head should use: its MODE setting if a mode satisfies it, else
/// the preferred mode, else the largest. `None` when it has no modes at all.
pub fn find_mode(cfg: &Cfg, head: &Head) -> Option<u32> {
    let preferred = head.preferred.and_then(|id| head.modes.iter().find(|mode| mode.id == id));
    let sorted = greatest_first(&head.modes);
    let chosen = cfg.modes.iter().find(|(name, _)| matches(head, name)).and_then(|(_, choice)| match choice {
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
    for head in heads {
        let conditional = cfg.disabled.iter().any(|(name, conditions)| !conditions.is_empty() && matches(head, name));
        if !conditional {
            continue;
        }
        let named: Vec<String> = requested.iter().filter(|(name, _)| !is_rule(name) && matches(head, name)).map(|(name, _)| name.clone()).collect();
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
            match find_mode(cfg, head) {
                Some(mode) => head.des.mode = Some(mode),
                None => head.des.enabled = false,
            }
        }
        if head.des.enabled {
            let mode = head.des.mode.and_then(|id| head.modes.iter().find(|mode| mode.id == id)).copied();
            head.des.scale = if !cfg.scaling {
                fixed_scale(cfg, 1.0)
            } else if let Some((_, scale)) = cfg.scales.iter().find(|(name, _)| matches(head, name)) {
                fixed_scale(cfg, *scale)
            } else if let (true, Some(mode)) = (cfg.auto_scale, mode) {
                auto_scale(cfg, head, &mode)
            } else {
                fixed_scale(cfg, 1.0)
            };
            head.des.transform = cfg.transforms.iter().find(|(name, _)| matches(head, name)).map_or(0, |(_, transform)| *transform);
            if !head.vrr_failed {
                head.des.vrr = !cfg.vrr_off.iter().any(|name| matches(head, name));
            }
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
/// regular expressions, then partial names; anything unmatched comes last.
pub fn ordered(cfg: &Cfg, heads: &[Head]) -> Vec<usize> {
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); cfg.order.len()];
    let mut left: Vec<usize> = (0..heads.len()).collect();
    for test in [matches_exact as fn(&Head, &str) -> bool, matches_regex, matches_fuzzy] {
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

/// The next change to send. Reapplying, a mode change and a VRR change each
/// travel alone, because a compositor can refuse those one by one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    Reapply(usize),
    Mode(usize),
    Vrr(usize),
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
    if let Some(&index) = changing.iter().find(|&&i| heads[i].des.vrr != heads[i].cur.vrr) {
        return Plan::Vrr(index);
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
        // 3840 wide on 600 mm and 2160 on 340 mm: about 162 dpi, so 1.625 (162/96 = 1.69 → 1.75).
        let heads = pair();
        let mut cfg = Cfg { auto_scale: true, ..Default::default() };
        let mode = heads[0].modes[1];
        assert_eq!(auto_scale(&cfg, &heads[0], &mode), 448);
        cfg.auto_max = 1.5;
        assert_eq!(auto_scale(&cfg, &heads[0], &mode), 384);
        cfg.auto_max = -1.0;
        cfg.auto_min = 2.0;
        assert_eq!(auto_scale(&cfg, &heads[0], &mode), 512);
        let mut unknown = heads[0].clone();
        unknown.width_mm = 0;
        assert_eq!(auto_scale(&cfg, &unknown, &mode), 256, "no physical size means scale 1");
    }

    #[test]
    fn a_saved_mode_wins_then_the_preferred_one() {
        let heads = pair();
        let mut cfg = Cfg::default();
        assert_eq!(find_mode(&cfg, &heads[1]), Some(20), "preferred");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: Some(60000) })];
        assert_eq!(find_mode(&cfg, &heads[1]), Some(21), "exact refresh");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: Some(239_900) })];
        assert_eq!(find_mode(&cfg, &heads[1]), Some(20), "same rounded refresh");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: None })];
        assert_eq!(find_mode(&cfg, &heads[1]), Some(20), "highest refresh at that size");
        cfg.modes = vec![("DP-2".into(), ModeChoice::Size { width: 1, height: 1, mhz: None })];
        assert_eq!(find_mode(&cfg, &heads[1]), Some(20), "a missing mode falls back to preferred");
        cfg.modes = vec![("HDMI".into(), ModeChoice::Max)];
        assert_eq!(find_mode(&cfg, &heads[0]), Some(11));
        let mut none = heads[0].clone();
        none.preferred = None;
        assert_eq!(find_mode(&Cfg::default(), &none), Some(11), "largest without a preferred mode");
        none.modes.clear();
        assert_eq!(find_mode(&Cfg::default(), &none), None);
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
    fn vrr_is_on_unless_turned_off_or_refused() {
        let mut heads = pair();
        let cfg = Cfg { vrr_off: vec!["HDMI".into()], ..Default::default() };
        desire(&cfg, &mut heads, None);
        assert!(!heads[0].des.vrr && heads[1].des.vrr);
        heads[1].vrr_failed = true;
        heads[1].cur.vrr = false;
        desire(&cfg, &mut heads, None);
        assert!(!heads[1].des.vrr, "a refused change is not asked for again");
    }

    #[test]
    fn modes_and_vrr_change_one_at_a_time_before_the_rest() {
        let mut heads = pair();
        let cfg = Cfg { modes: vec![("DP-2".into(), ModeChoice::Size { width: 3840, height: 2160, mhz: Some(60000) })], scales: vec![("HDMI".into(), 2.0)], ..Default::default() };
        desire(&cfg, &mut heads, None);
        assert_eq!(plan(&heads), Plan::Mode(1));
        heads[1].cur.mode = heads[1].des.mode;
        assert_eq!(plan(&heads), Plan::Vrr(0));
        heads[0].cur.vrr = heads[0].des.vrr;
        assert_eq!(plan(&heads), Plan::Vrr(1));
        heads[1].cur.vrr = heads[1].des.vrr;
        match plan(&heads) {
            Plan::All(indices) => assert!(indices.contains(&0)),
            other => panic!("{other:?}"),
        }
        for head in &mut heads { head.cur = head.des; }
        assert_eq!(plan(&heads), Plan::Nothing);
        heads[0].reapply = true;
        assert_eq!(plan(&heads), Plan::Reapply(0));
    }
}
