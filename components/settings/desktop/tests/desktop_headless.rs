//! GTK checks of the desktop pages: ports of test_desktop_pages.py,
//! test_dock_pages.py, test_layout_presets.LayoutPageTests plus Lock/Login,
//! and fake-HOME runs of the pages.
//! Runs only inside the private headless labwc:
//!
//!   AGENT=gs-desktop OUTPUTS=2 components/settings/tests/headless.sh <this binary>
//!
//! Every service is a fake inside the private XDG_RUNTIME_DIR / private bus:
//! the tessel-dock Settings D-Bus API (the real Rust SettingsApi over a fake
//! host on its own connection and thread), tessel-shell.sock and pkexec.
#![allow(deprecated)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gs_base::{Host, PageHost, SettingsPage};
use gs_desktop::bridge::{self, BridgeOptions, DockSettingsBridge, PresetBridge};
use gs_desktop::dock::{self, DockPage, DockPageOptions, TilingPart, WorkspacePart};
use gs_desktop::layout::{LayoutOptions, LayoutSettings};
use gs_desktop::layout_presets::{self as presets, Preset, PresetIcon, TilingBackend, WorkspaceBackend, WorkspaceState};
use gs_desktop::icon_themes;
use gs_desktop::theme::{self, ColorSettings, ThemeBridge, ThemeOptions, ThemeWallpaperPage, MODES};
use gtk::prelude::*;
use serde_json::{json, Map, Value};
use tessel_dock::core::config::{ConfigStore, DockConfig};

// ------------------------------------------------------------------ harness

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait(mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        settle(0.02);
        if predicate() {
            return true;
        }
    }
    false
}

thread_local! {
    static PASSED: Cell<usize> = const { Cell::new(0) };
    static FAILED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn run(name: &str, test: impl FnOnce()) {
    gs_base::style::set_chooser(None);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test));
    gs_base::style::set_chooser(None);
    settle(0.05);
    match result {
        Ok(()) => {
            PASSED.with(|p| p.set(p.get() + 1));
            println!("ok   {name}");
        }
        Err(error) => {
            let message = error.downcast_ref::<String>().cloned()
                .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            println!("FAIL {name}: {message}");
            FAILED.with(|f| f.borrow_mut().push(name.to_owned()));
        }
    }
}

fn root() -> PathBuf {
    PathBuf::from(std::env::var("GS_ROOT").expect("GS_ROOT"))
}

fn scratch(name: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = root().join("tmp").join(format!("{name}-{}", COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct TestHost {
    window: gtk::ApplicationWindow,
    app: gtk::Application,
    errors: RefCell<Vec<(String, String, Option<String>, String)>>,
}

impl PageHost for TestHost {
    fn show_section(&self, _section: &str) {}
    fn application(&self) -> Option<gtk::Application> { Some(self.app.clone()) }
    fn window(&self) -> gtk::Window { self.window.clone().upcast() }
    fn show_page_error(&self, section: &str, error: &str, title: Option<&str>, icon: &str) {
        self.errors.borrow_mut().push((section.into(), error.into(), title.map(str::to_owned), icon.into()));
    }
}

fn host(app: &gtk::Application) -> (Rc<TestHost>, Host) {
    let window = gtk::ApplicationWindow::new(app);
    window.set_default_size(1100, 800);
    let host = Rc::new(TestHost { window, app: app.clone(), errors: RefCell::new(Vec::new()) });
    (host.clone(), host as Host)
}

fn show(host: &TestHost, page: &SettingsPage) {
    host.window.set_child(Some(&page.widget));
    host.window.present();
    settle(0.1);
}

fn finish(host: &TestHost, page: &SettingsPage) {
    page.close();
    host.window.destroy();
    settle(0.05);
}

fn connectors() -> Vec<String> {
    let monitors = gtk::gdk::Display::default().unwrap().monitors();
    (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast::<gtk::gdk::Monitor>())
        .map(|m| m.connector().map(|c| c.to_string()).unwrap_or_default()).collect()
}

fn config_with(values: Value) -> DockConfig {
    let Value::Object(mut base) = DockConfig::default().to_value() else { unreachable!() };
    for (k, v) in values.as_object().unwrap() {
        base.insert(k.clone(), v.clone());
    }
    DockConfig::from_value(&Value::Object(base)).unwrap()
}

fn replaced(config: &DockConfig, values: Value) -> DockConfig {
    let Value::Object(mut base) = config.to_value() else { unreachable!() };
    for (k, v) in values.as_object().unwrap() {
        base.insert(k.clone(), v.clone());
    }
    DockConfig::from_value(&Value::Object(base)).unwrap()
}

fn buttons(root: &gtk::Widget) -> Vec<gtk::Button> {
    let mut found = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            found.push(button.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
    found
}

// ------------------------------------------------------------- theme fakes

struct FakeBridge {
    config: RefCell<DockConfig>,
    calls: RefCell<Vec<DockConfig>>,
    closed: Cell<bool>,
    reject: Cell<bool>,
    refresh: RefCell<Option<Box<dyn Fn(&FakeBridge)>>>,
}

impl FakeBridge {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            config: RefCell::new(config_with(json!({"match_wallpaper": false, "transparency": 35, "pins": ["test.desktop"]}))),
            calls: RefCell::new(Vec::new()), closed: Cell::new(false), reject: Cell::new(false), refresh: RefCell::new(None),
        })
    }
    fn get(&self) -> DockConfig { self.config.borrow().clone() }
}

impl ThemeBridge for FakeBridge {
    fn config(&self) -> DockConfig { self.get() }
    fn save_config(&self, config: &DockConfig) -> bool {
        self.calls.borrow_mut().push(config.clone());
        if self.reject.get() {
            return false;
        }
        self.config.replace(config.clone());
        true
    }
    fn refresh(&self) -> bool {
        if let Some(refresh) = self.refresh.borrow().as_ref() {
            refresh(self);
        }
        true
    }
    fn close(&self) { self.closed.set(true) }
}

struct FakeColors {
    value: RefCell<String>,
    handlers: RefCell<Vec<Rc<dyn Fn()>>>,
    writes: RefCell<Vec<String>>,
}

impl ColorSettings for FakeColors {
    fn get_string(&self, _key: &str) -> String { self.value.borrow().clone() }
    fn set_string(&self, _key: &str, value: &str) -> bool {
        self.value.replace(value.to_owned());
        self.writes.borrow_mut().push(value.to_owned());
        let handlers = self.handlers.borrow().clone();
        for handler in handlers {
            handler();
        }
        true
    }
    fn connect_changed(&self, _key: &str, handler: Rc<dyn Fn()>) -> u64 {
        self.handlers.borrow_mut().push(handler);
        self.handlers.borrow().len() as u64
    }
    fn disconnect(&self, _id: u64) { self.handlers.borrow_mut().clear() }
}

struct FakeIcons {
    value: RefCell<Option<String>>,
    handlers: RefCell<Vec<Rc<dyn Fn()>>>,
    writes: RefCell<Vec<String>>,
    fail: Cell<bool>,
}

impl FakeIcons {
    fn new(value: &str) -> Rc<Self> {
        Rc::new(Self { value: RefCell::new(Some(value.into())), handlers: RefCell::default(), writes: RefCell::default(),
                       fail: Cell::new(false) })
    }
    /// Another program changed the setting.
    fn external(&self, value: &str) {
        self.value.replace(Some(value.into()));
        let handlers = self.handlers.borrow().clone();
        for handler in handlers {
            handler();
        }
    }
}

impl icon_themes::IconThemeSettings for FakeIcons {
    fn current(&self) -> Option<String> { self.value.borrow().clone() }
    fn set(&self, theme: &str) -> Result<(), String> {
        self.writes.borrow_mut().push(theme.into());
        if self.fail.get() {
            return Err("read-only".into());
        }
        self.external(theme);
        Ok(())
    }
    fn connect_changed(&self, handler: Rc<dyn Fn()>) -> u64 {
        self.handlers.borrow_mut().push(handler);
        self.handlers.borrow().len() as u64
    }
    fn disconnect(&self, _id: u64) { self.handlers.borrow_mut().clear() }
}

/// A fake XDG data dir: two real themes (read-only symlinks, so the
/// screenshot shows real icons), a fake one with its own folder icon, and
/// entries that must not be offered.
fn fake_icon_dir(root: &Path) -> PathBuf {
    let icons = root.join("share/icons");
    std::fs::create_dir_all(&icons).unwrap();
    for theme in ["Papirus-Dark", "Adwaita"] {
        let real = Path::new("/usr/share/icons").join(theme);
        if real.join("index.theme").is_file() {
            std::os::unix::fs::symlink(&real, icons.join(theme)).unwrap();
        } else {
            write_theme(&icons, theme, theme);
        }
    }
    write_theme(&icons, "test-icons", "Test Icons");
    std::fs::write(icons.join("test-icons/places/folder.svg"),
        "<svg xmlns='http://www.w3.org/2000/svg' width='32' height='32'><rect width='32' height='24' y='4' rx='3' fill='#e0b030'/></svg>").unwrap();
    std::fs::create_dir_all(icons.join("cursors-only")).unwrap();
    std::fs::write(icons.join("cursors-only/index.theme"), "[Icon Theme]\nName=Cursors Only\n").unwrap();
    write_theme(&icons, "hicolor", "Hicolor");
    write_theme(&icons, "mirror", "Private Mirror");
    std::fs::write(icons.join("mirror/.tessel-dock-papirus.json"), "{}").unwrap();
    icons
}

fn write_theme(icons: &Path, id: &str, name: &str) {
    std::fs::create_dir_all(icons.join(id).join("places")).unwrap();
    std::fs::write(icons.join(id).join("index.theme"),
        format!("[Icon Theme]\nName={name}\nDirectories=places\n\n[places]\nSize=32\nType=Scalable\nMinSize=8\nMaxSize=512\n")).unwrap();
}

fn dropdown_names(dropdown: &gtk::DropDown) -> Vec<String> {
    let model = dropdown.model().unwrap();
    (0..model.n_items()).map(|i| model.item(i).and_downcast::<gtk::StringObject>().unwrap().string().to_string()).collect()
}

/// grim inside the private nested session only (main() checked the session).
fn screenshot(name: &str) -> PathBuf {
    let path = root().join(format!("{name}.png"));
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(runtime != "/run/user/1000" && runtime.starts_with(root().to_str().unwrap()), "private session only");
    let status = std::process::Command::new("/usr/bin/grim").arg(&path).status().expect("grim");
    assert!(status.success(), "grim failed");
    path
}

fn icon_tests(app: &gtk::Application) {
    run("icons_dropdown_lists_installed_themes_and_saves_through_the_backend", || {
        let (test_host, host) = host(app);
        let dir = scratch("icons");
        let icons = fake_icon_dir(&dir);
        let backend = FakeIcons::new("breeze");
        let page = theme::build_theme_page(&host, ThemeOptions {
            bridge: Some(FakeBridge::new()), config_path: Some(dir.join("wallpapers.json")), color_settings: Some(None),
            icon_settings: Some(backend.clone()), icon_dirs: Some(vec![icons.clone()]), ..Default::default()
        }).unwrap();
        show(&test_host, &page);
        let c = page.controller::<ThemeWallpaperPage>().unwrap();
        let chooser = c.icons.clone();
        let ids: Vec<&str> = chooser.themes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["Adwaita", "Papirus-Dark", "test-icons"]);
        assert_eq!(dropdown_names(&chooser.dropdown), ["Adwaita", "Papirus-Dark", "Test Icons"]);
        // "breeze" is not installed: Papirus-Dark is shown, nothing is saved on open.
        assert_eq!(chooser.selected_id().as_deref(), Some("Papirus-Dark"));
        assert!(backend.writes.borrow().is_empty());
        assert!(page.search_targets.borrow().contains_key("icon_theme"));
        settle(0.3);
        let shot = screenshot("icons-page");
        println!("screenshot {}", shot.display());
        chooser.dropdown.set_selected(2);
        assert_eq!(*backend.writes.borrow(), ["test-icons"]);
        let files = chooser.sample_files();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].as_deref(), Some(icons.join("test-icons/places/folder.svg").as_path()), "{files:?}");
        // Another program switches the theme: the page follows without saving.
        backend.external("Adwaita");
        assert_eq!(chooser.selected_id().as_deref(), Some("Adwaita"));
        assert_eq!(backend.writes.borrow().len(), 1);
        // A failed save is reported and the saved theme stays selected.
        backend.fail.set(true);
        chooser.dropdown.set_selected(1);
        assert_eq!(*backend.writes.borrow(), ["test-icons", "Papirus-Dark"]);
        assert_eq!(chooser.selected_id().as_deref(), Some("Adwaita"));
        assert!(chooser.status.get_visible());
        assert!(chooser.status.text().contains("read-only"));
        finish(&test_host, &page);
        assert!(backend.handlers.borrow().is_empty());
    });
    run("icons_row_without_themes_is_disabled", || {
        let (test_host, host) = host(app);
        let dir = scratch("noicons");
        let page = theme::build_theme_page(&host, ThemeOptions {
            bridge: Some(FakeBridge::new()), config_path: Some(dir.join("wallpapers.json")), color_settings: Some(None),
            icon_settings: Some(FakeIcons::new("Papirus-Dark")), icon_dirs: Some(vec![dir.join("none")]), ..Default::default()
        }).unwrap();
        show(&test_host, &page);
        let c = page.controller::<ThemeWallpaperPage>().unwrap();
        assert!(!c.icons.dropdown.is_sensitive());
        assert_eq!(c.icons.status.text(), "No icon themes are installed.");
        finish(&test_host, &page);
    });
}

