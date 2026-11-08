//! Desktop application metadata and launches through Gio, never shell Exec
//! (port of app_catalog.py; contract §5 "App launch").
//!
//! API:
//! - [`AppEntry`] (`id, name, icon, startup_wm_class, filename, visible,
//!   description, info, categories, keywords, generic_name, executable`;
//!   equality ignores `icon`/`info` like the dataclass), [`AppCategory`],
//!   [`MENU_CATEGORIES`], [`menu_categories_for`].
//! - [`AppInfoData`]: the metadata `AppCatalog.refresh` reads from an
//!   `AppInfo` ([`AppInfoData::from_gio`]; [`AppInfoData::fake`] for tests).
//! - [`AppCatalog`]: `new()` (all installed apps), `from_infos`, `with_source`,
//!   `refresh`, `apps`, `get`, `menu_apps`, `menu_categories`,
//!   `apps_in_category`, `search` (ranked + typo tolerant), `resolve(app_id,
//!   wm_class)`, `default_browser`, `launch(&Launcher, …)`; implements
//!   `config::PinCatalog` and `model::CatalogLookup`.
//! - [`LaunchHistory`] (`launch-counts.json`, Frequent row).
//! - Launching: [`launch_command`] (exact systemd-run argv), [`SERVICE_ENV`],
//!   [`launch_environment`], the [`LaunchContext`] trait (implemented for
//!   `gio::AppLaunchContext`, so a `gdk::AppLaunchContext` upcast works),
//!   [`Launcher`] (`launch_separately`, `launch_process`,
//!   `launch_desktop_file`, `launch_uri`) over an injectable
//!   [`Spawner`](super::spawn::Spawner). The helper argv defaults to
//!   `tessel-dock-launch <mode> <target>`; [`run_launch_helper`] /
//!   [`launch_helper_main`] are its implementation.

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use glib::translate::{from_glib, from_glib_full, ToGlibPtr};
use gio::prelude::*;
use serde_json::Value;
use tessel_core::pyjson::casefold;

use super::ordered::OrderedMap;
use super::spawn::{GioSpawner, Spawner};

// ---- GDesktopAppInfo (GioUnix, not wrapped by gio-rs 0.22) --------------

extern "C" {
    fn g_desktop_app_info_get_type() -> glib::ffi::GType;
    fn g_desktop_app_info_new_from_filename(filename: *const c_char) -> *mut gio::ffi::GAppInfo;
    fn g_desktop_app_info_new_from_keyfile(key_file: *mut glib::ffi::GKeyFile) -> *mut gio::ffi::GAppInfo;
    fn g_desktop_app_info_get_boolean(info: *mut gio::ffi::GAppInfo, key: *const c_char) -> glib::ffi::gboolean;
    fn g_desktop_app_info_get_startup_wm_class(info: *mut gio::ffi::GAppInfo) -> *const c_char;
    fn g_desktop_app_info_get_filename(info: *mut gio::ffi::GAppInfo) -> *const c_char;
    fn g_desktop_app_info_get_categories(info: *mut gio::ffi::GAppInfo) -> *const c_char;
    fn g_desktop_app_info_get_keywords(info: *mut gio::ffi::GAppInfo) -> *const *const c_char;
    fn g_desktop_app_info_get_generic_name(info: *mut gio::ffi::GAppInfo) -> *const c_char;
    fn g_desktop_app_info_get_is_hidden(info: *mut gio::ffi::GAppInfo) -> glib::ffi::gboolean;
}

fn is_desktop_info(info: &gio::AppInfo) -> bool {
    let wanted: glib::Type = unsafe { from_glib(g_desktop_app_info_get_type()) };
    info.type_().is_a(wanted)
}

fn optional_text(pointer: *const c_char) -> Option<String> {
    (!pointer.is_null()).then(|| unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned())
}

/// `DesktopAppInfo.new_from_filename(path)`.
pub fn desktop_info_from_filename(path: &str) -> Option<gio::AppInfo> {
    let path = CString::new(path).ok()?;
    unsafe { from_glib_full(g_desktop_app_info_new_from_filename(path.as_ptr())) }
}

/// `DesktopAppInfo.new_from_keyfile(keyfile)`.
pub fn desktop_info_from_keyfile(keyfile: &glib::KeyFile) -> Option<gio::AppInfo> {
    unsafe { from_glib_full(g_desktop_app_info_new_from_keyfile(keyfile.to_glib_none().0)) }
}

/// `DesktopAppInfo.new(desktop_id)`.
pub fn desktop_info(desktop_id: &str) -> Option<gio::AppInfo> {
    if desktop_id.contains('\0') {
        return None;
    }
    tessel_core::glib_compat::desktop_app_info(desktop_id)
}

/// `getattr(info, "get_boolean", lambda _key: False)(key)`.
pub fn desktop_boolean(info: &gio::AppInfo, key: &str) -> bool {
    if !is_desktop_info(info) {
        return false;
    }
    let key = CString::new(key).unwrap_or_default();
    unsafe { g_desktop_app_info_get_boolean(info.to_glib_none().0, key.as_ptr()) != 0 }
}

// ---- Entries and categories ---------------------------------------------

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub icon: Option<gio::Icon>,
    pub startup_wm_class: String,
    pub filename: String,
    pub visible: bool,
    pub description: String,
    /// Python `_info` (the Gio app info used for launches).
    pub info: Option<gio::AppInfo>,
    pub categories: Vec<String>,
    /// Search-only metadata; never displayed or used for identity.
    pub keywords: Vec<String>,
    pub generic_name: String,
    pub executable: String,
}

