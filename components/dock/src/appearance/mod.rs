//! W-d appearance publishers (port of tessel-dock palette_state.py,
//! gtk_color_scheme.py, window_buttons.py, labwc_titlebar_theme.py,
//! papirus_palette.py, dock_appearance.py, wallpaper_palette.py,
//! frosted_wallpaper.py, dock_style.py and the palette export of tessel-dock.py).
//!
//! Public API the application calls (every path / command / bus is injectable;
//! `None`/`Default` options mean the real locations and services):
//! - [`palette_state::PaletteState`] — `new(None)` (= `~/.cache/tessel-dock/palette.json`),
//!   `publish(is_dark, transparency, &monitors, &AppOptions) -> bool`, `error`.
//!   Entries come from [`palette_state::monitor_entry`].
//! - [`gtk_color_scheme::GtkColorScheme`] — `new(None)` (= `$XDG_CONFIG_HOME`),
//!   `select_palette(&[Value]) -> Option<&Value>` (way-displays primary output
//!   block of `labwc/environment`), `publish(enabled, is_dark, Option<&Value>,
//!   colored) -> bool` (`publish(false, ..)` = disable), `error`.
//! - [`window_buttons::WindowButtons`] — `new(WindowButtonsOptions)`,
//!   `set_palette(is_dark, Option<&Palette>)` (600 ms debounce),
//!   `apply(left, colored, shadows) -> bool`, `close()`, `error()`;
//!   [`window_buttons::ReconfigureRequest`] runs `labwc --reconfigure` off the
//!   main loop, coalesced, only when `LABWC_PID` is set.
//! - [`labwc_theme`] — `titlebar_colors`, `themerc_values`, `button_files`,
//!   `updated_themerc`, `theme_name`, `theme_dir`, `current_palette`.
//! - [`rc_xml`] — byte-preserving `<theme>` edits of labwc's rc.xml.
//! - [`papirus::PapirusPalette`] — `new(on_changed, PapirusOptions)`,
//!   `publish(enabled, Option<&Value palette>) -> bool`, `close()`, `error()`.
//! - [`controller::AppearanceController`] — `new(on_changed, ControllerOptions)`,
//!   `start()`, `is_dark()`, `icon_theme_name()`, `refresh_icons()`, `close()`;
//!   portal color-scheme (ReadAll + SettingChanged) with GTK fallback.
//! - [`wallpaper::WallpaperPalette`] — `new(on_changed, WallpaperOptions)`,
//!   `start()`, `set_enabled()`, `set_monitor_sizes()`, `set_frosted_enabled()`,
//!   `refresh()`, `color_for(monitor)`, `frost_for(monitor)`, `close()`.
//! - [`frosted::FrostedWallpaperCache`] — legacy Frosted textures (the dock
//!   keeps them disabled; ported for parity).
//! - [`style`] — `CSS`/`LIGHT_CSS` (byte-identical to dock_style.py),
//!   `appearance_css(is_dark)`, `background_css`/`popup_css` with Python
//!   validation, `dock_css(&DockCssRequest) -> DockCss` (one clone's panel,
//!   popup and application-menu CSS, as dock_ui.set_background loads them).
//! - [`export`] — `export_palette(..) -> ExportPlan`, `wallpaper_enabled`,
//!   `sync_wallpaper_preferences`, `dock_background` (refresh_backgrounds
//!   routing) and [`export::Publisher`] (owns the four publishers and applies
//!   one export in Python's order).

pub mod controller;
pub mod export;
pub mod frosted;
pub mod fsx;
pub mod gtk_color_scheme;
pub mod labwc_theme;
pub mod palette_state;
pub mod papirus;
pub mod rc_xml;
pub mod style;
pub mod wallpaper;
pub mod window_buttons;

pub use controller::AppearanceController;
pub use export::Publisher;
pub use gtk_color_scheme::GtkColorScheme;
pub use palette_state::PaletteState;
pub use papirus::PapirusPalette;
pub use wallpaper::WallpaperPalette;
pub use window_buttons::WindowButtons;
