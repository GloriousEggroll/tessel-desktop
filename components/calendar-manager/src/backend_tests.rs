//! Port of test_date_backend.py: no bus connection or system clock writes.

use crate::backend::*;
use crate::civil::Date;
use crate::tz;
use glib::prelude::*;
use glib::Variant;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

struct PendingCall {
    method: String,
    params: Option<Variant>,
    flags: gio::DBusCallFlags,
    timeout: i32,
    cancellable: bool,
    done: Box<dyn FnOnce(Reply)>,
}

#[derive(Default)]
struct FakeProxy {
    values: RefCell<BTreeMap<String, Variant>>,
    pending: RefCell<Vec<PendingCall>>,
    history: RefCell<Vec<(String, Option<Variant>)>>,
    owner: RefCell<Option<String>>,
}

impl FakeProxy {
    fn new(ntp: bool) -> Rc<Self> {
        let proxy = Self::default();
        proxy.values.borrow_mut().extend([("Timezone".to_owned(), "America/Denver".to_variant()), ("NTP".into(), ntp.to_variant()),
                                          ("CanNTP".into(), true.to_variant()), ("NTPSynchronized".into(), false.to_variant())]);
        proxy.owner.replace(Some(":1.fake".into()));
        Rc::new(proxy)
    }

    fn set(&self, name: &str, value: Variant) {
        self.values.borrow_mut().insert(name.into(), value);
    }

    fn methods(&self) -> Vec<String> {
        self.history.borrow().iter().map(|(m, _)| m.clone()).collect()
    }

    fn pending_method(&self, index: usize) -> String {
        self.pending.borrow()[index].method.clone()
    }

    fn all(values: &BTreeMap<String, Variant>) -> Variant {
        let dict = glib::VariantDict::new(None);
        for (key, value) in values {
            dict.insert_value(key, value);
        }
        Variant::tuple_from_iter([dict.end()])
    }

    fn reply_with(&self, index: usize, values: Option<BTreeMap<String, Variant>>, zones: Option<Vec<&str>>, error: Option<BusError>) {
        let call = self.pending.borrow_mut().remove(index);
        let result = if let Some(error) = error {
            Err(error)
        } else if call.method == GET_ALL {
            Ok(Self::all(&values.unwrap_or_else(|| self.values.borrow().clone())))
        } else if call.method == "ListTimezones" {
            let zones: Vec<String> = zones.unwrap_or(vec!["UTC", "America/Denver", "Europe/London"]).into_iter().map(String::from).collect();
            Ok(Variant::tuple_from_iter([zones.to_variant()]))
        } else {
            Ok(().to_variant())
        };
        (call.done)(result);
    }

    fn reply(&self) {
        self.reply_with(0, None, None, None);
    }
}

impl Proxy for FakeProxy {
    fn name_owner(&self) -> Option<String> { self.owner.borrow().clone() }
    fn cached_property(&self, name: &str) -> Option<Variant> { self.values.borrow().get(name).cloned() }
    fn set_cached_property(&self, name: &str, value: &Variant) { self.set(name, value.clone()); }
    fn call(&self, method: &str, params: Option<Variant>, flags: gio::DBusCallFlags, timeout: i32,
            cancellable: Option<&gio::Cancellable>, done: Box<dyn FnOnce(Reply)>) {
        self.history.borrow_mut().push((method.into(), params.clone()));
        self.pending.borrow_mut().push(PendingCall { method: method.into(), params, flags, timeout, cancellable: cancellable.is_some(), done });
    }
    fn connect(&self, _: Box<dyn Fn()>, _: Box<dyn Fn()>) {}
    fn disconnect(&self) {}
}

struct Fixture {
    backend: DateBackend,
    proxy: Rc<FakeProxy>,
    dyn_proxy: Rc<dyn Proxy>,
    changes: Rc<RefCell<Vec<(bool, bool, bool)>>>,
    errors: Rc<RefCell<Vec<String>>>,
    scheduled: Rc<RefCell<Vec<String>>>,
}

fn fixed_clock(h: i64, mi: i64, s: i64, us: i64) -> ClockFn {
    Rc::new(move |zone: &tz::ZoneRef| zoned(2026, 9, 18, h, mi, s, us, zone))
}

