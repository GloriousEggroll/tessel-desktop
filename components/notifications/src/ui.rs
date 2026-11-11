//! GTK4 notifications, history and settings using the desktop's shared
//! palette (port of notification_ui.py).
//!
//! The daemon owns delivery, expiry and fullscreen policy. These views never
//! take keyboard focus for a toast and never deserialize notification markup:
//! every label uses `set_text`.

use crate::model::{app_groups, dnd_deadline, AppGroup};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::popup::{header_button, install_popup_css, popup_footer, popup_header, set_popup_width, HeaderAction,
                       PopupController, POPUP_PADDING};
use tessel_gtk::theme::{self, WindowTheme};
use tessel_gtk::utils::{accessible, add_css, children, headerbar, image, remove_css, set_margins, PRIORITY_APPLICATION};

pub const DND_CHOICES: [(&str, &str); 3] = [("hour", "For 1 hour"), ("tomorrow", "Until tomorrow"), ("off", "Until turned off")];

pub const STYLE: &str = "
window.tessel-notifications.tray-popup { background: transparent; }
.tessel-notifications .popup-surface {
    background: @theme_bg_color; color: @theme_fg_color;
    border: 1px solid alpha(@theme_fg_color, .15); border-radius: 8px;
}
.tessel-notifications .notification-card {
    padding: 12px; border-radius: 8px;
    border: 1px solid alpha(@theme_fg_color, .12);
}
.tessel-notifications .notification-card.popup-surface { border-radius: 8px; }
.tessel-notifications .notification-avatar { border-radius: 8px; }
.tessel-notifications .notification-card.critical {
    border-color: alpha(@theme_selected_bg_color, .7);
}
.tessel-notifications .notification-heading { font-size: 16pt; font-weight: 600; }
.tessel-notifications .notification-summary { font-size: 10.5pt; font-weight: 650; }
.tessel-notifications .notification-app { font-size: 9pt; font-weight: 600; }
.tessel-notifications .notification-time { font-size: 8pt; }
.tessel-notifications .muted { opacity: .76; }
.tessel-notifications .section-title { font-size: 10pt; font-weight: 600; }
.tessel-notifications button { border-radius: 8px; padding: 5px 10px; }
.tessel-notifications button.icon-button { min-width: 22px; min-height: 22px; padding: 3px; }
.tessel-notifications .notification-unread { color: @theme_selected_bg_color; }
.tessel-notifications .count-badge { border-radius: 8px; padding: 2px 6px; }
.tessel-notifications progressbar trough, .tessel-notifications progressbar progress {
    min-height: 5px; border-radius: 4px;
}
.tessel-notifications .settings-card {
    border-radius: 8px; padding: 12px; border: 1px solid alpha(@theme_fg_color, .12);
}
.tessel-notifications .settings-description { font-size: 9pt; }
.tessel-notifications .group-header { padding: 2px 0; }
.tessel-notifications .group-header button.group-toggle { padding: 2px 6px; }
.tessel-notifications .group-cards { margin-top: 8px; }
.tessel-notifications .dnd-card { padding: 12px 14px; border-radius: 8px;
    background-color: alpha(@theme_fg_color, 0.045); border: 1px solid alpha(@theme_fg_color, 0.10); }
.tessel-notifications .notification-center-title { font-size: 10.5pt; }
";

/// What the views need from the service: the daemon's in-process model or
/// the D-Bus [`crate::client::NotificationClient`].
pub trait Client {
    fn state(&self) -> Map<String, Value>;
    fn ready(&self) -> bool;
    fn error(&self) -> String;
    fn set_settings(&self, patch: Value);
    fn clear_history(&self);
    fn clear_app(&self, key: &str);
    fn mark_read(&self);
    fn dismiss(&self, ident: u32);
    fn activate(&self, ident: u32, key: &str);
    fn show_settings(&self);
}

impl Client for crate::client::NotificationClient {
    fn state(&self) -> Map<String, Value> { crate::client::NotificationClient::state(self) }
    fn ready(&self) -> bool { crate::client::NotificationClient::ready(self) }
    fn error(&self) -> String { crate::client::NotificationClient::error(self) }
    fn set_settings(&self, patch: Value) { crate::client::NotificationClient::set_settings(self, &patch); }
    fn clear_history(&self) { crate::client::NotificationClient::clear_history(self); }
    fn clear_app(&self, key: &str) { crate::client::NotificationClient::clear_app(self, key); }
    fn mark_read(&self) { crate::client::NotificationClient::mark_read(self); }
    fn dismiss(&self, ident: u32) { crate::client::NotificationClient::dismiss(self, ident); }
    fn activate(&self, ident: u32, key: &str) { crate::client::NotificationClient::activate(self, ident, key); }
    fn show_settings(&self) { crate::client::NotificationClient::show_settings(self); }
}

pub type Dismiss = Rc<dyn Fn(u32)>;
pub type Activate = Rc<dyn Fn(u32, &str)>;
pub type Hover = Rc<dyn Fn(u32, bool)>;

fn icon_button(icon: &str, tooltip: &str, callback: impl Fn() + 'static) -> gtk::Button {
    let text = tr!(tooltip);
    let button = gtk::Button::builder().child(&image(icon, 16)).tooltip_text(&text).build();
    button.add_css_class("icon-button");
    // Every card and group action is the same flat round-cornered icon.
    button.add_css_class("popup-flat-icon");
    accessible(&button, &text);
    button.connect_clicked(move |_| callback());
    button
}

/// `_text(value, limit)` of a JSON value.
pub fn text(value: Option<&Value>, limit: usize) -> String {
    crate::model::text_value(value, limit)
}

fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push(high << 4 | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Do not open remote URIs or let icon fields trigger network access.
pub fn local_path(value: &Value) -> Option<PathBuf> {
    let value = value.as_str()?;
    if value.contains('\0') {
        return None;
    }
    let mut value = value.to_owned();
    if let Some(rest) = value.strip_prefix("file:") {
        let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (netloc, path) = match rest.strip_prefix("//") {
            Some(after) => match after.find('/') {
                Some(slash) => (&after[..slash], &after[slash..]),
                None => (after, ""),
            },
            None => ("", rest),
        };
        if !(netloc.is_empty() || netloc == "localhost") || !query.is_empty() || !fragment.is_empty() {
            return None;
        }
        value = percent_decode(path);
    }
    value.starts_with('/').then(|| PathBuf::from(value))
}

pub fn file_texture(value: &Value) -> Option<gdk::Texture> {
    let path = local_path(value)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > 4 * 1024 * 1024 {
        return None;
    }
    let (_, width, height) = gtk::gdk_pixbuf::Pixbuf::file_info(&path)?;
    if !(0 < width && width <= 2048 && 0 < height && height <= 2048) {
        return None;
    }
    gdk::Texture::from_file(&gio::File::for_path(&path)).ok()
}

/// A paintable for an image hint: theme icon name, local file or raw data.
pub fn notification_texture(value: &Value) -> Option<gdk::Paintable> {
    if let Some(name) = value.as_str() {
        if !name.is_empty() && name.chars().count() <= 240 && !name.contains('/') && !name.contains(':') {
            let display = gdk::Display::default()?;
            let theme = gtk::IconTheme::for_display(&display);
            return Some(theme.lookup_icon(name, &[], 96, 1, gtk::TextDirection::None, gtk::IconLookupFlags::PRELOAD).upcast());
        }
        return file_texture(value).map(|t| t.upcast());
    }
    let map = value.as_object()?;
    // The backend validates pixels before publishing them. Recheck lengths at
    // the UI boundary because the history client is also a D-Bus consumer.
    let int = |key: &str| match map.get(key) {
        Some(Value::Number(n)) if !n.is_f64() => n.as_i64(),
        _ => None,
    };
    let (width, height, rowstride, channels) = (int("width")?, int("height")?, int("rowstride")?, int("channels")?);
    let alpha = map.get("has_alpha")?.as_bool()?;
    if !(0 < width && width <= 2048 && 0 < height && height <= 2048) || channels != if alpha { 4 } else { 3 }
        || rowstride < width * channels || rowstride * height > 4 * 1024 * 1024 {
        return None;
    }
    let encoded = match map.get("data") {
        None => "",
        Some(Value::String(text)) => text.as_str(),
        Some(_) => return None,
    };
    if encoded.len() > 6 * 1024 * 1024 {
        return None;
    }
    let mut data = tessel_core::base64::decode_strict(encoded)?;
    let needed = ((height - 1) * rowstride + width * channels) as usize;
    if !(needed <= data.len() && data.len() <= (rowstride * height) as usize) {
        return None;
    }
    data.resize((rowstride * height) as usize, 0);
    let format = if alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
    Some(gdk::MemoryTexture::new(width as i32, height as i32, format, &glib::Bytes::from_owned(data), rowstride as usize).upcast())
}

// Icons inside these directories are application or theme icons, not content
// images such as screenshots, so they never get the large picture.
const ICON_DIRECTORIES: [&str; 3] = ["/share/icons/", "/share/pixmaps/", "/icons/hicolor/"];
pub const FALLBACK_ICON: &str = "dialog-information-symbolic";

pub fn icon_name(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(name)) if !name.is_empty() && name.chars().count() <= 240 && !name.contains('/') && !name.contains(':') => name.clone(),
        _ => String::new(),
    }
}