struct ThemeFx {
    host: Rc<TestHost>,
    page: SettingsPage,
    c: Rc<ThemeWallpaperPage>,
    bridge: Rc<FakeBridge>,
    colors: Rc<FakeColors>,
    requests: Arc<Mutex<Vec<Value>>>,
    dir: PathBuf,
}

type Request = Arc<dyn Fn(&Value, &Arc<Mutex<Vec<Value>>>) -> Result<Value, String> + Send + Sync>;

fn theme_fx(app: &gtk::Application, bridge: Option<Rc<FakeBridge>>, request: Option<Request>) -> ThemeFx {
    let (test_host, host) = host(app);
    let bridge = bridge.unwrap_or_else(FakeBridge::new);
    let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
    let monitors = connectors();
    let request = request.unwrap_or_else(|| Arc::new(move |payload: &Value, seen: &Arc<Mutex<Vec<Value>>>| {
        seen.lock().unwrap().push(payload.clone());
        Ok(json!({"ok": true, "monitors": monitors}))
    }));
    let seen = requests.clone();
    let colors = Rc::new(FakeColors { value: RefCell::new("prefer-dark".into()), handlers: RefCell::default(), writes: RefCell::default() });
    let dir = scratch("theme");
    let page = theme::build_theme_page(&host, ThemeOptions {
        bridge: Some(bridge.clone()), request: Some(Arc::new(move |payload| request(payload, &seen))),
        config_path: Some(dir.join("wallpapers.json")), color_settings: Some(Some(colors.clone())),
        icon_settings: Some(FakeIcons::new("Papirus-Dark")), icon_dirs: Some(vec![dir.join("icons")]),
    }).unwrap();
    show(&test_host, &page);
    let c = page.controller::<ThemeWallpaperPage>().unwrap();
    ThemeFx { host: test_host, page, c, bridge, colors, requests, dir }
}

impl ThemeFx {
    fn done(self) {
        finish(&self.host, &self.page);
    }
    fn requests(&self) -> Vec<Value> { self.requests.lock().unwrap().clone() }
}

