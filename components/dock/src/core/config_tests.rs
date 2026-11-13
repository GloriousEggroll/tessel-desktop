//! Port of test_dock_config.py.

use serde_json::{json, Value};

use super::config::*;
use super::ordered::OrderedMap;
use super::testutil::TempDir;

fn from(value: Value) -> DockConfig {
    DockConfig::from_value(&value).unwrap()
}

fn rejects(value: Value) {
    assert!(DockConfig::from_value(&value).is_err(), "accepted {value}");
}

fn store(dir: &TempDir) -> ConfigStore {
    ConfigStore::new(dir.join("config.json"))
}

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

struct FakeCatalog(Vec<&'static str>);

impl PinCatalog for FakeCatalog {
    fn get_id(&self, id: &str) -> Option<String> {
        self.0.iter().find(|x| **x == id).map(|x| x.to_string())
    }
    fn resolve_id(&self, value: &str) -> Option<String> {
        self.get_id(value).or_else(|| self.get_id(&format!("{value}.desktop")))
    }
}

#[test]
fn bar_border_defaults_validation_and_persistence() {
    let config = from(json!({}));
    assert!(!config.bar_border_enabled);
    assert_eq!(config.bar_border_sides, s(&["top", "bottom", "left", "right"]));
    let dir = TempDir::new();
    let mut store = ConfigStore::new(dir.join("dock.json"));
    store
        .save(&DockConfig {
            bar_border_enabled: true,
            bar_border_sides: s(&["right", "top", "right"]),
            bar_border_color: "#AaBbCc".into(),
            ..Default::default()
        })
        .unwrap();
    let saved = store.load(None, None);
    assert!(saved.bar_border_enabled);
    assert_eq!(saved.bar_border_sides, s(&["top", "right"]));
    assert_eq!(saved.bar_border_color, "#aabbcc");
    saved.for_monitor("DP-1").bar_border_sides.clear();
    assert_eq!(saved.bar_border_sides, s(&["top", "right"]));
    for (field, bad) in [
        ("bar_border_enabled", json!(1)),
        ("bar_border_sides", json!("top")),
        ("bar_border_sides", json!([null])),
        ("bar_border_sides", json!(["start"])),
        ("bar_border_sides", json!([{}])),
        ("bar_border_color", json!("red")),
        ("bar_border_color", json!(null)),
        ("bar_border_color", json!("#abcdef;")),
    ] {
        rejects(json!({ field: bad }));
    }
}

#[test]
fn default_app_visibility_defaults_empty_and_custom_round_trip() {
    let defaults = s(&["idle_inhibitor", "capslock", "audio", "brightness", "showdesktop"]);
    assert_eq!(from(json!({})).always_visible_apps, defaults);
    for selection in [vec![], s(&["network", "screen_record"]), defaults.clone()] {
        let dir = TempDir::new();
        let mut store = store(&dir);
        store.save(&DockConfig { always_visible_apps: selection.clone(), ..Default::default() }).unwrap();
        let saved = store.load(None, None);
        assert_eq!(saved.always_visible_apps, selection);
        saved.for_monitor("DP-1").always_visible_apps.push("power".into());
        assert_eq!(saved.always_visible_apps, selection);
    }
    for selection in [json!(null), json!(true), json!("audio"), json!(["unknown"]), json!([null]), json!([42])] {
        rejects(json!({ "always_visible_apps": selection }));
    }
    // Media moved to its own group and is stripped on load.
    assert_eq!(from(json!({"always_visible_apps": ["media", "audio"]})).always_visible_apps, s(&["audio"]));
}

#[test]
fn always_show_tray_persists_and_monitor_choices_do_not_alias() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    store.save(&DockConfig { always_show_tray: s(&["id:Nextcloud", "id:Steam", "id:Nextcloud"]), ..Default::default() }).unwrap();
    let config = store.load(None, None);
    assert_eq!(config.always_show_tray, s(&["id:Nextcloud", "id:Steam"]));
    for value in [json!("app"), json!(null), json!(true), json!([null]), json!([""]), json!(["id:a\n"]), json!(["a".repeat(513)])] {
        rejects(json!({ "always_show_tray": value }));
    }
    rejects(json!({ "always_show_tray": vec!["x"; 513] }));
}

#[test]
fn application_display_defaults_and_preserves_pins_when_changed() {
    assert_eq!(from(json!({})).application_display, "icons");
    let dir = TempDir::new();
    let mut store = store(&dir);
    store.save(&DockConfig { pins: s(&["browser.desktop"]), application_display: "title".into(), ..Default::default() }).unwrap();
    let restored = store.load(None, None);
    assert_eq!(restored.application_display, "title");
    assert_eq!(restored.pins, s(&["browser.desktop"]));
    assert_eq!(restored.for_monitor("DP-1").application_display, "title");
    for value in [json!("windows"), json!("TITLE"), json!(""), json!(null), json!(1), json!(true), json!([]), json!({})] {
        rejects(json!({ "application_display": value }));
    }
}

