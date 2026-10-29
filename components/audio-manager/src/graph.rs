//! pw-dump graph parsing: `parse_snapshot`, `merge_update`, `split_batches`
//! (audio_backend.py:42-227). Pure functions, no I/O or audio writes.

use crate::model::{Choice, Device, Node};
use suite_core::pyjson::{contains_int, first_truthy, is_true_value, py_float, py_int, py_str, py_str_opt, truthy};
use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value};

pub const AUDIO_CLASSES: [&str; 5] = ["Audio/Sink", "Audio/Source", "Audio/Source/Virtual", "Stream/Output/Audio", "Stream/Input/Audio"];
pub const GRAPH_TYPES: [&str; 4] = ["Node", "Device", "Link", "Metadata"];

pub type Nodes = IndexMap<i64, Node>;
pub type Devices = IndexMap<i64, Device>;
pub type Defaults = IndexMap<String, i64>;
/// The merged `pw-dump --monitor` graph (id -> object), in Python dict order.
pub type Graph = IndexMap<i64, Value>;

static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);

fn object_of(value: Option<&Value>) -> &Map<String, Value> {
    match value {
        Some(Value::Object(map)) if !map.is_empty() => map,
        _ => &EMPTY,
    }
}

fn list_of(value: Option<&Value>) -> &[Value] {
    match value {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

/// `cbrt(float(v))` for finite positive values, else 0.
pub fn level(value: &Value) -> f64 {
    match py_float(value) {
        Some(v) if v.is_finite() && v > 0.0 => v.cbrt(),
        _ => 0.0,
    }
}

fn choices(params: &[Value]) -> Vec<Choice> {
    params
        .iter()
        .filter_map(|p| {
            let p = p.as_object()?;
            let index = py_int(p.get("index"))?;
            let label = first_truthy(&[p.get("description"), p.get("name")])
                .map(py_str)
                .unwrap_or_else(|| format!("Profile {}", py_str_opt(p.get("index"))));
            Some(Choice { index, label, available: p.get("available") != Some(&Value::String("no".into())) })
        })
        .collect()
}

/// `str(obj.get("type") or "").rsplit(":", 1)[-1]`.
pub fn kind(obj: &Map<String, Value>) -> String {
    let text = obj.get("type").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
    text.rsplit(':').next().unwrap_or_default().to_owned()
}

fn get_str<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// `d.get(key, fallback)`: a present null does not fall back.
fn get_or<'a>(map: &'a Map<String, Value>, key: &str, fallback: Option<&'a Value>) -> Option<&'a Value> {
    if map.contains_key(key) { map.get(key) } else { fallback }
}

/// Convert a native pw-dump snapshot to UI models.
pub fn parse_snapshot<'a>(objects: impl IntoIterator<Item = &'a Value>) -> (Nodes, Devices, Defaults) {
    let mut nodes = Nodes::new();
    let mut devices = Devices::new();
    let mut defaults = Defaults::new();
    let mut raw_devices: IndexMap<i64, &Map<String, Value>> = IndexMap::new();
    let mut raw_nodes: IndexMap<i64, &Map<String, Value>> = IndexMap::new();
    let mut links: Vec<(Option<i64>, Option<i64>)> = Vec::new();
    let mut metadata: Vec<&Value> = Vec::new();

    for obj in objects {
        let Some(obj) = obj.as_object() else { continue };
        let info = object_of(obj.get("info"));
        let props = object_of(info.get("props"));
        let kind = kind(obj);
        let Some(object_id) = py_int(obj.get("id")) else { continue };
        let media_class = get_str(props, "media.class");
        if kind == "Device" && media_class == Some("Audio/Device") {
            let params = object_of(info.get("params"));
            let profiles = list_of(params.get("Profile"));
            raw_devices.insert(object_id, params);
            devices.insert(object_id, Device {
                id: object_id,
                serial: py_int(props.get("object.serial")).unwrap_or(object_id),
                name: props.get("device.name").map_or_else(String::new, py_str),
                description: first_truthy(&[props.get("device.description"), props.get("device.nick"), props.get("device.name")])
                    .map_or_else(|| "Audio device".into(), py_str),
                profiles: choices(list_of(params.get("EnumProfile"))),
                profile_index: profiles.first().and_then(|p| p.as_object()).and_then(|p| py_int(p.get("index"))),
            });
        } else if kind == "Node" && media_class.is_some_and(|c| AUDIO_CLASSES.contains(&c)) {
            raw_nodes.insert(object_id, props);
            let params = object_of(info.get("params"));
            let mut volume_props = Map::new();
            for item in list_of(params.get("Props")) {
                if let Some(item) = item.as_object() {
                    for (key, value) in item {
                        volume_props.insert(key.clone(), value.clone());
                    }
                }
            }
            let volumes: Vec<f64> = list_of(volume_props.get("channelVolumes")).iter().map(level).collect();
            let mut channels: Vec<String> = match volume_props.get("channelMap") {
                Some(value) if truthy(value) => match value {
                    Value::Array(items) => items.iter().map(py_str).collect(),
                    Value::String(text) => text.chars().map(String::from).collect(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            for i in channels.len()..volumes.len() {
                channels.push(format!("Channel {}", i + 1));
            }
            let fallback = level(volume_props.get("volume").unwrap_or(&Value::from(1)));
            let volume = volumes.iter().copied().reduce(f64::max).unwrap_or(fallback);
            nodes.insert(object_id, Node {
                id: object_id,
                serial: py_int(props.get("object.serial")).unwrap_or(object_id),
                name: props.get("node.name").map_or_else(String::new, py_str),
                description: first_truthy(&[props.get("node.description"), props.get("node.nick"), props.get("application.name"), props.get("node.name")])
                    .map_or_else(|| "Audio stream".into(), py_str),
                media_class: media_class.unwrap_or_default().to_owned(),
                icon: first_truthy(&[props.get("application.icon-name"), props.get("device.icon-name")])
                    .map_or_else(|| "audio-card-symbolic".into(), py_str),
                application: props.get("application.name").map_or_else(String::new, py_str),
                media_name: props.get("media.name").map_or_else(String::new, py_str),
                volume,
                mute: volume_props.get("mute").is_some_and(truthy),
                channels,
                volume_available: !volumes.is_empty() || volume_props.contains_key("volume"),
                channel_volumes: volumes,
                device_id: py_int(props.get("device.id")),
                route_index: None,
                routes: Vec::new(),
                target_id: None,
                state: info.get("state").map_or_else(|| "unknown".into(), py_str),
                loopback_id: props.get("audio.manager.loopback.id").filter(|v| truthy(v)).map(py_str),
                loopback_internal: is_true_value(props.get("audio.manager.loopback.internal"))
                    || is_true_value(props.get("audio.manager.meter")),
            });
        } else if kind == "Link" {
            links.push((
                py_int(get_or(info, "output-node-id", props.get("link.output.node"))),
                py_int(get_or(info, "input-node-id", props.get("link.input.node"))),
            ));
        } else if kind == "Metadata"
            && (get_str(props, "metadata.name") == Some("default")
                || get_str(object_of(obj.get("props")), "metadata.name") == Some("default"))
        {
            metadata.extend(list_of(obj.get("metadata")));
        }
    }

    let names: IndexMap<String, i64> = nodes.values().map(|n| (n.name.clone(), n.id)).collect();
    let serials: IndexMap<String, i64> = nodes.values().map(|n| (n.serial.to_string(), n.id)).collect();
    let resolve = |text: &str| names.get(text).or_else(|| serials.get(text)).copied();
    let mut targets: IndexMap<Option<i64>, Option<i64>> = IndexMap::new();
    for entry in metadata {
        let Some(entry) = entry.as_object() else { continue };
        let key = get_str(entry, "key");
        let value = entry.get("value");
        match key {
            Some(key @ ("default.audio.sink" | "default.audio.source")) => {
                let parsed;
                let mut value = value;
                if let Some(Value::String(text)) = value {
                    match serde_json::from_str::<Value>(text) {
                        Ok(v) => {
                            parsed = v;
                            value = Some(&parsed);
                        }
                        Err(_) => continue,
                    }
                }
                if let Some(Value::Object(map)) = value {
                    if let Some(id) = map.get("name").and_then(Value::as_str).and_then(|n| names.get(n)) {
                        defaults.insert(if key.ends_with("sink") { "Audio/Sink" } else { "Audio/Source" }.into(), *id);
                    }
                }
            }
            Some("target.object") => {
                targets.insert(py_int(entry.get("subject")), resolve(&py_str_opt(value)));
            }
            _ => {}
        }
    }

    let ids: Vec<i64> = nodes.keys().copied().collect();
    for id in ids {
        let props = raw_nodes[&id];
        let node = &nodes[&id];
        let params = node.device_id.and_then(|d| raw_devices.get(&d).copied()).unwrap_or(&EMPTY);
        let profile_device = py_int(props.get("card.profile.device"));
        let direction = if node.media_class == "Audio/Sink" { "Output" } else { "Input" };
        let device = node.device_id.and_then(|d| devices.get(&d));
        let mut routes = Vec::new();
        for route in list_of(params.get("EnumRoute")) {
            let Some(map) = route.as_object() else { continue };
            if get_str(map, "direction") != Some(direction) {
                continue;
            }
            if let Some(route_devices) = map.get("devices").filter(|v| truthy(v)) {
                if !contains_int(route_devices, profile_device) {
                    continue;
                }
            }
            if let (Some(profiles), Some(device)) = (map.get("profiles").filter(|v| truthy(v)), device) {
                if !contains_int(profiles, device.profile_index) {
                    continue;
                }
            }
            routes.push(route.clone());
        }
        let routes = choices(&routes);
        let mut route_index = None;
        for route in list_of(params.get("Route")) {
            let Some(map) = route.as_object() else { continue };
            if get_str(map, "direction") == Some(direction) && py_int(map.get("device")) == profile_device {
                route_index = py_int(map.get("index"));
                break;
            }
        }
        let mut target_id = targets.get(&Some(id)).copied().flatten();
        if target_id.is_none() {
            target_id = resolve(&py_str_opt(props.get("target.object")));
        }
        // Actual links take precedence over a configured target that policy may
        // have rejected or that vanished while the stream was running.
        for (output, input) in &links {
            if node.media_class == "Stream/Output/Audio" && *output == Some(id) && input.is_some_and(|i| nodes.contains_key(&i)) {
                if nodes[&input.unwrap()].media_class == "Audio/Sink" {
                    target_id = *input;
                    break;
                }
            } else if node.media_class == "Stream/Input/Audio" && *input == Some(id) && output.is_some_and(|o| nodes.contains_key(&o)) {
                if ["Audio/Source", "Audio/Source/Virtual", "Audio/Sink"].contains(&nodes[&output.unwrap()].media_class.as_str()) {
                    target_id = *output;
                    break;
                }
            }
        }
        let node = nodes.get_mut(&id).unwrap();
        node.routes = routes;
        node.route_index = route_index;
        node.target_id = target_id;
    }
    (nodes, devices, defaults)
}

/// Merge one `pw-dump --monitor` batch into `graph`. Returns the changed ids
/// and the Node subset whose update changed only params (a volume echo).
pub fn merge_update(graph: &mut Graph, objects: &Value) -> (IndexSet<i64>, IndexSet<i64>) {
    let mut changed = IndexSet::new();
    let mut props_only = IndexSet::new();
    let Value::Array(objects) = objects else { return (changed, props_only) };
    for obj in objects {
        let Some(obj) = obj.as_object() else { continue };
        let Some(object_id) = py_int(obj.get("id")) else { continue };
        if obj.get("info") == Some(&Value::Null) && !obj.contains_key("type") {
            if graph.shift_remove(&object_id).is_some() {
                changed.insert(object_id);
            }
            continue;
        }
        let kind = kind(obj);
        let Some(current) = graph.get_mut(&object_id) else {
            if GRAPH_TYPES.contains(&kind.as_str()) {
                graph.insert(object_id, Value::Object(obj.clone()));
                changed.insert(object_id);
            }
            continue;
        };
        // pw-dump repeats the full info; its change-mask names what changed.
        if kind == "Node" {
            if let Some(Value::Object(info)) = obj.get("info") {
                let mask = info.get("change-mask").filter(|v| truthy(v));
                let only_params = match mask {
                    Some(Value::Array(items)) => items.iter().all(|i| i.as_str() == Some("params")),
                    _ => false,
                };
                if only_params {
                    props_only.insert(object_id);
                }
            }
        }
        let current = current.as_object_mut().expect("graph objects are JSON objects");
        for (key, value) in obj {
            match (key.as_str(), value, current.get_mut("info")) {
                ("info", Value::Object(update), Some(Value::Object(merged))) => {
                    for (field, item) in update {
                        match (field.as_str(), item, merged.get_mut("params")) {
                            ("params", Value::Object(params), Some(Value::Object(existing))) => {
                                for (name, param) in params {
                                    existing.insert(name.clone(), param.clone());
                                }
                            }
                            _ => {
                                merged.insert(field.clone(), item.clone());
                            }
                        }
                    }
                }
                _ => {
                    current.insert(key.clone(), value.clone());
                }
            }
        }
        changed.insert(object_id);
    }
    (changed, props_only)
}

/// Split complete top-level monitor arrays from a byte buffer. With the
/// default two-space indent only a batch's closing bracket starts a line, and
/// JSON strings cannot contain raw newlines.
pub fn split_batches(mut buffer: Vec<u8>) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut batches = Vec::new();
    loop {
        let Some(end) = buffer.windows(3).position(|w| w == b"\n]\n") else { return (batches, buffer) };
        let rest = buffer.split_off(end + 3);
        buffer.truncate(end + 2);
        batches.push(buffer);
        buffer = rest;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    fn object(kind: &str, id: i64, props: Value, params: Value, info: Value) -> Value {
        let mut map = json!({"props": props, "params": params});
        for (k, v) in info.as_object().unwrap() {
            map[k] = v.clone();
        }
        json!({"id": id, "type": format!("PipeWire:Interface:{kind}"), "info": map})
    }

    /// test_audio_backend.fixture()
    pub fn fixture() -> Vec<Value> {
        vec![
            object("Device", 10, json!({"object.serial": 100, "media.class": "Audio/Device", "device.name": "card", "device.description": "USB Audio"}), json!({
                "Profile": [{"index": 2}],
                "EnumProfile": [{"index": 0, "description": "Off", "available": "yes"}, {"index": 2, "description": "Duplex", "available": "yes"}, {"index": 3, "description": "HDMI", "available": "no"}],
                "EnumRoute": [
                    {"index": 5, "direction": "Output", "description": "Speakers", "available": "yes", "profiles": [2], "devices": [7]},
                    {"index": 6, "direction": "Output", "description": "Headphones", "available": "no", "profiles": [2], "devices": [7]},
                    {"index": 7, "direction": "Output", "description": "Wrong PCM", "profiles": [2], "devices": [8]},
                    {"index": 8, "direction": "Input", "description": "Microphone", "profiles": [2], "devices": [9]},
                    {"index": 9, "direction": "Output", "description": "Wrong profile", "profiles": [3], "devices": [7]}],
                "Route": [{"index": 5, "direction": "Output", "device": 7}]}), json!({})),
            object("Node", 20, json!({"object.serial": 200, "media.class": "Audio/Sink", "node.name": "speakers", "node.description": "Speakers", "device.id": 10, "card.profile.device": 7}),
                json!({"Props": [{"mute": false, "channelVolumes": [0.125, 1.0], "channelMap": ["FL", "FR"]}]}), json!({"state": "running"})),
            object("Node", 21, json!({"object.serial": 210, "media.class": "Audio/Source", "node.name": "mic"}), json!({"Props": [{"mute": true, "channelVolumes": [0.125], "channelMap": ["MONO"]}]}), json!({})),
            object("Node", 30, json!({"object.serial": 300, "media.class": "Stream/Output/Audio", "node.name": "music", "application.name": "Music", "target.object": "210"}), json!({"Props": [{"volume": 1, "channelVolumes": [1.0, 1.0]}]}), json!({})),
            object("Node", 31, json!({"object.serial": 310, "media.class": "Stream/Input/Audio", "node.name": "recorder"}), json!({}), json!({})),
            object("Node", 99, json!({"media.class": "Video/Source", "node.name": "camera"}), json!({}), json!({})),
            object("Link", 40, json!({}), json!({}), json!({"output-node-id": 30, "input-node-id": 20})),
            object("Link", 41, json!({}), json!({}), json!({"output-node-id": 20, "input-node-id": 31})),
            json!({"id": 50, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"}, "metadata": [
                {"subject": 0, "key": "default.audio.sink", "value": {"name": "speakers"}},
                {"subject": 0, "key": "default.audio.source", "value": "{\"name\":\"mic\"}"}]}),
        ]
    }

    #[test]
    fn filters_non_audio_and_reads_live_defaults() {
        let (nodes, devices, defaults) = parse_snapshot(&fixture());
        assert_eq!(nodes.keys().copied().collect::<Vec<_>>(), [20, 21, 30, 31]);
        assert_eq!(devices.keys().copied().collect::<Vec<_>>(), [10]);
        assert_eq!(defaults, Defaults::from([("Audio/Sink".into(), 20), ("Audio/Source".into(), 21)]));
    }

    #[test]
    fn desktop_cubic_volume_and_individual_channels() {
        let (nodes, _, _) = parse_snapshot(&fixture());
        assert!((nodes[&20].channel_volumes[0] - 0.5).abs() < 1e-7);
        assert!((nodes[&20].channel_volumes[1] - 1.0).abs() < 1e-7);
        assert_eq!(nodes[&20].volume, 1.0);
        assert!(nodes[&21].mute);
        assert!(!nodes[&31].volume_available);
    }

    #[test]
    fn routes_are_scoped_to_pcm_direction_and_profile() {
        let (nodes, devices, _) = parse_snapshot(&fixture());
        assert_eq!(nodes[&20].routes.iter().map(|r| r.index).collect::<Vec<_>>(), [5, 6]);
        assert!(!nodes[&20].routes[1].available);
        assert_eq!(nodes[&20].route_index, Some(5));
        assert_eq!(devices[&10].profile_index, Some(2));
        assert!(!devices[&10].profiles[2].available);
    }

    #[test]
    fn actual_playback_and_monitor_capture_links_win() {
        let (nodes, _, _) = parse_snapshot(&fixture());
        assert_eq!(nodes[&30].target_id, Some(20));
        assert_eq!(nodes[&31].target_id, Some(20));
    }

    #[test]
    fn metadata_target_uses_serial_not_recycled_object_id() {
        let mut data: Vec<Value> = fixture().into_iter().filter(|o| !o["type"].as_str().unwrap().ends_with(":Link")).collect();
        let last = data.len() - 1;
        data[last]["metadata"].as_array_mut().unwrap().push(json!({"subject": 30, "key": "target.object", "value": "200"}));
        let (nodes, _, _) = parse_snapshot(&data);
        assert_eq!(nodes[&30].target_id, Some(20));
        let n = data[last]["metadata"].as_array().unwrap().len();
        data[last]["metadata"][n - 1]["value"] = json!("20");
        data[3]["info"]["props"].as_object_mut().unwrap().shift_remove("target.object");
        let (nodes, _, _) = parse_snapshot(&data);
        assert_eq!(nodes[&30].target_id, None);
    }

    #[test]
    fn snapshot_replaces_removed_objects_and_invalid_defaults() {
        let data = fixture();
        let (nodes, devices, defaults) = parse_snapshot(&data[data.len() - 1..]);
        assert!(nodes.is_empty() && devices.is_empty() && defaults.is_empty());
    }

    #[test]
    fn partial_updates_merge_params_and_removals() {
        let mut graph: Graph = fixture().into_iter().map(|o| (o["id"].as_i64().unwrap(), o)).collect();
        let (changed, params_only) = merge_update(&mut graph, &json!([
            {"id": 20, "type": "PipeWire:Interface:Node", "info": {
                "change-mask": ["params"], "params": {"Props": [{"mute": true, "channelVolumes": [1.0, 1.0]}]}}},
            {"id": 21, "type": "PipeWire:Interface:Node", "info": {
                "change-mask": ["props"], "props": {"object.serial": 210, "media.class": "Audio/Source",
                                                    "node.name": "mic", "node.description": "Renamed"}}},
            {"id": 30, "info": null},
            {"id": 77, "type": "PipeWire:Interface:Port", "info": {}},
            {"id": 78, "type": "PipeWire:Interface:Node", "info": {"props": {
                "object.serial": 780, "media.class": "Audio/Sink", "node.name": "new"}}}]));
        assert_eq!(changed.iter().copied().collect::<std::collections::BTreeSet<_>>(), [20, 21, 30, 78].into());
        assert_eq!(params_only.iter().copied().collect::<Vec<_>>(), [20]);
        assert!(!graph.contains_key(&77));
        let (nodes, _, _) = parse_snapshot(graph.values());
        assert!(nodes[&20].mute);
        assert_eq!(nodes[&20].route_index, Some(5), "Route data survives params merge");
        assert!(!nodes[&20].routes.is_empty());
        assert_eq!(nodes[&21].description, "Renamed");
        assert!(!nodes.contains_key(&30));
        assert!(nodes.contains_key(&78));
        let (changed, params_only) = merge_update(&mut graph, &json!([{"id": 999, "info": null}]));
        assert!(changed.is_empty() && params_only.is_empty());
    }

    #[test]
    fn batches_split_only_at_top_level() {
        let data = b"[\n  {\n    \"id\": 1,\n    \"info\": {\n      \"params\": [\n      ]\n    }\n  }\n]\n[\n  {\n    \"id\": 2".to_vec();
        let (batches, rest) = split_batches(data);
        assert_eq!(batches.len(), 1);
        let parsed: Value = serde_json::from_slice(&batches[0]).unwrap();
        assert_eq!(parsed[0]["id"], 1);
        assert_eq!(rest, b"[\n  {\n    \"id\": 2");
    }

    #[test]
    fn meter_streams_are_hidden_like_internal_loopbacks() {
        let mut data = fixture();
        data.push(object("Node", 60, json!({"object.serial": 600, "media.class": "Stream/Input/Audio", "node.name": "meter", "audio.manager.meter": true}), json!({}), json!({})));
        let (nodes, _, _) = parse_snapshot(&data);
        assert!(nodes[&60].loopback_internal);
        assert!(!nodes[&31].loopback_internal);
    }

    #[test]
    fn native_snapshot_marks_internal_streams_without_truthy_false_strings() {
        const ID: &str = "532aa4b1-bd34-4d0c-8a4e-34580ebf1228";
        let node = |id: i64, internal: &str| json!({"id": id, "type": "PipeWire:Interface:Node", "info": {"props": {
            "media.class": "Audio/Sink", "node.name": id.to_string(),
            "audio.manager.loopback.id": ID, "audio.manager.loopback.internal": internal}}});
        let (nodes, _, _) = parse_snapshot(&[node(1, "true"), node(2, "false")]);
        assert_eq!(nodes[&1].loopback_id.as_deref(), Some(ID));
        assert!(nodes[&1].loopback_internal);
        assert!(!nodes[&2].loopback_internal);
    }
}
