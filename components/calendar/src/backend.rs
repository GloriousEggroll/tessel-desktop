//! Asynchronous system date settings through timedated and its Polkit policy
//! (date_backend.py). Nothing is written by `start()`/`refresh()`; the only
//! mutating calls are explicit `set_*` requests.

use crate::civil::{Checked, Date, Naive};
use crate::tz::{self, Zone, ZoneRef};
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

pub const BUS_NAME: &str = "org.freedesktop.timedate1";
pub const OBJECT_PATH: &str = "/org/freedesktop/timedate1";
pub const INTERFACE: &str = BUS_NAME;
pub const GET_ALL: &str = "org.freedesktop.DBus.Properties.GetAll";
/// Linux's TIME_SETTOD_SEC_MAX leaves 30 years of uptime before the signed
/// nanosecond clock wraps. The first rejected second is in 2232, not 2262.
pub const MAX_TIME_USEC: i64 = ((i64::MAX / 1_000_000_000 - 30 * 365 * 86400) * 1_000_000) - 1;
pub const WRITE_TIMEOUT: i32 = i32::MAX;
pub const READ_TIMEOUT: i32 = 5000;

/// An aware `datetime` (wall time, fold, zone).
#[derive(Clone, Debug)]
pub struct ZonedTime {
    pub wall: Naive,
    pub fold: u8,
    pub zone: ZoneRef,
}

impl PartialEq for ZonedTime {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp_micros() == other.timestamp_micros()
    }
}

impl ZonedTime {
    pub fn new(wall: Naive, zone: ZoneRef) -> Self {
        Self { wall, fold: 0, zone }
    }

    pub fn from_epoch_micros(micros: i128, zone: &ZoneRef) -> Checked<Self> {
        let (wall, fold) = zone.fromutc(Naive::from_epoch_micros(micros)?)?;
        Ok(Self { wall, fold, zone: zone.clone() })
    }

    pub fn now_in(zone: &ZoneRef) -> Self {
        let micros = glib::real_time() as i128;
        Self::from_epoch_micros(micros, zone).unwrap_or_else(|_| Self::new(Naive::MIN, zone.clone()))
    }

    pub fn date(&self) -> Date { self.wall.date() }
    pub fn hour(&self) -> i64 { self.wall.hour() }
    pub fn minute(&self) -> i64 { self.wall.minute() }
    pub fn second(&self) -> i64 { self.wall.second() }
    pub fn utcoffset(&self) -> i64 { self.zone.utcoffset(self.wall, self.fold) }
    pub fn tzname(&self) -> String { self.zone.tzname(self.wall, self.fold) }

    pub fn timestamp_micros(&self) -> i128 {
        self.wall.epoch_micros() - self.utcoffset() as i128 * 1_000_000
    }

    pub fn astimezone(&self, zone: &ZoneRef) -> Checked<Self> {
        Self::from_epoch_micros(self.timestamp_micros(), zone)
    }

    /// `replace(year, month, day)` keeping time, fold and zone.
    pub fn with_date(&self, date: Date) -> Self {
        Self { wall: Naive::combine(date, self.wall.time_micros()), fold: self.fold, zone: self.zone.clone() }
    }

    pub fn add_micros(&self, micros: i128) -> Checked<Self> {
        Ok(Self { wall: Naive::from_micros(self.wall.0 as i128 + micros)?, fold: 0, zone: self.zone.clone() })
    }

    /// A `glib::DateTime` with the same wall time and offset (for locale-aware strftime).
    pub fn to_glib(&self) -> Option<glib::DateTime> {
        let zone = glib::TimeZone::from_offset(self.utcoffset() as i32);
        let (y, m, d) = self.date().parts();
        glib::DateTime::new(&zone, y as i32, m as i32, d as i32, self.hour() as i32, self.minute() as i32,
                            self.second() as f64 + self.wall.microsecond() as f64 / 1e6).ok()
    }
}

/// A naive `datetime.time`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub micro: i64,
}