impl PartialEq for AppEntry {
    fn eq(&self, other: &Self) -> bool {
        (&self.id, &self.name, &self.startup_wm_class, &self.filename, self.visible, &self.description, &self.categories)
            == (&other.id, &other.name, &other.startup_wm_class, &other.filename, other.visible, &other.description, &other.categories)
            && (&self.keywords, &self.generic_name, &self.executable) == (&other.keywords, &other.generic_name, &other.executable)
    }
}

impl AppEntry {
    /// `AppEntry(id, name)` with the dataclass defaults.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            icon: None,
            startup_wm_class: String::new(),
            filename: String::new(),
            visible: true,
            description: String::new(),
            info: None,
            categories: vec!["Other".into()],
            keywords: Vec::new(),
            generic_name: String::new(),
            executable: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppCategory {
    pub id: &'static str,
    /// English name; translate with `tr!` for display.
    pub name: &'static str,
    pub icon: &'static str,
}

const fn category(id: &'static str, name: &'static str, icon: &'static str) -> AppCategory {
    AppCategory { id, name, icon }
}

/// Stable menu order with the catch-all last. Audio and Video share Multimedia.
pub const MENU_CATEGORIES: &[AppCategory] = &[
    category("Utility", "Accessories", "applications-utilities-symbolic"),
    category("Development", "Development", "applications-development-symbolic"),
    category("Education", "Education", "applications-education-symbolic"),
    category("Game", "Games", "applications-games-symbolic"),
    category("Graphics", "Graphics", "applications-graphics-symbolic"),
    category("HealthFitness", "Health & Fitness", "applications-science-symbolic"),
    category("Network", "Internet", "applications-internet-symbolic"),
    category("AudioVideo", "Multimedia", "applications-multimedia-symbolic"),
    category("Office", "Office", "applications-office-symbolic"),
    category("Science", "Science", "applications-science-symbolic"),
    category("Settings", "Settings", "preferences-system-symbolic"),
    category("System", "System", "applications-system-symbolic"),
    category("Other", "Other", "applications-other-symbolic"),
];

/// `_menu_categories(value)`: top-level menu sections, never submenus.
pub fn menu_categories_for(value: Option<&str>) -> Vec<String> {
    let mut names: Vec<String> =
        value.unwrap_or("").split(';').map(|n| tessel_core::pyjson::py_strip(n).to_owned()).filter(|n| !n.is_empty()).collect();
    if names.iter().any(|n| n == "Audio" || n == "Video") {
        names.push("AudioVideo".into());
    }
    let categories: Vec<String> =
        MENU_CATEGORIES.iter().filter(|c| c.id != "Other" && names.iter().any(|n| n == c.id)).map(|c| c.id.to_owned()).collect();
    if categories.is_empty() { vec!["Other".into()] } else { categories }
}

fn stem(value: &str) -> &str {
    value.strip_suffix(".desktop").unwrap_or(value)
}

/// `PurePath(value).name`.
fn pure_name(value: &str) -> &str {
    value.split('/').filter(|part| !part.is_empty() && *part != ".").next_back().unwrap_or("")
}

/// What `AppCatalog.refresh` reads from one app info.
#[derive(Clone, Debug, Default)]
pub struct AppInfoData {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub name: Option<String>,
    pub icon: Option<gio::Icon>,
    pub startup_wm_class: Option<String>,
    pub filename: Option<String>,
    pub should_show: bool,
    pub description: Option<String>,
    pub categories: Option<String>,
    pub keywords: Vec<String>,
    pub generic_name: Option<String>,
    pub executable: Option<String>,
    pub hidden: bool,
    pub info: Option<gio::AppInfo>,
}

impl AppInfoData {
    pub fn from_gio(info: &gio::AppInfo) -> Self {
        // gio-rs asserts non-NULL for some getters that desktop entries may
        // leave NULL (e.g. no Exec), so read those through the C API.
        let raw: *mut gio::ffi::GAppInfo = info.to_glib_none().0;
        let (display_name, name, executable) = unsafe {
            (
                optional_text(gio::ffi::g_app_info_get_display_name(raw)),
                optional_text(gio::ffi::g_app_info_get_name(raw)),
                optional_text(gio::ffi::g_app_info_get_executable(raw)),
            )
        };
        let mut data = AppInfoData {
            id: info.id().map(|s| s.to_string()),
            display_name,
            name,
            icon: info.icon(),
            should_show: info.should_show(),
            description: info.description().map(|s| s.to_string()),
            executable,
            info: Some(info.clone()),
            ..Default::default()
        };
        if is_desktop_info(info) {
            unsafe {
                data.startup_wm_class = optional_text(g_desktop_app_info_get_startup_wm_class(raw));
                data.filename = optional_text(g_desktop_app_info_get_filename(raw));
                data.categories = optional_text(g_desktop_app_info_get_categories(raw));
                data.generic_name = optional_text(g_desktop_app_info_get_generic_name(raw));
                data.hidden = g_desktop_app_info_get_is_hidden(raw) != 0;
                let mut words = g_desktop_app_info_get_keywords(raw);
                if !words.is_null() {
                    while !(*words).is_null() {
                        data.keywords.push(CStr::from_ptr(*words).to_string_lossy().into_owned());
                        words = words.add(1);
                    }
                }
            }
        }
        data
    }

