//! Port of test_weather_location.py PortalTests (a FAKE portal on a private
//! `gio::TestDBus`; never the real position), the SNI/dbusmenu tray contract
//! over that bus, and the `tessel-weather-status` binary against a pre-filled shared
//! cache (no network: the cache entry is fresh).


use glib::variant::ToVariant;
use glib::Variant;
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tessel_weather::backend::{system_clock, ConfigStore, WeatherClient, WeatherConfig};
use tessel_weather::dates;
use tessel_weather::location::{LocationRequest, INTERFACE, PATH, PORTAL};
use tessel_weather::tray::{TrayHost, WeatherTray, ITEM, ITEM_PATH, MENU, MENU_PATH};
use tessel_weather::ui::WeatherSource;

const STATUS: &str = env!("CARGO_BIN_EXE_tessel-weather-status");

fn spin(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    predicate()
}

const XML: &str = r#"<node><interface name="org.freedesktop.portal.Location">
<method name="CreateSession"><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
<method name="Start"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
<signal name="LocationUpdated"><arg type="o"/><arg type="a{sv}"/></signal>
</interface></node>"#;

fn connection(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, gio::Cancellable::NONE).unwrap()
}

#[derive(Default)]
struct PortalState {
    closes: Vec<(String, String)>,
    delay_create: bool,
    pending_create: Option<gio::DBusMethodInvocation>,
    started: bool,
    session: Option<String>,
    request: Option<String>,
    accuracy: Option<u32>,
    registrations: Vec<gio::RegistrationId>,
}

struct Portal {
    server: gio::DBusConnection,
    client: gio::DBusConnection,
    state: Rc<RefCell<PortalState>>,
    results: Rc<RefCell<Vec<(Option<(f64, f64)>, Option<String>)>>>,
    locator: RefCell<Option<LocationRequest>>,
}

fn register_handle(server: &gio::DBusConnection, state: &Rc<RefCell<PortalState>>, path: &str, interface: &str) {
    let xml = format!("<node><interface name=\"org.freedesktop.portal.{interface}\"><method name=\"Close\"/></interface></node>");
    let info = gio::DBusNodeInfo::for_xml(&xml).unwrap();
    let interface_info = info.interfaces()[0].clone();
    let (weak, name) = (Rc::downgrade(state), interface.to_owned());
    let id = server.register_object(path, &interface_info).method_call(move |_, _, path, _, _, _, invocation| {
        if let Some(state) = weak.upgrade() {
            state.borrow_mut().closes.push((path.to_owned(), name.clone()));
        }
        invocation.return_value(None);
    }).build().unwrap();
    state.borrow_mut().registrations.push(id);
}

