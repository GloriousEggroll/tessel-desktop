//! Bounded, asynchronous fullscreen checks shared with Tessel Dock's game policy
//! (ports of notification_policy.py and the verdict logic of
//! tessel-dock/fullscreen_guard.py).
//!
//! Checks are event-driven: foreign-toplevel focus/state changes and new
//! notifications call `refresh()`. There is no polling. The X11 probe process
//! (`python3 -I -S <helper dir>/fullscreen_guard.py --probe`, 120 ms deadline)
//! runs only when the focused window may be Xwayland and the cached verdict
//! for its focus signature is missing (or, forced, older than 2 s).

use crate::backend::{Output, Window, WindowBackend};
use indexmap::IndexMap;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

pub const PROBE_TIMEOUT_MS: u64 = 120;
/// A forced refresh (for example a new notification) reuses a verdict this young.
pub const PROBE_MAX_AGE: f64 = 2.0;

/// The windows and outputs one check looks at.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub windows: IndexMap<String, Window>,
    pub outputs: IndexMap<String, Output>,
}

impl Snapshot {
    pub fn focused(&self) -> Vec<(&String, &Window)> {
        self.windows.iter().filter(|(_, w)| w.activated && !w.minimized).collect()
    }
}

type Signature = Vec<(String, String, String, bool, bool, Vec<String>)>;

/// Foreign-toplevel state that can change a focused-fullscreen verdict.
pub fn focus_signature(snapshot: &Snapshot) -> Signature {
    let mut signature: Signature = snapshot.focused().into_iter()
        .map(|(id, w)| (id.clone(), w.app_id.clone(), w.title.clone(), w.fullscreen, w.maximized, w.outputs.clone())).collect();
    signature.sort();
    signature
}

/// Verdicts keyed by focus signature; reads never spawn a probe.
pub struct FullscreenCache {
    clock: Box<dyn Fn() -> f64>,
    signature: Option<Signature>,
    result: bool,
    stamp: Option<f64>,
    native: HashSet<String>,
    votes: HashMap<String, u32>,
}

impl Default for FullscreenCache {
    fn default() -> Self { Self::new(Box::new(crate::model::monotonic)) }
}

impl FullscreenCache {
    pub fn new(clock: Box<dyn Fn() -> f64>) -> Self {
        Self { clock, signature: None, result: false, stamp: None, native: HashSet::new(), votes: HashMap::new() }
    }

    pub fn probe_candidates<'a>(&self, snapshot: &'a Snapshot) -> Vec<&'a Window> {
        snapshot.focused().into_iter().filter(|(id, w)| !w.app_id.is_empty() && !self.native.contains(*id)).map(|(_, w)| w).collect()
    }

    /// Whether a fresh X11 check could change the cached verdict.
    pub fn needs_probe(&self, snapshot: &Snapshot, force: bool) -> bool {
        let focused = snapshot.focused();
        if focused.is_empty() || focused.iter().any(|(_, w)| w.fullscreen) {
            return false;
        }
        if self.probe_candidates(snapshot).is_empty() {
            return false;
        }
        if self.signature.as_ref() != Some(&focus_signature(snapshot)) || self.stamp.is_none() {
            return true;
        }
        force && (self.clock)() - self.stamp.unwrap_or(0.0) >= PROBE_MAX_AGE
    }

    /// Cached verdict: native state is exact, unknown X11 fails open.
    pub fn last_state(&self, snapshot: Option<&Snapshot>) -> bool {
        let Some(snapshot) = snapshot else { return self.result };
        let focused = snapshot.focused();
        if focused.iter().any(|(_, w)| w.fullscreen) {
            return true;
        }
        if focused.is_empty() || self.probe_candidates(snapshot).is_empty() {
            return false;
        }
        if self.signature.as_ref() == Some(&focus_signature(snapshot)) { self.result } else { false }
    }

    pub fn record(&mut self, signature: Signature, focused: &[(String, Window)], x11: Option<&Value>, result: bool) {
        self.signature = Some(signature);
        self.result = result;
        self.stamp = Some((self.clock)());
        let Some(Value::Array(clients)) = x11.and_then(|s| s.get("clients")) else { return };
        let known: HashSet<String> = clients.iter().filter_map(Value::as_array).flatten()
            .filter_map(Value::as_str).map(casefold).collect();
        for (ident, window) in focused {
            // A focused window absent from two complete X11 client lists is
            // native Wayland for its whole lifetime. One absence could be an
            // Xwayland window probed before its X11 map completed.
            if !window.app_id.is_empty() && !known.contains(&casefold(&window.app_id)) {
                let votes = self.votes.entry(ident.clone()).or_insert(0);
                *votes += 1;
                if *votes >= 2 {
                    self.native.insert(ident.clone());
                }
            } else {
                self.votes.remove(ident);
            }
        }
    }

    /// Require a fresh check on the next `needs_probe()`, keeping native-window memory.
    pub fn invalidate(&mut self) {
        self.signature = None;
        self.stamp = None;
    }

    /// Drop native-window memory for closed windows.
    pub fn forget<'a>(&mut self, live: impl Iterator<Item = &'a String>) {
        let live: HashSet<&String> = live.collect();
        self.native.retain(|id| live.contains(id));
        self.votes.retain(|id, _| live.contains(id));
    }

    pub fn is_native(&self, ident: &str) -> bool { self.native.contains(ident) }
}