#[test]
fn cairo_import_preserves_order_and_resolves_installed_origin_choices_only() {
    let catalog = FakeCatalog(vec!["net.example.mail.desktop", "browser.desktop"]);
    let dir = TempDir::new();
    let root = dir.join("launchers");
    std::fs::create_dir(&root).unwrap();
    let files = [
        ("mail.desktop", "Type=Application\nOrder=9\nOrigin=missing.desktop;mail.desktop\n"),
        ("browser.desktop", "Type=Application\nOrder=2\nOrigin=/usr/share/applications/browser.desktop\n"),
        ("duplicate.desktop", "Type=Application\nOrder=11\nOrigin=browser.desktop\n"),
        ("separator.desktop", "Type=Separator\nOrder=1\nOrigin=mail.desktop\n"),
        ("external.desktop", "Type=Application\nOrder=0\nExec=touch MUST_NOT_RUN\n"),
        ("subdock.desktop", "Type=Application\nOrder=0\nContainer=OtherDock\nOrigin=mail.desktop\n"),
    ];
    for (name, content) in files {
        std::fs::write(root.join(name), format!("[Desktop Entry]\n{content}")).unwrap();
    }
    // The Python test's catalog resolves "mail.desktop" through the basename index.
    struct Mail(FakeCatalog);
    impl PinCatalog for Mail {
        fn get_id(&self, id: &str) -> Option<String> {
            self.0.get_id(id)
        }
        fn resolve_id(&self, value: &str) -> Option<String> {
            (value == "mail.desktop").then(|| "net.example.mail.desktop".into()).or_else(|| self.0.resolve_id(value))
        }
    }
    assert_eq!(cairo_pins(&Mail(catalog), Some(dir.path())), s(&["browser.desktop", "net.example.mail.desktop"]));
    assert!(!dir.join("MUST_NOT_RUN").exists());
}

#[test]
fn cairo_parser_skips_broken_files_and_handles_numbered_names() {
    let catalog = FakeCatalog(vec!["app.desktop", "other.desktop", "third.desktop"]);
    let dir = TempDir::new();
    let root = dir.join("launchers");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("01app.desktop"), "[Desktop Entry]\nType = Application\nOrder: 3\n").unwrap();
    std::fs::write(root.join("bad.desktop"), "Type=Application\n").unwrap();
    std::fs::write(root.join("noval.desktop"), "[Desktop Entry]\nType=Application\nbroken line\nOrigin=other.desktop\n").unwrap();
    std::fs::write(root.join("third.desktop"), "[Desktop Entry]\n# c\nType=Application\nOrder=nan\n").unwrap();
    std::fs::write(root.join("x.desktop"), "[Desktop Entry]\nType=Application\nOrder=abc\nOrigin=other.desktop\n").unwrap();
    assert_eq!(cairo_pins(&catalog, Some(&root)), s(&["app.desktop", "third.desktop"]));
}

#[test]
fn round_trip_preserves_pin_order_and_all_monitor_edge_choices() {
    let dir = TempDir::new();
    let mut store = ConfigStore::new(dir.join("settings/config.json"));
    let config = DockConfig { pins: s(&["b.desktop", "a.desktop", "b.desktop"]), edge: "left".into(), monitor: "DP-2".into(), ..Default::default() };
    let saved = store.save(&config).unwrap();
    assert_eq!(saved.pins, s(&["b.desktop", "a.desktop"]));
    assert_eq!(store.load(None, None).to_value(), saved.to_value());
    let text = std::fs::read_to_string(&store.path).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["version"], json!(1));
    let names: Vec<_> = std::fs::read_dir(dir.join("settings")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(names, vec![store.path.clone()]);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn existing_empty_pins_stay_empty_and_missing_load_never_writes() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    let catalog = FakeCatalog(vec!["org.dnf.AppCenter.desktop"]);
    let first = store.load(Some(&catalog), Some(&dir.join("cairo")));
    assert_eq!(first.pins, s(&["org.dnf.AppCenter.desktop"]));
    let config = store.load(None, None);
    assert_eq!(config.monitor, "all");
    assert!(config.hide_on_fullscreen);
    assert_eq!(config.hover_scale, 1.0);
    assert!(!store.path.exists());
    store.save(&config).unwrap();
    assert_eq!(store.load(Some(&catalog), None).pins, Vec::<String>::new());
}

#[test]
fn bad_config_is_reported_and_preserved() {
    let dir = TempDir::new();
    let path = dir.join("config.json");
    std::fs::write(&path, "invalid JSON").unwrap();
    let mut store = ConfigStore::new(&path);
    assert_eq!(store.load(None, None), DockConfig::default());
    assert_eq!(store.error, "Expecting value: line 1 column 1 (char 0)");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid JSON");
    std::fs::write(&path, "{\"version\": 2}").unwrap();
    store.load(None, None);
    assert_eq!(store.error, "This dock settings version is not supported.");
    std::fs::write(&path, vec![b' '; 1024 * 1024 + 1]).unwrap();
    store.load(None, None);
    assert_eq!(store.error, "Dock settings file is too large.");
    std::fs::write(&path, b"{\"pins\": \"\xff\"}").unwrap();
    store.load(None, None);
    assert_eq!(store.error, "'utf-8' codec can't decode byte 0xff in position 10: invalid start byte");
    std::fs::write(&path, "{}").unwrap();
    store.load(None, None);
    assert_eq!(store.error, "");
}

