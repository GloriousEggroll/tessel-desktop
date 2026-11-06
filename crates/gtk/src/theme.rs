//! Live, per-monitor Tessel Dock colors for Tessel's GTK4 windows.
//!
//! Tessel Dock publishes `~/.cache/tessel/dock/palette.json`; [`attach`] follows it for
//! a window (and [`attach_application`] for every window of an app) with a
//! uniquely scoped stylesheet at APPLICATION+30. Without a published palette
//! the application's original GTK styling is left in place.

use crate::blur::{self, BlurBinding};
use crate::palette::{round_half_even, Rgb};
use crate::utils::{PRIORITY_APPLICATION, PRIORITY_USER};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::LayerShell;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const MAX_PALETTE_BYTES: usize = 256 * 1024;

pub fn palette_path() -> PathBuf {
    tessel_core::paths::component_cache_dir("dock").join("palette.json")
}

/// One monitor of the published palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorPalette {
    pub name: String,
    /// `(x, y, width, height)` in logical pixels.
    pub geometry: [i64; 4],
    pub background: Rgb,
    pub foreground: Rgb,
    pub accent: Rgb,
}

/// A validated `palette.json` (version 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteFile {
    pub is_dark: bool,
    pub monitors: Vec<MonitorPalette>,
    pub apps_transparency_enabled: bool,
    pub apps_transparency: u8,
    pub apps_frosted: bool,
    pub apps_rounded_corners: bool,
    pub apps_frost_strength: u8,
    pub colored_window_buttons: bool,
}

// Python `type(value) is int`: JSON integers only, never bools or floats.
fn int(value: Option<&Value>) -> Option<i64> {
    value.filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64)
}

fn color(value: Option<&Value>) -> Result<Rgb, String> {
    let parts = value.and_then(Value::as_array).filter(|a| a.len() == 3).ok_or("Invalid palette color")?;
    let mut rgb = [0u8; 3];
    for (slot, part) in rgb.iter_mut().zip(parts) {
        *slot = int(Some(part)).filter(|p| (0..=255).contains(p)).ok_or("Invalid palette color")? as u8;
    }
    Ok(rgb)
}

fn percent(value: Option<&Value>, default: i64, error: &str) -> Result<u8, String> {
    match value {
        None => Ok(default as u8),
        Some(v) => int(Some(v)).filter(|p| (0..=100).contains(p)).map(|p| p as u8).ok_or_else(|| error.to_owned()),
    }
}

fn flag(value: Option<&Value>, default: bool, error: &str) -> Result<bool, String> {
    match value {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(error.to_owned()),
    }
}

/// Validate a decoded palette exactly as Python does (ValueError → `Err`).
pub fn validate_palette(value: &Value) -> Result<PaletteFile, String> {
    let object = value.as_object().ok_or("Invalid Tessel Dock palette")?;
    let monitors = object.get("monitors").and_then(Value::as_array);
    let valid = int(object.get("version")) == Some(1) && object.get("is_dark").is_some_and(Value::is_boolean)
        && monitors.is_some_and(|m| m.len() <= 64);
    if !valid {
        return Err("Invalid Tessel Dock palette".into());
    }
    let apps_transparency_enabled = flag(object.get("apps_transparency_enabled"), false, "Invalid application transparency")?;
    let apps_transparency = percent(object.get("apps_transparency"), 5, "Invalid application transparency")?;
    let strength_default = if object.contains_key("apps_transparency") { apps_transparency as i64 } else { 50 };
    let apps_frost_strength = percent(object.get("apps_frost_strength"), strength_default, "Invalid application Blur strength")?;
    let appearance = "Invalid application appearance";
    let apps_frosted = flag(object.get("apps_frosted"), false, appearance)?;
    let apps_rounded_corners = flag(object.get("apps_rounded_corners"), true, appearance)?;
    let colored_window_buttons = flag(object.get("colored_window_buttons"), false, appearance)?;
    let mut result = Vec::new();
    for entry in monitors.unwrap() {
        let name = entry.get("name").and_then(Value::as_str).ok_or("Invalid palette monitor")?;
        let geometry = entry.get("geometry").and_then(Value::as_array).filter(|g| g.len() == 4).ok_or("Invalid palette geometry")?;
        let mut parts = [0i64; 4];
        for (slot, part) in parts.iter_mut().zip(geometry) {
            *slot = int(Some(part)).ok_or("Invalid palette geometry")?;
        }
        if parts[2] <= 0 || parts[3] <= 0 {
            return Err("Invalid palette geometry".into());
        }
        result.push(MonitorPalette { name: name.into(), geometry: parts, background: color(entry.get("background"))?,
                                     foreground: color(entry.get("foreground"))?, accent: color(entry.get("accent"))? });
    }
    Ok(PaletteFile { is_dark: object["is_dark"].as_bool().unwrap(), monitors: result, apps_transparency_enabled,
                     apps_transparency, apps_frosted, apps_rounded_corners, apps_frost_strength, colored_window_buttons })
}

/// Unknown outputs keep their GTK theme instead of taking another's color.
pub fn palette_for_geometry<'a>(palette: Option<&'a PaletteFile>, geometry: Option<[i64; 4]>) -> Option<&'a MonitorPalette> {
    let (palette, geometry) = (palette?, geometry?);
    palette.monitors.iter().find(|entry| entry.geometry == geometry)
}

fn rgb(value: Rgb) -> String {
    format!("rgb({},{},{})", value[0], value[1], value[2])
}

fn rgba(value: Rgb, alpha: f64) -> String {
    format!("rgba({},{},{},{alpha:.3})", value[0], value[1], value[2])
}

/// Older or hand-edited palettes may still carry a near-grey accent that makes
/// suggested buttons, selections and switches look disabled. Such accents use
/// the dock's colorless-wallpaper fallback blues instead.
pub fn vivid_accent(accent: Rgb, is_dark: bool) -> Rgb {
    let spread = accent.iter().max().unwrap() - accent.iter().min().unwrap();
    if spread < 24 { if is_dark { [120, 174, 237] } else { [27, 106, 202] } } else { accent }
}

/// Text color on an accent fill.
pub fn on_accent(accent: Rgb) -> &'static str {
    if crate::palette::relative_luminance(accent) > 0.179 { "#101114" } else { "#ffffff" }
}

/// Paint only the tint; blur is requested separately on the native surface.
pub fn frost_background(background: Rgb, transparency: u8) -> String {
    format!("background-image: none; background-color: {};", rgba(background, (100.0 - transparency.min(100) as f64) / 100.0))
}

