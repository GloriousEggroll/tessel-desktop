//! Bounded notification lifetime, history and preferences, independent of GTK
//! (port of notification_model.py).
//!
//! The model never calls back while it is borrowed: every operation queues
//! [`Event`]s that the owner drains with [`NotificationModel::take_events`]
//! (or through [`SharedModel`], which dispatches them to observers in order).

use suite_core::base64::encode as base64_encode;
use suite_core::pyjson::{py_float, py_str_or_empty as py_str, truthy};
use glib::Variant;
use indexmap::IndexMap;
use serde_json::{json, Map, Number, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const MAX_MUTED_APPS: usize = 500;
pub const TOMORROW_HOUR: i32 = 8;
pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ACTIVE: usize = 100;
pub const MAX_HISTORY: i64 = 500;
pub const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
pub const SETTING_KEYS: [&str; 6] =
    ["do_not_disturb", "dnd_until", "suppress_fullscreen", "timeout_ms", "history_limit", "muted_apps"];

/// `DEFAULT_SETTINGS`, in Python's key order.
pub fn default_settings() -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("do_not_disturb".into(), json!(false));
    // Wall-clock epoch seconds when Do Not Disturb turns itself off; 0 = until turned off.
    map.insert("dnd_until".into(), json!(0));
    map.insert("suppress_fullscreen".into(), json!(true));
    map.insert("timeout_ms".into(), json!(6000));
    map.insert("history_limit".into(), json!(100));
    // Application keys (see app_key) whose popups are muted; history is kept.
    map.insert("muted_apps".into(), json!([]));
    map
}

/// `text(value, limit)` for strings: NUL removed, at most `limit` characters.
pub fn text(value: &str, limit: usize) -> String {
    value.chars().filter(|c| *c != '\0').take(limit).collect()
}

/// `text()` of an arbitrary JSON value (`str(value or "")`).
pub fn text_value(value: Option<&Value>, limit: usize) -> String {
    text(&value.map(py_str).unwrap_or_default(), limit)
}

/// A D-Bus hint value after `unpack()`.
#[derive(Clone, Debug, PartialEq)]
pub enum Hint {
    Bool(bool),
    Int(i64),
    Str(String),
    Other(Variant),
}

impl Hint {
    pub fn from_variant(value: &Variant) -> Hint {
        let value = unbox(value);
        match value.type_().as_str() {
            "b" => Hint::Bool(value.get::<bool>().unwrap_or(false)),
            "y" => Hint::Int(value.get::<u8>().unwrap_or(0) as i64),
            "n" => Hint::Int(value.get::<i16>().unwrap_or(0) as i64),
            "q" => Hint::Int(value.get::<u16>().unwrap_or(0) as i64),
            "i" | "h" => Hint::Int(value.get::<i32>().unwrap_or(0) as i64),
            "u" => Hint::Int(value.get::<u32>().unwrap_or(0) as i64),
            "x" => Hint::Int(value.get::<i64>().unwrap_or(0)),
            "t" => Hint::Int(value.get::<u64>().unwrap_or(0).min(i64::MAX as u64) as i64),
            "s" | "o" | "g" => Hint::Str(value.str().unwrap_or("").to_owned()),
            _ => Hint::Other(value),
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Hint::Bool(flag) => *flag,
            Hint::Int(value) => *value != 0,
            Hint::Str(value) => !value.is_empty(),
            Hint::Other(value) => value.n_children() > 0 || value.type_().is_basic(),
        }
    }

    /// `str(value or "")`.
    fn text(&self, limit: usize) -> String {
        if !self.truthy() {
            return String::new();
        }
        text(&match self {
            Hint::Bool(_) => "True".to_owned(),
            Hint::Int(value) => value.to_string(),
            Hint::Str(value) => value.clone(),
            Hint::Other(value) => value.print(false).to_string(),
        }, limit)
    }
}

pub type Hints = IndexMap<String, Hint>;

/// Unwrap nested `v` boxes (Python's recursive `unpack()`).
fn unbox(value: &Variant) -> Variant {
    let mut value = value.clone();
    while value.type_().as_str() == "v" {
        match value.as_variant() {
            Some(inner) => value = inner,
            None => break,
        }
    }
    value
}

