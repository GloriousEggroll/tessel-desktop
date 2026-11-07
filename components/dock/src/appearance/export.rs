//! The appearance-publishing part of tessel-dock.py (`export_palette`,
//! `sync_wallpaper_preferences`, the per-dock routing of
//! `refresh_backgrounds`) as pure functions, plus [`Publisher`], which owns
//! the four publishers and applies one export in Python's order:
//! palette.json → gtk.css/tessel-dock-colors.css → labwc title-bar palette +
//! window buttons → Papirus (only once the saved wallpaper colour is known).
//!
//! The app maps its `DockConfig` (W-a) onto [`ExportSettings`] /
//! [`SurfaceSettings`]; nothing here depends on the config type.

use super::gtk_color_scheme::GtkColorScheme;
use super::palette_state::{monitor_entry, AppOptions, PaletteState};
use super::papirus::PapirusPalette;
use super::wallpaper::WallpaperPalette;
use super::window_buttons::WindowButtons;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tessel_gtk::palette::{color_palette, Palette, Rgb};

/// One output: connector name and logical geometry `[x, y, width, height]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub name: String,
    pub geometry: [i64; 4],
}

/// Inputs of `export_palette` (preview-aware where Python is).
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSettings {
    /// `_appearance_preview[0]`, else `config.effective_transparency`.
    pub transparency: f64,
    /// `_appearance_preview[1]`, else `config.match_wallpaper` (dock tint).
    pub match_wallpaper: bool,
    /// The saved `config.match_wallpaper` (global GTK / Papirus colours).
    pub saved_match_wallpaper: bool,
    pub apps_transparency: f64,
    pub apps_transparency_enabled: bool,
    pub apps_frosted: bool,
    pub apps_frost_strength: f64,
    pub apps_rounded_corners: bool,
    pub colored_window_buttons: bool,
    pub window_buttons_left: bool,
    pub window_shadows: bool,
}

/// What one export publishes.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportPlan {
    /// palette.json `monitors` (dock tint; previews apply).
    pub monitors: Vec<Value>,
    /// GTK candidates (`wallpaper_ready` + saved-preference palette).
    pub gtk_monitors: Vec<Value>,
    /// `WallpaperPalette.set_monitor_sizes` argument.
    pub monitor_sizes: BTreeMap<String, (i64, i64)>,
    pub app_options: AppOptions,
}

pub fn export_palette(is_dark: bool, settings: &ExportSettings, monitors: &[MonitorInfo],
                      color_for: &dyn Fn(&str) -> Option<Rgb>) -> ExportPlan {
    let mut plan = ExportPlan { monitors: Vec::new(), gtk_monitors: Vec::new(), monitor_sizes: BTreeMap::new(),
        app_options: AppOptions {
            apps_transparency: settings.apps_transparency, apps_transparency_enabled: settings.apps_transparency_enabled,
            apps_frosted: settings.apps_frosted, apps_frost_strength: settings.apps_frost_strength,
            apps_rounded_corners: settings.apps_rounded_corners, colored_window_buttons: settings.colored_window_buttons,
        } };
    // Companion windows can be moved to outputs without a dock clone.
    for monitor in monitors {
        let color = if settings.match_wallpaper { color_for(&monitor.name) } else { None };
        plan.monitors.push(monitor_entry(&monitor.name, monitor.geometry, &color_palette(is_dark, color)));
        let saved = if settings.saved_match_wallpaper { color_for(&monitor.name) } else { None };
        plan.gtk_monitors.push(gtk_entry(&monitor.name, saved.is_some(), &color_palette(is_dark, saved)));
        plan.monitor_sizes.insert(monitor.name.clone(), (monitor.geometry[2], monitor.geometry[3]));
    }
    plan
}

/// `{"name", "wallpaper_ready", "background", "foreground", "accent"}`.
pub fn gtk_entry(name: &str, wallpaper_ready: bool, palette: &Palette) -> Value {
    json!({"name": name, "wallpaper_ready": wallpaper_ready, "background": palette.background,
           "foreground": palette.foreground, "accent": palette.accent})
}

fn palette_of(entry: &Value) -> Option<Palette> {
    let rgb = |key: &str| -> Option<Rgb> {
        let parts = entry.get(key)?.as_array()?;
        (parts.len() == 3).then(|| std::array::from_fn(|i| parts[i].as_u64().unwrap_or(0) as u8))
    };
    Some(Palette { background: rgb("background")?, foreground: rgb("foreground")?, accent: rgb("accent")? })
}

