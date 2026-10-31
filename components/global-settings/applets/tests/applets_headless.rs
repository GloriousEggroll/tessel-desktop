//! Embedded applet pages on fake services, in a private headless compositor
//! only (port of global-settings/test_applet_pages.py plus the
//! test_global_ui.py checks about these pages, and fake-HOME parity of the
//! files the pages write against the Python pages).
//!
//!   AGENT=gs-applets components/global-settings/tests/headless.sh \
//!       target/debug/deps/applets_headless-<hash>
//!
//! Under plain `cargo test` (no GS_HEADLESS) it only prints "skipped".
#![allow(deprecated)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gs_applets::*;
use gs_base::{Host, PageHost, SettingsPage};
use gtk::prelude::*;
use serde_json::{json, Map, Value};

#[path = "support/fake_nm.rs"]
mod fake_nm;
#[path = "support/parity.rs"]
mod parity;

// ------------------------------------------------------------------ helpers

pub fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn wait(mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if condition() {
            return true;
        }
        settle(0.02);
    }
    condition()
}

macro_rules! check {
    ($cond:expr) => { if !$cond { panic!("check failed: {}", stringify!($cond)); } };
    ($cond:expr, $($msg:tt)+) => { if !$cond { panic!("check failed: {}: {}", stringify!($cond), format!($($msg)+)); } };
}

pub struct TestHost {
    pub window: gtk::ApplicationWindow,
    pub app: gtk::Application,
    pub navigation: RefCell<Vec<String>>,
}

impl PageHost for TestHost {
    fn show_section(&self, section: &str) { self.navigation.borrow_mut().push(section.into()) }
    fn application(&self) -> Option<gtk::Application> { Some(self.app.clone()) }
    fn window(&self) -> gtk::Window { self.window.clone().upcast() }
    fn show_page_error(&self, _section: &str, _error: &str, _title: Option<&str>, _icon: &str) {}
}

pub fn new_host(app: &gtk::Application) -> Rc<TestHost> {
    let window = gtk::ApplicationWindow::new(app);
    window.set_title(Some("Global Settings test"));
    window.set_default_size(1120, 750);
    Rc::new(TestHost { window, app: app.clone(), navigation: RefCell::default() })
}

// ------------------------------------------------------------------- fakes

pub struct FakeKeepAwake {
    changed: Changed,
    pub ready: Cell<bool>,
    pub active: Cell<bool>,
    pub requested: Cell<bool>,
    pub closed: Cell<bool>,
    pub history: RefCell<Vec<bool>>,
}

impl keep_awake::ui::Controls for FakeKeepAwake {
    fn ready(&self) -> bool { self.ready.get() }
    fn active(&self) -> bool { self.active.get() }
    fn requested(&self) -> bool { self.requested.get() }
    fn error(&self) -> String { String::new() }
    fn duration(&self) -> u32 { 0 }
    fn until(&self) -> Option<f64> { None }
    fn set_enabled(&self, enabled: bool) {
        self.history.borrow_mut().push(enabled);
        self.requested.set(enabled);
        self.active.set(enabled);
        (self.changed)();
    }
    fn set_duration(&self, _minutes: u32) {}
    fn has_set_duration(&self) -> bool { false }
}

impl KeepAwakeService for FakeKeepAwake {
    fn start(&self) { (self.changed)() }
    fn close(&self) { self.closed.set(true) }
}

pub struct FakePolicy {
    changed: Changed,
    pub closed: Cell<bool>,
    pub settings: RefCell<Value>,
    pub history: RefCell<Vec<Value>>,
    callback: RefCell<Option<SaveReply>>,
}

impl FakePolicy {
    pub fn finish(&self, ok: bool) {
        if ok {
            let last = self.history.borrow().last().cloned().unwrap();
            self.settings.replace(last);
        }
        let callback = self.callback.take().expect("a pending save");
        callback(ok, if ok { String::new() } else { "Policy service rejected this change".into() });
        (self.changed)();
    }
}

impl PowerPolicy for FakePolicy {
    fn ready(&self) -> bool { true }
    fn error(&self) -> String { String::new() }
    fn settings(&self) -> Option<Value> { Some(self.settings.borrow().clone()) }
    fn set_settings(&self, settings: &Value, callback: SaveReply) -> bool {
        self.history.borrow_mut().push(settings.clone());
        self.callback.replace(Some(callback));
        true
    }
    fn start(&self) { (self.changed)() }
    fn close(&self) { self.closed.set(true) }
}

pub struct FakeNotifications {
    pub changed: Changed,
    pub closed: Cell<bool>,
    pub state: RefCell<Map<String, Value>>,
    pub history: RefCell<Vec<Value>>,
}

impl notification_manager::ui::Client for FakeNotifications {
    fn state(&self) -> Map<String, Value> { self.state.borrow().clone() }
    fn ready(&self) -> bool { true }
    fn error(&self) -> String { String::new() }
    fn set_settings(&self, patch: Value) {
        self.history.borrow_mut().push(patch.clone());
        {
            let mut state = self.state.borrow_mut();
            let settings = state.get_mut("settings").and_then(Value::as_object_mut).unwrap();
            for (key, value) in patch.as_object().unwrap() {
                settings.insert(key.clone(), value.clone());
            }
        }
        (self.changed)();
    }
    fn clear_history(&self) {}
    fn clear_app(&self, _key: &str) {}
    fn mark_read(&self) {}
    fn dismiss(&self, _ident: u32) {}
    fn activate(&self, _ident: u32, _key: &str) {}
    fn show_settings(&self) {}
}

impl NotificationService for FakeNotifications {
    fn start(&self) { (self.changed)() }
    fn close(&self) { self.closed.set(true) }
}

pub struct FakeBattery {
    changed: Changed,
    pub closed: Cell<bool>,
    pub battery_history: RefCell<Vec<(String, bool)>>,
    pub charge_limit_history: RefCell<Vec<Value>>,
}