fn casefold(text: &str) -> String {
    text.to_lowercase()
}

/// Conservative identity/geometry checks on one fresh X11 focus snapshot.
pub fn matching_x11_fullscreen(window: &Window, outputs: &IndexMap<String, Output>, snapshot: Option<&Value>) -> bool {
    let Some(snapshot) = snapshot else { return false };
    if snapshot.get("focused") != Some(&Value::Bool(true)) {
        return false;
    }
    let classes = match snapshot.get("classes") {
        None => Vec::new(),
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(casefold).collect(),
        Some(_) => return false,
    };
    if window.app_id.is_empty() || !classes.contains(&casefold(&window.app_id))
        || snapshot.get("title").and_then(Value::as_str) != Some(window.title.as_str()) {
        return false;
    }
    if snapshot.get("fullscreen") == Some(&Value::Bool(true)) {
        return true;
    }
    let is_int = |v: &Value| matches!(v, Value::Number(n) if !n.is_f64());
    let border_zero = snapshot.get("border_width").is_some_and(|v| v.as_f64() == Some(0.0));
    if snapshot.get("maximized") == Some(&Value::Bool(true)) || !border_zero {
        return false;
    }
    let zero_extents = snapshot.get("extents").and_then(Value::as_array)
        .is_some_and(|e| e.len() == 4 && e.iter().all(|v| v.as_f64() == Some(0.0)));
    let no_decorations = snapshot.get("motif").and_then(Value::as_array).is_some_and(|m| {
        m.len() >= 3 && is_int(&m[0]) && m[0].as_i64().unwrap_or(0) & 2 != 0 && m[2].as_f64() == Some(0.0)
    });
    if !(zero_extents || no_decorations) {
        return false;
    }
    let Some(geometry) = snapshot.get("geometry").and_then(Value::as_array) else { return false };
    if geometry.len() != 4 || !geometry.iter().all(is_int) {
        return false;
    }
    let geometry: Vec<i64> = geometry.iter().map(|v| v.as_i64().unwrap_or(0)).collect();
    if geometry[2].min(geometry[3]) <= 0 {
        return false;
    }
    for output in outputs.values() {
        if !window.outputs.is_empty() && !window.outputs.contains(&output.name) {
            continue;
        }
        let expected = [output.x, output.y, output.width, output.height];
        if expected[2].min(expected[3]) > 0 && geometry.iter().zip(expected).all(|(a, t)| (a - t).abs() <= 1) {
            return true;
        }
    }
    false
}

fn local_display(display: &str) -> bool {
    // (?:unix)?:[0-9]+(?:\.[0-9]+)?
    let rest = display.strip_prefix("unix").unwrap_or(display);
    let Some(rest) = rest.strip_prefix(':') else { return false };
    let (number, screen) = match rest.split_once('.') {
        Some((n, s)) => (n, Some(s)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(number) && screen.is_none_or(digits)
}

fn python() -> &'static str {
    if std::path::Path::new("/usr/bin/python3").is_file() { "/usr/bin/python3" } else { "python3" }
}

/// Fresh X11 focus snapshot in an expendable process (fail-open `None`).
pub fn x11_snapshot(done: impl FnOnce(Option<Value>) + 'static) {
    // Xwayland is local. Do not turn a notification into a remote DISPLAY query.
    if !local_display(&std::env::var("DISPLAY").unwrap_or_default()) {
        glib::idle_add_local_once(move || done(None));
        return;
    }
    let script = crate::backend::helper_dir().join("fullscreen_guard.py");
    let argv = [std::ffi::OsStr::new(python()), "-I".as_ref(), "-S".as_ref(), script.as_os_str(), "--probe".as_ref()];
    let process = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) {
        Ok(process) => process,
        Err(_) => {
            glib::idle_add_local_once(move || done(None));
            return;
        }
    };
    let done = Rc::new(RefCell::new(Some(Box::new(done) as Box<dyn FnOnce(Option<Value>)>)));
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let (killer, timed_out, slot) = (process.clone(), done.clone(), timer.clone());
    let id = glib::timeout_add_local_once(Duration::from_millis(PROBE_TIMEOUT_MS), move || {
        slot.replace(None);
        killer.force_exit();
        if let Some(done) = timed_out.take() {
            done(None);
        }
    });
    timer.replace(Some(id));
    let waited = process.clone();
    process.communicate_async(None, None::<&gio::Cancellable>, move |result| {
        if let Some(timer) = timer.take() {
            timer.remove();
        }
        let Some(done) = done.take() else { return };
        let snapshot = result.ok().and_then(|(stdout, _)| stdout).and_then(|stdout| {
            let ok = waited.has_exited() && waited.exit_status() == 0;
            (ok && stdout.len() <= 65536).then(|| serde_json::from_slice::<Value>(&stdout).ok()).flatten()
        }).filter(Value::is_object);
        done(snapshot);
    });
}

