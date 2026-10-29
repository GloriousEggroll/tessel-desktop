//! Pure application grouping and pointer state for the dock (port of
//! dock_model.py). No GTK, timing, display or window operations.
//!
//! API:
//! - [`WindowRecord`]: what the model reads from a window (implement it for
//!   the window backend's record; [`WindowData`] is a plain implementation).
//! - [`CatalogLookup`]: `get`/`resolve` of [`AppEntry`] (implemented by
//!   [`AppCatalog`]).
//! - [`DockItem<W>`] (`id, app, app_id, name, pinned, windows` + `count`,
//!   `running`, `focused`, `desktop_id`), [`UTILITY_IDS`] / [`utility_id`],
//!   [`utility_partition`] with [`UtilityOptions`], [`DockModel`] (`update`),
//!   [`HoverState`] (`enter/leave/tick/set_hide_on_fullscreen/clear`,
//!   `hovered/active/promoted/layer`).

use std::collections::{HashMap, HashSet};

use suite_core::pyjson::{casefold, py_strip};

use super::app_catalog::{AppCatalog, AppEntry};

/// A window as the dock model sees it (Python accepted records or dicts).
pub trait WindowRecord: Clone {
    fn id(&self) -> String;
    fn app_id(&self) -> String;
    fn title(&self) -> String;
    fn activated(&self) -> bool;
    fn minimized(&self) -> bool;
    /// Parent window id, `""` for none.
    fn parent_id(&self) -> String;
    /// Optional location metadata (`uri` or `location`), `""` if unknown.
    fn uri(&self) -> String {
        String::new()
    }
}

/// Plain window record (the Python tests' `Window` dataclass).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowData {
    pub id: String,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub outputs: Vec<String>,
    pub parent_id: String,
    pub uri: String,
}

impl WindowData {
    pub fn new(id: &str, app_id: &str, title: &str) -> Self {
        Self { id: id.into(), app_id: app_id.into(), title: title.into(), outputs: vec!["DP-1".into()], ..Default::default() }
    }
}

impl WindowRecord for WindowData {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn app_id(&self) -> String {
        self.app_id.clone()
    }
    fn title(&self) -> String {
        self.title.clone()
    }
    fn activated(&self) -> bool {
        self.activated
    }
    fn minimized(&self) -> bool {
        self.minimized
    }
    fn parent_id(&self) -> String {
        self.parent_id.clone()
    }
    fn uri(&self) -> String {
        self.uri.clone()
    }
}

/// `catalog.get(id)` / `catalog.resolve(id)`.
pub trait CatalogLookup {
    fn get(&self, desktop_id: &str) -> Option<AppEntry>;
    fn resolve(&self, value: &str) -> Option<AppEntry>;
}

impl CatalogLookup for AppCatalog {
    fn get(&self, desktop_id: &str) -> Option<AppEntry> {
        AppCatalog::get(self, desktop_id).cloned()
    }
    fn resolve(&self, value: &str) -> Option<AppEntry> {
        AppCatalog::resolve(self, value, None).cloned()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DockItem<W> {
    pub id: String,
    pub app: Option<AppEntry>,
    pub app_id: String,
    pub name: String,
    pub pinned: bool,
    pub windows: Vec<W>,
}

impl<W: WindowRecord> DockItem<W> {
    pub fn count(&self) -> usize {
        self.windows.len()
    }
    pub fn running(&self) -> bool {
        !self.windows.is_empty()
    }
    pub fn focused(&self) -> bool {
        self.windows.iter().any(|w| w.activated() && !w.minimized())
    }
    /// A pinnable desktop ID, or `""` for an unknown app.
    pub fn desktop_id(&self) -> String {
        match &self.app {
            Some(app) => app.id.clone(),
            None if self.pinned => self.id.clone(),
            None => String::new(),
        }
    }
}

pub const UTILITY_ROLES: &[&str] = &["home", "settings", "trash"];

/// `UTILITY_IDS[role]` = `"utility:<role>"`.
pub const UTILITY_IDS: &[(&str, &str)] = &[("home", "utility:home"), ("settings", "utility:settings"), ("trash", "utility:trash")];

pub fn utility_id(role: &str) -> String {
    format!("utility:{role}")
}

fn application_key(value: &str) -> String {
    let folded = casefold(value);
    folded.strip_suffix(".desktop").map(str::to_owned).unwrap_or(folded)
}

/// Keyword arguments of `utility_partition`.
#[derive(Clone, Debug)]
pub struct UtilityOptions {
    pub file_managers: Vec<String>,
    pub trash_managers: Vec<String>,
    pub enabled: bool,
    pub trash_titles: Vec<String>,
    /// role → display name (defaults: Home folder / Global Settings / Trash).
    pub names: Option<Vec<(String, String)>>,
}

impl Default for UtilityOptions {
    fn default() -> Self {
        Self {
            file_managers: Vec::new(),
            trash_managers: Vec::new(),
            enabled: true,
            trash_titles: ["Trash", "Wastebin", "Rubbish Bin", "Recycle Bin"].map(String::from).to_vec(),
            names: None,
        }
    }
}

/// Utility items keyed by role (`home`, `settings`, `trash`).
pub type Utilities<W> = Vec<(String, DockItem<W>)>;

pub fn utility<'a, W>(utilities: &'a Utilities<W>, role: &str) -> &'a DockItem<W> {
    &utilities.iter().find(|(r, _)| r == role).expect("utility role").1
}

