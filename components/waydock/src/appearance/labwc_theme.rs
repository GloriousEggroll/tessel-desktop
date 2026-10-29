//! labwc title bars, menus and OSD in the suite's palette
//! (labwc_titlebar_theme.py, library part).
//!
//! `button_files` renders the same window-control glyphs GTK windows use as
//! labwc SVG buttons; `updated_themerc` rewrites only the managed themerc
//! keys. Colours follow GTK's header bar (`titlebar_colors`). The Python
//! `--apply` CLI stays installed for session-startup (not ported here).

use super::fsx;
use std::path::{Path, PathBuf};
use suite_gtk::palette::{color_palette, mix, Palette, Rgb};
use suite_gtk::theme::{on_accent, validate_palette, vivid_accent, window_glyph_svg, window_light_svg, CLOSE_HOVER, LIGHTS};

/// Rest, hover and unfocused glyph colours (dark-mode defaults).
pub const ACTIVE: Rgb = [190, 191, 194];
pub const HOVER: Rgb = [238, 238, 238];
pub const INACTIVE: Rgb = [112, 114, 118];
pub const BUTTONS: [(&str, &str); 4] = [("iconify", "minimize"), ("max", "maximize"), ("max_toggled", "restore"), ("close", "close")];
pub const THEMERC: [(&str, &str); 13] = [
    ("window.titlebar.padding.width", "6"),
    ("window.titlebar.padding.height", "4"),
    ("window.button.width", "26"),
    ("window.button.height", "26"),
    ("window.button.spacing", "6"),
    ("window.button.hover.bg.color", "#00000000"),
    ("window.label.text.justify", "center"),
    // Soft drop-shadows lit from above, and a faint shine on the title bar.
    ("window.active.shadow.size", "40"),
    ("window.inactive.shadow.size", "26"),
    ("window.active.shadow.color", "#000000a0"),
    ("window.inactive.shadow.color", "#00000060"),
    ("window.active.shadow.offset.y", "14"),
    ("window.inactive.shadow.offset.y", "8"),
];

/// `titlebar_colors()` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TitlebarColors {
    pub title: Rgb,
    pub inactive_title: Rgb,
    pub label: Rgb,
    pub inactive_label: Rgb,
    pub button: Rgb,
    pub button_hover: Rgb,
    pub inactive_button: Rgb,
    pub border: Rgb,
    pub inactive_border: Rgb,
    pub highlight: [u8; 4],
    pub inactive_highlight: [u8; 4],
    pub menu: Rgb,
    pub menu_text: Rgb,
    pub menu_line: Rgb,
    pub menu_active: Rgb,
    pub menu_active_text: Rgb,
    pub menu_disabled_text: Rgb,
}

fn hex(color: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn hex_alpha(color: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", color[0], color[1], color[2], color[3])
}

/// Title bar, label, button, border, menu and OSD colours for one appearance.
/// `None` uses `color_palette(is_dark)`.
pub fn titlebar_colors(is_dark: bool, palette: Option<&Palette>) -> TitlebarColors {
    let palette = palette.copied().unwrap_or_else(|| color_palette(is_dark, None));
    let (background, foreground) = (palette.background, palette.foreground);
    let accent = vivid_accent(palette.accent, is_dark);
    let title = mix(background, foreground, 0.045);
    let popup = mix(background, foreground, 0.07);
    let line = mix(background, foreground, 0.16);
    let on = suite_gtk::palette::parse_hex(on_accent(accent)).unwrap();
    TitlebarColors {
        title, inactive_title: mix(title, [0, 0, 0], if is_dark { 0.30 } else { 0.035 }),
        label: foreground, inactive_label: mix(background, foreground, 0.55),
        button: mix(title, foreground, 0.75), button_hover: foreground,
        inactive_button: mix(title, foreground, 0.45),
        border: mix(background, foreground, 0.12), inactive_border: mix(background, foreground, 0.08),
        highlight: [255, 255, 255, if is_dark { 0x1f } else { 0x80 }],
        inactive_highlight: [255, 255, 255, if is_dark { 0x10 } else { 0x40 }],
        menu: popup, menu_text: foreground, menu_line: line,
        menu_active: accent, menu_active_text: on, menu_disabled_text: mix(popup, foreground, 0.45),
    }
}

/// `(is_dark, first monitor's palette)` from Waydock's palette.json, or `None`.
pub fn current_palette(path: Option<&Path>) -> Option<(bool, Palette)> {
    let path = path.map(Path::to_owned).unwrap_or_else(super::palette_state::palette_path);
    let mut data = Vec::new();
    use std::io::Read;
    std::fs::File::open(path).ok()?.take(256 * 1024).read_to_end(&mut data).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&data).ok()?;
    let palette = validate_palette(&value).ok()?;
    let first = palette.monitors.first()?;
    Some((palette.is_dark, Palette { background: first.background, foreground: first.foreground, accent: first.accent }))
}

