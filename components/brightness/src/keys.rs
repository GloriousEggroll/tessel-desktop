//! Coalesced keyboard brightness changes through the existing hardware core
//! (`brightness_keys.py`). OSD detail strings are English literals, exactly as
//! in Python (the OSD shows them verbatim).

use crate::core::BrightnessCore;
use crate::model::{self, number};
use serde_json::Value;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

/// The pointer's output as GDK reports it (`Gdk.Monitor`), plus the handle
/// handed back to the OSD for placement.
#[derive(Clone)]
pub struct Monitor {
    pub geometry: [i64; 4],
    pub model: Option<String>,
    pub handle: Option<Rc<dyn Any>>,
}

/// `AdjustmentOSD` as seen by the keyboard controller.
pub trait Osd {
    fn show(&self, value: Option<f64>, monitor: Option<&Monitor>, detail: &str);
    fn resolve_monitor(&self, callback: Box<dyn FnOnce(Option<Monitor>)>);
    fn close(&self);
}

pub fn percentage(display: Option<&Value>) -> Option<f64> {
    display.and_then(|d| d.get("brightness")).and_then(|b| b.get("value")).and_then(number).filter(|v| (0.0..=100.0).contains(v))
}

pub fn supported(display: &Value) -> bool {
    model::truthy(display.get("id").unwrap_or(&Value::Null)) && model::truthy(display.get("connector").unwrap_or(&Value::Null))
        && model::truthy(display.get("enabled").unwrap_or(&Value::Null))
        && model::supported(display, "brightness") && percentage(Some(display)).is_some()
}

fn palette_path() -> PathBuf {
    tessel_core::paths::cache_home().join("tessel-dock/palette.json")
}

fn palette_match(path: &Path, rectangle: &[i64; 4]) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > 256 * 1024 {
        return None;
    }
    let data: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if data.get("version") != Some(&Value::from(1)) {
        return None;
    }
    let rectangle: Value = rectangle.to_vec().into();
    let matches: Vec<String> = data.get("monitors")?.as_array()?.iter()
        .filter(|e| e.is_object() && e.get("geometry") == Some(&rectangle))
        .filter_map(|e| e.get("name").and_then(Value::as_str).map(String::from)).collect();
    (matches.len() == 1).then(|| matches[0].clone())
}

/// Match the active output when known; never guess between external panels.
pub fn select_display(displays: &[Value], monitor: Option<&Monitor>, palette: Option<&Path>) -> Option<Value> {
    let candidates: Vec<&Value> = displays.iter().filter(|d| supported(d)).collect();
    let enabled: Vec<&Value> = displays.iter().filter(|d| model::enabled(d, false)).collect();
    let mut connector = None;
    if let Some(monitor) = monitor {
        let rectangle: Value = monitor.geometry.to_vec().into();
        let exact: Vec<&&Value> = enabled.iter().filter(|d| d.get("geometry") == Some(&rectangle)).collect();
        if exact.len() == 1 {
            return supported(exact[0]).then(|| (*exact[0]).clone());
        }
        // GTK and the compositor can round fractional-scale sizes differently.
        let position: Value = monitor.geometry[..2].to_vec().into();
        let positioned: Vec<&&Value> = enabled.iter().filter(|d| {
            d.get("geometry").and_then(Value::as_array).is_some_and(|g| Value::from(g.iter().take(2).cloned().collect::<Vec<_>>()) == position)
        }).collect();
        if positioned.len() == 1 {
            return supported(positioned[0]).then(|| (*positioned[0]).clone());
        }
        if let Some(name) = &monitor.model {
            if displays.iter().any(|d| model::text(d, "connector") == name) {
                connector = Some(name.clone());
            }
        }
        if connector.is_none() {
            let path = palette.map(Path::to_path_buf).unwrap_or_else(palette_path);
            connector = palette_match(&path, &monitor.geometry);
        }
    }
    if let Some(connector) = connector {
        if let Some(found) = displays.iter().find(|d| model::text(d, "connector") == connector) {
            return supported(found).then(|| found.clone());
        }
    }
    let internal: Vec<&&Value> = candidates.iter().filter(|d| model::text(d, "method") == "backlight").collect();
    if internal.len() == 1 {
        return Some((*internal[0]).clone());
    }
    (candidates.len() == 1).then(|| candidates[0].clone())
}

pub type Chooser = Box<dyn Fn(&[Value], Option<&Monitor>) -> Option<Value>>;

pub struct Inner {
    core: BrightnessCore,
    osd: Rc<dyn Osd>,
    choose: Chooser,
    closed: Cell<bool>,
    epoch: Cell<u64>,
    target: RefCell<Option<(String, String)>>,
    desired: Cell<Option<f64>>,
    steps: RefCell<Vec<f64>>,
    resolving: Cell<bool>,
    monitor: RefCell<Option<Monitor>>,
}

/// Keep at most one write in flight and retain the latest requested level.
#[derive(Clone)]
pub struct BrightnessKeys(pub Rc<Inner>);

fn name_of(record: &Value) -> String {
    let name = record.get("name").filter(|n| model::truthy(n)).or(record.get("connector")).cloned().unwrap_or(Value::Null);
    name.as_str().map(String::from).unwrap_or_else(|| name.to_string())
}

