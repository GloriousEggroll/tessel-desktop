//! `tessel-lock/lock_weather.py`: optional lock weather using the Weather
//! app's saved location and shared cache (tessel-weather's backend).
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use serde_json::Value;
pub use tessel_weather::backend::{ConfigStore as WeatherStore, WeatherClient, WeatherConfig, WeatherSnapshot};

/// `configured_weather(store=None)`: the explicitly saved, valid location, or `None`.
pub fn configured_weather(store: Option<&WeatherStore>) -> Option<WeatherConfig> {
    let owned;
    let store = match store {
        Some(store) => store,
        None => {
            owned = WeatherStore::new(None);
            &owned
        }
    };
    let mut bytes = Vec::new();
    std::fs::File::open(&store.path).ok()?.read_to_end(&mut bytes).ok()?;
    // stream.read(8193): at most 8193 characters.
    let text: String = String::from_utf8(bytes).ok()?.chars().take(8193).collect();
    let raw: Value = serde_json::from_str(&text).ok()?;
    let raw = raw.as_object()?;
    if !["name", "latitude", "longitude"].iter().all(|key| raw.contains_key(*key)) {
        return None;
    }
    let config = store.load();
    (store.error().is_none() && !tessel_core::pyjson::py_strip(&config.name).is_empty()).then_some(config)
}

/// The `client.read(config)` the feed uses (a fake in tests).
pub trait WeatherReader: Send + Sync {
    fn read(&self, config: &WeatherConfig) -> Option<WeatherSnapshot>;
}

impl WeatherReader for WeatherClient {
    fn read(&self, config: &WeatherConfig) -> Option<WeatherSnapshot> { WeatherClient::read(self, Some(config), false).ok() }
}

struct Inner {
    changed: RefCell<Rc<dyn Fn()>>,
    store: Arc<WeatherStore>,
    client: Arc<dyn WeatherReader>,
    config: RefCell<Option<WeatherConfig>>,
    snapshot: RefCell<Option<WeatherSnapshot>>,
    closed: Cell<bool>,
    pending: Cell<bool>,
    generation: Cell<u64>,
    next_read: Cell<f64>,
}

/// `LockWeather(changed, store=None, client=None)`. Reads run on a worker
/// thread; results for an older location (generation) are discarded; a
/// successful read is reused for 60 s.
#[derive(Clone)]
pub struct LockWeather(Rc<Inner>);

impl LockWeather {
    pub fn new(changed: impl Fn() + 'static, store: Option<Arc<WeatherStore>>, client: Option<Arc<dyn WeatherReader>>) -> Self {
        let store = store.unwrap_or_else(|| Arc::new(WeatherStore::new(None)));
        let client = client.unwrap_or_else(|| Arc::new(WeatherClient::new(store.clone(), None, None, None)));
        Self(Rc::new(Inner {
            changed: RefCell::new(Rc::new(changed)), store, client, config: RefCell::new(None), snapshot: RefCell::new(None),
            closed: Cell::new(false), pending: Cell::new(false), generation: Cell::new(0), next_read: Cell::new(0.0),
        }))
    }

    /// Replace the change callback (the owner usually builds its window after the feed).
    pub fn set_changed(&self, changed: impl Fn() + 'static) {
        if !self.0.closed.get() {
            self.0.changed.replace(Rc::new(changed));
        }
    }

    pub fn config(&self) -> Option<WeatherConfig> { self.0.config.borrow().clone() }
    pub fn snapshot(&self) -> Option<WeatherSnapshot> { self.0.snapshot.borrow().clone() }
    pub fn pending(&self) -> bool { self.0.pending.get() }
    pub fn closed(&self) -> bool { self.0.closed.get() }

    fn changed(&self) {
        let callback = self.0.changed.borrow().clone();
        callback();
    }