impl Clock {
    pub fn new(hour: i64, minute: i64, second: i64) -> Option<Clock> {
        ((0..24).contains(&hour) && (0..60).contains(&minute) && (0..60).contains(&second))
            .then_some(Clock { hour, minute, second, micro: 0 })
    }

    fn micros(self) -> i64 {
        ((self.hour * 60 + self.minute) * 60 + self.second) * 1_000_000 + self.micro
    }
}

/// Replace the local calendar date, respecting that date's UTC offset.
///
/// A nonexistent local time is rejected instead of silently moving it across
/// a DST transition. For a repeated time the current fold is retained.
pub fn date_timestamp(selected: Date, current: &ZonedTime) -> Result<i64, String> {
    if !(1970..=9999).contains(&selected.year()) {
        return Err("Choose a date in 1970 or later.".into());
    }
    let wall = Naive::combine(selected, current.wall.time_micros());
    let zone = &current.zone;
    let overflow = |e: crate::civil::Overflow| e.to_string();
    let utc = if zone.is_utc() { wall } else { zone.to_utc(wall, current.fold).map_err(overflow)? };
    let (round_trip, _) = zone.fromutc(utc).map_err(overflow)?;
    if round_trip != wall {
        return Err("That local time does not exist on the selected date because of a daylight-saving change. Choose another time.".into());
    }
    let usec = utc.epoch_micros();
    if !(0 < usec && usec <= MAX_TIME_USEC as i128) {
        return Err("That date is outside the system clock's supported range.".into());
    }
    Ok(usec as i64)
}

/// Use an exact entered wall time in the current system timezone.
pub fn datetime_timestamp(selected: Date, time: Clock, current: &ZonedTime) -> Result<i64, String> {
    let wall = Naive::combine(current.date(), time.micros());
    date_timestamp(selected, &ZonedTime { wall, fold: current.fold, zone: current.zone.clone() })
}

/// `[A-Za-z0-9_+-]+(?:/[A-Za-z0-9_+-]+)*\Z`
pub fn valid_zone_name(name: &str) -> bool {
    !name.is_empty() && name.split('/').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b)))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DateState {
    pub ready: bool,
    pub busy: bool,
    pub error: String,
    pub timezone: String,
    pub timezones: Vec<String>,
    pub timezones_ready: bool,
    pub ntp: bool,
    pub can_ntp: bool,
    pub synchronized: bool,
}

/// What the windows need from a date backend (real or fake).
pub trait DateSystem {
    fn state(&self) -> DateState;
    fn now(&self) -> ZonedTime;
    fn start(&self);
    fn close(&self);
    fn set_ntp(&self, enabled: bool) -> bool;
    fn set_date(&self, selected: Date) -> bool;
    fn set_datetime(&self, selected: Date, time: Clock) -> bool;
    fn set_timezone(&self, name: &str) -> bool;
}

// ------------------------------------------------------------------- D-Bus

#[derive(Clone, Debug, PartialEq)]
pub struct BusError {
    pub remote: Option<String>,
    pub message: String,
    pub cancelled: bool,
}

impl BusError {
    pub fn from_glib(error: &glib::Error) -> Self {
        let remote = gio::DBusError::remote_error(error).map(|n| n.to_string());
        let cancelled = error.matches(gio::IOErrorEnum::Cancelled);
        let mut stripped = error.clone();
        gio::DBusError::strip_remote_error(&mut stripped);
        Self { remote, message: stripped.message().to_owned(), cancelled }
    }

    pub fn remote(name: &str, message: &str) -> Self {
        Self { remote: Some(name.into()), message: message.into(), cancelled: false }
    }

    pub fn local(message: &str) -> Self {
        Self { remote: None, message: message.into(), cancelled: false }
    }
}

/// `DateBackend._error_text`.
pub fn error_text(error: &BusError) -> String {
    let name = error.remote.as_deref().unwrap_or("");
    if ["AccessDenied", "NotAuthorized", "InteractiveAuthorizationRequired", "Cancelled", "Canceled"].iter().any(|w| name.contains(w)) {
        return "Authorization was canceled or denied. No change was applied.".into();
    }
    if name.contains("AutomaticTimeSyncEnabled") {
        return "Turn off automatic date and time before changing the date.".into();
    }
    if error.cancelled {
        return "The request was canceled.".into();
    }
    error.message.clone()
}

