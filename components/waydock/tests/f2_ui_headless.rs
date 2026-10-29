//! W-f part 1 widgets (ui::indicators / tray / media / capture_controls) in a
//! private headless labwc with a fake DockView/DockApp and real pointer input
//! (wlrctl on the PRIVATE compositor). Ports of test_always_tray_ui,
//! test_tray_expander, test_tray_refresh_ui, test_inline_media_ui,
//! test_default_apps_ui (indicator parts), test_tray_coordinates (UI part) and
//! the indicator/clock/workspace parts of test_bar_activities_ui.
//!
//! Runs only inside tests/run_f2_headless.sh (WAYDOCK_F2_HEADLESS=1 and a
//! private XDG_RUNTIME_DIR); anywhere else it skips without connecting.


use glib::Variant;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use waydock::core::config::{DockConfig, ALWAYS_VISIBLE_APPS, DEFAULT_APPS};
use waydock::dbus::sni::{Action, StatusTrayBackend, TrayItem, ITEM, WATCHER, WATCHER_PATH};
use waydock::status::backend::{indicator, Status, ICONS};
use waydock::ui::capture_controls::register_capture_controls;
use waydock::ui::host::{surface, transparent_area, translate, DockApp, DockView, MenuEntry, TrayBackend};
use waydock::ui::indicators::DockIndicators;
use waydock::ui::tray::{ExpanderOptions, Hole, StatusTray, TrayExpander, TrayMenu, TrayOptions};
use waydock::wl::Workspace;

// ------------------------------------------------------------------ basics

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

fn settle(seconds: f64) { wait_for(seconds, || false); }

macro_rules! ensure {
    ($cond:expr) => { if !$cond { return Err(format!("{}:{}: {}", file!(), line!(), stringify!($cond))); } };
    ($cond:expr, $($msg:tt)+) => { if !$cond { return Err(format!("{}:{}: {}", file!(), line!(), format!($($msg)+))); } };
}

type TestResult = Result<(), String>;

fn wlrctl(args: &[&str]) {
    let status = Command::new("wlrctl").args(args).status().expect("wlrctl");
    assert!(status.success(), "wlrctl {args:?}");
}

fn pointer_move(x: f64, y: f64) {
    wlrctl(&["pointer", "move", "-100000", "-100000"]);
    wlrctl(&["pointer", "move", &(x as i64).to_string(), &(y as i64).to_string()]);
    settle(0.07);
}

fn pointer_click(x: f64, y: f64, button: &str) {
    pointer_move(x, y);
    wlrctl(&["pointer", "click", button]);
    settle(0.14);
}

fn monitor(index: u32) -> gdk::Monitor {
    gdk::Display::default().unwrap().monitors().item(index).and_downcast::<gdk::Monitor>().unwrap()
}

fn n_monitors() -> u32 { gdk::Display::default().unwrap().monitors().n_items() }

fn python_title(text: &str) -> String {
    let mut out = String::new();
    let mut cased = false;
    for c in text.chars() {
        if c.is_alphabetic() {
            if cased { out.extend(c.to_lowercase()) } else { out.extend(c.to_uppercase()) }
            cased = true;
        } else {
            out.push(c);
            cased = false;
        }
    }
    out
}