/// `sync_wallpaper_preferences`: Frosted textures stay off; sampling runs
/// while any surface (or the saved global GTK scheme) needs wallpaper colours.
pub fn wallpaper_enabled(preview_match: Option<bool>, match_wallpaper: bool, uses_app_dock_matching: bool) -> bool {
    preview_match.unwrap_or(match_wallpaper) || match_wallpaper || uses_app_dock_matching
}

/// Apply [`wallpaper_enabled`] and the monitor sizes to a wallpaper controller.
pub fn sync_wallpaper_preferences(wallpaper: &WallpaperPalette, monitors: &[MonitorInfo], preview_match: Option<bool>,
                                  match_wallpaper: bool, separate_app_dock: bool, app_dock_match_wallpaper: bool) {
    let sizes = monitors.iter().map(|m| (m.name.clone(), (m.geometry[2], m.geometry[3]))).collect();
    wallpaper.set_monitor_sizes(&sizes);
    wallpaper.set_frosted_enabled(false);
    // A preview may hide the dock tint while the saved global GTK scheme
    // still needs the sampled wallpaper colours.
    wallpaper.set_enabled(wallpaper_enabled(preview_match, match_wallpaper, separate_app_dock && app_dock_match_wallpaper));
}

/// Appearance fields of one surface config (`DockConfig` or `for_app_dock()`).
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceSettings {
    pub effective_transparency: i64,
    pub match_wallpaper: bool,
    pub background_color: Option<String>,
    pub effective_rounded_corners: bool,
    pub effective_float_mode: bool,
    pub effective_frosted: bool,
    pub frost_strength: i64,
}

/// `_appearance_preview`: `(transparency, match_wallpaper, rounded, float)`.
pub type AppearancePreview = (i64, bool, bool, bool);

/// `_bar_border_preview` or the saved bar border.
#[derive(Clone, Debug, PartialEq)]
pub struct BarBorder {
    pub enabled: bool,
    pub sides: Vec<String>,
    pub color: String,
}

/// Application-popup appearance (always the saved config).
#[derive(Clone, Debug, PartialEq)]
pub struct AppsAppearance {
    pub effective_apps_transparency: i64,
    pub effective_apps_frosted: bool,
    pub apps_frost_strength: i64,
    pub apps_rounded_corners: bool,
}

/// The arguments `refresh_backgrounds` passes to `DockSurface.set_background`
/// and `set_float_mode` for one clone.
#[derive(Clone, Debug, PartialEq)]
pub struct DockBackground {
    pub is_dark: bool,
    pub transparency: i64,
    pub wallpaper: Option<Rgb>,
    pub border_sides: Vec<String>,
    pub border_color: Option<String>,
    pub background_color: Option<String>,
    pub rounded_corners: bool,
    pub frosted: bool,
    pub frost_strength: i64,
    pub apps: AppsAppearance,
    pub float_mode: bool,
}

/// Route one dock clone's appearance (main config with previews, or the
/// separate application dock's own saved settings).
#[allow(clippy::too_many_arguments)]
pub fn dock_background(is_dark: bool, surface: &SurfaceSettings, separate: bool, preview: Option<AppearancePreview>,
                       frosted_preview: Option<bool>, strength_preview: Option<i64>, border: &BarBorder,
                       apps: &AppsAppearance, wallpaper: Option<Rgb>) -> DockBackground {
    let (transparency, match_wallpaper, rounded, float_mode) = preview.unwrap_or((
        surface.effective_transparency, surface.match_wallpaper, surface.effective_rounded_corners, surface.effective_float_mode));
    let matched = if separate { surface.match_wallpaper } else { match_wallpaper };
    DockBackground {
        is_dark,
        transparency: if separate { surface.effective_transparency } else { transparency },
        wallpaper: if matched { wallpaper } else { None },
        border_sides: if border.enabled { border.sides.clone() } else { Vec::new() },
        border_color: if matched { None } else { Some(border.color.clone()) },
        background_color: if matched { None } else { surface.background_color.clone() },
        rounded_corners: if separate { surface.effective_rounded_corners } else { rounded },
        frosted: if separate { surface.effective_frosted } else { frosted_preview.unwrap_or(surface.effective_frosted) },
        frost_strength: if separate { surface.frost_strength } else { strength_preview.unwrap_or(surface.frost_strength) },
        apps: apps.clone(),
        float_mode: if separate { surface.effective_float_mode } else { float_mode },
    }
}