pub type Reply = Result<Variant, BusError>;

/// The parts of `Gio.DBusProxy` the backend uses (fakes in tests).
pub trait Proxy {
    fn name_owner(&self) -> Option<String>;
    fn cached_property(&self, name: &str) -> Option<Variant>;
    fn set_cached_property(&self, name: &str, value: &Variant);
    fn call(&self, method: &str, params: Option<Variant>, flags: gio::DBusCallFlags, timeout: i32,
            cancellable: Option<&gio::Cancellable>, done: Box<dyn FnOnce(Reply)>);
    /// `g-properties-changed` and `notify::g-name-owner`.
    fn connect(&self, properties_changed: Box<dyn Fn()>, owner_changed: Box<dyn Fn()>);
    fn disconnect(&self);
}

pub struct GioProxy {
    proxy: gio::DBusProxy,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl GioProxy {
    pub fn new(proxy: gio::DBusProxy) -> Self {
        Self { proxy, handlers: RefCell::new(Vec::new()) }
    }
}

impl Proxy for GioProxy {
    fn name_owner(&self) -> Option<String> {
        self.proxy.name_owner().map(|n| n.to_string())
    }
    fn cached_property(&self, name: &str) -> Option<Variant> {
        self.proxy.cached_property(name)
    }
    fn set_cached_property(&self, name: &str, value: &Variant) {
        self.proxy.set_cached_property(name, Some(value));
    }
    fn call(&self, method: &str, params: Option<Variant>, flags: gio::DBusCallFlags, timeout: i32,
            cancellable: Option<&gio::Cancellable>, done: Box<dyn FnOnce(Reply)>) {
        self.proxy.call(method, params.as_ref(), flags, timeout, cancellable, move |result| {
            done(result.map_err(|e| BusError::from_glib(&e)))
        });
    }
    fn connect(&self, properties_changed: Box<dyn Fn()>, owner_changed: Box<dyn Fn()>) {
        let a = self.proxy.connect_local("g-properties-changed", false, move |_| {
            properties_changed();
            None
        });
        let b = self.proxy.connect_notify_local(Some("g-name-owner"), move |_, _| owner_changed());
        self.handlers.borrow_mut().extend([a, b]);
    }
    fn disconnect(&self) {
        for handler in self.handlers.borrow_mut().drain(..) {
            self.proxy.disconnect(handler);
        }
    }
}

pub type ProxyResult = Result<Rc<dyn Proxy>, BusError>;
/// `Gio.DBusProxy.new_for_bus(SYSTEM, GET_INVALIDATED_PROPERTIES, …)`.
pub type Connector = Rc<dyn Fn(&gio::Cancellable, Box<dyn FnOnce(ProxyResult)>)>;
/// `datetime.now(tz)`.
pub type ClockFn = Rc<dyn Fn(&ZoneRef) -> ZonedTime>;
/// Test hook replacing `_later(name, ms, callback)`.
pub type LaterHook = Rc<dyn Fn(&str, u32, Rc<dyn Fn()>)>;

pub fn system_connector() -> Connector {
    Rc::new(|cancellable: &gio::Cancellable, done: Box<dyn FnOnce(ProxyResult)>| {
        gio::DBusProxy::for_bus(gio::BusType::System, gio::DBusProxyFlags::GET_INVALIDATED_PROPERTIES, None,
                                    BUS_NAME, OBJECT_PATH, INTERFACE, Some(cancellable), move |result| {
            done(result.map(|p| Rc::new(GioProxy::new(p)) as Rc<dyn Proxy>).map_err(|e| BusError::from_glib(&e)))
        });
    })
}

pub fn system_clock() -> ClockFn {
    Rc::new(ZonedTime::now_in)
}

struct Inner {
    state: RefCell<DateState>,
    /// Kept stable for clients holding the list (Python keeps the list object).
    timezones: Rc<RefCell<Vec<String>>>,
    on_changed: Box<dyn Fn()>,
    on_error: Box<dyn Fn(&str)>,
    proxy: RefCell<Option<Rc<dyn Proxy>>>,
    closed: Cell<bool>,
    started: Cell<bool>,
    generation: Cell<u64>,
    operation: Cell<u64>,
    sources: RefCell<HashMap<String, glib::SourceId>>,
    read_cancel: RefCell<gio::Cancellable>,
    refresh_pending: Cell<bool>,
    timezones_pending: Cell<bool>,
    clock: RefCell<ClockFn>,
    connector: Connector,
    later_hook: RefCell<Option<LaterHook>>,
}

#[derive(Clone)]
pub struct DateBackend(Rc<Inner>);

type Properties = HashMap<String, Variant>;

fn is_string(value: &Variant) -> bool {
    value.str().is_some()
}

/// Python truthiness of an unpacked variant.
fn truthy(value: &Variant) -> bool {
    if let Some(b) = value.get::<bool>() {
        return b;
    }
    if let Some(s) = value.str() {
        return !s.is_empty();
    }
    if value.is_container() {
        return value.n_children() > 0;
    }
    value.get::<i64>().map(|v| v != 0).or_else(|| value.get::<i32>().map(|v| v != 0)).or_else(|| value.get::<u32>().map(|v| v != 0))
        .or_else(|| value.get::<u64>().map(|v| v != 0)).or_else(|| value.get::<f64>().map(|v| v != 0.0)).unwrap_or(true)
}

fn properties_of(reply: &Variant) -> Option<Properties> {
    let dict = reply.try_child_value(0)?;
    let mut properties = Properties::new();
    for entry in dict.iter() {
        let key = entry.try_child_value(0)?.str()?.to_owned();
        let value = entry.try_child_value(1)?.as_variant()?;
        properties.insert(key, value);
    }
    Some(properties)
}

impl DateBackend {
    pub fn new(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static) -> Self {
        Self::with(on_changed, on_error, system_connector(), system_clock())
    }