fn tray_item(name: &str, properties: &[(&str, Variant)]) -> Rc<TrayItem> {
    let item = Rc::new(TrayItem::new(name, "/StatusNotifierItem"));
    item.set_properties(properties.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    item.set_ready(true);
    item
}

fn set_property(item: &TrayItem, name: &str, value: Variant) {
    let mut properties = item.properties();
    properties.insert(name.into(), value);
    item.set_properties(properties);
}

// ------------------------------------------------------------------ fakes

#[derive(Default)]
struct FakeTray {
    items: RefCell<Vec<Rc<TrayItem>>>,
    listeners: RefCell<Vec<(u64, Rc<dyn Fn()>)>>,
    next: Cell<u64>,
    calls: RefCell<Vec<String>>,
}

impl FakeTray {
    fn changed(&self) {
        let listeners: Vec<_> = self.listeners.borrow().iter().map(|(_, l)| l.clone()).collect();
        for listener in listeners {
            listener();
        }
    }
    fn add(&self, item: Rc<TrayItem>) { self.items.borrow_mut().push(item); }
    fn remove(&self, key: &str) { self.items.borrow_mut().retain(|item| item.key() != key); }
    fn listener_count(&self) -> usize { self.listeners.borrow().len() }
}

impl TrayBackend for FakeTray {
    fn items(&self) -> Vec<Rc<TrayItem>> { self.items.borrow().clone() }
    fn subscribe(&self, changed: Rc<dyn Fn()>) -> u64 {
        self.next.set(self.next.get() + 1);
        self.listeners.borrow_mut().push((self.next.get(), changed));
        self.next.get()
    }
    fn unsubscribe(&self, id: u64) { self.listeners.borrow_mut().retain(|(known, _)| *known != id); }
    fn action(&self, item: &Rc<TrayItem>, action: Action, _failed: Option<Box<dyn FnOnce()>>) -> bool {
        self.calls.borrow_mut().push(format!("{} {:?}", item.key(), action));
        true
    }
    fn connection(&self) -> Option<gio::DBusConnection> { None }
}

thread_local!(static APPS: RefCell<Vec<Rc<FakeApp>>> = RefCell::default());

struct FakeApp {
    docks: RefCell<Vec<Rc<FakeDock>>>,
    status: RefCell<Status>,
    history: RefCell<Vec<String>>,
    media_icon: RefCell<Option<gio::Icon>>,
    fake_tray: Rc<FakeTray>,
    tray: RefCell<Option<Rc<dyn TrayBackend>>>,
    fullscreen: Cell<bool>,
    workspaces: RefCell<Vec<Workspace>>,
    workspace_error: RefCell<String>,
    clock_path: PathBuf,
}

impl FakeApp {
    /// test_dock_ui.attach_panel.
    fn new() -> Rc<Self> {
        let mut status = Map::new();
        for (name, text) in ICONS {
            let value = indicator(&json!({"text": text, "tooltip": python_title(name), "class": "on"}), name == "capslock").unwrap();
            status.insert(name.into(), Value::Object(value));
        }
        status.insert("weather".into(), Value::Object(indicator(&json!({"text": "☀ 72°", "tooltip": "Denver: Sunny, 72°F"}), false).unwrap()));
        status.insert("showdesktop".into(), Value::Object(indicator(&json!({"text": "\u{f111}", "tooltip": "Show Desktop", "class": "regular"}), false).unwrap()));
        let fake_tray = Rc::new(FakeTray::default());
        for (index, icon) in ["applications-games", "mail-unread", "dialog-information"].iter().enumerate() {
            fake_tray.add(tray_item(&format!("org.waydock.TrayFixture{index}"), &[
                ("IconName", icon.to_variant()), ("Title", "Tray fixture".to_variant()), ("Status", "Active".to_variant())]));
        }
        let workspaces = (1..=4).map(|i| Workspace { id: i.to_string(), name: i.to_string(), active: i == 1, can_activate: true,
                                                     ..Default::default() }).collect();
        let clock_path = PathBuf::from(std::env::var("TMPDIR").unwrap()).join("clock.json");
        let tray: Rc<dyn TrayBackend> = fake_tray.clone();
        let app = Rc::new(Self {
            docks: RefCell::default(), status: RefCell::new(status), history: RefCell::default(), media_icon: RefCell::new(None),
            fake_tray, tray: RefCell::new(Some(tray)), fullscreen: Cell::new(false), workspaces: RefCell::new(workspaces),
            workspace_error: RefCell::default(), clock_path,
        });
        APPS.with(|apps| apps.borrow_mut().push(app.clone()));
        app
    }
    fn push(&self, entry: impl Into<String>) { self.history.borrow_mut().push(entry.into()); }
    fn last(&self) -> Option<String> { self.history.borrow().last().cloned() }
    fn status_set(&self, name: &str, key: &str, value: Value) {
        let mut status = self.status.borrow_mut();
        let entry = status.entry(name.to_owned()).or_insert_with(|| json!({}));
        entry.as_object_mut().unwrap().insert(key.into(), value);
    }
    fn destroy_all(&self) {
        let docks = self.docks.borrow().clone();
        for dock in docks {
            dock.destroy(self);
        }
    }
}

impl DockApp for FakeApp {
    fn application(&self) -> Option<gtk::Application> { None }
    fn docks(&self) -> Vec<Rc<dyn DockView>> {
        self.docks.borrow().iter().map(|dock| dock.clone() as Rc<dyn DockView>).collect()
    }
    fn status(&self) -> Status { self.status.borrow().clone() }
    fn status_action(&self, name: &str) -> bool { self.push(format!("panel-action {name}")); true }
    fn audio_scroll(&self, step: f64) { self.push(format!("volume {step}")); }
    fn media_icon(&self) -> Option<gio::Icon> { self.media_icon.borrow().clone() }
    fn tray_backend(&self) -> Option<Rc<dyn TrayBackend>> { self.tray.borrow().clone() }
    fn panel_open(&self, name: &str, settings: bool, edge: &str, offset: i64) -> bool {
        self.push(format!("panel-open {name} settings={settings} edge={edge} offset={offset}"));
        true
    }
    fn panel_capture(&self, action: &str) -> bool { self.push(format!("capture {action}")); true }
    fn workspaces(&self, _monitor: &str) -> Option<Vec<Workspace>> { Some(self.workspaces.borrow().clone()) }
    fn activate_workspace(&self, id: &str) -> bool { self.push(format!("workspace {id}")); true }
    fn workspace_error(&self) -> String { self.workspace_error.borrow().clone() }
    fn fullscreen_active(&self) -> bool { self.fullscreen.get() }
    fn clock_store(&self) -> suite_core::clock::ClockPreferencesStore { suite_core::clock::ClockPreferencesStore::new(&self.clock_path) }
}

struct FakeDock {
    config: RefCell<Rc<DockConfig>>,
    window: gtk::Window,
    core: gtk::Box,
    monitor: gdk::Monitor,
    monitor_name: String,
    destroyed: Cell<bool>,
    holds: RefCell<Vec<String>>,
    groups: RefCell<Vec<(String, gtk::Widget, bool)>>,
    context_menu: RefCell<Option<gtk::Popover>>,
    indicators: RefCell<Option<DockIndicators>>,
    application_expander: RefCell<Option<TrayExpander>>,
    home: gtk::Button,
    layout_queued: Cell<bool>,
    me: RefCell<std::rc::Weak<FakeDock>>,
}

fn make_dock(app: &Rc<FakeApp>, index: u32, name: &str, tweak: impl FnOnce(&mut DockConfig)) -> Rc<FakeDock> {
    let mut config = DockConfig::default();
    tweak(&mut config);
    let monitor = monitor(index);
    let window = surface(None, &monitor, "waydock", Layer::Top, false);
    let horizontal = matches!(config.edge.as_str(), "top" | "bottom");
    let orientation = if horizontal { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
    let edge = match config.edge.as_str() { "top" => Edge::Top, "left" => Edge::Left, "right" => Edge::Right, _ => Edge::Bottom };
    window.set_anchor(edge, true);
    if config.mode == "bar" {
        for side in if horizontal { [Edge::Left, Edge::Right] } else { [Edge::Top, Edge::Bottom] } {
            window.set_anchor(side, true);
        }
    } else if config.float_mode {
        window.set_margin(edge, config.gap as i32);
    }
    let core = gtk::Box::new(orientation, 4);
    let home = gtk::Button::with_label("Home");
    window.set_child(Some(&core));
    let dock = Rc::new(FakeDock {
        config: RefCell::new(Rc::new(config)), window, core, monitor, monitor_name: name.into(), destroyed: Cell::new(false),
        holds: RefCell::default(), groups: RefCell::default(), context_menu: RefCell::new(None), indicators: RefCell::new(None),
        application_expander: RefCell::new(None), home, layout_queued: Cell::new(false), me: RefCell::new(std::rc::Weak::new()),
    });
    dock.me.replace(Rc::downgrade(&dock));
    let view: Rc<dyn DockView> = dock.clone();
    let panel = DockIndicators::new(&view, app.clone());
    dock.indicators.replace(Some(panel.clone()));
    if let Some(start) = panel.bar_start() {
        dock.core.append(&start);
    }
    dock.core.append(&panel.widget());
    let history = app.clone();
    dock.home.connect_clicked(move |_| history.push("home"));
    dock.core.append(&dock.home);
    app.docks.borrow_mut().push(dock.clone());
    dock.refresh_groups();
    dock.window.present();
    settle(0.15);
    dock
}

impl FakeDock {
    fn panel(&self) -> DockIndicators { self.indicators.borrow().clone().unwrap() }
    fn set_config(&self, tweak: impl FnOnce(&mut DockConfig)) {
        let mut config = (**self.config.borrow()).clone();
        tweak(&mut config);
        self.config.replace(Rc::new(config));
    }
    fn destroy(&self, app: &FakeApp) {
        if self.destroyed.replace(true) {
            return;
        }
        if let Some(panel) = self.indicators.borrow().clone() {
            panel.close();
        }
        if let Some(expander) = self.application_expander.take() {
            expander.close();
        }
        self.window.destroy();
        app.docks.borrow_mut().retain(|dock| !std::ptr::eq(dock.as_ref(), self));
        settle(0.05);
    }
    /// The window's monitor-local origin from its anchors/margins.
    fn origin(&self) -> (f64, f64) {
        let area = self.monitor.geometry();
        let (width, height) = (self.window.width() as f64, self.window.height() as f64);
        let mut x = (area.width() as f64 - width) / 2.0;
        let mut y = (area.height() as f64 - height) / 2.0;
        if self.window.is_anchor(Edge::Left) {
            x = self.window.margin(Edge::Left) as f64;
        } else if self.window.is_anchor(Edge::Right) {
            x = area.width() as f64 - width - self.window.margin(Edge::Right) as f64;
        }
        if self.window.is_anchor(Edge::Top) {
            y = self.window.margin(Edge::Top) as f64;
        } else if self.window.is_anchor(Edge::Bottom) {
            y = area.height() as f64 - height - self.window.margin(Edge::Bottom) as f64;
        }
        (x, y)
    }
    /// TrayExpanderTests.widget_point: global center of `widget`.
    fn point(&self, widget: &impl IsA<gtk::Widget>) -> (f64, f64) {
        let area = self.monitor.geometry();
        let (x, y) = self.origin();
        let local = translate(widget, &self.window, 0.0, 0.0).expect("widget inside the dock");
        (area.x() as f64 + x + local.0 + widget.width() as f64 / 2.0,
         area.y() as f64 + y + local.1 + widget.height() as f64 / 2.0)
    }
    fn sync_expanders(&self) {
        if let Some(expander) = self.indicators.borrow().clone().and_then(|panel| panel.tray_expander()) {
            expander.sync();
        }
        if let Some(expander) = self.application_expander.borrow().clone() {
            expander.sync();
        }
    }
}

impl DockView for FakeDock {
    fn config(&self) -> Rc<DockConfig> { self.config.borrow().clone() }
    fn window(&self) -> gtk::Window { self.window.clone() }
    fn orientation(&self) -> gtk::Orientation { self.core.orientation() }
    fn edge_gap(&self) -> i32 { 0 }
    fn monitor(&self) -> gdk::Monitor { self.monitor.clone() }
    fn monitor_name(&self) -> String { self.monitor_name.clone() }
    fn destroyed(&self) -> bool { self.destroyed.get() }
    fn enter(&self, token: &str) { self.holds.borrow_mut().push(token.into()); }
    fn leave(&self, token: &str) {
        let mut holds = self.holds.borrow_mut();
        if let Some(index) = holds.iter().position(|known| known == token) {
            holds.remove(index);
        }
    }
    fn queue_panel_layout(&self) {
        if self.layout_queued.replace(true) {
            return;
        }
        let weak = self.me.borrow().clone();
        glib::idle_add_local_once(move || {
            if let Some(dock) = weak.upgrade() {
                dock.layout_queued.set(false);
                if !dock.destroyed.get() {
                    dock.sync_expanders();
                }
            }
        });
    }
    fn refresh_groups(&self) {
        if self.destroyed.get() {
            return;
        }
        let Some(panel) = self.indicators.borrow().clone() else { return };
        panel.sync_apps_visibility();
        let available = panel.group_availability();
        panel.refresh_trays(false);
        let config = self.config();
        for (name, widget, registered) in self.groups.borrow().iter() {
            let visible = available.get(name.as_str()).copied().unwrap_or(*registered) && config.is_group_visible(name);
            widget.set_visible(visible);
        }
        self.sync_expanders();
    }
    fn add_group(&self, name: &str, parent: &gtk::Box, widget: &gtk::Widget, available: bool) {
        if widget.parent().is_none() {
            parent.append(widget);
        }
        widget.set_visible(true);
        self.groups.borrow_mut().push((name.into(), widget.clone(), available));
    }
    fn context_menu(&self) -> Option<gtk::Popover> { self.context_menu.borrow().clone() }
    /// dock_ui.popup_menu.
    fn popup_menu(&self, anchor: &gtk::Widget, _position: Option<(f64, f64)>, entries: Vec<MenuEntry>) {
        if let Some(expander) = self.indicators.borrow().clone().and_then(|panel| panel.tray_expander()) {
            expander.set_expanded(false);
        }
        if let Some(expander) = self.application_expander.borrow().clone() {
            expander.menu_active(true);
        }
        self.enter("menu");
        let menu = gtk::Popover::new();
        menu.set_autohide(true);
        menu.set_has_arrow(false);
        menu.add_css_class("waydock-context-menu");
        menu.set_parent(anchor);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 2);
        menu.set_child(Some(&rows));
        self.context_menu.replace(Some(menu.clone()));
        for (title, callback) in entries {
            let row = gtk::Button::with_label(&title);
            let popover = menu.clone();
            row.connect_clicked(move |_| { popover.popdown(); callback(); });
            rows.append(&row);
        }
        let weak = self.me.borrow().clone();
        menu.connect_closed(move |menu| {
            let Some(dock) = weak.upgrade() else { return };
            if dock.context_menu.borrow().as_ref() == Some(menu) {
                dock.context_menu.replace(None);
                dock.leave("menu");
            }
            if let Some(panel) = dock.indicators.borrow().clone() {
                panel.schedule_apps_collapse();
            }
            if let Some(expander) = dock.application_expander.borrow().clone() {
                expander.menu_active(false);
            }
            let menu = menu.clone();
            glib::idle_add_local_once(move || if menu.parent().is_some() { menu.unparent() });
        });
        menu.popup();
    }
    fn close_preview(&self) {}
    fn close_app_menu(&self) {}
    fn application_expander(&self) -> Option<TrayExpander> { self.application_expander.borrow().clone() }
    fn indicators(&self) -> Option<DockIndicators> { self.indicators.borrow().clone() }
    fn screen_position(&self, widget: &gtk::Widget, x: f64, y: f64) -> Option<(i32, i32)> {
        let point = translate(widget, &self.window, x, y)?;
        let area = self.monitor.geometry();
        let (origin_x, origin_y) = self.origin();
        Some(((area.x() as f64 + origin_x.round() + point.0).round_ties_even() as i32,
              (area.y() as f64 + origin_y.round() + point.1).round_ties_even() as i32))
    }
    fn dock_dimensions(&self) -> (i32, i32) { (self.window.width(), self.window.height()) }
    fn panel_frost_geometry(&self) -> (i32, i32, i32, i32) {
        let (x, y) = self.origin();
        (self.window.width(), self.window.height(), x.round() as i32, y.round() as i32)
    }
}

fn keys(tray: &StatusTray) -> BTreeSet<String> { tray.keys().into_iter().collect() }

fn children(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> { suite_gtk::utils::children(widget) }

fn bounds(widget: &impl IsA<gtk::Widget>, target: &impl IsA<gtk::Widget>) -> gtk::graphene::Rect {
    widget.compute_bounds(target).expect("bounds")
}

/// TrayExpanderTests.receiver: a BOTTOM surface counting clicks that reach it.
fn receiver(monitor: &gdk::Monitor) -> (gtk::Window, Rc<Cell<usize>>) {
    let window = surface(None, monitor, "tray-underlying-test", Layer::Bottom, false);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    window.set_child(Some(&transparent_area()));
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    waydock::ui::host::click(&window, 0, true, false, move |_, _| { seen.set(seen.get() + 1); true });
    window.present();
    settle(0.08);
    (window, count)
}

fn assert_closed(expander: &TrayExpander, edge: &str, old: &[gtk::Window]) -> TestResult {
    ensure!(!expander.expanded());
    let expected = match edge { "top" => "pan-down-symbolic", "left" => "pan-end-symbolic", "right" => "pan-start-symbolic",
                                _ => "pan-up-symbolic" };
    ensure!(expander.arrow().icon_name().as_deref() == Some(expected), "{:?}", expander.arrow().icon_name());
    ensure!(expander.button().child() == Some(expander.arrow().upcast()));
    ensure!(!expander.content().get_visible());
    ensure!(expander.backdrops().is_empty());
    ensure!(old.iter().all(|window| !window.is_mapped()));
    Ok(())
}

fn application_expander(app: &Rc<FakeApp>, dock: &Rc<FakeDock>, extra_holes: Option<Rc<dyn Fn() -> Vec<Hole>>>)
                        -> (TrayExpander, Rc<RefCell<(bool, bool, usize)>>) {
    let content = gtk::Box::new(dock.orientation(), 0);
    content.append(&gtk::Button::with_label("Application"));
    let state = Rc::new(RefCell::new((true, true, 0usize)));
    let (a, e, c) = (state.clone(), state.clone(), state.clone());
    let view: Rc<dyn DockView> = dock.clone();
    let host: Rc<dyn DockApp> = app.clone();
    let expander = TrayExpander::new(&view, &host, content.upcast_ref(), ExpanderOptions {
        kind: Some("applications".into()),
        available: Some(Rc::new(move || a.borrow().0)),
        enabled: Some(Rc::new(move || e.borrow().1)),
        close_content: Some(Rc::new(move || c.borrow_mut().2 += 1)),
        extra_holes,
    });
    dock.application_expander.replace(Some(expander.clone()));
    dock.core.prepend(&expander.widget());
    dock.queue_panel_layout();
    settle(0.15);
    (expander, state)
}

// ------------------------------------------------------------------ test_always_tray_ui

fn always_tray_routing_all_layouts_and_return_to_normal_tray() -> TestResult {
    let app = FakeApp::new();
    let items = app.fake_tray.items();
    set_property(&items[0], "Id", "music-player".to_variant());
    set_property(&items[0], "Status", "Passive".to_variant());
    let selected = "id:music-player".to_string();
    for mode in ["dock", "bar"] {
        for display in ["icons", "title"] {
            for edge in ["bottom", "top", "left", "right"] {
                let dock = make_dock(&app, 0, "HEADLESS-1", |c| {
                    c.mode = mode.into(); c.application_display = display.into(); c.edge = edge.into();
                    c.always_show_tray = vec![selected.clone()];
                });
                let panel = dock.panel();
                let (tray, always) = (panel.tray().unwrap(), panel.always_tray().unwrap());
                ensure!(keys(&always) == BTreeSet::from([items[0].key()]), "{mode} {edge}: {:?}", always.keys());
                ensure!(!keys(&tray).contains(&items[0].key()));
                ensure!(tray.keys().len() == 2);
                ensure!(always.widget().is_mapped(), "{mode} {display} {edge}");
                ensure!(children(&panel.service_area()) == vec![always.widget().upcast::<gtk::Widget>(), panel.apps().upcast()]);
                let order = children(&panel.apps());
                let brightness = order.iter().position(|w| Some(w) == panel.button("brightness").map(|b| b.upcast()).as_ref()).unwrap();
                ensure!(order[brightness + 1] == panel.showdesktop().upcast::<gtk::Widget>());
                ensure!(!order.iter().any(|w| w.is::<gtk::Separator>()));
                ensure!(!panel.tray_expander().unwrap().expanded());
                panel.showdesktop().emit_clicked();
                ensure!(app.last().as_deref() == Some("panel-action showdesktop"));
                dock.set_config(|c| c.always_show_tray = Vec::new());
                dock.refresh_groups();
                ensure!(!always.widget().get_visible());
                ensure!(always.keys().is_empty());
                set_property(&items[0], "Status", "Active".to_variant());
                tray.refresh();
                ensure!(keys(&tray).contains(&items[0].key()));
                set_property(&items[0], "Status", "Passive".to_variant());
                dock.destroy(&app);
            }
        }
    }
    Ok(())
}

fn always_tray_late_pixmap_item_fits_before_always_tray() -> TestResult {
    let app = FakeApp::new();
    set_property(&app.fake_tray.items()[0], "Id", "always".to_variant());
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| {
        c.mode = "bar".into(); c.application_display = "title".into(); c.edge = "top".into();
        c.always_show_tray = vec!["id:always".into()];
    });
    let panel = dock.panel();
    panel.tray_expander().unwrap().set_expanded(true);
    settle(0.2);
    let pixels: Vec<u8> = [255u8, 255, 0, 0].repeat(48 * 48);
    let pixmap = vec![(48i32, 48i32, pixels)].to_variant();
    let item = tray_item("org.waydock.LargePixmapFixture", &[("Id", "late-pixmap".to_variant()), ("Status", "Active".to_variant()),
                         ("Title", "Late pixmap".to_variant()), ("IconPixmap", pixmap)]);
    app.fake_tray.add(item.clone());
    app.fake_tray.changed();
    settle(0.3);
    let tray = panel.tray().unwrap();
    let button = tray.button(&item.key()).ok_or("late item missing")?;
    let icon = bounds(&button.child().unwrap(), &dock.window);
    let regular = bounds(&tray.widget(), &dock.window);
    let always = bounds(&panel.always_tray().unwrap().widget(), &dock.window);
    ensure!(icon.x() + icon.width() <= regular.x() + regular.width() + 0.5);
    ensure!(regular.x() + regular.width() <= always.x() + 0.5, "{regular:?} {always:?}");
    ensure!(button.width() >= tray.icon_size(), "{} < {}", button.width(), tray.icon_size());
    ensure!(tray.image(&item.key()).unwrap().paintable().is_some(), "pixmap rendered as a texture");
    Ok(())
}

fn always_tray_native_tooltips() -> TestResult {
    let app = FakeApp::new();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.application_display = "title".into(); c.edge = "top".into(); });
    let panel = dock.panel();
    for name in panel.button_names() {
        let button = panel.button(&name).unwrap();
        ensure!(button.tooltip_text().is_some_and(|t| !t.is_empty()), "{name}");
    }
    ensure!(panel.logout().tooltip_text().is_some() && panel.clock().tooltip_text().is_some());
    ensure!(panel.button("idle_inhibitor").unwrap().tooltip_text().unwrap().starts_with("Keep Awake"));
    ensure!(panel.button("brightness").unwrap().tooltip_text().unwrap().starts_with("Displays"));
    ensure!(panel.button("weather").unwrap().tooltip_text().unwrap() == "Weather\nDenver: Sunny, 72°F");
    let old = panel.button("audio").unwrap().tooltip_text();
    panel.refresh();
    ensure!(panel.button("audio").unwrap().tooltip_text() == old);
    // Status classes from the scripts become CSS classes; removed ones are dropped.
    let audio = panel.button("audio").unwrap();
    ensure!(audio.has_css_class("on") && audio.has_css_class("waydock-status") && audio.has_css_class("status-audio"));
    app.status_set("audio", "classes", json!(["muted"]));
    panel.refresh();
    ensure!(audio.has_css_class("muted") && !audio.has_css_class("on"));
    Ok(())
}

