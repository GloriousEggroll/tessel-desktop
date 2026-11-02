//! `global_style.py`: navigation styling plus the shared page header, card
//! and row look. CSS text is byte-identical to Python.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tessel_core::tr;
use tessel_gtk::utils::{add_css, icon_fallbacks, PRIORITY_APPLICATION};

use crate::tags;

/// Symbolic stand-ins for icon themes (such as Breeze) that lack a name.
pub const SYMBOLIC_FALLBACKS: &[(&str, &[&str])] = &[
    ("bluetooth-active-symbolic", &["preferences-system-bluetooth-symbolic", "network-bluetooth-symbolic"]),
    ("video-display-symbolic", &["monitor-symbolic", "preferences-desktop-display-randr-symbolic", "computer-symbolic"]),
    ("preferences-desktop-display-symbolic", &["view-app-grid-symbolic", "view-grid-symbolic"]),
    ("preferences-desktop-symbolic", &["virtual-desktops-symbolic", "window-symbolic"]),
    ("preferences-system-time-symbolic", &["clock-symbolic", "smallclock-symbolic"]),
    ("view-reveal-symbolic", &["view-visible-symbolic", "visibility-symbolic"]),
    ("preferences-system-notifications-symbolic", &["notifications-symbolic", "preferences-desktop-notification-symbolic"]),
    ("preferences-desktop-wallpaper-symbolic", &["preferences-desktop-theme-global-symbolic", "image-x-generic-symbolic"]),
    ("preferences-desktop-appearance-symbolic", &["preferences-desktop-theme-global-symbolic", "weather-clear-night-symbolic"]),
    ("preferences-desktop-keyboard-shortcuts-symbolic", &["preferences-desktop-keyboard-shortcut-symbolic",
        "configure-shortcuts-symbolic", "input-keyboard-symbolic"]),
    ("night-light-symbolic", &["weather-clear-night-symbolic", "redshift-status-on-symbolic"]),
    ("power-profile-balanced-symbolic", &["battery-good-symbolic"]),
    ("dialog-warning-symbolic", &["emblem-important-symbolic", "state-warning-symbolic"]),
    ("dialog-information-symbolic", &["state-information-symbolic", "help-about-symbolic"]),
    ("dialog-password-symbolic", &["lock-symbolic", "changes-prevent-symbolic"]),
];

pub const OPTICAL_SCALE_CLASS: &str = "gs-icon-optical";
pub const OPTICAL_SCALE_FILES: &[&str] = &[
    "help-about-symbolic.svg", "clock-symbolic.svg", "smallclock-symbolic.svg",
    "state-information-symbolic.svg", "dialog-information-symbolic.svg",
];

fn symbolic_fallbacks(name: &str) -> &'static [&'static str] {
    SYMBOLIC_FALLBACKS.iter().find(|(key, _)| *key == name).map(|(_, v)| *v).unwrap_or(&[])
}

/// The candidate names `symbolic_icon` resolves through.
pub fn symbolic_names(name: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for candidate in std::iter::once(name).chain(symbolic_fallbacks(name).iter().copied())
        .chain(icon_fallbacks(name).iter().copied()) {
        if candidate.ends_with("-symbolic") && !names.iter().any(|n| n == candidate) {
            names.push(candidate.to_owned());
        }
    }
    if names.is_empty() {
        names.push(name.to_owned());
    }
    names
}

/// An image that only ever resolves to symbolic (recolourable) icons.
pub fn symbolic_icon(name: &str, size: i32) -> gtk::Image {
    let result = gtk::Image::from_gicon(&symbolic_gicon(&symbolic_names(name), size));
    result.set_pixel_size(size);
    optical_scale(&result);
    result
}

fn optical_scale(picture: &gtk::Image) {
    let round = picture.gicon().and_then(|g| g.downcast::<gio::FileIcon>().ok())
        .and_then(|f| f.file().basename())
        .map(|b| OPTICAL_SCALE_FILES.contains(&b.to_string_lossy().as_ref()))
        .unwrap_or(false);
    if round {
        picture.add_css_class(OPTICAL_SCALE_CLASS);
    } else {
        picture.remove_css_class(OPTICAL_SCALE_CLASS);
    }
}

