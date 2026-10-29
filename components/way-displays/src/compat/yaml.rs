//! The YAML of the way-displays IPC: `yaml.safe_load` of replies into JSON
//! values and `yaml.safe_dump(value, sort_keys=False)` (block style,
//! indentless sequences, PyYAML's plain/single/double scalar choice).

use regex::Regex;
use serde_json::{Map, Number, Value};
use std::sync::LazyLock;
use suite_core::pyfmt::float_repr;
use yaml_rust2::{Yaml, YamlLoader};

fn convert(value: &Yaml) -> Value {
    match value {
        Yaml::Real(text) => match value.as_f64() {
            Some(number) => Number::from_f64(number).map(Value::Number).unwrap_or(Value::Null),
            None => Value::String(text.clone()),
        },
        Yaml::Integer(number) => Value::from(*number),
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Boolean(flag) => Value::Bool(*flag),
        Yaml::Array(items) => Value::Array(items.iter().map(convert).collect()),
        Yaml::Hash(entries) => {
            let mut map = Map::new();
            for (key, item) in entries {
                let key = match key {
                    Yaml::String(text) | Yaml::Real(text) => text.clone(),
                    Yaml::Integer(number) => number.to_string(),
                    Yaml::Boolean(flag) => if *flag { "True".into() } else { "False".into() },
                    Yaml::Null => "None".into(),
                    other => format!("{other:?}"),
                };
                map.insert(key, convert(item));
            }
            Value::Object(map)
        }
        Yaml::Null | Yaml::Alias(_) | Yaml::BadValue => Value::Null,
    }
}

/// `yaml.safe_load(text)`; an empty document is `None` (JSON null).
pub fn load(text: &str) -> Result<Value, String> {
    let documents = YamlLoader::load_from_str(text).map_err(|error| error.to_string())?;
    Ok(documents.first().map(convert).unwrap_or(Value::Null))
}

static IMPLICIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF",
        r"|y|Y|n|N|~|null|Null|NULL|=|<<",
        r"|[-+]?(?:[0-9][0-9_]*)?\.[0-9_]*(?:[eE][-+][0-9]+)?",
        r"|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*",
        r"|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN)",
        r"|[-+]?0b[0-1_]+|[-+]?0[0-7_]+|[-+]?(?:0|[1-9][0-9_]*)|[-+]?0x[0-9a-fA-F_]+|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+",
        r"|[0-9][0-9][0-9][0-9]-[0-9][0-9]?-[0-9][0-9]?.*)$"
    )).unwrap()
});

/// PyYAML `analyze_scalar` + `choose_scalar_style` for the scalars this
/// program sends: plain when it round-trips, else single quotes, else double.
fn string_scalar(text: &str, _simple_key: bool) -> String {
    let printable = text.chars().all(|c| c == '\n' || (' '..='~').contains(&c));
    let multiline = text.contains('\n');
    let mut plain = !text.is_empty() && printable && !multiline && !IMPLICIT.is_match(text);
    if plain {
        let first = text.chars().next().unwrap();
        let second = text.chars().nth(1);
        let leading_indicator = "#,[]{}&*!|>'\"%@`".contains(first)
            || (matches!(first, '?' | ':' | '-') && second.is_none_or(|c| c == ' '));
        if leading_indicator || text.starts_with(' ') || text.ends_with(' ') || text.contains(": ") || text.contains(" #")
            || text.ends_with(':') || text.starts_with("---") || text.starts_with("...")
        {
            plain = false;
        }
    }
    if plain {
        return text.to_owned();
    }
    if printable && !multiline {
        return format!("'{}'", text.replace('\'', "''"));
    }
    double_quoted(text)
}

fn double_quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            ' '..='~' => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02X}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push_str(&format!("\\U{:08X}", c as u32)),
        }
    }
    out.push('"');
    out
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => match (number.as_i64(), number.as_u64()) {
            (Some(int), _) => int.to_string(),
            (None, Some(int)) => int.to_string(),
            _ => {
                let value = number.as_f64().unwrap_or(0.0);
                // PyYAML represent_float: repr with an exponent gets ".0" before "e".
                let text = float_repr(value);
                if text.contains('e') && !text.contains('.') {
                    text.replacen('e', ".0e", 1)
                } else {
                    text
                }
            }
        },
        Value::String(text) => string_scalar(text, false),
        Value::Array(items) if items.is_empty() => "[]".into(),
        Value::Object(map) if map.is_empty() => "{}".into(),
        _ => unreachable!("collections are emitted as blocks"),
    }
}

