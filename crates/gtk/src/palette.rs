//! Wallpaper palette → CSS: the dock's colour math (tessel-dock/dock_style.py) and
//! the published GTK named colours (tessel-dock/gtk_color_scheme.py `color_css`).
//!
//! Tokens every component shares: pt font sizes, 12px surfaces / 8px controls,
//! white switch/scale knobs, an accent that is never grey (HLS saturation
//! ≥ 0.45) with ≥ 4.5:1 contrast on its surface, and GNOME-blue fallbacks.

use crate::theme::{frost_background, window_controls_css, window_frame_css};

pub type Rgb = [u8; 3];

pub fn relative_luminance(color: Rgb) -> f64 {
    let linear = |channel: u8| {
        let value = channel as f64 / 255.0;
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    linear(color[0]) * 0.2126 + linear(color[1]) * 0.7152 + linear(color[2]) * 0.0722
}

/// WCAG contrast ratio.
pub fn contrast(first: Rgb, second: Rgb) -> f64 {
    let (a, b) = (relative_luminance(first), relative_luminance(second));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Python `round()` (half to even) of a mix, as the palette code uses it.
pub fn round_half_even(value: f64) -> f64 {
    let rounded = value.round();
    if (value - value.trunc()).abs() == 0.5 { 2.0 * (value / 2.0).round() } else { rounded }
}

/// `tuple(round(a * (1 - amount) + b * amount))`.
pub fn mix(first: Rgb, second: Rgb, amount: f64) -> Rgb {
    std::array::from_fn(|i| round_half_even(first[i] as f64 * (1.0 - amount) + second[i] as f64 * amount) as u8)
}

/// A colorless wallpaper (or one whose hue is too weak to read as a color)
/// uses the familiar GNOME blues rather than a grey that looks disabled.
pub fn fallback_accent(is_dark: bool) -> Rgb {
    if is_dark { [120, 174, 237] } else { [53, 132, 228] }
}
pub const MIN_ACCENT_SATURATION: f64 = 0.45;
const COLORLESS_CHROMA: u8 = 14;
/// Accents when wallpaper matching is off.
pub fn default_accent(is_dark: bool) -> Rgb {
    if is_dark { [105, 167, 255] } else { [36, 112, 210] }
}

/// Python's `colorsys.rgb_to_hls`.
pub fn rgb_to_hls(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (low + high) / 2.0;
    if low == high {
        return (0.0, lightness, 0.0);
    }
    let range = high - low;
    let saturation = if lightness <= 0.5 { range / (high + low) } else { range / (2.0 - high - low) };
    let (rc, gc, bc) = ((high - r) / range, (high - g) / range, (high - b) / range);
    let hue = if r == high { bc - gc } else if g == high { 2.0 + rc - bc } else { 4.0 + gc - rc };
    ((hue / 6.0).rem_euclid(1.0), lightness, saturation)
}

/// Python's `colorsys.hls_to_rgb`.
pub fn hls_to_rgb(h: f64, l: f64, s: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (l, l, l);
    }
    let m2 = if l <= 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let m1 = 2.0 * l - m2;
    let v = |hue: f64| {
        let hue = hue.rem_euclid(1.0);
        if hue < 1.0 / 6.0 { m1 + (m2 - m1) * hue * 6.0 }
        else if hue < 0.5 { m2 }
        else if hue < 2.0 / 3.0 { m1 + (m2 - m1) * (2.0 / 3.0 - hue) * 6.0 }
        else { m1 }
    };
    (v(h + 1.0 / 3.0), v(h), v(h - 1.0 / 3.0))
}

/// Retain the wallpaper hue as a clearly coloured, legible accent.
///
/// Saturation never drops below MIN_ACCENT_SATURATION, so selected rows,
/// suggested buttons, switches and the active workspace never look disabled;
/// only lightness moves until the accent reaches 4.5:1 on the surface.
pub fn wallpaper_accent(color: Rgb, background: Rgb, is_dark: bool) -> Rgb {
    let spread = color.iter().max().unwrap() - color.iter().min().unwrap();
    let color = if spread < COLORLESS_CHROMA { fallback_accent(is_dark) } else { color };
    let (hue, lightness, saturation) = rgb_to_hls(color[0] as f64 / 255.0, color[1] as f64 / 255.0, color[2] as f64 / 255.0);
    let saturation = saturation.max(MIN_ACCENT_SATURATION);
    // Start from a mid lightness so the hue stays visible, then step toward
    // white (dark mode) or black (light mode) only as far as legibility needs.
    let lightness = if is_dark { lightness.clamp(0.55, 0.72) } else { lightness.clamp(0.36, 0.45) };
    let target = if is_dark { 1.0 } else { 0.0 };
    let mut candidate = color;
    for step in 0..=100 {
        let amount = step as f64 / 100.0;
        let value = lightness * (1.0 - amount) + target * amount;
        let (r, g, b) = hls_to_rgb(hue, value, saturation);
        candidate = [r, g, b].map(|channel| round_half_even(channel * 255.0) as u8);
        if contrast(candidate, background) >= 4.5 {
            return candidate;
        }
    }
    candidate
}

/// `{"background", "foreground", "accent"}` of one surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub accent: Rgb,
}

