//! Pure ports of test_layout_presets.PresetTests and the non-GTK parts of
//! test_desktop_pages / test_dock_pages (bridge offline store, tessel-shell.sock
//! protocol, HDR outputs). No bus, display or real file outside $TMPDIR.
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use tessel_dock::core::config::{save_bytes, ConfigStore, DockConfig};

use crate::bridge::{self, BridgeCore, PresetBridge};
use crate::layout_presets::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gs-desktop-unit-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn dock(values: Value) -> Map<String, Value> {
    values.as_object().unwrap().clone()
}

fn config_with(values: Value) -> DockConfig {
    let Value::Object(mut base) = DockConfig::default().to_value() else { unreachable!() };
    for (k, v) in values.as_object().unwrap() {
        base.insert(k.clone(), v.clone());
    }
    DockConfig::from_value(&Value::Object(base)).unwrap()
}

struct FakeBridge {
    config: Mutex<DockConfig>,
    fail: bool,
    saved: Mutex<Vec<DockConfig>>,
    error: Mutex<String>,
}

impl FakeBridge {
    fn new(config: DockConfig, fail: bool) -> Self {
        Self { config: Mutex::new(config), fail, saved: Mutex::new(Vec::new()), error: Mutex::new(String::new()) }
    }
}

impl PresetBridge for FakeBridge {
    fn config(&self) -> DockConfig { self.config.lock().unwrap().clone() }
    fn refresh(&self) -> bool { true }
    fn save_config(&self, config: &DockConfig) -> bool {
        if self.fail {
            *self.error.lock().unwrap() = "dock offline".into();
            return false;
        }
        self.saved.lock().unwrap().push(config.clone());
        *self.config.lock().unwrap() = config.clone();
        true
    }
    fn error(&self) -> String { self.error.lock().unwrap().clone() }
    fn close(&self) {}
}

struct FakeWorkspaces {
    count: Mutex<i64>,
    saves: Mutex<Vec<i64>>,
}

impl WorkspaceBackend for FakeWorkspaces {
    fn load(&self) -> Result<WorkspaceState, String> {
        let count = *self.count.lock().unwrap();
        Ok(WorkspaceState { count, signature: format!("n{count}") })
    }
    fn save(&self, count: i64, expected: &WorkspaceState) -> Result<WorkspaceState, String> {
        let current = *self.count.lock().unwrap();
        assert_eq!(expected.count, current);
        self.saves.lock().unwrap().push(count);
        *self.count.lock().unwrap() = count;
        self.load()
    }
}

struct FakeTiling {
    gapless: Mutex<bool>,
    calls: Mutex<Vec<bool>>,
}

impl TilingBackend for FakeTiling {
    fn load_preferences(&self) -> Result<Map<String, Value>, String> {
        Ok(dock(json!({"gapless": *self.gapless.lock().unwrap()})))
    }
    fn set_gapless(&self, enabled: bool) -> Result<(), String> {
        self.calls.lock().unwrap().push(enabled);
        *self.gapless.lock().unwrap() = enabled;
        Ok(())
    }
}

fn fakes(count: i64, gapless: bool) -> (FakeWorkspaces, FakeTiling) {
    (FakeWorkspaces { count: Mutex::new(count), saves: Mutex::new(Vec::new()) },
     FakeTiling { gapless: Mutex::new(gapless), calls: Mutex::new(Vec::new()) })
}

fn preset(values: Value) -> Preset {
    Preset::new("test", "Test", PresetIcon::File("/nonexistent.svg".into()), dock(values), Some(3), Some(false))
}

#[test]
fn a_new_user_starts_on_the_tessel_layout() {
    let (presets, problems) = crate::layout_presets::load_presets();
    assert!(problems.is_empty(), "{problems:?}");
    let tessel = presets.iter().find(|p| p.key == "tessel").unwrap();
    let config = tessel_dock::core::config::new_user_config();
    assert_eq!(config.mode, "bar");
    assert!(config.bar_islands && config.edge == "top");
    assert!(config.hot_corners.is_empty() && config.monitor_overrides.is_empty(), "no one else's monitor names");
    assert_eq!(tessel.gapless, Some(false), "session startup gives a new user this gap");
    assert!(crate::layout_presets::matches(tessel, &config, tessel.workspaces, tessel.gapless, None), "Desktop Layout should show Tessel as current");
}