    /// The Python tests' `FakeInfo(app_id, name)`.
    pub fn fake(id: &str, name: &str) -> Self {
        AppInfoData {
            id: Some(id.into()),
            display_name: Some(name.into()),
            name: Some(name.into()),
            filename: Some(format!("/usr/share/applications/{id}")),
            should_show: true,
            description: Some("Fixture description".into()),
            ..Default::default()
        }
    }

    fn entry(&self, desktop_id: &str) -> AppEntry {
        let nonempty = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
        AppEntry {
            id: desktop_id.to_owned(),
            name: nonempty(&self.display_name).or_else(|| nonempty(&self.name)).unwrap_or_else(|| stem(desktop_id).to_owned()),
            icon: Some(self.icon.clone().unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable-symbolic").upcast())),
            startup_wm_class: self.startup_wm_class.clone().unwrap_or_default(),
            filename: self.filename.clone().unwrap_or_default(),
            visible: self.should_show,
            description: self.description.clone().unwrap_or_default(),
            info: self.info.clone(),
            categories: menu_categories_for(self.categories.as_deref()),
            keywords: self.keywords.clone(),
            generic_name: self.generic_name.clone().unwrap_or_default(),
            executable: self.executable.as_deref().map(pure_name).unwrap_or("").to_owned(),
        }
    }
}

// ---- Fuzzy search ---------------------------------------------------------

/// Damerau distance <= 1, so "fierfox" and "firfox" still find Firefox.
fn within_one_edit(word: &[char], token: &[char]) -> bool {
    if word == token {
        return true;
    }
    if word.len().abs_diff(token.len()) > 1 {
        return false;
    }
    let mut start = 0;
    while start < word.len().min(token.len()) && word[start] == token[start] {
        start += 1;
    }
    let tail = |s: &[char], from: usize| s.get(from..).unwrap_or(&[]).to_vec();
    if word.len() == token.len() {
        let rest = tail(word, start + 1) == tail(token, start + 1);
        let swapped = tail(word, start + 2) == tail(token, start + 2)
            && start + 1 < word.len()
            && word[start] == token[start + 1]
            && word[start + 1] == token[start];
        return rest || swapped;
    }
    let (longer, shorter) = if word.len() > token.len() { (word, token) } else { (token, word) };
    tail(longer, start + 1) == tail(shorter, start)
}

fn subsequence(word: &[char], token: &[char]) -> bool {
    let mut remaining = token.iter();
    word.iter().all(|c| remaining.any(|t| t == c))
}

/// A conservative typo match of `word` against name/keyword tokens.
fn fuzzy_word(word: &str, tokens: &[String]) -> bool {
    let word: Vec<char> = word.chars().collect();
    if word.len() < 3 {
        return false;
    }
    for token in tokens {
        let token: Vec<char> = token.chars().collect();
        if token.len() < 3 || (token[0] != word[0] && word.len() < 5) {
            continue;
        }
        if within_one_edit(&word, &token) || (token.len() > word.len() && within_one_edit(&word, &token[..word.len()])) {
            return true;
        }
        if word.len() >= 4 && token[0] == word[0] && token.len() <= word.len() + 3 && subsequence(&word, &token) {
            return true;
        }
    }
    false
}

fn word_tokens(text: &str) -> Vec<String> {
    thread_local! {
        static WORD: regex::Regex = regex::Regex::new(r"\w+").expect("regex");
    }
    WORD.with(|re| re.find_iter(text).map(|m| m.as_str().to_owned()).collect())
}

/// `str.split()` (whitespace runs).
fn py_split(text: &str) -> Vec<&str> {
    text.split(tessel_core::pyjson::is_py_space).filter(|w| !w.is_empty()).collect()
}

// ---- Launch history -------------------------------------------------------

/// Small per-user launch counter for the menu's Frequent row
/// (`~/.local/state/tessel/dock/launch-counts.json`). Failures are harmless.
pub struct LaunchHistory {
    pub path: PathBuf,
    pub counts: OrderedMap<i64>,
}

impl LaunchHistory {
    pub const LIMIT: usize = 200;

    pub fn default_path() -> PathBuf {
        tessel_core::paths::component_state_dir("dock").join("launch-counts.json")
    }

    pub fn new(path: Option<PathBuf>) -> Self {
        let path = path.unwrap_or_else(Self::default_path);
        let mut counts = OrderedMap::new();
        if let Some(Value::Object(data)) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            for (key, value) in data {
                let count = match value {
                    Value::Bool(true) => Some(1),
                    Value::Number(n) => n.as_i64(),
                    _ => None,
                };
                if let Some(count) = count.filter(|c| *c > 0) {
                    counts.insert(key, count);
                }
            }
        }
        Self { path, counts }
    }

    pub fn record(&mut self, desktop_id: &str) {
        if desktop_id.is_empty() {
            return;
        }
        let current = self.counts.get(desktop_id).copied().unwrap_or(0);
        self.counts.insert(desktop_id, current + 1);
        if self.counts.len() > Self::LIMIT {
            let mut keep: Vec<(String, i64)> = std::mem::take(&mut self.counts).into_entries();
            keep.sort_by_key(|item| -item.1); // stable, like sorted()
            keep.truncate(Self::LIMIT);
            self.counts = keep.into_iter().collect();
        }
        let value = Value::Object(self.counts.iter().map(|(k, v)| (k.to_owned(), Value::from(*v))).collect());
        let style = tessel_core::pyfmt::JsonStyle { sort_keys: true, ..tessel_core::pyfmt::JsonStyle::DEFAULT };
        let text = tessel_core::pyfmt::json_dumps(&value, &style);
        let write = || -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut name = self.path.file_name().unwrap_or_default().to_os_string();
            name.push(".tmp");
            let temporary = self.path.with_file_name(name);
            std::fs::write(&temporary, text.as_bytes())?;
            std::fs::rename(&temporary, &self.path)
        };
        let _ = write();
    }

