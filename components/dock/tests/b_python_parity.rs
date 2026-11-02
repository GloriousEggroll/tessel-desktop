//! Byte parity of W-c's pure helpers with the Python modules (run read-only
//! from ~/tessel-bluetooth/tessel-dock with `python3 -I`, HOME pointed at a scratch dir).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use tessel_dock::dbus::settings_api::workarea_snapshot;
use tessel_dock::dbus::sni::{parse_service, pixmap_rgba, tooltip_text};
use tessel_dock::status::backend::{audio_status, bluetooth_status, indicator, plain_text};

const SCRIPT: &str = r#"
import json, sys, os
sys.path[:0] = [sys.argv[2] + '/tessel_dock', sys.argv[2] + '/shared']
os.environ['HOME'] = sys.argv[1]
import status_backend as s, settings_api as a
cases = json.loads(sys.stdin.read())
out = {}
def safe(f, *args):
    try:
        return {'ok': f(*args)}
    except Exception as error:
        return {'error': str(error)}
out['indicator'] = [safe(lambda v, h: s.indicator(v, hide_empty=h), v, h) for v, h in cases['indicator']]
out['audio'] = [safe(s.audio_status, line) for line in cases['audio']]
out['bluetooth'] = [s.bluetooth_status(o) for o in cases['bluetooth']]
out['plain'] = [s.plain_text(t) for t in cases['plain']]
out['workareas'] = [json.dumps(a.workarea_snapshot([(n, tuple(g)) for n, g in m], [tuple(r) for r in r_]), separators=(',', ':')) for m, r_ in cases['workareas']]
from gi.repository import GLib
import status_tray as t
out['service'] = [safe(t.parse_service, v, sender) for v, sender in cases['service']]
out['tooltip'] = [t.tooltip_text({'ToolTip': ('', [], title, body)}) for title, body in cases['tooltip']]
px = t.pixmap_rgba([tuple(p) for p in cases['pixmap'][0]], cases['pixmap'][1])
out['pixmap'] = [px[0], px[1], list(px[2])] if px else None
print(json.dumps(out, default=list))
"#;