#[test]
fn previous_layout_round_trips_and_changes_are_described() {
    let config = config_with(json!({"edge": "bottom", "icon_size": 40}));
    let folder = scratch("previous");
    let path = folder.join("state/previous-layout.json");
    assert!(load_previous(Some(&path), "Previous layout").is_none());
    save_previous(&capture(&config, Some(4), Some(true), "Previous layout", None), Some(&path)).unwrap();
    let previous = load_previous(Some(&path), "Previous layout").unwrap();
    assert_eq!((previous.key.as_str(), previous.workspaces, previous.gapless), ("previous", Some(4), Some(true)));
    assert!(matches(&previous, &config, Some(4), Some(true), None));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    let lines = describe_changes(&preset(json!({"edge": "left", "icon_size": 40, "gap": 3})), &config, Some(4), Some(true),
                                 None, &identity).unwrap();
    assert_eq!(lines, ["Dock and appearance: Position on screen", "1 other dock and panel settings",
                       "Workspaces: 4 → 3", "Gapless tiling: off"]);
}

#[test]
fn previous_layout_bytes_are_sorted_indented_and_ascii() {
    let data = json!({"version": 1, "name": "Prévious", "dock": {"edge": "top", "alignment": "end"}, "workspaces": 2});
    assert_eq!(previous_bytes(&data),
               "{\n  \"dock\": {\n    \"alignment\": \"end\",\n    \"edge\": \"top\"\n  },\n  \"name\": \"Pr\\u00e9vious\",\n  \"version\": 1,\n  \"workspaces\": 2\n}\n");
}

#[test]
fn shipped_presets_name_no_monitors() {
    let (presets, problems) = load_presets();
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(presets.len(), 6);
    for preset in &presets {
        assert_eq!(preset.dock["hot_corners"], json!({}), "{}", preset.name);
        assert_eq!(preset.dock["monitor_overrides"], json!({}), "{}", preset.name);
    }
}

#[test]
fn shipped_win_preset_is_valid_and_excludes_wallpapers_and_pins() {
    let (presets, problems) = load_presets();
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["GNOME", "GNOME (Ubuntu)", "KDE", "Mac", "Tessel", "Win"]);
    let win = presets.iter().find(|p| p.key == "win").unwrap();
    assert_eq!(win.name, "Win");
    assert!(win.icon.exists());
    assert!(!win.dock.contains_key("pins"));
    let data: Value = serde_json::from_slice(BUILTIN.iter().find(|b| b.0 == "win").unwrap().1).unwrap();
    assert!(!data.as_object().unwrap().keys().any(|k| k.contains("wallpaper")));
    let fields = bridge::settings_fields();
    assert!(win.dock.keys().all(|k| fields.contains(k)));
    assert_eq!((win.dock["mode"].as_str(), win.dock["edge"].as_str(), win.dock["alignment"].as_str()),
               (Some("bar"), Some("bottom"), Some("center")));
    // The embedded copies are byte-identical to the data files next to the crate.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts");
    let (from_dir, problems) = load_presets_from(&dir);
    assert!(problems.is_empty());
    assert_eq!(from_dir.iter().map(|p| (&p.key, &p.dock)).collect::<Vec<_>>(), presets.iter().map(|p| (&p.key, &p.dock)).collect::<Vec<_>>());
}

#[test]
fn capture_round_trips_and_omits_pins() {
    let config = config_with(json!({"pins": ["a.desktop"], "edge": "top", "icon_size": 40}));
    let data = capture(&config, Some(6), Some(true), "Mine", None);
    assert!(!data["dock"].as_object().unwrap().contains_key("pins"));
    let parsed = parse_preset("mine", &serde_json::from_str(&data.to_string()).unwrap(), PresetIcon::File("x".into())).unwrap();
    assert_eq!((parsed.dock["edge"].as_str(), parsed.dock["icon_size"].as_i64(), parsed.workspaces, parsed.gapless),
               (Some("top"), Some(40), Some(6), Some(true)));
    assert!(matches(&parsed, &config, Some(6), Some(true), None));
    assert!(!matches(&parsed, &config_with(json!({"pins": ["a.desktop"], "edge": "top", "icon_size": 48})), Some(6), Some(true), None));
    assert!(!matches(&parsed, &config, Some(5), Some(true), None));
}