fn symbolic_gicon(names: &[String], size: i32) -> gio::Icon {
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let themed = gio::ThemedIcon::from_names(&refs);
    let Some(display) = gdk::Display::default() else { return themed.upcast() };
    if size <= 16 {
        return themed.upcast();
    }
    let found = gtk::IconTheme::for_display(&display).lookup_by_gicon(
        &themed, 16, 1, gtk::TextDirection::None, gtk::IconLookupFlags::FORCE_SYMBOLIC);
    let path = found.file();
    match path {
        Some(path) if path.basename().map(|b| b.to_string_lossy().ends_with("-symbolic.svg")).unwrap_or(false) =>
            gio::FileIcon::new(&path).upcast(),
        _ => themed.upcast(),
    }
}

/// Re-resolve an image through the symbolic-only fallbacks, now and on updates.
pub fn symbolic_image(picture: &gtk::Image) {
    if tags::flag(picture, "gs_symbolic_busy") {
        return;
    }
    let mut name = picture.icon_name().map(|n| n.to_string());
    if name.as_deref().map(str::is_empty).unwrap_or(true) {
        name = picture.gicon().and_then(|g| g.downcast::<gio::ThemedIcon>().ok())
            .and_then(|t| t.names().first().map(|n| n.to_string()));
    }
    if let Some(mut name) = name.filter(|n| !n.is_empty()) {
        if !name.ends_with("-symbolic") {
            name.push_str("-symbolic");
        }
        tags::set_flag(picture, "gs_symbolic_busy", true);
        let size = picture.pixel_size();
        if let Some(gicon) = symbolic_icon(&name, 24).gicon() {
            picture.set_from_gicon(&gicon);
        }
        if size > 0 {
            picture.set_pixel_size(size);
        }
        optical_scale(picture);
        tags::set_flag(picture, "gs_symbolic_busy", false);
    }
    if !tags::flag(picture, "gs_symbolic_watch") {
        tags::set_flag(picture, "gs_symbolic_watch", true);
        picture.connect_notify_local(Some("icon-name"), |widget, _| symbolic_image(widget));
        picture.connect_notify_local(Some("gicon"), |widget, _| symbolic_image(widget));
    }
}

pub const TITLE_SIZE: &str = "18pt";
pub const CARD_RADIUS: &str = "12px";
pub const PAGE_MARGIN: i32 = 24;

/// `PAGE_CSS` (TITLE_SIZE / CARD_RADIUS already substituted).
pub const PAGE_CSS: &str = "
.gs-page-header { margin-bottom: 4px; }
.gs-page-title { font-size: 18pt; font-weight: 700; }
.gs-page-subtitle { font-size: 9.5pt; font-weight: 400; color: alpha(@theme_fg_color, .7); }
.gs-page-icon { background: none; border: none; box-shadow: none; padding: 0;
    min-width: 24px; min-height: 24px; color: @theme_fg_color; }
.gs-icon-optical { -gtk-icon-transform: scale(1.12); }
.gs-section-title { font-size: 11pt; font-weight: 700; }
.gs-row-title { font-size: 10.5pt; font-weight: 400; }
.gs-row-subtitle, .gs-muted { color: alpha(@theme_fg_color, .7); font-size: 9.5pt; }
.gs-card, .system-page .card { padding: 4px 12px; border-radius: 12px;
    border: 1px solid alpha(@theme_fg_color, .12); background-color: alpha(@theme_fg_color, .035); }