fn icon_name_str(name: &str) -> String {
    icon_name(Some(&json!(name)))
}

fn theme_has(name: &str) -> bool {
    !name.is_empty() && gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(name))
}

fn desktop_icon(entry: Option<&Value>) -> Option<gio::Icon> {
    let entry = icon_name(entry);
    if entry.is_empty() {
        return None;
    }
    let name = if entry.ends_with(".desktop") { entry } else { entry + ".desktop" };
    tessel_core::glib_compat::desktop_app_info(&name).and_then(|info| info.icon())
}

/// A paintable, a GIcon or nothing.
#[derive(Clone, Debug)]
pub enum IconValue {
    Paintable(gdk::Paintable),
    Icon(gio::Icon),
}

/// The sender's own icon: icon file → theme icon → desktop entry icon →
/// lower-cased app name with dashes.
pub fn app_icon(notification: &Map<String, Value>) -> Option<IconValue> {
    let icon = notification.get("icon").cloned().unwrap_or_else(|| json!(""));
    if let Some(texture) = file_texture(&icon) {
        return Some(IconValue::Paintable(texture.upcast()));
    }
    let name = icon_name(Some(&icon));
    if theme_has(&name) {
        return Some(IconValue::Icon(gio::ThemedIcon::new(&name).upcast()));
    }
    if let Some(gicon) = desktop_icon(notification.get("desktop_entry")) {
        return Some(IconValue::Icon(gicon));
    }
    let guess = icon_name_str(&text(notification.get("app_name"), 240).trim().to_lowercase().replace(' ', "-"));
    theme_has(&guess).then(|| IconValue::Icon(gio::ThemedIcon::new(&guess).upcast()))
}