#[test]
fn monitor_placements_and_inactive_hiding_persist() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    let overrides: OrderedMap<OrderedMap<String>> = [
        ("DP-1", [("edge", "left".to_string()), ("alignment", "start".to_string())].into_iter().collect()),
        ("HDMI-A-1", [("alignment", "center".to_string())].into_iter().collect()),
    ]
    .into_iter()
    .collect();
    let config = DockConfig { edge: "bottom".into(), alignment: "end".into(), auto_hide: true, monitor_overrides: overrides, ..Default::default() };
    store.save(&config).unwrap();
    let restored = store.load(None, None);
    assert_eq!(restored.to_value(), config.to_value());
    let placement = |name: &str| {
        let view = restored.for_monitor(name);
        (view.edge, view.alignment)
    };
    assert_eq!(placement("DP-1"), ("left".into(), "start".into()));
    assert_eq!(placement("HDMI-A-1"), ("bottom".into(), "center".into()));
    assert_eq!(placement("new-output"), ("bottom".into(), "end".into()));
    assert!(restored.for_monitor("DP-1").auto_hide);
    assert!(restored.for_monitor("DP-1").hide_on_fullscreen);
}

#[test]
fn effective_monitor_config_does_not_mutate_or_alias_global_preferences() {
    let config = from(json!({"pins": ["app.desktop"], "monitor_overrides": {"DP-1": {"edge": "right"}}, "hot_corners": {"DP-1": ["top_left"]}}));
    let before = config.to_value();
    let mut effective = config.for_monitor("DP-1");
    assert_eq!(effective.edge, "right");
    effective.edge = "top".into();
    effective.pins.clear();
    effective.group_visibility.insert("weather", false);
    effective.hot_corners["DP-1"].push("bottom_right".into());
    effective.monitor_overrides["DP-1"].insert("edge", "left".into());
    assert_eq!(config.to_value(), before);
}

#[test]
fn legacy_config_defaults_fullscreen_menu_hotkey_off_and_all_groups_but_media_visible() {
    let config = from(json!({"version": 1, "pins": ["app.desktop"]}));
    assert!(!config.menu_hotkey_fullscreen);
    assert_eq!(config.group_visibility, GROUPS.iter().map(|(n, _)| (*n, *n != "media")).collect());
    assert!(GROUPS.iter().all(|(name, _)| config.is_group_visible(name) == (*name != "media")));
    assert!(!config.media_enabled());
    assert_eq!(config.pins, s(&["app.desktop"]));
    // Python: True == 1 == 1.0.
    from(json!({"version": true}));
    from(json!({"version": 1.0}));
}

#[test]
fn group_visibility_and_fullscreen_hotkey_round_trip_in_both_modes() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    let visibility: OrderedMap<bool> = GROUPS.iter().enumerate().map(|(i, (n, _))| (*n, i % 2 == 0)).collect();
    for mode in ["dock", "bar"] {
        for hotkey in [false, true] {
            let config = DockConfig { mode: mode.into(), menu_hotkey_fullscreen: hotkey, group_visibility: visibility.clone(), ..Default::default() };
            store.save(&config).unwrap();
            let loaded = store.load(None, None);
            assert_eq!(loaded.to_value(), config.to_value());
            for monitor in ["DP-1", "HDMI-A-1"] {
                let view = loaded.for_monitor(monitor);
                assert_eq!(view.menu_hotkey_fullscreen, hotkey);
                for (name, visible) in visibility.iter() {
                    assert_eq!(view.is_group_visible(name), *visible);
                }
            }
        }
    }
}