impl suite_power::BatteryModel for FakeBattery {
    fn power_ready(&self) -> bool { true }
    fn power_source_known(&self) -> bool { true }
    fn on_battery(&self) -> bool { false }
    fn has_battery(&self) -> bool { false }
    fn percentage(&self) -> Option<f64> { None }
    fn busy(&self) -> bool { false }
    fn error(&self) -> String { String::new() }
    fn batteries(&self) -> Vec<Value> {
        vec![json!({"path": "/org/freedesktop/UPower/devices/controller", "name": "Sony Wireless Controller",
            "kind": "Game controller", "type": 12, "icon_name": "input-gaming-symbolic", "percentage": 100.0, "level": "",
            "state": "Fully charged", "state_code": 4, "power_supply": false, "rechargeable": true, "time_to_empty": null,
            "time_to_full": null, "health": null, "cycles": null, "warning": "", "bluetooth": false,
            "charge_threshold_supported": false, "charge_threshold_enabled": null, "charge_start_threshold": null,
            "charge_end_threshold": null, "busy": false})]
    }
    fn charge_limit(&self) -> Value {
        json!({"supported": false, "maximum": null, "battery_count": 0, "busy": false, "error": "", "reason": ""})
    }
    fn set_charge_thresholds(&self, path: &str, enabled: bool) -> bool {
        self.battery_history.borrow_mut().push((path.into(), enabled));
        true
    }
    fn set_charge_limit(&self, maximum: &Value) -> bool {
        self.charge_limit_history.borrow_mut().push(maximum.clone());
        true
    }
}

impl BatterySource for FakeBattery {
    fn start(&self) { (self.changed)() }
    fn close(&self) { self.closed.set(true) }
}

pub struct FakeLanguage {
    pub calls: Mutex<Vec<String>>,
}

impl language_manager::ui::WizardBackend for FakeLanguage {
    fn status(&self) -> Result<Value, String> {
        Ok(json!({"language": "en", "locale": "en_US.UTF-8", "system_locale": "en_US.UTF-8", "system_available": true,
                  "account_available": true, "warnings": [],
                  "languages": language_manager::backend::LANGUAGES.iter().map(|l| json!({"code": l.code, "name": l.name,
                      "locale": l.locale, "installed": true})).collect::<Vec<_>>()}))
    }
    fn apply(&self, code: &str, _progress: &dyn Fn(&str)) -> Result<language_manager::backend::ApplyResult, String> {
        self.calls.lock().unwrap().push(code.into());
        let locale = language_manager::backend::LANGUAGES.iter().find(|l| l.code == code).unwrap().locale;
        Ok(language_manager::backend::ApplyResult::new(true, code, locale))
    }
}

/// Handles to every fake a factory created (Python kept them on the page).
#[derive(Default)]
pub struct Fakes {
    pub audio: RefCell<Option<Rc<audio_manager::fake::FakeAudioBackend>>>,
    pub date: RefCell<Option<Rc<calendar_manager::fake::FakeDateBackend>>>,
    pub battery: RefCell<Option<Rc<FakeBattery>>>,
    pub bluez: RefCell<Option<Rc<bluetooth_manager::testing::FakeBluez>>>,
    pub keep: RefCell<Option<Rc<FakeKeepAwake>>>,
    pub power_backend: RefCell<Option<Rc<power_manager::fakes::FakeSystem>>>,
    pub policy: RefCell<Option<Rc<FakePolicy>>>,
    pub notifications: RefCell<Option<Rc<FakeNotifications>>>,
    pub language: RefCell<Option<Arc<FakeLanguage>>>,
    pub saved_modes: Rc<RefCell<Vec<String>>>,
}

pub fn factories(fakes: &Rc<Fakes>, scratch: &Path) -> Factories {
    let mut f = Factories::default();
    let k = fakes.clone();
    f.audio = Some(Box::new(move |changed, failed| {
        let fake = audio_manager::fake::FakeAudioBackend::new(audio_manager::fake::FakeMode::Populated);
        fake.set_callbacks(move || changed(), move |message| failed(message));
        k.audio.replace(Some(fake.clone()));
        fake
    }));
    let k = fakes.clone();
    f.datetime = Some(Box::new(move |changed, failed| {
        let fake = calendar_manager::fake::FakeDateBackend::new(move || changed(), move |message| failed(message));
        k.date.replace(Some(fake.clone()));
        fake
    }));
    let k = fakes.clone();
    f.battery = Some(Box::new(move |changed, _failed| {
        let fake = Rc::new(FakeBattery { changed, closed: Cell::new(false), battery_history: RefCell::default(),
                                         charge_limit_history: RefCell::default() });
        k.battery.replace(Some(fake.clone()));
        fake
    }));
    let k = fakes.clone();
    f.bluetooth = Some(Box::new(move || {
        let fake = bluetooth_manager::testing::FakeBluez::new("multi-adapter", false);
        k.bluez.replace(Some(fake.clone()));
        Ok(fake.bt.clone())
    }));
    let saved = fakes.saved_modes.clone();
    f.bluetooth_services = Some(bluetooth_manager::manager::Services {
        load_receive_mode: Rc::new(|| "ask".into()),
        save_receive_mode: Rc::new(move |mode| { saved.borrow_mut().push(mode.into()); Ok(()) }),
        receiver: Rc::new(|_, _| panic!("no OBEX agent in Global Settings tests")),
        monitor_config: false,
        ..Default::default()
    });
    let k = fakes.clone();
    f.keepawake = Some(Box::new(move |changed| {
        let fake = Rc::new(FakeKeepAwake { changed, ready: Cell::new(true), active: Cell::new(false),
                                           requested: Cell::new(false), closed: Cell::new(false), history: RefCell::default() });
        k.keep.replace(Some(fake.clone()));
        fake
    }));
    let k = fakes.clone();
    f.power_backend = Some(Box::new(move |_changed, _failed| {
        let fake = power_manager::fakes::FakeSystem::ready();
        k.power_backend.replace(Some(fake.clone()));
        fake
    }));
    let k = fakes.clone();
    f.power = Some(Box::new(move |changed| {
        let mut settings = power_manager::config::validate_settings(&power_manager::config::defaults()).unwrap();
        settings["lock_after"] = json!(777);
        let fake = Rc::new(FakePolicy { changed, closed: Cell::new(false), settings: RefCell::new(settings),
                                        history: RefCell::default(), callback: RefCell::new(None) });
        k.policy.replace(Some(fake.clone()));
        fake
    }));
    let k = fakes.clone();
    f.notifications = Some(Box::new(move |changed| {
        let state = json!({"settings": {"do_not_disturb": false, "suppress_fullscreen": true, "timeout_ms": 6000,
                                        "history_limit": 100}, "notifications": [], "unread": 0});
        let fake = Rc::new(FakeNotifications { changed, closed: Cell::new(false),
                                               state: RefCell::new(state.as_object().unwrap().clone()), history: RefCell::default() });
        k.notifications.replace(Some(fake.clone()));
        fake
    }));
    let k = fakes.clone();
    f.language = Some(Box::new(move || {
        let fake = Arc::new(FakeLanguage { calls: Mutex::default() });
        k.language.replace(Some(fake.clone()));
        fake
    }));
    f.weather_store = Some(Arc::new(weather_manager::backend::ConfigStore::new(Some(scratch.join("weather/config.json")))));
    f.weather_services = Some(weather_manager::settings::Services {
        search: Some(Arc::new(|_query: &str| Ok(Vec::new()))),
        location_probe: Some(Arc::new(|| (false, String::new()))),
        fetch: Some(Arc::new(|_url: &str| Err(weather_manager::backend::WeatherError("offline test".into())))),
        ..Default::default()
    });
    f.capture_path = Some(scratch.join("capture/config.json"));
    f.archive_path = Some(scratch.join("archive/archive-manager.json"));
    f.clipboard_path = Some(scratch.join("clipboard/clipboard.json"));
    f
}

