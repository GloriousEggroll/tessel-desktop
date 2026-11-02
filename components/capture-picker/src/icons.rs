//! Resolve window application icons without changing opaque portal choices
//! (capture_icons.py).

use crate::compat::AppCatalog;
use crate::sources::{display_text, CaptureSource, Kind};
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

/// Token → icon for window sources whose title maps to exactly one app id.
pub fn icons_from_snapshot(sources: &[CaptureSource], windows: &[Map<String, Value>], catalog: &dyn Fn() -> AppCatalog)
                           -> HashMap<String, gio::Icon> {
    let mut titles: HashMap<String, BTreeSet<String>> = HashMap::new();
    for item in windows {
        let app_id = match item.get("app_id") {
            Some(Value::String(id)) if !id.is_empty() => id.clone(),
            _ => continue,
        };
        if item.get("event").and_then(Value::as_str) != Some("window") {
            continue;
        }
        let title = match item.get("title") {
            Some(Value::String(title)) => title.as_str(),
            _ => "",
        };
        titles.entry(display_text(title)).or_default().insert(app_id);
    }
    let mut icons = HashMap::new();
    let mut resolved: Option<AppCatalog> = None;
    for source in sources {
        let Some(ids) = titles.get(&source.title) else { continue };
        if source.kind != Kind::Window || ids.len() != 1 {
            continue;
        }
        let catalog = resolved.get_or_insert_with(catalog);
        if let Some(icon) = catalog.resolve(ids.iter().next().unwrap()).and_then(|app| app.icon.clone()) {
            icons.insert(source.token.clone(), icon);
        }
    }
    icons
}

/// `window_icons(sources)`: the helper snapshot (0.8 s) matched by title.
pub fn window_icons(sources: &[CaptureSource]) -> HashMap<String, gio::Icon> {
    let Some(helper) = crate::output::helper() else { return HashMap::new() };
    match crate::output::snapshot(&helper, Duration::from_millis(800)) {
        Some(windows) => icons_from_snapshot(sources, &windows, &AppCatalog::new),
        None => HashMap::new(),
    }
}
