//! `timer_countdowns.py`, `alarm_indicators.py` (read-only scheduler
//! snapshots), `screen_timers.py` (TimerCards/ScreenTimers) and
//! `screen_alarms.py`.
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use serde_json::{json, Map, Value};
use tessel_core::pyjson::{py_float, py_int, py_str, py_strip};

use crate::levels::Worker;

pub const TIMER_CSS: &str = r#"
.timer-card { background: rgba(20,27,39,.78); border: 1px solid rgba(186,218,255,.30);
    border-radius: 22px; padding: 14px 24px; min-width: 180px; }
.timer-card .timer-title { font-size: 21px; font-weight: 500; color: #dcecff; }
.timer-card .timer-digits { font-size: 70px; font-weight: 600; font-feature-settings: "tnum"; color: white; }
.timer-card .timer-caption { font-size: 17px; color: #c2d7ec; }
.timer-card.timer-ringing { border-color: #ffd08a; background: rgba(57,42,22,.88); }
.timer-card.timer-ringing .timer-digits, .timer-card.timer-ringing .timer-caption { color: #ffda9d; }
.lock-compact .timer-card { padding: 10px 18px; min-width: 150px; }
.lock-compact .timer-card .timer-digits { font-size: 54px; }
.timer-card.compact-timer-card { padding: 8px 14px; min-width: 140px; border-radius: 14px; }
.timer-card.compact-timer-card .timer-title { font-size: 15px; }
.timer-card.compact-timer-card .timer-digits { font-size: 26px; }
.timer-card.compact-timer-card .timer-caption { font-size: 13px; }
"#;

pub const ALARM_CSS: &str = "
.timer-card.alarm-card { padding: 8px 14px; min-width: 140px; border-radius: 14px; background: rgba(20,27,39,.72); }
.timer-card.alarm-card .timer-title { font-size: 15px; }
.timer-card.alarm-card .timer-digits { font-size: 26px; }
.timer-card.alarm-card .timer-caption { font-size: 13px; }
";

/// `SOCKET = Path.home()/'.local/share/kasumi-control/voice.sock'`.
pub fn socket_path() -> PathBuf { tessel_core::paths::home().join(".local/share/kasumi-control/voice.sock") }

/// `read_snapshot(operation, path)`: one request line, one ≤ 262145-byte reply line, 0.7 s timeout.
pub fn read_snapshot(operation: &str, path: &Path) -> Result<Map<String, Value>, String> {
    let mut client = UnixStream::connect(path).map_err(|e| e.to_string())?;
    let timeout = Some(Duration::from_millis(700));
    client.set_read_timeout(timeout).map_err(|e| e.to_string())?;
    client.set_write_timeout(timeout).map_err(|e| e.to_string())?;
    let request = tessel_core::pyfmt::json_dumps(&json!({"operation": operation}), &tessel_core::pyfmt::JsonStyle::DEFAULT);
    client.write_all(format!("{request}\n").as_bytes()).map_err(|e| e.to_string())?;
    let line = read_line_until(&mut client, 262_145, std::time::Instant::now() + Duration::from_millis(700))?;
    let response: Value = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
    let Value::Object(response) = response else { return Err("Invalid scheduler response".into()) };
    if let Some(error) = response.get("error") {
        return Err(py_str(error));
    }
    Ok(response)
}

/// One reply line of at most `limit` bytes, within an overall `deadline`
/// (hardening: Python's 0.7 s timeout was per recv, so a peer trickling one
/// byte at a time could hold the worker for hours).
fn read_line_until(client: &mut UnixStream, limit: usize, deadline: std::time::Instant) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    let mut buffer = [0u8; 8192];
    while line.len() < limit && !line.contains(&b'\n') {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err("timed out".into());
        }
        client.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
        let want = buffer.len().min(limit - line.len());
        match client.read(&mut buffer[..want]) {
            Ok(0) => break,
            Ok(n) => line.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    if let Some(end) = line.iter().position(|b| *b == b'\n') {
        line.truncate(end + 1);
    }
    Ok(line)
}

fn field(mut response: Map<String, Value>, key: &str) -> Result<Vec<Value>, String> {
    match response.remove(key) {
        Some(Value::Array(rows)) => Ok(rows),
        Some(_) => Err(format!("Invalid '{key}'")),
        None => Err(format!("KeyError: '{key}'")),
    }
}

pub fn read_countdowns(path: &Path) -> Result<Vec<Value>, String> { field(read_snapshot("countdowns", path)?, "timers") }
pub fn read_alarms(path: &Path) -> Result<Vec<Value>, String> { field(read_snapshot("alarms_today", path)?, "alarms") }

/// `remaining_text(seconds)`.
pub fn remaining_text(seconds: f64) -> String {
    let seconds = seconds.ceil().max(0.0) as i64;
    let (days, seconds) = (seconds / 86400, seconds % 86400);
    let (hours, seconds) = (seconds / 3600, seconds % 3600);
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    if days > 0 {
        format!("{days}d {hours:02}:{minutes:02}:{seconds:02}")
    } else if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn get<'a>(row: &'a Value, key: &str) -> Result<&'a Value, String> { row.get(key).ok_or_else(|| format!("KeyError: '{key}'")) }

fn truthy(row: &Value, key: &str) -> bool { row.get(key).is_some_and(tessel_core::pyjson::truthy) }

/// Python ordering of JSON scalars used in sort keys.
fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => match (py_float(a), py_float(b)) {
            (Some(x), Some(y)) if !matches!(a, Value::String(_)) && !matches!(b, Value::String(_)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => py_str(a).cmp(&py_str(b)),
        },
    }
}

/// `visible_countdowns(rows, now, compact=False)`: >30 s left (compact) or
/// urgent/ringing (large), never both; sorted by (position, id).
pub fn visible_countdowns(rows: &[Value], now: f64, compact: bool) -> Result<Vec<Map<String, Value>>, String> {
    let mut visible = Vec::new();
    for row in rows {
        let remaining = py_float(get(row, "due")?).ok_or("ValueError: due")? - now;
        let urgent = remaining <= 30.0 || truthy(row, "ringing");
        if compact == urgent {
            continue;
        }
        let seconds = remaining.ceil().max(0.0);
        let position = py_int(Some(get(row, "position")?)).ok_or("ValueError: position")?;
        let label = py_strip(&py_str(get(row, "label")?)).to_owned();
        let title = if label.is_empty() || label.to_lowercase() == "timer" { format!("Timer {position}") }
                    else { format!("{label} · Timer {position}") };
        let mut item = Map::new();
        item.insert("id".into(), get(row, "id")?.clone());
        item.insert("position".into(), position.into());
        item.insert("title".into(), title.into());
        item.insert("seconds".into(), (seconds as i64).into());
        item.insert("text".into(), remaining_text(seconds).into());
        item.insert("ringing".into(), (truthy(row, "ringing") || remaining <= 0.0).into());
        visible.push(item);
    }
    visible.sort_by(|a, b| compare(&a["position"], &b["position"]).then_with(|| compare(&a["id"], &b["id"])));
    Ok(visible)
}

pub fn compact_countdowns(rows: &[Value], now: f64) -> Result<Vec<Map<String, Value>>, String> { visible_countdowns(rows, now, true) }

pub fn large_countdowns(rows: &[Value], now: f64) -> Result<Vec<Map<String, Value>>, String> { visible_countdowns(rows, now, false) }

/// `visible_alarms(rows, now)`: today's (zone-local) alarms, ringing ones
/// always; sorted by (due, id).
pub fn visible_alarms(rows: &[Value], now: f64) -> Result<Vec<Map<String, Value>>, String> {
    use tessel_weather::dates::local;
    let mut visible = Vec::new();
    for row in rows {
        let zone = py_str(get(row, "zone")?);
        let due_value = get(row, "due")?;
        let due = py_float(due_value).filter(|_| !matches!(due_value, Value::String(_))).ok_or("TypeError: due")?;
        let when = local(due, &zone).ok_or_else(|| format!("No time zone found with key {zone}"))?;
        let today = local(now, &zone).ok_or("Invalid time")?;
        if !truthy(row, "ringing") && when.date != today.date {
            continue;
        }
        let label = py_strip(&row.get("label").map(py_str).unwrap_or_else(|| "Alarm".into())).to_owned();
        let label = if label.is_empty() { "Alarm".to_owned() } else { label };
        let title = if label.to_lowercase() == "alarm" { "Alarm".to_owned() } else { format!("{label} · Alarm") };
        let hour12 = if when.hour % 12 == 0 { 12 } else { when.hour % 12 };
        let clock = format!("{hour12}:{:02} {}", when.minute, if when.hour < 12 { "AM" } else { "PM" });
        let seconds = (due - now).ceil().max(0.0);
        let ringing = truthy(row, "ringing") || due <= now;
        let mut item = Map::new();
        item.insert("id".into(), get(row, "id")?.clone());
        item.insert("title".into(), title.into());
        item.insert("text".into(), remaining_text(seconds).into());
        item.insert("seconds".into(), (seconds as i64).into());
        item.insert("alarm_time".into(), clock.clone().into());
        item.insert("caption".into(), (if ringing { format!("Ringing · {clock}") } else { format!("until {clock}") }).into());
        item.insert("ringing".into(), ringing.into());
        item.insert("due".into(), due_value.clone());
        visible.push(item);
    }
    visible.sort_by(|a, b| compare(&a["due"], &b["due"]).then_with(|| compare(&a["id"], &b["id"])));
    Ok(visible)
}

// ───────────────────────────── GTK cards ─────────────────────────────

pub type Card = (gtk::Box, gtk::Label, gtk::Label, gtk::Label);

struct CardsInner {
    style: Option<&'static str>,
    widget: gtk::FlowBox,
    cards: RefCell<Vec<(Value, Card)>>,
    visible: RefCell<Vec<Map<String, Value>>>,
}

/// `TimerCards(style=None)`.
#[derive(Clone)]
pub struct TimerCards(Rc<CardsInner>);

impl TimerCards {
    pub fn new(style: Option<&'static str>) -> Self {
        let widget = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).homogeneous(true).column_spacing(18)
            .row_spacing(12).halign(gtk::Align::Center).visible(false).min_children_per_line(1).max_children_per_line(6).build();
        Self(Rc::new(CardsInner { style, widget, cards: RefCell::new(Vec::new()), visible: RefCell::new(Vec::new()) }))
    }

    pub fn widget(&self) -> &gtk::FlowBox { &self.0.widget }
    pub fn status(&self) -> Vec<Map<String, Value>> { self.0.visible.borrow().clone() }
    /// `self.cards` (insertion order): id → (card, title, digits, caption).
    pub fn cards(&self) -> Vec<(Value, Card)> { self.0.cards.borrow().clone() }
    pub fn card(&self, id: &Value) -> Option<Card> { self.0.cards.borrow().iter().find(|(k, _)| k == id).map(|(_, c)| c.clone()) }

    pub fn render(&self, visible: Vec<Map<String, Value>>) {
        let inner = &self.0;
        let ids: Vec<Value> = visible.iter().map(|row| row["id"].clone()).collect();
        let current: Vec<Value> = inner.cards.borrow().iter().map(|(id, _)| id.clone()).collect();
        if ids != current {
            inner.widget.set_max_children_per_line(visible.len().clamp(1, 6) as u32);
            while let Some(child) = inner.widget.first_child() {
                inner.widget.remove(&child);
            }
            let mut cards = Vec::new();
            for row in &visible {
                let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).css_classes(["timer-card"]).build();
                if let Some(style) = inner.style {
                    card.add_css_class(style);
                }
                let title = gtk::Label::builder().max_width_chars(24).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["timer-title"]).build();
                let digits = gtk::Label::builder().css_classes(["timer-digits"]).build();
                let caption = gtk::Label::builder().css_classes(["timer-caption"]).build();
                card.append(&title);
                card.append(&digits);
                card.append(&caption);
                inner.widget.append(&card);
                cards.push((row["id"].clone(), (card, title, digits, caption)));
            }
            inner.cards.replace(cards);
        }
        for row in &visible {
            let Some((card, title, digits, caption)) = self.card(&row["id"]) else { continue };
            let text = |key: &str| row.get(key).map(py_str).unwrap_or_default();
            title.set_text(&text("title"));
            title.set_tooltip_text(Some(&text("title")));
            digits.set_text(&text("text"));
            let ringing = row.get("ringing").is_some_and(tessel_core::pyjson::truthy);
            let given = row.get("caption").filter(|c| tessel_core::pyjson::truthy(c)).map(py_str);
            caption.set_text(&given.unwrap_or_else(|| (if ringing { "Time’s up" } else { "remaining" }).into()));
            if ringing { card.add_css_class("timer-ringing") } else { card.remove_css_class("timer-ringing") }
        }
        inner.widget.set_visible(!visible.is_empty());
        inner.visible.replace(visible);
    }
}

