#![allow(deprecated)]
//! W-f part 2 widgets in a real compositor: the application menu (search,
//! categories, Frequent, pin stars, grid keyboard, outside-click/Escape
//! dismissal, drag-to-pin), window previews (exact-window activation, close,
//! mute) and dock drops. Ports test_menu_categories_ui.py and the menu /
//! preview / pin / drop cases of test_dock_ui.py and test_utility_ui.py.
//!
//! Runs only inside the private headless labwc of tests/run_f1_headless.sh
//! (WAYDOCK_F1_HEADLESS=1, private XDG_RUNTIME_DIR, private bus with no
//! service directories); anywhere else it skips without connecting. Every
//! host, backend, launch and audio source is fake.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, LayerShell};
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::process::{Command, Stdio};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use tessel_dock::core::app_catalog::{AppCatalog, AppInfoData, LaunchContext, LaunchHistory, Launcher};
use tessel_dock::core::model::{DockItem, DockModel, WindowData};
use tessel_dock::core::spawn::FakeSpawner;
use tessel_dock::ui::app_audio::AudioControl;
use tessel_dock::ui::app_menu::drag::{self, DropHost};
use tessel_dock::ui::app_menu::logic::{DropValue, MenuMetrics, PopupPlacement};
use tessel_dock::ui::app_menu::popup::{children, translate, PopupAnchor, PopupHost};
use tessel_dock::ui::app_menu::{self, desktop_id, AppMenu, MenuHost};
use tessel_dock::ui::preview::{PreviewHost, WindowActions, WindowPreview};
use tessel_dock::wl::Window;

// ---- harness -----------------------------------------------------------------

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

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => { if !$cond { return Err(format!("line {}: {}", line!(), format!($($msg)+))); } };
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![widget.clone()];
    for child in children(widget) {
        result.extend(descendants(&child));
    }
    result
}

fn find<T: IsA<gtk::Widget>>(widget: &impl IsA<gtk::Widget>) -> Vec<T> {
    descendants(widget.upcast_ref()).into_iter().filter_map(|w| w.downcast::<T>().ok()).collect()
}