fn stem(path: &std::path::Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// True when an image hint is really an icon: a theme name, a file in an icon
/// directory, or the same file or name as the application icon.
pub fn image_is_icon(value: Option<&Value>, notification: &Map<String, Value>) -> bool {
    let Some(value) = value.filter(|v| v.as_str().is_some_and(|s| !s.is_empty())) else { return false };
    if !icon_name(Some(value)).is_empty() {
        return true;
    }
    let Some(path) = local_path(value) else { return false };
    let icon = notification.get("icon").cloned().unwrap_or_else(|| json!(""));
    if *value == icon || local_path(&icon).is_some_and(|p| p == path) {
        return true;
    }
    let mut names: HashSet<String> = [icon_name(Some(&icon)), icon_name(notification.get("desktop_entry")),
                                      text(notification.get("app_name"), 240).trim().to_lowercase()].into_iter().collect();
    names.remove("");
    let path_text = path.to_string_lossy();
    ICON_DIRECTORIES.iter().any(|part| path_text.contains(part))
        || names.iter().map(|n| n.to_lowercase()).any(|n| n == stem(&path).to_lowercase())
}

/// Whether an icon-like image hint names the application icon itself.
pub fn same_icon(value: Option<&Value>, notification: &Map<String, Value>, app_icon: &IconValue) -> bool {
    let mut names: HashSet<String> = [icon_name(notification.get("icon")), icon_name(notification.get("desktop_entry")),
                                      text(notification.get("app_name"), 240).trim().to_lowercase()].into_iter().collect();
    if let IconValue::Icon(icon) = app_icon {
        if let Some(themed) = icon.downcast_ref::<gio::ThemedIcon>() {
            names.extend(themed.names().iter().map(|n| n.to_string()));
        }
    }
    let names: HashSet<String> = names.into_iter().filter(|n| !n.is_empty()).map(|n| n.to_lowercase()).collect();
    let name = icon_name(value);
    if !name.is_empty() {
        return names.contains(&name.to_lowercase());
    }
    let path = value.and_then(local_path);
    let icon_path = notification.get("icon").and_then(local_path);
    path.is_some_and(|p| Some(&p) == icon_path.as_ref() || names.contains(&stem(&p).to_lowercase()))
}

/// Show a paintable, GIcon or the fallback icon name on a `gtk::Image`.
pub fn set_image(widget: &gtk::Image, value: Option<&IconValue>) {
    match value {
        Some(IconValue::Paintable(paintable)) => widget.set_paintable(Some(paintable)),
        Some(IconValue::Icon(icon)) => widget.set_from_gicon(icon),
        None => widget.set_icon_name(Some(FALLBACK_ICON)),
    }
}

/// An icon-like image hint as something a `gtk::Image` can show.
pub fn icon_value(value: Option<&Value>) -> Option<IconValue> {
    let name = icon_name(value);
    if !name.is_empty() {
        return theme_has(&name).then(|| IconValue::Icon(gio::ThemedIcon::new(&name).upcast()));
    }
    value.and_then(file_texture).map(|t| IconValue::Paintable(t.upcast()))
}

/// Timed Do Not Disturb buttons; `callback` receives a settings patch.
pub fn dnd_options(callback: Rc<dyn Fn(Value)>) -> (gtk::Box, HashMap<&'static str, gtk::Button>) {
    let row = gtk::Box::builder().spacing(6).homogeneous(true).build();
    row.add_css_class("dnd-options");
    let mut buttons = HashMap::new();
    for (choice, title) in DND_CHOICES {
        let button = gtk::Button::builder().hexpand(true).build();
        button.add_css_class("popup-chip");
        // Wrap long translations onto a second line instead of truncating.
        let label = gtk::Label::builder().label(tr!(title)).wrap(true).justify(gtk::Justification::Center).build();
        label.set_wrap_mode(pango::WrapMode::WordChar);
        label.set_max_width_chars(12);
        button.set_child(Some(&label));
        button.set_tooltip_text(Some(&tr!(title)));
        let callback = callback.clone();
        button.connect_clicked(move |_| {
            let until = dnd_deadline(choice, None).unwrap_or(0.0);
            let until = if choice == "off" { json!(0) } else { json!(until) };
            callback(json!({"do_not_disturb": true, "dnd_until": until}));
        });
        row.append(&button);
        buttons.insert(choice, button);
    }
    (row, buttons)
}

fn local_time(timestamp: f64) -> Option<glib::DateTime> {
    if !timestamp.is_finite() {
        return None;
    }
    glib::DateTime::from_unix_local(timestamp.floor() as i64).ok()
}

fn is_today(date: &glib::DateTime) -> bool {
    glib::DateTime::now_local().is_ok_and(|now| now.ymd() == date.ymd())
}

/// Human end time for an active timed Do Not Disturb, else empty.
pub fn dnd_until_text(settings: &Map<String, Value>) -> String {
    let until = settings.get("dnd_until");
    let dnd = settings.get("do_not_disturb").is_some_and(tessel_core::pyjson::truthy);
    let Some(until) = until.and_then(Value::as_f64).filter(|u| *u != 0.0) else { return String::new() };
    if !dnd {
        return String::new();
    }
    let Some(end) = local_time(until) else { return String::new() };
    let format = if is_today(&end) { "%H:%M" } else { "%a %H:%M" };
    end.format(format).map(|s| s.to_string()).unwrap_or_default()
}

pub fn time_label(timestamp: Option<&Value>) -> String {
    let Some(date) = timestamp.and_then(tessel_core::pyjson::py_float).and_then(local_time) else { return String::new() };
    let format = if is_today(&date) { "%H:%M" } else { "%b %d · %H:%M" };
    date.format(format).map(|s| s.to_string()).unwrap_or_default()
}

pub struct CardInner {
    pub root: gtk::Box,
    pub history: bool,
    dismiss: Dismiss,
    activate: Activate,
    hover: Option<Hover>,
    hovered: Cell<bool>,
    notification: RefCell<Map<String, Value>>,
    image_value: RefCell<Option<Value>>,
    image_icon: RefCell<Option<IconValue>>,
    action_signature: RefCell<Option<Vec<(String, String)>>>,
    pub action_buttons: RefCell<HashMap<String, gtk::Button>>,
    pub icon: gtk::Image,
    pub app_label: gtk::Label,
    pub unread_dot: gtk::Image,
    pub time_label: gtk::Label,
    pub close_button: gtk::Button,
    pub avatar: gtk::Image,
    pub summary_label: gtk::Label,
    pub body_label: gtk::Label,
    pub picture: gtk::Picture,
    pub progress: gtk::ProgressBar,
    pub action_box: gtk::FlowBox,
}

#[derive(Clone)]
pub struct NotificationCard(pub Rc<CardInner>);

impl NotificationCard {
    pub fn new(notification: &Value, dismiss: Dismiss, activate: Activate, history: bool, hover: Option<Hover>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("notification-card");
        root.add_css_class(if history { "card" } else { "popup-surface" });
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = image(FALLBACK_ICON, 20);
        top.append(&icon);
        let app_label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        app_label.set_ellipsize(pango::EllipsizeMode::End);
        app_label.set_max_width_chars(24);
        app_label.add_css_class("notification-app");
        top.append(&app_label);
        let unread_dot = image("media-record-symbolic", 7);
        unread_dot.add_css_class("notification-unread");
        top.append(&unread_dot);
        let time_label = gtk::Label::new(None);
        time_label.add_css_class("muted");
        time_label.add_css_class("notification-time");
        top.append(&time_label);
        let close_slot: Rc<RefCell<Weak<CardInner>>> = Rc::new(RefCell::new(Weak::new()));
        let slot = close_slot.clone();
        let close_button = icon_button("window-close-symbolic", "Dismiss notification", move || {
            if let Some(inner) = slot.borrow().upgrade() {
                NotificationCard(inner).close();
            }
        });
        top.append(&close_button);
        root.append(&top);
        let message = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let avatar = gtk::Image::builder().pixel_size(40).valign(gtk::Align::Start).visible(false).build();
        avatar.add_css_class("notification-avatar");
        message.append(&avatar);
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
        message.append(&texts);
        root.append(&message);
        let summary_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
        summary_label.set_wrap_mode(pango::WrapMode::WordChar);
        summary_label.set_max_width_chars(38);
        summary_label.set_lines(if history { 3 } else { 2 });
        summary_label.set_ellipsize(pango::EllipsizeMode::End);
        summary_label.add_css_class("notification-summary");
        texts.append(&summary_label);
        let body_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
        body_label.set_wrap_mode(pango::WrapMode::WordChar);
        body_label.set_max_width_chars(40);
        body_label.set_lines(if history { 12 } else { 4 });
        body_label.set_ellipsize(pango::EllipsizeMode::End);
        body_label.set_selectable(history);
        texts.append(&body_label);
        let picture = gtk::Picture::builder().can_shrink(true).height_request(100).hexpand(true).build();
        picture.set_content_fit(gtk::ContentFit::Contain);
        root.append(&picture);
        let progress = gtk::ProgressBar::builder().show_text(true).build();
        root.append(&progress);
        let action_box = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).build();
        action_box.set_min_children_per_line(1);
        action_box.set_max_children_per_line(3);
        action_box.set_column_spacing(6);
        action_box.set_row_spacing(6);
        // Buttons sit at the card's right edge, at their natural width.
        action_box.set_halign(gtk::Align::End);
        root.append(&action_box);
        let card = Self(Rc::new(CardInner {
            root: root.clone(), history, dismiss, activate, hover, hovered: Cell::new(false),
            notification: RefCell::new(Map::new()), image_value: RefCell::new(None), image_icon: RefCell::new(None),
            action_signature: RefCell::new(None), action_buttons: RefCell::new(HashMap::new()), icon, app_label,
            unread_dot, time_label, close_button, avatar, summary_label, body_label, picture, progress, action_box,
        }));
        close_slot.replace(Rc::downgrade(&card.0));
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&card.0);
        motion.connect_enter(move |_, _, _| if let Some(inner) = weak.upgrade() { NotificationCard(inner).enter() });
        let weak = Rc::downgrade(&card.0);
        motion.connect_leave(move |_| if let Some(inner) = weak.upgrade() { NotificationCard(inner).leave() });
        root.add_controller(motion);
        card.update(notification);
        card
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.root }

    pub fn notification_id(&self) -> u32 {
        self.0.notification.borrow().get("id").and_then(Value::as_u64).unwrap_or(0) as u32
    }

    pub fn notification(&self) -> Map<String, Value> { self.0.notification.borrow().clone() }

    pub fn enter(&self) {
        if !self.0.hovered.replace(true) {
            if let Some(hover) = &self.0.hover {
                hover(self.notification_id(), true);
            }
        }
    }

    pub fn leave(&self) {
        if self.0.hovered.replace(false) {
            if let Some(hover) = &self.0.hover {
                hover(self.notification_id(), false);
            }
        }
    }

    fn active(&self) -> bool {
        self.0.notification.borrow().get("active").map_or(true, tessel_core::pyjson::truthy)
    }

    fn close(&self) {
        if self.0.history || self.active() {
            (self.0.dismiss)(self.notification_id());
        }
    }

    fn action(&self, key: &str) {
        if self.active() {
            (self.0.activate)(self.notification_id(), key);
        }
    }

    pub fn update(&self, notification: &Value) {
        let map = notification.as_object().cloned().unwrap_or_default();
        self.0.notification.replace(map.clone());
        let inner = &self.0;
        let active = map.get("active").map_or(true, tessel_core::pyjson::truthy);
        let app = text(map.get("app_name"), 240);
        inner.app_label.set_text(&if app.is_empty() { tr!("Notification") } else { app });
        inner.time_label.set_text(&time_label(map.get("timestamp")));
        inner.time_label.set_visible(inner.history);
        inner.unread_dot.set_visible(inner.history && !map.get("read").is_some_and(tessel_core::pyjson::truthy));
        inner.close_button.set_visible(inner.history || active);
        inner.summary_label.set_text(&text(map.get("summary"), 16000));
        inner.summary_label.set_visible(!inner.summary_label.text().is_empty());
        inner.body_label.set_text(&text(map.get("body"), 16000));
        inner.body_label.set_visible(!inner.body_label.text().is_empty());
        let app_icon = app_icon(&map);
        let value = map.get("image").filter(|v| tessel_core::pyjson::truthy(v)).or_else(|| map.get("image_path")).cloned();
        if *inner.image_value.borrow() != value {
            inner.image_value.replace(value.clone());
            let icon_like = image_is_icon(value.as_ref(), &map);
            // Icon names and app-icon files are not content: they belong in
            // the header (or a small avatar), never the full-width picture.
            inner.image_icon.replace(if icon_like { icon_value(value.as_ref()) } else { None });
            let texture = if icon_like { None } else { value.as_ref().and_then(notification_texture) };
            inner.picture.set_paintable(texture.as_ref());
        }
        let image_icon = inner.image_icon.borrow().clone();
        set_image(&inner.icon, app_icon.as_ref().or(image_icon.as_ref()));
        // A different icon (a contact's avatar, say) sits beside the text.
        let show_avatar = match (&image_icon, &app_icon) {
            (Some(_), Some(app)) => !same_icon(value.as_ref(), &map, app),
            _ => false,
        };
        if show_avatar {
            set_image(&inner.avatar, image_icon.as_ref());
        }
        inner.avatar.set_visible(show_avatar);
        inner.picture.set_visible(inner.picture.paintable().is_some());
        let progress = map.get("progress").and_then(|p| match p {
            Value::Number(n) => n.as_f64(),
            _ => None,
        });
        inner.progress.set_visible(progress.is_some());
        if let Some(progress) = progress {
            let percent = progress.clamp(0.0, 100.0);
            inner.progress.set_fraction(percent / 100.0);
            inner.progress.set_text(Some(&format!("{percent:.0}%")));
        }
        if map.get("urgency").and_then(Value::as_f64) == Some(2.0) {
            inner.root.add_css_class("critical");
        } else {
            inner.root.remove_css_class("critical");
        }
        let mut actions = Vec::new();
        if let Some(Value::Array(pairs)) = map.get("actions") {
            for pair in pairs {
                if let Some([key, title]) = pair.as_array().map(Vec::as_slice) {
                    let key = match key { Value::String(k) => k.clone(), other => tessel_core::pyjson::py_str(other) };
                    let title = text(Some(title), 160);
                    actions.push((key, if title.is_empty() { tr!("Open") } else { title }));
                }
            }
        }
        if inner.action_signature.borrow().as_ref() != Some(&actions) {
            inner.action_signature.replace(Some(actions.clone()));
            for child in children(&inner.action_box) {
                inner.action_box.remove(&child);
            }
            inner.action_buttons.borrow_mut().clear();
            for (key, title) in &actions {
                let button = gtk::Button::builder().label(title).build();
                if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                    label.set_ellipsize(pango::EllipsizeMode::End);
                    label.set_max_width_chars(18);
                }
                button.set_tooltip_text(Some(title));
                if key == "default" {
                    button.add_css_class("primary");
                }
                let weak = Rc::downgrade(&self.0);
                let action = key.clone();
                button.connect_clicked(move |_| if let Some(inner) = weak.upgrade() { NotificationCard(inner).action(&action) });
                inner.action_box.append(&button);
                inner.action_buttons.borrow_mut().insert(key.clone(), button);
            }
        }
        inner.action_box.set_visible(!actions.is_empty() && active);
        inner.action_box.set_sensitive(active);
    }
}