#[test]
fn invalid_presets_are_rejected() {
    for data in [json!({"version": 2, "name": "x"}), json!({"version": 1, "name": ""}),
                 json!({"version": 1, "name": "x", "dock": {"pins": []}}),
                 json!({"version": 1, "name": "x", "dock": {"edge": "middle"}}),
                 json!({"version": 1, "name": "x", "workspaces": 0}),
                 json!({"version": 1, "name": "x", "gapless": "yes"}),
                 json!({"version": 1, "name": "x", "clock": {"stacked": "no"}}),
                 json!({"version": 1, "name": "x", "clock": {"unknown": true}})] {
        assert!(parse_preset("bad", &data, PresetIcon::File("x".into())).is_err(), "{data}");
    }
    assert_eq!(parse_preset("bad", &json!({"version": 2}), PresetIcon::File("x".into())).unwrap_err(), "Unsupported layout preset: bad");
    assert_eq!(parse_preset("bad", &json!({"version": 1, "name": "x", "dock": {"pins": []}}), PresetIcon::File("x".into())).unwrap_err(),
               "Layout preset bad contains settings it cannot change.");
    assert!(parse_preset("ok", &json!({"version": 1, "name": " ok ", "clock": {"use_24_hour": true}}), PresetIcon::File("x".into()))
        .is_ok_and(|p| p.name == "ok"));
}

#[test]
fn broken_file_is_reported_without_hiding_others() {
    let dir = scratch("broken");
    std::fs::write(dir.join("good.layout"), r#"{"version": 1, "name": "Good"}"#).unwrap();
    std::fs::write(dir.join("bad.layout"), "{").unwrap();
    let (presets, problems) = load_presets_from(&dir);
    assert_eq!(presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Good"]);
    assert_eq!(problems.len(), 1);
}

#[test]
fn apply_sets_dock_workspaces_and_tiling_and_keeps_pins() {
    let bridge = FakeBridge::new(config_with(json!({"pins": ["mine.desktop"]})), false);
    let (workspaces, tiling) = fakes(4, true);
    let errors = apply_preset(&preset(json!({"edge": "left", "alignment": "start"})), &bridge, Some(&workspaces), Some(&tiling), None).unwrap();
    assert!(errors.is_empty());
    let config = bridge.config();
    assert_eq!((config.edge.as_str(), config.alignment.as_str(), config.pins.clone()), ("left", "start", vec!["mine.desktop".to_owned()]));
    assert_eq!((workspaces.saves.lock().unwrap().clone(), tiling.calls.lock().unwrap().clone()), (vec![3], vec![false]));
    // Already applied parts are not rewritten.
    assert!(apply_preset(&preset(json!({"edge": "left"})), &bridge, Some(&workspaces), Some(&tiling), None).unwrap().is_empty());
    assert_eq!((workspaces.saves.lock().unwrap().clone(), tiling.calls.lock().unwrap().clone()), (vec![3], vec![false]));
}

#[test]
fn dock_failure_is_reported_and_other_parts_still_apply() {
    let (workspaces, tiling) = fakes(4, false);
    let errors = apply_preset(&preset(json!({"edge": "left"})), &FakeBridge::new(DockConfig::default(), true),
                              Some(&workspaces), Some(&tiling), None).unwrap();
    assert_eq!(errors, ["dock offline"]);
    assert_eq!(workspaces.saves.lock().unwrap().clone(), vec![3]);
}

#[test]
fn clock_part_writes_only_changed_keys() {
    let dir = scratch("clock");
    let store = Mutex::new(tessel_core::clock::ClockPreferencesStore::new(dir.join("clock.json")));
    let mut target = preset(json!({}));
    target.clock = Some(dock(json!({"use_24_hour": true, "stacked": true})));
    let bridge = FakeBridge::new(DockConfig::default(), false);
    assert!(apply_preset(&target, &bridge, None, None, Some(&store)).unwrap().is_empty());
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("clock.json")).unwrap()).unwrap();
    assert_eq!(saved, json!({"use_24_hour": true, "version": 1}));
    let clock = store.load().unwrap();
    assert!(matches(&target, &bridge.config(), None, None, Some(&clock)));
}

#[test]
fn settings_api_accepts_every_preset_field() {
    let current = DockConfig::default();
    let values = current.to_value();
    let changes = json!({"edge": "top", "alignment": "end", "hover_scale": values["hover_scale"],
                         "hide_delay_ms": values["hide_delay_ms"], "reveal_delay_ms": values["reveal_delay_ms"]});
    let expected: Map<String, Value> = changes.as_object().unwrap().keys().map(|k| (k.clone(), values[k].clone())).collect();
    let updated = tessel_dock::dbus::settings_api::apply_patch(&current, &json!({"changes": changes, "expected": expected})).unwrap();
    assert_eq!((updated.edge.as_str(), updated.alignment.as_str()), ("top", "end"));
    assert!(tessel_dock::dbus::settings_api::apply_patch(&current, &json!({"changes": {"pins": []}, "expected": {"pins": current.pins}})).is_err());
}

/// An offline bridge core (Python's OfflineBridge: `_running()` is False, no bus).
fn offline_core(store: &Path, fields: BTreeSet<String>) -> crate::bridge::DockSettingsBridge {
    let options = bridge::BridgeOptions { fields, bus: None, store: Some(ConfigStore::new(store)), read: false,
                                          running: Some(Arc::new(|| false)) };
    crate::bridge::DockSettingsBridge::new_without_display(None, options).unwrap()
}