/// Parse an `a{sv}` hint dictionary.
pub fn hints_from_variant(value: &Variant) -> Hints {
    let mut hints = Hints::new();
    if value.type_().as_str() != "a{sv}" {
        return hints;
    }
    for entry in value.iter() {
        if let Some(key) = entry.child_value(0).str().map(String::from) {
            hints.insert(key, Hint::from_variant(&entry.child_value(1)));
        }
    }
    hints
}

fn boolean_hint(value: Option<&Hint>) -> bool {
    matches!(value, Some(Hint::Bool(true)) | Some(Hint::Int(1)))
}

fn integer_hint(value: Option<&Hint>, default: i64, minimum: i64, maximum: i64) -> i64 {
    match value {
        Some(Hint::Int(value)) => (*value).clamp(minimum, maximum),
        _ => default,
    }
}

/// `integer()` for JSON values: ints only (bools and floats give `default`).
pub fn integer_value(value: Option<&Value>, default: i64, minimum: i64, maximum: i64) -> i64 {
    match value {
        Some(Value::Number(number)) if !number.is_f64() => {
            let value = number.as_i64().unwrap_or(if number.as_u64().is_some() { i64::MAX } else { i64::MIN });
            value.clamp(minimum, maximum)
        }
        _ => default,
    }
}

/// `boolean()` for JSON values.
pub fn boolean_value(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => !number.is_f64() && number.as_i64() == Some(1),
        _ => false,
    }
}

/// Validated raw image (`image-data` hint), stored base64-encoded.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageData {
    pub width: i64,
    pub height: i64,
    pub rowstride: i64,
    pub has_alpha: bool,
    pub channels: i64,
    pub data: String,
}

impl ImageData {
    pub fn to_json(&self) -> Value {
        json!({"width": self.width, "height": self.height, "rowstride": self.rowstride,
               "has_alpha": self.has_alpha, "channels": self.channels, "data": self.data})
    }
}

/// Validate before copying/encoding any raw image, including row padding.
pub fn image_data(value: &Hint) -> Option<ImageData> {
    let Hint::Other(value) = value else { return None };
    if !value.type_().is_tuple() || value.n_children() != 7 {
        return None;
    }
    let child = |index| unbox(&value.child_value(index));
    let int = |index| match Hint::from_variant(&child(index)) {
        Hint::Int(value) => Some(value),
        _ => None,
    };
    let (width, height, stride, bits, channels) = (int(0)?, int(1)?, int(2)?, int(4)?, int(5)?);
    let alpha = match Hint::from_variant(&child(3)) {
        Hint::Bool(flag) => flag,
        _ => return None,
    };
    raw_image(width, height, stride, alpha, bits, channels, &child(6))
}

fn raw_image(width: i64, height: i64, stride: i64, alpha: bool, bits: i64, channels: i64, data: &Variant)
             -> Option<ImageData> {
    if !(0 < width && width <= 2048 && 0 < height && height <= 2048 && bits == 8) {
        return None;
    }
    if channels != if alpha { 4 } else { 3 } {
        return None;
    }
    if stride < width * channels || stride * height > MAX_IMAGE_BYTES as i64 {
        return None;
    }
    let needed = ((height - 1) * stride + width * channels) as usize;
    let kind = data.type_();
    if !kind.is_array() {
        return None;
    }
    let length = data.n_children();
    if !(needed <= length && length <= MAX_IMAGE_BYTES) {
        return None;
    }
    // The protocol allows the last row to omit its trailing padding.
    let raw: Vec<u8> = if kind.as_str() == "ay" {
        data.fixed_array::<u8>().ok()?[..needed].to_vec()
    } else {
        let mut bytes = Vec::with_capacity(needed);
        for index in 0..needed {
            match Hint::from_variant(&data.child_value(index)) {
                Hint::Int(value) if (0..=255).contains(&value) => bytes.push(value as u8),
                Hint::Bool(flag) => bytes.push(flag as u8),
                _ => return None,
            }
        }
        bytes
    };
    Some(ImageData { width, height, rowstride: stride, has_alpha: alpha, channels, data: base64_encode(&raw) })
}

/// One notification. Optional fields mirror keys a Python dict may lack.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub icon: String,
    pub image_path: String,
    pub image: Option<ImageData>,
    pub actions: Vec<(String, String)>,
    pub urgency: i64,
    pub progress: Option<i64>,
    pub timestamp: f64,
    pub active: bool,
    pub read: bool,
    pub transient: bool,
    pub resident: bool,
    pub desktop_entry: String,
    pub category: String,
    pub stack_tag: String,
    pub sender: String,
    /// Absent on items restored from the journal.
    pub output: Option<String>,
    pub workspace: Option<bool>,
    pub expire_timeout: i64,
    /// Monotonic seconds.
    pub deadline: Option<f64>,
    pub hover_remaining: Option<f64>,
    pub close_reason: Option<u32>,
}