.gs-card list, .system-page .card list { background: transparent; }
.gs-card > :not(.gs-row), .system-page .card > :not(.gs-row) { margin-top: 8px; margin-bottom: 8px; }
.gs-row { min-height: 50px; padding-top: 8px; padding-bottom: 8px; }
list.card, list.gs-card { padding: 0 12px; }
list.card > row, list.gs-card > row { min-height: 50px; margin: 0; padding: 4px 0; background: none; }
list.card > row + row, list.gs-card > row + row { border-top: 1px solid alpha(@theme_fg_color, .08); }
.gs-row + .gs-row { border-top: 1px solid alpha(@theme_fg_color, .08); }
.gs-footer { font-size: 9.5pt; color: alpha(@theme_fg_color, .7); }
.gs-footer.error { color: @error_color; }
.gs-empty-state { padding: 36px; }
.gs-empty-state .gs-empty-title { font-size: 14pt; font-weight: 700; }
.gs-empty-state .gs-empty-detail { font-size: 10pt; color: alpha(@theme_fg_color, .7); }
.gs-empty-state button { padding: 8px 20px; border-radius: 8px; }
.gs-toast { padding: 8px 8px 8px 16px; border-radius: 12px;
    background-color: alpha(@theme_fg_color, .10); border: 1px solid alpha(@theme_fg_color, .14); }
";

pub const CSS: &str = "
window.tessel-settings { background: @theme_bg_color; color: @theme_fg_color; }
.tessel-settings headerbar {
  background-image: none; background-color: @theme_bg_color;
  box-shadow: none; border-bottom: 1px solid alpha(@theme_fg_color, .1); padding: 6px 10px;
}
.tessel-settings .settings-navigation {
  background-color: alpha(@theme_fg_color, .025);
  border-right: 1px solid alpha(@theme_fg_color, .10);
}
.tessel-settings .settings-navigation list { background: transparent; }
.tessel-settings .settings-navigation row { border-radius: 8px; margin: 2px 9px; padding: 0; }
.tessel-settings .settings-navigation row:selected {
  background-color: alpha(@theme_selected_bg_color, .17); color: @theme_fg_color;
}
.tessel-settings .settings-navigation row:hover { background-color: alpha(@theme_fg_color, .07); }
.tessel-settings .settings-navigation row image { color: @theme_fg_color; }
.tessel-settings .settings-navigation row:selected image { color: @theme_selected_bg_color; }
.tessel-settings .navigation-title { font-size: 16pt; font-weight: 700; margin: 22px 18px 18px; }
.tessel-settings .navigation-item { padding: 10px 12px; font-size: 10.5pt; }
.tessel-settings .settings-search { margin: 0 12px 12px; }
.tessel-settings .settings-search image { margin-right: 8px; }
.tessel-settings .settings-search-page { padding: 24px; }
.tessel-settings .settings-search-results { background: transparent; }
.tessel-settings .settings-search-results row {
  padding: 12px; margin: 3px 0; border-radius: 12px;
  border: 1px solid alpha(@theme_fg_color, .10);
}
.tessel-settings .settings-search-results row:hover { background: alpha(@theme_fg_color, .06); }
.tessel-settings .settings-search-results row:selected { background: alpha(@theme_selected_bg_color, .15); }
.tessel-settings .search-setting-title { font-size: 10.5pt; font-weight: 600; }
.tessel-settings .search-section { font-size: 9pt; font-weight: 600; color: alpha(@theme_fg_color, .7); }
.tessel-settings .search-detail { font-size: 9.5pt; color: alpha(@theme_fg_color, .7); }
.tessel-settings .settings-search-target { outline: 2px solid currentColor; outline-offset: -2px; border-radius: 8px; }
/* Explicit, non-negative slider boxes: some GTK themes (Nobara) pair negative
   margins with borders that add up to a negative size (\"GtkGizmo slider
   reported min width -2\"). */
