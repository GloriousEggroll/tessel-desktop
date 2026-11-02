//! Byte-for-byte parity of palette_css / _on_accent / read_palette with the
//! Python originals in ~/tessel-bluetooth/tessel-shell (read-only; run with `python3 -I -B`).
//! Skips itself when python3 or the sources are missing.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tessel_theme::{on_accent, palette_css, parse_palette, Colors, CssOptions};

const SCRIPT: &str = r#"
import json, pathlib, sys, tempfile
home = sys.argv[1]
sys.path[:0] = [home + "/tessel-bluetooth/tessel-shell", home + "/tessel-bluetooth/shared"]
import wayshell_theme as t
data = json.load(sys.stdin)
css = []
for c in data["css"]:
    try:
        css.append(t.palette_css(c["cls"], c["colors"], c["t"], **c["kw"]).decode())
    except ValueError as error:
        css.append({"error": str(error)})
accent = [t._on_accent(a) for a in data["accent"]]
palettes = []
for raw in data["palettes"]:
    with tempfile.TemporaryDirectory() as d:
        p = pathlib.Path(d) / "palette.json"
        p.write_text(raw)
        palettes.append(t.read_palette(p))
json.dump({"css": css, "accent": accent, "palettes": palettes}, sys.stdout)
"#;

fn source_home() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.join("tessel-bluetooth/tessel-shell/wayshell_theme.py").is_file().then_some(home)
}

fn python(home: &Path, input: &Value) -> Option<Value> {
    let tmp = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let script = tmp.join(format!("tessel-theme-parity-{}.py", std::process::id()));
    std::fs::write(&script, SCRIPT).ok()?;
    let mut child = Command::new("python3").arg("-I").arg("-B").arg(&script).arg(home)
        .env("TMPDIR", &tmp).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().ok()?;
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().ok()?;
    let _ = std::fs::remove_file(&script);
    assert!(output.status.success(), "python helper failed");
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

/// Deterministic LCG so the case set is stable.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn channel(&mut self) -> Value {
        match self.next() % 4 {
            0 => json!(self.next() % 256),
            1 => json!((self.next() % 510) as f64 / 2.0), // .5 ties
            2 => json!((self.next() % 25500) as f64 / 100.0),
            _ => json!([0, 255, 128, 127][(self.next() % 4) as usize]),
        }
    }
    fn rgb(&mut self) -> Value {
        json!([self.channel(), self.channel(), self.channel()])
    }
}

fn to_rgb(value: &Value) -> [f64; 3] {
    let list = value.as_array().unwrap();
    [list[0].as_f64().unwrap(), list[1].as_f64().unwrap(), list[2].as_f64().unwrap()]
}

#[test]
fn palette_css_on_accent_and_reader_match_python_byte_for_byte() {
    let Some(home) = source_home() else {
        eprintln!("skipped: ~/tessel-bluetooth/tessel-shell not available");
        return;
    };
    let mut rng = Rng(0x5eed);
    let mut cases = Vec::new();
    let modes = [json!({}), json!({"desktop": true}), json!({"switcher": true}), json!({"backdrop": true}),
                 json!({"desktop": true, "switcher": true})];
    for mode in &modes {
        for frosted in [false, true] {
            for rounded in [true, false] {
                for amount in [0, 5, 20, 33, 50, 67, 99, 100] {
                    for strength in [0, 50, 100] {
                        let colors = json!({"background": rng.rgb(), "foreground": rng.rgb(), "accent": rng.rgb()});
                        let mut kw = mode.clone();
                        kw["frosted"] = json!(frosted);
                        kw["rounded_corners"] = json!(rounded);
                        kw["frost_strength"] = json!(strength);
                        let class = format!("tessel-shell-palette-{}", cases.len());
                        cases.push(json!({"cls": class, "colors": colors, "t": amount, "kw": kw}));
                    }
                }
            }
        }
    }
    for (amount, strength) in [(-1, 50), (101, 50), (5, -1), (5, 101)] {
        cases.push(json!({"cls": "bad", "colors": {"background": [1, 2, 3], "foreground": [4, 5, 6], "accent": [7, 8, 9]},
                          "t": amount, "kw": {"frost_strength": strength, "frosted": true}}));
    }
    let accents: Vec<Value> = (0..3000).map(|_| rng.rgb()).collect();
    let base = json!({"version": 1, "is_dark": true, "monitors": [
        {"name": "A", "geometry": [0, 0, 10, 10], "background": [1, 2, 3], "foreground": [4, 5, 6], "accent": [7, 8.5, 9],
         "frost_texture": "/x.png", "extra": {"k": [1]}}]});
    let mut palettes = vec![base.to_string(), "bad".into(), "[]".into(), "{}".into()];
    for (key, value) in [("apps_transparency", json!(30)), ("apps_frost_strength", json!(0)), ("version", json!(true)),
                         ("version", json!(1.0)), ("apps_frosted", json!(null)), ("monitors", json!([{"name": 1}])),
                         ("is_dark", json!(0)), ("apps_rounded_corners", json!(false)),
                         ("apps_transparency_enabled", json!(true)), ("apps_transparency", json!(100))] {
        let mut changed = base.clone();
        changed[key] = value;
        palettes.push(changed.to_string());
    }
    for (key, value) in [("geometry", json!([0, 0, 0, 10])), ("geometry", json!([0, 0, 10.0, 10])),
                         ("geometry", json!([0, 0, 10, true])), ("accent", json!([0, 0, 255.5])),
                         ("accent", json!([0, true, 1])), ("name", json!(null))] {
        let mut changed = base.clone();
        changed["monitors"][0][key] = value;
        palettes.push(changed.to_string());
    }
    let input = json!({"css": cases, "accent": accents, "palettes": palettes});
    let Some(expected) = python(&home, &input) else {
        eprintln!("skipped: python3 unavailable");
        return;
    };
    let mut checked = 0;
    for (case, wanted) in cases.iter().zip(expected["css"].as_array().unwrap()) {
        let kw = &case["kw"];
        let flag = |name: &str, default: bool| kw.get(name).and_then(Value::as_bool).unwrap_or(default);
        let options = CssOptions {
            desktop: flag("desktop", false), switcher: flag("switcher", false), backdrop: flag("backdrop", false),
            frosted: flag("frosted", false), rounded_corners: flag("rounded_corners", true),
            frost_strength: kw.get("frost_strength").and_then(Value::as_i64).unwrap_or(50),
        };
        let colors = Colors { background: to_rgb(&case["colors"]["background"]), foreground: to_rgb(&case["colors"]["foreground"]),
                              accent: to_rgb(&case["colors"]["accent"]) };
        let ours = palette_css(case["cls"].as_str().unwrap(), &colors, case["t"].as_i64().unwrap(), &options);
        match (ours, wanted) {
            (Ok(text), Value::String(python)) => assert_eq!(&text, python, "case {case}"),
            (Err(error), Value::Object(python)) => assert_eq!(error.0, python["error"].as_str().unwrap()),
            (ours, python) => panic!("mismatch for {case}: {ours:?} vs {python}"),
        }
        checked += 1;
    }
    assert_eq!(checked, cases.len());
    for (accent, wanted) in accents.iter().zip(expected["accent"].as_array().unwrap()) {
        assert_eq!(on_accent(to_rgb(accent)), wanted.as_str().unwrap(), "accent {accent}");
    }
    for (raw, wanted) in palettes.iter().zip(expected["palettes"].as_array().unwrap()) {
        let ours = parse_palette(raw.as_bytes()).map(|palette| palette.value).unwrap_or(Value::Null);
        assert_eq!(&ours, wanted, "palette {raw}");
    }
}