fn denver() -> tz::ZoneRef {
    tz::zoneinfo("America/Denver").unwrap()
}

impl Fixture {
    fn new() -> Self {
        let changes: Rc<RefCell<Vec<(bool, bool, bool)>>> = Rc::default();
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let holder: Rc<RefCell<Option<DateBackend>>> = Rc::default();
        let (c, h) = (changes.clone(), holder.clone());
        let e = errors.clone();
        let connector: Connector = Rc::new(|_, _| panic!("tests never connect to a bus"));
        let backend = DateBackend::with(move || {
            if let Some(b) = h.borrow().as_ref() {
                let s = b.state();
                c.borrow_mut().push((s.ready, s.busy, s.ntp));
            }
        }, move |m| e.borrow_mut().push(m.into()), connector, fixed_clock(14, 20, 31, 123456));
        holder.replace(Some(backend.clone()));
        let proxy = FakeProxy::new(false);
        let dyn_proxy: Rc<dyn Proxy> = proxy.clone();
        backend.set_proxy(dyn_proxy.clone());
        backend.read_cached();
        let scheduled: Rc<RefCell<Vec<String>>> = Rc::default();
        let s = scheduled.clone();
        backend.set_later_hook(Rc::new(move |name, _, _| s.borrow_mut().push(name.into())));
        Self { backend, proxy, dyn_proxy, changes, errors, scheduled }
    }

    fn last_error(&self) -> String {
        self.errors.borrow().last().cloned().unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.backend.close();
    }
}

fn denied() -> BusError {
    BusError::remote("org.freedesktop.DBus.Error.AccessDenied", "Not authorized")
}

fn from_usec(usec: i64, zone: &tz::ZoneRef) -> ZonedTime {
    ZonedTime::from_epoch_micros(usec as i128, zone).unwrap()
}

// ---------------------------------------------------------- CalendarDateTests

#[test]
fn date_preserves_local_time_with_target_dst_offset() {
    let current = zoned(2026, 9, 18, 14, 21, 37, 123456, &denver());
    let target = from_usec(date_timestamp(Date::ymd(2026, 12, 18), &current).unwrap(), &denver());
    assert_eq!(target.wall, zoned(2026, 12, 18, 14, 21, 37, 123456, &denver()).wall);
    assert_eq!(target.utcoffset(), -7 * 3600);
}

#[test]
fn dst_gap_is_rejected() {
    let current = zoned(2026, 1, 18, 2, 30, 0, 0, &denver());
    assert!(date_timestamp(Date::ymd(2026, 3, 8), &current).unwrap_err().contains("does not exist"));
}

#[test]
fn repeated_time_retains_fold() {
    let mut current = zoned(2025, 11, 2, 1, 30, 0, 0, &denver());
    current.fold = 1;
    let target = from_usec(date_timestamp(Date::ymd(2026, 11, 1), &current).unwrap(), &denver());
    assert_eq!(target.fold, 1);
    assert_eq!(target.utcoffset(), -7 * 3600);
}

#[test]
fn invalid_date_types_and_clock_ranges() {
    let current = zoned(2026, 9, 18, 12, 0, 0, 0, &tz::Zone::utc());
    for value in [Date::ymd(1969, 12, 31), Date::ymd(2233, 1, 1), Date::ymd(9999, 12, 31)] {
        assert!(date_timestamp(value, &current).is_err(), "{value:?}");
    }
    assert!(date_timestamp(Date::ymd(2232, 1, 1), &current).is_ok());
}

#[test]
fn entered_time_replaces_all_clock_fields() {
    let current = zoned(2026, 9, 18, 14, 21, 37, 123456, &denver());
    let time = Clock { hour: 1, minute: 2, second: 3, micro: 234567 };
    let target = from_usec(datetime_timestamp(Date::ymd(2026, 12, 18), time, &current).unwrap(), &denver());
    assert_eq!(target.wall, zoned(2026, 12, 18, 1, 2, 3, 234567, &denver()).wall);
}

