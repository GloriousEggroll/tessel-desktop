//! Port of test_weather_ui.py: fixture data, fake location services, no
//! Internet. Runs only inside the private nested headless labwc
//! (tests/run_headless.sh); under a plain `cargo test` it skips itself.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use weather_manager::app::{WeatherApp, APPLICATION_ID};
use weather_manager::backend::{week_dates, ConfigStore, CurrentWeather, ForecastDay, Location, Num, WeatherConfig, WeatherSnapshot};
use weather_manager::dates::Date;
use weather_manager::location::{LocationCallback, PendingLocation};
use weather_manager::settings::{Services, WeatherSettings};
use weather_manager::tray::{TrayHost, WeatherTray};
use weather_manager::ui::WeatherSource;

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.01);
        if predicate() {
            return true;
        }
    }
    predicate()
}

fn snapshot(config: &WeatherConfig, stale: bool, error: Option<String>, current: bool) -> WeatherSnapshot {
    let days = week_dates(Date::new(2026, 9, 24).unwrap(), &config.week).unwrap().into_iter().enumerate()
        .map(|(i, day)| ForecastDay { date: day, temp_min: Some(48.0 + i as f64), temp_max: Some(68.0 + i as f64),
                                      code: Some(i as i64 % 4), precip_probability: Some(i as f64 * 10.0), past: false })
        .collect();
    let value = current.then(|| CurrentWeather { time: "2026-09-24T12:00".into(),
        temp: if config.units == "fahrenheit" { 72.0 } else { 22.0 }, code: 0, is_day: true,
        apparent: Some(71.0), humidity: Some(45.0), wind: Some(8.0) });
    WeatherSnapshot { config: config.clone(), current: value, days, fetched_at: Some(1790272800.0), stale, error }
}

type ReadFn = Box<dyn Fn(&WeatherConfig, bool) -> Result<WeatherSnapshot, String> + Send + Sync>;

#[derive(Default)]
struct FakeState {
    calls: Vec<(WeatherConfig, bool, std::thread::ThreadId)>,
    stale: bool,
    error: Option<String>,
    no_current: bool,
    read: Option<Arc<ReadFn>>,
    cache_dir: Option<PathBuf>,
}

#[derive(Default)]
struct FakeClient(Mutex<FakeState>);

impl WeatherSource for FakeClient {
    fn read(&self, config: &WeatherConfig, force: bool) -> Result<WeatherSnapshot, String> {
        let custom = {
            let mut state = self.0.lock().unwrap();
            state.calls.push((config.clone(), force, std::thread::current().id()));
            state.read.clone()
        };
        if let Some(read) = custom {
            return read(config, force);
        }
        let state = self.0.lock().unwrap();
        Ok(snapshot(config, state.stale, state.error.clone(), !state.no_current))
    }
    fn cache_path(&self, config: &WeatherConfig) -> Option<PathBuf> {
        self.0.lock().unwrap().cache_dir.as_ref().map(|d| d.join(format!("{}.json", config.name)))
    }
    fn cache_dir(&self) -> Option<PathBuf> { self.0.lock().unwrap().cache_dir.clone() }
}

impl FakeClient {
    fn calls(&self) -> usize { self.0.lock().unwrap().calls.len() }
    fn set_read(&self, read: ReadFn) { self.0.lock().unwrap().read = Some(Arc::new(read)); }
}

/// A gate a worker thread can wait on.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn set(&self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }
    fn is_set(&self) -> bool { *self.0 .0.lock().unwrap() }
    fn wait(&self, seconds: f64) {
        let guard = self.0 .0.lock().unwrap();
        let _ = self.0 .1.wait_timeout_while(guard, Duration::from_secs_f64(seconds), |set| !*set).unwrap();
    }
}

struct FakeRequest {
    callback: RefCell<Option<LocationCallback>>,
    cancelled: Cell<bool>,
}

impl PendingLocation for FakeRequest {
    fn cancel(&self) { self.cancelled.set(true); }
}