impl Portal {
    fn new(address: &str) -> Self {
        let (server, client) = (connection(address), connection(address));
        let state: Rc<RefCell<PortalState>> = Rc::default();
        let info = gio::DBusNodeInfo::for_xml(XML).unwrap();
        let (weak, srv) = (Rc::downgrade(&state), server.clone());
        let id = server.register_object(PATH, &info.interfaces()[0]).method_call(move |_, sender, _, _, method, args, invocation| {
            let Some(state) = weak.upgrade() else { return };
            let sender = sender.unwrap_or("").trim_start_matches(':').replace('.', "_");
            if method == "CreateSession" {
                let options = glib::VariantDict::new(Some(&args.child_value(0)));
                let token: String = options.lookup_value("session_handle_token", None).unwrap().get().unwrap();
                let session = format!("{PATH}/session/{sender}/{token}");
                {
                    let mut s = state.borrow_mut();
                    s.accuracy = options.lookup_value("accuracy", None).and_then(|v| v.get());
                    s.session = Some(session.clone());
                }
                if state.borrow().delay_create {
                    state.borrow_mut().pending_create = Some(invocation);
                } else {
                    register_handle(&srv, &state, &session, "Session");
                    invocation.return_value(Some(&(glib::variant::ObjectPath::try_from(session).unwrap(),).to_variant()));
                }
            } else if method == "Start" {
                let session = args.child_value(0).str().unwrap().to_owned();
                assert_eq!(Some(session), state.borrow().session.clone());
                let options = glib::VariantDict::new(Some(&args.child_value(2)));
                let token: String = options.lookup_value("handle_token", None).unwrap().get().unwrap();
                let request = format!("{PATH}/request/{sender}/{token}");
                register_handle(&srv, &state, &request, "Request");
                state.borrow_mut().request = Some(request.clone());
                invocation.return_value(Some(&(glib::variant::ObjectPath::try_from(request).unwrap(),).to_variant()));
                state.borrow_mut().started = true;
            }
        }).build().unwrap();
        state.borrow_mut().registrations.push(id);
        server.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                         Some(&(PORTAL, 0u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, gio::Cancellable::NONE).unwrap();
        Self { server, client, state, results: Rc::default(), locator: RefCell::new(None) }
    }

    fn locate(&self) {
        let results = self.results.clone();
        let locator = LocationRequest::new(Box::new(move |point, error| results.borrow_mut().push((point, error))),
                                           Some(self.client.clone()), Duration::from_secs(45));
        self.locator.replace(Some(locator));
    }

    fn finish_create(&self) {
        let session = self.state.borrow().session.clone().unwrap();
        register_handle(&self.server, &self.state, &session, "Session");
        let invocation = self.state.borrow_mut().pending_create.take().unwrap();
        invocation.return_value(Some(&(glib::variant::ObjectPath::try_from(session).unwrap(),).to_variant()));
    }

    fn position(&self, session: Option<&str>, latitude: f64) {
        let session = session.map(String::from).unwrap_or_else(|| self.state.borrow().session.clone().unwrap());
        let values = glib::VariantDict::new(None);
        values.insert_value("Latitude", &latitude.to_variant());
        values.insert_value("Longitude", &2.294481f64.to_variant());
        let parameters = Variant::tuple_from_iter([glib::variant::ObjectPath::try_from(session).unwrap().to_variant(), values.end()]);
        self.server.emit_signal(None, PATH, INTERFACE, "LocationUpdated", Some(&parameters)).unwrap();
    }

    fn response(&self, code: u32) {
        let request = self.state.borrow().request.clone().unwrap();
        let parameters = Variant::tuple_from_iter([code.to_variant(), glib::VariantDict::new(None).end()]);
        self.server.emit_signal(None, &request, "org.freedesktop.portal.Request", "Response", Some(&parameters)).unwrap();
    }

    fn closed(&self, path: Option<String>, interface: &str) -> bool {
        let path = path.unwrap_or_default();
        self.state.borrow().closes.iter().any(|(p, i)| *p == path && i == interface)
    }
    fn session(&self) -> Option<String> { self.state.borrow().session.clone() }
    fn request(&self) -> Option<String> { self.state.borrow().request.clone() }
    fn started(&self) -> bool { self.state.borrow().started }
}

impl Drop for Portal {
    fn drop(&mut self) {
        if let Some(locator) = self.locator.take() {
            use tessel_weather::location::PendingLocation;
            locator.cancel();
        }
        spin(|| false, 0.02);
        for id in self.state.borrow_mut().registrations.drain(..) {
            let _ = self.server.unregister_object(id);
        }
        let _ = self.client.close_sync(gio::Cancellable::NONE);
        let _ = self.server.close_sync(gio::Cancellable::NONE);
    }
}

fn position_requires_permission_and_session_is_closed_after_result(address: &str) {
    let portal = Portal::new(address);
    portal.locate();
    assert!(spin(|| portal.started(), 2.0));
    assert_eq!(portal.state.borrow().accuracy, Some(2));
    portal.position(Some("/unrelated"), 48.85837);
    portal.position(None, 48.85837);
    assert!(spin(|| portal.locator.borrow().as_ref().unwrap().position().is_some(), 2.0));
    assert!(portal.results.borrow().is_empty());
    portal.response(0);
    assert!(spin(|| !portal.results.borrow().is_empty(), 2.0));
    assert_eq!(*portal.results.borrow(), vec![(Some((48.86, 2.29)), None)]);
    assert!(spin(|| portal.closed(portal.session(), "Session"), 2.0));
    portal.position(None, 48.85837);
    spin(|| false, 0.03);
    assert_eq!(portal.results.borrow().len(), 1);
}

fn denial_closes_session(address: &str) {
    let portal = Portal::new(address);
    portal.locate();
    assert!(spin(|| portal.started(), 2.0));
    portal.response(2);
    assert!(spin(|| !portal.results.borrow().is_empty(), 2.0));
    assert!(portal.results.borrow()[0].0.is_none());
    assert!(portal.results.borrow()[0].1.as_ref().unwrap().contains("denied"));
    assert!(spin(|| portal.closed(portal.session(), "Session"), 2.0));
}

fn timeout_closes_pending_request(address: &str) {
    let portal = Portal::new(address);
    portal.locate();
    assert!(spin(|| portal.started(), 2.0));
    portal.locator.borrow().as_ref().unwrap().timeout();
    assert!(portal.results.borrow()[0].0.is_none());
    assert!(portal.results.borrow()[0].1.as_ref().unwrap().contains("could not be found"));
    assert!(spin(|| portal.closed(portal.request(), "Request"), 2.0));
}

fn cancel_during_create_closes_late_session_without_callback(address: &str) {
    use tessel_weather::location::PendingLocation;
    let portal = Portal::new(address);
    portal.state.borrow_mut().delay_create = true;
    portal.locate();
    assert!(spin(|| portal.state.borrow().pending_create.is_some(), 2.0));
    portal.locator.borrow().as_ref().unwrap().cancel();
    portal.finish_create();
    assert!(spin(|| portal.closed(portal.session(), "Session"), 2.0));
    assert!(portal.results.borrow().is_empty());
    assert!(!portal.started());
}

fn invalid_location_does_not_return_a_position(address: &str) {
    let portal = Portal::new(address);
    portal.locate();
    assert!(spin(|| portal.started(), 2.0));
    portal.response(0);
    portal.position(None, f64::NAN);
    assert!(spin(|| !portal.results.borrow().is_empty(), 2.0));
    assert!(portal.results.borrow()[0].0.is_none());
    assert!(spin(|| portal.closed(portal.session(), "Session"), 2.0));
}

#[derive(Default)]
struct Host(RefCell<Vec<String>>);
impl TrayHost for Host {
    fn show_popup(&self, edge: &str, offset: i32) { self.0.borrow_mut().push(format!("popup {edge} {offset}")); }
    fn show_settings(&self) { self.0.borrow_mut().push("settings".into()); }
    fn quit(&self) { self.0.borrow_mut().push("quit".into()); }
}

struct NoNetwork(WeatherClient);
impl WeatherSource for NoNetwork {
    fn read(&self, config: &WeatherConfig, force: bool) -> Result<tessel_weather::backend::WeatherSnapshot, String> {
        WeatherSource::read(&self.0, config, force)
    }
    fn cache_path(&self, config: &WeatherConfig) -> Option<PathBuf> { Some(self.0.cache_path(config)) }
    fn cache_dir(&self) -> Option<PathBuf> { Some(self.0.cache_dir.clone()) }
}

fn call(bus: &gio::DBusConnection, name: &str, path: &str, interface: &str, method: &str, args: Option<Variant>)
        -> Result<Variant, glib::Error> {
    let result = Rc::new(RefCell::new(None));
    let slot = result.clone();
    bus.call(Some(name), path, interface, method, args.as_ref(), None, gio::DBusCallFlags::NONE, 2000, gio::Cancellable::NONE,
             move |reply| { slot.replace(Some(reply)); });
    assert!(spin(|| result.borrow().is_some(), 3.0));
    result.take().unwrap()
}

fn tray_exports_sni_and_dbusmenu(address: &str, root: &Path) {
    let service = connection(address);
    let caller = connection(address);
    let owner = service.unique_name().unwrap().to_string();
    let store = Arc::new(ConfigStore::new(Some(root.join("tray/config.json"))));
    let fetch: tessel_weather::backend::Fetch = Arc::new(|_: &str| Err(tessel_weather::backend::WeatherError("Offline".into())));
    let client = Arc::new(NoNetwork(WeatherClient::new(store.clone(), Some(root.join("tray/cache")), Some(fetch), None)));
    let host = Rc::new(Host::default());
    let dyn_host: Rc<dyn TrayHost> = host.clone();
    let tray = WeatherTray::new(Rc::downgrade(&dyn_host), service.clone(), store, client, "top", 40);
    let get = |interface: &str, name: &str| {
        call(&caller, &owner, if interface == MENU { MENU_PATH } else { ITEM_PATH }, "org.freedesktop.DBus.Properties", "Get",
             Some((interface, name).to_variant())).unwrap().child_value(0).as_variant().unwrap()
    };
    assert_eq!(get(ITEM, "Id").str(), Some("tessel-weather"));
    assert_eq!(get(ITEM, "Category").str(), Some("ApplicationStatus"));
    assert_eq!(get(ITEM, "Title").str(), Some("Weather"));
    assert_eq!(get(ITEM, "IconName").str(), Some("weather-few-clouds-symbolic"));
    assert_eq!(get(ITEM, "Menu").str(), Some("/Menu"));
    assert_eq!(get(ITEM, "ToolTip").type_().as_str(), "(sa(iiay)ss)");
    assert_eq!(get(ITEM, "ToolTip").child_value(3).str(), Some("Choose a location in Weather Settings"));
    assert_eq!(get(MENU, "Version").get::<u32>(), Some(3));
    let layout = call(&caller, &owner, MENU_PATH, MENU, "GetLayout", Some((0i32, -1i32, Vec::<String>::new()).to_variant())).unwrap();
    assert_eq!(layout.type_().as_str(), "(u(ia{sv}av))");
    assert_eq!(layout.child_value(0).get::<u32>(), Some(1));
    let items = layout.child_value(1).child_value(2);
    assert_eq!(items.n_children(), 3);
    let first = items.child_value(0).as_variant().unwrap();
    let props = glib::VariantDict::new(Some(&first.child_value(1)));
    assert_eq!(props.lookup_value("label", None).unwrap().str(), Some("Weather"));
    assert_eq!(props.lookup_value("icon-name", None).unwrap().str(), Some("weather-few-clouds"));
    let filtered = call(&caller, &owner, MENU_PATH, MENU, "GetGroupProperties", Some((vec![2i32, 9], vec!["label".to_owned()]).to_variant())).unwrap();
    assert_eq!(filtered.child_value(0).n_children(), 1);
    let error = call(&caller, &owner, MENU_PATH, MENU, "GetProperty", Some((7i32, "label").to_variant())).unwrap_err();
    assert!(error.message().contains("InvalidArgs") || gio::DBusError::remote_error(&error).as_deref() == Some("org.freedesktop.DBus.Error.InvalidArgs"));
    let errors = call(&caller, &owner, MENU_PATH, MENU, "EventGroup",
                      Some((vec![(2i32, "clicked".to_owned(), 0i32.to_variant(), 0u32), (8, "clicked".into(), 0i32.to_variant(), 0)],).to_variant())).unwrap();
    assert_eq!(errors.child_value(0).get::<Vec<i32>>(), Some(vec![8]));
    call(&caller, &owner, ITEM_PATH, ITEM, "Activate", Some((0i32, 0i32).to_variant())).unwrap();
    assert_eq!(call(&caller, &owner, MENU_PATH, MENU, "AboutToShow", Some((1i32,).to_variant())).unwrap().get::<(bool,)>(), Some((false,)));
    let group = call(&caller, &owner, MENU_PATH, MENU, "AboutToShowGroup", Some((vec![1i32, 5],).to_variant())).unwrap();
    assert_eq!(group.get::<(Vec<i32>, Vec<i32>)>(), Some((vec![], vec![5])));
    assert!(spin(|| host.0.borrow().len() == 2, 2.0));
    assert_eq!(*host.0.borrow(), vec!["settings".to_owned(), "popup top 40".to_owned()]);
    tray.close();
}

fn tray_registers_with_watcher(address: &str, root: &Path) {
    // A fake StatusNotifierWatcher receives the object path only (sender = unique name).
    let watcher = connection(address);
    let registered: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let xml = r#"<node><interface name="org.kde.StatusNotifierWatcher"><method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method></interface></node>"#;
    let info = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let seen = registered.clone();
    let id = watcher.register_object("/StatusNotifierWatcher", &info.interfaces()[0]).method_call(move |_, sender, _, _, _, args, invocation| {
        seen.borrow_mut().push((sender.unwrap_or("").to_owned(), args.child_value(0).str().unwrap_or("").to_owned()));
        invocation.return_value(None);
    }).build().unwrap();
    watcher.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                      Some(&("org.kde.StatusNotifierWatcher", 0u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                      gio::Cancellable::NONE).unwrap();
    let service = connection(address);
    let store = Arc::new(ConfigStore::new(Some(root.join("watch/config.json"))));
    let client = Arc::new(NoNetwork(WeatherClient::new(store.clone(), Some(root.join("watch/cache")),
                                                       Some(Arc::new(|_: &str| panic!("no network"))), None)));
    let host: Rc<dyn TrayHost> = Rc::new(Host::default());
    let tray = WeatherTray::new(Rc::downgrade(&host), service.clone(), store, client, "top", 40);
    assert!(spin(|| !registered.borrow().is_empty(), 3.0));
    assert_eq!(registered.borrow()[0], (service.unique_name().unwrap().to_string(), "/StatusNotifierItem".to_owned()));
    tray.close();
    let _ = watcher.unregister_object(id);
}

fn write_fresh_cache(root: &Path) -> (PathBuf, PathBuf) {
    let config_path = root.join("status/config.json");
    let cache = root.join("status/cache");
    let store = Arc::new(ConfigStore::new(Some(config_path.clone())));
    let config = WeatherConfig::new("Denver", 39.7392, -104.9847, "America/Denver", "fahrenheit");
    store.save(&config).unwrap();
    let now = system_clock();
    let today = dates::local_date(now, "America/Denver").unwrap();
    let days: Vec<String> = (-7..16).map(|n| today.add_days(n).iso()).collect();
    let response = json!({"current": {"time": format!("{}T12:00", today.iso()), "temperature_2m": 72, "weather_code": 0, "is_day": 1,
                                      "apparent_temperature": 71, "relative_humidity_2m": 40, "wind_speed_10m": 8},
                          "daily": {"time": days, "temperature_2m_min": vec![1; 23], "temperature_2m_max": vec![2; 23],
                                    "weather_code": vec![3; 23], "precipitation_probability_max": vec![25; 23]}});
    let client = WeatherClient::new(store, Some(cache.clone()), Some(Arc::new(|_: &str| panic!("no network"))), None);
    let entry = json!({"version": 1, "config": config.to_json(), "response": response, "fetched_at": now, "attempted_at": now, "error": null});
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(client.cache_path(&config), entry.to_string()).unwrap();
    (config_path, cache)
}

fn status_binary_reads_shared_cache_and_reports_unconfigured(root: &Path) {
    let output = Command::new(STATUS).args(["--config", root.join("none/config.json").to_str().unwrap(), "--cache-dir",
                                            root.join("none/cache").to_str().unwrap()]).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "{\"text\": \"󰖐\", \"tooltip\": \"Weather\\nChoose a location in Weather Settings.\", \
        \"class\": [\"weather\", \"unconfigured\"], \"alt\": \"unconfigured\", \"icon\": \"weather-few-clouds-symbolic\"}\n");
    let (config, cache) = write_fresh_cache(root);
    let output = Command::new(STATUS).args(["--config", config.to_str().unwrap(), "--cache-dir", cache.to_str().unwrap()]).output().unwrap();
    let line = String::from_utf8(output.stdout).unwrap();
    assert!(line.starts_with("{\"text\": \"☀ 72°F\", \"tooltip\": \"Denver\\nClear · 72°F\\nFeels like 71°F\\nUpdated "), "{line}");
    assert!(line.ends_with("\"class\": [\"weather\"], \"alt\": \"Clear\", \"icon\": \"weather-clear-symbolic\"}\n"), "{line}");
    // A broken settings file is reported in the tooltip.
    std::fs::create_dir_all(root.join("none")).unwrap();
    std::fs::write(root.join("none/bad.json"), "{").unwrap();
    let output = Command::new(STATUS).args(["--config", root.join("none/bad.json").to_str().unwrap(), "--cache-dir",
                                            root.join("none/cache").to_str().unwrap()]).output().unwrap();
    assert!(String::from_utf8(output.stdout).unwrap().contains("\\nSettings could not be read: "));
    // --watch prints once, then only changes; SIGTERM exits 0.
    let child = Command::new(STATUS).args(["--watch", "--config", config.to_str().unwrap(), "--cache-dir", cache.to_str().unwrap()])
        .stdout(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    // SAFETY: signalling our own child.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);
    let help = Command::new(STATUS).arg("--bogus").output().unwrap();
    assert_eq!(help.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&help.stderr).ends_with("error: unrecognized arguments: --bogus\n"));
}

#[test]
fn private_bus() {
    let tmp = PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into())).join(format!("weather-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    assert_eq!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(), address, "TestDBus did not take over the session bus");
    // SAFETY: single test function; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };
    position_requires_permission_and_session_is_closed_after_result(&address);
    denial_closes_session(&address);
    timeout_closes_pending_request(&address);
    cancel_during_create_closes_late_session_without_callback(&address);
    invalid_location_does_not_return_a_position(&address);
    tray_exports_sni_and_dbusmenu(&address, &tmp);
    tray_registers_with_watcher(&address, &tmp);
    status_binary_reads_shared_cache_and_reports_unconfigured(&tmp);
    bus.down();
    let _ = std::fs::remove_dir_all(&tmp);
}
