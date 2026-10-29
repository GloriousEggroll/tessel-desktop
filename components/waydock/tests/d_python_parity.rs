//! W-d byte parity with the Python appearance modules.
//!
//! Each test writes fixtures under `$TMPDIR`, runs the Python module with
//! `python3 -I -B` (sys.path → ~/btmanager/waydock, read-only; HOME and every
//! XDG directory point into the fixture root; GSettings and labwc reloads are
//! patched out) and compares the bytes the Rust port produces for the same
//! inputs. Skipped when ~/btmanager/waydock is not present.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use waydock::appearance::{frosted, gtk_color_scheme, labwc_theme, palette_state, papirus, rc_xml, style, wallpaper, window_buttons};

const WAYDOCK: &str = "/home/tcrider/btmanager/waydock";

fn root(name: &str) -> Option<PathBuf> {
    if !Path::new(WAYDOCK).join("palette_state.py").is_file() {
        eprintln!("skipping: {WAYDOCK} not available");
        return None;
    }
    let base = PathBuf::from(std::env::var_os("TMPDIR").expect("TMPDIR must point at the private scratch area"))
        .join(format!("d-parity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home")).unwrap();
    Some(base)
}

/// Run `script` with `request` (JSON) and return its JSON stdout.
fn python(root: &Path, script: &str, request: &Value) -> Value {
    let prelude = format!(
        "import sys, json\nsys.path.insert(0, {WAYDOCK:?})\nsys.path.insert(1, {:?})\nrequest = json.load(open(sys.argv[1]))\n",
        Path::new(WAYDOCK).parent().unwrap().join("shared").to_string_lossy());
    let script_path = root.join("parity.py");
    std::fs::write(&script_path, format!("{prelude}{script}")).unwrap();
    let request_path = root.join("request.json");
    std::fs::write(&request_path, serde_json::to_vec(request).unwrap()).unwrap();
    let home = root.join("home");
    let output = std::process::Command::new("python3").arg("-I").arg("-B").arg(&script_path).arg(&request_path)
        .current_dir(root).env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home).env("XDG_CONFIG_HOME", home.join(".config")).env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share")).env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_RUNTIME_DIR", root).env("TMPDIR", root).env("LANG", "C.UTF-8")
        .output().expect("python3");
    assert!(output.status.success(), "python failed:\n{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("bad python output {e}: {}", String::from_utf8_lossy(&output.stdout)))
}

fn latin1(value: &Value) -> Vec<u8> {
    value.as_str().unwrap().chars().map(|c| c as u32 as u8).collect()
}

fn rgb(value: &Value) -> [u8; 3] {
    std::array::from_fn(|i| value[i].as_u64().unwrap() as u8)
}

#[test]
fn static_dock_css_is_byte_identical() {
    let Some(root) = root("css") else { return };
    let out = python(&root, "import dock_style\nprint(json.dumps({'css': dock_style.CSS.decode('latin-1'), 'light': dock_style.LIGHT_CSS.decode('latin-1'), 'a': dock_style.appearance_css(False).decode('latin-1')}))\n", &json!({}));
    assert_eq!(style::CSS.as_bytes(), latin1(&out["css"]));
    assert_eq!(style::LIGHT_CSS.as_bytes(), latin1(&out["light"]));
    assert_eq!(style::appearance_css(false).as_bytes(), latin1(&out["a"]));
}