/// Fresh check: native fullscreen is immediate; otherwise the bounded X11 probe.
/// The verdict is recorded in `cache`; `done` always runs from the main loop.
pub fn focused_fullscreen(snapshot: Snapshot, cache: Rc<RefCell<FullscreenCache>>, done: impl FnOnce(bool) + 'static) {
    let signature = focus_signature(&snapshot);
    let focused: Vec<(String, Window)> = snapshot.focused().into_iter().map(|(i, w)| (i.clone(), w.clone())).collect();
    if focused.iter().any(|(_, w)| w.fullscreen) {
        cache.borrow_mut().record(signature, &focused, None, true);
        glib::idle_add_local_once(move || done(true));
        return;
    }
    let candidates: Vec<Window> = cache.borrow().probe_candidates(&snapshot).into_iter().cloned().collect();
    if candidates.is_empty() {
        cache.borrow_mut().record(signature, &focused, None, false);
        glib::idle_add_local_once(move || done(false));
        return;
    }
    x11_snapshot(move |x11| {
        let result = candidates.iter().any(|w| matching_x11_fullscreen(w, &snapshot.outputs, x11.as_ref()));
        cache.borrow_mut().record(signature, &focused, x11.as_ref(), result);
        done(result);
    });
}

struct WatcherInner {
    backend: WindowBackend,
    on_changed: RefCell<Option<Rc<dyn Fn()>>>,
    cache: Rc<RefCell<FullscreenCache>>,
    blocked: Cell<bool>,
    checking: Cell<bool>,
    closed: Cell<bool>,
    serial: Cell<u64>,
    busy: Cell<bool>,
    again: Cell<bool>,
}

#[derive(Clone)]
pub struct FullscreenWatcher(Rc<WatcherInner>);

impl FullscreenWatcher {
    pub fn new(backend: &WindowBackend, on_changed: impl Fn() + 'static, cache: Option<Rc<RefCell<FullscreenCache>>>) -> Self {
        Self(Rc::new(WatcherInner {
            backend: backend.clone(), on_changed: RefCell::new(Some(Rc::new(on_changed))),
            cache: cache.unwrap_or_else(|| Rc::new(RefCell::new(FullscreenCache::default()))),
            blocked: Cell::new(false), checking: Cell::new(true), closed: Cell::new(false), serial: Cell::new(0),
            busy: Cell::new(false), again: Cell::new(false),
        }))
    }

    pub fn blocked(&self) -> bool { self.0.blocked.get() }
    pub fn checking(&self) -> bool { self.0.checking.get() }
    pub fn cache(&self) -> Rc<RefCell<FullscreenCache>> { self.0.cache.clone() }