.tessel-settings scrollbar slider, .tessel-settings scrollbar.horizontal slider {
  min-width: 6px; min-height: 6px; margin: 0; padding: 0;
  border: 2px solid transparent; border-radius: 8px; background-clip: padding-box;
}
.tessel-settings scrollbar.vertical slider { min-height: 30px; }
.tessel-settings scrollbar.horizontal slider { min-width: 30px; }
.tessel-settings .tessel-dock-settings scale highlight { margin: 0; }
/* Cards and headers of pages that other suite components build. */
window.tessel-settings .card, window.tessel-settings .settings-card, window.tessel-settings .power-card {
  border-radius: 12px; border: 1px solid alpha(@theme_fg_color, .12); background-color: alpha(@theme_fg_color, .035);
}
window.tessel-settings .settings-section-title { font-size: 11pt; font-weight: 700; }
";

/// `install(window)`: class, CSS + PAGE_CSS, companion palette. Returns the
/// closer that releases both.
pub fn install(window: &impl IsA<gtk::Window>) -> impl Fn() {
    let window = window.as_ref().clone();
    window.add_css_class("tessel-settings");
    let display = WidgetExt::display(&window);
    let provider = add_css(&format!("{CSS}{PAGE_CSS}"), Some(&display), PRIORITY_APPLICATION);
    let theme = tessel_gtk::theme::attach(&window, None);
    let done = Cell::new(false);
    move || {
        if done.replace(true) {
            return;
        }
        theme.close();
        gtk::style_context_remove_provider_for_display(&display, &provider);
    }
}

/// `page_css(display)`.
pub fn page_css(display: Option<&gdk::Display>) -> gtk::CssProvider {
    add_css(PAGE_CSS, display, PRIORITY_APPLICATION)
}

/// `page_header()` result: the box plus its `title`/`subtitle`/`icon`.
#[derive(Clone)]
pub struct PageHeader {
    pub root: gtk::Box,
    pub title: gtk::Label,
    pub subtitle: Option<gtk::Label>,
    pub icon: Option<gtk::Image>,
}

/// The one page header: optional 24px symbolic icon, title, muted subtitle.
pub fn page_header(title: &str, subtitle: Option<&str>, icon: Option<&str>) -> PageHeader {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.add_css_class("gs-page-header");
    let mut picture = None;
    if let Some(icon) = icon.filter(|i| !i.is_empty()) {
        let image = symbolic_icon(icon, 24);
        image.add_css_class("gs-page-icon");
        image.set_valign(gtk::Align::Center);
        header.append(&image);
        picture = Some(image);
    }
    let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true)
        .valign(gtk::Align::Center).build();
    let heading = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    heading.add_css_class("gs-page-title");
    titles.append(&heading);
    let mut sub = None;
    if let Some(text) = subtitle.filter(|s| !s.is_empty()) {
        let label = gtk::Label::builder().label(text).xalign(0.0).wrap(true).build();
        label.add_css_class("gs-page-subtitle");
        titles.append(&label);
        sub = Some(label);
    }
    header.append(&titles);
    PageHeader { root: header, title: heading, subtitle: sub, icon: picture }
}

pub const TITLE_CLASSES: &[&str] = &["gs-page-title", "page-title", "title", "settings-title", "notification-heading",
    "popup-title", "title-1", "title-2", "search-title"];
pub const SUBTITLE_CLASSES: &[&str] = &["subtitle", "page-subtitle", "popup-subtitle", "settings-muted", "muted", "detail"];
pub const TILE_CLASSES: &[&str] = &["settings-tile", "tile", "notification-tile", "popup-icon-tile", "icon-tile"];

fn children(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut child = widget.first_child();
    while let Some(item) = child {
        child = item.next_sibling();
        out.push(item);
    }
    out
}

/// `_find_title(root, depth=9)`: breadth-first, first label with a title class.
pub fn find_title(root: &impl IsA<gtk::Widget>, depth: usize) -> Option<gtk::Label> {
    let mut level = vec![root.as_ref().clone()];
    for _ in 0..depth {
        let mut following = Vec::new();
        for widget in &level {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                if TITLE_CLASSES.iter().any(|name| label.has_css_class(name)) {
                    return Some(label.clone());
                }
            }
            following.extend(children(widget));
        }
        level = following;
    }
    None
}