/// One application's history cards under a collapsible header.
pub struct GroupInner {
    pub root: gtk::Box,
    pub key: String,
    pub toggle: gtk::Button,
    pub arrow: gtk::Image,
    pub icon: gtk::Image,
    pub name_label: gtk::Label,
    pub count_label: gtk::Label,
    pub clear_button: gtk::Button,
    pub cards: gtk::Box,
    pub revealer: gtk::Revealer,
}

#[derive(Clone)]
pub struct NotificationGroup(pub Rc<GroupInner>);

impl NotificationGroup {
    pub fn new(key: &str, toggled: Rc<dyn Fn(&str)>, clear: Rc<dyn Fn(&str)>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("notification-group");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("group-header");
        let toggle = gtk::Button::builder().hexpand(true).build();
        toggle.add_css_class("flat");
        toggle.add_css_class("group-toggle");
        let inner_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let arrow = image("pan-down-symbolic", 16);
        inner_box.append(&arrow);
        let icon = image("dialog-information-symbolic", 18);
        inner_box.append(&icon);
        let name_label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        name_label.set_ellipsize(pango::EllipsizeMode::End);
        name_label.add_css_class("section-title");
        inner_box.append(&name_label);
        let count_label = gtk::Label::new(None);
        count_label.add_css_class("count-badge");
        count_label.add_css_class("muted");
        inner_box.append(&count_label);
        toggle.set_child(Some(&inner_box));
        let owned = key.to_owned();
        toggle.connect_clicked(move |_| toggled(&owned));
        header.append(&toggle);
        let owned = key.to_owned();
        let clear_button = icon_button("edit-clear-all-symbolic", "Clear notifications from this application", move || clear(&owned));
        header.append(&clear_button);
        root.append(&header);
        let cards = gtk::Box::new(gtk::Orientation::Vertical, 10);
        cards.add_css_class("group-cards");
        let revealer = gtk::Revealer::builder().child(&cards).reveal_child(true).build();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        root.append(&revealer);
        Self(Rc::new(GroupInner { root, key: key.into(), toggle, arrow, icon, name_label, count_label, clear_button, cards, revealer }))
    }