    /// Visible, still-installed apps ordered by launch count then name.
    pub fn frequent(&self, catalog: &AppCatalog, limit: usize) -> Vec<AppEntry> {
        let mut entries: Vec<(i64, AppEntry)> =
            self.counts.iter().filter_map(|(id, count)| catalog.get(id).filter(|a| a.visible).map(|a| (*count, a.clone()))).collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| casefold(&a.1.name).cmp(&casefold(&b.1.name))).then_with(|| a.1.id.cmp(&b.1.id)));
        entries.into_iter().take(limit).map(|(_, app)| app).collect()
    }
}

// ---- Catalog --------------------------------------------------------------

type Source = Box<dyn Fn() -> Vec<AppInfoData>>;

pub struct AppCatalog {
    source: Source,
    apps: Vec<AppEntry>,
    by_id: std::collections::HashMap<String, usize>,
    exact: std::collections::HashMap<String, Vec<usize>>,
    classes: std::collections::HashMap<String, Vec<usize>>,
    basenames: std::collections::HashMap<String, Vec<usize>>,
    /// `Gio.AppInfo.get_default_for_uri_scheme("https"/"http")` id lookup.
    pub browser_lookup: Box<dyn Fn() -> Option<String>>,
}

fn gio_browser() -> Option<String> {
    gio::AppInfo::default_for_uri_scheme("https")
        .or_else(|| gio::AppInfo::default_for_uri_scheme("http"))
        .and_then(|info| info.id().map(|s| s.to_string()))
}

impl Default for AppCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl AppCatalog {
    /// All installed applications (`Gio.AppInfo.get_all()`).
    pub fn new() -> Self {
        Self::with_source(Box::new(|| gio::AppInfo::all().iter().map(AppInfoData::from_gio).collect()))
    }

    pub fn from_infos(infos: Vec<AppInfoData>) -> Self {
        Self::with_source(Box::new(move || infos.clone()))
    }

    pub fn with_source(source: Source) -> Self {
        let mut catalog = Self {
            source,
            apps: Vec::new(),
            by_id: Default::default(),
            exact: Default::default(),
            classes: Default::default(),
            basenames: Default::default(),
            browser_lookup: Box::new(gio_browser),
        };
        catalog.refresh();
        catalog
    }

    fn index(index: &mut std::collections::HashMap<String, Vec<usize>>, key: &str, app: usize) {
        if !key.is_empty() {
            let slot = index.entry(casefold(key)).or_default();
            if !slot.contains(&app) {
                slot.push(app);
            }
        }
    }

    pub fn refresh(&mut self) -> &[AppEntry] {
        let mut apps: Vec<AppEntry> = Vec::new();
        let mut by_id = std::collections::HashMap::new();
        for info in (self.source)() {
            let Some(desktop_id) = info.id.clone().filter(|id| !id.is_empty() && id.ends_with(".desktop")) else { continue };
            if info.hidden {
                continue;
            }
            let entry = info.entry(&desktop_id);
            match by_id.get(&desktop_id) {
                Some(&index) => apps[index] = entry,
                None => {
                    by_id.insert(desktop_id, apps.len());
                    apps.push(entry);
                }
            }
        }
        self.exact.clear();
        self.classes.clear();
        self.basenames.clear();
        for (index, app) in apps.iter().enumerate() {
            Self::index(&mut self.exact, &app.id, index);
            Self::index(&mut self.exact, stem(&app.id), index);
            Self::index(&mut self.classes, &app.startup_wm_class, index);
            Self::index(&mut self.basenames, stem(&app.id).rsplit('.').next().unwrap_or(""), index);
        }
        self.apps = apps;
        self.by_id = by_id;
        &self.apps
    }

    /// Every entry in discovery order (`catalog.apps.values()`).
    pub fn apps(&self) -> &[AppEntry] {
        &self.apps
    }

    pub fn get(&self, desktop_id: &str) -> Option<&AppEntry> {
        self.by_id.get(desktop_id).map(|&i| &self.apps[i])
    }

    fn alphabetical(a: &AppEntry, b: &AppEntry) -> std::cmp::Ordering {
        casefold(&a.name).cmp(&casefold(&b.name)).then_with(|| a.id.cmp(&b.id))
    }

    pub fn menu_apps(&self) -> Vec<AppEntry> {
        let mut apps: Vec<AppEntry> = self.apps.iter().filter(|a| a.visible).cloned().collect();
        apps.sort_by(Self::alphabetical);
        apps
    }

    /// Only populated sections; hidden launchers never create a section.
    pub fn menu_categories(&self) -> Vec<AppCategory> {
        MENU_CATEGORIES
            .iter()
            .filter(|c| self.apps.iter().any(|a| a.visible && a.categories.iter().any(|x| x == c.id)))
            .copied()
            .collect()
    }

    /// Visible entries in a section; `None`/`""` means All Applications.
    pub fn apps_in_category(&self, category_id: Option<&str>) -> Vec<AppEntry> {
        let entries = self.menu_apps();
        match category_id.filter(|c| !c.is_empty()) {
            None => entries,
            Some(id) => entries.into_iter().filter(|a| a.categories.iter().any(|c| c == id)).collect(),
        }
    }