    pub fn with(on_changed: impl Fn() + 'static, on_error: impl Fn(&str) + 'static, connector: Connector, clock: ClockFn) -> Self {
        // Unknown state must never permit a manual date write.
        let state = DateState { ntp: true, ..DateState::default() };
        Self(Rc::new(Inner { state: RefCell::new(state), timezones: Rc::default(), on_changed: Box::new(on_changed),
                             on_error: Box::new(on_error), proxy: RefCell::new(None), closed: Cell::new(false),
                             started: Cell::new(false), generation: Cell::new(0), operation: Cell::new(0),
                             sources: RefCell::default(), read_cancel: RefCell::new(gio::Cancellable::new()),
                             refresh_pending: Cell::new(false), timezones_pending: Cell::new(false),
                             clock: RefCell::new(clock), connector, later_hook: RefCell::new(None) }))
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn up(weak: &Weak<Inner>) -> Option<DateBackend> {
        weak.upgrade().map(DateBackend)
    }

    // ---- test hooks
    pub fn set_clock(&self, clock: ClockFn) {
        self.0.clock.replace(clock);
    }
    pub fn set_later_hook(&self, hook: LaterHook) {
        self.0.later_hook.replace(Some(hook));
    }
    pub fn set_proxy(&self, proxy: Rc<dyn Proxy>) {
        self.0.proxy.replace(Some(proxy));
    }
    pub fn timezones_handle(&self) -> Rc<RefCell<Vec<String>>> {
        self.0.timezones.clone()
    }
    pub fn update_state(&self, change: impl FnOnce(&mut DateState)) {
        change(&mut self.0.state.borrow_mut());
        let zones = self.0.state.borrow().timezones.clone();
        *self.0.timezones.borrow_mut() = zones;
    }
    pub fn read_cached(&self) {
        let Some(proxy) = self.proxy() else { return };
        let mut properties = Properties::new();
        for name in ["Timezone", "NTP", "CanNTP", "NTPSynchronized"] {
            if let Some(value) = proxy.cached_property(name) {
                properties.insert(name.into(), value);
            }
        }
        self.apply_properties(&properties);
    }
    pub fn properties_changed(&self, proxy: &Rc<dyn Proxy>) {
        if !self.0.closed.get() && self.is_current(proxy) {
            self.read_cached();
        }
    }
    pub fn owner_changed(&self, proxy: &Rc<dyn Proxy>) {
        if self.0.closed.get() || !self.is_current(proxy) {
            return;
        }
        if proxy.name_owner().is_none() {
            self.unavailable("The system date service is unavailable. Reconnecting…");
        } else {
            self.refresh();
        }
    }

    fn is_current(&self, proxy: &Rc<dyn Proxy>) -> bool {
        self.proxy().is_some_and(|p| Rc::ptr_eq(&p, proxy))
    }

    fn proxy(&self) -> Option<Rc<dyn Proxy>> {
        self.0.proxy.borrow().clone()
    }

    fn set(&self, change: impl FnOnce(&mut DateState)) {
        change(&mut self.0.state.borrow_mut());
    }

    fn later(&self, name: &str, milliseconds: u32, callback: Rc<dyn Fn()>) {
        if let Some(hook) = self.0.later_hook.borrow().clone() {
            hook(name, milliseconds, callback);
            return;
        }
        if self.0.sources.borrow().contains_key(name) || self.0.closed.get() {
            return;
        }
        let weak = self.weak();
        let key = name.to_owned();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(milliseconds as u64), move || {
            if let Some(this) = Self::up(&weak) {
                this.0.sources.borrow_mut().remove(&key);
                if !this.0.closed.get() {
                    callback();
                }
            }
        });
        self.0.sources.borrow_mut().insert(name.into(), id);
    }