/// Route Home/Settings/Trash windows once, independently per dock/output.
/// Only an explicit trash URI or a whole leading title segment identifies
/// Trash. With `enabled = false` every original item is returned untouched.
pub fn utility_partition<W: WindowRecord>(items: &[DockItem<W>], options: &UtilityOptions) -> (Vec<DockItem<W>>, Utilities<W>) {
    let names: Vec<(String, String)> = options.names.clone().unwrap_or_else(|| {
        vec![("home".into(), "Home folder".into()), ("settings".into(), "Global Settings".into()), ("trash".into(), "Trash".into())]
    });
    let name_of = |role: &str| names.iter().find(|(r, _)| r == role).map(|(_, n)| n.clone()).unwrap_or_default();
    let placeholder = |role: &str| DockItem { id: utility_id(role), app: None, app_id: String::new(), name: name_of(role), pinned: true, windows: Vec::new() };
    let mut roles: Utilities<W> = UTILITY_ROLES.iter().map(|r| (r.to_string(), placeholder(r))).collect();
    if !options.enabled {
        return (items.to_vec(), roles);
    }
    let file_ids: HashSet<String> = options.file_managers.iter().filter(|v| !v.is_empty()).map(|v| application_key(v)).collect();
    let mut trash_ids: HashSet<String> = options.trash_managers.iter().filter(|v| !v.is_empty()).map(|v| application_key(v)).collect();
    if trash_ids.is_empty() {
        trash_ids = file_ids.clone();
    }
    let settings_ids: HashSet<String> = ["org.waydock.globalsettings", "global-settings"].map(String::from).into();
    let title_names: HashSet<String> = options.trash_titles.iter().filter(|v| !v.is_empty()).map(|v| casefold(py_strip(v))).collect();
    let windows: HashMap<String, W> = items.iter().flat_map(|i| i.windows.iter()).map(|w| (w.id(), w.clone())).collect();
    let separator = regex::Regex::new(r"\s+[—–-]\s+").expect("regex");

    fn trash_window<W: WindowRecord>(
        window: &W,
        seen: &HashSet<String>,
        windows: &HashMap<String, W>,
        titles: &HashSet<String>,
        separator: &regex::Regex,
    ) -> bool {
        let window_id = window.id();
        if seen.contains(&window_id) {
            return false;
        }
        let uri = window.uri();
        let title = casefold(py_strip(&window.title()));
        let leading = py_strip(separator.splitn(&title, 2).next().unwrap_or("")).to_owned();
        if uri.to_lowercase().starts_with("trash:") || title.starts_with("trash:/") || titles.contains(&leading) {
            return true;
        }
        let parent = window.parent_id();
        match windows.get(&parent) {
            Some(parent_window) => {
                let mut seen = seen.clone();
                seen.insert(window_id);
                trash_window(parent_window, &seen, windows, titles, separator)
            }
            None => false,
        }
    }

    let mut grouped: HashMap<&str, Vec<W>> = UTILITY_ROLES.iter().map(|r| (*r, Vec::new())).collect();
    let mut apps: HashMap<&str, AppEntry> = HashMap::new();
    let mut app_ids: HashMap<&str, String> = HashMap::new();
    let mut original = Vec::new();
    for item in items {
        let mut keys: HashSet<String> = [application_key(&item.app_id), application_key(&item.id)].into();
        if let Some(app) = &item.app {
            keys.insert(application_key(&app.id));
        }
        let settings = !keys.is_disjoint(&settings_ids);
        let home = !keys.is_disjoint(&file_ids);
        let trash = !keys.is_disjoint(&trash_ids);
        if !(settings || home || trash) {
            original.push(item.clone());
            continue;
        }
        let mut remaining = Vec::new();
        let mut claimed: HashSet<&str> = HashSet::new();
        for window in &item.windows {
            let role = if settings {
                Some("settings")
            } else if trash && trash_window(window, &HashSet::new(), &windows, &title_names, &separator) {
                Some("trash")
            } else if home {
                Some("home")
            } else {
                None
            };
            let Some(role) = role else {
                remaining.push(window.clone());
                continue;
            };
            claimed.insert(role);
            grouped.get_mut(role).unwrap().push(window.clone());
            if let Some(app) = &item.app {
                apps.entry(role).or_insert_with(|| app.clone());
            }
            app_ids.entry(role).or_insert_with(|| item.app_id.clone());
        }
        // Utility buttons replace a redundant explicit pin; a different trash
        // handler keeps its non-Trash windows normally.
        if !remaining.is_empty() || (!settings && !home && claimed.is_empty()) {
            original.push(DockItem { windows: remaining, ..item.clone() });
        }
        if settings || home {
            let role = if settings { "settings" } else { "home" };
            if let Some(app) = &item.app {
                apps.entry(role).or_insert_with(|| app.clone());
            }
            app_ids.entry(role).or_insert_with(|| item.app_id.clone());
        }
    }
    for (role, item) in roles.iter_mut() {
        let role = role.as_str();
        item.app = apps.get(role).cloned();
        item.app_id = app_ids.get(role).cloned().unwrap_or_default();
        item.windows = grouped.remove(role).unwrap_or_default();
    }
    (original, roles)
}