    pub fn default_browser(&self) -> Option<&AppEntry> {
        (self.browser_lookup)().and_then(|id| self.get(&id))
    }

    fn haystack(app: &AppEntry) -> String {
        casefold(&[app.name.as_str(), &app.id, &app.description, &app.generic_name, &app.keywords.join(" "), &app.executable].join(" "))
    }

    pub fn search(&self, query: &str) -> Vec<AppEntry> {
        let folded = casefold(query);
        let words: Vec<&str> = py_split(&folded);
        let query = words.join(" ");
        if query.is_empty() {
            return self.menu_apps();
        }
        let (mut entries, mut fuzzy) = (Vec::new(), Vec::new());
        for app in self.menu_apps() {
            let haystack = Self::haystack(&app);
            if words.iter().all(|w| haystack.contains(w)) {
                entries.push(app);
                continue;
            }
            let tokens = word_tokens(&casefold(&[app.name.as_str(), &app.generic_name, &app.keywords.join(" "), &app.executable].join(" ")));
            if words.iter().all(|w| haystack.contains(w) || fuzzy_word(w, &tokens)) {
                fuzzy.push(app);
            }
        }
        let relevance = |app: &AppEntry| -> u8 {
            let name_folded = casefold(&app.name);
            let name = py_split(&name_folded).join(" ");
            if name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if words.iter().all(|w| word_tokens(&name).iter().any(|t| t.starts_with(w))) {
                2
            } else if words.iter().all(|w| name.contains(w)) {
                3
            } else if {
                let meta = casefold(&[app.generic_name.as_str(), &app.keywords.join(" "), &app.executable].join(" "));
                words.iter().all(|w| meta.contains(w))
            } {
                4
            } else {
                5
            }
        };
        let mut ranked: Vec<(u8, AppEntry)> = entries.into_iter().map(|a| (relevance(&a), a)).collect();
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| Self::alphabetical(&a.1, &b.1)));
        fuzzy.sort_by(Self::alphabetical);
        ranked.into_iter().map(|(_, a)| a).chain(fuzzy).collect()
    }

    fn unique<'a>(&'a self, index: &std::collections::HashMap<String, Vec<usize>>, value: &str) -> Option<&'a AppEntry> {
        match index.get(&casefold(value)) {
            Some(matches) if matches.len() == 1 => Some(&self.apps[matches[0]]),
            _ => None,
        }
    }

    /// Match stable IDs/classes, refusing ambiguous basename guesses. A
    /// reverse-DNS app ID is authoritative; titles and names are never used.
    pub fn resolve(&self, app_id: &str, wm_class: Option<&str>) -> Option<&AppEntry> {
        let values: Vec<&str> = [Some(app_id), wm_class]
            .into_iter()
            .flatten()
            .map(tessel_core::pyjson::py_strip)
            .filter(|v| !v.is_empty())
            .collect();
        for value in &values {
            if let Some(exact) = self.get(value).or_else(|| self.get(&format!("{value}.desktop"))) {
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
            let candidate = stem(pure_name(value));
            if !candidate.contains('.') {
                if let Some(found) = self.unique(&self.basenames, candidate) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// `catalog.launch(desktop_id, context, on_done)`.
    pub fn launch(
        &self,
        launcher: &Launcher,
        desktop_id: &str,
        context: Option<Rc<dyn LaunchContext>>,
        on_done: Option<DoneFn>,
    ) -> Result<(), LaunchError> {
        let Some(entry) = self.get(desktop_id) else {
            return Err(LaunchError::Value("That application is no longer installed.".into()));
        };
        launcher.launch_separately("--launch-desktop", &entry.id, entry.info.as_ref(), context, on_done).map_err(LaunchError::Glib)
    }
}

impl super::config::PinCatalog for AppCatalog {
    fn get_id(&self, desktop_id: &str) -> Option<String> {
        self.get(desktop_id).map(|a| a.id.clone())
    }
    fn resolve_id(&self, value: &str) -> Option<String> {
        self.resolve(value, None).map(|a| a.id.clone())
    }
    fn default_browser_id(&self) -> Option<String> {
        self.default_browser().map(|a| a.id.clone())
    }
}

// ---- Launching ------------------------------------------------------------

/// Execution metadata belonging to the dock's service, never to an app.
pub const SERVICE_ENV: &[&str] = &[
    "INVOCATION_ID", "JOURNAL_STREAM", "NOTIFY_SOCKET", "WATCHDOG_PID", "WATCHDOG_USEC", "WATCHDOG_DEVICE", "LISTEN_PID",
    "LISTEN_FDS", "LISTEN_FDNAMES", "SYSTEMD_EXEC_PID", "MAINPID", "MANAGERPID", "PIDFILE", "EXIT_CODE", "EXIT_STATUS",
    "SERVICE_RESULT", "GIO_LAUNCHED_DESKTOP_FILE", "GIO_LAUNCHED_DESKTOP_FILE_PID", "TESSEL_DOCK_LAUNCH_STARTUP_ID",
];

pub const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A random `uuid.uuid4().hex`.
pub fn uuid_hex() -> String {
    glib::uuid_string_random().replace('-', "")
}

/// Build a separate app service; its lifetime is never tied to
/// tessel-dock.service. `Type=forking` waits for the helper's result and
/// `ExitType=cgroup` keeps launched descendants alive. Environment values
/// stay out of argv: systemd-run copies each named variable from its own
/// environment. No `--wait`, `--pipe` or `--scope`.
pub fn launch_command<'a>(
    helper_argv: &[String],
    environment: impl IntoIterator<Item = &'a str>,
    unit_name: Option<&str>,
    service_type: &str,
    cwd: Option<&str>,
) -> Vec<String> {
    let unit = unit_name.map(str::to_owned).unwrap_or_else(|| format!("app-tessel-dock-{}.service", uuid_hex()));
    let mut command: Vec<String> = vec![
        SYSTEMD_RUN.into(),
        "--user".into(),
        "--collect".into(),
        "--quiet".into(),
        format!("--service-type={service_type}"),
        "--property=GuessMainPID=no".into(),
        "--property=ExitType=cgroup".into(),
        "--property=TimeoutStartSec=90s".into(),
        "--slice=app.slice".into(),
        "--expand-environment=no".into(),
        format!("--unit={unit}"),
    ];
    if let Some(cwd) = cwd {
        command.push(format!("--working-directory={cwd}"));
    }
    let mut names: Vec<&str> = environment.into_iter().collect();
    names.sort_unstable();
    names.dedup();
    command.extend(names.into_iter().filter(|n| valid_env_name(n)).map(|n| format!("--setenv={n}")));
    command.push("--".into());
    command.extend(helper_argv.iter().cloned());
    command
}

/// The launch context Tessel Dock passes from GTK (`Gdk.AppLaunchContext`).
pub trait LaunchContext {
    /// `context.get_environment()` (`NAME=value` items).
    fn environment(&self) -> Vec<String>;
    /// The native display is a `GdkWayland*` object (DISPLAY must not be replaced).
    fn native_wayland(&self) -> bool;
    /// `Gio.AppLaunchContext.get_display(context, info, files)`.
    fn display(&self, info: &gio::AppInfo, files: &[gio::File]) -> Option<String>;
    fn startup_notify_id(&self, info: &gio::AppInfo, files: &[gio::File]) -> Option<String>;
    fn launch_failed(&self, token: &str);
}

impl LaunchContext for gio::AppLaunchContext {
    fn environment(&self) -> Vec<String> {
        AppLaunchContextExt::environment(self).into_iter().filter_map(|item| item.into_string().ok()).collect()
    }
    fn native_wayland(&self) -> bool {
        if self.find_property("display").is_none() {
            return false;
        }
        self.property_value("display")
            .get::<Option<glib::Object>>()
            .ok()
            .flatten()
            .is_some_and(|display| display.type_().name().starts_with("GdkWayland"))
    }
    fn display(&self, info: &gio::AppInfo, files: &[gio::File]) -> Option<String> {
        AppLaunchContextExt::display(self, info, files).map(|s| s.to_string())
    }
    fn startup_notify_id(&self, info: &gio::AppInfo, files: &[gio::File]) -> Option<String> {
        AppLaunchContextExt::startup_notify_id(self, Some(info), files).map(|s| s.to_string())
    }
    fn launch_failed(&self, token: &str) {
        AppLaunchContextExt::launch_failed(self, token)
    }
}

fn process_environment() -> Vec<(String, String)> {
    std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect()
}

/// `_launch_environment(context, info, files)` with `base` = `os.environ`
/// (used only without a context). Returns the app environment (service
/// variables removed, startup token in `TESSEL_DOCK_LAUNCH_STARTUP_ID`) and token.
pub fn launch_environment(
    context: Option<&dyn LaunchContext>,
    info: Option<&gio::AppInfo>,
    files: &[gio::File],
    base: &[(String, String)],
) -> (OrderedMap<String>, Option<String>) {
    let mut environment: OrderedMap<String> = match context {
        None => base.iter().cloned().collect(),
        Some(context) => context
            .environment()
            .into_iter()
            .filter_map(|item| item.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect(),
    };
    for key in SERVICE_ENV {
        environment.remove(key);
    }
    let mut token = None;
    if let (Some(context), Some(info)) = (context, info) {
        if !context.native_wayland() {
            if let Some(display) = context.display(info, files).filter(|d| !d.is_empty()) {
                environment.insert("DISPLAY", display);
            }
        }
        if desktop_boolean(info, "StartupNotify") {
            token = context.startup_notify_id(info, files);
        }
    }
    let token = token.filter(|t| !t.is_empty());
    if let Some(token) = &token {
        environment.insert("TESSEL_DOCK_LAUNCH_STARTUP_ID", token.clone());
    }
    (environment, token)
}

/// Completion callback: `None` on success, else the error text.
pub type DoneFn = Box<dyn FnOnce(Option<String>)>;

/// Python's raised exceptions: `ValueError` text or a `GLib.Error`.
#[derive(Debug)]
pub enum LaunchError {
    Value(String),
    Glib(glib::Error),
}

impl std::fmt::Display for LaunchError {
    /// `str(exception)` as Python shows it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::Value(text) => f.write_str(text),
            LaunchError::Glib(error) => f.write_str(&tessel_core::pyerr::python_error_text(error)),
        }
    }
}

/// Default helper argv prefix: the `tessel-dock-launch` program.
pub fn default_helper() -> Vec<String> {
    vec![tessel_core::paths::program("tessel-dock-launch").to_string_lossy().into_owned()]
}

/// Launches apps in independent `app-tessel-dock-*.service` units.
pub struct Launcher {
    pub spawner: Rc<dyn Spawner>,
    /// argv before `<mode> <target>` (default [`default_helper`]).
    pub helper: Vec<String>,
    /// Replaces `/usr/bin/systemd-run` in the argv (tests only).
    pub systemd_run: Option<String>,
    /// `os.environ` (used when no launch context is given).
    pub base_environment: Box<dyn Fn() -> Vec<(String, String)>>,
    pub default_for_uri_scheme: Box<dyn Fn(&str) -> Option<gio::AppInfo>>,
    pub default_for_type: Box<dyn Fn(&str, bool) -> Option<gio::AppInfo>>,
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new(Rc::new(GioSpawner))
    }
}