/// The shared dock and companion-app palette, with readable foregrounds.
pub fn color_palette(is_dark: bool, wallpaper: Option<Rgb>) -> Palette {
    let mut base: Rgb = if is_dark { [0, 0, 0] } else { [250, 250, 252] };
    if let Some(wallpaper) = wallpaper {
        let strength = if is_dark { 0.35 } else { 0.18 };
        base = std::array::from_fn(|i| round_half_even(base[i] as f64 * (1.0 - strength) + wallpaper[i] as f64 * strength) as u8);
    }
    let accent = match wallpaper {
        Some(wallpaper) => wallpaper_accent(wallpaper, base, is_dark),
        None => default_accent(is_dark),
    };
    Palette { background: base, foreground: if is_dark { [238, 238, 238] } else { [35, 38, 45] }, accent }
}

/// Parse `#RRGGBB`.
pub fn parse_hex(color: &str) -> Option<Rgb> {
    let hex = color.strip_prefix('#').filter(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()))?;
    Some(std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()))
}

/// A manual panel colour (`#RRGGBB`) is used exactly, with readable text and accents.
pub fn surface_palette(is_dark: bool, wallpaper: Option<Rgb>, background_color: Option<Rgb>) -> Palette {
    let Some(base) = background_color else { return color_palette(is_dark, wallpaper) };
    let dark = relative_luminance(base) < 0.179;
    let mut palette = color_palette(dark, None);
    palette.background = base;
    palette.accent = wallpaper_accent(palette.accent, base, dark);
    palette
}

/// Light surfaces over a dark wallpaper turn muddy grey when very translucent,
/// so light mode keeps panels, docks, menus and popups at least this opaque.
pub const LIGHT_MIN_OPACITY: f64 = 0.85;

/// Opacity for a surface colour; light surfaces never drop below LIGHT_MIN_OPACITY.
pub fn surface_alpha(background: Rgb, transparency: u8) -> f64 {
    let alpha = (100.0 - transparency as f64) / 100.0;
    if relative_luminance(background) >= 0.179 { alpha.max(LIGHT_MIN_OPACITY) } else { alpha }
}

fn join(color: Rgb) -> String {
    format!("{}, {}, {}", color[0], color[1], color[2])
}

/// Options of the dock's `background_css` (Python keyword defaults).
#[derive(Clone, Debug)]
pub struct PanelStyle<'a> {
    pub transparency: u8,
    pub wallpaper: Option<Rgb>,
    pub rounded_corners: bool,
    pub inner_rounded_corners: Option<bool>,
    pub frosted: bool,
    pub frost_strength: u8,
    pub scope: Option<&'a str>,
    pub background_color: Option<Rgb>,
    /// Any of `top`, `bottom`, `left`, `right`.
    pub border_sides: &'a [&'a str],
    pub border_color: Option<Rgb>,
    pub shine: bool,
    /// A soft drop shadow outside the panel (the surface must leave room for it).
    pub shadow: bool,
}

impl Default for PanelStyle<'_> {
    fn default() -> Self {
        Self { transparency: 5, wallpaper: None, rounded_corners: true, inner_rounded_corners: None, frosted: false,
               frost_strength: 50, scope: None, background_color: None, border_sides: &[], border_color: None, shine: false,
               shadow: false }
    }
}

