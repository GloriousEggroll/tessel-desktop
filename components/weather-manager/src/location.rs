//! weather_location.py: one-shot, city-accuracy location through the desktop
//! permission portal. Availability checks never start positioning; no
//! IP-lookup fallback or background tracking.

use crate::backend::{urlencode, validate_config, Fetch, Location, Num, WeatherConfig, FORECAST_URL};
use glib::variant::ToVariant;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub const PATH: &str = "/org/freedesktop/portal/desktop";
pub const INTERFACE: &str = "org.freedesktop.portal.Location";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const DBUS: &str = "org.freedesktop.DBus";

/// Worker-only, bounded read-only checks of the service and its provider.
pub fn available() -> (bool, String) {
    let check = || -> Result<bool, glib::Error> {
        let system = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE)?;
        let mut names: Vec<String> = Vec::new();
        for method in ["ListNames", "ListActivatableNames"] {
            let reply = system.call_sync(Some(DBUS), "/org/freedesktop/DBus", DBUS, method, None,
                                         Some(glib::VariantTy::new("(as)").unwrap()), gio::DBusCallFlags::NO_AUTO_START,
                                         1500, gio::Cancellable::NONE)?;
            names.extend(reply.child_value(0).get::<Vec<String>>().unwrap_or_default());
        }
        if !names.iter().any(|n| n == "org.freedesktop.GeoClue2") {
            return Ok(false);
        }
        let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
        bus.call_sync(Some(PORTAL), PATH, PROPERTIES, "Get", Some(&(INTERFACE, "version").to_variant()), None,
                      gio::DBusCallFlags::NONE, 2000, gio::Cancellable::NONE)?;
        Ok(true)
    };
    match check() {
        Ok(true) => (true, "Use your approximate location once. The system may ask for permission.".into()),
        Ok(false) => (false, "System location services are not installed. You can still choose a city manually.".into()),
        Err(_) => (false, "System location services are unavailable. You can still choose a city manually.".into()),
    }
}

fn round2(value: f64) -> f64 {
    // Python round(x, 2): correctly rounded half-even on the decimal value.
    let text = format!("{value:.2}");
    text.parse().unwrap_or(value)
}

/// Validated, rounded (2 dp) coordinates from portal values.
pub fn coordinates(latitude: Option<f64>, longitude: Option<f64>) -> Result<(f64, f64), String> {
    match (latitude, longitude) {
        (Some(lat), Some(lon)) if lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon) => Ok((round2(lat), round2(lon))),
        // Weather does not need street-level coordinates, even if a provider sends them.
        _ => Err("The location service returned invalid coordinates.".into()),
    }
}

/// `Latitude`/`Longitude` of a portal `a{sv}`: only int or double values count.
pub fn coordinates_from(values: &glib::VariantDict) -> Result<(f64, f64), String> {
    let number = |key: &str| -> Option<f64> {
        let value = values.lookup_value(key, None)?;
        value.get::<f64>().or_else(|| value.get::<i32>().map(f64::from)).or_else(|| value.get::<i64>().map(|v| v as f64))
            .or_else(|| value.get::<u32>().map(f64::from))
    };
    coordinates(number("Latitude"), number("Longitude"))
}

/// Resolve the detected location's time zone with the weather provider.
pub fn weather_location(latitude: f64, longitude: f64, fetch: &Fetch) -> Result<Location, String> {
    let (latitude, longitude) = coordinates(Some(latitude), Some(longitude))?;
    let query = urlencode(&[("latitude", Num::Float(latitude).text()), ("longitude", Num::Float(longitude).text()),
                            ("timezone", "auto".into()), ("current", "temperature_2m".into()), ("forecast_days", "1".into())]);
    let data = fetch(&format!("{FORECAST_URL}?{query}")).map_err(|e| e.0)?;
    let timezone = data.get("timezone").and_then(|t| t.as_str()).unwrap_or("").to_owned();
    let config = WeatherConfig { name: "Current location".into(), latitude: latitude.into(), longitude: longitude.into(),
                                 timezone: timezone.clone(), ..WeatherConfig::default() };
    let config = validate_config(&config, true).map_err(|e| e.0)?;
    Ok(Location { name: config.name, latitude: latitude.into(), longitude: longitude.into(), timezone: config.timezone })
}

pub type LocationCallback = Box<dyn FnOnce(Option<(f64, f64)>, Option<String>)>;

/// The pending request object handed to the settings widget (fakeable in tests).
pub trait PendingLocation {
    fn cancel(&self);
}

