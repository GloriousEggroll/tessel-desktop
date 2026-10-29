//! Port of test_weather_backend.py (fixture data only; never the network).

use crate::backend::*;
use crate::dates::{self, Date};
use crate::status::status_payload;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub fn fixture(today: Date, temperature: f64) -> Value {
    let days: Vec<String> = (-7..16).map(|n| today.add_days(n).iso()).collect();
    json!({"current": {"time": format!("{}T12:00", today.iso()), "temperature_2m": temperature,
                       "weather_code": 2, "is_day": 1, "apparent_temperature": temperature - 1.0,
                       "relative_humidity_2m": 40, "wind_speed_10m": 8},
           "daily": {"time": days, "temperature_2m_min": (0..23).collect::<Vec<i64>>(),
                     "temperature_2m_max": (20..43).collect::<Vec<i64>>(), "weather_code": vec![3; 23],
                     "precipitation_probability_max": vec![25; 23]}})
}

fn denver() -> WeatherConfig { WeatherConfig::new("Denver", 39.7392, -104.9847, "America/Denver", "fahrenheit") }

struct Fx {
    root: PathBuf,
    store: Arc<ConfigStore>,
    denver: Arc<ConfigStore>,
    now: Arc<Mutex<f64>>,
    response: Arc<Mutex<Result<Value, WeatherError>>>,
    calls: Arc<AtomicUsize>,
}

fn sept24() -> Date { Date::new(2026, 9, 24).unwrap() }

impl Fx {
    fn new(name: &str) -> Self {
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let root = Path::new(&base).join(format!("weather-backend-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(ConfigStore::new(Some(root.join("config/config.json"))));
        let denver_store = Arc::new(ConfigStore::new(Some(root.join("denver/config.json"))));
        denver_store.save(&denver()).unwrap();
        let now = dates::midnight(sept24(), "America/Denver").unwrap() + 12.0 * 3600.0;
        Self { root, store, denver: denver_store, now: Arc::new(Mutex::new(now)),
               response: Arc::new(Mutex::new(Ok(fixture(sept24(), 72.0)))), calls: Arc::default() }
    }

    fn fetch(&self) -> Fetch {
        let (response, calls) = (self.response.clone(), self.calls.clone());
        Arc::new(move |_url: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            response.lock().unwrap().clone()
        })
    }

    fn clock(&self) -> Clock {
        let now = self.now.clone();
        Arc::new(move || *now.lock().unwrap())
    }

    fn client(&self) -> WeatherClient {
        WeatherClient::new(self.denver.clone(), Some(self.root.join("cache")), Some(self.fetch()), Some(self.clock()))
    }

    fn advance(&self, seconds: f64) { *self.now.lock().unwrap() += seconds; }
    fn now(&self) -> f64 { *self.now.lock().unwrap() }
    fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }
}

impl Drop for Fx {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
}

fn read(client: &WeatherClient, config: Option<&WeatherConfig>) -> WeatherSnapshot { client.read(config, false).unwrap() }

#[test]
fn default_config_does_not_write_or_locate_user() {
    let fx = Fx::new("default");
    assert_eq!(fx.store.load(), WeatherConfig::default());
    assert!(!fx.store.path.exists());
    assert_eq!(fx.store.error(), None);
}

#[test]
fn configured_location_rejects_missing_partial_and_invalid_data() {
    let fx = Fx::new("configured");
    assert_eq!(fx.store.configured(), None);
    std::fs::create_dir_all(fx.store.path.parent().unwrap()).unwrap();
    for value in [json!({}), json!({"units": "celsius"}), json!({"name": "Paris"}),
                  json!({"name": "Paris", "latitude": 100, "longitude": 2}), json!({"name": "", "latitude": 48, "longitude": 2})] {
        std::fs::write(&fx.store.path, value.to_string()).unwrap();
        assert_eq!(fx.store.configured(), None, "{value}");
    }
    std::fs::write(&fx.store.path, "broken json").unwrap();
    assert_eq!(fx.store.configured(), None);
    let config = WeatherConfig::new("Paris", 48.85, 2.35, "Europe/Paris", &default_units());
    fx.store.save(&config).unwrap();
    assert_eq!(fx.store.configured(), Some(config));
}

