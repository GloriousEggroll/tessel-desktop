//! Port of `desktop_workarea.py`: live Tessel Dock reservations — no service
//! activation, polling, or saved geometry.
//!
//! `org.tessel.Tessel Dock /org/tessel/Settings org.tessel.Settings`:
//! `GetWorkareas() -> s` (2000 ms, NO_AUTO_START) and `WorkareasChanged(s)`.

use super::icon_layout::Rect;
use gio::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

pub const BUS_NAME: &str = "org.tessel.Tessel Dock";
pub const OBJECT_PATH: &str = "/org/tessel/Settings";
pub const INTERFACE: &str = "org.tessel.Settings";

/// connector name -> (monitor geometry, workarea).
pub type Areas = BTreeMap<String, (Rect, Rect)>;

fn rect(value: Option<&Value>) -> Result<Rect, String> {
    let bad = || "Invalid monitor rectangle".to_owned();
    let items = value.and_then(Value::as_array).ok_or_else(bad)?;
    if items.len() != 4 {
        return Err(bad());
    }
    let mut out = [0i64; 4];
    for (slot, item) in out.iter_mut().zip(items) {
        // `type(value) is int` (bool and float rejected), abs <= 2**30.
        let int = match item {
            Value::Number(n) if !n.is_f64() => n.as_i64().ok_or_else(bad)?,
            _ => return Err(bad()),
        };
        if int.abs() > 1 << 30 {
            return Err(bad());
        }
        *slot = int;
    }
    Ok((out[0], out[1], out[2], out[3]))
}

/// Reject malformed/stale rectangles instead of moving icons off screen.
pub fn parse_workareas(payload: &str) -> Result<Areas, String> {
    if payload.chars().count() > 65536 {
        return Err("Invalid workarea payload".into());
    }
    let data: Value = serde_json::from_str(payload).map_err(|e| e.to_string())?;
    let version_ok = data.as_object().and_then(|d| d.get("version")).is_some_and(|v| matches!(v, Value::Number(n) if !n.is_f64() && n.as_i64() == Some(1)));
    if !version_ok {
        return Err("Unknown workarea version".into());
    }
    let monitors = match data.get("monitors") {
        Some(Value::Array(list)) if list.len() <= 128 => list,
        _ => return Err("Invalid monitor list".into()),
    };
    let mut areas = Areas::new();
    for monitor in monitors {
        let Some(monitor) = monitor.as_object() else { return Err("Invalid monitor".into()) };
        let name = match monitor.get("name") {
            Some(Value::String(name)) if !name.is_empty() && name.chars().count() <= 256 && !areas.contains_key(name) => name.clone(),
            _ => return Err("Invalid monitor name".into()),
        };
        let geometry = rect(monitor.get("geometry"))?;
        let workarea = rect(monitor.get("workarea"))?;
        let (x, y, width, height) = geometry;
        let (ax, ay, aw, ah) = workarea;
        if width <= 0 || height <= 0 || aw < 0 || ah < 0 || ax < x || ay < y || ax + aw > x + width || ay + ah > y + height {
            return Err("Workarea exceeds monitor".into());
        }
        areas.insert(name, (geometry, workarea));
    }
    Ok(areas)
}

/// `WorkareaClient.rectangle` over a set of known areas.
pub fn rectangle_in(areas: &Areas, name: Option<&str>, geometry: Rect) -> Rect {
    let mut entry = name.and_then(|n| areas.get(n)).copied();
    // Connector names may arrive from the compositor after GDK geometry.
    // Geometry fallback is unambiguous only for a single matching output.
    if entry.is_none() {
        let matches: Vec<_> = areas.values().filter(|value| value.0 == geometry).collect();
        entry = (matches.len() == 1).then(|| *matches[0]);
    }
    match entry {
        Some((known, area)) if known == geometry => area,
        _ => geometry,
    }
}

/// What the desktop asks of a workarea provider (tests inject fixtures).
pub trait WorkareaSource {
    fn rectangle(&self, name: Option<&str>, geometry: Rect) -> Rect;
    /// Areas Tessel Dock has reported: name -> (geometry, workarea).
    fn known(&self) -> Areas;
    fn close(&self) {}
}

/// Pure request/serial bookkeeping of the client (unit-testable).
#[derive(Default, Debug)]
pub struct WorkareaState {
    pub areas: Areas,
    pub serial: u64,
    pub closed: bool,
}