fn is_box(widget: Option<&gtk::Widget>, orientation: gtk::Orientation) -> bool {
    widget.and_then(|w| w.downcast_ref::<gtk::Box>()).map(|b| b.orientation() == orientation).unwrap_or(false)
}

fn hold_header_size(picture: &gtk::Image) {
    picture.set_pixel_size(24);
    if !tags::flag(picture, "gs_size_watch") {
        tags::set_flag(picture, "gs_size_watch", true);
        picture.connect_notify_local(Some("pixel-size"), |widget, _| {
            if widget.pixel_size() != 24 {
                widget.set_pixel_size(24);
            }
        });
    }
}

/// Icon argument of `unify_header`: a name or a ready image.
pub enum HeaderIcon<'a> {
    Name(&'a str),
    Image(gtk::Image),
}

impl HeaderIcon<'_> {
    fn picture(self) -> gtk::Image {
        match self {
            HeaderIcon::Name(name) => symbolic_icon(name, 24),
            HeaderIcon::Image(image) => image,
        }
    }
}

/// `unify_header(root, icon)`: restyle the first page title under `root` as
/// the shared page header. Returns the header box.
pub fn unify_header(root: &impl IsA<gtk::Widget>, icon: Option<HeaderIcon>) -> Option<gtk::Widget> {
    let title = find_title(root, 9)?;
    let mut titles: gtk::Widget = title.parent()?;
    if is_box(Some(&titles), gtk::Orientation::Vertical)
        && children(&titles).iter().any(|c| !c.is::<gtk::Label>()) {
        let mut moved: Vec<gtk::Widget> = vec![title.clone().upcast()];
        let mut widget = title.next_sibling();
        while let Some(item) = widget.clone().filter(|w| w.is::<gtk::Label>()) {
            widget = item.next_sibling();
            moved.push(item);
        }
        let previous = title.prev_sibling();
        let block = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
        let container = titles.downcast_ref::<gtk::Box>().unwrap().clone();
        for item in &moved {
            container.remove(item);
            block.append(item);
        }
        container.insert_child_after(&block, previous.as_ref());
        titles = block.upcast();
    }
    let mut header = titles.clone();
    if is_box(Some(&titles), gtk::Orientation::Vertical)
        && is_box(titles.parent().as_ref(), gtk::Orientation::Horizontal) {
        header = titles.parent().unwrap();
    }
    if header.has_css_class("gs-page-header") {
        if let Some(icon) = icon {
            if !children(&header).iter().any(|c| c.has_css_class("gs-page-icon")) {
                let picture = icon.picture();
                picture.add_css_class("gs-page-icon");
                picture.set_valign(gtk::Align::Center);
                if let Some(b) = header.downcast_ref::<gtk::Box>() {
                    b.prepend(&picture);
                }
            }
        }
        return Some(header);
    }
    for name in TITLE_CLASSES {
        title.remove_css_class(name);
    }
    title.add_css_class("gs-page-title");
    if titles != header || is_box(Some(&titles), gtk::Orientation::Vertical) {
        for widget in children(&titles) {
            if widget != *title.upcast_ref::<gtk::Widget>() && widget.is::<gtk::Label>() {
                for name in SUBTITLE_CLASSES {
                    widget.remove_css_class(name);
                }
                widget.add_css_class("gs-page-subtitle");
            }
        }
    }
    let mut found_icon = false;
    if is_box(Some(&header), gtk::Orientation::Horizontal) {
        header.downcast_ref::<gtk::Box>().unwrap().set_spacing(12);
        for widget in children(&header) {
            if widget == titles || widget == *title.upcast_ref::<gtk::Widget>() {
                break;
            }
            let pictures: Vec<gtk::Image> = if let Some(image) = widget.downcast_ref::<gtk::Image>() {
                vec![image.clone()]
            } else {
                children(&widget).into_iter().filter_map(|c| c.downcast::<gtk::Image>().ok()).collect()
            };
            if pictures.is_empty() {
                continue;
            }
            for name in TILE_CLASSES {
                widget.remove_css_class(name);
            }
            widget.add_css_class("gs-page-icon");
            widget.set_valign(gtk::Align::Center);
            for picture in &pictures {
                symbolic_image(picture);
                hold_header_size(picture);
                picture.set_size_request(-1, -1);
            }
            widget.set_size_request(-1, -1);
            found_icon = true;
        }
    }
    if !found_icon {
        if let Some(icon) = icon {
            let picture = icon.picture();
            picture.add_css_class("gs-page-icon");
            picture.set_valign(gtk::Align::Center);
            if is_box(Some(&header), gtk::Orientation::Horizontal) {
                header.downcast_ref::<gtk::Box>().unwrap().prepend(&picture);
            } else if let Some(parent) = header.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) {
                let previous = header.prev_sibling();
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                parent.remove(&header);
                header.set_hexpand(true);
                row.append(&picture);
                row.append(&header);
                parent.insert_child_after(&row, previous.as_ref());
                header = row.upcast();
            }
        }
    }
    header.add_css_class("gs-page-header");
    Some(header)
}