fn theme_tests(app: &gtk::Application) {
    run("wallpaper_has_per_monitor_controls_without_saving_on_open", || {
        let fx = theme_fx(app, None, None);
        assert!(!fx.c.rows.borrow().is_empty());
        assert!(fx.requests().is_empty());
        assert!(fx.bridge.calls.borrow().is_empty());
        assert!(!fx.dir.join("wallpapers.json").exists());
        for row in fx.c.rows.borrow().values() {
            assert_eq!(row.mode.model().unwrap().n_items(), 6);
        }
        fx.done();
    });
    run("global_palette_toggle_only_changes_shared_match_preference", || {
        let fx = theme_fx(app, None, None);
        fx.c.r#match.set_active(true);
        let config = fx.bridge.get();
        assert!(config.match_wallpaper);
        assert_eq!(config.transparency, 35);
        assert_eq!(config.pins, ["test.desktop"]);
        assert_eq!(fx.bridge.calls.borrow().len(), 1);
        assert!(fx.requests().is_empty());
        fx.page.close();
        assert!(fx.bridge.closed.get());
        fx.done();
    });
    run("failed_palette_update_restores_toggle", || {
        let fx = theme_fx(app, None, None);
        fx.bridge.reject.set(true);
        fx.c.r#match.set_active(true);
        assert!(!fx.c.r#match.is_active());
        assert!(!fx.bridge.get().match_wallpaper);
        fx.done();
    });
    run("transparency_controls_save_independently_and_preserve_disabled_levels", || {
        let fx = theme_fx(app, None, None);
        let c = &fx.c;
        assert!(c.dock_transparency_enabled.is_active());
        assert!(!c.apps_transparency_enabled.is_active());
        assert!(!c.apps_transparency.is_sensitive());
        c.apps_transparency_enabled.set_active(true);
        c.apps_transparency.set_value(62.0);
        settle(0.3);
        assert_eq!(fx.bridge.get().apps_transparency, 62);
        assert_eq!(fx.bridge.get().transparency, 35);
        c.dock_transparency_enabled.set_active(false);
        assert!(!c.dock_transparency.is_sensitive());
        assert_eq!(fx.bridge.get().transparency, 35);
        assert!(fx.bridge.get().apps_transparency_enabled);
        c.apps_transparency_enabled.set_active(false);
        assert_eq!(fx.bridge.get().apps_transparency, 62);
        c.apps_transparency_enabled.set_active(true);
        assert_eq!(c.apps_transparency.value(), 62.0);
        assert!(c.apps_transparency.is_sensitive());
        assert!(!fx.bridge.get().transparency_enabled);
        assert_eq!(fx.bridge.get().pins, ["test.desktop"]);
        assert!(fx.requests().is_empty());
        fx.done();
    });
    run("transparency_drag_is_debounced_and_flushed_on_navigation", || {
        let fx = theme_fx(app, None, None);
        for value in [40.0, 41.0, 42.0, 43.0] {
            fx.c.dock_transparency.set_value(value);
        }
        assert!(fx.bridge.calls.borrow().is_empty());
        settle(0.3);
        assert_eq!(fx.bridge.calls.borrow().len(), 1);
        assert_eq!(fx.bridge.get().transparency, 43);
        fx.c.dock_transparency.set_value(54.0);
        fx.page.deactivate();
        assert_eq!(fx.bridge.calls.borrow().len(), 2);
        assert_eq!(fx.bridge.get().transparency, 54);
        assert!(!fx.c.appearance_pending_timer());
        settle(0.3);
        assert_eq!(fx.bridge.calls.borrow().len(), 2);
        fx.done();
    });
    run("frosted_strength_and_transparency_are_independent_for_each_background", || {
        let fx = theme_fx(app, None, None);
        let c = &fx.c;
        for (field, level_field, alpha_field, prefix) in [("frosted", "frost_strength", "transparency", "dock"),
                                                          ("apps_frosted", "apps_frost_strength", "apps_transparency", "apps")] {
            let (toggle, strength) = (c.toggle(field), c.strength(level_field));
            let (enabled, transparency) = c.transparency_pair(prefix);
            assert!(toggle.is_sensitive());
            assert!(!strength.is_sensitive());
            assert_eq!(strength.value(), 50.0);
            enabled.set_active(false);
            let previous = fx.bridge.get();
            toggle.set_active(true);
            assert!(strength.is_sensitive());
            assert_eq!(fx.bridge.get(), replaced(&previous, json!({field: true})));
            strength.set_value(72.0);
            fx.page.deactivate();
            assert_eq!(fx.bridge.get(), replaced(&previous, json!({field: true, level_field: 72})));
            assert!(!transparency.is_sensitive());
            assert!(transparency.tooltip_text().unwrap().contains("fully transparent"));
            enabled.set_active(true);
            transparency.set_value(58.0);
            fx.page.deactivate();
            assert_eq!(fx.bridge.get().to_value()[level_field], json!(72));
            assert_eq!(fx.bridge.get().to_value()[alpha_field], json!(58));
            toggle.set_active(false);
            assert!(!strength.is_sensitive());
            assert_eq!(strength.value(), 72.0);
            assert_eq!(fx.bridge.get().to_value()[level_field], json!(72));
            assert!(transparency.is_sensitive());
        }
        assert!(fx.requests().is_empty());
        fx.done();
    });
    run("frost_strength_drag_is_debounced_and_flushed_on_navigation", || {
        let bridge = FakeBridge::new();
        bridge.config.replace(replaced(&bridge.get(), json!({"frosted": true, "apps_frosted": true})));
        let fx = theme_fx(app, Some(bridge), None);
        for value in [55.0, 60.0, 70.0, 80.0] {
            fx.c.strength("frost_strength").set_value(value);
        }
        assert!(fx.bridge.calls.borrow().is_empty());
        settle(0.3);
        assert_eq!(fx.bridge.calls.borrow().len(), 1);
        assert_eq!((fx.bridge.get().frost_strength, fx.bridge.get().apps_frost_strength, fx.bridge.get().transparency), (80, 50, 35));
        fx.c.strength("apps_frost_strength").set_value(40.0);
        fx.page.deactivate();
        assert_eq!(fx.bridge.calls.borrow().len(), 2);
        assert_eq!((fx.bridge.get().apps_frost_strength, fx.bridge.get().frost_strength), (40, 80));
        assert!(!fx.c.appearance_pending_timer());
        settle(0.3);
        assert_eq!(fx.bridge.calls.borrow().len(), 2);
        fx.done();
    });
    run("failed_frost_strength_update_restores_saved_levels_and_sensitivity", || {
        let bridge = FakeBridge::new();
        bridge.config.replace(replaced(&bridge.get(), json!({"frosted": true, "apps_frosted": true, "frost_strength": 65, "apps_frost_strength": 28})));
        let fx = theme_fx(app, Some(bridge), None);
        fx.bridge.reject.set(true);
        let previous = fx.bridge.get();
        fx.c.strength("frost_strength").set_value(90.0);
        fx.c.strength("apps_frost_strength").set_value(70.0);
        fx.page.deactivate();
        assert_eq!(fx.c.strength("frost_strength").value(), 65.0);
        assert_eq!(fx.c.strength("apps_frost_strength").value(), 28.0);
        assert!(fx.c.strength("frost_strength").is_sensitive());
        assert!(fx.c.strength("apps_frost_strength").is_sensitive());
        assert_eq!(fx.bridge.get(), previous);
        fx.done();
    });
    run("application_corners_do_not_change_dock_or_transparency_preferences", || {
        let fx = theme_fx(app, None, None);
        let previous = fx.bridge.get();
        let corners = fx.c.toggle("apps_rounded_corners");
        assert!(corners.is_active() && corners.is_sensitive());
        corners.set_active(false);
        assert_eq!(fx.bridge.get(), replaced(&previous, json!({"apps_rounded_corners": false})));
        assert!(!fx.c.apps_transparency_enabled.is_active());
        assert!(fx.c.toggle("apps_frosted").is_sensitive());
        corners.set_active(true);
        assert_eq!(fx.bridge.get(), previous);
        fx.done();
    });
    run("failed_frosted_and_corners_updates_restore_saved_values", || {
        let bridge = FakeBridge::new();
        bridge.config.replace(replaced(&bridge.get(), json!({"apps_transparency_enabled": true})));
        let fx = theme_fx(app, Some(bridge), None);
        fx.bridge.reject.set(true);
        let previous = fx.bridge.get();
        fx.c.toggle("frosted").set_active(true);
        assert!(!fx.c.toggle("frosted").is_active());
        assert!(fx.c.toggle("frosted").is_sensitive());
        assert!(fx.c.dock_transparency.tooltip_text().unwrap().contains("fully transparent"));
        fx.c.toggle("apps_frosted").set_active(true);
        assert!(!fx.c.toggle("apps_frosted").is_active());
        fx.c.toggle("apps_rounded_corners").set_active(false);
        assert!(fx.c.toggle("apps_rounded_corners").is_active());
        assert_eq!(fx.bridge.get(), previous);
        fx.done();
    });
    run("failed_transparency_update_restores_both_controls_and_sensitivity", || {
        let fx = theme_fx(app, None, None);
        fx.bridge.reject.set(true);
        fx.c.apps_transparency_enabled.set_active(true);
        assert!(!fx.c.apps_transparency_enabled.is_active());
        assert!(!fx.c.apps_transparency.is_sensitive());
        fx.c.dock_transparency.set_value(90.0);
        settle(0.3);
        assert_eq!(fx.c.dock_transparency.value(), 35.0);
        assert!(fx.c.dock_transparency_enabled.is_active());
        assert!(fx.c.dock_transparency.is_sensitive());
        assert_eq!(fx.bridge.get().transparency, 35);
        fx.done();
    });
    run("appearance_reloads_from_dock_page_without_an_extra_write", || {
        let fx = theme_fx(app, None, None);
        fx.bridge.refresh.replace(Some(Box::new(|bridge: &FakeBridge| {
            let config = replaced(&bridge.get(), json!({"transparency": 70, "transparency_enabled": false, "frosted": true,
                "frost_strength": 78, "apps_transparency": 45, "apps_transparency_enabled": true, "match_wallpaper": true,
                "apps_frosted": true, "apps_frost_strength": 32, "apps_rounded_corners": false}));
            bridge.config.replace(config);
        })));
        fx.page.activate();
        let c = &fx.c;
        assert_eq!(c.dock_transparency.value(), 70.0);
        assert_eq!(c.strength("frost_strength").value(), 78.0);
        assert!(c.strength("frost_strength").is_sensitive());
        assert_eq!(c.strength("apps_frost_strength").value(), 32.0);
        assert!(!c.dock_transparency_enabled.is_active());
        assert!(!c.dock_transparency.is_sensitive());
        assert!(c.toggle("frosted").is_active());
        assert_eq!(c.apps_transparency.value(), 45.0);
        assert!(c.apps_transparency_enabled.is_active() && c.apps_transparency.is_sensitive());
        assert!(c.toggle("apps_frosted").is_active());
        assert!(!c.toggle("apps_rounded_corners").is_active());
        assert!(c.r#match.is_active());
        assert!(fx.bridge.calls.borrow().is_empty());
        fx.done();
    });
    run("closing_flushes_pending_slider_once", || {
        let fx = theme_fx(app, None, None);
        fx.c.dock_transparency.set_value(58.0);
        fx.page.close();
        assert!(fx.bridge.closed.get());
        assert_eq!(fx.bridge.get().transparency, 58);
        assert!(!fx.c.appearance_pending_timer());
        settle(0.3);
        assert_eq!(fx.bridge.calls.borrow().len(), 1);
        fx.done();
    });
    run("wallpaper_apply_uses_existing_tessel_shell_protocol_only_for_changed_display", || {
        let fx = theme_fx(app, None, None);
        let connector = fx.c.rows.borrow().keys().next().unwrap().clone();
        fx.c.rows.borrow()[&connector].mode.set_selected(MODES.iter().position(|m| *m == "Color Only").unwrap() as u32);
        fx.c.apply(None);
        assert!(wait(|| !fx.c.busy.get()));
        let requests = fx.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], json!({"cmd": "list-monitors"}));
        assert_eq!(requests[1]["monitor"], json!(connector));
        assert_eq!(requests[1]["mode"], json!("Color Only"));
        assert!(requests[1]["file"].is_null());
        assert!(!fx.c.rows.borrow()[&connector].dirty.get());
        assert!(!fx.dir.join("wallpapers.json").exists());
        fx.done();
    });
    run("style_selector_writes_shared_color_scheme", || {
        let fx = theme_fx(app, None, None);
        assert!(fx.c.scheme_buttons["prefer-dark"].is_active());
        fx.c.scheme_buttons["prefer-light"].set_active(true);
        assert_eq!(fx.colors.writes.borrow().clone(), ["prefer-light"]);
        fx.colors.set_string("color-scheme", "default");
        assert!(fx.c.scheme_buttons["default"].is_active());
        assert_eq!(fx.colors.writes.borrow().clone(), ["prefer-light", "default"]);
        assert!(fx.bridge.calls.borrow().is_empty());
        fx.done();
    });
    run("unapplied_wallpaper_prompts_before_closing", || {
        let answers: Rc<RefCell<Vec<i32>>> = Rc::default();
        let queue = answers.clone();
        gs_base::style::set_chooser(Some(Rc::new(move |_m, _d, _b, callback, _default, _cancel| {
            let answer = queue.borrow_mut().remove(0);
            callback(Some(answer));
        })));
        let fx = theme_fx(app, None, None);
        assert_eq!(fx.c.apply_button.label().unwrap(), "Apply wallpaper");
        assert!(fx.page.can_close());
        let connector = fx.c.rows.borrow().keys().next().unwrap().clone();
        fx.c.rows.borrow()[&connector].mode.set_selected(5);
        assert!(fx.c.discard_button.is_sensitive());
        answers.borrow_mut().push(2);
        assert!(!fx.page.can_close());
        assert!(fx.c.dirty());
        answers.borrow_mut().push(1);
        assert!(!fx.page.can_close()); // Discard, then the host is closed again
        assert!(!fx.c.dirty());
        assert_eq!(fx.c.rows.borrow()[&connector].mode.selected(), 0);
        assert!(fx.requests().is_empty());
        fx.done();
    });
    run("palette_reloads_on_navigation_without_an_extra_write", || {
        let fx = theme_fx(app, None, None);
        fx.bridge.refresh.replace(Some(Box::new(|bridge: &FakeBridge| {
            let config = replaced(&bridge.get(), json!({"match_wallpaper": true}));
            bridge.config.replace(config);
        })));
        fx.page.activate();
        assert!(fx.c.r#match.is_active());
        assert!(fx.bridge.calls.borrow().is_empty());
        fx.done();
    });
    run("partial_wallpaper_failure_keeps_unsaved_monitor_dirty", || {
        let monitors = connectors();
        assert!(monitors.len() >= 2, "Two private outputs are required (OUTPUTS=2)");
        let failing = monitors[1].clone();
        let list = monitors.clone();
        let fx = theme_fx(app, None, Some(Arc::new(move |payload: &Value, seen: &Arc<Mutex<Vec<Value>>>| {
            seen.lock().unwrap().push(payload.clone());
            if payload["cmd"] == "set" && payload["monitor"] == json!(failing) {
                return Err("Test wallpaper service error".into());
            }
            Ok(json!({"ok": true, "monitors": list}))
        })));
        for row in fx.c.rows.borrow().values() {
            row.mode.set_selected(5);
        }
        fx.c.apply(None);
        assert!(wait(|| !fx.c.busy.get()));
        assert!(!fx.c.rows.borrow()[&monitors[0]].dirty.get());
        assert!(fx.c.rows.borrow()[&monitors[1]].dirty.get());
        assert!(fx.c.status.text().contains("Some wallpapers were applied"));
        assert!(fx.c.apply_button.is_sensitive());
        fx.done();
    });
    run("missing_wallpaper_file_fails_before_service_changes", || {
        let fx = theme_fx(app, None, None);
        {
            let rows = fx.c.rows.borrow();
            let row = rows.values().next().unwrap();
            row.path.replace(Some("/definitely-missing-test-picture.png".into()));
            row.dirty.set(true);
        }
        fx.c.apply(None);
        assert!(fx.requests().is_empty());
        assert!(fx.c.status.text().contains("Choose an existing"));
        fx.done();
    });
    run("disconnected_monitor_fails_preflight_without_any_changes", || {
        let fx = theme_fx(app, None, Some(Arc::new(|payload: &Value, seen: &Arc<Mutex<Vec<Value>>>| {
            seen.lock().unwrap().push(payload.clone());
            Ok(json!({"ok": true, "monitors": []}))
        })));
        fx.c.rows.borrow().values().next().unwrap().mode.set_selected(5);
        fx.c.apply(None);
        assert!(wait(|| !fx.c.busy.get()));
        assert_eq!(fx.requests(), [json!({"cmd": "list-monitors"})]);
        assert!(fx.c.status.text().contains("disconnected"));
        fx.done();
    });
    run("theme_search_targets_use_settings_search_names", || {
        let fx = theme_fx(app, None, None);
        for name in ["match", "dock_transparency", "apps_transparency", "frosted", "apps_frosted", "frost_strength",
                     "apps_frost_strength", "apps_rounded_corners"] {
            assert!(fx.page.search_targets.borrow().contains_key(name), "{name}");
        }
        fx.done();
    });
}

// ----------------------------------------------------------------- displays

use tessel_displays::compat::brightness_client::BrightnessModel;
use tessel_displays::window::{self as wd_window, Hooks};

struct FakeClient {
    on_changed: RefCell<Rc<dyn Fn()>>,
    state: RefCell<Value>,
    calls: RefCell<Vec<(String, String, i64)>>,
    started: Cell<bool>,
    closed: Cell<bool>,
}

impl BrightnessModel for FakeClient {
    fn state(&self) -> Value { self.state.borrow().clone() }
    fn ready(&self) -> bool { true }
    fn error(&self) -> String { String::new() }
    fn set_value(&self, connector: &str, feature: &str, value: f64, _expected: Option<&Value>) -> Result<bool, String> {
        self.calls.borrow_mut().push((connector.into(), feature.into(), value as i64));
        {
            let mut state = self.state.borrow_mut();
            if let Some(target) = state["displays"].as_array_mut().unwrap().iter_mut().find(|d| d["connector"] == connector) {
                target[feature]["value"] = json!(value as i64);
            }
        }
        let callback = self.on_changed.borrow().clone();
        callback();
        Ok(true)
    }
    fn set_auto_dim(&self, _enabled: bool) -> Result<bool, String> { Ok(true) }
    fn set_settings(&self, _settings: &Value) -> Result<bool, String> { Ok(true) }
    fn start(&self) -> Result<(), String> {
        self.started.set(true);
        let callback = self.on_changed.borrow().clone();
        callback();
        Ok(())
    }
    fn close(&self) { self.closed.set(true) }
}

fn display(connector: &str, value: i64) -> Value {
    json!({"connector": connector, "id": format!("fixture-{connector}"), "enabled": true, "error": "",
           "brightness": {"supported": true, "value": value, "busy": false, "error": ""},
           "contrast": {"supported": false, "value": null, "busy": false, "error": ""}})
}

fn head(connector: &str, description: &str) -> Value {
    let mode = json!({"WIDTH": 1920, "HEIGHT": 1080, "REFRESH_MHZ": 60000});
    json!({"NAME": connector, "DESCRIPTION": description, "MAKE": "Example", "MODEL": "Display",
           "MODES": [mode.clone()], "CURRENT": {"MODE": mode, "ENABLED": true, "SCALE": 1.0, "VRR": false, "TRANSFORM": "NORMAL"}})
}

fn display_hooks() -> Hooks {
    Hooks {
        ipc_operation: Rc::new(|op, _| panic!("Opening must not write ({op})")),
        ipc_config_operation: Rc::new(|op, _| panic!("Opening must not apply ({op})")),
        get_config_data: Rc::new(|| panic!("no GET in UI tests")),
        confirm_seconds: 0,
        get_xwayland_primary_output: Rc::new(|| None),
        apply_xwayland_primary_output: Rc::new(|_| panic!("Opening must not apply")),
        read_labwc_primary_environment: Rc::new(|_| None),
        write_labwc_primary_environment: Rc::new(|_, _| panic!("unexpected environment write")),
        write_labwc_primary_xinitrc: Rc::new(|_, _| panic!("unexpected xinitrc write")),
        read_labwc_hdr_config: Rc::new(|_| tessel_displays::labwc::HdrConfig { global: false, overrides: Default::default(),
                                                                         managed: Default::default(), text: String::new() }),
        write_labwc_hdr_overrides: Rc::new(|_, _| panic!("unexpected rc.xml write")),
        reconfigure_labwc: Rc::new(|| (true, None)),
    }
}

fn display_page(app: &gtk::Application) -> (Rc<TestHost>, SettingsPage, Rc<gs_desktop::displays::DisplaysPage>, Rc<FakeClient>) {
    wd_window::set_default_hooks(Some(display_hooks()));
    tessel_displays::color_config::set_status_override(Some(Rc::new(Default::default)));
    let monitors = gtk::gdk::Display::default().unwrap().monitors();
    let descriptions: Vec<String> = (0..monitors.n_items()).filter_map(|i| monitors.item(i).and_downcast::<gtk::gdk::Monitor>())
        .map(|m| m.description().map(|d| d.to_string()).unwrap_or_default()).collect();
    tessel_displays::ipc::set_config_data(json!({"CFG": {"AUTO_SCALE": true, "CALLBACK_CMD": "notify-send saved"},
        "STATE": {"HEADS": [head("DP-1", &descriptions[0]), head("HDMI-A-1", descriptions.get(1).unwrap_or(&descriptions[0]))]}}));
    let client = Rc::new(FakeClient {
        on_changed: RefCell::new(Rc::new(|| {})),
        state: RefCell::new(json!({"displays": [display("DP-1", 70), display("HDMI-A-1", 40)],
                                   "settings": {"version": 1, "auto_dim": false, "dim_after": 300, "dim_percent": 30}})),
        calls: RefCell::default(), started: Cell::new(false), closed: Cell::new(false),
    });
    let (test_host, host) = host(app);
    let fake = client.clone();
    let page = gs_desktop::displays::build_displays_page(&host, gs_desktop::displays::DisplaysOptions {
        brightness_client_factory: Some(Box::new(move |changed| {
            fake.on_changed.replace(changed);
            Ok(fake as Rc<dyn BrightnessModel>)
        })),
        load_data: Some(false), hdr_status: None, launch: None,
    });
    show(&test_host, &page);
    let controller = page.controller::<gs_desktop::displays::DisplaysPage>().unwrap();
    (test_host, page, controller, client)
}

fn display_tests(app: &gtk::Application) {
    run("displays_embeds_all_controls_without_applying_initial_values", || {
        let (host, page, c, client) = display_page(app);
        let owner = c.owner.borrow().clone().expect("embedded editor");
        assert!(!owner.window.is_visible());
        assert_eq!(owner.monitor_widgets.borrow().len(), 2);
        assert!(!owner.monitor_widgets.borrow()[0].color_controls.changed());
        assert!(owner.auto_scale_switch.is_active());
        assert!(owner.notify_toggle.is_active());
        assert_eq!(owner.overlay_count(), 0);
        assert!(client.started.get());
        assert!(client.calls.borrow().is_empty());
        assert_eq!(page.widget.root().map(|r| r.upcast::<gtk::Widget>()), Some(host.window.clone().upcast()));
        page.close();
        assert!(client.closed.get());
        finish(&host, &page);
    });
    run("embedded_brightness_retains_existing_backend_and_apply_independence", || {
        let (host, page, c, client) = display_page(app);
        let owner = c.owner.borrow().clone().unwrap();
        let controls = owner.monitor_widgets.borrow()[0].brightness_controls.clone();
        controls.scales["brightness"].set_value(55.0);
        settle(0.3);
        assert_eq!(client.calls.borrow().clone(), [("DP-1".to_owned(), "brightness".to_owned(), 55)]);
        assert!(!owner.save_btn.is_sensitive());
        finish(&host, &page);
    });
    run("display_identify_is_explicit_and_deactivation_cancels_it", || {
        let (host, page, c, _client) = display_page(app);
        c.identify();
        let owner = c.owner.borrow().clone().unwrap();
        assert_eq!(owner.overlay_count(), 2);
        assert!(c.overlay_timer_active());
        page.deactivate();
        assert!(!c.overlay_timer_active());
        assert_eq!(owner.overlay_count(), 0);
        finish(&host, &page);
    });
    run("calibrate_hdr_is_offered_only_for_hdr_monitors", || {
        let status: tessel_displays::color_config::Status = serde_json::from_value(json!({"compositor": {"pid": "1"},
            "DP-1": {"enabled": "true", "hdr_supported": "true"}, "HDMI-A-1": {"enabled": "true", "hdr_supported": "false"}})).unwrap();
        assert_eq!(gs_desktop::displays::hdr_outputs(&status), ["DP-1"]);
        let (host, page, c, _client) = display_page(app);
        assert!(!c.calibrate.borrow().as_ref().unwrap().is_visible());
        let launcher = PathBuf::from(std::env::var("HOME").unwrap()).join(".local/lib/tessel/tessel-hdr-calibration");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&launcher, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let launched: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
        let record = launched.clone();
        c.hdr.replace(vec!["DP-1".into()]);
        c.launch.replace(Rc::new(move |command| {
            record.borrow_mut().push(command.to_vec());
            Ok(())
        }));
        c.calibrate_hdr();
        let _ = std::fs::remove_file(&launcher);
        let command = launched.borrow()[0].clone();
        assert_eq!(command[command.len() - 2..], ["--output".to_owned(), "DP-1".to_owned()]);
        finish(&host, &page);
    });
    run("displays_failure_uses_the_shared_error_state", || {
        tessel_displays::ipc::set_config_data(json!({"STATE": {"HEADS": []}}));
        let (test_host, host) = host(app);
        let page = gs_desktop::displays::build_displays_page(&host, gs_desktop::displays::DisplaysOptions {
            load_data: Some(false), ..Default::default() });
        assert_eq!(test_host.errors.borrow()[0].2.as_deref(), Some("No displays found"));
        finish(&test_host, &page);
    });
}

// --------------------------------------------------------------------- dock

struct DockFx {
    host: Rc<TestHost>,
    page: SettingsPage,
    dock: Rc<DockPage>,
    store: PathBuf,
    rc: PathBuf,
    reloads: Arc<Mutex<usize>>,
    dir: PathBuf,
}

impl DockFx {
    fn controls(&self) -> tessel_dock::ui::settings::SettingsWindow { self.dock.controller().unwrap() }
    fn saved(&self) -> DockConfig { ConfigStore::new(&self.store).load(None, None) }
    fn save(&self, config: &DockConfig) { ConfigStore::new(&self.store).save(config).unwrap(); }
    fn workspace(&self) -> gs_system::workspace_config::WorkspaceConfig {
        gs_system::workspace_config::WorkspaceConfig::at(&self.rc, self.dir.join("backups"), Arc::new(|| Ok(())))
    }
    fn done(self) { finish(&self.host, &self.page) }
}

fn offline_factory(store: PathBuf) -> dock::BridgeFactory {
    Rc::new(move |application, fields| {
        DockSettingsBridge::new(application, BridgeOptions { fields, bus: None, store: Some(ConfigStore::new(&store)), read: true,
                                                             running: Some(Arc::new(|| false)) }, None)
    })
}

fn dock_fx(app: &gtk::Application, section: &str, tiling: Option<dock::TilingFactory>) -> DockFx {
    let dir = scratch("dock");
    let store = dir.join("dock.json");
    let mut config = config_with(json!({"pins": ["org.example.App.desktop"], "hot_corners": {"offline-output": ["bottom_left"]}}));
    config.pins = vec!["org.example.App.desktop".into()];
    ConfigStore::new(&store).save(&config).unwrap();
    let rc = dir.join("rc.xml");
    std::fs::write(&rc, "<labwc_config><desktops number=\"4\"/></labwc_config>").unwrap();
    let reloads: Arc<Mutex<usize>> = Arc::default();
    let (rc2, backups, counter) = (rc.clone(), dir.join("backups"), reloads.clone());
    let workspace: dock::WorkspaceFactory = Rc::new(move || {
        let counter = counter.clone();
        let backend = gs_system::workspace_config::WorkspaceConfig::at(&rc2, &backups, Arc::new(move || {
            *counter.lock().unwrap() += 1;
            Ok(())
        }));
        Ok(gs_system::workspace_settings::WorkspaceControls::new(Some(Rc::new(backend))) as Rc<dyn WorkspacePart>)
    });
    let tiling = tiling.unwrap_or_else(|| {
        let backend = Arc::new(tiling_backend(&dir.join("rc.xml"), &dir));
        Rc::new(move || Rc::new(tessel_tiling::ui::settings::TilingControls::new(Some(backend.clone()),
            Some(Arc::new(|| serde_json::Map::new())), false, None, tessel_tiling::ui::settings::Runner::Inline)) as Rc<dyn TilingPart>)
    });
    let authentication: dock::AuthenticationFactory = Rc::new(|| dock::AuthenticationStatus::new(None));
    let (test_host, _host) = host(app);
    let options = DockPageOptions { bridge_factory: Some(offline_factory(store.clone())), workspace_factory: Some(workspace),
                                    tiling_factory: Some(tiling), authentication_factory: Some(authentication) };
    let page = dock::create_page(Some(app.clone()), section, options).unwrap();
    show(&test_host, &page);
    let dock = page.controller::<DockPage>().unwrap();
    DockFx { host: test_host, page, dock, store, rc, reloads, dir }
}

fn tiling_backend(rc: &Path, dir: &Path) -> tessel_tiling::config::TilingConfig {
    tessel_tiling::config::TilingConfig::new(Some(rc.to_path_buf()), Some(dir.to_path_buf()), Some(dir.to_path_buf()),
        Some(Arc::new(|_| Ok(serde_json::Map::new()))), Some(Arc::new(|| Ok(()))), None)
}


fn rgba(text: &str) -> gtk::gdk::RGBA { gtk::gdk::RGBA::parse(text).unwrap() }

fn dock_tests(app: &gtk::Application) {
    run("discovery_default_app_visibility_is_configurable", || {
        let fx = dock_fx(app, "dock", None);
        let c = fx.controls();
        assert!(!c.controls().default_app_switches["discovery"].is_active());
        c.controls().default_app_switches["discovery"].set_active(true);
        c.apply();
        assert!(fx.saved().always_visible_apps.iter().any(|a| a == "discovery"));
        fx.done();
    });
    run("bar_border_controls_save_sides_color_and_follow_global_matching", || {
        let fx = dock_fx(app, "dock", None);
        let w = fx.controls();
        let c = w.controls();
        assert!(!c.bar_border_enabled.is_sensitive());
        c.mode.set_active_id(Some("bar"));
        assert!(c.bar_border_enabled.is_sensitive());
        assert!(!c.bar_border_color.button.is_sensitive());
        c.bar_border_enabled.set_active(true);
        c.bar_border_switches["left"].set_active(false);
        c.bar_border_switches["right"].set_active(false);
        c.bar_border_color.button.set_rgba(&rgba("#bbaa77"));
        assert!(c.bar_border_color.button.is_sensitive());
        let bridge = fx.dock.bridge().unwrap();
        bridge.set_online(true);
        w.preview_appearance();
        let values = bridge.preview_values().unwrap();
        assert_eq!(values["bar_border_enabled"], json!(true));
        assert_eq!(values["bar_border_sides"], json!(["top", "bottom"]));
        assert_eq!(values["bar_border_color"], json!("#bbaa77"));
        bridge.restore_appearance();
        bridge.set_online(false);
        fx.save(&replaced(&fx.saved(), json!({"match_wallpaper": true})));
        fx.page.activate();
        settle(0.3);
        assert!(!c.bar_border_color.button.is_sensitive());
        assert!(c.bar_border_switches["top"].is_sensitive());
        w.apply();
        let saved = fx.saved();
        assert!(saved.bar_border_enabled);
        assert_eq!(saved.bar_border_sides, ["top", "bottom"]);
        assert_eq!(saved.bar_border_color, "#bbaa77");
        assert!(saved.match_wallpaper);
        fx.save(&replaced(&saved, json!({"match_wallpaper": false})));
        fx.page.activate();
        settle(0.3);
        assert!(c.bar_border_color.button.is_sensitive());
        c.mode.set_active_id(Some("dock"));
        w.apply();
        assert!(fx.saved().bar_border_enabled, "Switching modes retains border preferences");
        fx.done();
    });
    run("dock_page_embeds_controls_and_saves_only_its_fields", || {
        let fx = dock_fx(app, "dock", None);
        let w = fx.controls();
        let c = w.controls();
        assert!(!w.window().is_visible());
        assert!(c.section_cards["placement"].is_visible());
        assert!(c.section_cards["group_layout"].is_visible());
        assert!(c.section_cards["app_dock"].is_visible());
        assert!(!c.section_cards["hot_corners"].is_visible());
        assert!(c.match_wallpaper.parent().is_none());
        assert!(!fx.dock.bridge().unwrap().fields().contains("match_wallpaper"));
        c.transparency.set_value(44.0);
        c.transparency_enabled.set_active(false);
        c.frosted.set_active(true);
        c.frost_strength.set_value(75.0);
        c.auto_hide.set_active(true);
        c.application_display.set_active_id(Some("title"));
        c.separate_app_dock.set_active(true);
        c.app_dock_edge.set_active_id(Some("right"));
        c.app_dock_controls["app_dock_transparency"].set_value(65.0);
        c.app_dock_controls["app_dock_frosted"].set_active(true);
        c.app_dock_controls["app_dock_frost_strength"].set_value(30.0);
        c.app_dock_layout_controls.move_group("pinned", "center");
        c.app_dock_layout_controls.move_group("running", "center");
        c.app_dock_layout_controls.reorder("running", -1);
        c.app_dock_background_color.button.set_rgba(&rgba("#445566"));
        c.background_color.button.set_rgba(&rgba("#102030"));
        w.add_tray_choice("id:Nextcloud");
        c.default_app_switches["network"].set_active(true);
        c.layout_controls.move_group("clock", "end");
        c.menu_hotkey_fullscreen.set_active(true); // Hidden page must not save this.
        w.apply();
        let saved = fx.saved();
        let v = saved.to_value();
        assert_eq!((saved.transparency, saved.transparency_enabled, saved.frosted, saved.frost_strength), (44, false, true, 75));
        assert_eq!((saved.apps_frost_strength, saved.apps_transparency_enabled, saved.apps_transparency), (50, false, 5));
        assert!(saved.auto_hide);
        assert_eq!(saved.application_display, "title");
        assert!(saved.separate_app_dock);
        assert_eq!(saved.app_dock_edge, "right");
        assert_eq!((saved.app_dock_transparency, saved.app_dock_frosted, saved.app_dock_frost_strength), (65, true, 30));
        assert_eq!(v["app_dock_group_layout"], json!({"start": ["app_menu"], "center": ["running", "pinned"], "end": ["files"]}));
        assert_eq!(saved.background_color.as_deref(), Some("#102030"));
        assert_eq!(saved.app_dock_background_color, "#445566");
        assert_eq!(saved.always_show_tray, ["id:Nextcloud"]);
        assert!(saved.always_visible_apps.iter().any(|a| a == "network"));
        assert_eq!(v["group_layouts"]["dock"]["end"], json!(["clock"]));
        assert!(!saved.menu_hotkey_fullscreen);
        assert_eq!(saved.pins, ["org.example.App.desktop"]);
        assert!(fx.page.widget.is_visible());
        fx.done();
    });
    run("returning_to_dock_page_syncs_shared_transparency_preserving_other_edits", || {
        let fx = dock_fx(app, "dock", None);
        let w = fx.controls();
        let c = w.controls();
        c.transparency.set_value(46.0);
        c.frost_strength.set_value(80.0);
        c.auto_hide.set_active(true);
        fx.page.deactivate();
        fx.save(&replaced(&fx.saved(), json!({"transparency": 68, "transparency_enabled": false, "apps_transparency_enabled": true,
            "apps_transparency": 48, "frosted": true, "frost_strength": 60, "apps_frost_strength": 35, "icon_size": 64})));
        fx.page.activate();
        settle(0.3);
        assert_eq!(c.transparency.value(), 68.0);
        assert!(!c.transparency_enabled.is_active());
        assert!(!c.transparency.is_sensitive());
        assert!(c.frosted.is_active());
        assert_eq!(c.frost_strength.value(), 60.0);
        assert!(c.frost_strength.is_sensitive());
        assert!(c.auto_hide.is_active());
        w.apply();
        let saved = fx.saved();
        assert_eq!((saved.transparency, saved.transparency_enabled, saved.apps_transparency, saved.apps_transparency_enabled),
                   (68, false, 48, true));
        assert_eq!((saved.frost_strength, saved.apps_frost_strength, saved.icon_size, saved.auto_hide), (60, 35, 64, true));
        fx.done();
    });
    run("preview_supports_previous_arguments_and_optional_transparency_toggle", || {
        let fx = dock_fx(app, "dock", None);
        let bridge = fx.dock.bridge().unwrap();
        bridge.set_online(true);
        bridge.preview_appearance(bridge::PreviewArgs { transparency: 30, match_wallpaper: false, ..Default::default() });
        let values = bridge.preview_values().unwrap();
        assert_eq!(values["transparency"], json!(30));
        assert!(!values.contains_key("transparency_enabled") && !values.contains_key("frost_strength"));
        bridge.preview_appearance(bridge::PreviewArgs { transparency: 55, match_wallpaper: true, transparency_enabled: Some(false),
                                                        frosted: Some(true), frost_strength: Some(72), ..Default::default() });
        let values = bridge.preview_values().unwrap();
        assert_eq!((values["transparency_enabled"].clone(), values["transparency"].clone(), values["frosted"].clone(),
                    values["frost_strength"].clone()), (json!(false), json!(55), json!(true), json!(72)));
        assert_eq!(values.keys().collect::<Vec<_>>(), ["transparency", "match_wallpaper", "rounded_corners", "float_mode",
                                                        "transparency_enabled", "frosted", "frost_strength"]);
        bridge.restore_appearance();
        bridge.set_online(false);
        fx.done();
    });
    run("theme_and_dock_pages_share_persistent_values_without_cross_saving", || {
        let fx = dock_fx(app, "dock", None);
        let w = fx.controls();
        let c = w.controls();
        c.auto_hide.set_active(true);
        fx.page.deactivate();
        let factory = offline_factory(fx.store.clone());
        let bridge = factory(Some(app.clone()), bridge::appearance_fields()).unwrap();
        let (_unused, host) = host(app);
        let theme = ThemeWallpaperPage::new(&host, ThemeOptions { bridge: Some(Rc::new(bridge)),
            config_path: Some(fx.dir.join("wallpapers.json")), color_settings: Some(None), ..Default::default() }).unwrap();
        theme.dock_transparency.set_value(67.0);
        theme.toggle("frosted").set_active(true);
        theme.strength("frost_strength").set_value(75.0);
        theme.apps_transparency_enabled.set_active(true);
        theme.apps_transparency.set_value(37.0);
        theme.toggle("apps_frosted").set_active(true);
        theme.strength("apps_frost_strength").set_value(28.0);
        theme.deactivate();
        assert!(wait(|| fx.saved().apps_frost_strength == 28));
        settle(0.3);
        let saved = fx.saved();
        assert_eq!((saved.transparency, saved.apps_transparency, saved.frost_strength, saved.apps_frost_strength), (67, 37, 75, 28));
        assert!(saved.apps_transparency_enabled);
        assert!(!saved.auto_hide); // The other page still has an unsaved edit.
        assert_eq!(saved.pins, ["org.example.App.desktop"]);
        fx.page.activate();
        settle(0.3);
        assert_eq!(c.transparency.value(), 67.0);
        assert_eq!(c.frost_strength.value(), 75.0);
        assert!(c.frost_strength.is_sensitive());
        assert!(c.auto_hide.is_active());
        c.transparency_enabled.set_active(false);
        w.apply();
        theme.activate();
        settle(0.3);
        assert!(!theme.dock_transparency_enabled.is_active());
        assert_eq!(theme.dock_transparency.value(), 67.0);
        assert_eq!(theme.apps_transparency.value(), 37.0);
        assert!(theme.apps_transparency_enabled.is_active());
        assert_eq!(theme.strength("frost_strength").value(), 75.0);
        assert!(theme.strength("frost_strength").is_sensitive());
        assert_eq!(theme.strength("apps_frost_strength").value(), 28.0);
        assert!(fx.saved().auto_hide);
        theme.close();
        fx.done();
    });
    run("returning_to_dock_page_refreshes_global_color_without_losing_edits", || {
        let fx = dock_fx(app, "dock", None);
        let w = fx.controls();
        let c = w.controls();
        c.transparency.set_value(46.0);
        c.frost_strength.set_value(62.0);
        c.auto_hide.set_active(true);
        fx.page.deactivate();
        fx.save(&replaced(&fx.saved(), json!({"match_wallpaper": true, "icon_size": 64})));
        fx.page.activate();
        settle(0.3);
        assert!(c.match_wallpaper.is_active());
        assert_eq!(c.transparency.value(), 46.0);
        assert_eq!(c.frost_strength.value(), 62.0);
        assert!(c.auto_hide.is_active());
        w.apply();
        let saved = fx.saved();
        assert_eq!((saved.transparency, saved.frost_strength, saved.icon_size, saved.match_wallpaper, saved.auto_hide),
                   (46, 62, 64, true, true));
        fx.done();
    });
    run("behavior_page_retains_disconnected_monitor_and_other_settings", || {
        let fx = dock_fx(app, "desktop", None);
        let w = fx.controls();
        let c = w.controls();
        assert!(!c.section_cards["appearance"].is_visible());
        assert!(c.section_cards["shortcuts"].is_visible());
        assert!(c.section_cards["hot_corners"].is_visible());
        c.menu_hotkey_fullscreen.set_active(true);
        c.hot_corner_delay.set_value(300.0);
        let output = connectors()[0].clone();
        c.hot_corner_switches[&output]["top_left"].set_active(true);
        c.transparency.set_value(80.0); // Hidden field remains unchanged.
        let extra = fx.dock.extra_controls().unwrap();
        assert_eq!(extra.count.value_as_int(), 4);
        assert!(extra.workspace.limit().text().contains("1–100"));
        extra.count.set_value(6.0);
        w.apply();
        assert_eq!(gs_system::workspace_config::WorkspaceConfig::load(&fx.workspace()).unwrap().count, 6);
        assert_eq!(*fx.reloads.lock().unwrap(), 1);
        let saved = fx.saved();
        assert!(saved.menu_hotkey_fullscreen);
        assert_eq!(saved.hot_corner_delay_ms, 300);
        assert_eq!(saved.hot_corners.get("offline-output").cloned(), Some(vec!["bottom_left".to_owned()]));
        assert_eq!(saved.hot_corners.get(&output).cloned(), Some(vec!["top_left".to_owned()]));
        assert_eq!(saved.transparency, 5);
        for name in ["workspace_count", "gapless_switch", "tiling_switch", "focus_outline_switch", "authentication_status",
                     "hot_corner_delay", "menu_hotkey_fullscreen"] {
            assert!(fx.page.search_targets.borrow().contains_key(name), "{name}");
        }
        fx.done();
    });
    run("gapless_shared_control_applies_refreshes_and_reports_failures", || {
        let dir = scratch("gapless");
        let rc = dir.join("rc.xml");
        let backend = Arc::new(tiling_backend(&rc, &dir));
        let shared = backend.clone();
        let tiling: dock::TilingFactory = Rc::new(move || Rc::new(tessel_tiling::ui::settings::GaplessControls::new(shared.clone(),
            tessel_tiling::ui::settings::Runner::Thread)) as Rc<dyn TilingPart>);
        let fx = dock_fx(app, "desktop", Some(tiling));
        std::fs::write(&rc, "<labwc_config><core gap=\"14\"/><desktops number=\"4\"/></labwc_config>").unwrap();
        let extra = fx.dock.extra_controls().unwrap();
        let switch = extra.tiling.switch();
        switch.set_active(true);
        assert!(wait(|| backend.load_preferences().is_ok_and(|p| p.get("gapless") == Some(&json!(true)))));
        assert_eq!(tessel_tiling::gaps::with_gap(&std::fs::read(&rc).unwrap(), 0).unwrap().1, 0);
        // The standalone popup uses this same control/backend and sees the save.
        let popup = tessel_tiling::ui::settings::GaplessControls::new(backend.clone(), tessel_tiling::ui::settings::Runner::Thread);
        assert!(wait(|| popup.switch().is_active()));
        popup.switch().set_active(false);
        assert!(wait(|| backend.load_preferences().is_ok_and(|p| p.get("gapless") == Some(&json!(false)))));
        fx.page.activate();
        assert!(wait(|| !switch.is_active()));
        assert_eq!(tessel_tiling::gaps::with_gap(&std::fs::read(&rc).unwrap(), 0).unwrap().1, 14);
        fx.done();
    });
    run("workspace_reset_and_conflict_do_not_overwrite_external_changes", || {
        let fx = dock_fx(app, "desktop", None);
        let extra = fx.dock.extra_controls().unwrap();
        extra.count.set_value(7.0);
        let config = fx.workspace();
        config.save(3, &config.load().unwrap()).unwrap();
        fx.controls().apply();
        assert!(extra.workspace.status().text().contains("changed elsewhere"));
        assert_eq!(config.load().unwrap().count, 3);
        let reset = buttons(&fx.page.widget).into_iter().find(|b| b.label().as_deref() == Some("Reset")).unwrap();
        reset.emit_clicked();
        settle(0.1);
        assert_eq!(fx.dock.extra_controls().unwrap().count.value_as_int(), 3);
        fx.done();
    });
    run("concurrent_pin_change_survives_and_conflicting_page_edit_is_rejected", || {
        let fx = dock_fx(app, "dock", None);
        fx.save(&replaced(&fx.saved(), json!({"pins": ["org.example.New.desktop"], "transparency": 33})));
        let w = fx.controls();
        w.controls().transparency.set_value(60.0);
        w.apply();
        assert_eq!(fx.saved().transparency, 33);
        assert!(w.controls().result_label.text().contains("changed elsewhere"));
        assert_eq!(fx.saved().pins, ["org.example.New.desktop"]);
        let reset = buttons(&fx.page.widget).into_iter().find(|b| b.label().as_deref() == Some("Reset")).unwrap();
        reset.emit_clicked();
        settle(0.1);
        let fresh = fx.controls();
        assert_eq!(fresh.controls().transparency.value(), 33.0);
        fresh.controls().transparency.set_value(60.0);
        fresh.apply();
        assert_eq!(fx.saved().transparency, 60);
        assert_eq!(fx.saved().pins, ["org.example.New.desktop"]);
        fx.page.close();
        settle(0.1);
        assert!(!app.windows().iter().any(|w| w.has_css_class("tessel-dock-settings")));
        fx.done();
    });
    run("dock_search_targets_use_settings_search_names", || {
        let fx = dock_fx(app, "dock", None);
        for name in ["mode", "monitor", "size", "float_mode", "rounded_corners", "auto_hide", "fullscreen", "application_display",
                     "separate_app_dock", "background_color", "bar_border_enabled", "app_dock_layout_controls", "group_media"] {
            assert!(fx.page.search_targets.borrow().contains_key(name), "{name}");
        }
        // Searching "Media" spotlights the panel's Media switch, which starts off.
        let media = fx.page.search_targets.borrow()["group_media"].clone();
        let media = media.downcast::<gtk::Switch>().expect("the Media switch");
        assert!(!media.is_active());
        fx.done();
    });
}

// ------------------------------------------------- online bridge (fake dock)

struct FakeDockHost {
    store: RefCell<ConfigStore>,
    previews: Arc<Mutex<Vec<Value>>>,
}

impl tessel_dock::dbus::settings_api::SettingsHost for FakeDockHost {
    fn config(&self) -> DockConfig { self.store.borrow_mut().load(None, None) }
    fn save_config(&self, config: DockConfig, _rebuild: bool) -> bool { self.store.borrow_mut().save(&config).is_ok() }
    fn preview_appearance(&self, values: &Map<String, Value>) { self.previews.lock().unwrap().push(Value::Object(values.clone())) }
    fn restore_appearance(&self) { self.previews.lock().unwrap().push(json!("restore")) }
    fn tray_apps(&self) -> Value { json!([{"id": "id:steam", "label": "Steam", "running": true}]) }
    fn monitors(&self) -> Vec<tessel_dock::dbus::settings_api::MonitorInfo> {
        vec![tessel_dock::dbus::settings_api::MonitorInfo { name: "HEADLESS-1".into(), label: "HEADLESS-1 · Fake".into(),
                                                         geometry: Default::default() }]
    }
    fn docks(&self) -> Vec<tessel_dock::dbus::settings_api::DockInfo> { Vec::new() }
}

/// The real Rust `org.tessel.Settings` API over a fake host, on its own
/// private-bus connection and main context (so blocking client calls from
/// the GTK thread are answered).
fn start_fake_dock(store: PathBuf, previews: Arc<Mutex<Vec<Value>>>) -> Arc<std::sync::atomic::AtomicBool> {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
            assert!(!address.contains("/run/user/1000/bus"), "refusing the real session bus");
            let connection = gio::DBusConnection::for_address_sync(&address,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None, gio::Cancellable::NONE).unwrap();
            let host = Rc::new(FakeDockHost { store: RefCell::new(ConfigStore::new(&store)), previews });
            let _api = tessel_dock::dbus::settings_api::SettingsApi::new(&connection, host).unwrap();
            let reply = connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus",
                "RequestName", Some(&(tessel_dock::dbus::settings_api::BUS_NAME, 4u32).to_variant()), None,
                gio::DBusCallFlags::NONE, 2000, gio::Cancellable::NONE).unwrap();
            assert_eq!(reply.child_value(0).get::<u32>(), Some(1));
            ready_tx.send(()).unwrap();
            while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                context.iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
            let _ = connection.close_sync(gio::Cancellable::NONE);
        }).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).expect("fake tessel-dock");
    stop
}