impl WorkareaState {
    /// Returns true when the areas changed.
    pub fn replace(&mut self, areas: Areas) -> bool {
        if areas != self.areas {
            self.areas = areas;
            return true;
        }
        false
    }

    /// `_owner_changed`: clears the areas; returns (changed, request) where
    /// request = Some((serial, owner)) when GetWorkareas must be called.
    pub fn owner_changed(&mut self, owner: Option<&str>) -> (bool, Option<(u64, String)>) {
        self.serial += 1;
        let changed = self.replace(Areas::new());
        match owner {
            Some(owner) if !self.closed && !owner.is_empty() => (changed, Some((self.serial, owner.to_owned()))),
            _ => (changed, None),
        }
    }

    /// `_received`: a GetWorkareas reply for `request`, the owner now being `owner`.
    pub fn received(&mut self, request: &(u64, String), owner: Option<&str>, reply: &glib::Variant) -> bool {
        if self.closed || request.0 != self.serial || Some(request.1.as_str()) != owner {
            return false;
        }
        self.accept(reply)
    }

    /// `_signal`: only WorkareasChanged from the current owner; a live
    /// signal wins over an older GetWorkareas reply still in flight.
    pub fn signal(&mut self, sender: Option<&str>, owner: Option<&str>, signal: &str, parameters: &glib::Variant) -> bool {
        if self.closed || signal != "WorkareasChanged" || sender.is_none() || sender != owner {
            return false;
        }
        self.serial += 1;
        self.accept(parameters)
    }

    pub fn accept(&mut self, parameters: &glib::Variant) -> bool {
        if parameters.type_().as_str() != "(s)" {
            return false;
        }
        let payload: String = parameters.child_value(0).get().unwrap_or_default();
        match parse_workareas(&payload) {
            Ok(areas) => self.replace(areas),
            Err(_) => false,
        }
    }
}

struct Inner {
    state: RefCell<WorkareaState>,
    on_changed: Box<dyn Fn()>,
    proxy: RefCell<Option<gio::DBusProxy>>,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    cancellable: gio::Cancellable,
    started: Cell<bool>,
    connection: RefCell<Option<gio::DBusConnection>>,
}

/// `desktop_workarea.WorkareaClient`.
#[derive(Clone)]
pub struct WorkareaClient(Rc<Inner>);

impl WorkareaClient {
    pub fn new(on_changed: impl Fn() + 'static) -> Self {
        Self(Rc::new(Inner {
            state: RefCell::default(),
            on_changed: Box::new(on_changed),
            proxy: RefCell::default(),
            handlers: RefCell::default(),
            cancellable: gio::Cancellable::new(),
            started: Cell::new(false),
            connection: RefCell::default(),
        }))
    }

    /// Same client on an explicit connection (private-bus tests).
    pub fn with_connection(connection: &gio::DBusConnection, on_changed: impl Fn() + 'static) -> Self {
        let client = Self::new(on_changed);
        client.0.connection.replace(Some(connection.clone()));
        client
    }

    pub fn start(&self) {
        if self.0.started.get() || self.0.state.borrow().closed {
            return;
        }
        self.0.started.set(true);
        let flags = gio::DBusProxyFlags::DO_NOT_AUTO_START | gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES;
        let weak = Rc::downgrade(&self.0);
        let ready = move |result: Result<gio::DBusProxy, glib::Error>| {
            if let (Some(inner), Ok(proxy)) = (weak.upgrade(), result) {
                Self::ready(&inner, proxy);
            }
        };
        let connection = self.0.connection.borrow().clone();
        match connection {
            Some(connection) => gio::DBusProxy::new(&connection, flags, None, Some(BUS_NAME), OBJECT_PATH, INTERFACE, Some(&self.0.cancellable), ready),
            None => gio::DBusProxy::for_bus(gio::BusType::Session, flags, None, BUS_NAME, OBJECT_PATH, INTERFACE, Some(&self.0.cancellable), ready),
        }
    }