    pub fn update(&self, group: &AppGroup, collapsed: bool) {
        let inner = &self.0;
        let name = text(Some(&json!(group.name)), 240);
        let name = if name.is_empty() { tr!("Notification") } else { name };
        let count = group.items.len();
        inner.name_label.set_text(&name);
        inner.count_label.set_text(&count.to_string());
        let mut sample = group.items.first().and_then(Value::as_object).cloned().unwrap_or_default();
        if !sample.get("icon").is_some_and(tessel_core::pyjson::truthy) {
            sample.insert("icon".into(), group.icon.clone());
        }
        sample.entry("app_name").or_insert_with(|| json!(group.name));
        let mut icon = app_icon(&sample);
        if icon.is_none() {
            let value = sample.get("image_path").cloned();
            icon = if image_is_icon(value.as_ref(), &sample) { icon_value(value.as_ref()) } else { None };
        }
        set_image(&inner.icon, icon.as_ref());
        inner.revealer.set_reveal_child(!collapsed);
        inner.arrow.set_icon_name(Some(if collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" }));
        let label = tr!("{name}, {count} notifications", name = name.as_str(), count = count);
        inner.toggle.set_tooltip_text(Some(&if collapsed { tr!("Expand") } else { tr!("Collapse") }));
        accessible(&inner.toggle, &label);
        let clear = tr!("Clear notifications from {name}", name = name.as_str());
        accessible(&inner.clear_button, &clear);
        inner.clear_button.set_tooltip_text(Some(&clear));
    }
}

/// At most three passive cards; its layer never covers native fullscreen.
pub struct ToastInner {
    pub window: gtk::ApplicationWindow,
    css: gtk::CssProvider,
    destroyed: Cell<bool>,
    pub monitor: Option<gdk::Monitor>,
    pub workspace: bool,
    placement_ready: Cell<bool>,
    wants_visible: Cell<bool>,
    dismiss: Dismiss,
    activate: Activate,
    hover: Option<Hover>,
    pub cards: RefCell<indexmap::IndexMap<u32, NotificationCard>>,
    pub container: gtk::Box,
    pub theme: WindowTheme,
}

#[derive(Clone)]
pub struct ToastStack(pub Rc<ToastInner>);

impl ToastStack {
    pub fn new(application: &gtk::Application, monitor: Option<&gdk::Monitor>, dismiss: Dismiss, activate: Activate,
               hover: Option<Hover>, workspace: bool) -> Self {
        let window = gtk::ApplicationWindow::builder().application(application).decorated(false).resizable(false).build();
        window.set_title(Some(&tr!("Notifications")));
        window.add_css_class("tessel-notifications");
        window.add_css_class("tray-popup");
        let display = WidgetExt::display(&window);
        let css = add_css(STYLE, Some(&display), PRIORITY_APPLICATION);
        install_popup_css(Some(&display));
        let container = gtk::Box::new(gtk::Orientation::Vertical, 10);
        container.set_valign(gtk::Align::Start);
        let geometry = monitor.map(|m| m.geometry());
        let width = geometry.map_or(380, |g| 380.min(240.max(g.width() - 24)));
        window.set_default_size(width, -1);
        window.set_size_request(width, -1);
        let scroller = gtk::ScrolledWindow::builder().propagate_natural_height(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_max_content_height(geometry.map_or(680, |g| 120.max(g.height() - 24)));
        scroller.set_child(Some(&container));
        window.set_child(Some(&scroller));
        let supported = gtk4_layer_shell::is_supported();
        if supported {
            window.init_layer_shell();
            window.set_namespace(Some("tessel-dock-notifications"));
            window.set_layer(Layer::Top);
            // Zero reserves nothing while honoring a bar's existing workarea.
            window.set_exclusive_zone(0);
            window.set_keyboard_mode(KeyboardMode::None);
            if let Some(monitor) = monitor {
                window.set_monitor(Some(monitor));
            }
            for edge in [Edge::Top, Edge::Right] {
                window.set_anchor(edge, true);
                window.set_margin(edge, 12);
            }
        }
        let theme = theme::attach(&window, None);
        let stack = Self(Rc::new(ToastInner {
            window, css, destroyed: Cell::new(false), monitor: monitor.cloned(), workspace,
            placement_ready: Cell::new(!supported), wants_visible: Cell::new(false), dismiss, activate, hover,
            cards: RefCell::new(indexmap::IndexMap::new()), container, theme,
        }));
        stack.position();
        stack
    }

    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }

    pub fn card_ids(&self) -> Vec<u32> { self.0.cards.borrow().keys().copied().collect() }

    pub fn card(&self, ident: u32) -> Option<NotificationCard> { self.0.cards.borrow().get(&ident).cloned() }

    pub fn position(&self) {
        if !gtk4_layer_shell::is_supported() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        tessel_gtk::placement::dock_placement(self.0.monitor.as_ref(), "service_apps", move |value| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.destroyed.get() {
                return;
            }
            // Position against the physical output so the panel offset is
            // counted once, including floating gaps and dock-mode overlays.
            if let Some(value) = value {
                inner.window.set_exclusive_zone(-1);
                let mut align = value.align.clone();
                if inner.workspace {
                    align = if matches!(value.edge.as_str(), "top" | "left") { "end".into() } else { "start".into() };
                }
                tessel_gtk::placement::anchor_popup(&inner.window, &value.edge, value.offset, &align);
            }
            inner.placement_ready.set(true);
            inner.window.set_visible(inner.wants_visible.get());
        });
    }

    pub fn update(&self, notifications: &[Value]) {
        if self.0.destroyed.get() {
            return;
        }
        let notifications: Vec<&Value> = notifications.iter().take(3).collect();
        let ids: Vec<u32> = notifications.iter().filter_map(|e| e.get("id").and_then(Value::as_u64)).map(|i| i as u32).collect();
        let removed: Vec<NotificationCard> = {
            let mut cards = self.0.cards.borrow_mut();
            let gone: Vec<u32> = cards.keys().filter(|id| !ids.contains(id)).copied().collect();
            gone.iter().filter_map(|id| cards.shift_remove(id)).collect()
        };
        for card in removed {
            card.leave();
            self.0.container.remove(card.widget());
        }
        let mut previous: Option<gtk::Widget> = None;
        for entry in notifications {
            let ident = entry.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
            let existing = self.0.cards.borrow().get(&ident).cloned();
            let card = match existing {
                Some(card) => {
                    card.update(entry);
                    card
                }
                None => {
                    let card = NotificationCard::new(entry, self.0.dismiss.clone(), self.0.activate.clone(), false, self.0.hover.clone());
                    self.0.cards.borrow_mut().insert(ident, card.clone());
                    self.0.container.append(card.widget());
                    card
                }
            };
            self.0.container.reorder_child_after(card.widget(), previous.as_ref());
            previous = Some(card.widget().clone().upcast());
        }
        // set_visible, not present: do not generate an activation request.
        self.0.wants_visible.set(!ids.is_empty());
        self.0.window.set_visible(self.0.wants_visible.get() && self.0.placement_ready.get());
    }

    pub fn hide(&self) {
        self.0.wants_visible.set(false);
        let cards: Vec<NotificationCard> = self.0.cards.borrow().values().cloned().collect();
        for card in cards {
            card.leave();
        }
        self.0.window.set_visible(false);
    }

    pub fn destroy(&self) {
        if self.0.destroyed.replace(true) {
            return;
        }
        self.hide();
        self.0.theme.close();
        remove_css(&WidgetExt::display(&self.0.window), &self.0.css);
        self.0.window.destroy();
        self.0.cards.borrow_mut().clear();
    }

    pub fn is_destroyed(&self) -> bool { self.0.destroyed.get() }
}

pub struct CenterInner {
    client: Rc<dyn Client>,
    syncing: Cell<bool>,
    destroyed: Cell<bool>,
    pub cards: RefCell<HashMap<u32, NotificationCard>>,
    pub groups: RefCell<indexmap::IndexMap<String, NotificationGroup>>,
    pub collapsed: RefCell<HashSet<String>>,
    pub window: gtk::ApplicationWindow,
    css: gtk::CssProvider,
    pub theme: WindowTheme,
    pub count_label: gtk::Label,
    pub settings_button: gtk::Button,
    pub header_tile: gtk::Box,
    pub header_icon: gtk::Image,
    pub dnd_switch: gtk::Switch,
    pub dnd_options: gtk::Box,
    pub dnd_buttons: HashMap<&'static str, gtk::Button>,
    pub status_label: gtk::Label,
    pub listing: gtk::Box,
    pub stack: gtk::Stack,
    pub clear_button: gtk::Button,
    pub controller: RefCell<Option<PopupController>>,
}

#[derive(Clone)]
pub struct NotificationCenter(pub Rc<CenterInner>);

impl NotificationCenter {
    pub fn new(application: &gtk::Application, client: Rc<dyn Client>, edge: &str, offset: i32) -> Self {
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Notifications")).build();
        window.set_default_size(set_popup_width(window.upcast_ref(), tessel_gtk::popup::POPUP_WIDTH), 550);
        window.add_css_class("tessel-notifications");
        let css = add_css(STYLE, Some(&WidgetExt::display(&window)), PRIORITY_APPLICATION);
        let theme = theme::attach(&window, None);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("popup-surface");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        set_margins(&content, POPUP_PADDING);
        root.append(&content);
        window.set_child(Some(&root));
        let count_label = gtk::Label::builder().valign(gtk::Align::Center).build();
        count_label.add_css_class("count-badge");
        let slot: Rc<RefCell<Weak<CenterInner>>> = Rc::new(RefCell::new(Weak::new()));
        let settings_slot = slot.clone();
        let settings_button = header_button("emblem-system-symbolic", &tr!("Notification Settings"), Some(Box::new(move || {
            if let Some(inner) = settings_slot.borrow().upgrade() {
                NotificationCenter(inner).open_settings();
            }
        })), false);
        let title_row = popup_header(&["notifications-symbolic", "preferences-system-notifications-symbolic",
                                       "notifications-applet-symbolic", "dialog-information-symbolic"],
                                     &tr!("Notifications"), None,
                                     vec![HeaderAction::Widget(count_label.clone().upcast()),
                                          HeaderAction::Widget(settings_button.clone().upcast())]);
        content.append(&title_row.root);
        let dnd_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        dnd_row.append(&image("notifications-disabled-symbolic", 16));
        let dnd_title = gtk::Label::builder().label(tr!("Do Not Disturb")).xalign(0.0).hexpand(true).build();
        dnd_title.add_css_class("notification-center-title");
        dnd_row.append(&dnd_title);
        let dnd_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        accessible(&dnd_switch, &tr!("Do Not Disturb"));
        dnd_row.append(&dnd_switch);
        let dnd_card = gtk::Box::new(gtk::Orientation::Vertical, 10);
        dnd_card.add_css_class("dnd-card");
        dnd_card.append(&dnd_row);
        content.append(&dnd_card);
        let patch_slot = slot.clone();
        let (dnd_options, dnd_buttons) = dnd_options(Rc::new(move |patch| {
            if let Some(inner) = patch_slot.borrow().upgrade() {
                if !inner.syncing.get() && !inner.destroyed.get() {
                    inner.client.set_settings(patch);
                }
            }
        }));
        dnd_card.append(&dnd_options);
        let status_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
        status_label.set_max_width_chars(42);
        status_label.add_css_class("muted");
        content.append(&status_label);
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).min_content_height(180).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let listing = gtk::Box::new(gtk::Orientation::Vertical, 10);
        listing.set_valign(gtk::Align::Start);
        scroller.set_child(Some(&listing));
        let empty = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).valign(gtk::Align::Center).vexpand(true).build();
        let icon = gtk::Image::from_gicon(&gio::ThemedIcon::from_names(&["notifications-symbolic", "preferences-system-notifications-symbolic"]));
        icon.set_pixel_size(44);
        icon.add_css_class("muted");
        empty.append(&icon);
        let label = gtk::Label::builder().label(tr!("You're all caught up")).wrap(true).build();
        label.add_css_class("section-title");
        empty.append(&label);
        let label = gtk::Label::builder().label(tr!("Your notifications will appear here.")).wrap(true).build();
        label.add_css_class("muted");
        empty.append(&label);
        let stack = gtk::Stack::builder().vexpand(true).build();
        stack.add_named(&scroller, Some("history"));
        stack.add_named(&empty, Some("empty"));
        content.append(&stack);
        let clear_button = gtk::Button::with_label(&tr!("Clear All"));
        content.append(&popup_footer(&[Some(clear_button.clone().upcast())]));
        let center = Self(Rc::new(CenterInner {
            client, syncing: Cell::new(false), destroyed: Cell::new(false), cards: RefCell::new(HashMap::new()),
            groups: RefCell::new(indexmap::IndexMap::new()), collapsed: RefCell::new(HashSet::new()), window: window.clone(), css,
            theme, count_label, settings_button, header_tile: title_row.tile.clone(), header_icon: title_row.icon.clone(),
            dnd_switch: dnd_switch.clone(), dnd_options, dnd_buttons, status_label, listing, stack, clear_button: clear_button.clone(),
            controller: RefCell::new(None),
        }));
        slot.replace(Rc::downgrade(&center.0));
        let weak = Rc::downgrade(&center.0);
        dnd_switch.connect_active_notify(move |switch| {
            if let Some(inner) = weak.upgrade() {
                if !inner.syncing.get() {
                    inner.client.set_settings(json!({"do_not_disturb": switch.is_active()}));
                }
            }
        });
        let weak = Rc::downgrade(&center.0);
        clear_button.connect_clicked(move |_| if let Some(inner) = weak.upgrade() { inner.client.clear_history() });
        let weak = Rc::downgrade(&center.0);
        let controller = PopupController::new(window.upcast_ref(), move || {
            if let Some(inner) = weak.upgrade() {
                NotificationCenter(inner).refresh();
            }
        }, edge, offset, "service_apps");
        center.0.controller.replace(Some(controller));
        center.refresh();
        center
    }

    pub fn window(&self) -> &gtk::ApplicationWindow { &self.0.window }

    pub fn controller(&self) -> Option<PopupController> { self.0.controller.borrow().clone() }

    fn toggle_group(&self, key: &str) {
        {
            let mut collapsed = self.0.collapsed.borrow_mut();
            if !collapsed.remove(key) {
                collapsed.insert(key.to_owned());
            }
        }
        self.refresh();
    }

    fn clear_group(&self, key: &str) {
        self.0.client.clear_app(key);
    }

    fn open_settings(&self) {
        if let Some(controller) = self.controller() {
            controller.dismiss();
        }
        self.0.client.show_settings();
    }

    pub fn refresh(&self) {
        let inner = &self.0;
        if inner.destroyed.get() {
            return;
        }
        let client = inner.client.clone();
        let state = client.state();
        let ready = client.ready();
        let settings = state.get("settings").and_then(Value::as_object).cloned().unwrap_or_default();
        inner.syncing.set(true);
        inner.dnd_switch.set_active(settings.get("do_not_disturb").is_some_and(tessel_core::pyjson::truthy));
        inner.syncing.set(false);
        inner.dnd_switch.set_sensitive(ready);
        inner.dnd_options.set_sensitive(ready);
        let unread = state.get("unread").cloned().unwrap_or(json!(0));
        inner.count_label.set_text(&tessel_core::pyjson::py_str(&unread));
        inner.count_label.set_visible(tessel_core::pyjson::truthy(&unread));
        let error = client.error();
        let until = dnd_until_text(&settings);
        let status = if !error.is_empty() {
            error.clone()
        } else if !ready {
            tr!("Connecting to the notification service…")
        } else if !until.is_empty() {
            tr!("Popups are paused until {time}. Notifications are kept here.", time = until.as_str())
        } else if settings.get("do_not_disturb").is_some_and(tessel_core::pyjson::truthy) {
            tr!("Popups are paused. Notifications are kept here.")
        } else {
            String::new()
        };
        inner.status_label.set_text(&status);
        inner.status_label.set_visible(!status.is_empty());
        if !error.is_empty() { inner.status_label.add_css_class("error") } else { inner.status_label.remove_css_class("error") }
        let entries: Vec<Value> = state.get("notifications").and_then(Value::as_array).cloned().unwrap_or_default();
        let groups = app_groups(&entries);
        let ids: HashSet<u32> = entries.iter().filter_map(|e| e.get("id").and_then(Value::as_u64)).map(|i| i as u32).collect();
        let gone: Vec<NotificationCard> = {
            let mut cards = inner.cards.borrow_mut();
            let stale: Vec<u32> = cards.keys().filter(|id| !ids.contains(id)).copied().collect();
            stale.iter().filter_map(|id| cards.remove(id)).collect()
        };
        for card in gone {
            if let Some(parent) = card.widget().parent().and_downcast::<gtk::Box>() {
                parent.remove(card.widget());
            }
        }
        let keys: HashSet<String> = groups.iter().map(|g| g.key.clone()).collect();
        let stale_groups: Vec<NotificationGroup> = {
            let mut known = inner.groups.borrow_mut();
            let stale: Vec<String> = known.keys().filter(|k| !keys.contains(*k)).cloned().collect();
            stale.iter().filter_map(|k| known.shift_remove(k)).collect()
        };
        for group in stale_groups {
            inner.listing.remove(&group.0.root);
        }
        inner.collapsed.borrow_mut().retain(|k| keys.contains(k));
        let mut previous_group: Option<gtk::Widget> = None;
        for group in &groups {
            let existing = inner.groups.borrow().get(&group.key).cloned();
            let widget = match existing {
                Some(widget) => widget,
                None => {
                    let (toggle, clear) = (Rc::downgrade(&self.0), Rc::downgrade(&self.0));
                    let widget = NotificationGroup::new(&group.key,
                        Rc::new(move |key| if let Some(inner) = toggle.upgrade() { NotificationCenter(inner).toggle_group(key) }),
                        Rc::new(move |key| if let Some(inner) = clear.upgrade() { NotificationCenter(inner).clear_group(key) }));
                    inner.groups.borrow_mut().insert(group.key.clone(), widget.clone());
                    inner.listing.append(&widget.0.root);
                    widget
                }
            };
            let collapsed = inner.collapsed.borrow().contains(&group.key);
            widget.update(group, collapsed);
            widget.0.clear_button.set_sensitive(ready);
            inner.listing.reorder_child_after(&widget.0.root, previous_group.as_ref());
            previous_group = Some(widget.0.root.clone().upcast());
            let mut previous: Option<gtk::Widget> = None;
            for entry in &group.items {
                let ident = entry.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
                let existing = inner.cards.borrow().get(&ident).cloned();
                let card = match existing {
                    Some(card) => {
                        card.update(entry);
                        card
                    }
                    None => {
                        let (dismiss, activate) = (client.clone(), client.clone());
                        let card = NotificationCard::new(entry, Rc::new(move |id| dismiss.dismiss(id)),
                                                         Rc::new(move |id, key| activate.activate(id, key)), true, None);
                        inner.cards.borrow_mut().insert(ident, card.clone());
                        card
                    }
                };
                let parent = card.widget().parent();
                if parent.as_ref() != Some(widget.0.cards.upcast_ref()) {
                    if let Some(parent) = parent.and_downcast::<gtk::Box>() {
                        parent.remove(card.widget());
                    }
                    widget.0.cards.append(card.widget());
                }
                widget.0.cards.reorder_child_after(card.widget(), previous.as_ref());
                previous = Some(card.widget().clone().upcast());
            }
        }
        inner.stack.set_visible_child_name(if entries.is_empty() { "empty" } else { "history" });
        inner.clear_button.set_sensitive(!entries.is_empty() && ready);
        inner.clear_button.set_visible(!entries.is_empty());
    }

    pub fn present(&self) {
        if let Some(controller) = self.controller() {
            controller.show();
        }
        if self.0.client.ready() {
            self.0.client.mark_read();
        }
    }

    pub fn destroy(&self) {
        if self.0.destroyed.replace(true) {
            return;
        }
        if let Some(controller) = self.0.controller.take() {
            controller.dismiss();
            controller.close();
        }
        self.0.theme.close();
        remove_css(&WidgetExt::display(&self.0.window), &self.0.css);
        self.0.window.destroy();
        self.0.cards.borrow_mut().clear();
        self.0.groups.borrow_mut().clear();
    }

    pub fn group(&self, key: &str) -> Option<NotificationGroup> { self.0.groups.borrow().get(key).cloned() }
    pub fn group_keys(&self) -> Vec<String> { self.0.groups.borrow().keys().cloned().collect() }
    pub fn card(&self, ident: u32) -> Option<NotificationCard> { self.0.cards.borrow().get(&ident).cloned() }
}