// ----------------------------------------------------------------- fixture

pub struct Fx {
    pub app: gtk::Application,
    pub host: Rc<TestHost>,
    pub dyn_host: Host,
    pub fakes: Rc<Fakes>,
    pub factories: Factories,
    pub pages: RefCell<Vec<Rc<SettingsPage>>>,
}

impl Fx {
    pub fn new(app: &gtk::Application, scratch: &Path) -> Fx {
        let host = new_host(app);
        let fakes = Rc::new(Fakes::default());
        let factories = factories(&fakes, scratch);
        let dyn_host: Host = host.clone();
        Fx { app: app.clone(), host, dyn_host, fakes, factories, pages: RefCell::default() }
    }

    pub fn build(&self, name: &str) -> Rc<SettingsPage> {
        let page = Rc::new(build_page_with(name, &self.dyn_host, &self.factories).unwrap_or_else(|e| panic!("{name}: {e}")));
        self.pages.borrow_mut().push(page.clone());
        page
    }

    /// `page(name)`: build, show in the host, activate; it must be mapped
    /// inside the host and must not open a separate settings window.
    pub fn page(&self, name: &str) -> Rc<SettingsPage> {
        let page = self.build(name);
        self.host.window.set_child(Some(&page.widget));
        self.host.window.present();
        page.activate();
        settle(0.10);
        check!(wait(|| page.widget.is_mapped()), "{name} page is not mapped");
        check!(page.widget.root().map(|r| r == self.host.window.clone().upcast::<gtk::Root>()).unwrap_or(false));
        let host: gtk::Window = self.host.window.clone().upcast();
        let visible: Vec<String> = gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
            .filter(|w| w.is_visible() && *w != host).map(|w| w.title().map(|t| t.to_string()).unwrap_or_default()).collect();
        check!(visible.is_empty(), "embedding opened a separate window: {visible:?}");
        page
    }

    pub fn teardown(&self) {
        for page in self.pages.borrow().iter().rev() {
            page.close();
            page.close(); // Idempotence matters when host close and shutdown race.
        }
        self.host.window.destroy();
        settle(0.05);
    }
}

fn controller<T: 'static>(page: &SettingsPage) -> Rc<T> {
    page.controller::<T>().expect("page controller type")
}

// ------------------------------------------------------------------- tests

fn test_audio_controls_and_loopback_dialog_are_embedded(fx: &Fx) {
    use audio_manager::fake::{Call, SPEAKERS};
    let page = fx.page("audio");
    let audio = controller::<AudioPage>(&page);
    let backend = fx.fakes.audio.borrow().clone().unwrap();
    let row = audio.controller.node_rows.borrow().get(&SPEAKERS).cloned().expect("speaker row");
    row.mute_button.set_active(true);
    check!(backend.history.borrow().contains(&Call::Mute(SPEAKERS, true)));
    audio.controller.add_loopback();
    let dialog = audio.controller.loopback_dialog.borrow().clone().expect("loopback dialog");
    check!(dialog.dialog.transient_for() == Some(fx.host.window.clone().upcast()));
    // Tab headings repeat the sidebar names and are hidden; the footer is shared.
    check!(audio.controller.status_label.has_css_class("gs-footer"));
    check!(page.widget.last_child() == Some(audio.controller.status_label.clone().upcast()));
    check!(page.widget.has_css_class("audiomanager"));
    let row = audio.controller.navigation.row_at_index(3).unwrap();
    check!(gs_base::tags::get_str(&row, gs_base::tags::PAGE_NAME).as_deref() == Some("inputs"));
    let headings: Vec<gtk::Widget> = gs_applets::embed::descendants(&audio.controller.stack, 5).into_iter()
        .filter(|w| w.has_css_class("section-title")).collect();
    check!(!headings.is_empty() && headings.iter().all(|w| !w.is_visible()), "tab headings hidden");
    page.close();
    check!(backend.closed.get());
    check!(!dialog.dialog.is_visible());
}

