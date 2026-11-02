//! Pure palette → CSS functions of wayshell_theme.py (text byte-for-byte).

use std::fmt;

/// Python `ValueError` raised by `frost_background` / `palette_css`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError(pub &'static str);

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ValueError {}

const TRANSPARENCY_ERROR: &str = "Transparency must be between 0 and 100 percent";
const STRENGTH_ERROR: &str = "Blur strength must be between 0 and 100 percent";

/// The three palette colours of one monitor entry (ints or floats, 0..=255).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colors {
    pub background: [f64; 3],
    pub foreground: [f64; 3],
    pub accent: [f64; 3],
}

/// Variant flags of `palette_css` (Python keyword arguments and defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CssOptions {
    pub desktop: bool,
    pub switcher: bool,
    pub backdrop: bool,
    pub frosted: bool,
    pub rounded_corners: bool,
    pub frost_strength: i64,
}

impl Default for CssOptions {
    fn default() -> Self {
        Self { desktop: false, switcher: false, backdrop: false, frosted: false, rounded_corners: true, frost_strength: 50 }
    }
}

/// Python `"%d" % x` (int() truncation toward zero for floats).
fn pct_d(value: f64) -> String {
    format!("{}", value.trunc() as i64)
}

/// Python `"%.3f" % x`.
fn pct_3f(value: f64) -> String {
    format!("{value:.3}")
}

/// Python `round(x)` (half to even) as an int.
fn py_round(value: f64) -> i64 {
    value.round_ties_even() as i64
}

/// `rgba(rgb, alpha)` helper inside `palette_css` (channels rounded first).
fn rgba(rgb: [f64; 3], alpha: f64) -> String {
    format!("rgba({},{},{},{})", py_round(rgb[0]), py_round(rgb[1]), py_round(rgb[2]), pct_3f(alpha))
}

/// `frost_texture_path(texture)`: legacy wallpaper textures are ignored.
pub fn frost_texture_path<T>(_texture: Option<T>) -> Option<String> {
    None
}

/// `frost_background(background, transparency, texture=None, *, frost_strength=50)`.
pub fn frost_background(background: [f64; 3], transparency: i64, _texture: Option<&str>, frost_strength: i64)
                        -> Result<String, ValueError> {
    if !(0..=100).contains(&transparency) {
        return Err(ValueError(TRANSPARENCY_ERROR));
    }
    if !(0..=100).contains(&frost_strength) {
        return Err(ValueError(STRENGTH_ERROR));
    }
    let alpha = (100 - transparency) as f64 / 100.0;
    let color = format!("rgba({},{},{},{})", pct_d(background[0]), pct_d(background[1]), pct_d(background[2]), pct_3f(alpha));
    Ok(format!("background-image: none; background-color: {color};"))
}

/// CPython 3.12+ `sum()` of floats: Neumaier compensated summation (the int
/// start value 0 is folded into the first term).
pub(crate) fn py_sum(values: &[f64]) -> f64 {
    let mut iter = values.iter().copied();
    let Some(first) = iter.next() else { return 0.0 };
    let mut total = 0.0 + first;
    let mut compensation = 0.0;
    for x in iter {
        let t = total + x;
        if total.abs() >= x.abs() {
            compensation += (total - t) + x;
        } else {
            compensation += (x - t) + total;
        }
        total = t;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    total
}

/// `_on_accent(accent)`: dark text on light accents (sRGB luminance > 0.179).
pub fn on_accent(accent: [f64; 3]) -> &'static str {
    let linear: Vec<f64> = accent.iter().map(|part| {
        let value = part / 255.0;
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    }).collect();
    let weighted = [linear[0] * 0.2126, linear[1] * 0.7152, linear[2] * 0.0722];
    if py_sum(&weighted) > 0.179 { "#101114" } else { "#ffffff" }
}

