//! `level_controls.py` (shared, read-back-verified speaker controls and
//! percentage rules) and `screen_levels.py` (idle-screen adjustment feedback;
//! audio queries never block the GTK thread).
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use serde_json::{json, Value};

pub const CSS: &str = "
.screen-level { background: rgba(18,23,33,.94); border: 1px solid rgba(255,255,255,.22);
    border-radius: 18px; padding: 20px 24px; color: white; }
.screen-level label { color: white; font-size: 22px; }
.screen-level progressbar trough { min-height: 7px; background: rgba(255,255,255,.18); border-radius: 5px; }
.screen-level progressbar progress { min-height: 7px; background: #81c5ff; border-radius: 5px; }
.screen-dimmer { background: black; }
";

/// `resolve_level(current, action, minimum=0)`.
pub fn resolve_level(current: i64, action: &str, minimum: i64) -> Result<i64, String> {
    if action == "up" || action == "down" {
        return Ok(minimum.max(100.min(current + if action == "up" { 10 } else { -10 })));
    }
    let digits = action.strip_suffix('%').unwrap_or(action);
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Use up, down, or a percentage".into());
    }
    let value: i64 = digits.parse().map_err(|_| "Use up, down, or a percentage".to_string())?;
    if !(minimum..=100).contains(&value) {
        return Err(format!("Choose a level from {minimum} to 100 percent"));
    }
    Ok(value)
}

/// `{'sink': name, 'value': N, 'muted': bool}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    pub sink: String,
    pub value: i64,
    pub muted: bool,
}

impl Volume {
    pub fn to_json(&self) -> Value { json!({"sink": self.sink, "value": self.value, "muted": self.muted}) }
}

/// Run with a 2 s timeout (`subprocess` `timeout=2`), stderr discarded.
fn run(args: &[&str], capture: bool) -> Result<String, String> { run_timeout(args, Duration::from_secs(2), capture) }

/// `subprocess.check_output/run(args, timeout=…, check=True)` without a shell;
/// stdout captured (stripped) or discarded, stderr discarded.
pub fn run_timeout(args: &[&str], timeout: Duration, capture: bool) -> Result<String, String> {
    let mut child = Command::new(args[0]).args(&args[1..]).stdin(Stdio::null())
        .stdout(if capture { Stdio::piped() } else { Stdio::null() }).stderr(Stdio::null())
        .spawn().map_err(|e| e.to_string())?;
    let mut output = String::new();
    let reader = child.stdout.take().map(|mut out| std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut text);
        text
    }));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Command '{}' timed out after {} seconds", args.join(" "), timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if let Some(reader) = reader {
        output = reader.join().unwrap_or_default();
    }
    if !status.success() {
        return Err(format!("Command '{}' returned non-zero exit status {}", args.join(" "), status.code().unwrap_or(-1)));
    }
    Ok(output.trim().to_owned())
}

/// `read_volume()`: the current default sink's highest channel and mute.
pub fn read_volume() -> Result<Volume, String> {
    let name = run(&["pactl", "get-default-sink"], true)?;
    let sinks: Value = serde_json::from_str(&run(&["pactl", "--format=json", "list", "sinks"], true)?).map_err(|e| e.to_string())?;
    let sink = sinks.as_array().into_iter().flatten().find(|s| s.get("name").and_then(Value::as_str) == Some(name.as_str()))
        .ok_or("StopIteration")?;
    let channels: Vec<f64> = sink.get("volume").and_then(Value::as_object).ok_or("KeyError: 'volume'")?
        .values().map(|v| v.get("value").and_then(tessel_core::pyjson::py_float).ok_or("KeyError: 'value'"))
        .collect::<Result<_, _>>()?;
    let max = channels.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if !max.is_finite() {
        return Err("max() arg is an empty sequence".into());
    }
    let muted = sink.get("mute").map(tessel_core::pyjson::truthy).ok_or("KeyError: 'mute'")?;
    Ok(Volume { sink: name, value: (max / 65536.0 * 100.0).round_ties_even() as i64, muted })
}

/// `change_volume(action)`: up/down/N%/mute/unmute/toggle, verified by reading back.
pub fn change_volume(action: &str) -> Result<Volume, String> {
    let current = read_volume()?;
    let mut target = None;
    let args: Vec<String> = match action {
        "mute" | "unmute" | "toggle" => {
            let flag = match action { "mute" => "1", "unmute" => "0", _ => "toggle" };
            vec!["pactl".into(), "set-sink-mute".into(), current.sink.clone(), flag.into()]
        }
        _ => {
            let value = resolve_level(current.value, action, 0)?;
            target = Some(value);
            vec!["pactl".into(), "set-sink-volume".into(), current.sink.clone(), format!("{value}%")]
        }
    };
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&refs, false)?;
    let result = read_volume()?;
    if result.sink != current.sink || target.is_some_and(|t| result.value != t) {
        return Err("Speaker changed or volume did not match requested level".into());
    }
    if (action == "mute" || action == "unmute") && result.muted != (action == "mute") {
        return Err("Mute setting did not change".into());
    }
    Ok(result)
}

