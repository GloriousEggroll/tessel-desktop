//! Native Dock Settings window for `tessel-dock --settings` (port of
//! tessel-dock.py `SettingsWindow`, W-g; contract §6 CSS classes, §8 schema).
//!
//! The window talks to the running dock only through [`SettingsApp`] (the
//! Python names it calls on `app`). It never writes files itself: Apply builds
//! the same `DockConfig` Python's `dataclasses.replace(self.app.config, …)`
//! builds and hands it to [`SettingsApp::save_config`] (→ `ConfigStore.save`).
//!
//! API:
//! - [`SettingsApp`] — implement in the application (documented per method).
//! - [`SettingsWindow::new(app, SettingsOptions)`] builds one window
//!   (`SettingsOptions::default()` = standalone `section="all"`, not embedded).
//!   `.window()`, `.present()`, `.destroy()`, `.apply()` (Apply button),
//!   `.cancel()` (Cancel/Reset button), `.controls()` (every control under its
//!   Python attribute name, for tests and integration), `.tray_switches()`,
//!   `.refresh_tray_rows()`, `.refresh_tray_list()`, `.mode_appearance()`.
//! - [`show_settings(slot, app)`] — `Tessel Dock.show_settings` without
//!   `close_popups()`: presents the existing single instance or creates one and
//!   clears `slot` when it is unrealized (call `close_popups` first).
//! - [`error_dialog`] — `Tessel Dock.show_error`'s dialog (`Dock` + secondary text,
//!   `tessel-dock-settings` class, palette theme attached) for the app's `show_error`.
//! - [`ExtraControls`] — Python `extra_controls` (`.widget`, `.apply()`).
//!
//! CSS classes (exact): `tessel-dock-settings` (window), `settings-tile`,
//! `settings-title`, `settings-muted`, `settings-card`,
//! `settings-section-heading`, `settings-section-title`, `settings-hint`,
//! `suggested-action` (Apply). The styles come from the application's
//! `appearance::style::CSS` display provider, as in Python.
//!
//! `section`/`embedded` (Global Settings keeps embedding the *Python* window):
//! `section` = `"all"` | `"dock"` | `"desktop"` hides the other cards
//! (`shortcuts`/`hot_corners` belong to desktop) and picks the page title;
//! embedded + `"dock"` omits the Match wallpaper row; embedded changes Cancel
//! into Reset (calls `reset`) and Apply keeps the window and shows
//! "Settings saved."; only standalone windows attach the palette theme.

#![allow(deprecated)] // ComboBoxText/MessageDialog: kept for parity with the GTK4 Python UI.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use indexmap::IndexMap;
use tessel_core::tr;

use super::group_layout_settings::GroupLayoutControls;
use crate::core::config::{DockConfig, IntHint, BORDER_SIDES, CORNERS, DEFAULT_APPS, GROUPS};
use crate::core::group_layout::APP_DOCK_GROUPS;
use crate::core::ordered::OrderedMap;
use crate::core::tray_prefs::TrayEntry;

/// The dock application as seen by the settings window. Python names; the
/// window calls only these.
pub trait SettingsApp {
    /// `app.config`: the current saved preferences (read again on every Apply
    /// and availability check, exactly like Python's attribute access).
    fn config(&self) -> DockConfig;
    /// `app.save_config(config, rebuild=True)`: validate + save through the
    /// `ConfigStore`, rebuild docks, refresh backgrounds; on error show
    /// `tr('Could not save dock settings: {value1}')` with [`SettingsApp::show_error`]
    /// and return `false` (the window then keeps the edits open).
    fn save_config(&self, config: DockConfig, rebuild: bool) -> bool;
    /// `app.preview_appearance(transparency, match_wallpaper, rounded_corners,
    /// float_mode, transparency_enabled=, frosted=, frost_strength=,
    /// bar_border_enabled=, bar_border_sides=, bar_border_color=)` — a live,
    /// unsaved preview of the dock backgrounds. The window always passes every value.
    fn preview_appearance(&self, preview: &AppearancePreview);
    /// `app.restore_appearance()`: drop the preview (called when the window
    /// is unrealized: Cancel, Apply, titlebar close).
    fn restore_appearance(&self);
    /// `app.monitor_entries()` as `(name, label)` pairs (the GdkMonitor of
    /// the Python triple is not used by the window).
    fn monitor_entries(&self) -> Vec<(String, String)>;
    /// `tray_entries(app.tray_backend)` when the tray backend exists, else
    /// `app.tray_apps` (Python's `getattr(app, "tray_apps", [])`).
    fn tray_entries(&self) -> Vec<TrayEntry>;
    /// `app.refresh_tray_entries(callback)` when the app has one (re-query
    /// the tray, then call `done`). Default: Python's fallback, call `done` now.
    fn refresh_tray_entries(&self, done: Box<dyn FnOnce()>) {
        done()
    }
    /// `app.tray_backend.subscribe(callback)`; `None` when there is no tray
    /// backend (Python `getattr(app, "tray_backend", None) is None`).
    fn subscribe_tray(&self, _callback: Rc<dyn Fn()>) -> Option<u64> {
        None
    }
    /// `app.tray_backend.unsubscribe(callback)` for the id `subscribe_tray` returned.
    fn unsubscribe_tray(&self, _id: u64) {}
    /// `app.appearance.is_dark`, `None` without an AppearanceController
    /// (then GTK's prefer-dark / theme name decide the colour-picker default).
    fn appearance_is_dark(&self) -> Option<bool> {
        None
    }
    /// The `Gtk.Application` the window belongs to.
    fn application(&self) -> Option<gtk::Application> {
        None
    }
    /// `app.show_error(message)` (the window itself never calls it; listed
    /// because `save_config` reports through it — see [`error_dialog`]).
    fn show_error(&self, message: &str);
}

/// Arguments of `app.preview_appearance(...)`.
#[derive(Clone, Debug, PartialEq)]
pub struct AppearancePreview {
    pub transparency: i64,
    pub match_wallpaper: bool,
    pub rounded_corners: bool,
    pub float_mode: bool,
    pub transparency_enabled: bool,
    pub frosted: bool,
    pub frost_strength: i64,
    pub bar_border_enabled: bool,
    pub bar_border_sides: Vec<String>,
    pub bar_border_color: String,
}

/// Python `extra_controls` (Global Settings only): a widget appended before
/// the Shortcuts card and an `apply()` that may veto saving.
pub trait ExtraControls {
    fn widget(&self) -> gtk::Widget;
    fn apply(&self) -> bool;
}

/// `SettingsWindow(app, section=…, embedded=…, reset=…, extra_controls=…)`.
#[derive(Clone)]
pub struct SettingsOptions {
    pub section: String,
    pub embedded: bool,
    pub reset: Option<Rc<dyn Fn()>>,
    pub extra_controls: Option<Rc<dyn ExtraControls>>,
}

impl Default for SettingsOptions {
    fn default() -> Self {
        Self { section: "all".into(), embedded: false, reset: None, extra_controls: None }
    }
}

/// Python round(): ties to even.
fn py_round(value: f64) -> i64 {
    value.round_ties_even() as i64
}

fn accessible_name(widget: &impl IsA<gtk::Accessible>, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

/// gtk4_support.pack: expand along the box orientation, optional padding.
fn pack(container: &gtk::Box, widget: &impl IsA<gtk::Widget>, expand: bool) {
    if container.orientation() == gtk::Orientation::Horizontal {
        widget.set_hexpand(expand);
    } else {
        widget.set_vexpand(expand);
    }
    container.append(widget);
}

fn label(text: &str, css: Option<&str>) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    if let Some(css) = css {
        label.add_css_class(css);
    }
    label
}

fn icon(name: &str, size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(size);
    image
}

fn combo(entries: &[(String, String)], active: &str) -> gtk::ComboBoxText {
    let combo = gtk::ComboBoxText::new();
    for (key, caption) in entries {
        combo.append(Some(key), caption);
    }
    combo.set_active_id(Some(active));
    if combo.active().is_none() {
        combo.set_active(Some(0));
    }
    combo
}

fn active_id(combo: &gtk::ComboBoxText) -> String {
    combo.active_id().map(|id| id.to_string()).unwrap_or_default()
}

fn percent_scale(minimum: f64, maximum: f64, step: f64) -> gtk::Scale {
    gtk::Scale::with_range(gtk::Orientation::Horizontal, minimum, maximum, step)
}