fn test_bluetooth_stays_embedded_and_scan_uses_host(fx: &Fx) {
    let page = fx.page("bluetooth");
    let bz = fx.fakes.bluez.borrow().clone().unwrap();
    check!(bz.history().is_empty(), "{:?}", bz.history().iter().map(|c| c.method.clone()).collect::<Vec<_>>());
    let bt = controller::<BluetoothPage>(&page);
    let view = &bt.controller;
    check!(view.window.child().is_none(), "content moved into the page");
    view.on_add_device_clicked();
    let add = view.add.borrow().as_ref().map(|a| a.window.clone()).expect("add window");
    check!(add.transient_for() == Some(fx.host.window.clone().upcast()));
    settle(0.05);
    check!(bz.pending().last().map(|p| p.0.clone()).as_deref() == Some("StartDiscovery"), "{:?}", bz.pending());
    bz.complete(None);
    settle(0.05);
    page.deactivate();
    settle(0.05);
    check!(view.add.borrow().is_none());
    check!(bz.pending().last().map(|p| p.0.clone()).as_deref() == Some("StopDiscovery"), "{:?}", bz.pending());
    bz.complete(None);
    let subscriptions = bz.conn.subscription_count();
    check!(!bz.bt.objmgr_ids.borrow().is_empty());
    page.close();
    check!(bz.bt.objmgr_ids.borrow().is_empty(), "ObjectManager subscriptions released");
    check!(bz.conn.subscription_count() < subscriptions);
    check!(view.manager.watched().is_empty());
}

fn test_battery_is_a_live_monitor_with_charge_controls(fx: &Fx) {
    let page = fx.page("battery");
    let battery = controller::<BatteryPage>(&page);
    let fake = fx.fakes.battery.borrow().clone().unwrap();
    check!(battery.controller.batteries.root.is_mapped());
    check!(!battery.controller.close_button.is_visible(), "close button hidden in a page");
    check!(fake.battery_history.borrow().is_empty());
    check!(fake.charge_limit_history.borrow().is_empty());
    page.close();
    check!(fake.closed.get());
    check!(battery.controller.is_destroyed());
}

fn test_datetime_applies_only_explicit_interaction(fx: &Fx) {
    use calendar_manager::fake::FakeOp;
    let page = fx.page("datetime");
    let date = controller::<DatePage>(&page);
    let backend = fx.fakes.date.borrow().clone().unwrap();
    check!(backend.history.borrow().is_empty());
    check!(date.ticking(), "15 s timer runs while active");
    date.controller.0.ntp_switch.set_active(false);
    settle(0.02);
    check!(backend.history.borrow().contains(&FakeOp::Ntp(false)), "{:?}", backend.history.borrow());
    page.deactivate();
    check!(!date.ticking(), "timer stops while hidden");
    page.activate();
    check!(date.ticking());
    page.close();
    check!(!date.ticking());
    check!(backend.closed.get());
}

fn test_datetime_clock_preferences_share_store_without_system_changes(fx: &Fx) {
    use suite_core::clock::ClockPreferencesStore;
    let page = fx.page("datetime");
    let c = controller::<DatePage>(&page).controller.0.clone();
    let backend = fx.fakes.date.borrow().clone().unwrap();
    c.clock_stack_switch.set_active(false);
    c.clock_reverse_switch.set_active(true);
    c.clock_hour_combo.set_selected(1);
    c.clock_date_combo.set_selected(3);
    c.clock_show_year_switch.set_active(false);
    c.clock_show_weekday_switch.set_active(true);
    c.clock_show_date_switch.set_active(false);
    let settings = ClockPreferencesStore::default().load();
    check!(!settings.stacked);
    check!(settings.date_first);
    check!(settings.use_24_hour);
    check!(settings.date_order == "ydm");
    check!(!settings.show_date);
    check!(!settings.show_year);
    check!(settings.show_weekday);
    check!(!c.clock_date_preview.is_visible());
    check!(c.clock_weekday_preview.is_visible());
    check!(backend.history.borrow().is_empty());
    page.deactivate();
    let mut changes = Map::new();
    changes.insert("date_order".into(), json!("dmy"));
    changes.insert("show_date".into(), json!(true));
    changes.insert("show_weekday".into(), json!(false));
    ClockPreferencesStore::default().update(&changes).unwrap();
    page.activate();
    check!(c.clock_date_combo.selected() == 1);
    check!(c.clock_show_date_switch.is_active());
    check!(!c.clock_show_weekday_switch.is_active());
    check!(c.clock_date_preview.is_visible());
    check!(!c.clock_weekday_preview.is_visible());
    check!(c.clock_date_preview.text() == "18/9", "{}", c.clock_date_preview.text());
    check!(backend.history.borrow().is_empty());
    let _ = std::fs::remove_file(ClockPreferencesStore::default().path);
}

fn test_keepawake_closing_page_preserves_service_state(fx: &Fx) {
    let page = fx.page("keepawake");
    let keep = controller::<KeepAwakePage>(&page);
    let fake = fx.fakes.keep.borrow().clone().unwrap();
    keep.controller.toggle().set_active(true);
    check!(fake.active.get());
    check!(*fake.history.borrow() == vec![true]);
    // Inner margins, no popup chrome, shared footer.
    check!(keep.controller.content().margin_top() == 24 && keep.controller.content().margin_start() == 24);
    check!(!page.widget.first_child().unwrap().has_css_class("popup-surface"));
    check!(page.widget.last_child().unwrap().has_css_class("gs-footer"));
    // Companion theme handover: the host supplies the palette; the applet's own is closed.
    check!(gs_applets::embed::companion_theme(keep.controller.window()).is_none());
    check!(!keep.controller.window().css_classes().iter().any(|c| c.starts_with("waydock-companion-")));
    check!(page.widget.has_css_class("keep-awake"));
    page.close();
    check!(fake.closed.get());
    check!(fake.active.get());
    check!(*fake.history.borrow() == vec![true]);
}