/// Combine ordered pins with running windows without duplicate icons.
/// Running groups and their windows keep first-seen order; pin order follows
/// the configuration; closing the last window removes an unpinned group.
#[derive(Debug)]
pub struct DockModel<W> {
    pub items: Vec<DockItem<W>>,
    group_order: Vec<String>,
    window_order: Vec<String>,
}

impl<W> Default for DockModel<W> {
    fn default() -> Self {
        Self { items: Vec::new(), group_order: Vec::new(), window_order: Vec::new() }
    }
}

impl<W: WindowRecord> DockModel<W> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, pins: &[String], windows: &[W], catalog: &dyn CatalogLookup) -> &[DockItem<W>] {
        let mut by_id: Vec<(String, W)> = Vec::new();
        for window in windows {
            let id = window.id();
            match by_id.iter_mut().find(|(k, _)| *k == id) {
                Some(slot) => slot.1 = window.clone(),
                None => by_id.push((id, window.clone())),
            }
        }
        let lookup: HashMap<&str, &W> = by_id.iter().map(|(k, w)| (k.as_str(), w)).collect();
        self.window_order.retain(|k| lookup.contains_key(k.as_str()));
        let known: HashSet<String> = self.window_order.iter().cloned().collect();
        self.window_order.extend(by_id.iter().map(|(k, _)| k.clone()).filter(|k| !known.contains(k)));

        let mut pinned: Vec<(String, AppEntry)> = Vec::new();
        for pin in pins.iter().filter(|p| !p.is_empty()) {
            // Config stores canonical IDs; resolve legacy aliases if present.
            if let Some(app) = catalog.get(pin).or_else(|| catalog.resolve(pin)) {
                if !pinned.iter().any(|(id, _)| *id == app.id) {
                    pinned.push((app.id.clone(), app));
                }
            }
        }

        type Identity = (String, Option<AppEntry>, String);
        fn identity<W: WindowRecord>(
            window_id: &str,
            seen: &HashSet<String>,
            lookup: &HashMap<&str, &W>,
            catalog: &dyn CatalogLookup,
            cache: &mut HashMap<String, Identity>,
        ) -> Identity {
            if let Some(found) = cache.get(window_id) {
                return found.clone();
            }
            let window = lookup[window_id];
            let app_id = window.app_id();
            let app = if app_id.is_empty() { None } else { catalog.resolve(&app_id) };
            let result = if let Some(app) = app {
                (app.id.clone(), Some(app), app_id)
            } else if !app_id.is_empty() {
                // Missing launchers have no placeholder; the window still runs.
                (format!("app:{app_id}"), None, app_id)
            } else {
                let parent = window.parent_id();
                if lookup.contains_key(parent.as_str()) && !seen.contains(&parent) && parent != window_id {
                    let mut seen = seen.clone();
                    seen.insert(window_id.to_owned());
                    identity(&parent, &seen, lookup, catalog, cache)
                } else {
                    (format!("window:{window_id}"), None, String::new())
                }
            };
            cache.insert(window_id.to_owned(), result.clone());
            result
        }

