//! The `--check` JSON line (waydock.py `do_command_line`), keys and order
//! identical to Python, serialised with Python's `json.dumps` defaults.

use serde_json::{json, Map, Value};

use super::App;
use suite_core::pyfmt::{json_dumps, JsonStyle};

fn opt<T: Into<Value>>(value: Option<T>) -> Value { value.map(Into::into).unwrap_or(Value::Null) }

fn rgb(color: [u8; 3]) -> Value { json!([color[0], color[1], color[2]]) }

impl App {
    /// The `--check` payload (without the trailing newline).
    pub fn check_value(&self) -> Value {
        let config = self.config();
        let backend = self.backend();
        let docks = self.docks();
        let appearance = self.appearance.borrow().clone();
        let mut result = Map::new();
        result.insert("ready".into(), json!(backend.as_ref().is_some_and(|b| b.ready())));
        result.insert("docks".into(), json!(docks.len()));
        result.insert("mode".into(), json!(config.mode));
        result.insert("application_display".into(), json!(config.application_display));
        let labels: Map<String, Value> = self.monitor_entries().into_iter().map(|(name, label, _)| (name, Value::from(label))).collect();
        result.insert("monitor_labels".into(), Value::Object(labels));
        result.insert("menu_hotkey_fullscreen".into(), json!(config.menu_hotkey_fullscreen));
        let mut visible = Map::new();
        for dock in docks.iter().filter(|d| !d.is_app_dock) {
            visible.insert(dock.monitor_name.clone(), json!(dock.visible_groups()));
        }
        result.insert("visible_groups".into(), Value::Object(visible));
        let app_docks: Vec<Value> = docks.iter().filter(|d| d.is_app_dock).map(|dock| json!({
            "monitor": dock.monitor_name, "edge": dock.config.edge, "expanded": dock.bar_mode,
            "visible_groups": dock.visible_groups(),
        })).collect();
        result.insert("app_docks".into(), Value::Array(app_docks));
        result.insert("hot_corners".into(), self.hot_corners().map(|c| c.status()).unwrap_or_else(|| json!({})));
        result.insert("windows".into(), json!(backend.as_ref().map(|b| b.windows().len()).unwrap_or(0)));
        result.insert("outputs".into(), json!(backend.as_ref().map(|b| b.outputs().keys().cloned().collect::<Vec<_>>()).unwrap_or_default()));
        result.insert("error".into(), json!(backend.as_ref().map(|b| b.error()).unwrap_or_else(|| "not running".into())));
        result.insert("icon_theme".into(), appearance.as_ref().map(|a| opt(a.icon_theme_name())).unwrap_or(Value::Null));
        result.insert("appearance".into(),
                      appearance.as_ref().map(|a| json!(if a.is_dark() { "dark" } else { "light" })).unwrap_or(Value::Null));
        let wallpaper = self.wallpaper.borrow().clone();
        let publisher = self.publisher.borrow();
        let colors: Map<String, Value> =
            wallpaper.as_ref().map(|w| w.colors().into_iter().map(|(k, v)| (k, rgb(v))).collect()).unwrap_or_default();
        let mut screens = Map::new();
        for dock in &docks {
            let source = wallpaper.as_ref().and_then(|w| w.sources().get(&dock.monitor_name).cloned().flatten())
                .map(|p| Value::from(p.to_string_lossy().into_owned())).unwrap_or(Value::Null);
            let color = wallpaper.as_ref().and_then(|w| w.color_for(&dock.monitor_name)).map(rgb).unwrap_or(Value::Null);
            screens.insert(dock.monitor_name.clone(), json!({"source": source, "color": color}));
        }
        result.insert("background".into(), json!({
            "transparency": config.transparency,
            "frosted": config.frosted,
            "frost_strength": config.frost_strength,
            "transparency_enabled": config.transparency_enabled,
            "apps_transparency": config.apps_transparency,
            "apps_transparency_enabled": config.apps_transparency_enabled,
            "apps_frosted": config.apps_frosted,
            "apps_frost_strength": config.apps_frost_strength,
            "apps_rounded_corners": config.apps_rounded_corners,
            "match_wallpaper": config.match_wallpaper,
            "gtk_color_scheme_error": publisher.as_ref().map(|p| opt(p.gtk_color_scheme.error.clone())).unwrap_or(Value::Null),
            "papirus_folders_error": publisher.as_ref().map(|p| opt(p.papirus_palette.error())).unwrap_or(Value::Null),
            "rounded_corners": config.effective_rounded_corners(),
            "float_mode": config.effective_float_mode(),
            "colors": colors,
            "screens": screens,
            "error": match &wallpaper { Some(w) => opt(w.error()), None => json!("") },
        }));
        drop(publisher);
        let workspaces = self.workspace_backend.borrow().clone();
        let tray = self.tray_backend();
        let status = self.status_backend();
        let states = status.as_ref().map(|s| s.status()).unwrap_or_default();
        let mut names: Vec<&String> = states.keys().collect();
        names.sort();
        let indicator_states: Map<String, Value> = states.iter().map(|(name, state)| {
            (name.clone(), state.get("classes").cloned().unwrap_or_else(|| json!([])))
        }).collect();
        result.insert("panel".into(), json!({
            "workspaces_ready": workspaces.as_ref().is_some_and(|w| w.ready()),
            "workspaces": workspaces.as_ref().map(|w| w.workspaces().len()).unwrap_or(0),
            "tray_ready": tray.as_ref().is_some_and(|t| t.ready()),
            "tray_watcher": tray.as_ref().is_some_and(|t| t.is_watcher()),
            "tray_host_registered": tray.as_ref().is_some_and(|t| t.host_registered()),
            "tray_watcher_owner": tray.as_ref().map(|t| opt(t.watcher_owner())).unwrap_or(Value::Null),
            "tray_items": tray.as_ref().map(|t| t.items().len()).unwrap_or(0),
            "indicators": names,
            "indicator_states": indicator_states,
            "status_errors": status.as_ref().map(|s| {
                Value::Object(s.errors().into_iter().map(|(name, error)| (name, Value::from(error))).collect())
            }).unwrap_or_else(|| json!({})),
        }));
        Value::Object(result)
    }

    /// `json.dumps(result) + "\n"` and the exit status (0 if ready and docks).
    pub fn check_line(&self) -> (String, i32) {
        let value = self.check_value();
        let ok = value["ready"] == json!(true) && value["docks"].as_u64().unwrap_or(0) > 0;
        (json_dumps(&value, &JsonStyle::DEFAULT) + "\n", if ok { 0 } else { 1 })
    }
}
