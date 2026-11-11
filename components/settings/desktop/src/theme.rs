//! Theme & Wallpaper (desktop_pages.ThemeWallpaperPage): light/dark style
//! through `org.gnome.desktop.interface color-scheme`, dock/app appearance
//! through a [`ThemeBridge`] (APPEARANCE_FIELDS of the dock settings), and
//! per-display wallpapers through Tessel Shell's own `tessel-shell.sock` protocol.
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gs_base::style::{confirm_leave, page_header};
use gs_base::{Host, SettingsPage};
use gtk::prelude::*;
use gtk::{gdk, pango};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::tr;
use tessel_dock::core::config::DockConfig;

use crate::bridge::{appearance_fields, BridgeOptions, DockSettingsBridge};
use crate::util::{accessible_label, label, py_round};

pub const MODES: [&str; 6] = ["Fill", "Fit", "Stretch", "Center", "Tile", "Color Only"];
pub const COLOR_SCHEMES: [(&str, &str); 3] = [("default", "Automatic"), ("prefer-light", "Light"), ("prefer-dark", "Dark")];
pub const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
pub const THEME_CSS: &str = "
.theme-settings .subtitle { color: alpha(@theme_fg_color, .66); }
.theme-settings button { padding: 8px 12px; border-radius: 8px; box-shadow: none; }
.theme-settings switch { padding: 2px; min-width: 40px; min-height: 22px; }
.theme-settings switch slider { margin: 0; padding: 0; min-width: 18px; min-height: 18px; }
.theme-settings scrollbar slider { min-width: 6px; min-height: 20px; }
";
const TOGGLES: [&str; 6] = ["frosted", "apps_frosted", "apps_rounded_corners", "window_buttons_left", "colored_window_buttons",
                            "window_shadows"];
const STRENGTHS: [&str; 2] = ["frost_strength", "apps_frost_strength"];

/// The shared GNOME interface settings, or `None` when the schema (or its
/// `color-scheme` key) is missing.
pub fn interface_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(INTERFACE_SCHEMA, true)?;
    if !schema.has_key("color-scheme") {
        return None;
    }
    Some(gio::Settings::new(INTERFACE_SCHEMA))
}

/// What the Style selector needs of `Gio.Settings` (tests use a fake).
pub trait ColorSettings {
    fn get_string(&self, key: &str) -> String;
    fn set_string(&self, key: &str, value: &str) -> bool;
    fn connect_changed(&self, key: &str, handler: Rc<dyn Fn()>) -> u64;
    fn disconnect(&self, id: u64);
}

pub struct GioColorSettings {
    settings: gio::Settings,
    handlers: RefCell<Vec<(u64, glib::SignalHandlerId)>>,
    next: Cell<u64>,
}

impl GioColorSettings {
    pub fn new(settings: gio::Settings) -> Self {
        Self { settings, handlers: RefCell::new(Vec::new()), next: Cell::new(1) }
    }
}

impl ColorSettings for GioColorSettings {
    fn get_string(&self, key: &str) -> String { self.settings.string(key).to_string() }
    fn set_string(&self, key: &str, value: &str) -> bool { self.settings.set_string(key, value).is_ok() }
    fn connect_changed(&self, key: &str, handler: Rc<dyn Fn()>) -> u64 {
        let id = self.settings.connect_changed(Some(key), move |_, _| handler());
        let number = self.next.get();
        self.next.set(number + 1);
        self.handlers.borrow_mut().push((number, id));
        number
    }
    fn disconnect(&self, id: u64) {
        let mut handlers = self.handlers.borrow_mut();
        if let Some(index) = handlers.iter().position(|(n, _)| *n == id) {
            let (_, handler) = handlers.remove(index);
            self.settings.disconnect(handler);
        }
    }
}

/// The dock-settings duck type ThemeWallpaperPage uses. `has_async` = the
/// bridge has `refresh_async`/`save_config_async` (the real one does).
pub trait ThemeBridge {
    fn config(&self) -> DockConfig;
    fn save_config(&self, config: &DockConfig) -> bool;
    fn has_async(&self) -> bool { false }
    fn save_config_async(&self, config: DockConfig, done: Box<dyn FnOnce(bool)>) { done(self.save_config(&config)) }
    fn refresh_async(&self, done: Box<dyn FnOnce(bool)>) { done(self.refresh()) }
    /// `bridge.refresh()` when the bridge has one (Python `getattr(..., "refresh", None)`).
    fn refresh(&self) -> bool { true }
    fn close(&self) {}
}

impl ThemeBridge for DockSettingsBridge {
    fn config(&self) -> DockConfig { DockSettingsBridge::config(self) }
    fn save_config(&self, config: &DockConfig) -> bool { DockSettingsBridge::save_config(self, config, true) }
    fn has_async(&self) -> bool { true }
    fn save_config_async(&self, config: DockConfig, done: Box<dyn FnOnce(bool)>) {
        DockSettingsBridge::save_config_async(self, config, Some(done))
    }
    fn refresh_async(&self, done: Box<dyn FnOnce(bool)>) { DockSettingsBridge::refresh_async(self, Some(done), None) }
    fn refresh(&self) -> bool { DockSettingsBridge::refresh(self, None, true) }
    fn close(&self) { DockSettingsBridge::close(self) }
}

/// One tessel-shell.sock request (sent from a worker thread).
pub type WallpaperRequest = Arc<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;

/// `wallpaper_request(payload, timeout=8)`: one JSON line to
/// `$XDG_RUNTIME_DIR/tessel-shell.sock`, one JSON line back with `"ok": true`.
pub fn wallpaper_request(payload: &Value) -> Result<Value, String> {
    wallpaper_request_timeout(payload, 8.0)
}