/// One-at-a-time background jobs (Python: `ThreadPoolExecutor(max_workers=1)`).
#[derive(Clone, Default)]
pub struct Worker(Arc<Mutex<()>>);

impl Worker {
    pub fn submit<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(Option<T>) + 'static) {
        let lock = self.0.clone();
        let handle = gio::spawn_blocking(move || {
            let _serial = lock.lock().unwrap_or_else(|e| e.into_inner());
            job()
        });
        glib::spawn_future_local(async move { done(handle.await.ok()) });
    }
}

pub fn read_brightness(path: &Path) -> i64 {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| tessel_core::pyjson::py_int(value.get("brightness")))
        .map(|value| value.clamp(10, 100)).unwrap_or(100)
}

/// `json.dumps({'brightness': value}) + '\n'` via `<path>.tmp` then rename.
pub fn write_brightness(path: &Path, value: i64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, format!("{{\"brightness\": {value}}}\n"))?;
    std::fs::rename(&temporary, path)
}

struct Inner {
    state_path: PathBuf,
    worker: Worker,
    closed: Cell<bool>,
    busy: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    volume: RefCell<Option<Volume>>,
    current: RefCell<Option<Value>>,
    brightness: Cell<i64>,
    dimmer: gtk::Box,
    revealer: gtk::Revealer,
    icon: gtk::Image,
    label: gtk::Label,
    value: gtk::Label,
    progress: gtk::ProgressBar,
    reader: Arc<dyn Fn() -> Result<Volume, String> + Send + Sync>,
    changer: Arc<dyn Fn(&str) -> Result<Volume, String> + Send + Sync>,
}

/// `ScreenLevels(overlay, state_path)`.
#[derive(Clone)]
pub struct ScreenLevels(Rc<Inner>);