/// Owns the publishers tessel-dock.py creates in `__init__`.
pub struct Publisher {
    pub palette_state: PaletteState,
    pub gtk_color_scheme: GtkColorScheme,
    pub window_buttons: WindowButtons,
    pub papirus_palette: PapirusPalette,
}

impl Publisher {
    /// The real publishers; `folder_icons_changed` is Papirus' `on_changed`
    /// (call `AppearanceController::refresh_icons` from it).
    pub fn new(folder_icons_changed: Option<Box<dyn Fn()>>) -> Self {
        Self {
            palette_state: PaletteState::new(None),
            gtk_color_scheme: GtkColorScheme::new(None),
            window_buttons: WindowButtons::new(Default::default()),
            papirus_palette: PapirusPalette::new(folder_icons_changed, Default::default()),
        }
    }

    /// `export_palette()`; returns the GTK palette entry that was used.
    pub fn export(&mut self, is_dark: bool, settings: &ExportSettings, monitors: &[MonitorInfo],
                  wallpaper: Option<&WallpaperPalette>) -> Option<Value> {
        let color_for = |name: &str| wallpaper.and_then(|w| w.color_for(name));
        let plan = export_palette(is_dark, settings, monitors, &color_for);
        if let Some(wallpaper) = wallpaper {
            wallpaper.set_monitor_sizes(&plan.monitor_sizes);
        }
        self.palette_state.publish(is_dark, settings.transparency, &plan.monitors, &plan.app_options);
        let saved = self.gtk_color_scheme.select_palette(&plan.gtk_monitors).cloned();
        self.gtk_color_scheme.publish(settings.saved_match_wallpaper, is_dark, saved.as_ref(), settings.colored_window_buttons);
        // labwc title bars use the same palette as GTK header bars.
        self.window_buttons.set_palette(is_dark, saved.as_ref().and_then(palette_of).as_ref());
        self.window_buttons.apply(settings.window_buttons_left, settings.colored_window_buttons, settings.window_shadows,
                                 settings.apps_rounded_corners);
        // Folder icons are global, so follow the same primary display as GTK;
        // wait for the real wallpaper rather than briefly applying default blue.
        let ready = saved.as_ref().and_then(|s| s.get("wallpaper_ready")).and_then(Value::as_bool).unwrap_or(false);
        if !settings.saved_match_wallpaper || ready {
            self.papirus_palette.publish(settings.saved_match_wallpaper, saved.as_ref());
        }
        saved
    }

