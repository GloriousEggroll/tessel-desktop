//! panel_ui.py: persistent workspace, tray, default-app, clock and session
//! groups for dock and bar layouts (`DockIndicators`).
//!
//! Pure API: [`INDICATORS`], [`DEFAULTS`]/[`default_glyph`], [`TITLES`]/[`title_source`],
//! [`TEXT_INDICATORS`], [`status_icon_names`] (glyph/class → themed symbolic
//! icon candidates, verbatim from panel_ui.py), [`status_icon_size`]
//! (compact `clamp(round(icon·.4), 12, 20)`, else `clamp(round(icon·.5), 14, 32)`),
//! [`tray_icon_size`] (16 compact / 22), [`status_classes`], [`status_tooltip`],
//! [`tray_info`] (TrayItem → core::tray_prefs::TrayItemInfo).
//!
//! Widget API: `DockIndicators::new(&Rc<dyn DockView>, Rc<dyn DockApp>)` builds
//! and registers the groups (`add_group`): bar mode → `weather`/`workspaces`
//! in `bar_start`; `media`, `system_tray` (TrayExpander), `service_apps`
//! (always tray + default apps), `weather` (dock mode), `clock`, `session`.
//! Methods (Python names): `refresh`, `refresh_trays(queue)`,
//! `refresh_workspaces`, `group_availability`, `sync_apps_visibility`,
//! `set_apps_expanded`, `schedule_apps_collapse`, `always_tray_item`,
//! `regular_tray_item`, `icon_size`, `tray_icon_size`, `clock_tick`, `close`,
//! and getters for every widget (`widget`, `bar_start`/`set_bar_start`,
//! `workspaces_box`, `workspace_button(id)`, `media`, `tray`, `always_tray`,
//! `tray_expander`, `apps`, `apps_toggle`, `service_area`, `clock`, labels,
//! `logout`, `showdesktop`, `button(name)`, `status_button(name)`,
//! `record_menu`, `capture_preserved_menu`, `apps_expanded`, `status_visible`).

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::{Rc, Weak};

use tessel_core::clock::{ClockPreferences, ClockPreferencesStore};
use tessel_core::pyjson::{casefold, py_str, py_str_or_empty, truthy};
use tessel_core::timer::{self, Timeout};
use tessel_core::tr;

use crate::core::config::DockConfig;
use crate::core::tray_prefs::{tray_identity, TrayItemInfo};
use crate::dbus::sni::TrayItem;

use super::host::{accessible_description, accessible_name, click, keys, motion, pack, remove, scroll, DockApp, DockView,
                  MenuEntry};
use super::media::MediaControls;
use super::tray::{StatusTray, TrayExpander, TrayOptions};

/// `INDICATORS` = DEFAULT_APPS names in order.
pub const INDICATORS: [&str; 14] = ["idle_inhibitor", "screen_record", "notifications", "power", "battery", "capslock",
    "bluetooth", "tiling", "keyboard", "network", "discovery", "audio", "brightness", "showdesktop"];

pub const DEFAULTS: [(&str, &str); 15] = [
    ("idle_inhibitor", "\u{f070}"), ("screen_record", "\u{f03d}"), ("capslock", "⇪"), ("power", "\u{f135}"),
    ("media", "\u{f001}"), ("discovery", "\u{f1e0}"), ("brightness", "\u{f108}"), ("battery", "\u{f244}"),
    ("network", "\u{f6ff}"), ("bluetooth", "\u{f294}"), ("audio", "\u{f028}"), ("notifications", "\u{f0f3}"),
    ("tiling", "▦"), ("keyboard", "\u{f11c}"), ("weather", "…"),
];

pub const TITLES: [(&str, &str); 16] = [
    ("idle_inhibitor", "Keep Awake"), ("screen_record", "Screen Record"), ("media", "Media"),
    ("discovery", "Discover Network Devices"), ("notifications", "Notifications"), ("power", "Power"),
    ("battery", "Battery"), ("bluetooth", "Bluetooth"), ("network", "Network"), ("audio", "Audio"),
    ("brightness", "Displays"), ("showdesktop", "Minimize / restore windows"), ("capslock", "Caps Lock"),
    ("tiling", "Window Tiling"), ("keyboard", "On-screen Keyboard"), ("weather", "Weather"),
];

/// Text indicators keep their label; every other status uses a themed icon.
pub const TEXT_INDICATORS: [&str; 2] = ["capslock", "weather"];
/// How long the bar's "More apps" section stays expanded after the pointer leaves.
const APPS_COLLAPSE_DELAY_MS: u32 = 5000;

/// `DEFAULTS.get(name, "")`.
pub fn default_glyph(name: &str) -> &'static str {
    DEFAULTS.iter().find(|(key, _)| *key == name).map(|(_, glyph)| *glyph).unwrap_or("\u{f111}")
}

/// Python `str.title()` (word starts upper-cased, the rest lower-cased).
fn py_title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_cased = false;
    for c in text.chars() {
        if c.is_alphabetic() {
            if previous_cased { out.extend(c.to_lowercase()) } else { out.extend(c.to_uppercase()) }
            previous_cased = true;
        } else {
            out.push(c);
            previous_cased = false;
        }
    }
    out
}

/// `TITLES.get(name, name.replace("_", " ").title())` (untranslated source).
pub fn title_source(name: &str) -> String {
    TITLES.iter().find(|(key, _)| *key == name).map(|(_, title)| title.to_string())
        .unwrap_or_else(|| py_title(&name.replace('_', " ")))
}

fn translated_title(name: &str) -> String { tr!(title_source(name)) }

fn battery_icons(glyph: &str) -> Option<&'static [&'static str]> {
    Some(match glyph {
        "\u{f244}" => &["battery-level-0-symbolic", "battery-empty-symbolic", "battery-caution-symbolic"],
        "\u{f243}" => &["battery-level-20-symbolic", "battery-low-symbolic"],
        "\u{f242}" => &["battery-level-50-symbolic", "battery-good-symbolic"],
        "\u{f241}" => &["battery-level-70-symbolic", "battery-good-symbolic"],
        "\u{f240}" => &["battery-level-100-symbolic", "battery-full-symbolic"],
        _ => return None,
    })
}

fn fixed_icons(name: &str) -> &'static [&'static str] {
    match name {
        "screen_record" => &["media-record-symbolic"],
        "brightness" => &["display-brightness-symbolic", "video-display-symbolic"],
        "media" => &["audio-x-generic-symbolic", "multimedia-player-symbolic"],
        "discovery" => &["network-workgroup-symbolic", "network-server-symbolic"],
        "keyboard" => &["input-keyboard-symbolic"],
        "tiling" => &["view-grid-symbolic", "view-app-grid-symbolic"],
        "showdesktop" => &["user-desktop-symbolic", "preferences-desktop-wallpaper-symbolic"],
        "logout" => &["system-shutdown-symbolic"],
        _ => &[],
    }
}

fn owned(names: &[&str]) -> Vec<String> { names.iter().map(|name| name.to_string()).collect() }