fn always_tray_restarted_application_keeps_placement_on_every_clone() -> TestResult {
    let app = FakeApp::new();
    let original = app.fake_tray.items()[0].clone();
    set_property(&original, "Id", "persistent-player".to_variant());
    let first = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.always_show_tray = vec!["id:persistent-player".into()]; });
    let second = make_dock(&app, 1, "HEADLESS-2", |c| { c.application_display = "title".into(); c.always_show_tray = vec!["id:persistent-player".into()]; });
    app.fake_tray.remove(&original.key());
    app.fake_tray.changed();
    settle(0.05);
    for dock in [&first, &second] {
        ensure!(!dock.panel().always_tray().unwrap().widget().get_visible());
    }
    let restarted = Rc::new(TrayItem::new(":1.900", "/StatusNotifierItem"));
    restarted.set_properties(original.properties());
    restarted.set_ready(true);
    app.fake_tray.add(restarted.clone());
    app.fake_tray.changed();
    settle(0.05);
    for dock in [&first, &second] {
        let panel = dock.panel();
        ensure!(panel.always_tray().unwrap().widget().get_visible());
        ensure!(keys(&panel.always_tray().unwrap()) == BTreeSet::from([restarted.key()]));
        ensure!(!keys(&panel.tray().unwrap()).contains(&restarted.key()));
    }
    Ok(())
}

// ------------------------------------------------------------------ test_tray_refresh_ui

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" rx="5" fill="COLOR"/></svg>"##;

struct RefreshFixture {
    backend: Rc<FakeTray>,
    item: Rc<TrayItem>,
    other: Rc<TrayItem>,
    views: Vec<StatusTray>,
    window: gtk::Window,
    root: PathBuf,
}

fn refresh_fixture(tag: &str) -> RefreshFixture {
    let root = PathBuf::from(std::env::var("TMPDIR").unwrap()).join(format!("tray-refresh-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("example.svg"), SVG.replace("COLOR", "#e34243")).unwrap();
    let backend = Rc::new(FakeTray::default());
    let item = Rc::new(TrayItem::new("org.example.ChangingTitle", "/Tray"));
    item.set_properties(HashMap::from([("IconName".into(), "example".to_variant()),
        ("IconThemePath".into(), root.to_str().unwrap().to_variant()), ("Title".into(), "An application".to_variant())]));
    item.set_ready(true);
    let other = Rc::new(TrayItem::new("org.example.Other", "/Tray"));
    other.set_properties(HashMap::from([("IconName".into(), "network-wired-symbolic".to_variant()),
                                        ("Title".into(), "Another application".to_variant())]));
    other.set_ready(true);
    backend.add(item.clone());
    backend.add(other.clone());
    let window = gtk::Window::new();
    window.set_default_size(300, 100);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let dyn_backend: Rc<dyn TrayBackend> = backend.clone();
    let views = vec![StatusTray::new(dyn_backend.clone(), TrayOptions::default()), StatusTray::new(dyn_backend, TrayOptions::default())];
    for view in &views {
        column.append(&view.widget());
    }
    window.set_child(Some(&column));
    window.present();
    settle(0.2);
    RefreshFixture { backend, item, other, views, window, root }
}

impl RefreshFixture {
    fn paintables(&self) -> HashMap<(usize, String), Option<gdk::Paintable>> {
        let mut result = HashMap::new();
        for (index, view) in self.views.iter().enumerate() {
            for key in view.keys() {
                result.insert((index, key.clone()), view.image(&key).unwrap().paintable());
            }
        }
        result
    }
    fn close(self) {
        for view in &self.views {
            view.close();
        }
        self.window.destroy();
        settle(0.05);
    }
}

fn rss_kib() -> i64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).unwrap().parse().unwrap()
}

fn same(a: &Option<gdk::Paintable>, b: &Option<gdk::Paintable>) -> bool { a == b }

fn tray_refresh_static_metadata_reuses_paintables_and_bounds_memory() -> TestResult {
    let f = refresh_fixture("static");
    let original = f.paintables();
    ensure!(original.values().all(|p| p.is_some()));
    let calls: Vec<u64> = f.views.iter().map(|v| v.pixbuf_calls()).collect();
    settle(0.2);
    let before = rss_kib();
    for number in 0..5000 {
        let mut properties = f.item.properties();
        properties.insert("Title".into(), format!("Working {number}").to_variant());
        properties.insert("ToolTip".into(), ("", Vec::<(i32, i32, Vec<u8>)>::new(), "Application", format!("Updated {number}")).to_variant());
        f.item.set_properties(properties);
        f.backend.changed();
        if number % 100 == 0 {
            settle(0.002);
        }
    }
    settle(0.15);
    let after_calls: Vec<u64> = f.views.iter().map(|v| v.pixbuf_calls()).collect();
    ensure!(calls == after_calls, "Metadata changes must not resolve or decode icons: {calls:?} → {after_calls:?}");
    let after = f.paintables();
    for (key, value) in &original {
        ensure!(same(&after[key], value), "{key:?} paintable replaced");
    }
    ensure!(f.views[0].button(&f.item.key()).unwrap().tooltip_text().as_deref() == Some("Application\nUpdated 4999"));
    let growth = rss_kib() - before;
    ensure!(growth < 64 * 1024, "Static tray updates retained too much memory: {growth} KiB");
    f.close();
    Ok(())
}

fn tray_refresh_icon_signal_and_theme_change_reload_only_when_needed() -> TestResult {
    let f = refresh_fixture("signal");
    let original = f.paintables();
    let png = |p: &Option<gdk::Paintable>| p.clone().and_downcast::<gdk::Texture>().unwrap().save_to_png_bytes();
    let original_pixels = png(&original[&(0, f.item.key())]);
    std::fs::write(f.root.join("example.svg"), SVG.replace("COLOR", "#31a763")).unwrap();
    f.item.note_signal("NewIcon");
    f.backend.changed();
    let changed = f.paintables();
    ensure!(png(&changed[&(0, f.item.key())]) != original_pixels, "same-file icon reload");
    for index in 0..2 {
        ensure!(!same(&changed[&(index, f.item.key())], &original[&(index, f.item.key())]));
        ensure!(same(&changed[&(index, f.other.key())], &original[&(index, f.other.key())]));
    }
    f.views[0].theme_changed();
    let themed = f.paintables();
    ensure!(!same(&themed[&(0, f.item.key())], &changed[&(0, f.item.key())]));
    ensure!(same(&themed[&(1, f.item.key())], &changed[&(1, f.item.key())]));
    f.backend.changed();
    let repeated = f.paintables();
    for (key, value) in &themed {
        ensure!(same(&repeated[key], value));
    }
    f.close();
    Ok(())
}

fn tray_refresh_private_themes_are_bounded_by_current_items() -> TestResult {
    let f = refresh_fixture("themes");
    for number in 0..16 {
        let path = f.root.join(number.to_string());
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("example.svg"), SVG.replace("COLOR", "#9a78c2")).unwrap();
        set_property(&f.item, "IconThemePath", path.to_str().unwrap().to_variant());
        f.backend.changed();
        for view in &f.views {
            ensure!(view.custom_theme_paths() == vec![path.to_str().unwrap().to_string()], "{:?}", view.custom_theme_paths());
        }
    }
    f.backend.remove(&f.item.key());
    f.backend.changed();
    for view in &f.views {
        ensure!(view.custom_theme_paths().is_empty());
        ensure!(view.button(&f.item.key()).is_none());
    }
    for view in &f.views {
        view.close();
    }
    ensure!(f.backend.listener_count() == 0);
    f.window.destroy();
    Ok(())
}