impl Launcher {
    pub fn new(spawner: Rc<dyn Spawner>) -> Self {
        Self {
            spawner,
            helper: default_helper(),
            systemd_run: None,
            base_environment: Box::new(process_environment),
            default_for_uri_scheme: Box::new(gio::AppInfo::default_for_uri_scheme),
            default_for_type: Box::new(gio::AppInfo::default_for_type),
        }
    }

    fn start_service(&self, mut command: Vec<String>, environment: &OrderedMap<String>, complete: DoneFn) -> Result<(), glib::Error> {
        if let Some(systemd_run) = &self.systemd_run {
            command[0] = systemd_run.clone();
        }
        let environment: Vec<(String, String)> = environment.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        // Never cancelled on dock shutdown: the service belongs to the app.
        self.spawner.spawn(
            &command,
            Some(&environment),
            Box::new(move |result| match result {
                Ok((true, _)) => complete(None),
                Ok((false, stderr)) => {
                    let detail = if stderr.is_empty() { "The application service could not start.".to_owned() } else { stderr };
                    complete(Some(format!("Could not launch the application: {}", tessel_core::pyjson::py_strip(&detail))));
                }
                Err(error) => complete(Some(format!("Could not launch the application: {}", tessel_core::pyerr::python_error_text(&error)))),
            }),
        )
    }