fn network_world() -> fake_nm::Shared {
    WORLD.with(|w| w.clone())
}

thread_local!(static WORLD: fake_nm::Shared = Default::default());

fn calls(method: &str) -> usize {
    network_world().borrow().calls.iter().filter(|c| c.1 == method).count()
}

const WIFI_PROFILE: &str = "22222222-2222-4222-8222-222222222222";
const WIFI_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/11";

fn test_network_controls_and_editors_share_host_without_an_agent(fx: &Fx) {
    let page = fx.page("network");
    let net = controller::<NetworkPage>(&page);
    check!(wait(|| net.backend.ready()), "fake NetworkManager loaded");
    check!(net.application.0.agent.borrow().is_none());
    check!(net.gapp.application_id().as_deref() == Some(NETWORK_FACADE_ID));
    check!(net.gapp.flags().contains(gio::ApplicationFlags::NON_UNIQUE));
    let titles: Vec<String> = suite_gtk::utils::children(&net.tabs).iter()
        .map(|c| net.tabs.page(c).title().map(|t| t.to_string()).unwrap_or_default()).collect();
    check!(titles == ["Connections", "Saved connections"], "{titles:?}");
    net.overview.settings_button.emit_clicked();
    check!(net.tabs.visible_child_name().as_deref() == Some("saved"));
    settle(0.1);
    let row = net.controller.rows.borrow().get(WIFI_PROFILE).cloned().expect("wifi profile row");
    let before = calls("ActivateConnection");
    row.connect_button.emit_clicked();
    check!(wait(|| calls("ActivateConnection") > before), "activate_profile reached NetworkManager");
    net.controller.add_button.emit_clicked();
    settle(0.1);
    let editors = net.gui.editors();
    check!(!editors.is_empty());
    check!(editors[0].1.window.transient_for() == Some(fx.host.window.clone().upcast()));
    let profile = net.backend.state().profiles.get(WIFI_PROFILE).cloned().unwrap();
    let dialog = net.controller.confirm_remove(&profile).expect("confirm dialog");
    check!(dialog.transient_for() == Some(fx.host.window.clone().upcast()));
    dialog.response(gtk::ResponseType::Cancel);
    page.close();
    check!(net.backend.closed());
    check!(net.gui.children().is_empty());
}

fn test_network_overview_scans_toggles_and_native_wifi_wizards(fx: &Fx) {
    let page = fx.page("network");
    let net = controller::<NetworkPage>(&page);
    check!(wait(|| net.backend.ready()));
    settle(0.1);
    let popup = &net.overview;
    check!(popup.controller().is_none());
    check!(net.tabs.visible_child_name().as_deref() == Some("connections"));
    check!(wait(|| popup.scan_buttons.borrow().contains_key(WIFI_DEVICE)), "wifi scan button");
    let before = calls("RequestScan");
    let scan = popup.scan_buttons.borrow().get(WIFI_DEVICE).cloned().unwrap();
    scan.emit_clicked();
    check!(wait(|| calls("RequestScan") > before), "scan reached NetworkManager");
    let previous = network_world().borrow().calls.len();
    popup.hidden_button.emit_clicked();
    settle(0.1);
    let dialog = net.gui.children().last().cloned().expect("hidden network dialog");
    check!(dialog.transient_for() == Some(fx.host.window.clone().upcast()));
    check!(dialog.title().as_deref() == Some("Hidden Wi-Fi Network"));
    dialog.destroy();
    popup.hotspot_button.emit_clicked();
    settle(0.1);
    let dialog = net.gui.children().last().cloned().expect("hotspot dialog");
    check!(dialog.transient_for() == Some(fx.host.window.clone().upcast()));
    check!(dialog.title().as_deref() == Some("Create Wi-Fi Network"));
    check!(network_world().borrow().calls.len() == previous, "Opening a wizard must not change networking");
    check!(net.tabs.visible_child_name().as_deref() == Some("connections"));
    // One header above the tabs; the overview's settings button is replaced by the tabs.
    check!(!popup.settings_button.is_visible());
    check!(popup.subtitle.is_ancestor(&page.widget.first_child().unwrap()));
    // Notification preferences button in the saved tab's heading.
    let heading = net.controller.window.child().is_none();
    check!(heading, "settings content moved into the page");
}

fn test_power_save_waits_for_resident_service_acknowledgement(fx: &Fx) {
    let page = fx.page("power");
    let power = controller::<PowerPage>(&page);
    let view = &power.controller;
    let client = fx.fakes.policy.borrow().clone().unwrap();
    check!(view.lock_choice.active_id().as_deref() == Some("777"));
    view.lock_choice.set_active_id(Some("600"));
    check!(view.save_button.is_sensitive());
    check!(view.saved.borrow()["lock_after"] == 777);
    view.save_button.emit_clicked();
    check!(client.history.borrow().last().unwrap()["lock_after"] == 600);
    check!(view.saved.borrow()["lock_after"] == 777);
    check!(!view.save_button.is_sensitive());
    client.finish(false);
    check!(view.save_button.is_sensitive());
    check!(view.status_label.text().contains("rejected"), "{}", view.status_label.text());
    view.save_button.emit_clicked();
    client.finish(true);
    check!(view.saved.borrow()["lock_after"] == 600);
    check!(view.status_label.has_css_class("gs-footer"));
    check!(!view.close_button.is_visible());
    page.close();
    let backend = fx.fakes.power_backend.borrow().clone().unwrap();
    check!(backend.closed.get() == 1);
    check!(client.closed.get());
}