impl ScreenLevels {
    pub fn new(overlay: &gtk::Overlay, state_path: &Path) -> Self {
        let state_path = tessel_locking::expanduser(state_path);
        let brightness = read_brightness(&state_path);
        // Added above the dashboard, below the indicator. Does not intercept input.
        let dimmer = gtk::Box::builder().hexpand(true).vexpand(true).can_target(false).css_classes(["screen-dimmer"]).build();
        dimmer.set_opacity(1.0 - brightness as f64 / 100.0);
        overlay.add_overlay(&dimmer);
        let revealer = gtk::Revealer::builder().halign(gtk::Align::End).valign(gtk::Align::Start).margin_top(28).margin_end(28)
            .can_target(false).build();
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_transition_duration(180);
        let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).css_classes(["screen-level"]).build();
        card.set_size_request(320, -1);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let icon = gtk::Image::builder().pixel_size(30).build();
        let label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        let value = gtk::Label::builder().xalign(1.0).build();
        row.append(&icon);
        row.append(&label);
        row.append(&value);
        card.append(&row);
        let progress = gtk::ProgressBar::new();
        card.append(&progress);
        revealer.set_child(Some(&card));
        overlay.add_overlay(&revealer);
        Self(Rc::new(Inner {
            state_path, worker: Worker::default(), closed: Cell::new(false), busy: Cell::new(false), timer: RefCell::new(None),
            volume: RefCell::new(None), current: RefCell::new(None), brightness: Cell::new(brightness), dimmer, revealer, icon,
            label, value, progress, reader: Arc::new(read_volume), changer: Arc::new(change_volume),
        }))
    }

    pub fn brightness(&self) -> i64 { self.0.brightness.get() }
    pub fn dimmer(&self) -> &gtk::Box { &self.0.dimmer }
    pub fn revealer(&self) -> &gtk::Revealer { &self.0.revealer }
    pub fn value_label(&self) -> &gtk::Label { &self.0.value }
    pub fn current(&self) -> Option<Value> { self.0.current.borrow().clone() }

    /// `show(kind, value, muted=False, direction='', error=None)`.
    pub fn show(&self, kind: &str, value: i64, muted: bool, direction: &str, error: Option<&str>) {
        let inner = &self.0;
        inner.current.replace(Some(json!({"kind": kind, "value": value, "muted": muted, "direction": direction, "error": error})));
        inner.icon.set_icon_name(Some(if kind == "brightness" { "display-brightness-symbolic" }
                                      else if muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" }));
        inner.label.set_text(if kind == "brightness" { "Screen brightness" } else { "Volume" });
        let text = match error {
            Some(error) => error.to_owned(),
            None if muted => "Muted".into(),
            None => tessel_core::pyjson::py_strip(&format!("{direction} {value}%")).to_owned(),
        };
        inner.value.set_text(&text);
        inner.progress.set_fraction(if muted || error.is_some() { 0.0 } else { (value as f64 / 100.0).clamp(0.0, 1.0) });
        inner.revealer.set_reveal_child(true);
        if let Some(id) = inner.timer.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            if let Some(inner) = weak.upgrade() {
                inner.timer.replace(None);
                ScreenLevels(inner).hide();
            }
        });
        inner.timer.replace(Some(id));
    }

    pub fn hide(&self) {
        if let Some(id) = self.0.timer.borrow_mut().take() {
            id.remove();
        }
        self.0.current.replace(None);
        self.0.revealer.set_reveal_child(false);
    }

    /// Poll the default sink (every second); shows the card on external changes.
    pub fn refresh(&self) {
        if self.0.closed.get() || self.0.busy.get() {
            return;
        }
        self.0.busy.set(true);
        let reader = self.0.reader.clone();
        let weak = Rc::downgrade(&self.0);
        self.0.worker.submit(move || reader(), move |result| {
            if let Some(inner) = weak.upgrade() {
                ScreenLevels(inner).volume_ready(result.unwrap_or_else(|| Err("worker".into())), false);
            }
        });
    }

    fn volume_ready(&self, result: Result<Volume, String>, requested: bool) {
        self.0.busy.set(false);
        if self.0.closed.get() {
            return;
        }
        let value = match result {
            Ok(value) => value,
            Err(_) => {
                if requested {
                    self.show("volume", 0, false, "", Some("Unavailable"));
                }
                return;
            }
        };
        let old = self.0.volume.replace(Some(value.clone()));
        if requested || old.as_ref().is_some_and(|o| *o != value) {
            let direction = match &old {
                None => "",
                Some(old) if value.value > old.value => "↑",
                Some(old) if value.value < old.value => "↓",
                _ => "",
            };
            self.show("volume", value.value, value.muted, direction, None);
        }
    }

    /// `adjust_volume(action)`: validation errors are returned synchronously.
    pub fn adjust_volume(&self, action: &str) -> Result<(), String> {
        if !matches!(action, "up" | "down" | "mute" | "unmute") {
            resolve_level(0, action, 0)?;
        }
        let action = if action == "mute" { "toggle".to_owned() } else { action.to_owned() };
        let changer = self.0.changer.clone();
        let weak = Rc::downgrade(&self.0);
        self.0.worker.submit(move || changer(&action), move |result| {
            if let Some(inner) = weak.upgrade() {
                ScreenLevels(inner).volume_ready(result.unwrap_or_else(|| Err("worker".into())), true);
            }
        });
        Ok(())
    }

    /// `adjust_brightness(action)`: dims only the idle screen (10–100 %).
    pub fn adjust_brightness(&self, action: &str) -> Result<(), String> {
        let current = self.0.brightness.get();
        let value = resolve_level(current, action, 10)?;
        let direction = if value > current { "↑" } else if value < current { "↓" } else { "" };
        write_brightness(&self.0.state_path, value).map_err(|e| e.to_string())?;
        self.0.brightness.set(value);
        self.0.dimmer.set_opacity(1.0 - value as f64 / 100.0);
        self.show("brightness", value, false, direction, None);
        Ok(())
    }

    pub fn status(&self) -> Value {
        json!({"volume": self.0.volume.borrow().as_ref().map(Volume::to_json), "screen_brightness": self.0.brightness.get(),
               "brightness_mode": "idle-screen dimming", "indicator": self.0.current.borrow().clone()})
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        if let Some(id) = self.0.timer.borrow_mut().take() {
            id.remove();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_level_matches_python() {
        assert_eq!(resolve_level(50, "up", 0), Ok(60));
        assert_eq!(resolve_level(95, "up", 0), Ok(100));
        assert_eq!(resolve_level(15, "down", 10), Ok(10));
        assert_eq!(resolve_level(0, "15%", 0), Ok(15));
        assert_eq!(resolve_level(0, "100", 0), Ok(100));
        assert_eq!(resolve_level(0, "loud", 0), Err("Use up, down, or a percentage".into()));
        assert_eq!(resolve_level(0, "1000", 0), Err("Use up, down, or a percentage".into()));
        assert_eq!(resolve_level(0, "%", 0), Err("Use up, down, or a percentage".into()));
        assert_eq!(resolve_level(0, "5", 10), Err("Choose a level from 10 to 100 percent".into()));
        assert_eq!(resolve_level(0, "101", 0), Err("Choose a level from 0 to 100 percent".into()));
        assert_eq!(resolve_level(0, "-5", 0), Err("Use up, down, or a percentage".into()));
    }

    #[test]
    fn brightness_file_round_trip_and_clamp() {
        let dir = std::env::temp_dir().join(format!("kasumi-levels-{}", std::process::id()));
        let path = dir.join("nested/levels.json");
        assert_eq!(read_brightness(&path), 100);
        write_brightness(&path, 40).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"brightness\": 40}\n");
        assert!(!path.with_extension("tmp").exists());
        assert_eq!(read_brightness(&path), 40);
        std::fs::write(&path, "{\"brightness\": 3}").unwrap();
        assert_eq!(read_brightness(&path), 10);
        std::fs::write(&path, "{\"brightness\": \"250\"}").unwrap();
        assert_eq!(read_brightness(&path), 100);
        std::fs::write(&path, "[]").unwrap();
        assert_eq!(read_brightness(&path), 100);
        let _ = std::fs::remove_dir_all(dir);
    }
}