/// `palette_css(css_class, colors, transparency=0, *, desktop, switcher, backdrop,
/// frosted, rounded_corners, frost_strength)` — returns the UTF-8 text.
pub fn palette_css(css_class: &str, colors: &Colors, transparency: i64, options: &CssOptions) -> Result<String, ValueError> {
    if !(0..=100).contains(&transparency) {
        return Err(ValueError(TRANSPARENCY_ERROR));
    }
    if !(0..=100).contains(&options.frost_strength) {
        return Err(ValueError(STRENGTH_ERROR));
    }
    let rounded_corners = options.rounded_corners;
    let radius = if rounded_corners { 12 } else { 0 };
    let root = format!(".{css_class}");
    if options.backdrop {
        return Ok(format!("window{root}.tessel-shell-switcher-backdrop {{ background-image: none; \
background-color: transparent; border: none; box-shadow: none; }}"));
    }
    let (background, foreground, accent) = (colors.background, colors.foreground, colors.accent);
    let alpha = (100 - transparency) as f64 / 100.0;
    let bg = rgba(background, alpha);
    let menu_bg = bg.clone();
    let material = if options.frosted {
        frost_background(background, transparency, None, options.frost_strength)?
    } else {
        format!("background-image: none; background-color: {bg};")
    };
    let fg = rgba(foreground, 1.0);
    let muted = rgba(foreground, 0.65);
    let line = rgba(foreground, 0.13);
    let highlight = rgba(accent, 1.0);
    let on_accent = on_accent(accent);
    let mix = |weight_base: f64, weight_top: f64| -> [f64; 3] {
        [0, 1, 2].map(|i| background[i] * weight_base + foreground[i] * weight_top)
    };
    let surface_rgb = mix(0.94, 0.06).map(|value| py_round(value) as f64);
    let surface = rgba(surface_rgb, alpha);
    let hover = rgba(mix(0.90, 0.10), alpha);
    let selected = rgba(accent, 0.22 * alpha);
    let mut css = format!("
{root} popover {{ background-image: none; background-color: transparent; box-shadow: none; }}
{root} popover > contents {{ {material} color: {fg};
    border: 1px solid {line}; border-radius: {radius}px; box-shadow: none; }}
{root} popover > arrow {{ background-image: none; background-color: {menu_bg}; border-color: {line}; }}
{root} popover modelbutton {{ background-image: none; background-color: transparent;
    border-color: transparent; color: {fg}; box-shadow: none; text-shadow: none; }}
{root} popover label, {root} popover image {{ color: inherit; }}
{root} popover modelbutton:hover, {root} popover modelbutton:selected,
{root} popover modelbutton:focus-visible {{ background-image: none;
    background-color: {selected}; border-color: {highlight}; outline-color: {highlight}; color: {fg}; }}
{root} popover modelbutton:disabled {{ color: {muted}; }}
{root} popover modelbutton arrow {{ background-image: none; background-color: transparent;
    border-color: transparent; color: inherit; }}
{root} popover separator {{ background-color: {line}; }}
");
    let controls;
    if options.desktop {
        controls = format!("{root} popover");
        css += &format!("
{root} entry {{ background-image: none; background-color: {surface}; color: {fg};
    border-color: {line}; box-shadow: none; }}
{root} .selection-box {{ background-color: {selected}; border-color: {highlight}; }}
{root} .label-highlight {{ color: #ffffff; }}
{root} .label-highlight label {{ color: inherit; text-shadow: 0 1px 2px rgba(0,0,0,.6); }}
{root} .label-highlight.selected {{ background-color: {highlight}; color: {on_accent}; }}
{root} .label-highlight.selected label {{ text-shadow: none; }}
");
    } else if options.switcher {
        controls = root.clone();
        let activities = rgba(background, 0.40);
        let slider = rgba(foreground, 0.3);
        css += &format!("
window{root}.tessel-shell-switcher {{ background-image: none; background-color: transparent;
    border: none; box-shadow: none; color: {fg}; }}
{root} .switcher-surface {{ {material} border-radius: {radius}px;
    color: {fg}; border-color: {line}; }}
{root} .switcher-count, {root} .switcher-hint, {root} .switcher-title {{ color: {muted}; }}
{root} .switcher-grid, {root} scrolledwindow {{ background-color: transparent; }}
{root}.activities .switcher-surface {{ background-image: none; background-color: {activities};
    border: none; border-radius: 0; }}
{root} scrolledwindow scrollbar slider {{ background-color: {slider}; }}
");
    } else {
        controls = root.clone();
        let button_radius = if rounded_corners { 8 } else { 0 };
        let slider = rgba(foreground, 0.3);
        css += &format!("
window{root} {{ {material} border-radius: {radius}px; color: {fg}; }}
{root} headerbar {{ background-image: none; background-color: transparent; color: {fg};
    border-color: {line}; box-shadow: none; }}
{root} headerbar windowcontrols button.image-button {{
    min-width: 24px; min-height: 24px; padding: 2px; margin: 0;
    border: none; border-radius: {button_radius}px; background: transparent; color: {fg}; }}
{root} headerbar windowcontrols button.image-button:hover {{ background-color: {hover}; }}
{root} headerbar windowcontrols button image {{ color: {fg}; }}
{root} .wallpaper-card {{ background-color: {surface}; border-color: {line}; }}
{root} .wallpaper-subtitle, {root} .wallpaper-detail {{ color: {muted}; }}
{root} .wallpaper-tile {{ background-color: {selected}; color: {highlight}; }}
{root} .wallpaper-display-icon {{ color: {highlight}; }}
{root} separator {{ background-color: {line}; }}
{root} listview, {root} gridview, {root} viewport, {root} stack, {root} scrolledwindow {{ background-color: transparent; color: {fg}; }}
{root} gridview child:hover {{ background-color: {hover}; color: {fg}; }}
{root} row:selected, {root} gridview child:selected {{ background-color: {selected}; color: {fg}; }}
{root} scrolledwindow scrollbar slider {{ background-color: {slider}; }}
");
    }
    let checked = rgba(accent, alpha);
    css += &format!("
{controls} button, {controls} entry, {controls} spinbutton {{ background-image: none;
    background-color: {surface}; color: {fg}; border-color: {line}; box-shadow: none; text-shadow: none; }}
{controls} button:hover {{ background-image: none; background-color: {hover}; }}
{controls} check, {controls} radio {{ background-image: none; background-color: {surface};
    color: {fg}; border-color: {line}; box-shadow: none; }}
{controls} button.suggested-action, {controls} button:checked, {controls} switch:checked,
{controls} check:checked, {controls} radio:checked {{ background-image: none;
    background-color: {checked}; color: {on_accent}; border-color: transparent; }}
{controls} button.suggested-action label, {controls} button.suggested-action image,
{controls} button:checked label, {controls} button:checked image {{ color: {on_accent}; }}
{controls} switch:checked slider {{ background-color: {on_accent}; }}
{controls} button:focus-visible, {root} entry:focus, {root} entry:focus-within,
{controls} spinbutton:focus-within, {controls} check:focus, {controls} radio:focus {{
    outline-color: {highlight}; border-color: {highlight}; }}
{root} entry, {controls} spinbutton, {controls} textview text {{ caret-color: {highlight}; }}
{root} selection {{ background-color: {highlight}; color: {on_accent}; }}
{controls} scale highlight, {controls} progressbar progress, {controls} levelbar block.filled {{
    background-image: none; background-color: {highlight}; border-color: {highlight}; }}
{controls} notebook tab:checked {{ background-color: {selected}; color: {fg};
    border-color: {highlight}; box-shadow: inset 0 -2px {highlight}; }}
{controls} notebook header.bottom tab:checked {{ box-shadow: inset 0 2px {highlight}; }}
{controls} notebook header.left tab:checked {{ box-shadow: inset -2px 0 {highlight}; }}
{controls} notebook header.right tab:checked {{ box-shadow: inset 2px 0 {highlight}; }}
");
    if options.switcher {
        css += &format!("
{root} button.switcher-card {{ background-color: {surface}; border-color: transparent; color: {fg}; }}
{root} button.switcher-card:hover {{ background-color: {hover}; }}
{root} button.switcher-card.selected {{ background-color: {selected}; border-color: {highlight}; }}
");
    }
    if !rounded_corners {
        // This provider already outranks local app/control styles. Clear all
        // descendant radii too, including selection tiles and popup controls.
        css += &format!("\n{root}, {root} * {{ border-radius: 0; }}\n");
        let rounded_controls = ["switch", "switch slider", "scale trough", "scale highlight",
                                "scale fill", "scale slider", "scrollbar trough", "scrollbar slider"]
            .iter().map(|selector| format!("{root} {selector}")).collect::<Vec<_>>().join(", ");
        css += &format!("{rounded_controls} {{ border-radius: 9999px; }}\n");
    }
    Ok(css)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_sum_compensates() {
        // 0.1 + 0.2 + 0.3 differs between naive and compensated summation.
        assert_eq!(py_sum(&[0.1, 0.2, 0.3]), 0.6);
        assert_eq!(py_sum(&[1e100, 1.0, -1e100]), 1.0);
    }

    #[test]
    fn percent_formats_match_python() {
        assert_eq!(pct_d(18.9), "18");
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(3.5), 4);
        assert_eq!(pct_3f(0.2095), "0.209");
        assert_eq!(pct_3f(0.0005), "0.001");
    }
}