pub type Reader = Arc<dyn Fn() -> Result<Vec<Value>, String> + Send + Sync>;
pub type Transform = fn(&[Value], f64) -> Result<Vec<Map<String, Value>>, String>;

struct ScreenInner {
    cards: TimerCards,
    compact: Option<TimerCards>,
    reader: Reader,
    transform: Transform,
    worker: Worker,
    closed: Cell<bool>,
    busy: Cell<bool>,
    rows: RefCell<Vec<Value>>,
    received: Cell<f64>,
    clock: RefCell<Rc<dyn Fn() -> f64>>,
}

/// `ScreenTimers(reader, transform, style, compact)`; `ScreenAlarms` is
/// [`ScreenTimers::alarms`].
#[derive(Clone)]
pub struct ScreenTimers(Rc<ScreenInner>);

fn wall_clock() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) }

impl ScreenTimers {
    pub fn new(reader: Reader, transform: Transform, style: Option<&'static str>, compact: bool) -> Self {
        Self(Rc::new(ScreenInner {
            cards: TimerCards::new(style), compact: compact.then(|| TimerCards::new(Some("compact-timer-card"))), reader, transform,
            worker: Worker::default(), closed: Cell::new(false), busy: Cell::new(false), rows: RefCell::new(Vec::new()),
            received: Cell::new(0.0), clock: RefCell::new(Rc::new(wall_clock)),
        }))
    }