/// `_active_colors()`: the published palette, else the dark default.
pub fn active_colors(palette_path: Option<&Path>) -> TitlebarColors {
    match current_palette(palette_path) {
        Some((dark, palette)) => titlebar_colors(dark, Some(&palette)),
        None => titlebar_colors(true, None),
    }
}

/// Every themerc key this module manages, in Python's dict order.
pub fn themerc_values(colors: &TitlebarColors) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = THEMERC.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let updates = [
        ("window.active.title.bg.color", hex(colors.title)),
        ("window.inactive.title.bg.color", hex(colors.inactive_title)),
        ("window.active.label.text.color", hex(colors.label)),
        ("window.inactive.label.text.color", hex(colors.inactive_label)),
        ("window.active.button.unpressed.image.color", hex(colors.button)),
        ("window.inactive.button.unpressed.image.color", hex(colors.inactive_button)),
        ("window.active.border.color", hex(colors.border)),
        ("window.inactive.border.color", hex(colors.inactive_border)),
        ("window.active.title.highlight.color", hex_alpha(colors.highlight)),
        ("window.inactive.title.highlight.color", hex_alpha(colors.inactive_highlight)),
        ("menu.items.bg.color", hex(colors.menu)),
        ("menu.items.text.color", hex(colors.menu_text)),
        ("menu.items.active.bg.color", hex(colors.menu_active)),
        ("menu.items.active.text.color", hex(colors.menu_active_text)),
        ("menu.items.disabled.text.color", hex(colors.menu_disabled_text)),
        ("menu.border.color", hex(colors.menu_line)),
        ("menu.separator.color", hex(colors.menu_line)),
        ("menu.title.bg.color", hex(colors.menu)),
        ("menu.title.text.color", hex(colors.menu_text)),
        ("osd.bg.color", hex(colors.menu)),
        ("osd.border.color", hex(colors.menu_line)),
        ("osd.label.text.color", hex(colors.menu_text)),
    ];
    for (key, value) in updates {
        match values.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => values.push((key.to_owned(), value)),
        }
    }
    values
}

/// Look up one value of [`themerc_values`].
pub fn themerc_value<'a>(values: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `theme_name(rc.xml)`: `<theme>…<name>` (regex, like Python), else "Default".
pub fn theme_name(rc_xml: &Path) -> std::io::Result<String> {
    let text = fsx::read_text(rc_xml)?;
    let pattern = regex::Regex::new(r"(?s)<theme>.*?<name>([^<]+)</name>").unwrap();
    Ok(pattern.captures(&text).map(|c| suite_core::pyjson::py_strip(&c[1]).to_owned()).unwrap_or_else(|| "Default".into()))
}

/// `$XDG_DATA_HOME/themes/<name>/openbox-3` (`data_home` overrides the env).
pub fn theme_dir(name: &str, data_home: Option<&Path>) -> PathBuf {
    let data = data_home.map(Path::to_owned).unwrap_or_else(suite_core::paths::data_home);
    data.join("themes").join(name).join("openbox-3")
}

fn light(name: &str) -> (Rgb, Rgb) {
    LIGHTS.iter().find(|(key, ..)| *key == name).map(|e| (e.1, e.2)).unwrap()
}

/// Plain glyphs, or coloured dots that show their glyph on hover.
pub fn button_files(colored: bool, colors: &TitlebarColors) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let (active, hover, inactive) = (colors.button, colors.button_hover, colors.inactive_button);
    for (button, glyph) in BUTTONS {
        if colored {
            let (fill, mark) = light(glyph);
            files.push((format!("{button}-active.svg"), window_light_svg(glyph, fill, None, 5)));
            files.push((format!("{button}-inactive.svg"), window_light_svg(glyph, inactive, None, 5)));
            let hover_svg = window_light_svg(glyph, fill, Some(mark), 5);
            files.push((format!("{button}_hover-active.svg"), hover_svg.clone()));
            files.push((format!("{button}_hover-inactive.svg"), hover_svg));
            continue;
        }
        let hover_svg = window_glyph_svg(glyph, if button == "close" { CLOSE_HOVER } else { hover }, 5, 1.0);
        for (state, color) in [("active", active), ("inactive", inactive)] {
            files.push((format!("{button}-{state}.svg"), window_glyph_svg(glyph, color, 5, 1.0)));
            files.push((format!("{button}_hover-{state}.svg"), hover_svg.clone()));
        }
    }
    files
}

