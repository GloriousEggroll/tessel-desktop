//! Compare Rust models with the Python `dataclasses.asdict` reference.

use crate::graph::{Defaults, Devices, Nodes};
use crate::model::Choice;
use serde_json::Value;

fn close(a: f64, b: &Value) -> bool {
    b.as_f64().is_some_and(|b| (a - b).abs() < 1e-9)
}

fn choices(actual: &[Choice], expected: &Value) {
    let expected = expected.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(expected) {
        assert_eq!(a.index, e["index"].as_i64().unwrap());
        assert_eq!(a.label, e["label"].as_str().unwrap());
        assert_eq!(a.available, e["available"].as_bool().unwrap());
    }
}

fn opt_int(a: Option<i64>, e: &Value) -> bool {
    a == e.as_i64()
}

pub fn assert_model(actual: &(Nodes, Devices, Defaults), expected: &Value) {
    let (nodes, devices, defaults) = actual;
    let en = expected["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), en.len(), "node count");
    for (n, e) in nodes.values().zip(en) {
        let ctx = format!("node {}", n.id);
        assert_eq!(n.id, e["id"].as_i64().unwrap(), "{ctx}");
        assert_eq!(n.serial, e["serial"].as_i64().unwrap(), "{ctx}");
        for (field, value) in [("name", &n.name), ("description", &n.description), ("media_class", &n.media_class), ("icon", &n.icon),
                               ("application", &n.application), ("media_name", &n.media_name), ("state", &n.state)] {
            assert_eq!(value, e[field].as_str().unwrap(), "{ctx} {field}");
        }
        assert!(close(n.volume, &e["volume"]), "{ctx} volume {} {}", n.volume, e["volume"]);
        assert_eq!(n.mute, e["mute"].as_bool().unwrap(), "{ctx}");
        assert_eq!(n.channels, e["channels"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_owned()).collect::<Vec<_>>(), "{ctx}");
        let ev = e["channel_volumes"].as_array().unwrap();
        assert_eq!(n.channel_volumes.len(), ev.len(), "{ctx}");
        assert!(n.channel_volumes.iter().zip(ev).all(|(a, b)| close(*a, b)), "{ctx}");
        assert!(opt_int(n.device_id, &e["device_id"]), "{ctx} device_id");
        assert!(opt_int(n.route_index, &e["route_index"]), "{ctx} route_index");
        assert!(opt_int(n.target_id, &e["target_id"]), "{ctx} target_id {:?} {}", n.target_id, e["target_id"]);
        choices(&n.routes, &e["routes"]);
        assert_eq!(n.volume_available, e["volume_available"].as_bool().unwrap(), "{ctx}");
        assert_eq!(n.loopback_id.as_deref(), e["loopback_id"].as_str(), "{ctx}");
        assert_eq!(n.loopback_internal, e["loopback_internal"].as_bool().unwrap(), "{ctx}");
    }
    let ed = expected["devices"].as_array().unwrap();
    assert_eq!(devices.len(), ed.len());
    for (d, e) in devices.values().zip(ed) {
        assert_eq!(d.id, e["id"].as_i64().unwrap());
        assert_eq!(d.serial, e["serial"].as_i64().unwrap());
        assert_eq!(d.name, e["name"].as_str().unwrap());
        assert_eq!(d.description, e["description"].as_str().unwrap());
        assert!(opt_int(d.profile_index, &e["profile_index"]));
        choices(&d.profiles, &e["profiles"]);
    }
    let edef = expected["defaults"].as_object().unwrap();
    assert_eq!(defaults.len(), edef.len());
    for (k, v) in edef {
        assert_eq!(defaults.get(k).copied(), v.as_i64(), "default {k}");
    }
}