fn percent_format(scale: &gtk::Scale) {
    scale.set_format_value_func(|_scale, value| format!("{value:.0}%"));
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// Refill an alignment combo for an edge (Python nested `alignment()`).
fn fill_alignment(combo: &gtk::ComboBoxText, edge: &str, selected: Option<&str>) {
    let previous = selected
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| combo.active_id().map(|id| id.to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "center".to_owned());
    combo.remove_all();
    let labels = if edge == "top" || edge == "bottom" {
        [tr!("Left"), tr!("Centered"), tr!("Right")]
    } else {
        [tr!("Top"), tr!("Centered"), tr!("Bottom")]
    };
    for (key, caption) in ["start", "center", "end"].iter().zip(labels) {
        combo.append(Some(key), &caption);
    }
    combo.set_active_id(Some(&previous));
}

/// `color_picker(color, title)`: a ColorDialogButton plus Python's
/// `_saved_color` (the configured value until the colour is edited).
#[derive(Clone)]
pub struct ColorPicker {
    pub button: gtk::ColorDialogButton,
    saved: Rc<RefCell<Option<String>>>,
}

impl ColorPicker {
    fn new(color: Option<&str>, title: &str, dark: bool) -> Self {
        let dialog = gtk::ColorDialog::new();
        dialog.set_title(title);
        dialog.set_with_alpha(false);
        let button = gtk::ColorDialogButton::new(Some(dialog));
        button.set_halign(gtk::Align::End);
        let fallback = if dark { "#000000" } else { "#fafafc" };
        let text = color.unwrap_or(fallback);
        if let Ok(rgba) = gdk::RGBA::parse(text) {
            button.set_rgba(&rgba);
        }
        let saved = Rc::new(RefCell::new(color.map(str::to_owned)));
        // Preserve the automatic light/dark default until explicitly edited.
        let target = saved.clone();
        button.connect_rgba_notify(move |button| {
            let current = button.rgba();
            let channel = |c: f32| py_round(c as f64 * 255.0);
            *target.borrow_mut() =
                Some(format!("#{:02x}{:02x}{:02x}", channel(current.red()), channel(current.green()), channel(current.blue())));
        });
        accessible_name(&button, title);
        Self { button, saved }
    }

    /// Python `button._saved_color`.
    pub fn saved_color(&self) -> Option<String> {
        self.saved.borrow().clone()
    }
}

/// One `app_dock_controls` value (combo, switch, spin button or scale).
#[derive(Clone)]
pub enum AppDockControl {
    Combo(gtk::ComboBoxText),
    Switch(gtk::Switch),
    Spin(gtk::SpinButton),
    Scale(gtk::Scale),
}

impl AppDockControl {
    pub fn widget(&self) -> gtk::Widget {
        match self {
            Self::Combo(w) => w.clone().upcast(),
            Self::Switch(w) => w.clone().upcast(),
            Self::Spin(w) => w.clone().upcast(),
            Self::Scale(w) => w.clone().upcast(),
        }
    }
    pub fn switch(&self) -> Option<&gtk::Switch> {
        if let Self::Switch(w) = self { Some(w) } else { None }
    }
    pub fn active(&self) -> bool {
        self.switch().is_some_and(|w| w.is_active())
    }
    pub fn set_active(&self, value: bool) {
        if let Some(w) = self.switch() {
            w.set_active(value);
        }
    }
    pub fn set_value(&self, value: f64) {
        match self {
            Self::Spin(w) => w.set_value(value),
            Self::Scale(w) => w.set_value(value),
            _ => {}
        }
    }
    pub fn value(&self) -> f64 {
        match self {
            Self::Spin(w) => w.value(),
            Self::Scale(w) => w.value(),
            _ => 0.0,
        }
    }
}

/// Every control, under its Python attribute name.
pub struct SettingsControls {
    pub mode: gtk::ComboBoxText,
    pub monitor: gtk::ComboBoxText,
    /// monitor name → (edge, alignment)
    pub edge_widgets: IndexMap<String, (gtk::ComboBoxText, gtk::ComboBoxText)>,
    pub separate_app_dock: gtk::Switch,
    pub app_dock_edge: gtk::ComboBoxText,
    pub app_dock_controls: IndexMap<String, AppDockControl>,
    pub app_dock_expand_label: gtk::Label,
    pub app_dock_background_color: ColorPicker,
    pub app_dock_layout_controls: Rc<GroupLayoutControls>,
    pub size: gtk::SpinButton,
    pub compact_bar: gtk::Switch,
    pub bar_islands: gtk::Switch,
    pub rounded_corners: gtk::Switch,
    pub float_mode: gtk::Switch,
    pub gap: gtk::SpinButton,
    pub gap_control: gtk::Box,
    pub transparency_enabled: gtk::Switch,
    pub transparency: gtk::Scale,
    pub frosted: gtk::Switch,
    pub frost_strength: gtk::Scale,
    pub match_wallpaper: gtk::Switch,
    pub background_color: ColorPicker,
    pub bar_border_enabled: gtk::Switch,
    pub bar_border_switches: IndexMap<String, gtk::Switch>,
    pub bar_border_color: ColorPicker,
    pub application_display: gtk::ComboBoxText,
    pub group_switches: IndexMap<String, gtk::Switch>,
    pub group_labels: IndexMap<String, gtk::Label>,
    pub layout_controls: Rc<GroupLayoutControls>,
    pub default_app_switches: IndexMap<String, gtk::Switch>,
    pub tray_rows: gtk::Box,
    pub tray_refresh: gtk::Button,
    pub fullscreen: gtk::Switch,
    pub auto_hide: gtk::Switch,
    pub magnification: gtk::Switch,
    pub magnification_size: gtk::Scale,
    pub menu_hotkey_fullscreen: gtk::Switch,
    /// monitor name → corner → switch
    pub hot_corner_switches: IndexMap<String, IndexMap<String, gtk::Switch>>,
    pub hot_corner_delay: gtk::SpinButton,
    pub hint_row: gtk::Box,
    pub result_label: gtk::Label,
    pub cancel: gtk::Button,
    pub apply: gtk::Button,
    pub section_cards: IndexMap<String, gtk::Box>,
}

struct State {
    mode_appearance: HashMap<String, (bool, bool)>,
    displayed_mode: String,
    tray_choices: BTreeSet<String>,
    tray_switches: IndexMap<String, gtk::Switch>,
    tray_rows_signature: Option<Vec<(String, String, bool)>>,
}

struct Inner {
    window: glib::WeakRef<gtk::ApplicationWindow>,
    app: Rc<dyn SettingsApp>,
    section_mode: String,
    embedded: bool,
    reset_page: Option<Rc<dyn Fn()>>,
    extra_controls: Option<Rc<dyn ExtraControls>>,
    c: SettingsControls,
    state: RefCell<State>,
    switching_mode: Cell<bool>,
    tray_subscription: Cell<Option<u64>>,
}

/// Row/section builder state (`self.grid`, `self.row_number`).
struct Builder {
    section_mode: String,
    grid: gtk::Grid,
    row: i32,
    cards: IndexMap<String, gtk::Box>,
}

impl Builder {
    fn section(&mut self, content: &gtk::Box, title: &str, icon_name: &str, key: &str) {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 14);
        self.cards.insert(key.to_owned(), card.clone());
        let belongs = if key == "shortcuts" || key == "hot_corners" { "desktop" } else { "dock" };
        card.set_visible(self.section_mode == "all" || self.section_mode == belongs);
        card.add_css_class("settings-card");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        heading.add_css_class("settings-section-heading");
        pack(&heading, &icon(icon_name, 18), false);
        pack(&heading, &label(title, Some("settings-section-title")), false);
        pack(&card, &heading, false);
        self.grid = gtk::Grid::new();
        self.grid.set_column_spacing(16);
        self.grid.set_row_spacing(12);
        self.row = 0;
        pack(&card, &self.grid, false);
        pack(content, &card, false);
    }

    fn row(&mut self, title: &str, widget: &impl IsA<gtk::Widget>, detail: Option<&str>) -> gtk::Label {
        let caption = label(title, None);
        caption.set_ellipsize(pango::EllipsizeMode::End);
        caption.set_max_width_chars(27);
        caption.set_tooltip_text(Some(title));
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
        labels.set_hexpand(true);
        labels.set_valign(gtk::Align::Center);
        pack(&labels, &caption, false);
        if let Some(detail) = detail.filter(|d| !d.is_empty()) {
            let description = label(detail, Some("settings-muted"));
            description.set_wrap(true);
            description.set_max_width_chars(42);
            pack(&labels, &description, false);
        }
        widget.set_valign(gtk::Align::Center);
        self.grid.attach(&labels, 0, self.row, 1, 1);
        self.grid.attach(widget, 1, self.row, 1, 1);
        self.row += 1;
        caption
    }

    fn full(&mut self, widget: &impl IsA<gtk::Widget>) {
        self.grid.attach(widget, 0, self.row, 2, 1);
        self.row += 1;
    }
}

