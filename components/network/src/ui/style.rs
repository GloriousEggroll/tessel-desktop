//! `network_style.py`: the shared sheet (APPLICATION priority) and widget helpers.

use gtk::prelude::*;

pub const STYLE_CSS: &str = r#"
@define-color network_accent @theme_selected_bg_color;
@define-color network_surface mix(@theme_base_color, @theme_bg_color, 0.18);
@define-color network_muted alpha(@theme_fg_color, 0.62);
@define-color network_line alpha(@theme_fg_color, 0.10);
@define-color network_green mix(@theme_fg_color, #28a273, 0.70);
@define-color network_error mix(@theme_fg_color, #e05d65, 0.75);
window.networkmanager { background-color: @theme_bg_color; }
.networkmanager headerbar {
    background-image: none; background-color: @theme_bg_color;
    border: none; box-shadow: none; padding: 8px 14px;
}
.networkmanager headerbar windowcontrols button.image-button {
    min-width: 24px; min-height: 24px; padding: 2px; margin: 0;
    border: none; border-radius: 6px; background: transparent;
}
.networkmanager headerbar windowcontrols button:hover {
    background-color: alpha(@theme_fg_color, 0.08);
}
.networkmanager headerbar windowcontrols button image {
    color: @theme_fg_color;
}
.networkmanager .title { font-size: 24pt; font-weight: 700; }
.networkmanager headerbar .title { font-size: inherit; }
.networkmanager .subtitle, .networkmanager .muted { color: @network_muted; }
.networkmanager .subtitle { font-size: 10pt; }
.networkmanager .section-title { font-size: 11pt; font-weight: 600; }
.networkmanager .device-name { font-size: 10pt; font-weight: 600; }
.networkmanager .device-detail { font-size: 8.5pt; color: @network_muted; }
.networkmanager .card, .networkmanager .device-list {
    background-color: @network_surface; border: 1px solid @network_line;
    border-radius: 14px;
}
.networkmanager .device-list > row {
    background: transparent; border: none; padding: 0;
    border-bottom: 1px solid @network_line;
}
.networkmanager .device-list > row:last-child { border-bottom: none; }
.networkmanager .device-list > row:hover {
    background-color: alpha(@theme_fg_color, 0.025);
}
.networkmanager .device-icon, .networkmanager .network-tile {
    min-width: 42px; min-height: 42px; border-radius: 12px;
    color: @network_accent; background-color: alpha(@network_accent, 0.10);
}
.networkmanager .badge {
    font-size: 8pt; font-weight: 600; border-radius: 8px;
    padding: 2px 6px; color: @network_muted;
    background-color: alpha(@theme_fg_color, 0.065);
}
.networkmanager .badge.connected {
    color: @network_green; background-color: alpha(@network_green, 0.10);
}
.networkmanager button:not(.titlebutton) {
    background-image: none; background-color: @network_surface;
    color: @theme_fg_color; border: 1px solid @network_line;
    border-radius: 8px; box-shadow: none; text-shadow: none;
    min-height: 28px; padding: 4px 12px;
}
.networkmanager button:hover {
    background-color: mix(@network_surface, @theme_fg_color, 0.06);
}
.networkmanager button:checked {
    background-color: alpha(@network_accent, 0.15); color: @network_accent;
}
.networkmanager button.primary {
    background-color: @network_accent;
    color: @theme_selected_fg_color; border-color: transparent;
}
.networkmanager button.connect-action {
    color: @network_accent; background-color: alpha(@network_accent, 0.09);
    border-color: transparent;
}
.networkmanager button.icon-button {
    padding: 4px 7px; min-width: 24px;
    background-color: transparent; border-color: transparent;
}
.networkmanager button.icon-button:hover {
    background-color: alpha(@theme_fg_color, 0.07);
}
.networkmanager button.destructive-action {
    color: @network_error; background-color: alpha(@network_error, 0.09);
    border-color: transparent;
}
.networkmanager button:focus-visible {
    outline: 2px solid alpha(@network_accent, 0.65); outline-offset: -2px;
}
.networkmanager button:disabled { opacity: 0.45; }
.networkmanager entry {
    border-radius: 8px; border-color: @network_line; padding: 8px;
}
.networkmanager switch:checked {
    background-image: none; background-color: @network_accent;
    border-color: @network_accent;
}
.networkmanager switch:checked slider {
    background-image: none; background-color: @theme_selected_fg_color;
}
.networkmanager switch:disabled {
    background-image: none; background-color: alpha(@theme_fg_color, 0.16);
    border-color: alpha(@theme_fg_color, 0.12); opacity: 0.5;
}
.networkmanager switch:disabled slider {
    background-image: none; background-color: @theme_fg_color;
    border-color: transparent; box-shadow: none;
}
.networkmanager .status { font-size: 9pt; color: @network_muted; }
.networkmanager .error { color: @network_error; }
.networkmanager .empty { color: @network_muted; padding: 32px 20px; }
.networkmanager .empty-icon { color: alpha(@theme_fg_color, 0.34); }
.networkmanager .editor-sidebar { background: transparent; border: none; }
.networkmanager .editor-sidebar row { padding: 9px 12px; border-radius: 8px; }
.networkmanager .editor-sidebar row:selected,
.networkmanager .connection-types row:selected {
    background-color: alpha(@network_accent, 0.18); color: @theme_fg_color;
}
window.networkmanager.tray-popup { background-color: transparent; }
.networkmanager .popup-surface {
    background-color: @theme_bg_color; border-radius: 12px;
    border: 1px solid alpha(@theme_fg_color, 0.14);
}
.networkmanager.tray-popup .device-icon {
    min-width: 34px; min-height: 34px; border-radius: 10px;
}
.networkmanager.tray-popup button:not(.titlebutton) {
    min-height: 24px; padding: 3px 10px;
}
.networkmanager.tray-popup button.icon-button { min-width: 22px; padding: 3px 5px; }
"#;

/// Add CSS classes; windows also follow the dock palette (companion theme).
pub fn style<W: IsA<gtk::Widget>>(widget: &W, names: &[&str]) -> W {
    for name in names {
        widget.add_css_class(name);
    }
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        tessel_gtk::theme::attach(window, None);
    }
    widget.clone()
}

/// Direct children snapshot (including those hidden by a filter).
pub fn children(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    tessel_gtk::utils::children(widget)
}

pub fn remove_children(container: &impl IsA<gtk::Widget>) {
    for child in children(container) {
        if let Some(b) = container.dynamic_cast_ref::<gtk::Box>() {
            b.remove(&child);
        } else if let Some(g) = container.dynamic_cast_ref::<gtk::Grid>() {
            g.remove(&child);
        } else {
            child.unparent();
        }
    }
}

/// Give a box child the available space on the box's layout axis.
pub fn expand<W: IsA<gtk::Widget>>(widget: &W, container: &gtk::Box) -> W {
    if container.orientation() == gtk::Orientation::Horizontal {
        widget.set_hexpand(true);
    } else {
        widget.set_vexpand(true);
    }
    widget.clone()
}

pub fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
    widget.set_margin_start(value);
    widget.set_margin_end(value);
}

pub fn icon(name: &str, size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(if name.is_empty() { "network-workgroup-symbolic" } else { name });
    image.set_pixel_size(size);
    image
}

pub fn label(text: &str, css: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::builder().label(text).xalign(0.0).build();
    widget.set_ellipsize(gtk::pango::EllipsizeMode::End);
    if let Some(css) = css {
        widget.add_css_class(css);
    }
    widget
}

pub fn install_style(widget: &impl IsA<gtk::Widget>) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(STYLE_CSS);
    gtk::style_context_add_provider_for_display(&WidgetExt::display(widget), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    provider
}

pub fn remove_style(widget: &impl IsA<gtk::Widget>, provider: &gtk::CssProvider) {
    gtk::style_context_remove_provider_for_display(&WidgetExt::display(widget), provider);
}

pub fn accessible_label(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}