fn online_tests(app: &gtk::Application) {
    run("online_bridge_reads_applies_previews_and_restores_through_dbus", || {
        let dir = scratch("online");
        let store = dir.join("tessel-dock/config.json");
        ConfigStore::new(&store).save(&config_with(json!({"pins": ["a.desktop"], "transparency": 20}))).unwrap();
        let previews: Arc<Mutex<Vec<Value>>> = Arc::default();
        let stop = start_fake_dock(store.clone(), previews.clone());
        let bridge = DockSettingsBridge::new(Some(app.clone()), BridgeOptions { fields: bridge::appearance_fields(), ..Default::default() },
                                             None).unwrap();
        assert!(bridge.online());
        assert_eq!(bridge.config().transparency, 20);
        assert_eq!(bridge.monitors(), [("HEADLESS-1".to_owned(), "HEADLESS-1 · Fake".to_owned())]);
        assert_eq!(bridge.tray_apps()[0].id, "id:steam");
        bridge.preview_appearance(bridge::PreviewArgs { transparency: 40, match_wallpaper: true, ..Default::default() });
        assert!(wait(|| !previews.lock().unwrap().is_empty()));
        assert_eq!(previews.lock().unwrap()[0]["transparency"], json!(40));
        let mut desired = bridge.config();
        desired.transparency = 61;
        desired.auto_hide = true; // not an appearance field
        assert!(bridge.save_config(&desired, true));
        let saved = ConfigStore::new(&store).load(None, None);
        assert_eq!((saved.transparency, saved.auto_hide, saved.pins.clone()), (61, false, vec!["a.desktop".to_owned()]));
        assert!(wait(|| previews.lock().unwrap().iter().any(|v| v == "restore")));
        // A concurrent change of the same field is rejected by the dock.
        ConfigStore::new(&store).save(&replaced(&saved, json!({"transparency": 10}))).unwrap();
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = errors.clone();
        bridge.set_on_error(Rc::new(move |message| sink.borrow_mut().push(message.to_owned())));
        desired.transparency = 70;
        assert!(!bridge.save_config(&desired, true));
        assert!(errors.borrow()[0].contains("Dock settings changed elsewhere. Reset this page before applying again."), "{:?}", errors.borrow());
        let done = Rc::new(Cell::new(None));
        let flag = done.clone();
        bridge.refresh_async(Some(Box::new(move |ok| flag.set(Some(ok)))), None);
        assert!(wait(|| done.get().is_some()));
        assert_eq!(bridge.config().transparency, 10);
        let refreshed = Rc::new(Cell::new(false));
        let flag = refreshed.clone();
        bridge.refresh_tray_entries(Box::new(move || flag.set(true)));
        assert!(wait(|| refreshed.get()));
        bridge.close();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        settle(0.1);
    });
}

