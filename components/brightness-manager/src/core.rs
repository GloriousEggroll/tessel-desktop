//! Persistent dimming preferences and crash-recoverable hardware restoration
//! (`brightness_core.py`).

use crate::backend::Hardware;
use crate::compat::fsutil;
use crate::model::{self, integer, Callback};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

pub fn defaults() -> Value {
    json!({"auto_dim": false, "dim_after": 120, "dim_percent": 30})
}

pub fn validate_settings(value: &Value) -> Result<Value, String> {
    let object = value.as_object().filter(|o| {
        o.len() == 3 && ["auto_dim", "dim_after", "dim_percent"].iter().all(|k| o.contains_key(*k))
    }).ok_or_else(|| "Choose valid automatic dimming settings.".to_owned())?;
    if !object["auto_dim"].is_boolean() {
        return Err("Auto-dim must be on or off.".into());
    }
    if !integer(&object["dim_after"]).is_some_and(|v| (1..=86400).contains(&v)) {
        return Err("The dimming delay must be between one second and 24 hours.".into());
    }
    if !integer(&object["dim_percent"]).is_some_and(|v| (1..=90).contains(&v)) {
        return Err("Choose a dimmed brightness from 1% to 90%.".into());
    }
    Ok(value.clone())
}

fn valid_entry(entry: &Value) -> bool {
    let Some(entry) = entry.as_object() else { return false };
    if !entry.get("connector").is_some_and(Value::is_string) {
        return false;
    }
    let field = |name: &str| entry.get(name).and_then(integer);
    let (Some(raw), Some(target), Some(maximum)) = (field("raw"), field("target"), field("max")) else { return false };
    0 <= raw && raw <= maximum && 0 <= target && target < raw && maximum <= u32::MAX as i64
}

pub type JournalWriter = Box<dyn Fn(&Path, &Value) -> std::io::Result<()>>;

pub struct Inner {
    backend: Rc<dyn Hardware>,
    changed: Box<dyn Fn()>,
    pub config_path: PathBuf,
    pub journal_path: PathBuf,
    settings: RefCell<Value>,
    error: RefCell<String>,
    entries: RefCell<Map<String, Value>>,
    pending: RefCell<HashMap<String, i64>>,
    want_dim: Cell<bool>,
    epoch: Cell<u64>,
    waiting: RefCell<Vec<Box<dyn FnOnce()>>>,
    retry: RefCell<HashMap<String, Instant>>,
    pumping: Cell<bool>,
    draining: Cell<bool>,
    journal_valid: Cell<bool>,
    writer: RefCell<JournalWriter>,
}

#[derive(Clone)]
pub struct BrightnessCore(pub Rc<Inner>);