/// Reusable controls; the host connects its client's change callback.
pub struct PageInner {
    pub root: gtk::Box,
    client: Rc<dyn Client>,
    syncing: Cell<bool>,
    destroyed: Cell<bool>,
    css: gtk::CssProvider,
    pub dnd_switch: gtk::Switch,
    pub fullscreen_switch: gtk::Switch,
    pub timeout_spin: gtk::SpinButton,
    pub dnd_until_label: gtk::Label,
    pub dnd_options: gtk::Box,
    pub dnd_buttons: HashMap<&'static str, gtk::Button>,
    pub apps_box: gtk::Box,
    pub apps_empty: gtk::Label,
    pub app_switches: RefCell<indexmap::IndexMap<String, gtk::Switch>>,
    apps_signature: RefCell<Option<Vec<(String, String, Value)>>>,
    pub status_label: gtk::Label,
}

#[derive(Clone)]
pub struct SettingsPage(pub Rc<PageInner>);

fn settings_row(title: &str, description: &str) -> (gtk::Box, gtk::Box) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("settings-card");
    let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
    let label = gtk::Label::builder().label(tr!(title)).xalign(0.0).wrap(true).build();
    label.add_css_class("section-title");
    labels.append(&label);
    let label = gtk::Label::builder().label(tr!(description)).xalign(0.0).wrap(true).build();
    label.set_max_width_chars(54);
    label.add_css_class("settings-description");
    label.add_css_class("muted");
    labels.append(&label);
    row.append(&labels);
    (row, labels)
}