/// Themed symbolic icon candidates for a status, most specific first.
///
/// Indicator scripts publish icon-font glyphs; their state is read from those
/// glyphs/classes. An empty result means the glyph stays.
pub fn status_icon_names(name: &str, text: &str, classes: &BTreeSet<String>) -> Vec<String> {
    let text = text.split_whitespace().next().unwrap_or("");
    let has = |class: &str| classes.contains(class);
    let any = |list: &[&str]| list.iter().any(|class| classes.contains(*class));
    if TEXT_INDICATORS.contains(&name) {
        return Vec::new();
    }
    match name {
        "audio" => {
            let level = match text {
                "\u{f6a9}" => "muted",
                "\u{f026}" => "low",
                "\u{f027}" => "medium",
                _ => "high",
            };
            if has("muted") { owned(&["audio-volume-muted-symbolic"]) } else { vec![format!("audio-volume-{level}-symbolic")] }
        }
        "battery" => {
            if has("unavailable") {
                owned(&["battery-missing-symbolic"])
            } else if has("plugged") {
                owned(&["battery-full-charging-symbolic", "battery-good-charging-symbolic", "ac-adapter-symbolic"])
            } else {
                owned(battery_icons(text).unwrap_or(&["battery-good-symbolic"]))
            }
        }
        "network" => {
            if any(&["unavailable", "disabled"]) {
                return owned(&["network-wireless-disabled-symbolic", "network-offline-symbolic"]);
            }
            if has("disconnected") {
                return owned(&["network-offline-symbolic", "network-wireless-offline-symbolic"]);
            }
            if has("connecting") {
                return owned(&["network-wireless-acquiring-symbolic", "network-wired-acquiring-symbolic"]);
            }
            let limited = has("limited");
            if has("wifi") {
                let strength = match text {
                    "󰤟" => "weak",
                    "󰤢" => "ok",
                    "󰤥" => "good",
                    "󰤨" => "excellent",
                    _ => "good",
                };
                let mut names = if limited { owned(&["network-wireless-no-route-symbolic"]) } else { Vec::new() };
                names.push(format!("network-wireless-signal-{strength}-symbolic"));
                names.push("network-wireless-symbolic".into());
                return names;
            }
            if has("mobile") {
                return owned(&["network-cellular-signal-good-symbolic", "network-cellular-symbolic"]);
            }
            let mut names = if limited { owned(&["network-wired-no-route-symbolic"]) } else { Vec::new() };
            names.push("network-wired-symbolic".into());
            names
        }
        "bluetooth" => {
            if has("connected") {
                owned(&["bluetooth-active-symbolic", "bluetooth-symbolic"])
            } else if any(&["off", "unavailable"]) {
                owned(&["bluetooth-disabled-symbolic", "bluetooth-symbolic"])
            } else {
                owned(&["bluetooth-symbolic", "bluetooth-active-symbolic"])
            }
        }
        "power" => {
            let profile = if has("performance") { "performance" } else if has("power-saver") { "power-saver" } else { "balanced" };
            vec![format!("power-profile-{profile}-symbolic"), "power-profile-balanced-symbolic".into()]
        }
        "notifications" => {
            if any(&["dnd", "do-not-disturb", "silent", "paused", "disabled"]) {
                owned(&["notifications-disabled-symbolic", "preferences-system-notifications-symbolic"])
            } else {
                owned(&["preferences-system-notifications-symbolic", "notification-symbolic"])
            }
        }
        "idle_inhibitor" => {
            let active = text == "\u{f06e}" || has("activated");
            owned(if active { &["caffeine-cup-full-symbolic"] } else { &["caffeine-cup-empty-symbolic"] })
        }
        _ => owned(fixed_icons(name)),
    }
}

/// Python `round()` of a float (half to even).
fn py_round(value: f64) -> i64 { value.round_ties_even() as i64 }

/// Status icon pixels from the configured dock/bar icon size.
pub fn status_icon_size(config: &DockConfig) -> i32 {
    let base = if config.icon_size == 0 { 40 } else { config.icon_size };
    if config.compact_bar {
        py_round(base as f64 * 0.4).clamp(12, 20) as i32
    } else {
        py_round(base as f64 * 0.5).clamp(14, 32) as i32
    }
}

/// 16 px tray icons in the compact top bar (including title mode), else 22 px.
pub fn tray_icon_size(panel_compact: bool, config: &DockConfig, is_app_dock: bool) -> i32 {
    if panel_compact || (config.compact_bar && !is_app_dock) { 16 } else { 22 }
}

/// `set(raw if list/tuple else [raw]) - {""}` from `state.get("classes", state.get("class", ()))`.
pub fn status_classes(state: &Map<String, Value>) -> BTreeSet<String> {
    let raw = state.get("classes").or_else(|| state.get("class"));
    let values: Vec<String> = match raw {
        None => Vec::new(),
        Some(Value::Array(list)) => list.iter().map(|value| py_str(value)).collect(),
        Some(value) => vec![py_str(value)],
    };
    values.into_iter().filter(|class| !class.is_empty()).collect()
}

/// The indicator tooltip: the detail when it already starts with the title.
pub fn status_tooltip(title: &str, detail: &str) -> String {
    if casefold(detail).starts_with(&casefold(title)) {
        detail.to_owned()
    } else if detail.is_empty() {
        title.to_owned()
    } else {
        format!("{title}\n{detail}")
    }
}

/// The tray_preferences fields of a StatusNotifier item.
pub fn tray_info(item: &TrayItem) -> TrayItemInfo {
    TrayItemInfo { name: item.name.clone(), ready: item.ready(), id: item.string("Id"),
                   desktop_entry: item.string("DesktopEntry"), title: item.string("Title") }
}

fn always_tray_item(config: &DockConfig, item: &TrayItem) -> bool {
    config.always_show_tray.contains(&tray_identity(&tray_info(item)))
}

fn regular_tray_item(config: &DockConfig, item: &TrayItem) -> bool {
    !always_tray_item(config, item) && !item.is_passive()
}

// ------------------------------------------------------------ status buttons

/// One `tessel-dock-status status-<name>` button (Python's attribute-laden Gtk.Button).
pub struct StatusButton {
    pub button: gtk::Button,
    pub name: String,
    pub label: gtk::Label,
    pub icon: gtk::Image,
    /// The slot holding the themed icon, or the glyph if the theme lacks it.
    pub visual: gtk::Box,
    pub vpn_badge: Option<gtk::Image>,
    panel_classes: RefCell<BTreeSet<String>>,
}

impl StatusButton {
    pub fn panel_classes(&self) -> BTreeSet<String> { self.panel_classes.borrow().clone() }
}