impl BrightnessCore {
    /// `config_dir`/`state_dir` default to `$XDG_CONFIG_HOME` / `$XDG_STATE_HOME`.
    pub fn new(backend: Rc<dyn Hardware>, changed: impl Fn() + 'static, config_dir: Option<&Path>, state_dir: Option<&Path>) -> Self {
        let config = config_dir.map(Path::to_path_buf).unwrap_or_else(suite_core::paths::config_home);
        let state = state_dir.map(Path::to_path_buf).unwrap_or_else(suite_core::paths::state_home);
        let core = Self(Rc::new(Inner {
            backend, changed: Box::new(changed), config_path: config.join("brightness-manager/settings.json"),
            journal_path: state.join("brightness-manager/restore.json"), settings: RefCell::new(defaults()),
            error: RefCell::new(String::new()), entries: RefCell::new(Map::new()), pending: RefCell::new(HashMap::new()),
            want_dim: Cell::new(false), epoch: Cell::new(0), waiting: RefCell::new(Vec::new()), retry: RefCell::new(HashMap::new()),
            pumping: Cell::new(false), draining: Cell::new(false), journal_valid: Cell::new(true),
            writer: RefCell::new(Box::new(|path: &Path, value: &Value| fsutil::write_json(path, value))),
        }));
        match fsutil::read_json(&core.0.config_path, defaults()).and_then(|v| validate_settings(&v)) {
            Ok(settings) => {
                core.0.settings.replace(settings);
            }
            Err(error) => {
                core.0.error.replace(format!("Could not load dimming settings: {error}"));
            }
        }
        let entries = fsutil::read_json(&core.0.journal_path, json!({})).and_then(|entries| {
            let map = entries.as_object().cloned().ok_or_else(|| "Invalid brightness recovery data.".to_owned())?;
            if map.values().all(valid_entry) { Ok(map) } else { Err("Invalid brightness recovery data.".into()) }
        });
        match entries {
            Ok(entries) => {
                core.0.entries.replace(entries);
            }
            Err(error) => {
                core.0.journal_valid.set(false);
                core.0.error.replace(format!("Could not load brightness recovery data: {error}"));
            }
        }
        core
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// Tests: replace `write_json` (e.g. a full disk).
    pub fn set_writer(&self, writer: JournalWriter) {
        let _ = self.0.writer.replace(writer);
    }

    pub fn settings(&self) -> Value {
        self.0.settings.borrow().clone()
    }

    pub fn setting_i64(&self, key: &str) -> i64 {
        integer(&self.0.settings.borrow()[key]).unwrap_or(0)
    }

    pub fn auto_dim(&self) -> bool {
        self.0.settings.borrow()["auto_dim"] == true
    }

    pub fn error(&self) -> String {
        self.0.error.borrow().clone()
    }

    pub fn entries(&self) -> Map<String, Value> {
        self.0.entries.borrow().clone()
    }

    pub fn dimmed(&self) -> bool {
        !self.0.entries.borrow().is_empty()
    }

    pub fn busy(&self) -> bool {
        !self.0.pending.borrow().is_empty()
    }

    pub fn backend(&self) -> Rc<dyn Hardware> {
        self.0.backend.clone()
    }

    fn changed(&self) {
        (self.0.changed)();
    }

    fn record(&self, ident: &str, connector: Option<&str>) -> Option<Value> {
        self.0.backend.displays().into_iter().find(|r| {
            model::text(r, "id") == ident && connector.is_none_or(|c| model::text(r, "connector") == c)
        })
    }

    fn journal(&self) -> std::io::Result<()> {
        let entries = Value::Object(self.0.entries.borrow().clone());
        (self.0.writer.borrow())(&self.0.journal_path, &entries)
    }

    fn add_pending(&self, ident: &str) {
        *self.0.pending.borrow_mut().entry(ident.to_owned()).or_default() += 1;
    }

    pub fn set_settings(&self, settings: &Value) -> bool {
        let result = validate_settings(settings).and_then(|settings| {
            (self.0.writer.borrow())(&self.0.config_path, &settings).map_err(|e| suite_core::pyerr::os_error_text(&e))?;
            Ok(settings)
        });
        match result {
            Err(error) => {
                self.0.error.replace(error);
                self.changed();
                false
            }
            Ok(settings) => {
                self.0.settings.replace(settings);
                self.0.error.replace(String::new());
                self.restore_all(None);
                true
            }
        }
    }

    pub fn dim(&self, guard: Rc<dyn Fn() -> bool>) {
        if !self.auto_dim() || !self.0.journal_valid.get() || !guard() {
            return;
        }
        if !self.0.entries.borrow().is_empty() && !self.0.want_dim.get() {
            return;
        }
        self.0.want_dim.set(true);
        let epoch = self.0.epoch.get();
        for record in self.0.backend.displays() {
            let ident = model::text(&record, "id").to_owned();
            let connector = model::text(&record, "connector").to_owned();
            if self.0.pending.borrow().contains_key(&ident) || self.0.entries.borrow().contains_key(&ident)
                || !model::enabled(&record, true) || !model::supported(&record, "brightness") {
                continue;
            }
            self.add_pending(&ident);
            let (weak, guard, id, conn) = (self.weak(), guard.clone(), ident.clone(), connector.clone());
            let current: Callback = Box::new(move |success, _error| {
                let Some(inner) = weak.upgrade() else { return };
                let core = BrightnessCore(inner);
                let live = core.record(&id, Some(&conn));
                let valid = success && live.is_some() && core.0.want_dim.get() && epoch == core.0.epoch.get() && guard();
                let Some(live) = live.filter(|_| valid) else {
                    core.finished(&id);
                    return;
                };
                let raw = integer(&live["brightness"]["raw"]).unwrap_or(0);
                let maximum = integer(&live["brightness"]["max"]).unwrap_or(0);
                let percent = core.setting_i64("dim_percent");
                let target = (model::py_round(maximum as f64 * percent as f64 / 100.0) as i64).max(1);
                if target >= raw {
                    core.finished(&id);
                    return;
                }
                let entry = json!({"connector": conn, "raw": raw, "target": target, "max": maximum});
                core.0.entries.borrow_mut().insert(id.clone(), entry);
                // Must be durable before the hardware can change.
                if let Err(failure) = core.journal() {
                    core.0.entries.borrow_mut().shift_remove(&id);
                    core.0.error.replace(format!("Could not save the original brightness: {}", suite_core::pyerr::os_error_text(&failure)));
                    core.finished(&id);
                    return;
                }
                let weak = core.weak();
                let finished_id = id.clone();
                let dimmed: Callback = Box::new(move |ok, failure| {
                    let Some(inner) = weak.upgrade() else { return };
                    let core = BrightnessCore(inner);
                    if !ok {
                        core.0.error.replace(if failure.is_empty() { "Could not dim the display.".into() } else { failure });
                        core.0.want_dim.set(false);
                    }
                    core.finished(&finished_id);
                });
                core.0.backend.set_raw(&conn, "brightness", target, Some(dimmed), Some(&id), Some(raw));
            });
            self.0.backend.read_feature(&connector, "brightness", Some(current), Some(&ident));
        }
        self.changed();
    }

    pub fn restore_all(&self, done: Option<Box<dyn FnOnce()>>) {
        self.0.want_dim.set(false);
        self.0.epoch.set(self.0.epoch.get() + 1);
        self.0.retry.borrow_mut().clear();
        if let Some(done) = done {
            self.0.waiting.borrow_mut().push(done);
        }
        self.pump();
    }

    pub fn pump(&self) {
        if self.0.pumping.get() || self.0.want_dim.get() {
            return;
        }
        if !self.0.journal_valid.get() || !self.0.backend.ready() {
            self.drain();
            return;
        }
        self.0.pumping.set(true);
        let entries: Vec<(String, Value)> = self.0.entries.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (ident, entry) in entries {
            let retry = self.0.retry.borrow().get(&ident).copied();
            if self.0.pending.borrow().contains_key(&ident) || retry.is_some_and(|at| Instant::now() < at) {
                continue;
            }
            let connector = model::text(&entry, "connector").to_owned();
            let Some(record) = self.record(&ident, Some(&connector)) else { continue };
            if !model::supported(&record, "brightness") {
                continue;
            }
            self.add_pending(&ident);
            let (weak, id, saved) = (self.weak(), ident.clone(), entry.clone());
            self.0.backend.read_feature(&connector, "brightness", Some(Box::new(move |ok, error| {
                if let Some(inner) = weak.upgrade() {
                    BrightnessCore(inner).restore_read(&id, &saved, ok, &error);
                }
            })), Some(&ident));
        }
        self.0.pumping.set(false);
        self.drain();
    }

    fn restore_read(&self, ident: &str, entry: &Value, success: bool, error: &str) {
        let record = self.record(ident, Some(model::text(entry, "connector")));
        let Some(record) = record.filter(|_| success) else {
            self.failed_restore(ident, error);
            return;
        };
        let feature = &record["brightness"];
        if feature.get("max") != entry.get("max") {
            self.failed_restore(ident, "The display's brightness range changed.");
            return;
        }
        if feature.get("raw") != entry.get("target") {
            // An OSD adjustment or another app superseded our dimmed value, or
            // the process stopped before the dim write. Preserve that value.
            self.forget(ident);
            return;
        }
        let (weak, id) = (self.weak(), ident.to_owned());
        let restored: Callback = Box::new(move |ok, failure| {
            let Some(inner) = weak.upgrade() else { return };
            let core = BrightnessCore(inner);
            if ok { core.forget(&id) } else { core.failed_restore(&id, &failure) }
        });
        self.0.backend.set_raw(model::text(entry, "connector"), "brightness", integer(&entry["raw"]).unwrap_or(0), Some(restored),
                               Some(ident), integer(&entry["target"]));
    }

    fn forget(&self, ident: &str) {
        let entry = self.0.entries.borrow_mut().shift_remove(ident);
        if let Err(error) = self.journal() {
            if let Some(entry) = entry {
                self.0.entries.borrow_mut().insert(ident.to_owned(), entry);
            }
            self.failed_restore(ident, &suite_core::pyerr::os_error_text(&error));
            return;
        }
        if self.0.entries.borrow().is_empty() && self.0.error.borrow().starts_with("Brightness restoration is pending:") {
            self.0.error.replace(String::new());
        }
        self.finished(ident);
    }

    fn failed_restore(&self, ident: &str, error: &str) {
        let error = if error.is_empty() { "Display unavailable." } else { error };
        self.0.error.replace(format!("Brightness restoration is pending: {error}"));
        self.0.retry.borrow_mut().insert(ident.to_owned(), Instant::now() + Duration::from_secs(30));
        self.finished(ident);
    }

    fn finished(&self, ident: &str) {
        {
            let mut pending = self.0.pending.borrow_mut();
            let count = pending.entry(ident.to_owned()).or_default();
            *count -= 1;
            if *count <= 0 {
                pending.remove(ident);
            }
        }
        self.changed();
        self.pump();
        self.drain();
    }

    fn drain(&self) {
        if self.0.draining.get() {
            return;
        }
        self.0.draining.set(true);
        loop {
            if self.0.waiting.borrow().is_empty() || self.busy() || self.0.pumping.get() {
                break;
            }
            let done = self.0.waiting.borrow_mut().remove(0);
            done();
        }
        self.0.draining.set(false);
    }

    /// Restores auto-dim first, then writes; `Err` = Python ValueError.
    pub fn set_value(&self, connector: &str, feature: &str, value: f64, expected_id: &str, callback: Option<Callback>)
                     -> Result<bool, String> {
        if !["brightness", "contrast"].contains(&feature) || self.record(expected_id, Some(connector)).is_none() {
            return Err("This display is no longer connected.".into());
        }
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return Err("Choose a value from 0% to 100%.".into());
        }
        self.0.error.replace(String::new());
        let (weak, connector, feature, expected_id) = (self.weak(), connector.to_owned(), feature.to_owned(), expected_id.to_owned());
        self.restore_all(Some(Box::new(move || {
            let Some(inner) = weak.upgrade() else { return };
            let core = BrightnessCore(inner);
            if core.record(&expected_id, Some(&connector)).is_none() {
                model::call(callback, false, "This display is no longer connected.");
                return;
            }
            core.add_pending(&expected_id);
            let (weak, id, is_brightness) = (core.weak(), expected_id.clone(), feature == "brightness");
            let applied: Callback = Box::new(move |success, error| {
                if let Some(inner) = weak.upgrade() {
                    let core = BrightnessCore(inner);
                    if success && is_brightness && core.0.entries.borrow().contains_key(&id) {
                        core.forget(&id);
                    } else {
                        if !success {
                            core.0.error.replace(if error.is_empty() { "Could not change the display setting.".into() } else { error.clone() });
                        }
                        core.finished(&id);
                    }
                }
                model::call(callback, success, &error);
            });
            core.0.backend.set_value(&connector, &feature, value, Some(applied), Some(&expected_id));
        })));
        Ok(true)
    }
}