/// `align_page_gutter(root, header)`; returns the applied shift.
pub fn align_page_gutter(root: &impl IsA<gtk::Widget>, header: Option<&gtk::Widget>) -> i32 {
    let Some(header) = header else { return 0 };
    let root = root.as_ref();
    let mut chain = Vec::new();
    let mut widget = Some(header.clone());
    let mut reached = false;
    while let Some(item) = widget {
        chain.push(item.clone());
        if item == *root {
            reached = true;
            break;
        }
        widget = item.parent();
    }
    if !reached {
        return 0;
    }
    let delta = PAGE_MARGIN - chain.iter().map(|w| w.margin_start()).sum::<i32>();
    if delta == 0 || delta.abs() > 8 {
        return 0;
    }
    for item in chain.iter().rev() {
        let (start, end) = (item.margin_start(), item.margin_end());
        if start != 0 && start == end && start + delta >= 0 {
            item.set_margin_start(start + delta);
            item.set_margin_end(end + delta);
            return delta;
        }
    }
    if header.margin_start() + delta >= 0 {
        header.set_margin_start(header.margin_start() + delta);
        return delta;
    }
    0
}

/// `empty_state()` result.
#[derive(Clone)]
pub struct EmptyState {
    pub root: gtk::Box,
    pub title: gtk::Label,
    pub detail: gtk::Label,
    pub button: Option<gtk::Button>,
}

/// Centered icon, title and description for errors and empty pages.
pub fn empty_state(title: &str, detail: &str, icon: &str, action: Option<&str>,
                   callback: Option<Box<dyn Fn()>>) -> EmptyState {
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).vexpand(true).hexpand(true)
        .valign(gtk::Align::Center).halign(gtk::Align::Center).build();
    root.add_css_class("gs-empty-state");
    let picture = symbolic_icon(icon, 48);
    picture.add_css_class("dim-label");
    root.append(&picture);
    let heading = gtk::Label::builder().label(title).wrap(true).justify(gtk::Justification::Center).build();
    heading.add_css_class("gs-empty-title");
    root.append(&heading);
    let text = gtk::Label::builder().label(detail).wrap(true).max_width_chars(58)
        .justify(gtk::Justification::Center).visible(!detail.is_empty()).build();
    text.add_css_class("gs-empty-detail");
    root.append(&text);
    let mut button = None;
    if let Some(action) = action.filter(|a| !a.is_empty()) {
        let b = gtk::Button::builder().label(action).halign(gtk::Align::Center).build();
        if let Some(callback) = callback {
            b.connect_clicked(move |_| callback());
        }
        root.append(&b);
        button = Some(b);
    }
    EmptyState { root, title: heading, detail: text, button }
}