// ------------------------------------------------------------------- layout

struct LBridge {
    config: Mutex<DockConfig>,
    fail: Mutex<bool>,
    error: Mutex<String>,
}

impl PresetBridge for LBridge {
    fn config(&self) -> DockConfig { self.config.lock().unwrap().clone() }
    fn refresh(&self) -> bool { true }
    fn save_config(&self, config: &DockConfig) -> bool {
        if *self.fail.lock().unwrap() {
            *self.error.lock().unwrap() = "dock offline".into();
            return false;
        }
        *self.config.lock().unwrap() = config.clone();
        true
    }
    fn error(&self) -> String { self.error.lock().unwrap().clone() }
    fn close(&self) {}
}

struct LWorkspaces(Mutex<i64>);
impl WorkspaceBackend for LWorkspaces {
    fn load(&self) -> Result<WorkspaceState, String> {
        let count = *self.0.lock().unwrap();
        Ok(WorkspaceState { count, signature: count.to_string() })
    }
    fn save(&self, count: i64, _expected: &WorkspaceState) -> Result<WorkspaceState, String> {
        *self.0.lock().unwrap() = count;
        self.load()
    }
}

struct LTiling(Mutex<bool>);
impl TilingBackend for LTiling {
    fn load_preferences(&self) -> Result<Map<String, Value>, String> {
        Ok(json!({"gapless": *self.0.lock().unwrap()}).as_object().unwrap().clone())
    }
    fn set_gapless(&self, enabled: bool) -> Result<(), String> {
        *self.0.lock().unwrap() = enabled;
        Ok(())
    }
}

