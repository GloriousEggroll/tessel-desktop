//! lock_weather.configured_weather: the Weather app's explicitly saved location.
use std::io::Read;

use serde_json::Value;
use weather_manager::backend::{ConfigStore, WeatherConfig};

/// `configured_weather(store=None)`: the saved location, or `None`.
pub fn configured_weather(store: Option<&ConfigStore>) -> Option<WeatherConfig> {
    let owned;
    let store = match store {
        Some(store) => store,
        None => {
            owned = ConfigStore::new(None);
            &owned
        }
    };
    let mut file = std::fs::File::open(&store.path).ok()?;
    // stream.read(8193): at most 8193 characters.
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let text: String = text.chars().take(8193).collect();
    let raw: Value = serde_json::from_str(&text).ok()?;
    let raw = raw.as_object()?;
    if !["name", "latitude", "longitude"].iter().all(|key| raw.contains_key(*key)) {
        return None;
    }
    let config = store.load();
    (store.error().is_none() && !suite_core::pyjson::py_strip(&config.name).is_empty()).then_some(config)
}