    /// `_launch_separately(mode, target, info, context, on_done)`.
    pub fn launch_separately(
        &self,
        mode: &str,
        target: &str,
        info: Option<&gio::AppInfo>,
        context: Option<Rc<dyn LaunchContext>>,
        on_done: Option<DoneFn>,
    ) -> Result<(), glib::Error> {
        let files = if mode == "--open-uri" { vec![gio::File::for_uri(target)] } else { Vec::new() };
        let base = if context.is_none() { (self.base_environment)() } else { Vec::new() };
        let (environment, token) = launch_environment(context.as_deref(), info, &files, &base);
        let mut helper = self.helper.clone();
        helper.extend([mode.to_owned(), target.to_owned()]);
        let command = launch_command(&helper, environment.keys(), None, "forking", None);
        let (failed_context, failed_token) = (context.clone(), token.clone());
        let complete: DoneFn = Box::new(move |error| {
            if let (Some(_), Some(token), Some(context)) = (&error, &token, &context) {
                context.launch_failed(token);
            }
            if let Some(on_done) = on_done {
                on_done(error);
            }
        });
        self.start_service(command, &environment, complete).inspect_err(|_| {
            if let (Some(token), Some(context)) = (failed_token, failed_context) {
                context.launch_failed(&token);
            }
        })
    }

    /// Start a command outside the calling desktop service's cgroup
    /// (`--service-type=exec`).
    pub fn launch_process(
        &self,
        argv: &[String],
        environment: Option<Vec<(String, String)>>,
        cwd: Option<&str>,
        on_done: Option<DoneFn>,
    ) -> Result<(), LaunchError> {
        if argv.is_empty() {
            return Err(LaunchError::Value("An application command is required.".into()));
        }
        let mut environment: OrderedMap<String> = environment.unwrap_or_else(|| (self.base_environment)()).into_iter().collect();
        for key in SERVICE_ENV {
            environment.remove(key);
        }
        let command = launch_command(argv, environment.keys(), None, "exec", cwd);
        self.start_service(command, &environment, on_done.unwrap_or_else(|| Box::new(|_| {}))).map_err(LaunchError::Glib)
    }

    pub fn launch_desktop_file(&self, path: &Path, context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) -> Result<(), LaunchError> {
        let path = path.to_string_lossy().into_owned();
        let Some(info) = desktop_info_from_filename(&path) else {
            return Err(LaunchError::Value("That desktop file is not a valid application.".into()));
        };
        self.launch_separately("--launch-file", &path, Some(&info), context, on_done).map_err(LaunchError::Glib)
    }

    /// Open Home/Trash (or any URI) through Gio in an independent service.
    pub fn launch_uri(&self, uri: &str, context: Option<Rc<dyn LaunchContext>>, on_done: Option<DoneFn>) -> Result<(), LaunchError> {
        let scheme = glib::Uri::parse_scheme(uri).map(|s| s.to_string()).unwrap_or_default();
        let info = (self.default_for_uri_scheme)(&scheme).or_else(|| (self.default_for_type)("inode/directory", true));
        self.launch_separately("--open-uri", uri, info.as_ref(), context, on_done).map_err(LaunchError::Glib)
    }
}

// ---- The launch helper (runs inside the independent service) ---------------

mod transferred {
    //! `_TransferredLaunchContext`: a `GAppLaunchContext` subclass that hands
    //! the original startup token to Gio and reports DISPLAY from its own
    //! environment (never the Wayland socket).
    use super::*;
    use std::sync::OnceLock;

    const TOKEN_KEY: &[u8] = b"tessel-dock-transferred-token\0";