    fn ready(inner: &Rc<Inner>, proxy: gio::DBusProxy) {
        if inner.state.borrow().closed {
            return;
        }
        let weak = Rc::downgrade(inner);
        let owner_handler = proxy.connect_notify_local(Some("g-name-owner"), move |_, _| {
            if let Some(inner) = weak.upgrade() {
                Self::owner_changed(&inner);
            }
        });
        let weak = Rc::downgrade(inner);
        let signal_handler = proxy.connect_local("g-signal", false, move |args| {
            let inner = weak.upgrade()?;
            let proxy = args[0].get::<gio::DBusProxy>().ok()?;
            let sender = args[1].get::<Option<String>>().ok().flatten();
            let signal = args[2].get::<String>().ok()?;
            let parameters = args[3].get::<glib::Variant>().ok()?;
            let owner = proxy.name_owner().map(|o| o.to_string());
            let changed = inner.state.borrow_mut().signal(sender.as_deref(), owner.as_deref(), &signal, &parameters);
            if changed {
                (inner.on_changed)();
            }
            None
        });
        inner.handlers.replace(vec![owner_handler, signal_handler]);
        inner.proxy.replace(Some(proxy));
        Self::owner_changed(inner);
    }

    fn owner_changed(inner: &Rc<Inner>) {
        let Some(proxy) = inner.proxy.borrow().clone() else { return };
        let owner = proxy.name_owner().map(|o| o.to_string());
        let (changed, request) = inner.state.borrow_mut().owner_changed(owner.as_deref());
        if changed {
            (inner.on_changed)();
        }
        let Some(request) = request else { return };
        let weak: Weak<Inner> = Rc::downgrade(inner);
        proxy.call("GetWorkareas", None, gio::DBusCallFlags::NO_AUTO_START, 2000, Some(&inner.cancellable), move |result| {
            let (Some(inner), Ok(reply)) = (weak.upgrade(), result) else { return };
            let owner = inner.proxy.borrow().as_ref().and_then(|p| p.name_owner()).map(|o| o.to_string());
            let changed = inner.state.borrow_mut().received(&request, owner.as_deref(), &reply);
            if changed {
                (inner.on_changed)();
            }
        });
    }

    pub fn serial(&self) -> u64 {
        self.0.state.borrow().serial
    }

    pub fn is_closed(&self) -> bool {
        self.0.state.borrow().closed
    }

    pub fn close_client(&self) {
        let inner = &self.0;
        {
            let mut state = inner.state.borrow_mut();
            if state.closed {
                return;
            }
            state.closed = true;
            state.serial += 1;
            state.areas.clear();
        }
        inner.cancellable.cancel();
        if let Some(proxy) = inner.proxy.take() {
            for handler in inner.handlers.take() {
                proxy.disconnect(handler);
            }
        }
        inner.handlers.borrow_mut().clear();
    }
}