impl SettingsPage {
    pub fn new(client: Rc<dyn Client>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        root.add_css_class("tessel-notifications");
        let css = add_css(STYLE, Some(&WidgetExt::display(&root)), PRIORITY_APPLICATION);
        let title = gtk::Label::builder().label(tr!("Notifications")).xalign(0.0).build();
        title.add_css_class("notification-heading");
        root.append(&title);
        let subtitle = gtk::Label::builder().label(tr!("Choose when notifications appear and how long they stay."))
            .xalign(0.0).wrap(true).build();
        subtitle.add_css_class("muted");
        root.append(&subtitle);
        let switch_row = |title: &str, description: &str| {
            let (row, _) = settings_row(title, description);
            let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
            accessible(&switch, &tr!(title));
            row.append(&switch);
            root.append(&row);
            switch
        };
        let dnd_switch = switch_row("Do Not Disturb", "Keep notifications in history without showing popups.");
        let fullscreen_switch = switch_row("Pause during fullscreen",
            "Hide popups while a fullscreen or borderless fullscreen application is in focus.");
        let (row, _) = settings_row("Popup duration",
            "Default duration for notifications without a requested timeout. Critical notifications stay until dismissed.");
        let timeout_spin = gtk::SpinButton::with_range(1.0, 60.0, 1.0);
        timeout_spin.set_value(6.0);
        timeout_spin.set_valign(gtk::Align::Center);
        accessible(&timeout_spin, &tr!("Popup duration in seconds"));
        row.append(&timeout_spin);
        row.append(&gtk::Label::new(Some(&tr!("seconds"))));
        root.append(&row);
        let (row, labels) = settings_row("Do Not Disturb duration", "Pause popups for a while. Critical notifications still appear.");
        row.set_orientation(gtk::Orientation::Vertical);
        row.set_spacing(10);
        let dnd_until_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
        dnd_until_label.add_css_class("settings-description");
        dnd_until_label.add_css_class("muted");
        labels.append(&dnd_until_label);
        let slot: Rc<RefCell<Weak<PageInner>>> = Rc::new(RefCell::new(Weak::new()));
        let patch_slot = slot.clone();
        let (dnd_options, dnd_buttons) = dnd_options(Rc::new(move |patch| {
            if let Some(inner) = patch_slot.borrow().upgrade() {
                if !inner.syncing.get() && !inner.destroyed.get() {
                    inner.client.set_settings(patch);
                }
            }
        }));
        row.append(&dnd_options);
        root.append(&row);
        let apps_title = gtk::Label::builder().label(tr!("Applications")).xalign(0.0).build();
        apps_title.add_css_class("section-title");
        root.append(&apps_title);
        let apps_subtitle = gtk::Label::builder()
            .label(tr!("Mute popups from an application. Its notifications are still kept in history.")).xalign(0.0).wrap(true).build();
        apps_subtitle.add_css_class("settings-description");
        apps_subtitle.add_css_class("muted");
        root.append(&apps_subtitle);
        let apps_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.append(&apps_box);
        let apps_empty = gtk::Label::builder().label(tr!("Applications appear here after they send a notification."))
            .xalign(0.0).wrap(true).build();
        apps_empty.add_css_class("muted");
        root.append(&apps_empty);
        let status_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
        status_label.add_css_class("muted");
        root.append(&status_label);
        let page = Self(Rc::new(PageInner {
            root, client, syncing: Cell::new(false), destroyed: Cell::new(false), css, dnd_switch: dnd_switch.clone(),
            fullscreen_switch: fullscreen_switch.clone(), timeout_spin: timeout_spin.clone(), dnd_until_label, dnd_options,
            dnd_buttons, apps_box, apps_empty, app_switches: RefCell::new(indexmap::IndexMap::new()),
            apps_signature: RefCell::new(None), status_label,
        }));
        slot.replace(Rc::downgrade(&page.0));
        for (switch, key) in [(&dnd_switch, "do_not_disturb"), (&fullscreen_switch, "suppress_fullscreen")] {
            let weak = Rc::downgrade(&page.0);
            switch.connect_active_notify(move |switch| {
                if let Some(inner) = weak.upgrade() {
                    if !inner.syncing.get() && !inner.destroyed.get() {
                        let mut patch = Map::new();
                        patch.insert(key.into(), json!(switch.is_active()));
                        inner.client.set_settings(Value::Object(patch));
                    }
                }
            });
        }
        let weak = Rc::downgrade(&page.0);
        timeout_spin.connect_value_changed(move |spin| {
            if let Some(inner) = weak.upgrade() {
                if !inner.syncing.get() && !inner.destroyed.get() {
                    inner.client.set_settings(json!({"timeout_ms": spin.value_as_int() * 1000}));
                }
            }
        });
        page.refresh();
        page
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.root }