#[test]
fn entered_time_rejects_dst_gap_and_uses_current_fold() {
    let current = zoned(2026, 9, 18, 12, 0, 0, 0, &denver());
    assert!(datetime_timestamp(Date::ymd(2026, 3, 8), Clock::new(2, 30, 0).unwrap(), &current).unwrap_err().contains("does not exist"));
    for fold in [0, 1] {
        let mut current = current.clone();
        current.fold = fold;
        let usec = datetime_timestamp(Date::ymd(2026, 11, 1), Clock::new(1, 30, 0).unwrap(), &current).unwrap();
        assert_eq!(from_usec(usec, &denver()).fold, fold);
    }
}

#[test]
fn entered_time_rejects_invalid_time_and_date() {
    assert!(Clock::new(24, 0, 0).is_none() && Clock::new(0, 60, 0).is_none() && Clock::new(0, 0, 60).is_none());
    let current = zoned(2026, 9, 18, 12, 0, 0, 0, &tz::Zone::utc());
    for selected in [Date::ymd(1969, 1, 1), Date::ymd(2233, 1, 1)] {
        assert!(datetime_timestamp(selected, Clock::new(12, 0, 0).unwrap(), &current).is_err());
    }
}

// ---------------------------------------------------------- DateBackendTests

#[test]
fn start_requests_system_proxy_without_writing() {
    let calls: Rc<RefCell<u32>> = Rc::default();
    let c = calls.clone();
    let connector: Connector = Rc::new(move |_, _| *c.borrow_mut() += 1);
    let backend = DateBackend::with(|| {}, |_| {}, connector, fixed_clock(0, 0, 0, 0));
    backend.start();
    backend.start();
    assert_eq!(*calls.borrow(), 1);
    backend.close();
    // The production connector targets the system bus name, path and interface.
    assert_eq!((BUS_NAME, OBJECT_PATH, INTERFACE), ("org.freedesktop.timedate1", "/org/freedesktop/timedate1", "org.freedesktop.timedate1"));
}

#[test]
fn cached_read_and_external_changes_never_write() {
    let f = Fixture::new();
    let s = f.backend.state();
    assert!(s.ready && !s.ntp);
    assert_eq!(s.timezone, "America/Denver");
    f.proxy.set("NTP", true.to_variant());
    f.proxy.set("Timezone", "Pacific/Auckland".to_variant());
    f.backend.properties_changed(&f.dyn_proxy);
    assert!(f.backend.state().ntp);
    assert_eq!(f.backend.now().zone.key, "Pacific/Auckland");
    assert!(f.proxy.history.borrow().is_empty());
}

#[test]
fn ntp_write_uses_interactive_authorization_without_optimistic_state() {
    let f = Fixture::new();
    assert!(f.backend.set_ntp(true));
    {
        let pending = f.proxy.pending.borrow();
        let call = &pending[0];
        assert_eq!(call.method, "SetNTP");
        let params = call.params.as_ref().unwrap();
        assert_eq!(params.type_().as_str(), "(bb)");
        assert_eq!(params.get::<(bool, bool)>(), Some((true, true)));
        assert_eq!(call.flags, gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION);
        assert!(call.timeout > 120000);
        assert!(!call.cancellable);
    }
    assert!(f.backend.state().busy);
    assert!(!f.backend.state().ntp);
    f.proxy.reply();
    assert!(!f.backend.state().busy);
    assert_eq!(f.proxy.pending_method(0), GET_ALL);
    f.proxy.set("NTP", true.to_variant());
    f.proxy.reply();
    assert!(f.backend.state().ntp);
}

#[test]
fn date_refreshes_properties_and_uses_latest_wall_clock() {
    let f = Fixture::new();
    let selected = Date::ymd(2026, 12, 20);
    assert!(f.backend.set_date(selected));
    assert_eq!(f.proxy.pending_method(0), GET_ALL);
    f.backend.set_clock(fixed_clock(15, 23, 32, 234567));
    f.proxy.set("Timezone", "Europe/London".to_variant());
    f.proxy.reply();
    let expected = date_timestamp(selected, &f.backend.now()).unwrap();
    {
        let pending = f.proxy.pending.borrow();
        assert_eq!(pending[0].method, "SetTime");
        let params = pending[0].params.as_ref().unwrap();
        assert_eq!(params.type_().as_str(), "(xbb)");
        assert_eq!(params.get::<(i64, bool, bool)>(), Some((expected, false, true)));
        assert_eq!(pending[0].flags, gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION);
    }
    f.proxy.reply();
    assert!(!f.backend.state().busy);
}

