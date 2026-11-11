//! Dock styling (dock_style.py + the per-clone CSS of dock_ui.set_background).
//!
//! [`CSS`] / [`LIGHT_CSS`] are the Python constants byte for byte (the files
//! in `css/` are generated from dock_style.py and checked by the Python parity
//! test). The palette maths, `background_css` and `popup_css` come from
//! `tessel_gtk::palette`; this module adds Python's argument validation and the
//! combination one dock clone loads ([`dock_css`]).

use tessel_gtk::palette::{self, PanelStyle, Rgb};

/// `dock_style.CSS` (display provider at APPLICATION priority).
pub const CSS: &str = include_str!("css/dock.css");
/// `dock_style.LIGHT_CSS`, appended in light mode.
pub const LIGHT_CSS: &str = include_str!("css/light.css");

/// `appearance_css(is_dark)`.
pub fn appearance_css(is_dark: bool) -> String {
    if is_dark { CSS.to_owned() } else { format!("{CSS}{LIGHT_CSS}") }
}

const SIDES: [&str; 4] = ["top", "bottom", "left", "right"];

fn parse_border_color(color: &str) -> Result<Rgb, String> {
    let valid = color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err("Choose a border color in #RRGGBB format.".into());
    }
    Ok(palette::parse_hex(color).unwrap())
}

fn parse_background(color: Option<&str>) -> Result<Option<Rgb>, String> {
    match color {
        None => Ok(None),
        // Python: int(background_color[1:3], 16) for each channel.
        Some(text) => palette::parse_hex(&text.to_ascii_lowercase())
            .map(Some).ok_or_else(|| format!("invalid literal for int() with base 16: {text:?}")),
    }
}

/// Inputs of one `background_css` call with Python's types.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundRequest {
    pub transparency: i64,
    pub wallpaper: Option<Rgb>,
    pub rounded_corners: bool,
    pub inner_rounded_corners: Option<bool>,
    pub frosted: bool,
    pub frost_strength: i64,
    pub scope: Option<String>,
    pub background_color: Option<String>,
    pub border_sides: Vec<String>,
    pub border_color: Option<String>,
    pub shine: bool,
    pub shadow: bool,
}

impl Default for BackgroundRequest {
    fn default() -> Self {
        Self { transparency: 5, wallpaper: None, rounded_corners: true, inner_rounded_corners: None, frosted: false,
               frost_strength: 50, scope: None, background_color: None, border_sides: Vec::new(), border_color: None,
               shine: false, shadow: false }
    }
}

fn percent(value: i64, message: &str) -> Result<u8, String> {
    u8::try_from(value).ok().filter(|v| *v <= 100).ok_or_else(|| message.to_owned())
}

/// `dock_style.background_css` with Python's validation and error text.
pub fn background_css(is_dark: bool, request: &BackgroundRequest) -> Result<String, String> {
    let transparency = percent(request.transparency, "Transparency must be between 0 and 100 percent.")?;
    let frost_strength = percent(request.frost_strength, "Blur strength must be between 0 and 100 percent")?;
    let background_color = parse_background(request.background_color.as_deref())?;
    if request.border_sides.iter().any(|side| !SIDES.contains(&side.as_str())) {
        return Err("Invalid bar border sides.".into());
    }
    let border_color = request.border_color.as_deref().map(parse_border_color).transpose()?;
    let sides: Vec<&str> = request.border_sides.iter().map(String::as_str).collect();
    palette::background_css(is_dark, &PanelStyle {
        transparency, wallpaper: request.wallpaper, rounded_corners: request.rounded_corners,
        inner_rounded_corners: request.inner_rounded_corners, frosted: request.frosted, frost_strength,
        scope: request.scope.as_deref(), background_color, border_sides: &sides, border_color, shine: request.shine,
        shadow: request.shadow,
    })
}