    fn later_refresh(&self) {
        let weak = self.weak();
        self.later("refresh", 20000, Rc::new(move || if let Some(this) = Self::up(&weak) { this.refresh() }));
    }

    fn notify(&self) {
        if !self.0.closed.get() {
            (self.0.on_changed)();
        }
    }

    fn fail(&self, message: &str) -> bool {
        self.set(|s| s.error = message.into());
        if !self.0.closed.get() {
            (self.0.on_error)(message);
            self.notify();
        }
        false
    }

    fn detach_proxy(&self) {
        if let Some(proxy) = self.0.proxy.take() {
            proxy.disconnect();
        }
    }

    fn renew_cancel(&self) {
        self.0.read_cancel.borrow().cancel();
        self.0.read_cancel.replace(gio::Cancellable::new());
    }

    fn cancellable(&self) -> gio::Cancellable {
        self.0.read_cancel.borrow().clone()
    }

    fn connect(&self) {
        if self.0.closed.get() {
            return;
        }
        let generation = self.0.generation.get() + 1;
        self.0.generation.set(generation);
        self.renew_cancel();
        self.0.refresh_pending.set(false);
        self.0.timezones_pending.set(false);
        self.detach_proxy();
        let weak = self.weak();
        (self.0.connector)(&self.cancellable(), Box::new(move |result| {
            let Some(this) = Self::up(&weak) else { return };
            this.connected(result, generation);
        }));
    }

    fn connected(&self, result: ProxyResult, generation: u64) {
        let proxy = match result {
            Ok(proxy) => proxy,
            Err(error) => {
                if !self.0.closed.get() && generation == self.0.generation.get() {
                    self.unavailable(&error_text(&error));
                }
                return;
            }
        };
        if self.0.closed.get() || generation != self.0.generation.get() {
            return;
        }
        self.0.proxy.replace(Some(proxy.clone()));
        let (w1, w2) = (self.weak(), self.weak());
        let (p1, p2) = (Rc::downgrade(&proxy), Rc::downgrade(&proxy));
        proxy.connect(
            Box::new(move || if let (Some(this), Some(p)) = (Self::up(&w1), p1.upgrade()) { this.properties_changed(&p) }),
            Box::new(move || if let (Some(this), Some(p)) = (Self::up(&w2), p2.upgrade()) { this.owner_changed(&p) }));
        if proxy.name_owner().is_none() {
            self.unavailable("The system date service is unavailable.");
            return;
        }
        self.read_cached();
        self.load_timezones();
        self.later_refresh();
    }