#[test]
fn offline_bridge_saves_only_its_fields_in_the_dock_byte_format() {
    let dir = scratch("bridge");
    let path = dir.join("tessel-dock/config.json");
    let mut store = ConfigStore::new(&path);
    store.save(&config_with(json!({"pins": ["org.example.App.desktop"], "hot_corners": {"offline-output": ["bottom_left"]}}))).unwrap();
    let appearance = offline_core(&path, bridge::appearance_fields());
    let dock_page = offline_core(&path, {
        let mut f = bridge::dock_fields();
        f.remove("match_wallpaper");
        f
    });
    // Theme saves transparency; the dock page's stale auto_hide edit is not saved by it.
    let mut desired = appearance.config();
    desired.transparency = 67;
    desired.auto_hide = true;
    assert!(appearance.core().save(&desired).is_ok());
    let saved = ConfigStore::new(&path).load(None, None);
    assert_eq!((saved.transparency, saved.auto_hide), (67, false));
    assert_eq!(saved.pins, ["org.example.App.desktop"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), save_bytes(&saved));
    // The dock page's baseline is older: changing transparency conflicts.
    let mut edit = dock_page.config();
    edit.transparency = 60;
    let error = dock_page.core().save(&edit).unwrap_err();
    assert_eq!(error, "Dock settings changed elsewhere. Reset this page before applying again.");
    assert_eq!(dock_page.error(), error);
    // A refresh limited to the appearance fields keeps the page's other baseline values.
    assert!(dock_page.core().refresh(Some(&bridge::appearance_fields()), None).is_ok());
    assert_eq!(dock_page.config().transparency, 67);
    let mut edit = dock_page.config();
    edit.auto_hide = true;
    assert!(dock_page.core().save(&edit).is_ok());
    let saved = ConfigStore::new(&path).load(None, None);
    assert_eq!((saved.transparency, saved.auto_hide, saved.hot_corners.get("offline-output").cloned()),
               (67, true, Some(vec!["bottom_left".to_owned()])));
}

#[test]
fn offline_bridge_reports_a_malformed_store() {
    let dir = scratch("malformed");
    let path = dir.join("config.json");
    std::fs::write(&path, "{").unwrap();
    let options = bridge::BridgeOptions { fields: bridge::appearance_fields(), bus: None, store: Some(ConfigStore::new(&path)),
                                          read: false, running: Some(Arc::new(|| false)) };
    assert!(crate::bridge::DockSettingsBridge::new_without_display(None, options).is_err());
}

#[test]
fn request_lists_only_changed_page_fields() {
    let dir = scratch("request");
    let core: Arc<BridgeCore> = offline_core(&dir.join("c.json"), bridge::desktop_fields()).core();
    let mut config = core.config();
    config.hot_corner_delay_ms = 300;
    config.transparency = 80;
    assert_eq!(core.request_for(&config), json!({"changes": {"hot_corner_delay_ms": 300}, "expected": {"hot_corner_delay_ms": 150}}));
}

fn fake_tessel_shell(socket: PathBuf, replies: Vec<&'static str>) -> std::thread::JoinHandle<Vec<String>> {
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            seen.push(line);
            (&stream).write_all(reply.as_bytes()).unwrap();
        }
        seen
    })
}

#[test]
fn tessel_shell_socket_protocol_bytes_and_errors() {
    let dir = scratch("sock");
    let socket = dir.join("tessel-shell.sock");
    let server = fake_tessel_shell(socket.clone(), vec!["{\"ok\": true, \"monitors\": [\"HEADLESS-1\"]}\n",
                                                    "{\"ok\": false, \"msg\": \"No such monitor\"}\n", "{\"ok\": false}\n", ""]);
    let payload = json!({"cmd": "set", "monitor": "HEADLESS-1", "file": null, "mode": "Color Only", "color": "rgb(1,2,3)"});
    assert_eq!(crate::theme::wallpaper_request_at(&socket, &json!({"cmd": "list-monitors"}), 2.0).unwrap()["monitors"], json!(["HEADLESS-1"]));
    assert_eq!(crate::theme::wallpaper_request_at(&socket, &payload, 2.0).unwrap_err(), "No such monitor");
    assert_eq!(crate::theme::wallpaper_request_at(&socket, &payload, 2.0).unwrap_err(), "The wallpaper could not be changed.");
    assert_eq!(crate::theme::wallpaper_request_at(&socket, &payload, 2.0).unwrap_err(), "The wallpaper service closed the connection.");
    let seen = server.join().unwrap();
    assert_eq!(seen[0], "{\"cmd\": \"list-monitors\"}\n");
    assert_eq!(seen[1], "{\"cmd\": \"set\", \"monitor\": \"HEADLESS-1\", \"file\": null, \"mode\": \"Color Only\", \"color\": \"rgb(1,2,3)\"}\n");
    assert!(crate::theme::wallpaper_request_at(&dir.join("missing.sock"), &payload, 1.0).unwrap_err().starts_with("[Errno 2]"));
}

