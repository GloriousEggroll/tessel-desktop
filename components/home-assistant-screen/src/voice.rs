//! `voice_controls.py`: small local voice-command backend. No LLM, Home
//! Assistant call, or shell interpolation.
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Map, Value};
use suite_lock::weather::{WeatherClient, WeatherStore};
use weather_manager::backend::weather_description;

use crate::levels::{change_volume, resolve_level, run_timeout};

/// The `luna-clock` program next to this one (Python: `ROOT/'luna_clock.py'`).
pub fn clock_program() -> PathBuf {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("luna-clock"))).unwrap_or_else(|| "luna-clock".into())
}

fn object(value: Value) -> Map<String, Value> {
    match value { Value::Object(map) => map, _ => Map::new() }
}

pub fn brightness(action: &str) -> Result<Map<String, Value>, String> {
    // Avoid launching an unmanaged GTK instance if the dashboard service is down.
    run_timeout(&["systemctl", "--user", "is-active", "--quiet", "luna-clock"], Duration::from_secs(3), false)?;
    let program = clock_program();
    let program = program.to_string_lossy();
    let status = || -> Result<i64, String> {
        let text = run_timeout(&[&program, "--status"], Duration::from_secs(4), true)?;
        let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        value.get("levels").and_then(|l| l.get("screen_brightness")).and_then(Value::as_i64).ok_or_else(|| "KeyError".to_string())
    };
    let target = resolve_level(status()?, action, 10)?;
    run_timeout(&[&program, "--brightness", &target.to_string()], Duration::from_secs(4), true)?;
    let actual = status()?;
    if actual != target {
        return Err("Screen brightness did not match requested level".into());
    }
    Ok(object(json!({"success": true, "value": actual, "response": format!("Screen brightness is {actual} percent.")})))
}

pub fn weather(store: &WeatherStore, client: &WeatherClient) -> Result<Map<String, Value>, String> {
    // Never speak the library's fallback city as though it were user configured.
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&store.path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if !raw.as_object().is_some_and(|m| ["name", "latitude", "longitude"].iter().all(|k| m.contains_key(*k))) {
        return Err("Weather location is not configured".into());
    }
    let config = store.load();
    if store.error().is_some() {
        return Err("Weather location could not be read".into());
    }
    let snapshot = client.read(Some(&config), false).map_err(|e| e.0)?;
    let current = match snapshot.current {
        Some(current) if !snapshot.stale => current,
        _ => return Ok(object(json!({"success": false, "response": "I could not get a current weather report just now."}))),
    };
    let description = weather_description(Some(current.code)).to_lowercase();
    let city = config.name.split(',').next().unwrap_or_default();
    let mut response = format!("In {city}, it is {} degrees {}", current.temp.round_ties_even() as i64, config.units);
    if description != "unavailable" {
        response.push_str(&format!(" and {description}"));
    }
    if let Some(apparent) = current.apparent {
        response.push_str(&format!(", feeling like {} degrees", apparent.round_ties_even() as i64));
    }
    Ok(object(json!({"success": true, "response": format!("{response}."), "source": "Open-Meteo", "observed_at": current.time})))
}

pub fn execute(kind: &str, action: &str) -> Result<Map<String, Value>, String> {
    match (kind, action) {
        ("brightness", _) => brightness(action),
        ("volume", _) => {
            let result = change_volume(action)?;
            let response = if result.muted { "Volume is muted.".to_owned() } else { format!("Volume is {} percent.", result.value) };
            Ok(object(json!({"success": true, "response": response, "sink": result.sink, "value": result.value, "muted": result.muted})))
        }
        ("weather", "current") => {
            let store = std::sync::Arc::new(WeatherStore::new(None));
            let client = WeatherClient::new(store.clone(), None, None, None);
            weather(&store, &client)
        }
        _ => Err("Unknown local command".into()),
    }
}

/// `__main__`: always one JSON line, exit status 0.
pub fn main_with(args: &[String]) -> String {
    let result = if args.len() != 2 { Err("Expected command and action".to_string()) } else { execute(&args[0], &args[1]) };
    let result = result.unwrap_or_else(|_| {
        let response = if args.first().map(String::as_str) == Some("weather") { "I could not check the weather just now." }
                       else { "I could not change that setting just now." };
        object(json!({"success": false, "response": response}))
    });
    suite_core::pyfmt::json_dumps(&Value::Object(result), &suite_core::pyfmt::JsonStyle::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_arguments_answer_in_json() {
        assert_eq!(main_with(&[]), r#"{"success": false, "response": "I could not change that setting just now."}"#);
        assert_eq!(main_with(&["weather".into()]), r#"{"success": false, "response": "I could not check the weather just now."}"#);
        assert_eq!(main_with(&["weather".into(), "tomorrow".into()]), r#"{"success": false, "response": "I could not check the weather just now."}"#);
        assert_eq!(main_with(&["lights".into(), "on".into()]), r#"{"success": false, "response": "I could not change that setting just now."}"#);
    }
}