    pub fn load_timezones(&self) {
        let Some(proxy) = self.proxy() else { return };
        if self.0.closed.get() || self.0.timezones_pending.get() {
            return;
        }
        self.0.timezones_pending.set(true);
        let generation = self.0.generation.get();
        let weak = self.weak();
        proxy.call("ListTimezones", None, gio::DBusCallFlags::NONE, READ_TIMEOUT, Some(&self.cancellable()), Box::new(move |reply| {
            let Some(this) = Self::up(&weak) else { return };
            let names = reply.map_err(|e| error_text(&e)).and_then(|value| {
                let names: Option<Vec<String>> = value.try_child_value(0).and_then(|list| list.get::<Vec<String>>());
                match names {
                    Some(names) if !names.is_empty() && names.iter().all(|n| valid_zone_name(n)) => Ok(names),
                    _ => Err("The system service returned an invalid time zone list.".to_owned()),
                }
            });
            let names = match names {
                Ok(names) => names,
                Err(message) => {
                    if !this.0.closed.get() && generation == this.0.generation.get() {
                        this.0.timezones_pending.set(false);
                        this.fail(&format!("Unable to load time zones: {message}"));
                    }
                    return;
                }
            };
            if this.0.closed.get() || generation != this.0.generation.get() {
                return;
            }
            this.0.timezones_pending.set(false);
            let mut zones = names;
            zones.sort();
            zones.dedup();
            let mut changed = !this.0.state.borrow().timezones_ready || *this.0.timezones.borrow() != zones;
            *this.0.timezones.borrow_mut() = zones.clone();
            this.set(|s| {
                s.timezones = zones;
                s.timezones_ready = true;
                if s.error.starts_with("Unable to load time zones:") {
                    s.error.clear();
                    changed = true;
                }
            });
            if changed {
                this.notify();
            }
        }));
    }

    fn unavailable(&self, message: &str) {
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.read_cancel.borrow().cancel();
        self.0.refresh_pending.set(false);
        self.0.timezones_pending.set(false);
        self.set(|s| {
            s.ready = false;
            s.busy = false;
            s.ntp = true;
            s.can_ntp = false;
            s.synchronized = false;
        });
        self.detach_proxy();
        self.fail(message);
        let weak = self.weak();
        self.later("reconnect", 2500, Rc::new(move || if let Some(this) = Self::up(&weak) { this.connect() }));
    }

    fn apply_properties(&self, properties: &Properties) -> bool {
        let ntp = properties.get("NTP").and_then(|v| v.get::<bool>());
        let zone = properties.get("Timezone").filter(|v| is_string(v)).and_then(|v| v.str().map(str::to_owned));
        let (Some(ntp), Some(zone)) = (ntp, zone) else {
            self.set(|s| {
                s.ready = false;
                s.ntp = true;
            });
            self.fail("The system date service returned incomplete settings.");
            return false;
        };
        let can_ntp = properties.get("CanNTP").is_some_and(truthy);
        let synchronized = properties.get("NTPSynchronized").is_some_and(truthy);
        self.set(|s| {
            let was_ready = s.ready;
            s.timezone = zone;
            s.ntp = ntp;
            s.can_ntp = can_ntp;
            s.synchronized = synchronized;
            s.ready = true;
            if !was_ready {
                s.error.clear();
            }
        });
        self.notify();
        true
    }

    fn cache_properties(&self, properties: &Properties) {
        let Some(proxy) = self.proxy() else { return };
        for (name, kind) in [("Timezone", "s"), ("NTP", "b"), ("CanNTP", "b"), ("NTPSynchronized", "b")] {
            if let Some(value) = properties.get(name).filter(|v| v.type_().as_str() == kind) {
                proxy.set_cached_property(name, value);
            }
        }
    }