fn glyph_button(text: &str, tooltip: &str, name: &str, size: i32) -> Rc<StatusButton> {
    let button = gtk::Button::new();
    button.add_css_class("tessel-dock-status");
    button.add_css_class(&format!("status-{name}"));
    let label = gtk::Label::new(Some(text));
    label.add_css_class("tessel-dock-status-glyph");
    let icon = gtk::Image::new();
    icon.add_css_class("tessel-dock-status-icon");
    let visual = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    visual.set_halign(gtk::Align::Center);
    visual.set_valign(gtk::Align::Center);
    visual.append(&icon);
    visual.append(&label);
    let mut vpn_badge = None;
    if name == "network" {
        let content = gtk::Overlay::new();
        content.set_halign(gtk::Align::Center);
        content.set_valign(gtk::Align::Center);
        content.set_size_request(size + 4, size + 4);
        content.set_child(Some(&visual));
        let badge = gtk::Image::from_icon_name("changes-prevent-symbolic");
        badge.set_pixel_size(10);
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::Start);
        badge.set_can_target(false);
        badge.set_visible(false);
        content.add_overlay(&badge);
        button.set_child(Some(&content));
        vpn_badge = Some(badge);
    } else {
        button.set_child(Some(&visual));
    }
    let status = Rc::new(StatusButton { button, name: name.into(), label, icon, visual, vpn_badge,
                                        panel_classes: RefCell::default() });
    set_status_visual(&status, text, &BTreeSet::new(), size);
    status.button.set_tooltip_text(Some(tooltip));
    accessible_name(&status.button, tooltip);
    accessible_description(&status.button, tooltip);
    status
}

/// `_set_status_visual`: themed icon if the theme has a candidate, else the glyph.
pub fn set_status_visual(status: &StatusButton, text: &str, classes: &BTreeSet<String>, size: i32) {
    let names = status_icon_names(&status.name, text, classes);
    let theme = gtk::IconTheme::for_display(&status.button.display());
    let icon = names.iter().find(|candidate| theme.has_icon(candidate));
    if let Some(icon) = icon {
        if status.icon.icon_name().as_deref() != Some(icon.as_str()) {
            status.icon.set_icon_name(Some(icon));
        }
        status.icon.set_pixel_size(size);
    } else if !TEXT_INDICATORS.contains(&status.name.as_str()) {
        // Glyph fallback scales with the configured icon size, not CSS px.
        let attributes = pango::AttrList::new();
        attributes.insert(pango::AttrSize::new_size_absolute(size * pango::SCALE));
        status.label.set_attributes(Some(&attributes));
    }
    status.icon.set_visible(icon.is_some());
    status.label.set_visible(icon.is_none());
}

// ------------------------------------------------------------ DockIndicators

struct Inner {
    dock: Weak<dyn DockView>,
    app: Rc<dyn DockApp>,
    root: gtk::Box,
    closed: Cell<bool>,
    buttons: RefCell<IndexMap<String, Rc<StatusButton>>>,
    workspace_buttons: RefCell<HashMap<String, gtk::Button>>,
    status_visibility: RefCell<HashMap<String, bool>>,
    apps_expanded: Cell<bool>,
    apps_pointer: Cell<bool>,
    apps_menu: Cell<bool>,
    apps_hide: RefCell<Option<Timeout>>,
    record_menu: RefCell<Option<gtk::Popover>>,
    capture_preserved_menu: RefCell<Option<gtk::Popover>>,
    tray_selection: RefCell<Option<Vec<String>>>,
    tray_subscription: Cell<Option<u64>>,
    bar_start: RefCell<Option<gtk::Box>>,
    workspaces: gtk::Box,
    media: RefCell<Option<MediaControls>>,
    tray: Option<StatusTray>,
    always_tray: Option<StatusTray>,
    tray_expander: RefCell<Option<TrayExpander>>,
    apps: gtk::Box,
    app_button_sizes: gtk::SizeGroup,
    apps_toggle: gtk::Button,
    apps_focus: gtk::EventControllerFocus,
    service_area: gtk::Box,
    clock: gtk::Button,
    clock_store: RefCell<ClockPreferencesStore>,
    clock_settings: RefCell<Option<ClockPreferences>>,
    clock_content: gtk::Box,
    clock_box: gtk::Box,
    weekday_label: gtk::Label,
    time_label: gtk::Label,
    date_label: gtk::Label,
    logout: RefCell<Option<Rc<StatusButton>>>,
    timer: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct DockIndicators(Rc<Inner>);

impl PartialEq for DockIndicators {
    fn eq(&self, other: &Self) -> bool { Rc::ptr_eq(&self.0, &other.0) }
}

impl DockIndicators {
    pub fn new(dock: &Rc<dyn DockView>, app: Rc<dyn DockApp>) -> Self {
        let orientation = dock.orientation();
        let config = dock.config();
        let tray_backend = app.tray_backend();
        let size = status_icon_size(&config);
        let trays = tray_backend.as_ref().map(|backend| {
            let tray_size = tray_icon_size(
                dock.panel().is_some_and(|panel| panel.has_css_class("tessel-dock-compact-bar")), &config, dock.is_app_dock());
            let make = |always: bool| {
                let (filter_dock, position_dock) = (Rc::downgrade(dock), Rc::downgrade(dock));
                StatusTray::new(backend.clone(), TrayOptions {
                    orientation, icon_size: tray_size, menu_state: None, palette_class: dock.popup_class(),
                    item_filter: Some(Rc::new(move |item: &TrayItem| filter_dock.upgrade().is_some_and(|dock| {
                        let config = dock.config();
                        if always { always_tray_item(&config, item) } else { regular_tray_item(&config, item) }
                    }))),
                    screen_position: Some(Rc::new(move |widget: &gtk::Widget, x, y| {
                        position_dock.upgrade().and_then(|dock| dock.screen_position(widget, x, y))
                    })),
                })
            };
            (make(false), make(true))
        });
        let (tray, always_tray) = match trays { Some((tray, always)) => (Some(tray), Some(always)), None => (None, None) };
        let inner = Inner {
            dock: Rc::downgrade(dock), app: app.clone(), root: gtk::Box::new(orientation, 3), closed: Cell::new(false),
            buttons: RefCell::default(), workspace_buttons: RefCell::default(), status_visibility: RefCell::default(),
            apps_expanded: Cell::new(false), apps_pointer: Cell::new(false), apps_menu: Cell::new(false),
            apps_hide: RefCell::new(None), record_menu: RefCell::new(None), capture_preserved_menu: RefCell::new(None),
            tray_selection: RefCell::new(None), tray_subscription: Cell::new(None), bar_start: RefCell::new(None),
            workspaces: gtk::Box::new(orientation, 1), media: RefCell::new(None), tray, always_tray,
            tray_expander: RefCell::new(None), apps: gtk::Box::new(orientation, 1),
            app_button_sizes: gtk::SizeGroup::new(gtk::SizeGroupMode::Both),
            apps_toggle: gtk::Button::from_icon_name("view-more-symbolic"),
            apps_focus: gtk::EventControllerFocus::new(), service_area: gtk::Box::new(orientation, 1),
            clock: gtk::Button::new(), clock_store: RefCell::new(app.clock_store()), clock_settings: RefCell::new(None),
            clock_content: gtk::Box::new(gtk::Orientation::Horizontal, 6),
            clock_box: gtk::Box::new(gtk::Orientation::Vertical, 0), weekday_label: gtk::Label::new(None),
            time_label: gtk::Label::new(None), date_label: gtk::Label::new(None), logout: RefCell::new(None),
            timer: RefCell::new(None),
        };
        let panel = Self(Rc::new(inner));
        panel.build(dock, size);
        panel
    }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }
    fn upgrade(weak: &Weak<Inner>) -> Option<Self> { weak.upgrade().map(Self) }
    fn dock(&self) -> Option<Rc<dyn DockView>> { self.0.dock.upgrade() }

