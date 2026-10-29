//! Shared styling for the Power/Battery popups and settings windows (`power_style.py`).

use gtk::prelude::*;
use gtk::{gio, pango};

pub const STYLE_CSS: &str = r#"
@define-color power_accent #3584e4;
@define-color power_surface mix(@theme_base_color, @theme_bg_color, 0.20);
@define-color power_muted alpha(@theme_fg_color, 0.64);
@define-color power_line alpha(@theme_fg_color, 0.10);
@define-color power_error mix(@theme_fg_color, #e05d65, 0.75);
window.powermanager { background-color: @theme_bg_color; }
.powermanager headerbar {
    background-image: none; background-color: @theme_bg_color;
    border: none; box-shadow: none; padding: 8px 14px;
}
.powermanager .title { font-size: 23pt; font-weight: 700; }
.powermanager headerbar .title { font-size: 11pt; font-weight: 600; }
.powermanager .section-title { font-size: 11pt; font-weight: 600; }
.powermanager .row-title { font-size: 10pt; font-weight: 600; }
.powermanager .muted, .powermanager .subtitle { color: @power_muted; }
.powermanager .subtitle { font-size: 10pt; }
.powermanager .status { color: @power_muted; font-size: 9pt; }
.powermanager .error { color: @power_error; }
.powermanager .card {
    background-color: @power_surface; border: 1px solid @power_line;
    border-radius: 14px;
}
.powermanager .power-tile {
    min-width: 42px; min-height: 42px; border-radius: 12px;
    color: @power_accent; background-color: alpha(@power_accent, 0.12);
}
.powermanager .profile-check { color: @power_accent; }
.powermanager .battery-row {
    background-color: alpha(@theme_fg_color, 0.025); border-radius: 10px;
}
.powermanager .battery-percentage { font-size: 11pt; font-weight: 600; }
.powermanager levelbar.battery-level {
    min-width: 60px; padding: 0; margin: 0;
    background-image: none; box-shadow: none; border: none;
}
.powermanager levelbar.battery-level trough {
    min-height: 6px; border: none; border-radius: 3px;
    padding: 0; margin: 0; background-image: none; box-shadow: none;
    background-color: alpha(@theme_fg_color, 0.09);
}
.powermanager levelbar.battery-level block {
    min-height: 6px; min-width: 0; border: none; border-radius: 3px;
    padding: 0; margin: 0; background-image: none; box-shadow: none;
}
.powermanager levelbar.battery-level block.filled { background-color: @power_accent; }
.powermanager levelbar.battery-level block.empty { background-color: transparent; }
.powermanager switch:checked { background-color: @power_accent; }
.powermanager switch:disabled { opacity: 0.45; }
.powermanager button:not(.titlebutton) {
    background-image: none; background-color: @power_surface;
    border: 1px solid @power_line; border-radius: 8px;
    color: @theme_fg_color; box-shadow: none; text-shadow: none;
    min-height: 27px; padding: 5px 12px;
}
.powermanager button:hover { background-color: mix(@power_surface, @theme_fg_color, 0.06); }
.powermanager button:disabled { opacity: 0.45; }
.powermanager button:focus { outline: 2px solid alpha(@power_accent, 0.6); outline-offset: -2px; }
.powermanager button.primary {
    background-color: @power_accent; color: white; border-color: transparent;
}
.powermanager button.icon-button {
    background-color: transparent; border-color: transparent;
    padding: 5px; min-width: 25px;
}
.powermanager button.icon-button:hover { background-color: alpha(@theme_fg_color, 0.07); }
.powermanager button.profile-row {
    background-color: transparent; border-color: transparent; border-radius: 10px;
    padding: 10px 12px;
}
.powermanager button.profile-row:hover { background-color: alpha(@theme_fg_color, 0.045); }
.powermanager button.profile-row.selected {
    background-color: alpha(@power_accent, 0.12); border-color: alpha(@power_accent, 0.22);
}
.powermanager combobox button { min-height: 27px; }
.powermanager separator { background-color: @power_line; min-height: 1px; }
.powermanager scrolledwindow { background: transparent; border: none; }
window.powermanager.tray-popup { background-color: transparent; }
.powermanager .popup-surface {
    background-color: @theme_bg_color; border-radius: 12px;
    border: 1px solid alpha(@theme_fg_color, 0.14);
}
.powermanager.tray-popup .status { font-size: 9pt; }
"#;

pub const ICON_FALLBACKS: &[(&str, &[&str])] = &[
    ("preferences-system-power-symbolic", &["system-shutdown-symbolic"]),
    ("ac-adapter-symbolic", &["battery-ac-adapter-symbolic", "battery-level-100-plugged-in-symbolic", "battery-symbolic"]),
    ("battery-missing-symbolic", &["battery-level-0-symbolic", "battery-symbolic"]),
    ("battery-caution-symbolic", &["battery-level-10-symbolic", "battery-symbolic"]),
    ("battery-low-symbolic", &["battery-level-20-symbolic", "battery-symbolic"]),
    ("battery-good-symbolic", &["battery-level-70-symbolic", "battery-symbolic"]),
    ("battery-full-symbolic", &["battery-level-100-symbolic", "battery-symbolic"]),
];

/// Add classes; windows also follow the dock palette (companion_theme.attach).
pub fn style<W: IsA<gtk::Widget>>(widget: &W, names: &[&str]) -> W {
    for name in names {
        widget.add_css_class(name);
    }
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        suite_gtk::theme::attach(window, None);
    }
    widget.clone()
}

pub fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    suite_gtk::utils::set_margins(widget, value);
}

pub fn icon_names(name: &str) -> Vec<&str> {
    let mut names = vec![name];
    match ICON_FALLBACKS.iter().find(|(k, _)| *k == name) {
        Some((_, fallbacks)) => names.extend_from_slice(fallbacks),
        None => names.extend_from_slice(suite_gtk::utils::icon_fallbacks(name)),
    }
    names
}

pub fn set_icon(image: &gtk::Image, name: &str, size: Option<i32>) {
    image.set_from_gicon(&gio::ThemedIcon::from_names(&icon_names(name)));
    if let Some(size) = size {
        image.set_pixel_size(size);
    }
}

pub fn icon(name: &str, size: i32) -> gtk::Image {
    let image = gtk::Image::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    set_icon(&image, name, Some(size));
    image
}

pub fn label(text: &str, css: Option<&str>, wrap: bool) -> gtk::Label {
    let widget = gtk::Label::builder().label(text).xalign(0.0).build();
    if let Some(css) = css {
        widget.add_css_class(css);
    }
    if wrap {
        widget.set_wrap(true);
        widget.set_wrap_mode(pango::WrapMode::WordChar);
        widget.set_max_width_chars(44);
    } else {
        widget.set_ellipsize(pango::EllipsizeMode::End);
    }
    widget
}

pub fn install_style(window: &impl IsA<gtk::Widget>) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(STYLE_CSS);
    gtk::style_context_add_provider_for_display(&window.display(), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    provider
}