/// `dock_style.popup_css` with Python's validation and error text.
#[allow(clippy::too_many_arguments)]
pub fn popup_css(scope: &str, is_dark: bool, wallpaper: Option<Rgb>, transparency: i64, frosted: bool,
                 rounded_corners: bool, frost_strength: i64, background_color: Option<&str>) -> Result<String, String> {
    let transparency = percent(transparency, "Transparency must be between 0 and 100 percent.")?;
    let frost_strength = percent(frost_strength, "Blur strength must be between 0 and 100 percent")?;
    let background_color = parse_background(background_color)?;
    palette::popup_css(scope, is_dark, wallpaper, transparency, frosted, rounded_corners, frost_strength, background_color)
}

/// Everything `DockSurface.set_background` styles for one clone.
#[derive(Clone, Debug, PartialEq)]
pub struct DockCssRequest {
    pub is_dark: bool,
    pub transparency: i64,
    pub wallpaper: Option<Rgb>,
    /// The panel outline's corners after maximized-window flattening.
    pub rounded_corners: bool,
    /// The configured corners (workspace buttons, application menu).
    pub requested_rounding: bool,
    pub frosted: bool,
    pub frost_strength: i64,
    /// The clone's unique popup class (`tessel-dock-clone-<n>`).
    pub scope: String,
    pub background_color: Option<String>,
    /// Already empty outside bar mode.
    pub border_sides: Vec<String>,
    pub border_color: Option<String>,
    pub shine: bool,
    pub shadow: bool,
    pub apps_transparency: i64,
    pub apps_frosted: bool,
    pub apps_frost_strength: i64,
    pub apps_rounded_corners: bool,
    /// Also style `.tessel-dock-islands`: the panel turns transparent and
    /// each `.tessel-dock-island` inside it takes the panel's material.
    pub islands: bool,
}

/// The space between two islands of a bar (CSS margin on the center one).
pub const ISLAND_GAP: i32 = 8;

/// The panel's own rule re-targeted at the islands inside it.
fn islands_css(panel_css: &str, scope: &str) -> String {
    let panel = format!(".{scope}.tessel-dock-panel");
    let Some(body) = panel_css.find(&format!("{panel} {{")).and_then(|start| {
        let rest = &panel_css[start + panel.len() + 2..];
        rest.find("\n}").map(|end| rest[..end].trim().to_owned())
    }) else { return String::new() };
    format!("
{panel}.tessel-dock-islands {{ background-image: none; background-color: transparent; box-shadow: none; padding: 0; }}
{panel}.tessel-dock-islands .tessel-dock-island {{ {body}
    padding: 7px;
}}
{panel}.tessel-dock-islands.tessel-dock-compact-bar .tessel-dock-island {{ padding: 2px 6px; }}
{panel}.tessel-dock-islands.tessel-dock-compact-bar.tessel-dock-vertical .tessel-dock-island {{ padding: 6px 2px; }}
{panel}.tessel-dock-islands .tessel-dock-applications.tessel-dock-island {{ margin: 0 {ISLAND_GAP}px; }}
{panel}.tessel-dock-islands.tessel-dock-vertical .tessel-dock-applications.tessel-dock-island {{ margin: {ISLAND_GAP}px 0; }}
")
}

/// Panel CSS (provider at APPLICATION+2), then the popup and application-menu
/// CSS that share a second provider (`popup + menu`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockCss {
    pub panel: String,
    pub popup: String,
    pub menu: String,
}