impl Item {
    /// The public dict (`state()` entry): without deadline/sender/hover_remaining.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("id".into(), json!(self.id));
        map.insert("app_name".into(), json!(self.app_name));
        map.insert("summary".into(), json!(self.summary));
        map.insert("body".into(), json!(self.body));
        map.insert("icon".into(), json!(self.icon));
        map.insert("image_path".into(), json!(self.image_path));
        map.insert("image".into(), self.image.as_ref().map_or(Value::Null, ImageData::to_json));
        map.insert("actions".into(), Value::Array(self.actions.iter().map(|(k, l)| json!([k, l])).collect()));
        map.insert("urgency".into(), json!(self.urgency));
        map.insert("progress".into(), self.progress.map_or(Value::Null, |p| json!(p)));
        map.insert("timestamp".into(), float(self.timestamp));
        map.insert("active".into(), json!(self.active));
        map.insert("read".into(), json!(self.read));
        map.insert("transient".into(), json!(self.transient));
        map.insert("resident".into(), json!(self.resident));
        map.insert("desktop_entry".into(), json!(self.desktop_entry));
        map.insert("category".into(), json!(self.category));
        map.insert("stack_tag".into(), json!(self.stack_tag));
        if let Some(output) = &self.output {
            map.insert("output".into(), json!(output));
        }
        if let Some(workspace) = self.workspace {
            map.insert("workspace".into(), json!(workspace));
        }
        map.insert("expire_timeout".into(), json!(self.expire_timeout));
        if let Some(reason) = self.close_reason {
            map.insert("close_reason".into(), json!(reason));
        }
        map
    }
}

fn float(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// Stable per-application identity: the desktop entry, else the app name.
pub fn app_key_of(desktop_entry: &str, app_name: &str) -> String {
    let entry = text(desktop_entry, 256);
    if !entry.is_empty() {
        return entry;
    }
    let name = text(app_name, 256);
    if !name.is_empty() { name } else { "Application".into() }
}

/// `app_key(item)` for a state dict.
pub fn app_key(item: &Value) -> String {
    let entry = text_value(item.get("desktop_entry"), 256);
    if !entry.is_empty() {
        return entry;
    }
    let name = text_value(item.get("app_name"), 256);
    if !name.is_empty() { name } else { "Application".into() }
}

/// One `app_groups` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct AppGroup {
    pub key: String,
    pub name: String,
    pub icon: Value,
    pub items: Vec<Value>,
}

/// Group newest-first notifications by application, newest group first.
pub fn app_groups(notifications: &[Value]) -> Vec<AppGroup> {
    let mut groups: IndexMap<String, AppGroup> = IndexMap::new();
    for item in notifications {
        let key = app_key(item);
        let group = groups.entry(key.clone()).or_insert_with(|| {
            let name = text_value(item.get("app_name"), 256);
            AppGroup { key: key.clone(), name: if name.is_empty() { key.clone() } else { name },
                       icon: item.get("icon").cloned().unwrap_or_else(|| json!("")), items: Vec::new() }
        });
        group.items.push(item.clone());
    }
    groups.into_values().collect()
}

/// Epoch seconds for a timed Do Not Disturb choice: `hour`, `tomorrow` or `off`.
pub fn dnd_deadline(choice: &str, now: Option<f64>) -> Result<f64, String> {
    let now = now.unwrap_or_else(wall_clock);
    match choice {
        "hour" => Ok(now + 3600.0),
        "tomorrow" => {
            let local = glib::DateTime::from_unix_local(now.floor() as i64).map_err(|e| e.to_string())?;
            let next = local.add_days(1).map_err(|e| e.to_string())?;
            let morning = glib::DateTime::from_local(next.year(), next.month(), next.day_of_month(), TOMORROW_HOUR, 0, 0.0)
                .map_err(|e| e.to_string())?;
            Ok(morning.to_unix() as f64)
        }
        "off" => Ok(0.0),
        _ => Err("Unknown Do Not Disturb duration".into()),
    }
}