    fn build(&self, dock: &Rc<dyn DockView>, size: i32) {
        let inner = &self.0;
        let config = dock.config();
        inner.root.add_css_class("tessel-dock-indicators");
        let weather = self.indicator("weather", size);
        inner.workspaces.add_css_class("tessel-dock-workspaces");
        if config.mode == "bar" {
            let start = gtk::Box::new(dock.orientation(), 3);
            start.add_css_class("tessel-dock-indicators");
            start.add_css_class("tessel-dock-bar-start");
            inner.bar_start.replace(Some(start.clone()));
            dock.add_group("weather", &start, weather.button.upcast_ref(), true);
            dock.add_group("workspaces", &start, inner.workspaces.upcast_ref(), false);
        } else {
            dock.add_group("workspaces", &inner.root, inner.workspaces.upcast_ref(), false);
        }
        let media_button = self.indicator("media", size);
        let media = MediaControls::new(dock, &inner.app, &media_button);
        dock.add_group("media", &inner.root, media.widget().upcast_ref(), true);
        inner.media.replace(Some(media));
        if let (Some(tray), Some(backend)) = (&inner.tray, inner.app.tray_backend()) {
            let weak = self.weak();
            let always = inner.always_tray.as_ref().unwrap();
            always.set_menu_state(Rc::new(move |active| {
                if let Some(panel) = Self::upgrade(&weak) { panel.always_tray_menu(active) }
            }));
            let weak = self.weak();
            tray.set_menu_state(Rc::new(move |active| {
                if let Some(panel) = Self::upgrade(&weak) { panel.tray_menu(active) }
            }));
            let expander = TrayExpander::for_tray(dock, &inner.app, tray);
            dock.add_group("system_tray", &inner.root, expander.widget().upcast_ref(), false);
            inner.tray_expander.replace(Some(expander));
            let weak = self.weak();
            let id = backend.subscribe(Rc::new(move || {
                if let Some(panel) = Self::upgrade(&weak) { panel.refresh_trays(true) }
            }));
            inner.tray_subscription.set(Some(id));
        }
        for name in INDICATORS {
            let button = self.indicator(name, size);
            inner.app_button_sizes.add_widget(&button.button);
            pack(&inner.apps, &button.button, false, false, 0);
        }
        inner.apps_toggle.add_css_class("tessel-dock-status");
        inner.apps_toggle.set_tooltip_text(Some(&tr!("More apps · Hover to expand")));
        accessible_name(&inner.apps_toggle, &tr!("Expand app icons"));
        inner.app_button_sizes.add_widget(&inner.apps_toggle);
        let weak = self.weak();
        inner.apps_toggle.connect_clicked(move |_| {
            if let Some(panel) = Self::upgrade(&weak) { panel.set_apps_expanded(!panel.apps_expanded()) }
        });
        inner.apps.append(&inner.apps_toggle);
        let (enter, leave) = (self.weak(), self.weak());
        motion(&inner.apps, move || {
            if let Some(panel) = Self::upgrade(&enter) {
                panel.0.apps_pointer.set(true);
                panel.set_apps_expanded(true);
            }
        }, move || {
            if let Some(panel) = Self::upgrade(&leave) {
                panel.0.apps_pointer.set(false);
                panel.schedule_apps_collapse();
            }
        });
        let weak = self.weak();
        inner.apps_focus.connect_enter(move |_| {
            if let Some(panel) = Self::upgrade(&weak) {
                if panel.dock().is_some_and(|dock| dock.window().gets_focus_visible()) {
                    panel.set_apps_expanded(true);
                }
            }
        });
        let weak = self.weak();
        inner.apps_focus.connect_leave(move |_| {
            if let Some(panel) = Self::upgrade(&weak) { panel.schedule_apps_collapse() }
        });
        inner.apps.add_controller(inner.apps_focus.clone());
        let weak = self.weak();
        keys(&inner.apps, move |_, key| Self::upgrade(&weak).is_some_and(|panel| panel.apps_key(key)));
        if let Some(always) = &inner.always_tray {
            inner.service_area.append(&always.widget());
        }
        inner.service_area.append(&inner.apps);
        dock.add_group("service_apps", &inner.root, inner.service_area.upcast_ref(), false);
        if inner.bar_start.borrow().is_none() {
            dock.add_group("weather", &inner.root, weather.button.upcast_ref(), true);
        }
        // Clock.
        let clock_title = tr!("Calendar and date/time settings");
        inner.clock.set_tooltip_text(Some(&clock_title));
        accessible_name(&inner.clock, &clock_title);
        inner.clock.add_css_class("tessel-dock-clock");
        // Keep the clock block centered when the button is larger than its labels.
        inner.clock_content.set_halign(gtk::Align::Center);
        inner.clock_content.set_valign(gtk::Align::Center);
        inner.clock_box.set_valign(gtk::Align::Center);
        inner.weekday_label.add_css_class("tessel-dock-clock-date");
        inner.weekday_label.add_css_class("tessel-dock-clock-weekday");
        inner.weekday_label.set_xalign(0.5);
        inner.weekday_label.set_valign(gtk::Align::Center);
        inner.clock_content.append(&inner.weekday_label);
        inner.clock_content.append(&inner.clock_box);
        inner.time_label.add_css_class("tessel-dock-clock-time");
        inner.date_label.add_css_class("tessel-dock-clock-date");
        for label in [&inner.time_label, &inner.date_label] {
            label.set_xalign(0.5);
            pack(&inner.clock_box, label, false, false, 0);
        }
        inner.clock.set_child(Some(&inner.clock_content));
        let weak = self.weak();
        inner.clock.connect_clicked(move |_| {
            if let Some(panel) = Self::upgrade(&weak) { panel.open("calendar", false); }
        });
        let weak = self.weak();
        click(&inner.clock, 3, false, true, move |_, event| {
            Self::upgrade(&weak).is_some_and(|panel| panel.right_click(event.button, "calendar"))
        });
        dock.add_group("clock", &inner.root, inner.clock.upcast_ref(), true);
        let logout = glyph_button("\u{f011}", &tr!("Lock, log out, sleep or power off"), "logout", size);
        let weak = self.weak();
        logout.button.connect_clicked(move |_| {
            if let Some(panel) = Self::upgrade(&weak) { panel.open("session", false); }
        });
        dock.add_group("session", &inner.root, logout.button.upcast_ref(), true);
        inner.logout.replace(Some(logout));
        let weak = self.weak();
        let source = glib::timeout_add_seconds_local(1, move || match Self::upgrade(&weak) {
            Some(panel) if panel.clock_tick() => glib::ControlFlow::Continue,
            Some(panel) => {
                panel.0.timer.replace(None);
                glib::ControlFlow::Break
            }
            None => glib::ControlFlow::Break,
        });
        inner.timer.replace(Some(source));
        inner.root.set_visible(true);
        if let Some(start) = inner.bar_start.borrow().as_ref() {
            start.set_visible(true);
        }
        self.clock_tick();
        self.refresh();
        self.refresh_trays(true);
    }