#[test]
fn hdr_outputs_lists_enabled_hdr_monitors_only() {
    let status: tessel_displays::color_config::Status = serde_json::from_value(json!({
        "compositor": {"pid": "1"}, "DP-1": {"enabled": "true", "hdr_supported": "true"},
        "HDMI-A-1": {"enabled": "true", "hdr_supported": "false"}, "DP-2": {"enabled": "false", "hdr_supported": "true"}})).unwrap();
    assert_eq!(crate::displays::hdr_outputs(&status), ["DP-1"]);
}

#[test]
fn appearance_fields_replace_only_named_values() {
    let mut config = config_with(json!({"pins": ["test.desktop"], "transparency": 35}));
    crate::theme::set_appearance_field(&mut config, "apps_transparency", &json!(62));
    crate::theme::set_appearance_field(&mut config, "match_wallpaper", &json!(true));
    assert_eq!((config.apps_transparency, config.match_wallpaper, config.transparency), (62, true, 35));
    for field in tessel_dock::dbus::settings_api::APPEARANCE_FIELDS {
        let mut copy = config.clone();
        let value = if copy.to_value()[*field].is_boolean() { json!(!copy.to_value()[*field].as_bool().unwrap()) }
            else if crate::theme::COLOR_FIELDS.contains(field) { json!("#123456") } else { json!(13) };
        crate::theme::set_appearance_field(&mut copy, field, &value);
        assert_eq!(copy.to_value()[*field], value, "{field}");
    }
    // A chosen colour goes back to the built-in one (null).
    for field in crate::theme::COLOR_FIELDS {
        let mut copy = config_with(json!({field: "#a1b2c3"}));
        assert_eq!(copy.to_value()[field], json!("#a1b2c3"));
        crate::theme::set_appearance_field(&mut copy, field, &Value::Null);
        assert_eq!(copy.to_value()[field], Value::Null, "{field}");
        assert_eq!(copy, DockConfig::default(), "{field}");
    }
}

#[test]
fn layouts_keep_the_chosen_colors() {
    let (presets, problems) = load_presets();
    assert!(problems.is_empty(), "{problems:?}");
    let chosen = json!({"accent_color": "#c01c28", "apps_background_color": "#1e2a3b", "edge": "left"});
    for preset in &presets {
        for field in crate::theme::COLOR_FIELDS {
            assert!(!preset.dock.contains_key(field), "{} sets {field}", preset.name);
        }
        let bridge = FakeBridge::new(config_with(chosen.clone()), false);
        assert!(apply_preset(preset, &bridge, None, None, None).unwrap().is_empty(), "{}", preset.name);
        let config = bridge.config();
        assert_eq!((config.accent_color.as_deref(), config.apps_background_color.as_deref()),
                   (Some("#c01c28"), Some("#1e2a3b")), "{}", preset.name);
        assert_eq!(bridge.saved.lock().unwrap().len(), 1, "{}", preset.name);
        // The colours never make a layout look changed.
        assert!(matches(preset, &config, None, None, None), "{}", preset.name);
    }
}

#[test]
fn chosen_color_helpers() {
    let rgba = gtk::gdk::RGBA::new(0.5, 1.0, 0.0, 1.0);
    assert_eq!(crate::theme::rgba_hex(&rgba), "#80ff00");
    for dark in [false, true] {
        for field in crate::theme::COLOR_FIELDS {
            let color = crate::theme::builtin_color(field, dark);
            assert!(tessel_gtk::palette::parse_hex(&color).is_some() && color == color.to_lowercase(), "{color}");
        }
    }
    assert_ne!(crate::theme::builtin_color("apps_background_color", false), crate::theme::builtin_color("apps_background_color", true));
}

#[test]
fn sections_and_unknown_page() {
    assert_eq!(crate::SECTIONS, ["displays", "theme", "dock", "behavior", "layout", "lock", "login"]);
}

// ---------------------------------------------------------- window positions

mod window_positions {
    use super::scratch;
    use crate::window_positions::*;
    use serde_json::{json, Value};
    use std::cell::Cell;
    use tessel_displays::compat::fsx;
    use tessel_displays::labwc::BlockWrite;