fn buttons(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Button> {
    find::<gtk::Button>(widget)
}

fn key_press(widget: &impl IsA<gtk::Widget>, key: gdk::Key) -> bool {
    let controllers = widget.observe_controllers();
    for index in 0..controllers.n_items() {
        if let Some(controller) = controllers.item(index).and_downcast::<gtk::EventControllerKey>() {
            let handled: bool = controller.emit_by_name("key-pressed", &[&key.into_glib(), &0u32, &gdk::ModifierType::empty()]);
            if handled {
                return true;
            }
        }
    }
    false
}

use glib::translate::IntoGlib;

fn has_controller<T: IsA<gtk::EventController> + IsA<glib::Object>>(widget: &impl IsA<gtk::Widget>) -> bool {
    let controllers = widget.observe_controllers();
    (0..controllers.n_items()).any(|i| controllers.item(i).and_downcast::<T>().is_some())
}

fn ids(listing: &gtk::FlowBox) -> Vec<Option<String>> {
    children(listing).iter().map(desktop_id).collect()
}

fn app_ids(listing: &gtk::FlowBox) -> Vec<String> {
    ids(listing).into_iter().flatten().collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn wlrctl(args: &[&str]) {
    let _ = Command::new("wlrctl").args(args).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn pointer_to(x: f64, y: f64) {
    wlrctl(&["pointer", "move", "-100000", "-100000"]);
    wlrctl(&["pointer", "move", &(x as i64).to_string(), &(y as i64).to_string()]);
    settle(0.06);
}

/// Top-left of a layer surface in the global layout (Python click_widget).
fn surface_origin(monitor: &gdk::Monitor, window: &gtk::Window) -> (f64, f64) {
    let geometry = monitor.geometry();
    let (width, height) = (window.width() as f64, window.height() as f64);
    let mut x = geometry.x() as f64 + (geometry.width() as f64 - width) / 2.0;
    let mut y = geometry.y() as f64 + (geometry.height() as f64 - height) / 2.0;
    if window.is_anchor(Edge::Left) {
        x = geometry.x() as f64 + window.margin(Edge::Left) as f64;
    } else if window.is_anchor(Edge::Right) {
        x = geometry.x() as f64 + geometry.width() as f64 - width - window.margin(Edge::Right) as f64;
    }
    if window.is_anchor(Edge::Top) {
        y = geometry.y() as f64 + window.margin(Edge::Top) as f64;
    } else if window.is_anchor(Edge::Bottom) {
        y = geometry.y() as f64 + geometry.height() as f64 - height - window.margin(Edge::Bottom) as f64;
    }
    (x, y)
}

fn click_widget(monitor: &gdk::Monitor, window: &gtk::Window, widget: &impl IsA<gtk::Widget>) {
    let (x, y) = surface_origin(monitor, window);
    let (wx, wy) = translate(widget, window, 0.0, 0.0).unwrap_or((0.0, 0.0));
    pointer_to(x + wx + widget.width() as f64 / 2.0, y + wy + widget.height() as f64 / 2.0);
    wlrctl(&["pointer", "click"]);
    settle(0.15);
}

fn send_keys(keys: &[&str]) {
    let mut args = vec!["-s".to_owned(), "100".to_owned()];
    for key in keys {
        args.extend(["-k".to_owned(), key.to_string(), "-s".to_owned(), "80".to_owned()]);
    }
    if let Ok(mut child) = Command::new("wtype").args(&args).spawn() {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            settle(0.01);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    settle(0.1);
}

// ---- fake application + dock surface ----------------------------------------

#[derive(Default)]
struct FakeAudio {
    muted: Cell<bool>,
    closed: Cell<bool>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
}

impl AudioControl for FakeAudio {
    fn start(&self) {}
    fn state(&self, _app_id: &str) -> (bool, bool) {
        (true, self.muted.get())
    }
    fn toggle(&self, _app_id: &str) -> bool {
        self.muted.set(!self.muted.get());
        let changed = self.changed.borrow().clone();
        if let Some(changed) = changed {
            changed();
        }
        true
    }
    fn close(&self) {
        self.closed.set(true);
    }
}

struct Fake {
    monitor: gdk::Monitor,
    edge: RefCell<String>,
    catalog: Rc<RefCell<AppCatalog>>,
    pins: RefCell<Vec<String>>,
    history: RefCell<Vec<(String, String)>>,
    holds: RefCell<Vec<String>>,
    launch_history: RefCell<Option<Rc<RefCell<LaunchHistory>>>>,
    launcher: RefCell<Option<(Launcher, Rc<FakeSpawner>)>>,
    windows: RefCell<IndexMap<String, WindowData>>,
    refuse_close: Cell<bool>,
    model: RefCell<DockModel<WindowData>>,
    audio: RefCell<Option<Rc<FakeAudio>>>,
    dock: gtk::Window,
    pinned_box: gtk::Box,
    menu: RefCell<Option<Rc<AppMenu>>>,
    preview: RefCell<Option<Rc<WindowPreview<WindowData>>>>,
}

fn info(id: &str, name: &str, categories: &str, wm_class: &str) -> AppInfoData {
    let mut data = AppInfoData::fake(id, name);
    data.categories = Some(categories.to_owned());
    data.startup_wm_class = Some(wm_class.to_owned()).filter(|s| !s.is_empty());
    data.icon = Some(gio::ThemedIcon::new("application-x-executable").upcast());
    data
}

fn base_catalog() -> Vec<AppInfoData> {
    vec![
        info("browser.desktop", "Web Browser", "Network;", "BrowserClass"),
        info("files.desktop", "Files", "System;", ""),
        info("terminal.desktop", "Terminal", "System;", ""),
    ]
}

fn category_catalog() -> Vec<AppInfoData> {
    let mut hidden = info("hidden.desktop", "Hidden Education Tool", "Education;", "");
    hidden.should_show = false;
    vec![
        info("browser.desktop", "Web Browser", "Network;WebBrowser;", ""),
        info("mail.desktop", "Mail", "Network;Email;", ""),
        info("chess.desktop", "Chess", "Game;BoardGame;", ""),
        info("game.desktop", "Clair Obscur", "Game;", ""),
        info("obs.desktop", "OBS Studio", "AudioVideo;Video;", ""),
        info("editor.desktop", "Code Editor", "Development;", ""),
        info("writer.desktop", "Writer", "Office;WordProcessor;", ""),
        info("calculator.desktop", "Calculator", "Utility;Calculator;", ""),
        info("settings.desktop", "Display Settings", "Settings;", ""),
        info("paint.desktop", "Drawing", "Graphics;", ""),
        info("monitor.desktop", "System Monitor", "System;Monitor;", ""),
        info("other.desktop", "Uncategorized Application", "", ""),
        hidden,
    ]
}

fn windows() -> IndexMap<String, WindowData> {
    let mut first = WindowData::new("browser-first", "browser", "Project documentation");
    first.activated = true;
    let second = WindowData::new("browser-second", "BrowserClass", "Mail inbox");
    let files = WindowData::new("files-first", "files", "Downloads");
    [first, second, files].into_iter().map(|w| (w.id.clone(), w)).collect()
}

impl Fake {
    fn new(monitor: &gdk::Monitor, infos: Vec<AppInfoData>) -> Rc<Fake> {
        // A minimal bottom dock: the pinned strip receives drops and anchors previews.
        let dock = tessel_dock::ui::app_menu::popup::surface(None, monitor, "f1-test-dock", gtk4_layer_shell::Layer::Bottom, false);
        dock.set_anchor(Edge::Bottom, true);
        dock.set_margin(Edge::Bottom, 8);
        let pinned_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        dock.set_child(Some(&pinned_box));
        let fake = Rc::new(Fake {
            monitor: monitor.clone(),
            edge: RefCell::new("bottom".into()),
            catalog: Rc::new(RefCell::new(AppCatalog::from_infos(infos))),
            pins: RefCell::new(vec!["browser.desktop".into(), "terminal.desktop".into()]),
            history: RefCell::new(Vec::new()),
            holds: RefCell::new(Vec::new()),
            launch_history: RefCell::new(None),
            launcher: RefCell::new(None),
            windows: RefCell::new(windows()),
            refuse_close: Cell::new(false),
            model: RefCell::new(DockModel::new()),
            audio: RefCell::new(None),
            dock,
            pinned_box,
            menu: RefCell::new(None),
            preview: RefCell::new(None),
        });
        let host: Rc<dyn MenuHost> = fake.clone();
        *fake.menu.borrow_mut() = Some(AppMenu::new(Rc::downgrade(&host)));
        let host: Rc<dyn PreviewHost<WindowData>> = fake.clone();
        *fake.preview.borrow_mut() = Some(WindowPreview::new(Rc::downgrade(&host)));
        fake.render();
        fake.dock.set_visible(true);
        settle(0.1);
        fake
    }

    fn menu(&self) -> Rc<AppMenu> {
        self.menu.borrow().clone().unwrap()
    }
    fn preview(&self) -> Rc<WindowPreview<WindowData>> {
        self.preview.borrow().clone().unwrap()
    }
    fn record(&self, what: &str, id: &str) {
        self.history.borrow_mut().push((what.into(), id.into()));
    }
    fn last(&self) -> Option<(String, String)> {
        self.history.borrow().last().cloned()
    }

    fn items(&self) -> Vec<DockItem<WindowData>> {
        let windows: Vec<WindowData> = self.windows.borrow().values().cloned().collect();
        let pins = self.pins.borrow().clone();
        let catalog = self.catalog.borrow();
        self.model.borrow_mut().update(&pins, &windows, &*catalog).to_vec()
    }

    fn item(&self, id: &str) -> DockItem<WindowData> {
        self.items().into_iter().find(|i| i.id == id).expect("item")
    }

    /// `app.refresh()`: rebuild the pinned strip and update an open preview.
    fn render(self: &Rc<Self>) {
        for child in children(&self.pinned_box) {
            self.pinned_box.remove(&child);
        }
        let items = self.items();
        for item in items.iter().filter(|i| i.pinned) {
            let button = gtk::Button::with_label(&item.name);
            button.set_widget_name(&item.desktop_id());
            let (weak, id) = (Rc::downgrade(self), item.desktop_id());
            button.connect_clicked(move |_| {
                if let Some(fake) = weak.upgrade() {
                    fake.record("dock-click", &id);
                }
            });
            drag::install_button_drag_source(&button, |w| w.widget_name().to_string(), || {}, || {});
            self.pinned_box.append(&button);
        }
        if let Some(id) = self.preview().preview_id() {
            if let Some(item) = items.into_iter().find(|i| i.id == id) {
                self.preview().update(&item);
            }
        }
    }

    fn window_of(data: &WindowData) -> Window {
        Window { id: data.id.clone(), app_id: data.app_id.clone(), title: data.title.clone(), activated: data.activated,
                 minimized: data.minimized, ..Default::default() }
    }
}

struct Actions(Weak<Fake>);

impl WindowActions for Actions {
    fn window(&self, id: &str) -> Option<Window> {
        self.0.upgrade()?.windows.borrow().get(id).map(Fake::window_of)
    }
    fn activate(&self, id: &str) -> bool {
        let Some(fake) = self.0.upgrade() else { return false };
        if !fake.windows.borrow().contains_key(id) {
            return false;
        }
        fake.record("activate", id);
        true
    }
    fn minimize(&self, id: &str) -> bool {
        let Some(fake) = self.0.upgrade() else { return false };
        if !fake.windows.borrow().contains_key(id) {
            return false;
        }
        fake.record("minimize", id);
        true
    }
    fn close_window(&self, id: &str) -> bool {
        let Some(fake) = self.0.upgrade() else { return false };
        if fake.refuse_close.get() {
            fake.record("close-refused", id);
            return false;
        }
        if fake.windows.borrow_mut().shift_remove(id).is_none() {
            return false;
        }
        fake.record("close", id);
        fake.render();
        true
    }
}

impl PopupHost for Fake {
    fn application(&self) -> Option<gtk::Application> {
        None
    }
    fn monitor(&self) -> gdk::Monitor {
        self.monitor.clone()
    }
    fn edge(&self) -> String {
        self.edge.borrow().clone()
    }
    fn popup_class(&self) -> String {
        "tessel-dock-palette-f1".into()
    }
    fn popup_placement(&self, anchor: &PopupAnchor) -> PopupPlacement {
        let geometry = self.monitor.geometry();
        let horizontal = PopupHost::horizontal(self);
        let anchor = match anchor {
            PopupAnchor::Menu => None,
            PopupAnchor::Item(id) => children(&self.pinned_box)
                .into_iter()
                .find(|b| b.widget_name() == id.as_str())
                .and_then(|b| translate(&b, &self.dock, 0.0, 0.0).map(|p| (p.0, b.width() as f64))),
        };
        PopupPlacement {
            edge: self.edge(),
            edge_gap: 8,
            thickness: if horizontal { self.dock.height() } else { 64 },
            headroom: 0,
            total: if horizontal { geometry.width() } else { geometry.height() },
            dock_length: if horizontal { self.dock.width() } else { 600 },
            dock_alignment: "center".into(),
            app_dock_origin: None,
            anchor: if horizontal { anchor } else { None },
            alignment: "center".into(),
            minimum: 8.0,
        }
    }
    fn set_popup_blur(&self, _popup: &gtk::Window, menu: bool) {
        self.holds.borrow_mut().push(format!("blur:{menu}"));
    }
    fn enter(&self, zone: &str) {
        self.holds.borrow_mut().push(format!("enter:{zone}"));
    }
    fn leave(&self, zone: &str) {
        self.holds.borrow_mut().push(format!("leave:{zone}"));
    }
    fn destroyed(&self) -> bool {
        false
    }
    fn catalog(&self) -> Rc<RefCell<AppCatalog>> {
        self.catalog.clone()
    }
}

impl MenuHost for Fake {
    fn pins(&self) -> Vec<String> {
        self.pins.borrow().clone()
    }
    fn launch_history(&self) -> Option<Rc<RefCell<LaunchHistory>>> {
        self.launch_history.borrow().clone()
    }
    fn launch(&self, desktop_id: &str, context: Rc<dyn LaunchContext>) {
        self.record("launch", desktop_id);
        if let Some((launcher, _)) = &*self.launcher.borrow() {
            let errors = Rc::new(RefCell::new(Vec::<String>::new()));
            let sink = errors.clone();
            app_menu::launch_app(&self.catalog.borrow(), launcher, None, desktop_id, Some(context),
                                 Rc::new(move |e: &str| sink.borrow_mut().push(e.to_owned())));
        }
    }
    fn toggle_pin(&self, desktop_id: &str) {
        self.record("pin", desktop_id);
        let mut pins = self.pins.borrow_mut();
        if let Some(index) = pins.iter().position(|p| p == desktop_id) {
            pins.remove(index);
        } else {
            pins.push(desktop_id.into());
        }
    }
    fn show_settings(&self) {
        self.record("settings", "");
    }
    fn before_toggle(&self) {}
    fn close_preview(&self) {
        self.preview().close();
    }
    fn menu_metrics(&self) -> MenuMetrics {
        let geometry = self.monitor.geometry();
        MenuMetrics { monitor_width: geometry.width(), monitor_height: geometry.height(), reserved: 0,
                      horizontal: PopupHost::horizontal(self), dock_height: 56, edge_gap: 8, headroom: 0, dock_window_width: 64 }
    }
}

impl PreviewHost<WindowData> for Fake {
    fn tray_expanded(&self) -> bool {
        false
    }
    fn menu_open(&self) -> bool {
        self.menu().is_open()
    }
    fn find_item(&self, item_id: &str) -> Option<DockItem<WindowData>> {
        self.items().into_iter().find(|i| i.id == item_id)
    }
    fn actions(&self) -> Rc<dyn WindowActions> {
        Rc::new(Actions(SELF.with(|s| s.borrow().clone())))
    }
    fn show_error(&self, text: &str) {
        self.record("error", text);
    }
    fn clear_focused_title(&self) {}
    fn collapse_application_expander(&self) {}
    fn icon_size(&self) -> i32 {
        48
    }
    fn create_audio(&self, changed: Rc<dyn Fn()>) -> Rc<dyn AudioControl> {
        let audio = Rc::new(FakeAudio::default());
        *audio.changed.borrow_mut() = Some(changed);
        *self.audio.borrow_mut() = Some(audio.clone());
        audio
    }
}

impl DropHost for Fake {
    fn catalog(&self) -> Rc<RefCell<AppCatalog>> {
        self.catalog.clone()
    }
    fn panel(&self) -> gtk::Widget {
        self.pinned_box.clone().upcast()
    }
    fn pinned_children(&self) -> Vec<(gtk::Widget, Option<String>)> {
        children(&self.pinned_box).into_iter().map(|b| { let id = b.widget_name().to_string(); (b, Some(id)) }).collect()
    }
    fn horizontal(&self) -> bool {
        true
    }
    fn place_pin(&self, desktop_id: &str, before: Option<&str>) {
        self.record("place-pin", &format!("{desktop_id}|{}", before.unwrap_or("None")));
    }
    fn enter(&self, zone: &str) {
        PopupHost::enter(self, zone)
    }
    fn leave(&self, zone: &str) {
        PopupHost::leave(self, zone)
    }
}

thread_local! {
    static SELF: RefCell<Weak<Fake>> = RefCell::new(Weak::new());
}

fn fake(monitor: &gdk::Monitor, infos: Vec<AppInfoData>) -> Rc<Fake> {
    let fake = Fake::new(monitor, infos);
    SELF.with(|s| *s.borrow_mut() = Rc::downgrade(&fake));
    fake
}

impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(menu) = self.menu.borrow().clone() {
            menu.close();
        }
        if let Some(preview) = self.preview.borrow().clone() {
            preview.close();
        }
        self.dock.destroy();
    }
}

fn open(fake: &Rc<Fake>) -> Result<app_menu::MenuWidgets, String> {
    fake.menu().toggle();
    settle(0.12);
    fake.menu().widgets().ok_or_else(|| "menu did not open".to_owned())
}

fn search_for(widgets: &app_menu::MenuWidgets, text: &str) {
    widgets.search.set_text(text);
    widgets.search.emit_by_name::<()>("search-changed", &[]);
    settle(0.06);
}

// ---- application menu -----------------------------------------------------------

/// test_app_search_pin_unpin_and_launch_use_desktop_identity
fn search_pin_unpin_and_launch(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let w = open(&fake)?;
    ensure!(fake.menu().is_open(), "menu not open");
    ensure!(app_ids(&w.listing) == ["files.desktop", "terminal.desktop", "browser.desktop"], "{:?}", ids(&w.listing));
    // Results read from the top even with search at the bottom edge.
    ensure!(w.listing.valign() == gtk::Align::Start, "valign");
    ensure!(children(&w.content).last() == Some(w.search.upcast_ref()), "search must be last on the bottom edge");
    key_press(&w.search, gdk::Key::Up);
    ensure!(w.listing.selected_children().first().and_then(desktop_id).as_deref() == Some("files.desktop"), "Up selects the first result");
    search_for(&w, "Files");
    let rows: Vec<gtk::Widget> = children(&w.listing).into_iter().filter(|r| desktop_id(r).is_some()).collect();
    ensure!(rows.iter().map(|r| desktop_id(r).unwrap()).collect::<Vec<_>>() == ["files.desktop"], "{:?}", ids(&w.listing));
    ensure!(w.section.text() == "Search results", "section {}", w.section.text());
    ensure!(w.count.text() == "1 app", "count {}", w.count.text());
    buttons(&rows[0])[0].emit_clicked();
    ensure!(fake.pins.borrow().contains(&"files.desktop".to_owned()), "pinned");
    let first = children(&w.listing)[0].clone();
    ensure!(buttons(&first)[0].has_css_class("pinned"), "star shows pinned");
    ensure!(buttons(&first)[0].tooltip_text().as_deref() == Some("Unpin application"), "tooltip");
    buttons(&first)[0].emit_clicked();
    ensure!(!fake.pins.borrow().contains(&"files.desktop".to_owned()), "unpinned");
    ensure!(fake.menu().is_open(), "pinning keeps the menu open");
    let first = children(&w.listing)[0].clone().downcast::<gtk::FlowBoxChild>().unwrap();
    w.listing.emit_by_name::<()>("child-activated", &[&first]);
    ensure!(fake.last() == Some(("launch".into(), "files.desktop".into())), "{:?}", fake.last());
    ensure!(!fake.menu().is_open(), "launch closes the menu");
    let w = open(&fake)?;
    w.search.emit_by_name::<()>("activate", &[]);
    ensure!(fake.last() == Some(("launch".into(), "files.desktop".into())), "{:?}", fake.last());
    ensure!(!fake.menu().is_open(), "activate closes the menu");
    // The settings button and the hover holds.
    let w = open(&fake)?;
    w.settings.emit_clicked();
    ensure!(fake.last() == Some(("settings".into(), "".into())), "settings");
    ensure!(w.popup.has_css_class("tessel-dock-palette-f1-applications"), "menu class");
    fake.menu().close();
    let holds = fake.holds.borrow().clone();
    ensure!(holds.iter().filter(|h| *h == "enter:menu").count() == holds.iter().filter(|h| *h == "leave:menu").count(), "{holds:?}");
    ensure!(holds.contains(&"blur:true".to_owned()), "menu blur");
    Ok(())
}

/// test_obs_search_ranks_results_bottom_up_and_launches_with_native_gtk4_context
fn ranked_search_launches_with_gdk_context(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, vec![
        info("com.obsproject.Studio.desktop", "OBS Studio", "AudioVideo;", ""),
        info("game.desktop", "Clair Obscur Expedition 33", "Game;", ""),
        info("a-metadata-obs.desktop", "A metadata match", "", ""),
        info("b-metadata-obs.desktop", "B metadata match", "", ""),
        info("word.desktop", "Zebra OBS", "", ""),
        info("substring.desktop", "Gobservation", "", ""),
        info("prefix.desktop", "OBS Tools", "", ""),
    ]);
    let spawner = Rc::new(FakeSpawner::new());
    let mut launcher = Launcher::new(spawner.clone());
    launcher.helper = vec!["/fixture/tessel-dock-launch".into()];
    *fake.launcher.borrow_mut() = Some((launcher, spawner.clone()));
    let w = open(&fake)?;
    let mut order = Vec::new();
    for query in ["obs", "OBS", "oBs"] {
        search_for(&w, query);
        let rows: Vec<gtk::Widget> = children(&w.listing).into_iter().filter(|r| desktop_id(r).is_some()).collect();
        let mut reading: Vec<(i32, i32, String)> = rows.iter().map(|r| {
            let a = r.allocation();
            (a.y(), a.x(), desktop_id(r).unwrap())
        }).collect();
        reading.sort();
        order = reading.into_iter().map(|r| r.2).collect::<Vec<_>>();
        let expected: Vec<String> = fake.catalog.borrow().search(query).into_iter().map(|e| e.id).collect();
        ensure!(order == expected, "{query}: {order:?} vs {expected:?}");
        ensure!(order[0] == "com.obsproject.Studio.desktop", "{order:?}");
    }
    let first = children(&w.listing).into_iter().find(|r| desktop_id(r).as_deref() == Some(order[0].as_str())).unwrap();
    w.listing.emit_by_name::<()>("child-activated", &[&first.downcast::<gtk::FlowBoxChild>().unwrap()]);
    ensure!(!fake.menu().is_open(), "closed");
    let argv = spawner.last_argv();
    ensure!(argv[argv.len() - 2..] == ["--launch-desktop", "com.obsproject.Studio.desktop"], "{argv:?}");
    ensure!(argv.contains(&"--property=ExitType=cgroup".to_owned()) && argv.contains(&"--slice=app.slice".to_owned()), "{argv:?}");
    let environment = spawner.last_environment().unwrap_or_default();
    let get = |k: &str| environment.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    ensure!(get("WAYLAND_DISPLAY") == std::env::var("WAYLAND_DISPLAY").ok(), "WAYLAND_DISPLAY {:?}", get("WAYLAND_DISPLAY"));
    ensure!(get("INVOCATION_ID").is_none(), "service variables are stripped");
    Ok(())
}

/// test_app_grid_search_placement_spacing_order_and_keyboard_on_every_edge
fn grid_placement_order_and_keyboard(monitor: &gdk::Monitor) -> Result<(), String> {
    let mut infos = base_catalog();
    for number in 0..4 {
        let mut fixture = info(&format!("fixture-{number}.desktop"), &format!("Grid fixture {number} — {}", "Long application name ".repeat(18)), "", "");
        fixture.description = Some("A full application description".into());
        infos.push(fixture);
    }
    let fake = fake(monitor, infos);
    let ordered: Vec<String> = fake.catalog.borrow().menu_apps().into_iter().map(|e| e.id).collect();
    for edge in ["bottom", "top", "left", "right"] {
        *fake.edge.borrow_mut() = edge.into();
        let w = open(&fake)?;
        let popup = w.popup.clone();
        let search_at_bottom = edge == "bottom";
        let search_y = translate(&w.search, &popup, 0.0, 0.0).unwrap().1;
        let results_y = translate(&w.listing, &popup, 0.0, 0.0).unwrap().1;
        ensure!((search_y > results_y) == search_at_bottom, "{edge}: search placement");
        ensure!(w.listing.valign() == gtk::Align::Start, "{edge}");
        let adjustment = w.scroll.vadjustment();
        ensure!(adjustment.value() == adjustment.lower(), "{edge}: opens scrolled to the start");
        let cells = children(&w.listing);
        let mut expected: Vec<Option<String>> = ordered.iter().cloned().map(Some).collect();
        expected.extend([None, None]);
        ensure!(ids(&w.listing) == expected, "{edge}: {:?}", ids(&w.listing));
        ensure!(w.listing.min_children_per_line() == 3 && w.listing.max_children_per_line() == 3, "{edge}: columns");
        ensure!(w.listing.is_homogeneous(), "homogeneous");
        let widths: Vec<i32> = cells.iter().map(|c| c.width()).collect();
        ensure!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1, "{edge}: widths {widths:?}");
        let xs: std::collections::BTreeSet<i32> = cells.iter().map(|c| c.allocation().x()).collect();
        let ys: std::collections::BTreeSet<i32> = cells.iter().map(|c| c.allocation().y()).collect();
        ensure!(xs.len() == 3 && ys.len() == 3, "{edge}: grid {xs:?} {ys:?}");
        ensure!(popup.width() <= 1200, "{edge}: width {}", popup.width());
        // The menu is anchored on the dock edge with the side margin of position_popup.
        let anchored = match edge { "top" => Edge::Top, "left" => Edge::Left, "right" => Edge::Right, _ => Edge::Bottom };
        ensure!(popup.is_anchor(anchored) && popup.margin(anchored) == 8 + if matches!(edge, "top" | "bottom") { fake.dock.height() } else { 64 } + 7,
                "{edge}: edge margin {}", popup.margin(anchored));
        for cell in &cells {
            if desktop_id(cell).is_none() {
                ensure!(!cell.is_sensitive() && !cell.can_focus() && cell.has_css_class("tessel-dock-empty-cell"), "padding cell");
                let before = fake.history.borrow().len();
                w.listing.emit_by_name::<()>("child-activated", &[cell.downcast_ref::<gtk::FlowBoxChild>().unwrap()]);
                ensure!(fake.history.borrow().len() == before && fake.menu().is_open(), "padding cells are inert");
                continue;
            }
            let name = find::<gtk::Label>(cell)[0].clone();
            let pin = buttons(cell)[0].clone();
            let (name_x, _) = translate(&name, cell, 0.0, 0.0).unwrap();
            let (pin_x, _) = translate(&pin, cell, 0.0, 0.0).unwrap();
            ensure!(name_x + name.width() as f64 <= pin_x, "{edge}: name overlaps pin");
            ensure!(pin_x + pin.width() as f64 <= cell.width() as f64, "{edge}: pin outside tile");
            let id = desktop_id(cell).unwrap();
            let entry_name = fake.catalog.borrow().get(&id).unwrap().name.clone();
            ensure!(cell.tooltip_text().is_some_and(|t| t.contains(&entry_name)), "tooltip");
        }
        let before = fake.history.borrow().len();
        search_for(&w, "No matching fixture exists");
        ensure!(children(&w.listing).is_empty(), "empty results");
        ensure!(w.results.visible_child_name().as_deref() == Some("empty"), "empty page");
        ensure!(find::<gtk::Label>(&popup).iter().any(|l| l.text() == "No applications found"), "empty text");
        key_press(&w.search, gdk::Key::Up);
        w.search.emit_by_name::<()>("activate", &[]);
        ensure!(fake.history.borrow().len() == before && fake.menu().is_open(), "no launch without results");
        search_for(&w, "Grid fixture");
        let mut grid: Vec<Option<String>> = (0..4).map(|n| Some(format!("fixture-{n}.desktop"))).collect();
        grid.extend([None, None]);
        ensure!(ids(&w.listing) == grid, "{edge}: {:?}", ids(&w.listing));
        let (toward_results, toward_search) = if search_at_bottom { (gdk::Key::Up, gdk::Key::Down) } else { (gdk::Key::Down, gdk::Key::Up) };
        key_press(&w.search, toward_results);
        let selected = w.listing.selected_children()[0].clone();
        ensure!(desktop_id(&selected).as_deref() == Some("fixture-0.desktop"), "first selected");
        ensure!(GtkWindowExt::focus(&popup).as_ref() == Some(selected.upcast_ref()), "{edge}: focus on the tile");
        let mut selected: gtk::Widget = selected.upcast();
        if search_at_bottom {
            // The first row is at the top; moving past the last row returns to search.
            key_press(&selected, gdk::Key::Down);
            selected = w.listing.selected_children()[0].clone().upcast();
            ensure!(desktop_id(&selected).as_deref() == Some("fixture-3.desktop"), "down to the next row");
        }
        key_press(&selected, toward_search);
        let focus = GtkWindowExt::focus(&popup);
        ensure!(focus.as_ref().is_some_and(|f| f == w.search.upcast_ref::<gtk::Widget>() || f.is_ancestor(&w.search)), "{edge}: back to search");
        key_press(&w.search, toward_results);
        w.search.emit_by_name::<()>("activate", &[]);
        let expected = if search_at_bottom { "fixture-3.desktop" } else { "fixture-0.desktop" };
        ensure!(fake.last() == Some(("launch".into(), expected.into())), "{edge}: {:?}", fake.last());
        ensure!(!fake.menu().is_open(), "closed");
    }
    Ok(())
}