fn layout_tests(app: &gtk::Application) {
    let setup = |app: &gtk::Application| {
        let icon = PresetIcon::Embedded(presets::BUILTIN[3].2);
        let win = Preset::new("win", "Win", icon.clone(), json!({"edge": "bottom", "alignment": "center"}).as_object().unwrap().clone(), Some(4), Some(true));
        let side = Preset::new("side", "Side", icon, json!({"edge": "left", "alignment": "start"}).as_object().unwrap().clone(), Some(2), Some(false));
        let bridge = Arc::new(LBridge { config: Mutex::new(config_with(json!({"edge": "bottom", "alignment": "center"}))),
                                        fail: Mutex::new(false), error: Mutex::new(String::new()) });
        let workspaces = Arc::new(LWorkspaces(Mutex::new(4)));
        let tiling = Arc::new(LTiling(Mutex::new(true)));
        let prompts: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
        let answer = Rc::new(Cell::new(1));
        let (record, reply) = (prompts.clone(), answer.clone());
        gs_base::style::set_chooser(Some(Rc::new(move |message, detail, _buttons, callback, _default, _cancel| {
            record.borrow_mut().push((message.to_owned(), detail.to_owned()));
            callback(Some(reply.get()));
        })));
        let previous = scratch("layout").join("previous-layout.json");
        let page = LayoutSettings::new(Some(app.clone()), LayoutOptions {
            bridge: Some(bridge.clone()), workspace_backend: Some(Some(workspaces.clone())), tiling_backend: Some(Some(tiling.clone())),
            clock_store: Some(None), presets: Some((vec![side, win], Vec::new())), previous_path: Some(previous.clone()),
            runner: gs_desktop::util::Runner::Thread,
        }).unwrap();
        let window = gtk::Window::builder().child(&page.widget).default_width(900).default_height(600).build();
        window.present();
        settle(0.3);
        (page, window, bridge, workspaces, tiling, prompts, answer, previous)
    };
    run("current_layout_is_marked_selected_and_cannot_be_reapplied", || {
        let (page, window, ..) = setup(app);
        assert!(page.card("win").unwrap().current.is_visible());
        assert!(!page.card("side").unwrap().current.is_visible());
        assert_eq!(page.selected().unwrap().key, "win");
        assert!(!page.apply_button.is_sensitive());
        page.close();
        window.destroy();
    });
    run("choosing_and_applying_another_layout", || {
        let (page, window, bridge, workspaces, tiling, prompts, answer, previous) = setup(app);
        page.flow.select_child(&page.card("side").unwrap().child);
        settle(0.05);
        assert!(page.apply_button.is_sensitive());
        answer.set(0); // Cancel changes nothing
        page.apply_button.emit_clicked();
        settle(0.3);
        assert_eq!(bridge.config().edge, "bottom");
        assert!(!previous.exists());
        answer.set(1);
        page.apply_button.emit_clicked();
        assert!(wait(|| !page.busy.get() && page.status.text().contains("Side")));
        settle(0.2);
        let (message, detail) = prompts.borrow().last().unwrap().clone();
        assert!(message.contains("Side"));
        assert!(detail.contains("Workspaces: 4 → 2"));
        assert!(detail.contains("Gapless tiling: off"));
        assert_eq!((bridge.config().edge.as_str(), *workspaces.0.lock().unwrap(), *tiling.0.lock().unwrap()), ("left", 2, false));
        assert!(page.card("side").unwrap().current.is_visible());
        assert!(!page.card("win").unwrap().current.is_visible());
        assert!(!page.apply_button.is_sensitive());
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&previous).unwrap()).unwrap();
        assert_eq!(saved["workspaces"], json!(4));
        assert!(page.restore_button.is_sensitive());
        assert!(page.toast.widget().reveals_child());
        page.toast.action.emit_clicked();
        assert!(wait(|| page.status.text().contains("restored")));
        settle(0.2);
        assert_eq!((bridge.config().edge.as_str(), *workspaces.0.lock().unwrap(), *tiling.0.lock().unwrap()), ("bottom", 4, true));
        assert!(page.card("win").unwrap().current.is_visible());
        page.close();
        window.destroy();
    });
    run("layout_failure_is_shown", || {
        let (page, window, bridge, ..) = setup(app);
        *bridge.fail.lock().unwrap() = true;
        page.flow.select_child(&page.card("side").unwrap().child);
        page.apply_button.emit_clicked();
        assert!(wait(|| page.status.text().contains("dock offline")));
        assert!(page.status.has_css_class("error"));
        page.close();
        window.destroy();
    });
}