#[test]
fn helpers_match_python_bytes() {
    let home = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("b-parity-home");
    std::fs::create_dir_all(&home).unwrap();
    let cases = json!({
        "indicator": [[{"text": "<b>x</b> &amp; y", "tooltip": "<span>t&lt;</span>", "class": ["A", "b c", "ok-1", ""]}, false],
                      [{"text": ""}, true], [{"text": "x", "class": "One"}, false], [{"text": 1}, false], [{"text": "x", "class": [1]}, false],
                      [{"text": "&copy; &notit; &#x41;"}, false]],
        "audio": ["Volume: 0.42", "Volume: 0.425 [MUTED]", "Volume: 0.00", "Volume: 0.005", "Volume: 100.01", "Volume: 1.5", "nope", "Volume: 0.5 x"],
        "bluetooth": [{}, {"/a": {"org.bluez.Adapter1": {"Powered": false}}},
                      {"/a": {"org.bluez.Adapter1": {"Powered": true}}, "/a/d": {"org.bluez.Device1": {"Connected": true, "Name": "Pad"}},
                       "/a/e": {"org.bluez.Device1": {"Connected": true, "Alias": ""}}}],
        "plain": ["<i>a</i>&lt;b&gt;", "x < y", "&ampx &AMP; &unknown;"],
        "workareas": [[[["DP-1", [0, 0, 1920, 1080]], ["HDMI-A-1", [1920, 0, 2560, 1440]]],
                       [["DP-1", "top", 40], ["DP-1", "left", 30], ["HDMI-A-1", "bottom", 5000], ["DP-1", "right", 0]]]],
        "service": [["org.example.App", null], ["/Item", ":1.7"], [":1.7/Item", null], ["invalid", null], ["/x", null], [":1.7/bad-path", null]],
        "tooltip": [["Music", "<b>A &amp; B</b><br/>Playing"], ["", "<p>one</p><p>two</p>"], ["T", "  "]],
        "pixmap": [[[2, 2, [255, 1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3]], [1, 1, [128, 5, 7, 9]], [3, 3, [0]]], 2],
    });
    let mut child = Command::new("/usr/bin/python3").arg("-I").arg("-c").arg(SCRIPT).arg(&home).arg(std::path::Path::new(&std::env::var("HOME").unwrap()).join("tessel-bluetooth"))
        .env("HOME", &home).env("LANGUAGE", "en").env("LANG", "C.UTF-8").env_remove("LC_ALL")
        .env_remove("DBUS_SESSION_BUS_ADDRESS").env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY")
        .current_dir(&home).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(cases.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let python: Value = serde_json::from_slice(&output.stdout).unwrap();

    let result = |r: Result<serde_json::Map<String, Value>, String>| match r { Ok(v) => json!({"ok": v}), Err(e) => json!({"error": e}) };
    let indicators: Vec<Value> = cases["indicator"].as_array().unwrap().iter()
        .map(|case| result(indicator(&case[0], case[1].as_bool().unwrap()))).collect();
    assert_eq!(Value::Array(indicators), python["indicator"]);
    let audio: Vec<Value> = cases["audio"].as_array().unwrap().iter().map(|line| result(audio_status(line.as_str().unwrap()))).collect();
    assert_eq!(Value::Array(audio), python["audio"]);
    let bluetooth: Vec<Value> = cases["bluetooth"].as_array().unwrap().iter().map(|o| Value::Object(bluetooth_status(o))).collect();
    assert_eq!(Value::Array(bluetooth), python["bluetooth"]);
    let plain: Vec<Value> = cases["plain"].as_array().unwrap().iter().map(|t| plain_text(t.as_str().unwrap()).into()).collect();
    assert_eq!(Value::Array(plain), python["plain"]);
    let case = &cases["workareas"][0];
    let monitors: Vec<(String, (i64, i64, i64, i64))> = case[0].as_array().unwrap().iter().map(|m| {
        let g: Vec<i64> = m[1].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
        (m[0].as_str().unwrap().to_owned(), (g[0], g[1], g[2], g[3]))
    }).collect();
    let reservations: Vec<(String, String, i64)> = case[1].as_array().unwrap().iter()
        .map(|r| (r[0].as_str().unwrap().to_owned(), r[1].as_str().unwrap().to_owned(), r[2].as_i64().unwrap())).collect();
    let bytes = tessel_core::pyfmt::json_dumps(&workarea_snapshot(&monitors, &reservations), &tessel_core::pyfmt::JsonStyle::COMPACT);
    assert_eq!(Value::String(bytes), python["workareas"][0]);
    let services: Vec<Value> = cases["service"].as_array().unwrap().iter().map(|case| {
        match parse_service(case[0].as_str().unwrap(), case[1].as_str()) {
            Ok((name, path)) => json!({"ok": [name, path]}),
            Err(error) => json!({"error": error}),
        }
    }).collect();
    assert_eq!(Value::Array(services), python["service"]);
    use glib::prelude::*;
    let tooltips: Vec<Value> = cases["tooltip"].as_array().unwrap().iter().map(|case| {
        let mut properties = HashMap::new();
        properties.insert("ToolTip".to_owned(), ("", Vec::<(i32, i32, Vec<u8>)>::new(), case[0].as_str().unwrap(), case[1].as_str().unwrap()).to_variant());
        Value::String(tooltip_text(&properties))
    }).collect();
    assert_eq!(Value::Array(tooltips), python["tooltip"]);
    let pixmaps: Vec<(i64, i64, Vec<u8>)> = cases["pixmap"][0].as_array().unwrap().iter().map(|p| (p[0].as_i64().unwrap(), p[1].as_i64().unwrap(),
        p[2].as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect())).collect();
    let best = pixmap_rgba(&pixmaps, cases["pixmap"][1].as_i64().unwrap()).map(|(w, h, d)| json!([w, h, d]));
    assert_eq!(best.unwrap_or(Value::Null), python["pixmap"]);
}