/// `footer(label, real_error)`: one muted status line at the bottom of a page.
pub fn footer(label: &gtk::Label, real_error: Option<Rc<dyn Fn() -> bool>>) {
    label.add_css_class("gs-footer");
    for name in ["status", "muted", "settings-muted"] {
        label.remove_css_class(name);
    }
    if let Some(real_error) = real_error {
        let check = {
            let real_error = real_error.clone();
            move |widget: &gtk::Label| {
                if widget.has_css_class("error") && !real_error() {
                    widget.remove_css_class("error");
                }
            }
        };
        let c1 = check.clone();
        label.connect_notify_local(Some("css-classes"), move |widget, _| c1(widget));
        let c2 = check.clone();
        label.connect_notify_local(Some("label"), move |widget, _| {
            let widget = widget.clone();
            let c2 = c2.clone();
            glib::idle_add_local_full(glib::Priority::HIGH, move || {
                c2(&widget);
                glib::ControlFlow::Break
            });
        });
        check(label);
    }
}

pub fn card(spacing: i32) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, spacing);
    b.add_css_class("gs-card");
    b
}

/// `setting_row()` result.
#[derive(Clone)]
pub struct SettingRow {
    pub root: gtk::Box,
    pub title: gtk::Label,
    pub subtitle: Option<gtk::Label>,
}

/// A labelled row: title, optional help text, and its control at the end.
pub fn setting_row(title: &str, subtitle: Option<&str>, control: Option<&gtk::Widget>) -> SettingRow {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("gs-row");
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true)
        .valign(gtk::Align::Center).build();
    let heading = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    heading.add_css_class("gs-row-title");
    text.append(&heading);
    let mut sub = None;
    if let Some(subtitle) = subtitle.filter(|s| !s.is_empty()) {
        let detail = gtk::Label::builder().label(subtitle).xalign(0.0).wrap(true).max_width_chars(60).build();
        detail.add_css_class("gs-row-subtitle");
        text.append(&detail);
        sub = Some(detail);
    }
    row.append(&text);
    if let Some(control) = control {
        control.set_valign(gtk::Align::Center);
        row.append(control);
        control.update_property(&[gtk::accessible::Property::Label(title)]);
    }
    SettingRow { root: row, title: heading, subtitle: sub }
}

fn parent_window(widget: &gtk::Widget) -> Option<gtk::Window> {
    widget.root().and_then(|r| r.downcast::<gtk::Window>().ok())
}

/// Test hook replacing the dialog (`global_style.CHOOSER`):
/// `(message, detail, buttons, callback, default, cancel)`.
pub type Chooser = Rc<dyn Fn(&str, &str, &[String], Box<dyn FnOnce(Option<i32>)>, i32, Option<i32>)>;

thread_local! {
    static CHOOSER: RefCell<Option<Chooser>> = const { RefCell::new(None) };
}

pub fn set_chooser(chooser: Option<Chooser>) {
    CHOOSER.with(|c| *c.borrow_mut() = chooser);
}

/// Ask a question with a Gtk.AlertDialog; `callback(index)` runs on reply
/// (`cancel` when dismissed).
pub fn choose(widget: &impl IsA<gtk::Widget>, message: &str, detail: &str, buttons: &[String],
              callback: impl FnOnce(Option<i32>) + 'static, default: i32, cancel: Option<i32>) -> Option<gtk::AlertDialog> {
    if let Some(chooser) = CHOOSER.with(|c| c.borrow().clone()) {
        chooser(message, detail, buttons, Box::new(callback), default, cancel);
        return None;
    }
    let refs: Vec<&str> = buttons.iter().map(String::as_str).collect();
    let dialog = gtk::AlertDialog::builder().message(message).detail(detail).buttons(refs).modal(true).build();
    dialog.set_default_button(default);
    if let Some(cancel) = cancel {
        dialog.set_cancel_button(cancel);
    }
    let parent = parent_window(widget.as_ref());
    dialog.choose(parent.as_ref(), gio::Cancellable::NONE, move |result| {
        callback(match result { Ok(index) => Some(index), Err(_) => cancel });
    });
    Some(dialog)
}