    fn mute_changed(&self, active: bool, key: &str) {
        if self.0.syncing.get() || self.0.destroyed.get() {
            return;
        }
        let state = self.0.client.state();
        let mut muted: Vec<Value> = state.get("settings").and_then(|s| s.get("muted_apps")).and_then(Value::as_array)
            .map(|list| list.iter().filter(|n| n.as_str().is_some_and(|n| n != key)).cloned().collect()).unwrap_or_default();
        if active {
            muted.push(json!(key));
        }
        self.0.client.set_settings(json!({"muted_apps": muted}));
    }

    /// Apps seen in history plus muted apps whose history is gone.
    pub fn applications(state: &Map<String, Value>) -> (Vec<(String, String, Value)>, HashSet<String>) {
        let muted: Vec<String> = state.get("settings").and_then(|s| s.get("muted_apps")).and_then(Value::as_array)
            .map(|list| list.iter().filter_map(|n| n.as_str().map(String::from)).collect()).unwrap_or_default();
        let entries: Vec<Value> = state.get("notifications").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut apps: Vec<(String, String, Value)> = app_groups(&entries).into_iter().map(|group| {
            let name = text(Some(&json!(group.name)), 240);
            let name = if name.is_empty() { group.key.clone() } else { name };
            (group.key, name, group.icon)
        }).collect();
        let seen: HashSet<String> = apps.iter().map(|a| a.0.clone()).collect();
        for key in &muted {
            if !seen.contains(key) {
                apps.push((key.clone(), key.clone(), json!("")));
            }
        }
        // Python's sorted() is stable.
        apps.sort_by_key(|app| app.1.to_lowercase());
        (apps, muted.into_iter().collect())
    }

    fn rebuild_apps(&self, apps: &[(String, String, Value)]) {
        let inner = &self.0;
        for child in children(&inner.apps_box) {
            inner.apps_box.remove(&child);
        }
        inner.app_switches.borrow_mut().clear();
        for (key, name, icon) in apps {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("settings-card");
            let icon = icon.as_str().filter(|i| !i.is_empty() && !i.contains('/') && !i.contains(':')).unwrap_or("dialog-information-symbolic");
            row.append(&image(icon, 22));
            let label = gtk::Label::builder().label(name.as_str()).xalign(0.0).hexpand(true).build();
            label.set_ellipsize(pango::EllipsizeMode::End);
            row.append(&label);
            let mute = gtk::Label::new(Some(&tr!("Mute")));
            mute.add_css_class("muted");
            row.append(&mute);
            let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
            accessible(&switch, &tr!("Mute {name}", name = name.as_str()));
            let weak = Rc::downgrade(&self.0);
            let owned = key.clone();
            switch.connect_active_notify(move |switch| {
                if let Some(inner) = weak.upgrade() {
                    SettingsPage(inner).mute_changed(switch.is_active(), &owned);
                }
            });
            row.append(&switch);
            inner.apps_box.append(&row);
            inner.app_switches.borrow_mut().insert(key.clone(), switch);
        }
    }

    pub fn refresh(&self) {
        let inner = &self.0;
        if inner.destroyed.get() {
            return;
        }
        let state = inner.client.state();
        let settings = state.get("settings").and_then(Value::as_object).cloned().unwrap_or_default();
        inner.syncing.set(true);
        inner.dnd_switch.set_active(settings.get("do_not_disturb").is_some_and(tessel_core::pyjson::truthy));
        inner.fullscreen_switch.set_active(settings.get("suppress_fullscreen").map_or(true, tessel_core::pyjson::truthy));
        inner.timeout_spin.set_value(settings.get("timeout_ms").and_then(Value::as_f64).unwrap_or(6000.0) / 1000.0);
        let until = dnd_until_text(&settings);
        inner.dnd_until_label.set_text(&if until.is_empty() { String::new() } else { tr!("Paused until {time}", time = until.as_str()) });
        inner.dnd_until_label.set_visible(!until.is_empty());
        let (apps, muted) = Self::applications(&state);
        if inner.apps_signature.borrow().as_ref() != Some(&apps) {
            inner.apps_signature.replace(Some(apps.clone()));
            self.rebuild_apps(&apps);
        }
        let switches: Vec<(String, gtk::Switch)> = inner.app_switches.borrow().iter().map(|(k, s)| (k.clone(), s.clone())).collect();
        for (key, switch) in &switches {
            switch.set_active(muted.contains(key));
        }
        inner.apps_empty.set_visible(apps.is_empty());
        inner.syncing.set(false);
        let ready = inner.client.ready();
        inner.dnd_switch.set_sensitive(ready);
        inner.fullscreen_switch.set_sensitive(ready);
        inner.timeout_spin.set_sensitive(ready);
        inner.dnd_options.set_sensitive(ready);
        for (_, switch) in switches {
            switch.set_sensitive(ready);
        }
        let error = inner.client.error();
        let status = if !error.is_empty() { error.clone() } else if ready { String::new() } else { tr!("Connecting to the notification service…") };
        inner.status_label.set_text(&status);
        inner.status_label.set_visible(!status.is_empty());
        if !error.is_empty() { inner.status_label.add_css_class("error") } else { inner.status_label.remove_css_class("error") }
    }

    pub fn destroy(&self) {
        if !self.0.destroyed.replace(true) {
            remove_css(&WidgetExt::display(&self.0.root), &self.0.css);
        }
    }
}

pub struct SettingsWindowInner {
    pub window: gtk::ApplicationWindow,
    pub page: SettingsPage,
    theme: WindowTheme,
    destroyed: Cell<bool>,
}

#[derive(Clone)]
pub struct NotificationSettings(pub Rc<SettingsWindowInner>);

impl NotificationSettings {
    pub fn new(application: &gtk::Application, client: Rc<dyn Client>) -> Self {
        let title = tr!("Notification Settings");
        let window = gtk::ApplicationWindow::builder().application(application).title(&title).build();
        window.set_default_size(650, 410);
        window.add_css_class("tessel-notifications");
        window.set_titlebar(Some(&headerbar(&title)));
        let page = SettingsPage::new(client);
        set_margins(page.widget(), 22);
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(page.widget()));
        window.set_child(Some(&scroller));
        let theme = theme::attach(&window, None);
        window.connect_close_request(|window| {
            window.set_visible(false);
            glib::Propagation::Stop
        });
        Self(Rc::new(SettingsWindowInner { window, page, theme, destroyed: Cell::new(false) }))
    }

    pub fn present(&self) {
        self.refresh();
        self.0.window.unminimize();
        self.0.window.present();
    }

    pub fn refresh(&self) { self.0.page.refresh(); }

    pub fn destroy(&self) {
        if !self.0.destroyed.replace(true) {
            self.0.page.destroy();
            self.0.theme.close();
            self.0.window.destroy();
        }
    }
}