// ------------------------------------------------------------------ test_tray_expander

fn expander_application_outside_dismissal_and_tray_mutual_exclusion() -> TestResult {
    let app = FakeApp::new();
    let first = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "bottom".into(); });
    let second = make_dock(&app, 1, "HEADLESS-2", |c| { c.mode = "bar".into(); c.edge = "left".into(); });
    let (application, state) = application_expander(&app, &first, None);
    let (other_application, _other) = application_expander(&app, &second, None);
    let tray = first.panel().tray_expander().unwrap();
    tray.set_expanded(true);
    ensure!(tray.expanded());
    settle(0.15);
    let (x, y) = first.point(&application.button());
    pointer_click(x, y, "left");
    ensure!(application.expanded());
    ensure!(!tray.expanded());
    ensure!(application.button().tooltip_text().as_deref() == Some("Hide applications"));
    ensure!(application.content().get_visible());
    ensure!(application.backdrops().len() == n_monitors() as usize);
    let old = application.backdrops();
    pointer_click(40.0, 40.0, "left");
    assert_closed(&application, "bottom", &old)?;
    ensure!(state.borrow().2 == 1);
    ensure!(application.button().tooltip_text().as_deref() == Some("Show applications"));
    application.set_expanded(true);
    other_application.set_expanded(true);
    ensure!(!application.expanded(), "Application groups on clones are mutually exclusive");
    ensure!(other_application.expanded());
    tray.set_expanded(true);
    ensure!(!other_application.expanded(), "A system tray also closes a clone's applications");
    ensure!(tray.expanded());
    Ok(())
}

fn expander_application_availability_and_native_menu_lifecycle() -> TestResult {
    let app = FakeApp::new();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| c.mode = "bar".into());
    let (application, state) = application_expander(&app, &dock, None);
    application.set_expanded(true);
    application.menu_active(true);
    ensure!(application.expanded());
    ensure!(application.backdrops().is_empty());
    application.menu_active(false);
    ensure!(!application.expanded());
    for key in 0..2 {
        application.set_expanded(true);
        ensure!(application.expanded());
        if key == 0 { state.borrow_mut().0 = false } else { state.borrow_mut().1 = false }
        application.sync();
        ensure!(!application.expanded());
        ensure!(application.backdrops().is_empty());
        application.set_expanded(true);
        ensure!(!application.expanded());
        if key == 0 { state.borrow_mut().0 = true } else { state.borrow_mut().1 = true }
    }
    Ok(())
}

fn expander_preview_hole_keeps_layer_surface_clickable() -> TestResult {
    let app = FakeApp::new();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "bottom".into(); });
    let preview = surface(None, &dock.monitor, "application-preview-test", Layer::Top, false);
    preview.set_anchor(Edge::Top, true);
    preview.set_anchor(Edge::Left, true);
    preview.set_margin(Edge::Top, 100);
    preview.set_margin(Edge::Left, 100);
    let button = gtk::Button::with_label("Choose window");
    button.set_size_request(180, 80);
    preview.set_child(Some(&button));
    let received = Rc::new(Cell::new(0));
    let seen = received.clone();
    button.connect_clicked(move |_| seen.set(seen.get() + 1));
    preview.present();
    settle(0.15);
    let (monitor, window) = (dock.monitor.clone(), preview.clone());
    let (application, _) = application_expander(&app, &dock, Some(Rc::new(move || {
        vec![(monitor.clone(), 100, 100, window.width(), window.height())]
    })));
    application.set_expanded(true);
    settle(0.1);
    let area = dock.monitor.geometry();
    pointer_click(area.x() as f64 + 100.0 + preview.width() as f64 / 2.0, area.y() as f64 + 100.0 + preview.height() as f64 / 2.0, "left");
    ensure!(received.get() == 1, "The outside catcher must not block application previews");
    ensure!(application.expanded());
    pointer_click(40.0, 40.0, "left");
    ensure!(!application.expanded());
    preview.destroy();
    Ok(())
}

fn expander_toggle_and_outside_click_on_each_monitor() -> TestResult {
    let app = FakeApp::new();
    let receivers: Vec<_> = (0..n_monitors()).map(|i| receiver(&monitor(i))).collect();
    let dock = make_dock(&app, 0, "HEADLESS-1", |_| {});
    let expander = dock.panel().tray_expander().unwrap();
    assert_closed(&expander, "bottom", &[])?;
    ensure!(expander.content() == dock.panel().tray().unwrap().widget().upcast::<gtk::Widget>());
    for (index, (_window, received)) in receivers.iter().enumerate() {
        let (x, y) = dock.point(&expander.button());
        pointer_click(x, y, "left");
        ensure!(expander.expanded());
        ensure!(expander.arrow().icon_name().as_deref() == Some("pan-down-symbolic"));
        ensure!(expander.content().get_visible());
        ensure!(expander.backdrops().len() == n_monitors() as usize);
        ensure!(dock.holds.borrow().contains(&"tray-expanded".to_string()));
        for window in expander.backdrops() {
            ensure!(window.layer() == Layer::Top);
            ensure!(window.keyboard_mode() == KeyboardMode::None);
            ensure!(window.exclusive_zone() == -1);
            ensure!(window.namespace().as_deref() == Some("waydock-tray-dismiss"));
            ensure!(!window.is_active());
        }
        let area = monitor(index as u32).geometry();
        let (x, y) = (area.x() as f64 + 40.0, area.y() as f64 + 40.0);
        pointer_move(x, y);
        settle(0.25);
        ensure!(expander.expanded(), "Pointer leave must not retract the tray");
        let before = received.get();
        let backdrops = expander.backdrops();
        pointer_click(x, y, "left");
        assert_closed(&expander, "bottom", &backdrops).map_err(|e| format!("monitor {index} outside click: {e}"))?;
        ensure!(received.get() == before, "The dismissal click is consumed");
        ensure!(!dock.holds.borrow().contains(&"tray-expanded".to_string()));
        pointer_click(x, y, "left");
        ensure!(received.get() == before + 1, "No hidden click catcher remains");
    }
    let (x, y) = dock.point(&expander.button());
    pointer_click(x, y, "left");
    ensure!(expander.expanded());
    settle(0.1);
    let backdrops = expander.backdrops();
    // The centered dock grew with the inline tray: aim at the button's new place.
    let (x, y) = dock.point(&expander.button());
    pointer_click(x, y, "left");
    assert_closed(&expander, "bottom", &backdrops).map_err(|e| format!("toggle off: {e}"))?;
    for (window, _) in receivers {
        window.destroy();
    }
    Ok(())
}

fn expander_cloned_docks_remain_clickable_and_holes_follow_geometry() -> TestResult {
    let app = FakeApp::new();
    let first = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "top".into(); c.float_mode = true; });
    let second = make_dock(&app, 1, "HEADLESS-2", |c| { c.edge = "left".into(); c.alignment = "end".into(); });
    let expanded = first.panel().tray_expander().unwrap();
    let (x, y) = first.point(&expanded.button());
    pointer_click(x, y, "left");
    ensure!(expanded.expanded());
    let before = app.history.borrow().len();
    let (x, y) = second.point(&second.home);
    pointer_click(x, y, "left");
    ensure!(app.history.borrow()[before..] == ["home".to_string()], "{:?}", app.history.borrow());
    ensure!(expanded.expanded(), "A click inside a dock clone is not outside the dock");
    let extra = tray_item("org.waydock.ExtraTrayFixture", &[("IconName", "mail-unread".to_variant()),
        ("Title", "Extra tray item".to_variant()), ("Status", "Active".to_variant())]);
    app.fake_tray.add(extra);
    app.fake_tray.changed();
    settle(0.25);
    ensure!(expanded.expanded());
    let (x, y) = first.point(&expanded.button());
    pointer_click(x, y, "left");
    ensure!(!expanded.expanded());
    Ok(())
}

fn expander_inline_expansion_on_every_edge() -> TestResult {
    let app = FakeApp::new();
    for mode in ["dock", "bar"] {
        for edge in ["top", "bottom", "left", "right"] {
            let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = mode.into(); c.edge = edge.into(); });
            let expander = dock.panel().tray_expander().unwrap();
            ensure!(expander.widget().orientation() == dock.orientation());
            assert_closed(&expander, edge, &[])?;
            let (x, y) = dock.point(&expander.button());
            pointer_click(x, y, "left");
            ensure!(expander.expanded(), "{mode} {edge}");
            let expected = match edge { "bottom" => "pan-down-symbolic", "top" => "pan-up-symbolic", "left" => "pan-start-symbolic",
                                        _ => "pan-end-symbolic" };
            ensure!(expander.arrow().icon_name().as_deref() == Some(expected));
            let backdrops = expander.backdrops();
            let (x, y) = dock.point(&expander.button());
            pointer_click(x, y, "left");
            assert_closed(&expander, edge, &backdrops)?;
            dock.destroy(&app);
        }
    }
    Ok(())
}

struct PopoverMenu {
    popover: gtk::Popover,
}

impl TrayMenu for PopoverMenu {
    fn popup(&self) { self.popover.popup() }
    fn popdown(&self) { self.popover.popdown() }
    fn close(&self) {}
}

fn expander_native_tray_popover_stays_clickable_and_dismissal_retracts() -> TestResult {
    let app = FakeApp::new();
    let item = app.fake_tray.items()[0].clone();
    set_property(&item, "Menu", glib::variant::ObjectPath::try_from("/Menu").unwrap().to_variant());
    let dock = make_dock(&app, 0, "HEADLESS-1", |_| {});
    let (expander, tray) = (dock.panel().tray_expander().unwrap(), dock.panel().tray().unwrap());
    let invoked = Rc::new(Cell::new(0));
    let popover_slot: Rc<RefCell<Option<gtk::Popover>>> = Rc::default();
    let (seen, slot) = (invoked.clone(), popover_slot.clone());
    tray.set_menu_factory(Some(Rc::new(move |_item, button, closed| {
        let popover = gtk::Popover::new();
        popover.set_has_arrow(false);
        let action = gtk::Button::with_label("Tray menu action");
        popover.set_child(Some(&action));
        popover.set_parent(button);
        let menu: Rc<dyn TrayMenu> = Rc::new(PopoverMenu { popover: popover.clone() });
        let (seen, target) = (seen.clone(), popover.clone());
        action.connect_clicked(move |_| { seen.set(seen.get() + 1); target.popdown(); });
        let handle = Rc::downgrade(&menu);
        popover.connect_closed(move |_| if let Some(menu) = handle.upgrade() { closed(&menu) });
        slot.replace(Some(popover));
        Some(menu)
    })));
    tray.refresh();
    let (x, y) = dock.point(&expander.button());
    pointer_click(x, y, "left");
    ensure!(expander.expanded());
    let button = tray.button(&item.key()).unwrap();
    let (x, y) = dock.point(&button);
    pointer_click(x, y, "right");
    let popup = popover_slot.borrow().clone().ok_or("menu not prepared")?;
    ensure!(popup.is_mapped(), "Native right-click must open the tray popover");
    ensure!(expander.expanded());
    ensure!(expander.backdrops().is_empty(), "Native menu grab owns outside-click dismissal");
    let child = popup.child().unwrap();
    let local = translate(&child, &popup, 0.0, 0.0).unwrap();
    let area = dock.monitor.geometry();
    let (ox, oy) = dock.origin();
    // The popup surface position relative to the dock window (xdg_popup).
    let (px, py) = popup_position(&popup).ok_or("popup position")?;
    pointer_click(area.x() as f64 + ox + px + local.0 + child.width() as f64 / 2.0,
                  area.y() as f64 + oy + py + local.1 + child.height() as f64 / 2.0, "left");
    ensure!(invoked.get() == 1, "Opening the tray must not block native menu actions");
    assert_closed(&expander, "bottom", &[])?;
    let (x, y) = dock.point(&expander.button());
    pointer_click(x, y, "left");
    let (x, y) = dock.point(&button);
    pointer_click(x, y, "right");
    ensure!(popup.is_mapped());
    pointer_click(35.0, 35.0, "left");
    settle(0.1);
    ensure!(!popup.get_visible());
    assert_closed(&expander, "bottom", &[])?;
    popup.unparent();
    Ok(())
}