    fn discord() -> Position {
        Position { app_id: "discord".into(), name: "Discord".into(), output: "DP-2".into(), x: 1620, y: 40, width: 900,
                   height: 1300, maximized: false }
    }

    fn browser() -> Position {
        Position { app_id: "firefox".into(), name: "Firefox".into(), output: "HDMI-A-1".into(), x: 0, y: 0, width: 1280,
                   height: 1000, maximized: true }
    }

    /// The block from the contract, byte for byte.
    const CONTRACT: &str = "<!-- tessel window positions begin -->
<windowRules>
  <windowRule identifier=\"discord\" type=\"normal\" matchOnce=\"true\">
    <action name=\"PlaceOnOutput\" output=\"DP-2\" x=\"1620\" y=\"40\" width=\"900\" height=\"1300\" maximized=\"no\"/>
  </windowRule>
</windowRules>
<!-- tessel window positions end -->";

    #[test]
    fn json_round_trip_matches_the_contract_schema() {
        let mut positions = WindowPositions::default();
        positions.set(discord());
        let saved: Value = serde_json::from_str(&positions.to_json()).unwrap();
        assert_eq!(saved, json!({"version": 1, "positions": [{"app_id": "discord", "name": "Discord", "output": "DP-2",
            "x": 1620, "y": 40, "width": 900, "height": 1300, "maximized": false}]}));
        assert_eq!(WindowPositions::parse(&positions.to_json()).positions, positions.positions);
        // Replacing keeps the entry's place; a new app is added at the end.
        positions.set(browser());
        positions.set(Position { x: 10, ..discord() });
        assert_eq!(positions.positions.iter().map(|p| (p.app_id.as_str(), p.x)).collect::<Vec<_>>(), [("discord", 10), ("firefox", 0)]);
        // labwc matches identifiers case-insensitively: one entry per app.
        positions.set(Position { app_id: "Discord".into(), ..discord() });
        assert_eq!(positions.positions.len(), 2);
        assert_eq!(positions.get("DISCORD").unwrap().app_id, "Discord");
        assert!(positions.remove("discord"));
        assert!(!positions.remove("discord"));
        assert_eq!(positions.positions, [browser()]);
        assert_eq!(serde_json::from_str::<Value>(&WindowPositions::default().to_json()).unwrap(), json!({"version": 1, "positions": []}));
    }

