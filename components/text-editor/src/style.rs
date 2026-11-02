//! CSS, shortcut table and small pure helpers of editor_ui.py.

use gtk::gdk;
use gtk::pango;
use tessel_core::tr;
use tessel_gtk::app_menu::{Shortcut, ShortcutSection};

pub const CSS: &str = "
.tessel-text-editor button { border-radius: 8px; }
.tessel-text-editor .editor-toolbar { padding: 8px 12px; }
.tessel-text-editor .editor-status { padding: 7px 14px; font-size: 9pt; }
.tessel-text-editor .search-panel { padding: 10px 14px; }
.tessel-text-editor .error-banner { padding: 10px 14px; }
.tessel-text-editor .muted { opacity: .7; }
.tessel-text-editor .editor-title { font-weight: 650; }
.tessel-text-editor .editor-subtitle { font-size: 9pt; }
.tessel-text-editor .settings-card { padding: 12px; border-radius: 12px; }
.tessel-text-editor textview { padding: 8px; }
";

/// `SHORTCUTS` for the standard Keyboard Shortcuts window (English; translated there).
pub fn shortcuts() -> Vec<ShortcutSection> {
    vec![
        ("Files".into(), vec![Shortcut::any(&["<Control>n", "<Control><Shift>n"], "New window"), Shortcut::new("<Control>o", "Open"),
                              Shortcut::new("<Control>s", "Save"), Shortcut::new("<Control><Shift>s", "Save As…"),
                              Shortcut::new("<Control>p", "Print…"), Shortcut::new("<Control>w", "Close")]),
        ("Editing".into(), vec![Shortcut::new("<Control>z", "Undo"), Shortcut::any(&["<Control><Shift>z", "<Control>y"], "Redo"),
                                Shortcut::new("<Control>f", "Find…"), Shortcut::new("<Control>h", "Replace…"),
                                Shortcut::new("F3", "Find next"), Shortcut::new("<Shift>F3", "Find previous"),
                                Shortcut::any(&["<Control>l", "<Control>g"], "Go to Line…")]),
        ("View".into(), vec![Shortcut::any(&["<Control>plus", "<Control>equal"], "Zoom In"), Shortcut::new("<Control>minus", "Zoom Out"),
                             Shortcut::new("<Control>0", "Reset Zoom")]),
        ("Bookmarks".into(), vec![Shortcut::new("<Control>F2", "Toggle Bookmark"), Shortcut::new("F2", "Next Bookmark"),
                                  Shortcut::new("<Shift>F2", "Previous Bookmark")]),
    ]
}

/// Window accelerators set in `do_startup`.
pub const ACCELERATORS: [(&str, &[&str]); 19] = [
    ("new-window", &["<Primary>n", "<Primary><Shift>n"]), ("open", &["<Primary>o"]),
    ("save", &["<Primary>s"]), ("save-as", &["<Primary><Shift>s"]), ("close", &["<Primary>w"]),
    ("undo", &["<Primary>z"]), ("redo", &["<Primary><Shift>z", "<Primary>y"]),
    ("find", &["<Primary>f"]), ("replace", &["<Primary>h"]), ("find-next", &["F3"]),
    ("find-previous", &["<Shift>F3"]), ("goto", &["<Primary>l", "<Primary>g"]),
    ("zoom-in", &["<Primary>plus", "<Primary>equal"]), ("zoom-out", &["<Primary>minus"]),
    ("zoom-reset", &["<Primary>0"]), ("print", &["<Primary>p"]),
    ("bookmark", &["<Primary>F2"]), ("next-bookmark", &["F2"]), ("previous-bookmark", &["<Shift>F2"]),
];

pub fn line_count(count: i32) -> String {
    if count == 1 { tr!("1 line") } else { tr!("{count} lines", count = count as i64) }
}

