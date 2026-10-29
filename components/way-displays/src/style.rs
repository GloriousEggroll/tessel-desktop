//! `display_style.py`: Displays styling and Waydock's live, per-monitor palette.

use gtk::prelude::*;
use suite_gtk::theme::{self, WindowTheme};

pub const CSS: &str = r#"
window.way-displays { background-color: @theme_bg_color; color: @theme_fg_color; }
.way-displays headerbar { background: transparent; border: none; box-shadow: none; }
.way-displays headerbar windowcontrols button.image-button {
    min-width: 24px; min-height: 24px; padding: 2px; margin: 0; border: none;
    border-radius: 6px; background: transparent;
}
.way-displays headerbar windowcontrols button image { color: @theme_fg_color; }
.way-displays label { color: inherit; }
.way-displays .page-title { font-size: 24px; font-weight: 700; }
.way-displays .subtitle { color: alpha(@theme_fg_color, .65); font-size: 13px; }
.way-displays button { background-image: none; border-radius: 8px; padding: 7px 12px;
    box-shadow: none; border: 1px solid alpha(@theme_fg_color, .12); }
.way-displays button:hover { background-color: alpha(@theme_fg_color, .10); }
.way-displays button.primary { background-color: @theme_selected_bg_color; color: @theme_selected_fg_color; }
.way-displays entry, .way-displays spinbutton { border-radius: 8px; min-height: 30px; box-shadow: none; }
.way-displays .settings-card, .way-displays .brightness-controls {
    border: 1px solid alpha(@theme_fg_color, .12); border-radius: 12px;
    padding: 16px; background-color: alpha(@theme_fg_color, .035); }
.way-displays .monitor-title { font-size: 15px; font-weight: 650; }
.way-displays .settings-card .heading { font-weight: 650; }
.way-displays .settings-card > .settings-row { min-height: 36px; }
.way-displays button.orientation-button { padding: 6px 10px; }
.way-displays .hdr-details { border-radius: 10px; padding: 12px 14px;
    background-color: alpha(@theme_fg_color, .04); }
.way-displays .hdr-details-title { font-weight: 600; }
.way-displays .hdr-value { font-feature-settings: "tnum"; }
.way-displays button.orientation-button:checked { background-color: alpha(@theme_selected_bg_color, .35); }
.way-displays notebook { background: transparent; border: none; }
.way-displays notebook > header { background: transparent; border: none; padding: 4px; }
.way-displays notebook > header tab { border: none; border-radius: 8px; padding: 10px 18px; }
.way-displays notebook > header tab:checked { background: alpha(@theme_selected_bg_color, .20); }
.way-displays notebook > stack { padding: 16px; background: transparent; }
.way-displays frame { border-radius: 12px; border: 1px solid alpha(@theme_fg_color, .12); }
.way-displays scrollbar { background: transparent; }
.way-displays scrollbar slider { min-width: 6px; min-height: 24px; border-radius: 8px;
    background: alpha(@theme_fg_color, .24); border: none; margin: 2px; }
.way-displays .monitor-frame { background-color: alpha(@theme_fg_color, .07);
    border: 1px solid @theme_selected_bg_color; border-radius: 10px; }
.way-displays .overlay-label { color: @theme_fg_color; }
.way-displays .panel-bar { background-color: @theme_selected_bg_color; border-color: @theme_selected_bg_color; }
.way-displays .snap-flash { border-color: @theme_selected_bg_color; }
window.way-displays-overlay { background: transparent; }
.way-displays-overlay .overlay-box { background-color: @theme_bg_color; color: @theme_fg_color;
    border: 1px solid alpha(@theme_fg_color, .14); border-radius: 16px; padding: 18px; }
.way-displays-overlay label { color: inherit; }
"#;

const STYLE_KEY: &str = "way-displays-style";

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
    window.add_css_class(if overlay { "way-displays-overlay" } else { "way-displays" });
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
