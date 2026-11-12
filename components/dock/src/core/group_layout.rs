//! Ordered dock groups, independent of GTK and monitor orientation
//! (port of group_layout.py).
//!
//! API: [`POSITIONS`], [`APP_DOCK_GROUPS`], [`LAYOUT_GROUPS`] (`(id, English
//! label)`; translate labels with `tr!` in UIs), [`default_layout`],
//! [`copy_layouts`], [`default_app_dock_layout`], [`validate_app_dock_layout`],
//! [`validate_layouts`]. A [`Layout`] maps `start`/`center`/`end` to group
//! ids; [`Layouts`] maps `dock`/`bar` to a layout.

use serde_json::Value;

use super::config::LEGACY_TRAY_GROUP;
use super::ordered::OrderedMap;

pub type Layout = OrderedMap<Vec<String>>;
pub type Layouts = OrderedMap<Layout>;

pub const POSITIONS: &[(&str, &str)] = &[("start", "Top / Left"), ("center", "Center"), ("end", "Bottom / Right")];
pub const APP_DOCK_GROUPS: &[&str] = &["app_menu", "pinned", "running", "files"];
pub const LAYOUT_GROUPS: &[(&str, &str)] = &[
    ("weather", "Weather"),
    ("workspaces", "Workspaces"),
    ("files", "Home, Settings and Trash"),
    ("app_menu", "Application menu"),
    ("pinned", "Pinned applications"),
    ("running", "Open application windows"),
    ("activities", "Activities"),
    ("focused_title", "Focused window title"),
    ("clock", "Time and date"),
    ("media", "Media"),
    ("service_apps", "System tray"),
    ("session", "Session manager"),
];

fn zones(values: [&[&str]; 3]) -> Layout {
    POSITIONS
        .iter()
        .zip(values)
        .map(|((position, _), names)| (*position, names.iter().map(|n| n.to_string()).collect()))
        .collect()
}

pub fn default_layout(mode: &str, display: &str) -> Layout {
    if mode == "bar" {
        if display == "title" {
            zones([
                &["app_menu", "weather", "workspaces", "files", "pinned", "running", "activities", "focused_title"],
                &["clock"],
                &["media", "service_apps", "session"],
            ])
        } else {
            zones([
                &["weather", "workspaces"],
                &["app_menu", "pinned", "running", "activities", "focused_title", "files"],
                &["media", "service_apps", "clock", "session"],
            ])
        }
    } else if display == "title" {
        zones([
            &[],
            &[
                "app_menu", "workspaces", "files", "pinned", "running", "activities", "focused_title", "media",
                "service_apps", "weather", "clock", "session",
            ],
            &[],
        ])
    } else {
        zones([
            &[],
            &[
                "app_menu", "pinned", "running", "activities", "focused_title", "files", "workspaces", "media",
                "service_apps", "weather", "clock", "session",
            ],
            &[],
        ])
    }
}

pub fn copy_layouts(layouts: &Layouts) -> Layouts {
    layouts.clone()
}

pub fn default_app_dock_layout(expanded: bool) -> Layout {
    if expanded {
        zones([&APP_DOCK_GROUPS[..3], &[], &["files"]])
    } else {
        zones([&[], APP_DOCK_GROUPS, &[]])
    }
}

fn has_positions(map: &serde_json::Map<String, Value>) -> bool {
    map.len() == POSITIONS.len() && POSITIONS.iter().all(|(p, _)| map.contains_key(*p))
}