pub fn dock_css(request: &DockCssRequest) -> Result<DockCss, String> {
    let mut panel = background_css(request.is_dark, &BackgroundRequest {
        transparency: request.transparency, wallpaper: request.wallpaper, rounded_corners: request.rounded_corners,
        inner_rounded_corners: Some(request.requested_rounding), frosted: request.frosted,
        frost_strength: request.frost_strength, scope: Some(request.scope.clone()),
        background_color: request.background_color.clone(), border_sides: request.border_sides.clone(),
        border_color: request.border_color.clone(), shine: request.shine, shadow: request.shadow,
    })?;
    if request.islands {
        panel.push_str(&islands_css(&panel, &request.scope));
    }
    // Application menus inherit the surface that opened them; other app popups
    // keep the independently configured application appearance.
    let popup = popup_css(&request.scope, request.is_dark, request.wallpaper, request.apps_transparency,
                          request.apps_frosted, request.apps_rounded_corners, request.apps_frost_strength, None)?;
    let menu = popup_css(&format!("{}-applications", request.scope), request.is_dark, request.wallpaper,
                         request.transparency, request.frosted, request.requested_rounding, request.frost_strength,
                         request.background_color.as_deref())?;
    Ok(DockCss { panel, popup, menu })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_css_shape() {
        assert!(CSS.starts_with("\nwindow.tessel-dock-surface { background: transparent;"));
        assert!(LIGHT_CSS.starts_with("\nwindow.tessel-dock-surface { color: #23262d; }"));
        assert_eq!(appearance_css(true), CSS);
        assert!(appearance_css(false).ends_with(LIGHT_CSS));
    }

    #[test]
    fn validation_matches_python_messages() {
        let bad = |request: BackgroundRequest| background_css(true, &request).unwrap_err();
        assert_eq!(bad(BackgroundRequest { transparency: 101, ..Default::default() }), "Transparency must be between 0 and 100 percent.");
        assert_eq!(bad(BackgroundRequest { frost_strength: -1, ..Default::default() }), "Blur strength must be between 0 and 100 percent");
        assert_eq!(bad(BackgroundRequest { border_sides: vec!["middle".into()], ..Default::default() }), "Invalid bar border sides.");
        assert_eq!(bad(BackgroundRequest { border_color: Some("red".into()), ..Default::default() }), "Choose a border color in #RRGGBB format.");
        assert!(background_css(true, &BackgroundRequest { border_color: Some("#AbCdEf".into()), border_sides: vec!["top".into()], ..Default::default() })
            .unwrap().contains("rgb(171, 205, 239)"));
    }

    #[test]
    fn clone_css_routes_app_and_menu_appearance() {
        let css = dock_css(&DockCssRequest {
            is_dark: true, transparency: 30, wallpaper: Some([40, 60, 80]), rounded_corners: false, requested_rounding: true,
            frosted: false, frost_strength: 50, scope: "tessel-dock-clone-1".into(), background_color: None,
            border_sides: vec![], border_color: None, shine: false, shadow: false, apps_transparency: 23, apps_frosted: true,
            apps_frost_strength: 50, apps_rounded_corners: false, islands: false }).unwrap();
        assert!(css.panel.contains(".tessel-dock-clone-1.tessel-dock-panel {"));
        assert!(css.panel.contains("border-radius: 0px;"));
        assert!(css.panel.contains("button.tessel-dock-workspace { border-radius: 8px; }"));
        assert!(css.popup.contains(".tessel-dock-clone-1 .tessel-dock-popup *"));
        assert!(css.menu.contains(".tessel-dock-clone-1-applications .tessel-dock-popup, "));
        assert!(!css.menu.contains("border-radius: 0;"));
        assert!(!css.panel.contains("tessel-dock-island"));
    }

    #[test]
    fn islands_take_the_panel_material_and_the_panel_turns_transparent() {
        let css = dock_css(&DockCssRequest {
            is_dark: true, transparency: 30, wallpaper: Some([40, 60, 80]), rounded_corners: true, requested_rounding: true,
            frosted: false, frost_strength: 50, scope: "tessel-dock-clone-1".into(), background_color: None,
            border_sides: vec![], border_color: None, shine: false, shadow: false, apps_transparency: 23, apps_frosted: true,
            apps_frost_strength: 50, apps_rounded_corners: false, islands: true }).unwrap().panel;
        let panel = ".tessel-dock-clone-1.tessel-dock-panel";
        assert!(css.contains(&format!("{panel}.tessel-dock-islands {{ background-image: none; background-color: transparent;")));
        let island = css.split(&format!("{panel}.tessel-dock-islands .tessel-dock-island {{")).nth(1).unwrap();
        let island = &island[..island.find('}').unwrap()];
        let material = css.split(&format!("{panel} {{")).nth(1).unwrap();
        let material = material[..material.find(';').unwrap()].trim();
        assert!(material.starts_with("background-image: none"), "{material}");
        assert!(island.contains("background-color: rgba(") && island.contains("border-radius: 12px;") && island.contains("padding: 7px;"), "{island}");
    }
}