/// The old `system_tray` group merged into `service_apps`, the system tray: a
/// saved config (this shape is a real one) loads, keeps the tray where the
/// applets were, shows it if either old group was shown, and saves one id.
#[test]
fn the_old_tray_group_loads_into_the_system_tray() {
    let saved = json!({"version": 1, "mode": "bar", "edge": "top", "always_show_tray": ["id:dnf-app-center-updater", "id:steam"],
        "always_visible_apps": ["idle_inhibitor", "capslock", "bluetooth", "audio", "tiling", "showdesktop"],
        "group_layouts": {"bar": {"start": ["app_menu", "workspaces", "pinned", "running", "files"], "center": ["clock", "weather"],
                                  "end": ["media", "system_tray", "service_apps", "session", "activities", "focused_title"]}},
        "group_visibility": {"app_menu": true, "pinned": true, "running": true, "files": true, "workspaces": true, "media": true,
                             "system_tray": true, "service_apps": true, "weather": true, "clock": true, "session": true,
                             "activities": false, "focused_title": false}});
    let config = from(saved);
    assert_eq!(config.group_layouts["bar"]["end"], s(&["media", "service_apps", "session", "activities", "focused_title"]));
    assert!(config.is_group_visible("service_apps") && config.is_group_visible("media"));
    assert!(!config.group_visibility.contains_key("system_tray"));
    assert_eq!(config.always_show_tray, s(&["id:dnf-app-center-updater", "id:steam"]));
    assert_eq!(config.always_visible_apps, s(&["idle_inhibitor", "capslock", "bluetooth", "audio", "tiling", "showdesktop"]));
    let bytes = save_bytes(&config);
    assert!(!bytes.contains("system_tray"), "{bytes}");
    assert_eq!(from(serde_json::from_str(&bytes).unwrap()), config);
    // Shown when either was shown; hidden only when both were.
    for (tray, applets, shown) in [(true, true, true), (true, false, true), (false, true, true), (false, false, false)] {
        let config = from(json!({"group_visibility": {"system_tray": tray, "service_apps": applets}}));
        assert_eq!(config.is_group_visible("service_apps"), shown, "{tray} {applets}");
    }
    // Without the old key, the saved value stands (new saves never write it).
    assert!(!from(json!({"group_visibility": {"service_apps": false}})).is_group_visible("service_apps"));
    assert!(from(json!({"group_visibility": {"system_tray": false}})).is_group_visible("service_apps"));
    assert!(DockConfig::from_value(&json!({"group_visibility": {"system_tray": 1}})).is_err());
}

#[test]
fn partial_group_settings_default_visible() {
    let config = from(json!({"group_visibility": {"weather": false, "clock": false}}));
    assert!(!config.is_group_visible("weather"));
    assert!(!config.is_group_visible("clock"));
    assert!(config.is_group_visible("app_menu"));
    assert!(!config.is_group_visible("media"), "media is off unless saved on");
    assert_eq!(config.group_visibility.len(), GROUPS.len());
    // Normalized into GROUPS order.
    assert_eq!(config.group_visibility.keys().collect::<Vec<_>>(), GROUPS.iter().map(|g| g.0).collect::<Vec<_>>());
    assert!(DockConfig::default().is_group_visible("unknown"));
}

#[test]
fn group_settings_reject_unknown_groups_and_non_boolean_values() {
    for value in [json!(null), json!([]), json!(true), json!("weather"), json!({"unknown": true})] {
        rejects(json!({ "group_visibility": value }));
    }
    for value in [json!(null), json!("true"), json!("false"), json!(0), json!(1), json!(0.0), json!([]), json!({})] {
        rejects(json!({"group_visibility": {"weather": value}}));
        rejects(json!({ "menu_hotkey_fullscreen": value }));
    }
}

#[test]
fn hot_corner_defaults_and_saved_fullscreen_preference() {
    let config = from(json!({}));
    assert!(config.hot_corners.is_empty());
    assert_eq!(config.hot_corner_delay_ms, 150);
    assert!(!config.menu_hotkey_fullscreen);
    assert!(from(json!({"menu_hotkey_fullscreen": true})).menu_hotkey_fullscreen);
}

#[test]
fn hot_corners_persist_each_monitor_and_delay_in_both_layouts() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    let corners: OrderedMap<Vec<String>> = [
        ("DP-1", CORNERS.iter().map(|c| c.0.to_string()).collect()),
        ("HDMI-A-1", s(&["bottom_right"])),
        ("unplugged-monitor", s(&["top_left"])),
    ]
    .into_iter()
    .collect();
    for mode in ["dock", "bar"] {
        for delay in [100, 350, 2000] {
            let config = DockConfig { mode: mode.into(), hot_corners: corners.clone(), hot_corner_delay_ms: delay, ..Default::default() };
            let saved = store.save(&config).unwrap();
            let loaded = store.load(None, None);
            assert_eq!(loaded.to_value(), saved.to_value());
            assert_eq!(loaded.hot_corners, corners);
            assert_eq!(loaded.hot_corner_delay_ms, delay);
        }
    }
}

#[test]
fn loaded_hot_corners_deduplicate() {
    let config = from(json!({"hot_corners": {"DP-1": ["top_left", "bottom_right", "top_left"]}}));
    assert_eq!(config.hot_corners["DP-1"], s(&["top_left", "bottom_right"]));
}

#[test]
fn hot_corners_reject_unknown_names_and_invalid_value_types() {
    for value in [
        json!(null), json!([]), json!(true), json!("DP-1"), json!({"": []}), json!({"DP-1\n": []}),
        json!({"DP-1": "top_left"}), json!({"DP-1": {}}), json!({"DP-1": null}), json!({"DP-1": ["top"]}),
        json!({"DP-1": ["TOP_LEFT"]}), json!({"DP-1": [true]}), json!({"DP-1": [0]}), json!({"DP-1": [[]]}),
        json!({"DP-1": [{}]}),
    ] {
        rejects(json!({ "hot_corners": value }));
    }
    for value in [json!(null), json!(true), json!("350"), json!(350.0), json!(99), json!(2001), json!([]), json!({})] {
        rejects(json!({ "hot_corner_delay_ms": value }));
    }
}