/// test_app_menu_size_follows_monitor_and_frequent_row (Frequent part)
fn frequent_row(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let dir = std::env::temp_dir().join(format!("f1-history-{}", std::process::id()));
    let mut history = LaunchHistory::new(Some(dir.join("launch-counts.json")));
    for id in ["files.desktop", "terminal.desktop", "files.desktop", "missing.desktop"] {
        history.record(id);
    }
    *fake.launch_history.borrow_mut() = Some(Rc::new(RefCell::new(history)));
    let w = open(&fake)?;
    let tiles: Vec<String> = children(&w.frequent_list).iter().filter_map(desktop_id).collect();
    ensure!(tiles == ["files.desktop", "terminal.desktop"], "{tiles:?}");
    ensure!(children(&w.frequent_list).len() == w.columns as usize, "spacers keep one column per tile");
    ensure!(w.frequent.is_visible(), "frequent visible");
    let tile = children(&w.frequent_list)[0].clone();
    ensure!(tile.has_css_class("tessel-dock-app-frequent-tile") && has_controller::<gtk::DragSource>(&tile), "frequent tile");
    search_for(&w, "Web");
    ensure!(!w.frequent.is_visible(), "Search results replace the Frequent row");
    let row = children(&w.listing).into_iter().find(|r| desktop_id(r).is_some()).unwrap();
    ensure!(has_controller::<gtk::DragSource>(&row), "tiles are drag sources");
    search_for(&w, "");
    let tile = children(&w.frequent_list)[1].clone().downcast::<gtk::Button>().unwrap();
    tile.emit_clicked();
    ensure!(fake.last() == Some(("launch".into(), "terminal.desktop".into())) && !fake.menu().is_open(), "frequent launch");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn category(w: &app_menu::MenuWidgets, id: Option<&str>) -> gtk::ListBoxRow {
    children(&w.categories)
        .into_iter()
        .find(|r| app_menu::category_id(r) == Some(id.map(str::to_owned)))
        .and_then(|r| r.downcast::<gtk::ListBoxRow>().ok())
        .expect("category row")
}

fn selected_category(w: &app_menu::MenuWidgets) -> Option<Option<String>> {
    w.categories.selected_row().and_then(|r| app_menu::category_id(&r))
}

/// test_native_category_click_pin_launch_and_reopen
fn category_click_pin_launch_and_reopen(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, category_catalog());
    *fake.edge.borrow_mut() = "top".into();
    let w = open(&fake)?;
    ensure!(selected_category(&w) == Some(None), "All Applications selected");
    let listed: Vec<Option<String>> = children(&w.categories).iter().filter_map(app_menu::category_id).collect();
    ensure!(!listed.contains(&Some("Education".into())), "hidden-only categories must not appear");
    ensure!(listed[0].is_none() && listed.contains(&Some("Other".into())) && listed.last() == Some(&Some("Other".into())), "{listed:?}");
    click_widget(monitor, &w.popup, &category(&w, Some("Network")));
    ensure!(selected_category(&w) == Some(Some("Network".into())), "{:?}", selected_category(&w));
    ensure!(sorted(app_ids(&w.listing)) == ["browser.desktop", "mail.desktop"], "{:?}", app_ids(&w.listing));
    ensure!(w.section.text() == "Internet", "section {}", w.section.text());
    let mail = children(&w.listing).into_iter().find(|r| desktop_id(r).as_deref() == Some("mail.desktop")).unwrap();
    click_widget(monitor, &w.popup, &buttons(&mail)[0]);
    ensure!(fake.last() == Some(("pin".into(), "mail.desktop".into())), "{:?}", fake.history.borrow());
    ensure!(fake.menu().window().as_ref() == Some(&w.popup), "pin keeps the menu");
    ensure!(selected_category(&w) == Some(Some("Network".into())), "category kept");
    ensure!(sorted(app_ids(&w.listing)) == ["browser.desktop", "mail.desktop"], "listing kept");
    let mail = children(&w.listing).into_iter().find(|r| desktop_id(r).as_deref() == Some("mail.desktop")).unwrap();
    let label = find::<gtk::Label>(&mail)[0].clone();
    click_widget(monitor, &w.popup, &label);
    ensure!(fake.last() == Some(("launch".into(), "mail.desktop".into())), "{:?}", fake.history.borrow());
    ensure!(!fake.menu().is_open(), "launch closes");
    let w = open(&fake)?;
    ensure!(selected_category(&w) == Some(None), "reopen resets the category");
    let all: Vec<String> = fake.catalog.borrow().menu_apps().into_iter().map(|e| e.id).collect();
    ensure!(app_ids(&w.listing) == all, "all apps");
    Ok(())
}