#[test]
fn ntp_prevents_manual_date_before_and_after_refresh() {
    let f = Fixture::new();
    f.backend.update_state(|s| s.ntp = true);
    assert!(!f.backend.set_date(Date::ymd(2026, 9, 19)));
    assert!(f.proxy.history.borrow().is_empty());
    f.backend.update_state(|s| s.ntp = false);
    assert!(f.backend.set_date(Date::ymd(2026, 9, 19)));
    f.proxy.set("NTP", true.to_variant());
    f.proxy.reply();
    assert!(!f.backend.state().busy);
    assert!(f.backend.state().ntp);
    assert_eq!(f.proxy.methods(), [GET_ALL]);
    assert!(f.last_error().contains("was enabled"));
}

#[test]
fn denied_authorization_does_not_change_state() {
    let f = Fixture::new();
    f.backend.set_ntp(true);
    f.proxy.reply_with(0, None, None, Some(denied()));
    assert!(!f.backend.state().busy);
    assert!(!f.backend.state().ntp);
    assert!(f.last_error().contains("canceled or denied"));
    f.proxy.reply();
    assert!(f.backend.state().error.contains("canceled or denied"));
}

#[test]
fn service_loss_blocks_controls_and_ignores_stale_write_reply() {
    let f = Fixture::new();
    f.backend.set_ntp(true);
    f.proxy.owner.replace(None);
    f.backend.owner_changed(&f.dyn_proxy);
    assert!(!f.backend.state().ready);
    assert!(!f.backend.state().busy);
    assert!(!f.backend.set_date(Date::ymd(2026, 9, 19)));
    let changes = f.changes.borrow().len();
    f.proxy.reply();
    assert_eq!(f.changes.borrow().len(), changes);
    assert!(f.scheduled.borrow().contains(&"reconnect".to_owned()));
}

#[test]
fn close_ignores_stale_date_preflight_and_write() {
    let f = Fixture::new();
    f.backend.set_date(Date::ymd(2026, 9, 19));
    f.backend.close();
    let changes = f.changes.borrow().len();
    f.proxy.reply();
    assert_eq!(f.changes.borrow().len(), changes);
    assert_eq!(f.proxy.methods(), [GET_ALL]);
}

#[test]
fn concurrent_writes_and_invalid_requests_are_rejected() {
    let f = Fixture::new();
    f.backend.update_state(|s| s.can_ntp = false);
    assert!(!f.backend.set_ntp(true));
    f.backend.update_state(|s| s.can_ntp = true);
    assert!(f.proxy.history.borrow().is_empty());
    assert!(f.backend.set_ntp(true));
    assert!(!f.backend.set_date(Date::ymd(2026, 9, 19)));
    assert!(!f.backend.set_ntp(false));
    assert_eq!(f.proxy.history.borrow().len(), 1);
}

#[test]
fn missing_properties_fail_closed() {
    let f = Fixture::new();
    f.proxy.values.borrow_mut().remove("NTP");
    f.backend.read_cached();
    assert!(!f.backend.state().ready);
    assert!(f.backend.state().ntp);
    assert!(!f.backend.set_date(Date::ymd(2026, 9, 19)));
    assert!(f.proxy.history.borrow().is_empty());
}

#[test]
fn background_read_cannot_overwrite_a_more_recent_write() {
    let f = Fixture::new();
    f.backend.refresh();
    f.backend.set_ntp(true);
    f.proxy.reply_with(1, None, None, None);
    let mut stale = f.proxy.values.borrow().clone();
    stale.insert("NTP".into(), false.to_variant());
    f.proxy.reply_with(0, Some(stale), None, None);
    assert_eq!(f.proxy.pending_method(0), GET_ALL);
    let mut fresh = f.proxy.values.borrow().clone();
    fresh.insert("NTP".into(), true.to_variant());
    f.proxy.reply_with(0, Some(fresh), None, None);
    assert!(f.backend.state().ntp);
}