#[test]
fn legacy_config_inherits_center_alignment_without_inactive_hiding() {
    let config = from(json!({"version": 1, "edge": "top", "monitor": "primary"}));
    assert_eq!(config.mode, "dock");
    assert_eq!(config.alignment, "center");
    assert!(!config.auto_hide);
    assert!(config.monitor_overrides.is_empty());
    assert!(config.hide_on_fullscreen);
    assert_eq!(config.transparency, 5);
    assert!(config.transparency_enabled);
    assert!(!config.frosted);
    assert_eq!(config.effective_transparency(), 5);
    assert_eq!(config.apps_transparency, 5);
    assert_eq!((config.frost_strength, config.apps_frost_strength), (50, 50));
    assert!(!config.apps_transparency_enabled);
    assert_eq!(config.effective_apps_transparency(), 0);
    assert!(!config.apps_frosted);
    assert!(config.apps_rounded_corners);
    assert!(!config.match_wallpaper);
    assert!(config.rounded_corners && config.float_mode);
    assert!(!config.bar_rounded_corners && !config.bar_float_mode);
    assert!(config.effective_rounded_corners() && config.effective_float_mode());
}

#[test]
fn background_preferences_round_trip_and_validate() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    for transparency in [0, 20, 100] {
        let config = from(json!({"transparency": transparency, "match_wallpaper": true, "pins": ["app.desktop"], "monitor_overrides": {"DP-1": {"edge": "left"}}}));
        store.save(&config).unwrap();
        assert_eq!(store.load(None, None).to_value(), config.to_value());
        assert_eq!(store.load(None, None).for_monitor("DP-1").transparency, transparency);
    }
    for invalid in [json!({"transparency": -1}), json!({"transparency": 101}), json!({"transparency": true}), json!({"transparency": 0.5}), json!({"match_wallpaper": "yes"}), json!({"match_wallpaper": 1})] {
        rejects(invalid);
    }
    assert_eq!(from(json!({"background_color": "#ABCDEF"})).background_color.as_deref(), Some("#abcdef"));
    assert_eq!(from(json!({"background_color": null})).background_color, None);
    assert_eq!(DockConfig::from_value(&json!({"app_dock_background_color": null})).unwrap_err(), "Choose a color in #RRGGBB format.");
}

#[test]
fn independent_transparency_toggles_retain_saved_levels() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    for dock_enabled in [false, true] {
        for apps_enabled in [false, true] {
            let config = DockConfig { transparency: 75, transparency_enabled: dock_enabled, apps_transparency: 30, apps_transparency_enabled: apps_enabled, ..Default::default() };
            store.save(&config).unwrap();
            let loaded = store.load(None, None).for_monitor("DP-1");
            assert_eq!((loaded.transparency, loaded.apps_transparency), (75, 30));
            assert_eq!(loaded.effective_transparency(), if dock_enabled { 75 } else { 0 });
            assert_eq!(loaded.effective_apps_transparency(), if apps_enabled { 30 } else { 0 });
        }
    }
    let cases: [(&str, Vec<Value>); 6] = [
        ("apps_transparency", vec![json!(-1), json!(101), json!(true), json!(20.5), json!("20")]),
        ("transparency_enabled", vec![json!(0), json!("true"), json!(null)]),
        ("frosted", vec![json!(0), json!("true"), json!(null)]),
        ("apps_transparency_enabled", vec![json!(1), json!("false"), json!(null)]),
        ("apps_frosted", vec![json!(1), json!("true"), json!(null)]),
        ("apps_rounded_corners", vec![json!(0), json!("false"), json!(null)]),
    ];
    for (key, values) in cases {
        for value in values {
            rejects(json!({ key: value }));
        }
    }
}

#[test]
fn frost_and_app_corners_persist_independently_of_dock() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    let original = DockConfig { apps_frosted: true, apps_rounded_corners: false, apps_transparency_enabled: true, transparency: 83, ..Default::default() };
    store.save(&original).unwrap();
    let mut loaded = store.load(None, None).for_monitor("DP-1");
    assert_eq!(loaded.to_value(), original.to_value());
    assert!(loaded.effective_apps_frosted());
    loaded.apps_transparency_enabled = false;
    assert!(loaded.effective_apps_frosted());
    assert_eq!(loaded.transparency, 83);
    assert!(loaded.rounded_corners);
}

#[test]
fn frost_can_be_opaque_independent_from_transparency() {
    let mut config = from(json!({"frosted": true, "transparency_enabled": false, "apps_transparency_enabled": true, "apps_frosted": true}));
    assert!(config.effective_frosted());
    assert_eq!(config.effective_transparency(), 0);
    assert!(config.effective_apps_frosted());
    config.transparency_enabled = true;
    config.apps_transparency_enabled = false;
    assert!(config.effective_frosted() && config.effective_apps_frosted());
    assert_eq!(config.effective_apps_transparency(), 0);
}