/// test_search_is_global_and_restores_browsing_category
fn search_is_global_and_restores_category(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, category_catalog());
    let w = open(&fake)?;
    w.categories.emit_by_name::<()>("row-activated", &[&category(&w, Some("Game"))]);
    settle(0.06);
    ensure!(sorted(app_ids(&w.listing)) == ["chess.desktop", "game.desktop"], "{:?}", app_ids(&w.listing));
    search_for(&w, "oBs");
    ensure!(w.categories.selected_row().is_none(), "A global search must not imply a category filter");
    ensure!(sorted(app_ids(&w.listing)) == ["game.desktop", "obs.desktop"], "{:?}", app_ids(&w.listing));
    search_for(&w, "This fixture does not exist");
    ensure!(app_ids(&w.listing).is_empty(), "no rows");
    let before = fake.history.borrow().len();
    w.search.emit_by_name::<()>("activate", &[]);
    ensure!(fake.history.borrow().len() == before && fake.menu().is_open(), "activate without results");
    ensure!(find::<gtk::Label>(&w.popup).iter().any(|l| l.text() == "No applications found"), "empty text");
    search_for(&w, "");
    ensure!(selected_category(&w) == Some(Some("Game".into())), "category restored");
    ensure!(sorted(app_ids(&w.listing)) == ["chess.desktop", "game.desktop"], "{:?}", app_ids(&w.listing));
    search_for(&w, "OBS Studio");
    click_widget(monitor, &w.popup, &category(&w, Some("Network")));
    ensure!(w.search.text().is_empty(), "choosing a category clears the search");
    ensure!(selected_category(&w) == Some(Some("Network".into())), "network");
    ensure!(sorted(app_ids(&w.listing)) == ["browser.desktop", "mail.desktop"], "{:?}", app_ids(&w.listing));
    click_widget(monitor, &w.popup, &category(&w, None));
    let all: Vec<String> = fake.catalog.borrow().menu_apps().into_iter().map(|e| e.id).collect();
    ensure!(app_ids(&w.listing) == all, "all again");
    Ok(())
}