#[test]
fn dock_clone_css_matches_python() {
    let Some(root) = root("clonecss") else { return };
    let cases = json!([
        {"dark": true, "t": 30, "w": [40, 60, 80], "kw": {"rounded_corners": false, "inner_rounded_corners": true, "scope": "waydock-clone-1", "border_sides": ["top", "left"], "shine": true}},
        {"dark": false, "t": 0, "w": null, "kw": {"frosted": true, "frost_strength": 70, "scope": "waydock-clone-2", "background_color": "#336699", "border_color": "#AABBCC", "border_sides": ["bottom"]}},
        {"dark": true, "t": 100, "w": [250, 250, 250], "kw": {}},
    ]);
    let popups = json!([
        {"scope": "waydock-clone-1", "dark": true, "w": [190, 22, 66], "kw": {"transparency": 23, "frosted": true, "rounded_corners": false, "frost_strength": 50}},
        {"scope": "waydock-clone-1-applications", "dark": false, "w": null, "kw": {"transparency": 5, "background_color": "#204060", "rounded_corners": true}},
    ]);
    let out = python(&root, "from dock_style import background_css, popup_css\n\
        def t(v): return None if v is None else tuple(v)\n\
        print(json.dumps({'bg': [background_css(c['dark'], c['t'], t(c['w']), **c['kw']).decode() for c in request['cases']],\n\
                          'popup': [popup_css(c['scope'], c['dark'], t(c['w']), **c['kw']).decode() for c in request['popups']]}))\n",
        &json!({"cases": cases, "popups": popups}));
    for (case, expected) in cases.as_array().unwrap().iter().zip(out["bg"].as_array().unwrap()) {
        let kw = &case["kw"];
        let request = style::BackgroundRequest {
            transparency: case["t"].as_i64().unwrap(), wallpaper: case["w"].as_array().map(|_| rgb(&case["w"])),
            rounded_corners: kw.get("rounded_corners").and_then(Value::as_bool).unwrap_or(true),
            inner_rounded_corners: kw.get("inner_rounded_corners").and_then(Value::as_bool),
            frosted: kw.get("frosted").and_then(Value::as_bool).unwrap_or(false),
            frost_strength: kw.get("frost_strength").and_then(Value::as_i64).unwrap_or(50),
            scope: kw.get("scope").and_then(Value::as_str).map(str::to_owned),
            background_color: kw.get("background_color").and_then(Value::as_str).map(str::to_owned),
            border_sides: kw.get("border_sides").and_then(Value::as_array).map(|a| a.iter().map(|s| s.as_str().unwrap().to_owned()).collect()).unwrap_or_default(),
            border_color: kw.get("border_color").and_then(Value::as_str).map(str::to_owned),
            shine: kw.get("shine").and_then(Value::as_bool).unwrap_or(false),
        };
        assert_eq!(style::background_css(case["dark"].as_bool().unwrap(), &request).unwrap(), expected.as_str().unwrap(), "{case}");
    }
    for (case, expected) in popups.as_array().unwrap().iter().zip(out["popup"].as_array().unwrap()) {
        let kw = &case["kw"];
        let css = style::popup_css(case["scope"].as_str().unwrap(), case["dark"].as_bool().unwrap(),
                                   case["w"].as_array().map(|_| rgb(&case["w"])), kw["transparency"].as_i64().unwrap(),
                                   kw.get("frosted").and_then(Value::as_bool).unwrap_or(false),
                                   kw.get("rounded_corners").and_then(Value::as_bool).unwrap_or(true),
                                   kw.get("frost_strength").and_then(Value::as_i64).unwrap_or(50),
                                   kw.get("background_color").and_then(Value::as_str)).unwrap();
        assert_eq!(css, expected.as_str().unwrap(), "{case}");
    }
}

fn monitor_cases() -> Value {
    json!([
        {"dark": true, "t": 35, "monitors": [
            {"name": "DP-1", "geometry": [0, 0, 1920, 1080], "background": [20, 24, 29], "foreground": [240, 241, 243], "accent": [70, 140, 250]},
            {"name": "HDMI-A-1", "geometry": [-2560, 0, 2560, 1440], "background": [30, 20, 25], "foreground": [245, 238, 243], "accent": [180, 95, 140]}],
         "kw": {}},
        {"dark": false, "t": 2.5, "monitors": [{"name": "Écran \"1\" ✓", "geometry": [0, 0, 1, 1], "background": [1, 2, 3], "foreground": [4, 5, 6], "accent": [7, 8, 9], "frost_texture": "/tmp/a.png"}],
         "kw": {"apps_transparency": 37.5, "apps_transparency_enabled": true, "apps_frosted": true, "apps_frost_strength": 101, "apps_rounded_corners": false, "colored_window_buttons": true}},
        {"dark": true, "t": -4, "monitors": [], "kw": {"apps_transparency": 3.5}},
    ])
}

#[test]
fn palette_json_bytes_match_python() {
    let Some(root) = root("palette") else { return };
    let cases = monitor_cases();
    let out = python(&root, "from palette_state import PaletteState\nresult = []\n\
        for i, c in enumerate(request['cases']):\n\
        \x20   path = f'py-{i}.json'\n\
        \x20   PaletteState(path).publish(c['dark'], c['t'], c['monitors'], **c['kw'])\n\
        \x20   result.append(open(path, 'rb').read().decode('latin-1'))\n\
        print(json.dumps(result))\n", &json!({"cases": cases}));
    for (index, (case, expected)) in cases.as_array().unwrap().iter().zip(out.as_array().unwrap()).enumerate() {
        let kw = &case["kw"];
        let defaults = palette_state::AppOptions::default();
        let options = palette_state::AppOptions {
            apps_transparency: kw.get("apps_transparency").and_then(Value::as_f64).unwrap_or(defaults.apps_transparency),
            apps_transparency_enabled: kw.get("apps_transparency_enabled").and_then(Value::as_bool).unwrap_or(false),
            apps_frosted: kw.get("apps_frosted").and_then(Value::as_bool).unwrap_or(false),
            apps_frost_strength: kw.get("apps_frost_strength").and_then(Value::as_f64).unwrap_or(50.0),
            apps_rounded_corners: kw.get("apps_rounded_corners").and_then(Value::as_bool).unwrap_or(true),
            colored_window_buttons: kw.get("colored_window_buttons").and_then(Value::as_bool).unwrap_or(false),
        };
        let path = root.join(format!("rs-{index}.json"));
        let mut state = palette_state::PaletteState::new(Some(path.clone()));
        assert!(state.publish(case["dark"].as_bool().unwrap(), case["t"].as_f64().unwrap(), case["monitors"].as_array().unwrap(), &options));
        assert_eq!(std::fs::read(&path).unwrap(), latin1(expected), "case {index}");
        // A restart over Python's file must not rewrite it.
        let python_file = root.join(format!("py-{index}.json"));
        let mut restarted = palette_state::PaletteState::new(Some(python_file.clone()));
        let before = std::fs::metadata(&python_file).unwrap().modified().unwrap();
        assert!(!restarted.publish(case["dark"].as_bool().unwrap(), case["t"].as_f64().unwrap(), case["monitors"].as_array().unwrap(), &options));
        assert_eq!(std::fs::metadata(&python_file).unwrap().modified().unwrap(), before);
    }
}

