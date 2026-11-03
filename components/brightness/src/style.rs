//! Shared styling for the Brightness popup (`brightness_style.py`).

use gtk::prelude::*;
use tessel_gtk::gtk;
use tessel_gtk::utils::{add_css, PRIORITY_APPLICATION};

pub const STYLE_CSS: &str = r#"
@define-color brightness_accent #3584e4;
@define-color brightness_surface mix(@theme_base_color, @theme_bg_color, 0.20);
@define-color brightness_muted alpha(@theme_fg_color, 0.64);
@define-color brightness_line alpha(@theme_fg_color, 0.10);
@define-color brightness_error mix(@theme_fg_color, #e05d65, 0.75);
window.brightnessmanager { background-color: @theme_bg_color; }
.brightnessmanager headerbar {
    background-image: none; background-color: @theme_bg_color;
    border: none; box-shadow: none; padding: 8px 14px;
}
.brightnessmanager .title { font-size: 23pt; font-weight: 700; }
.brightnessmanager headerbar .title { font-size: 11pt; font-weight: 600; }
.brightnessmanager .section-title { font-size: 11pt; font-weight: 600; }
.brightnessmanager .row-title { font-size: 10pt; font-weight: 600; }
.brightnessmanager .muted, .brightnessmanager .subtitle { color: @brightness_muted; }
.brightnessmanager .subtitle { font-size: 10pt; }
.brightnessmanager .status { color: @brightness_muted; font-size: 9pt; }
.brightnessmanager .error { color: @brightness_error; }
.brightnessmanager .card {
    background-color: @brightness_surface; border: 1px solid @brightness_line;
    border-radius: 14px;
}
.brightnessmanager .brightness-tile {
    min-width: 42px; min-height: 42px; border-radius: 12px;
    color: @brightness_accent; background-color: alpha(@brightness_accent, 0.12);
}
.brightnessmanager .profile-check { color: @brightness_accent; }
.brightnessmanager button:not(.titlebutton) {
    background-image: none; background-color: @brightness_surface;
    border: 1px solid @brightness_line; border-radius: 8px;
    color: @theme_fg_color; box-shadow: none; text-shadow: none;
    min-height: 27px; padding: 5px 12px;
}
.brightnessmanager button:hover { background-color: mix(@brightness_surface, @theme_fg_color, 0.06); }
.brightnessmanager button:disabled { opacity: 0.45; }
.brightnessmanager button:focus-visible { outline: 2px solid alpha(@brightness_accent, 0.6); outline-offset: -2px; }
.brightnessmanager button.primary {
    background-color: @brightness_accent; color: white; border-color: transparent;
}
.brightnessmanager button.icon-button {
    background-color: transparent; border-color: transparent;
    padding: 5px; min-width: 25px;
}
.brightnessmanager button.icon-button:hover { background-color: alpha(@theme_fg_color, 0.07); }
.brightnessmanager button.profile-row {
    background-color: transparent; border-color: transparent; border-radius: 10px;
    padding: 10px 12px;
}
.brightnessmanager button.profile-row:hover { background-color: alpha(@theme_fg_color, 0.045); }
.brightnessmanager button.profile-row.selected {
    background-color: alpha(@brightness_accent, 0.12); border-color: alpha(@brightness_accent, 0.22);
}
.brightnessmanager combobox button { min-height: 27px; }
.brightnessmanager separator { background-color: @brightness_line; min-height: 1px; }
.brightnessmanager scrolledwindow { background: transparent; border: none; }
window.brightnessmanager.tray-popup { background-color: transparent; }
.brightnessmanager .popup-surface {
    background-color: @theme_bg_color; border-radius: 12px;
    border: 1px solid alpha(@theme_fg_color, 0.14);
}
.brightnessmanager.tray-popup .status { font-size: 9pt; }
.brightnessmanager scale { padding: 4px 0; }
.brightnessmanager scale trough { min-height: 5px; border-radius: 4px; background-color: alpha(@theme_fg_color, 0.12); }
.brightnessmanager scale highlight { background-color: @brightness_accent; border-radius: 4px; }
.brightnessmanager scale slider { margin: -4px; padding: 0; min-width: 14px; min-height: 14px; background-color: @theme_fg_color; border: none; box-shadow: none; }
.brightnessmanager scale:disabled { opacity: 0.3; }
.brightnessmanager .value { font-weight: 600; font-size: 10pt; }
.brightnessmanager switch:checked { background-color: @brightness_accent; }
"#;

/// Add CSS classes; windows also follow the dock palette (`companion_theme.attach`).
pub fn style<W: IsA<gtk::Widget>>(widget: &W, names: &[&str]) {
    for name in names {
        widget.add_css_class(name);
    }
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        tessel_gtk::theme::attach(window, None);
    }
}

pub fn margins<W: IsA<gtk::Widget>>(widget: &W, value: i32) {
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
    widget.set_margin_start(value);
    widget.set_margin_end(value);
}

pub fn icon(name: &str, size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(size);
    image
}

pub fn label(text: &str, css: Option<&str>, wrap: bool) -> gtk::Label {
    let widget = gtk::Label::new(Some(text));
    widget.set_xalign(0.0);
    if let Some(css) = css {
        style(&widget, &[css]);
    }
    if wrap {
        widget.set_wrap(true);
        widget.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        widget.set_max_width_chars(44);
    } else {
        widget.set_ellipsize(gtk::pango::EllipsizeMode::End);
    }
    widget
}

/// One provider per window at APPLICATION priority, removed on unrealize.
pub fn install_style(window: &gtk::Window) -> gtk::CssProvider {
    add_css(STYLE_CSS, Some(&WidgetExt::display(window)), PRIORITY_APPLICATION)
}