#[test]
fn timezones_load_async_without_authorization_and_keep_stable_list() {
    let f = Fixture::new();
    let original = f.backend.timezones_handle();
    f.backend.load_timezones();
    f.backend.load_timezones();
    assert_eq!(f.proxy.pending.borrow().len(), 1);
    {
        let pending = f.proxy.pending.borrow();
        assert_eq!(pending[0].method, "ListTimezones");
        assert!(pending[0].params.is_none());
        assert_eq!(pending[0].flags, gio::DBusCallFlags::NONE);
    }
    assert!(!f.backend.state().busy);
    assert!(!f.backend.state().timezones_ready);
    f.proxy.reply_with(0, None, Some(vec!["UTC", "America/Denver", "UTC"]), None);
    assert!(Rc::ptr_eq(&original, &f.backend.timezones_handle()));
    assert_eq!(*original.borrow(), ["America/Denver", "UTC"]);
    assert!(f.backend.state().timezones_ready);
    let count = f.changes.borrow().len();
    f.backend.load_timezones();
    f.proxy.reply_with(0, None, Some(vec!["UTC", "America/Denver"]), None);
    assert_eq!(f.changes.borrow().len(), count);
}

#[test]
fn invalid_timezone_lists_are_rejected() {
    let f = Fixture::new();
    for zones in [vec![], vec!["UTC", "../etc"], vec!["America/Denver\n"]] {
        f.backend.load_timezones();
        f.proxy.reply_with(0, None, Some(zones), None);
        assert!(f.last_error().starts_with("Unable to load time zones: The system service returned an invalid time zone list."));
        assert!(!f.backend.state().timezones_ready);
    }
}

#[test]
fn list_failure_keeps_clock_settings_usable_and_recovers() {
    let f = Fixture::new();
    f.backend.load_timezones();
    f.proxy.reply_with(0, None, None, Some(BusError::remote("org.freedesktop.DBus.Error.Failed", "Unavailable")));
    assert!(f.backend.state().ready);
    assert!(!f.backend.state().timezones_ready);
    assert!(!f.backend.state().busy);
    assert!(f.last_error().contains("Unable to load time zones"));
    assert!(f.backend.set_date(Date::ymd(2026, 9, 19)));
    f.proxy.reply();
    f.proxy.reply();
    f.proxy.reply();
    assert_eq!(f.proxy.pending_method(0), "ListTimezones");
    f.proxy.reply();
    assert!(f.backend.state().timezones_ready);
    assert!(f.backend.state().error.is_empty());
}

#[test]
fn timezone_list_stale_reply_after_close_is_ignored() {
    let f = Fixture::new();
    f.backend.load_timezones();
    f.backend.close();
    let count = f.changes.borrow().len();
    f.proxy.reply();
    assert!(!f.backend.state().timezones_ready);
    assert!(f.backend.state().timezones.is_empty());
    assert_eq!(f.changes.borrow().len(), count);
}

#[test]
fn timezone_only_queues_set_timezone_with_interactive_signature() {
    let f = Fixture::new();
    f.backend.update_state(|s| {
        s.timezones = vec!["America/Denver".into(), "Europe/London".into()];
        s.timezones_ready = true;
    });
    assert!(f.backend.set_timezone("Europe/London"));
    assert_eq!(f.proxy.pending_method(0), GET_ALL);
    f.proxy.reply();
    {
        let pending = f.proxy.pending.borrow();
        assert_eq!(pending[0].method, "SetTimezone");
        let params = pending[0].params.as_ref().unwrap();
        assert_eq!(params.type_().as_str(), "(sb)");
        assert_eq!(params.get::<(String, bool)>(), Some(("Europe/London".into(), true)));
        assert_eq!(pending[0].flags, gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION);
        assert!(!pending[0].cancellable);
    }
    assert_eq!(f.backend.state().timezone, "America/Denver");
    f.proxy.reply();
    f.proxy.set("Timezone", "Europe/London".to_variant());
    f.proxy.reply();
    assert_eq!(f.backend.state().timezone, "Europe/London");
    assert!(!f.proxy.methods().contains(&"SetTime".to_owned()));
}