pub fn wallpaper_request_timeout(payload: &Value, timeout: f64) -> Result<Value, String> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok().filter(|r| !r.is_empty())
        .ok_or_else(|| tr!("The desktop session is unavailable."))?;
    wallpaper_request_at(&Path::new(&runtime).join("tessel-shell.sock"), payload, timeout)
}

/// One request to an explicit socket (tests use a private one).
pub fn wallpaper_request_at(socket: &Path, payload: &Value, timeout: f64) -> Result<Value, String> {
    let os = |e: std::io::Error| if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
        "timed out".to_owned()
    } else {
        tessel_core::pyerr::os_error_text(&e)
    };
    let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(os)?;
    let duration = std::time::Duration::from_secs_f64(timeout);
    stream.set_read_timeout(Some(duration)).map_err(os)?;
    stream.set_write_timeout(Some(duration)).map_err(os)?;
    let mut line = json_dumps(payload, &JsonStyle::DEFAULT).into_bytes();
    line.push(b'\n');
    stream.write_all(&line).map_err(os)?;
    let mut data: Vec<u8> = Vec::new();
    while !data.contains(&b'\n') {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk).map_err(os)?;
        if count == 0 {
            return Err(tr!("The wallpaper service closed the connection."));
        }
        data.extend_from_slice(&chunk[..count]);
        if data.len() > 65536 {
            return Err(tr!("The wallpaper service returned an invalid response."));
        }
    }
    let first = data.split(|b| *b == b'\n').next().unwrap_or_default();
    let result: Value = serde_json::from_slice(first).map_err(|e| e.to_string())?;
    let ok = result.as_object().and_then(|r| r.get("ok")).is_some_and(tessel_core::pyjson::truthy);
    if !ok {
        let message = result.as_object().and_then(|r| r.get("msg")).filter(|m| tessel_core::pyjson::truthy(m))
            .map(tessel_core::pyjson::py_str);
        return Err(message.unwrap_or_else(|| tr!("The wallpaper could not be changed.")));
    }
    Ok(result)
}

/// `dataclasses.replace(config, **{field: value})` for the appearance fields.
pub fn set_appearance_field(config: &mut DockConfig, field: &str, value: &Value) {
    let flag = || value.as_bool().unwrap_or(false);
    let number = || value.as_i64().unwrap_or(0);
    match field {
        "match_wallpaper" => config.match_wallpaper = flag(),
        "transparency" => config.transparency = number(),
        "transparency_enabled" => config.transparency_enabled = flag(),
        "frosted" => config.frosted = flag(),
        "frost_strength" => config.frost_strength = number(),
        "apps_transparency" => config.apps_transparency = number(),
        "apps_transparency_enabled" => config.apps_transparency_enabled = flag(),
        "apps_frosted" => config.apps_frosted = flag(),
        "apps_frost_strength" => config.apps_frost_strength = number(),
        "apps_rounded_corners" => config.apps_rounded_corners = flag(),
        "window_buttons_left" => config.window_buttons_left = flag(),
        "colored_window_buttons" => config.colored_window_buttons = flag(),
        "window_shadows" => config.window_shadows = flag(),
        _ => {}
    }
}

fn config_bool(config: &DockConfig, field: &str) -> bool {
    match config.to_value().get(field) {
        Some(value) => tessel_core::pyjson::truthy(value),
        None => false,
    }
}

fn config_number(config: &DockConfig, field: &str) -> f64 {
    config.to_value().get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

/// One display's wallpaper row (`self.rows[connector]`).
pub struct WallpaperRow {
    pub chooser: gtk::Button,
    pub mode: gtk::DropDown,
    pub color: gtk::ColorDialogButton,
    pub path: RefCell<Option<String>>,
    pub dirty: Cell<bool>,
}

/// Construction options (Python keyword arguments).
#[derive(Default)]
pub struct ThemeOptions {
    pub bridge: Option<Rc<dyn ThemeBridge>>,
    pub request: Option<WallpaperRequest>,
    pub config_path: Option<PathBuf>,
    /// `color_settings=False` (default) → [`interface_settings`]; `Some(None)` = unavailable.
    pub color_settings: Option<Option<Rc<dyn ColorSettings>>>,
    /// Icon theme storage (default: gsettings + GTK settings.ini).
    pub icon_settings: Option<Rc<dyn crate::icon_themes::IconThemeSettings>>,
    /// Icon theme search directories (default: the XDG icon paths).
    pub icon_dirs: Option<Vec<PathBuf>>,
}

pub struct ThemeWallpaperPage {
    pub host: Host,
    pub widget: gtk::Box,
    pub content: gtk::Box,
    pub request: WallpaperRequest,
    pub config_path: PathBuf,
    pub bridge: RefCell<Option<Rc<dyn ThemeBridge>>>,
    pub color_settings: Option<Rc<dyn ColorSettings>>,
    pub scheme_buttons: IndexMap<String, gtk::ToggleButton>,
    pub scheme_card: gtk::Box,
    scheme_handler: Cell<u64>,
    pub icons: Rc<crate::icon_themes::IconThemeChooser>,
    pub r#match: gtk::Switch,
    pub dock_transparency: gtk::Scale,
    pub dock_transparency_enabled: gtk::Switch,
    pub apps_transparency: gtk::Scale,
    pub apps_transparency_enabled: gtk::Switch,
    pub toggles: IndexMap<String, gtk::Switch>,
    pub strengths: IndexMap<String, gtk::Scale>,
    pub status: gtk::Label,
    pub cards: gtk::Box,
    pub discard_button: gtk::Button,
    pub apply_button: gtk::Button,
    pub rows: RefCell<IndexMap<String, Rc<WallpaperRow>>>,
    style_provider: gtk::CssProvider,
    pub closed: Cell<bool>,
    pub busy: Cell<bool>,
    pub updating: Cell<bool>,
    saving: Cell<bool>,
    pending_appearance: RefCell<Map<String, Value>>,
    appearance_source: RefCell<Option<glib::SourceId>>,
    dialogs: RefCell<Vec<gio::Cancellable>>,
    media_pickers: RefCell<Vec<(gio::Cancellable, tessel_shell::wallpaper_chooser::WallpaperChooser)>>,
    apply_done: RefCell<Option<Box<dyn FnOnce(bool)>>>,
}

fn switch() -> gtk::Switch {
    gtk::Switch::builder().valign(gtk::Align::Center).halign(gtk::Align::End).build()
}

fn percent_scale() -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_digits(0);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Top);
    scale.set_format_value_func(|_scale, value| format!("{value:.0}%"));
    scale.set_hexpand(true);
    scale
}