#[test]
fn legacy_tint_migrates_to_strength_without_changing_transparency() {
    let loaded = from(json!({"frosted": true, "transparency": 15, "apps_frosted": true, "apps_transparency": 82}));
    assert_eq!((loaded.frost_strength, loaded.apps_frost_strength), (15, 82));
    assert_eq!((loaded.transparency, loaded.apps_transparency), (15, 82));
    assert_eq!(from(json!({})).frost_strength, 50);
    assert_eq!(from(json!({"frosted": true})).frost_strength, 20);
    let mut values = loaded.to_value();
    values["frost_strength"] = json!(70);
    values["apps_frost_strength"] = json!(25);
    let loaded = from(values);
    assert_eq!((loaded.frost_strength, loaded.apps_frost_strength), (70, 25));
    assert_eq!((loaded.transparency, loaded.apps_transparency), (15, 82));
    let dir = TempDir::new();
    let mut store = store(&dir);
    store.save(&loaded).unwrap();
    assert_eq!(store.load(None, None).to_value(), loaded.to_value());
    for field in ["frost_strength", "apps_frost_strength"] {
        for invalid in [json!(-1), json!(101), json!(true), json!(25.5), json!("50"), json!(null)] {
            rejects(json!({ field: invalid }));
        }
    }
}

#[test]
fn dock_and_bar_modes_preserve_independent_appearance() {
    let dir = TempDir::new();
    let mut store = store(&dir);
    let original = from(json!({"pins": ["browser.desktop", "terminal.desktop"], "gap": 19, "transparency": 37, "match_wallpaper": true, "alignment": "end", "monitor_overrides": {"DP-1": {"edge": "top", "alignment": "start"}}}));
    for mode in ["bar", "dock"] {
        for (floating, rounded) in [(true, true), (false, true), (true, false), (false, false)] {
            let mut config = DockConfig { mode: mode.into(), ..original.clone() };
            if mode == "dock" {
                (config.float_mode, config.rounded_corners, config.bar_float_mode, config.bar_rounded_corners) = (floating, rounded, !floating, !rounded);
            } else {
                (config.float_mode, config.rounded_corners, config.bar_float_mode, config.bar_rounded_corners) = (!floating, !rounded, floating, rounded);
            }
            store.save(&config).unwrap();
            let loaded = store.load(None, None);
            assert_eq!(loaded.to_value(), config.to_value());
            let effective = loaded.for_monitor("DP-1");
            assert_eq!(effective.mode, mode);
            assert_eq!(effective.gap, 19);
            assert_eq!(effective.effective_float_mode(), floating);
            assert_eq!(effective.effective_rounded_corners(), rounded);
            assert_eq!(effective.alignment, "start");
            let other = DockConfig { mode: if mode == "dock" { "bar" } else { "dock" }.into(), ..loaded };
            assert_eq!(other.effective_float_mode(), !floating);
            assert_eq!(other.effective_rounded_corners(), !rounded);
        }
    }
}

#[test]
fn bar_defaults_are_square_and_flush_without_overwriting_dock_choices() {
    let config = from(json!({"mode": "bar", "gap": 21, "rounded_corners": true, "float_mode": true}));
    assert!(!config.effective_rounded_corners() && !config.effective_float_mode());
    let dock = DockConfig { mode: "dock".into(), ..config };
    assert!(dock.effective_rounded_corners() && dock.effective_float_mode());
    assert_eq!(dock.gap, 21);
}

#[test]
fn invalid_layout_modes_are_rejected() {
    for value in [json!(null), json!(true), json!(false), json!(0), json!(1), json!([]), json!({}), json!(""), json!("panel"), json!("Dock"), json!("BAR")] {
        assert_eq!(DockConfig::from_value(&json!({ "mode": value })).unwrap_err(), "Choose Dock or Bar mode.");
    }
}

#[test]
fn rounded_and_float_preferences_require_real_booleans() {
    for name in ["rounded_corners", "float_mode", "bar_rounded_corners", "bar_float_mode"] {
        for value in [json!(null), json!("true"), json!("false"), json!(0), json!(1), json!(0.0), json!([]), json!({})] {
            rejects(json!({ name: value }));
        }
    }
}

