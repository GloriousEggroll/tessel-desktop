//! Byte parity with the Python modules: every fixture goes through the same
//! operations in Python (tests/parity_driver.py, the read-only tessel-bluetooth
//! sources) and in the Rust port, and every output file must be identical.
//! Runs only inside components/settings/tests/headless.sh (fake HOME,
//! private session); under plain `cargo test` it skips itself.
use std::path::{Path, PathBuf};

use gs_system::compat::Error;
use gs_system::input_config::{self, Updates};
use gs_system::keyboard_config::{KeyboardConfig, KeyboardState};
use gs_system::labwc_settings::{self, EnvValues};
use gs_system::night_light::{self, NightLightConfig, NightLightStore};
use gs_system::shortcuts_model;
use gs_system::workspace_config;
use serde_json::{json, Value};

const FIXTURES: &[(&str, &str)] = &[
    ("system_pages", r#"<?xml version="1.0"?>
<!-- user comment -->
<labwc_config>
  <core><hdr>no</hdr></core>
  <keyboard>
    <default/>
    <repeatRate>30</repeatRate>
    <keybind key="W-d"><action name="ToggleShowDesktop"/></keybind>
    <keybind key="W-l"><action name="Execute"><command>/home/u/.local/bin/tessel_shell-lock</command></action></keybind>
    <keybind key="A-F4"/>
    <!-- Tessel Tiling keyboard begin -->
    <keybind key="W-t"><action name="Tiling" command="float"/></keybind>
    <!-- Tessel Tiling keyboard end -->
  </keyboard>
  <mouse><default/></mouse>
</labwc_config>
"#),
    ("input_config", r#"<?xml version="1.0"?>
<!-- Keep the rest of this file byte for byte. -->
<labwc_config>
  <mouse><default/><context name="Root"><mousebind button="Right" action="Press"/></context></mouse>
  <libinput>
    <!-- Device names and unknown options must survive. -->
    <device category="default"><leftHanded>no</leftHanded></device>
    <device category="non-touch" pointerSpeed="0.2"><accelProfile>flat</accelProfile><futureOption>yes</futureOption></device>
    <device category="touchpad"><tap>yes</tap><scrollMethod>twofinger</scrollMethod></device>
    <device category="Mouse"><pointerSpeed>1</pointerSpeed></device>
  </libinput>
  <hdrOutputs><output name="DP-2"/></hdrOutputs>
</labwc_config>
"#),
    ("workspaces", r#"<?xml version="1.0"?>
<!-- Preserve all unrelated formatting. -->
<labwc_config>
  <keyboard><keybind key='W-r'><action name='Execute' command='example &amp; test'/></keybind></keyboard>
  <desktops popupTime="500"><names><name>1</name><name>2</name><name>3</name><name>4</name></names></desktops>
  <theme><name>GTK</name></theme>
</labwc_config>
"#),
    ("named_workspaces", "<labwc_config><desktops number=\"8\"><names><name>Work</name><name>Games</name></names><initial>Games</initial><prefix>Desk</prefix></desktops></labwc_config>"),
    ("number_child", "<labwc_config><desktops number=\"4\"><number> 6 </number><initial>9</initial></desktops></labwc_config>"),
    ("empty_root", "<labwc_config/>"),
    ("empty_root_space", "<?xml version=\"1.0\"?>\n<labwc_config />\n"),
    ("empty_file", ""),
    ("whitespace", "  \n\t"),
    ("namespace", "<labwc_config xmlns=\"http://openbox.org/3.4/rc\"><desktops number=\"8\"/><mouse/></labwc_config>"),
    ("namespace_attrs", "<labwc_config xmlns=\"http://openbox.org/3.4/rc\"><libinput><device foo=\"1\" xmlns:x=\"urn:x\" category=\"pad\"><tap>yes</tap></device></libinput></labwc_config>"),
    ("comment_root", "<!-- <labwc_config is a comment --><labwc_config/>"),
    ("crlf", "<?xml version=\"1.0\"?>\r\n<labwc_config>\r\n  <keyboard>\r\n    <repeatDelay>250</repeatDelay>\r\n    <keybind key=\"W-e\"><action name=\"Execute\" command=\"thunar\"/></keybind>\r\n  </keyboard>\r\n</labwc_config>\r\n"),
    ("bom", "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<labwc_config>\n  <keyboard/>\n</labwc_config>\n"),
    ("latin1_decl", "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><labwc_config/>"),
    ("self_closing_keyboard", "<labwc_config>\n  <keyboard repeatRate=\"9\" />\n</labwc_config>\n"),
    ("entities", "<labwc_config><keyboard><keybind key=\"W-&amp;\" onRelease=\"yes\"><action name=\"Execute\"><command>a &lt;b&gt; &#x263A; <![CDATA[c & d]]></command></action></keybind><repeatRate> <!-- x -->25 </repeatRate></keyboard><desktops><names><name>A&amp;B</name></names></desktops></labwc_config>"),
    ("nested_action", "<labwc_config><keyboard><keybind key=\"W-x\"><action name=\"Execute\" command=\"attr\"><command>child</command><extra><deep>t</deep></extra></action><action name=\"ShowMenu\"><menu>root-menu</menu></action></keybind><keybind key=\"C-A-Left\"><action name=\"GoToDesktop\" to=\"left\"/></keybind><keybind key=\"W-S-3\"><action name=\"SendToDesktop\" to=\"3\"/></keybind><keybind key=\"XF86AudioRaiseVolume\"><action name=\"Execute\" command=\"pactl set-sink-volume @DEFAULT_SINK@ +5%\"/></keybind><keybind key=\"W-minus\"><action name=\"None\"/></keybind></keyboard></labwc_config>"),
    ("tiling_block", "<labwc_config><keyboard><keybind key=\"W-Return\"><action name=\"Execute\" command=\"foot\"/></keybind><!-- Tessel Tiling keyboard begin --><keybind key=\"W-q\"><action name=\"Tiling\" command=\"close\"/></keybind><keybind key=\"W-1\"><action name=\"Tiling\" command=\"workspace 1\"/></keybind><keybind key=\"W-y\"><action name=\"Tiling\" command=\"unknown\"/></keybind><!-- Tessel Tiling keyboard end --><keybind key=\"W-slash\"><action name=\"Execute\" command=\"~/.local/bin/tessel-action shortcuts\"/></keybind></keyboard></labwc_config>"),
    ("two_keyboards", "<labwc_config><keyboard/><keyboard/></labwc_config>"),
    ("two_desktops", "<labwc_config><desktops/><desktops/></labwc_config>"),
    ("two_libinput", "<labwc_config><libinput/><libinput/></labwc_config>"),
    ("doctype", "<!DOCTYPE labwc_config><labwc_config/>"),
    ("foreign_root", "<oops/>"),
    ("malformed", "<labwc_config><keyboard></labwc_config>"),
    ("bad_number", "<labwc_config><desktops number=\"x\"/></labwc_config>"),
    ("indented_values", "<labwc_config>\n  <keyboard>\n    <repeatRate>30</repeatRate>\n    <repeatDelay>600</repeatDelay>  \n    <layoutScope>global</layoutScope>\n  </keyboard>\n</labwc_config>\n"),
    ("libinput_selfclosing", "<labwc_config>\n  <libinput/>\n</labwc_config>\n"),
    ("empty_value", "<labwc_config><keyboard><repeatRate/></keyboard></labwc_config>"),
    ("empty_keybind_last", "<labwc_config><keyboard><keybind key=\"A-F4\"/></keyboard><libinput><device category=\"x\"/></libinput></labwc_config>"),
    ("cdata_names", "<labwc_config><desktops number=\"2\"><names><name><![CDATA[1]]></name><name>2</name></names><initial>2</initial></desktops></labwc_config>"),
    ("prolog_tiling_comment", "<!-- Tessel Tiling keyboard begin --><labwc_config><keyboard><keybind key=\"W-b\"><action name=\"Close\"/></keybind></keyboard></labwc_config>"),
    ("quotes_in_tag", "<labwc_config><keyboard><keybind name='a>b' key=\"W-k\"><action name=\"Close\"/></keybind></keyboard></labwc_config>"),
];

fn environments() -> Value {
    json!([
        ["# keep\nXKB_DEFAULT_LAYOUT=se\nFOO=1\n#XKB_DEFAULT_LAYOUT=fr\nXKB_DEFAULT_LAYOUT=us\nXKB_DEFAULT_OPTIONS=x:y",
         [["XKB_DEFAULT_LAYOUT", "us,de"], ["XKB_DEFAULT_OPTIONS", null], ["XKB_DEFAULT_VARIANT", ",nodeadkeys"]]],
        ["", [["TERMINAL", "foot"]]],
        ["A=1", [["B", "2"], ["A", null]]],
        ["export XCURSOR_SIZE = 24\r\nOTHER=x\n\n", [["XCURSOR_SIZE", "48"]]],
        ["A=1\nA=2\n", [["A", "3"]]],
        ["bad-name=1\n  A=1  \n", [["A", null], ["C", ""]]],
        ["A=1\n", [["A", "x\ny"]]],
        ["A=1\n", [["9bad", "1"]]],
        ["A=1\u{2028}B=2\x0cC=3", [["B", "9"]]]
    ])
}

fn keyboard_saves() -> Value {
    json!([
        ["system_pages.xml", "XKB_DEFAULT_LAYOUT=us\nXKB_DEFAULT_OPTIONS=grp:alt_shift_toggle\nOTHER=1\n",
         [[["us", ""], ["de", "nodeadkeys"]], ["grp:alt_shift_toggle", "compose:ralt"], 30, 400]],
        ["empty_root.xml", "", [[["fr", ""]], [], 50, 250]],
        ["empty_file.xml", "XKB_DEFAULT_LAYOUT=us\n", [[["us", ""]], [], 25, 600]],
        ["crlf.xml", "XKB_DEFAULT_LAYOUT=us", [[["us", "dvorak"], ["ru", ""]], ["caps:escape"], 33, 250]],
        ["system_pages.xml", "", [[["us;rm", ""]], [], 25, 600]]
    ])
}

fn night() -> Value {
    json!([
        {"enabled": true, "schedule": "manual", "sunset": "7:05", "sunrise": "06:30", "temperature": 3500},
        {"enabled": false, "schedule": "sun", "sunset": "20:00", "sunrise": "07:00", "temperature": 4000},
        {"enabled": true, "schedule": "sun", "sunset": "25:00", "sunrise": "07:00", "temperature": 4000},
        {"enabled": true, "schedule": "sun", "sunset": "20:00", "sunrise": "07:00", "temperature": 9000},
        {"enabled": true, "schedule": "both", "sunset": "20:00", "sunrise": "07:00", "temperature": 4000}
    ])
}

fn error(e: &Error) -> Vec<u8> {
    let kind = match e { Error::Value(_) => "Value", Error::Runtime(_) => "Runtime", Error::Os(_) => "Os", Error::Xml(_) => "Xml", Error::Glib(_) => "Glib" };
    format!("ERROR:{kind}:{e}").into_bytes()
}

fn save(out: &Path, name: &str, result: Result<Vec<u8>, Error>) {
    std::fs::write(out.join(name), result.unwrap_or_else(|e| error(&e))).unwrap();
}

fn b(v: bool) -> &'static str { if v { "1" } else { "0" } }
fn py_bool(v: bool) -> &'static str { if v { "True" } else { "False" } }

fn keyboard_summary(data: &[u8]) -> Result<Vec<u8>, Error> {
    let section = labwc_settings::read_keyboard(data)?;
    let mut lines = vec![match section.span { Some((a, z)) => format!("span={a},{z}"), None => "span=None".into() },
        format!("open_end={}", section.open_end), format!("self_closing={}", b(section.self_closing)),
        format!("root_end={}", section.root_end), format!("has_default={}", b(section.has_default))];
    let mut names: Vec<&String> = section.values.keys().collect();
    names.sort();
    for name in names {
        let (text, (start, end)) = &section.values[name];
        lines.push(format!("value {name}={text}@{start},{end}"));
    }
    for bind in &section.keybinds {
        lines.push(format!("keybind {}|{}|{}|{}|{}", bind.key, bind.start, bind.end, b(bind.tiling), b(bind.on_release)));
        for action in &bind.actions {
            lines.push(format!("  action {} {}", action.name, action.attributes.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";")));
        }
    }
    Ok((lines.join("\n") + "\n").into_bytes())
}

fn input_summary(data: &[u8]) -> Result<Vec<u8>, Error> {
    let state = input_config::read_state(data)?;
    let mut lines = Vec::new();
    for (key, profile) in &state.profiles {
        lines.push(format!("profile {key}|{}", profile.name));
        lines.extend(profile.values.iter().map(|(k, v)| format!("  {k}={v}")));
        lines.push(format!("  xml={}", profile.xml));
    }
    Ok((lines.join("\n") + "\n").into_bytes())
}

fn workspace_summary(data: &[u8]) -> Result<Vec<u8>, Error> {
    let state = workspace_config::read_state(data)?;
    Ok(format!("count={}\nsignature={}\n", state.count, state.signature).into_bytes())
}

fn shortcuts_summary(data: &[u8]) -> Result<Vec<u8>, Error> {
    let items = shortcuts_model::collect(data, &shortcuts_model::tiling_bindings(), None)?;
    Ok(items.iter().map(|i| format!("{}|{}|{}|{}|{}|{}|{}|{}\n", i.key, i.description,
        i.values.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";"), i.group, i.source, b(i.active), i.start, i.overrides))
        .collect::<String>().into_bytes())
}

fn changes(pairs: &[(&str, Option<&str>)]) -> Option<Vec<(String, Option<String>)>> {
    Some(pairs.iter().map(|(k, v)| (k.to_string(), v.map(str::to_owned))).collect())
}

fn input_updates() -> Vec<Updates> {
    let p = |n: &str, k: &str| (n.to_string(), k.to_string());
    vec![
        vec![(p("non-touch", "non-touch"), changes(&[("pointerSpeed", Some("0.5"))]))],
        vec![(p("touchpad", "touchpad"), changes(&[("tap", Some("no")), ("naturalScroll", Some("yes")), ("clickMethod", Some("clickfinger"))]))],
        vec![(p("Mouse A & B", "non-touch"), changes(&[("leftHanded", Some("yes"))])), (p("non-touch", "non-touch"), changes(&[("pointerSpeed", Some("-1"))]))],
        vec![(p("Mouse", "non-touch"), None)],
        vec![(p("touchpad", "touchpad"), changes(&[("scrollMethod", None), ("tap", None)]))],
        vec![(p("non-touch", "non-touch"), changes(&[("sendEventsMode", Some("no"))]))],
        vec![(p("Pad <x>", "touchpad"), changes(&[("sendEventsMode", Some("disabledOnExternalMouse")), ("scrollFactor", Some("1.5"))]))],
    ]
}

fn keyboard_values() -> Vec<EnvValues> {
    let v = labwc_settings::env_values;
    vec![v(&[("repeatRate", Some("40"))]), v(&[("repeatRate", Some("40")), ("repeatDelay", Some("300"))]), v(&[("repeatDelay", None)]),
         v(&[("repeatRate", None), ("repeatDelay", None)]), v(&[("repeatRate", Some("5 & <6>"))])]
}

fn night_repr(c: &NightLightConfig) -> String {
    format!("NightLightConfig(enabled={}, schedule='{}', sunset='{}', sunrise='{}', temperature={})",
            py_bool(c.enabled), c.schedule, c.sunset, c.sunrise, c.temperature)
}

fn rust_outputs(fixtures: &Path, out: &Path) {
    std::fs::create_dir_all(out).unwrap();
    let mut names: Vec<PathBuf> = std::fs::read_dir(fixtures).unwrap().flatten().map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "xml")).collect();
    names.sort();
    for fixture in names {
        let data = std::fs::read(&fixture).unwrap();
        let stem = fixture.file_stem().unwrap().to_string_lossy().into_owned();
        save(out, &format!("{stem}.keyboard"), keyboard_summary(&data));
        save(out, &format!("{stem}.input"), input_summary(&data));
        save(out, &format!("{stem}.workspace"), workspace_summary(&data));
        save(out, &format!("{stem}.shortcuts"), shortcuts_summary(&data));
        for count in [1, 2, 3, 5, 12, 101] {
            save(out, &format!("{stem}.count{count}"), workspace_config::with_count(&data, count));
        }
        for (index, updates) in input_updates().iter().enumerate() {
            save(out, &format!("{stem}.profiles{index}"),
                 input_config::read_state(&data).and_then(|state| input_config::with_profiles(&data, updates, &state)));
        }
        for (index, values) in keyboard_values().iter().enumerate() {
            save(out, &format!("{stem}.kbvalues{index}"), labwc_settings::set_keyboard_values(&data, values));
        }
        let binds = labwc_settings::read_keyboard(&data).map(|s| s.keybinds).unwrap_or_default();
        for (index, bind) in binds.iter().enumerate() {
            save(out, &format!("{stem}.rebind{index}"), labwc_settings::rebind(&data, bind.start, &bind.key, "W-C-x\t1"));
        }
    }
    for (index, item) in environments().as_array().unwrap().iter().enumerate() {
        let text = item[0].as_str().unwrap();
        let values: EnvValues = item[1].as_array().unwrap().iter()
            .map(|p| (p[0].as_str().unwrap().to_owned(), p[1].as_str().map(str::to_owned))).collect();
        save(out, &format!("env{index}.edit"), labwc_settings::edit_environment(text, &values).map(String::into_bytes));
        let mut parsed: Vec<(String, String)> = labwc_settings::parse_environment(text).into_iter().collect();
        parsed.sort();
        save(out, &format!("env{index}.parse"), Ok(parsed.iter().map(|(k, v)| format!("{k}={v}\n")).collect::<String>().into_bytes()));
    }
    let work = out.join("work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    // SAFETY: single-threaded binary.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", work.join("state"));
        std::env::set_var("XDG_CONFIG_HOME", work.join("config"));
    }
    for (index, item) in keyboard_saves().as_array().unwrap().iter().enumerate() {
        let (rc, env) = (work.join(format!("kb{index}.rc.xml")), work.join(format!("kb{index}.environment")));
        std::fs::write(&rc, std::fs::read(fixtures.join(item[0].as_str().unwrap())).unwrap()).unwrap();
        std::fs::write(&env, item[1].as_str().unwrap()).unwrap();
        let reloads = std::rc::Rc::new(std::cell::Cell::new(0));
        let r = reloads.clone();
        let config = KeyboardConfig::new(Some(&rc), Some(&env), Some(std::rc::Rc::new(move |_| r.set(r.get() + 1))));
        let s = &item[2];
        let state = KeyboardState {
            layouts: s[0].as_array().unwrap().iter().map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into())).collect(),
            options: s[1].as_array().unwrap().iter().map(|o| o.as_str().unwrap().into()).collect(),
            repeat_rate: s[2].as_i64().unwrap(), repeat_delay: s[3].as_i64().unwrap(),
        };
        save(out, &format!("kbsave{index}.result"), config.save(&state, None)
             .map(|(a, z)| format!("({}, {}) reloads={}", py_bool(a), py_bool(z), reloads.get()).into_bytes()));
        save(out, &format!("kbsave{index}.rc"), Ok(std::fs::read(&rc).unwrap()));
        save(out, &format!("kbsave{index}.env"), Ok(std::fs::read(&env).unwrap()));
        let backup = work.join("state/tessel/backups/labwc-rc").join(format!("kb{index}.rc.xml.orig"));
        save(out, &format!("kbsave{index}.rcbackup"), Ok(std::fs::read(&backup).unwrap_or_else(|_| b"(none)".to_vec())));
    }
    for (index, item) in night().as_array().unwrap().iter().enumerate() {
        let path = work.join(format!("night{index}.json"));
        let store = NightLightStore::new(Some(&path), Some(&work.join("weather.json")));
        let config = NightLightConfig { enabled: item["enabled"].as_bool().unwrap(), schedule: item["schedule"].as_str().unwrap().into(),
            sunset: item["sunset"].as_str().unwrap().into(), sunrise: item["sunrise"].as_str().unwrap().into(),
            temperature: item["temperature"].as_i64().unwrap() };
        save(out, &format!("night{index}.result"), store.save(&config).map(|c| night_repr(&c).into_bytes()));
        save(out, &format!("night{index}.file"), Ok(std::fs::read(&path).unwrap_or_else(|_| b"(none)".to_vec())));
        let location = ("X".to_string(), 59.91, 10.75);
        let command = night_light::command(&store.load(), Some(&location), &["DP-1".into()], &["DP-1".into(), "HDMI-A-1".into()], "wlsunset");
        let text = match command {
            None => "None".to_string(),
            Some(argv) => format!("[{}]", argv.iter().map(|a| format!("'{a}'")).collect::<Vec<_>>().join(", ")),
        };
        save(out, &format!("night{index}.command"), Ok(text.into_bytes()));
    }
}