// ------------------------------------------------------------- lock & login

struct FakePower {
    ready: Cell<bool>,
    state: RefCell<Map<String, Value>>,
    calls: RefCell<Vec<Map<String, Value>>>,
    changed: Rc<dyn Fn()>,
    closed: Cell<bool>,
}

impl gs_desktop::lock::power::LockPower for FakePower {
    fn ready(&self) -> bool { self.ready.get() }
    fn state(&self) -> Map<String, Value> { self.state.borrow().clone() }
    fn error(&self) -> String { String::new() }
    fn start(&self) { (self.changed)() }
    fn close(&self) { self.closed.set(true) }
    fn set_settings(&self, settings: &Map<String, Value>, done: gs_desktop::lock::power::Done) {
        self.calls.borrow_mut().push(settings.clone());
        self.state.borrow_mut().insert("settings".into(), Value::Object(settings.clone()));
        done(true, String::new());
    }
}

fn lock_tests(app: &gtk::Application) {
    run("lock_settings_save_appearance_and_shared_lock_delay", || {
        let dir = scratch("lock");
        let power_slot: Rc<RefCell<Option<Rc<FakePower>>>> = Rc::default();
        let slot = power_slot.clone();
        let previews = Rc::new(Cell::new(0));
        let count = previews.clone();
        let (test_host, host) = host(app);
        let page = gs_desktop::lock::build_lock_page(&host, gs_desktop::lock::LockOptions {
            store_path: Some(dir.join("lock-screen.json")),
            power_factory: Some(Rc::new(move |changed| {
                let power = Rc::new(FakePower { ready: Cell::new(true), state: RefCell::new(json!({"settings": {"lock_after": 300, "dim_after": 120}}).as_object().unwrap().clone()),
                                                calls: RefCell::default(), changed, closed: Cell::new(false) });
                slot.replace(Some(power.clone()));
                power
            })),
            assistant_detector: Some(Rc::new(|| false)),
            preview: Some(Rc::new(move || { count.set(count.get() + 1); Ok(()) })),
            weather_location: Some(Rc::new(|| Some("Berlin".into()))),
        }).unwrap();
        show(&test_host, &page);
        let c = page.controller::<gs_desktop::lock::LockSettings>().unwrap();
        assert!(!c.assistant.is_visible());
        assert!(c.weather_options.is_visible());
        assert_eq!(c.weather_location_label.text(), "Uses your Weather settings: Berlin");
        assert_eq!(c.delay.value_as_int(), 300);
        assert_eq!(c.power_status.text(), "Shared with Power settings.");
        assert!(!c.choose.is_visible());
        c.mode.set_selected(1);
        assert!(c.choose.is_visible());
        c.save.emit_clicked();
        assert_eq!(c.status.text(), "Choose a picture first.");
        assert!(!dir.join("lock-screen.json").exists());
        c.mode.set_selected(3);
        assert!(!c.placement.is_sensitive());
        c.color.set_text("#123456");
        c.blur.set_active(false);
        c.delay.set_value(600.0);
        c.preview.emit_clicked();
        assert_eq!(c.status.text(), "Lock screen settings saved.");
        assert_eq!(previews.get(), 1);
        let power = power_slot.borrow().clone().unwrap();
        assert_eq!(power.calls.borrow()[0]["lock_after"], json!(600));
        assert_eq!(power.calls.borrow()[0]["dim_after"], json!(120));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("lock-screen.json")).unwrap()).unwrap();
        assert_eq!((saved["wallpaper_mode"].clone(), saved["background_color"].clone(), saved["blur"].clone()),
                   (json!("color"), json!("#123456"), json!(false)));
        c.color.set_text("bad");
        c.save.emit_clicked();
        assert_eq!(c.status.text(), "Background color must be a six-digit hex color, such as #161820.");
        for key in ["wallpaper", "blur", "transparency", "automatic", "weather", "forecast", "pause_media", "media", "battery",
                    "notifications", "notification_names"] {
            assert!(page.search_targets.borrow().contains_key(key), "{key}");
        }
        page.close();
        assert!(power.closed.get());
        finish(&test_host, &page);
    });
    run("login_settings_send_the_validated_request_to_the_helper", || {
        let dir = scratch("login");
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("labwc.desktop"), "[Desktop Entry]\nName=labwc\nExec=sh\n").unwrap();
        std::fs::write(sessions.join("other.desktop"), "[Desktop Entry]\nName=Other\nExec=true\n").unwrap();
        let fake = dir.join("pkexec");
        let request = dir.join("request.json");
        std::fs::write(&fake, format!("#!/bin/sh\necho \"$*\" > '{}/args'\ncat > '{}'\n[ -n \"$FAIL\" ] && {{ echo 'Not authorized' >&2; exit 1; }}\nexit 0\n",
                                      dir.display(), request.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut helper: Vec<String> = gs_desktop::login::HELPER_COMMAND.iter().map(|s| s.to_string()).collect();
        helper[0] = fake.to_string_lossy().into_owned();
        let (test_host, host) = host(app);
        let page = gs_desktop::login::build_login_page(&host, gs_desktop::login::LoginOptions {
            config_path: Some(dir.join("missing-appearance.json")), session_roots: Some(vec![sessions.clone()]),
            helper_command: Some(helper),
        }).unwrap();
        show(&test_host, &page);
        let c = page.controller::<gs_desktop::login::LoginSettings>().unwrap();
        assert_eq!(c.sessions.iter().map(|s| s.ident.as_str()).collect::<Vec<_>>(), ["labwc", "other"]);
        c.picture.set_text("  /usr/share/wallpapers/x.png ");
        c.clock.set_active(true);
        c.scales[0].1.set_value(70.4);
        c.session.set_selected(1);
        c.apply.emit_clicked();
        assert!(wait(|| !c.busy.get() && !c.status.text().is_empty()));
        assert_eq!(c.status.text(), "Login screen settings saved.");
        assert_eq!(std::fs::read_to_string(&request).unwrap(),
            "{\"expected\": {\"picture\": \"/usr/share/wallpapers/tessel/slate-3840x2160.png\", \"placement\": \"fill\", \"background_color\": \"#161820\", \"blur_strength\": 50, \"transparency\": 5, \"idle_seconds\": 30, \"clock_24h\": false, \"default_session\": \"labwc\"}, \"appearance\": {\"picture\": \"/usr/share/wallpapers/x.png\", \"placement\": \"fill\", \"background_color\": \"#161820\", \"blur_strength\": 70, \"transparency\": 5, \"idle_seconds\": 30, \"clock_24h\": true, \"default_session\": \"other\"}}");
        assert_eq!(std::fs::read_to_string(dir.join("args")).unwrap().trim(), "/usr/local/lib/tessel/tessel-login-helper");
        c.color.set_text("nope");
        c.apply.emit_clicked();
        assert_eq!(c.status.text(), "Invalid background color");
        for key in ["picture", "blur_strength", "idle_seconds", "default_session"] {
            assert!(page.search_targets.borrow().contains_key(key), "{key}");
        }
        finish(&test_host, &page);
    });
}