/// test_search_placement_three_columns_and_width_on_every_edge
fn three_columns_and_reading_order_on_every_edge(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, category_catalog());
    let all: Vec<String> = fake.catalog.borrow().menu_apps().into_iter().map(|e| e.id).collect();
    for edge in ["top", "bottom", "left", "right"] {
        *fake.edge.borrow_mut() = edge.into();
        let w = open(&fake)?;
        let search_y = translate(&w.search, &w.popup, 0.0, 0.0).unwrap().1;
        let listing_y = translate(&w.listing, &w.popup, 0.0, 0.0).unwrap().1;
        ensure!((search_y > listing_y) == (edge == "bottom"), "{edge}");
        ensure!(w.listing.min_children_per_line() == 3 && w.listing.max_children_per_line() == 3, "{edge}");
        let rows: Vec<gtk::Widget> = children(&w.listing).into_iter().filter(|r| desktop_id(r).is_some()).collect();
        let xs: std::collections::BTreeSet<i32> = rows.iter().map(|r| r.allocation().x()).collect();
        ensure!(xs.len() == 3, "{edge}: {xs:?}");
        ensure!(w.popup.width() <= 1200, "{edge}: {}", w.popup.width());
        ensure!(translate(&w.categories, &w.popup, 0.0, 0.0).unwrap().0 < translate(&w.listing, &w.popup, 0.0, 0.0).unwrap().0, "sidebar first");
        // Top-to-bottom reading order on every edge: the menu opens on A.
        let mut reading: Vec<(i32, i32, String)> = rows.iter().map(|r| (r.allocation().y(), r.allocation().x(), desktop_id(r).unwrap())).collect();
        reading.sort();
        ensure!(reading.into_iter().map(|r| r.2).collect::<Vec<_>>() == all, "{edge}: reading order");
        fake.menu().close();
    }
    Ok(())
}

/// test_native_category_keyboard_filters_grid
fn category_keyboard_filters_grid(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, category_catalog());
    *fake.edge.borrow_mut() = "top".into();
    let w = open(&fake)?;
    click_widget(monitor, &w.popup, &category(&w, Some("Network")));
    send_keys(&["Down"]);
    ensure!(selected_category(&w) == Some(Some("AudioVideo".into())), "{:?}", selected_category(&w));
    ensure!(app_ids(&w.listing) == ["obs.desktop"], "{:?}", app_ids(&w.listing));
    Ok(())
}

