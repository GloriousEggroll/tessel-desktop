//! weather_status.py: the Tessel Dock JSON payload (strings are NOT translated;
//! the dock translates them). Times use the C locale like the Python stream.

use crate::backend::{format_temperature, has_location, weather_description, weather_icon, WeatherSnapshot, NO_LOCATION};
use crate::dates::{self, MONTHS, WEEKDAYS};
use serde_json::{json, Value};
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub fn status_payload(snapshot: &WeatherSnapshot) -> Value {
    let config = &snapshot.config;
    if !has_location(config) {
        return json!({"text": "󰖐", "tooltip": format!("Weather\n{NO_LOCATION}"),
                      "class": ["weather", "unconfigured"], "alt": "unconfigured", "icon": "weather-few-clouds-symbolic"});
    }
    let Some(current) = &snapshot.current else {
        return json!({"text": "󰖐 —",
                      "tooltip": format!("{}\n{}", config.name, snapshot.error.as_deref().unwrap_or("Weather unavailable")),
                      "class": ["weather", "unavailable"], "alt": "unavailable", "icon": "weather-severe-alert-symbolic"});
    };
    let icon = weather_icon(Some(current.code), current.is_day);
    let symbol = match icon {
        "weather-clear-symbolic" => "☀",
        "weather-clear-night-symbolic" => "☾",
        "weather-few-clouds-symbolic" => "⛅",
        "weather-few-clouds-night-symbolic" | "weather-overcast-symbolic" => "☁",
        "weather-fog-symbolic" => "≋",
        "weather-snow-symbolic" => "❄",
        "weather-storm-symbolic" => "ϟ",
        "weather-showers-symbolic" => "☂",
        _ => "☁",
    };
    let description = weather_description(Some(current.code));
    let temperature = format_temperature(Some(current.temp), &config.units);
    let mut lines = vec![config.name.clone(), format!("{description} · {temperature}")];
    if current.apparent.is_some() {
        lines.push(format!("Feels like {}", format_temperature(current.apparent, &config.units)));
    }
    if let Some(fetched) = snapshot.fetched_at {
        if let Some(local) = dates::local(fetched, &config.timezone) {
            lines.push(format!("Updated {} {:02}:{:02}", WEEKDAYS[local.date.weekday() as usize], local.hour, local.minute));
        }
    }
    if snapshot.stale {
        lines.push("Out of date — showing the last available forecast.".into());
    }
    if let Some(error) = &snapshot.error {
        lines.push(error.clone());
    }
    json!({"text": format!("{symbol} {temperature}"), "tooltip": lines.join("\n"),
           "class": if snapshot.stale { json!(["weather", "stale"]) } else { json!(["weather"]) },
           "alt": description, "icon": icon})
}

/// `json.dumps(data, ensure_ascii=False)`.
pub fn line(data: &Value) -> String { json_dumps(data, &JsonStyle { ensure_ascii: false, ..JsonStyle::DEFAULT }) }

/// `%b %d` in the C locale (used by tests and the C-locale status stream).
pub fn c_month_day(date: dates::Date) -> String { format!("{} {:02}", MONTHS[date.month as usize - 1], date.day) }