/// Drop-shadow offset (down) and blur radius; [`SHADOW_ROOM`] holds both.
pub const SHADOW_OFFSET: i32 = 4;
pub const SHADOW_BLUR: i32 = 16;
/// Transparent room a surface leaves around a shadowed panel.
pub const SHADOW_ROOM: i32 = SHADOW_BLUR + SHADOW_OFFSET;

/// Style one dock panel and its accents, keeping icons and text fully opaque.
pub fn background_css(is_dark: bool, style: &PanelStyle) -> Result<String, String> {
    if style.transparency > 100 || style.frost_strength > 100 {
        return Err("Transparency must be between 0 and 100 percent.".into());
    }
    let palette = surface_palette(is_dark, style.wallpaper, style.background_color);
    // Paint inside the panel, without affecting its size or exclusive zone.
    // Inset shadows also follow rounded corners and never extend into the float gap.
    let offsets = [("top", (0, 1)), ("bottom", (0, -1)), ("left", (1, 0)), ("right", (-1, 0))];
    if style.border_sides.iter().any(|side| !offsets.iter().any(|(name, _)| name == side)) {
        return Err("Invalid bar border sides.".into());
    }
    let border_rgb = join(style.border_color.unwrap_or(palette.accent));
    let mut parts: Vec<String> = offsets.iter().filter(|(side, _)| style.border_sides.contains(side))
        .map(|(_, (x, y))| format!("inset {x}px {y}px 0 0 rgb({border_rgb})")).collect();
    if style.shine {
        // A soft 1px highlight along the top edge, like a macOS dock.
        parts.insert(0, "inset 0 1px 0 0 rgba(255, 255, 255, 0.16)".into());
    }
    if style.shadow {
        parts.insert(0, format!("0 {SHADOW_OFFSET}px {SHADOW_BLUR}px 0 rgba(0, 0, 0, 0.55)"));
    }
    let border = if parts.is_empty() { "none".to_owned() } else { parts.join(", ") };
    let base = palette.background;
    let (accent, foreground) = (join(palette.accent), join(palette.foreground));
    let panel = style.scope.map_or(".tessel-dock-panel".to_owned(), |scope| format!(".{scope}.tessel-dock-panel"));
    let alpha = surface_alpha(base, style.transparency);
    let material = if style.frosted {
        frost_background(base, round_half_even(100.0 - alpha * 100.0) as u8)
    } else {
        format!("background-image: none; background-color: rgba({}, {}, {}, {alpha:.3});", base[0], base[1], base[2])
    };
    // Semantic warning, battery and VPN colors keep their separate meaning.
    let ordinary = ":not(.power-saver):not(.full):not(.vpn):not(.performance):not(.dimmed):not(.low):not(.limited):not(.critical):not(.needs-attention)";
    let status = ["connected", "plugged", "balanced", "auto", "active"].iter()
        .map(|state| format!("{panel} button.tessel-dock-status.{state}{ordinary}")).collect::<Vec<_>>().join(",\n");
    let radius = if style.rounded_corners { 12 } else { 0 };
    let inner = if style.inner_rounded_corners.unwrap_or(style.rounded_corners) { 8 } else { 0 };
    Ok(format!("
{panel} {{ {material}
    background-clip: border-box;
    border-radius: {radius}px;
    border: none; box-shadow: {border}; color: rgb({foreground});
}}
{panel} button:active {{ background-color: rgba({accent}, 0.24); }}
{panel} button.focused {{ background-color: rgba({accent}, 0.16); }}
{panel} button:focus-visible, {panel} entry:focus-within {{ outline-color: rgb({accent}); }}
{panel} .tessel_dock-indicator.focused {{ background-color: rgb({accent}); }}
{panel} button.tessel_dock-workspace {{ border-radius: {inner}px; }}
{panel} button.tessel_dock-workspace.active {{
    background-color: rgba({accent}, 0.38); box-shadow: inset 0 0 0 1px rgba({accent}, 0.70);
    font-weight: 700;
}}
{panel} button.tessel_dock-workspace.active:not(.urgent) {{ color: rgb({foreground}); }}
{status} {{ color: rgb({accent}); }}
"))
}

/// Scope each dock clone's open menus (`.{scope} .tessel-dock-popup`, context
/// popovers) to its own wallpaper-derived palette.
pub fn popup_css(scope: &str, is_dark: bool, wallpaper: Option<Rgb>, transparency: u8, frosted: bool,
                 rounded_corners: bool, frost_strength: u8, background_color: Option<Rgb>) -> Result<String, String> {
    if transparency > 100 || frost_strength > 100 {
        return Err("Transparency must be between 0 and 100 percent.".into());
    }
    let radius = if rounded_corners { 12 } else { 0 };
    let palette = surface_palette(is_dark, wallpaper, background_color);
    let alpha = surface_alpha(palette.background, transparency);
    let (background, foreground, accent) = (join(palette.background), join(palette.foreground), join(palette.accent));
    let material = if frosted {
        frost_background(palette.background, round_half_even(100.0 - alpha * 100.0) as u8)
    } else {
        format!("background-image: none; background-color: rgba({background}, {alpha:.3});")
    };
    let popup = format!(".{scope} .tessel-dock-popup");
    let menu = format!("popover.{scope}.tessel-dock-context-menu contents");
    let mut corners = String::new();
    if !rounded_corners {
        corners = format!("{popup}, {popup} *, {menu}, {menu} * {{ border-radius: 0; }}");
        let controls = [&popup, &menu].iter().flat_map(|root| ROUNDED_CONTROLS.iter().map(move |s| format!("{root} {s}")))
            .collect::<Vec<_>>().join(", ");
        corners += &format!("\n{controls} {{ border-radius: 9999px; }}");
    }
    Ok(format!("
popover.{scope}.tessel_dock-context-menu {{ background-image: none; background-color: transparent; box-shadow: none; }}
{popup}, {menu} {{
    {material} border-radius: {radius}px;
    color: rgb({foreground}); border-color: rgba({foreground}, 0.14);
}}
{popup} button, {popup} flowbox, {popup} list, {popup} row,
{menu} button {{ color: rgb({foreground}); }}
{popup} .tessel_dock-subtitle {{ color: rgba({foreground}, 0.65); }}
{popup} button:hover, {popup} flowboxchild:hover, {popup} row:hover,
{menu} button:hover {{ background-color: rgba({foreground}, 0.09); }}
{popup} button.focused, {popup} flowboxchild:selected, {popup} row:selected,
{menu} button:selected, {menu} button:checked {{ background-color: rgba({accent}, 0.18); }}
{popup} button:active, {menu} button:active {{ background-color: rgba({accent}, 0.24); }}
{popup} flowboxchild:focus, {popup} button:focus-visible, {popup} row:focus-visible,
{menu} button:focus-visible {{ outline-color: rgb({accent}); }}
{popup} .tessel_dock-indicator.focused {{ background-color: rgb({accent}); }}
{popup} .tessel_dock-app-menu-header {{ border-bottom-color: rgba({foreground}, 0.10); }}
{popup} separator {{ background-color: rgba({foreground}, 0.10); }}
{popup} button.tessel_dock-app-pin {{ color: rgba({foreground}, 0.65); }}
{popup} button.tessel_dock-app-pin.pinned {{ color: rgb({accent}); }}
{popup} button.tessel_dock-app-pin:hover {{ color: rgb({foreground}); }}
{popup} button.tessel_dock-app-menu-settings {{ background-color: rgba({foreground}, 0.05); }}
{popup} entry {{
    background-image: none; background-color: mix(rgba({background}, {alpha:.3}), rgba({foreground}, {alpha:.3}), 0.07);
    color: rgb({foreground}); border-color: rgba({foreground}, 0.14);
}}
{popup} entry:focus, {popup} entry:focus-within {{ border-color: rgb({accent}); outline-color: rgba({accent}, 0.55); }}
{popup} entry selection {{ background-color: rgba({accent}, 0.30); color: rgb({foreground}); }}
{corners}
"))
}

/// Controls that stay round when square corners are chosen.
pub const ROUNDED_CONTROLS: [&str; 8] = ["switch", "switch slider", "scale trough", "scale highlight", "scale fill",
                                         "scale slider", "scrollbar trough", "scrollbar slider"];

fn legible_foreground(color: Rgb) -> Rgb {
    if contrast(color, [0, 0, 0]) >= contrast(color, [255, 255, 255]) { [0, 0, 0] } else { [255, 255, 255] }
}

fn standalone_accent(accent: Rgb, background: Rgb, foreground: Rgb) -> Rgb {
    (0..33).map(|step| mix(accent, foreground, step as f64 / 32.0))
        .find(|candidate| contrast(*candidate, background) >= 4.5).unwrap_or(foreground)
}

pub const GTK_GENERATED_HEADER: &str = "/* Tessel Dock managed GTK4 wallpaper colors. */";

/// `~/.config/gtk-4.0/tessel-dock-colors.css`: opaque, readable colours under
/// GTK's legacy names and libadwaita's public names, plus title-bar controls.
pub fn color_css(is_dark: bool, palette: &Palette, colored_buttons: bool) -> String {
    let Palette { background, mut foreground, accent } = *palette;
    if contrast(background, foreground) < 4.5 {
        foreground = legible_foreground(background);
    }
    let neutral = if is_dark { [0, 0, 0] } else { [255, 255, 255] };
    let view = mix(background, neutral, if is_dark { 0.14 } else { 0.45 });
    let surface = mix(background, foreground, 0.045);
    let raised = mix(background, foreground, 0.07);
    let line = mix(background, foreground, 0.20);
    let accent_fg = legible_foreground(accent);
    let standalone = standalone_accent(accent, background, foreground);
    // Keep foregrounds readable even if a caller supplied an unusual palette.
    let text_on = |color: Rgb| if contrast(color, foreground) >= 4.5 { foreground } else { legible_foreground(color) };
    let colors: Vec<(&str, Rgb)> = vec![
        ("window_bg_color", background), ("window_fg_color", foreground),
        ("view_bg_color", view), ("view_fg_color", text_on(view)),
        ("headerbar_bg_color", surface), ("headerbar_fg_color", text_on(surface)),
        ("headerbar_border_color", text_on(surface)), ("headerbar_backdrop_color", surface),
        ("sidebar_bg_color", surface), ("sidebar_fg_color", text_on(surface)), ("sidebar_backdrop_color", surface),
        ("secondary_sidebar_bg_color", background), ("secondary_sidebar_fg_color", foreground),
        ("secondary_sidebar_backdrop_color", background),
        ("card_bg_color", raised), ("card_fg_color", text_on(raised)),
        ("dialog_bg_color", raised), ("dialog_fg_color", text_on(raised)),
        ("popover_bg_color", raised), ("popover_fg_color", text_on(raised)),
        ("overview_bg_color", surface), ("overview_fg_color", text_on(surface)),
        ("thumbnail_bg_color", raised), ("thumbnail_fg_color", text_on(raised)),
        ("active_toggle_bg_color", raised), ("active_toggle_fg_color", text_on(raised)),
        ("accent_bg_color", accent), ("accent_fg_color", accent_fg), ("accent_color", standalone),
    ];
    let legacy: Vec<(&str, Rgb)> = vec![
        ("theme_bg_color", background), ("theme_fg_color", foreground),
        ("theme_base_color", view), ("theme_text_color", text_on(view)),
        ("theme_selected_bg_color", accent), ("theme_selected_fg_color", accent_fg),
        ("theme_unfocused_bg_color", background), ("theme_unfocused_fg_color", foreground),
        ("theme_unfocused_base_color", view), ("theme_unfocused_text_color", text_on(view)),
        ("theme_unfocused_selected_bg_color", accent), ("theme_unfocused_selected_fg_color", accent_fg),
        ("insensitive_bg_color", surface), ("insensitive_fg_color", mix(background, foreground, 0.55)),
        ("insensitive_base_color", view), ("borders", line), ("unfocused_borders", line),
        ("content_view_bg", view),
    ];
    let hex = |c: Rgb| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    let mut lines = vec![GTK_GENERATED_HEADER.to_owned(),
                         "/* Colors and title bar controls; application layouts remain unchanged. */".to_owned()];
    for (name, value) in legacy.iter().chain(&colors) {
        lines.push(format!("@define-color {name} {};", hex(*value)));
    }
    lines.push(":root {".into());
    for (name, value) in &colors {
        lines.push(format!("  --{}: {};", name.replace('_', "-"), hex(*value)));
    }
    lines.push("}".into());
    lines.push(window_controls_css("window", text_on(surface), colored_buttons));
    lines.push(window_frame_css("window") + "\n");
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallpaper_hue_reaches_controls_and_published_gtk_scheme() {
        for dark in [false, true] {
            for (wallpaper, dominant) in [([190, 22, 66], 0), ([25, 160, 55], 1), ([30, 65, 190], 2)] {
                let palette = color_palette(dark, Some(wallpaper));
                let accent = palette.accent;
                let others = (0..3).filter(|i| *i != dominant).map(|i| accent[i]).max().unwrap();
                assert!(accent[dominant] > others, "{dark} {wallpaper:?} {accent:?}");
                assert_ne!(accent, color_palette(dark, None).accent);
                assert!(contrast(accent, palette.background) >= 4.5);
                let css = color_css(dark, &palette, false);
                let rgb = format!("#{:02x}{:02x}{:02x}", accent[0], accent[1], accent[2]);
                assert!(css.contains(&format!("@define-color accent_bg_color {rgb};")));
                assert!(css.contains(&format!("--accent-bg-color: {rgb};")));
            }
        }
    }

    #[test]
    fn extreme_and_neutral_wallpapers_stay_readable() {
        for dark in [false, true] {
            for wallpaper in [[0, 0, 0], [255, 255, 255], [128, 128, 128], [255, 0, 255], [255, 255, 0], [0, 255, 255]] {
                let palette = color_palette(dark, Some(wallpaper));
                assert!(contrast(palette.accent, palette.background) >= 4.5);
                assert!(contrast(palette.accent, [0, 0, 0]).max(contrast(palette.accent, [255, 255, 255])) >= 4.5);
                if wallpaper[0] == wallpaper[1] && wallpaper[1] == wallpaper[2] {
                    // Colorless wallpapers fall back to blue, never a disabled-looking grey.
                    let [red, green, blue] = palette.accent;
                    assert!(blue as i32 > red.max(green) as i32 + 30, "{wallpaper:?} {:?}", palette.accent);
                }
            }
        }
    }

    #[test]
    fn accent_is_never_near_grey() {
        for dark in [false, true] {
            // The live wallpaper produced (119, 122, 130); near-neutral and dim samples too.
            for wallpaper in [[20, 26, 40], [60, 60, 64], [119, 122, 130], [109, 110, 112], [200, 190, 180], [5, 5, 5],
                              [250, 250, 250]] {
                let palette = color_palette(dark, Some(wallpaper));
                let a = palette.accent.map(|c| c as f64 / 255.0);
                let (_, _, saturation) = rgb_to_hls(a[0], a[1], a[2]);
                assert!(saturation >= 0.44, "{wallpaper:?}");
                assert!(contrast(palette.accent, palette.background) >= 4.5);
            }
        }
    }

    #[test]
    fn matching_disabled_retains_normal_light_and_dark_accents() {
        assert_eq!(color_palette(true, None).accent, [105, 167, 255]);
        assert_eq!(color_palette(false, None).accent, [36, 112, 210]);
    }

    #[test]
    fn python_reference_values() {
        // Values computed by tessel-dock/dock_style.py and gtk_color_scheme.py.
        assert_eq!(color_palette(true, Some([119, 122, 130])), Palette { background: [42, 43, 46], foreground: [238, 238, 238], accent: [120, 174, 237] });
        assert_eq!(color_palette(false, Some([190, 22, 66])), Palette { background: [239, 209, 219], foreground: [35, 38, 45], accent: [186, 22, 65] });
        assert_eq!(surface_palette(true, None, parse_hex("#336699")).accent, [204, 225, 255]);
        assert_eq!(mix([0, 0, 0], [5, 3, 1], 0.5), [2, 2, 0]);
        let css = background_css(true, &PanelStyle { border_sides: &["top"], shine: true, ..Default::default() }).unwrap();
        assert!(css.contains("box-shadow: inset 0 1px 0 0 rgba(255, 255, 255, 0.16), inset 0px 1px 0 0 rgb(105, 167, 255);"));
        assert!(css.contains("background-color: rgba(0, 0, 0, 0.950);"));
        let light = background_css(false, &PanelStyle { transparency: 50, ..Default::default() }).unwrap();
        assert!(light.contains("rgba(250, 250, 252, 0.850)"));
    }
}