impl FakeRequest {
    fn call(&self, position: Option<(f64, f64)>, error: Option<&str>) {
        if let Some(callback) = self.callback.take() {
            callback(position, error.map(String::from));
        }
    }
}

type Requests = Rc<RefCell<Vec<Rc<FakeRequest>>>>;

fn request_factory(requests: &Requests) -> weather_manager::settings::RequestFactory {
    let requests = requests.clone();
    Rc::new(move |callback| {
        let request = Rc::new(FakeRequest { callback: RefCell::new(Some(callback)), cancelled: Cell::new(false) });
        requests.borrow_mut().push(request.clone());
        request as Rc<dyn PendingLocation>
    })
}

fn no_network() -> weather_manager::backend::Fetch {
    // Even an accidental fixture regression must never fetch real weather.
    Arc::new(|url: &str| panic!("GUI tests cannot access the Internet: {url}"))
}

thread_local!(static COUNTER: Cell<u32> = const { Cell::new(0) });

struct Fx {
    root: PathBuf,
    store: Arc<ConfigStore>,
    client: Arc<FakeClient>,
    app: Rc<WeatherApp>,
    widgets: RefCell<Vec<Rc<WeatherSettings>>>,
}

impl Fx {
    fn new() -> Self { Self::with_id(None) }