fn names_of(value: &Value) -> Vec<String> {
    value.as_array().map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// `validate_app_dock_layout(layout, expanded)`; `{}` keeps the automatic defaults.
pub fn validate_app_dock_layout(layout: &Value, expanded: bool) -> Result<Layout, String> {
    let Some(map) = layout.as_object() else {
        return Err("App dock group positions must be an object.".into());
    };
    if map.is_empty() {
        return Ok(Layout::new());
    }
    if !has_positions(map) {
        return Err("App dock layout needs Top/Left, Center and Bottom/Right positions.".into());
    }
    let mut seen: Vec<String> = Vec::new();
    for names in map.values() {
        let Some(names) = names.as_array() else {
            return Err("App dock positions must contain ordered group names.".into());
        };
        for name in names {
            match name.as_str() {
                Some(name) if APP_DOCK_GROUPS.contains(&name) && !seen.iter().any(|s| s == name) => seen.push(name.into()),
                _ => return Err("Each supported app dock group may appear only once.".into()),
            }
        }
    }
    let mut result: Layout = POSITIONS.iter().map(|(p, _)| (*p, names_of(&map[*p]))).collect();
    for (position, names) in default_app_dock_layout(expanded) {
        let target = result.get_mut(&position).expect("position");
        target.extend(names.into_iter().filter(|n| !seen.contains(n)));
    }
    Ok(result)
}

/// `validate_layouts(layouts, display)`: normalizes position order, inserts
/// Media before the tray in older custom layouts, merges the old
/// `system_tray` group into `service_apps` (the system tray keeps the
/// `service_apps` place; a layout naming only `system_tray` puts it there) and
/// appends groups missing from a layout at their default position.
pub fn validate_layouts(layouts: &Value, display: &str) -> Result<Layouts, String> {
    let map = match layouts.as_object() {
        Some(map) if map.keys().all(|k| k == "dock" || k == "bar") => map,
        _ => return Err("Group layouts must describe Dock or Bar mode.".into()),
    };
    let mut result = Layouts::new();
    for (mode, layout) in map {
        let layout = match layout.as_object() {
            Some(layout) if has_positions(layout) => layout,
            _ => return Err("Each group layout needs Top/Left, Center and Bottom/Right positions.".into()),
        };
        let mut seen: Vec<String> = Vec::new();
        for names in layout.values() {
            let Some(names) = names.as_array() else {
                return Err("Layout positions must contain ordered group names.".into());
            };
            for name in names {
                match name.as_str() {
                    Some(name) if (name == LEGACY_TRAY_GROUP || LAYOUT_GROUPS.iter().any(|(g, _)| *g == name))
                        && !seen.iter().any(|s| s == name) => {
                        seen.push(name.into())
                    }
                    _ => return Err("Each supported group may appear only once in a layout.".into()),
                }
            }
        }
        let mut normalized: Layout = POSITIONS.iter().map(|(p, _)| (*p, names_of(&layout[*p]))).collect();
        if !seen.iter().any(|s| s == "media") {
            let tray = if seen.iter().any(|s| s == LEGACY_TRAY_GROUP) { LEGACY_TRAY_GROUP } else { "service_apps" };
            for (_, names) in normalized.iter_mut() {
                if let Some(index) = names.iter().position(|n| n == tray) {
                    names.insert(index, "media".into());
                    seen.push("media".into());
                    break;
                }
            }
        }
        if seen.iter().any(|s| s == LEGACY_TRAY_GROUP) {
            let merged = seen.iter().any(|s| s == "service_apps");
            for (_, names) in normalized.iter_mut() {
                if merged {
                    names.retain(|n| n != LEGACY_TRAY_GROUP);
                } else if let Some(name) = names.iter_mut().find(|n| *n == LEGACY_TRAY_GROUP) {
                    *name = "service_apps".into();
                }
            }
            seen.retain(|s| s != LEGACY_TRAY_GROUP);
            if !merged {
                seen.push("service_apps".into());
            }
        }
        for (position, names) in default_layout(mode, display) {
            let target = normalized.get_mut(&position).expect("position");
            target.extend(names.into_iter().filter(|n| !seen.contains(n)));
        }
        result.insert(mode.clone(), normalized);
    }
    Ok(result)
}

/// JSON form of a layout (`{"start": [...], ...}`) in its own key order.
pub fn layout_value(layout: &Layout) -> Value {
    Value::Object(layout.iter().map(|(k, v)| (k.to_owned(), Value::from(v.clone()))).collect())
}

/// JSON form of `group_layouts`.
pub fn layouts_value(layouts: &Layouts) -> Value {
    Value::Object(layouts.iter().map(|(k, v)| (k.to_owned(), layout_value(v))).collect())
}

#[cfg(test)]
mod tests {
    //! Port of test_group_positions.py (the pure layout part; the GUI
    //! separator checks of test_group_layout.py belong to the dock UI).
    use super::*;
    use crate::core::config::{ConfigStore, DockConfig};
    use serde_json::json;

    fn all_names(layout: &Layout) -> Vec<String> {
        let mut names: Vec<String> = layout.values().flatten().cloned().collect();
        names.sort();
        names
    }

    fn layout_names() -> Vec<String> {
        let mut names: Vec<String> = LAYOUT_GROUPS.iter().map(|(n, _)| n.to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn defaults_include_every_group_once_in_each_mode() {
        for mode in ["dock", "bar"] {
            for display in ["title", "icons"] {
                let layout = default_layout(mode, display);
                assert_eq!(layout.keys().collect::<Vec<_>>(), ["start", "center", "end"]);
                assert_eq!(all_names(&layout), layout_names());
            }
        }
        assert!(DockConfig::from_value(&json!({})).unwrap().group_layouts.is_empty());
    }

    #[test]
    fn round_trip_and_monitor_clones_do_not_share_layout_lists() {
        let mut layouts: Layouts = ["dock", "bar"].iter().map(|m| (*m, default_layout(m, "title"))).collect();
        layouts["dock"]["center"].retain(|n| n != "weather");
        layouts["dock"]["end"].push("weather".into());
        let dir = crate::core::testutil::TempDir::new();
        let mut store = ConfigStore::new(dir.join("config.json"));
        store.save(&DockConfig { group_layouts: layouts.clone(), ..Default::default() }).unwrap();
        let saved = store.load(None, None);
        assert_eq!(saved.group_layouts, layouts);
        let mut clone = saved.for_monitor("DP-1");
        clone.group_layouts["dock"]["end"].clear();
        assert_eq!(saved.group_layouts, layouts);
    }

    #[test]
    fn missing_groups_gain_defaults_without_reordering_saved_groups() {
        let config = DockConfig::from_value(&json!({"group_layouts": {"bar": {"start": ["session", "clock"], "center": [], "end": []}}})).unwrap();
        assert_eq!(config.group_layouts["bar"]["start"][..2], ["session".to_string(), "clock".to_string()]);
        assert_eq!(all_names(&config.group_layouts["bar"]), layout_names());
    }

    #[test]
    fn media_migrates_out_of_default_apps_and_before_the_custom_tray() {
        let config = DockConfig::from_value(&json!({"always_visible_apps": ["media", "audio"], "group_layouts": {"bar": {"start": ["system_tray", "clock"], "center": [], "end": ["session"]}}})).unwrap();
        assert_eq!(config.always_visible_apps, ["audio"]);
        // The old tray's place becomes the system tray's.
        assert_eq!(config.group_layouts["bar"]["start"][..3], ["media".to_string(), "service_apps".to_string(), "clock".to_string()]);
        assert!(config.is_group_visible("media"));
        let config = DockConfig::from_value(&json!({"group_layouts": {"bar": {"start": ["service_apps", "clock"], "center": [], "end": []}}})).unwrap();
        assert_eq!(config.group_layouts["bar"]["start"][..3], ["media".to_string(), "service_apps".to_string(), "clock".to_string()]);
    }

    #[test]
    fn the_old_tray_group_merges_into_the_system_tray() {
        // Both named: the system tray stays where the applets were.
        let config = DockConfig::from_value(&json!({"group_layouts": {
            "bar": {"start": ["system_tray"], "center": ["clock"], "end": ["media", "service_apps", "session"]},
            "dock": {"start": [], "center": ["pinned", "media", "system_tray", "service_apps", "clock"], "end": []}}})).unwrap();
        assert!(!config.group_layouts["bar"]["start"].iter().any(|n| n == "service_apps" || n == "system_tray"));
        assert_eq!(config.group_layouts["bar"]["end"][..3], ["media".to_string(), "service_apps".to_string(), "session".to_string()]);
        assert_eq!(config.group_layouts["dock"]["center"][..4],
                   ["pinned".to_string(), "media".to_string(), "service_apps".to_string(), "clock".to_string()]);
        for layout in config.group_layouts.values() {
            assert_eq!(all_names(layout), layout_names());
        }
        // Saving never writes the old id.
        let saved = crate::core::config::save_bytes(&config);
        assert!(!saved.contains("system_tray"), "{saved}");
        // The old id twice is still rejected.
        assert!(validate_layouts(&json!({"bar": {"start": ["system_tray"], "center": ["system_tray"], "end": []}}), "icons").is_err());
    }

    #[test]
    fn invalid_layouts_are_rejected() {
        for layout in [
            json!(null), json!([]), json!({"unknown": {}}), json!({"bar": {}}),
            json!({"bar": {"start": "clock", "center": [], "end": []}}),
            json!({"bar": {"start": ["clock"], "center": ["clock"], "end": []}}),
            json!({"bar": {"start": [42], "center": [], "end": []}}),
            json!({"bar": {"start": ["nonsense"], "center": [], "end": []}}),
            json!({"bar": {"start": [], "center": [], "end": [], "extra": []}}),
        ] {
            assert!(validate_layouts(&layout, "icons").is_err(), "{layout}");
        }
    }

    #[test]
    fn positions_are_normalized_into_start_center_end_order() {
        let result = validate_layouts(&json!({"dock": {"end": ["clock"], "start": ["weather"], "center": []}}), "icons").unwrap();
        assert_eq!(result["dock"].keys().collect::<Vec<_>>(), ["start", "center", "end"]);
        assert_eq!(result["dock"]["start"], ["weather"]);
        assert_eq!(result["dock"]["end"], ["clock"]);
    }

    #[test]
    fn app_dock_layouts() {
        assert!(validate_app_dock_layout(&json!({}), true).unwrap().is_empty());
        assert_eq!(default_app_dock_layout(true)["start"], ["app_menu", "pinned", "running"]);
        assert_eq!(default_app_dock_layout(true)["end"], ["files"]);
        assert_eq!(default_app_dock_layout(false)["center"], APP_DOCK_GROUPS);
        let layout = validate_app_dock_layout(&json!({"end": ["pinned"], "start": [], "center": []}), false).unwrap();
        assert_eq!(layout.keys().collect::<Vec<_>>(), ["start", "center", "end"]);
        assert_eq!(layout["center"], ["app_menu", "running", "files"]);
        assert_eq!(layout["end"], ["pinned"]);
        for (value, message) in [
            (json!([]), "App dock group positions must be an object."),
            (json!({"start": []}), "App dock layout needs Top/Left, Center and Bottom/Right positions."),
            (json!({"start": 1, "center": [], "end": []}), "App dock positions must contain ordered group names."),
            (json!({"start": ["clock"], "center": [], "end": []}), "Each supported app dock group may appear only once."),
            (json!({"start": ["files"], "center": ["files"], "end": []}), "Each supported app dock group may appear only once."),
        ] {
            assert_eq!(validate_app_dock_layout(&value, true).unwrap_err(), message);
        }
    }
}