    /// `ScreenTimers()`: the scheduler's countdowns with a compact view.
    pub fn timers(socket: PathBuf) -> Self {
        Self::new(Arc::new(move || read_countdowns(&socket)), large_countdowns, None, true)
    }

    /// `ScreenAlarms()`.
    pub fn alarms(socket: PathBuf) -> Self {
        Self::new(Arc::new(move || read_alarms(&socket)), visible_alarms, Some("alarm-card"), false)
    }

    pub fn widget(&self) -> &gtk::FlowBox { self.0.cards.widget() }
    pub fn cards(&self) -> &TimerCards { &self.0.cards }
    pub fn compact(&self) -> Option<&TimerCards> { self.0.compact.as_ref() }
    pub fn status(&self) -> Vec<Map<String, Value>> { self.0.cards.status() }
    pub fn rows(&self) -> Vec<Value> { self.0.rows.borrow().clone() }

    /// Tests: `panel.rows = …; panel.received = time.monotonic() - age`.
    pub fn set_rows(&self, rows: Vec<Value>, age: f64) {
        self.0.rows.replace(rows);
        self.0.received.set(crate::monotonic() - age);
    }

    /// Tests: `patch('screen_timers.time.time', return_value=now)`.
    pub fn set_clock(&self, clock: impl Fn() -> f64 + 'static) { self.0.clock.replace(Rc::new(clock)); }

    pub fn poll(&self) {
        if self.0.closed.get() || self.0.busy.get() {
            return;
        }
        self.0.busy.set(true);
        let reader = self.0.reader.clone();
        let weak = Rc::downgrade(&self.0);
        self.0.worker.submit(move || reader(), move |result| {
            if let Some(inner) = weak.upgrade() {
                ScreenTimers(inner).ready(result.unwrap_or_else(|| Err("worker".into())));
            }
        });
    }

    fn ready(&self, result: Result<Vec<Value>, String>) {
        self.0.busy.set(false);
        if self.0.closed.get() {
            return;
        }
        match result {
            Ok(rows) => {
                self.0.rows.replace(rows);
                self.0.received.set(crate::monotonic());
            }
            Err(_) => {
                if crate::monotonic() - self.0.received.get() > 3.0 {
                    self.0.rows.replace(Vec::new());
                }
            }
        }
        self.tick();
    }

    /// Both views change in one GTK callback, using the same snapshot/time.
    /// Malformed rows leave the cards unchanged (Python raised in the callback).
    pub fn tick(&self) {
        if self.0.closed.get() {
            return;
        }
        let rows = if crate::monotonic() - self.0.received.get() <= 3.0 { self.0.rows.borrow().clone() } else { Vec::new() };
        let now = (self.0.clock.borrow().clone())();
        if let Ok(visible) = (self.0.transform)(&rows, now) {
            self.0.cards.render(visible);
        }
        if let Some(compact) = &self.0.compact {
            if let Ok(visible) = compact_countdowns(&rows, now) {
                compact.render(visible);
            }
        }
    }

    pub fn close(&self) { self.0.closed.set(true); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn remaining_text_formats_like_python() {
        assert_eq!(remaining_text(0.2), "0:01");
        assert_eq!(remaining_text(-5.0), "0:00");
        assert_eq!(remaining_text(90.0), "1:30");
        assert_eq!(remaining_text(3723.0), "1:02:03");
        assert_eq!(remaining_text(32400.0), "9:00:00");
        assert_eq!(remaining_text(90061.0), "1d 01:01:01");
    }

    #[test]
    fn countdowns_split_at_thirty_seconds_and_sort_by_position() {
        let now = 1000.0;
        let rows = vec![json!({"id": 2, "position": 2, "label": "Timer", "due": now + 33.0}),
                        json!({"id": 1, "position": 1, "label": "Pasta", "due": now + 31.0}),
                        json!({"id": 3, "position": 3, "label": " ", "due": now + 10.0, "ringing": false}),
                        json!({"id": 4, "position": 4, "label": "Tea", "due": now - 1.0})];
        let compact = compact_countdowns(&rows, now).unwrap();
        assert_eq!(compact.iter().map(|r| r["text"].clone()).collect::<Vec<_>>(), vec![json!("0:31"), json!("0:33")]);
        assert_eq!(compact[1]["title"], json!("Timer 2"));
        assert_eq!(compact[0]["title"], json!("Pasta · Timer 1"));
        let large = large_countdowns(&rows, now).unwrap();
        assert_eq!(large.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(), vec![json!(3), json!(4)]);
        assert_eq!(large[1]["ringing"], json!(true));
        assert_eq!(large[1]["text"], json!("0:00"));
        let keys: Vec<&String> = large[0].keys().collect();
        assert_eq!(keys, ["id", "position", "title", "seconds", "text", "ringing"]);
        assert!(visible_countdowns(&[json!({"id": 1})], now, false).is_err());
    }

    #[test]
    fn alarms_show_today_only_with_am_pm_and_ringing_across_midnight() {
        let zone = "America/Denver";
        // 2026-10-01 10:00 MDT = 16:00 UTC.
        let base = 1_790_870_400.0;
        let hour = 3600.0;
        let rows = vec![json!({"id": 1, "label": "Work", "zone": zone, "due": base - 3.0 * hour}),
                        json!({"id": 2, "label": "Evening", "zone": zone, "due": base + 9.0 * hour}),
                        json!({"id": 3, "label": "Tomorrow", "zone": zone, "due": base + 21.0 * hour}),
                        json!({"id": 5, "label": "alarm", "zone": zone, "due": base - 30.0 * hour, "ringing": true})];
        let visible = visible_alarms(&rows, base).unwrap();
        assert_eq!(visible.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(), vec![json!(5), json!(1), json!(2)]);
        assert_eq!(visible[1]["alarm_time"], json!("7:00 AM"));
        assert_eq!(visible[2]["alarm_time"], json!("7:00 PM"));
        assert_eq!(visible[1]["text"], json!("0:00"));
        assert_eq!(visible[2]["text"], json!("9:00:00"));
        assert_eq!(visible[1]["caption"], json!("Ringing · 7:00 AM"));
        assert_eq!(visible[2]["caption"], json!("until 7:00 PM"));
        assert_eq!(visible[0]["title"], json!("Alarm"));
        assert_eq!(visible[2]["title"], json!("Evening · Alarm"));
        let midnight = base + 14.0 * hour;
        let next = visible_alarms(&rows[..3], midnight).unwrap();
        assert_eq!(next.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(), vec![json!(3)]);
        assert!(visible_alarms(&[json!({"id": 1, "zone": "No/Such_Zone", "due": base})], base).is_err());
        let keys: Vec<&String> = visible[0].keys().collect();
        assert_eq!(keys, ["id", "title", "text", "seconds", "alarm_time", "caption", "ringing", "due"]);
    }

    #[test]
    fn snapshot_socket_protocol_and_errors() {
        let dir = std::env::temp_dir().join(format!("kasumi-sock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.sock");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for reply in [r#"{"timers": [{"id": 1}]}"#, r#"{"error": "denied"}"#, r#"{"alarms": []}"#] {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                requests.push(line);
                (&stream).write_all(format!("{reply}\n").as_bytes()).unwrap();
            }
            requests
        });
        assert_eq!(read_countdowns(&path).unwrap(), vec![json!({"id": 1})]);
        assert_eq!(read_countdowns(&path).unwrap_err(), "denied");
        assert_eq!(read_alarms(&path).unwrap(), Vec::<Value>::new());
        let requests = server.join().unwrap();
        assert_eq!(requests, ["{\"operation\": \"countdowns\"}\n", "{\"operation\": \"countdowns\"}\n", "{\"operation\": \"alarms_today\"}\n"]);
        assert!(read_countdowns(&dir.join("missing.sock")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hostile_scheduler_replies_are_bounded_and_never_panic() {
        let dir = std::env::temp_dir().join(format!("kasumi-sock-hostile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.sock");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        let replies: Vec<Vec<u8>> = vec![
            "x".repeat(400_000).into_bytes(),                     // over-long, no newline
            deep.into_bytes(),                                      // recursion bomb
            b"{\"timers\": 5}\n".to_vec(),                       // wrong type
            b"\xff\xfe\n".to_vec(),                              // not UTF-8
            b"[1, 2]\n".to_vec(),                                  // not an object
        ];
        let count = replies.len();
        let server = std::thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let _ = stream.write_all(&reply);
            }
            // A peer that trickles a byte every 100 ms must not hold the worker past the deadline.
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..40 {
                if stream.write_all(b" ").is_err() { break; }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        for _ in 0..count {
            assert!(read_countdowns(&path).is_err());
        }
        let started = std::time::Instant::now();
        assert!(read_countdowns(&path).is_err());
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
        server.join().unwrap();
        // Hostile rows: huge / NaN-like / wrong-typed values are errors or clamp, never panics.
        let rows = vec![json!({"id": 1, "position": 1e300, "label": "x", "due": 1e308}),
                        json!({"id": [], "position": "1", "label": null, "due": "nan"})];
        let _ = large_countdowns(&rows, 0.0);
        let _ = compact_countdowns(&rows, 0.0);
        let alarms = vec![json!({"id": 1, "zone": "../../etc/passwd", "due": 1.0}),
                          json!({"id": 2, "zone": "UTC", "due": 1e300}),
                          json!({"id": 3, "zone": "UTC", "due": -1e300, "ringing": true})];
        let _ = visible_alarms(&alarms, 0.0);
        assert_eq!(remaining_text(f64::MAX), remaining_text(i64::MAX as f64));
        let _ = std::fs::remove_dir_all(dir);
    }
}