/// Window controls look the same in suite apps, other GTK4 apps and labwc title
/// bars: thin glyphs with no background. Filled shapes on a 16px grid.
pub const WINDOW_GLYPHS: [(&str, &str); 4] = [
    ("minimize", r#"<rect x="4" y="7.25" width="8" height="1.5" rx=".75"/>"#),
    ("maximize", concat!(r#"<path fill-rule="evenodd" d="M6 4h4a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z"#,
                         r#"M6.25 5.5h3.5a.75.75 0 0 1 .75.75v3.5a.75.75 0 0 1-.75.75h-3.5a.75.75 0 0 1-.75-.75v-3.5"#,
                         r#"a.75.75 0 0 1 .75-.75z"/>"#)),
    ("restore", concat!(r#"<path fill-rule="evenodd" d="M5 6.5h3a1.5 1.5 0 0 1 1.5 1.5v3A1.5 1.5 0 0 1 8 12.5H5"#,
                        r#"A1.5 1.5 0 0 1 3.5 11V8A1.5 1.5 0 0 1 5 6.5zM5.25 8h2.5a.25.25 0 0 1 .25.25v2.5a.25.25 0 0 1-.25.25"#,
                        r#"h-2.5a.25.25 0 0 1-.25-.25v-2.5A.25.25 0 0 1 5.25 8z"/>"#,
                        r#"<path d="M6.5 3.5H11A1.5 1.5 0 0 1 12.5 5v4.5H11V5H6.5z"/>"#)),
    ("close", concat!(r#"<rect x="7.25" y="3" width="1.5" height="10" rx=".75" transform="rotate(45 8 8)"/>"#,
                      r#"<rect x="7.25" y="3" width="1.5" height="10" rx=".75" transform="rotate(-45 8 8)"/>"#)),
];
pub const CLOSE_HOVER: Rgb = [237, 92, 99];
/// Colored buttons: macOS-style red, yellow and green dots `(fill, glyph)` whose
/// glyphs appear while the pointer is over the button group.
pub const LIGHTS: [(&str, Rgb, Rgb); 4] = [("close", [255, 95, 87], [110, 14, 10]), ("minimize", [254, 188, 46], [122, 76, 0]),
                                          ("maximize", [40, 200, 64], [0, 88, 14]), ("restore", [40, 200, 64], [0, 88, 14])];

fn glyph(name: &str) -> &'static str {
    WINDOW_GLYPHS.iter().find(|(key, _)| *key == name).map(|e| e.1).expect("known window glyph")
}

fn light(name: &str) -> (Rgb, Rgb) {
    LIGHTS.iter().find(|(key, ..)| *key == name).map(|e| (e.1, e.2)).expect("known window light")
}

fn hex(color: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

/// One control glyph; `margin` pads it for labwc, which scales icons to the button.
pub fn window_glyph_svg(name: &str, color: Rgb, margin: i32, alpha: f64) -> String {
    let size = 16 + 2 * margin;
    let fade = if alpha < 1.0 { format!(r#" fill-opacity="{alpha:.2}""#) } else { String::new() };
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="{} {} {size} {size}"><g fill="{}"{fade}>{}</g></svg>"#,
            -margin, -margin, hex(color), glyph(name))
}

/// A colored dot on the 16px grid, optionally with its glyph shrunk inside.
pub fn window_light_svg(name: &str, fill: Rgb, glyph_color: Option<Rgb>, margin: i32) -> String {
    let size = 16 + 2 * margin;
    let inner = glyph_color.map(|g| format!(r#"<g fill="{}" transform="translate(8 8) scale(.6) translate(-8 -8)">{}</g>"#, hex(g), glyph(name)))
        .unwrap_or_default();
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="{} {} {size} {size}"><circle cx="8" cy="8" r="6" fill="{}"/>{inner}</svg>"#,
            -margin, -margin, hex(fill))
}

fn glyph_url(name: &str, color: Rgb, alpha: f64) -> String {
    let data = glib::base64_encode(window_glyph_svg(name, color, 0, alpha).as_bytes());
    format!(r#"url("data:image/svg+xml;base64,{data}")"#)
}

fn light_css(window: &str, foreground: Rgb) -> String {
    let button = format!("{window} windowcontrols > button");
    let [r, g, b] = foreground;
    let mut css = vec![
        format!("{window} windowcontrols {{ border-spacing: 8px; }}"),
        format!("{button}, {button}:hover, {button}:active, {button}:checked, {button}:backdrop \
                 {{ min-width: 16px; min-height: 16px; padding: 0 2px; margin: 0; border: none; box-shadow: none; \
                 background: none; outline: none; }}"),
        format!("{button} > image, {button}:hover > image, {button}:active > image {{ padding: 0; \
                 min-width: 12px; min-height: 12px; border-radius: 9999px; box-shadow: none; \
                 -gtk-icon-filter: opacity(0); background-image: none; background-repeat: no-repeat; \
                 background-position: center; background-size: 10px 10px; }}"),
        format!("{button}:focus-visible > image {{ outline: 1px solid rgba({r},{g},{b},0.45); }}"),
    ];
    for name in ["close", "minimize", "maximize"] {
        let (fill, glyph) = light(name);
        css.push(format!("{button}.{name} > image {{ background-color: rgb({},{},{}); }}", fill[0], fill[1], fill[2]));
        // Unfocused windows show neutral dots, as macOS does.
        css.push(format!("{window}:backdrop windowcontrols > button.{name} > image \
                          {{ background-color: rgba({r},{g},{b},0.22); background-image: none; }}"));
        css.push(format!("{window} windowcontrols:hover > button.{name} > image {{ background-image: {}; }}", glyph_url(name, glyph, 1.0)));
    }
    css.push(format!("{window}.maximized windowcontrols:hover > button.maximize > image {{ background-image: {}; }}",
                     glyph_url("restore", light("restore").1, 1.0)));
    css.join("\n")
}

pub const TILED: [&str; 7] = [".maximized", ".fullscreen", ".tiled", ".tiled-top", ".tiled-bottom", ".tiled-left", ".tiled-right"];

/// labwc draws window drop-shadows, so GTK's own is replaced by a hairline.
/// The transparent spread keeps GTK's resize margin outside the window. The
/// title bar gets a faint shine along its top edge, like macOS windows.
pub fn window_frame_css(window: &str) -> String {
    let flush = TILED.iter().map(|state| format!("{window}.csd{state}")).collect::<Vec<_>>().join(", ");
    [format!("{window}.csd {{ box-shadow: 0 0 0 1px rgba(0,0,0,0.22), 0 0 0 8px transparent; }}"),
     format!("{flush} {{ box-shadow: none; }}"),
     format!("{window}.csd headerbar, {window}.csd .titlebar {{ box-shadow: inset 0 1px rgba(255,255,255,0.07); }}"),
     format!("{window}.csd:backdrop headerbar, {window}.csd:backdrop .titlebar {{ box-shadow: inset 0 1px rgba(255,255,255,0.04); }}")]
        .join("\n")
}

/// Title buttons for every GTK4 `windowcontrols` under `window`.
///
/// The glyph is drawn on the button's image, filtering out the toolkit's own,
/// so icon themes with differently drawn window icons (breeze uses chevrons)
/// never show. Plain buttons have no background: hover brightens the glyph and
/// close turns red. `colored` draws macOS-style red, yellow and green dots.
pub fn window_controls_css(window: &str, foreground: Rgb, colored: bool) -> String {
    if colored {
        return light_css(window, foreground);
    }
    let button = format!("{window} windowcontrols > button");
    let [r, g, b] = foreground;
    let mut css = vec![
        format!("{window} windowcontrols {{ border-spacing: 6px; }}"),
        format!("{button}, {button}:hover, {button}:active, {button}:checked, {button}:backdrop \
                 {{ min-width: 24px; min-height: 24px; padding: 0; margin: 0; border: none; box-shadow: none; \
                 background: none; outline: none; }}"),
        format!("{button} > image, {button}:hover > image, {button}:active > image {{ padding: 4px; \
                 min-width: 16px; min-height: 16px; \
                 border-radius: 0; box-shadow: none; -gtk-icon-filter: opacity(0); background-color: transparent; \
                 background-repeat: no-repeat; background-position: center; background-size: 16px 16px; }}"),
        format!("{button}:focus-visible > image {{ outline: 1px solid rgba({r},{g},{b},0.45); }}"),
    ];
    // Glyphs fade inside their SVG, never through CSS opacity: rest .75,
    // hover 1, unfocused window .45.
    let maximized = format!("{window}.maximized windowcontrols > button.maximize");
    for (selector, name) in [(format!("{button}.minimize"), "minimize"), (format!("{button}.maximize"), "maximize"),
                             (maximized, "restore"), (format!("{button}.close"), "close")] {
        css.push(format!("{selector} > image {{ background-image: {}; }}", glyph_url(name, foreground, 0.75)));
        let hover = if name == "close" { CLOSE_HOVER } else { foreground };
        css.push(format!("{selector}:hover > image, {selector}:active > image {{ background-image: {}; }}", glyph_url(name, hover, 1.0)));
        let unfocused = selector.replacen(window, &format!("{window}:backdrop"), 1);
        css.push(format!("{unfocused} > image {{ background-image: {}; }}", glyph_url(name, foreground, 0.45)));
    }
    css.join("\n")
}

/// Dialogs and prompts sit over other windows, whose text would bleed through
/// a translucent background, so they stay (almost) opaque.
pub const DIALOG_MIN_OPACITY: f64 = 0.97;
pub const DIALOG_CLASSES: [&str; 4] = ["dialog", "messagedialog", "tessel-prompt", "prompt"];

/// Modal, transient and dialog-classed windows get DIALOG_MIN_OPACITY.
pub fn is_dialog(window: &gtk::Window) -> bool {
    window.is_modal() || window.transient_for().is_some() || DIALOG_CLASSES.iter().any(|c| window.has_css_class(c))
}

/// `theme_css` keyword options (Python defaults via `Default`).
#[derive(Clone, Copy, Debug)]
pub struct ThemeOptions {
    pub transparency: u8,
    pub frosted: bool,
    pub rounded_corners: bool,
    pub frost_strength: u8,
    pub colored_buttons: bool,
    pub dialog: bool,
}

impl Default for ThemeOptions {
    fn default() -> Self {
        Self { transparency: 0, frosted: false, rounded_corners: true, frost_strength: 50, colored_buttons: false, dialog: false }
    }
}

/// Literal colors and a unique window class (`tessel-dock-companion-<n>`) avoid
/// colors leaking between windows.
fn rule(root: &str, selectors: &[&str], declaration: impl AsRef<str>) -> String {
    let joined = selectors.iter().map(|s| if s.is_empty() { root.to_owned() } else { format!("{root} {s}") })
        .collect::<Vec<_>>().join(", ");
    format!("{joined} {{ {} }}", declaration.as_ref())
}

pub fn theme_css(scope: &str, entry: &MonitorPalette, is_dark: bool, options: &ThemeOptions) -> Result<String, String> {
    if !scope.strip_prefix("tessel-dock-companion-").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
        return Err("Invalid theme scope".into());
    }
    if options.transparency > 100 {
        return Err("Transparency must be between 0 and 100 percent".into());
    }
    if options.frost_strength > 100 {
        return Err("Blur strength must be between 0 and 100 percent".into());
    }
    let mut transparency = options.transparency;
    if options.dialog {
        transparency = transparency.min(round_half_even(100.0 - DIALOG_MIN_OPACITY * 100.0) as u8);
    }
    let alpha = (100.0 - transparency as f64) / 100.0;
    // Surfaces (windows, popups, menus) use 12px; controls and inner cards 8px.
    let radius = if options.rounded_corners { 12 } else { 0 };
    let background_color = |value: Rgb| rgba(value, alpha);
    let mix = crate::palette::mix;
    let (background, foreground) = (entry.background, entry.foreground);
    let accent = vivid_accent(entry.accent, is_dark);
    let (bg, fg, ac) = (background_color(background), rgb(foreground), rgb(accent));
    let surface = background_color(mix(background, foreground, 0.045));
    let field = background_color(mix(background, foreground, 0.075));
    let hover = background_color(mix(background, foreground, 0.11));
    let line = rgb(mix(background, foreground, 0.16));
    let muted = rgb(mix(background, foreground, 0.66));
    let faint = rgb(mix(background, foreground, 0.40));
    let selected = background_color(mix(background, accent, 0.20));
    let (green, error) = if is_dark { ("#8fd6ab", "#ed858c") } else { ("#287747", "#b63240") };
    let on_accent = on_accent(accent);
    let material = if options.frosted { frost_background(background, transparency) }
                   else { format!("background-image: none; background-color: {bg};") };
    let root = format!(".{scope}");
    let mut rules: Vec<String> = Vec::new();
    rules.push(rule(&root, &[""], &format!("color: {fg}; {material} border-radius: {radius}px;")));
    rules.extend([
        format!("window{root}.tray-popup {{ background-image: none; background-color: transparent; border-radius: {radius}px; }}"),
        format!("window{root}.way-displays-overlay {{ background-image: none; background-color: transparent; border-radius: 0; }}"),
    ]);
    rules.push(rule(&root, &["label", "image"], "color: inherit;"));
    // Each surface paints its background once: ordinary windows at the root,
    // layer popups inside their transparent window, and popovers on contents.
    rules.push(rule(&root, &["headerbar", ".popup-surface", "popover"],
         format!("background-image: none; background-color: transparent; color: {fg}; border-color: {line}; box-shadow: none;")));
    rules.push(format!("window{root}.tray-popup .popup-surface {{ {material} border-radius: {radius}px; }}"));
    rules.push(rule(&root, &["menu", "popover contents"], format!("{material} border-radius: {radius}px; color: {fg}; border-color: {line};")));
    rules.push(rule(&root, &["popover arrow"], format!("background-image: none; background-color: {bg}; border-color: {line};")));
    rules.push(rule(&root, &[".card", ".power-card", ".device-list", ".settings-card", "frame > border"],
         format!("background-color: {surface}; border-color: {line}; color: {fg};")));
    rules.push(rule(&root, &["list", "list row", "listview", "gridview", "columnview", "columnview header", "stack", "scrolledwindow", ".editor-sidebar"],
         format!("background-color: transparent; color: {fg}; border-color: {line};")));
    rules.push(rule(&root, &["button", "entry", "spinbutton", "textview", "textview text", "treeview"],
         format!("background-image: none; background-color: {field}; color: {fg}; border-color: {line}; box-shadow: none; text-shadow: none;")));
    rules.push(rule(&root, &["button:hover", "menuitem:hover", "row:hover", "gridview child:hover"], format!("background-color: {hover}; color: {fg};")));
    rules.push(rule(&root, &["button:checked", "button:active", "row:selected", "gridview child:selected", "treeview:selected", "menuitem:hover"],
         format!("background-color: {selected}; color: {fg};")));
    rules.push(rule(&root, &["button:focus-visible", "entry:focus", "spinbutton:focus"], format!("outline-color: {ac}; border-color: {ac};")));
    // Text fields focus their inner text node, so the frame uses :focus-within.
    rules.push(rule(&root, &["entry:focus-within", "spinbutton:focus-within", "searchentry:focus-within", "textview:focus-within", "passwordentry:focus-within"],
         format!("outline: 2px solid alpha({ac}, 0.55); outline-offset: -2px; border-color: {ac};")));
    rules.push(rule(&root, &["*:focus-visible"], format!("outline-color: {ac};")));
    rules.push(rule(&root, &["entry", "spinbutton", "textview text"], format!("caret-color: {ac};")));
    rules.push(rule(&root, &["selection"], format!("background-color: {ac}; color: {on_accent};")));
    // Drag selection has a separate CSS node from selected items/text. Keep
    // its fill translucent and visible independently of window transparency.
    rules.push(rule(&root, &["rubberband", ".rubberband"],
         format!("background-image: none; background-color: rgba({},{},{},0.22); border: 1px solid {ac}; box-shadow: none;",
                 accent[0], accent[1], accent[2])));
    rules.push(rule(&root, &["button.titlebutton", "button.icon-button", "button.profile-row", "button.calendar-day"],
         format!("background-color: transparent; color: {fg}; border-color: transparent;")));
    rules.push(rule(&root, &["button.titlebutton:hover", "button.icon-button:hover", "button.profile-row:hover", "button.calendar-day:hover"],
         format!("background-color: {hover};")));
    rules.push(rule(&root, &["button.primary", "button.suggested-action", "button.calendar-day.selected"],
         format!("background-color: {}; color: {on_accent}; border-color: {ac};", background_color(accent))));
    rules.push(rule(&root, &["button.connect-action", "button.profile-row.selected"], format!("color: {ac}; background-color: {selected}; border-color: transparent;")));
    rules.push(rule(&root, &["button.calendar-day.today"], format!("color: {ac}; border-color: {ac};")));
    rules.push(rule(&root, &["button.calendar-day.today.selected"], format!("color: {on_accent};")));
    rules.push(rule(&root, &["button.calendar-day.adjacent"], format!("color: {faint};")));
    rules.push(rule(&root, &[".muted", ".subtitle", ".page-subtitle", ".secondary-label", ".status", ".event-bar", ".device-address", ".empty",
           ".empty-label", ".calendar-weekday", ".settings-muted"], format!("color: {muted};")));
    rules.push(rule(&root, &[".empty-icon", ".off-symbol"], format!("color: {faint};")));
    rules.push(rule(&root, &[".badge", ".count-badge"], format!("background-color: {field}; color: {muted};")));
    rules.push(rule(&root, &[".bluetooth-tile", ".audio-tile", ".power-tile", ".network-tile", ".calendar-tile", ".brightness-tile", ".device-icon",
           ".settings-tile"], format!("color: {ac}; background-color: {selected};")));
    rules.push(rule(&root, &[".profile-check", ".device-icon.connected", ".settings-section-heading image", ".settings-navigation row:selected image"],
         format!("color: {ac};")));
    rules.push(rule(&root, &[".connection-status", ".connected-count", ".badge.connected"], format!("color: {green};")));
    rules.push(rule(&root, &[".error", ".event-bar.error", ".status.error", "button.destructive-action"], format!("color: {error};")));
    rules.push(rule(&root, &["separator"], format!("background-color: {line};")));
    rules.push(rule(&root, &["switch"], format!("background-image: none; background-color: {line}; border-color: {line}; border-radius: 9999px;")));
    // Knobs are white in every state with a soft shadow, as in GNOME and macOS;
    // only the track changes, taking the accent once checked.
    let knob = "background-image: none; background-color: #ffffff; border-color: transparent; \
                box-shadow: 0 1px 2px rgba(0,0,0,.3); border-radius: 9999px;";
    rules.push(rule(&root, &["switch slider", "switch:checked slider", "switch:hover slider", "switch:disabled slider", "switch:backdrop slider"], knob));
    // Every scale thumb, with or without marks, is the same 18px circle.
    rules.push(rule(&root, &["scale slider", "scale.marks-before slider", "scale.marks-after slider", "scale.marks-before.marks-after slider",
           "scale:hover slider", "scale slider:hover", "scale slider:active", "scale:disabled slider"],
         format!("{knob} min-width: 18px; min-height: 18px; margin: -7px; padding: 0; border-width: 0;")));
    rules.push(rule(&root, &["scale trough"], "min-height: 4px; min-width: 4px; border-radius: 9999px;"));
    rules.push(rule(&root, &["scale highlight", "scale fill"], "border-radius: 9999px;"));
    rules.push(rule(&root, &["switch:checked", "scale highlight", "progressbar progress", "levelbar block.filled"],
         format!("background-image: none; background-color: {ac}; border-color: {ac};")));
    rules.push(rule(&root, &["switch:disabled"], format!("background-color: {line}; border-color: {line};")));
    rules.push(rule(&root, &["scale trough", "progressbar trough", "levelbar trough", "levelbar block.empty"],
         format!("background-image: none; background-color: {field}; border-color: transparent;")));
    rules.push(rule(&root, &["check", "radio"], format!("background-color: {field}; border-color: {line}; color: {fg};")));
    rules.push(rule(&root, &["check:checked", "radio:checked"], format!("background-color: {ac}; border-color: {ac}; color: {on_accent};")));
    rules.push(rule(&root, &["notebook header", "notebook stack"], format!("background-color: transparent; border-color: {line}; color: {fg};")));
    rules.push(rule(&root, &["notebook tab:checked"], format!("background-color: {selected}; color: {fg}; border-color: {ac}; box-shadow: inset 0 -2px {ac};")));
    rules.push(rule(&root, &["notebook header.bottom tab:checked"], format!("box-shadow: inset 0 2px {ac};")));
    rules.push(rule(&root, &["notebook header.left tab:checked"], format!("box-shadow: inset -2px 0 {ac};")));
    rules.push(rule(&root, &["notebook header.right tab:checked"], format!("box-shadow: inset 2px 0 {ac};")));
    rules.push(rule(&root, &["scrollbar"], "background-color: transparent;"));
    rules.push(rule(&root, &["scrollbar slider"], format!("background-color: {line}; border-color: transparent;")));
    // Compact title bar without top or bottom borders; its tools never draw a
    // background. Hover brightens an icon and a checked toggle takes the accent.
    rules.push(rule(&root, &["headerbar"], "min-height: 34px; padding-top: 0; padding-bottom: 0; border-top-width: 0; border-bottom-width: 0;"));
    rules.push(rule(&root, &["headerbar button", "headerbar button:hover", "headerbar button:active", "headerbar button:checked",
           "headerbar button:focus:not(:focus-visible)", "headerbar menubutton > button"],
         "min-height: 24px; padding: 2px 5px; margin-top: 0; margin-bottom: 0; background: none; border-color: transparent; box-shadow: none;"));
    rules.push(rule(&root, &["headerbar button image"], "-gtk-icon-filter: opacity(.8);"));
    rules.push(rule(&root, &["headerbar button:hover image", "headerbar button:checked image"], "-gtk-icon-filter: none;"));
    rules.push(rule(&root, &["headerbar button:checked"], format!("color: {ac};")));
    rules.push(rule(&root, &["headerbar .title"], "font-weight: 600;"));
    rules.push(window_controls_css(&format!("window{root}"), foreground, options.colored_buttons));
    rules.push(window_frame_css(&format!("window{root}")));
    rules.push(rule(&root, &[".brightness-controls", ".monitor-frame"], format!("background-color: {surface}; color: {fg}; border-color: {line};")));
    rules.push(rule(&root, &[".monitor-frame", ".snap-flash"], format!("border-color: {ac};")));
    rules.push(rule(&root, &[".panel-bar"], format!("background-color: {ac}; border-color: {ac};")));
    rules.push(rule(&root, &[".overlay-label", ".connector-label", ".resolution-label", ".device-label"], format!("color: {fg};")));
    rules.push(rule(&root, &[".overlay-box"], format!("{material} border-radius: {radius}px; color: {fg}; border-color: {line};")));
    Ok(rules.join("\n"))
}

/// The per-scope square-corner override (USER+3) used when rounding is off.
pub fn square_corners_css(scope: &str) -> String {
    let controls = crate::palette::ROUNDED_CONTROLS.iter().map(|s| format!(".{scope} {s}")).collect::<Vec<_>>().join(", ");
    format!(".{scope}, .{scope} * {{ border-radius: 0; }}\n{controls} {{ border-radius: 9999px; }}")
}

struct ClientInner {
    path: PathBuf,
    palette: RefCell<Option<PaletteFile>>,
    bindings: RefCell<Vec<Weak<ThemeInner>>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    timer: RefCell<Option<glib::SourceId>>,
    pending: RefCell<Option<glib::SourceId>>,
    payload: RefCell<Option<Vec<u8>>>,
}

/// One small palette file watcher shared by all windows in an app process.
#[derive(Clone)]
pub struct PaletteClient(Rc<ClientInner>);

impl PaletteClient {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self(Rc::new(ClientInner {
            path: path.unwrap_or_else(palette_path), palette: RefCell::new(None), bindings: RefCell::new(Vec::new()),
            monitor: RefCell::new(None), timer: RefCell::new(None), pending: RefCell::new(None), payload: RefCell::new(None),
        }))
    }

    /// The last valid palette, if any.
    pub fn palette(&self) -> Option<PaletteFile> { self.0.palette.borrow().clone() }

    fn bindings(&self) -> Vec<WindowTheme> {
        let mut bindings = self.0.bindings.borrow_mut();
        bindings.retain(|weak| weak.strong_count() > 0);
        bindings.iter().filter_map(Weak::upgrade).map(WindowTheme).collect()
    }

    fn add(&self, binding: &WindowTheme) {
        let first = self.bindings().is_empty();
        if !self.0.bindings.borrow().iter().any(|w| std::ptr::eq(w.as_ptr(), Rc::as_ptr(&binding.0))) {
            self.0.bindings.borrow_mut().push(Rc::downgrade(&binding.0));
        }
        if first {
            self.watch();
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_seconds_local(2, move || {
                let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
                PaletteClient(inner).poll()
            });
            if let Some(old) = self.0.timer.replace(Some(id)) {
                old.remove();
            }
        }
        self.refresh();
        binding.refresh();
    }

    fn remove(&self, binding: &WindowTheme) {
        self.0.bindings.borrow_mut().retain(|w| !std::ptr::eq(w.as_ptr(), Rc::as_ptr(&binding.0)) && w.strong_count() > 0);
        if self.0.bindings.borrow().is_empty() {
            self.close();
        }
    }

    fn watch(&self) {
        if self.0.monitor.borrow().is_some() {
            return;
        }
        // The dock publishes by atomic replacement. Watch its directory so
        // a long-running process follows every replacement inode.
        let Some(parent) = self.0.path.parent() else { return };
        let Ok(monitor) = gio::File::for_path(parent).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>)
        else { return };
        let weak = Rc::downgrade(&self.0);
        monitor.connect_changed(move |_, file, other, _| {
            let Some(inner) = weak.upgrade() else { return };
            let client = PaletteClient(inner);
            let ours = [Some(file), other].into_iter().flatten().any(|f| f.path().as_deref() == Some(client.0.path.as_path()));
            if ours && !client.bindings().is_empty() && client.0.pending.borrow().is_none() {
                let weak = Rc::downgrade(&client.0);
                let id = glib::timeout_add_local_once(Duration::from_millis(60), move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.pending.replace(None);
                        PaletteClient(inner).refresh();
                    }
                });
                client.0.pending.replace(Some(id));
            }
        });
        self.0.monitor.replace(Some(monitor));
    }

    fn poll(&self) -> glib::ControlFlow {
        let bindings = self.bindings();
        if bindings.is_empty() {
            self.0.timer.replace(None);
            return glib::ControlFlow::Break;
        }
        self.watch();
        self.refresh();
        // On Wayland a move between same-scale outputs need not generate a
        // configure event. Resolve each mapped window's actual output too.
        for binding in bindings {
            binding.refresh();
        }
        glib::ControlFlow::Continue
    }

    /// Re-read the palette file; refresh every window when the palette changed.
    pub fn refresh(&self) {
        let read = (|| -> Result<(Vec<u8>, PaletteFile), String> {
            use std::io::Read;
            let mut data = Vec::new();
            std::fs::File::open(&self.0.path).map_err(|e| e.to_string())?
                .take(MAX_PALETTE_BYTES as u64 + 1).read_to_end(&mut data).map_err(|e| e.to_string())?;
            if data.len() > MAX_PALETTE_BYTES {
                return Err("Palette is too large".into());
            }
            if self.0.payload.borrow().as_deref() == Some(&data[..]) {
                return Err(String::new());
            }
            let value: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
            Ok((data.clone(), validate_palette(&value)?))
        })();
        let (data, palette) = match read {
            Err(unchanged) if unchanged.is_empty() => return,
            Err(_) => (None, None),
            Ok((data, palette)) => (Some(data), Some(palette)),
        };
        let changed = palette != *self.0.palette.borrow();
        self.0.payload.replace(data);
        self.0.palette.replace(palette);
        if changed {
            for binding in self.bindings() {
                binding.refresh();
            }
        }
    }

    pub fn close(&self) {
        if let Some(monitor) = self.0.monitor.take() {
            monitor.cancel();
        }
        for source in [self.0.timer.take(), self.0.pending.take()].into_iter().flatten() {
            source.remove();
        }
    }
}

thread_local!(static CLIENT: RefCell<Option<PaletteClient>> = const { RefCell::new(None) });
static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct ThemeInner {
    window: glib::WeakRef<gtk::Window>,
    client: PaletteClient,
    scope: String,
    provider: RefCell<Option<gtk::CssProvider>>,
    corner_provider: RefCell<Option<gtk::CssProvider>>,
    controls_provider: RefCell<Option<gtk::CssProvider>>,
    display: RefCell<Option<gdk::Display>>,
    signature: RefCell<Option<(Option<String>, bool)>>,
    closed: Cell<bool>,
    monitor_name: RefCell<Option<String>>,
    pending: RefCell<Option<glib::SourceId>>,
    blur: BlurBinding,
    signals: RefCell<Vec<glib::SignalHandlerId>>,
}

/// A window following the dock palette; returned by [`attach`].
#[derive(Clone)]
pub struct WindowTheme(Rc<ThemeInner>);

impl WindowTheme {
    fn new(window: &gtk::Window, client: PaletteClient) -> Self {
        let scope = format!("tessel-dock-companion-{}", SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let theme = Self(Rc::new(ThemeInner {
            window: window.downgrade(), client: client.clone(), scope: scope.clone(), provider: RefCell::new(None),
            corner_provider: RefCell::new(None), controls_provider: RefCell::new(None), display: RefCell::new(None), signature: RefCell::new(None),
            closed: Cell::new(false), monitor_name: RefCell::new(None), pending: RefCell::new(None),
            blur: blur::attach(window, None, true, true), signals: RefCell::new(Vec::new()),
        }));
        window.add_css_class(&scope);
        let mut signals = Vec::new();
        let event = |weak: Weak<ThemeInner>| move || if let Some(inner) = weak.upgrade() { WindowTheme(inner).event() };
        let handler = event(Rc::downgrade(&theme.0));
        signals.push(window.connect_map(move |_| handler()));
        for property in ["scale-factor", "transient-for", "is-active", "modal"] {
            let handler = event(Rc::downgrade(&theme.0));
            signals.push(window.connect_notify_local(Some(property), move |_, _| handler()));
        }
        let weak = Rc::downgrade(&theme.0);
        signals.push(window.connect_unrealize(move |_| {
            // A destroyed GTK4 window may remain referenced by its controller.
            // Stop watching while unrealized; a later realization resumes it.
            if let Some(inner) = weak.upgrade() {
                let theme = WindowTheme(inner);
                theme.remove_provider();
                theme.0.client.remove(&theme);
                theme.0.signature.replace(None);
            }
        }));
        let weak = Rc::downgrade(&theme.0);
        signals.push(window.connect_realize(move |_| {
            if let Some(inner) = weak.upgrade() {
                let theme = WindowTheme(inner);
                if !theme.0.closed.get() {
                    theme.0.client.add(&theme);
                }
            }
        }));
        theme.0.signals.replace(signals);
        client.add(&theme);
        theme
    }

    /// The scope class (`tessel-dock-companion-<n>`) added to the window.
    pub fn scope(&self) -> &str { &self.0.scope }
    /// Name of the palette monitor this window currently follows.
    pub fn monitor_name(&self) -> Option<String> { self.0.monitor_name.borrow().clone() }
    /// The palette stylesheet currently installed, if any.
    pub fn provider(&self) -> Option<gtk::CssProvider> { self.0.provider.borrow().clone() }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }
    pub fn blur(&self) -> &BlurBinding { &self.0.blur }

    fn event(&self) {
        self.0.client.refresh();
        self.refresh();
        if !self.0.closed.get() && self.0.pending.borrow().is_none() {
            // Wayland reports the surface's entered output after its first
            // map/configure. Recheck once that initial event burst settles.
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_local_once(Duration::from_millis(40), move || {
                if let Some(inner) = weak.upgrade() {
                    inner.pending.replace(None);
                    WindowTheme(inner).refresh();
                }
            });
            self.0.pending.replace(Some(id));
        }
    }

    fn geometry(&self, window: &gtk::Window) -> Option<[i64; 4]> {
        let mut native = window.surface();
        if !window.is_mapped() {
            if let Some(parent) = window.transient_for() {
                native = parent.surface().or(native);
            }
        }
        let display = WidgetExt::display(window);
        let mut monitor = if window.is_layer_window() { window.monitor() } else { None };
        if monitor.is_none() {
            monitor = native.and_then(|surface| display.monitor_at_surface(&surface));
        }
        let geometry = monitor?.geometry();
        Some([geometry.x() as i64, geometry.y() as i64, geometry.width() as i64, geometry.height() as i64])
    }

    /// Re-resolve the window's output and restyle it if its palette changed.
    pub fn refresh(&self) {
        let Some(window) = self.0.window.upgrade() else { return };
        if self.0.closed.get() {
            return;
        }
        let palette = self.0.client.palette();
        let entry = palette_for_geometry(palette.as_ref(), self.geometry(&window)).cloned();
        self.0.monitor_name.replace(entry.as_ref().map(|e| e.name.clone()));
        let target = if window.has_css_class("tray-popup") { "popup-surface" }
                     else if window.has_css_class("way-displays-overlay") { "overlay-box" } else { "" };
        let backdrop = window.has_css_class("desktop-applet-backdrop");
        self.0.blur.configure(entry.is_some() && palette.as_ref().is_some_and(|p| p.apps_frosted) && !backdrop,
                              palette.as_ref().map_or(50, |p| p.apps_frost_strength),
                              palette.as_ref().is_none_or(|p| p.apps_rounded_corners), Some(target), Some(!backdrop));
        let data = match (&entry, &palette) {
            (Some(entry), Some(palette)) => {
                let transparency = if palette.apps_transparency_enabled { palette.apps_transparency } else { 0 };
                theme_css(&self.0.scope, entry, palette.is_dark, &ThemeOptions {
                    transparency, frosted: palette.apps_frosted, frost_strength: palette.apps_frost_strength,
                    rounded_corners: palette.apps_rounded_corners, colored_buttons: palette.colored_window_buttons,
                    dialog: is_dialog(&window) }).ok()
            }
            _ => None,
        };
        let display = WidgetExt::display(&window);
        let square = palette.as_ref().is_some_and(|p| !p.apps_rounded_corners);
        let signature = Some((data.clone(), square));
        if *self.0.signature.borrow() == signature && self.0.display.borrow().as_ref() == Some(&display) {
            return;
        }
        self.remove_provider();
        self.0.signature.replace(signature);
        self.0.display.replace(Some(display.clone()));
        if let Some(data) = data {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&data);
            gtk::style_context_add_provider_for_display(&display, &provider, PRIORITY_APPLICATION + 30);
            self.0.provider.replace(Some(provider));
        }
        if let (Some(entry), Some(palette)) = (&entry, &palette) {
            // GTK reads the user's gtk.css once, when the process starts, and
            // above application CSS. Its window buttons are the ones of the
            // layout chosen back then, so this window's current style goes
            // above it, or a layout switch would not reach open windows.
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&window_controls_css(&format!("window.{}", self.0.scope), entry.foreground, palette.colored_window_buttons));
            gtk::style_context_add_provider_for_display(&display, &provider, PRIORITY_USER + 2);
            self.0.controls_provider.replace(Some(provider));
        }
        if square {
            // Radius alone takes precedence over app-specific styles (Files'
            // navigation, popup wrappers, and native GTK controls). Keeping it
            // separate preserves their intentional colors and flat toolbars;
            // switches and slider tracks/thumbs stay rounded for legibility.
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&square_corners_css(&self.0.scope));
            gtk::style_context_add_provider_for_display(&display, &provider, PRIORITY_USER + 3);
            self.0.corner_provider.replace(Some(provider));
        }
    }

    fn remove_provider(&self) {
        if let Some(display) = self.0.display.take() {
            for provider in [self.0.provider.take(), self.0.corner_provider.take(), self.0.controls_provider.take()].into_iter().flatten() {
                gtk::style_context_remove_provider_for_display(&display, &provider);
            }
        }
        self.0.provider.replace(None);
        self.0.corner_provider.replace(None);
        self.0.controls_provider.replace(None);
    }

    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        self.0.blur.close();
        if let Some(id) = self.0.pending.take() {
            id.remove();
        }
        self.remove_provider();
        if let Some(window) = self.0.window.upgrade() {
            for handler in self.0.signals.take() {
                window.disconnect(handler);
            }
            window.remove_css_class(&self.0.scope);
            unsafe { window.steal_data::<WindowTheme>(THEME_KEY) };
        }
        self.0.client.remove(self);
    }
}

const THEME_KEY: &str = "tessel-dock-companion-theme";

/// Idempotently follow dock colors for a window (including child dialogs).
/// `client` defaults to the process-wide palette watcher.
pub fn attach(window: &impl IsA<gtk::Window>, client: Option<&PaletteClient>) -> WindowTheme {
    let window = window.upcast_ref::<gtk::Window>();
    // SAFETY: THEME_KEY only ever stores a WindowTheme.
    if let Some(existing) = unsafe { window.data::<WindowTheme>(THEME_KEY) } {
        let existing = unsafe { existing.as_ref() }.clone();
        if !existing.is_closed() {
            existing.refresh();
            return existing;
        }
    }
    let client = client.cloned().unwrap_or_else(|| CLIENT.with(|c| c.borrow_mut().get_or_insert_with(|| PaletteClient::new(None)).clone()));
    let theme = WindowTheme::new(window, client);
    unsafe { window.set_data(THEME_KEY, theme.clone()) };
    theme
}

/// Theme present and future GtkApplication-owned windows, including prompts.
pub fn attach_application(application: &impl IsA<gtk::Application>) {
    let application = application.upcast_ref::<gtk::Application>();
    if unsafe { application.data::<bool>("tessel-dock-theme-window-signal") }.is_none() {
        application.connect_window_added(|_, window| {
            attach(window, None);
        });
        unsafe { application.set_data("tessel-dock-theme-window-signal", true) };
    }
    for window in application.windows() {
        attach(&window, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn palette() -> Value {
        json!({"version": 1, "is_dark": true, "transparency": 20, "monitors": [
            {"name": "HEADLESS-1", "geometry": [0, 0, 1920, 1080],
             "background": [30, 15, 50], "foreground": [238, 238, 238], "accent": [105, 167, 255]},
            {"name": "HEADLESS-2", "geometry": [1920, 0, 1920, 1080],
             "background": [15, 50, 30], "foreground": [238, 238, 238], "accent": [105, 167, 255]}]})
    }

    fn with(base: Value, key: &str, value: Value) -> Value {
        let mut base = base;
        base[key] = value;
        base
    }

    #[test]
    fn palette_validation_and_unknown_monitor_fallback() {
        let value = validate_palette(&palette()).unwrap();
        assert_eq!(palette_for_geometry(Some(&value), Some([1920, 0, 1920, 1080])).unwrap().background, [15, 50, 30]);
        assert!(palette_for_geometry(Some(&value), Some([0, 0, 800, 600])).is_none());
        for (field, wrong) in [("version", json!(2)), ("is_dark", json!(1)), ("monitors", json!({}))] {
            assert!(validate_palette(&with(palette(), field, wrong)).is_err(), "{field}");
        }
        for (role, wrong) in [("background", json!([0, 0, 256])), ("accent", json!([true, 0, 0])), ("foreground", json!([0, 0])),
                              ("geometry", json!([0, 0, -1, 2]))] {
            let mut invalid = palette();
            invalid["monitors"][0][role] = wrong;
            assert!(validate_palette(&invalid).is_err(), "{role}");
        }
    }

    #[test]
    fn application_transparency_is_validated_and_legacy_dock_value_is_ignored() {
        for dock in [0, 20, 100] {
            let value = validate_palette(&with(palette(), "transparency", json!(dock))).unwrap();
            assert!(!value.apps_transparency_enabled);
            assert_eq!(value.apps_transparency, 5);
        }
        for amount in [0, 50, 100] {
            let value = validate_palette(&with(with(palette(), "apps_transparency_enabled", json!(true)), "apps_transparency", json!(amount))).unwrap();
            assert!(value.apps_transparency_enabled);
            assert_eq!(value.apps_transparency, amount);
        }
        for (key, invalid) in [("apps_transparency_enabled", json!(1)), ("apps_transparency_enabled", json!("yes")),
                               ("apps_transparency", json!(true)), ("apps_transparency", json!(-1)),
                               ("apps_transparency", json!(101)), ("apps_transparency", json!(20.5))] {
            assert!(validate_palette(&with(palette(), key, invalid)).is_err(), "{key}");
        }
    }

    #[test]
    fn frosted_defaults_and_strength_validation() {
        let value = validate_palette(&palette()).unwrap();
        assert!(!value.apps_frosted && value.apps_rounded_corners);
        assert_eq!(value.apps_frost_strength, 50);
        assert_eq!(validate_palette(&with(palette(), "apps_transparency", json!(35))).unwrap().apps_frost_strength, 35);
        for strength in [0, 50, 100] {
            assert_eq!(validate_palette(&with(palette(), "apps_frost_strength", json!(strength))).unwrap().apps_frost_strength, strength);
        }
        for invalid in [json!(-1), json!(101), json!(true), json!(20.5), json!("20")] {
            assert!(validate_palette(&with(palette(), "apps_frost_strength", invalid)).is_err());
        }
        for key in ["apps_frosted", "apps_rounded_corners"] {
            for invalid in [json!(1), json!("true"), Value::Null] {
                assert!(validate_palette(&with(palette(), key, invalid)).is_err());
            }
        }
        assert_eq!(frost_background([30, 15, 50], 5), "background-image: none; background-color: rgba(30,15,50,0.950);");
        assert!(theme_css("tessel-dock-companion-1", &validate_palette(&palette()).unwrap().monitors[0], true,
                          &ThemeOptions { frost_strength: 101, ..Default::default() }).is_err());
    }

    #[test]
    fn generated_css_scope_controls_transparency_and_dialogs() {
        let mut entry = validate_palette(&palette()).unwrap().monitors[0].clone();
        for dark in [true, false] {
            let data = theme_css("tessel-dock-companion-123", &entry, dark, &ThemeOptions::default()).unwrap();
            assert!(!data.contains("@define-color"));
            assert!(data.lines().all(|rule| rule.contains(".tessel-dock-companion-123")));
            assert!(data.contains("window.tessel-dock-companion-123.tray-popup { background-image: none; background-color: transparent; border-radius: 12px; }"));
            let knob = data.lines().find(|l| l.contains("switch:checked slider")).unwrap();
            assert!(knob.contains("background-color: #ffffff;") && knob.contains("box-shadow: 0 1px 2px rgba(0,0,0,.3);"));
            let slider = data.lines().find(|l| l.contains("scale.marks-after slider")).unwrap();
            for part in ["min-width: 18px;", "min-height: 18px;", "border-radius: 9999px;"] {
                assert!(slider.contains(part));
            }
            assert!(data.contains("outline: 2px solid alpha(rgb(105,167,255), 0.55)"));
            for (amount, alpha) in [(0, "1.000"), (50, "0.500"), (100, "0.000")] {
                let data = theme_css("tessel-dock-companion-123", &entry, dark, &ThemeOptions { transparency: amount, ..Default::default() }).unwrap();
                assert!(data.contains(&format!("background-color: rgba(30,15,50,{alpha});")));
                assert!(data.contains("color: rgb(238,238,238);"));
                assert!(!data.contains("opacity:"));
            }
        }
        // Dialogs and prompts never let the window below bleed through.
        for (amount, alpha) in [(0, "1.000"), (10, "0.970"), (50, "0.970")] {
            let dialog = theme_css("tessel-dock-companion-123", &entry, true, &ThemeOptions { transparency: amount, dialog: true, ..Default::default() }).unwrap();
            assert!(dialog.contains(&format!(",{alpha});")));
        }
        assert!(theme_css("unsafe > *", &entry, true, &ThemeOptions::default()).is_err());
        assert!(theme_css("tessel-dock-companion-", &entry, true, &ThemeOptions::default()).is_err());
        // A near-grey published accent never reaches suggested buttons or switches.
        entry.accent = [119, 122, 130];
        let data = theme_css("tessel-dock-companion-123", &entry, true, &ThemeOptions::default()).unwrap();
        assert!(!data.contains("rgb(119,122,130)") && data.contains("rgb(120,174,237)"));
    }
}