#[test]
fn timezone_rejects_paths_unknown_or_unlisted_zones() {
    let f = Fixture::new();
    for name in ["", "/etc/localtime", "../America/Denver", "America/../Denver", "America//Denver", "America/Denver\n",
                 "Mars/Olympus", "Europe/London; anything"] {
        assert!(!f.backend.set_timezone(name), "{name:?}");
    }
    f.backend.update_state(|s| {
        s.timezones = vec!["America/Denver".into()];
        s.timezones_ready = true;
    });
    assert!(!f.backend.set_timezone("Europe/London"));
    assert!(f.proxy.history.borrow().is_empty());
    f.backend.update_state(|s| s.timezones_ready = false);
    assert!(f.backend.set_timezone("Europe/London"));
}

#[test]
fn datetime_uses_entered_seconds_and_latest_timezone() {
    let f = Fixture::new();
    let (selected, clock) = (Date::ymd(2026, 12, 20), Clock::new(4, 5, 6).unwrap());
    assert!(f.backend.set_datetime(selected, clock));
    f.proxy.set("Timezone", "Europe/London".to_variant());
    f.proxy.reply();
    let expected = datetime_timestamp(selected, clock, &f.backend.now()).unwrap();
    {
        let pending = f.proxy.pending.borrow();
        assert_eq!(pending[0].method, "SetTime");
        assert_eq!(pending[0].params.as_ref().unwrap().get::<(i64, bool, bool)>(), Some((expected, false, true)));
        assert_eq!(pending[0].flags, gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION);
    }
    let actual = from_usec(expected, &tz::zoneinfo("Europe/London").unwrap());
    assert_eq!((actual.hour(), actual.minute(), actual.second()), (4, 5, 6));
}

#[test]
fn new_manual_setters_reject_ntp_initially_and_after_preflight() {
    let f = Fixture::new();
    let actions: [&dyn Fn() -> bool; 2] = [&|| f.backend.set_timezone("Europe/London"),
                                           &|| f.backend.set_datetime(Date::ymd(2026, 9, 19), Clock::new(12, 0, 0).unwrap())];
    for action in actions {
        f.backend.update_state(|s| s.ntp = true);
        let count = f.proxy.history.borrow().len();
        assert!(!action());
        assert_eq!(f.proxy.history.borrow().len(), count);
        f.backend.update_state(|s| s.ntp = false);
        assert!(action());
        f.proxy.set("NTP", true.to_variant());
        f.proxy.reply();
        assert!(!f.backend.state().busy);
        assert!(f.proxy.pending.borrow().is_empty());
        f.proxy.set("NTP", false.to_variant());
    }
    assert!(f.proxy.methods().iter().all(|m| m == GET_ALL));
}

#[test]
fn timezone_denied_authorization_leaves_timezone_unchanged() {
    let f = Fixture::new();
    f.backend.set_timezone("Europe/London");
    f.proxy.reply();
    f.proxy.reply_with(0, None, None, Some(denied()));
    assert_eq!(f.backend.state().timezone, "America/Denver");
    assert!(!f.backend.state().busy);
    assert!(f.backend.state().error.contains("canceled or denied"));
}

#[test]
fn new_setters_reject_service_loss_and_stale_preflight() {
    let f = Fixture::new();
    f.backend.set_datetime(Date::ymd(2026, 9, 19), Clock::new(12, 0, 0).unwrap());
    f.proxy.owner.replace(None);
    f.backend.owner_changed(&f.dyn_proxy);
    f.proxy.reply();
    assert!(!f.backend.set_timezone("Europe/London"));
    assert!(!f.backend.set_datetime(Date::ymd(2026, 9, 19), Clock::new(12, 0, 0).unwrap()));
    assert!(f.proxy.methods().iter().all(|m| m == GET_ALL));
}

#[test]
fn error_text_mapping() {
    assert_eq!(error_text(&BusError::remote("org.freedesktop.timedate1.AutomaticTimeSyncEnabled", "x")),
               "Turn off automatic date and time before changing the date.");
    assert_eq!(error_text(&BusError::remote("org.freedesktop.PolicyKit1.Error.Cancelled", "x")),
               "Authorization was canceled or denied. No change was applied.");
    assert_eq!(error_text(&BusError { remote: None, message: "Operation was cancelled".into(), cancelled: true }), "The request was canceled.");
    assert_eq!(error_text(&BusError::local("Boom")), "Boom");
    assert_eq!(MAX_TIME_USEC, 8277292035999999);
}