fn test_switching_pages_preserves_controls_and_resident_state(fx: &Fx) {
    let stack = gtk::Stack::new();
    fx.host.window.set_child(Some(&stack));
    fx.host.window.present();
    let mut by_name = BTreeMap::new();
    for name in APPLET_SECTIONS {
        let page = fx.build(name);
        stack.add_named(&page.widget, Some(name));
        stack.set_visible_child_name(name);
        page.activate();
        settle(0.03);
        check!(wait(|| page.widget.is_mapped()), "{name}");
        page.deactivate();
        by_name.insert(*name, page);
    }
    controller::<KeepAwakePage>(&by_name["keepawake"]).controller.toggle().set_active(true);
    stack.set_visible_child_name("audio");
    by_name["audio"].activate();
    let audio = controller::<AudioPage>(&by_name["audio"]);
    let row = audio.controller.node_rows.borrow().get(&audio_manager::fake::SPEAKERS).cloned().unwrap();
    row.mute_button.set_active(true);
    let fake = fx.fakes.audio.borrow().clone().unwrap();
    check!(fake.history.borrow().contains(&audio_manager::fake::Call::Mute(audio_manager::fake::SPEAKERS, true)));
    let keep = fx.fakes.keep.borrow().clone().unwrap();
    check!(keep.active.get());
    check!(*keep.history.borrow() == vec![true]);
}

fn test_notifications_share_service_controls_and_release_subscription(fx: &Fx) {
    let page = fx.page("notifications");
    let view = controller::<NotificationsPage>(&page).controller.0.clone();
    let client = fx.fakes.notifications.borrow().clone().unwrap();
    view.dnd_switch.set_active(true);
    view.fullscreen_switch.set_active(false);
    view.timeout_spin.set_value(9.0);
    check!(*client.history.borrow() == vec![json!({"do_not_disturb": true}), json!({"suppress_fullscreen": false}),
                                          json!({"timeout_ms": 9000})], "{:?}", client.history.borrow());
    client.state.borrow_mut().get_mut("settings").unwrap()["do_not_disturb"] = json!(false);
    (client.changed)();
    check!(!view.dnd_switch.is_active());
    check!(client.history.borrow().len() == 3);
    // Margins + scroller + shared footer.
    check!(page.widget.first_child().unwrap().is::<gtk::ScrolledWindow>());
    check!(page.widget.last_child() == Some(view.status_label.clone().upcast()));
    page.close();
    check!(client.closed.get());
}

fn test_language_wizard_is_embedded_with_close_and_can_close(fx: &Fx) {
    let page = fx.page("language");
    let wizard = controller::<language_manager::ui::LanguageWizard>(&page);
    check!(!wizard.cancel_button.is_visible());
    check!(page.widget.has_css_class("languagemanager"));
    check!(wizard.window.transient_for() == Some(fx.host.window.clone().upcast()));
    check!(wait(|| !wizard.progress_row.is_visible()), "status loaded");
    check!(page.can_close());
    wizard.busy.set(true);
    check!(!page.can_close(), "an in-flight change refuses close");
    wizard.busy.set(false);
    // The result page's Close button returns to the first step and reloads.
    wizard.show_page("result");
    wizard.close();
    check!(wizard.page.get() == "choose");
    check!(wait(|| !wizard.progress_row.is_visible()));
    page.close();
    check!(wizard.closed.get());
}

fn test_weather_and_capture_register_search_targets(fx: &Fx) {
    let weather = fx.page("weather");
    for key in ["location", "current_location", "units", "forecast"] {
        check!(weather.search_targets.borrow().contains_key(key), "weather {key}");
    }
    let capture = fx.page("capture");
    let controls = controller::<CapturePage>(&capture);
    let card = controls.card.clone().expect("shared card");
    check!(card.has_css_class("gs-card"));
    for key in ["copy_to_clipboard", "open_editor"] {
        let switch = controls.controls.switches[key].clone();
        check!(capture.search_targets.borrow().get(key) == Some(&switch.clone().upcast()));
        let row = switch.parent().unwrap();
        check!(row.parent() == Some(card.clone().upcast()));
        check!(row.has_css_class("gs-row"));
        let title = row.first_child().unwrap().first_child().unwrap();
        check!(title.has_css_class("gs-row-title"));
        let hint = title.next_sibling().unwrap();
        check!(hint.has_css_class("gs-row-subtitle") && !hint.has_css_class("dim-label"));
    }
    check!(controls.controls.page.margin_top() == 24);
}

fn test_archive_page_edits_the_archive_manager_settings_file(fx: &Fx) {
    let page = fx.page("archive");
    let controls = controller::<gs_applets::ArchivePage>(&page).controls.clone();
    let path = fx.factories.archive_path.clone().unwrap();
    for key in ["show_info_panel", "open_destination", "close_after_extraction", "preserve_paths", "extract_subfolder",
                "internal_preview", "limit_preview_size", "plugin-cli7z", "preview_max_mb"] {
        check!(page.search_targets.borrow().contains_key(key), "archive target {key}");
    }
    check!(controls.page.margin_top() == 24);
    check!(controls.page.first_child().is_some());
    controls.switches["open_destination"].set_active(true);
    controls.plugin_switches["clizip"].set_active(false);
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    check!(saved["open_destination"] == serde_json::json!(true));
    check!(saved["disabled_plugins"] == serde_json::json!(["clizip"]));
    // Archive Manager writes the same file: the page follows.
    archive_settings::Store::new(Some(path.clone())).update(|s| s.preserve_paths = false).unwrap();
    check!(wait(|| !controls.switches["preserve_paths"].is_active()), "page did not reload");
    page.close();
}

fn test_clipboard_page_edits_the_clipboard_settings_file(fx: &Fx) {
    let page = fx.page("clipboard");
    let controls = controller::<gs_applets::ClipboardPage>(&page).controls.clone();
    let path = fx.factories.clipboard_path.clone().unwrap();
    for key in ["keep_clipboard", "keep_primary", "skip_secrets", "max_mb"] {
        check!(page.search_targets.borrow().contains_key(key), "clipboard target {key}");
    }
    check!(controls.page.margin_top() == 24);
    check!(controls.switches["keep_clipboard"].is_active() && !controls.switches["keep_primary"].is_active());
    controls.switches["keep_primary"].set_active(true);
    controls.max_size.set_value(8.0);
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    check!(saved["keep_primary"] == serde_json::json!(true));
    check!(saved["max_mb"] == serde_json::json!(8));
    controls.switches["keep_clipboard"].set_active(false);
    controls.switches["keep_primary"].set_active(false);
    check!(!controls.max_size.is_sensitive() && !controls.switches["skip_secrets"].is_sensitive());
    // Another writer changes the file: the page follows.
    clipboard_settings::Store::new(Some(path.clone())).update(|s| s.keep_clipboard = true).unwrap();
    check!(wait(|| controls.switches["keep_clipboard"].is_active()), "page did not reload");
    check!(controls.max_size.is_sensitive());
    page.close();
}