/// Replace managed keys once each (dropping duplicates), append missing ones.
pub fn updated_themerc(text: &str, colors: &TitlebarColors) -> String {
    let values = themerc_values(colors);
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for line in fsx::splitlines(text) {
        let key = suite_core::pyjson::py_strip(line.split(':').next().unwrap_or(""));
        if let Some((key, value)) = values.iter().find(|(k, _)| k == key) {
            if seen.contains(&key.as_str()) {
                continue;
            }
            seen.push(key);
            lines.push(format!("{key}: {value}"));
        } else {
            lines.push(line.to_owned());
        }
    }
    for (key, value) in &values {
        if !seen.contains(&key.as_str()) {
            lines.push(format!("{key}: {value}"));
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(value: &str) -> Rgb {
        suite_gtk::palette::parse_hex(value).unwrap()
    }

    fn luminance(c: Rgb) -> f64 {
        c[0] as f64 * 0.2126 + c[1] as f64 * 0.7152 + c[2] as f64 * 0.0722
    }

    #[test]
    fn light_and_dark_sets_match_gtk_headerbar_and_dim_when_inactive() {
        for dark in [true, false] {
            let palette = color_palette(dark, None);
            let values = themerc_values(&titlebar_colors(dark, Some(&palette)));
            let get = |k: &str| themerc_value(&values, k).unwrap().to_owned();
            let expected: Rgb = std::array::from_fn(|i| suite_gtk::palette::round_half_even(
                palette.background[i] as f64 * 0.955 + palette.foreground[i] as f64 * 0.045) as u8);
            assert_eq!(rgb(&get("window.active.title.bg.color")), expected);
            assert!(luminance(rgb(&get("window.inactive.title.bg.color"))) < luminance(rgb(&get("window.active.title.bg.color"))));
            assert_ne!(get("window.active.title.bg.color"), "#222226");
            assert_eq!(rgb(&get("window.active.label.text.color")), palette.foreground);
            assert_ne!(get("window.active.label.text.color"), get("window.inactive.label.text.color"));
            assert_ne!(get("menu.items.bg.color"), "#3584e4");
            assert_eq!(rgb(&get("menu.items.active.bg.color")), palette.accent);
        }
        let dark = themerc_values(&titlebar_colors(true, None));
        let light = themerc_values(&titlebar_colors(false, None));
        assert!(luminance(rgb(themerc_value(&light, "window.active.title.bg.color").unwrap()))
            > luminance(rgb(themerc_value(&dark, "window.active.title.bg.color").unwrap())));
    }

    #[test]
    fn grey_published_accent_is_replaced_in_menus() {
        let palette = Palette { background: [7, 9, 14], foreground: [238, 238, 238], accent: [119, 122, 130] };
        let values = themerc_values(&titlebar_colors(true, Some(&palette)));
        let [red, _green, blue] = rgb(themerc_value(&values, "menu.items.active.bg.color").unwrap());
        assert!(blue as i32 > red as i32 + 40);
    }

    #[test]
    fn updated_themerc_replaces_hardcoded_colors_once_and_keeps_other_keys() {
        let current = "window.active.title.bg.color: #222226\nwindow.inactive.title.bg.color: #222226\n\
                       menu.items.bg.color: #3584e4\nwindow.font: sans-serif-20:pixels\nwindow.active.title.bg.color: #000000\n";
        let colors = titlebar_colors(false, None);
        let text = updated_themerc(current, &colors);
        assert!(!text.contains("#222226") && !text.contains("#3584e4"));
        assert_eq!(text.matches("window.active.title.bg.color:").count(), 1);
        assert!(text.contains("window.font: sans-serif-20:pixels"));
        assert_eq!(updated_themerc(&text, &colors), text);
    }

    #[test]
    fn published_palette_drives_defaults_and_buttons() {
        let base = std::env::temp_dir().join(format!("d-labwc-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("palette.json");
        std::fs::write(&path, r#"{"version": 1, "is_dark": false, "monitors": [{"name": "A", "geometry": [0, 0, 100, 100], "background": [250, 250, 252], "foreground": [35, 38, 45], "accent": [36, 112, 210]}]}"#).unwrap();
        assert!(!current_palette(Some(&path)).unwrap().0);
        let values = themerc_values(&active_colors(Some(&path)));
        assert_eq!(values, themerc_values(&titlebar_colors(false, Some(&color_palette(false, None)))));
        let files = button_files(false, &active_colors(Some(&path)));
        let button = themerc_value(&values, "window.active.button.unpressed.image.color").unwrap();
        let close = &files.iter().find(|(n, _)| n == "close-active.svg").unwrap().1;
        assert!(close.contains(&format!("fill=\"{button}\"")));
        let missing = base.join("missing.json");
        assert!(current_palette(Some(&missing)).is_none());
        assert_eq!(themerc_values(&active_colors(Some(&missing))), themerc_values(&titlebar_colors(true, None)));
    }

    #[test]
    fn labwc_buttons_switch_between_plain_and_colored() {
        let colors = titlebar_colors(true, None);
        let (plain, colored) = (button_files(false, &colors), button_files(true, &colors));
        let names = |files: &[(String, String)]| { let mut n: Vec<_> = files.iter().map(|f| f.0.clone()).collect(); n.sort(); n };
        assert_eq!(names(&plain), names(&colored));
        let get = |files: &[(String, String)], name: &str| files.iter().find(|f| f.0 == name).unwrap().1.clone();
        assert!(!get(&plain, "close-active.svg").contains("<circle"));
        assert!(get(&colored, "close-active.svg").contains("fill=\"#ff5f57\""));
        assert!(get(&colored, "close_hover-active.svg").contains("scale(.6)"));
        assert!(!get(&colored, "close-active.svg").contains("scale(.6)"));
    }
}