fn fake_tool(name: &str, body: &str) -> PathBuf {
    let bin = PathBuf::from(std::env::var("GS_FAKE_BIN").expect("GS_FAKE_BIN"));
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn extra_tests(app: &gtk::Application) {
    run("lock_preview_starts_lock_screen_preview_through_systemd_run", || {
        let log = scratch("preview").join("calls");
        fake_tool("systemd-run", &format!("printf '%s\\n' \"$*\" >> '{}'", log.display()));
        let dir = scratch("lock-preview");
        let (test_host, host) = host(app);
        let settings = gs_desktop::lock::LockSettings::new(&host, gs_desktop::lock::LockOptions {
            store_path: Some(dir.join("lock.json")),
            power_factory: Some(Rc::new(|changed| Rc::new(FakePower { ready: Cell::new(false), state: RefCell::default(),
                calls: RefCell::default(), changed, closed: Cell::new(false) }) as Rc<dyn gs_desktop::lock::power::LockPower>)),
            assistant_detector: Some(Rc::new(|| false)), weather_location: Some(Rc::new(|| None)), ..Default::default() }).unwrap();
        assert_eq!(settings.power_status.text(), "Power management is unavailable in this session.");
        assert!(!settings.weather_options.is_visible());
        // Only the fakes count as installed, so tessel-lock is looked up on PATH.
        std::env::set_var("TESSEL_PROGRAM_DIR", std::env::var("GS_FAKE_BIN").expect("GS_FAKE_BIN"));
        settings.preview.emit_clicked();
        let started = wait(|| std::fs::read_to_string(&log).is_ok_and(|t| !t.is_empty()));
        std::env::remove_var("TESSEL_PROGRAM_DIR");
        assert!(started);
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(),
                   "--user --collect --quiet --property=Type=exec tessel-lock --preview");
        settings.close();
        test_host.window.destroy();
    });
    run("authentication_status_reads_the_polkitman_unit_state", || {
        let status = dock::AuthenticationStatus::new(Some(vec!["sh".into(), "-c".into(), "echo active".into()]));
        assert!(wait(|| status.status.text() == "Status: running"));
        let status = dock::AuthenticationStatus::new(Some(vec!["sh".into(), "-c".into(), "echo inactive".into()]));
        assert!(wait(|| status.status.text() == "Status: not running. Sign out and back in to start it."));
        let status = dock::AuthenticationStatus::new(Some(vec!["/nonexistent/systemctl".into()]));
        assert_eq!(status.status.text(), "Status: unknown");
        assert_eq!(dock::AUTHENTICATION_COMMAND, ["systemctl", "--user", "is-active", "tessel-auth.service"]);
    });
    run("interface_settings_drive_the_style_selector", || {
        let settings = gs_desktop::interface_settings().expect("org.gnome.desktop.interface color-scheme");
        settings.set_string("color-scheme", "default").unwrap();
        let (test_host, host) = host(app);
        let page = ThemeWallpaperPage::new(&host, ThemeOptions { bridge: Some(FakeBridge::new()), ..Default::default() }).unwrap();
        assert!(page.scheme_buttons["default"].is_active());
        page.scheme_buttons["prefer-dark"].set_active(true);
        assert_eq!(settings.string("color-scheme"), "prefer-dark");
        settings.set_string("color-scheme", "prefer-light").unwrap();
        settle(0.05);
        assert!(page.scheme_buttons["prefer-light"].is_active());
        page.close();
        test_host.window.destroy();
    });
}

// --------------------------------------------------------------- build_page

fn build_page_tests(app: &gtk::Application) {
    run("every_section_builds_in_the_private_session", || {
        // Fake services inside the private runtime dir only.
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
        assert!(!runtime.starts_with("/run/user/1000/") || runtime.contains("/cbt/"), "private runtime only");
        let (test_host, host) = host(app);
        for section in gs_desktop::SECTIONS {
            match gs_desktop::build_page(section, &host) {
                Ok(page) => {
                    show(&test_host, &page);
                    page.activate();
                    settle(0.2);
                    page.deactivate();
                    assert!(page.can_close() || *section == "theme", "{section}");
                    page.close();
                }
                Err(error) => panic!("{section}: {error}"),
            }
        }
        assert!(gs_desktop::build_page("nope", &host).is_err());
        test_host.window.destroy();
    });
}

// -------------------------------------------------------------------- helpers









fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if std::env::var_os("GS_HEADLESS").is_none() || runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none() {
        println!("desktop_headless: skipped (run it through components/settings/tests/headless.sh)");
        return;
    }
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    assert!(!bus.is_empty() && !bus.contains("/run/user/1000/bus"), "private bus only");
    tessel_core::i18n::initialize();
    gtk::init().expect("display");
    let app = gtk::Application::new(Some("org.tessel.GlobalSettings.DesktopTests"), gio::ApplicationFlags::NON_UNIQUE);
    app.register(gio::Cancellable::NONE).unwrap();
    let only = std::env::args().nth(1);
    let groups: Vec<(&str, fn(&gtk::Application))> = vec![("theme", theme_tests), ("icons", icon_tests), ("displays", display_tests), ("dock", dock_tests),
        ("online", online_tests), ("layout", layout_tests), ("lock", lock_tests), ("build", build_page_tests), ("extra", extra_tests)];
    for (name, group) in groups {
        if only.as_deref().is_none_or(|o| o == name) {
            group(&app);
        }
    }
    let failed = FAILED.with(|f| f.borrow().clone());
    println!("desktop_headless: {} passed, {} failed {:?}", PASSED.with(|p| p.get()), failed.len(), failed);
    let _ = BTreeSet::<String>::new();
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