struct Inner {
    callback: RefCell<Option<LocationCallback>>,
    bus: RefCell<Option<gio::DBusConnection>>,
    done: Cell<bool>,
    session: RefCell<Option<String>>,
    request: RefCell<Option<String>>,
    subscriptions: RefCell<Vec<gio::SignalSubscriptionId>>,
    authorized: Cell<bool>,
    position: Cell<Option<(f64, f64)>>,
    timer: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct LocationRequest(Rc<Inner>);

fn token() -> String {
    let uuid = glib::uuid_string_random().replace('-', "");
    format!("weather_{uuid}")
}

fn sender_path(bus: &gio::DBusConnection) -> String {
    bus.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default()
}

impl LocationRequest {
    pub fn new(callback: LocationCallback, connection: Option<gio::DBusConnection>, timeout: Duration) -> Self {
        let request = Self(Rc::new(Inner {
            callback: RefCell::new(Some(callback)), bus: RefCell::new(None), done: Cell::new(false),
            session: RefCell::new(None), request: RefCell::new(None), subscriptions: RefCell::new(Vec::new()),
            authorized: Cell::new(false), position: Cell::new(None), timer: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&request.0);
        let timer = glib::timeout_add_local_once(timeout, move || {
            if let Some(this) = up(&weak) {
                this.0.timer.replace(None);
                this.timeout();
            }
        });
        request.0.timer.replace(Some(timer));
        match connection {
            Some(bus) => {
                request.0.bus.replace(Some(bus));
                request.create();
            }
            None => {
                let weak = Rc::downgrade(&request.0);
                gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |result| {
                    let Some(this) = up(&weak) else { return };
                    match result {
                        Ok(bus) => {
                            this.0.bus.replace(Some(bus));
                            if !this.0.done.get() {
                                this.create();
                            }
                        }
                        Err(_) => this.finish(None, Some("The desktop location service could not be reached.".into()), true),
                    }
                });
            }
        }
        request
    }

    pub fn done(&self) -> bool { self.0.done.get() }
    pub fn position(&self) -> Option<(f64, f64)> { self.0.position.get() }

    fn bus(&self) -> Option<gio::DBusConnection> { self.0.bus.borrow().clone() }

    fn subscribe(&self, interface: &str, signal: &str, path: Option<&str>, handler: fn(&LocationRequest, &str, &Variant)) {
        let Some(bus) = self.bus() else { return };
        let weak = Rc::downgrade(&self.0);
        #[allow(deprecated)]
        let id = bus.signal_subscribe(Some(PORTAL), Some(interface), Some(signal), path, None, gio::DBusSignalFlags::NONE,
            move |_, _, path, _, _, parameters| if let Some(this) = up(&weak) { handler(&this, path, parameters) });
        self.0.subscriptions.borrow_mut().push(id);
    }

    fn create(&self) {
        let Some(bus) = self.bus() else { return };
        let token = token();
        self.0.session.replace(Some(format!("{PATH}/session/{}/{token}", sender_path(&bus))));
        self.subscribe(INTERFACE, "LocationUpdated", Some(PATH), |this, _, parameters| this.location(parameters));
        self.subscribe("org.freedesktop.portal.Session", "Closed", None, |this, path, _| this.closed(path));
        let options = glib::VariantDict::new(None);
        options.insert_value("session_handle_token", &token.to_variant());
        options.insert_value("accuracy", &2u32.to_variant());
        let weak = Rc::downgrade(&self.0);
        bus.call(Some(PORTAL), PATH, INTERFACE, "CreateSession", Some(&Variant::tuple_from_iter([options.end()])),
                 Some(glib::VariantTy::new("(o)").unwrap()), gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE,
                 move |result| if let Some(this) = up(&weak) { this.created(result) });
    }

    fn created(&self, result: Result<Variant, glib::Error>) {
        let session = match result {
            Ok(reply) => reply.child_value(0).str().map(String::from),
            Err(_) => None,
        };
        let Some(session) = session else {
            self.finish(None, Some("Location services are unavailable or disabled. Choose a city manually or enable system location services.".into()), true);
            return;
        };
        self.0.session.replace(Some(session.clone()));
        if self.0.done.get() {
            self.close_handle(Some(&session), "Session");
            return;
        }
        let Some(bus) = self.bus() else { return };
        let token = token();
        self.0.request.replace(Some(format!("{PATH}/request/{}/{token}", sender_path(&bus))));
        self.subscribe("org.freedesktop.portal.Request", "Response", None, |this, path, parameters| this.response(path, parameters));
        let options = glib::VariantDict::new(None);
        options.insert_value("handle_token", &token.to_variant());
        let session_path = glib::variant::ObjectPath::try_from(session).expect("portal object path");
        let parameters = Variant::tuple_from_iter([session_path.to_variant(), "".to_variant(), options.end()]);
        let weak = Rc::downgrade(&self.0);
        bus.call(Some(PORTAL), PATH, INTERFACE, "Start", Some(&parameters), Some(glib::VariantTy::new("(o)").unwrap()),
                 gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE, move |result| {
            let Some(this) = up(&weak) else { return };
            match result {
                Ok(reply) => {
                    let request = reply.child_value(0).str().map(String::from);
                    this.0.request.replace(request.clone());
                    if this.0.done.get() {
                        this.close_handle(request.as_deref(), "Request");
                    }
                }
                Err(_) => this.finish(None, Some("Location access was denied or the service is unavailable. Your selected location was not changed.".into()), true),
            }
        });
    }

    fn response(&self, path: &str, parameters: &Variant) {
        if self.0.done.get() || self.0.request.borrow().as_deref() != Some(path) {
            return;
        }
        let code = parameters.child_value(0).get::<u32>().unwrap_or(2);
        if code != 0 {
            self.finish(None, Some("Location access was cancelled or denied. Your selected location was not changed.".into()), true);
        } else {
            self.0.authorized.set(true);
            if let Some(position) = self.0.position.get() {
                self.finish(Some(position), None, true);
            }
        }
    }

    fn location(&self, parameters: &Variant) {
        if parameters.n_children() != 2 {
            return;
        }
        let session = parameters.child_value(0).str().map(String::from);
        if self.0.done.get() || session != *self.0.session.borrow() {
            return;
        }
        let values = glib::VariantDict::new(Some(&parameters.child_value(1)));
        match coordinates_from(&values) {
            Ok(position) => {
                self.0.position.set(Some(position));
                if self.0.authorized.get() {
                    self.finish(Some(position), None, true);
                }
            }
            Err(error) => self.finish(None, Some(error), true),
        }
    }

    fn closed(&self, path: &str) {
        if self.0.session.borrow().as_deref() == Some(path) && !self.0.done.get() {
            self.finish(None, Some("The location session was closed. Your selected location was not changed.".into()), true);
        }
    }

    /// The 45 s timeout (public for tests, like Python's `_timeout`).
    pub fn timeout(&self) {
        if let Some(timer) = self.0.timer.take() {
            timer.remove();
        }
        self.finish(None, Some("A current location could not be found. Check system location services or choose a city manually.".into()), true);
    }

    fn close_handle(&self, path: Option<&str>, interface: &str) {
        let (Some(bus), Some(path)) = (self.bus(), path) else { return };
        bus.call(Some(PORTAL), path, &format!("org.freedesktop.portal.{interface}"), "Close", None, None,
                 gio::DBusCallFlags::NO_AUTO_START, 1500, gio::Cancellable::NONE, |_| {});
    }

    fn finish(&self, position: Option<(f64, f64)>, error: Option<String>, notify: bool) {
        if self.0.done.get() {
            return;
        }
        self.0.done.set(true);
        if let Some(timer) = self.0.timer.take() {
            timer.remove();
        }
        if let Some(bus) = self.bus() {
            for id in self.0.subscriptions.borrow_mut().drain(..) {
                #[allow(deprecated)]
                bus.signal_unsubscribe(id);
            }
        }
        let request = self.0.request.borrow().clone();
        let session = self.0.session.borrow().clone();
        self.close_handle(request.as_deref(), "Request");
        self.close_handle(session.as_deref(), "Session");
        let callback = self.0.callback.take();
        if notify {
            if let Some(callback) = callback {
                callback(position, error);
            }
        }
    }
}

impl PendingLocation for LocationRequest {
    fn cancel(&self) { self.finish(None, None, false); }
}

fn up(weak: &Weak<Inner>) -> Option<LocationRequest> { weak.upgrade().map(LocationRequest) }

/// Keep a reference per request so callbacks fire even when the caller only keeps the trait object.
pub fn new_request(callback: LocationCallback) -> Rc<dyn PendingLocation> {
    Rc::new(LocationRequest::new(callback, None, Duration::from_secs(45)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn coordinates_are_validated_and_rounded() {
        assert_eq!(coordinates(Some(48.85837), Some(2.294481)), Ok((48.86, 2.29)));
        for (lat, lon) in [(None, None), (Some(91.0), Some(2.0)), (Some(1.0), Some(f64::NAN)), (Some(1.0), Some(-181.0))] {
            assert!(coordinates(lat, lon).is_err());
        }
        // Booleans are not numbers (Python checked `type(...) in (int, float)`).
        let values = glib::VariantDict::new(None);
        values.insert_value("Latitude", &true.to_variant());
        values.insert_value("Longitude", &0.0f64.to_variant());
        assert!(coordinates_from(&values).is_err());
    }

    #[test]
    fn timezone_comes_from_detected_location_not_existing_city() {
        let urls: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = urls.clone();
        let fetch: Fetch = Arc::new(move |url: &str| {
            seen.lock().unwrap().push(url.into());
            Ok(json!({"timezone": "Europe/Paris"}))
        });
        let place = weather_location(48.85837, 2.294481, &fetch).unwrap();
        assert_eq!(place.timezone, "Europe/Paris");
        assert_eq!((place.latitude, place.longitude), (Num::Float(48.86), Num::Float(2.29)));
        assert_eq!(urls.lock().unwrap()[0],
                   "https://api.open-meteo.com/v1/forecast?latitude=48.86&longitude=2.29&timezone=auto&current=temperature_2m&forecast_days=1");
        let invalid: Fetch = Arc::new(|_: &str| Ok(json!({"timezone": "Invalid/Zone"})));
        assert!(weather_location(48.8, 2.2, &invalid).is_err());
    }
}