/// The popover's surface origin within its parent toplevel (Python reads
/// `native.get_position_x/y`, i.e. the popup surface position).
fn popup_position(popup: &gtk::Popover) -> Option<(f64, f64)> {
    let surface = popup.surface()?.downcast::<gdk::Popup>().ok()?;
    Some((surface.position_x() as f64, surface.position_y() as f64))
}

fn expander_hidden_empty_and_destroyed_release_input_surfaces() -> TestResult {
    let app = FakeApp::new();
    let dock = make_dock(&app, 0, "HEADLESS-1", |_| {});
    let expander = dock.panel().tray_expander().unwrap();
    for reason in ["hidden", "empty", "destroy"] {
        expander.set_expanded(true);
        settle(0.15);
        ensure!(expander.expanded(), "{reason}");
        let backdrops = expander.backdrops();
        ensure!(!backdrops.is_empty());
        match reason {
            "hidden" => {
                dock.set_config(|c| { c.group_visibility.insert("system_tray", false); });
                dock.refresh_groups();
            }
            "empty" => {
                for item in app.fake_tray.items() {
                    set_property(&item, "Status", "Passive".to_variant());
                }
                app.fake_tray.changed();
            }
            _ => dock.destroy(&app),
        }
        settle(0.2);
        assert_closed(&expander, "bottom", &backdrops)?;
        match reason {
            "hidden" => {
                dock.set_config(|c| { c.group_visibility.insert("system_tray", true); });
                dock.refresh_groups();
            }
            "empty" => {
                for item in app.fake_tray.items() {
                    set_property(&item, "Status", "Active".to_variant());
                }
                app.fake_tray.changed();
            }
            _ => {}
        }
        settle(0.1);
    }
    Ok(())
}

fn expander_catchers_never_cover_fullscreen() -> TestResult {
    let app = FakeApp::new();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "top".into(); });
    let expander = dock.panel().tray_expander().unwrap();
    expander.set_expanded(true);
    settle(0.1);
    ensure!(expander.expanded());
    let clicked = Rc::new(Cell::new(0));
    let game = gtk::Window::new();
    game.set_title(Some("Private fullscreen tray fixture"));
    game.set_child(Some(&gtk::Label::new(Some("Fullscreen content"))));
    let seen = clicked.clone();
    waydock::ui::host::click(&game, 0, false, false, move |_, _| { seen.set(seen.get() + 1); true });
    game.fullscreen_on_monitor(&dock.monitor);
    game.present();
    settle(0.3);
    ensure!(game.is_fullscreen());
    let area = dock.monitor.geometry();
    pointer_click(area.x() as f64 + 50.0, area.y() as f64 + 50.0, "left");
    ensure!(clicked.get() == 1, "Tray backdrops must stay beneath dedicated fullscreen content");
    // A fullscreen game also refuses (re-)expansion and sync retracts.
    app.fullscreen.set(true);
    expander.sync();
    ensure!(!expander.expanded() && expander.backdrops().is_empty());
    expander.set_expanded(true);
    ensure!(!expander.expanded());
    app.fullscreen.set(false);
    game.destroy();
    Ok(())
}

// ------------------------------------------------------------------ test_inline_media_ui

fn media_state(app: &FakeApp) {
    app.status.borrow_mut().insert("media".into(), json!({
        "title": "A track <with> symbols", "artist": "An artist", "tooltip": "Spotify\nA track <with> symbols",
        "playing": true, "can_previous": true, "can_toggle": true, "can_next": true, "visible": true}));
}

fn available_icon() -> Option<String> {
    let theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());
    ["utilities-terminal", "web-browser", "org.gnome.Settings", "applications-games", "mail-unread", "dialog-information"]
        .into_iter().find(|name| theme.has_icon(name)).map(str::to_owned)
}

fn media_inline_controls_and_popup_button() -> TestResult {
    let app = FakeApp::new();
    media_state(&app);
    let icon_name = available_icon().ok_or("no themed icon available")?;
    app.media_icon.replace(Some(gio::ThemedIcon::from_names(&["multimedia-player", &icon_name]).upcast()));
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.application_display = "title".into(); c.edge = "top".into(); });
    let panel = dock.panel();
    let media = panel.media();
    let media_button = panel.button("media").unwrap();
    ensure!(!media.widget().is_ancestor(&panel.apps()));
    ensure!(media_button.parent() == Some(media.widget().upcast()));
    ensure!(media.widget().last_child() == Some(media_button.clone().upcast()));
    ensure!(media.widget().first_child() == Some(media.song().upcast()));
    ensure!(media_button.child() == Some(media.popup_visual().upcast()));
    ensure!(media.player_icon().parent() == Some(media.popup_visual().upcast()));
    ensure!(media.song().text().contains("A track <with> symbols") && media.song().text().contains("An artist"));
    ensure!(media.song().text() == "A track <with> symbols · An artist");
    ensure!(media.player_icon().gicon().unwrap().to_string().unwrap().contains(&icon_name));
    ensure!(media.popup_visual().visible_child_name().as_deref() == Some("player"));
    ensure!(media.control("toggle").unwrap().tooltip_text().as_deref() == Some("Pause"));
    for name in ["previous", "next", "toggle"] {
        media.control(name).unwrap().emit_clicked();
        ensure!(app.last() == Some(format!("panel-action media-{name}")));
    }
    app.status_set("media", "playing", json!(false));
    app.status_set("media", "can_previous", json!(false));
    media.refresh();
    ensure!(media.control("toggle").unwrap().tooltip_text().as_deref() == Some("Play"));
    ensure!(!media.control("previous").unwrap().is_sensitive());
    media_button.emit_clicked();
    ensure!(app.last().is_some_and(|l| l.starts_with("panel-open media ")), "{:?}", app.last());
    // Default-app visibility does not affect the separate group.
    dock.set_config(|c| { c.group_visibility.insert("service_apps", false); });
    dock.refresh_groups();
    ensure!(media.widget().get_visible());
    dock.set_config(|c| { c.group_visibility.insert("media", false); });
    dock.refresh_groups();
    ensure!(!media.widget().get_visible());
    Ok(())
}

fn media_idle_bar_hides_nothing_playing_and_disabled_controls() -> TestResult {
    let app = FakeApp::new();
    media_state(&app);
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "top".into(); });
    let media = dock.panel().media();
    ensure!(media.text().get_visible());
    let saved = app.status.borrow().get("media").cloned().unwrap();
    app.status.borrow_mut().insert("media".into(), json!({}));
    media.refresh();
    ensure!(!media.text().get_visible());
    ensure!(media.song().text() == "Nothing playing");
    ensure!(media.widget().has_css_class("idle"));
    ensure!(media.controls().values().all(|b| !b.get_visible()));
    ensure!(dock.panel().button("media").unwrap().get_visible());
    app.status.borrow_mut().insert("media".into(), json!({"can_toggle": true}));
    media.refresh();
    ensure!(media.control("toggle").unwrap().get_visible());
    ensure!(!media.control("next").unwrap().get_visible());
    app.status.borrow_mut().insert("media".into(), saved);
    media.refresh();
    ensure!(media.text().get_visible());
    ensure!(!media.widget().has_css_class("idle"));
    ensure!(media.controls().values().all(|b| b.get_visible()));
    Ok(())
}

fn media_missing_player_icons_fall_back_to_music_button() -> TestResult {
    let app = FakeApp::new();
    media_state(&app);
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "top".into(); });
    let panel = dock.panel();
    let media = panel.media();
    let status = panel.status_button("media").unwrap();
    let icons: Vec<Option<gio::Icon>> = vec![
        None,
        Some(gio::ThemedIcon::from_names(&["missing-player-12345", "multimedia-player", "application-x-executable"]).upcast()),
        Some(gio::FileIcon::new(&gio::File::for_path("/no-player-icon-here.png")).upcast()),
    ];
    let before = app.history.borrow().len();
    for icon in icons {
        app.media_icon.replace(icon);
        media.refresh();
        ensure!(media.popup_visual().visible_child_name().as_deref() == Some("music"));
        ensure!(media.popup_visual().visible_child() == Some(status.visual.clone().upcast()));
        ensure!(media.player_icon().gicon().is_none());
        panel.button("media").unwrap().emit_clicked();
    }
    ensure!(app.history.borrow()[before..].iter().filter(|l| l.starts_with("panel-open media")).count() == 3);
    let bytes = gio::BytesIcon::new(&glib::Bytes::from_static(b"\x89PNG"));
    app.media_icon.replace(Some(bytes.upcast()));
    media.refresh();
    ensure!(media.popup_visual().visible_child_name().as_deref() == Some("player"), "BytesIcon is a real icon");
    Ok(())
}

fn media_titles_and_controls_fit_all_edges() -> TestResult {
    let app = FakeApp::new();
    media_state(&app);
    app.status_set("media", "title", json!("An extremely long song name ".repeat(15)));
    for edge in ["top", "bottom", "left", "right"] {
        for mode in ["bar", "dock"] {
            let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = mode.into(); c.edge = edge.into(); c.application_display = "title".into(); });
            let media = dock.panel().media();
            settle(0.15);
            ensure!(media.widget().is_mapped(), "{mode} {edge}");
            let horizontal = dock.horizontal();
            let cross = if horizontal { media.widget().height() } else { media.widget().width() };
            let along = if horizontal { media.widget().width() } else { media.widget().height() };
            ensure!(cross <= 48, "{mode} {edge}: cross {cross}");
            ensure!(along < 400, "{mode} {edge}: length {along}");
            ensure!(horizontal == media.text().is::<gtk::Label>(), "VerticalText on side edges");
            dock.destroy(&app);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ test_default_apps_ui

fn visible_apps(panel: &DockIndicators) -> BTreeSet<String> {
    DEFAULT_APPS.iter().filter(|(name, _)| panel.button(name).unwrap().get_visible()).map(|(name, _)| name.to_string()).collect()
}

fn default_apps_setup() -> Rc<FakeApp> {
    let app = FakeApp::new();
    app.status_set("capslock", "text", json!("⇪"));
    app.status_set("capslock", "visible", json!(true));
    pointer_move(500.0, 300.0);
    app
}

fn default_apps_hover_expands_and_leaving_collapses() -> TestResult {
    let app = default_apps_setup();
    let always: BTreeSet<String> = ALWAYS_VISIBLE_APPS.iter().map(|s| s.to_string()).collect();
    for mode in ["dock", "bar"] {
        for display in ["icons", "title"] {
            for edge in ["bottom", "top", "left", "right"] {
                let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = mode.into(); c.application_display = display.into(); c.edge = edge.into(); });
                let panel = dock.panel();
                ensure!(visible_apps(&panel) == always, "{mode} {display} {edge}: {:?}", visible_apps(&panel));
                ensure!(panel.apps_toggle().is_mapped());
                let (x, y) = dock.point(&panel.apps_toggle());
                pointer_move(x, y);
                settle(0.2);
                ensure!(panel.apps_expanded(), "{mode} {display} {edge}");
                let expected: BTreeSet<String> = DEFAULT_APPS.iter()
                    .filter(|(name, _)| panel.status_visible(name).unwrap_or(false)).map(|(n, _)| n.to_string()).collect();
                ensure!(visible_apps(&panel) == expected);
                panel.refresh();
                ensure!(visible_apps(&panel) == expected);
                pointer_move(500.0, 300.0);
                settle(0.45);
                ensure!(!panel.apps_expanded(), "{mode} {display} {edge} stayed expanded");
                ensure!(visible_apps(&panel) == always);
                dock.destroy(&app);
            }
        }
    }
    Ok(())
}