    fn indicator(&self, name: &str, size: i32) -> Rc<StatusButton> {
        let title = translated_title(name);
        let button = glyph_button(default_glyph(name), &title, name, size);
        if let Some(child) = button.button.child() {
            child.set_visible(true);
        }
        self.0.buttons.borrow_mut().insert(name.to_owned(), button.clone());
        let ident = name.to_owned();
        if name == "showdesktop" {
            let app = self.0.app.clone();
            button.button.connect_clicked(move |_| { app.status_action(&ident); });
        } else if name != "capslock" {
            let weak = self.weak();
            let target = ident.clone();
            button.button.connect_clicked(move |_| {
                if let Some(panel) = Self::upgrade(&weak) { panel.open(&target, false); }
            });
            if name != "bluetooth" {
                let weak = self.weak();
                let target = ident.clone();
                click(&button.button, 3, false, true, move |_, event| {
                    Self::upgrade(&weak).is_some_and(|panel| panel.right_click(event.button, &target))
                });
            }
        }
        if name == "audio" {
            let weak = self.weak();
            scroll(&button.button, move |_, dx, dy| Self::upgrade(&weak).is_some_and(|panel| panel.audio_scroll(dx, dy)));
        }
        button
    }

    // ---------------------------------------------------------- getters

    pub fn widget(&self) -> gtk::Box { self.0.root.clone() }
    pub fn is_closed(&self) -> bool { self.0.closed.get() }
    pub fn bar_start(&self) -> Option<gtk::Box> { self.0.bar_start.borrow().clone() }
    /// DockSurface recreates `bar_start` when a layout needs it.
    pub fn set_bar_start(&self, start: Option<gtk::Box>) { self.0.bar_start.replace(start); }
    pub fn workspaces_box(&self) -> gtk::Box { self.0.workspaces.clone() }
    pub fn workspace_button(&self, id: &str) -> Option<gtk::Button> { self.0.workspace_buttons.borrow().get(id).cloned() }
    pub fn workspace_ids(&self) -> Vec<String> { self.0.workspace_buttons.borrow().keys().cloned().collect() }
    pub fn media(&self) -> MediaControls { self.0.media.borrow().clone().expect("media controls") }
    pub fn tray(&self) -> Option<StatusTray> { self.0.tray.clone() }
    pub fn always_tray(&self) -> Option<StatusTray> { self.0.always_tray.clone() }
    pub fn tray_expander(&self) -> Option<TrayExpander> { self.0.tray_expander.borrow().clone() }
    pub fn apps(&self) -> gtk::Box { self.0.apps.clone() }
    pub fn apps_toggle(&self) -> gtk::Button { self.0.apps_toggle.clone() }
    pub fn service_area(&self) -> gtk::Box { self.0.service_area.clone() }
    pub fn clock(&self) -> gtk::Button { self.0.clock.clone() }
    pub fn clock_box(&self) -> gtk::Box { self.0.clock_box.clone() }
    pub fn time_label(&self) -> gtk::Label { self.0.time_label.clone() }
    pub fn date_label(&self) -> gtk::Label { self.0.date_label.clone() }
    pub fn weekday_label(&self) -> gtk::Label { self.0.weekday_label.clone() }
    pub fn logout(&self) -> gtk::Button { self.0.logout.borrow().as_ref().expect("logout").button.clone() }
    pub fn showdesktop(&self) -> gtk::Button { self.button("showdesktop").expect("showdesktop") }
    pub fn weather(&self) -> gtk::Button { self.button("weather").expect("weather") }
    pub fn button(&self, name: &str) -> Option<gtk::Button> { self.status_button(name).map(|status| status.button.clone()) }
    pub fn status_button(&self, name: &str) -> Option<Rc<StatusButton>> { self.0.buttons.borrow().get(name).cloned() }
    pub fn button_names(&self) -> Vec<String> { self.0.buttons.borrow().keys().cloned().collect() }
    pub fn record_menu(&self) -> Option<gtk::Popover> { self.0.record_menu.borrow().clone() }
    pub fn capture_preserved_menu(&self) -> Option<gtk::Popover> { self.0.capture_preserved_menu.borrow().clone() }
    pub fn set_capture_preserved_menu(&self, menu: Option<gtk::Popover>) { self.0.capture_preserved_menu.replace(menu); }
    pub fn apps_expanded(&self) -> bool { self.0.apps_expanded.get() }
    /// `_status_visibility[name]` (None until the first refresh saw it).
    pub fn status_visible(&self, name: &str) -> Option<bool> { self.0.status_visibility.borrow().get(name).copied() }

    pub fn icon_size(&self) -> i32 { self.dock().map(|dock| status_icon_size(&dock.config())).unwrap_or(20) }

    pub fn tray_icon_size(&self) -> i32 {
        let Some(dock) = self.dock() else { return 22 };
        let compact = dock.panel().is_some_and(|panel| panel.has_css_class("tessel-dock-compact-bar"));
        tray_icon_size(compact, &dock.config(), dock.is_app_dock())
    }

    // ---------------------------------------------------------- default apps

    fn apps_key(&self, key: gdk::Key) -> bool {
        let no_menu = self.dock().is_some_and(|dock| dock.context_menu().is_none());
        if key == gdk::Key::Escape && self.apps_expanded() && no_menu {
            self.0.apps_toggle.grab_focus();
            self.set_apps_expanded(false);
            return true;
        }
        false
    }

    pub fn schedule_apps_collapse(&self) {
        if self.0.closed.get() || self.0.apps_hide.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let source = timer::once(APPS_COLLAPSE_DELAY_MS, move || {
            if let Some(panel) = Self::upgrade(&weak) {
                panel.0.apps_hide.replace(None);
                panel.collapse_apps();
            }
        });
        self.0.apps_hide.replace(Some(source));
    }

    fn collapse_apps(&self) {
        if self.0.closed.get() {
            return;
        }
        let Some(dock) = self.dock() else { return };
        let menu = dock.context_menu();
        let anchor = menu.and_then(|menu| menu.parent());
        let menu_open = anchor.is_some_and(|anchor| anchor == *self.0.apps.upcast_ref::<gtk::Widget>()
                                           || anchor.is_ancestor(&self.0.apps));
        let keyboard = dock.window().gets_focus_visible() && self.0.apps_focus.contains_focus();
        if !(self.0.apps_pointer.get() || self.0.apps_menu.get() || menu_open || keyboard) {
            self.set_apps_expanded(false);
        }
    }

    pub fn set_apps_expanded(&self, expanded: bool) {
        if self.0.closed.get() {
            return;
        }
        if let Some(source) = self.0.apps_hide.take() {
            source.cancel();
        }
        self.0.apps_expanded.set(expanded);
        self.sync_apps_visibility();
        if let Some(dock) = self.dock() {
            dock.queue_panel_layout();
        }
    }

    pub fn sync_apps_visibility(&self) {
        let Some(dock) = self.dock() else { return };
        let config = dock.config();
        let always = &config.always_visible_apps;
        let visibility = self.0.status_visibility.borrow().clone();
        let available: Vec<&str> = INDICATORS.iter().copied().filter(|name| visibility.get(*name).copied().unwrap_or(false)).collect();
        let expanded = self.apps_expanded();
        for name in INDICATORS {
            if let Some(button) = self.button(name) {
                button.set_visible(available.contains(&name) && (expanded || always.iter().any(|known| known == name)));
            }
        }
        self.0.apps_toggle.set_visible(available.iter().any(|name| !always.iter().any(|known| known == name)));
        accessible_name(&self.0.apps_toggle,
                        &if expanded { tr!("Collapse app icons") } else { tr!("Expand app icons") });
    }