    /// Refresh properties which timedated does not emit change signals for.
    pub fn refresh(&self) {
        let proxy = self.proxy();
        if self.0.closed.get() || proxy.is_none() || self.0.refresh_pending.get() || self.0.state.borrow().busy {
            self.later_refresh();
            return;
        }
        let proxy = proxy.unwrap();
        self.0.refresh_pending.set(true);
        let (generation, operation) = (self.0.generation.get(), self.0.operation.get());
        let weak = self.weak();
        proxy.call(GET_ALL, Some((INTERFACE,).to_variant()), gio::DBusCallFlags::NONE, READ_TIMEOUT, Some(&self.cancellable()),
                   Box::new(move |reply| {
            let Some(this) = Self::up(&weak) else { return };
            let values = match reply {
                Ok(value) => properties_of(&value).unwrap_or_default(),
                Err(error) => {
                    if !this.0.closed.get() && generation == this.0.generation.get() {
                        this.unavailable(&error_text(&error));
                    }
                    return;
                }
            };
            if this.0.closed.get() || generation != this.0.generation.get() {
                return;
            }
            this.0.refresh_pending.set(false);
            if operation != this.0.operation.get() {
                this.refresh();
                return;
            }
            this.cache_properties(&values);
            this.apply_properties(&values);
            if !this.0.state.borrow().timezones_ready {
                this.load_timezones();
            }
            this.later_refresh();
        }));
    }

    fn can_write(&self) -> bool {
        let (ready, busy) = { let s = self.0.state.borrow(); (s.ready, s.busy) };
        if self.0.closed.get() || !ready || self.proxy().is_none() {
            return self.fail("The system date service is unavailable.");
        }
        if busy {
            return self.fail("Wait for the current date settings request to finish.");
        }
        true
    }

    fn manual_write(&self, request: Rc<dyn Fn(&DateBackend) -> Result<(&'static str, Variant), String>>) -> bool {
        if !self.can_write() {
            return false;
        }
        if self.0.state.borrow().ntp {
            return self.fail("Turn off automatic date and time before changing these settings.");
        }
        if let Err(message) = request(self) {
            return self.fail(&message);
        }
        self.set(|s| {
            s.busy = true;
            s.error.clear();
        });
        self.0.operation.set(self.0.operation.get() + 1);
        let token = (self.0.generation.get(), self.0.operation.get());
        self.notify();
        // Read immediately before applying, so another settings application's
        // timezone/NTP change cannot be hidden by our cached properties.
        let Some(proxy) = self.proxy() else { return false };
        let weak = self.weak();
        proxy.call(GET_ALL, Some((INTERFACE,).to_variant()), gio::DBusCallFlags::NONE, READ_TIMEOUT, Some(&self.cancellable()),
                   Box::new(move |reply| {
            let Some(this) = Self::up(&weak) else { return };
            let values = match reply {
                Ok(value) => properties_of(&value).unwrap_or_default(),
                Err(error) => {
                    if this.current(token) {
                        this.set(|s| s.busy = false);
                        this.fail(&error_text(&error));
                    }
                    return;
                }
            };
            if !this.current(token) {
                return;
            }
            this.cache_properties(&values);
            if !this.apply_properties(&values) {
                this.set(|s| s.busy = false);
                this.notify();
                return;
            }
            if this.0.state.borrow().ntp {
                this.set(|s| s.busy = false);
                this.fail("Automatic date and time was enabled. Turn it off before changing these settings.");
                return;
            }
            match request(&this) {
                Ok((method, parameters)) => {
                    this.write(method, parameters, Some(token));
                }
                Err(message) => {
                    this.set(|s| s.busy = false);
                    this.fail(&message);
                }
            }
        }));
        true
    }

    fn current(&self, token: (u64, u64)) -> bool {
        !self.0.closed.get() && token == (self.0.generation.get(), self.0.operation.get())
    }

    fn write(&self, method: &str, parameters: Variant, token: Option<(u64, u64)>) -> bool {
        let token = token.unwrap_or_else(|| {
            self.0.operation.set(self.0.operation.get() + 1);
            (self.0.generation.get(), self.0.operation.get())
        });
        self.set(|s| {
            s.busy = true;
            s.error.clear();
        });
        self.notify();
        let Some(proxy) = self.proxy() else { return false };
        let weak = self.weak();
        // Authorization can take longer than the default 25-second timeout.
        proxy.call(method, Some(parameters), gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION, WRITE_TIMEOUT, None,
                   Box::new(move |reply| {
            let Some(this) = Self::up(&weak) else { return };
            if let Err(error) = reply {
                if this.current(token) {
                    this.set(|s| s.busy = false);
                    this.fail(&error_text(&error));
                    this.refresh();
                }
                return;
            }
            if !this.current(token) {
                return;
            }
            this.set(|s| {
                s.busy = false;
                s.error.clear();
            });
            this.notify();
            this.refresh();
        }));
        true
    }
}

impl DateSystem for DateBackend {
    fn state(&self) -> DateState {
        self.0.state.borrow().clone()
    }