fn pixels(spin: &gtk::SpinButton, name: &str) -> gtk::Box {
    spin.set_numeric(true);
    spin.set_width_chars(3);
    spin.set_max_width_chars(3);
    accessible_name(spin, &tr!("{value1} in pixels", value1 = name));
    let container = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    container.set_halign(gtk::Align::End);
    pack(&container, spin, false);
    pack(&container, &label(&tr!("px"), Some("settings-muted")), false);
    container
}

fn switch(active: bool) -> gtk::Switch {
    let toggle = gtk::Switch::new();
    toggle.set_active(active);
    toggle.set_halign(gtk::Align::End);
    toggle
}

fn is_dark_default(app: &dyn SettingsApp) -> bool {
    if let Some(dark) = app.appearance_is_dark() {
        return dark;
    }
    let Some(settings) = gtk::Settings::default() else { return false };
    settings.is_gtk_application_prefer_dark_theme()
        || settings.gtk_theme_name().map(|n| n.to_lowercase().contains("dark")).unwrap_or(false)
}

#[derive(Clone)]
pub struct SettingsWindow {
    window: gtk::ApplicationWindow,
    inner: Rc<Inner>,
}

impl SettingsWindow {
    pub fn new(app: Rc<dyn SettingsApp>, options: SettingsOptions) -> Self {
        let config = app.config();
        let section = options.section.clone();
        let embedded = options.embedded;
        let window = gtk::ApplicationWindow::builder().title(tr!("Dock Settings")).build();
        if let Some(application) = app.application() {
            window.set_application(Some(&application));
        }
        window.set_default_size(580, 700);
        window.set_size_request(490, 440);
        window.add_css_class("tessel-dock-settings");
        let header = gtk::HeaderBar::new();
        header.set_show_title_buttons(true);
        header.set_title_widget(Some(&gtk::Label::new(Some(&tr!("Dock Settings")))));
        window.set_titlebar(Some(&header));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        root.set_margin_top(20);
        root.set_margin_bottom(20);
        root.set_margin_start(20);
        root.set_margin_end(20);
        window.set_child(Some(&root));
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tile.add_css_class("settings-tile");
        pack(&tile, &icon("view-app-grid-symbolic", 26), true);
        pack(&heading, &tile, false);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let desktop = section == "desktop";
        let page_title = if desktop { tr!("Desktop Behavior") } else { tr!("Dock & Panel") };
        pack(&titles, &label(&page_title, Some("settings-title")), false);
        let subtitle = label(
            &if desktop {
                tr!("Configure workspaces, shortcuts, and hot corners.")
            } else {
                tr!("Choose where your dock lives and how it behaves.")
            },
            Some("settings-muted"),
        );
        subtitle.set_wrap(true);
        pack(&titles, &subtitle, false);
        pack(&heading, &titles, true);
        pack(&root, &heading, false);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_hexpand(true);
        scroll.set_vexpand(true);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.set_margin_end(4);
        scroll.set_child(Some(&content));
        pack(&root, &scroll, true);

        let mut b = Builder { section_mode: section.clone(), grid: gtk::Grid::new(), row: 0, cards: IndexMap::new() };
        let dark = is_dark_default(app.as_ref());

        // Placement
        b.section(&content, &tr!("Placement"), "video-display-symbolic", "placement");
        let mode = combo(&[("dock".into(), tr!("Dock")), ("bar".into(), tr!("Bar"))], &config.mode);
        accessible_name(&mode, &tr!("Layout mode"));
        b.row(&tr!("Layout mode"), &mode, Some(&tr!("Use a compact dock or a bar along the screen edge.")));
        let entries = app.monitor_entries();
        let mut monitor_choices = vec![("all".to_owned(), tr!("Clone on all monitors")), ("primary".to_owned(), tr!("Primary monitor"))];
        monitor_choices.extend(entries.iter().cloned());
        let monitor = combo(&monitor_choices, &config.monitor);
        accessible_name(&monitor, &tr!("Show dock on"));
        for renderer in monitor.cells() {
            renderer.set_property("ellipsize", pango::EllipsizeMode::End);
            renderer.set_property("max-width-chars", 28i32);
        }
        let labels: HashMap<String, String> = entries.iter().cloned().collect();
        let tooltip_for = {
            let labels = labels.clone();
            move |combo: &gtk::ComboBoxText| {
                let id = combo.active_id().map(|id| id.to_string()).unwrap_or_default();
                labels.get(&id).cloned().or_else(|| combo.active_text().map(|t| t.to_string()))
            }
        };
        {
            let tooltip_for = tooltip_for.clone();
            monitor.connect_changed(move |combo| combo.set_tooltip_text(tooltip_for(combo).as_deref()));
        }
        monitor.set_tooltip_text(
            labels.get(&config.monitor).cloned().or_else(|| monitor.active_text().map(|t| t.to_string())).as_deref(),
        );
        b.row(&tr!("Show dock on"), &monitor, None);
        let mut edge_widgets = IndexMap::new();
        for (name, caption) in &entries {
            let placement = config.for_monitor(name);
            let container = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let edge_choices: Vec<(String, String)> =
                ["bottom", "top", "left", "right"].iter().map(|e| (e.to_string(), tr!(title_case(e)))).collect();
            let edge = combo(&edge_choices, &placement.edge);
            let align = gtk::ComboBoxText::new();
            fill_alignment(&align, &placement.edge, Some(&placement.alignment));
            align.set_sensitive(config.mode == "dock");
            {
                let target = align.clone();
                edge.connect_changed(move |widget| fill_alignment(&target, &active_id(widget), None));
            }
            let edge_title = tr!("Dock edge on {value1}", value1 = caption.as_str());
            edge.set_tooltip_text(Some(&edge_title));
            accessible_name(&edge, &edge_title);
            let align_title = tr!("Dock alignment on {value1}", value1 = caption.as_str());
            align.set_tooltip_text(Some(&align_title));
            accessible_name(&align, &align_title);
            pack(&container, &edge, true);
            pack(&container, &align, true);
            let prefix = format!("{name} · ");
            let detail = (caption != name).then(|| caption.strip_prefix(&prefix).unwrap_or(caption).to_owned());
            b.row(name, &container, detail.as_deref());
            edge_widgets.insert(name.clone(), (edge, align));
        }

        // Separate app dock
        b.section(&content, &tr!("Separate app dock"), "view-dual-symbolic", "app_dock");
        let separate_app_dock = switch(config.separate_app_dock);
        accessible_name(&separate_app_dock, &tr!("Use separate app dock"));
        b.row(
            &tr!("Use separate app dock"),
            &separate_app_dock,
            Some(&tr!("Available in top bar position only. Moves Application menu, pinned applications, open windows, and Home, Settings and Trash to a second dock on the same screen.")),
        );
        let mut app_dock_controls: IndexMap<String, AppDockControl> = IndexMap::new();
        let app_dock_edge_choices: Vec<(String, String)> =
            ["left", "right", "bottom"].iter().map(|e| (e.to_string(), tr!(title_case(e)))).collect();
        let app_dock_edge = combo(&app_dock_edge_choices, &config.app_dock_edge);
        accessible_name(&app_dock_edge, &tr!("App dock position"));
        b.row(&tr!("App dock position"), &app_dock_edge, None);
        app_dock_controls.insert("app_dock_edge".into(), AppDockControl::Combo(app_dock_edge.clone()));
        let mut app_dock_expand_label = None;
        let toggles: [(&str, &str, bool); 9] = [
            ("expand", "Expand to full height", config.app_dock_expand),
            ("float_mode", "Float mode", config.app_dock_float_mode),
            ("rounded_corners", "Rounded corners", config.app_dock_rounded_corners),
            ("transparency_enabled", "Transparency", config.app_dock_transparency_enabled),
            ("frosted", "Blur", config.app_dock_frosted),
            ("match_wallpaper", "Match wallpaper colors", config.app_dock_match_wallpaper),
            ("auto_hide", "Auto-hide", config.app_dock_auto_hide),
            ("shine", "Top highlight", config.app_dock_shine),
            ("shadow", "Drop shadow", config.app_dock_shadow),
        ];
        for (key, title, active) in toggles {
            let control = switch(active);
            accessible_name(&control, &format!("{}: {}", tr!("App dock"), tr!(title)));
            let caption = b.row(&tr!(title), &control, None);
            if key == "expand" {
                app_dock_expand_label = Some(caption);
            }
            app_dock_controls.insert(format!("app_dock_{key}"), AppDockControl::Switch(control));
        }
        let app_dock_background_color =
            ColorPicker::new(Some(&config.app_dock_background_color), &tr!("App dock background color"), dark);
        b.row(&tr!("Background color"), &app_dock_background_color.button, Some(&tr!("Used when Match wallpaper colors is off.")));
        let numbers: [(&str, &str, f64, f64, i64); 4] = [
            ("icon_size", "Icon size", 16.0, 128.0, config.app_dock_icon_size),
            ("gap", "Distance from edge", 0.0, 48.0, config.app_dock_gap),
            ("transparency", "Transparency", 0.0, 100.0, config.app_dock_transparency),
            ("frost_strength", "Blur strength", 0.0, 100.0, config.app_dock_frost_strength),
        ];
        for (key, title, minimum, maximum, value) in numbers {
            let control = if key == "transparency" || key == "frost_strength" {
                let scale = percent_scale(minimum, maximum, 1.0);
                scale.set_draw_value(true);
                scale.set_digits(0);
                scale.set_size_request(165, -1);
                percent_format(&scale);
                AppDockControl::Scale(scale)
            } else {
                let spin = gtk::SpinButton::with_range(minimum, maximum, 1.0);
                spin.set_numeric(true);
                AppDockControl::Spin(spin)
            };
            control.set_value(value as f64);
            let widget = control.widget();
            accessible_name(&widget, &format!("{}: {}", tr!("App dock"), tr!(title)));
            b.row(&tr!(title), &widget, None);
            app_dock_controls.insert(format!("app_dock_{key}"), control);
        }
        b.section(&content, &tr!("App dock group positions and order"), "view-sort-ascending-symbolic", "app_dock_group_layout");
        let app_dock_layout_controls = GroupLayoutControls::new(&config, true);
        b.full(app_dock_layout_controls.widget());

        // Appearance
        b.section(&content, &tr!("Appearance"), "preferences-desktop-theme-symbolic", "appearance");
        let size = gtk::SpinButton::with_range(24.0, 80.0, 4.0);
        size.set_value(config.icon_size as f64);
        b.row(&tr!("Icon size"), &pixels(&size, &tr!("Icon size")), None);
        let compact_bar = switch(config.compact_bar);
        accessible_name(&compact_bar, &tr!("Compact bar"));
        b.row(&tr!("Compact bar"), &compact_bar, Some(&tr!("Use a thin bar with smaller text and status icons. Applies to bars, not docks.")));
        let bar_islands = switch(config.bar_islands);
        accessible_name(&bar_islands, &tr!("Split bar into islands"));
        b.row(&tr!("Split bar into islands"), &bar_islands,
              Some(&tr!("Draw the start, center and end of the bar as separate pieces with the wallpaper showing between them. Applies to bars, not docks.")));
        bar_islands.set_sensitive(config.mode == "bar");
        let rounded_corners = switch(config.effective_rounded_corners());
        accessible_name(&rounded_corners, &tr!("Rounded corners"));
        b.row(&tr!("Rounded corners"), &rounded_corners, Some(&tr!("Round the dock outline.")));
        let float_mode = switch(config.effective_float_mode());
        accessible_name(&float_mode, &tr!("Float mode"));
        b.row(&tr!("Float mode"), &float_mode, Some(&tr!("Keep a gap between the dock and screen edges.")));
        let gap = gtk::SpinButton::with_range(0.0, 48.0, 1.0);
        gap.set_value(config.gap as f64);
        let gap_control = pixels(&gap, &tr!("Distance from edge"));
        gap.set_sensitive(config.effective_float_mode());
        gap_control.set_sensitive(config.effective_float_mode());
        b.row(&tr!("Distance from edge"), &gap_control, None);
        let transparency_enabled = switch(config.transparency_enabled);
        accessible_name(&transparency_enabled, &tr!("Dock transparency"));
        b.row(
            &tr!("Dock transparency"),
            &transparency_enabled,
            Some(&tr!("Use a transparent dock background. Application transparency is set separately in Theme & Wallpaper.")),
        );
        let transparency = percent_scale(0.0, 100.0, 1.0);
        transparency.set_digits(0);
        transparency.set_draw_value(true);
        transparency.set_value_pos(gtk::PositionType::Top);
        transparency.set_value(config.transparency as f64);
        transparency.set_sensitive(config.transparency_enabled);
        transparency.set_size_request(165, -1);
        percent_format(&transparency);
        accessible_name(&transparency, &tr!("Dock background transparency"));
        transparency.set_tooltip_text(Some(&tr!("0% is opaque; 100% is fully transparent. Icons stay opaque.")));
        b.row(&tr!("Transparency"), &transparency, Some(&tr!("Adjust the dock background.")));
        let frosted = switch(config.frosted);
        accessible_name(&frosted, &tr!("Blur"));
        b.row(&tr!("Blur"), &frosted, Some(&tr!("Blur windows and content behind the dock. Application Blur is set separately.")));
        let frost_strength = percent_scale(0.0, 100.0, 1.0);
        frost_strength.set_digits(0);
        frost_strength.set_draw_value(true);
        frost_strength.set_value_pos(gtk::PositionType::Top);
        frost_strength.set_value(config.frost_strength as f64);
        frost_strength.set_size_request(165, -1);
        percent_format(&frost_strength);
        accessible_name(&frost_strength, &tr!("Blur strength"));
        frost_strength.set_tooltip_text(Some(&tr!("Adjust background blur strength. Transparency is controlled separately.")));
        b.row(&tr!("Blur strength"), &frost_strength, Some(&tr!("Adjust background blur without changing transparency.")));
        // update_material_controls()
        frost_strength.set_sensitive(frosted.is_active());
        transparency.set_tooltip_text(Some(&tr!("0% is opaque; 100% is fully transparent. Icons stay opaque.")));
        let match_wallpaper = switch(config.match_wallpaper);
        accessible_name(&match_wallpaper, &tr!("Match wallpaper colors"));
        if !(embedded && section == "dock") {
            b.row(&tr!("Match wallpaper colors"), &match_wallpaper, Some(&tr!("Automatically tint each dock to its monitor’s wallpaper.")));
        }
        let background_color = ColorPicker::new(config.background_color.as_deref(), &tr!("Dock / bar background color"), dark);
        background_color.button.set_sensitive(!config.match_wallpaper);
        b.row(
            &tr!("Background color"),
            &background_color.button,
            Some(&tr!("Used when Match wallpaper colors is off. The separate app dock has its own color.")),
        );

        // Bar border
        b.section(&content, &tr!("Bar border"), "preferences-desktop-display-symbolic", "bar_border");
        let bar_border_enabled = switch(config.bar_border_enabled);
        accessible_name(&bar_border_enabled, &tr!("Show bar border"));
        b.row(&tr!("Show bar border"), &bar_border_enabled, Some(&tr!("Add a thin border in Bar mode. Each edge can be shown separately.")));
        let mut bar_border_switches = IndexMap::new();
        for (side, title) in BORDER_SIDES.iter().zip([tr!("Top"), tr!("Bottom"), tr!("Left"), tr!("Right")]) {
            let toggle = switch(config.bar_border_sides.iter().any(|s| s == side));
            accessible_name(&toggle, &tr!("{value1} border", value1 = title.as_str()));
            b.row(&title, &toggle, None);
            bar_border_switches.insert(side.to_string(), toggle);
        }
        let bar_border_color = ColorPicker::new(Some(&config.bar_border_color), &tr!("Bar border color"), dark);
        b.row(
            &tr!("Border color"),
            &bar_border_color.button,
            Some(&tr!("Follows the wallpaper accent when Match wallpaper colors is on. Choose a custom color when it is off.")),
        );

        // Visible groups
        b.section(&content, &tr!("Visible groups"), "view-grid-symbolic", "groups");
        let application_display = combo(
            &[("icons".into(), tr!("Application icons")), ("title".into(), tr!("Focused window title"))],
            &config.application_display,
        );
        accessible_name(&application_display, &tr!("Application display"));
        b.row(
            &tr!("Application display"),
            &application_display,
            Some(&tr!("Show application icons, or the focused window title with Activities in Bar layout and an application tray in Dock layout.")),
        );
        let mut group_switches = IndexMap::new();
        let mut group_labels = IndexMap::new();
        for (name, title) in GROUPS {
            let title = tr!(title);
            let toggle = switch(config.is_group_visible(name));
            accessible_name(&toggle, &tr!("Show {value1}", value1 = title.as_str()));
            // Media is also the dock's media player watcher: off stops it.
            let detail = (*name == "media").then(|| tr!("Show what is playing in the panel, with playback controls. When off, the panel stops following media players."));
            group_labels.insert(name.to_string(), b.row(&title, &toggle, detail.as_deref()));
            group_switches.insert(name.to_string(), toggle);
        }

        // Group positions
        b.section(&content, &tr!("Group positions and order"), "view-sort-ascending-symbolic", "group_layout");
        let layout_controls = GroupLayoutControls::new(&config, false);
        b.full(layout_controls.widget());

        // Always-visible applets
        b.section(&content, &tr!("Always-visible applets"), "applications-system-symbolic", "default_apps");
        let description = label(
            &tr!("Click More apps in the system tray to show the other applets. Enable Always visible to keep an applet shown."),
            Some("settings-muted"),
        );
        description.set_wrap(true);
        description.set_max_width_chars(56);
        b.full(&description);
        let mut default_app_switches = IndexMap::new();
        for (name, title) in DEFAULT_APPS {
            let toggle = switch(config.always_visible_apps.iter().any(|a| a == name));
            toggle.set_tooltip_text(Some(&tr!("Always visible")));
            accessible_name(&toggle, &tr!("Always visible: {value1}", value1 = tr!(title)));
            b.row(&tr!(title), &toggle, None);
            default_app_switches.insert(name.to_string(), toggle);
        }

        // System tray
        b.section(&content, &tr!("System tray"), "view-more-symbolic", "tray");
        let tray_rows = gtk::Box::new(gtk::Orientation::Vertical, 10);
        b.full(&tray_rows);
        let tray_refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        tray_refresh.set_tooltip_text(Some(&tr!("Refresh running tray applications")));
        accessible_name(&tray_refresh, &tr!("Refresh running tray applications"));
        b.row(
            &tr!("Always show"),
            &tray_refresh,
            Some(&tr!("Selected applications stay shown in the system tray without clicking More apps.")),
        );

        // Behavior
        b.section(&content, &tr!("Behavior"), "preferences-system-symbolic", "behavior");
        let fullscreen = switch(config.hide_on_fullscreen);
        fullscreen.set_tooltip_text(Some(&tr!("Keep the idle dock behind windows and reveal it on hover. Fullscreen apps keep it hidden.")));
        accessible_name(&fullscreen, &tr!("Keep behind windows"));
        b.row(&tr!("Keep behind windows"), &fullscreen, Some(&tr!("Reveal on hover, except in fullscreen.")));
        let auto_hide = switch(config.auto_hide);
        accessible_name(&auto_hide, &tr!("Auto-hide when inactive"));
        b.row(&tr!("Auto-hide when inactive"), &auto_hide, Some(&tr!("Move the pointer over the dock’s area to reveal it.")));
        let magnification = switch(config.magnification);
        accessible_name(&magnification, &tr!("Magnify icons"));
        b.row(&tr!("Magnify icons"), &magnification, Some(&tr!("Enlarge application icons near the pointer. Applies to docks, not bars.")));
        let magnification_size = percent_scale(110.0, 200.0, 5.0);
        let hover = config.hover_scale;
        magnification_size.set_value(if hover <= 1.0 { py_round(hover.max(1.6) * 100.0) } else { py_round(hover * 100.0) } as f64);
        magnification_size.set_hexpand(true);
        magnification_size.set_size_request(180, -1);
        percent_format(&magnification_size);
        magnification_size.set_sensitive(config.magnification);
        {
            let target = magnification_size.clone();
            magnification.connect_active_notify(move |switch| target.set_sensitive(switch.is_active()));
        }
        accessible_name(&magnification_size, &tr!("Magnified size"));
        b.row(&tr!("Magnified size"), &magnification_size, Some(&tr!("The largest size of the icon under the pointer.")));
        if let Some(extra) = &options.extra_controls {
            content.append(&extra.widget());
        }

        // Shortcuts
        b.section(&content, &tr!("Shortcuts"), "input-keyboard-symbolic", "shortcuts");
        let menu_hotkey_fullscreen = switch(config.menu_hotkey_fullscreen);
        accessible_name(&menu_hotkey_fullscreen, &tr!("Application menu hotkey in fullscreen"));
        b.row(
            &tr!("Menu hotkey in fullscreen"),
            &menu_hotkey_fullscreen,
            Some(&tr!("Allow the application menu shortcut while a fullscreen or borderless fullscreen app is focused.")),
        );

        // Hot corners
        b.section(&content, &tr!("Hot corners"), "view-grid-symbolic", "hot_corners");
        let description = label(&tr!("Pause in an enabled corner to open Activities, the list of open windows."), Some("settings-muted"));
        description.set_wrap(true);
        description.set_max_width_chars(56);
        b.full(&description);
        let mut hot_corner_switches = IndexMap::new();
        for (monitor_name, caption) in &entries {
            let selected = config.hot_corners.get(monitor_name).cloned().unwrap_or_default();
            let mut switches = IndexMap::new();
            let heading = label(caption, Some("settings-muted"));
            heading.set_wrap(true);
            heading.set_max_width_chars(48);
            heading.set_margin_top(10);
            b.full(&heading);
            for (corner, corner_caption) in CORNERS {
                let corner_caption = tr!(corner_caption);
                let title = format!("{caption} · {corner_caption}");
                let toggle = gtk::Switch::new();
                toggle.set_active(selected.iter().any(|c| c == corner));
                toggle.set_halign(gtk::Align::End);
                toggle.set_tooltip_text(Some(&tr!("Open Activities from {value1}", value1 = title.as_str())));
                accessible_name(&toggle, &tr!("Activities hot corner: {value1}", value1 = title.as_str()));
                b.row(&corner_caption, &toggle, None);
                switches.insert(corner.to_string(), toggle);
            }
            hot_corner_switches.insert(monitor_name.clone(), switches);
        }
        let hot_corner_delay = gtk::SpinButton::with_range(100.0, 2000.0, 50.0);
        hot_corner_delay.set_value(config.hot_corner_delay_ms as f64);
        hot_corner_delay.set_numeric(true);
        hot_corner_delay.set_width_chars(4);
        hot_corner_delay.set_max_width_chars(4);
        accessible_name(&hot_corner_delay, &tr!("Hot corner activation delay in milliseconds"));
        let delay = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        delay.set_halign(gtk::Align::End);
        pack(&delay, &hot_corner_delay, false);
        pack(&delay, &label(&tr!("ms"), Some("settings-muted")), false);
        b.row(&tr!("Activation delay"), &delay, Some(&tr!("A short delay helps prevent accidental activation.")));
        let hint_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pack(&hint_row, &icon("dialog-information-symbolic", 16), false);
        let hint = label(
            &tr!("Right-click an app to pin, unpin, or reorder it. Drag apps from the menu or a file manager onto the dock to pin them, and drag pinned apps to reorder."),
            Some("settings-muted"),
        );
        hint.set_wrap(true);
        pack(&hint_row, &hint, true);
        hint_row.add_css_class("settings-hint");
        pack(&content, &hint_row, false);
        hint_row.set_visible(section != "desktop");
        let result_label = label("", Some("settings-muted"));
        result_label.set_wrap(true);
        result_label.set_visible(false);
        root.append(&result_label);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let cancel = gtk::Button::with_label(&if embedded { tr!("Reset") } else { tr!("Cancel") });
        cancel.set_size_request(86, -1);
        let apply = gtk::Button::with_label(&tr!("Apply"));
        apply.set_size_request(86, -1);
        apply.add_css_class("suggested-action");
        buttons.set_halign(gtk::Align::End);
        buttons.append(&cancel);
        buttons.append(&apply);
        root.append(&buttons);

        let mut mode_appearance = HashMap::new();
        mode_appearance.insert("dock".to_owned(), (config.rounded_corners, config.float_mode));
        mode_appearance.insert("bar".to_owned(), (config.bar_rounded_corners, config.bar_float_mode));
        let controls = SettingsControls {
            mode,
            monitor,
            edge_widgets,
            separate_app_dock,
            app_dock_edge,
            app_dock_controls,
            app_dock_expand_label: app_dock_expand_label.unwrap(),
            app_dock_background_color,
            app_dock_layout_controls,
            size,
            compact_bar,
            bar_islands,
            rounded_corners,
            float_mode,
            gap,
            gap_control,
            transparency_enabled,
            transparency,
            frosted,
            frost_strength,
            match_wallpaper,
            background_color,
            bar_border_enabled,
            bar_border_switches,
            bar_border_color,
            application_display,
            group_switches,
            group_labels,
            layout_controls,
            default_app_switches,
            tray_rows,
            tray_refresh,
            fullscreen,
            auto_hide,
            magnification,
            magnification_size,
            menu_hotkey_fullscreen,
            hot_corner_switches,
            hot_corner_delay,
            hint_row,
            result_label,
            cancel,
            apply,
            section_cards: b.cards,
        };
        let inner = Rc::new(Inner {
            window: window.downgrade(),
            app: app.clone(),
            section_mode: section,
            embedded,
            reset_page: options.reset.clone(),
            extra_controls: options.extra_controls.clone(),
            c: controls,
            state: RefCell::new(State {
                mode_appearance,
                displayed_mode: config.mode.clone(),
                tray_choices: config.always_show_tray.iter().cloned().collect(),
                tray_switches: IndexMap::new(),
                tray_rows_signature: None,
            }),
            switching_mode: Cell::new(false),
            tray_subscription: Cell::new(None),
        });
        // The window owns the state; handles and closures only borrow it.
        unsafe { window.set_data("tessel-dock-settings-window", inner.clone()) };
        Inner::connect(&inner, &window);
        Self { window, inner }
    }

    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.window
    }
    pub fn present(&self) {
        self.window.present();
    }
    pub fn destroy(&self) {
        self.window.destroy();
    }
    pub fn controls(&self) -> &SettingsControls {
        &self.inner.c
    }
    pub fn apply(&self) {
        self.inner.apply();
    }
    pub fn cancel(&self) {
        self.inner.c.cancel.emit_clicked();
    }
    pub fn refresh_tray_rows(&self) {
        self.inner.refresh_tray_rows();
    }
    pub fn refresh_tray_list(&self) {
        self.inner.refresh_tray_list();
    }
    /// Python `self.tray_switches`.
    pub fn tray_switches(&self) -> IndexMap<String, gtk::Switch> {
        self.inner.state.borrow().tray_switches.clone()
    }
    /// Python `self.tray_choices` (sorted).
    pub fn tray_choices(&self) -> Vec<String> {
        self.inner.state.borrow().tray_choices.iter().cloned().collect()
    }
    /// Python `self._mode_appearance[mode]` = (rounded_corners, float_mode).
    pub fn mode_appearance(&self, mode: &str) -> Option<(bool, bool)> {
        self.inner.state.borrow().mode_appearance.get(mode).copied()
    }
    /// Global Settings (dock_pages.py) sets Python's `controller._switching_mode`
    /// while it syncs shared appearance values, so no preview is sent per control.
    pub fn set_switching_mode(&self, value: bool) {
        self.inner.switching_mode.set(value);
    }
    /// Python `controller.update_material_controls()`.
    pub fn update_material_controls(&self) {
        self.inner.update_material_controls();
    }
    /// Python `controller.preview_appearance()`.
    pub fn preview_appearance(&self) {
        self.inner.preview_appearance();
    }
    /// Python `controller.tray_choices.add(ident)` (Global Settings tests).
    pub fn add_tray_choice(&self, ident: &str) {
        self.inner.state.borrow_mut().tray_choices.insert(ident.to_owned());
    }
    /// Recover the handle of a window built by [`SettingsWindow::new`].
    pub fn from_window(window: &gtk::ApplicationWindow) -> Option<Self> {
        let inner = unsafe { window.data::<Rc<Inner>>("tessel-dock-settings-window") }?;
        let inner = unsafe { inner.as_ref() }.clone();
        Some(Self { window: window.clone(), inner })
    }
}