impl WorkareaSource for WorkareaClient {
    fn rectangle(&self, name: Option<&str>, geometry: Rect) -> Rect {
        rectangle_in(&self.0.state.borrow().areas, name, geometry)
    }
    fn known(&self) -> Areas {
        self.0.state.borrow().areas.clone()
    }
    fn close(&self) {
        self.close_client();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(monitors: Value) -> String {
        json!({"version": 1, "monitors": monitors}).to_string()
    }

    #[test]
    fn parses_valid_payload() {
        let areas = parse_workareas(&payload(json!([
            {"name": "HEADLESS-1", "geometry": [0, 0, 1920, 1080], "workarea": [0, 40, 1920, 1040]},
            {"name": "HEADLESS-2", "geometry": [1920, 0, 1280, 720], "workarea": [1920, 0, 1280, 720]}]))).unwrap();
        assert_eq!(areas["HEADLESS-1"], ((0, 0, 1920, 1080), (0, 40, 1920, 1040)));
        assert_eq!(areas.len(), 2);
    }

    #[test]
    fn rejects_malformed_payloads() {
        let good = json!({"name": "A", "geometry": [0, 0, 100, 100], "workarea": [0, 0, 100, 100]});
        let cases = vec![
            "not json".to_owned(),
            json!([]).to_string(),
            json!({"version": 2, "monitors": []}).to_string(),
            json!({"version": true, "monitors": []}).to_string(),
            json!({"version": 1.0, "monitors": []}).to_string(),
            json!({"version": 1, "monitors": {}}).to_string(),
            json!({"version": 1}).to_string(),
            payload(json!(vec![good.clone(); 129])),
            payload(json!([good.clone(), good.clone()])),
            payload(json!(["x"])),
            payload(json!([{"name": "", "geometry": [0, 0, 1, 1], "workarea": [0, 0, 1, 1]}])),
            payload(json!([{"name": "x".repeat(257), "geometry": [0, 0, 1, 1], "workarea": [0, 0, 1, 1]}])),
            payload(json!([{"name": 5, "geometry": [0, 0, 1, 1], "workarea": [0, 0, 1, 1]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, 1.0, 1], "workarea": [0, 0, 1, 1]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, true, 1], "workarea": [0, 0, 1, 1]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, 1], "workarea": [0, 0, 1, 1]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, (1i64 << 30) + 1, 1], "workarea": [0, 0, 1, 1]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, 0, 1], "workarea": [0, 0, 0, 1]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, 10, 10], "workarea": [0, 0, -1, 10]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, 10, 10], "workarea": [-1, 0, 5, 5]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, 10, 10], "workarea": [5, 5, 6, 5]}])),
            payload(json!([{"name": "A", "geometry": [0, 0, 10, 10]}])),
            format!("\"{}\"", "x".repeat(65537)),
        ];
        for case in cases {
            assert!(parse_workareas(&case).is_err(), "{}", &case[..case.len().min(120)]);
        }
        // 2**30 itself is allowed; 64 KiB counts characters.
        assert!(parse_workareas(&payload(json!([{"name": "A", "geometry": [0, 0, 1 << 30, 1], "workarea": [0, 0, 1, 1]}]))).is_ok());
    }

    #[test]
    fn rectangle_matches_by_name_then_unique_geometry() {
        let mut areas = Areas::new();
        areas.insert("A".into(), ((0, 0, 100, 100), (0, 10, 100, 90)));
        assert_eq!(rectangle_in(&areas, Some("A"), (0, 0, 100, 100)), (0, 10, 100, 90));
        assert_eq!(rectangle_in(&areas, None, (0, 0, 100, 100)), (0, 10, 100, 90));
        assert_eq!(rectangle_in(&areas, Some("A"), (0, 0, 200, 100)), (0, 0, 200, 100), "stale geometry");
        areas.insert("B".into(), ((0, 0, 100, 100), (0, 0, 90, 100)));
        assert_eq!(rectangle_in(&areas, Some("C"), (0, 0, 100, 100)), (0, 0, 100, 100), "ambiguous geometry");
    }

    fn signal_payload() -> glib::Variant {
        (payload(json!([{"name": "A", "geometry": [0, 0, 100, 100], "workarea": [0, 10, 100, 90]}])),).to_variant()
    }

    #[test]
    fn serial_owner_and_sender_rules() {
        let mut state = WorkareaState::default();
        let (changed, request) = state.owner_changed(Some(":1.5"));
        assert!(!changed);
        let request = request.unwrap();
        // A live signal beats the in-flight reply.
        assert!(!state.signal(Some(":1.9"), Some(":1.5"), "WorkareasChanged", &signal_payload()), "foreign sender");
        assert!(!state.signal(Some(":1.5"), Some(":1.5"), "Other", &signal_payload()));
        assert!(state.signal(Some(":1.5"), Some(":1.5"), "WorkareasChanged", &signal_payload()));
        let empty = (payload(json!([])),).to_variant();
        assert!(!state.received(&request, Some(":1.5"), &empty), "stale reply must be ignored");
        assert_eq!(state.areas.len(), 1);
        // Owner loss clears; a reply for an old owner is ignored.
        let (changed, request2) = state.owner_changed(None);
        assert!(changed && request2.is_none() && state.areas.is_empty());
        let (_, request3) = state.owner_changed(Some(":1.7"));
        let request3 = request3.unwrap();
        assert!(!state.received(&request3, Some(":1.8"), &signal_payload()), "owner changed meanwhile");
        assert!(state.received(&request3, Some(":1.7"), &signal_payload()));
        // Malformed or wrongly typed payloads are ignored.
        assert!(!state.accept(&("{bad".to_owned(),).to_variant()));
        assert!(!state.accept(&(5u32,).to_variant()));
        assert_eq!(state.areas.len(), 1);
        state.closed = true;
        assert!(!state.signal(Some(":1.7"), Some(":1.7"), "WorkareasChanged", &(payload(json!([])),).to_variant()));
    }
}