    pub fn poll(&self, enabled: bool) {
        let inner = &self.0;
        if inner.closed.get() {
            return;
        }
        let config = if enabled { configured_weather(Some(&inner.store)) } else { None };
        if config != *inner.config.borrow() {
            inner.config.replace(config.clone());
            inner.snapshot.replace(None);
            inner.generation.set(inner.generation.get() + 1);
            inner.next_read.set(0.0);
            self.changed();
        }
        let Some(config) = config else { return };
        if inner.closed.get() || inner.pending.get() || crate::voice_state::monotonic() < inner.next_read.get() {
            return;
        }
        inner.pending.set(true);
        let generation = inner.generation.get();
        let client = inner.client.clone();
        // Weather failure must never interfere with the secure surface.
        let handle = gio::spawn_blocking(move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| client.read(&config))).ok().flatten());
        let weak: Weak<Inner> = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let snapshot = handle.await.ok().flatten();
            if let Some(inner) = weak.upgrade() {
                LockWeather(inner).ready(generation, snapshot);
            }
        });
    }

    fn ready(&self, generation: u64, snapshot: Option<WeatherSnapshot>) {
        let inner = &self.0;
        inner.pending.set(false);
        if !inner.closed.get() && generation == inner.generation.get() {
            inner.snapshot.replace(snapshot);
            inner.next_read.set(crate::voice_state::monotonic() + 60.0);
            self.changed();
        }
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.changed.replace(Rc::new(|| {}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_context(f: impl FnOnce()) {
        let context = glib::MainContext::new();
        context.with_thread_default(f).unwrap();
    }
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;
    use tessel_weather::backend::{CurrentWeather, ForecastDay};
    use tessel_weather::dates::Date;

    fn denver() -> WeatherConfig { WeatherConfig::new("Denver", 39.7392, -104.9903, "America/Denver", "fahrenheit") }

    pub fn snapshot(config: &WeatherConfig) -> WeatherSnapshot {
        let sunday = Date::new(2026, 9, 27).unwrap();
        WeatherSnapshot {
            config: config.clone(),
            current: Some(CurrentWeather { time: "2026-09-24T12:00".into(), temp: 72.0, code: 2, is_day: true, apparent: None, humidity: None, wind: None }),
            days: (0..7).map(|i| ForecastDay { date: sunday.add_days(i), temp_min: Some(55.0 + i as f64), temp_max: Some(72.0 + i as f64),
                                               code: Some(2), precip_probability: None, past: false }).collect(),
            fetched_at: None, stale: false, error: None,
        }
    }

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tessel-locking-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settle() { tessel_core::timer::settle(&glib::MainContext::ref_thread_default(), Duration::from_millis(150)); }

    #[test]
    fn only_valid_explicit_location_enables_weather() {
        let dir = folder("weather1");
        let store = WeatherStore::new(Some(dir.join("weather.json")));
        assert!(configured_weather(Some(&store)).is_none());
        std::fs::write(&store.path, "{}").unwrap();
        assert!(configured_weather(Some(&store)).is_none());
        store.save(&denver()).unwrap();
        assert_eq!(configured_weather(Some(&store)), Some(denver()));
        std::fs::write(&store.path, r#"{"name":"Broken", "latitude":999, "longitude":0}"#).unwrap();
        assert!(configured_weather(Some(&store)).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    struct Client {
        calls: Mutex<Vec<WeatherConfig>>,
        gate: Mutex<(bool, bool)>, // (entered, finish)
        signal: Condvar,
    }

    impl WeatherReader for Client {
        fn read(&self, config: &WeatherConfig) -> Option<WeatherSnapshot> {
            self.calls.lock().unwrap().push(config.clone());
            let mut gate = self.gate.lock().unwrap();
            gate.0 = true;
            self.signal.notify_all();
            let _ = self.signal.wait_timeout_while(gate, Duration::from_secs(3), |g| !g.1).unwrap();
            Some(snapshot(config))
        }
    }

    #[test]
    fn disabled_weather_does_not_fetch_and_inflight_old_location_is_discarded() {
        in_context(|| {
            let dir = folder("weather2");
            let store = Arc::new(WeatherStore::new(Some(dir.join("weather.json"))));
            store.save(&denver()).unwrap();
            let client = Arc::new(Client { calls: Mutex::new(Vec::new()), gate: Mutex::new((false, false)), signal: Condvar::new() });
            let feed = LockWeather::new(|| {}, Some(store.clone()), Some(client.clone()));
            feed.poll(false);
            assert!(client.calls.lock().unwrap().is_empty());
            feed.poll(true);
            {
                let gate = client.gate.lock().unwrap();
                let (gate, _) = client.signal.wait_timeout_while(gate, Duration::from_secs(1), |g| !g.0).unwrap();
                assert!(gate.0);
            }
            let mut another = denver();
            another.name = "Another location".into();
            another.latitude = 45.0.into();
            store.save(&another).unwrap();
            feed.poll(true);
            {
                client.gate.lock().unwrap().1 = true;
                client.signal.notify_all();
            }
            settle();
            assert!(feed.snapshot().is_none());
            feed.poll(true);
            settle();
            assert_eq!(feed.snapshot().unwrap().config.name, "Another location");
            feed.poll(false);
            assert!(feed.snapshot().is_none());
            assert!(feed.config().is_none());
            feed.close();
            feed.poll(true);
            assert_eq!(client.calls.lock().unwrap().len(), 2);
            let _ = std::fs::remove_dir_all(dir);
        });
    }
}