fn default_apps_empty_and_all_selections_keep_reveal_and_tray() -> TestResult {
    let app = default_apps_setup();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| {
        c.mode = "bar".into(); c.always_visible_apps = Vec::new(); c.always_show_tray = vec!["name:org.waydock.TrayFixture0".into()];
    });
    let panel = dock.panel();
    let always = panel.always_tray().unwrap();
    ensure!(visible_apps(&panel).is_empty());
    ensure!(panel.apps_toggle().is_mapped());
    ensure!(always.widget().is_mapped());
    ensure!(children(&panel.service_area()) == vec![always.widget().upcast::<gtk::Widget>(), panel.apps().upcast()]);
    let (_, tray_button) = always.buttons().into_iter().next().ok_or("always tray empty")?;
    let (x, y) = dock.point(&tray_button);
    pointer_move(x, y);
    settle(0.1);
    ensure!(!panel.apps_expanded(), "Hovering the separate tray must not expand Default apps");
    panel.apps_toggle().emit_clicked();
    ensure!(panel.apps_expanded());
    panel.set_apps_expanded(false);
    ensure!(always.widget().is_mapped());
    dock.set_config(|c| c.always_visible_apps = DEFAULT_APPS.iter().map(|(n, _)| n.to_string()).collect());
    dock.refresh_groups();
    ensure!(!panel.apps_toggle().get_visible());
    let expected: BTreeSet<String> = DEFAULT_APPS.iter()
        .filter(|(name, _)| panel.status_visible(name).unwrap_or(false)).map(|(n, _)| n.to_string()).collect();
    ensure!(visible_apps(&panel) == expected);
    app.status_set("battery", "visible", json!(false));
    panel.refresh();
    ensure!(!panel.button("battery").unwrap().get_visible(), "Always visible does not invent unavailable hardware");
    Ok(())
}

fn default_apps_recording_menu_holds_expansion_until_closed() -> TestResult {
    let app = default_apps_setup();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| c.mode = "bar".into());
    let panel = dock.panel();
    panel.set_apps_expanded(true);
    panel.button("screen_record").unwrap().emit_clicked();
    let menu = panel.record_menu().ok_or("record menu missing")?;
    ensure!(dock.context_menu() == Some(menu.clone()));
    // Heading row prepended: icon, title and the settings gear.
    let rows = menu.child().and_downcast::<gtk::Box>().unwrap();
    let heading = rows.first_child().and_downcast::<gtk::Box>().ok_or("heading")?;
    ensure!(children(&heading).len() == 3);
    pointer_move(500.0, 300.0);
    panel.schedule_apps_collapse();
    settle(0.45);
    ensure!(panel.apps_expanded());
    ensure!(menu.is_mapped());
    menu.popdown();
    settle(0.5);
    ensure!(!panel.apps_expanded());
    ensure!(panel.record_menu().is_none());
    Ok(())
}

// ------------------------------------------------------------------ indicators: clicks, record presets, capture actions

fn indicators_clicks_offsets_and_record_presets() -> TestResult {
    let app = FakeApp::new();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "top".into(); });
    let panel = dock.panel();
    let thickness = dock.window.height();
    let offset = 40.max(thickness + 8);
    panel.set_apps_expanded(true);
    settle(0.1);
    // Left = popup, right = settings (bluetooth has no right-click handler,
    // idle_inhibitor right-click toggles, capslock does nothing).
    for (name, mouse, expected) in [
        ("power", "left", format!("panel-open power settings=false edge=top offset={offset}")),
        ("power", "right", format!("panel-open power settings=true edge=top offset={offset}")),
        ("idle_inhibitor", "right", "panel-action idle_inhibitor".to_string()),
        ("idle_inhibitor", "left", format!("panel-open idle_inhibitor settings=false edge=top offset={offset}")),
        ("network", "left", format!("panel-open network settings=false edge=top offset={offset}")),
    ] {
        let button = panel.button(name).unwrap();
        ensure!(button.is_mapped(), "{name}");
        let (x, y) = dock.point(&button);
        pointer_click(x, y, mouse);
        ensure!(app.last() == Some(expected.clone()), "{name} {mouse}: {:?}", app.last());
    }
    let before = app.history.borrow().len();
    let (x, y) = dock.point(&panel.button("bluetooth").unwrap());
    pointer_click(x, y, "right");
    ensure!(app.history.borrow().len() == before, "bluetooth right-click: {:?}", app.last());
    let (x, y) = dock.point(&panel.clock());
    pointer_click(x, y, "right");
    ensure!(app.last() == Some(format!("panel-open calendar settings=true edge=top offset={offset}")));
    panel.clock().emit_clicked();
    ensure!(app.last() == Some(format!("panel-open calendar settings=false edge=top offset={offset}")));
    panel.logout().emit_clicked();
    ensure!(app.last() == Some(format!("panel-open session settings=false edge=top offset={offset}")));
    // Audio scroll: down = -1, up = +1.
    let audio = panel.button("audio").unwrap();
    let (x, y) = dock.point(&audio);
    pointer_move(x, y);
    wlrctl(&["pointer", "scroll", "10", "0"]);
    settle(0.1);
    ensure!(app.last().as_deref() == Some("volume -1"), "{:?}", app.last());
    wlrctl(&["pointer", "scroll", "-10", "0"]);
    settle(0.1);
    ensure!(app.last().as_deref() == Some("volume 1"), "{:?}", app.last());
    // Screen record: idle → preset menu; preset row → capture preset.
    panel.button("screen_record").unwrap().emit_clicked();
    let menu = panel.record_menu().ok_or("record menu")?;
    let rows = children(&menu.child().unwrap());
    let labels: Vec<String> = rows.iter().filter_map(|w| w.downcast_ref::<gtk::Button>().and_then(|b| b.label()).map(|l| l.to_string())).collect();
    ensure!(labels == ["With no audio", "With desktop audio", "With desktop + microphone audio"], "{labels:?}");
    rows[2].downcast_ref::<gtk::Button>().unwrap().emit_clicked();
    ensure!(app.last().as_deref() == Some("capture desktop"));
    settle(0.1);
    // Gear → settings.
    panel.button("screen_record").unwrap().emit_clicked();
    let menu = panel.record_menu().unwrap();
    let heading = menu.child().unwrap().first_child().unwrap();
    heading.last_child().and_downcast::<gtk::Button>().unwrap().emit_clicked();
    ensure!(app.last().as_deref() == Some("capture settings"));
    settle(0.1);
    // Recording → stop; stopping → nothing; a refresh while recording pops the menu down.
    panel.button("screen_record").unwrap().emit_clicked();
    let menu = panel.record_menu().unwrap();
    app.status_set("screen_record", "state", json!("recording"));
    panel.refresh();
    ensure!(!menu.get_visible());
    panel.button("screen_record").unwrap().emit_clicked();
    ensure!(app.last().as_deref() == Some("capture stop"));
    app.status_set("screen_record", "state", json!("stopping"));
    let before = app.history.borrow().len();
    panel.button("screen_record").unwrap().emit_clicked();
    ensure!(app.history.borrow().len() == before);
    // Right click on screen record opens the recorder settings via panel_open.
    panel.button("screen_record").unwrap();
    Ok(())
}

fn capture_controls_prepare_and_finish() -> TestResult {
    let app = FakeApp::new();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| c.mode = "bar".into());
    let group = gio::SimpleActionGroup::new();
    let source = app.clone();
    register_capture_controls(&group, Rc::new(move || source.docks()));
    let panel = dock.panel();
    panel.set_apps_expanded(true);
    settle(0.1);
    panel.button("screen_record").unwrap().emit_clicked();
    let menu = panel.record_menu().ok_or("menu")?;
    settle(0.1);
    ensure!(menu.is_mapped() && menu.is_autohide());
    group.activate_action("prepare-capture", Some(&false.to_variant()));
    settle(0.1);
    ensure!(menu.is_mapped(), "a screenshot keeps the menu open");
    ensure!(!menu.is_autohide());
    ensure!(panel.capture_preserved_menu() == Some(menu.clone()));
    group.activate_action("finish-screenshot", None);
    settle(0.1);
    ensure!(menu.is_autohide() && menu.is_mapped());
    ensure!(panel.capture_preserved_menu().is_none());
    group.activate_action("prepare-capture", Some(&true.to_variant()));
    settle(0.1);
    ensure!(!menu.get_visible(), "recording pops the preset menu down");
    ensure!(panel.record_menu().is_none());
    Ok(())
}

// ------------------------------------------------------------------ bar activities: workspaces + clock

fn workspaces_and_clock() -> TestResult {
    let app = FakeApp::new();
    std::fs::write(&app.clock_path, r#"{"version": 1, "stacked": true, "date_first": true, "show_weekday": true}"#).unwrap();
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "top".into(); });
    let panel = dock.panel();
    ensure!(panel.workspaces_box().parent() == panel.bar_start().map(|b| b.upcast()), "bar mode: workspaces in bar_start");
    ensure!(panel.weather().parent() == panel.bar_start().map(|b| b.upcast()));
    let buttons = children(&panel.workspaces_box());
    let labels: Vec<String> = buttons.iter().map(|w| w.downcast_ref::<gtk::Button>().unwrap().label().unwrap().to_string()).collect();
    ensure!(labels == ["1", "2", "3", "4"], "{labels:?}");
    let first = panel.workspace_button("1").unwrap();
    ensure!(first.has_css_class("active") && first.has_css_class("waydock-workspace"));
    ensure!(first.tooltip_text().as_deref() == Some("Workspace 1"));
    ensure!(waydock::ui::host::accessible_name_of(&first).as_deref() == Some("Workspace 1 · Current"));
    let (x, y) = dock.point(&panel.workspace_button("3").unwrap());
    pointer_click(x, y, "left");
    ensure!(app.last().as_deref() == Some("workspace 3"));
    // Reorder, rename, remove, urgent, insensitive, error tooltip.
    app.workspaces.replace(vec![
        Workspace { id: "3".into(), name: "".into(), active: true, urgent: true, can_activate: false, ..Default::default() },
        Workspace { id: "1".into(), name: "Web".into(), ..Default::default() },
        Workspace { id: "2".into(), name: "x".into(), hidden: true, ..Default::default() },
    ]);
    app.workspace_error.replace("workspace helper failed".into());
    panel.refresh_workspaces();
    let labels: Vec<String> = children(&panel.workspaces_box()).iter()
        .map(|w| w.downcast_ref::<gtk::Button>().unwrap().label().unwrap().to_string()).collect();
    ensure!(labels == ["1", "Web"], "{labels:?}");
    let third = panel.workspace_button("3").unwrap();
    ensure!(third.has_css_class("urgent") && third.has_css_class("active") && !third.is_sensitive());
    ensure!(!panel.workspace_button("1").unwrap().has_css_class("active"));
    ensure!(panel.workspace_button("2").is_none() && panel.workspace_button("4").is_none());
    ensure!(panel.workspaces_box().tooltip_text().as_deref() == Some("workspace helper failed"));
    ensure!(panel.group_availability()["workspaces"]);
    app.workspaces.replace(Vec::new());
    panel.refresh_workspaces();
    ensure!(!panel.group_availability()["workspaces"]);
    // Clock: stacked, date first, weekday shown, tooltip format.
    ensure!(panel.clock_box().orientation() == gtk::Orientation::Vertical);
    ensure!(panel.clock_box().first_child() == Some(panel.date_label().upcast()));
    ensure!(panel.weekday_label().get_visible() && !panel.weekday_label().text().is_empty());
    ensure!(!panel.clock().has_css_class("clock-inline"));
    ensure!(!panel.time_label().text().is_empty());
    let tooltip = panel.clock().tooltip_text().unwrap();
    ensure!(tooltip.ends_with("\nClick for calendar · Right-click for date/time settings"), "{tooltip}");
    ensure!(panel.time_label().has_css_class("waydock-clock-time") && panel.date_label().has_css_class("waydock-clock-date"));
    std::fs::write(&app.clock_path, r#"{"version": 1, "stacked": false, "show_date": false}"#).unwrap();
    panel.clock_tick();
    ensure!(panel.clock_box().orientation() == gtk::Orientation::Horizontal);
    ensure!(panel.clock().has_css_class("clock-inline"));
    ensure!(!panel.date_label().get_visible() && !panel.weekday_label().get_visible());
    ensure!(panel.clock_box().first_child() == Some(panel.time_label().upcast()));
    // Dock mode: workspaces/weather live in the indicators box.
    let other = make_dock(&app, 1, "HEADLESS-2", |_| {});
    ensure!(other.panel().bar_start().is_none());
    ensure!(other.panel().workspaces_box().parent() == Some(other.panel().widget().upcast()));
    Ok(())
}