    fn changed(&self) {
        let callback = self.0.on_changed.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// Re-evaluate; `force` asks for a fresh X11 verdict (rate limited).
    pub fn refresh(&self, force: bool) {
        if self.0.closed.get() {
            return;
        }
        let serial = self.0.serial.get() + 1;
        self.0.serial.set(serial);
        // The native flag covers Wayland and Xwayland fullscreen without
        // consulting X11. Unknown window state conservatively holds new popups.
        let windows = self.0.backend.windows();
        self.0.cache.borrow_mut().forget(windows.keys());
        let snapshot = Snapshot { windows, outputs: self.0.backend.outputs() };
        let active = snapshot.focused();
        if !self.0.backend.ready() || active.iter().any(|(_, w)| w.fullscreen) {
            self.0.blocked.set(true);
            self.0.checking.set(false);
            self.changed();
            return;
        }
        let no_active = active.is_empty();
        if no_active || !self.0.cache.borrow().needs_probe(&snapshot, force) {
            let blocked = self.0.cache.borrow().last_state(Some(&snapshot));
            self.0.blocked.set(blocked);
            self.0.checking.set(false);
            self.changed();
            return;
        }
        self.0.checking.set(true);
        if self.0.busy.get() {
            self.0.again.set(true);
            return;
        }
        self.0.busy.set(true);
        let weak = Rc::downgrade(&self.0);
        focused_fullscreen(snapshot, self.0.cache.clone(), move |blocked| {
            if let Some(inner) = weak.upgrade() {
                FullscreenWatcher(inner).finished(serial, blocked);
            }
        });
    }

    fn finished(&self, serial: u64, blocked: bool) {
        self.0.busy.set(false);
        if self.0.closed.get() {
            return;
        }
        if serial == self.0.serial.get() {
            self.0.blocked.set(blocked);
            self.0.checking.set(false);
            self.changed();
        }
        if self.0.again.get() || serial != self.0.serial.get() {
            self.0.again.set(false);
            self.refresh(false);
        }
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.on_changed.replace(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window(id: &str, app: &str, title: &str) -> Window {
        Window { id: id.into(), app_id: app.into(), title: title.into(), activated: true,
                 outputs: vec!["HEADLESS-1".into()], ..Default::default() }
    }

    fn snapshot(windows: Vec<Window>) -> Snapshot {
        let mut snap = Snapshot::default();
        for w in windows {
            snap.windows.insert(w.id.clone(), w);
        }
        snap.outputs.insert("HEADLESS-1".into(), Output { name: "HEADLESS-1".into(), width: 1920, height: 1080, scale: 1, ..Default::default() });
        snap
    }

    #[test]
    fn x11_verdict_requires_identity_and_geometry() {
        let snap = snapshot(vec![window("1:1", "steam_app_1", "Game")]);
        let w = &snap.windows["1:1"];
        let x11 = json!({"focused": true, "classes": ["Steam_App_1"], "title": "Game", "fullscreen": false,
                         "maximized": false, "border_width": 0, "extents": [0, 0, 0, 0], "motif": null,
                         "geometry": [0, 0, 1920, 1080]});
        assert!(matching_x11_fullscreen(w, &snap.outputs, Some(&x11)));
        let mut off = x11.clone();
        off["geometry"] = json!([0, 0, 1900, 1080]);
        assert!(!matching_x11_fullscreen(w, &snap.outputs, Some(&off)));
        let mut maximized = x11.clone();
        maximized["maximized"] = json!(true);
        assert!(!matching_x11_fullscreen(w, &snap.outputs, Some(&maximized)));
        let mut decorated = x11.clone();
        decorated["extents"] = json!([1, 1, 20, 1]);
        assert!(!matching_x11_fullscreen(w, &snap.outputs, Some(&decorated)));
        decorated["motif"] = json!([2, 0, 0]);
        assert!(matching_x11_fullscreen(w, &snap.outputs, Some(&decorated)));
        let mut other = x11.clone();
        other["title"] = json!("Other");
        assert!(!matching_x11_fullscreen(w, &snap.outputs, Some(&other)));
        let mut flagged = other.clone();
        flagged["title"] = json!("Game");
        flagged["fullscreen"] = json!(true);
        flagged["geometry"] = json!(null);
        assert!(matching_x11_fullscreen(w, &snap.outputs, Some(&flagged)));
        assert!(!matching_x11_fullscreen(w, &snap.outputs, None));
    }

    #[test]
    fn cache_reuses_verdicts_and_learns_native_windows() {
        let now = Rc::new(Cell::new(100.0));
        let clock = now.clone();
        let mut cache = FullscreenCache::new(Box::new(move || clock.get()));
        let snap = snapshot(vec![window("1:1", "editor", "Doc")]);
        assert!(cache.needs_probe(&snap, false));
        let focused: Vec<(String, Window)> = snap.focused().into_iter().map(|(i, w)| (i.clone(), w.clone())).collect();
        let clients = json!({"clients": [["xterm", "XTerm"]]});
        cache.record(focus_signature(&snap), &focused, Some(&clients), false);
        assert!(!cache.needs_probe(&snap, false));
        assert!(!cache.needs_probe(&snap, true), "a young verdict is reused");
        now.set(103.0);
        assert!(cache.needs_probe(&snap, true));
        cache.record(focus_signature(&snap), &focused, Some(&clients), false);
        assert!(cache.is_native("1:1"));
        assert!(!cache.needs_probe(&snap, true), "proven native windows are never probed");
        cache.forget(std::iter::empty());
        assert!(!cache.is_native("1:1"));
        let mut full = snap.clone();
        full.windows.get_mut("1:1").unwrap().fullscreen = true;
        assert!(cache.last_state(Some(&full)));
        assert!(!cache.needs_probe(&full, true));
        cache.invalidate();
        assert!(cache.needs_probe(&snap, false));
    }

    #[test]
    fn display_must_be_local() {
        for good in [":0", ":12", "unix:1", ":0.1"] {
            assert!(local_display(good), "{good}");
        }
        for bad in ["", "host:0", ":", ":a", "localhost:10.0"] {
            assert!(!local_display(bad), "{bad}");
        }
    }
}