fn main() {
    let root = std::env::var("GS_ROOT").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    if std::env::var("GS_HEADLESS").as_deref() != Ok("1") || root.is_empty() || !home.starts_with(&root) {
        println!("python_parity: skipped (run inside components/settings/tests/headless.sh)");
        return;
    }
    let base = PathBuf::from(std::env::var("TMPDIR").unwrap()).join("parity");
    let _ = std::fs::remove_dir_all(&base);
    let fixtures = base.join("fixtures");
    std::fs::create_dir_all(&fixtures).unwrap();
    for (name, text) in FIXTURES {
        std::fs::write(fixtures.join(format!("{name}.xml")), text).unwrap();
    }
    std::fs::write(fixtures.join("invalid_utf8.xml"), b"<labwc_config>\xff</labwc_config>").unwrap();
    if let Ok(data) = std::fs::read("/usr/share/doc/labwc/rc.xml.all") {
        std::fs::write(fixtures.join("labwc_rc_all.xml"), data).unwrap();
    }
    std::fs::write(fixtures.join("environments.json"), environments().to_string()).unwrap();
    std::fs::write(fixtures.join("keyboard_saves.json"), keyboard_saves().to_string()).unwrap();
    std::fs::write(fixtures.join("night.json"), night().to_string()).unwrap();
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity_driver.py");
    let (py, rs) = (base.join("python"), base.join("rust"));
    let status = std::process::Command::new("python3").arg("-B").arg("-s").arg(&driver).arg(&fixtures).arg(&py).status().expect("python3");
    assert!(status.success(), "parity driver failed");
    rust_outputs(&fixtures, &rs);
    let mut names: Vec<String> = std::fs::read_dir(&py).unwrap().flatten().filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let rust_names: std::collections::BTreeSet<String> = std::fs::read_dir(&rs).unwrap().flatten().filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let mut failures = Vec::new();
    for name in &names {
        let expected = std::fs::read(py.join(name)).unwrap();
        match std::fs::read(rs.join(name)) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => failures.push(format!("{name}\n  python: {:?}\n  rust:   {:?}", String::from_utf8_lossy(&expected),
                                                String::from_utf8_lossy(&actual))),
            Err(_) => failures.push(format!("{name}: missing in Rust output")),
        }
    }
    for name in rust_names.iter().filter(|n| !names.contains(n)) {
        failures.push(format!("{name}: missing in Python output"));
    }
    for failure in &failures {
        println!("MISMATCH {failure}");
    }
    println!("python_parity: {} files compared, {} identical, {} mismatched", names.len(), names.len() - failures.len().min(names.len()), failures.len());
    if !failures.is_empty() {
        std::process::exit(1);
    }
}