impl Inner {
    fn connect(this: &Rc<Self>, window: &gtk::ApplicationWindow) {
        let weak = Rc::downgrade(this);
        let call = move |f: fn(&Inner)| {
            let weak = weak.clone();
            move || {
                if let Some(inner) = weak.upgrade() {
                    f(&inner);
                }
            }
        };
        {
            let app = Rc::downgrade(&this.app);
            window.connect_unrealize(move |_| {
                if let Some(app) = app.upgrade() {
                    app.restore_appearance();
                }
            });
        }
        let c = &this.c;
        {
            let f = call(Inner::mode_changed);
            c.mode.connect_changed(move |_| f());
        }
        {
            let f = call(Inner::update_app_dock_controls);
            c.separate_app_dock.connect_active_notify(move |_| f());
        }
        {
            let f = call(Inner::update_app_dock_controls);
            c.app_dock_edge.connect_changed(move |_| f());
        }
        for key in ["float_mode", "transparency_enabled", "frosted", "match_wallpaper", "expand"] {
            let f = call(Inner::update_app_dock_controls);
            if let Some(control) = c.app_dock_controls.get(&format!("app_dock_{key}")).and_then(|c| c.switch()) {
                control.connect_active_notify(move |_| f());
            }
        }
        {
            let f = call(Inner::update_app_dock_controls);
            c.monitor.connect_changed(move |_| f());
        }
        for (edge, _align) in c.edge_widgets.values() {
            let f = call(Inner::update_app_dock_controls);
            edge.connect_changed(move |_| f());
        }
        this.update_app_dock_controls();
        for toggle in c.bar_border_switches.values() {
            let f = call(Inner::preview_appearance);
            toggle.connect_active_notify(move |_| f());
        }
        {
            let f = call(Inner::preview_appearance);
            c.bar_border_enabled.connect_active_notify(move |_| f());
        }
        {
            let f = call(Inner::preview_appearance);
            c.bar_border_color.button.connect_rgba_notify(move |_| f());
        }
        this.update_bar_border_controls();
        {
            let f = call(Inner::preview_appearance);
            c.transparency.connect_value_changed(move |_| f());
        }
        for toggle in [&c.transparency_enabled, &c.frosted, &c.match_wallpaper, &c.rounded_corners, &c.float_mode] {
            let f = call(Inner::preview_appearance);
            toggle.connect_active_notify(move |_| f());
        }
        {
            let f = call(Inner::preview_appearance);
            c.frost_strength.connect_value_changed(move |_| f());
        }
        {
            let f = call(Inner::application_display_changed);
            c.application_display.connect_changed(move |_| f());
        }
        this.application_display_changed();
        this.update_app_dock_controls();
        {
            let f = call(Inner::refresh_tray_list);
            c.tray_refresh.connect_clicked(move |_| f());
        }
        this.refresh_tray_rows();
        {
            let weak = Rc::downgrade(this);
            let callback: Rc<dyn Fn()> = Rc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.refresh_tray_rows();
                }
            });
            if let Some(id) = this.app.subscribe_tray(callback) {
                this.tray_subscription.set(Some(id));
                let weak = Rc::downgrade(this);
                window.connect_unrealize(move |_| {
                    if let Some(inner) = weak.upgrade() {
                        if let Some(id) = inner.tray_subscription.take() {
                            inner.app.unsubscribe_tray(id);
                        }
                    }
                });
            }
        }
        {
            let weak = Rc::downgrade(this);
            c.cancel.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                match (&inner.reset_page, inner.embedded) {
                    (Some(reset), true) => reset(),
                    _ => {
                        if let Some(window) = inner.window.upgrade() {
                            window.destroy();
                        }
                    }
                }
            });
        }
        {
            let f = call(Inner::apply);
            c.apply.connect_clicked(move |_| f());
        }
        if !this.embedded {
            tessel_gtk::theme::attach(window, None);
        }
    }

    fn mode_id(&self) -> String {
        active_id(&self.c.mode)
    }

    fn apply(&self) {
        let c = &self.c;
        if let Some(extra) = &self.extra_controls {
            if !extra.apply() {
                c.result_label.set_visible(false);
                return;
            }
        }
        let mode = self.mode_id();
        self.state
            .borrow_mut()
            .mode_appearance
            .insert(mode.clone(), (c.rounded_corners.is_active(), c.float_mode.is_active()));
        let base = self.app.config();
        let mut overrides = base.monitor_overrides.clone();
        for (name, (edge, alignment)) in &c.edge_widgets {
            let mut value = OrderedMap::new();
            value.insert("edge", active_id(edge));
            value.insert("alignment", active_id(alignment));
            overrides.insert(name.clone(), value);
        }
        let mut hot_corners = base.hot_corners.clone();
        for (monitor, switches) in &c.hot_corner_switches {
            let selected: Vec<String> =
                switches.iter().filter(|(_, toggle)| toggle.is_active()).map(|(corner, _)| corner.clone()).collect();
            if selected.is_empty() {
                hot_corners.remove(monitor);
            } else {
                hot_corners.insert(monitor.clone(), selected);
            }
        }
        let appearance = self.state.borrow().mode_appearance.clone();
        let (dock_rounded, dock_float) = appearance.get("dock").copied().unwrap_or((base.rounded_corners, base.float_mode));
        let (bar_rounded, bar_float) = appearance.get("bar").copied().unwrap_or((base.bar_rounded_corners, base.bar_float_mode));
        let mut config = base.clone();
        config.mode = mode;
        config.separate_app_dock = c.separate_app_dock.is_active();
        config.app_dock_background_color = c.app_dock_background_color.saved_color().unwrap_or_default();
        config.background_color = c.background_color.saved_color();
        config.bar_border_enabled = c.bar_border_enabled.is_active();
        config.bar_border_sides =
            c.bar_border_switches.iter().filter(|(_, t)| t.is_active()).map(|(side, _)| side.clone()).collect();
        config.bar_border_color = c.bar_border_color.saved_color().unwrap_or_default();
        for (name, control) in &c.app_dock_controls {
            let number = || py_round(control.value());
            match (name.as_str(), control) {
                ("app_dock_edge", AppDockControl::Combo(w)) => config.app_dock_edge = active_id(w),
                ("app_dock_expand", _) => config.app_dock_expand = control.active(),
                ("app_dock_float_mode", _) => config.app_dock_float_mode = control.active(),
                ("app_dock_rounded_corners", _) => config.app_dock_rounded_corners = control.active(),
                ("app_dock_transparency_enabled", _) => config.app_dock_transparency_enabled = control.active(),
                ("app_dock_frosted", _) => config.app_dock_frosted = control.active(),
                ("app_dock_match_wallpaper", _) => config.app_dock_match_wallpaper = control.active(),
                ("app_dock_auto_hide", _) => config.app_dock_auto_hide = control.active(),
                ("app_dock_shine", _) => config.app_dock_shine = control.active(),
                ("app_dock_shadow", _) => config.app_dock_shadow = control.active(),
                ("app_dock_icon_size", _) => config.app_dock_icon_size = number(),
                ("app_dock_gap", _) => config.app_dock_gap = number(),
                ("app_dock_transparency", _) => config.app_dock_transparency = number(),
                ("app_dock_frost_strength", _) => config.app_dock_frost_strength = number(),
                _ => {}
            }
        }
        config.group_layouts = c.layout_controls.layouts();
        config.app_dock_group_layout = c.app_dock_layout_controls.layouts().get("dock").cloned().unwrap_or_default();
        config.always_show_tray = self.state.borrow().tray_choices.iter().cloned().collect();
        config.always_visible_apps =
            c.default_app_switches.iter().filter(|(_, t)| t.is_active()).map(|(name, _)| name.clone()).collect();
        config.application_display = active_id(&c.application_display);
        config.monitor = active_id(&c.monitor);
        config.monitor_overrides = overrides;
        config.icon_size = c.size.value_as_int() as i64;
        config.gap = c.gap.value_as_int() as i64;
        config.transparency = py_round(c.transparency.value());
        config.transparency_enabled = c.transparency_enabled.is_active();
        config.frosted = c.frosted.is_active();
        config.frost_strength = py_round(c.frost_strength.value());
        config.match_wallpaper = c.match_wallpaper.is_active();
        config.rounded_corners = dock_rounded;
        config.float_mode = dock_float;
        config.bar_rounded_corners = bar_rounded;
        config.bar_float_mode = bar_float;
        config.menu_hotkey_fullscreen = c.menu_hotkey_fullscreen.is_active();
        config.group_visibility = c.group_switches.iter().map(|(name, t)| (name.clone(), t.is_active())).collect();
        config.hot_corners = hot_corners;
        config.hot_corner_delay_ms = c.hot_corner_delay.value_as_int() as i64;
        config.hide_on_fullscreen = c.fullscreen.is_active();
        config.auto_hide = c.auto_hide.is_active();
        config.magnification = c.magnification.is_active();
        config.compact_bar = c.compact_bar.is_active();
        config.bar_islands = c.bar_islands.is_active();
        // Keep the saved size unless magnification is in use.
        if c.magnification.is_active() {
            config.hover_scale = py_round(c.magnification_size.value()) as f64 / 100.0;
            config.hover_scale_int = IntHint(false);
        }
        if self.app.save_config(config, true) {
            if self.embedded {
                c.result_label.set_text(&tr!("Settings saved."));
                c.result_label.set_visible(true);
            } else if let Some(window) = self.window.upgrade() {
                window.destroy();
            }
        }
    }

    fn refresh_tray_list(&self) {
        let window = self.window.clone();
        self.app.refresh_tray_entries(Box::new(move || {
            if let Some(window) = window.upgrade() {
                if let Some(handle) = SettingsWindow::from_window(&window) {
                    handle.inner.refresh_tray_rows();
                }
            }
        }));
    }

    fn refresh_tray_rows(&self) {
        let mut entries: IndexMap<String, TrayEntry> =
            self.app.tray_entries().into_iter().map(|record| (record.id.clone(), record)).collect();
        let choices = self.state.borrow().tray_choices.clone();
        for ident in &choices {
            entries.entry(ident.clone()).or_insert_with(|| TrayEntry {
                id: ident.clone(),
                label: ident.split_once(':').map(|(_, rest)| rest.to_owned()).unwrap_or_else(|| ident.clone()),
                running: false,
            });
        }
        let mut records: Vec<TrayEntry> = entries.into_values().collect();
        records.sort_by_key(|row| (!row.running, tessel_core::pyjson::casefold(&row.label)));
        let signature: Vec<(String, String, bool)> =
            records.iter().map(|r| (r.id.clone(), r.label.clone(), r.running)).collect();
        if self.state.borrow().tray_rows_signature.as_ref() == Some(&signature) {
            return;
        }
        self.state.borrow_mut().tray_rows_signature = Some(signature);
        let rows = &self.c.tray_rows;
        while let Some(child) = rows.first_child() {
            rows.remove(&child);
        }
        self.state.borrow_mut().tray_switches.clear();
        if records.is_empty() {
            rows.append(&label(&tr!("No tray applications are running."), Some("settings-muted")));
        }
        for record in records {
            let ident = record.id.clone();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
            labels.set_hexpand(true);
            let caption = label(&record.label, None);
            caption.set_wrap(true);
            caption.set_max_width_chars(32);
            labels.append(&caption);
            if !record.running {
                labels.append(&label(&tr!("Not running · preference saved"), Some("settings-muted")));
            }
            let toggle = gtk::Switch::new();
            toggle.set_active(choices.contains(&ident));
            toggle.set_valign(gtk::Align::Center);
            accessible_name(&toggle, &tr!("Always show {value1}", value1 = record.label.as_str()));
            toggle.set_tooltip_text(Some(&tr!("Always show")));
            {
                let window = self.window.clone();
                let key = ident.clone();
                toggle.connect_active_notify(move |widget| {
                    let Some(handle) = window.upgrade().and_then(|w| SettingsWindow::from_window(&w)) else { return };
                    let mut state = handle.inner.state.borrow_mut();
                    if widget.is_active() {
                        state.tray_choices.insert(key.clone());
                    } else {
                        state.tray_choices.remove(&key);
                    }
                });
            }
            row.append(&labels);
            row.append(&toggle);
            rows.append(&row);
            self.state.borrow_mut().tray_switches.insert(ident, toggle);
        }
    }

    fn application_display_changed(&self) {
        let c = &self.c;
        let title_mode = active_id(&c.application_display) == "title";
        let caption = if title_mode { tr!("Open windows and focused title") } else { tr!("Open windows") };
        if let Some(label) = c.group_labels.get("running") {
            label.set_text(&caption);
            label.set_tooltip_text(Some(&caption));
        }
        if let Some(toggle) = c.group_switches.get("running") {
            accessible_name(toggle, &tr!("Show {value1}", value1 = caption.as_str()));
        }
        c.layout_controls.set_context(&self.mode_id(), &active_id(&c.application_display));
    }

    fn mode_changed(&self) {
        let c = &self.c;
        let selected = self.mode_id();
        {
            let mut state = self.state.borrow_mut();
            let displayed = state.displayed_mode.clone();
            state.mode_appearance.insert(displayed, (c.rounded_corners.is_active(), c.float_mode.is_active()));
            state.displayed_mode = selected.clone();
        }
        self.switching_mode.set(true);
        let saved = self.state.borrow().mode_appearance.get(&selected).copied();
        if let Some((rounded, floating)) = saved {
            c.rounded_corners.set_active(rounded);
            c.float_mode.set_active(floating);
        }
        self.switching_mode.set(false);
        // The full-length bar retains the compact dock's saved alignment.
        for (_edge, alignment) in c.edge_widgets.values() {
            alignment.set_sensitive(selected == "dock");
        }
        self.update_app_dock_controls();
        c.layout_controls.set_context(&selected, &active_id(&c.application_display));
        self.preview_appearance();
    }

    fn update_app_dock_controls(&self) {
        let c = &self.c;
        let monitor = active_id(&c.monitor);
        let edges: Vec<&gtk::ComboBoxText> = if monitor == "primary" {
            c.edge_widgets.values().take(1).map(|(edge, _)| edge).collect()
        } else if monitor != "all" {
            c.edge_widgets.iter().filter(|(name, _)| **name == monitor).map(|(_, (edge, _))| edge).collect()
        } else {
            c.edge_widgets.values().map(|(edge, _)| edge).collect()
        };
        let config_top = || self.app.config().edge == "top";
        let available = self.mode_id() == "bar"
            && if edges.is_empty() { config_top() } else { edges.iter().any(|edge| active_id(edge) == "top") };
        let active = c.separate_app_dock.is_active();
        c.separate_app_dock.set_sensitive(available || active);
        if let Some(card) = c.section_cards.get("app_dock_group_layout") {
            card.set_visible(available && active && self.section_mode != "desktop");
        }
        let expand = c.app_dock_controls.get("app_dock_expand").is_some_and(|w| w.active());
        c.app_dock_layout_controls.set_expanded(expand);
        // Other monitor edges still use the main layout. Keep their rows
        // available when only some of the selected bars have an app dock.
        let all_top = if edges.is_empty() { config_top() } else { edges.iter().all(|edge| active_id(edge) == "top") };
        if available && active && all_top {
            c.layout_controls.set_hidden_groups(APP_DOCK_GROUPS);
        } else {
            c.layout_controls.set_hidden_groups::<&str>(&[]);
        }
        for control in c.app_dock_controls.values() {
            control.widget().set_sensitive(available && active);
        }
        let matched = c.app_dock_controls.get("app_dock_match_wallpaper").is_some_and(|w| w.active());
        c.app_dock_background_color.button.set_sensitive(available && active && !matched);
        for (key, toggle) in [("gap", "float_mode"), ("transparency", "transparency_enabled"), ("frost_strength", "frosted")] {
            let on = c.app_dock_controls.get(&format!("app_dock_{toggle}")).is_some_and(|w| w.active());
            if let Some(control) = c.app_dock_controls.get(&format!("app_dock_{key}")) {
                control.widget().set_sensitive(available && active && on);
            }
        }
        let caption =
            if active_id(&c.app_dock_edge) == "bottom" { tr!("Expand to full width") } else { tr!("Expand to full height") };
        c.app_dock_expand_label.set_text(&caption);
        if let Some(control) = c.app_dock_controls.get("app_dock_expand") {
            accessible_name(&control.widget(), &format!("{}: {}", tr!("App dock"), caption));
        }
    }

    fn update_material_controls(&self) {
        let c = &self.c;
        c.frost_strength.set_sensitive(c.frosted.is_active());
        c.transparency.set_tooltip_text(Some(&tr!("0% is opaque; 100% is fully transparent. Icons stay opaque.")));
    }

    fn update_bar_border_controls(&self) {
        let c = &self.c;
        let bar = self.mode_id() == "bar";
        c.bar_border_enabled.set_sensitive(bar);
        c.bar_islands.set_sensitive(bar);
        let enabled = bar && c.bar_border_enabled.is_active();
        for toggle in c.bar_border_switches.values() {
            toggle.set_sensitive(enabled);
        }
        c.bar_border_color.button.set_sensitive(enabled && !c.match_wallpaper.is_active());
    }

    fn preview_appearance(&self) {
        if self.switching_mode.get() {
            return;
        }
        let c = &self.c;
        let floating = c.float_mode.is_active();
        {
            let mut state = self.state.borrow_mut();
            let displayed = state.displayed_mode.clone();
            state.mode_appearance.insert(displayed, (c.rounded_corners.is_active(), floating));
        }
        c.gap.set_sensitive(floating);
        c.gap_control.set_sensitive(floating);
        let enabled = c.transparency_enabled.is_active();
        c.background_color.button.set_sensitive(!c.match_wallpaper.is_active());
        self.update_bar_border_controls();
        c.transparency.set_sensitive(enabled);
        self.update_material_controls();
        let preview = AppearancePreview {
            transparency: py_round(c.transparency.value()),
            match_wallpaper: c.match_wallpaper.is_active(),
            rounded_corners: c.rounded_corners.is_active(),
            float_mode: floating,
            transparency_enabled: enabled,
            frosted: c.frosted.is_active(),
            frost_strength: py_round(c.frost_strength.value()),
            bar_border_enabled: c.bar_border_enabled.is_active(),
            bar_border_sides: c.bar_border_switches.iter().filter(|(_, t)| t.is_active()).map(|(s, _)| s.clone()).collect(),
            bar_border_color: c.bar_border_color.saved_color().unwrap_or_default(),
        };
        self.app.preview_appearance(&preview);
    }
}

