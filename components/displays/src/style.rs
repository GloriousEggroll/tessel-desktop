//! `display_style.py`: Displays styling and Tessel Dock's live, per-monitor palette.

use gtk::prelude::*;
use tessel_gtk::theme::{self, WindowTheme};

pub const CSS: &str = r#"
window.tessel-displays { background-color: @theme_bg_color; color: @theme_fg_color; }
.tessel-displays headerbar { background: transparent; border: none; box-shadow: none; }
.tessel-displays headerbar windowcontrols button.image-button {
    min-width: 24px; min-height: 24px; padding: 2px; margin: 0; border: none;
    border-radius: 6px; background: transparent;
}
.tessel-displays headerbar windowcontrols button image { color: @theme_fg_color; }
.tessel-displays label { color: inherit; }
.tessel-displays .page-title { font-size: 24px; font-weight: 700; }
.tessel-displays .subtitle { color: alpha(@theme_fg_color, .65); font-size: 13px; }
.tessel-displays button { background-image: none; border-radius: 8px; padding: 7px 12px;
    box-shadow: none; border: 1px solid alpha(@theme_fg_color, .12); }
.tessel-displays button:hover { background-color: alpha(@theme_fg_color, .10); }
.tessel-displays button.primary { background-color: @theme_selected_bg_color; color: @theme_selected_fg_color; }
.tessel-displays entry, .tessel-displays spinbutton { border-radius: 8px; min-height: 30px; box-shadow: none; }
.tessel-displays .settings-card, .tessel-displays .brightness-controls {
    border: 1px solid alpha(@theme_fg_color, .12); border-radius: 8px;
    padding: 16px; background-color: alpha(@theme_fg_color, .035); }
.tessel-displays .monitor-title { font-size: 15px; font-weight: 650; }
.tessel-displays .settings-card .heading { font-weight: 650; }
.tessel-displays .settings-card > .settings-row { min-height: 36px; }
.tessel-displays button.orientation-button { padding: 6px 10px; }
.tessel-displays .hdr-details { border-radius: 8px; padding: 12px 14px;
    background-color: alpha(@theme_fg_color, .04); }
.tessel-displays .hdr-details-title { font-weight: 600; }
.tessel-displays .hdr-value { font-feature-settings: "tnum"; }
.tessel-displays button.orientation-button:checked { background-color: alpha(@theme_selected_bg_color, .35); }
.tessel-displays notebook { background: transparent; border: none; }
.tessel-displays notebook > header { background: transparent; border: none; padding: 4px; }
.tessel-displays notebook > header tab { border: none; border-radius: 8px; padding: 10px 18px; }
.tessel-displays notebook > header tab:checked { background: alpha(@theme_selected_bg_color, .20); }
.tessel-displays notebook > stack { padding: 16px; background: transparent; }
.tessel-displays frame { border-radius: 8px; border: 1px solid alpha(@theme_fg_color, .12); }
.tessel-displays scrollbar { background: transparent; }
.tessel-displays scrollbar slider { min-width: 6px; min-height: 24px; border-radius: 8px;
    background: alpha(@theme_fg_color, .24); border: none; margin: 2px; }
.tessel-displays .monitor-frame { background-color: alpha(@theme_fg_color, .07);
    border: 1px solid @theme_selected_bg_color; border-radius: 8px; }
.tessel-displays .overlay-label { color: @theme_fg_color; }
.tessel-displays .panel-bar { background-color: @theme_selected_bg_color; border-color: @theme_selected_bg_color; }
.tessel-displays .snap-flash { border-color: @theme_selected_bg_color; }
window.tessel-displays-overlay { background: transparent; }
.tessel-displays-overlay .overlay-box { background-color: @theme_bg_color; color: @theme_fg_color;
    border: 1px solid alpha(@theme_fg_color, .14); border-radius: 8px; padding: 18px; }
.tessel-displays-overlay label { color: inherit; }
"#;

const STYLE_KEY: &str = "tessel-displays-style";

/// The display CSS provider `style_window` added (Python `window._display_style`);
/// an embedded window is never realized, so its host removes it explicitly.
pub fn display_style(window: &impl IsA<gtk::Window>) -> Option<gtk::CssProvider> {
    // SAFETY: STYLE_KEY only ever stores a CssProvider.
    unsafe { window.upcast_ref::<gtk::Window>().data::<gtk::CssProvider>(STYLE_KEY).map(|p| p.as_ref().clone()) }
}

/// `style_window(window, overlay=…)`: class, CSS at APPLICATION+10 (removed on
/// unrealize) and the dock palette binding.
pub fn style_window(window: &impl IsA<gtk::Window>, overlay: bool) -> WindowTheme {
    let window = window.upcast_ref::<gtk::Window>();
    window.add_css_class(if overlay { "tessel-displays-overlay" } else { "tessel-displays" });
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    let display = WidgetExt::display(window);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 10);
    // SAFETY: STYLE_KEY only ever stores a CssProvider.
    unsafe { window.set_data(STYLE_KEY, provider.clone()) };
    window.connect_unrealize(move |window| {
        gtk::style_context_remove_provider_for_display(&WidgetExt::display(window), &provider);
    });
    theme::attach(window, None)
}