/// test_native_app_menu_dismisses_outside_on_both_monitors_and_escape
fn native_dismissal_pin_and_keyboard(monitor: &gdk::Monitor, monitors: &[gdk::Monitor]) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let assert_closed = |backdrops: &[gtk::Window]| -> Result<(), String> {
        ensure!(!fake.menu().is_open(), "menu still open");
        ensure!(fake.menu().backdrops().is_empty(), "backdrops kept");
        ensure!(backdrops.iter().all(|b| !b.is_mapped()), "backdrop still mapped");
        Ok(())
    };
    // A pin click must not also activate its enclosing FlowBox tile.
    let w = open(&fake)?;
    ensure!(gtk4_layer_shell::LayerShell::keyboard_mode(&w.popup) == gtk4_layer_shell::KeyboardMode::Exclusive, "exclusive keyboard");
    ensure!(gtk4_layer_shell::LayerShell::namespace(&w.popup).as_deref() == Some("tessel-dock-applications"), "namespace");
    let files = children(&w.listing).into_iter().find(|c| desktop_id(c).as_deref() == Some("files.desktop")).unwrap();
    let before = fake.history.borrow().len();
    click_widget(monitor, &w.popup, &buttons(&files)[0]);
    ensure!(fake.menu().window().as_ref() == Some(&w.popup), "Pinning must keep the application menu open");
    ensure!(fake.history.borrow()[before..] == [("pin".to_owned(), "files.desktop".to_owned())], "{:?}", &fake.history.borrow()[before..]);
    let files = children(&w.listing).into_iter().find(|c| desktop_id(c).as_deref() == Some("files.desktop")).unwrap();
    let icon = find::<gtk::Image>(&files)[0].clone();
    let backdrops = fake.menu().backdrops();
    ensure!(backdrops.len() == monitors.len(), "one backdrop per monitor");
    ensure!(backdrops.iter().all(|b| gtk4_layer_shell::LayerShell::namespace(b).as_deref() == Some("tessel-dock-menu-dismiss")), "backdrop namespace");
    let before = fake.history.borrow().len();
    click_widget(monitor, &w.popup, &icon);
    ensure!(fake.history.borrow()[before..] == [("launch".to_owned(), "files.desktop".to_owned())], "{:?}", &fake.history.borrow()[before..]);
    assert_closed(&backdrops)?;

    // Keyboard: Up from search selects the first tile, Right moves, Return launches.
    let w = open(&fake)?;
    let before = fake.history.borrow().len();
    send_keys(&["Up"]);
    let selected: Vec<String> = w.listing.selected_children().iter().filter_map(desktop_id).collect();
    ensure!(selected == ["files.desktop"], "{selected:?}");
    send_keys(&["Right"]);
    let selected: Vec<String> = w.listing.selected_children().iter().filter_map(desktop_id).collect();
    ensure!(selected == ["terminal.desktop"], "after Right {selected:?} focus {:?}", GtkWindowExt::focus(&w.popup));
    let backdrops = fake.menu().backdrops();
    send_keys(&["Return"]);
    ensure!(fake.history.borrow()[before..] == [("launch".to_owned(), "terminal.desktop".to_owned())], "{:?}", &fake.history.borrow()[before..]);
    assert_closed(&backdrops)?;

    for target in monitors {
        let w = open(&fake)?;
        let backdrops = fake.menu().backdrops();
        click_widget(monitor, &w.popup, &w.search);
        ensure!(fake.menu().window().as_ref() == Some(&w.popup), "Clicking inside the search field must keep the menu open");
        let geometry = target.geometry();
        pointer_to(geometry.x() as f64 + 10.0, geometry.y() as f64 + 10.0);
        wlrctl(&["pointer", "click"]);
        settle(0.16);
        assert_closed(&backdrops)?;
        let button = children(&fake.pinned_box)[0].clone();
        let before = fake.history.borrow().len();
        click_widget(monitor, &fake.dock, &button);
        ensure!(fake.history.borrow()[before..] == [("dock-click".to_owned(), "browser.desktop".to_owned())],
                "Dismissed backdrops must not swallow subsequent dock clicks: {:?}", &fake.history.borrow()[before..]);
    }
    // No click inside: the opening must give the search keyboard focus, so Escape dismisses.
    let _w = open(&fake)?;
    let backdrops = fake.menu().backdrops();
    send_keys(&["Escape"]);
    settle(0.16);
    assert_closed(&backdrops)?;
    Ok(())
}

/// Drag start lets the dock receive the drop; drag end closes the menu.
fn drag_hides_backdrops_and_drop_pins(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let _w = open(&fake)?;
    let backdrops = fake.menu().backdrops();
    fake.menu().drag_began();
    ensure!(fake.menu().dragging() && backdrops.iter().all(|b| !b.is_visible()), "backdrops hidden while dragging");
    ensure!(fake.holds.borrow().contains(&"enter:drag".to_owned()), "drag hold");
    fake.menu().drag_ended();
    ensure!(fake.menu().is_open(), "closes only after idle");
    settle(0.05);
    ensure!(!fake.menu().is_open() && fake.holds.borrow().contains(&"leave:drag".to_owned()), "closed after drag end");

    // test_drops_pin_and_reorder_relative_to_pinned_icons (dock side).
    let button = children(&fake.pinned_box)[0].clone();
    ensure!(has_controller::<gtk::DragSource>(&button), "dock buttons are drag sources");
    let panel = fake.pinned_box.clone().upcast::<gtk::Widget>();
    let text = |s: &str| DropValue::Text(s.into());
    ensure!(drag::dropped(&*fake, &panel, &text("tessel-dock-app:files.desktop"), -1000.0, -1000.0), "drop");
    ensure!(fake.last() == Some(("place-pin".into(), "files.desktop|browser.desktop".into())), "{:?}", fake.last());
    ensure!(drag::dropped(&*fake, &panel, &text("tessel-dock-app:browser.desktop"), 100000.0, 100000.0), "drop end");
    ensure!(fake.last() == Some(("place-pin".into(), "browser.desktop|None".into())), "{:?}", fake.last());
    let second = children(&fake.pinned_box)[1].clone();
    let x = translate(&second, &panel, 0.0, 0.0).unwrap().0 + second.width() as f64 * 0.75;
    ensure!(drag::dropped(&*fake, &panel, &text("file:///usr/share/applications/files.desktop"), x, 1.0), "file drop");
    ensure!(fake.last() == Some(("place-pin".into(), "files.desktop|None".into())), "{:?}", fake.last());
    ensure!(!drag::dropped(&*fake, &panel, &text("plain text"), 0.0, 0.0), "plain text is refused");
    let files = gdk::FileList::from_array(&[gio::File::for_path("/usr/share/applications/terminal.desktop")]);
    let value = drag::drop_value(&files.to_value()).unwrap();
    ensure!(value == DropValue::Files(vec![Some("/usr/share/applications/terminal.desktop".into())]), "{value:?}");
    let host: Rc<dyn DropHost> = fake.clone();
    let target = drag::install_drop_target(&fake.pinned_box, Rc::downgrade(&host));
    ensure!(target.types().len() == 2, "file lists and strings");
    Ok(())
}

/// dock_ui.py `pin_drop_index`: a button's middle is its translated origin
/// plus half of `get_allocated_width()` (which adds the CSS margin, border
/// and padding), not half of `width()`.
fn drop_index_uses_margin_box(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let first = children(&fake.pinned_box)[0].clone();
    first.add_css_class("f1-wide-box");
    let provider = gtk::CssProvider::new();
    provider.load_from_string("button.f1-wide-box { margin: 0 30px; padding: 0 20px; }");
    gtk::style_context_add_provider_for_display(&first.display(), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 10);
    settle(0.15);
    let panel = fake.pinned_box.clone().upcast::<gtk::Widget>();
    let origin = translate(&first, &panel, 0.0, 0.0).ok_or("translate")?.0;
    #[allow(deprecated)]
    let (border, margin) = (first.width() as f64, first.allocated_width() as f64);
    ensure!(margin >= border + 40.0, "allocated width adds the CSS box: {margin} vs {border}");
    let between = origin + (border / 2.0 + margin / 2.0) / 2.0;
    ensure!(drag::pin_index_at(&*fake, between, 1.0) == 0, "index at {between} (origin {origin}, {border}/{margin})");
    ensure!(drag::pin_index_at(&*fake, origin + margin / 2.0 + 1.0, 1.0) == 1, "past the margin-box middle");
    let text = DropValue::Text("tessel-dock-app:files.desktop".into());
    ensure!(drag::dropped(&*fake, &panel, &text, between, 1.0), "drop");
    ensure!(fake.last() == Some(("place-pin".into(), "files.desktop|browser.desktop".into())), "{:?}", fake.last());
    gtk::style_context_remove_provider_for_display(&first.display(), &provider);
    Ok(())
}