/// Shared Apply / Discard / Cancel prompt for pages with unapplied edits.
/// `apply(done)` must call `done(ok)`. Returns true while asking.
pub fn confirm_leave(widget: &impl IsA<gtk::Widget>, pending: &str,
                     apply: impl FnOnce(Box<dyn FnOnce(bool)>) + 'static,
                     discard: impl FnOnce() + 'static, close_host: Rc<dyn Fn()>) -> bool {
    let widget = widget.as_ref().clone();
    if tags::flag(&widget, "gs_leave_prompt") {
        return true;
    }
    tags::set_flag(&widget, "gs_leave_prompt", true);
    let w = widget.clone();
    let answered = move |index: Option<i32>| {
        tags::set_flag(&w, "gs_leave_prompt", false);
        match index {
            Some(0) => {
                let close_host = close_host.clone();
                apply(Box::new(move |ok| if ok { close_host() }));
            }
            Some(1) => {
                discard();
                close_host();
            }
            _ => {}
        }
    };
    choose(&widget, &tr!("Apply your changes?"), pending,
           &[tr!("Apply"), tr!("Discard"), tr!("Cancel")], answered, 0, Some(2));
    true
}

/// An in-page notice with an optional action, such as Undo.
#[derive(Clone)]
pub struct Toast(Rc<ToastInner>);

pub struct ToastInner {
    pub revealer: gtk::Revealer,
    pub label: gtk::Label,
    pub action: gtk::Button,
    callback: RefCell<Option<Box<dyn FnOnce()>>>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl std::ops::Deref for Toast {
    type Target = ToastInner;
    fn deref(&self) -> &ToastInner { &self.0 }
}

impl Default for Toast {
    fn default() -> Self { Self::new() }
}

impl Toast {
    pub fn new() -> Toast {
        let revealer = gtk::Revealer::builder().transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(false).build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.add_css_class("gs-toast");
        let label = gtk::Label::builder().xalign(0.0).wrap(true).hexpand(true).build();
        content.append(&label);
        let action = gtk::Button::new();
        content.append(&action);
        let close = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text(tr!("Dismiss")).build();
        close.add_css_class("flat");
        content.append(&close);
        revealer.set_child(Some(&content));
        let toast = Toast(Rc::new(ToastInner { revealer, label, action, callback: RefCell::new(None),
                                               timer: RefCell::new(None) }));
        let weak = Rc::downgrade(&toast.0);
        toast.action.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let callback = inner.callback.borrow_mut().take();
                Toast(inner).dismiss();
                if let Some(callback) = callback {
                    callback();
                }
            }
        });
        let weak = Rc::downgrade(&toast.0);
        close.connect_clicked(move |_| if let Some(inner) = weak.upgrade() { Toast(inner).dismiss() });
        toast
    }

    pub fn widget(&self) -> &gtk::Revealer { &self.revealer }

    pub fn show(&self, message: &str, action: Option<&str>, callback: Option<Box<dyn FnOnce()>>, timeout: u32) {
        self.label.set_text(message);
        *self.callback.borrow_mut() = callback;
        self.action.set_label(action.unwrap_or(""));
        self.action.set_visible(action.map(|a| !a.is_empty()).unwrap_or(false));
        self.revealer.set_reveal_child(true);
        if let Some(id) = self.timer.borrow_mut().take() {
            tessel_core::glib_compat::source_remove(id);
        }
        if timeout > 0 {
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_seconds_local(timeout, move || {
                if let Some(inner) = weak.upgrade() {
                    inner.timer.borrow_mut().take();
                    Toast(inner).dismiss();
                }
                glib::ControlFlow::Break
            });
            *self.timer.borrow_mut() = Some(id);
        }
    }

    pub fn dismiss(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            tessel_core::glib_compat::source_remove(id);
        }
        self.callback.borrow_mut().take();
        self.revealer.set_reveal_child(false);
    }
}
