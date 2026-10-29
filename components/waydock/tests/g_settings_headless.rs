//! waydock::ui::settings (Dock Settings) in a private headless labwc: ports
//! the settings cases of test_dock_ui.py, test_settings_groups.py,
//! test_group_positions_ui.py, test_separate_app_dock_ui.py,
//! test_default_apps_ui.py, test_focused_title.py and test_always_tray_ui.py
//! with a fake `SettingsApp` (Python's `appearance_settings()` harness), plus
//! Python parity (the Python `SettingsWindow` with the same edits must save
//! byte-identical config.json) and screenshots of both windows.
//!
//! Runs only inside tests/run_g_headless.sh (WAYDOCK_G_HEADLESS=1, private
//! XDG_RUNTIME_DIR, fake HOME); anywhere else it skips without connecting.

#![allow(deprecated)] // ComboBoxText/MessageDialog mirror the Python UI.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use waydock::core::config::{ConfigStore, DockConfig, ALWAYS_VISIBLE_APPS, CORNERS, GROUPS};
use waydock::core::group_layout::{default_app_dock_layout, default_layout, Layout, APP_DOCK_GROUPS, LAYOUT_GROUPS};
use waydock::core::ordered::OrderedMap;
use waydock::core::tray_prefs::TrayEntry;
use waydock::ui::settings::{show_settings, AppearancePreview, ExtraControls, SettingsApp, SettingsOptions, SettingsWindow};

type R = Result<(), String>;

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => { if !$cond { return Err(format!("line {}: {}", line!(), format!($($msg)+))); } };
}
macro_rules! ensure_eq {
    ($a:expr, $b:expr) => {{
        match (&$a, &$b) {
            (a, b) => if a != b { return Err(format!("line {}: {:?} != {:?}", line!(), a, b)); }
        }
    }};
}

fn wait_for(seconds: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn settle(seconds: f64) {
    wait_for(seconds, || false);
}

fn tmp() -> PathBuf {
    PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/run/user/1000/cbt/rust-waydock/g".into()))
}

#[derive(Clone, Copy, PartialEq)]
enum SaveMode {
    Store,
    Fail,
}

struct FakeApp {
    application: gtk::Application,
    config: RefCell<DockConfig>,
    store: RefCell<ConfigStore>,
    history: RefCell<Vec<String>>,
    preview_values: RefCell<Option<(i64, bool, bool, bool)>>,
    preview_frosted: Cell<Option<bool>>,
    preview_frost_strength: Cell<Option<i64>>,
    save_mode: Cell<SaveMode>,
    entries: RefCell<Vec<(String, String)>>,
    tray: RefCell<Vec<TrayEntry>>,
    subscribers: RefCell<Vec<(u64, Rc<dyn Fn()>)>>,
    tray_backend: Cell<bool>,
    next_id: Cell<u64>,
}

impl SettingsApp for FakeApp {
    fn config(&self) -> DockConfig {
        self.config.borrow().clone()
    }
    fn save_config(&self, config: DockConfig, rebuild: bool) -> bool {
        if self.save_mode.get() == SaveMode::Fail {
            return false;
        }
        let saved = self.store.borrow_mut().save(&config).expect("save");
        *self.config.borrow_mut() = saved;
        self.history.borrow_mut().push(format!("settings-save {rebuild}"));
        true
    }
    fn preview_appearance(&self, p: &AppearancePreview) {
        self.preview_frosted.set(Some(p.frosted));
        self.preview_frost_strength.set(Some(p.frost_strength));
        let transparency = if p.transparency_enabled { p.transparency } else { 0 };
        let values = (transparency, p.match_wallpaper, p.rounded_corners, p.float_mode);
        *self.preview_values.borrow_mut() = Some(values);
        self.history.borrow_mut().push(format!("appearance-preview {values:?}"));
    }
    fn restore_appearance(&self) {
        *self.preview_values.borrow_mut() = None;
        let c = self.config.borrow();
        self.history
            .borrow_mut()
            .push(format!("appearance-restore {:?}", (c.transparency, c.match_wallpaper, c.rounded_corners, c.float_mode)));
    }
    fn monitor_entries(&self) -> Vec<(String, String)> {
        self.entries.borrow().clone()
    }
    fn tray_entries(&self) -> Vec<TrayEntry> {
        self.tray.borrow().clone()
    }
    fn subscribe_tray(&self, callback: Rc<dyn Fn()>) -> Option<u64> {
        if !self.tray_backend.get() {
            return None;
        }
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        self.subscribers.borrow_mut().push((id, callback));
        Some(id)
    }
    fn unsubscribe_tray(&self, id: u64) {
        self.subscribers.borrow_mut().retain(|(i, _)| *i != id);
    }
    fn application(&self) -> Option<gtk::Application> {
        Some(self.application.clone())
    }
    fn show_error(&self, message: &str) {
        self.history.borrow_mut().push(format!("error {message}"));
    }
}

impl FakeApp {
    fn last(&self) -> String {
        self.history.borrow().last().cloned().unwrap_or_default()
    }
    fn saved(&self) -> bool {
        self.history.borrow().iter().any(|e| e.starts_with("settings-save"))
    }
    fn load(&self) -> DockConfig {
        ConfigStore::new(self.store.borrow().path.clone()).load(None, None)
    }
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.store.borrow().path).unwrap_or_default()
    }
}

static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn monitor_count() -> u32 {
    gdk::Display::default().map(|d| d.monitors().n_items()).unwrap_or(0)
}