pub fn wall_clock() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The GLib monotonic clock in seconds (same base as GLib timeouts).
pub fn monotonic() -> f64 {
    glib::monotonic_time() as f64 / 1_000_000.0
}

/// Change kinds: `notify`, `close`, `settings`, `history`, `read`, `presentation`.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Changed(&'static str, u32),
    Closed(u32, u32),
    Action(u32, String),
}

/// Arguments of `Notify` (`susssasa{sv}i`).
#[derive(Clone, Debug, Default)]
pub struct NotifyArgs {
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<String>,
    pub hints: Hints,
    pub expire_timeout: i32,
}

pub struct NotificationModel {
    pub directory: PathBuf,
    pub settings: Map<String, Value>,
    pub items: IndexMap<u32, Item>,
    pub next_id: u32,
    pub storage_error: String,
    pub presentation: (bool, bool),
    clock: Box<dyn Fn() -> f64>,
    monotonic: Box<dyn Fn() -> f64>,
    events: Vec<Event>,
}

impl NotificationModel {
    /// `directory` defaults to `$XDG_STATE_HOME/notification-manager`.
    pub fn new(directory: Option<&Path>) -> Self {
        Self::with_clocks(directory, Box::new(wall_clock), Box::new(monotonic))
    }

    pub fn with_clocks(directory: Option<&Path>, clock: Box<dyn Fn() -> f64>, monotonic: Box<dyn Fn() -> f64>) -> Self {
        let directory = directory.map(Path::to_path_buf)
            .unwrap_or_else(|| suite_core::paths::state_home().join("notification-manager"));
        let mut model = Self {
            directory, settings: default_settings(), items: IndexMap::new(), next_id: 1, storage_error: String::new(),
            presentation: (false, false), clock, monotonic, events: Vec::new(),
        };
        model.load();
        model
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    fn changed(&mut self, kind: &'static str, ident: u32) {
        self.save();
        self.events.push(Event::Changed(kind, ident));
    }

    fn new_id(&mut self) -> u32 {
        while self.items.contains_key(&self.next_id) {
            self.next_id = self.next_id % 0xffff_ffff + 1;
        }
        let ident = self.next_id;
        self.next_id = ident % 0xffff_ffff + 1;
        ident
    }

    pub fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.settings.get(key).map_or(default, truthy)
    }

    pub fn timeout_ms(&self) -> i64 {
        self.settings.get("timeout_ms").and_then(Value::as_i64).unwrap_or(6000)
    }

    pub fn history_limit(&self) -> usize {
        self.settings.get("history_limit").and_then(Value::as_i64).unwrap_or(100).max(0) as usize
    }

    pub fn dnd_until(&self) -> f64 {
        self.settings.get("dnd_until").and_then(Value::as_f64).unwrap_or(0.0)
    }