#[test]
fn waydock_colors_css_matches_python() {
    let Some(root) = root("colors") else { return };
    let palettes = json!([
        {"dark": true, "p": {"background": [26, 21, 35], "foreground": [242, 239, 248], "accent": [157, 109, 222]}, "colored": false},
        {"dark": false, "p": {"background": [242, 236, 249], "foreground": [35, 29, 44], "accent": [117, 71, 182]}, "colored": true},
        {"dark": true, "p": {"background": [128, 128, 128], "foreground": [130, 130, 130], "accent": [128, 128, 128]}, "colored": false},
        {"dark": false, "p": {"background": [255, 255, 255], "foreground": [0, 0, 0], "accent": [255, 255, 0]}, "colored": true},
    ]);
    let out = python(&root, "from gtk_color_scheme import color_css\nprint(json.dumps([color_css(c['dark'], c['p'], c['colored']).decode() for c in request]))\n", &palettes);
    for (case, expected) in palettes.as_array().unwrap().iter().zip(out.as_array().unwrap()) {
        let palette = gtk_color_scheme::palette_from_value(Some(&case["p"])).unwrap();
        let css = gtk_color_scheme::color_css(case["dark"].as_bool().unwrap(), &palette, case["colored"].as_bool().unwrap());
        assert_eq!(css, expected.as_str().unwrap().as_bytes(), "{case}");
    }
}

fn read_dir_bytes(directory: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(directory).map(|entries| entries.flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap_or_default())).collect()).unwrap_or_default();
    files.sort();
    files
}

#[test]
fn gtk_css_edits_match_python() {
    let Some(root) = root("gtkcss") else { return };
    let originals: Vec<&[u8]> = vec![b"", b"/* no final newline */", b"/* LF */\n", b"/* CRLF */\r\nlabel {}\r\n",
        b"\xef\xbb\xbf", b"\xef\xbb\xbf/* BOM */", b"@import url(\"waydock-colors.css\"); /* Waydock wallpaper colors */\nbutton {}",
        b"x\n@import url(\"waydock-colors.css\"); /* Waydock wallpaper colors */\n@import url(\"waydock-colors.css\"); /* Waydock wallpaper colors */\n",
        b"a\rb\r", b"tail\n/* Waydock separator: original stylesheet has no final newline. */\n@import url(\"waydock-colors.css\"); /* Waydock wallpaper colors */\n"];
    let palette = json!({"background": [26, 21, 35], "foreground": [242, 239, 248], "accent": [157, 109, 222]});
    for (index, original) in originals.iter().enumerate() {
        for side in ["py", "rs"] {
            let directory = root.join(format!("{side}-{index}/gtk-4.0"));
            std::fs::create_dir_all(&directory).unwrap();
            if index != 0 {
                std::fs::write(directory.join("gtk.css"), original).unwrap();
            }
        }
    }
    let out = python(&root, "from gtk_color_scheme import GtkColorScheme\nresult = []\n\
        for i in range(request['count']):\n\
        \x20   state = GtkColorScheme(f'py-{i}')\n\
        \x20   steps = [state.publish(True, True, request['palette'], False), state.publish(True, False, request['palette'], True),\n\
        \x20            state.publish(True, False, request['palette'], True)]\n\
        \x20   enabled = open(state.path, 'rb').read().decode('latin-1')\n\
        \x20   steps.append(state.publish(False, True, None))\n\
        \x20   result.append({'steps': steps, 'enabled': enabled, 'error': state.error})\n\
        print(json.dumps(result))\n", &json!({"count": originals.len(), "palette": palette}));
    for index in 0..originals.len() {
        let mut state = gtk_color_scheme::GtkColorScheme::new(Some(root.join(format!("rs-{index}"))));
        let steps = [state.publish(true, true, Some(&palette), false), state.publish(true, false, Some(&palette), true),
                     state.publish(true, false, Some(&palette), true)];
        let enabled = std::fs::read(&state.path).unwrap();
        let disabled = state.publish(false, true, None, false);
        let expected = &out[index];
        assert_eq!(json!([steps[0], steps[1], steps[2], disabled]), expected["steps"], "case {index}");
        assert_eq!(enabled, latin1(&expected["enabled"]), "case {index}");
        assert_eq!(state.error, None);
        assert_eq!(read_dir_bytes(&root.join(format!("rs-{index}/gtk-4.0"))), read_dir_bytes(&root.join(format!("py-{index}/gtk-4.0"))), "case {index}");
    }
}