fn key(text: &str) -> String {
    string_scalar(text, true)
}

fn block(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (name, item) in map {
                let name = key(name);
                match item {
                    Value::Object(inner) if !inner.is_empty() => {
                        out.push_str(&format!("{pad}{name}:\n"));
                        block(item, indent + 2, out);
                    }
                    Value::Array(inner) if !inner.is_empty() => {
                        out.push_str(&format!("{pad}{name}:\n"));
                        block(item, indent, out);
                    }
                    _ => out.push_str(&format!("{pad}{name}: {}\n", scalar(item))),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(inner) if !inner.is_empty() => {
                        let mut nested = String::new();
                        block(item, indent + 2, &mut nested);
                        out.push_str(&format!("{pad}- {}", &nested[indent + 2..]));
                    }
                    Value::Array(inner) if !inner.is_empty() => {
                        let mut nested = String::new();
                        block(item, indent + 2, &mut nested);
                        out.push_str(&format!("{pad}- {}", &nested[indent + 2..]));
                    }
                    _ => out.push_str(&format!("{pad}- {}\n", scalar(item))),
                }
            }
        }
        other => out.push_str(&format!("{}\n", scalar(other))),
    }
}

/// `yaml.safe_dump(value, sort_keys=False)`.
pub fn safe_dump(value: &Value) -> String {
    let mut out = String::new();
    block(value, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dump_matches_pyyaml_for_daemon_payloads() {
        let cfg = json!({"CFG": {
            "MODE": {"Dell Inc. DELL U2720Q 1234": {"WIDTH": 3840, "HEIGHT": 2160, "HZ": 59.997}},
            "SCALE": {"A: quoted display": 1.25},
            "AUTO_SCALE": true,
            "TRANSFORM": {"Mon": "90"},
            "DISABLED": {"Mon B": null},
            "VRR_OFF": ["Mon"],
            "ORDER": ["Mon", "yes"],
            "ARRANGE": "ROW", "ALIGN": "TOP",
            "EMPTY": [], "NONE": {},
            "CALLBACK_CMD": "notify-send \"way-displays ${CALLBACK_LEVEL}\" \"${CALLBACK_MSG}\""}});
        let expected = "CFG:\n  MODE:\n    Dell Inc. DELL U2720Q 1234:\n      WIDTH: 3840\n      HEIGHT: 2160\n      HZ: 59.997\n  SCALE:\n    'A: quoted display': 1.25\n  AUTO_SCALE: true\n  TRANSFORM:\n    Mon: '90'\n  DISABLED:\n    Mon B: null\n  VRR_OFF:\n  - Mon\n  ORDER:\n  - Mon\n  - 'yes'\n  ARRANGE: ROW\n  ALIGN: TOP\n  EMPTY: []\n  NONE: {}\n  CALLBACK_CMD: notify-send \"way-displays ${CALLBACK_LEVEL}\" \"${CALLBACK_MSG}\"\n";
        assert_eq!(safe_dump(&cfg), expected);
        assert_eq!(load(&safe_dump(&cfg)).unwrap()["CFG"]["SCALE"], json!({"A: quoted display": 1.25}));
        let multi = json!({"CALLBACK_CMD": "# a\nxrandr 'x'\n"});
        assert_eq!(load(&safe_dump(&multi)).unwrap(), multi);
        assert_eq!(safe_dump(&json!({"HZ": 60.0})), "HZ: 60.0\n");
    }

    #[test]
    fn replies_convert_like_safe_load() {
        let reply = "CFG:\n  AUTO_SCALE: true\n  SCALE:\n    First display: 1.25\nSTATE:\n  HEADS:\n    - NAME: DP-1\n      DESCRIPTION: First display\n      CURRENT:\n        ENABLED: true\n        SCALE: 1.0\n";
        let value = load(reply).unwrap();
        assert_eq!(value["CFG"]["SCALE"]["First display"], json!(1.25));
        assert_eq!(value["STATE"]["HEADS"][0]["CURRENT"]["SCALE"], json!(1.0));
        assert_eq!(load("").unwrap(), Value::Null);
    }
}
