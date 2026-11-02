//! Differential test: the Rust rc.xml editors produce byte-identical output
//! to the Python implementation (`~/tessel-bluetooth`, read-only) on a corpus of
//! configurations, including a scratch COPY of the live rc.xml when present.
//! Skips itself when the Python sources are not available.

use std::path::{Path, PathBuf};
use tessel_tiling::compat::b64encode;
use tessel_tiling::config::edit_bindings;
use tessel_tiling::gaps::with_gap;

const CORPUS: &[&str] = &[
    "<labwc_config></labwc_config>",
    "<labwc_config/>",
    "<?xml version=\"1.0\"?>\n<labwc_config>\n  <core><gap>4</gap></core>\n</labwc_config>\n",
    "<labwc_config><keyboard /><mouse/></labwc_config>",
    "<labwc_config>\n  <keyboard>\n    <default/>\n    <keybind key=\"W-l\"><action name=\"Execute\" command=\"lock\"/></keybind>\n  </keyboard>\n</labwc_config>\n",
    "<labwc_config><!-- <keyboard/> --><keyboard><default/></keyboard><mouse><default/><context name=\"Root\"/></mouse></labwc_config>",
    "<labwc_config><keyboard><default/></keyboard><keyboard/></labwc_config>",
    "<labwc_config><core gap=\"9\" adaptiveSync=\"no\"><decoration>server</decoration></core></labwc_config>",
    "<labwc_config><core>\n  <gap>3</gap>\n  <x a=\"&amp;&#9;\"/>\n  <gap>12</gap>\n</core><theme><name>x</name></theme></labwc_config>",
    "<labwc_config><core><![CDATA[a<b]]><gap> 5 </gap><?pi x?></core></labwc_config>",
    "<labwc_config><core gap=\"broken\"/></labwc_config>",
    "<labwc_config><core/><core/></labwc_config>",
    "<!DOCTYPE labwc_config><labwc_config/>",
    "<openbox_config/>",
    "<labwc_config>",
    "<labwc_config><keyboard>\r\n<default/>\r\n</keyboard>\r\n</labwc_config>\r\n",
    "<labwc_config><keyboard repeatRate=\"25\" repeatDelay='600'>\n<default/></keyboard ></labwc_config>",
    "<labwc_config>\n  <!-- Tessel Tiling keyboard section PGtleWJvYXJkLz4= --><keyboard><default/><keybind key=\"W-z\"/></keyboard><!-- Tessel Tiling keyboard section end -->\n</labwc_config>",
    "<labwc_config><libinput><device category=\"touchpad\"><naturalScroll>yes</naturalScroll></device></libinput>\u{e9}\u{2192}</labwc_config>",
];

fn source() -> Option<(PathBuf, PathBuf)> {
    let home = PathBuf::from(std::env::var("HOME").ok()?);
    let tiling = home.join("tessel-bluetooth/tessel-tiling");
    let settings = home.join("tessel-bluetooth/tessel-settings");
    (tiling.join("tiling_config.py").is_file() && settings.join("workspace_config.py").is_file()).then_some((tiling, settings))
}

fn encode(result: Result<Vec<u8>, String>) -> serde_json::Value {
    match result {
        Ok(data) => b64encode(&data).into(),
        Err(_) => serde_json::json!({"error": true}),
    }
}

fn encode_gap(result: Result<(Vec<u8>, i64), String>) -> serde_json::Value {
    match result {
        Ok((data, gap)) => serde_json::json!([b64encode(&data), gap]),
        Err(_) => serde_json::json!({"error": true}),
    }
}

fn normalize(value: &serde_json::Value) -> serde_json::Value {
    // Error kinds differ in name between the languages; that an error happened must match.
    match value {
        serde_json::Value::Object(map) if map.contains_key("error") => serde_json::json!({"error": true}),
        serde_json::Value::Array(items) => serde_json::Value::Array(items.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

#[test]
fn rc_xml_edits_match_python_byte_for_byte() {
    let Some((tiling, settings)) = source() else {
        eprintln!("skipped: Python sources not found");
        return;
    };
    let scratch = std::env::temp_dir().join(format!("tessel-tiling-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let mut inputs: Vec<PathBuf> = Vec::new();
    for (index, text) in CORPUS.iter().enumerate() {
        let path = scratch.join(format!("case{index}.xml"));
        std::fs::write(&path, text).unwrap();
        inputs.push(path);
    }
    // Only ever a copy: the live file is read, never written.
    let live = PathBuf::from(std::env::var("HOME").unwrap()).join(".config/labwc/rc.xml");
    if let Ok(data) = std::fs::read(&live) {
        let copy = scratch.join("live-copy.xml");
        std::fs::write(&copy, data).unwrap();
        inputs.push(copy);
    }
    let output = std::process::Command::new("python3").arg("-I").arg("-B")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py_edit.py"))
        .arg(&tiling).arg(&settings).args(&inputs).env("HOME", &scratch).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8(output.stdout).unwrap();
    // keyboard_xml uses Path.home() for the lock path: compare with the same HOME.
    unsafe { std::env::set_var("HOME", &scratch) };
    let launcher = Path::new("/tmp/tessel-tiling");
    for (line, path) in text.lines().zip(&inputs) {
        let expected: serde_json::Value = serde_json::from_str(line).unwrap();
        let data = std::fs::read(path).unwrap();
        let on = edit_bindings(&data, true, launcher).map_err(|e| e.0);
        let actual = serde_json::json!({
            "on": encode(on.clone()),
            "off": encode(edit_bindings(&data, false, launcher).map_err(|e| e.0)),
            "onoff": match &on { Ok(on) => encode(edit_bindings(on, false, launcher).map_err(|e| e.0)), Err(_) => serde_json::Value::Null },
            "gap0": encode_gap(with_gap(&data, 0)),
            "gap7": encode_gap(with_gap(&data, 7)),
        });
        let expected = serde_json::Value::Object(expected.as_object().unwrap().iter().map(|(k, v)| (k.clone(), normalize(v))).collect());
        assert_eq!(actual, expected, "{}", path.display());
        if let Ok(on) = &on {
            if path.file_name().unwrap() != "case17.xml" {
                // enable → disable is the identity on every accepted input.
                assert_eq!(edit_bindings(on, false, launcher).unwrap(), data, "{}", path.display());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