fn card(spacing: i32) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, spacing);
    card.add_css_class("gs-card");
    card
}

impl ThemeWallpaperPage {
    pub fn new(host: &Host, options: ThemeOptions) -> Result<Rc<Self>, String> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 12);
        for name in ["tessel-shell-wallpaper-settings", "wallpaper-body", "theme-settings"] {
            widget.add_css_class(name);
        }
        let scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(24)
            .margin_bottom(24).margin_start(24).margin_end(24).build();
        scroll.set_child(Some(&content));
        widget.append(&scroll);
        content.append(&page_header(&tr!("Theme & Wallpaper"),
            Some(&tr!("Choose a light or dark style, colors, and a background for each display. Style and appearance options apply immediately.")),
            Some("preferences-desktop-wallpaper-symbolic")).root);

        // Style (color scheme).
        let color_settings: Option<Rc<dyn ColorSettings>> = match options.color_settings {
            Some(settings) => settings,
            None => interface_settings().map(|s| Rc::new(GioColorSettings::new(s)) as Rc<dyn ColorSettings>),
        };
        let scheme_card = card(8);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        text.append(&label(&tr!("Style"), None));
        text.append(&label(&tr!("Use light or dark colors in the dock, our apps and other apps that support it. Automatic follows the GTK theme."), Some("subtitle")));
        row.append(&text);
        let group = gtk::Box::builder().valign(gtk::Align::Center).css_classes(["linked"]).build();
        let mut scheme_buttons = IndexMap::new();
        let mut first: Option<gtk::ToggleButton> = None;
        for (value, caption) in COLOR_SCHEMES {
            let button = gtk::ToggleButton::with_label(&tr!(caption));
            match &first {
                None => first = Some(button.clone()),
                Some(first) => button.set_group(Some(first)),
            }
            group.append(&button);
            scheme_buttons.insert(value.to_owned(), button);
        }
        row.append(&group);
        scheme_card.append(&row);
        content.append(&scheme_card);
        if color_settings.is_none() {
            group.set_sensitive(false);
            scheme_card.append(&label(&tr!("The shared color scheme setting is unavailable on this system."), Some("subtitle")));
        }

        // Icons.
        let icon_settings = options.icon_settings
            .unwrap_or_else(|| Rc::new(crate::icon_themes::SystemIconThemeSettings::from_session()));
        let icons = crate::icon_themes::IconThemeChooser::new(icon_settings,
            options.icon_dirs.unwrap_or_else(tessel_core::icon_theme::search_dirs));
        content.append(&icons.card);

        // Match wallpaper colors.
        let match_card = card(8);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        text.append(&label(&tr!("Match wallpaper colors"), None));
        text.append(&label(&tr!("Match the dock and our apps to each monitor’s wallpaper. Other GTK4 apps use the primary display’s colors."), Some("subtitle")));
        row.append(&text);
        let match_switch = switch();
        row.append(&match_switch);
        match_card.append(&row);
        content.append(&match_card);

        let mut toggles = IndexMap::new();
        let mut strengths = IndexMap::new();
        let (dock_card, dock_transparency, dock_transparency_enabled) = Self::transparency_controls(&content,
            &tr!("Dock transparency"), &tr!("Adjust the dock background independently of application windows."));
        toggles.insert("frosted".into(), Self::appearance_toggle(&dock_card, &tr!("Blur"),
            &tr!("Blur the content behind the dock. Requires compositor blur support; transparency controls how much shows through.")));
        strengths.insert("frost_strength".into(), Self::frost_strength_control(&dock_card));
        let (apps_card, apps_transparency, apps_transparency_enabled) = Self::transparency_controls(&content,
            &tr!("Application transparency"), &tr!("Adjust application and desktop menu backgrounds. Text and icons stay opaque."));
        toggles.insert("apps_frosted".into(), Self::appearance_toggle(&apps_card, &tr!("Blur"),
            &tr!("Blur the content behind windows and menus. Keep the matching color tint and your transparency setting.")));
        strengths.insert("apps_frost_strength".into(), Self::frost_strength_control(&apps_card));
        toggles.insert("apps_rounded_corners".into(), Self::appearance_toggle(&apps_card, &tr!("Rounded corners"),
            &tr!("Round application windows and desktop menus independently of the dock.")));
        toggles.insert("window_buttons_left".into(), Self::appearance_toggle(&apps_card, &tr!("Window buttons on the left"),
            &tr!("Place close, minimize and maximize at the left of title bars, in that order.")));
        toggles.insert("colored_window_buttons".into(), Self::appearance_toggle(&apps_card, &tr!("Colored window buttons"),
            &tr!("Show red, yellow and green buttons. Their symbols appear when the pointer is over them.")));
        toggles.insert("window_shadows".into(), Self::appearance_toggle(&apps_card, &tr!("Window shadows"),
            &tr!("Draw a soft shadow below windows, as if lit from above. Maximized and tiled windows have none.")));
        let status = label("", Some("subtitle"));
        status.set_visible(false);
        content.append(&status);

        let display = crate::util::host_display(host);
        let style_provider = tessel_gtk::utils::add_css(THEME_CSS, Some(&display), gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 12);

        let cards = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let discard_button = gtk::Button::builder().label(tr!("Discard changes")).sensitive(false).build();
        let apply_button = gtk::Button::with_label(&tr!("Apply wallpaper"));
        apply_button.add_css_class("suggested-action");

        let config_path = options.config_path.unwrap_or_else(|| tessel_core::paths::component_config_dir("shell").join("wallpapers.json"));
        let page = Rc::new(Self {
            host: host.clone(), widget, content: content.clone(),
            request: options.request.unwrap_or_else(|| Arc::new(wallpaper_request)), config_path,
            bridge: RefCell::new(None), color_settings, scheme_buttons, scheme_card, scheme_handler: Cell::new(0), icons,
            r#match: match_switch, dock_transparency, dock_transparency_enabled, apps_transparency,
            apps_transparency_enabled, toggles, strengths, status, cards: cards.clone(), discard_button: discard_button.clone(),
            apply_button: apply_button.clone(), rows: RefCell::new(IndexMap::new()), style_provider,
            closed: Cell::new(false), busy: Cell::new(false), updating: Cell::new(false), saving: Cell::new(false),
            pending_appearance: RefCell::new(Map::new()), appearance_source: RefCell::new(None),
            dialogs: RefCell::new(Vec::new()), media_pickers: RefCell::new(Vec::new()), apply_done: RefCell::new(None),
        });
        // Scheme handlers.
        for (value, button) in &page.scheme_buttons {
            let weak = Rc::downgrade(&page);
            let value = value.clone();
            button.connect_toggled(move |button| {
                if let Some(page) = weak.upgrade() {
                    page.scheme_toggled(button, &value);
                }
            });
        }
        if let Some(settings) = &page.color_settings {
            let weak = Rc::downgrade(&page);
            let id = settings.connect_changed("color-scheme", Rc::new(move || {
                if let Some(page) = weak.upgrade() {
                    page.sync_scheme();
                }
            }));
            page.scheme_handler.set(id);
            page.sync_scheme();
        }

        let bridge: Rc<dyn ThemeBridge> = match options.bridge {
            Some(bridge) => bridge,
            None => {
                let weak = Rc::downgrade(&page);
                let on_error: Rc<dyn Fn(&str)> = Rc::new(move |message| {
                    if let Some(page) = weak.upgrade() {
                        page.error(message);
                    }
                });
                let options = BridgeOptions { fields: appearance_fields(), read: false, ..BridgeOptions::default() };
                match DockSettingsBridge::new(host.application(), options, Some(on_error)) {
                    Ok(bridge) => Rc::new(bridge),
                    Err(error) => {
                        gtk::style_context_remove_provider_for_display(&display, &page.style_provider);
                        page.icons.close();
                        return Err(error);
                    }
                }
            }
        };
        page.bridge.replace(Some(bridge));
        page.sync_appearance_controls();
        {
            let weak = Rc::downgrade(&page);
            page.r#match.connect_active_notify(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.match_changed();
                }
            });
        }
        for (prefix, field) in [("dock", "transparency"), ("apps", "apps_transparency")] {
            let (toggle, scale) = page.transparency_pair(prefix);
            let weak = Rc::downgrade(&page);
            toggle.connect_active_notify(move |toggle| {
                if let Some(page) = weak.upgrade() {
                    page.transparency_toggled(toggle, prefix, field);
                }
            });
            let weak = Rc::downgrade(&page);
            scale.connect_value_changed(move |scale| {
                if let Some(page) = weak.upgrade() {
                    page.transparency_changed(scale, field);
                }
            });
        }
        for field in TOGGLES {
            let weak = Rc::downgrade(&page);
            page.toggles[field].connect_active_notify(move |toggle| {
                if let Some(page) = weak.upgrade() {
                    page.appearance_toggled(toggle, field);
                }
            });
        }
        for field in STRENGTHS {
            let weak = Rc::downgrade(&page);
            page.strengths[field].connect_value_changed(move |scale| {
                if let Some(page) = weak.upgrade() {
                    page.transparency_changed(scale, field);
                }
            });
        }

        // Wallpapers are chosen per display and applied together.
        let wallpaper = gtk::Box::new(gtk::Orientation::Vertical, 12);
        wallpaper.append(&label(&tr!("Wallpaper"), Some("gs-section-title")));
        wallpaper.append(&label(&tr!("Choose an image, video or color for each display, then press Apply wallpaper."), Some("subtitle")));
        wallpaper.append(&cards);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        actions.append(&gtk::Box::builder().hexpand(true).build());
        {
            let weak = Rc::downgrade(&page);
            discard_button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.discard();
                }
            });
        }
        actions.append(&discard_button);
        {
            let weak = Rc::downgrade(&page);
            apply_button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.apply(None);
                }
            });
        }
        actions.append(&apply_button);
        wallpaper.append(&actions);
        content.append(&wallpaper);
        page.load_rows();
        let bridge = page.bridge();
        if bridge.has_async() {
            let weak = Rc::downgrade(&page);
            bridge.refresh_async(Box::new(move |ok| {
                if let Some(page) = weak.upgrade() {
                    if ok && !page.closed.get() {
                        page.sync_appearance_controls();
                    }
                }
            }));
        }
        Ok(page)
    }

    pub fn bridge(&self) -> Rc<dyn ThemeBridge> {
        self.bridge.borrow().clone().expect("bridge")
    }

    pub fn transparency_pair(&self, prefix: &str) -> (gtk::Switch, gtk::Scale) {
        if prefix == "dock" {
            (self.dock_transparency_enabled.clone(), self.dock_transparency.clone())
        } else {
            (self.apps_transparency_enabled.clone(), self.apps_transparency.clone())
        }
    }

    /// `getattr(page, field)` for a toggle or strength control.
    pub fn toggle(&self, field: &str) -> gtk::Switch { self.toggles[field].clone() }
    pub fn strength(&self, field: &str) -> gtk::Scale { self.strengths[field].clone() }

    fn transparency_controls(content: &gtk::Box, title: &str, detail: &str) -> (gtk::Box, gtk::Scale, gtk::Switch) {
        let card = card(10);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        labels.append(&label(title, None));
        labels.append(&label(detail, Some("subtitle")));
        row.append(&labels);
        let toggle = switch();
        accessible_label(&toggle, title);
        toggle.set_tooltip_text(Some(&tr!("Turn off to use an opaque background without changing the saved level.")));
        row.append(&toggle);
        card.append(&row);
        let scale = percent_scale();
        accessible_label(&scale, title);
        scale.set_tooltip_text(Some(&tr!("0% is opaque; 100% is fully transparent.")));
        card.append(&scale);
        content.append(&card);
        (card, scale, toggle)
    }

    fn appearance_toggle(card: &gtk::Box, title: &str, detail: &str) -> gtk::Switch {
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(16).margin_top(8).build();
        let labels = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(5).hexpand(true).build();
        labels.append(&label(title, None));
        labels.append(&label(detail, Some("subtitle")));
        row.append(&labels);
        let toggle = switch();
        accessible_label(&toggle, title);
        toggle.set_tooltip_text(Some(detail));
        row.append(&toggle);
        card.append(&row);
        toggle
    }

    fn frost_strength_control(card: &gtk::Box) -> gtk::Scale {
        let title = tr!("Blur strength");
        card.append(&label(&title, None));
        let scale = percent_scale();
        accessible_label(&scale, &title);
        scale.set_tooltip_text(Some(&tr!("Adjust the background blur radius independently of transparency. Text and icons stay sharp.")));
        card.append(&scale);
        scale
    }

    pub fn sync_scheme(&self) {
        let Some(settings) = &self.color_settings else { return };
        if self.closed.get() {
            return;
        }
        let value = settings.get_string("color-scheme");
        self.updating.set(true);
        self.scheme_buttons.get(&value).unwrap_or(&self.scheme_buttons["default"]).set_active(true);
        self.updating.set(false);
    }

    fn scheme_toggled(&self, button: &gtk::ToggleButton, value: &str) {
        if self.updating.get() || self.closed.get() || !button.is_active() {
            return;
        }
        let Some(settings) = &self.color_settings else { return };
        if settings.get_string("color-scheme") != value && !settings.set_string("color-scheme", value) {
            self.error(&tr!("The color scheme could not be changed."));
            self.sync_scheme();
        }
    }

    fn sync_material_controls(&self) {
        for (field, strength) in [("frosted", "frost_strength"), ("apps_frosted", "apps_frost_strength")] {
            self.strengths[strength].set_sensitive(self.toggles[field].is_active());
        }
    }

    pub fn sync_appearance_controls(&self) {
        self.updating.set(true);
        let config = self.bridge().config();
        self.r#match.set_active(config.match_wallpaper);
        for (prefix, field) in [("dock", "transparency"), ("apps", "apps_transparency")] {
            let enabled = config_bool(&config, &format!("{field}_enabled"));
            let (toggle, scale) = self.transparency_pair(prefix);
            toggle.set_active(enabled);
            scale.set_value(config_number(&config, field));
            scale.set_sensitive(enabled);
        }
        for field in TOGGLES {
            self.toggles[field].set_active(config_bool(&config, field));
        }
        for field in STRENGTHS {
            self.strengths[field].set_value(config_number(&config, field));
        }
        self.sync_material_controls();
        self.updating.set(false);
    }

    fn transparency_toggled(self: &Rc<Self>, toggle: &gtk::Switch, prefix: &str, field: &str) {
        if self.updating.get() || self.closed.get() {
            return;
        }
        let enabled = toggle.is_active();
        self.transparency_pair(prefix).1.set_sensitive(enabled);
        self.sync_material_controls();
        self.pending_appearance.borrow_mut().insert(format!("{field}_enabled"), enabled.into());
        self.flush_appearance();
    }

    fn appearance_toggled(self: &Rc<Self>, toggle: &gtk::Switch, field: &str) {
        if self.updating.get() || self.closed.get() {
            return;
        }
        self.pending_appearance.borrow_mut().insert(field.into(), toggle.is_active().into());
        self.sync_material_controls();
        self.flush_appearance();
    }

    fn transparency_changed(self: &Rc<Self>, scale: &gtk::Scale, field: &str) {
        if self.updating.get() || self.closed.get() {
            return;
        }
        self.pending_appearance.borrow_mut().insert(field.into(), py_round(scale.value()).into());
        if let Some(id) = self.appearance_source.take() {
            id.remove();
        }
        // Coalesce a drag into one save.
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            if let Some(page) = weak.upgrade() {
                page.appearance_source.take();
                page.flush_appearance();
            }
            glib::ControlFlow::Break
        });
        self.appearance_source.replace(Some(id));
    }

    /// Python `page._appearance_source != 0`.
    pub fn appearance_pending_timer(&self) -> bool { self.appearance_source.borrow().is_some() }

    pub fn flush_appearance(self: &Rc<Self>) {
        if let Some(id) = self.appearance_source.take() {
            id.remove();
        }
        if self.pending_appearance.borrow().is_empty() || self.saving.get() {
            return; // A save in progress flushes the rest when done.
        }
        let changes = std::mem::take(&mut *self.pending_appearance.borrow_mut());
        let bridge = self.bridge();
        let mut desired = bridge.config();
        for (field, value) in &changes {
            set_appearance_field(&mut desired, field, value);
        }
        if !bridge.has_async() {
            let ok = bridge.save_config(&desired);
            self.saved(ok);
        } else {
            // The dock's D-Bus Apply can take seconds; never wait on the GTK thread.
            self.saving.set(true);
            let weak = Rc::downgrade(self);
            bridge.save_config_async(desired, Box::new(move |ok| {
                if let Some(page) = weak.upgrade() {
                    page.saved(ok);
                }
            }));
        }
    }

    fn saved(self: &Rc<Self>, ok: bool) {
        self.saving.set(false);
        if self.closed.get() {
            return;
        }
        if !ok {
            // Show what is really saved: revert every control.
            self.pending_appearance.borrow_mut().clear();
            let bridge = self.bridge();
            if bridge.has_async() {
                let weak = Rc::downgrade(self);
                bridge.refresh_async(Box::new(move |_ok| {
                    if let Some(page) = weak.upgrade() {
                        if !page.closed.get() {
                            page.sync_appearance_controls();
                        }
                    }
                }));
            } else {
                bridge.refresh();
            }
            self.sync_appearance_controls();
        } else if !self.pending_appearance.borrow().is_empty() && self.appearance_source.borrow().is_none() {
            self.flush_appearance();
        }
    }

    pub fn error(&self, message: &str) {
        if !self.closed.get() {
            self.status.set_text(message);
            self.status.set_visible(true);
        }
    }

    fn read_config(&self) -> Map<String, Value> {
        let data = match std::fs::read(&self.config_path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Map::new(),
            Err(error) => {
                self.error(&tessel_core::pyerr::os_error_path(&error, &self.config_path));
                return Map::new();
            }
        };
        let text = match String::from_utf8(data) {
            Ok(text) => text,
            Err(error) => {
                self.error(&error.to_string());
                return Map::new();
            }
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                self.error(&tr!("Wallpaper settings must be an object."));
                Map::new()
            }
            Err(error) => {
                self.error(&error.to_string());
                Map::new()
            }
        }
    }

    fn load_rows(self: &Rc<Self>) {
        let saved = self.read_config();
        let monitors = crate::util::host_display(&self.host).monitors();
        for index in 0..monitors.n_items() {
            let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() else { continue };
            let connector = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty())
                .unwrap_or_else(|| format!("Monitor-{}", index + 1));
            let previous = saved.get(&connector).and_then(Value::as_object).cloned().unwrap_or_default();
            let card = card(12);
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let icon = gtk::Image::from_icon_name("video-display-symbolic");
            icon.set_pixel_size(24);
            heading.append(&icon);
            let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
            let description = monitor.description().map(|d| d.to_string()).filter(|d| !d.is_empty())
                .unwrap_or_else(|| connector.clone());
            titles.append(&label(&description, Some("gs-section-title")));
            titles.append(&label(&connector, Some("subtitle")));
            heading.append(&titles);
            card.append(&heading);
            let grid = gtk::Grid::builder().row_spacing(10).column_spacing(20).build();
            let chooser = gtk::Button::builder().hexpand(true).build();
            let translated: Vec<String> = MODES.iter().map(|m| tr!(*m)).collect();
            let refs: Vec<&str> = translated.iter().map(String::as_str).collect();
            let modes = gtk::DropDown::from_strings(&refs);
            modes.set_hexpand(true);
            let mode = previous.get("mode").and_then(Value::as_str).unwrap_or("Fill");
            modes.set_selected(MODES.iter().position(|m| *m == mode).unwrap_or(0) as u32);
            let color_text = previous.get("color").and_then(Value::as_str).unwrap_or("black");
            let rgba = gdk::RGBA::parse(color_text).unwrap_or_else(|_| gdk::RGBA::parse("black").unwrap());
            let colors = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().title(tr!("Background color")).build()));
            colors.set_rgba(&rgba);
            colors.set_halign(gtk::Align::Start);
            let controls: [(String, gtk::Widget); 3] = [(tr!("Image or video"), chooser.clone().upcast()),
                (tr!("Placement"), modes.clone().upcast()), (tr!("Background color"), colors.clone().upcast())];
            for (number, (title, control)) in controls.iter().enumerate() {
                let caption = label(title, None);
                caption.set_mnemonic_widget(Some(control));
                grid.attach(&caption, 0, number as i32, 1, 1);
                grid.attach(control, 1, number as i32, 1, 1);
            }
            card.append(&grid);
            self.cards.append(&card);
            let mut path = previous.get("file_path").and_then(Value::as_str).map(str::to_owned);
            if !saved.contains_key(&connector) {
                // Nothing chosen for this screen yet: it shows the default wallpaper.
                let size = monitor.geometry();
                path = tessel_core::wallpaper::default_wallpaper(Some((size.width(), size.height()))).map(|p| p.to_string_lossy().into_owned());
            }
            let row = Rc::new(WallpaperRow { chooser: chooser.clone(), mode: modes.clone(), color: colors.clone(),
                                             path: RefCell::new(path), dirty: Cell::new(false) });
            self.rows.borrow_mut().insert(connector.clone(), row);
            self.update_media_label(&connector);
            {
                let weak = Rc::downgrade(self);
                let connector = connector.clone();
                chooser.connect_clicked(move |_| {
                    if let Some(page) = weak.upgrade() {
                        page.choose_media(&connector);
                    }
                });
            }
            for kind in ["selected", "rgba"] {
                let weak = Rc::downgrade(self);
                let connector = connector.clone();
                let mark = move || {
                    if let Some(page) = weak.upgrade() {
                        page.mark_dirty(&connector);
                    }
                };
                if kind == "selected" {
                    modes.connect_selected_notify(move |_| mark());
                } else {
                    colors.connect_rgba_notify(move |_| mark());
                }
            }
        }
        if self.rows.borrow().is_empty() {
            self.cards.append(&label(&tr!("No displays found."), Some("subtitle")));
        }
        self.apply_button.set_sensitive(!self.rows.borrow().is_empty());
    }

    fn mark_dirty(&self, connector: &str) {
        if let Some(row) = self.rows.borrow().get(connector) {
            row.dirty.set(true);
        }
        self.update_wallpaper_buttons();
    }

    pub fn dirty(&self) -> bool {
        self.rows.borrow().values().any(|row| row.dirty.get())
    }

    fn update_wallpaper_buttons(&self) {
        self.discard_button.set_sensitive(self.dirty() && !self.busy.get());
    }

    /// Forget wallpaper choices that were not applied.
    pub fn discard(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        crate::util::clear_box(&self.cards);
        self.rows.borrow_mut().clear();
        self.load_rows();
        self.update_wallpaper_buttons();
    }

    fn update_media_label(&self, connector: &str) {
        let rows = self.rows.borrow();
        let Some(row) = rows.get(connector) else { return };
        let path = row.path.borrow().clone().filter(|p| !p.is_empty());
        let name = path.as_ref().map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        row.chooser.set_label(&name.unwrap_or_else(|| tr!("Choose image or video…")));
        row.chooser.set_tooltip_text(Some(&path.clone().unwrap_or_else(|| tr!("Choose an image, animation, or video"))));
        if let Some(child) = row.chooser.child().and_downcast::<gtk::Label>() {
            child.set_ellipsize(pango::EllipsizeMode::Middle);
            child.set_max_width_chars(38);
        }
    }

    /// Stage a path for a display (what the chooser's callback does).
    pub fn choose_path(&self, connector: &str, path: &str) {
        if self.closed.get() || path.is_empty() {
            return;
        }
        let found = match self.rows.borrow().get(connector) {
            Some(row) => {
                row.path.replace(Some(path.to_owned()));
                row.dirty.set(true);
                true
            }
            None => false,
        };
        if found {
            self.update_media_label(connector);
            self.update_wallpaper_buttons();
        }
    }

    pub fn choose_media(self: &Rc<Self>, connector: &str) {
        let cancellable = gio::Cancellable::new();
        self.dialogs.borrow_mut().push(cancellable.clone());
        let weak = Rc::downgrade(self);
        let connector = connector.to_owned();
        let token = cancellable.clone();
        let options = tessel_shell::wallpaper_chooser::Options { cancellable: Some(cancellable.clone()), ..Default::default() };
        let parent = self.host.window();
        let picker = tessel_shell::wallpaper_chooser::WallpaperChooser::with_options(Some(&parent), move |path| {
            let Some(page) = weak.upgrade() else { return };
            page.dialogs.borrow_mut().retain(|c| c != &token);
            page.media_pickers.borrow_mut().retain(|(c, _)| c != &token);
            if let Some(path) = path {
                page.choose_path(&connector, &path.to_string_lossy());
            }
        }, options);
        self.media_pickers.borrow_mut().push((cancellable, picker.clone()));
        tessel_gtk::theme::attach(picker.window(), None);
        picker.present();
    }

    fn match_changed(self: &Rc<Self>) {
        if self.updating.get() || self.closed.get() {
            return;
        }
        self.pending_appearance.borrow_mut().insert("match_wallpaper".into(), self.r#match.is_active().into());
        self.flush_appearance();
    }

    /// `apply(done=None)`.
    pub fn apply(self: &Rc<Self>, done: Option<Box<dyn FnOnce(bool)>>) {
        if self.busy.get() || self.closed.get() {
            return;
        }
        self.apply_done.replace(done);
        let mut updates = Vec::new();
        for (connector, row) in self.rows.borrow().iter() {
            if !row.dirty.get() {
                continue;
            }
            let mode = MODES[(row.mode.selected() as usize).min(MODES.len() - 1)];
            let path = row.path.borrow().clone().filter(|p| !p.is_empty());
            if mode != "Color Only" && !path.as_ref().is_some_and(|p| Path::new(p).is_file()) {
                self.error(&tr!("Choose an existing image or video for {monitor}.", monitor = connector.as_str()));
                self.finish_apply(false);
                return;
            }
            let file = if mode == "Color Only" { Value::Null } else { Value::from(path.unwrap_or_default()) };
            updates.push(json!({"cmd": "set", "monitor": connector, "file": file, "mode": mode,
                                "color": row.color.rgba().to_str().to_string()}));
        }
        if updates.is_empty() {
            self.error(&tr!("No wallpaper changes to apply."));
            self.finish_apply(true);
            return;
        }
        self.busy.set(true);
        self.update_wallpaper_buttons();
        self.cards.set_sensitive(false);
        self.apply_button.set_sensitive(false);
        self.error(&tr!("Applying wallpapers…"));
        let request = self.request.clone();
        let weak = Rc::downgrade(self);
        crate::util::Runner::Thread.run(move || apply_worker(&request, &updates), move |result| {
            let (completed, error) = result.unwrap_or_else(|| (Vec::new(), Some("worker failed".into())));
            if let Some(page) = weak.upgrade() {
                page.applied(completed, error);
            }
        });
    }

    fn applied(self: &Rc<Self>, completed: Vec<String>, error: Option<String>) {
        if self.closed.get() {
            return;
        }
        self.busy.set(false);
        self.cards.set_sensitive(true);
        self.apply_button.set_sensitive(!self.rows.borrow().is_empty());
        for connector in &completed {
            if let Some(row) = self.rows.borrow().get(connector) {
                row.dirty.set(false);
            }
        }
        let message = match &error {
            Some(error) if !completed.is_empty() =>
                tr!("Some wallpapers were applied, but another change failed: {error}", error = error.as_str()),
            Some(error) => error.clone(),
            None => tr!("Wallpaper settings saved."),
        };
        self.error(&message);
        self.update_wallpaper_buttons();
        self.finish_apply(error.is_none());
    }

    fn finish_apply(&self, ok: bool) {
        let done = self.apply_done.take();
        if let Some(done) = done {
            done(ok);
        }
    }

    pub fn can_close(self: &Rc<Self>) -> bool {
        if self.busy.get() {
            return false;
        }
        if !self.dirty() {
            return true;
        }
        let names: Vec<String> = self.rows.borrow().iter().filter(|(_, r)| r.dirty.get()).map(|(c, _)| c.clone()).collect();
        let pending = tr!("Wallpaper changes for {displays} have not been applied.", displays = names.join(", "));
        let (apply, discard) = (Rc::downgrade(self), Rc::downgrade(self));
        let host = self.host.clone();
        !confirm_leave(&self.widget, &pending,
            move |done| match apply.upgrade() {
                Some(page) => page.apply(Some(done)),
                None => done(false),
            },
            move || {
                if let Some(page) = discard.upgrade() {
                    page.discard();
                }
            },
            Rc::new(move || host.window().close()))
    }

    pub fn activate(self: &Rc<Self>) {
        self.flush_appearance();
        self.sync_scheme();
        let bridge = self.bridge();
        if bridge.has_async() {
            if !self.saving.get() {
                let weak = Rc::downgrade(self);
                bridge.refresh_async(Box::new(move |ok| {
                    let Some(page) = weak.upgrade() else { return };
                    // Never overwrite edits made or saved while the read was running.
                    if ok && !page.closed.get() && !page.saving.get() && page.pending_appearance.borrow().is_empty() {
                        page.sync_appearance_controls();
                    }
                }));
            }
            return;
        }
        bridge.refresh();
        self.sync_appearance_controls();
    }

    pub fn deactivate(self: &Rc<Self>) {
        self.flush_appearance();
    }

    pub fn close(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        self.flush_appearance();
        self.closed.set(true);
        if let Some(settings) = &self.color_settings {
            let id = self.scheme_handler.replace(0);
            if id != 0 {
                settings.disconnect(id);
            }
        }
        self.icons.close();
        gtk::style_context_remove_provider_for_display(&crate::util::host_display(&self.host), &self.style_provider);
        for cancellable in self.dialogs.take() {
            cancellable.cancel();
        }
        self.media_pickers.borrow_mut().clear();
        self.bridge().close();
    }

    /// Search spotlight targets under the attribute names settings_search.py uses.
    pub fn register_targets(&self, page: &SettingsPage) {
        page.add_target("match", &self.r#match);
        page.add_target("icon_theme", &self.icons.dropdown);
        page.add_target("dock_transparency", &self.dock_transparency);
        page.add_target("apps_transparency", &self.apps_transparency);
        for field in ["frosted", "apps_frosted", "apps_rounded_corners"] {
            page.add_target(field, &self.toggles[field]);
        }
        for field in STRENGTHS {
            page.add_target(field, &self.strengths[field]);
        }
    }
}

/// `_apply_worker(updates)`: list-monitors preflight, then one set per display.
fn apply_worker(request: &WallpaperRequest, updates: &[Value]) -> (Vec<String>, Option<String>) {
    let mut completed = Vec::new();
    let result = (|| -> Result<(), String> {
        let reply = request(&json!({"cmd": "list-monitors"}))?;
        let available: Vec<String> = reply.get("monitors").and_then(Value::as_array)
            .map(|items| items.iter().map(tessel_core::pyjson::py_str).collect()).unwrap_or_default();
        if updates.iter().any(|u| !available.iter().any(|a| Some(a.as_str()) == u["monitor"].as_str())) {
            return Err(tr!("A selected display was disconnected. Reopen this page and try again."));
        }
        for update in updates {
            request(update)?;
            completed.push(update["monitor"].as_str().unwrap_or_default().to_owned());
        }
        Ok(())
    })();
    (completed, result.err())
}

/// `build_page("theme", host)` with injection.
pub fn build_theme_page(host: &Host, options: ThemeOptions) -> Result<SettingsPage, String> {
    let controller = ThemeWallpaperPage::new(host, options)?;
    let page = SettingsPage::new(controller.widget.clone());
    let (c1, c2, c3, c4) = (controller.clone(), controller.clone(), controller.clone(), controller.clone());
    page.set_close(move || c1.close());
    page.set_activate(move || c2.activate());
    page.set_deactivate(move || c3.deactivate());
    page.set_can_close(move || c4.can_close());
    controller.register_targets(&page);
    page.set_controller(controller);
    Ok(page)
}