/// The menu is rebuilt on every open but reuses the icon paintables it
/// looked up (GTK otherwise re-loads every tile icon and the dock grows).
fn menu_reuses_icon_paintables(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    // Tile icons sit in the tile's line box (pin stars are inside buttons).
    let icons = |w: &app_menu::MenuWidgets| -> Vec<gtk::Image> {
        find::<gtk::Image>(&w.listing).into_iter().filter(|image| image.parent().is_some_and(|p| p.is::<gtk::Box>())).collect()
    };
    let paintables = |w: &app_menu::MenuWidgets| -> Vec<gdk::Paintable> { icons(w).into_iter().filter_map(|image| image.paintable()).collect() };
    // First open: lazy gicon images (fast first frame), queued for lookup.
    let widgets = open(&fake)?;
    let tiles = icons(&widgets).len();
    fake.menu().close();
    settle(0.8);
    let cached = app_menu::popup::menu_icon_cache_len();
    ensure!(tiles > 0 && cached > 0, "icons cached after the first menu ({tiles} tiles / {cached})");
    let widgets = open(&fake)?;
    let first = paintables(&widgets);
    ensure!(first.len() == tiles, "reopened tiles use cached paintables ({} of {tiles})", first.len());
    fake.menu().close();
    settle(0.05);
    let widgets = open(&fake)?;
    let second = paintables(&widgets);
    ensure!(app_menu::popup::menu_icon_cache_len() == cached, "reopen adds no cache entries");
    ensure!(first == second, "reopened tiles share the same paintables");
    fake.menu().close();
    Ok(())
}

// ---- previews ----------------------------------------------------------------------

/// test_horizontal_and_vertical_preview_activate_exact_window
fn preview_activates_exact_window(monitor: &gdk::Monitor) -> Result<(), String> {
    for (edge, orientation) in [("bottom", gtk::Orientation::Horizontal), ("left", gtk::Orientation::Vertical)] {
        let fake = fake(monitor, base_catalog());
        *fake.edge.borrow_mut() = edge.into();
        let preview = fake.preview();
        preview.show(&fake.item("browser.desktop"));
        settle(0.05);
        let rows: Vec<_> = preview.rows().values().cloned().collect();
        ensure!(rows.len() == 2, "{edge}: {}", rows.len());
        ensure!(rows[0].button.parent().as_ref() == Some(rows[0].tile.upcast_ref()), "button inside its tile");
        ensure!(preview.preview_box().unwrap().orientation() == orientation, "{edge}: orientation");
        let popup = preview.window().unwrap();
        ensure!(gtk4_layer_shell::LayerShell::namespace(&popup).as_deref() == Some("tessel-dock-windows"), "namespace");
        ensure!(gtk4_layer_shell::LayerShell::layer(&popup) == gtk4_layer_shell::Layer::Top, "TOP layer");
        ensure!(popup.has_css_class("tessel-dock-palette-f1"), "popup class");
        ensure!(rows[0].title_label.has_css_class("tessel-dock-window-title") && rows[0].close_button.has_css_class("tessel-dock-window-close"), "classes");
        ensure!(rows[0].button.has_css_class("focused") && !rows[1].button.has_css_class("focused"), "focused class");
        ensure!(rows[1].title_label.text() == "Mail inbox" && rows[1].button.tooltip_text().as_deref() == Some("Mail inbox"), "title");
        ensure!(tessel_dock::ui::app_menu::popup::accessible_label(&rows[1].close_button).as_deref() == Some("Close Mail inbox"), "close label");
        rows[1].button.emit_clicked();
        ensure!(fake.last() == Some(("activate".into(), "browser-second".into())), "{:?}", fake.last());
        ensure!(!preview.is_open(), "{edge}: activation closes the preview");
        ensure!(fake.holds.borrow().last().map(String::as_str) == Some("leave:preview"), "{:?}", fake.holds.borrow());
    }
    Ok(())
}

/// test_window_selection_minimizes_focused_and_restores_minimized_windows
fn preview_minimizes_focused_and_restores(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let preview = fake.preview();
    preview.show(&fake.item("browser.desktop"));
    preview.row("browser-first").unwrap().button.emit_clicked();
    ensure!(fake.last() == Some(("minimize".into(), "browser-first".into())), "{:?}", fake.last());
    ensure!(!preview.is_open(), "closed");
    // Focus can change after a preview was constructed: read state at click time.
    preview.show(&fake.item("browser.desktop"));
    {
        let mut windows = fake.windows.borrow_mut();
        let first = windows.get_mut("browser-first").unwrap();
        first.activated = false;
        first.minimized = true;
    }
    preview.row("browser-first").unwrap().button.emit_clicked();
    ensure!(fake.last() == Some(("activate".into(), "browser-first".into())), "{:?}", fake.last());
    preview.show(&fake.item("browser.desktop"));
    preview.row("browser-second").unwrap().button.emit_clicked();
    ensure!(fake.last() == Some(("activate".into(), "browser-second".into())), "{:?}", fake.last());
    // Single-window items act directly through the same rule.
    fake.windows.borrow_mut().get_mut("files-first").unwrap().activated = true;
    preview.activate_window("files-first");
    ensure!(fake.last() == Some(("minimize".into(), "files-first".into())), "{:?}", fake.last());
    fake.windows.borrow_mut().shift_remove("files-first");
    preview.activate_window("files-first");
    ensure!(fake.last() == Some(("error".into(), "This window is no longer available.".into())), "{:?}", fake.last());
    Ok(())
}

/// test_preview_refresh_and_stale_activation_are_safe + test_preview_mute_updates_every_window…
fn preview_refresh_mute_and_stale(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let preview = fake.preview();
    preview.show(&fake.item("browser.desktop"));
    settle(0.05);
    let rows: Vec<_> = preview.rows().values().cloned().collect();
    let audio = fake.audio.borrow().clone().unwrap();
    ensure!(rows.iter().all(|r| r.mute_button.is_sensitive()), "playback available");
    ensure!(rows[0].mute_button.tooltip_text().as_deref() == Some("Mute application"), "mute tooltip");
    rows[0].mute_button.emit_clicked();
    ensure!(fake.history.borrow().is_empty(), "mute never activates or closes: {:?}", fake.history.borrow());
    let icon = |r: &tessel_dock::ui::preview::PreviewRow| r.mute_button.child().and_downcast::<gtk::Image>().and_then(|i| i.icon_name()).map(|s| s.to_string());
    ensure!(rows.iter().all(|r| icon(r).as_deref() == Some("audio-volume-muted-symbolic")), "every row shows muted");
    ensure!(rows[1].mute_button.tooltip_text().as_deref() == Some("Unmute application"), "unmute tooltip");
    fake.windows.borrow_mut().get_mut("browser-second").unwrap().title = "Renamed document".into();
    fake.render();
    ensure!(Rc::ptr_eq(&rows[0], &preview.row("browser-first").unwrap()), "rows are kept across refreshes");
    ensure!(find::<gtk::Label>(&preview.window().unwrap()).iter().any(|l| l.text() == "Renamed document"), "title updated");
    let stale = preview.row("browser-second").unwrap();
    fake.windows.borrow_mut().shift_remove("browser-second");
    stale.button.emit_clicked();
    ensure!(fake.last().map(|l| l.0) == Some("error".into()), "{:?}", fake.history.borrow());
    ensure!(!fake.history.borrow().iter().any(|h| h.0 == "activate"), "no activation of a stale window");
    preview.close();
    ensure!(audio.closed.get() && preview.audio().is_none(), "audio closed with the preview");
    // An empty item closes; queue() opens after the hover delay.
    let item = fake.item("browser.desktop");
    preview.queue(&item);
    ensure!(preview.timer_pending() && !preview.is_open(), "delayed");
    settle(0.3);
    ensure!(preview.is_open() && preview.preview_id().as_deref() == Some("browser.desktop"), "queued preview opened");
    let mut empty = item.clone();
    empty.windows.clear();
    preview.queue(&empty);
    ensure!(!preview.is_open(), "an item without windows closes the preview");
    // The menu blocks previews.
    let _w = open(&fake)?;
    preview.show(&fake.item("browser.desktop"));
    ensure!(!preview.is_open(), "the menu blocks previews");
    Ok(())
}