    unsafe extern "C" fn get_display(
        context: *mut gio::ffi::GAppLaunchContext,
        _info: *mut gio::ffi::GAppInfo,
        _files: *mut glib::ffi::GList,
    ) -> *mut c_char {
        let environment = gio::ffi::g_app_launch_context_get_environment(context);
        let mut result = std::ptr::null_mut();
        if !environment.is_null() {
            let mut item = environment;
            while !(*item).is_null() {
                let text = CStr::from_ptr(*item).to_bytes();
                if let Some(value) = text.strip_prefix(b"DISPLAY=") {
                    let value = CString::new(value).unwrap_or_default();
                    result = glib::ffi::g_strdup(value.as_ptr());
                    break;
                }
                item = item.add(1);
            }
            glib::ffi::g_strfreev(environment);
        }
        result
    }

    unsafe extern "C" fn get_startup_notify_id(
        context: *mut gio::ffi::GAppLaunchContext,
        _info: *mut gio::ffi::GAppInfo,
        _files: *mut glib::ffi::GList,
    ) -> *mut c_char {
        let token = glib::gobject_ffi::g_object_get_data(context as *mut _, TOKEN_KEY.as_ptr() as *const c_char);
        if token.is_null() {
            std::ptr::null_mut()
        } else {
            glib::ffi::g_strdup(token as *const c_char)
        }
    }

    unsafe extern "C" fn class_init(class: glib::ffi::gpointer, _data: glib::ffi::gpointer) {
        let class = class as *mut gio::ffi::GAppLaunchContextClass;
        (*class).get_display = Some(get_display);
        (*class).get_startup_notify_id = Some(get_startup_notify_id);
    }

    fn type_() -> glib::ffi::GType {
        static TYPE: OnceLock<usize> = OnceLock::new();
        *TYPE.get_or_init(|| unsafe {
            glib::gobject_ffi::g_type_register_static_simple(
                gio::ffi::g_app_launch_context_get_type(),
                b"TesselDockTransferredLaunchContext\0".as_ptr() as *const c_char,
                std::mem::size_of::<gio::ffi::GAppLaunchContextClass>() as u32,
                Some(class_init),
                std::mem::size_of::<gio::ffi::GAppLaunchContext>() as u32,
                None,
                0,
            ) as usize
        }) as glib::ffi::GType
    }

    /// `_TransferredLaunchContext(token)`.
    pub fn new(token: Option<&str>) -> gio::AppLaunchContext {
        unsafe {
            let object = glib::gobject_ffi::g_object_new(type_(), std::ptr::null());
            if let Some(token) = token {
                let token = CString::new(token).unwrap_or_default();
                glib::gobject_ffi::g_object_set_data_full(
                    object,
                    TOKEN_KEY.as_ptr() as *const c_char,
                    glib::ffi::g_strdup(token.as_ptr()) as glib::ffi::gpointer,
                    Some(glib::ffi::g_free),
                );
            }
            from_glib_full(object as *mut gio::ffi::GAppLaunchContext)
        }
    }
}

/// `_TransferredLaunchContext(token)` (exposed for tests and the helper).
pub fn transferred_context(token: Option<&str>) -> gio::AppLaunchContext {
    transferred::new(token)
}

/// `_run_launch_helper(mode, target)` with an explicit startup token: launch
/// and wait for Gio's async result. `Err` is the Python exception text.
pub fn launch_helper(mode: &str, target: &str, token: Option<&str>) -> Result<(), String> {
    let context = transferred::new(token);
    let main_context = glib::MainContext::new();
    let failure: Rc<RefCell<Option<Option<String>>>> = Rc::new(RefCell::new(None));
    main_context
        .with_thread_default(|| -> Result<(), String> {
            let main_loop = glib::MainLoop::new(Some(&main_context), false);
            let finished = {
                let (failure, main_loop) = (failure.clone(), main_loop.clone());
                move |result: Result<(), glib::Error>| {
                    *failure.borrow_mut() = Some(result.err().map(|e| tessel_core::pyerr::python_error_text(&e)));
                    main_loop.quit();
                }
            };
            match mode {
                "--launch-desktop" | "--launch-file" => {
                    let info = if mode == "--launch-file" { desktop_info_from_filename(target) } else { desktop_info(target) };
                    let Some(info) = info else {
                        return Err("That application is no longer installed.".into());
                    };
                    info.launch_uris_async(&[], Some(&context), None::<&gio::Cancellable>, finished);
                }
                "--open-uri" => gio::AppInfo::launch_default_for_uri_async(target, Some(&context), None::<&gio::Cancellable>, finished),
                _ => return Err("Unknown launch operation.".into()),
            }
            if failure.borrow().is_none() {
                main_loop.run();
            }
            Ok(())
        })
        .map_err(|e| e.to_string())??;
    let outcome = failure.borrow_mut().take().flatten();
    match outcome {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The helper `__main__` for one operation: takes the token from
/// `TESSEL_DOCK_LAUNCH_STARTUP_ID` (removing it from the environment), prints
/// `Tessel Dock launch: <error>` on failure. Returns the exit status.
pub fn run_launch_helper(mode: &str, target: &str) -> i32 {
    let token = std::env::var("TESSEL_DOCK_LAUNCH_STARTUP_ID").ok();
    std::env::remove_var("TESSEL_DOCK_LAUNCH_STARTUP_ID");
    match launch_helper(mode, target, token.as_deref()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Tessel Dock launch: {error}");
            1
        }
    }
}

/// `tessel-dock-launch <mode> <target>` argument handling (`args`
/// excludes the program name).
pub fn launch_helper_main(args: &[String]) -> i32 {
    if args.len() != 2 {
        eprintln!("Tessel Dock launch: Expected a launch operation and its target.");
        return 1;
    }
    run_launch_helper(&args[0], &args[1])
}