/// Keep document colors independent of the surrounding wallpaper palette.
pub fn editor_surface_css(scope: &str, scheme: Option<&sourceview5::StyleScheme>, dark: bool) -> String {
    let color = |style_name: &str, background: bool, fallback: &str| -> String {
        let style = scheme.and_then(|s| s.style(style_name));
        let value = style.and_then(|style| {
            if background {
                style.is_background_set().then(|| style.background()).flatten()
            } else {
                style.is_foreground_set().then(|| style.foreground()).flatten()
            }
        });
        match value.filter(|v| !v.is_empty()).and_then(|v| gdk::RGBA::parse(v.as_str()).ok()) {
            Some(rgba) => rgba.to_str().to_string(),
            None => fallback.to_owned(),
        }
    };
    let background = color("text", true, if dark { "#1d1d20" } else { "#ffffff" });
    let foreground = color("text", false, if dark { "#c0bfbc" } else { "#504e55" });
    let gutter_background = color("line-numbers", true, &background);
    let gutter_foreground = color("line-numbers", false, if dark { "#777777" } else { "#9a9996" });
    let cursor = color("cursor", false, &foreground);
    // Adwaita leaves selection colors to GTK. Use its ordinary blue selection
    // when the source scheme does not supply one, never the wallpaper accent.
    let selection_background = color("selection", true, if dark { "#1b6acb" } else { "#3584e4" });
    let selection_foreground = color("selection", false, "#ffffff");
    let inactive_background = color("selection-unfocused", true, if dark { "#38383c" } else { "#deddda" });
    let inactive_foreground = color("selection-unfocused", false, &foreground);
    let root = format!("textview.{scope}");
    format!("
{root}, {root} text {{
    background-image: none; background-color: {background}; color: {foreground};
    caret-color: {cursor};
}}
{root} border, {root} border gutter {{
    background-image: none; background-color: {gutter_background}; color: {gutter_foreground};
}}
{root} text selection {{
    background-color: {inactive_background}; color: {inactive_foreground};
}}
{root}:focus text selection {{
    background-color: {selection_background}; color: {selection_foreground};
}}
")
}

/// Python `max(6, min(36, size))` printed by an f-string: the int bounds win ties.
pub fn font_size_text(size: f64) -> String {
    if size <= 6.0 {
        "6".into()
    } else if size >= 36.0 {
        "36".into()
    } else {
        tessel_core::pyfmt::float_repr(size)
    }
}

/// The per-view font rule (JSON quoting keeps font names from becoming selectors).
pub fn font_css(scope: &str, font: &str) -> String {
    let description = pango::FontDescription::from_string(font);
    let family = description.family().map(|f| f.to_string()).filter(|f| !f.is_empty()).unwrap_or_else(|| "Monospace".into());
    let quoted = tessel_core::pyfmt::json_dumps(&serde_json::Value::String(family), &tessel_core::pyfmt::JsonStyle::DEFAULT);
    let size = description.size() as f64 / pango::SCALE as f64;
    format!(".{scope} {{ font-family: {quoted}; font-size: {}pt; }}", font_size_text(size))
}

/// `zoom()`: the new font string.
pub fn zoomed_font(font: &str, amount: i32) -> String {
    let mut description = pango::FontDescription::from_string(font);
    let size = if amount == 0 { 11.0 } else { description.size() as f64 / pango::SCALE as f64 + amount as f64 };
    description.set_size((size.clamp(6.0, 36.0) * pango::SCALE as f64) as i32);
    description.to_str().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_rules_follow_python() {
        assert_eq!(font_css("editor-font-1", "Monospace 11"), ".editor-font-1 { font-family: \"Monospace\"; font-size: 11.0pt; }");
        assert_eq!(font_css("s", "Monospace"), ".s { font-family: \"Monospace\"; font-size: 6pt; }");
        assert_eq!(font_css("s", "Fira Code 40"), ".s { font-family: \"Fira Code\"; font-size: 36pt; }");
        assert_eq!(font_css("s", "Noto Sans Mono 10.5"), ".s { font-family: \"Noto Sans Mono\"; font-size: 10.5pt; }");
        assert_eq!(zoomed_font("Monospace 11", 1), "Monospace 12");
        assert_eq!(zoomed_font("Monospace 36", 1), "Monospace 36");
        assert_eq!(zoomed_font("Monospace 20", 0), "Monospace 11");
        assert_eq!(zoomed_font("Monospace 6", -1), "Monospace 6");
    }

    #[test]
    fn line_count_and_shortcut_table() {
        assert_eq!((line_count(1), line_count(3)), ("1 line".to_string(), "3 lines".to_string()));
        assert_eq!(shortcuts().iter().map(|(title, _)| title.as_str()).collect::<Vec<_>>(), ["Files", "Editing", "View", "Bookmarks"]);
        assert_eq!(ACCELERATORS.len(), 19);
    }
}