        let mut cache = HashMap::new();
        let mut grouped: Vec<(String, Vec<W>)> = Vec::new();
        let mut apps: HashMap<String, Option<AppEntry>> = pinned.iter().map(|(k, a)| (k.clone(), Some(a.clone()))).collect();
        let mut app_ids: HashMap<String, String> = HashMap::new();
        for window_id in &self.window_order {
            let (key, app, app_id) = identity(window_id, &HashSet::new(), &lookup, catalog, &mut cache);
            let window = (*lookup[window_id.as_str()]).clone();
            match grouped.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1.push(window),
                None => grouped.push((key.clone(), vec![window])),
            }
            if app.is_some() || !apps.contains_key(&key) {
                apps.insert(key.clone(), app);
            }
            app_ids.entry(key).or_insert(app_id);
        }

        let is_pinned = |key: &str| pinned.iter().any(|(k, _)| k == key);
        let present = |key: &str| is_pinned(key) || grouped.iter().any(|(k, _)| k == key);
        self.group_order.retain(|k| present(k));
        let known: HashSet<String> = self.group_order.iter().cloned().collect();
        self.group_order.extend(grouped.iter().map(|(k, _)| k.clone()).filter(|k| !known.contains(k)));
        let ordered: Vec<String> =
            pinned.iter().map(|(k, _)| k.clone()).chain(self.group_order.iter().filter(|k| !is_pinned(k)).cloned()).collect();
        let mut result = Vec::new();
        for key in ordered {
            let app = apps.get(&key).cloned().flatten();
            let group_windows = grouped.iter().find(|(k, _)| *k == key).map(|(_, w)| w.clone()).unwrap_or_default();
            let app_id = app_ids.get(&key).cloned().unwrap_or_default();
            let name = if let Some(app) = &app {
                app.name.clone()
            } else if !app_id.is_empty() {
                app_id.clone()
            } else if let Some(first) = group_windows.first() {
                let title = first.title();
                if title.is_empty() { "Application".into() } else { title }
            } else {
                key.strip_suffix(".desktop").unwrap_or(&key).to_owned()
            };
            let pinned_flag = is_pinned(&key);
            result.push(DockItem { id: key, app, app_id, name, pinned: pinned_flag, windows: group_windows });
        }
        self.items = result;
        &self.items
    }
}

/// Per-dock pointer/menu holds with a deterministic delayed layer change.
/// Pass monotonic seconds; schedule `tick()` at `deadline` and apply `layer`.
#[derive(Clone, Debug)]
pub struct HoverState {
    pub hide_on_fullscreen: bool,
    pub leave_delay: f64,
    pub deadline: Option<f64>,
    zones: HashSet<String>,
}

impl HoverState {
    /// Python `HoverState(hide_on_fullscreen=True, leave_delay=0.25)`.
    pub fn new(hide_on_fullscreen: bool, leave_delay: f64) -> Result<Self, String> {
        if leave_delay < 0.0 {
            return Err("The leave delay cannot be negative".into());
        }
        Ok(Self { hide_on_fullscreen, leave_delay, deadline: None, zones: HashSet::new() })
    }

    pub fn hovered(&self) -> bool {
        !self.zones.is_empty()
    }
    /// Pointer/menu activity, including the leave grace period.
    pub fn active(&self) -> bool {
        self.hovered() || self.deadline.is_some()
    }
    pub fn promoted(&self) -> bool {
        !self.hide_on_fullscreen || self.active()
    }
    pub fn layer(&self) -> &'static str {
        if self.promoted() { "top" } else { "bottom" }
    }
    pub fn enter(&mut self, zone: &str, _now: f64) -> bool {
        self.zones.insert(zone.to_owned());
        self.deadline = None;
        self.promoted()
    }
    pub fn leave(&mut self, zone: &str, now: f64) -> bool {
        if !self.zones.remove(zone) {
            return self.tick(now);
        }
        if self.zones.is_empty() {
            self.deadline = Some(now + self.leave_delay);
        }
        self.tick(now)
    }
    pub fn tick(&mut self, now: f64) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
        }
        self.promoted()
    }
    pub fn set_hide_on_fullscreen(&mut self, enabled: bool) -> bool {
        self.hide_on_fullscreen = enabled;
        self.promoted()
    }
    /// Drop holds when the dock is rebuilt or its monitor disappears.
    pub fn clear(&mut self) -> bool {
        self.zones.clear();
        self.deadline = None;
        self.promoted()
    }
}

impl Default for HoverState {
    fn default() -> Self {
        Self::new(true, 0.25).expect("valid default")
    }
}