    fn with_id(id: Option<&str>) -> Self {
        let n = COUNTER.with(|c| { c.set(c.get() + 1); c.get() });
        let root = PathBuf::from(std::env::var("TMPDIR").unwrap()).join(format!("weather-ui-{n}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(ConfigStore::new(Some(root.join("weather.json"))));
        let client = Arc::new(FakeClient::default());
        let id = id.map(String::from).unwrap_or_else(|| format!("org.waydock.WeatherTest{n}"));
        let services = Services { fetch: Some(no_network()), location_probe: probe((false, "Unavailable in tests.")),
                                  ..Services::default() };
        let app = WeatherApp::new(&id, Some(client.clone()), Some(store.clone()), services, "weather_manager.py");
        app.app.register(gio::Cancellable::NONE).expect("register");
        Self { root, store, client, app, widgets: RefCell::new(Vec::new()) }
    }

    fn save_denver(&self) {
        self.store.save(&WeatherConfig::new("Denver", 39.7392, -104.9847, "America/Denver", "fahrenheit")).unwrap();
    }

    fn popup(&self, edge: &str) -> Rc<weather_manager::ui::WeatherPopup> {
        self.app.show_popup(edge, 40);
        let popup = self.app.popup().unwrap();
        assert!(wait_for(|| popup.snapshot.borrow().is_some(), 3.0));
        popup
    }

    fn settings(&self, services: Services) -> Rc<WeatherSettings> {
        let services = Services { fetch: Some(no_network()), ..services };
        let widget = WeatherSettings::new(Some(self.store.clone()), services);
        self.widgets.borrow_mut().push(widget.clone());
        widget
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for widget in self.widgets.borrow().iter() {
            widget.close();
        }
        for window in self.app.app.windows() {
            window.destroy();
        }
        self.app.shutdown();
        settle(0.05);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn probe(result: (bool, &'static str)) -> Option<weather_manager::settings::ProbeFn> {
    Some(Arc::new(move || (result.0, result.1.to_owned())))
}

fn current_location_success_denial_and_manual_selection() {
    let fx = Fx::new();
    let requests: Requests = Rc::default();
    let widget = fx.settings(Services {
        location_probe: probe((true, "Available")), location_request: Some(request_factory(&requests)),
        location_resolve: Some(Arc::new(|lat, lon| Ok(Location { name: "Current location".into(), latitude: Num::Float(lat),
                                                                 longitude: Num::Float(lon), timezone: "Europe/Paris".into() }))),
        ..Services::default() });
    assert!(wait_for(|| widget.current_location.is_sensitive(), 3.0));
    assert!(!fx.store.path.exists());
    widget.current_location.emit_clicked();
    assert!(widget.cancel_location.is_visible());
    let last = || requests.borrow().last().unwrap().clone();
    last().call(Some((48.86, 2.29)), None);
    assert!(wait_for(|| fx.store.path.exists(), 3.0));
    assert_eq!(fx.store.load().timezone, "Europe/Paris");
    assert_eq!(fx.store.load().latitude, Num::Float(48.86));
    assert!(last().cancelled.get());
    let before = std::fs::read(&fx.store.path).unwrap();
    widget.current_location.emit_clicked();
    last().call(None, Some("Location access was denied."));
    assert_eq!(std::fs::read(&fx.store.path).unwrap(), before);
    assert!(widget.location_status.text().contains("denied"));
    widget.current_location.emit_clicked();
    let pending = last();
    widget.location_search.set_text("London");
    assert!(pending.cancelled.get());
    pending.call(Some((1.0, 2.0)), None);
    settle(0.05);
    assert_eq!(std::fs::read(&fx.store.path).unwrap(), before);
    widget.current_location.emit_clicked();
    widget.close();
    assert!(last().cancelled.get());
}

fn location_unavailable_and_slow_result_do_not_override_manual_choice() {
    let fx = Fx::new();
    let widget = fx.settings(Services { location_probe: probe((false, "System location services are unavailable.")), ..Services::default() });
    assert!(wait_for(|| widget.location_status.text().contains("unavailable"), 3.0));
    assert!(!widget.current_location.is_sensitive());
    assert!(widget.location_search.is_sensitive());
    assert!(!fx.store.path.exists());
    let requests: Requests = Rc::default();
    let (started, release) = (Gate::default(), Gate::default());
    let (s, r) = (started.clone(), release.clone());
    let widget = fx.settings(Services {
        location_probe: probe((true, "Available")), location_request: Some(request_factory(&requests)),
        location_resolve: Some(Arc::new(move |lat, lon| {
            s.set();
            r.wait(2.0);
            Ok(Location { name: "Current location".into(), latitude: Num::Float(lat), longitude: Num::Float(lon),
                          timezone: "Europe/Paris".into() })
        })), ..Services::default() });
    assert!(wait_for(|| widget.current_location.is_sensitive(), 3.0));
    widget.current_location.emit_clicked();
    requests.borrow().last().unwrap().call(Some((48.0, 2.0)), None);
    assert!(wait_for(|| started.is_set(), 1.0));
    widget.location_name.set_text("London");
    widget.latitude.set_value(51.5);
    widget.longitude.set_value(-0.12);
    widget.timezone.set_text("Europe/London");
    widget.manual_apply.emit_clicked();
    release.set();
    settle(0.1);
    assert_eq!(fx.store.load().name, "London");
    assert_eq!(fx.store.load().timezone, "Europe/London");
}

fn current_and_seven_day_forecast_units_stale_and_offline() {
    let fx = Fx::new();
    fx.save_denver();
    let popup = fx.popup("bottom");
    assert_eq!(popup.temperature.text(), "72°F");
    assert_eq!(popup.condition.text(), "Clear");
    assert_eq!(popup.day_rows.borrow().len(), 7);
    let days = popup.snapshot.borrow().as_ref().unwrap().days.clone();
    assert_eq!(days[0].date.weekday(), 6);
    assert_eq!(days[6].date.weekday(), 5);
    assert!(popup.forecast_title.text().contains("Sep 27–Oct 03"), "{}", popup.forecast_title.text());
    assert_ne!(fx.client.0.lock().unwrap().calls[0].2, std::thread::current().id());
    assert_eq!(popup.day_rows.borrow()[1].tooltip_text().unwrap(), "Mostly clear · Chance of precipitation: 10%");
    assert!(popup.details.text().contains("Humidity 45%") && popup.details.text().contains("Wind 8 mph"));
    let mut config = fx.store.load();
    config.name = "Paris".into();
    config.units = "celsius".into();
    config.week = "current".into();
    fx.store.save(&config).unwrap();
    popup.tick();
    assert!(wait_for(|| popup.snapshot.borrow().as_ref().is_some_and(|s| s.config.units == "celsius"), 3.0));
    assert_eq!(popup.temperature.text(), "22°C");
    assert_eq!(popup.location_label.text(), "Paris");
    {
        let mut state = fx.client.0.lock().unwrap();
        state.stale = true;
        state.error = Some("Offline fixture".into());
    }
    popup.refresh(true);
    assert!(wait_for(|| popup.snapshot.borrow().as_ref().is_some_and(|s| s.stale), 3.0));
    assert!(popup.message.text().contains("Showing saved weather"));
    assert!(popup.message.text().contains("Offline fixture"));
    assert_eq!(popup.temperature.text(), "22°C");
    fx.client.0.lock().unwrap().no_current = true;
    popup.refresh(true);
    assert!(wait_for(|| popup.snapshot.borrow().as_ref().is_some_and(|s| s.current.is_none()), 3.0));
    assert_eq!(popup.temperature.text(), "—");
    assert_eq!(popup.condition.text(), "Weather unavailable");
    assert_eq!(popup.day_rows.borrow().len(), 7);
}

fn pending_fetch_is_discarded_on_config_change_and_dismissal() {
    let fx = Fx::new();
    fx.save_denver();
    let (old_started, release_old) = (Gate::default(), Gate::default());
    let (s, r) = (old_started.clone(), release_old.clone());
    fx.client.set_read(Box::new(move |config, _| {
        if config.name == "Denver" {
            s.set();
            r.wait(4.0);
        }
        Ok(snapshot(config, false, None, true))
    }));
    fx.app.show_popup("bottom", 76);
    let popup = fx.app.popup().unwrap();
    assert!(wait_for(|| old_started.is_set(), 3.0));
    assert!(popup.message.text().contains("Loading"));
    let mut config = fx.store.load();
    config.name = "New location".into();
    fx.store.save(&config).unwrap();
    popup.tick();
    assert!(wait_for(|| popup.snapshot.borrow().is_some(), 3.0));
    assert_eq!(popup.snapshot.borrow().as_ref().unwrap().config.name, "New location");
    release_old.set();
    settle(0.1);
    assert_eq!(popup.snapshot.borrow().as_ref().unwrap().config.name, "New location");
    let (closing_started, release_closing) = (Gate::default(), Gate::default());
    let (s, r) = (closing_started.clone(), release_closing.clone());
    fx.client.set_read(Box::new(move |config, _| {
        s.set();
        r.wait(4.0);
        Ok(WeatherSnapshot { error: Some("This result must be discarded".into()), ..snapshot(config, false, None, true) })
    }));
    popup.refresh(true);
    assert!(wait_for(|| closing_started.is_set(), 3.0));
    popup.dismiss();
    assert!(wait_for(|| fx.app.popup().is_none(), 3.0));
    release_closing.set();
    settle(0.1);
    assert!(popup.closed.get());
    assert!(!popup.message.text().contains("discarded"));
    assert!(wait_for(|| fx.app.app.windows().is_empty(), 1.0));
}

fn no_location_prompts_setup_without_fetching() {
    let fx = Fx::new();
    fx.app.show_popup("bottom", 76);
    let popup = fx.app.popup().unwrap();
    assert!(wait_for(|| popup.setup_card.is_visible(), 3.0));
    assert!(!popup.current_card.is_visible());
    assert!(!popup.forecast_scroll.is_visible());
    assert_eq!(popup.location_label.text(), "No location chosen");
    assert_eq!(fx.client.calls(), 0);
    let locate = popup.locate_button.clone().expect("locate button");
    locate.emit_clicked();
    assert!(wait_for(|| fx.app.popup().is_none(), 3.0));
    // The route opened Weather Settings and asked it to locate once available.
    let settings = fx.app.settings.borrow().clone().expect("settings window");
    settings.window.destroy();
    settle(0.05);
    fx.app.show_popup("bottom", 76);
    let popup = fx.app.popup().unwrap();
    assert!(wait_for(|| popup.setup_card.is_visible(), 3.0));
    fx.save_denver();
    popup.tick();
    assert!(wait_for(|| popup.snapshot.borrow().is_some(), 3.0));
    assert!(!popup.setup_card.is_visible());
    assert!(popup.current_card.is_visible());
    assert_eq!(popup.location_label.text(), "Denver");
}

struct NoHost;
impl TrayHost for NoHost {
    fn show_popup(&self, _: &str, _: i32) {}
    fn show_settings(&self) {}
    fn quit(&self) {}
}

fn tray_schedules_one_refresh_and_follows_config_file() {
    let fx = Fx::new();
    let cache_dir = fx.root.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    fx.client.0.lock().unwrap().cache_dir = Some(cache_dir.clone());
    let host: Rc<dyn TrayHost> = Rc::new(NoHost);
    let tray = WeatherTray::new(Rc::downgrade(&host), fx.app.app.dbus_connection().unwrap(), fx.store.clone(),
                                fx.client.clone(), "top", 40);
    settle(0.2);
    assert_eq!(fx.client.calls(), 0);
    assert!(tray.0.timer.borrow().is_none(), "No location means nothing to refresh");
    fx.save_denver();
    assert!(wait_for(|| tray.0.snapshot.borrow().is_some(), 3.0));
    assert_eq!(fx.client.calls(), 1);
    assert!(tray.0.timer.borrow().is_some());
    assert!(tray.0.title.borrow().contains("Denver"));
    settle(0.5);
    assert_eq!(fx.client.calls(), 1, "No polling between scheduled refreshes");
    std::fs::write(cache_dir.join("Denver.json"), "{}").unwrap();
    assert!(wait_for(|| fx.client.calls() == 2, 3.0));
    std::fs::remove_file(&fx.store.path).unwrap();
    assert!(wait_for(|| tray.0.config.borrow().is_none(), 3.0));
    assert!(tray.0.timer.borrow().is_none());
    tray.close();
}

fn settings_locate_request_waits_for_availability() {
    let fx = Fx::new();
    let requests: Requests = Rc::default();
    let widget = fx.settings(Services {
        location_probe: Some(Arc::new(|| { std::thread::sleep(Duration::from_millis(100)); (true, "Available".into()) })),
        location_request: Some(request_factory(&requests)), ..Services::default() });
    assert_eq!(widget.location_label.text(), "Choose a location");
    assert!(!widget.units.is_sensitive());
    widget.locate_when_ready();
    assert!(requests.borrow().is_empty());
    assert!(wait_for(|| requests.borrow().len() == 1, 3.0));
    assert!(widget.locating.get());
}

fn location_search_selection_preferences_and_manual_validation() {
    let fx = Fx::new();
    let places = vec![Location { name: "Paris, France".into(), latitude: Num::Float(48.8566), longitude: Num::Float(2.3522),
                                 timezone: "Europe/Paris".into() }];
    let calls: Arc<Mutex<Vec<(String, std::thread::ThreadId)>>> = Arc::default();
    let (seen, results) = (calls.clone(), places.clone());
    let settings = fx.settings(Services { search: Some(Arc::new(move |query: &str| {
        seen.lock().unwrap().push((query.into(), std::thread::current().id()));
        Ok(results.clone())
    })), location_probe: probe((false, "x")), ..Services::default() });
    settings.location_search.set_text("Paris");
    settings.location_search.emit_by_name::<()>("activate", &[]);
    assert!(wait_for(|| settings.results.first_child().is_some(), 3.0));
    // A delayed SearchEntry search-changed must not cancel a fast Enter.
    settle(0.25);
    assert!(settings.results.first_child().is_some());
    assert!(fx.store.configured().is_none());
    let row = settings.results.row_at_index(0).unwrap();
    settings.results.emit_by_name::<()>("row-activated", &[&row]);
    assert_eq!(fx.store.load().name, places[0].name);
    assert_eq!(fx.store.load().timezone, "Europe/Paris");
    assert_ne!(calls.lock().unwrap()[0].1, std::thread::current().id());
    settings.units.set_selected(1);
    settings.week.set_selected(1);
    settings.refresh_minutes.set_value(30.0);
    let result = fx.store.load();
    assert_eq!((result.units.as_str(), result.week.as_str(), result.refresh_minutes), ("celsius", "current", 30));
    settings.location_name.set_text("Manual place");
    settings.latitude.set_value(1.2345);
    settings.longitude.set_value(-2.3456);
    settings.timezone.set_text("Invalid/timezone");
    settings.manual_apply.emit_clicked();
    assert_eq!(fx.store.load(), result);
    assert!(settings.status.text().contains("Could not save"));
    settings.timezone.set_text("UTC");
    settings.manual_apply.emit_clicked();
    assert_eq!(fx.store.load().name, "Manual place");
    assert!((fx.store.load().latitude.value() - 1.2345).abs() < 1e-7);
    let keys: Vec<&str> = settings.search_targets().keys().copied().collect();
    assert_eq!(keys, ["current_location", "forecast", "location", "refresh", "units"]);
}

fn location_search_ignores_old_query_and_closed_page() {
    let fx = Fx::new();
    let (first_started, first_release) = (Gate::default(), Gate::default());
    let (close_started, close_release) = (Gate::default(), Gate::default());
    let gates = (first_started.clone(), first_release.clone(), close_started.clone(), close_release.clone());
    let settings = fx.settings(Services { search: Some(Arc::new(move |query: &str| {
        if query == "first" {
            gates.0.set();
            gates.1.wait(4.0);
        }
        if query == "close" {
            gates.2.set();
            gates.3.wait(4.0);
        }
        Ok(vec![Location { name: query.into(), latitude: Num::Int(1), longitude: Num::Int(2), timezone: "UTC".into() }])
    })), location_probe: probe((false, "x")), ..Services::default() });
    settings.location_search.set_text("first");
    settings.search();
    assert!(wait_for(|| first_started.is_set(), 3.0));
    settings.location_search.set_text("second");
    settings.search();
    assert!(wait_for(|| settings.results.first_child().is_some(), 3.0));
    first_release.set();
    settle(0.1);
    let row = settings.results.row_at_index(0).unwrap();
    assert_eq!(settings.location_of(&row).unwrap().name, "second");
    settings.location_search.set_text("close");
    settings.search();
    assert!(wait_for(|| close_started.is_set(), 3.0));
    settings.close();
    close_release.set();
    settle(0.1);
    assert!(settings.results.first_child().is_none());
    assert!(fx.store.configured().is_none());
}

fn wlrctl(args: &[&str]) {
    let status = Command::new("wlrctl").args(args).status().expect("wlrctl");
    assert!(status.success());
}

fn real_click_away_escape_and_popup_settings_route() {
    let fx = Fx::new();
    fx.save_denver();
    let popup = fx.popup("top");
    assert!(popup.window.is_mapped());
    settle(0.2);
    wlrctl(&["pointer", "move", "-100000", "-100000"]);
    wlrctl(&["pointer", "move", "40", "600"]);
    settle(0.08);
    wlrctl(&["pointer", "click", "left"]);
    assert!(wait_for(|| fx.app.popup().is_none(), 3.0), "click-away closes the popup");
    let popup = fx.popup("left");
    popup.window.present();
    settle(0.1);
    // Exercise the actual window key controller used by native Escape.
    let controllers = popup.window.observe_controllers();
    let mut found = false;
    for index in 0..controllers.n_items() {
        if let Some(keys) = controllers.item(index).and_downcast::<gtk::EventControllerKey>() {
            found |= keys.emit_by_name::<bool>("key-pressed", &[&glib::translate::IntoGlib::into_glib(gdk::Key::Escape), &0u32, &gdk::ModifierType::empty()]);
        }
    }
    assert!(found);
    assert!(wait_for(|| fx.app.popup().is_none(), 3.0));
    let popup = fx.popup("right");
    popup.settings_button.emit_clicked();
    assert!(wait_for(|| fx.app.popup().is_none(), 3.0));
    let settings = fx.app.settings.borrow().clone().unwrap();
    assert!(wait_for(|| settings.window.is_mapped(), 3.0));
    assert_eq!(settings.window.title().unwrap(), "Weather Settings");
    fx.app.show_settings(false);
    assert!(Rc::ptr_eq(fx.app.settings.borrow().as_ref().unwrap(), &settings));
    settings.window.close();
    assert!(wait_for(|| fx.app.settings.borrow().is_none(), 3.0));
    fx.app.show_settings(false);
    assert!(!Rc::ptr_eq(fx.app.settings.borrow().as_ref().unwrap(), &settings));
}

fn singleton_cli_and_shared_settings() {
    let fx = Fx::with_id(Some(APPLICATION_ID));
    fx.save_denver();
    let popup = fx.popup("bottom");
    let binary = env!("CARGO_BIN_EXE_weather-manager");
    let mut child = Command::new(binary).args(["--popup", "--popup-edge", "top", "--popup-offset", "35"]).spawn().unwrap();
    let mut status = None;
    assert!(wait_for(|| { status = child.try_wait().unwrap(); status.is_some() }, 5.0));
    assert!(status.unwrap().success());
    assert!(Rc::ptr_eq(&fx.app.popup().unwrap(), &popup));
    assert_eq!((popup.controller().edge(), popup.controller().offset()), ("top".to_owned(), 35));
    let mut child = Command::new(binary).arg("--settings").spawn().unwrap();
    let mut status = None;
    assert!(wait_for(|| { status = child.try_wait().unwrap(); status.is_some() }, 5.0));
    assert!(status.unwrap().success());
    assert!(wait_for(|| fx.app.popup().is_none(), 3.0));
    let settings = fx.app.settings.borrow().clone().expect("settings");
    settings.settings.units.set_selected(1);
    assert_eq!(fx.store.load().units, "celsius");
    fx.app.show_settings(false);
    assert!(Rc::ptr_eq(fx.app.settings.borrow().as_ref().unwrap(), &settings));
    // Bad arguments are reported by the launcher without reaching the primary.
    let output = Command::new(binary).args(["--popup-offset", "-1"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).ends_with("error: --popup-offset must be nonnegative\n"));
    // Global Settings embeds the same widget and configuration.
    let page = fx.settings(Services::default());
    assert_eq!(page.units.selected(), 1);
    assert_eq!(page.search_targets()["units"], page.units.clone().upcast::<gtk::Widget>());
}

fn css_parses() {
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(Cell::new(0));
    let e = errors.clone();
    provider.connect_parsing_error(move |_, _, _| e.set(e.get() + 1));
    provider.load_from_string(weather_manager::ui::CSS);
    assert_eq!(errors.get(), 0);
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var_os("WEATHER_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/weather-manager/tests/run_headless.sh for the private compositor)");
        return;
    }
    suite_core::i18n::initialize();
    gtk::init().expect("display");
    let tests: [(&str, fn()); 12] = [
        ("css_parses", css_parses),
        ("current_location_success_denial_and_manual_selection", current_location_success_denial_and_manual_selection),
        ("location_unavailable_and_slow_result_do_not_override_manual_choice", location_unavailable_and_slow_result_do_not_override_manual_choice),
        ("current_and_seven_day_forecast_units_stale_and_offline", current_and_seven_day_forecast_units_stale_and_offline),
        ("pending_fetch_is_discarded_on_config_change_and_dismissal", pending_fetch_is_discarded_on_config_change_and_dismissal),
        ("no_location_prompts_setup_without_fetching", no_location_prompts_setup_without_fetching),
        ("tray_schedules_one_refresh_and_follows_config_file", tray_schedules_one_refresh_and_follows_config_file),
        ("settings_locate_request_waits_for_availability", settings_locate_request_waits_for_availability),
        ("location_search_selection_preferences_and_manual_validation", location_search_selection_preferences_and_manual_validation),
        ("location_search_ignores_old_query_and_closed_page", location_search_ignores_old_query_and_closed_page),
        ("real_click_away_escape_and_popup_settings_route", real_click_away_escape_and_popup_settings_route),
        ("singleton_cli_and_shared_settings", singleton_cli_and_shared_settings),
    ];
    let only = std::env::args().nth(1);
    for (name, test) in tests {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        println!("test {name} ...");
        test();
        println!("test {name} ok");
    }
    println!("ui_headless: all checks passed");
}