    #[test]
    fn loading_is_tolerant() {
        let text = r#"{"version": 1, "future": {"a": 1}, "positions": [
            {"app_id": "discord", "name": "Discord", "output": "DP-2", "x": 1620, "y": 40, "width": 900, "height": 1300, "maximized": false, "extra": 1},
            {"app_id": "", "output": "DP-2", "x": 0, "y": 0, "width": 1, "height": 1},
            {"app_id": "a", "output": "", "x": 0, "y": 0, "width": 1, "height": 1},
            {"app_id": "b", "output": "DP-1", "x": "0", "y": 0, "width": 1, "height": 1},
            {"app_id": "c", "output": "DP-1", "x": 0, "y": 0, "width": -1, "height": 1},
            {"app_id": "DISCORD", "output": "DP-1", "x": 0, "y": 0, "width": 1, "height": 1},
            {"app_id": "d", "output": "DP-1", "x": -5, "y": 2.0, "width": 0, "height": 0},
            7, null, "x"]}"#;
        let positions = WindowPositions::parse(text);
        assert_eq!(positions.positions.iter().map(|p| p.app_id.as_str()).collect::<Vec<_>>(), ["discord", "d"]);
        assert_eq!(positions.positions[0], discord());
        let d = &positions.positions[1];
        assert_eq!((d.name.as_str(), d.x, d.y, d.width, d.height, d.maximized), ("d", -5, 2, 0, 0, false));
        // Unknown top-level keys survive a save.
        let again: Value = serde_json::from_str(&positions.to_json()).unwrap();
        assert_eq!(again["future"], json!({"a": 1}));
        for junk in ["", "[]", "null", "{", "{\"positions\": {}}", "{\"positions\": [1]}", "\u{0}"] {
            assert!(WindowPositions::parse(junk).positions.is_empty(), "{junk:?}");
        }
        let dir = scratch("positions-load");
        assert_eq!(WindowPositions::load(&dir.join("missing.json")), WindowPositions::default());
        std::fs::write(dir.join("bad.json"), b"\xff\xfe").unwrap();
        assert!(WindowPositions::load(&dir.join("bad.json")).positions.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn json_is_saved_atomically_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("positions-save");
        let path = dir.join("tessel/window-positions.json");
        let mut positions = WindowPositions::default();
        positions.set(discord());
        positions.save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(WindowPositions::load(&path).positions, positions.positions);
        assert_eq!(WindowPositions::load(&path).to_json(), positions.to_json());
        assert_eq!(std::fs::read_dir(dir.join("tessel")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn block_text_matches_the_contract_and_escapes() {
        assert_eq!(block_text(&[discord()]).unwrap(), CONTRACT);
        assert_eq!(block_text(&[]), None);
        let both = block_text(&[discord(), browser()]).unwrap();
        assert!(both.contains("<windowRule identifier=\"firefox\" type=\"normal\" matchOnce=\"true\">\n    \
            <action name=\"PlaceOnOutput\" output=\"HDMI-A-1\" x=\"0\" y=\"0\" width=\"1280\" height=\"1000\" maximized=\"yes\"/>"));
        assert_eq!(both.matches("<windowRule ").count(), 2);
        assert_eq!(glob_escape("a*b?c[d]e\\f"), "a\\*b\\?c\\[d\\]e\\\\f");
        let odd = Position { app_id: "x*?[]\\&<>\"".into(), output: "D\"<&>".into(), ..discord() };
        let block = block_text(&[odd]).unwrap();
        assert!(block.contains("identifier=\"x\\*\\?\\[\\]\\\\&amp;&lt;&gt;&quot;\""), "{block}");
        assert!(block.contains("output=\"D&quot;&lt;&amp;&gt;\""), "{block}");
    }

    const USER_RC: &str = "<?xml version=\"1.0\"?>\n<labwc_config>\n  <core gap=\"4\"/>\n  <windowRules>\n    <windowRule identifier=\"mpv\" serverDecoration=\"no\"/>\n  </windowRules>\n</labwc_config>\n";

    #[test]
    fn block_coexists_with_snap_layouts_and_user_rules() {
        let mut layouts = crate::snap_layouts::SnapLayouts::default();
        layouts.set("DP-2", &crate::snap_layouts::Preset::Columns2.layout().unwrap());
        let snap = crate::snap_layouts::block_text(&layouts.outputs).unwrap();
        let ours = block_text(&[discord()]).unwrap();
        let other = block_text(&[browser()]).unwrap();
        // Either block first; each is replaced and removed without touching the other.
        for snap_first in [true, false] {
            let text = if snap_first {
                let text = crate::snap_layouts::with_block(USER_RC, Some(&snap)).unwrap();
                with_block(&text, Some(&ours)).unwrap()
            } else {
                let text = with_block(USER_RC, Some(&ours)).unwrap();
                crate::snap_layouts::with_block(&text, Some(&snap)).unwrap()
            };
            assert_eq!(current_block(&text), Some(ours.as_str()));
            assert_eq!(crate::snap_layouts::current_block(&text), Some(snap.as_str()));
            assert!(text.contains("<windowRule identifier=\"mpv\" serverDecoration=\"no\"/>"));
            let (s, o) = (text.find(&snap).unwrap(), text.find(&ours).unwrap());
            assert_eq!(s < o, snap_first);
            let replaced = with_block(&text, Some(&other)).unwrap();
            assert_eq!(replaced, text.replace(&ours, &other));
            let removed = with_block(&replaced, None).unwrap();
            assert_eq!(current_block(&removed), None);
            assert_eq!(crate::snap_layouts::current_block(&removed), Some(snap.as_str()));
            assert_eq!(crate::snap_layouts::with_block(&removed, None).unwrap(), USER_RC);
        }
        // The user's own section is never taken for ours.
        assert_eq!(current_block(USER_RC), None);
        assert_eq!(with_block(USER_RC, None).unwrap(), USER_RC);
        assert_eq!(with_block("<labwc_config/>\n", Some(&ours)).unwrap(), format!("<labwc_config>\n\n{ours}\n</labwc_config>\n"));
        assert!(with_block("<other/>", Some(&ours)).is_err());
    }

    #[test]
    fn rc_writes_only_on_change_and_reconfigures_once() {
        let dir = scratch("positions-rc");
        let rc = dir.join("rc.xml");
        std::fs::write(&rc, USER_RC).unwrap();
        let calls = Cell::new(0);
        let reconfigure = || {
            calls.set(calls.get() + 1);
            (true, None)
        };
        let mut positions = WindowPositions::default();
        positions.set(discord());
        assert!(!rc_in_sync(&rc, &positions));
        assert_eq!(sync_rc_with(&rc, &positions, &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Written);
        assert!(rc_in_sync(&rc, &positions));
        assert_eq!(sync_rc_with(&rc, &positions, &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Unchanged);
        assert_eq!(calls.get(), 1);
        assert_eq!(sync_rc_with(&rc, &WindowPositions::default(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::Written);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), USER_RC);
        assert_eq!(calls.get(), 2);
        let missing = dir.join("new/rc.xml");
        assert_eq!(sync_rc_with(&missing, &WindowPositions::default(), &fsx::os_replace, &reconfigure).unwrap(), BlockWrite::NoFile);
        assert!(!missing.exists());
        let error = sync_rc_with(&rc, &positions, &fsx::os_replace, &|| (false, Some("no compositor".into()))).unwrap_err();
        assert!(error.contains("no compositor"));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn reply(value: Value) -> tessel_tiling::client::Reply { value.as_object().unwrap().clone() }

    fn window(app_id: &str, output: &str, normal: bool) -> Value {
        json!({"id": 1, "active": false, "tiled": false, "visible": true, "output": 1, "x": 3000, "y": 50, "width": 900,
               "height": 1300, "fullscreen": false, "minimized": false, "app_id": app_id, "title": format!("{app_id} window"),
               "type_normal": normal, "output_name": output, "maximized": false,
               "place": {"x": 1620, "y": 40, "width": 900, "height": 1300}})
    }

    #[test]
    fn windows_reply_is_parsed_and_filtered() {
        let own = vec![OWN_APP_ID.to_owned()];
        // labwc before 0.20.2-29: no flag, or false.
        let old = reply(json!({"version": 1, "ready": true, "windows": [{"id": 1, "x": 0, "y": 0, "width": 10, "height": 10}]}));
        assert_eq!(parse_windows(&old, &own), Windows::Unsupported);
        let mut off = old.clone();
        off.insert("positions_supported".into(), json!(false));
        assert_eq!(parse_windows(&off, &own), Windows::Unsupported);
        let mut active = window("firefox", "HDMI-A-1", true);
        active["active"] = json!(true);
        active["maximized"] = json!(true);
        active["place"] = json!({"x": 0, "y": 0, "width": 1280, "height": 1000});
        let new = reply(json!({"version": 1, "ready": true, "positions_supported": true, "windows": [
            window("discord", "DP-2", true), window("discord", "DP-2", false), window("", "DP-2", true),
            window("vesktop", "", true), window("org.tessel.GlobalSettings", "DP-2", true), window("ORG.TESSEL.GLOBALSETTINGS", "DP-2", true),
            {"app_id": "noplace", "type_normal": true, "output_name": "DP-2"}, active, window("firefox", "DP-2", true), 5]}));
        let Windows::Open(open) = parse_windows(&new, &own) else { panic!() };
        assert_eq!(open.iter().map(|w| w.app_id.as_str()).collect::<Vec<_>>(), ["discord", "firefox", "firefox"]);
        assert_eq!(open[0], OpenWindow { app_id: "discord".into(), title: "discord window".into(), output: "DP-2".into(), x: 1620,
                                         y: 40, width: 900, height: 1300, maximized: false, active: false });
        let windows = Windows::Open(open.clone());
        // The active window of an app wins.
        assert!(windows.of_app("Firefox").unwrap().active);
        assert_eq!(windows.of_app("discord"), Some(&open[0]));
        assert_eq!(windows.of_app("steam"), None);
        let saved = Position::from_window(windows.of_app("firefox").unwrap(), "Firefox");
        assert_eq!(saved, browser());
        assert_eq!(Position::from_window(&open[0], "").name, "discord");
        // No answer.
        let failed = read_windows(&|| Err(tessel_tiling::client::ClientError::Unavailable("gone".into())), &own);
        assert_eq!(failed, Windows::Unavailable("gone".into()));
        assert!(failed.open().is_empty() && Windows::Unsupported.of_app("discord").is_none());
        let empty = reply(json!({"version": 1, "ready": true, "positions_supported": true}));
        assert_eq!(parse_windows(&empty, &own), Windows::Open(Vec::new()));
    }

    #[test]
    fn app_names_come_from_desktop_entries() {
        use tessel_dock::core::app_catalog::{AppCatalog, AppInfoData};
        let mut discord = AppInfoData::fake("discord.desktop", "Discord");
        discord.startup_wm_class = Some("discord".into());
        let catalog = AppCatalog::from_infos(vec![discord, AppInfoData::fake("org.mozilla.firefox.desktop", "Firefox")]);
        assert_eq!(app_label(&catalog, "discord").name, "Discord");
        assert_eq!(app_label(&catalog, "org.mozilla.firefox").name, "Firefox");
        assert_eq!(app_label(&catalog, "unknown.app").name, "unknown.app");
    }
}
