//! The YAML subset of the way-displays IPC: `yaml.safe_load` of replies into
//! JSON values and a block-style `yaml.safe_dump(sort_keys=False)` for requests.

use serde_json::{Map, Number, Value};
use tessel_core::pyfmt::{float_repr, json_dumps, JsonStyle};
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

/// `yaml.safe_load(bytes)`; an empty document is `None` (JSON null).
pub fn load(data: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(data).map_err(|_| "The display service returned an invalid response.".to_owned())?;
    let documents = YamlLoader::load_from_str(text).map_err(|error| error.to_string())?;
    Ok(documents.first().map(convert).unwrap_or(Value::Null))
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => match (number.as_i64(), number.as_u64()) {
            (Some(int), _) => int.to_string(),
            (None, Some(int)) => int.to_string(),
            _ => float_repr(number.as_f64().unwrap_or(0.0)),
        },
        Value::String(_) => json_dumps(value, &JsonStyle::DEFAULT),
        Value::Array(items) if items.is_empty() => "[]".into(),
        Value::Object(map) if map.is_empty() => "{}".into(),
        _ => unreachable!("collections are emitted as blocks"),
    }
}

fn block(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                let key = json_dumps(&Value::String(key.clone()), &JsonStyle::DEFAULT);
                match item {
                    Value::Object(inner) if !inner.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        block(item, indent + 2, out);
                    }
                    Value::Array(inner) if !inner.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        block(item, indent, out);
                    }
                    _ => out.push_str(&format!("{pad}{key}: {}\n", scalar(item))),
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
                    _ => out.push_str(&format!("{pad}- {}\n", scalar(item))),
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other))),
    }
}

/// `f"OP: {operation}\n"` + `yaml.safe_dump({"CFG": config}, sort_keys=False)`.
pub fn request(operation: &str, config: Option<&Value>) -> String {
    let mut payload = format!("OP: {operation}\n");
    if let Some(config) = config {
        let mut wrapper = Map::new();
        wrapper.insert("CFG".into(), config.clone());
        block(&Value::Object(wrapper), 0, &mut payload);
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_payload_round_trips() {
        let config = json!({"SCALE": {"A: quoted display": 1.25, "Ünïcode \"x\"": 2.0}, "AUTO_SCALE": false});
        let payload = request("CFG_SET", Some(&config));
        assert!(payload.starts_with("OP: CFG_SET\n"));
        assert_eq!(load(payload.as_bytes()).unwrap(), json!({"OP": "CFG_SET", "CFG": config}));
        assert_eq!(request("GET", None), "OP: GET\n");
        assert_eq!(load(b"").unwrap(), Value::Null);
    }

    #[test]
    fn replies_convert_like_safe_load() {
        let reply = b"DONE: true\nRC: 0\nCFG:\n  AUTO_SCALE: true\n  SCALE:\n    First display: 1.25\nSTATE:\n  HEADS:\n    - NAME: DP-1\n      DESCRIPTION: First display\n      CURRENT:\n        ENABLED: true\n        SCALE: 1.0\nMESSAGES: [\"a\", 'b']\n";
        let value = load(reply).unwrap();
        assert_eq!(value["DONE"], json!(true));
        assert_eq!(value["RC"], json!(0));
        assert_eq!(value["CFG"]["SCALE"]["First display"], json!(1.25));
        assert_eq!(value["STATE"]["HEADS"][0]["CURRENT"]["SCALE"], json!(1.0));
        assert_eq!(value["MESSAGES"], json!(["a", "b"]));
    }
}