/// `Tessel Dock.show_settings()` minus `close_popups()`: one window per app.
pub fn show_settings(slot: &Rc<RefCell<Option<SettingsWindow>>>, app: Rc<dyn SettingsApp>) -> SettingsWindow {
    if let Some(existing) = slot.borrow().clone() {
        existing.present();
        return existing;
    }
    let window = SettingsWindow::new(app, SettingsOptions::default());
    let target = Rc::downgrade(slot);
    window.window().connect_unrealize(move |_| {
        if let Some(slot) = target.upgrade() {
            slot.borrow_mut().take();
        }
    });
    *slot.borrow_mut() = Some(window.clone());
    window.present();
    window.present();
    window
}

/// `Tessel Dock.show_error(message)`'s dialog (after `close_popups()`):
/// modal error `MessageDialog` titled `Dock`, transient for the settings
/// window when one is open, `tessel-dock-settings` class, palette theme attached.
pub fn error_dialog(application: Option<&gtk::Application>, transient_for: Option<&gtk::Window>, message: &str) -> gtk::MessageDialog {
    let dialog = gtk::MessageDialog::new(
        transient_for,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Close,
        &tr!("Dock"),
    );
    if let Some(application) = application {
        application.add_window(&dialog);
    }
    dialog.add_css_class("tessel-dock-settings");
    dialog.set_secondary_text(Some(message));
    dialog.connect_response(|widget, _| widget.destroy());
    tessel_gtk::theme::attach(&dialog, None);
    dialog.present();
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_round_and_titles() {
        assert_eq!(py_round(152.5), 152);
        assert_eq!(py_round(153.5), 154);
        assert_eq!(py_round(74.0), 74);
        assert_eq!(title_case("bottom"), "Bottom");
        assert_eq!(format!("{:.0}%", 20.0), "20%");
    }
}