    fn now(&self) -> ZonedTime {
        let name = self.0.state.borrow().timezone.clone();
        let clock = self.0.clock.borrow().clone();
        let zone = if name.is_empty() { None } else { tz::zoneinfo(&name).ok() };
        let zone = zone.or_else(|| tz::zone_from_file(std::path::Path::new("/etc/localtime")).ok());
        match zone {
            Some(zone) => clock(&zone),
            None => {
                // datetime.now().astimezone(): a fixed offset from the C library's zone.
                let local = clock(&tz::local_zone());
                let fixed = Zone::fixed(local.utcoffset(), &local.tzname());
                ZonedTime { wall: local.wall, fold: 0, zone: fixed }
            }
        }
    }

    fn start(&self) {
        if self.0.closed.get() || self.0.started.get() {
            return;
        }
        self.0.started.set(true);
        self.connect();
    }

    fn close(&self) {
        if self.0.closed.get() {
            return;
        }
        self.0.closed.set(true);
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.read_cancel.borrow().cancel();
        for (_, source) in self.0.sources.borrow_mut().drain() {
            source.remove();
        }
        self.detach_proxy();
        self.set(|s| {
            s.ready = false;
            s.busy = false;
        });
        // An outstanding authenticated write is not cancelled (that would not
        // cancel the action in the service); its reply is ignored.
    }

    fn set_ntp(&self, enabled: bool) -> bool {
        if !self.can_write() {
            return false;
        }
        if !self.0.state.borrow().can_ntp {
            return self.fail("No network time synchronization service is available.");
        }
        self.write("SetNTP", (enabled, true).to_variant(), None)
    }

    fn set_date(&self, selected: Date) -> bool {
        self.manual_write(Rc::new(move |this: &DateBackend| {
            let usec = date_timestamp(selected, &this.now())?;
            Ok(("SetTime", (usec, false, true).to_variant()))
        }))
    }

    fn set_datetime(&self, selected: Date, time: Clock) -> bool {
        self.manual_write(Rc::new(move |this: &DateBackend| {
            let usec = datetime_timestamp(selected, time, &this.now())?;
            Ok(("SetTime", (usec, false, true).to_variant()))
        }))
    }

    fn set_timezone(&self, name: &str) -> bool {
        let name = name.to_owned();
        self.manual_write(Rc::new(move |this: &DateBackend| {
            if !valid_zone_name(&name) {
                return Err("Choose a valid time zone from the list.".into());
            }
            let (ready, listed) = { let s = this.0.state.borrow(); (s.timezones_ready, s.timezones.contains(&name)) };
            if ready && !listed {
                return Err("That time zone is not supported by the system.".into());
            }
            if tz::zoneinfo(&name).is_err() {
                return Err("That time zone is not installed on the system.".into());
            }
            Ok(("SetTimezone", (name.as_str(), true).to_variant()))
        }))
    }
}

/// Midnight-safe helper for tests and previews: `datetime(y, m, d, h, mi, s, us, tzinfo=zone)`.
pub fn zoned(y: i64, m: i64, d: i64, h: i64, mi: i64, s: i64, us: i64, zone: &ZoneRef) -> ZonedTime {
    ZonedTime::new(Naive::new(Date::ymd(y, m, d), h, mi, s, us), zone.clone())
}