#[test]
fn configuration_atomic_roundtrip_and_bad_config_keeps_defaults() {
    let fx = Fx::new("roundtrip");
    let config = WeatherConfig { refresh_minutes: 30, week: "current".into(),
                                 ..WeatherConfig::new("Paris", 48.85, 2.35, "Europe/Paris", "celsius") };
    fx.store.save(&config).unwrap();
    assert_eq!(fx.store.load(), config);
    // Byte-identical to Python's json.dump(asdict(config), ensure_ascii=False): no newline.
    assert_eq!(std::fs::read_to_string(&fx.store.path).unwrap(),
               r#"{"name": "Paris", "latitude": 48.85, "longitude": 2.35, "timezone": "Europe/Paris", "units": "celsius", "refresh_minutes": 30, "week": "current"}"#);
    let temporaries = std::fs::read_dir(fx.store.path.parent().unwrap()).unwrap().flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
    assert_eq!(temporaries, 0);
    std::fs::write(&fx.store.path, r#"{"units":"invalid"}"#).unwrap();
    assert_eq!(fx.store.load(), WeatherConfig::default());
    assert!(fx.store.error().unwrap().contains("Fahrenheit"));
    std::fs::write(&fx.store.path, " ".repeat(MAX_CONFIG_BYTES + 1)).unwrap();
    assert_eq!(fx.store.load(), WeatherConfig::default());
    assert!(fx.store.error().unwrap().contains("too large"));
    // Wrong types are reported with Python's messages.
    for (text, message) in [(r#"{"refresh_minutes": true}"#, "refresh interval"), (r#"{"refresh_minutes": 60.0}"#, "refresh interval"),
                            (r#"{"name": 5, "latitude": 1, "longitude": 2}"#, "location name"), ("[1]", "Invalid weather configuration."),
                            ("broken json", "Expecting value: line 1 column 1 (char 0)")] {
        std::fs::write(&fx.store.path, text).unwrap();
        fx.store.load();
        assert!(fx.store.error().unwrap().contains(message), "{text}: {:?}", fx.store.error());
    }
}

#[test]
fn no_default_city_and_units_follow_measurement_locale() {
    let fx = Fx::new("nodefault");
    let config = fx.store.load();
    assert!(!has_location(&config));
    assert_eq!(config.name, "");
    let client = WeatherClient::new(fx.store.clone(), Some(fx.root.join("cache")), Some(fx.fetch()), None);
    let snapshot = read(&client, None);
    assert!(snapshot.current.is_none());
    assert_eq!(snapshot.error.as_deref(), Some(NO_LOCATION));
    assert_eq!(fx.calls(), 0);
    let payload = status_payload(&snapshot);
    assert!(payload["class"].as_array().unwrap().contains(&json!("unconfigured")));
    assert!(!payload["tooltip"].as_str().unwrap().contains("Denver"));
    let cases: [(&[(&str, &str)], &str); 9] = [
        (&[("LANG", "en_US.UTF-8")], "fahrenheit"), (&[("LANG", "en_GB.UTF-8")], "celsius"),
        (&[("LANG", "en_US.UTF-8"), ("LC_MEASUREMENT", "de_DE.UTF-8")], "celsius"), (&[("LC_MEASUREMENT", "en_LR")], "fahrenheit"),
        (&[("LANG", "my_MM.UTF-8")], "fahrenheit"), (&[("LC_ALL", "C")], "celsius"), (&[], "celsius"),
        (&[("LANG", "sr_RS@latin")], "celsius"), (&[("LC_ALL", ""), ("LC_MEASUREMENT", "fr_FR.UTF-8")], "celsius")];
    for (environ, units) in cases {
        let lookup = |name: &str| environ.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned());
        assert_eq!(default_units_with(&lookup), units, "{environ:?}");
    }
}

#[test]
fn existing_configs_keep_their_values() {
    let fx = Fx::new("legacy");
    std::fs::create_dir_all(fx.store.path.parent().unwrap()).unwrap();
    std::fs::write(&fx.store.path, r#"{"name": "Denver", "latitude": 39.7392, "longitude": -104.9847}"#).unwrap();
    assert_eq!(fx.store.configured(), Some(denver()));
    std::fs::write(&fx.store.path, json!({"name": "Denver", "latitude": 39.7392, "longitude": -104.9847,
                                          "units": "celsius", "timezone": "Europe/Berlin"}).to_string()).unwrap();
    assert_eq!(fx.store.load().units, "celsius");
    assert_eq!(fx.store.load().timezone, "Europe/Berlin");
    std::fs::write(&fx.store.path, r#"{"week": "current"}"#).unwrap();
    assert_eq!(fx.store.configured(), None);
    assert_eq!(fx.store.load().week, "current");
    assert_eq!(fx.store.load().units, "fahrenheit");
    assert_eq!(fx.store.error(), None);
}

#[test]
fn refresh_delay_follows_ttl_retry_and_local_midnight() {
    let fx = Fx::new("delay");
    let snapshot = read(&fx.client(), None);
    let now = fx.now();
    assert_eq!(refresh_delay(&snapshot, now), Some(3600));
    assert_eq!(refresh_delay(&snapshot, now + 600.0), Some(3000));
    let late = dates::midnight(sept24(), "America/Denver").unwrap() + 23.0 * 3600.0 + 50.0 * 60.0;
    assert_eq!(refresh_delay(&WeatherSnapshot { fetched_at: Some(late), ..snapshot.clone() }, late), Some(601));
    assert_eq!(refresh_delay(&WeatherSnapshot { error: Some("Offline".into()), stale: true, ..snapshot.clone() }, now), Some(60));
    let empty = WeatherClient::new(fx.store.clone(), Some(fx.root.join("c2")), Some(fx.fetch()), None);
    assert_eq!(refresh_delay(&read(&empty, None), now), None);
}

#[test]
fn invalid_config_is_not_saved() {
    let fx = Fx::new("invalid");
    let base = denver();
    let cases: Vec<WeatherConfig> = vec![
        WeatherConfig { latitude: Num::Float(91.0), ..base.clone() }, WeatherConfig { longitude: Num::Float(-181.0), ..base.clone() },
        WeatherConfig { latitude: Num::Float(f64::NAN), ..base.clone() }, WeatherConfig { name: String::new(), ..base.clone() },
        WeatherConfig { timezone: "Bad/Zone".into(), ..base.clone() }, WeatherConfig { units: "kelvin".into(), ..base.clone() },
        WeatherConfig { refresh_minutes: 1, ..base.clone() }, WeatherConfig { week: "monday".into(), ..base.clone() },
        WeatherConfig { name: "Tab\there".into(), ..base.clone() }, WeatherConfig { name: "x".repeat(161), ..base.clone() }];
    for config in cases {
        assert!(fx.store.save(&config).is_err(), "{config:?}");
    }
    assert!(!fx.store.path.exists());
}

#[test]
fn sunday_first_current_and_upcoming_cross_month_year_boundaries() {
    let d = |y, m, day| Date::new(y, m, day).unwrap();
    let cases = [(d(2026, 9, 20), d(2026, 9, 20), d(2026, 9, 20)), (d(2026, 9, 24), d(2026, 9, 20), d(2026, 9, 27)),
                 (d(2026, 1, 31), d(2026, 1, 25), d(2026, 2, 1)), (d(2026, 12, 31), d(2026, 12, 27), d(2027, 1, 3))];
    for (today, current, upcoming) in cases {
        for (week, expected) in [("current", current), ("upcoming", upcoming)] {
            let days = week_dates(today, week).unwrap();
            assert_eq!(days, (0..7).map(|i| expected.add_days(i)).collect::<Vec<_>>());
            assert_eq!(days[0].weekday(), 6);
            assert_eq!(days[6].weekday(), 5);
        }
    }
    assert!(week_dates(d(2026, 9, 20), "monday").is_err());
}

#[test]
fn query_requests_sixteen_days_units_timezone_and_past_only_for_current() {
    for (units, wind) in [("fahrenheit", "mph"), ("celsius", "kmh")] {
        for week in ["upcoming", "current"] {
            let config = WeatherConfig { units: units.into(), week: week.into(), ..denver() };
            let url = forecast_url(&config).unwrap();
            let past = if week == "current" { "&past_days=7" } else { "" };
            assert_eq!(url, format!("https://api.open-meteo.com/v1/forecast?latitude=39.7392&longitude=-104.9847&timezone=America%2FDenver\
                &current=temperature_2m%2Cweather_code%2Cis_day%2Capparent_temperature%2Crelative_humidity_2m%2Cwind_speed_10m\
                &daily=temperature_2m_min%2Ctemperature_2m_max%2Cweather_code%2Cprecipitation_probability_max\
                &temperature_unit={units}&wind_speed_unit={wind}&forecast_days=16{past}"));
        }
    }
}

#[test]
fn forecast_has_real_sunday_saturday_dates_and_labels_past_days() {
    let fx = Fx::new("dates");
    let config = WeatherConfig { week: "current".into(), ..denver() };
    let snapshot = read(&fx.client(), Some(&config));
    let current = snapshot.current.as_ref().unwrap();
    assert_eq!((current.temp, current.humidity, current.wind), (72.0, Some(40.0), Some(8.0)));
    assert_eq!(snapshot.days.iter().map(|d| d.date).collect::<Vec<_>>(), week_dates(sept24(), "current").unwrap());
    assert_eq!(snapshot.days.iter().map(|d| d.past).collect::<Vec<_>>(), [true, true, true, true, false, false, false]);
    assert_eq!((snapshot.days[0].temp_min, snapshot.days[0].temp_max), (Some(3.0), Some(23.0)));
    assert!(!snapshot.stale);
}

#[test]
fn forecast_week_uses_location_date_not_computer_timezone() {
    let fx = Fx::new("zone");
    *fx.now.lock().unwrap() = dates::midnight(Date::new(2026, 9, 27).unwrap(), "UTC").unwrap() + 3600.0;
    let client = fx.client();
    let snapshot = read(&client, Some(&WeatherConfig { timezone: "Pacific/Kiritimati".into(), week: "current".into(), ..denver() }));
    assert_eq!(snapshot.days[0].date, Date::new(2026, 9, 27).unwrap());
    let snapshot = read(&client, Some(&WeatherConfig { week: "current".into(), ..denver() }));
    assert_eq!(snapshot.days[0].date, Date::new(2026, 9, 20).unwrap());
}

#[test]
fn missing_daily_values_are_unavailable_not_zero_or_wrong_day() {
    let fx = Fx::new("missing");
    let mut data = fixture(sept24(), 72.0);
    data["daily"]["temperature_2m_min"][10] = Value::Null;
    data["daily"]["weather_code"][10] = Value::Null;
    *fx.response.lock().unwrap() = Ok(data);
    let snapshot = read(&fx.client(), None);
    assert_eq!(snapshot.days[0].temp_min, None);
    assert_eq!(snapshot.days[0].code, None);
    assert_eq!(snapshot.days[0].date, Date::new(2026, 9, 27).unwrap());
    assert_eq!(format_temperature(None, "fahrenheit"), "—");
}

#[test]
fn shared_cache_ttl_config_changes_and_manual_refresh() {
    let fx = Fx::new("shared");
    let first = read(&fx.client(), None);
    let second = fx.client();
    assert_eq!(read(&second, None), first);
    assert_eq!(fx.calls(), 1);
    fx.advance(1.0);
    second.read(None, true).unwrap();
    assert_eq!(fx.calls(), 2);
    read(&second, Some(&WeatherConfig { units: "celsius".into(), ..denver() }));
    assert_eq!(fx.calls(), 3);
    fx.advance(3601.0);
    read(&second, None);
    assert_eq!(fx.calls(), 4);
}

#[test]
fn offline_keeps_last_good_data_marks_stale_and_backs_off() {
    let fx = Fx::new("offline");
    let client = fx.client();
    read(&client, None);
    fx.advance(3601.0);
    *fx.response.lock().unwrap() = Err(WeatherError("Offline".into()));
    let snapshot = read(&client, None);
    assert_eq!(snapshot.current.as_ref().unwrap().temp, 72.0);
    assert!(snapshot.stale);
    assert_eq!(snapshot.error.as_deref(), Some("Offline"));
    read(&client, None);
    assert_eq!(fx.calls(), 2);
    fx.advance(61.0);
    read(&client, None);
    assert_eq!(fx.calls(), 3);
    let payload = status_payload(&snapshot);
    assert!(payload["class"].as_array().unwrap().contains(&json!("stale")));
    assert!(payload["tooltip"].as_str().unwrap().contains("Out of date"));
}

#[test]
fn failed_initial_request_is_unavailable_and_does_not_retry_every_poll() {
    let fx = Fx::new("failed");
    *fx.response.lock().unwrap() = Err(WeatherError("Offline".into()));
    let client = fx.client();
    let snapshot = read(&client, None);
    assert!(snapshot.current.is_none());
    assert_eq!(snapshot.days.len(), 7);
    read(&client, None);
    assert_eq!(fx.calls(), 1);
    assert!(status_payload(&snapshot)["class"].as_array().unwrap().contains(&json!("unavailable")));
    // Python's entry order for a first failure: version, config, attempted_at, error.
    let text = std::fs::read_to_string(client.cache_path(&denver())).unwrap();
    assert!(text.starts_with(r#"{"version": 1, "config": {"name": "Denver", "latitude": 39.7392"#), "{text}");
    assert!(text.ends_with(r#""error": "Offline"}"#), "{text}");
}

#[test]
fn malformed_response_preserves_last_good_forecast() {
    let fx = Fx::new("malformed");
    let client = fx.client();
    let first = read(&client, None);
    fx.advance(3601.0);
    *fx.response.lock().unwrap() = Ok(json!({"current": {"temperature_2m": null}, "daily": {}}));
    let snapshot = read(&client, None);
    assert_eq!(snapshot.current, first.current);
    assert!(snapshot.stale);
    assert!(snapshot.error.unwrap().contains("incomplete"));
}

#[test]
fn cache_file_count_is_bounded_and_other_files_untouched() {
    let fx = Fx::new("bounded");
    let client = fx.client();
    std::fs::create_dir_all(&client.cache_dir).unwrap();
    let other = client.cache_dir.join("other.json");
    std::fs::write(&other, "keep").unwrap();
    for index in 0..MAX_CACHE_FILES + 3 {
        read(&client, Some(&WeatherConfig { name: format!("Location {index}"), ..denver() }));
    }
    let files = std::fs::read_dir(&client.cache_dir).unwrap().flatten()
        .filter(|e| e.file_name().to_string_lossy().strip_suffix(".json").is_some_and(|s| s.len() == 64)).count();
    assert_eq!(files, MAX_CACHE_FILES);
    assert_eq!(std::fs::read_to_string(other).unwrap(), "keep");
}

#[test]
fn concurrent_clients_share_single_network_request() {
    let fx = Fx::new("concurrent");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let fetch: Fetch = {
        let (release, calls) = (release.clone(), calls.clone());
        let entered = Mutex::new(entered_tx);
        Arc::new(move |_url: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            let _ = entered.lock().unwrap().send(());
            let (lock, signal) = &*release;
            let guard = lock.lock().unwrap();
            let _ = signal.wait_timeout_while(guard, std::time::Duration::from_secs(2), |released| !*released).unwrap();
            Ok(fixture(sept24(), 72.0))
        })
    };
    let make = || Arc::new(WeatherClient::new(fx.denver.clone(), Some(fx.root.join("cache")), Some(fetch.clone()), Some(fx.clock())));
    let (first, second) = (make(), make());
    let a = std::thread::spawn(move || first.read(None, false).unwrap());
    entered_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    let b = std::thread::spawn(move || second.read(None, false).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(50));
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let results = [a.join().unwrap(), b.join().unwrap()];
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(results.iter().all(|r| r.current.as_ref().unwrap().temp == 72.0));
}

#[test]
fn geocoding_is_explicit_bounded_and_returns_distinguishable_locations() {
    let urls: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = urls.clone();
    let fetch: Fetch = Arc::new(move |url: &str| {
        seen.lock().unwrap().push(url.into());
        Ok(json!({"results": [
            {"name": "Paris", "admin1": "Île-de-France", "country": "France", "latitude": 48.85, "longitude": 2.35, "timezone": "Europe/Paris"},
            {"name": "Paris", "admin1": "Texas", "country": "United States", "latitude": 33.66, "longitude": -95.55, "timezone": "America/Chicago"},
            {"name": "Invalid", "latitude": 1000, "longitude": 0, "timezone": "UTC"},
            {"name": "Monaco", "admin1": "Monaco", "country": "Monaco", "latitude": 43.73, "longitude": 7.42, "timezone": "Europe/Monaco"}]}))
    });
    assert_eq!(search_locations("p", &fetch).unwrap(), vec![]);
    assert!(urls.lock().unwrap().is_empty());
    let locations = search_locations("Paris", &fetch).unwrap();
    assert_eq!(locations.len(), 3);
    assert_eq!(locations[0].name, "Paris, Île-de-France, France");
    assert_eq!(locations[1].timezone, "America/Chicago");
    assert_eq!(locations[2].name, "Monaco");
    assert_eq!(urls.lock().unwrap()[0], "https://geocoding-api.open-meteo.com/v1/search?name=Paris&count=10&language=en&format=json");
    assert!(search_locations(&"x".repeat(101), &fetch).is_err());
}

#[test]
fn status_has_units_day_night_icon_and_weather_description() {
    assert_eq!(weather_icon(Some(0), false), "weather-clear-night-symbolic");
    assert_eq!(weather_description(Some(95)), "Thunderstorms");
    assert_eq!(format_temperature(Some(19.6), "celsius"), "20°C");
    assert_eq!(format_temperature(Some(-3.6), "fahrenheit"), "-4°F");
    assert_eq!(format_temperature(Some(0.5), "celsius"), "0°C");
    assert_eq!(format_temperature(Some(2.5), "celsius"), "2°C");
    let fx = Fx::new("status");
    let payload = status_payload(&read(&fx.client(), None));
    assert!(payload["text"].as_str().unwrap().contains("72°F"));
    assert!(payload["tooltip"].as_str().unwrap().contains("Denver"));
    assert_eq!(payload["icon"], "weather-few-clouds-symbolic");
    assert_eq!(crate::status::line(&payload), "{\"text\": \"⛅ 72°F\", \"tooltip\": \"Denver\\nPartly cloudy · 72°F\\nFeels like 71°F\\n\
        Updated Thu 12:00\", \"class\": [\"weather\"], \"alt\": \"Partly cloudy\", \"icon\": \"weather-few-clouds-symbolic\"}");
}

#[test]
fn http_response_is_bounded_and_rejects_nonfinite_json() {
    for body in [vec![b'x'; MAX_RESPONSE_BYTES + 1], b"{\"temperature\":NaN}".to_vec(), b"not JSON".to_vec(), b"[1]".to_vec(),
                 b"{\"error\": true, \"reason\": \"x\"}".to_vec()] {
        assert!(decode_fetched(&body).is_err());
    }
    assert!(decode_fetched(b"{\"error\": false}").is_ok());
}

#[test]
fn corrupt_cache_timestamps_trigger_valid_refresh() {
    let fx = Fx::new("corrupt");
    let client = fx.client();
    read(&client, None);
    let path = client.cache_path(&denver());
    let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["fetched_at"] = json!(1e100);
    std::fs::write(&path, value.to_string()).unwrap();
    assert_eq!(read(&client, None).current.unwrap().temp, 72.0);
    assert_eq!(fx.calls(), 2);
}

#[test]
fn cache_key_and_file_match_python() {
    // hashlib.sha256(json.dumps(asdict(config), sort_keys=True).encode()).hexdigest() — the first is
    // the name of a live Python-written cache file, so Rust and Python share entries.
    let config = WeatherConfig::new("Englewood, Colorado, United States", 39.64777, -104.98776, "America/Denver", "fahrenheit");
    assert_eq!(WeatherClient::key(&config), "03a3f2585baeea74abc6a72fe2363bc4f7cdbf8793502819332bf64fd1274622");
    let ints = WeatherConfig { latitude: Num::Int(48), longitude: Num::Int(2),
                               ..WeatherConfig::new("Île", 0.0, 0.0, "Europe/Paris", "celsius") };
    assert_eq!(WeatherClient::key(&ints), "b0a669e2791465499e13aa72eeddc75bc28c20aff17691f9f2d85bcea7c878e8");
}

#[test]
fn recorded_open_meteo_response_decodes() {
    // fixtures/open-meteo-forecast.json: a real Open-Meteo reply (recorded once,
    // 2026-10-29 03:00 America/Denver, upcoming week, 16 days).
    let response: Value = serde_json::from_str(include_str!("../fixtures/open-meteo-forecast.json")).unwrap();
    let now = 1793264413.8515923;
    let snapshot = decode_response(&response, &denver(), now, Some(now), false, None).unwrap();
    let current = snapshot.current.unwrap();
    assert_eq!((current.temp, current.code, current.is_day), (40.9, 0, false));
    assert_eq!(snapshot.days.len(), 7);
    assert_eq!(snapshot.days[0].date, Date::new(2026, 11, 1).unwrap());
    assert!(snapshot.days.iter().all(|d| d.code.is_some() && d.temp_max.is_some()));
}