fn status_visuals_and_vpn_badge() -> TestResult {
    let app = FakeApp::new();
    app.status_set("network", "text", json!("󰤨 \u{f023}"));
    app.status_set("network", "classes", json!(["wifi", "vpn"]));
    app.status_set("weather", "text", json!("☀ 72°"));
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| c.icon_size = 48);
    let panel = dock.panel();
    let network = panel.status_button("network").unwrap();
    ensure!(network.label.text() == "󰤨", "first glyph only: {}", network.label.text());
    ensure!(network.vpn_badge.as_ref().unwrap().get_visible(), "classes {:?} status {:?}", network.panel_classes(), app.status.borrow().get("network"));
    let theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());
    if theme.has_icon("network-wireless-signal-excellent-symbolic") {
        ensure!(network.icon.icon_name().as_deref() == Some("network-wireless-signal-excellent-symbolic"));
        ensure!(network.icon.pixel_size() == 24 && network.icon.get_visible() && !network.label.get_visible());
    }
    let weather = panel.status_button("weather").unwrap();
    ensure!(weather.label.get_visible() && !weather.icon.get_visible() && weather.label.text() == "☀ 72°");
    app.status_set("network", "classes", json!(["wifi"]));
    panel.refresh();
    ensure!(!network.vpn_badge.as_ref().unwrap().get_visible());
    // capslock: hidden when its text is empty (no explicit visible).
    app.status.borrow_mut().insert("capslock".into(), json!({"text": ""}));
    panel.refresh();
    ensure!(panel.status_visible("capslock") == Some(false));
    ensure!(!panel.button("capslock").unwrap().get_visible());
    // The weather group availability follows its status.
    app.status_set("weather", "visible", json!(false));
    panel.refresh();
    ensure!(!panel.group_availability()["weather"] && !panel.weather().get_visible());
    Ok(())
}

// ------------------------------------------------------------------ test_tray_coordinates (UI part)

const ITEM_XML: &str = r#"<node><interface name="org.kde.StatusNotifierItem">
<property name="Id" type="s" access="read"/>
<property name="Title" type="s" access="read"/>
<property name="Status" type="s" access="read"/>
<property name="IconName" type="s" access="read"/>
<property name="Menu" type="o" access="read"/>
<property name="ItemIsMenu" type="b" access="read"/>
<method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
</interface></node>"#;

struct SniService {
    connection: gio::DBusConnection,
    properties: Rc<RefCell<HashMap<String, Variant>>>,
    calls: Rc<RefCell<Vec<(String, Variant)>>>,
}

impl SniService {
    fn new() -> Self {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
        let connection = gio::DBusConnection::for_address_sync(&address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None::<&gio::DBusAuthObserver>, None::<&gio::Cancellable>).unwrap();
        connection.set_exit_on_close(false);
        let properties = Rc::new(RefCell::new(HashMap::from([
            ("Id".to_owned(), "test-app".to_variant()), ("Title".to_owned(), "Test application".to_variant()),
            ("Status".to_owned(), "Active".to_variant()), ("IconName".to_owned(), "network-wired".to_variant()),
            ("Menu".to_owned(), glib::variant::ObjectPath::try_from("/").unwrap().to_variant()),
            ("ItemIsMenu".to_owned(), false.to_variant())])));
        let calls: Rc<RefCell<Vec<(String, Variant)>>> = Rc::default();
        let info = gio::DBusNodeInfo::for_xml(ITEM_XML).unwrap().lookup_interface(ITEM).unwrap();
        let (recorded, props) = (calls.clone(), properties.clone());
        connection.register_object("/test/Tray", &info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                recorded.borrow_mut().push((method.to_owned(), params));
                invocation.return_value(Some(&().to_variant()));
            })
            .property(move |_, _, _, _, name| props.borrow()[name].clone())
            .build().unwrap();
        Self { connection, properties, calls }
    }
    fn key(&self) -> String { format!("{}/test/Tray", self.connection.unique_name().unwrap()) }
    fn register(&self) {
        let done = Rc::new(Cell::new(false));
        let flag = done.clone();
        self.connection.call(Some(WATCHER), WATCHER_PATH, WATCHER, "RegisterStatusNotifierItem", Some(&("/test/Tray",).to_variant()),
            None, gio::DBusCallFlags::NO_AUTO_START, 3000, None::<&gio::Cancellable>, move |r| { r.unwrap(); flag.set(true); });
        assert!(wait_for(5.0, || done.get()));
    }
}

fn coordinates_setup() -> Result<(Rc<FakeApp>, SniService, StatusTrayBackend), String> {
    let app = FakeApp::new();
    let backend = StatusTrayBackend::new(None, None);
    backend.start();
    ensure!(wait_for(5.0, || backend.is_watcher()), "watcher: {}", backend.error());
    let service = SniService::new();
    service.register();
    let key = service.key();
    ensure!(wait_for(5.0, || backend.item(&key).is_some_and(|i| i.ready())), "item never ready");
    let host: Rc<dyn TrayBackend> = Rc::new(backend.clone());
    app.tray.replace(Some(host));
    pointer_move(100.0, 100.0);
    Ok((app, service, backend))
}

fn displayed_button(dock: &FakeDock, service: &SniService, always: bool) -> Option<gtk::Button> {
    let panel = dock.panel();
    if !always {
        panel.tray_expander().unwrap().set_expanded(true);
    }
    settle(0.12);
    let tray = if always { panel.always_tray() } else { panel.tray() };
    tray.unwrap().button(&service.key())
}

fn assert_action_at_pointer(dock: &FakeDock, service: &SniService, button: &gtk::Button, mouse: &str, method: &str) -> TestResult {
    if !button.is_mapped() {
        dock.panel().tray_expander().unwrap().set_expanded(true);
        settle(0.12);
    }
    ensure!(button.is_mapped());
    let (x, y) = dock.point(button);
    let area = dock.monitor.geometry();
    ensure!(area.x() as f64 <= x && x < (area.x() + area.width()) as f64 && area.y() as f64 <= y && y < (area.y() + area.height()) as f64,
            "button outside its output: {x},{y}");
    service.calls.borrow_mut().clear();
    pointer_click(x, y, mouse);
    ensure!(wait_for(3.0, || !service.calls.borrow().is_empty()), "No tray method received ({mouse})");
    let calls = service.calls.borrow().clone();
    ensure!(calls.len() == 1, "{calls:?}");
    let (actual, point) = &calls[0];
    ensure!(actual == method, "{actual} != {method}");
    let (px, py) = point.get::<(i32, i32)>().unwrap();
    ensure!((px - x as i32).abs() <= 1 && (py - y as i32).abs() <= 1, "({px}, {py}) vs pointer ({x}, {y})");
    Ok(())
}

fn coordinates_bar_actions_on_all_edges_and_both_trays() -> TestResult {
    let (app, service, backend) = coordinates_setup()?;
    for edge in ["top", "bottom", "left", "right"] {
        for always in [false, true] {
            let dock = make_dock(&app, 0, "HEADLESS-1", |c| {
                c.mode = "bar".into(); c.edge = edge.into();
                c.always_show_tray = if always { vec!["id:test-app".into()] } else { Vec::new() };
            });
            let button = displayed_button(&dock, &service, always).ok_or("tray button missing")?;
            for (mouse, method) in [("right", "ContextMenu"), ("left", "Activate"), ("middle", "SecondaryActivate")] {
                assert_action_at_pointer(&dock, &service, &button, mouse, method).map_err(|e| format!("{edge} {always}: {e}"))?;
            }
            dock.destroy(&app);
        }
    }
    backend.close();
    Ok(())
}

fn coordinates_second_monitor_and_menu_releases_catchers() -> TestResult {
    let (app, service, backend) = coordinates_setup()?;
    let area = monitor(1).geometry();
    ensure!(area.x() != 0, "second output origin");
    for edge in ["bottom", "right"] {
        let dock = make_dock(&app, 1, "HEADLESS-2", |c| {
            c.edge = edge.into(); c.float_mode = true; c.gap = 17; c.always_show_tray = vec!["id:test-app".into()];
        });
        let button = displayed_button(&dock, &service, true).ok_or("always button missing")?;
        assert_action_at_pointer(&dock, &service, &button, "right", "ContextMenu")?;
        dock.destroy(&app);
    }
    // An application-drawn menu releases the regular tray's outside-click catchers.
    for always in [false, true] {
        let dock = make_dock(&app, 0, "HEADLESS-1", |c| {
            c.mode = "bar".into(); c.edge = "top".into();
            c.always_show_tray = if always { vec!["id:test-app".into()] } else { Vec::new() };
        });
        let button = displayed_button(&dock, &service, always).ok_or("button")?;
        let expander = dock.panel().tray_expander().unwrap();
        // An always-visible icon's menu must also release catchers of an
        // already-expanded regular tray (which needs an item of its own).
        let extra = always.then(|| {
            let extra = SniService::new();
            extra.properties.borrow_mut().insert("Id".into(), "other-app".to_variant());
            extra.register();
            extra
        });
        if let Some(extra) = &extra {
            let key = extra.key();
            ensure!(wait_for(5.0, || backend.item(&key).is_some_and(|i| i.ready())));
            settle(0.1);
            expander.set_expanded(true);
            settle(0.12);
        }
        ensure!(expander.expanded() && !expander.backdrops().is_empty(), "expanded with catchers ({always})");
        let backdrops = expander.backdrops();
        let (receiver_window, clicks) = receiver(&dock.monitor);
        assert_action_at_pointer(&dock, &service, &button, "right", "ContextMenu")?;
        pointer_click(40.0, 200.0, "left");
        ensure!(clicks.get() == 1, "The first outside click must reach the window below");
        ensure!(!expander.expanded());
        ensure!(expander.backdrops().is_empty());
        ensure!(backdrops.iter().all(|w| !w.is_mapped()));
        ensure!(!expander.menu_is_active());
        receiver_window.destroy();
        dock.destroy(&app);
        if let Some(extra) = extra {
            let _ = extra.connection.close_sync(None::<&gio::Cancellable>);
            settle(0.2);
        }
    }
    backend.close();
    Ok(())
}