impl BrightnessKeys {
    pub fn new(core: BrightnessCore, osd: Rc<dyn Osd>, choose: Option<Chooser>) -> Self {
        Self(Rc::new(Inner {
            core, osd, choose: choose.unwrap_or_else(|| Box::new(|displays, monitor| select_display(displays, monitor, None))),
            closed: Cell::new(false), epoch: Cell::new(0), target: RefCell::new(None), desired: Cell::new(None),
            steps: RefCell::new(Vec::new()), resolving: Cell::new(false), monitor: RefCell::new(None),
        }))
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub fn closed(&self) -> bool {
        self.0.closed.get()
    }

    fn record(&self) -> Option<Value> {
        let target = self.0.target.borrow().clone()?;
        self.0.core.backend().displays().into_iter().find(|d| {
            model::text(d, "connector") == target.0 && model::text(d, "id") == target.1 && supported(d)
        })
    }

    fn show(&self, value: Option<f64>, detail: &str) {
        if !self.0.closed.get() {
            let monitor = self.0.monitor.borrow().clone();
            self.0.osd.show(value, monitor.as_ref(), detail);
        }
    }

    pub fn adjust(&self, step: f64) -> bool {
        if self.0.closed.get() {
            return false;
        }
        let backend = self.0.core.backend();
        if !backend.ready() {
            let error = backend.error();
            self.show(None, if error.is_empty() { "Display controls are not ready." } else { &error });
            return false;
        }
        if let Some(desired) = self.0.desired.get() {
            self.0.desired.set(Some((desired + step).clamp(0.0, 100.0)));
            let record = self.record();
            match &record {
                Some(record) => self.show(percentage(Some(record)), &format!("Adjusting {}", name_of(record))),
                None => self.show(None, "This display is no longer connected."),
            }
            return true;
        }
        self.0.steps.borrow_mut().push(step);
        if !self.0.resolving.get() {
            self.0.resolving.set(true);
            let (weak, epoch) = (self.weak(), self.0.epoch.get());
            self.0.osd.resolve_monitor(Box::new(move |monitor| {
                if let Some(inner) = weak.upgrade() {
                    BrightnessKeys(inner).resolved(epoch, monitor);
                }
            }));
        }
        true
    }

    fn resolved(&self, epoch: u64, monitor: Option<Monitor>) {
        if self.0.closed.get() || epoch != self.0.epoch.get() {
            return;
        }
        let has_monitor = monitor.is_some();
        self.0.monitor.replace(monitor.clone());
        let displays = self.0.core.backend().displays();
        let Some(record) = (self.0.choose)(&displays, monitor.as_ref()) else {
            let available = displays.iter().any(supported);
            let detail = if !available {
                "No display supports hardware brightness control."
            } else if has_monitor {
                "Brightness is unavailable for this display."
            } else {
                "Choose a display in Brightness settings."
            };
            self.show(None, detail);
            self.cancel();
            return;
        };
        self.0.target.replace(Some((model::text(&record, "connector").into(), model::text(&record, "id").into())));
        self.show(percentage(Some(&record)), &format!("Adjusting {}", name_of(&record)));
        // Wait for auto-dim restoration before calculating a relative step.
        // Using a saved 30% dimmed value would jump down from a user's 75%.
        let weak = self.weak();
        self.0.core.restore_all(Some(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                BrightnessKeys(inner).restored(epoch);
            }
        })));
    }

    fn restored(&self, epoch: u64) {
        if self.0.closed.get() || epoch != self.0.epoch.get() {
            return;
        }
        let record = self.record();
        let dimmed = record.as_ref().is_some_and(|r| self.0.core.entries().contains_key(model::text(r, "id")));
        if record.is_none() || dimmed {
            let error = self.0.core.error();
            self.show(percentage(record.as_ref()), if error.is_empty() { "The display could not be restored." } else { &error });
            self.cancel();
            return;
        }
        let mut desired = percentage(record.as_ref()).unwrap_or(0.0);
        for step in self.0.steps.take() {
            desired = (desired + step).clamp(0.0, 100.0);
        }
        self.0.desired.set(Some(desired));
        self.0.resolving.set(false);
        self.submit(epoch);
    }

    fn submit(&self, epoch: u64) {
        let Some(record) = self.record() else {
            self.show(None, "This display is no longer connected.");
            self.cancel();
            return;
        };
        let requested = self.0.desired.get().unwrap_or(0.0);
        if Some(requested) == percentage(Some(&record)) {
            self.show(percentage(Some(&record)), &name_of(&record));
            self.cancel();
            return;
        }
        let weak = self.weak();
        let result = self.0.core.set_value(model::text(&record, "connector"), "brightness", requested, model::text(&record, "id"),
            Some(Box::new(move |success, error| {
                if let Some(inner) = weak.upgrade() {
                    BrightnessKeys(inner).applied(epoch, requested, success, &error);
                }
            })));
        if let Err(error) = result {
            self.applied(epoch, requested, false, &error);
        }
    }

    fn applied(&self, epoch: u64, requested: f64, success: bool, error: &str) {
        if self.0.closed.get() || epoch != self.0.epoch.get() {
            return;
        }
        let record = self.record();
        let Some(record) = record.filter(|_| success) else {
            let current = self.record();
            self.show(percentage(current.as_ref()), if error.is_empty() { "The display could not be adjusted." } else { error });
            self.cancel();
            return;
        };
        if self.0.desired.get() != Some(requested) {
            self.submit(epoch);
        } else {
            self.show(percentage(Some(&record)), &name_of(&record));
            self.cancel();
        }
    }

    pub fn cancel(&self) {
        self.0.epoch.set(self.0.epoch.get() + 1);
        self.0.target.replace(None);
        self.0.desired.set(None);
        self.0.steps.borrow_mut().clear();
        self.0.resolving.set(false);
        self.0.monitor.replace(None);
    }

    pub fn close(&self) {
        self.cancel();
        self.0.closed.set(true);
    }
}