fn test_passwords_page_lists_copies_and_deletes_waykeyring_entries(fx: &Fx) {
    let page_without = fx.page("passwords");
    let controls = controller::<gs_applets::PasswordsPage>(&page_without).controls.clone();
    check!(controls.status_title.label().contains("Another"), "no waykeyring yet: {}", controls.status_title.label());
    page_without.close();
    // A real waykeyring on this private bus (scratch keyring, no kernel keyring, fake prompt).
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/debug/waykeyring");
    check!(bin.exists(), "build it first: cargo build -p waykeyring ({})", bin.display());
    let dir = fx.factories.clipboard_path.clone().unwrap().parent().unwrap().parent().unwrap().join("keyring");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prompt = dir.join("prompt.sh");
    std::fs::write(&prompt, "#!/bin/sh\necho pw\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut login = std::process::Command::new(&bin).args(["--login", "--no-kernel", "--dir"]).arg(dir.join("k"))
        .stdin(std::process::Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    login.stdin.take().unwrap().write_all(b"pw").unwrap();
    check!(login.wait().unwrap().success());
    let mut daemon = std::process::Command::new(&bin).arg("--dir").arg(dir.join("k")).arg("--gnome-dir").arg(dir.join("g"))
        .arg("--prompt").arg(&prompt).args(["--no-kernel", "--no-chkpwd"]).spawn().unwrap();
    let store = |label: &str, attrs: &[&str], secret: &str| {
        let mut c = std::process::Command::new("secret-tool").arg("store").arg(format!("--label={label}")).args(attrs)
            .stdin(std::process::Stdio::piped()).spawn().unwrap();
        c.stdin.take().unwrap().write_all(secret.as_bytes()).unwrap();
        c.wait().unwrap().success()
    };
    check!(wait(|| store("GitHub", &["server", "github.com", "user", "me"], "token")), "secret-tool store");
    check!(store("Slack Safe Storage", &["application", "Slack"], "s2"));
    let page = fx.page("passwords");
    let controls = controller::<gs_applets::PasswordsPage>(&page).controls.clone();
    for key in ["saved", "status"] {
        check!(page.search_targets.borrow().contains_key(key), "passwords target {key}");
    }
    check!(controls.status_title.label() == "Protected by your login password", "{}", controls.status_title.label());
    let rows = |c: &waykeyring_ui::page::PasswordsPage| {
        let mut n = 0;
        let mut child = c.list.first_child();
        while let Some(w) = child {
            n += 1;
            child = w.next_sibling();
        }
        n
    };
    check!(rows(&controls) == 2, "rows {}", rows(&controls));
    let github = controls.list.first_child().unwrap();
    check!(github.widget_name().contains("/collection/login/"));
    controls.search.set_text("slack");
    check!(wait(|| rows(&controls) == 1));
    controls.search.set_text("nothing-like-this");
    check!(wait(|| rows(&controls) == 0 && controls.empty.is_visible()));
    controls.search.set_text("");
    check!(wait(|| rows(&controls) == 2));
    let client = controls.client.clone().unwrap();
    check!(client.secret(github.widget_name().as_str()).as_deref() == Some(&b"token"[..]));
    controls.delete(github.widget_name().as_str());
    check!(wait(|| rows(&controls) == 1), "delete");
    // Another client adds an entry: the page follows waykeyring's Changed signal.
    check!(store("Git", &["protocol", "https", "host", "gitlab.com"], "t3"));
    check!(wait(|| rows(&controls) == 2), "page did not follow Changed");
    controls.lock.emit_clicked();
    check!(wait(|| controls.status_title.label() == "Locked" && controls.unlock.is_visible()));
    controls.unlock.emit_clicked();
    check!(wait(|| controls.status_title.label() == "Protected by your login password"), "unlock via prompt");
    page.close();
    let _ = daemon.kill();
    let _ = daemon.wait();
}

/// Every `target=` of these sections in settings_search.py resolves to a widget.
fn test_search_targets_match_settings_search(fx: &Fx) {
    let source = std::env::var("BTMANAGER").map(PathBuf::from).unwrap_or_default().join("global-settings/settings_search.py");
    let text = std::fs::read_to_string(&source).expect("settings_search.py");
    let pattern = regex_lite(&text);
    check!(!pattern.is_empty());
    let mut built: BTreeMap<String, Rc<SettingsPage>> = BTreeMap::new();
    for (section, key, target) in pattern {
        let page = built.entry(section.clone()).or_insert_with(|| fx.build(&section)).clone();
        let targets = page.search_targets.borrow();
        let found = targets.contains_key(&key) || (!target.is_empty() && targets.contains_key(&target));
        check!(found || target.is_empty(), "{section}.{key} target {target:?} not registered");
    }
}

/// (section, key, target) of the Setting(...) rows for this crate's sections.
fn regex_lite(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Setting(") else { continue };
        let fields: Vec<String> = rest.split(',').take(2)
            .map(|f| f.trim().trim_matches(|c| c == '"' || c == '\'').to_owned()).collect();
        if fields.len() < 2 || !SECTIONS.contains(&fields[0].as_str()) {
            continue;
        }
        let target = line.find("target=").map(|i| {
            line[i + 8..].chars().take_while(|c| *c != '"' && *c != '\'').collect::<String>()
        }).unwrap_or_default();
        out.push((fields[0].clone(), fields[1].clone(), target));
    }
    out
}

/// test_global_ui: every page shares one lifecycle; network has one header
/// above its tabs; keep-awake has no popup chrome and a shared footer.
fn test_multiple_embedded_applets_and_language_share_one_lifecycle(fx: &Fx) {
    let stack = gtk::Stack::new();
    fx.host.window.set_child(Some(&stack));
    fx.host.window.present();
    let mut pages = BTreeMap::new();
    for name in SECTIONS.iter().chain(["audio"].iter()) {
        let page = match pages.get(*name) {
            Some(page) => Rc::clone(page),
            None => {
                let page = fx.build(name);
                stack.add_named(&page.widget, Some(name));
                pages.insert(*name, page.clone());
                page
            }
        };
        stack.set_visible_child_name(name);
        page.activate();
        settle(0.03);
        check!(page.widget.is_mapped(), "{name}");
        page.deactivate();
    }
    let wizard = controller::<language_manager::ui::LanguageWizard>(&pages["language"]);
    check!(!wizard.cancel_button.is_visible());
    let network = &pages["network"].widget;
    check!(network.has_css_class("networkmanager"));
    check!(!network.first_child().unwrap().is::<gtk::StackSwitcher>(), "header above the tabs");
    let keepawake = &pages["keepawake"].widget;
    check!(!keepawake.first_child().unwrap().has_css_class("popup-surface"));
    check!(keepawake.last_child().unwrap().has_css_class("gs-footer"));
    for page in pages.values() {
        check!(page.can_close());
    }
}

fn test_unknown_section_is_an_error(fx: &Fx) {
    check!(build_page_with("displays", &fx.dyn_host, &fx.factories).is_err());
    check!(SECTIONS.len() == 14);
}

// --------------------------------------------------------------------- main

type Test = (&'static str, fn(&Fx));

const TESTS: &[Test] = &[
    ("audio_controls_and_loopback_dialog_are_embedded", test_audio_controls_and_loopback_dialog_are_embedded),
    ("bluetooth_stays_embedded_and_scan_uses_host", test_bluetooth_stays_embedded_and_scan_uses_host),
    ("battery_is_a_live_monitor_with_charge_controls", test_battery_is_a_live_monitor_with_charge_controls),
    ("datetime_applies_only_explicit_interaction", test_datetime_applies_only_explicit_interaction),
    ("datetime_clock_preferences_share_store_without_system_changes",
     test_datetime_clock_preferences_share_store_without_system_changes),
    ("keepawake_closing_page_preserves_service_state", test_keepawake_closing_page_preserves_service_state),
    ("network_controls_and_editors_share_host_without_an_agent", test_network_controls_and_editors_share_host_without_an_agent),
    ("network_overview_scans_toggles_and_native_wifi_wizards", test_network_overview_scans_toggles_and_native_wifi_wizards),
    ("power_save_waits_for_resident_service_acknowledgement", test_power_save_waits_for_resident_service_acknowledgement),
    ("switching_pages_preserves_controls_and_resident_state", test_switching_pages_preserves_controls_and_resident_state),
    ("notifications_share_service_controls_and_release_subscription",
     test_notifications_share_service_controls_and_release_subscription),
    ("language_wizard_is_embedded_with_close_and_can_close", test_language_wizard_is_embedded_with_close_and_can_close),
    ("weather_and_capture_register_search_targets", test_weather_and_capture_register_search_targets),
    ("search_targets_match_settings_search", test_search_targets_match_settings_search),
    ("archive_page_edits_the_archive_manager_settings_file", test_archive_page_edits_the_archive_manager_settings_file),
    ("clipboard_page_edits_the_clipboard_settings_file", test_clipboard_page_edits_the_clipboard_settings_file),
    ("passwords_page_lists_copies_and_deletes_waykeyring_entries",
     test_passwords_page_lists_copies_and_deletes_waykeyring_entries),
    ("multiple_embedded_applets_and_language_share_one_lifecycle",
     test_multiple_embedded_applets_and_language_share_one_lifecycle),
    ("unknown_section_is_an_error", test_unknown_section_is_an_error),
];

fn main() {
    if std::env::var_os("GS_HEADLESS").is_none() {
        println!("applets_headless: skipped (run through components/global-settings/tests/headless.sh)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(!runtime.is_empty() && runtime != "/run/user/1000", "refusing to run on the live session");
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|a| a == "--rust-driver") {
        gtk::init().expect("headless display");
        suite_core::i18n::initialize();
        std::process::exit(parity::rust_driver(&args[index + 1]));
    }
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("private bus");
    assert_eq!(std::env::var("DBUS_SYSTEM_BUS_ADDRESS").ok().as_deref(), Some(address.as_str()), "system bus must be private");
    gtk::init().expect("headless display");
    suite_core::i18n::initialize();
    let _nm = fake_nm::serve_nm(&address, &network_world());
    let root = PathBuf::from(std::env::var("GS_ROOT").expect("GS_ROOT"));
    let scratch = root.join("tmp/applets");
    let app = gtk::Application::new(None::<&str>, gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gio::Cancellable>).expect("register");
    let filter: Vec<String> = args.iter().skip(1).filter(|a| !a.starts_with('-')).cloned().collect();
    let mut failed = Vec::new();
    let mut passed = 0;
    for (name, test) in TESTS {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let fx = Fx::new(&app, &scratch);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test(&fx)));
        let teardown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fx.teardown()));
        match (result, teardown) {
            (Ok(()), Ok(())) => {
                passed += 1;
                println!("ok   {name}");
            }
            _ => {
                println!("FAIL {name}");
                failed.push(name.to_string());
            }
        }
    }
    if filter.is_empty() || filter.iter().any(|f| f == "parity") {
        let (p, f) = parity::run_all(&root);
        passed += p;
        failed.extend(f);
    }
    println!("applets_headless: {passed} passed, {} failed {:?}", failed.len(), failed);
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