/// test_preview_close_handles_stale_and_rejected_window_requests + Close All snapshot.
fn preview_close_requests(monitor: &gdk::Monitor) -> Result<(), String> {
    let fake = fake(monitor, base_catalog());
    let preview = fake.preview();
    preview.show(&fake.item("browser.desktop"));
    let popup = preview.window().unwrap();
    let row = preview.row("browser-second").unwrap();
    fake.refuse_close.set(true);
    row.close_button.emit_clicked();
    let kinds: Vec<String> = fake.history.borrow().iter().map(|h| h.0.clone()).collect();
    ensure!(kinds == ["close-refused", "error"], "{kinds:?}");
    ensure!(fake.last() == Some(("error".into(), "This window could not be closed.".into())), "error text");
    ensure!(preview.window().as_ref() == Some(&popup) && Rc::ptr_eq(&preview.row("browser-second").unwrap(), &row), "kept");
    fake.history.borrow_mut().clear();
    fake.refuse_close.set(false);
    fake.windows.borrow_mut().shift_remove("browser-second");
    row.close_button.emit_clicked();
    ensure!(fake.history.borrow().is_empty(), "An already closed window needs no further action");
    // Close All resolves the group when it runs and snapshots ids.
    fake.windows.borrow_mut().insert("browser-third".into(), WindowData::new("browser-third", "browser", "New document"));
    preview.close_all_windows("browser.desktop");
    let closed: Vec<String> = fake.history.borrow().iter().filter(|h| h.0 == "close").map(|h| h.1.clone()).collect();
    ensure!(closed == ["browser-first", "browser-third"], "{closed:?}");
    ensure!(fake.windows.borrow().keys().cloned().collect::<Vec<_>>() == ["files-first"], "only files remain");
    Ok(())
}

/// test_native_preview_close_targets_one_window_on_both_orientations (pointer part).
fn native_preview_close_targets_one_window(monitor: &gdk::Monitor) -> Result<(), String> {
    for edge in ["bottom", "left"] {
        let fake = fake(monitor, base_catalog());
        *fake.edge.borrow_mut() = edge.into();
        let preview = fake.preview();
        preview.show(&fake.item("browser.desktop"));
        settle(0.1);
        let popup = preview.window().unwrap();
        let first = preview.row("browser-first").unwrap();
        let second = preview.row("browser-second").unwrap();
        for row in [&first, &second] {
            let (x, y) = translate(&row.close_button, &row.tile, 0.0, 0.0).unwrap();
            let (width, height) = (row.close_button.width() as f64, row.close_button.height() as f64);
            ensure!(row.close_button.parent().as_ref() == Some(row.tile.upcast_ref()), "close is a sibling, not nested");
            ensure!(width <= 32.0 && height <= 32.0, "{edge}: close size {width}x{height}");
            ensure!(x >= row.tile.width() as f64 - width - 6.0 && x + width <= row.tile.width() as f64, "{edge}: close x {x}");
            ensure!((0.0..=6.0).contains(&y), "{edge}: close y {y}");
        }
        click_widget(monitor, &popup, &first.close_button);
        ensure!(fake.history.borrow().as_slice() == [("close".to_owned(), "browser-first".to_owned())],
                "{edge}: the X must not activate or minimize: {:?}", fake.history.borrow());
        ensure!(preview.window().as_ref() == Some(&popup), "{edge}: preview stays");
        ensure!(Rc::ptr_eq(&preview.row("browser-second").unwrap(), &second), "{edge}: other row kept");
        ensure!(children(&preview.preview_box().unwrap()) == [second.tile.clone().upcast::<gtk::Widget>()], "{edge}: one tile left");
        click_widget(monitor, &popup, &second.close_button);
        ensure!(fake.history.borrow().len() == 2 && !preview.is_open() && preview.rows().is_empty(), "{edge}: {:?}", fake.history.borrow());
        ensure!(fake.item("browser.desktop").count() == 0, "no windows left");
    }
    Ok(())
}

// ---- main --------------------------------------------------------------------------

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if std::env::var("WAYDOCK_F1_HEADLESS").as_deref() != Ok("1") || runtime == "/run/user/1000" || std::env::var("WAYLAND_DISPLAY").is_err() {
        println!("f1_ui_headless: skipped (run tests/run_f1_headless.sh)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("gtk");
    let display = gdk::Display::default().unwrap();
    let provider = gtk::CssProvider::new();
    provider.load_from_string(tessel_dock::appearance::style::CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    let monitors = tessel_dock::ui::app_menu::popup::monitors(&display);
    let monitor = monitors[0].clone();
    // Keep one virtual keyboard attached for the whole run (Python harness).
    let keyboard = Command::new("wtype").args(["-s", "600000"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok();
    settle(0.2);
    let filter = std::env::args().nth(1).unwrap_or_default();
    type Case = (&'static str, Box<dyn Fn() -> Result<(), String>>);
    let (m, ms) = (monitor.clone(), monitors.clone());
    let cases: Vec<Case> = vec![
        ("search_pin_unpin_and_launch", Box::new({ let m = m.clone(); move || search_pin_unpin_and_launch(&m) })),
        ("ranked_search_launches_with_gdk_context", Box::new({ let m = m.clone(); move || ranked_search_launches_with_gdk_context(&m) })),
        ("grid_placement_order_and_keyboard", Box::new({ let m = m.clone(); move || grid_placement_order_and_keyboard(&m) })),
        ("frequent_row", Box::new({ let m = m.clone(); move || frequent_row(&m) })),
        ("category_click_pin_launch_and_reopen", Box::new({ let m = m.clone(); move || category_click_pin_launch_and_reopen(&m) })),
        ("search_is_global_and_restores_category", Box::new({ let m = m.clone(); move || search_is_global_and_restores_category(&m) })),
        ("three_columns_and_reading_order_on_every_edge", Box::new({ let m = m.clone(); move || three_columns_and_reading_order_on_every_edge(&m) })),
        ("category_keyboard_filters_grid", Box::new({ let m = m.clone(); move || category_keyboard_filters_grid(&m) })),
        ("native_dismissal_pin_and_keyboard", Box::new({ let m = m.clone(); move || native_dismissal_pin_and_keyboard(&m, &ms) })),
        ("drag_hides_backdrops_and_drop_pins", Box::new({ let m = m.clone(); move || drag_hides_backdrops_and_drop_pins(&m) })),
        ("drop_index_uses_margin_box", Box::new({ let m = m.clone(); move || drop_index_uses_margin_box(&m) })),
        ("menu_reuses_icon_paintables", Box::new({ let m = m.clone(); move || menu_reuses_icon_paintables(&m) })),
        ("preview_activates_exact_window", Box::new({ let m = m.clone(); move || preview_activates_exact_window(&m) })),
        ("preview_minimizes_focused_and_restores", Box::new({ let m = m.clone(); move || preview_minimizes_focused_and_restores(&m) })),
        ("preview_refresh_mute_and_stale", Box::new({ let m = m.clone(); move || preview_refresh_mute_and_stale(&m) })),
        ("preview_close_requests", Box::new({ let m = m.clone(); move || preview_close_requests(&m) })),
        ("native_preview_close_targets_one_window", Box::new({ let m = m.clone(); move || native_preview_close_targets_one_window(&m) })),
    ];
    let mut failed = 0;
    let mut ran = 0;
    for (name, case) in &cases {
        if !filter.is_empty() && !name.contains(&filter) {
            continue;
        }
        ran += 1;
        match case() {
            Ok(()) => println!("ok   {name}"),
            Err(error) => {
                failed += 1;
                println!("FAIL {name}: {error}");
            }
        }
        settle(0.1);
    }
    if let Some(mut keyboard) = keyboard {
        let _ = keyboard.kill();
        let _ = keyboard.wait();
    }
    println!("f1_ui_headless: {} passed, {failed} failed", ran - failed);
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