    pub fn close(&self) {
        self.window_buttons.close();
        self.papirus_palette.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitors() -> Vec<MonitorInfo> {
        vec![MonitorInfo { name: "DP-1".into(), geometry: [0, 0, 1920, 1080] },
             MonitorInfo { name: "HDMI-1".into(), geometry: [1920, 0, 2560, 1440] }]
    }

    fn settings() -> ExportSettings {
        ExportSettings { transparency: 65.0, match_wallpaper: false, saved_match_wallpaper: true, apps_transparency: 23.0,
                         apps_transparency_enabled: true, apps_frosted: true, apps_frost_strength: 50.0,
                         apps_rounded_corners: false, colored_window_buttons: false, window_buttons_left: false, window_shadows: true }
    }

    #[test]
    fn frost_does_not_watch_wallpaper_images_when_color_matching_is_off() {
        assert!(!wallpaper_enabled(None, false, false));
        assert!(wallpaper_enabled(Some(false), true, false), "a preview keeps the saved GTK colours sampled");
        assert!(wallpaper_enabled(Some(true), false, false));
        assert!(wallpaper_enabled(None, false, true));
    }

    #[test]
    fn color_preview_does_not_disable_frost_or_round_the_dock() {
        let colors = |name: &str| Some(if name == "DP-1" { [40, 60, 80] } else { [70, 30, 90] });
        let plan = export_palette(true, &settings(), &monitors(), &colors);
        assert!(plan.monitors.iter().all(|m| m.get("frost_texture").is_none()));
        assert_eq!(plan.monitors[0]["background"], json!(color_palette(true, None).background));
        assert_eq!(plan.gtk_monitors[0]["wallpaper_ready"], true);
        assert_eq!(plan.gtk_monitors[0]["background"], json!(color_palette(true, Some([40, 60, 80])).background));
        assert!(plan.app_options.apps_frosted);
        assert!(!plan.app_options.apps_rounded_corners);
        assert_eq!(plan.monitor_sizes["HDMI-1"], (2560, 1440));
        let surface = SurfaceSettings { effective_transparency: 70, match_wallpaper: true, background_color: None,
                                        effective_rounded_corners: true, effective_float_mode: true, effective_frosted: false,
                                        frost_strength: 50 };
        let apps = AppsAppearance { effective_apps_transparency: 23, effective_apps_frosted: true, apps_frost_strength: 50,
                                    apps_rounded_corners: false };
        let border = BarBorder { enabled: false, sides: vec!["top".into()], color: "#777777".into() };
        let background = dock_background(true, &surface, false, Some((65, false, true, false)), None, None, &border, &apps, Some([40, 60, 80]));
        assert_eq!(background, DockBackground {
            is_dark: true, transparency: 65, wallpaper: None, border_sides: vec![], border_color: Some("#777777".into()),
            background_color: None, rounded_corners: true, frosted: false, frost_strength: 50, apps, float_mode: false });
    }

    #[test]
    fn bar_border_preview_tracks_wallpaper_and_restores_saved_color() {
        let surface = SurfaceSettings { effective_transparency: 5, match_wallpaper: false, background_color: None,
                                        effective_rounded_corners: false, effective_float_mode: false, effective_frosted: false,
                                        frost_strength: 50 };
        let apps = AppsAppearance { effective_apps_transparency: 0, effective_apps_frosted: false, apps_frost_strength: 50,
                                    apps_rounded_corners: true };
        let preview = BarBorder { enabled: true, sides: vec!["bottom".into()], color: "#112233".into() };
        let matched = dock_background(true, &surface, false, Some((0, true, false, false)), None, None, &preview, &apps, None);
        assert_eq!(matched.border_sides, ["bottom"]);
        assert_eq!(matched.border_color, None, "Matching uses the current palette rather than a saved color");
        let plain = dock_background(true, &surface, false, Some((0, false, false, false)), None, None, &preview, &apps, None);
        assert_eq!(plain.border_color.as_deref(), Some("#112233"));
        let saved = BarBorder { enabled: false, sides: vec!["top".into()], color: "#aabbcc".into() };
        let restored = dock_background(true, &surface, false, None, None, None, &saved, &apps, None);
        assert!(restored.border_sides.is_empty());
        assert_eq!(restored.border_color.as_deref(), Some("#aabbcc"));
    }

    #[test]
    fn strength_preview_routes_only_to_dock_and_restores() {
        let surface = SurfaceSettings { effective_transparency: 0, match_wallpaper: false, background_color: None,
                                        effective_rounded_corners: true, effective_float_mode: true, effective_frosted: true,
                                        frost_strength: 67 };
        let apps = AppsAppearance { effective_apps_transparency: 40, effective_apps_frosted: true, apps_frost_strength: 29,
                                    apps_rounded_corners: true };
        let border = BarBorder { enabled: false, sides: vec![], color: "#777777".into() };
        let preview = dock_background(true, &surface, false, Some((0, false, true, true)), Some(true), Some(90), &border, &apps, None);
        assert_eq!((preview.transparency, preview.frosted, preview.frost_strength), (0, true, 90));
        assert_eq!((preview.apps.apps_frost_strength, preview.apps.effective_apps_transparency), (29, 40));
        let restored = dock_background(true, &surface, false, None, None, None, &border, &apps, None);
        assert_eq!(restored.frost_strength, 67);
        let separate = dock_background(true, &SurfaceSettings { frost_strength: 12, ..surface }, true, Some((80, true, false, false)),
                                       Some(false), Some(90), &border, &apps, Some([1, 2, 3]));
        assert_eq!((separate.transparency, separate.frost_strength, separate.wallpaper), (0, 12, None));
    }

    #[test]
    fn preview_without_frost_preserves_saved_global_gtk_colors() {
        let colors = |name: &str| Some(if name == "DP-1" { [40, 60, 80] } else { [70, 30, 90] });
        let saved = export_palette(true, &ExportSettings { match_wallpaper: true, ..settings() }, &monitors(), &colors);
        let preview = export_palette(true, &ExportSettings { match_wallpaper: false, transparency: 30.0, ..settings() }, &monitors(), &colors);
        assert_eq!(saved.gtk_monitors, preview.gtk_monitors);
        assert_ne!(saved.monitors, preview.monitors);
    }
}