#[test]
fn loading_legacy_preferences_keeps_file() {
    let dir = TempDir::new();
    let path = dir.join("config.json");
    let original = "{\"version\":1,\"pins\":[\"app.desktop\"],\"edge\":\"right\",\"gap\":19}\n";
    std::fs::write(&path, original).unwrap();
    let config = ConfigStore::new(&path).load(None, None);
    assert_eq!(config.mode, "dock");
    assert!(config.rounded_corners && config.float_mode);
    assert_eq!((config.gap, config.edge.as_str()), (19, "right"));
    assert_eq!(config.pins, s(&["app.desktop"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn invalid_nested_monitor_preferences_are_rejected() {
    for value in [
        json!({"alignment": "left"}), json!({"auto_hide": "yes"}), json!({"auto_hide": 1}),
        json!({"monitor_overrides": []}), json!({"monitor_overrides": {"": {}}}),
        json!({"monitor_overrides": {"DP-1\n": {}}}), json!({"monitor_overrides": {"DP-1": "left"}}),
        json!({"monitor_overrides": {"DP-1": {"edge": "center"}}}),
        json!({"monitor_overrides": {"DP-1": {"alignment": "bottom"}}}),
        json!({"monitor_overrides": {"DP-1": {"auto_hide": true}}}),
    ] {
        rejects(value);
    }
}

#[test]
fn invalid_paths_types_ranges_and_unknown_versions_are_rejected() {
    for value in [
        json!({"pins": ["/tmp/app.desktop"]}), json!({"pins": ["bad\n.desktop"]}), json!({"pins": "app.desktop"}),
        json!({"pins": [".desktop"]}), json!({"pins": ["a\\b.desktop"]}),
        json!({"icon_size": true}), json!({"icon_size": 9000}), json!({"hide_on_fullscreen": "yes"}),
        json!({"hover_scale": 2.5}), json!({"hover_scale": true}), json!({"hover_scale": "1"}),
        json!({"edge": "center"}), json!({"version": 2}), json!([]), json!(null),
    ] {
        rejects(value);
    }
}

#[test]
fn error_messages_match_python_text() {
    let cases = [
        (json!([]), "Dock settings must be an object."),
        (json!({"version": "1"}), "This dock settings version is not supported."),
        (json!({"bar_border_enabled": 0}), "Invalid bar border setting."),
        (json!({"bar_border_sides": ["x"]}), "Choose valid bar border sides: top, bottom, left or right."),
        (json!({"app_dock_edge": "top"}), "Choose Left, Right or Bottom for the separate app dock."),
        (json!({"app_dock_shine": 1}), "Invalid separate app dock setting: app_dock_shine."),
        (json!({"group_layouts": []}), "Group layouts must describe Dock or Bar mode."),
        (json!({"app_dock_group_layout": []}), "App dock group positions must be an object."),
        (json!({"edge": 1}), "Choose a screen edge for the dock."),
        (json!({"alignment": "left"}), "Choose a valid dock alignment."),
        (json!({"monitor": " "}), "Choose a valid monitor."),
        (json!({"monitor_overrides": 1}), "Monitor placement overrides must be an object."),
        (json!({"monitor_overrides": {"a": {"x": 1}}}), "Monitor overrides support only edge and alignment."),
        (json!({"hot_corners": 1}), "Hot corners must map monitor names to enabled corners."),
        (json!({"gap": 49}), "Invalid dock setting: gap."),
        (json!({"hide_delay_ms": 5001}), "Invalid dock setting: hide_delay_ms."),
        (json!({"hover_scale": 0.5}), "Invalid dock icon hover scale."),
        (json!({"hide_on_fullscreen": 1}), "Invalid window stacking setting."),
        (json!({"menu_hotkey_fullscreen": 1}), "Invalid fullscreen application menu hotkey setting."),
        (json!({"group_visibility": 1}), "Dock group visibility must be an object."),
        (json!({"group_visibility": {"x": true}}), "Unknown dock group visibility setting."),
        (json!({"group_visibility": {"clock": 1}}), "Dock group visibility values must be true or false."),
        (json!({"auto_hide": 1}), "Invalid automatic hiding setting."),
        (json!({"match_wallpaper": 1}), "Invalid wallpaper color matching setting."),
        (json!({"apps_frosted": 1}), "Invalid transparency setting: apps_frosted."),
        (json!({"compact_bar": 1}), "Invalid appearance setting: compact_bar."),
        (json!({"bar_islands": "yes"}), "Invalid appearance setting: bar_islands."),
        (json!({"rounded_corners": 1}), "Invalid rounded corners setting."),
        (json!({"float_mode": 1}), "Invalid float mode setting."),
        (json!({"bar_rounded_corners": 1}), "Invalid Bar rounded corners setting."),
        (json!({"bar_float_mode": 1}), "Invalid Bar float mode setting."),
        (json!({"pins": [1]}), "Pinned applications must use desktop entry IDs."),
        (json!({"always_show_tray": [" "]}), "Always show tray applications must use valid application identifiers."),
        (json!({"always_visible_apps": ["x"]}), "Always visible apps must use supported applet names."),
        // Colours are checked before everything else.
        (json!({"mode": "x", "bar_border_color": "x"}), "Choose a color in #RRGGBB format."),
    ];
    for (value, message) in cases {
        assert_eq!(DockConfig::from_value(&value).unwrap_err(), message, "{value}");
    }
}

#[test]
fn hover_scale_keeps_python_int_or_float_form() {
    let int = from(json!({"hover_scale": 1}));
    assert_eq!(int.to_value()["hover_scale"].to_string(), "1");
    assert_eq!(int, DockConfig::default());
    let float = from(json!({"hover_scale": 1.25}));
    assert_eq!(float.to_value()["hover_scale"].to_string(), "1.25");
    assert!(save_bytes(&DockConfig::default()).contains("\"hover_scale\": 1.0,"));
}

#[test]
fn for_app_dock_uses_app_dock_appearance() {
    let config = from(json!({"mode": "bar", "edge": "top", "separate_app_dock": true, "app_dock_edge": "right", "app_dock_icon_size": 64, "app_dock_background_color": "#112233", "application_display": "title"}));
    assert!(config.uses_separate_app_dock());
    let dock = config.for_app_dock();
    assert_eq!((dock.mode.as_str(), dock.edge.as_str(), dock.alignment.as_str(), dock.application_display.as_str()), ("dock", "right", "center", "icons"));
    assert_eq!(dock.icon_size, 64);
    assert_eq!(dock.background_color.as_deref(), Some("#112233"));
    assert!(!dock.float_mode && !dock.rounded_corners);
    assert!(!DockConfig { edge: "bottom".into(), ..config.clone() }.uses_separate_app_dock());
}

#[test]
fn field_names_follow_dataclass_order() {
    let value = DockConfig::default().to_value();
    let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, field_names());
    assert_eq!(field_names().len(), 65);
}

#[test]
fn bar_islands_is_off_by_default_and_only_splits_bars() {
    assert!(!DockConfig::default().bar_islands);
    // A configuration saved before the option existed keeps one solid bar.
    assert!(!from(json!({"mode": "bar"})).uses_bar_islands());
    assert!(from(json!({"mode": "bar", "bar_islands": true})).uses_bar_islands());
    assert!(!from(json!({"mode": "dock", "bar_islands": true})).uses_bar_islands());
    assert_eq!(from(json!({"mode": "bar", "bar_islands": true})).to_value()["bar_islands"], json!(true));
}

#[test]
fn media_is_off_by_default_and_a_saved_choice_is_kept() {
    let default = DockConfig::default();
    assert!(!default.media_enabled() && !default.group_visibility["media"]);
    assert!(GROUPS.iter().filter(|(n, _)| *n != "media").all(|(n, _)| default.is_group_visible(n)));
    let mut missing = DockConfig::default();
    missing.group_visibility.remove("media");
    assert!(!missing.is_group_visible("media"));
    assert!(from(json!({"group_visibility": {"media": true}})).media_enabled());
    assert!(!from(json!({"group_visibility": {"media": false}})).media_enabled());
    // Every saved config names every group, so a config saved before media was
    // off by default keeps media on.
    let saved = save_bytes(&DockConfig { group_visibility: GROUPS.iter().map(|(n, _)| (*n, true)).collect(), ..Default::default() });
    let value: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(value["group_visibility"]["media"], json!(true));
    assert!(from(value).media_enabled());
    // The old always-visible Media applet meant on, unless a saved switch says otherwise.
    assert!(from(json!({"always_visible_apps": ["media"]})).media_enabled());
    assert!(!from(json!({"always_visible_apps": ["media"], "group_visibility": {"media": false}})).media_enabled());
}

#[test]
fn new_users_start_with_media_off() {
    assert!(!new_user_config().media_enabled());
}

#[test]
fn tray_icon_size_defaults_to_medium_and_old_configs_get_medium() {
    assert_eq!(DockConfig::default().tray_icon_size, "medium");
    assert_eq!(DEFAULT_TRAY_ICON_SIZE, "medium");
    assert_eq!(new_user_config().tray_icon_size, "medium");
    // A config saved before the setting existed has no key: it gets Medium.
    assert_eq!(from(json!({"mode": "bar", "icon_size": 32})).tray_icon_size, "medium");
    let mut old = DockConfig::default().to_saved_value();
    old.as_object_mut().unwrap().remove("tray_icon_size");
    assert_eq!(from(old).tray_icon_size, "medium");
    // Saved with the other keys, right after the always-visible applets.
    let saved: Value = serde_json::from_str(&save_bytes(&DockConfig::default())).unwrap();
    assert_eq!(saved["tray_icon_size"], json!("medium"));
    let keys: Vec<&str> = saved.as_object().unwrap().keys().map(String::as_str).collect();
    let at = keys.iter().position(|k| *k == "tray_icon_size").unwrap();
    assert_eq!(keys[at - 1], "always_visible_apps");
}

#[test]
fn tray_icon_size_round_trips_and_rejects_bad_values() {
    assert_eq!(TRAY_ICON_SIZES.iter().map(|(n, _)| *n).collect::<Vec<_>>(), ["small", "medium", "large"]);
    for size in ["small", "medium", "large"] {
        let config = from(json!({"tray_icon_size": size}));
        assert_eq!(config.tray_icon_size, size);
        assert_eq!(from(config.to_saved_value()), config);
    }
    for bad in [json!("huge"), json!("Medium"), json!(""), json!(24), json!(null), json!(true), json!(["small"])] {
        rejects(json!({"tray_icon_size": bad}));
        assert_eq!(DockConfig::from_value(&json!({"tray_icon_size": bad})).unwrap_err(),
                   "Choose a small, medium or large system tray icon size.");
    }
    let dir = TempDir::new();
    let mut store = store(&dir);
    let saved = store.save(&DockConfig { tray_icon_size: "large".into(), ..Default::default() }).unwrap();
    assert_eq!(saved.tray_icon_size, "large");
    assert_eq!(store.load(None, None).tray_icon_size, "large");
    assert!(store.save(&DockConfig { tray_icon_size: "xl".into(), ..Default::default() }).is_err());
}