    // ---------------------------------------------------------- trays

    pub fn always_tray_item(&self, item: &TrayItem) -> bool {
        self.dock().is_some_and(|dock| always_tray_item(&dock.config(), item))
    }

    pub fn regular_tray_item(&self, item: &TrayItem) -> bool {
        self.dock().is_some_and(|dock| regular_tray_item(&dock.config(), item))
    }

    pub fn refresh_trays(&self, queue: bool) {
        if self.0.closed.get() {
            return;
        }
        let Some(dock) = self.dock() else { return };
        self.sync_tray_size();
        if let (Some(tray), Some(always)) = (&self.0.tray, &self.0.always_tray) {
            let config = dock.config();
            let selection = config.always_show_tray.clone();
            let changed = self.0.tray_selection.borrow().as_ref() != Some(&selection);
            if changed {
                self.0.tray_selection.replace(Some(selection));
                tray.refresh();
                always.refresh();
            }
            let items = self.0.app.tray_backend().map(|backend| backend.items()).unwrap_or_default();
            let visible = config.is_group_visible("system_tray")
                && items.iter().any(|item| item.ready() && always_tray_item(&config, item));
            always.widget().set_visible(visible);
        }
        if queue {
            dock.queue_panel_layout();
        }
    }

    /// Availability of the groups this panel owns (`dock.group_available`).
    pub fn group_availability(&self) -> IndexMap<&'static str, bool> {
        // Read backend items directly: tray subscribers can run in either order.
        let config = self.dock().map(|dock| dock.config());
        let items = if self.0.tray.is_some() {
            self.0.app.tray_backend().map(|backend| backend.items()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let visibility = self.0.status_visibility.borrow();
        let mut result = IndexMap::new();
        result.insert("workspaces", !self.0.workspace_buttons.borrow().is_empty());
        result.insert("system_tray", config.as_ref().is_some_and(|config| {
            items.iter().any(|item| item.ready() && regular_tray_item(config, item))
        }));
        result.insert("service_apps", INDICATORS.iter().any(|name| visibility.get(*name).copied().unwrap_or(false)));
        result.insert("weather", visibility.get("weather").copied().unwrap_or(true));
        result.insert("media", true);
        result.insert("clock", true);
        result.insert("session", true);
        result
    }

    fn sync_tray_size(&self) {
        let size = self.tray_icon_size();
        for tray in [&self.0.tray, &self.0.always_tray].into_iter().flatten() {
            tray.set_icon_size(size);
        }
    }

    fn tray_menu(&self, active: bool) {
        if let Some(expander) = self.tray_expander() {
            expander.menu_active(active);
        }
        if let Some(dock) = self.dock() {
            if active { dock.enter("tray-menu") } else { dock.leave("tray-menu") }
        }
    }

    fn always_tray_menu(&self, active: bool) {
        self.0.apps_menu.set(active);
        if !active {
            self.schedule_apps_collapse();
        }
        if active {
            if let Some(expander) = self.tray_expander() {
                expander.set_expanded(false);
            }
        }
        if let Some(dock) = self.dock() {
            if active { dock.enter("always-tray-menu") } else { dock.leave("always-tray-menu") }
        }
    }

    // ---------------------------------------------------------- actions

    /// `_open(name, settings)`: open an applet popup beside the panel.
    pub fn open(&self, name: &str, settings: bool) -> bool {
        if name == "screen_record" && !settings {
            self.record_menu_open();
            return true;
        }
        let Some(dock) = self.dock() else { return false };
        if let Some(expander) = self.tray_expander() {
            expander.set_expanded(false);
        }
        if let Some(expander) = dock.application_expander() {
            expander.set_expanded(false);
        }
        dock.close_app_menu();
        dock.close_preview();
        let window = dock.window();
        let thickness = if dock.horizontal() { window.height() } else { window.width() };
        let offset = 40.max(thickness + dock.edge_gap() + 8);
        self.0.app.panel_open(name, settings, &dock.config().edge, offset as i64)
    }

    fn record_menu_open(&self) {
        let status = self.0.app.status();
        let state = status.get("screen_record").and_then(|value| value.get("state")).map(py_str)
            .unwrap_or_else(|| "idle".into());
        if state != "idle" {
            if state != "stopping" {
                self.0.app.panel_capture("stop");
            }
            return;
        }
        let Some(dock) = self.dock() else { return };
        let entries: Vec<MenuEntry> = [("With no audio", "none"), ("With desktop audio", "desktop"),
                                       ("With desktop + microphone audio", "desktop-mic")].into_iter()
            .map(|(title, preset)| {
                let app = self.0.app.clone();
                let action: Rc<dyn Fn()> = Rc::new(move || { app.panel_capture(preset); });
                (tr!(title), action)
            }).collect();
        let Some(anchor) = self.button("screen_record") else { return };
        dock.popup_menu(anchor.upcast_ref(), None, entries);
        let Some(menu) = dock.context_menu() else { return };
        self.0.record_menu.replace(Some(menu.clone()));
        let weak = self.weak();
        menu.connect_closed(move |_| {
            if let Some(panel) = Self::upgrade(&weak) { panel.0.record_menu.replace(None); }
        });
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        heading.append(&gtk::Image::from_icon_name("camera-video-symbolic"));
        let label = gtk::Label::new(Some(&tr!("Screen Record")));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        heading.append(&label);
        let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
        let gear_title = tr!("Screen Recorder settings");
        gear.set_tooltip_text(Some(&gear_title));
        accessible_name(&gear, &gear_title);
        let (popover, app) = (menu.clone(), self.0.app.clone());
        gear.connect_clicked(move |_| {
            popover.popdown();
            app.panel_capture("settings");
        });
        heading.append(&gear);
        if let Some(child) = menu.child().and_downcast::<gtk::Box>() {
            child.prepend(&heading);
        }
    }

    fn right_click(&self, button: u32, name: &str) -> bool {
        if button == 3 {
            if name == "idle_inhibitor" {
                self.0.app.status_action(name);
            } else {
                self.open(name, true);
            }
            return true;
        }
        false
    }

    fn audio_scroll(&self, dx: f64, dy: f64) -> bool {
        if dx == 0.0 && dy == 0.0 {
            return false;
        }
        let delta = if dy.abs() >= dx.abs() { dy } else { dx };
        self.0.app.audio_scroll(if delta > 0.0 { -1.0 } else { 1.0 });
        true
    }

    // ---------------------------------------------------------- clock

    /// One clock update; false once closed.
    pub fn clock_tick(&self) -> bool {
        if self.0.closed.get() {
            return false;
        }
        let Ok(now) = glib::DateTime::now_local() else { return true };
        let settings = self.0.clock_store.borrow_mut().load();
        let changed = self.0.clock_settings.borrow().as_ref() != Some(&settings);
        if changed {
            self.0.clock_settings.replace(Some(settings.clone()));
            let inner = &self.0;
            inner.clock_box.set_orientation(if settings.stacked { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
            inner.clock_box.set_spacing(if settings.stacked { 0 } else { 6 });
            if let Some(dock) = self.dock() {
                dock.toggle_class(inner.clock.upcast_ref(), "clock-inline", !settings.stacked);
            }
            let (first, second) = if settings.date_first { (&inner.date_label, &inner.time_label) }
                                  else { (&inner.time_label, &inner.date_label) };
            inner.clock_box.reorder_child_after(first, None::<&gtk::Widget>);
            inner.clock_box.reorder_child_after(second, Some(first));
            inner.date_label.set_visible(settings.show_date);
            inner.weekday_label.set_visible(settings.show_weekday);
            if let Some(dock) = self.dock() {
                dock.queue_panel_layout();
            }
        }
        self.0.time_label.set_text(&settings.format_time(&now));
        self.0.date_label.set_text(&settings.format_date(&now));
        self.0.weekday_label.set_text(&settings.format_weekday(&now));
        let stamp = now.format("%A, %B %-d, %Y · %Z").map(|s| s.to_string()).unwrap_or_default();
        let tooltip = tr!("{value1}\nClick for calendar · Right-click for date/time settings", value1 = stamp);
        if self.0.clock.tooltip_text().as_deref() != Some(tooltip.as_str()) {
            self.0.clock.set_tooltip_text(Some(&tooltip));
        }
        true
    }

    // ---------------------------------------------------------- refresh

    pub fn refresh_workspaces(&self) {
        let Some(dock) = self.dock() else { return };
        let app = &self.0.app;
        let entries: Vec<_> = app.workspaces(&dock.monitor_name()).unwrap_or_default()
            .into_iter().filter(|entry| !entry.hidden).collect();
        let stale: Vec<String> = self.0.workspace_buttons.borrow().keys()
            .filter(|ident| !entries.iter().any(|entry| &entry.id == *ident)).cloned().collect();
        for ident in stale {
            if let Some(button) = self.0.workspace_buttons.borrow_mut().remove(&ident) {
                remove(&button);
            }
        }
        for (index, entry) in entries.iter().enumerate() {
            let existing = self.0.workspace_buttons.borrow().get(&entry.id).cloned();
            let button = match existing {
                Some(button) => button,
                None => {
                    let button = gtk::Button::new();
                    button.add_css_class("tessel-dock-workspace");
                    let (app, ident) = (app.clone(), entry.id.clone());
                    button.connect_clicked(move |_| { app.activate_workspace(&ident); });
                    pack(&self.0.workspaces, &button, false, false, 0);
                    self.0.workspace_buttons.borrow_mut().insert(entry.id.clone(), button.clone());
                    button
                }
            };
            let label = if entry.name.is_empty() { (index + 1).to_string() } else { entry.name.clone() };
            button.set_label(&label);
            let title = tr!("Workspace {value1}", value1 = label.as_str());
            button.set_tooltip_text(Some(&title));
            let current = if entry.active { format!(" · {}", tr!("Current")) } else { String::new() };
            accessible_name(&button, &format!("{title}{current}"));
            button.set_sensitive(entry.can_activate);
            dock.toggle_class(button.upcast_ref(), "active", entry.active);
            dock.toggle_class(button.upcast_ref(), "urgent", entry.urgent);
            let previous = if index > 0 { tessel_gtk::utils::children(&self.0.workspaces).get(index - 1).cloned() } else { None };
            self.0.workspaces.reorder_child_after(&button, previous.as_ref());
            button.set_visible(true);
        }
        let error = app.workspace_error();
        self.0.workspaces.set_tooltip_text(if error.is_empty() { None } else { Some(&error) });
    }

    pub fn refresh(&self) {
        if self.0.closed.get() {
            return;
        }
        let Some(dock) = self.dock() else { return };
        self.refresh_workspaces();
        self.sync_tray_size();
        let status = self.0.app.status();
        let size = self.icon_size();
        let buttons: Vec<(String, Rc<StatusButton>)> =
            self.0.buttons.borrow().iter().map(|(name, button)| (name.clone(), button.clone())).collect();
        let empty = Map::new();
        for (name, button) in buttons {
            let state = status.get(&name).and_then(|value| value.as_object()).unwrap_or(&empty);
            if name == "screen_record" && state.get("state").map(py_str).unwrap_or_else(|| "idle".into()) != "idle" {
                if let Some(menu) = self.record_menu() {
                    menu.popdown();
                }
            }
            let mut text = state.get("text").map(py_str).unwrap_or_else(|| default_glyph(&name).to_owned());
            let classes = status_classes(state);
            if let Some(badge) = &button.vpn_badge {
                // The second glyph is VPN state: keep the icon centered, show a badge.
                text = text.split_whitespace().next().unwrap_or("").to_owned();
                badge.set_visible(classes.contains("vpn"));
            }
            button.label.set_text(&text);
            set_status_visual(&button, &text, &classes, size);
            let title = translated_title(&name);
            let detail = state.get("tooltip").map(py_str_or_empty).unwrap_or_default();
            let tooltip = status_tooltip(&title, &detail);
            if button.button.tooltip_text().as_deref() != Some(tooltip.as_str()) {
                button.button.set_tooltip_text(Some(&tooltip));
            }
            accessible_name(&button.button, tooltip.split('\n').next().unwrap_or(""));
            // Screen readers get the complete state, not only its first line.
            accessible_description(&button.button, &tooltip);
            let previous = button.panel_classes.borrow().clone();
            for css in previous.difference(&classes) {
                button.button.remove_css_class(css);
            }
            for css in classes.difference(&previous) {
                button.button.add_css_class(css);
            }
            button.panel_classes.replace(classes);
            let default_visible = if name == "capslock" { !text.is_empty() } else { true };
            let visible = state.get("visible").map(truthy).unwrap_or(default_visible);
            self.0.status_visibility.borrow_mut().insert(name.clone(), visible);
            if !INDICATORS.contains(&name.as_str()) {
                button.button.set_visible(visible);
            }
        }
        if let Some(media) = self.0.media.borrow().clone() {
            media.refresh();
        }
        self.sync_apps_visibility();
        dock.refresh_groups();
        dock.queue_panel_layout();
    }

    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        if let Some(media) = self.0.media.borrow().clone() {
            media.close();
        }
        if let Some(source) = self.0.apps_hide.take() {
            source.cancel();
        }
        if let Some(source) = self.0.timer.take() {
            source.remove();
        }
        if let Some(tray) = &self.0.tray {
            if let Some(expander) = self.tray_expander() {
                expander.close();
            }
            if let (Some(backend), Some(id)) = (self.0.app.tray_backend(), self.0.tray_subscription.take()) {
                backend.unsubscribe(id);
            }
            tray.close();
            if let Some(always) = &self.0.always_tray {
                always.close();
            }
        }
        if let Some(start) = self.0.bar_start.take() {
            if start.parent().is_some() {
                start.unparent();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::DEFAULT_APPS;

    fn set(classes: &[&str]) -> BTreeSet<String> { classes.iter().map(|c| c.to_string()).collect() }

    #[test]
    fn glyph_and_class_icon_map_matches_python() {
        assert!(status_icon_names("capslock", "⇪", &set(&[])).is_empty());
        assert!(status_icon_names("weather", "☀ 72°", &set(&[])).is_empty());
        assert_eq!(status_icon_names("audio", "\u{f026}", &set(&[])), ["audio-volume-low-symbolic"]);
        assert_eq!(status_icon_names("audio", "\u{f6a9}", &set(&[])), ["audio-volume-muted-symbolic"]);
        assert_eq!(status_icon_names("audio", "\u{f028} 40%", &set(&["muted"])), ["audio-volume-muted-symbolic"]);
        assert_eq!(status_icon_names("audio", "x", &set(&[])), ["audio-volume-high-symbolic"]);
        assert_eq!(status_icon_names("battery", "\u{f243}", &set(&[])), ["battery-level-20-symbolic", "battery-low-symbolic"]);
        assert_eq!(status_icon_names("battery", "\u{f240}", &set(&["plugged"]))[0], "battery-full-charging-symbolic");
        assert_eq!(status_icon_names("battery", "\u{f240}", &set(&["unavailable", "plugged"])), ["battery-missing-symbolic"]);
        assert_eq!(status_icon_names("battery", "?", &set(&[])), ["battery-good-symbolic"]);
        assert_eq!(status_icon_names("network", "󰤟 \u{f023}", &set(&["wifi", "vpn"])),
                   ["network-wireless-signal-weak-symbolic", "network-wireless-symbolic"]);
        assert_eq!(status_icon_names("network", "󰤨", &set(&["wifi", "limited"])),
                   ["network-wireless-no-route-symbolic", "network-wireless-signal-excellent-symbolic", "network-wireless-symbolic"]);
        assert_eq!(status_icon_names("network", "", &set(&["limited"])), ["network-wired-no-route-symbolic", "network-wired-symbolic"]);
        assert_eq!(status_icon_names("network", "", &set(&["disabled", "wifi"]))[0], "network-wireless-disabled-symbolic");
        assert_eq!(status_icon_names("network", "", &set(&["disconnected"]))[0], "network-offline-symbolic");
        assert_eq!(status_icon_names("network", "", &set(&["connecting"]))[0], "network-wireless-acquiring-symbolic");
        assert_eq!(status_icon_names("network", "", &set(&["mobile"]))[0], "network-cellular-signal-good-symbolic");
        assert_eq!(status_icon_names("bluetooth", "", &set(&["connected", "off"]))[0], "bluetooth-active-symbolic");
        assert_eq!(status_icon_names("bluetooth", "", &set(&["off"]))[0], "bluetooth-disabled-symbolic");
        assert_eq!(status_icon_names("bluetooth", "", &set(&[]))[0], "bluetooth-symbolic");
        assert_eq!(status_icon_names("power", "", &set(&["power-saver"]))[0], "power-profile-power-saver-symbolic");
        assert_eq!(status_icon_names("power", "", &set(&["performance", "power-saver"]))[0], "power-profile-performance-symbolic");
        assert_eq!(status_icon_names("power", "", &set(&[])), ["power-profile-balanced-symbolic", "power-profile-balanced-symbolic"]);
        assert_eq!(status_icon_names("notifications", "", &set(&["dnd"]))[0], "notifications-disabled-symbolic");
        assert_eq!(status_icon_names("notifications", "", &set(&[]))[0], "preferences-system-notifications-symbolic");
        assert_eq!(status_icon_names("idle_inhibitor", "\u{f06e}", &set(&[])), ["caffeine-cup-full-symbolic"]);
        assert_eq!(status_icon_names("idle_inhibitor", "\u{f070}", &set(&["activated"])), ["caffeine-cup-full-symbolic"]);
        assert_eq!(status_icon_names("idle_inhibitor", "\u{f070}", &set(&[])), ["caffeine-cup-empty-symbolic"]);
        assert_eq!(status_icon_names("logout", "", &set(&[])), ["system-shutdown-symbolic"]);
        assert_eq!(status_icon_names("showdesktop", "", &set(&[]))[0], "user-desktop-symbolic");
        assert!(status_icon_names("unknown", "", &set(&[])).is_empty());
    }

    #[test]
    fn icon_sizes_follow_python_rounding_and_clamps() {
        let mut config = DockConfig::default();
        for (icon, compact, expected) in [(40, false, 20), (40, true, 16), (16, false, 14), (128, false, 32),
                                          (25, false, 14), (25, true, 12), (64, true, 20)] {
            config.icon_size = icon;
            config.compact_bar = compact;
            assert_eq!(status_icon_size(&config), expected, "{icon} {compact}");
        }
        // round(22.5) == 22 (half to even), round(17.5) == 18.
        config.compact_bar = false;
        config.icon_size = 45;
        assert_eq!(status_icon_size(&config), 22);
        config.icon_size = 35;
        assert_eq!(status_icon_size(&config), 18);
        config.compact_bar = true;
        assert_eq!(tray_icon_size(false, &config, false), 16);
        assert_eq!(tray_icon_size(false, &config, true), 22);
        assert_eq!(tray_icon_size(true, &DockConfig::default(), true), 16);
        assert_eq!(tray_icon_size(false, &DockConfig::default(), false), 22);
    }

    #[test]
    fn tooltips_classes_titles_and_defaults() {
        assert_eq!(status_tooltip("Keep Awake", ""), "Keep Awake");
        assert_eq!(status_tooltip("Battery", "battery: 50%"), "battery: 50%");
        assert_eq!(status_tooltip("Battery", "50%"), "Battery\n50%");
        let state: Map<String, Value> = serde_json::from_str(r#"{"classes": ["on", "", "wifi"], "class": "x"}"#).unwrap();
        assert_eq!(status_classes(&state), set(&["on", "wifi"]));
        let state: Map<String, Value> = serde_json::from_str(r#"{"class": "dnd"}"#).unwrap();
        assert_eq!(status_classes(&state), set(&["dnd"]));
        assert_eq!(title_source("idle_inhibitor"), "Keep Awake");
        assert_eq!(title_source("some_new_thing"), "Some New Thing");
        assert_eq!(default_glyph("showdesktop"), "\u{f111}");
        assert_eq!(default_glyph("weather"), "…");
        assert_eq!(INDICATORS.to_vec(), DEFAULT_APPS.iter().map(|(name, _)| *name).collect::<Vec<_>>());
    }

    #[test]
    fn tray_identity_routing() {
        let item = TrayItem::new("org.example.App", "/StatusNotifierItem");
        let mut properties = std::collections::HashMap::new();
        properties.insert("Id".to_string(), glib::Variant::from("music-player"));
        properties.insert("Status".to_string(), glib::Variant::from("Passive"));
        item.set_properties(properties);
        item.set_ready(true);
        let mut config = DockConfig::default();
        assert!(!regular_tray_item(&config, &item), "passive items stay out of the regular tray");
        config.always_show_tray = vec!["id:music-player".into()];
        assert!(always_tray_item(&config, &item));
        assert!(!regular_tray_item(&config, &item));
        let anonymous = TrayItem::new(":1.42", "/StatusNotifierItem");
        assert_eq!(tray_identity(&tray_info(&anonymous)), "");
    }
}