fn coordinates_scaled_output_and_menu_only_left_click() -> TestResult {
    let (app, service, backend) = coordinates_setup()?;
    let second = monitor(1);
    let width = second.geometry().width();
    let connector = second.connector().unwrap().to_string();
    let status = Command::new("wlr-randr").args(["--output", &connector, "--scale", "1.5"]).status().map_err(|e| e.to_string())?;
    ensure!(status.success());
    ensure!(wait_for(5.0, || second.geometry().width() < width), "scale not applied");
    service.properties.borrow_mut().insert("ItemIsMenu".into(), true.to_variant());
    let item = backend.item(&service.key()).unwrap();
    backend.refresh_item(&item);
    ensure!(wait_for(5.0, || backend.item(&service.key()).is_some_and(|i| i.boolean("ItemIsMenu"))));
    let dock = make_dock(&app, 1, "HEADLESS-2", |c| {
        c.mode = "bar".into(); c.edge = "top".into(); c.bar_float_mode = true; c.gap = 13; c.always_show_tray = vec!["id:test-app".into()];
    });
    let button = displayed_button(&dock, &service, true).ok_or("button")?;
    assert_action_at_pointer(&dock, &service, &button, "left", "ContextMenu")?;
    dock.destroy(&app);
    let _ = Command::new("wlr-randr").args(["--output", &connector, "--scale", "1"]).status();
    settle(0.3);
    backend.close();
    Ok(())
}


const MENU_XML: &str = r#"<node><interface name="com.canonical.dbusmenu">
<method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/>
<arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
<method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
<method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/>
<arg type="v" direction="in"/><arg type="u" direction="in"/></method>
</interface></node>"#;

fn menu_row(id: i32, label: &str) -> Variant {
    let props = glib::VariantDict::new(None);
    props.insert_value("label", &label.to_variant());
    props.insert_value("shortcut", &vec![vec!["Control".to_owned(), "q".to_owned()]].to_variant());
    Variant::tuple_from_iter([id.to_variant(), props.end(),
                              Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, Vec::<Variant>::new())])
}

/// Serve `/Menu` (one "Test action" row with <Control>q) on the item's connection.
fn serve_menu(service: &SniService) -> Rc<RefCell<Vec<String>>> {
    let events: Rc<RefCell<Vec<String>>> = Rc::default();
    let info = gio::DBusNodeInfo::for_xml(MENU_XML).unwrap().lookup_interface("com.canonical.dbusmenu").unwrap();
    let seen = events.clone();
    service.connection.register_object("/Menu", &info)
        .method_call(move |_, _, _, _, method, params, invocation| {
            if method == "GetLayout" {
                let root = Variant::tuple_from_iter([0i32.to_variant(), glib::VariantDict::new(None).end(),
                    Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, [Variant::from_variant(&menu_row(1, "Test action"))])]);
                invocation.return_value(Some(&Variant::tuple_from_iter([1u32.to_variant(), root])));
                return;
            }
            seen.borrow_mut().push(format!("{method} {}", params.print(false)));
            if method == "AboutToShow" {
                invocation.return_value(Some(&(false,).to_variant()));
            } else {
                invocation.return_value(Some(&().to_variant()));
            }
        })
        .build().unwrap();
    service.properties.borrow_mut().insert("Menu".into(), glib::variant::ObjectPath::try_from("/Menu").unwrap().to_variant());
    events
}

fn find_label(root: &gtk::Widget, text: &str) -> Option<gtk::Widget> {
    let mut child = root.first_child();
    while let Some(widget) = child {
        if widget.downcast_ref::<gtk::Label>().is_some_and(|label| label.text() == text) {
            return Some(widget);
        }
        if let Some(found) = find_label(&widget, text) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn dbusmenu_popover_from_right_click_and_item_click() -> TestResult {
    let (app, service, backend) = coordinates_setup()?;
    let events = serve_menu(&service);
    let item = backend.item(&service.key()).unwrap();
    backend.refresh_item(&item);
    ensure!(wait_for(5.0, || backend.item(&service.key()).and_then(|i| i.menu_path()).is_some()), "Menu property");
    let dock = make_dock(&app, 0, "HEADLESS-1", |c| { c.mode = "bar".into(); c.edge = "top".into(); });
    let button = displayed_button(&dock, &service, false).ok_or("button")?;
    let tray = dock.panel().tray().unwrap();
    let expander = dock.panel().tray_expander().unwrap();
    ensure!(expander.expanded());
    let menu = tray.menu(&service.key()).ok_or("menu not prepared on refresh")?;
    let popover = menu.popover().ok_or("no popover")?;
    ensure!(popover.has_css_class("waydock-context-menu") && !popover.has_arrow());
    ensure!(popover.parent() == Some(button.clone().upcast()));
    ensure!(wait_for(3.0, || popover.downcast_ref::<gtk::PopoverMenu>().unwrap().menu_model().is_some_and(|m| m.n_items() > 0)));
    service.calls.borrow_mut().clear();
    let (x, y) = dock.point(&button);
    pointer_click(x, y, "right");
    ensure!(wait_for(2.0, || popover.is_mapped()), "DBusMenu popover opens on right click");
    ensure!(service.calls.borrow().is_empty(), "no ContextMenu when the item has a DBusMenu: {:?}", service.calls.borrow());
    ensure!(wait_for(2.0, || events.borrow().iter().any(|e| e == "Event (0, 'opened', <0>, 0)")), "{:?}", events.borrow());
    ensure!(events.borrow().iter().any(|e| e == "AboutToShow (0,)"));
    ensure!(tray.menu_open() && expander.expanded() && expander.backdrops().is_empty() && expander.menu_is_active());
    ensure!(dock.holds.borrow().contains(&"tray-menu".to_string()));
    let row = find_label(popover.upcast_ref(), "Test action").ok_or("menu row")?;
    let local = translate(&row, &popover, 0.0, 0.0).unwrap();
    let (px, py) = popup_position(&popover).ok_or("popup position")?;
    let area = dock.monitor.geometry();
    let (ox, oy) = dock.origin();
    pointer_click(area.x() as f64 + ox + px + local.0 + row.width() as f64 / 2.0,
                  area.y() as f64 + oy + py + local.1 + row.height() as f64 / 2.0, "left");
    ensure!(wait_for(2.0, || events.borrow().iter().any(|e| e == "Event (1, 'clicked', <0>, 0)")), "{:?}", events.borrow());
    ensure!(wait_for(1.0, || !popover.get_visible()));
    settle(0.1);
    ensure!(!tray.menu_open());
    ensure!(!expander.expanded(), "closing the native menu retracts the tray");
    ensure!(!dock.holds.borrow().contains(&"tray-menu".to_string()));
    dock.destroy(&app);
    backend.close();
    Ok(())
}

// ------------------------------------------------------------------ runner

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if std::env::var("WAYDOCK_F2_HEADLESS").as_deref() != Ok("1") || runtime == "/run/user/1000" || display.is_empty() {
        println!("f2_ui_headless: skipped (run tests/run_f2_headless.sh)");
        return;
    }
    gtk::init().expect("gtk init in the private compositor");
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
    let tests: Vec<(&str, fn() -> TestResult)> = vec![
        ("tray_refresh_static_metadata_reuses_paintables_and_bounds_memory", tray_refresh_static_metadata_reuses_paintables_and_bounds_memory),
        ("tray_refresh_icon_signal_and_theme_change_reload_only_when_needed", tray_refresh_icon_signal_and_theme_change_reload_only_when_needed),
        ("tray_refresh_private_themes_are_bounded_by_current_items", tray_refresh_private_themes_are_bounded_by_current_items),
        ("always_tray_routing_all_layouts_and_return_to_normal_tray", always_tray_routing_all_layouts_and_return_to_normal_tray),
        ("always_tray_late_pixmap_item_fits_before_always_tray", always_tray_late_pixmap_item_fits_before_always_tray),
        ("always_tray_native_tooltips", always_tray_native_tooltips),
        ("always_tray_restarted_application_keeps_placement_on_every_clone", always_tray_restarted_application_keeps_placement_on_every_clone),
        ("expander_application_outside_dismissal_and_tray_mutual_exclusion", expander_application_outside_dismissal_and_tray_mutual_exclusion),
        ("expander_application_availability_and_native_menu_lifecycle", expander_application_availability_and_native_menu_lifecycle),
        ("expander_preview_hole_keeps_layer_surface_clickable", expander_preview_hole_keeps_layer_surface_clickable),
        ("expander_toggle_and_outside_click_on_each_monitor", expander_toggle_and_outside_click_on_each_monitor),
        ("expander_cloned_docks_remain_clickable_and_holes_follow_geometry", expander_cloned_docks_remain_clickable_and_holes_follow_geometry),
        ("expander_inline_expansion_on_every_edge", expander_inline_expansion_on_every_edge),
        ("expander_native_tray_popover_stays_clickable_and_dismissal_retracts", expander_native_tray_popover_stays_clickable_and_dismissal_retracts),
        ("expander_hidden_empty_and_destroyed_release_input_surfaces", expander_hidden_empty_and_destroyed_release_input_surfaces),
        ("expander_catchers_never_cover_fullscreen", expander_catchers_never_cover_fullscreen),
        ("media_inline_controls_and_popup_button", media_inline_controls_and_popup_button),
        ("media_idle_bar_hides_nothing_playing_and_disabled_controls", media_idle_bar_hides_nothing_playing_and_disabled_controls),
        ("media_missing_player_icons_fall_back_to_music_button", media_missing_player_icons_fall_back_to_music_button),
        ("media_titles_and_controls_fit_all_edges", media_titles_and_controls_fit_all_edges),
        ("default_apps_hover_expands_and_leaving_collapses", default_apps_hover_expands_and_leaving_collapses),
        ("default_apps_empty_and_all_selections_keep_reveal_and_tray", default_apps_empty_and_all_selections_keep_reveal_and_tray),
        ("default_apps_recording_menu_holds_expansion_until_closed", default_apps_recording_menu_holds_expansion_until_closed),
        ("indicators_clicks_offsets_and_record_presets", indicators_clicks_offsets_and_record_presets),
        ("capture_controls_prepare_and_finish", capture_controls_prepare_and_finish),
        ("workspaces_and_clock", workspaces_and_clock),
        ("status_visuals_and_vpn_badge", status_visuals_and_vpn_badge),
        ("coordinates_bar_actions_on_all_edges_and_both_trays", coordinates_bar_actions_on_all_edges_and_both_trays),
        ("coordinates_second_monitor_and_menu_releases_catchers", coordinates_second_monitor_and_menu_releases_catchers),
        ("dbusmenu_popover_from_right_click_and_item_click", dbusmenu_popover_from_right_click_and_item_click),
        ("coordinates_scaled_output_and_menu_only_left_click", coordinates_scaled_output_and_menu_only_left_click),
    ];
    let filter = std::env::args().nth(1).unwrap_or_default();
    let (mut passed, mut failed) = (0, Vec::new());
    for (name, test) in tests {
        if !filter.is_empty() && !name.contains(&filter) {
            continue;
        }
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test))
            .unwrap_or_else(|panic| Err(format!("panic: {:?}", panic.downcast_ref::<String>().cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())))));
        for app in APPS.with(|apps| apps.take()) {
            app.destroy_all();
            if let Some(tray) = app.tray.take() {
                drop(tray);
            }
        }
        settle(0.05);
        match result {
            Ok(()) => { passed += 1; println!("ok   {name} ({:.1}s)", started.elapsed().as_secs_f64()); }
            Err(error) => { println!("FAIL {name}: {error}"); failed.push(name); }
        }
    }
    println!("f2_ui_headless: {passed} passed, {} failed {:?}", failed.len(), failed);
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
