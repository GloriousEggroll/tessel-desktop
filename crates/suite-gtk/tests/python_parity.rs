//! Byte-for-byte parity with CSS/SVG produced by the Python modules
//! (fixtures generated from waydock/dock_style.py, gtk_color_scheme.py and
//! companion_theme.py; see tests/fixtures/README).

use serde_json::Value;
use suite_gtk::palette::{background_css, color_css, color_palette, parse_hex, popup_css, PanelStyle};
use suite_gtk::theme::{theme_css, window_glyph_svg, window_light_svg, MonitorPalette, ThemeOptions};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/python_reference.json")).unwrap()
}

fn rgb(value: &Value) -> [u8; 3] {
    std::array::from_fn(|i| value[i].as_u64().unwrap() as u8)
}

#[test]
fn dock_palette_css_matches_python() {
    let reference = fixture();
    let expected = reference["palette"].as_array().unwrap();
    let a = background_css(true, &PanelStyle { border_sides: &["top"], shine: true, ..Default::default() }).unwrap();
    assert_eq!(a, expected[0].as_str().unwrap());
    let b = background_css(false, &PanelStyle {
        transparency: 50, wallpaper: Some([30, 65, 190]), rounded_corners: false, scope: Some("dock-1"),
        background_color: parse_hex("#336699"), border_color: parse_hex("#ff0000"), border_sides: &["left", "bottom"],
        ..Default::default() }).unwrap();
    assert_eq!(b, expected[1].as_str().unwrap());
    let c = popup_css("dock-2", false, Some([25, 160, 55]), 30, false, false, 50, None).unwrap();
    assert_eq!(c, expected[2].as_str().unwrap());
    assert_eq!(color_css(true, &color_palette(true, Some([190, 22, 66])), false), expected[3].as_str().unwrap());
    assert_eq!(color_css(false, &color_palette(false, Some([25, 160, 55])), true), expected[4].as_str().unwrap());
}

#[test]
fn companion_theme_css_and_glyphs_match_python() {
    let reference = fixture();
    for case in reference["theme"].as_array().unwrap() {
        let entry = &case["entry"];
        let entry = MonitorPalette { name: "X".into(), geometry: [0, 0, 1, 1], background: rgb(&entry["background"]),
                                     foreground: rgb(&entry["foreground"]), accent: rgb(&entry["accent"]) };
        let kw = &case["kw"];
        let flag = |key: &str, default: bool| kw.get(key).and_then(Value::as_bool).unwrap_or(default);
        let options = ThemeOptions {
            transparency: case["transparency"].as_u64().unwrap() as u8, frosted: flag("frosted", false),
            rounded_corners: flag("rounded_corners", true), colored_buttons: flag("colored_buttons", false),
            dialog: flag("dialog", false), frost_strength: kw.get("frost_strength").and_then(Value::as_u64).unwrap_or(50) as u8 };
        let css = theme_css("waydock-companion-7", &entry, case["dark"].as_bool().unwrap(), &options).unwrap();
        assert_eq!(css, case["css"].as_str().unwrap(), "{kw}");
    }
    for (name, svg) in reference["glyphs"].as_object().unwrap() {
        assert_eq!(window_glyph_svg(name, [238, 238, 238], 2, 0.45), svg.as_str().unwrap());
    }
    assert_eq!(window_light_svg("close", [255, 95, 87], Some([110, 14, 10]), 1), reference["lights"]["close"].as_str().unwrap());
}
