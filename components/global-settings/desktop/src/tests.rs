//! Pure ports of test_layout_presets.PresetTests and the non-GTK parts of
//! test_desktop_pages / test_dock_pages (bridge offline store, wayshell.sock
//! protocol, HDR outputs). No bus, display or real file outside $TMPDIR.
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use waydock::core::config::{save_bytes, ConfigStore, DockConfig};

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
fn shipped_win_preset_is_valid_and_excludes_wallpapers_and_pins() {
    let (presets, problems) = load_presets();
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["GNOME", "KDE", "Mac", "Win"]);
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
    let store = Mutex::new(suite_core::clock::ClockPreferencesStore::new(dir.join("clock.json")));
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
    let updated = waydock::dbus::settings_api::apply_patch(&current, &json!({"changes": changes, "expected": expected})).unwrap();
    assert_eq!((updated.edge.as_str(), updated.alignment.as_str()), ("top", "end"));
    assert!(waydock::dbus::settings_api::apply_patch(&current, &json!({"changes": {"pins": []}, "expected": {"pins": current.pins}})).is_err());
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
    let path = dir.join("waydock/config.json");
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

fn fake_wayshell(socket: PathBuf, replies: Vec<&'static str>) -> std::thread::JoinHandle<Vec<String>> {
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
fn wayshell_socket_protocol_bytes_and_errors() {
    let dir = scratch("sock");
    let socket = dir.join("wayshell.sock");
    let server = fake_wayshell(socket.clone(), vec!["{\"ok\": true, \"monitors\": [\"HEADLESS-1\"]}\n",
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
    let status: way_displays::color_config::Status = serde_json::from_value(json!({
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
    for field in waydock::dbus::settings_api::APPEARANCE_FIELDS {
        let mut copy = config.clone();
        let value = if copy.to_value()[*field].is_boolean() { json!(!copy.to_value()[*field].as_bool().unwrap()) } else { json!(13) };
        crate::theme::set_appearance_field(&mut copy, field, &value);
        assert_eq!(copy.to_value()[*field], value, "{field}");
    }
}

#[test]
fn sections_and_unknown_page() {
    assert_eq!(crate::SECTIONS, ["displays", "theme", "dock", "behavior", "layout", "lock", "login"]);
}
