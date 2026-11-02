//! The parts of tessel-dock/app_catalog.py that tessel-shell uses: the desktop-entry
//! index, `get`, `resolve` (switcher card names/icons) and `default_browser`.
//! Launching lives in `desktop::launch` (systemd-run, labwc environment).
//! Candidate to share with the tessel-dock port.

use gio::prelude::*;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub icon: Option<gio::Icon>,
    pub startup_wm_class: String,
    pub filename: String,
    pub visible: bool,
    pub info: Option<gio::AppInfo>,
}

/// One application description as the catalog sees it (tests feed these
/// directly, like Python's `AppCatalog(infos=…)`).
#[derive(Clone, Debug, Default)]
pub struct InfoRecord {
    pub id: String,
    pub name: String,
    pub icon: Option<gio::Icon>,
    pub startup_wm_class: String,
    pub filename: String,
    pub hidden: bool,
    pub visible: bool,
    pub info: Option<gio::AppInfo>,
}

impl InfoRecord {
    pub fn from_app_info(info: &gio::AppInfo) -> Option<Self> {
        let id = info.id()?.to_string();
        let desktop = info.downcast_ref::<gio_unix::DesktopAppInfo>();
        Some(Self {
            name: {
                let d = info.display_name().to_string();
                if !d.is_empty() { d } else { let n = info.name().to_string(); if !n.is_empty() { n } else { stem(&id).to_string() } }
            },
            icon: info.icon(),
            startup_wm_class: desktop.and_then(|d| d.startup_wm_class()).map(|s| s.to_string()).unwrap_or_default(),
            filename: desktop.and_then(|d| d.filename()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            hidden: desktop.map(|d| d.is_hidden()).unwrap_or(false),
            visible: info.should_show(),
            info: Some(info.clone()),
            id,
        })
    }
}

pub fn stem(value: &str) -> &str {
    value.strip_suffix(".desktop").unwrap_or(value)
}

type Index = HashMap<String, Vec<String>>; // casefold key -> ids in insertion order (dict semantics)

#[derive(Default)]
pub struct AppCatalog {
    source: Option<Vec<InfoRecord>>,
    pub apps: HashMap<String, AppEntry>,
    exact: Index,
    classes: Index,
    basenames: Index,
}

fn casefold(value: &str) -> String {
    tessel_core::pyjson::casefold(value)
}

fn index(index: &mut Index, key: &str, id: &str) {
    if key.is_empty() {
        return;
    }
    let ids = index.entry(casefold(key)).or_default();
    if !ids.iter().any(|known| known == id) {
        ids.push(id.to_string());
    }
}

impl AppCatalog {
    /// `AppCatalog()` — indexes `Gio.AppInfo.get_all()`.
    pub fn new() -> Self {
        let mut catalog = Self::default();
        catalog.refresh();
        catalog
    }

    /// `AppCatalog(infos=…)`.
    pub fn with_infos(infos: Vec<InfoRecord>) -> Self {
        let mut catalog = Self { source: Some(infos), ..Self::default() };
        catalog.refresh();
        catalog
    }

    pub fn refresh(&mut self) {
        let records: Vec<InfoRecord> = match &self.source {
            Some(list) => list.clone(),
            None => gio::AppInfo::all().iter().filter_map(InfoRecord::from_app_info).collect(),
        };
        let mut apps = HashMap::new();
        let mut order = Vec::new();
        for record in records {
            if record.id.is_empty() || !record.id.ends_with(".desktop") || record.hidden {
                continue;
            }
            let icon = record.icon.clone().or_else(|| Some(gio::ThemedIcon::new("application-x-executable-symbolic").upcast()));
            if !apps.contains_key(&record.id) {
                order.push(record.id.clone());
            }
            apps.insert(record.id.clone(), AppEntry {
                id: record.id.clone(), name: record.name.clone(), icon,
                startup_wm_class: record.startup_wm_class.clone(), filename: record.filename.clone(),
                visible: record.visible, info: record.info.clone(),
            });
        }
        self.exact.clear();
        self.classes.clear();
        self.basenames.clear();
        for id in &order {
            let app = &apps[id];
            index(&mut self.exact, &app.id, id);
            index(&mut self.exact, stem(&app.id), id);
            index(&mut self.classes, &app.startup_wm_class, id);
            let base = stem(&app.id).rsplit('.').next().unwrap_or("");
            index(&mut self.basenames, base, id);
        }
        self.apps = apps;
    }

    pub fn get(&self, desktop_id: &str) -> Option<&AppEntry> {
        self.apps.get(desktop_id)
    }

    pub fn default_browser(&self) -> Option<&AppEntry> {
        let info = gio::AppInfo::default_for_uri_scheme("https").or_else(|| gio::AppInfo::default_for_uri_scheme("http"))?;
        self.get(info.id()?.as_str())
    }

    fn unique(&self, index: &Index, value: &str) -> Option<&AppEntry> {
        match index.get(&casefold(value)) {
            Some(ids) if ids.len() == 1 => self.apps.get(&ids[0]),
            _ => None,
        }
    }

    /// Match stable IDs/classes, refusing ambiguous basename guesses.
    pub fn resolve(&self, app_id: Option<&str>, wm_class: Option<&str>) -> Option<&AppEntry> {
        let values: Vec<&str> = [app_id, wm_class].into_iter().flatten().map(str::trim).filter(|v| !v.is_empty()).collect();
        for value in &values {
            if let Some(exact) = self.apps.get(*value).or_else(|| self.apps.get(&format!("{value}.desktop"))) {
                return Some(exact);
            }
            if let Some(found) = self.unique(&self.exact, value) {
                return Some(found);
            }
        }
        for value in &values {
            if let Some(found) = self.unique(&self.classes, value) {
                return Some(found);
            }
        }
        for value in &values {
            let name = Path::new(value).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let candidate = stem(&name);
            if !candidate.contains('.') {
                if let Some(found) = self.unique(&self.basenames, candidate) {
                    return Some(found);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, name: &str, class: &str) -> InfoRecord {
        InfoRecord { id: id.into(), name: name.into(), startup_wm_class: class.into(), visible: true, ..Default::default() }
    }

    #[test]
    fn resolve_exact_class_and_unique_basename() {
        let catalog = AppCatalog::with_infos(vec![
            record("org.example.Terminal.desktop", "Terminal", ""),
            record("firefox.desktop", "Firefox", "firefox-esr"),
            record("org.a.Editor.desktop", "A", ""),
            record("org.b.Editor.desktop", "B", ""),
        ]);
        assert_eq!(catalog.resolve(Some("org.example.Terminal"), None).unwrap().name, "Terminal");
        assert_eq!(catalog.resolve(Some("FIREFOX"), None).unwrap().name, "Firefox");
        assert_eq!(catalog.resolve(Some("firefox-esr"), None).unwrap().name, "Firefox");
        assert_eq!(catalog.resolve(Some("terminal"), None).unwrap().name, "Terminal");
        // reverse-DNS ids are authoritative: no basename collapse
        assert!(catalog.resolve(Some("org.other.Terminal"), None).is_none());
        // ambiguous basename
        assert!(catalog.resolve(Some("editor"), None).is_none());
        assert!(catalog.resolve(Some("  "), None).is_none());
    }
}
