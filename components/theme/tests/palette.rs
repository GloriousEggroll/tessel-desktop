//! Port of test_wayshell_theme.py `PaletteTests` (pure; plain `cargo test`).

use serde_json::{json, Value};
use std::path::PathBuf;
use tessel_theme::{monitor_palette, palette_css, read_palette, Colors, CssOptions, Palette};

fn snapshot() -> Value {
    json!({
        "version": 1, "is_dark": true, "transparency": 20,
        "monitors": [
            {"name": "HEADLESS-1", "geometry": [0, 0, 1280, 720],
             "background": [18, 25, 35], "foreground": [222, 228, 240], "accent": [105, 167, 255]},
            {"name": "HEADLESS-2", "geometry": [1280, 0, 1280, 720],
             "background": [35, 20, 25], "foreground": [235, 225, 220], "accent": [200, 100, 150]},
        ],
    })
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("tessel-theme-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn parsed(value: &Value) -> Palette {
    tessel_theme::parse_palette(value.to_string().as_bytes()).unwrap()
}

fn first_colors() -> Colors {
    parsed(&snapshot()).monitors[0].colors
}

fn css(class: &str, colors: &Colors, amount: i64, options: CssOptions) -> String {
    palette_css(class, colors, amount, &options).unwrap()
}

#[test]
fn test_valid_snapshot_and_malformed_cache() {
    let dir = scratch("valid");
    let path = dir.join("palette.json");
    assert!(read_palette(&path).is_none());
    std::fs::write(&path, snapshot().to_string()).unwrap();
    let mut expected = snapshot();
    for (key, value) in [("apps_transparency_enabled", json!(false)), ("apps_transparency", json!(5)),
                         ("apps_frosted", json!(false)), ("apps_rounded_corners", json!(true)),
                         ("apps_frost_strength", json!(50))] {
        expected[key] = value;
    }
    for monitor in expected["monitors"].as_array_mut().unwrap() {
        monitor["frost_texture"] = Value::Null;
    }
    assert_eq!(read_palette(&path).unwrap().value, expected);
    for (key, invalid) in [("version", json!(9)), ("apps_transparency", json!(-1)), ("is_dark", json!("yes")),
                           ("apps_transparency", json!(101)), ("apps_transparency", json!(true)),
                           ("apps_transparency", json!(10.5)), ("apps_transparency_enabled", json!(1)),
                           ("apps_transparency_enabled", json!("yes")),
                           ("apps_frosted", json!(1)), ("apps_rounded_corners", json!("true")),
                           ("apps_frost_strength", json!(true)), ("apps_frost_strength", json!(-1)),
                           ("apps_frost_strength", json!(101)), ("apps_frost_strength", json!(20.5)),
                           ("monitors", json!([]))] {
        let mut value = snapshot();
        value[key] = invalid;
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(read_palette(&path).is_none(), "{key}");
    }
    let mut value = snapshot();
    value["monitors"][0]["background"] = json!([1, 2, "red; }"]);
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(read_palette(&path).is_none());
    // Oversized caches are ignored.
    std::fs::write(&path, format!("{}{}", snapshot(), " ".repeat(131072))).unwrap();
    assert!(read_palette(&path).is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn test_connector_and_geometry_select_monitor_palette() {
    let palette = parsed(&snapshot());
    assert_eq!(monitor_palette(&palette, Some("HEADLESS-2"), None).colors.background, [35.0, 20.0, 25.0]);
    assert_eq!(monitor_palette(&palette, Some("unknown"), Some([1280, 0, 1280, 720])).name, "HEADLESS-2");
    assert_eq!(monitor_palette(&palette, None, None).name, "HEADLESS-1");
}

#[test]
fn test_desktop_css_only_themes_controls_and_popovers() {
    let colors = first_colors();
    let text = css("test", &colors, 20, CssOptions { desktop: true, ..Default::default() });
    assert!(text.contains(".test popover > contents"));
    assert!(text.contains(".test entry"));
    assert!(!text.contains("window.test"));
    assert!(!text.contains("picture"));
    assert!(!text.contains("#main-overlay"));
    assert!(!text.contains(".test {"));
    let settings = css("settings", &colors, 20, CssOptions::default());
    assert!(settings.contains("window.settings"));
    assert!(settings.contains("rgba(18,25,35,0.800)"));
    assert!(!settings.contains("rgba(18,25,35,1.000)"));
}

#[test]
fn test_app_transparency_is_independent_of_dock_and_does_not_fade_wallpaper() {
    let dir = scratch("independent");
    let path = dir.join("palette.json");
    for dock_amount in [0, 50, 100] {
        let mut value = snapshot();
        value["transparency"] = json!(dock_amount);
        std::fs::write(&path, value.to_string()).unwrap();
        let result = read_palette(&path).unwrap();
        assert!(!result.apps_transparency_enabled);
        assert_eq!(result.apps_transparency, 5);
    }
    std::fs::remove_dir_all(&dir).unwrap();
    let base = first_colors();
    let light = Colors { background: [240.0, 242.0, 250.0], ..base };
    for colors in [base, light] {
        for (amount, alpha) in [(0, "1.000"), (50, "0.500"), (100, "0.000")] {
            let background = colors.background.map(|v| (v as i64).to_string()).join(",");
            for mode in [CssOptions::default(), CssOptions { desktop: true, ..Default::default() },
                         CssOptions { switcher: true, ..Default::default() }] {
                let text = css("test", &colors, amount, mode);
                assert!(text.contains(&format!("background-color: rgba({background},{alpha});")));
                assert!(text.contains("color: rgba(222,228,240,1.000);"));
                assert!(!text.contains("opacity:"));
            }
            let backdrop = css("test", &colors, amount, CssOptions { backdrop: true, ..Default::default() });
            assert!(!backdrop.contains("rgba("));
            assert!(backdrop.contains("background-color: transparent;"));
        }
    }
    // Python also rejects True and 20.5, which the i64 parameter cannot express.
    for invalid in [-1, 101] {
        assert!(palette_css("test", &base, invalid, &CssOptions::default()).is_err());
    }
    for strength in [-1, 101] {
        assert!(palette_css("test", &base, 5, &CssOptions { frost_strength: strength, ..Default::default() }).is_err());
    }
}

#[test]
fn test_frosted_and_square_materials_do_not_paint_wallpaper_or_backdrops() {
    // The legacy texture path is dropped by read_palette; palette_css never sees it.
    let colors = first_colors();
    for mode in [CssOptions::default(), CssOptions { desktop: true, ..Default::default() },
                 CssOptions { switcher: true, ..Default::default() }] {
        let mut reference: Option<String> = None;
        for strength in [0, 25, 50, 100] {
            let text = css("test", &colors, 5, CssOptions { frosted: true, frost_strength: strength, rounded_corners: false, ..mode });
            assert!(!text.contains("url("));
            assert!(!text.contains("linear-gradient("));
            assert!(text.contains("background-color: rgba(18,25,35,0.950);"));
            assert!(text.contains("border-radius: 0px;"));
            assert!(!text.contains("opacity:"));
            if let Some(reference) = &reference {
                assert_eq!(&text, reference);
            }
            reference = Some(text.clone());
            if mode.desktop {
                assert!(!text.contains("window.test"));
                assert!(!text.contains("picture"));
            }
            if mode.switcher {
                assert!(text.contains("window.test.tessel-shell-switcher { background-image: none; background-color: transparent;"));
            }
        }
    }
    let text = css("test", &colors, 100, CssOptions { frosted: true, backdrop: true, ..Default::default() });
    assert!(!text.contains("url("));
    assert!(text.contains("background-color: transparent;"));
    let text = css("test", &colors, 100, CssOptions { frosted: true, ..Default::default() });
    assert!(text.contains("background-color: rgba(18,25,35,0.000);"));
    assert!(!text.contains("url("));
    // A cached texture path in the file is normalized away.
    let mut value = snapshot();
    value["monitors"][0]["frost_texture"] = json!("/somewhere/arbitrary.png");
    let palette = parsed(&value);
    assert_eq!(palette.monitors[0].frost_texture, None);
    assert_eq!(palette.value["monitors"][0]["frost_texture"], Value::Null);
}

#[test]
fn version_uses_python_equality() {
    for (version, ok) in [(json!(1), true), (json!(1.0), true), (json!(true), true), (json!("1"), false), (json!(null), false)] {
        let mut value = snapshot();
        value["version"] = version;
        assert_eq!(tessel_theme::parse_palette(value.to_string().as_bytes()).is_some(), ok);
    }
}