    pub fn muted_apps(&self) -> Vec<String> {
        self.settings.get("muted_apps").and_then(Value::as_array)
            .map(|list| list.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    }

    pub fn notify(&mut self, args: NotifyArgs, sender: &str) -> u32 {
        let hints = &args.hints;
        let mut app_name = text(&args.app_name, 4096);
        if app_name.is_empty() {
            app_name = "Application".into();
        }
        let sender = text(sender, 256);
        let first = hints.get("x-dunst-stack-tag").filter(|h| h.truthy())
            .or_else(|| hints.get("x-canonical-private-synchronous"));
        let stack_tag = first.map_or_else(String::new, |h| h.text(256));
        let desktop_entry = hints.get("desktop-entry").map_or_else(String::new, |h| h.text(256));
        let mut old = self.items.get(&args.replaces_id).filter(|item| item.active).map(|item| item.id);
        if old.is_none() && !stack_tag.is_empty() {
            old = self.items.values().find(|item| item.active && item.stack_tag == stack_tag
                && item.app_name == app_name && item.desktop_entry == desktop_entry).map(|item| item.id);
        }
        let ident = old.unwrap_or_else(|| self.new_id());
        let urgency = integer_hint(hints.get("urgency"), 1, 0, 2);
        let mut timeout = (args.expire_timeout as i64).clamp(-1, 0x7fff_ffff);
        if timeout == -1 {
            timeout = if urgency == 2 { 0 } else { self.timeout_ms() };
        }
        let mut pairs: Vec<(String, String)> = Vec::new();
        let limit = (args.actions.len() as i64 - 1).min(32);
        let mut index = 0;
        while (index as i64) < limit {
            let key = text(&args.actions[index], 256);
            let label = text(&args.actions[index + 1], 1024);
            if !key.is_empty() && !pairs.iter().any(|(k, _)| *k == key) {
                pairs.push((key, label));
            }
            index += 2;
        }
        let mut image = hints.get("image-data").or_else(|| hints.get("image_data")).and_then(image_data);
        let image_path = hints.get("image-path").or_else(|| hints.get("image_path")).map_or_else(String::new, |h| h.text(4096));
        if image.is_none() && image_path.is_empty() {
            image = hints.get("icon_data").and_then(image_data);
        }
        let now = (self.monotonic)();
        let item = Item {
            id: ident, app_name, summary: text(&args.summary, 4096), body: text(&args.body, 16384),
            icon: text(&args.app_icon, 4096), image_path, image, actions: pairs, urgency,
            progress: hints.contains_key("value").then(|| integer_hint(hints.get("value"), 0, 0, 100)),
            timestamp: (self.clock)(), active: true, read: false,
            transient: boolean_hint(hints.get("transient")), resident: boolean_hint(hints.get("resident")),
            desktop_entry, category: hints.get("category").map_or_else(String::new, |h| h.text(256)),
            stack_tag, sender,
            output: Some(hints.get("x-waydock-output").map_or_else(String::new, |h| h.text(128))),
            workspace: Some(boolean_hint(hints.get("x-waydock-workspace"))),
            expire_timeout: timeout,
            deadline: (timeout != 0).then(|| now + timeout as f64 / 1000.0),
            hover_remaining: None, close_reason: None,
        };
        self.items.insert(ident, item);
        self.trim();
        self.changed("notify", ident);
        ident
    }

    pub fn close(&mut self, ident: u32, reason: u32) -> bool {
        let Some(item) = self.items.get_mut(&ident) else { return false };
        if !item.active {
            return false;
        }
        item.active = false;
        item.deadline = None;
        item.close_reason = Some(reason);
        if reason == 2 {
            item.read = true;
        }
        if item.transient {
            self.items.shift_remove(&ident);
        }
        // Invalidate before signaling, as required by the standard protocol.
        self.events.push(Event::Closed(ident, reason));
        self.trim();
        self.changed("close", ident);
        true
    }

    pub fn activate(&mut self, ident: u32, key: &str) -> bool {
        let Some(item) = self.items.get_mut(&ident) else { return false };
        if !item.active || !item.actions.iter().any(|(k, _)| k == key) {
            return false;
        }
        item.read = true;
        let resident = item.resident;
        self.events.push(Event::Action(ident, key.to_owned()));
        if !resident {
            self.close(ident, 2);
        } else {
            self.changed("read", ident);
        }
        true
    }

    pub fn dismiss(&mut self, ident: u32) -> bool {
        let Some(item) = self.items.get(&ident) else { return false };
        if item.active {
            return self.close(ident, 2);
        }
        self.items.shift_remove(&ident);
        self.changed("history", ident);
        true
    }

    pub fn tick(&mut self) {
        let now = (self.monotonic)();
        let due: Vec<u32> = self.items.values()
            .filter(|item| item.active && item.deadline.is_some_and(|d| d <= now)).map(|item| item.id).collect();
        for ident in due {
            // An earlier close in this pass may have trimmed or closed it.
            let still = self.items.get(&ident).is_some_and(|item| item.active && item.deadline.is_some_and(|d| d <= now));
            if still {
                self.close(ident, 1);
            }
        }
    }

    /// Earliest monotonic expiry among live notifications, or `None` when idle.
    pub fn next_deadline(&self) -> Option<f64> {
        self.items.values().filter(|item| item.active).filter_map(|item| item.deadline).reduce(f64::min)
    }

    /// Do Not Disturb, honoring a timed end; pure, the timer calls `expire_dnd`.
    pub fn dnd_active(&self) -> bool {
        let until = self.dnd_until();
        self.setting_bool("do_not_disturb", false) && (until == 0.0 || (self.clock)() < until)
    }

    /// Turn a timed Do Not Disturb off once its end has passed.
    pub fn expire_dnd(&mut self) -> bool {
        let until = self.dnd_until();
        if self.setting_bool("do_not_disturb", false) && until != 0.0 && (self.clock)() >= until {
            let _ = self.set_settings(&json!({"do_not_disturb": false, "dnd_until": 0}));
            return true;
        }
        false
    }

    pub fn app_muted_key(&self, key: &str) -> bool {
        self.settings.get("muted_apps").and_then(Value::as_array).is_some_and(|list| list.iter().any(|v| v.as_str() == Some(key)))
    }

    pub fn item_muted(&self, item: &Item) -> bool {
        self.app_muted_key(&app_key_of(&item.desktop_entry, &item.app_name))
    }

    /// Remove one application's history; live ones close as dismissed.
    pub fn clear_app(&mut self, key: &str) -> bool {
        let matching: Vec<(u32, bool)> = self.items.values()
            .filter(|item| app_key_of(&item.desktop_entry, &item.app_name) == key).map(|item| (item.id, item.active)).collect();
        if matching.is_empty() {
            return false;
        }
        for (ident, _) in &matching {
            self.items.shift_remove(ident);
        }
        for (ident, active) in matching {
            if active {
                self.events.push(Event::Closed(ident, 2));
            }
        }
        self.changed("history", 0);
        true
    }

    pub fn set_hovered(&mut self, ident: u32, hovered: bool) {
        let now = (self.monotonic)();
        let Some(item) = self.items.get_mut(&ident) else { return };
        if !item.active {
            return;
        }
        if hovered {
            if let Some(deadline) = item.deadline {
                item.hover_remaining = Some((deadline - now).max(0.0));
                item.deadline = None;
            }
        } else if let Some(remaining) = item.hover_remaining.take() {
            item.deadline = Some(now + remaining);
        }
    }

    pub fn set_presentation(&mut self, fullscreen: bool, checking: bool) {
        if self.presentation != (fullscreen, checking) {
            self.presentation = (fullscreen, checking);
            self.events.push(Event::Changed("presentation", 0));
        }
    }

    pub fn mark_read(&mut self) {
        if self.items.values().any(|item| !item.read) {
            for item in self.items.values_mut() {
                item.read = true;
            }
            self.changed("read", 0);
        }
    }

    pub fn clear_history(&mut self) {
        let active: Vec<u32> = self.items.values().filter(|item| item.active).map(|item| item.id).collect();
        self.items.clear();
        // One journal write and one UI refresh for the whole operation. Each
        // sender still receives the required closure signal after invalidation.
        for ident in active {
            self.events.push(Event::Closed(ident, 2));
        }
        self.changed("history", 0);
    }

    pub fn validate_settings(settings: &Value) -> Result<&Map<String, Value>, String> {
        let Some(map) = settings.as_object() else { return Err("Settings must be an object".into()) };
        if map.keys().any(|key| !SETTING_KEYS.contains(&key.as_str())) {
            return Err("Unknown notification setting".into());
        }
        let is_int = |value: &Value| matches!(value, Value::Number(n) if !n.is_f64());
        for (key, value) in map {
            match key.as_str() {
                "do_not_disturb" | "suppress_fullscreen" if !value.is_boolean() => {
                    return Err(format!("{key} must be true or false"));
                }
                "timeout_ms" if !(is_int(value) && value.as_i64().is_some_and(|v| (1000..=60000).contains(&v))) => {
                    return Err("timeout_ms must be between 1000 and 60000".into());
                }
                "history_limit" if !(is_int(value) && value.as_i64().is_some_and(|v| (1..=MAX_HISTORY).contains(&v))) => {
                    return Err("history_limit must be between 1 and 500".into());
                }
                "dnd_until" => {
                    let ok = value.as_f64().is_some_and(|v| v.is_finite() && (0.0..(1u64 << 40) as f64).contains(&v));
                    if !value.is_number() || !ok {
                        return Err("dnd_until must be a time in seconds or 0".into());
                    }
                }
                "muted_apps" => {
                    let valid = value.as_array().is_some_and(|list| {
                        let mut seen = std::collections::HashSet::new();
                        list.len() <= MAX_MUTED_APPS && list.iter().all(|name| {
                            name.as_str().is_some_and(|name| {
                                let count = name.chars().count();
                                count > 0 && count <= 256 && !name.contains('\0') && seen.insert(name)
                            })
                        })
                    });
                    if !valid {
                        return Err("muted_apps must be a list of unique application names".into());
                    }
                }
                _ => {}
            }
        }
        Ok(map)
    }

    /// Validate and apply a patch atomically.
    pub fn set_settings(&mut self, patch: &Value) -> Result<(), String> {
        let mut settings = Self::validate_settings(patch)?.clone();
        if settings.contains_key("do_not_disturb") && !settings.contains_key("dnd_until") {
            // A plain toggle (older clients, the bar) means "until turned off".
            settings.insert("dnd_until".into(), json!(0));
        }
        for (key, value) in settings {
            self.settings.insert(key, value);
        }
        self.trim();
        self.changed("settings", 0);
        Ok(())
    }

    fn ordered_ids(&self) -> Vec<u32> {
        let mut ordered: Vec<(f64, u32)> = self.items.values().map(|item| (item.timestamp, item.id)).collect();
        ordered.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
        ordered.into_iter().map(|(_, id)| id).collect()
    }

    fn trim(&mut self) {
        let limit = self.history_limit();
        let (mut active, mut history, mut image_bytes) = (0usize, 0usize, 0usize);
        for ident in self.ordered_ids() {
            let Some(item) = self.items.get(&ident) else { continue };
            let is_active = item.active;
            if is_active {
                active += 1;
            }
            if !item.transient {
                history += 1;
            }
            if (is_active && active > MAX_ACTIVE) || (!is_active && history > limit) {
                self.items.shift_remove(&ident);
                if is_active {
                    self.events.push(Event::Closed(ident, 4));
                }
                continue;
            }
            let item = self.items.get_mut(&ident).expect("present");
            if let Some(image) = &item.image {
                image_bytes += image.data.len();
                if image_bytes > MAX_TOTAL_IMAGE_BYTES {
                    item.image = None;
                }
            }
        }
    }

    /// Newest-first public notification dicts.
    pub fn notifications(&self) -> Vec<Value> {
        self.ordered_ids().into_iter().filter_map(|id| self.items.get(&id)).map(|item| Value::Object(item.to_json())).collect()
    }

    pub fn unread(&self) -> usize {
        self.items.values().filter(|item| !item.read && !item.transient).count()
    }

    pub fn state(&self) -> Value {
        json!({"settings": self.settings.clone(), "notifications": self.notifications(), "unread": self.unread(),
               "storage_error": self.storage_error,
               "presentation": {"fullscreen": self.presentation.0, "checking": self.presentation.1},
               "ready": true, "error": ""})
    }

    fn save(&mut self) {
        let limit = self.history_limit();
        let mut history = Vec::new();
        for ident in self.ordered_ids() {
            let item = &self.items[&ident];
            if item.transient {
                continue;
            }
            // Pixel buffers and sender actions are session-local; the journal
            // stores bounded plain text and theme icons, never raw screenshots.
            let mut entry = item.to_json();
            entry.insert("image".into(), Value::Null);
            entry.insert("actions".into(), json!([]));
            entry.insert("active".into(), json!(false));
            history.push(Value::Object(entry));
        }
        history.truncate(limit);
        let payload = json!({"version": 1, "next_id": self.next_id, "settings": self.settings.clone(), "history": history});
        let text = suite_core::pyfmt::json_dumps(&payload, &suite_core::pyfmt::JsonStyle { ensure_ascii: false,
            ..suite_core::pyfmt::JsonStyle::DEFAULT });
        self.storage_error = match suite_core::fsutil::write_private(&self.directory, "state.json", ".notifications-", text.as_bytes()) {
            Ok(()) => String::new(),
            Err(error) => error,
        };
    }

    fn load(&mut self) {
        let path = self.directory.join("state.json");
        let Ok(meta) = std::fs::metadata(&path) else { return };
        if meta.len() > MAX_STATE_BYTES {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(payload) = serde_json::from_str::<Value>(&text) else { return };
        let Some(object) = payload.as_object() else { return };
        let version_one = match object.get("version") {
            Some(Value::Number(n)) => n.as_f64() == Some(1.0),
            Some(Value::Bool(true)) => true,
            _ => false,
        };
        if !version_one {
            return;
        }
        let empty = json!({});
        let Ok(settings) = Self::validate_settings(object.get("settings").unwrap_or(&empty)) else { return };
        for (key, value) in settings.clone() {
            self.settings.insert(key, value);
        }
        self.next_id = integer_value(object.get("next_id"), 1, 1, 0xffff_ffff) as u32;
        let Some(Value::Array(history)) = object.get("history") else { return };
        for raw in history.iter().take(self.history_limit()) {
            let Some(raw) = raw.as_object() else { continue };
            if raw.get("transient").is_some_and(truthy) {
                continue;
            }
            let ident = integer_value(raw.get("id"), 0, 0, 0xffff_ffff) as u32;
            if ident == 0 {
                continue;
            }
            // float(raw.get("timestamp", 0)); a TypeError/ValueError aborts the load.
            let Some(mut timestamp) = py_float(raw.get("timestamp").unwrap_or(&json!(0))) else { return };
            if !timestamp.is_finite() {
                timestamp = 0.0;
            }
            let get = |key: &str, limit| text_value(raw.get(key), limit);
            let progress = raw.get("progress").filter(|v| !v.is_null()).map(|v| integer_value(Some(v), 0, 0, 100));
            self.items.insert(ident, Item {
                id: ident, app_name: get("app_name", 4096), summary: get("summary", 4096), body: get("body", 16384),
                icon: get("icon", 4096), image_path: get("image_path", 4096), image: None, actions: Vec::new(),
                urgency: integer_value(raw.get("urgency"), 1, 0, 2), progress, timestamp, active: false,
                read: boolean_value(raw.get("read")), transient: false, resident: false,
                desktop_entry: get("desktop_entry", 256), category: get("category", 256), stack_tag: String::new(),
                sender: String::new(), output: None, workspace: None, expire_timeout: 0, deadline: None,
                hover_remaining: None, close_reason: None,
            });
        }
    }
}

type Observer = Rc<dyn Fn(&Event)>;

/// The model shared by the service, daemon and UI: operations drain the
/// model's events and dispatch them to observers after the borrow ends.
#[derive(Clone)]
pub struct SharedModel(Rc<SharedInner>);

struct SharedInner {
    model: RefCell<NotificationModel>,
    observers: RefCell<Vec<(u64, Observer)>>,
    sequence: std::cell::Cell<u64>,
}

impl SharedModel {
    pub fn new(model: NotificationModel) -> Self {
        Self(Rc::new(SharedInner { model: RefCell::new(model), observers: RefCell::new(Vec::new()),
                                   sequence: std::cell::Cell::new(0) }))
    }

    pub fn subscribe(&self, observer: impl Fn(&Event) + 'static) -> u64 {
        let id = self.0.sequence.get() + 1;
        self.0.sequence.set(id);
        self.0.observers.borrow_mut().push((id, Rc::new(observer)));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.0.observers.borrow_mut().retain(|(known, _)| *known != id);
    }

    /// Read-only access.
    pub fn with<R>(&self, f: impl FnOnce(&NotificationModel) -> R) -> R {
        f(&self.0.model.borrow())
    }

    /// Mutate, then dispatch the queued events.
    pub fn update<R>(&self, f: impl FnOnce(&mut NotificationModel) -> R) -> R {
        let (result, events) = {
            let mut model = self.0.model.borrow_mut();
            let result = f(&mut model);
            (result, model.take_events())
        };
        for event in events {
            let observers: Vec<Observer> = self.0.observers.borrow().iter().map(|(_, o)| o.clone()).collect();
            for observer in observers {
                observer(&event);
            }
        }
        result
    }

    pub fn state(&self) -> Value { self.with(NotificationModel::state) }
    pub fn notify(&self, args: NotifyArgs, sender: &str) -> u32 { self.update(|m| m.notify(args, sender)) }
    pub fn close(&self, ident: u32, reason: u32) -> bool { self.update(|m| m.close(ident, reason)) }
    pub fn activate(&self, ident: u32, key: &str) -> bool { self.update(|m| m.activate(ident, key)) }
    pub fn dismiss(&self, ident: u32) -> bool { self.update(|m| m.dismiss(ident)) }
    pub fn clear_app(&self, key: &str) -> bool { self.update(|m| m.clear_app(key)) }
    pub fn clear_history(&self) { self.update(|m| m.clear_history()) }
    pub fn mark_read(&self) { self.update(|m| m.mark_read()) }
    pub fn set_settings(&self, patch: &Value) -> Result<(), String> { self.update(|m| m.set_settings(patch)) }
    pub fn tick(&self) { self.update(|m| m.tick()) }
    pub fn expire_dnd(&self) -> bool { self.update(|m| m.expire_dnd()) }
    pub fn set_hovered(&self, ident: u32, hovered: bool) { self.update(|m| m.set_hovered(ident, hovered)) }
    pub fn set_presentation(&self, fullscreen: bool, checking: bool) { self.update(|m| m.set_presentation(fullscreen, checking)) }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