const RC: &str = "<?xml version=\"1.0\"?>
<labwc_config>
  <!-- keep me -->
  <theme>
    <name>GTK</name>
    <titleLayout>NLIMC</titleLayout>
    <active>
      <titlebar color=\"#282a36\"/>
    </active>
  </theme>
  <desktops number=\"4\"/>
</labwc_config>
";

#[test]
fn rc_xml_edits_match_python() {
    let Some(root) = root("rcxml") else { return };
    let documents = vec![
        RC.to_owned(),
        "<labwc_config>\n</labwc_config>\n".to_owned(),
        "<labwc_config><theme/></labwc_config>".to_owned(),
        "<labwc_config>\r\n  <Theme a='1\"' b=\"x&#10;y\tz\">\r\n    <titlebar><layout> old </layout><layout>2</layout></titlebar>\r\n    <dropShadows>yes</dropShadows>\r\n  </Theme>\r\n  <theme><name>second</name></theme>\r\n</labwc_config>\r\n".to_owned(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- c --><labwc_config><theme>t&amp;\"q\"&gt;&#x41;<!-- c --><?pi data?><x  /><y></y><![CDATA[c<d]]>\n<titlebar><layout>a&lt;b</layout></titlebar></theme><x><theme/></x></labwc_config>".to_owned(),
        "<labwc_config>\n  <theme>\n    <name>Ünïcode ✓</name>\n    <font place=\"ActiveWindow\"><name>sans</name></font>\n  </theme>\n</labwc_config>".to_owned(),
        "<!DOCTYPE labwc_config><labwc_config/>".to_owned(),
        "<labwc_config><theme></labwc_config>".to_owned(),
        "<other/>".to_owned(),
    ];
    let edits = json!([[["titlebar", "layout"], "close,iconify,max:"], [["dropShadows"], "no"], [["titlebar", "layout"], "a&b<c>\"d\""]]);
    let out = python(&root, "from window_buttons import theme_value, with_theme_value\nresult = []\n\
        for doc in request['docs']:\n\
        \x20   data = doc.encode()\n\
        \x20   row = []\n\
        \x20   for path, value in request['edits']:\n\
        \x20       try: row.append({'value': theme_value(data, tuple(path))})\n\
        \x20       except Exception as e: row.append({'value_error': True})\n\
        \x20       try: row.append({'edit': with_theme_value(data, tuple(path), value).decode('latin-1')})\n\
        \x20       except Exception as e: row.append({'edit_error': True})\n\
        \x20   result.append(row)\n\
        print(json.dumps(result))\n", &json!({"docs": documents, "edits": edits}));
    for (document, expected) in documents.iter().zip(out.as_array().unwrap()) {
        let mut row = Vec::new();
        for edit in edits.as_array().unwrap() {
            let path: Vec<&str> = edit[0].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
            row.push(match rc_xml::theme_value(document.as_bytes(), &path) {
                Ok(value) => json!({"value": value}),
                Err(_) => json!({"value_error": true}),
            });
            row.push(match rc_xml::with_theme_value(document.as_bytes(), &path, edit[1].as_str().unwrap()) {
                Ok(data) => json!({"edit": data.iter().map(|b| *b as char).collect::<String>()}),
                Err(_) => json!({"edit_error": true}),
            });
        }
        assert_eq!(Value::Array(row), *expected, "{document}");
    }
}

#[test]
fn ini_edits_match_python() {
    let Some(root) = root("ini") else { return };
    let texts = json!(["[Settings]\na=1\ngtk-decoration-layout=x\n", "[Settings]\na=1\n", "a=1\n", "",
                       "[Settings]\ngtk-decoration-layout = old\ngtk-decoration-layout=dup\n", "x\n[Settings]\n[Settings]\n",
                       "[Settings]\ngtk-decoration-layout\n=split\n"]);
    let out = python(&root, "from window_buttons import with_ini_value\nprint(json.dumps([with_ini_value(t, 'gtk-decoration-layout', 'close,minimize,maximize:') for t in request]))\n", &texts);
    for (text, expected) in texts.as_array().unwrap().iter().zip(out.as_array().unwrap()) {
        assert_eq!(window_buttons::with_ini_value(text.as_str().unwrap(), "gtk-decoration-layout", "close,minimize,maximize:"),
                   expected.as_str().unwrap(), "{text}");
    }
}

fn palette(value: &Value) -> suite_gtk::palette::Palette {
    suite_gtk::palette::Palette { background: rgb(&value["background"]), foreground: rgb(&value["foreground"]), accent: rgb(&value["accent"]) }
}

#[test]
fn labwc_themerc_and_svg_buttons_match_python() {
    let Some(root) = root("labwc") else { return };
    let cases = json!([
        {"dark": true, "p": null}, {"dark": false, "p": null},
        {"dark": true, "p": {"background": [7, 9, 14], "foreground": [238, 238, 238], "accent": [119, 122, 130]}},
        {"dark": false, "p": {"background": [239, 209, 219], "foreground": [35, 38, 45], "accent": [186, 22, 65]}},
    ]);
    let themerc = "window.active.title.bg.color: #222226\n  window.inactive.title.bg.color : #222226\r\nmenu.items.bg.color: #3584e4\nwindow.font: sans-serif-20:pixels\nwindow.active.title.bg.color: #000000\n\x0bodd\u{2028}line";
    let out = python(&root, "import labwc_titlebar_theme as t\nresult = []\n\
        for c in request['cases']:\n\
        \x20   colors = t.titlebar_colors(c['dark'], c['p'])\n\
        \x20   result.append({'values': list(t.themerc_values(colors).items()), 'plain': list(t.button_files(False, colors).items()),\n\
        \x20                  'colored': list(t.button_files(True, colors).items()), 'themerc': t.updated_themerc(request['themerc'], colors)})\n\
        print(json.dumps(result))\n", &json!({"cases": cases, "themerc": themerc}));
    for (case, expected) in cases.as_array().unwrap().iter().zip(out.as_array().unwrap()) {
        let p = case["p"].is_object().then(|| palette(&case["p"]));
        let colors = labwc_theme::titlebar_colors(case["dark"].as_bool().unwrap(), p.as_ref());
        let pairs = |items: Vec<(String, String)>| Value::Array(items.into_iter().map(|(k, v)| json!([k, v])).collect());
        assert_eq!(pairs(labwc_theme::themerc_values(&colors)), expected["values"], "{case}");
        assert_eq!(pairs(labwc_theme::button_files(false, &colors)), expected["plain"], "{case}");
        assert_eq!(pairs(labwc_theme::button_files(true, &colors)), expected["colored"], "{case}");
        assert_eq!(labwc_theme::updated_themerc(themerc, &colors), expected["themerc"].as_str().unwrap(), "{case}");
    }
}

fn write_tree(base: &Path, rc: &str) {
    let config = base.join("home/.config");
    std::fs::create_dir_all(config.join("labwc")).unwrap();
    std::fs::write(config.join("labwc/rc.xml"), rc).unwrap();
    for version in ["gtk-3.0", "gtk-4.0"] {
        std::fs::create_dir_all(config.join(version)).unwrap();
    }
    std::fs::write(config.join("gtk-3.0/settings.ini"), "[Settings]\r\ngtk-theme-name=Adwaita\r\ngtk-decoration-layout=menu:close\r\n").unwrap();
    std::fs::write(config.join("gtk-4.0/settings.ini"), "gtk-font-name=Sans 10\n").unwrap();
    let theme = base.join("home/.local/share/themes/Suite/openbox-3");
    std::fs::create_dir_all(&theme).unwrap();
    std::fs::write(theme.join("themerc"), "window.active.title.bg.color: #222226\nwindow.font: sans\n").unwrap();
    std::fs::write(theme.join("close-active.svg"), "old").unwrap();
}

#[test]
fn window_buttons_apply_writes_the_same_files_as_python() {
    let Some(root) = root("buttons") else { return };
    let rc = RC.replace("<name>GTK</name>", "<name>Suite</name>");
    let python_root = root.join("py");
    std::fs::create_dir_all(&python_root).unwrap();
    write_tree(&python_root, &rc);
    let rust_root = root.join("rs");
    write_tree(&rust_root, &rc);
    let dark = json!({"background": [42, 43, 46], "foreground": [238, 238, 238], "accent": [120, 174, 237]});
    // Python reads XDG paths from its environment: run it inside its own tree.
    let out = python(&python_root, "from window_buttons import WindowButtons\n\
        WindowButtons._gsettings = staticmethod(lambda left: False)\n\
        reloads = []\n\
        buttons = WindowButtons(reconfigure=lambda: reloads.append(1))\n\
        buttons.set_palette(True, request['palette'])\n\
        first = buttons.apply(True, True, False)\n\
        again = buttons.apply(True, True, False)\n\
        buttons.close()\n\
        print(json.dumps({'first': first, 'again': again, 'reloads': len(reloads), 'error': buttons.error}))\n", &json!({"palette": dark}));
    let reloads = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = reloads.clone();
    struct NoGsettings;
    impl window_buttons::ButtonLayoutStore for NoGsettings {
        fn get(&self) -> Option<String> { None }
        fn set(&self, _: &str) -> Result<(), String> { unreachable!() }
    }
    let home = rust_root.join("home");
    let buttons = window_buttons::WindowButtons::new(window_buttons::WindowButtonsOptions {
        config_home: Some(home.join(".config")), data_home: Some(home.join(".local/share")),
        palette_path: Some(home.join(".cache/waydock/palette.json")),
        reconfigure: Some(Box::new(move || counter.set(counter.get() + 1))),
        scheduler: Some(std::rc::Rc::new(suite_core::scheduler::FakeScheduler::default())),
        button_layout: Some(Box::new(NoGsettings)), labwc_rc: None,
    });
    buttons.set_palette(true, Some(&palette(&dark)));
    let first = buttons.apply(true, true, false);
    let again = buttons.apply(true, true, false);
    buttons.close();
    assert_eq!(json!({"first": first, "again": again, "reloads": reloads.get(), "error": buttons.error()}), out);
    for relative in [".config/labwc", ".config/gtk-3.0", ".config/gtk-4.0", ".local/share/themes/Suite/openbox-3"] {
        assert_eq!(read_dir_bytes(&home.join(relative)), read_dir_bytes(&python_root.join("home").join(relative)), "{relative}");
    }
}

fn gradient_png(path: &Path, width: i32, height: i32, alpha: bool, seed: u32) {
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
    let pixbuf = Pixbuf::new(Colorspace::Rgb, alpha, 8, width, height).unwrap();
    let channels = if alpha { 4 } else { 3 };
    let stride = pixbuf.rowstride() as usize;
    let mut data = vec![0u8; stride * height as usize];
    let mut state = seed;
    for y in 0..height as usize {
        for x in 0..width as usize {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            let noise = (state >> 16) as u8 % 24;
            let o = y * stride + x * channels;
            data[o] = ((x * 255 / width as usize) as u8).wrapping_add(noise);
            data[o + 1] = (y * 200 / height as usize) as u8;
            data[o + 2] = (((x + y) * 97) % 256) as u8 / 2 + noise;
            if alpha {
                data[o + 3] = if x < 8 { 0 } else { 255 - (y % 64) as u8 };
            }
        }
    }
    let bytes = glib::Bytes::from(&data);
    let pixbuf = Pixbuf::from_bytes(&bytes, Colorspace::Rgb, alpha, 8, width, height, stride as i32);
    let format = if path.extension().is_some_and(|e| e == "jpg") { "jpeg" } else { "png" };
    pixbuf.savev(path, format, &[]).unwrap();
}

#[test]
fn wallpaper_sampling_matches_python() {
    let Some(root) = root("sampling") else { return };
    let mut files = Vec::new();
    for (index, (width, height, alpha)) in [(640, 360, false), (333, 777, true), (64, 64, false), (1920, 1080, true), (17, 3, false)].into_iter().enumerate() {
        for extension in ["png", "jpg"] {
            if alpha && extension == "jpg" {
                continue;
            }
            let path = root.join(format!("image-{index}.{extension}"));
            gradient_png(&path, width, height, alpha, index as u32 + 7);
            files.push(path);
        }
    }
    if let Some(ffmpeg) = suite_core::shell::which("ffmpeg") {
        for (name, source) in [("clip.mkv", "testsrc=s=160x90:r=10"), ("clip.webm", "smptebars=s=128x72:r=10"), ("clip.gif", "color=c=0x3366aa:s=64x48:r=5")] {
            let path = root.join(name);
            let mut command = std::process::Command::new(&ffmpeg);
            command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", source, "-t", "1"]);
            if name.ends_with(".mkv") {
                command.args(["-c:v", "ffv1"]);
            }
            if command.arg(&path).stdin(std::process::Stdio::null()).status().is_ok_and(|s| s.success()) {
                files.push(path);
            }
        }
    }
    let colors = json!(["red", "#ff0055", "rgb(18,36,54)", "rgba(10,20,30,0.5)", "rgba(0,0,0,0)", "hsl(120, 50%, 40%)", "transparent",
                        "invalid", "#abc", "#80808080", "  #102030  ", "currentColor", 5, null]);
    let names: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let out = python(&root, "from pathlib import Path\nimport wallpaper_palette as w\n\
        def color(path):\n\
        \x20   try: return list(w.media_color(Path(path)) or []) or None\n\
        \x20   except Exception as e: return 'error'\n\
        print(json.dumps({'media': [color(p) for p in request['files']],\n\
        \x20                 'css': [list(c) if (c := w.configured_color(v)) else None for v in request['colors']]}))\n",
        &json!({"files": names, "colors": colors}));
    let sampler = wallpaper::MediaSampler::default();
    for (path, expected) in files.iter().zip(out["media"].as_array().unwrap()) {
        let actual = match sampler.media_color(path) {
            Ok(Some(color)) => json!(color),
            Ok(None) => Value::Null,
            Err(_) => json!("error"),
        };
        assert_eq!(actual, *expected, "{}", path.display());
    }
    for (value, expected) in colors.as_array().unwrap().iter().zip(out["css"].as_array().unwrap()) {
        assert_eq!(wallpaper::configured_color(Some(value)).map(|c| json!(c)).unwrap_or(Value::Null), *expected, "{value}");
    }
}

#[test]
fn wallpaper_reader_results_match_python() {
    let Some(root) = root("reader") else { return };
    let image = root.join("wall.png");
    gradient_png(&image, 300, 200, false, 3);
    let config = root.join("wallpapers.json");
    std::fs::write(&config, serde_json::to_vec(&json!({
        "DP-1": {"file_path": image, "mode": "Fill"},
        "DP-2": {"file_path": image, "mode": "Color Only", "color": "#204060"},
        "HDMI-A-1": {"file_path": root.join("movie.mp4"), "color": "rgb(50,40,30)"},
        "DP-3": {"file_path": "relative.png", "color": "blue"},
        "DP-4": {"file_path": root.join("missing.png")},
        "_view_prefs": {"hide_icons": true},
        "DP-5": "not a dict",
    })).unwrap()).unwrap();
    let out = python(&root, "from pathlib import Path\nfrom wallpaper_palette import PaletteReader\n\
        r = PaletteReader(Path(request['config'])).read()\n\
        print(json.dumps({'colors': {k: list(v) for k, v in r.colors.items()}, 'sources': {k: (str(v) if v else None) for k, v in r.sources.items()},\n\
        \x20                 'error_names': [part.split(':')[0] for part in (r.error or '').split('; ') if part]}))\n",
        &json!({"config": config}));
    use waydock::appearance::wallpaper::WallpaperReader;
    let result = wallpaper::PaletteReader::new(config, None, false, None).read(false);
    let colors: serde_json::Map<String, Value> = result.colors.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let sources: serde_json::Map<String, Value> = result.sources.iter()
        .map(|(k, v)| (k.clone(), v.as_ref().map(|p| json!(p)).unwrap_or(Value::Null))).collect();
    assert_eq!(Value::Object(colors), out["colors"]);
    assert_eq!(Value::Object(sources), out["sources"]);
    let names: Vec<Value> = result.error.unwrap_or_default().split("; ").filter(|p| !p.is_empty())
        .map(|p| json!(p.split(':').next().unwrap())).collect();
    assert_eq!(Value::Array(names), out["error_names"]);
}

#[test]
fn frost_glass_pipeline_matches_pillow() {
    let Some(root) = root("pillow") else { return };
    if std::process::Command::new("python3").args(["-I", "-B", "-c", "import PIL"]).status().map_or(true, |s| !s.success()) {
        return;
    }
    let mut rgb = Vec::new();
    let (width, height) = (257u32, 131u32);
    let mut state = 99u32;
    for y in 0..height {
        for x in 0..width {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            rgb.extend_from_slice(&[(x % 256) as u8, if (x / 40 + y / 30) % 2 == 0 { 230 } else { 20 }, (state >> 24) as u8]);
        }
    }
    std::fs::write(root.join("input.rgb"), &rgb).unwrap();
    let out = python(&root, "from PIL import Image, ImageEnhance, ImageFilter\n\
        w, h = request['size']\n\
        image = Image.frombytes('RGB', (w, h), open('input.rgb', 'rb').read())\n\
        result = {}\n\
        for radius in request['radii']:\n\
        \x20   rendered = image.filter(ImageFilter.GaussianBlur(radius=radius))\n\
        \x20   result[str(radius)] = rendered.tobytes().decode('latin-1')\n\
        full = image.filter(ImageFilter.GaussianBlur(radius=10 * max(image.size) / 1024))\n\
        full = ImageEnhance.Color(full).enhance(0.82)\n\
        full = Image.blend(full, Image.new('RGB', full.size, (193, 206, 213)), 0.09)\n\
        result['frost'] = full.tobytes().decode('latin-1')\n\
        print(json.dumps(result))\n", &json!({"size": [width, height], "radii": [0.5, 2.5, 7.3, 40]}));
    let image = frosted::Image::from_rgb(width, height, &rgb);
    for radius in [0.5f32, 2.5, 7.3, 40.0] {
        let key = if radius == 40.0 { "40".to_owned() } else { format!("{radius}") };
        assert_eq!(frosted::gaussian_blur(&image, radius).rgb_bytes(), latin1(&out[&key]), "radius {radius}");
    }
    assert_eq!(frosted::frost(&image).rgb_bytes(), latin1(&out["frost"]));
}

#[test]
fn papirus_folder_matching_matches_python() {
    let Some(root) = root("papirus") else { return };
    let theme = root.join("Papirus");
    let places = theme.join("48x48/places");
    std::fs::create_dir_all(&places).unwrap();
    let variants = [("blue", "#5294e2"), ("red", "#e25252"), ("violet", "#7e57c2"), ("teal", "#16a085"), ("yellow", "#f9bd30"),
                    ("grey", "#8e8e8e"), ("pink", "#f06292"), ("orange", "#ee923a"), ("black", "#4f4f4f"), ("bad.name", "#000000")];
    for (name, color) in variants {
        let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><path style=\"fill:#303030\"/><rect width=\"36\" height=\"16\" fill=\"#eeeeee\"/>\
                           <rect width=\"40\" height=\"26\" style=\"fill:{color};opacity:0.95\"/><path style=\"opacity:0.1;fill:#ffffff\"/></svg>");
        std::fs::write(places.join(format!("folder-{name}.svg")), &svg).unwrap();
        std::fs::write(places.join(format!("folder-{name}-documents.svg")), &svg).unwrap();
    }
    std::fs::write(places.join("folder-broken.svg"), "<svg").unwrap();
    std::fs::write(places.join("folder-broken-documents.svg"), "").unwrap();
    let accents = json!([[255, 34, 34], [120, 174, 237], [36, 112, 210], [186, 22, 65], [25, 160, 55], [128, 128, 128], [250, 250, 252], [0, 0, 0], [240, 200, 40]]);
    let out = python(&root, "from papirus_palette import available_colors, nearest_color\n\
        colors = available_colors(request['theme'])\n\
        print(json.dumps({'colors': {k: list(v) for k, v in colors.items()}, 'nearest': [nearest_color(tuple(a), colors) for a in request['accents']]}))\n",
        &json!({"theme": theme, "accents": accents}));
    let colors = papirus::available_colors(&theme);
    let actual: serde_json::Map<String, Value> = colors.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    assert_eq!(Value::Object(actual), out["colors"]);
    for (accent, expected) in accents.as_array().unwrap().iter().zip(out["nearest"].as_array().unwrap()) {
        assert_eq!(papirus::nearest_color(rgb(accent), &colors).unwrap(), expected.as_str().unwrap(), "{accent}");
    }
}

