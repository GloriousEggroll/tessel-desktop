//! Pieces the shared crates do not offer yet (candidates to upstream).
//!
//! `AppCatalog`: the identity rules of tessel-dock/app_catalog.py `resolve`
//! (exact id, unique stem, unique StartupWMClass, unique non-reverse-DNS
//! basename) and its icon fallback. Tessel Shell's port has a private copy in
//! components/shell/src/app_catalog.rs; both belong in a shared crate.

use gio::prelude::*;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct AppRecord {
    pub id: String,
    pub startup_wm_class: String,
    pub hidden: bool,
    pub icon: Option<gio::Icon>,
}

impl AppRecord {
    pub fn from_app_info(info: &gio::AppInfo) -> Option<Self> {
        let desktop = info.downcast_ref::<gio_unix::DesktopAppInfo>();
        Some(Self {
            id: info.id()?.to_string(),
            startup_wm_class: desktop.and_then(|d| d.startup_wm_class()).map(|s| s.to_string()).unwrap_or_default(),
            hidden: desktop.is_some_and(|d| d.is_hidden()),
            icon: info.icon(),
        })
    }
}

type Index = HashMap<String, Vec<String>>;

#[derive(Default)]
pub struct AppCatalog {
    apps: HashMap<String, AppRecord>,
    exact: Index,
    classes: Index,
    basenames: Index,
}

fn stem(value: &str) -> &str { value.strip_suffix(".desktop").unwrap_or(value) }

fn casefold(value: &str) -> String { tessel_core::pyjson::casefold(value) }

fn add(index: &mut Index, key: &str, id: &str) {
    if key.is_empty() {
        return;
    }
    let ids = index.entry(casefold(key)).or_default();
    if !ids.iter().any(|known| known == id) {
        ids.push(id.to_owned());
    }
}

impl AppCatalog {
    /// `AppCatalog()`: every `Gio.AppInfo.get_all()` entry.
    pub fn new() -> Self {
        Self::with_records(gio::AppInfo::all().iter().filter_map(AppRecord::from_app_info).collect())
    }

    pub fn with_records(records: Vec<AppRecord>) -> Self {
        let mut catalog = Self::default();
        let mut order = Vec::new();
        for mut record in records {
            if record.id.is_empty() || !record.id.ends_with(".desktop") || record.hidden {
                continue;
            }
            if record.icon.is_none() {
                record.icon = Some(gio::ThemedIcon::new("application-x-executable-symbolic").upcast());
            }
            if !catalog.apps.contains_key(&record.id) {
                order.push(record.id.clone());
            }
            catalog.apps.insert(record.id.clone(), record);
        }
        for id in &order {
            let app = &catalog.apps[id];
            add(&mut catalog.exact, &app.id, id);
            add(&mut catalog.exact, stem(&app.id), id);
            add(&mut catalog.classes, &app.startup_wm_class, id);
            add(&mut catalog.basenames, stem(&app.id).rsplit('.').next().unwrap_or(""), id);
        }
        catalog
    }

    fn unique(&self, index: &Index, value: &str) -> Option<&AppRecord> {
        match index.get(&casefold(value)) {
            Some(ids) if ids.len() == 1 => self.apps.get(&ids[0]),
            _ => None,
        }
    }

    /// Match stable IDs/classes, refusing ambiguous basename guesses.
    pub fn resolve(&self, app_id: &str) -> Option<&AppRecord> {
        let value = tessel_core::pyjson::py_strip(app_id);
        if value.is_empty() {
            return None;
        }
        if let Some(exact) = self.apps.get(value).or_else(|| self.apps.get(&format!("{value}.desktop"))) {
            return Some(exact);
        }
        if let Some(found) = self.unique(&self.exact, value).or_else(|| self.unique(&self.classes, value)) {
            return Some(found);
        }
        let name = Path::new(value).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let candidate = stem(&name);
        if !candidate.contains('.') {
            return self.unique(&self.basenames, candidate);
        }
        None
    }
}