/// `make_application` + `appearance_settings` config of test_dock_ui.py.
fn fake_app(name: &str) -> Rc<FakeApp> {
    let n = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let application = gtk::Application::new(
        Some(&format!("org.waydock.GTest{}N{n}", std::process::id())),
        gio::ApplicationFlags::NON_UNIQUE,
    );
    application.register(None::<&gio::Cancellable>).expect("register");
    let directory = tmp().join("stores").join(format!("{n}-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let mut config = DockConfig { pins: vec!["browser.desktop".into(), "terminal.desktop".into()], ..DockConfig::default() };
    config.edge = "bottom".into();
    config.alignment = "end".into();
    config.monitor = "all".into();
    config.icon_size = 44;
    config.gap = 17;
    config.auto_hide = true;
    config.hide_delay_ms = 780;
    config.transparency = 20;
    config.match_wallpaper = false;
    let mut overrides = OrderedMap::new();
    for (monitor, edge, alignment) in
        [("HEADLESS-1", "left", "start"), ("HEADLESS-2", "top", "end"), ("unplugged-monitor", "right", "center")]
    {
        let mut value = OrderedMap::new();
        value.insert("edge", edge.to_string());
        value.insert("alignment", alignment.to_string());
        overrides.insert(monitor, value);
    }
    config.monitor_overrides = overrides;
    let entries = (0..monitor_count()).map(|i| (format!("HEADLESS-{}", i + 1), format!("Test display {}", i + 1))).collect();
    Rc::new(FakeApp {
        application,
        config: RefCell::new(config),
        store: RefCell::new(ConfigStore::new(directory.join("config.json"))),
        history: RefCell::new(Vec::new()),
        preview_values: RefCell::new(None),
        preview_frosted: Cell::new(None),
        preview_frost_strength: Cell::new(None),
        save_mode: Cell::new(SaveMode::Store),
        entries: RefCell::new(entries),
        tray: RefCell::new(Vec::new()),
        subscribers: RefCell::new(Vec::new()),
        tray_backend: Cell::new(false),
        next_id: Cell::new(0),
    })
}

fn save_initial(app: &FakeApp) {
    let config = app.config();
    let saved = app.store.borrow_mut().save(&config).expect("initial save");
    *app.config.borrow_mut() = saved;
}

fn dynamic(app: &Rc<FakeApp>) -> Rc<dyn SettingsApp> {
    app.clone()
}

/// `appearance_settings()`: save the store, then show a real window.
fn appearance_settings(app: &Rc<FakeApp>) -> SettingsWindow {
    save_initial(app);
    let window = SettingsWindow::new(dynamic(app), SettingsOptions::default());
    window.window().set_visible(true);
    settle(0.05);
    window
}

fn reopen(app: &Rc<FakeApp>) -> SettingsWindow {
    SettingsWindow::new(dynamic(app), SettingsOptions::default())
}

fn teardown(app: &Rc<FakeApp>) {
    for window in app.application.windows() {
        window.destroy();
    }
    app.application.quit();
    settle(0.05);
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(current) = child {
        result.extend(descendants(&current));
        child = current.next_sibling();
    }
    result
}

fn button(window: &SettingsWindow, caption: &str) -> gtk::Button {
    descendants(window.window().upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.label().as_deref() == Some(caption))
        .unwrap_or_else(|| panic!("no {caption} button"))
}

fn click(window: &SettingsWindow, caption: &str) {
    button(window, caption).emit_clicked();
    settle(0.05);
}

fn rgba(text: &str) -> gdk::RGBA {
    gdk::RGBA::parse(text).unwrap()
}

fn layout(start: &[&str], center: &[&str], end: &[&str]) -> Layout {
    let list = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    [("start", list(start)), ("center", list(center)), ("end", list(end))].into_iter().collect()
}

fn active_id(combo: &gtk::ComboBoxText) -> String {
    combo.active_id().map(|s| s.to_string()).unwrap_or_default()
}

/// A config with the named fields reset to `before`'s values.
fn without(mut config: DockConfig, before: &DockConfig, fields: &[&str]) -> DockConfig {
    for field in fields {
        match *field {
            "transparency" => config.transparency = before.transparency,
            "match_wallpaper" => config.match_wallpaper = before.match_wallpaper,
            "rounded_corners" => config.rounded_corners = before.rounded_corners,
            "float_mode" => config.float_mode = before.float_mode,
            "group_visibility" => config.group_visibility = before.group_visibility.clone(),
            "menu_hotkey_fullscreen" => config.menu_hotkey_fullscreen = before.menu_hotkey_fullscreen,
            "hot_corners" => config.hot_corners = before.hot_corners.clone(),
            "hot_corner_delay_ms" => config.hot_corner_delay_ms = before.hot_corner_delay_ms,
            "application_display" => config.application_display = before.application_display.clone(),
            other => panic!("unknown field {other}"),
        }
    }
    config
}

// ---- test_dock_ui.py ----

fn appearance_controls_preview_without_saving_and_cancel_restores(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let before = app.config();
    let saved = app.bytes();
    let c = window.controls();
    ensure!(c.rounded_corners.is_active() && c.float_mode.is_active(), "corner/float defaults");
    ensure_eq!(c.transparency.value(), 20.0);
    ensure!(!c.match_wallpaper.is_active(), "match");
    c.transparency.set_value(65.0);
    c.match_wallpaper.set_active(true);
    c.rounded_corners.set_active(false);
    c.float_mode.set_active(false);
    ensure!(!c.gap.is_sensitive(), "gap sensitive");
    ensure_eq!(c.gap.value_as_int(), 17);
    settle(0.05);
    ensure_eq!(*app.preview_values.borrow(), Some((65, true, false, false)));
    ensure_eq!(app.config(), before);
    ensure_eq!(app.bytes(), saved);
    click(&window, "Cancel");
    ensure_eq!(*app.preview_values.borrow(), None);
    ensure_eq!(app.config(), before);
    ensure_eq!(app.bytes(), saved);
    ensure!(!app.saved(), "saved on cancel");
    ensure_eq!(app.last(), "appearance-restore (20, false, true, true)");
    Ok(())
}

fn appearance_apply_persists_new_fields_without_losing_pins_or_placement(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let before = app.config();
    let c = window.controls();
    c.transparency.set_value(74.0);
    c.match_wallpaper.set_active(true);
    c.rounded_corners.set_active(false);
    c.float_mode.set_active(false);
    click(&window, "Apply");
    let stored = app.load();
    ensure_eq!(stored.transparency, 74);
    ensure!(stored.match_wallpaper && !stored.rounded_corners && !stored.float_mode, "stored {stored:?}");
    let fields = ["transparency", "match_wallpaper", "rounded_corners", "float_mode"];
    ensure_eq!(without(stored.clone(), &before, &fields), before);
    ensure_eq!(app.config(), stored);
    ensure_eq!(*app.preview_values.borrow(), None);
    ensure!(!app.application.windows().iter().any(|w| w == window.window().upcast_ref::<gtk::Window>()), "window kept");
    ensure_eq!(app.last(), "appearance-restore (74, true, false, false)");
    let reopened = reopen(app);
    let r = reopened.controls();
    ensure!(!r.rounded_corners.is_active() && !r.float_mode.is_active(), "reopened");
    ensure!(!r.gap.is_sensitive(), "gap");
    r.float_mode.set_active(true);
    ensure!(r.gap.is_sensitive(), "gap after float");
    ensure_eq!(r.gap.value_as_int(), 17);
    reopened.destroy();
    Ok(())
}

fn appearance_failed_apply_retains_edits_and_window_close_reverts_preview(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let saved = app.bytes();
    app.save_mode.set(SaveMode::Fail);
    let c = window.controls();
    c.transparency.set_value(0.0);
    c.match_wallpaper.set_active(true);
    click(&window, "Apply");
    ensure!(window.window().is_visible(), "closed after failure");
    ensure_eq!(*app.preview_values.borrow(), Some((0, true, true, true)));
    ensure_eq!(c.transparency.value(), 0.0);
    ensure!(c.match_wallpaper.is_active(), "match");
    ensure_eq!(app.bytes(), saved);
    // The titlebar close path also destroys, with no Apply or Cancel.
    window.destroy();
    settle(0.05);
    ensure_eq!(*app.preview_values.borrow(), None);
    ensure_eq!(app.bytes(), saved);
    ensure_eq!(app.last(), "appearance-restore (20, false, true, true)");
    Ok(())
}

fn dock_transparency_toggle_keeps_level_and_app_preferences(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    {
        let mut config = app.config.borrow_mut();
        config.apps_transparency = 37;
        config.apps_transparency_enabled = true;
    }
    let c = window.controls();
    c.transparency.set_value(70.0);
    c.transparency_enabled.set_active(false);
    ensure!(!c.transparency.get_sensitive(), "transparency sensitive");
    ensure_eq!(c.transparency.value(), 70.0);
    ensure_eq!(app.preview_values.borrow().unwrap().0, 0);
    window.apply();
    let saved = app.load();
    ensure_eq!(saved.transparency, 70);
    ensure!(!saved.transparency_enabled, "enabled");
    ensure_eq!(saved.apps_transparency, 37);
    ensure!(saved.apps_transparency_enabled, "apps enabled");
    Ok(())
}

fn dock_frosted_preview_and_save_preserve_app_choices(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    {
        let mut config = app.config.borrow_mut();
        config.apps_frosted = true;
        config.apps_transparency = 43;
        config.apps_frost_strength = 82;
    }
    let c = window.controls();
    c.frosted.set_active(true);
    c.frost_strength.set_value(37.0);
    ensure_eq!(app.preview_frosted.get(), Some(true));
    ensure_eq!(app.preview_frost_strength.get(), Some(37));
    ensure!(!app.load().frosted, "saved early");
    c.transparency_enabled.set_active(false);
    ensure!(c.frosted.get_sensitive() && c.frosted.is_active() && c.frost_strength.get_sensitive(), "frost controls");
    ensure_eq!(app.preview_frosted.get(), Some(true));
    window.apply();
    let saved = app.load();
    ensure!(saved.frosted && saved.effective_frosted(), "frosted");
    ensure_eq!(saved.frost_strength, 37);
    ensure!(saved.apps_frosted, "apps_frosted");
    ensure_eq!((saved.apps_transparency, saved.apps_frost_strength), (43, 82));
    Ok(())
}

fn mode_settings_remember_separate_appearance_and_preserve_placement(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let before = app.config();
    let c = window.controls();
    ensure_eq!(active_id(&c.mode), "dock");
    c.mode.set_active_id(Some("bar"));
    ensure!(!c.float_mode.is_active() && !c.rounded_corners.is_active(), "bar defaults");
    ensure!(c.float_mode.is_sensitive() && c.rounded_corners.is_sensitive(), "sensitive");
    ensure!(!c.gap.is_sensitive(), "gap");
    ensure!(c.edge_widgets.values().all(|(_, a)| !a.is_sensitive()), "alignment in bar");
    c.float_mode.set_active(true);
    c.rounded_corners.set_active(true);
    c.mode.set_active_id(Some("dock"));
    c.rounded_corners.set_active(false);
    c.mode.set_active_id(Some("bar"));
    ensure!(c.rounded_corners.is_active() && c.float_mode.is_active(), "bar remembered");
    window.apply();
    let stored = app.load();
    ensure_eq!(stored.mode, "bar");
    ensure!(!stored.rounded_corners && stored.float_mode, "dock values {stored:?}");
    ensure!(stored.bar_rounded_corners && stored.bar_float_mode, "bar values");
    ensure_eq!(stored.monitor_overrides, before.monitor_overrides);
    ensure_eq!(stored.pins, before.pins);
    let reopened = reopen(app);
    let r = reopened.controls();
    ensure!(r.rounded_corners.is_active(), "reopened bar");
    r.mode.set_active_id(Some("dock"));
    ensure!(!r.rounded_corners.is_active(), "reopened dock");
    ensure!(r.edge_widgets.values().all(|(_, a)| a.is_sensitive()), "alignment in dock");
    reopened.destroy();
    ensure_eq!(app.load().mode, "bar");
    Ok(())
}

fn hot_corner_settings_keep_corner_names_readable_with_long_monitor_labels(app: &Rc<FakeApp>) -> R {
    let original = appearance_settings(app);
    original.destroy();
    let entries: Vec<(String, String)> =
        app.monitor_entries().into_iter().map(|(name, _)| (name.clone(), format!("{name} · Dell Inc. AW3225QF 2Q49YZ3"))).collect();
    *app.entries.borrow_mut() = entries.clone();
    let window = reopen(app);
    window.window().set_visible(true);
    settle(0.1);
    let labels: Vec<String> = descendants(window.window().upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .collect();
    for caption in ["Top left", "Top right", "Bottom left", "Bottom right"] {
        ensure_eq!(labels.iter().filter(|l| *l == caption).count(), 2);
    }
    let names: Vec<&String> = window.controls().hot_corner_switches.keys().collect();
    ensure_eq!(names, entries.iter().map(|(n, _)| n).collect::<Vec<_>>());
    ensure!(window.window().width() < 850, "too wide: {}", window.window().width());
    window.destroy();
    Ok(())
}

// ---- test_settings_groups.py ----

fn cancel_keeps_visibility_and_hotkey_unchanged(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let before = app.config();
    let saved = app.bytes();
    let c = window.controls();
    ensure_eq!(c.group_switches.keys().cloned().collect::<Vec<_>>(), GROUPS.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>());
    ensure!(!c.menu_hotkey_fullscreen.is_active(), "hotkey");
    for toggle in c.group_switches.values() {
        ensure!(toggle.is_active(), "group default");
        toggle.set_active(false);
    }
    c.menu_hotkey_fullscreen.set_active(true);
    settle(0.05);
    ensure_eq!(app.config(), before);
    ensure_eq!(app.bytes(), saved);
    click(&window, "Cancel");
    ensure_eq!(app.config(), before);
    ensure_eq!(app.bytes(), saved);
    ensure!(!app.saved(), "saved");
    Ok(())
}

fn apply_persists_each_group_and_hotkey_and_reopens_consistently(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let before = app.config();
    let expected: OrderedMap<bool> = GROUPS.iter().enumerate().map(|(i, (n, _))| (*n, i % 2 == 0)).collect();
    for (name, visible) in expected.iter() {
        window.controls().group_switches[name].set_active(*visible);
    }
    window.controls().menu_hotkey_fullscreen.set_active(true);
    click(&window, "Apply");
    let stored = app.load();
    ensure_eq!(stored.group_visibility, expected);
    ensure!(stored.menu_hotkey_fullscreen, "hotkey");
    ensure_eq!(without(stored.clone(), &before, &["group_visibility", "menu_hotkey_fullscreen"]), before);
    ensure!(app.history.borrow().iter().any(|e| e == "settings-save true"), "rebuild flag");
    let reopened = reopen(app);
    ensure!(reopened.controls().menu_hotkey_fullscreen.is_active(), "reopened hotkey");
    let visibility: OrderedMap<bool> =
        reopened.controls().group_switches.iter().map(|(n, t)| (n.clone(), t.is_active())).collect();
    ensure_eq!(visibility, expected);
    reopened.destroy();
    Ok(())
}

fn failed_save_keeps_edits_available_and_does_not_change_configuration(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let saved = app.bytes();
    app.save_mode.set(SaveMode::Fail);
    let c = window.controls();
    c.group_switches["app_menu"].set_active(false);
    c.menu_hotkey_fullscreen.set_active(true);
    click(&window, "Apply");
    ensure!(window.window().is_visible(), "closed");
    ensure!(!c.group_switches["app_menu"].is_active() && c.menu_hotkey_fullscreen.is_active(), "edits lost");
    ensure!(app.config().is_group_visible("app_menu") && !app.config().menu_hotkey_fullscreen, "config changed");
    ensure_eq!(app.bytes(), saved);
    Ok(())
}

fn hot_corner_controls_default_off_and_cancel_preserves_preferences(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let before = app.config();
    let saved = app.bytes();
    let c = window.controls();
    ensure_eq!(c.hot_corner_switches.len(), 2);
    ensure_eq!(c.hot_corner_delay.value_as_int(), 150);
    for switches in c.hot_corner_switches.values() {
        ensure_eq!(switches.keys().cloned().collect::<Vec<_>>(), CORNERS.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>());
        for toggle in switches.values() {
            ensure!(!toggle.is_active(), "corner default");
            ensure!(toggle.tooltip_text().unwrap_or_default().contains("Open Activities"), "tooltip");
            toggle.set_active(true);
        }
    }
    c.hot_corner_delay.set_value(750.0);
    settle(0.05);
    ensure_eq!(app.config(), before);
    click(&window, "Cancel");
    ensure_eq!(app.config(), before);
    ensure_eq!(app.bytes(), saved);
    Ok(())
}

fn hot_corner_apply_preserves_disconnected_monitors_and_other_preferences(app: &Rc<FakeApp>) -> R {
    {
        let mut config = app.config.borrow_mut();
        config.hot_corners = [("HEADLESS-1", vec!["top_left".to_string()]), ("unplugged-monitor", vec!["bottom_right".to_string()])]
            .into_iter()
            .collect();
    }
    let window = appearance_settings(app);
    let before = app.config();
    let c = window.controls();
    ensure!(c.hot_corner_switches["HEADLESS-1"]["top_left"].is_active(), "loaded");
    c.hot_corner_switches["HEADLESS-1"]["top_left"].set_active(false);
    c.hot_corner_switches["HEADLESS-2"]["top_right"].set_active(true);
    c.hot_corner_switches["HEADLESS-2"]["bottom_left"].set_active(true);
    c.hot_corner_delay.set_value(600.0);
    click(&window, "Apply");
    let stored = app.load();
    let expected: OrderedMap<Vec<String>> = [
        ("HEADLESS-2", vec!["top_right".to_string(), "bottom_left".to_string()]),
        ("unplugged-monitor", vec!["bottom_right".to_string()]),
    ]
    .into_iter()
    .collect();
    ensure_eq!(stored.hot_corners, expected);
    ensure_eq!(stored.hot_corner_delay_ms, 600);
    ensure_eq!(without(stored.clone(), &before, &["hot_corners", "hot_corner_delay_ms"]), before);
    let reopened = reopen(app);
    let r = reopened.controls();
    ensure!(!r.hot_corner_switches["HEADLESS-1"]["top_left"].is_active(), "h1");
    ensure!(r.hot_corner_switches["HEADLESS-2"]["top_right"].is_active(), "h2 tr");
    ensure!(r.hot_corner_switches["HEADLESS-2"]["bottom_left"].is_active(), "h2 bl");
    ensure_eq!(r.hot_corner_delay.value_as_int(), 600);
    reopened.destroy();
    Ok(())
}

// ---- test_group_positions_ui.py ----

fn settings_buttons_modes_apply_and_cancel(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let controls = window.controls().layout_controls.clone();
    let mut names = controls.names();
    names.sort();
    let mut expected: Vec<String> = LAYOUT_GROUPS.iter().map(|(n, _)| n.to_string()).collect();
    expected.sort();
    ensure_eq!(names, expected);
    controls.controls("weather").unwrap().0.set_selected(0);
    controls.controls("session").unwrap().0.set_selected(0);
    controls.controls("session").unwrap().1.emit_clicked();
    ensure_eq!(controls.current()["start"], vec!["session".to_string(), "weather".to_string()]);
    ensure!(!controls.controls("session").unwrap().1.get_sensitive(), "session up");
    ensure!(!controls.controls("weather").unwrap().2.get_sensitive(), "weather down");
    window.controls().mode.set_active_id(Some("bar"));
    controls.controls("workspaces").unwrap().0.set_selected(2);
    let bar = controls.current();
    window.controls().mode.set_active_id(Some("dock"));
    ensure_eq!(controls.current()["start"], vec!["session".to_string(), "weather".to_string()]);
    ensure!(app.load().group_layouts.is_empty(), "saved early");
    click(&window, "Apply");
    let saved = app.load();
    ensure_eq!(saved.group_layouts["bar"], bar);
    ensure_eq!(saved.group_layouts["dock"]["start"], vec!["session".to_string(), "weather".to_string()]);
    let reopened = reopen(app);
    ensure_eq!(reopened.controls().layout_controls.layouts(), saved.group_layouts);
    reopened.controls().layout_controls.reset();
    ensure!(!reopened.controls().layout_controls.layouts().contains_key("dock"), "reset dock");
    ensure_eq!(reopened.controls().layout_controls.layouts()["bar"], bar);
    reopened.destroy();
    ensure_eq!(app.load().group_layouts, saved.group_layouts);
    Ok(())
}

fn position_dropdown_can_move_its_own_row(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let selector = window.controls().layout_controls.controls("app_menu").unwrap().0;
    let mut scroll = selector.parent();
    while let Some(widget) = scroll.clone() {
        if widget.is::<gtk::ScrolledWindow>() {
            break;
        }
        scroll = widget.parent();
    }
    let scroll = scroll.and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok()).ok_or("no scroll")?;
    let adjustment = scroll.vadjustment();
    let point = selector.compute_point(&scroll, &gtk::graphene::Point::new(0.0, 0.0)).ok_or("no point")?;
    adjustment.set_value(adjustment.value() + point.y() as f64 - 100.0);
    selector.grab_focus();
    settle(0.1);
    selector.first_child().ok_or("no toggle")?.emit_by_name::<()>("clicked", &[]);
    settle(0.1);
    let popup = descendants(selector.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Popover>().ok())
        .ok_or("no popover")?;
    ensure!(popup.is_visible(), "popover hidden");
    let listing = descendants(popup.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::ListView>().ok())
        .ok_or("no list")?;
    listing.model().and_then(|m| m.downcast::<gtk::SingleSelection>().ok()).ok_or("model")?.set_selected(0);
    listing.emit_by_name::<()>("activate", &[&0u32]);
    settle(0.2);
    ensure_eq!(window.controls().layout_controls.current()["start"], vec!["app_menu".to_string()]);
    ensure!(app.load().group_layouts.is_empty(), "saved");
    Ok(())
}

// ---- test_separate_app_dock_ui.py ----

fn separate_layout_settings_order_apply_reset_and_disable_preserve_bar(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let c = window.controls();
    c.mode.set_active_id(Some("bar"));
    for (edge, _) in c.edge_widgets.values() {
        edge.set_active_id(Some("top"));
    }
    let bar_layout = default_layout("bar", "title");
    c.layout_controls.set_layout("bar", bar_layout.clone());
    ensure!(!c.section_cards["app_dock_group_layout"].is_visible(), "card visible early");
    c.separate_app_dock.set_active(true);
    let controls = c.app_dock_layout_controls.clone();
    ensure!(c.section_cards["app_dock_group_layout"].is_visible(), "card hidden");
    let mut names = controls.names();
    names.sort();
    let mut groups: Vec<String> = APP_DOCK_GROUPS.iter().map(|s| s.to_string()).collect();
    groups.sort();
    ensure_eq!(names, groups);
    ensure!(APP_DOCK_GROUPS.iter().all(|n| c.layout_controls.row(n).unwrap().parent().is_none()), "main rows not hidden");
    controls.controls("pinned").unwrap().0.set_selected(1);
    controls.controls("running").unwrap().0.set_selected(1);
    controls.controls("running").unwrap().1.emit_clicked();
    let expected = layout(&["app_menu"], &["running", "pinned"], &["files"]);
    ensure_eq!(controls.current(), expected);
    ensure!(!controls.controls("running").unwrap().1.get_sensitive(), "running up");
    ensure!(!controls.controls("pinned").unwrap().2.get_sensitive(), "pinned down");
    c.app_dock_controls["app_dock_expand"].set_active(false);
    ensure_eq!(controls.current(), expected);
    window.apply();
    let saved = app.load();
    ensure_eq!(saved.app_dock_group_layout, expected);
    ensure_eq!(saved.group_layouts["bar"], bar_layout);
    let reopened = reopen(app);
    let r = reopened.controls();
    ensure_eq!(r.app_dock_layout_controls.current(), expected);
    r.separate_app_dock.set_active(false);
    ensure!(!r.section_cards["app_dock_group_layout"].get_visible(), "card");
    ensure!(APP_DOCK_GROUPS.iter().all(|n| r.layout_controls.row(n).unwrap().parent().is_some()), "rows back");
    reopened.apply();
    ensure_eq!(app.load().app_dock_group_layout, expected);
    ensure_eq!(app.load().group_layouts["bar"], bar_layout);
    // Reset is scoped to the app dock and waits for Apply.
    r.separate_app_dock.set_active(true);
    r.app_dock_layout_controls.reset();
    ensure_eq!(r.app_dock_layout_controls.current(), default_app_dock_layout(false));
    ensure_eq!(app.load().app_dock_group_layout, expected);
    reopened.apply();
    ensure!(app.load().app_dock_group_layout.is_empty(), "reset not saved");
    ensure_eq!(app.load().group_layouts["bar"], bar_layout);
    reopened.destroy();
    Ok(())
}

fn settings_enable_only_on_top_bars_and_save_independent_values(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let c = window.controls();
    ensure!(!c.separate_app_dock.get_sensitive(), "enabled for dock");
    c.mode.set_active_id(Some("bar"));
    c.edge_widgets["HEADLESS-1"].0.set_active_id(Some("top"));
    ensure!(c.separate_app_dock.get_sensitive(), "disabled on top bar");
    c.separate_app_dock.set_active(true);
    c.app_dock_edge.set_active_id(Some("bottom"));
    ensure_eq!(c.app_dock_expand_label.text().as_str(), "Expand to full width");
    c.app_dock_controls["app_dock_transparency"].set_value(72.0);
    c.app_dock_controls["app_dock_frosted"].set_active(true);
    c.app_dock_controls["app_dock_frost_strength"].set_value(85.0);
    c.app_dock_controls["app_dock_rounded_corners"].set_active(true);
    c.app_dock_background_color.button.set_rgba(&rgba("#475869"));
    ensure!(c.app_dock_background_color.button.get_sensitive(), "color insensitive");
    c.app_dock_controls["app_dock_match_wallpaper"].set_active(true);
    ensure!(!c.app_dock_background_color.button.get_sensitive(), "color sensitive with match");
    c.app_dock_controls["app_dock_match_wallpaper"].set_active(false);
    c.background_color.button.set_rgba(&rgba("#102030"));
    window.apply();
    let saved = app.load();
    ensure!(saved.separate_app_dock, "separate");
    ensure_eq!(saved.app_dock_edge, "bottom");
    ensure_eq!((saved.app_dock_transparency, saved.transparency), (72, 20));
    ensure!(saved.app_dock_frosted && !saved.frosted, "frosted");
    ensure_eq!((saved.app_dock_frost_strength, saved.frost_strength), (85, 50));
    ensure_eq!(saved.background_color.as_deref(), Some("#102030"));
    ensure_eq!(saved.app_dock_background_color, "#475869");
    Ok(())
}

// ---- test_default_apps_ui.py / test_focused_title.py / test_always_tray_ui.py ----

fn default_apps_settings_save_cancel_and_reopen(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let c = window.controls();
    let mut active: Vec<&String> = c.default_app_switches.iter().filter(|(_, t)| t.is_active()).map(|(n, _)| n).collect();
    active.sort();
    let mut expected: Vec<String> = ALWAYS_VISIBLE_APPS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    ensure_eq!(active, expected.iter().collect::<Vec<_>>());
    for toggle in c.default_app_switches.values() {
        toggle.set_active(false);
    }
    c.default_app_switches["network"].set_active(true);
    ensure_eq!(app.config().always_visible_apps, ALWAYS_VISIBLE_APPS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    click(&window, "Apply");
    ensure_eq!(app.load().always_visible_apps, vec!["network".to_string()]);
    let reopened = reopen(app);
    ensure!(reopened.controls().default_app_switches["network"].is_active(), "network");
    reopened.controls().default_app_switches["audio"].set_active(true);
    reopened.destroy();
    ensure_eq!(app.load().always_visible_apps, vec!["network".to_string()]);
    Ok(())
}

fn settings_apply_and_cancel_keep_saved_pins_and_group_choices(app: &Rc<FakeApp>) -> R {
    let window = appearance_settings(app);
    let before = app.config();
    let c = window.controls();
    c.application_display.set_active_id(Some("title"));
    ensure!(c.group_switches["pinned"].get_sensitive() && c.group_switches["pinned"].is_active(), "pinned");
    ensure_eq!(c.group_labels["running"].text().as_str(), "Open windows and focused title");
    ensure_eq!(app.config(), before);
    click(&window, "Cancel");
    ensure_eq!(app.config(), before);
    let window = reopen(app);
    window.controls().application_display.set_active_id(Some("title"));
    window.apply();
    let stored = app.load();
    let mut expected = before.clone();
    expected.application_display = "title".into();
    ensure_eq!(stored, expected);
    window.destroy();
    let reopened = reopen(app);
    let r = reopened.controls();
    ensure_eq!(active_id(&r.application_display), "title");
    r.application_display.set_active_id(Some("icons"));
    ensure!(r.group_switches["pinned"].get_sensitive(), "pinned");
    ensure_eq!(r.group_labels["running"].text().as_str(), "Open windows");
    reopened.destroy();
    Ok(())
}

fn preferences_cancel_apply_and_reopen_keep_offline_choices(app: &Rc<FakeApp>) -> R {
    *app.tray.borrow_mut() = vec![TrayEntry { id: "id:persistent-player".into(), label: "Tray fixture".into(), running: true }];
    app.config.borrow_mut().always_show_tray = vec!["id:offline-app".into()];
    let ident = "id:persistent-player".to_string();
    let window = appearance_settings(app);
    let switches = window.tray_switches();
    ensure!(switches["id:offline-app"].is_active(), "offline choice");
    // Running rows sort first; the offline row says so.
    ensure_eq!(switches.keys().cloned().collect::<Vec<_>>(), vec![ident.clone(), "id:offline-app".to_string()]);
    let texts: Vec<String> = descendants(window.controls().tray_rows.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .collect();
    ensure!(texts.contains(&"Not running · preference saved".to_string()) && texts.contains(&"offline-app".to_string()), "{texts:?}");
    switches[&ident].set_active(true);
    ensure_eq!(app.config().always_show_tray, vec!["id:offline-app".to_string()]);
    click(&window, "Apply");
    let mut stored = app.load().always_show_tray;
    stored.sort();
    ensure_eq!(stored, vec!["id:offline-app".to_string(), ident.clone()]);
    let reopened = reopen(app);
    ensure!(reopened.tray_switches()[&ident].is_active(), "reopened");
    reopened.tray_switches()[&ident].set_active(false);
    reopened.destroy();
    ensure!(app.load().always_show_tray.contains(&ident), "cancel saved");
    Ok(())
}

// ---- Rust-side coverage of the remaining Python branches ----

fn empty_tray_subscription_and_refresh(app: &Rc<FakeApp>) -> R {
    app.tray_backend.set(true);
    let app2 = app;
    let window = appearance_settings(&app2);
    let texts = |w: &SettingsWindow| -> Vec<String> {
        descendants(w.controls().tray_rows.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.text().to_string())
            .collect()
    };
    ensure_eq!(texts(&window), vec!["No tray applications are running.".to_string()]);
    ensure_eq!(app2.subscribers.borrow().len(), 1);
    *app2.tray.borrow_mut() = vec![TrayEntry { id: "title:Zeta".into(), label: "zeta".into(), running: true },
                                   TrayEntry { id: "id:alpha".into(), label: "Alpha".into(), running: true }];
    let callback = app2.subscribers.borrow()[0].1.clone();
    callback();
    ensure_eq!(window.tray_switches().keys().cloned().collect::<Vec<_>>(), vec!["id:alpha".to_string(), "title:Zeta".to_string()]);
    let first = window.tray_switches()["id:alpha"].clone();
    callback(); // unchanged signature: rows are kept
    ensure!(window.tray_switches()["id:alpha"] == first, "rows rebuilt without change");
    let tooltip = window.controls().tray_refresh.tooltip_text().unwrap_or_default();
    ensure_eq!(tooltip.as_str(), "Refresh running tray applications");
    app2.tray.borrow_mut().pop();
    window.controls().tray_refresh.emit_clicked();
    ensure_eq!(window.tray_switches().keys().cloned().collect::<Vec<_>>(), vec!["title:Zeta".to_string()]);
    window.destroy();
    settle(0.05);
    ensure!(app2.subscribers.borrow().is_empty(), "not unsubscribed on unrealize");
    Ok(())
}

struct Extra {
    widget: gtk::Label,
    allow: Cell<bool>,
    calls: Cell<u32>,
}
impl ExtraControls for Extra {
    fn widget(&self) -> gtk::Widget {
        self.widget.clone().upcast()
    }
    fn apply(&self) -> bool {
        self.calls.set(self.calls.get() + 1);
        self.allow.get()
    }
}

fn sections_embedded_and_extra_controls(app: &Rc<FakeApp>) -> R {
    save_initial(app);
    let desktop = SettingsWindow::new(dynamic(app), SettingsOptions { section: "desktop".into(), ..SettingsOptions::default() });
    let cards = &desktop.controls().section_cards;
    for (key, card) in cards {
        let expected = key == "shortcuts" || key == "hot_corners";
        ensure_eq!((key.as_str(), card.get_visible()), (key.as_str(), expected));
    }
    ensure!(!desktop.controls().hint_row.get_visible(), "hint in desktop");
    let labels: Vec<String> = descendants(desktop.window().upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .collect();
    ensure!(labels.contains(&"Desktop Behavior".to_string()), "title");
    ensure!(labels.contains(&"Configure workspaces, shortcuts, and hot corners.".to_string()), "subtitle");
    desktop.destroy();
    let resets = Rc::new(Cell::new(0));
    let counter = resets.clone();
    let extra = Rc::new(Extra { widget: gtk::Label::new(Some("extra")), allow: Cell::new(false), calls: Cell::new(0) });
    let embedded = SettingsWindow::new(
        dynamic(app),
        SettingsOptions {
            section: "dock".into(),
            embedded: true,
            reset: Some(Rc::new(move || counter.set(counter.get() + 1))),
            extra_controls: Some(extra.clone()),
        },
    );
    let c = embedded.controls();
    ensure!(c.section_cards["placement"].get_visible() && !c.section_cards["hot_corners"].get_visible(), "dock cards");
    ensure!(c.match_wallpaper.parent().is_none(), "match wallpaper row shown when embedded in dock");
    ensure!(extra.widget.parent().is_some(), "extra widget");
    ensure_eq!(c.cancel.label().as_deref(), Some("Reset"));
    c.cancel.emit_clicked();
    ensure_eq!(resets.get(), 1);
    c.menu_hotkey_fullscreen.set_active(true);
    c.apply.emit_clicked();
    ensure!(extra.calls.get() == 1 && !c.result_label.get_visible() && !app.saved(), "extra veto");
    extra.allow.set(true);
    c.apply.emit_clicked();
    ensure!(app.saved(), "embedded save");
    ensure!(c.result_label.get_visible() && c.result_label.text() == "Settings saved.", "result label");
    embedded.destroy();
    Ok(())
}

fn show_settings_presents_one_window(app: &Rc<FakeApp>) -> R {
    save_initial(app);
    let slot = Rc::new(RefCell::new(None));
    let first = show_settings(&slot, dynamic(app));
    settle(0.1);
    let second = show_settings(&slot, dynamic(app));
    ensure!(first.window() == second.window(), "second window");
    ensure_eq!(app.application.windows().len(), 1);
    ensure!(first.window().has_css_class("waydock-settings"), "css class");
    ensure_eq!(first.window().title().as_deref(), Some("Dock Settings"));
    first.cancel();
    settle(0.05);
    ensure!(slot.borrow().is_none(), "slot not cleared");
    let third = show_settings(&slot, dynamic(app));
    ensure!(third.window() != first.window(), "stale window reused");
    third.destroy();
    let dialog = waydock::ui::settings::error_dialog(Some(&app.application), None, "boom");
    ensure!(dialog.has_css_class("waydock-settings"), "dialog class");
    ensure_eq!(dialog.text().as_deref(), Some("Dock"));
    ensure_eq!(dialog.secondary_text().as_deref(), Some("boom"));
    dialog.response(gtk::ResponseType::Close);
    settle(0.05);
    ensure!(!dialog.is_visible(), "dialog stays");
    Ok(())
}

// ---- Python parity ----

const PYTHON: &str = r#####"
import sys, os, json, time, subprocess
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from dataclasses import replace
waydock_dir, mode, store_path, out = sys.argv[1:5]
sys.path[:0] = [waydock_dir, str(Path(waydock_dir).parent / "shared")]
import ctypes
ctypes.CDLL("libgtk4-layer-shell.so", mode=ctypes.RTLD_GLOBAL)
import gi
gi.require_version("Gtk", "4.0"); gi.require_version("Gdk", "4.0"); gi.require_version("Gtk4LayerShell", "1.0")
from gi.repository import Gdk, Gio, GLib, Gtk
with patch.dict(sys.modules, {"window_backend": SimpleNamespace(WindowBackend=object),
                              "desktop_actions": SimpleNamespace(DesktopActions=object)}):
    from waydock import SettingsWindow
from dock_config import ConfigStore, DockConfig
from dock_style import CSS
provider = Gtk.CssProvider(); provider.load_from_data(CSS)
Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

def settle(duration=0.05):
    end = time.monotonic() + duration
    context = GLib.MainContext.default()
    while time.monotonic() < end:
        while context.pending():
            context.iteration(False)
        time.sleep(0.002)

app = Gtk.Application(application_id=f"org.waydock.GPy{os.getpid()}", flags=Gio.ApplicationFlags.NON_UNIQUE)
app.register(None)
app.history = []
app.config = replace(DockConfig(pins=["browser.desktop", "terminal.desktop"]), edge="bottom", alignment="end", monitor="all",
    icon_size=44, gap=17, auto_hide=True, hide_delay_ms=780, transparency=20, match_wallpaper=False,
    monitor_overrides={"HEADLESS-1": {"edge": "left", "alignment": "start"},
                       "HEADLESS-2": {"edge": "top", "alignment": "end"},
                       "unplugged-monitor": {"edge": "right", "alignment": "center"}})
app.store = ConfigStore(Path(store_path))
app.config = app.store.save(app.config)
display = Gdk.Display.get_default()
app.monitor_entries = lambda: [(f"HEADLESS-{i + 1}", f"Test display {i + 1}", display.get_monitors().get_item(i))
                               for i in range(display.get_monitors().get_n_items())]
if mode == "rich":
    app.tray_apps = [{"id": "id:player", "label": "Player", "running": True}]
def preview(transparency, match_wallpaper, rounded_corners=None, float_mode=None, transparency_enabled=True, **rest):
    app.history.append(["preview", transparency if transparency_enabled else 0, match_wallpaper, rounded_corners, float_mode,
                        rest.get("frosted"), rest.get("frost_strength"), rest.get("bar_border_enabled"),
                        rest.get("bar_border_sides"), rest.get("bar_border_color")])
def restore():
    app.history.append(["restore"])
def save(config, rebuild=False):
    app.config = app.store.save(config)
    app.history.append(["save", rebuild])
    return True
app.preview_appearance, app.restore_appearance, app.save_config = preview, restore, save
w = SettingsWindow(app)
w.set_visible(True)
settle(0.3)
if mode == "screenshot":
    settle(1.0)
    subprocess.run(["grim", out], check=True, timeout=10)
    w.destroy()
    settle(0.1)
    raise SystemExit(0)
if mode == "rich":
    rgba = Gdk.RGBA()
    w.mode.set_active_id("bar")
    w.edge_widgets["HEADLESS-1"][0].set_active_id("top")
    w.edge_widgets["HEADLESS-2"][1].set_active_id("start")
    w.monitor.set_active_id("HEADLESS-1")
    w.separate_app_dock.set_active(True)
    w.app_dock_edge.set_active_id("bottom")
    w.app_dock_controls["app_dock_transparency"].set_value(72)
    w.app_dock_controls["app_dock_frosted"].set_active(True)
    w.app_dock_controls["app_dock_icon_size"].set_value(52)
    w.app_dock_controls["app_dock_shine"].set_active(True)
    rgba.parse("#475869"); w.app_dock_background_color.set_rgba(rgba)
    rgba.parse("#102030"); w.background_color.set_rgba(rgba)
    rgba.parse("#abcdef"); w.bar_border_color.set_rgba(rgba)
    w.bar_border_enabled.set_active(True)
    w.bar_border_switches["left"].set_active(False)
    w.size.set_value(52)
    w.gap.set_value(9)
    w.compact_bar.set_active(True)
    w.rounded_corners.set_active(True)
    w.transparency.set_value(74)
    w.frosted.set_active(True)
    w.frost_strength.set_value(37)
    w.application_display.set_active_id("title")
    w.group_switches["weather"].set_active(False)
    lc = w.layout_controls
    lc.controls["session"][0].set_selected(0)
    lc.controls["clock"][0].set_selected(0)
    lc.controls["clock"][1].emit("clicked")
    w.app_dock_layout_controls.controls["pinned"][0].set_selected(1)
    w.default_app_switches["network"].set_active(True)
    w.default_app_switches["audio"].set_active(False)
    w.tray_switches["id:player"].set_active(True)
    w.fullscreen.set_active(False)
    w.auto_hide.set_active(False)
    w.magnification.set_active(True)
    w.magnification_size.set_value(150)
    w.menu_hotkey_fullscreen.set_active(True)
    w.hot_corner_switches["HEADLESS-2"]["top_right"].set_active(True)
    w.hot_corner_delay.set_value(600)
w.apply()
settle(0.1)
Path(out).write_text(json.dumps(app.history))
"#####;

fn python(mode: &str, store: &Path, out: &Path) -> Result<(), String> {
    let script = tmp().join("g_settings_python.py");
    std::fs::write(&script, PYTHON).map_err(|e| e.to_string())?;
    let waydock_dir = std::env::var("WAYDOCK_PY_DIR").map_err(|_| "WAYDOCK_PY_DIR unset")?;
    let output = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .args([waydock_dir.as_str(), mode])
        .arg(store)
        .arg(out)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("python {mode} failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

fn rich_edits(window: &SettingsWindow) {
    let c = window.controls();
    c.mode.set_active_id(Some("bar"));
    c.edge_widgets["HEADLESS-1"].0.set_active_id(Some("top"));
    c.edge_widgets["HEADLESS-2"].1.set_active_id(Some("start"));
    c.monitor.set_active_id(Some("HEADLESS-1"));
    c.separate_app_dock.set_active(true);
    c.app_dock_edge.set_active_id(Some("bottom"));
    c.app_dock_controls["app_dock_transparency"].set_value(72.0);
    c.app_dock_controls["app_dock_frosted"].set_active(true);
    c.app_dock_controls["app_dock_icon_size"].set_value(52.0);
    c.app_dock_controls["app_dock_shine"].set_active(true);
    c.app_dock_background_color.button.set_rgba(&rgba("#475869"));
    c.background_color.button.set_rgba(&rgba("#102030"));
    c.bar_border_color.button.set_rgba(&rgba("#abcdef"));
    c.bar_border_enabled.set_active(true);
    c.bar_border_switches["left"].set_active(false);
    c.size.set_value(52.0);
    c.gap.set_value(9.0);
    c.compact_bar.set_active(true);
    c.rounded_corners.set_active(true);
    c.transparency.set_value(74.0);
    c.frosted.set_active(true);
    c.frost_strength.set_value(37.0);
    c.application_display.set_active_id(Some("title"));
    c.group_switches["weather"].set_active(false);
    let lc = &c.layout_controls;
    lc.controls("session").unwrap().0.set_selected(0);
    lc.controls("clock").unwrap().0.set_selected(0);
    lc.controls("clock").unwrap().1.emit_clicked();
    c.app_dock_layout_controls.controls("pinned").unwrap().0.set_selected(1);
    c.default_app_switches["network"].set_active(true);
    c.default_app_switches["audio"].set_active(false);
    window.tray_switches()["id:player"].set_active(true);
    c.fullscreen.set_active(false);
    c.auto_hide.set_active(false);
    c.magnification.set_active(true);
    c.magnification_size.set_value(150.0);
    c.menu_hotkey_fullscreen.set_active(true);
    c.hot_corner_switches["HEADLESS-2"]["top_right"].set_active(true);
    c.hot_corner_delay.set_value(600.0);
}

fn parity(app: &Rc<FakeApp>, mode: &str) -> R {
    if mode == "rich" {
        *app.tray.borrow_mut() = vec![TrayEntry { id: "id:player".into(), label: "Player".into(), running: true }];
    }
    let window = appearance_settings(app);
    settle(0.25);
    if mode == "rich" {
        rich_edits(&window);
    }
    window.apply();
    settle(0.1);
    let rust = app.bytes();
    let directory = tmp().join("parity").join(mode);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let store = directory.join("config.json");
    python(mode, &store, &directory.join("history.json"))?;
    let py = std::fs::read(&store).map_err(|e| e.to_string())?;
    std::fs::write(directory.join("rust-config.json"), &rust).unwrap();
    if rust != py {
        return Err(format!(
            "{mode}: saved config differs\n--- python\n{}\n--- rust\n{}",
            String::from_utf8_lossy(&py),
            String::from_utf8_lossy(&rust)
        ));
    }
    // The previews Python sent, in order, must equal ours (values only).
    let history: serde_json::Value = serde_json::from_slice(&std::fs::read(directory.join("history.json")).unwrap()).unwrap();
    let py_previews = history.as_array().unwrap().iter().filter(|e| e[0] == "preview").count();
    let rust_previews = app.history.borrow().iter().filter(|e| e.starts_with("appearance-preview")).count();
    ensure_eq!(py_previews, rust_previews);
    Ok(())
}

fn python_parity_noop_apply(app: &Rc<FakeApp>) -> R {
    parity(app, "noop")
}

fn python_parity_rich_edits(app: &Rc<FakeApp>) -> R {
    parity(app, "rich")
}

// ---- Screenshots ----

fn pixels(path: &Path) -> Result<(i32, i32, Vec<u8>), String> {
    let texture = gdk::Texture::from_filename(path).map_err(|e| e.to_string())?;
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (w, h) = (texture.width(), texture.height());
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        data.extend_from_slice(&bytes[y * stride..y * stride + w as usize * 4]);
    }
    Ok((w, h, data))
}

fn screenshots(app: &Rc<FakeApp>) -> R {
    let directory = tmp().join("screens");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let rust_png = directory.join("rust-settings.png");
    let python_png = directory.join("python-settings.png");
    let window = appearance_settings(app);
    settle(1.3);
    let status = Command::new("grim").arg(&rust_png).status().map_err(|e| e.to_string())?;
    ensure!(status.success(), "grim failed");
    window.destroy();
    settle(0.3);
    python("screenshot", &directory.join("py-config.json"), &python_png)?;
    let (w1, h1, a) = pixels(&rust_png)?;
    let (w2, h2, b) = pixels(&python_png)?;
    ensure!(w1 == w2 && h1 == h2, "sizes differ {w1}x{h1} vs {w2}x{h2}");
    for (name, data) in [("rust", &a), ("python", &b)] {
        let drawn = data.chunks(4).filter(|p| p[0] > 16 || p[1] > 16 || p[2] > 16).count();
        ensure!(drawn > 10_000, "{name} screenshot is blank ({drawn} drawn pixels)");
    }
    let mut diff = vec![0u8; a.len()];
    let mut count = 0usize;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1, -1);
    for i in 0..(w1 * h1) as usize {
        let (p, q) = (&a[i * 4..i * 4 + 4], &b[i * 4..i * 4 + 4]);
        if p != q {
            count += 1;
            let (x, y) = ((i as i32) % w1, (i as i32) / w1);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            diff[i * 4..i * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        } else {
            let grey = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3 / 3 + 170;
            diff[i * 4..i * 4 + 4].copy_from_slice(&[grey as u8, grey as u8, grey as u8, 255]);
        }
    }
    let texture = gdk::MemoryTexture::new(w1, h1, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(diff), (w1 * 4) as usize);
    texture.save_to_png(directory.join("diff.png")).map_err(|e| e.to_string())?;
    let summary = format!(
        "size {w1}x{h1}\ndiffering pixels {count} ({:.3}%)\nbounding box {}\n",
        count as f64 * 100.0 / (w1 * h1) as f64,
        if count > 0 { format!("x {x0}..{x1}, y {y0}..{y1}") } else { "none".into() }
    );
    std::fs::write(directory.join("diff.txt"), &summary).unwrap();
    println!("screenshot diff: {}", summary.replace('\n', "; "));
    Ok(())
}

fn main() {
    if std::env::var("WAYDOCK_G_HEADLESS").as_deref() != Ok("1")
        || std::env::var("XDG_RUNTIME_DIR").map(|d| d == "/run/user/1000").unwrap_or(true)
    {
        println!("g_settings_headless: skipped (run tests/run_g_headless.sh)");
        return;
    }
    suite_core::i18n::initialize();
    gtk::init().expect("gtk init");
    let provider = gtk::CssProvider::new();
    provider.load_from_string(waydock::appearance::style::CSS);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let filter = std::env::args().nth(1).unwrap_or_default();
    let cases: &[(&str, fn(&Rc<FakeApp>) -> R)] = &[
        ("appearance_controls_preview_without_saving_and_cancel_restores", appearance_controls_preview_without_saving_and_cancel_restores),
        ("appearance_apply_persists_new_fields_without_losing_pins_or_placement", appearance_apply_persists_new_fields_without_losing_pins_or_placement),
        ("appearance_failed_apply_retains_edits_and_window_close_reverts_preview", appearance_failed_apply_retains_edits_and_window_close_reverts_preview),
        ("dock_transparency_toggle_keeps_level_and_app_preferences", dock_transparency_toggle_keeps_level_and_app_preferences),
        ("dock_frosted_preview_and_save_preserve_app_choices", dock_frosted_preview_and_save_preserve_app_choices),
        ("mode_settings_remember_separate_appearance_and_preserve_placement", mode_settings_remember_separate_appearance_and_preserve_placement),
        ("hot_corner_settings_keep_corner_names_readable_with_long_monitor_labels", hot_corner_settings_keep_corner_names_readable_with_long_monitor_labels),
        ("cancel_keeps_visibility_and_hotkey_unchanged", cancel_keeps_visibility_and_hotkey_unchanged),
        ("apply_persists_each_group_and_hotkey_and_reopens_consistently", apply_persists_each_group_and_hotkey_and_reopens_consistently),
        ("failed_save_keeps_edits_available_and_does_not_change_configuration", failed_save_keeps_edits_available_and_does_not_change_configuration),
        ("hot_corner_controls_default_off_and_cancel_preserves_preferences", hot_corner_controls_default_off_and_cancel_preserves_preferences),
        ("hot_corner_apply_preserves_disconnected_monitors_and_other_preferences", hot_corner_apply_preserves_disconnected_monitors_and_other_preferences),
        ("settings_buttons_modes_apply_and_cancel", settings_buttons_modes_apply_and_cancel),
        ("position_dropdown_can_move_its_own_row", position_dropdown_can_move_its_own_row),
        ("separate_layout_settings_order_apply_reset_and_disable_preserve_bar", separate_layout_settings_order_apply_reset_and_disable_preserve_bar),
        ("settings_enable_only_on_top_bars_and_save_independent_values", settings_enable_only_on_top_bars_and_save_independent_values),
        ("default_apps_settings_save_cancel_and_reopen", default_apps_settings_save_cancel_and_reopen),
        ("settings_apply_and_cancel_keep_saved_pins_and_group_choices", settings_apply_and_cancel_keep_saved_pins_and_group_choices),
        ("preferences_cancel_apply_and_reopen_keep_offline_choices", preferences_cancel_apply_and_reopen_keep_offline_choices),
        ("empty_tray_subscription_and_refresh", empty_tray_subscription_and_refresh),
        ("sections_embedded_and_extra_controls", sections_embedded_and_extra_controls),
        ("show_settings_presents_one_window", show_settings_presents_one_window),
        ("python_parity_noop_apply", python_parity_noop_apply),
        ("python_parity_rich_edits", python_parity_rich_edits),
        ("screenshots", screenshots),
    ];
    let mut failures = 0;
    let mut ran = 0;
    for (name, case) in cases {
        if !filter.is_empty() && !name.contains(&filter) {
            continue;
        }
        ran += 1;
        let app = fake_app(name);
        let result = case(&app);
        teardown(&app);
        match result {
            Ok(()) => println!("ok   {name}"),
            Err(error) => {
                failures += 1;
                println!("FAIL {name}: {error}");
            }
        }
    }
    println!("g_settings_headless: {} passed, {failures} failed", ran - failures);
    if failures > 0 {
        std::process::exit(1);
    }
}