#[test]
fn primary_output_selection_matches_python() {
    let Some(root) = root("primary") else { return };
    let begin = gtk_color_scheme::PRIMARY_BEGIN;
    let end = gtk_color_scheme::PRIMARY_END;
    let environments = vec![
        format!("SDL_VIDEO_DISPLAY_PRIORITY=DP-1\n{begin}\nSDL_VIDEO_DISPLAY_PRIORITY='HDMI-A-1'\nWAYLANDDRV_PRIMARY_MONITOR=DP-1\n{end}\n"),
        format!("{begin}\nPROTON_WAYLAND_MONITOR=\"DP-2\" # comment\nWAYLANDDRV_PRIMARY_MONITOR=nope\n{end}\n"),
        format!("{begin}\nSDL_VIDEO_DISPLAY_PRIORITY=$(touch /never)\n{end}\n"),
        format!("{end}\n{begin}\nSDL_VIDEO_DISPLAY_PRIORITY=DP-2\n"),
        format!("{begin}\r\n SDL_VIDEO_DISPLAY_PRIORITY=DP-2\r\nWAYLANDDRV_PRIMARY_MONITOR='unterminated\r\n{end}\r\n"),
        "no block".to_owned(),
    ];
    let monitors = json!([{"name": "DP-1"}, {"name": "HDMI-A-1"}, {"name": "DP-2"}]);
    let out = python(&root, "from gtk_color_scheme import GtkColorScheme\nimport os\nresult = []\n\
        for i, text in enumerate(request['envs']):\n\
        \x20   os.makedirs(f'py-{i}/labwc')\n\
        \x20   open(f'py-{i}/labwc/environment', 'w').write(text)\n\
        \x20   result.append(GtkColorScheme(f'py-{i}').select_palette(request['monitors'])['name'])\n\
        print(json.dumps(result))\n", &json!({"envs": environments, "monitors": monitors}));
    for (index, (text, expected)) in environments.iter().zip(out.as_array().unwrap()).enumerate() {
        let base = root.join(format!("rs-{index}"));
        std::fs::create_dir_all(base.join("labwc")).unwrap();
        std::fs::write(base.join("labwc/environment"), text).unwrap();
        let mut state = gtk_color_scheme::GtkColorScheme::new(Some(base));
        let entries = monitors.as_array().unwrap().clone();
        assert_eq!(state.select_palette(&entries).unwrap()["name"], *expected, "{text:?}");
    }
}
