//! `DesktopView` (wayshell:194-3748): one per-monitor window holding the
//! wallpaper picture, the icon grid and the drag-ghost layer.

use super::config;
use super::icon_layout::{self, clamp_point, constrain_positions, inset, point_of, Point, Rect};
use super::launch;
use super::wallpaper;
use super::DesktopsInner;
use crate::fileops::{self, Action, ConflictReply, ConflictResolver, FileTransfer, ListenerId, Mode, TransferItem, TransferResult, KEEP_BOTH, REPLACE, SKIP};
use gio::prelude::*;
use gtk::gdk;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::rc::{Rc, Weak};
use suite_core::tr;

pub const CELL_WIDTH: i64 = 128;
pub const CELL_HEIGHT: i64 = 128;
pub const DOUBLE_CLICK_MS: f64 = 350.0;
pub const DOUBLE_CLICK_PX: i64 = 8;
pub const PROTECTED: [&str; 4] = ["computer.desktop", "network.desktop", "trash-can.desktop", "user-home.desktop"];

pub const VIEW_CSS: &str = r#"
        .label-highlight {
            background: transparent;
        }
        .label-highlight .desktop-label {
            color: #ffffff;
            text-shadow: 0 1px 2px rgba(0,0,0,.6);
        }
        .label-highlight.selected {
            background-color: @theme_selected_bg_color;
            color: @theme_selected_fg_color;
            border-radius: 8px;
            padding: 2px 6px;
            margin: 1px;
        }
        .label-highlight.selected .desktop-label {
            color: inherit;
            text-shadow: none;
        }
        scrolledwindow scrollbar {
            min-width: 0;
            min-height: 0;
            padding: 0;
            margin: 0;
            background: transparent;
            opacity: 0;
        }
        scrolledwindow undershoot.top,
        scrolledwindow undershoot.bottom,
        scrolledwindow undershoot.left,
        scrolledwindow undershoot.right {
            background: none;
        }
        #main-overlay slider {
            min-width: 18px;
            min-height: 18px;
        }
        .selection-box {
            background-color: alpha(@theme_selected_bg_color, 0.25);
            border: 1px solid alpha(@theme_selected_bg_color, 0.8);
            border-radius: 4px;
        }
        .transfer-toast {
            background-color: @theme_bg_color;
            color: @theme_fg_color;
            border: 1px solid alpha(@theme_fg_color, 0.12);
            border-radius: 12px;
            padding: 10px 12px;
        }
        "#;

/// (st_ino, st_mtime_ns, st_size, st_mode) of `entry.stat(follow_symlinks=False)`.
pub type Signature = (u64, i128, u64, u32);

/// Python's `round()` (half to even).
pub fn py_round(value: f64) -> i64 {
    let floor = value.floor();
    let diff = value - floor;
    let result = if diff > 0.5 || (diff == 0.5 && (floor as i64) % 2 != 0) { floor + 1.0 } else { floor };
    result as i64
}

pub fn is_protected_name(path: &str) -> bool {
    let base = Path::new(path).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    PROTECTED.contains(&base.as_str())
}

/// `get_icon_from_desktop_file` / `get_display_name_for_file` key reads.
pub fn desktop_key(path: &str, key: &str) -> Option<String> {
    let file = glib::KeyFile::new();
    file.load_from_file(path, glib::KeyFileFlags::NONE).ok()?;
    file.string("Desktop Entry", key).ok().map(|s| s.to_string())
}

/// `get_display_name_for_file`: Name= of a .desktop, else the basename.
pub fn display_name_for_file(path: &str) -> String {
    if path.ends_with(".desktop") {
        if let Some(name) = desktop_key(path, "Name").filter(|n| !n.is_empty()) {
            return name;
        }
    }
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned())
}

/// Trash items: name starts with `trash` or the display name contains it.
pub fn is_trash_item(name: &str, display: &str) -> bool {
    name.to_lowercase().starts_with("trash") || display.to_lowercase().contains("trash")
}

pub fn trash_label(count: u32) -> (&'static str, String) {
    if count > 0 {
        ("user-trash-full", tr!("Trash ({value1})", value1 = count as i64))
    } else {
        ("user-trash", tr!("Trash"))
    }
}

/// Clipboard payloads: (`x-special/gnome-copied-files`, `text/uri-list`).
pub fn clipboard_payloads(action: &str, uris: &[String]) -> (String, String) {
    (format!("{action}\n{}", uris.join("\n")), uris.join("\n"))
}

pub fn wl_copy_argvs(action: &str, uris: &[String]) -> [Vec<String>; 2] {
    let (gnome, list) = clipboard_payloads(action, uris);
    [
        vec!["wl-copy".into(), "-t".into(), "x-special/gnome-copied-files".into(), gnome],
        vec!["wl-copy".into(), "-p".into(), "-t".into(), "text/uri-list".into(), list],
    ]
}

thread_local! {
    static NOTIFY_HOOK: RefCell<Option<Rc<dyn Fn(&str, &str)>>> = const { RefCell::new(None) };
}

/// Tests: capture `notify(summary, body)` instead of sending it.
pub fn set_notify_hook(hook: Option<Rc<dyn Fn(&str, &str)>>) {
    NOTIFY_HOOK.with(|h| h.replace(hook));
}

pub fn notify(summary: &str, body: &str) {
    if let Some(hook) = NOTIFY_HOOK.with(|h| h.borrow().clone()) {
        return hook(summary, body);
    }
    fileops::notify(summary, body);
}

pub struct Item {
    pub name: String,
    pub fullpath: String,
    pub display_name: String,
    pub widget: gtk::Box,
    pub highlight: gtk::Box,
    pub label: gtk::Label,
    pub image: gtk::Image,
    pub is_trash: bool,
    pub signature: Signature,
    /// Local position in the grid (what `Fixed.get_child_position` reports once realized).
    pub pos: Cell<(f64, f64)>,
}

/// A confirmation popover request (tests may intercept it).
#[derive(Clone)]
pub struct ConfirmRequest {
    pub title: String,
    pub body: String,
    pub buttons: Vec<String>,
    pub check_label: Option<String>,
    pub on_dismiss: bool,
    /// `(index or None when dismissed, check box state)`.
    pub callback: Rc<dyn Fn(Option<usize>, bool)>,
}

#[derive(Default)]
pub struct ViewState {
    pub show_hidden: bool,
    pub hide_icons_enabled: bool,
    pub align_to_grid: bool,
    pub selected_names: BTreeSet<String>,
    pub selection_order: Vec<String>,
    pub selection_anchor: Option<String>,
    pub extend_last_dir: Option<(i64, i64)>,
    pub suppress_bg_clear: bool,
    pub dragging: bool,
    pub multi_dragging: bool,
    pub rubber_drag_active: bool,
    pub rubber_drag_shift: bool,
    pub rubber_drag_seed: BTreeSet<String>,
    pub rubber_drag_blocked: bool,
    pub drag_base: Vec<(String, (i64, i64))>,
    pub drag_names: Vec<String>,
    pub drag_delta: (i64, i64),
    pub drag_offset: (f64, f64),
    pub drag_start_global: Option<(i64, i64)>,
    pub drag_box: Option<gtk::Box>,
    pub drag_start: (f64, f64),
    pub dc_last_name: Option<String>,
    pub dc_last_time: f64,
    pub dc_last_pos: (i64, i64),
    pub press_launched: bool,
    pub context_item: Option<String>,
    pub dropped_files: Option<Vec<gio::File>>,
    pub last_menu_anchor: Option<(gtk::Widget, i32, i32, std::time::Instant)>,
    pub ghosts: Vec<(usize, String, gtk::Box)>,
}

pub struct DesktopView {
    pub desktops: Weak<DesktopsInner>,
    pub window: gtk::Window,
    pub monitor: gdk::Monitor,
    pub origin: (i64, i64),
    pub is_grid: Cell<bool>,
    pub closed: Cell<bool>,
    pub overlay: gtk::Overlay,
    pub background: gtk::Picture,
    pub grid: gtk::Fixed,
    pub ghost_layer: gtk::Fixed,
    pub actions: gio::SimpleActionGroup,
    pub act_hide_icons: gio::SimpleAction,
    pub act_show_hidden: gio::SimpleAction,
    pub state: RefCell<ViewState>,
    pub items: RefCell<Vec<Rc<Item>>>,
    pub desktop_monitor: RefCell<Option<gio::FileMonitor>>,
    pub style_providers: RefCell<Vec<gtk::CssProvider>>,
    pub wallpaper_provider: RefCell<Option<gtk::CssProvider>>,
    wallpaper_css: RefCell<String>,
    pub wallpaper_texture: RefCell<Option<gdk::Texture>>,
    pub wallpaper_signature: RefCell<Option<wallpaper::TextureKey>>,
    pub wallpaper_rgba: RefCell<Option<gdk::RGBA>>,
    trash_listener: Cell<Option<ListenerId>>,
    pub transfer: RefCell<Option<FileTransfer>>,
    pub transfer_toast: RefCell<Option<gtk::Box>>,
    drag_tick: RefCell<Option<glib::SourceId>>,
    pub confirm_hook: RefCell<Option<Rc<dyn Fn(ConfirmRequest)>>>,
    pub launch_hook: RefCell<Option<Rc<dyn Fn(&str)>>>,
    me: RefCell<Weak<DesktopView>>,
}

fn now_ms() -> f64 {
    glib::monotonic_time() as f64 / 1000.0
}


impl DesktopView {
    /// `DesktopView(app, monitor=, origin=, is_grid=)`.
    pub fn new(desktops: &Rc<DesktopsInner>, monitor: &gdk::Monitor, origin: (i64, i64), is_grid: bool) -> Rc<Self> {
        let window = gtk::Window::new();
        window.add_css_class("wayshell-desktop");
        window.set_title(Some(&tr!("Desktop Folder View")));
        window.set_default_size(900, 600);
        window.set_focusable(true);
        let actions = gio::SimpleActionGroup::new();
        window.insert_action_group("win", Some(&actions));

        let grid = gtk::Fixed::new();
        grid.set_halign(gtk::Align::Fill);
        grid.set_valign(gtk::Align::Fill);
        grid.add_css_class("desktop-grid");
        let background = gtk::Picture::new();
        background.set_can_shrink(true);
        background.set_hexpand(true);
        background.set_vexpand(true);
        background.set_can_target(false);
        background.set_content_fit(gtk::ContentFit::Cover);
        let overlay = gtk::Overlay::new();
        overlay.set_halign(gtk::Align::Fill);
        overlay.set_valign(gtk::Align::Fill);
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        overlay.set_child(Some(&background));
        overlay.add_overlay(&grid);
        overlay.set_widget_name("main-overlay");
        let ghost_layer = gtk::Fixed::new();
        ghost_layer.set_halign(gtk::Align::Fill);
        ghost_layer.set_valign(gtk::Align::Fill);
        ghost_layer.set_hexpand(true);
        ghost_layer.set_vexpand(true);
        ghost_layer.set_can_target(false);
        overlay.add_overlay(&ghost_layer);
        window.set_child(Some(&overlay));

        let act_hide_icons = gio::SimpleAction::new_stateful("toggle_hide_icons", None, &false.to_variant());
        let act_show_hidden = gio::SimpleAction::new_stateful("toggle_show_hidden", None, &false.to_variant());
        let view = Rc::new(Self {
            desktops: Rc::downgrade(desktops),
            window,
            monitor: monitor.clone(),
            origin,
            is_grid: Cell::new(is_grid),
            closed: Cell::new(false),
            overlay,
            background,
            grid,
            ghost_layer,
            actions,
            act_hide_icons,
            act_show_hidden,
            state: RefCell::new(ViewState { align_to_grid: true, ..Default::default() }),
            items: RefCell::default(),
            desktop_monitor: RefCell::default(),
            style_providers: RefCell::default(),
            wallpaper_provider: RefCell::default(),
            wallpaper_css: RefCell::default(),
            wallpaper_texture: RefCell::default(),
            wallpaper_signature: RefCell::default(),
            wallpaper_rgba: RefCell::default(),
            trash_listener: Cell::new(None),
            transfer: RefCell::default(),
            transfer_toast: RefCell::default(),
            drag_tick: RefCell::default(),
            confirm_hook: RefCell::default(),
            launch_hook: RefCell::default(),
            me: RefCell::default(),
        });
        view.me.replace(Rc::downgrade(&view));
        view.build();
        desktops.views.borrow_mut().push(view.clone());
        if is_grid {
            view.load_view_prefs();
            view.load_icons();
            let weak = Rc::downgrade(&view);
            let id = desktops.trash_watcher().add_listener(move |count| {
                if let Some(view) = weak.upgrade() {
                    view.update_trash_status(count);
                }
            });
            view.trash_listener.set(Some(id));
        }
        view
    }


    pub fn desktops(&self) -> Option<Rc<DesktopsInner>> {
        self.desktops.upgrade()
    }

    pub fn trash_listener(&self) -> Option<ListenerId> {
        self.trash_listener.get()
    }

    fn build(self: &Rc<Self>) {
        // Rubber band selection on the whole window.
        let selection = gtk::GestureDrag::new();
        let w = Rc::downgrade(self);
        selection.connect_drag_begin(move |g, x, y| if let Some(v) = w.upgrade() { v.on_selection_drag_begin(g, x, y) });
        let w = Rc::downgrade(self);
        selection.connect_drag_update(move |_, x, y| if let Some(v) = w.upgrade() { v.on_selection_drag_update(x, y) });
        let w = Rc::downgrade(self);
        selection.connect_drag_end(move |_, _, _| if let Some(v) = w.upgrade() { v.on_selection_drag_end() });
        self.window.add_controller(selection);

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE);
        let w = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| w.upgrade().is_some_and(|v| v.on_drop_received(value)));
        self.grid.add_controller(drop);

        let right = gtk::GestureClick::new();
        right.set_button(3);
        let w = Rc::downgrade(self);
        right.connect_pressed(move |_, _, x, y| if let Some(v) = w.upgrade() { v.on_background_right_click(x, y) });
        self.grid.add_controller(right);

        let w = Rc::downgrade(self);
        let monitor = gio::File::for_path(config::desktop_dir()).monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE).ok();
        if let Some(monitor) = &monitor {
            monitor.connect_changed(move |_, _, _, event| if let Some(v) = w.upgrade() { v.on_desktop_changed(event) });
        }
        self.desktop_monitor.replace(monitor);

        let click = gtk::GestureClick::new();
        click.set_button(1);
        let w = Rc::downgrade(self);
        click.connect_released(move |_, _, x, y| if let Some(v) = w.upgrade() { v.on_background_click(x, y) });
        self.grid.add_controller(click);

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string("<Control>H"), Some(gtk::NamedAction::new("win.toggle_show_hidden"))));
        self.window.add_controller(shortcuts);

        // Arrow keys, then Esc/Delete, then the clipboard shortcuts (the
        // order Python adds its three key controllers in).
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, keyval, _, state| match w.upgrade() {
            Some(v) if v.on_key_pressed(keyval, state) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        self.window.add_controller(keys);

        let provider = gtk::CssProvider::new();
        provider.load_from_string(VIEW_CSS);
        gtk::style_context_add_provider_for_display(&WidgetExt::display(&self.window), &provider, gtk::STYLE_PROVIDER_PRIORITY_USER);
        self.style_providers.borrow_mut().push(provider);

        let add = |name: &str, f: Box<dyn Fn(&Rc<Self>)>| {
            let action = gio::SimpleAction::new(name, None);
            let w = Rc::downgrade(self);
            action.connect_activate(move |_, _| if let Some(v) = w.upgrade() { f(&v) });
            self.actions.add_action(&action);
        };
        add("open_item", Box::new(|v| v.action_open_item()));
        add("rename_item", Box::new(|v| v.action_rename_item(None)));
        add("drop_copy_here", Box::new(|v| v.handle_drop("copy")));
        add("drop_move_here", Box::new(|v| v.handle_drop("move")));
        add("delete_item", Box::new(|v| v.action_delete_item()));
        add("empty_trash", Box::new(|v| v.action_empty_trash()));
        add("cut_items", Box::new(|_| {}));
        add("copy_items", Box::new(|_| {}));
        add("delete_items", Box::new(|v| v.delete_selected_item()));
        add("sort_by_name", Box::new(|v| if let Some(d) = v.desktops() { d.sort_all_by_name() }));
        add("sort_by_type", Box::new(|v| if let Some(d) = v.desktops() { d.sort_all_by_type() }));
        add("cut", Box::new(|v| v.multi_cut()));
        add("copy", Box::new(|v| v.multi_copy()));
        add("paste_files", Box::new(|v| v.multi_paste()));
        add("move_to_trash", Box::new(|v| v.delete_selected_item()));

        let w = Rc::downgrade(self);
        self.act_hide_icons.connect_activate(move |_, _| if let Some(v) = w.upgrade() { v.toggle_hide_icons() });
        self.actions.add_action(&self.act_hide_icons);
        let align = gio::SimpleAction::new_stateful("toggle_align_to_grid", None, &false.to_variant());
        let w = Rc::downgrade(self);
        align.connect_activate(move |_, _| if let Some(v) = w.upgrade() { let mut s = v.state.borrow_mut(); s.align_to_grid = !s.align_to_grid; });
        self.actions.add_action(&align);
        let w = Rc::downgrade(self);
        self.act_show_hidden.connect_activate(move |action, _| {
            let Some(v) = w.upgrade() else { return };
            let state = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&state.to_variant());
            if let Some(d) = v.desktops() {
                d.set_show_hidden_all(state);
            }
            v.save_view_prefs();
        });
        self.actions.add_action(&self.act_show_hidden);
    }

    // ------------------------------------------------------------ prefs

    pub fn save_view_prefs(&self) {
        let (hide, show) = {
            let s = self.state.borrow();
            (s.hide_icons_enabled, s.show_hidden)
        };
        if let Err(error) = config::save_view_prefs(hide, show) {
            eprintln!("Failed to save view prefs: {error}");
        }
    }

    pub fn load_view_prefs(&self) {
        let (hide, show) = config::load_view_prefs();
        {
            let mut s = self.state.borrow_mut();
            s.hide_icons_enabled = hide;
            s.show_hidden = show;
        }
        for item in self.items.borrow().iter() {
            item.widget.set_visible(!hide);
        }
        self.act_hide_icons.set_state(&hide.to_variant());
        self.act_show_hidden.set_state(&show.to_variant());
    }

    pub fn hide_icons_enabled(&self) -> bool {
        self.state.borrow().hide_icons_enabled
    }

    pub fn show_hidden(&self) -> bool {
        self.state.borrow().show_hidden
    }

    pub fn toggle_hide_icons(&self) {
        let state = !self.hide_icons_enabled();
        if let Some(d) = self.desktops() {
            d.set_hide_icons_all(state);
        }
        self.save_view_prefs();
    }

    // ----------------------------------------------------------- launching

    fn launch_environment_context(&self) -> gio::AppLaunchContext {
        let context = gio::AppLaunchContext::new();
        launch::apply_launch_environment(&context, &launch::labwc_launch_environment());
        context
    }

    fn launch_failed_cb(&self) -> launch::OnDone {
        Box::new(|error| {
            if let Some(error) = error {
                notify(&tr!("Could not open application"), &error);
            }
        })
    }

    /// `_launch_path`: a .desktop file or a file/dir with its default handler.
    pub fn launch_path(&self, path: &str) {
        if let Some(hook) = self.launch_hook.borrow().clone() {
            return hook(path);
        }
        let context = self.launch_environment_context();
        let result = if path.ends_with(".desktop") {
            launch::launch_desktop_file(path, Some(&context), Some(self.launch_failed_cb()))
        } else {
            match glib::filename_to_uri(path, None) {
                Ok(uri) => launch::launch_uri(&uri, Some(&context), Some(self.launch_failed_cb())),
                Err(error) => Err(suite_core::pyerr::python_error_text(&error)),
            }
        };
        if let Err(error) = result {
            let base = Path::new(path.trim_end_matches('/')).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.to_owned());
            notify(&tr!("Could not open '{name}'", name = base), &error);
        }
    }

    pub fn launch_file_manager(&self) {
        self.launch_path(&config::desktop_dir().to_string_lossy());
    }

    pub fn launch_web_browser(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        fileops::default_web_browser_async(launch::labwc_launch_environment(), move |browser, error| {
            let Some(view) = weak.upgrade() else { return };
            if view.closed.get() {
                return;
            }
            match browser.filter(|b| !b.is_empty()) {
                Some(browser) => view.launch_suite_app(&browser),
                None => notify(&tr!("Could not open the web browser"), &error.filter(|e| !e.is_empty()).unwrap_or_else(|| tr!("No default web browser is set."))),
            }
        });
    }

    /// `launch_suite_app`: the shared app-service launcher.
    pub fn launch_suite_app(self: &Rc<Self>, desktop_id: &str) {
        let weak = Rc::downgrade(self);
        let failed = move |error: String| {
            if let Some(view) = weak.upgrade() {
                view.show_confirmation_dialog(&tr!("Could not open application"), &error, &[tr!("Close")], Rc::new(|_, _| {}), None, false);
            }
        };
        let failed = Rc::new(failed);
        let context: gio::AppLaunchContext = WidgetExt::display(&self.window).app_launch_context().upcast();
        launch::apply_launch_environment(&context, &launch::labwc_launch_environment());
        let catalog = crate::app_catalog::AppCatalog::new();
        let done = failed.clone();
        let result = launch::launch_desktop_id(&catalog, desktop_id, Some(&context), Some(Box::new(move |error| if let Some(e) = error { done(e) })));
        if let Err(error) = result {
            failed(error);
        }
    }

    pub fn launch_configure_displays(&self) {
        let environment = launch::labwc_launch_environment();
        let local = suite_core::paths::home().join(".local/bin/way-displays-gui");
        let exe = if suite_core::shell::is_executable(&local) { Some(local) } else { launch::which_in("way-displays-gui", environment.get("PATH").map(String::as_str)) };
        let Some(exe) = exe else {
            notify(&tr!("Could not open display settings"), &tr!("The display settings app is not installed."));
            return;
        };
        if let Err(error) = launch::launch_process(&[exe.to_string_lossy().into_owned()], Some(&environment), None, Some(self.launch_failed_cb())) {
            notify(&tr!("Could not open display settings"), &error);
        }
    }

    pub fn open_default_terminal(&self) {
        let cwd = suite_core::paths::home().to_string_lossy().into_owned();
        let environment = launch::labwc_launch_environment();
        let result = launch::open_default_terminal(&environment, &cwd, |error| {
            if let Some(error) = error {
                notify(&tr!("Could not open application"), &error);
            }
        });
        if result.is_err() {
            notify(&tr!("Could not open a terminal"), &tr!("No terminal app was found."));
        }
    }

    // ----------------------------------------------------------- geometry

    pub fn owns_global_point(&self, gx: i64, gy: i64) -> bool {
        let geometry = self.monitor.geometry();
        self.origin.0 <= gx && gx < self.origin.0 + geometry.width() as i64 && self.origin.1 <= gy && gy < self.origin.1 + geometry.height() as i64
    }

    pub fn connector(&self) -> Option<String> {
        self.monitor.connector().map(|c| c.to_string())
    }

    /// The monitor's usable area in this window's coordinates, inset.
    pub fn icon_workarea(&self) -> Rect {
        let g = self.monitor.geometry();
        let mut rectangle = (g.x() as i64, g.y() as i64, g.width() as i64, g.height() as i64);
        if let Some(d) = self.desktops() {
            if let Some(source) = d.workarea_source() {
                rectangle = source.rectangle(self.connector().as_deref(), rectangle);
            }
        }
        inset((rectangle.0 - self.origin.0, rectangle.1 - self.origin.1, rectangle.2, rectangle.3))
    }

    /// Snap inside the monitor's current usable desktop area.
    pub fn snap_to_grid(&self, x: i64, y: i64) -> Point {
        let (ax, ay, width, height) = self.icon_workarea();
        let point = (ax + py_round((x - ax) as f64 / CELL_WIDTH as f64) * CELL_WIDTH, ay + py_round((y - ay) as f64 / CELL_HEIGHT as f64) * CELL_HEIGHT);
        clamp_point(point, (ax, ay, width, height))
    }

    /// `save_icon_position`: persist global coordinates.
    pub fn save_icon_position(&self, name: &str, x: i64, y: i64) {
        let (x, y) = clamp_point((x, y), self.icon_workarea());
        config::store_position(name, (x + self.origin.0, y + self.origin.1));
    }

    fn widget_size(widget: &impl IsA<gtk::Widget>) -> (i32, i32) {
        let (mut width, mut height) = (widget.width(), widget.height());
        if width <= 0 || height <= 0 {
            let (_, natural) = widget.preferred_size();
            width = natural.width().max(1);
            height = natural.height().max(1);
        }
        (width, height)
    }

    // ------------------------------------------------------------- icons

    pub fn item(&self, name: &str) -> Option<Rc<Item>> {
        self.items.borrow().iter().find(|i| i.name == name).cloned()
    }

    pub fn item_names(&self) -> Vec<String> {
        self.items.borrow().iter().map(|i| i.name.clone()).collect()
    }

    pub fn item_position(&self, name: &str) -> Option<(f64, f64)> {
        let item = self.item(name)?;
        Some(item.pos.get())
    }

    fn place(&self, item: &Item, x: f64, y: f64) {
        item.pos.set((x, y));
        self.grid.move_(&item.widget, x, y);
    }

    fn release_item(&self, item: &Item) {
        if item.widget.parent().is_some() {
            self.grid.remove(&item.widget);
        }
    }

    pub fn image_for_file(path: &str, size: i32) -> gtk::Image {
        let image = gtk::Image::new();
        image.set_pixel_size(size);
        if Path::new(path).is_dir() {
            image.set_icon_name(Some("folder"));
            return image;
        }
        if path.ends_with(".desktop") {
            match desktop_key(path, "Icon").filter(|i| !i.is_empty()) {
                None => image.set_icon_name(Some("application-x-desktop")),
                Some(icon) if icon.starts_with('/') || icon.contains('/') => match gdk::Texture::from_filename(&icon) {
                    Ok(texture) => image.set_paintable(Some(&texture)),
                    Err(_) => image.set_icon_name(Some("application-x-desktop")),
                },
                Some(icon) => image.set_icon_name(Some(&icon)),
            }
            return image;
        }
        let (content_type, _) = gio::content_type_guess(Some(path), None::<&[u8]>);
        image.set_from_gicon(&gio::content_type_get_icon(&content_type));
        image
    }

    fn make_item(self: &Rc<Self>, name: &str, display: &str, image: gtk::Image, fullpath: &str, is_trash: bool, signature: Signature) -> Rc<Item> {
        image.set_halign(gtk::Align::Center);
        image.set_valign(gtk::Align::Start);
        image.set_vexpand(false);
        image.set_hexpand(false);
        let label = gtk::Label::new(Some(display));
        label.add_css_class("desktop-label");
        label.set_wrap(true);
        label.set_use_underline(false);
        label.set_lines(2);
        label.set_max_width_chars(14);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_vexpand(false);
        label.set_hexpand(false);
        label.set_halign(gtk::Align::Center);
        label.set_valign(gtk::Align::Start);
        label.set_justify(gtk::Justification::Center);
        let highlight = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        highlight.set_halign(gtk::Align::Center);
        highlight.set_valign(gtk::Align::End);
        highlight.add_css_class("label-highlight");
        highlight.append(&label);
        let item_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        item_box.set_halign(gtk::Align::Center);
        item_box.set_valign(gtk::Align::Center);
        item_box.set_hexpand(false);
        item_box.set_vexpand(true);
        item_box.append(&image);
        item_box.append(&highlight);
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.set_halign(gtk::Align::Center);
        widget.set_valign(gtk::Align::Center);
        widget.set_hexpand(false);
        widget.set_vexpand(true);
        widget.append(&item_box);
        widget.set_size_request(CELL_WIDTH as i32, CELL_HEIGHT as i32);
        for w in [image.upcast_ref::<gtk::Widget>(), label.upcast_ref(), highlight.upcast_ref(), item_box.upcast_ref()] {
            w.set_can_target(false);
        }
        let item = Rc::new(Item {
            name: name.to_owned(),
            fullpath: fullpath.to_owned(),
            display_name: display.to_owned(),
            widget: widget.clone(),
            highlight,
            label,
            image,
            is_trash,
            signature,
            pos: Cell::new((0.0, 0.0)),
        });

        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.set_exclusive(false);
        click.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let (w, display_name) = (Rc::downgrade(self), display.to_owned());
        click.connect_pressed(move |g, n, _, _| if let Some(v) = w.upgrade() { v.on_file_press(g, n, &display_name) });
        let (w, path) = (Rc::downgrade(self), fullpath.to_owned());
        click.connect_released(move |g, _, x, y| if let Some(v) = w.upgrade() { v.on_file_release(g.current_event_state(), x, y, &path) });
        widget.add_controller(click.clone());

        let right = gtk::GestureClick::new();
        right.set_button(3);
        let (w, display_name, path) = (Rc::downgrade(self), display.to_owned(), fullpath.to_owned());
        right.connect_pressed(move |g, _, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            if let Some(v) = w.upgrade() {
                v.on_right_click(&display_name, &path)
            }
        });
        widget.add_controller(right);

        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let (w, path) = (Rc::downgrade(self), fullpath.to_owned());
        drag.connect_drag_begin(move |g, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            if let Some(v) = w.upgrade() {
                v.on_drag_begin(&path)
            }
        });
        let (w, path) = (Rc::downgrade(self), fullpath.to_owned());
        drag.connect_drag_update(move |_, x, y| if let Some(v) = w.upgrade() { v.on_drag_update(&path, x, y) });
        let (w, path) = (Rc::downgrade(self), fullpath.to_owned());
        drag.connect_drag_end(move |_, _, _| if let Some(v) = w.upgrade() { v.drag_end(&path) });
        widget.add_controller(drag.clone());
        click.group_with(&drag);
        item
    }

    /// `load_icons`: reconcile widgets with ~/Desktop and the saved positions.
    pub fn load_icons(self: &Rc<Self>) {
        let (hidden, show_hidden) = {
            let s = self.state.borrow();
            (s.hide_icons_enabled, s.show_hidden)
        };
        if hidden || self.closed.get() {
            return;
        }
        let Some(desktops) = self.desktops() else { return };
        let positions = config::load_positions();
        let desktop = config::desktop_dir();
        let Ok(entries) = std::fs::read_dir(&desktop) else { return };
        let mut files: Vec<(String, Signature)> = Vec::new();
        let mut existing = HashSet::new();
        for entry in entries {
            let Ok(entry) = entry else { return };
            let name = entry.file_name().to_string_lossy().into_owned();
            existing.insert(name.clone());
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let Ok(info) = std::fs::symlink_metadata(entry.path()) else { continue };
            files.push((name, (info.ino(), info.mtime() as i128 * 1_000_000_000 + info.mtime_nsec() as i128, info.size(), info.mode())));
        }
        let positions: icon_layout::Positions = positions.into_iter().filter(|(k, _)| existing.contains(k)).collect();
        let mut positions = constrain_positions(&positions, &desktops.icon_regions());
        let primary = desktops.primary_grid().is_some_and(|p| Rc::ptr_eq(&p, self));
        let mut desired = BTreeSet::new();
        for (name, signature) in files {
            if !positions.contains_key(&name) {
                if !primary {
                    continue;
                }
                let slot = desktops.next_global_slot(&positions);
                positions.insert(name.clone(), icon_layout::point_value(slot));
            }
            let Some((gx, gy)) = point_of(&positions[&name]) else { continue };
            if !self.owns_global_point(gx, gy) {
                continue;
            }
            desired.insert(name.clone());
            let local = ((gx - self.origin.0) as f64, (gy - self.origin.1) as f64);
            let existing_item = self.item(&name);
            let reuse = existing_item.as_ref().filter(|item| item.signature == signature).cloned();
            if let Some(stale) = existing_item.as_ref().filter(|item| item.signature != signature) {
                self.release_item(stale);
            }
            let item = match reuse {
                Some(item) => {
                    self.place(&item, local.0, local.1);
                    item
                }
                None => {
                    let fullpath = desktop.join(&name).to_string_lossy().into_owned();
                    let mut display = display_name_for_file(&fullpath);
                    let image = Self::image_for_file(&fullpath, 64);
                    let is_trash = is_trash_item(&name, &display);
                    if is_trash {
                        let (icon, label) = trash_label(desktops.trash_count());
                        image.set_icon_name(Some(icon));
                        display = label;
                    }
                    let item = self.make_item(&name, &display, image, &fullpath, is_trash, signature);
                    {
                        let mut items = self.items.borrow_mut();
                        match items.iter().position(|i| i.name == name) {
                            Some(index) => items[index] = item.clone(),
                            None => items.push(item.clone()),
                        }
                    }
                    item.pos.set(local);
                    self.grid.put(&item.widget, local.0, local.1);
                    item
                }
            };
            item.widget.set_visible(true);
        }
        let removed: Vec<Rc<Item>> = self.items.borrow().iter().filter(|i| !desired.contains(&i.name)).cloned().collect();
        for item in &removed {
            self.release_item(item);
        }
        self.items.borrow_mut().retain(|i| desired.contains(&i.name));
        {
            let mut s = self.state.borrow_mut();
            s.selected_names.retain(|n| desired.contains(n));
            s.selection_order.retain(|n| desired.contains(n));
            s.selection_anchor = None;
        }
        self.apply_selection_visuals();
        config::save_all_positions(&positions);
    }

    pub fn update_trash_status(&self, count: u32) {
        if self.closed.get() {
            return;
        }
        for item in self.items.borrow().iter().filter(|i| i.is_trash) {
            let (icon, label) = trash_label(count);
            item.image.set_icon_name(Some(icon));
            item.label.set_text(&label);
        }
    }

    fn on_desktop_changed(&self, event: gio::FileMonitorEvent) {
        use gio::FileMonitorEvent as E;
        if !self.closed.get() && matches!(event, E::ChangesDoneHint | E::Created | E::Deleted | E::Moved | E::Renamed | E::MovedIn | E::MovedOut | E::AttributeChanged) {
            if let Some(d) = self.desktops() {
                d.queue_desktop_refresh();
            }
        }
    }

    /// Test hook: feed a folder-monitor event.
    pub fn desktop_changed(&self, event: gio::FileMonitorEvent) {
        self.on_desktop_changed(event);
    }

    pub fn refresh_desktop_icons(&self) {
        if !self.closed.get() {
            if let Some(d) = self.desktops() {
                d.queue_desktop_refresh();
            }
        }
    }

    // ---------------------------------------------------------- selection

    pub fn selected_names(&self) -> BTreeSet<String> {
        self.state.borrow().selected_names.clone()
    }

    pub fn set_selected_names(&self, names: impl IntoIterator<Item = String>) {
        self.state.borrow_mut().selected_names = names.into_iter().collect();
        self.apply_selection_visuals();
    }

    pub fn apply_selection_visuals(&self) {
        let selected = self.state.borrow().selected_names.clone();
        for item in self.items.borrow().iter() {
            if selected.contains(&item.name) {
                item.highlight.add_css_class("selected");
            } else {
                item.highlight.remove_css_class("selected");
            }
        }
    }

    pub fn clear_selection(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.selected_names.clear();
            s.suppress_bg_clear = false;
            s.selection_anchor = None;
            s.selection_order.clear();
            s.extend_last_dir = None;
        }
        self.apply_selection_visuals();
    }

    pub fn selected_contains_protected(&self) -> bool {
        self.state.borrow().selected_names.iter().any(|name| {
            let path = self.item(name).map(|i| i.fullpath.clone()).unwrap_or_else(|| config::desktop_dir().join(name).to_string_lossy().into_owned());
            is_protected_name(&path)
        })
    }

    /// `_grid_snapshot`: (name, row, col) of the grid's children in order.
    pub fn grid_snapshot(&self) -> Vec<(String, i64, i64)> {
        let (ax, ay, _, _) = self.icon_workarea();
        let items = self.items.borrow();
        let mut snapshot = Vec::new();
        let mut child = self.grid.first_child();
        while let Some(widget) = child {
            if let Some(item) = items.iter().find(|i| i.widget.upcast_ref::<gtk::Widget>() == &widget) {
                let (x, y) = item.pos.get();
                snapshot.push((item.name.clone(), py_round((y - ay as f64) / CELL_HEIGHT as f64), py_round((x - ax as f64) / CELL_WIDTH as f64)));
            }
            child = widget.next_sibling();
        }
        snapshot
    }

    /// The second `navigate_selection` definition (the one Python uses).
    pub fn navigate_selection(&self, keyval: gdk::Key, extend: bool) {
        let snapshot = self.grid_snapshot();
        if snapshot.is_empty() {
            return;
        }
        let by_cell = |row: i64, col: i64| snapshot.iter().find(|(_, r, c)| *r == row && *c == col).map(|(n, _, _)| n.clone());
        let cell_of = |name: &str| snapshot.iter().find(|(n, _, _)| n == name).map(|(_, r, c)| (*r, *c));
        let max_row = snapshot.iter().map(|s| s.1).max().unwrap_or(0);
        let max_col = snapshot.iter().map(|s| s.2).max().unwrap_or(0);
        let head = self.state.borrow().selection_order.last().cloned();
        let Some(head) = head else {
            let first = snapshot.iter().min_by_key(|(_, r, c)| (*r, *c)).unwrap().0.clone();
            self.clear_selection();
            {
                let mut s = self.state.borrow_mut();
                s.selected_names = [first.clone()].into();
                s.selection_anchor = Some(first.clone());
                s.selection_order = vec![first];
                s.extend_last_dir = None;
            }
            self.apply_selection_visuals();
            return;
        };
        let (dx, dy) = match keyval {
            gdk::Key::Right => (1, 0),
            gdk::Key::Left => (-1, 0),
            gdk::Key::Down => (0, 1),
            gdk::Key::Up => (0, -1),
            _ => return,
        };
        // Python looks the head up by name; a vanished head raises there.
        let Some(start) = cell_of(&head) else { return };
        let step_from = |(mut r, mut c): (i64, i64)| loop {
            r += dy;
            c += dx;
            if r < 0 || c < 0 || r > max_row || c > max_col {
                return None;
            }
            if let Some(found) = by_cell(r, c) {
                return Some(found);
            }
        };
        if !extend {
            let Some(candidate) = step_from(start) else { return };
            {
                let mut s = self.state.borrow_mut();
                s.selected_names = [candidate.clone()].into();
                s.selection_anchor = Some(candidate.clone());
                s.selection_order = vec![candidate];
                s.extend_last_dir = None;
            }
            self.apply_selection_visuals();
            return;
        }
        let mut s = self.state.borrow_mut();
        let last_dir = *s.extend_last_dir.get_or_insert((dx, dy));
        let is_same = (dx, dy) == last_dir;
        let is_reverse = (dx, dy) == (-last_dir.0, -last_dir.1);
        if is_reverse && s.selection_order.len() > 1 {
            let last = s.selection_order.pop().unwrap();
            s.selected_names.remove(&last);
            if s.selection_order.len() == 1 {
                s.extend_last_dir = None;
            }
            drop(s);
            self.apply_selection_visuals();
            return;
        }
        let Some(candidate) = step_from(start) else { return };
        if !is_same && !is_reverse {
            s.extend_last_dir = Some((dx, dy));
        }
        if s.selection_order.last() == Some(&candidate) {
            return;
        }
        s.selected_names.insert(candidate.clone());
        if !s.selection_order.contains(&candidate) {
            s.selection_order.push(candidate);
        }
        drop(s);
        self.apply_selection_visuals();
    }

    /// `get_item_by_display_name`.
    pub fn item_by_display_name(&self, display_name: &str) -> Option<Rc<Item>> {
        let lower = display_name.to_lowercase();
        self.items
            .borrow()
            .iter()
            .find(|item| {
                let base = item.name.to_lowercase();
                let (stem, _) = fileops::splitext(&base);
                lower == base || lower == stem || lower.starts_with(stem) || item.display_name.to_lowercase() == lower
            })
            .cloned()
    }

    fn on_file_press(&self, gesture: &gtk::GestureClick, n_press: i32, display_name: &str) {
        self.state.borrow_mut().suppress_bg_clear = true;
        self.window.grab_focus();
        let state = gesture.current_event_state();
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        if !shift && !ctrl && n_press >= 2 {
            let item = self.item_by_display_name(display_name);
            if let Some(item) = &item {
                self.clear_selection();
                self.state.borrow_mut().selected_names = [item.name.clone()].into();
                self.apply_selection_visuals();
            }
            if let Some(item) = item {
                // Deviation: Python's release handler would launch the same
                // double click a second time; launch once.
                let mut s = self.state.borrow_mut();
                s.press_launched = true;
                drop(s);
                self.launch_path(&item.fullpath);
            }
            return;
        }
        let Some(item) = self.item_by_display_name(display_name) else { return };
        let key = item.name.clone();
        if shift {
            self.state.borrow_mut().selected_names.insert(key);
            self.apply_selection_visuals();
            return;
        }
        if ctrl {
            {
                let mut s = self.state.borrow_mut();
                if !s.selected_names.remove(&key) {
                    s.selected_names.insert(key);
                }
            }
            self.apply_selection_visuals();
            return;
        }
        if self.state.borrow().selected_names.contains(&key) {
            return;
        }
        self.clear_selection();
        self.state.borrow_mut().selected_names = [key].into();
        self.apply_selection_visuals();
    }

    /// `on_file_release`: 350 ms / 8 px double click → open.
    pub fn on_file_release(&self, state: gdk::ModifierType, x: f64, y: f64, fullpath: &str) {
        if self.state.borrow().dragging {
            return;
        }
        if state.intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK) {
            return;
        }
        if std::mem::take(&mut self.state.borrow_mut().press_launched) {
            self.state.borrow_mut().dc_last_name = None;
            return;
        }
        self.release_at(now_ms(), (x as i64, y as i64), fullpath);
    }

    /// The timing core of `on_file_release` (tests drive the clock).
    pub fn release_at(&self, now: f64, pos: (i64, i64), fullpath: &str) -> bool {
        let name = Path::new(fullpath).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let double = {
            let s = self.state.borrow();
            let (dx, dy) = (pos.0 - s.dc_last_pos.0, pos.1 - s.dc_last_pos.1);
            s.dc_last_name.as_deref() == Some(name.as_str()) && now - s.dc_last_time <= DOUBLE_CLICK_MS && dx * dx + dy * dy <= DOUBLE_CLICK_PX * DOUBLE_CLICK_PX
        };
        if double {
            self.state.borrow_mut().dc_last_name = None;
            self.launch_path(fullpath);
            return true;
        }
        let mut s = self.state.borrow_mut();
        s.dc_last_name = Some(name);
        s.dc_last_time = now;
        s.dc_last_pos = pos;
        false
    }

    fn on_key_pressed(self: &Rc<Self>, keyval: gdk::Key, state: gdk::ModifierType) -> bool {
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        match keyval {
            gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down => {
                self.navigate_selection(keyval, shift);
                true
            }
            gdk::Key::Escape => {
                self.clear_selection();
                true
            }
            gdk::Key::Delete | gdk::Key::KP_Delete => {
                self.delete_selected_item();
                true
            }
            gdk::Key::c | gdk::Key::x | gdk::Key::v if ctrl => {
                if !self.selected_contains_protected() {
                    match keyval {
                        gdk::Key::c => self.multi_copy(),
                        gdk::Key::x => self.multi_cut(),
                        _ => self.multi_paste(),
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Test hook for the key handler.
    pub fn key_press(self: &Rc<Self>, keyval: gdk::Key, state: gdk::ModifierType) -> bool {
        self.on_key_pressed(keyval, state)
    }

    fn on_background_click(&self, x: f64, y: f64) {
        {
            let s = self.state.borrow();
            if s.rubber_drag_active || s.dragging {
                return;
            }
        }
        let hit = self.child_hit(x, y);
        let suppress = std::mem::take(&mut self.state.borrow_mut().suppress_bg_clear);
        if !hit && !suppress {
            self.clear_selection();
        }
    }

    fn child_hit(&self, x: f64, y: f64) -> bool {
        let mut child = self.grid.first_child();
        while let Some(widget) = child {
            if let Some(rect) = widget.compute_bounds(&self.grid) {
                let (rx, ry, rw, rh) = (rect.x() as f64, rect.y() as f64, rect.width() as f64, rect.height() as f64);
                if rx <= x && x <= rx + rw && ry <= y && y <= ry + rh {
                    return true;
                }
            }
            child = widget.next_sibling();
        }
        false
    }

    // --------------------------------------------------------- rubber band

    fn on_selection_drag_begin(&self, gesture: &gtk::GestureDrag, x: f64, y: f64) {
        if self.state.borrow().rubber_drag_blocked {
            return;
        }
        let shift = gesture.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
        let drag_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        drag_box.add_css_class("selection-box");
        drag_box.set_size_request(1, 1);
        drag_box.set_halign(gtk::Align::Start);
        drag_box.set_valign(gtk::Align::Start);
        self.overlay.add_overlay(&drag_box);
        drag_box.set_visible(true);
        let mut s = self.state.borrow_mut();
        s.rubber_drag_active = true;
        s.rubber_drag_shift = shift;
        s.rubber_drag_seed = if shift { s.selected_names.clone() } else { BTreeSet::new() };
        s.drag_start = (x, y);
        s.drag_box = Some(drag_box);
    }

    fn on_selection_drag_update(&self, offset_x: f64, offset_y: f64) {
        let (start, drag_box) = {
            let s = self.state.borrow();
            (s.drag_start, s.drag_box.clone())
        };
        let Some(drag_box) = drag_box else { return };
        let x0 = start.0.min(start.0 + offset_x);
        let y0 = start.1.min(start.1 + offset_y);
        let (w, h) = (offset_x.abs(), offset_y.abs());
        drag_box.set_margin_start(x0 as i32);
        drag_box.set_margin_top(y0 as i32);
        drag_box.set_size_request(w as i32, h as i32);
        self.update_selection_in_rect(x0 as i64, y0 as i64, w as i64, h as i64);
    }

    fn on_selection_drag_end(&self) {
        let drag_box = self.state.borrow_mut().drag_box.take();
        if let Some(drag_box) = drag_box {
            self.overlay.remove_overlay(&drag_box);
        }
        let mut s = self.state.borrow_mut();
        s.rubber_drag_active = false;
        s.rubber_drag_shift = false;
        s.rubber_drag_seed.clear();
    }

    pub fn set_rubber_drag_active(&self, active: bool) {
        self.state.borrow_mut().rubber_drag_active = active;
    }

    pub fn update_selection_in_rect(&self, x: i64, y: i64, w: i64, h: i64) {
        let mut hits = BTreeSet::new();
        for item in self.items.borrow().iter() {
            if item.widget.parent().is_none() {
                continue;
            }
            let (px, py) = item.pos.get();
            let (ix, iy, iw, ih) = (px as i64, py as i64, item.widget.width() as i64, item.widget.height() as i64);
            if ix < x + w && ix + iw > x && iy < y + h && iy + ih > y {
                hits.insert(item.name.clone());
            }
        }
        {
            let mut s = self.state.borrow_mut();
            s.selected_names = if s.rubber_drag_shift { s.rubber_drag_seed.union(&hits).cloned().collect() } else { hits };
        }
        self.apply_selection_visuals();
    }

    // -------------------------------------------------------------- clipboard

    fn selected_uris(&self) -> Vec<String> {
        let desktop = config::desktop_dir();
        self.state.borrow().selected_names.iter().map(|n| desktop.join(n)).filter(|p| p.exists()).map(|p| format!("file://{}", p.display())).collect()
    }

    pub fn multi_copy(&self) {
        if self.state.borrow().selected_names.is_empty() {
            return;
        }
        let uris = self.selected_uris();
        self.set_clipboard_file_uris("copy", &uris);
    }

    pub fn multi_cut(&self) {
        if self.state.borrow().selected_names.is_empty() {
            return;
        }
        let uris = self.selected_uris();
        self.set_clipboard_file_uris("cut", &uris);
    }

    pub fn set_clipboard_file_uris(&self, action: &str, uris: &[String]) {
        let clipboard = WidgetExt::display(&self.window).clipboard();
        let (gnome, list) = clipboard_payloads(action, uris);
        let provider = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_bytes(fileops::GNOME_COPIED_FILES, &glib::Bytes::from_owned(gnome.into_bytes())),
            gdk::ContentProvider::for_bytes(fileops::URI_LIST, &glib::Bytes::from_owned(list.into_bytes())),
        ]);
        let _ = clipboard.set_content(Some(&provider));
        // Also export to wl-copy so non-GTK apps can paste (asynchronous).
        for argv in wl_copy_argvs(action, uris) {
            let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_ref()).collect();
            match gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
                Ok(process) => process.wait_async(gio::Cancellable::NONE, |_| {}),
                Err(error) => {
                    eprintln!("wl-copy not available: {}", error.message());
                    break;
                }
            }
        }
    }

    pub fn multi_paste(self: &Rc<Self>) {
        let clipboard = WidgetExt::display(&self.window).clipboard();
        let available: Vec<String> = clipboard.formats().mime_types().iter().map(|m| m.to_string()).collect();
        let format = if available.iter().any(|m| m == fileops::GNOME_COPIED_FILES) {
            fileops::GNOME_COPIED_FILES
        } else if available.iter().any(|m| m == fileops::URI_LIST) {
            fileops::URI_LIST
        } else {
            return;
        };
        let weak = Rc::downgrade(self);
        clipboard.read_async(&[format], glib::Priority::DEFAULT, gio::Cancellable::NONE, move |result| {
            let stream = match result {
                Ok((stream, _)) => stream,
                Err(error) => return notify(&tr!("Could not paste"), error.message()),
            };
            let sink = gio::MemoryOutputStream::new_resizable();
            let target = sink.clone();
            sink.splice_async(&stream, gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET, glib::Priority::DEFAULT, gio::Cancellable::NONE, move |result| {
                if let Err(error) = result {
                    return notify(&tr!("Could not paste"), error.message());
                }
                let bytes = target.steal_as_bytes();
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if let Some(view) = weak.upgrade() {
                    view.do_paste_from_bytes(&text, format);
                }
            });
        });
    }

    pub fn do_paste_from_bytes(self: &Rc<Self>, text: &str, format: &str) {
        let (mode, uris) = fileops::parse_clipboard_files(text, format);
        if uris.is_empty() {
            return;
        }
        let mode = mode.map(|m| m.as_str()).unwrap_or("copy");
        self.start_file_transfer(uris.iter().map(|u| gio::File::for_uri(u)).collect(), mode, None);
    }

    // ------------------------------------------------------------ transfers

    pub fn start_file_transfer(self: &Rc<Self>, sources: Vec<gio::File>, mode: &str, dest_dir: Option<std::path::PathBuf>) {
        let mode = Mode::from_str(mode);
        let dest_dir = dest_dir.unwrap_or_else(config::desktop_dir);
        if sources.is_empty() {
            return;
        }
        let weak = Rc::downgrade(self);
        let dest = dest_dir.clone();
        fileops::plan_async(sources, dest_dir, mode, move |items| {
            let Some(view) = weak.upgrade() else { return };
            if view.closed.get() {
                return;
            }
            let ask = Rc::downgrade(&view);
            let run = Rc::downgrade(&view);
            ConflictResolver::new(
                items,
                move |item, remaining, reply| match ask.upgrade() {
                    Some(view) => view.ask_transfer_conflict(item, remaining, reply),
                    None => reply.send(Some(SKIP), true),
                },
                move |resolved| {
                    if let Some(view) = run.upgrade() {
                        view.run_transfer(resolved, dest, mode)
                    }
                },
            )
            .start();
        }, None);
    }

    fn ask_transfer_conflict(&self, item: &TransferItem, remaining: usize, reply: ConflictReply) {
        if self.closed.get() {
            return reply.send(Some(SKIP), true);
        }
        let title = if config::desktop_dir().join(&item.name).is_dir() {
            tr!("A folder named '{name}' already exists.", name = item.name.as_str())
        } else {
            tr!("A file named '{name}' already exists.", name = item.name.as_str())
        };
        let reply = RefCell::new(Some(reply));
        let choices: [Action; 3] = [SKIP, KEEP_BOTH, REPLACE];
        let check = (remaining > 1).then(|| tr!("Apply to all {count} conflicts", count = remaining as i64));
        self.show_confirmation_dialog(
            &title,
            &tr!("Replace it, keep both, or skip this item?"),
            &[tr!("Skip"), tr!("Keep Both"), tr!("Replace")],
            Rc::new(move |index, apply_all| {
                if let Some(reply) = reply.borrow_mut().take() {
                    reply.send(Some(index.map(|i| choices[i]).unwrap_or(SKIP)), apply_all);
                }
            }),
            check,
            true,
        );
    }

    fn run_transfer(self: &Rc<Self>, items: Vec<TransferItem>, dest_dir: std::path::PathBuf, mode: Mode) {
        let items: Vec<TransferItem> = items.into_iter().filter(|i| i.action != Some(SKIP)).collect();
        if items.is_empty() || self.closed.get() {
            return;
        }
        let text_for = move |name: &str| if mode == Mode::Move { tr!("Moving '{name}'…", name = name) } else { tr!("Copying '{name}'…", name = name) };
        let first = items[0].name.clone();
        let transfer = FileTransfer::new(items, dest_dir, mode);
        let row: Rc<RefCell<Option<(gtk::Box, gtk::Label, gtk::ProgressBar)>>> = Rc::default();
        let timer: Rc<RefCell<Option<suite_core::timer::Timeout>>> = Rc::default();
        let progress_row = row.clone();
        transfer.on_progress(move |fraction, name| {
            if let Some((_, label, bar)) = &*progress_row.borrow() {
                bar.set_fraction(fraction);
                label.set_text(&text_for(name));
            }
        });
        let (weak, done_row, done_timer) = (Rc::downgrade(self), row.clone(), timer.clone());
        transfer.on_done(move |result| {
            if let Some(timer) = done_timer.borrow_mut().take() {
                timer.cancel();
            }
            let Some(view) = weak.upgrade() else { return };
            if let Some((widget, _, _)) = done_row.borrow_mut().take() {
                view.remove_transfer_row(&widget);
            }
            view.transfer_finished(result, mode);
        });
        let (weak, cancel) = (Rc::downgrade(self), transfer.clone());
        let show_timer = timer.clone();
        // Short operations finish before the toast would appear.
        timer.replace(Some(suite_core::timer::once(400, move || {
            show_timer.borrow_mut().take();
            let Some(view) = weak.upgrade() else { return };
            if !view.closed.get() {
                let cancel = cancel.clone();
                row.replace(Some(view.add_transfer_row(&text_for(&first), Rc::new(move || cancel.cancel()))));
            }
        })));
        self.transfer.replace(Some(transfer.clone()));
        transfer.start();
    }

    /// `_add_transfer_row` -> (row, label, bar); the Cancel button is the row's last child.
    pub fn add_transfer_row(&self, text: &str, cancel: Rc<dyn Fn()>) -> (gtk::Box, gtk::Label, gtk::ProgressBar) {
        if self.transfer_toast.borrow().is_none() {
            let toast = gtk::Box::new(gtk::Orientation::Vertical, 6);
            toast.set_halign(gtk::Align::End);
            toast.set_valign(gtk::Align::End);
            let (x, y, width, height) = self.icon_workarea();
            let (ow, oh) = Self::widget_size(&self.overlay);
            toast.set_margin_end((ow as i64 - (x + width) + 16).max(16) as i32);
            toast.set_margin_bottom((oh as i64 - (y + height) + 16).max(16) as i32);
            self.overlay.add_overlay(&toast);
            self.transfer_toast.replace(Some(toast));
        }
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        widget.add_css_class("transfer-toast");
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        column.set_valign(gtk::Align::Center);
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_max_width_chars(36);
        label.set_width_chars(28);
        let bar = gtk::ProgressBar::new();
        column.append(&label);
        column.append(&bar);
        let button = gtk::Button::with_label(&tr!("Cancel"));
        button.set_valign(gtk::Align::Center);
        button.connect_clicked(move |b| {
            b.set_sensitive(false);
            cancel();
        });
        widget.append(&column);
        widget.append(&button);
        self.transfer_toast.borrow().as_ref().unwrap().append(&widget);
        (widget, label, bar)
    }

    pub fn remove_transfer_row(&self, widget: &gtk::Box) {
        if self.closed.get() {
            return;
        }
        let toast = self.transfer_toast.borrow().clone();
        let Some(toast) = toast else { return };
        toast.remove(widget);
        if toast.first_child().is_none() {
            self.overlay.remove_overlay(&toast);
            self.transfer_toast.replace(None);
        }
    }

    fn transfer_finished(self: &Rc<Self>, result: TransferResult, mode: Mode) {
        self.transfer.replace(None);
        // New items get a free global slot; replaced ones keep their place.
        let mut positions = config::load_positions();
        let mut changed = false;
        if let Some(d) = self.desktops() {
            for name in &result.placed {
                if !positions.contains_key(name) {
                    let slot = d.next_global_slot(&positions);
                    positions.insert(name.clone(), icon_layout::point_value(slot));
                    changed = true;
                }
            }
        }
        if changed {
            config::save_all_positions(&positions);
        }
        self.refresh_desktop_icons();
        if result.errors.is_empty() {
            return;
        }
        let (title, body) = if result.errors.len() == 1 {
            let (name, message) = &result.errors[0];
            (if mode == Mode::Move { tr!("Could not move '{name}'", name = name.as_str()) } else { tr!("Could not copy '{name}'", name = name.as_str()) }, message.clone())
        } else {
            let count = result.errors.len() as i64;
            (
                if mode == Mode::Move { tr!("{count} items could not be moved", count = count) } else { tr!("{count} items could not be copied", count = count) },
                result.errors.iter().take(6).map(|(n, m)| format!("{n}: {m}")).collect::<Vec<_>>().join("\n"),
            )
        };
        for (name, message) in &result.errors {
            eprintln!("{name}: {message}");
        }
        if self.closed.get() {
            notify(&title, &body);
        } else {
            self.show_confirmation_dialog(&title, &body, &[tr!("Close")], Rc::new(|_, _| {}), None, false);
        }
    }

    // ------------------------------------------------------------ dialogs

    fn set_menu_anchor(&self, parent: &gtk::Widget, x: i32, y: i32) {
        self.state.borrow_mut().last_menu_anchor = Some((parent.clone(), x, y, std::time::Instant::now()));
    }

    /// `_popover_parent_at_pointer` -> (overlay of the view under the pointer, x, y).
    pub fn popover_parent_at_pointer(&self) -> (gtk::Widget, i32, i32) {
        let anchor = self.state.borrow().last_menu_anchor.clone();
        if let Some((parent, x, y, at)) = anchor {
            if at.elapsed().as_secs_f64() < 3.0 {
                let (aw, ah) = (parent.width(), parent.height());
                return (parent, x.clamp(0, (aw - 1).max(0)), y.clamp(0, (ah - 1).max(0)));
            }
        }
        let center = |widget: &gtk::Widget| {
            let (aw, ah) = Self::widget_size(widget);
            (widget.clone(), (aw / 2).max(1), (ah / 2).max(1))
        };
        let own: gtk::Widget = self.overlay.clone().upcast();
        let Some(pointer) = WidgetExt::display(&self.window).default_seat().and_then(|s| s.pointer()) else { return center(&own) };
        let (surface, sx, sy) = pointer.surface_at_position();
        let clamp = |widget: gtk::Widget| {
            let (aw, ah) = Self::widget_size(&widget);
            let x = (sx as i32).clamp(0, (aw - 1).max(0));
            let y = (sy as i32).clamp(0, (ah - 1).max(0));
            (widget, x, y)
        };
        let Some(surface) = surface else { return center(&own) };
        if let Some(d) = self.desktops() {
            for view in d.views.borrow().iter() {
                if view.window.surface().as_ref() == Some(&surface) {
                    return clamp(view.overlay.clone().upcast());
                }
            }
        }
        clamp(own)
    }

    fn popup_at_pointer(&self, popover: &impl IsA<gtk::Popover>, remember: bool) {
        let (parent, x, y) = self.popover_parent_at_pointer();
        if remember {
            self.set_menu_anchor(&parent, x, y);
        }
        let popover: &gtk::Popover = popover.as_ref();
        popover.set_parent(&parent);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x, y, 1, 1)));
        let unparent = popover.clone();
        popover.connect_closed(move |_| {
            let p = unparent.clone();
            glib::idle_add_local_once(move || if p.parent().is_some() { p.unparent() });
        });
        popover.popup();
    }

    /// Wayland-safe confirmation popover at the pointer.
    pub fn show_confirmation_dialog(&self, title: &str, body: &str, buttons: &[String], callback: Rc<dyn Fn(Option<usize>, bool)>, check_label: Option<String>, on_dismiss: bool) {
        let request = ConfirmRequest { title: title.into(), body: body.into(), buttons: buttons.to_vec(), check_label: check_label.clone(), on_dismiss, callback: callback.clone() };
        if let Some(hook) = self.confirm_hook.borrow().clone() {
            return hook(request);
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(10);
        content.set_margin_bottom(10);
        content.set_margin_start(14);
        content.set_margin_end(14);
        let title_label = gtk::Label::new(Some(&format!("<b>{}</b>", glib::markup_escape_text(title))));
        title_label.set_use_markup(true);
        title_label.set_xalign(0.0);
        content.append(&title_label);
        let body_label = gtk::Label::new(Some(body));
        body_label.set_wrap(true);
        body_label.set_xalign(0.0);
        content.append(&body_label);
        let check = check_label.map(|l| gtk::CheckButton::with_label(&l));
        if let Some(check) = &check {
            content.append(check);
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.set_halign(gtk::Align::End);
        content.append(&row);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&content));
        let answered = Rc::new(Cell::new(false));
        let answer = {
            let (answered, popover, check) = (answered.clone(), popover.clone(), check.clone());
            Rc::new(move |index: Option<usize>| {
                if answered.replace(true) {
                    return;
                }
                popover.popdown();
                callback(index, check.as_ref().is_some_and(|c| c.is_active()));
            })
        };
        for (index, label) in buttons.iter().enumerate() {
            let button = gtk::Button::with_label(label);
            let answer = answer.clone();
            button.connect_clicked(move |_| answer(Some(index)));
            row.append(&button);
        }
        let dismiss = answer.clone();
        popover.connect_closed(move |_| {
            if on_dismiss && !answered.get() {
                dismiss(None);
            }
        });
        popover.set_has_arrow(true);
        popover.set_autohide(true);
        self.popup_at_pointer(&popover, false);
    }

    // ------------------------------------------------------------- trash

    pub fn action_empty_trash(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.show_confirmation_dialog(
            &tr!("Empty Trash"),
            &tr!("Are you sure you want to permanently delete all items in the Trash?"),
            &[tr!("Cancel"), tr!("Empty Trash")],
            Rc::new(move |index, _| {
                if index != Some(1) {
                    return;
                }
                let weak = weak.clone();
                fileops::empty_trash_async(None, move |error| {
                    if let Some(error) = error {
                        notify(&tr!("Could not empty the Trash"), &error);
                    }
                    if let Some(view) = weak.upgrade() {
                        view.refresh_desktop_icons();
                    }
                });
            }),
            None,
            false,
        );
    }

    pub fn delete_selected_item(self: &Rc<Self>) {
        let names: Vec<String> = self.state.borrow().selected_names.iter().cloned().collect();
        if names.is_empty() {
            return;
        }
        let message = if names.len() > 1 { tr!("Move {value1} items to Trash?", value1 = names.len() as i64) } else { tr!("Move '{value1}' to Trash?", value1 = names[0].as_str()) };
        let weak = Rc::downgrade(self);
        self.show_confirmation_dialog(
            &tr!("Move to Trash"),
            &message,
            &[tr!("Cancel"), tr!("Move to Trash")],
            Rc::new(move |index, _| {
                let Some(view) = weak.upgrade() else { return };
                if index != Some(1) {
                    return;
                }
                let desktop = config::desktop_dir();
                for name in view.selected_names() {
                    let path = desktop.join(&name);
                    if is_protected_name(&path.to_string_lossy()) || std::fs::symlink_metadata(&path).is_err() {
                        continue;
                    }
                    let weak = Rc::downgrade(&view);
                    fileops::trash_async(&path, move |result| {
                        if let Err(error) = result {
                            notify(&tr!("Could not move '{name}' to the Trash", name = name.as_str()), &error);
                        }
                        if let Some(view) = weak.upgrade() {
                            view.refresh_desktop_icons();
                        }
                    });
                }
                view.clear_selection();
                let weak = Rc::downgrade(&view);
                glib::idle_add_local_once(move || if let Some(v) = weak.upgrade() { v.refresh_desktop_icons() });
            }),
            None,
            false,
        );
    }

    // -------------------------------------------------------------- drops

    fn on_drop_received(self: &Rc<Self>, value: &glib::Value) -> bool {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let files = list.files();
        if files.is_empty() {
            return false;
        }
        self.state.borrow_mut().dropped_files = Some(files);
        let menu = gio::Menu::new();
        menu.append(Some(&tr!("Copy Here")), Some("win.drop_copy_here"));
        menu.append(Some(&tr!("Move Here")), Some("win.drop_move_here"));
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        popover.set_autohide(true);
        self.popup_at_pointer(&popover, true);
        true
    }

    pub fn set_dropped_files(&self, files: Vec<gio::File>) {
        self.state.borrow_mut().dropped_files = Some(files);
    }

    pub fn handle_drop(self: &Rc<Self>, mode: &str) {
        let Some(files) = self.state.borrow_mut().dropped_files.take() else { return };
        if mode == "copy" || mode == "move" {
            self.start_file_transfer(files, mode, None);
        }
    }

    // ------------------------------------------------------- item actions

    fn action_open_item(&self) {
        let path = self.state.borrow().context_item.clone();
        if let Some(path) = path {
            self.launch_path(&path);
        }
    }

    fn action_delete_item(self: &Rc<Self>) {
        if self.state.borrow().context_item.is_some() {
            self.delete_selected_item();
        }
    }

    /// `action_rename_item(param)`: an explicit path, else the context item.
    pub fn action_rename_item(self: &Rc<Self>, path: Option<&str>) {
        let target = path
            .filter(|p| Path::new(p).exists())
            .and_then(|p| self.items.borrow().iter().find(|i| i.fullpath == p).cloned())
            .or_else(|| {
                let context = self.state.borrow().context_item.clone()?;
                self.items.borrow().iter().find(|i| i.fullpath == context).cloned()
            });
        match target {
            Some(item) => self.start_rename(&item),
            None => eprintln!("Rename: no target item to rename"),
        }
    }

    pub fn start_rename(self: &Rc<Self>, item: &Rc<Item>) {
        let entry = gtk::Entry::new();
        EditableExt::set_text(&entry, &item.label.text());
        entry.set_hexpand(true);
        EntryExt::set_alignment(&entry, 0.5);
        item.highlight.remove(&item.label);
        item.highlight.append(&entry);
        entry.grab_focus();
        entry.select_region(0, -1);
        let (w, it) = (Rc::downgrade(self), item.clone());
        entry.connect_activate(move |entry| if let Some(v) = w.upgrade() { v.on_rename_commit(&entry.text(), &it) });
        let keys = gtk::EventControllerKey::new();
        let (w, it) = (Rc::downgrade(self), item.clone());
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(v) = w.upgrade() {
                    v.on_rename_cancel(&it, true);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        entry.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let (w, it) = (Rc::downgrade(self), item.clone());
        focus.connect_leave(move |_| if let Some(v) = w.upgrade() { v.on_rename_cancel(&it, true) });
        entry.add_controller(focus);
        let outside = gtk::GestureClick::new();
        outside.set_button(1);
        let (w, it, e) = (Rc::downgrade(self), item.clone(), entry.clone());
        outside.connect_released(move |gesture, _, _, _| {
            if !e.has_focus() {
                if let Some(v) = w.upgrade() {
                    v.on_rename_cancel(&it, true);
                    v.window.remove_controller(gesture);
                }
            }
        });
        self.window.add_controller(outside);
    }

    fn restore_label(item: &Item) {
        while let Some(child) = item.highlight.first_child() {
            item.highlight.remove(&child);
        }
        item.highlight.append(&item.label);
    }

    pub fn on_rename_cancel(&self, item: &Item, clear_selection: bool) {
        Self::restore_label(item);
        if clear_selection || item.highlight.has_css_class("selected") {
            item.highlight.remove_css_class("selected");
        }
    }

    /// `on_rename_commit`.
    pub fn on_rename_commit(self: &Rc<Self>, text: &str, item: &Rc<Item>) {
        let new_name = text.trim().to_owned();
        if new_name.is_empty() {
            return self.on_rename_cancel(item, true);
        }
        let old_text = item.label.text().to_string();
        let mut shown = new_name.clone();
        let path = item.fullpath.clone();
        let base = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if path.ends_with(".desktop") {
            // Launchers keep their file name; only their visible Name= changes.
            if let Err(error) = fileops::rename_desktop_entry(Path::new(&path), &new_name) {
                notify(&tr!("Could not rename '{name}'", name = old_text.as_str()), &suite_core::pyerr::python_error_text(&error));
                shown = old_text.clone();
            }
        } else if new_name != base {
            if let Some(message) = fileops::rename_error(&new_name) {
                notify(&tr!("Could not rename '{name}'", name = old_text.as_str()), &message);
                shown = old_text.clone();
            } else {
                // Reserve the old position under the new name first: the folder
                // monitor may reload icons before the rename callback runs.
                let mut positions = config::load_positions();
                if let Some(old) = positions.get(&base).cloned() {
                    if !positions.contains_key(&new_name) {
                        positions.insert(new_name.clone(), old);
                        config::save_all_positions(&positions);
                    }
                }
                let (weak, label, old, reserved) = (Rc::downgrade(self), item.label.clone(), old_text.clone(), new_name.clone());
                fileops::rename_async(Path::new(&path), &new_name, move |result| {
                    let view = weak.upgrade();
                    match result {
                        Ok(_) => {
                            if let Some(view) = view {
                                view.refresh_desktop_icons();
                            }
                        }
                        Err(error) => {
                            notify(&tr!("Could not rename '{name}'", name = old.as_str()), &error);
                            if view.as_ref().is_some_and(|v| !v.closed.get()) {
                                label.set_text(&old);
                            }
                            let mut positions = config::load_positions();
                            if positions.contains_key(&reserved) && std::fs::symlink_metadata(config::desktop_dir().join(&reserved)).is_err() {
                                positions.remove(&reserved);
                                config::save_all_positions(&positions);
                            }
                        }
                    }
                });
            }
        }
        item.label.set_text(&shown);
        Self::restore_label(item);
        item.highlight.remove_css_class("selected");
    }

    // -------------------------------------------------------------- drag

    /// `_pointer_global_xy` -> (gx, gy) of the pointer, if on one of our views.
    pub fn pointer_global_xy(&self) -> Option<(i64, i64)> {
        let pointer = WidgetExt::display(&self.window).default_seat()?.pointer()?;
        let (surface, sx, sy) = pointer.surface_at_position();
        let surface = surface?;
        let d = self.desktops()?;
        let views = d.views.borrow().clone();
        for view in views {
            if view.window.surface().as_ref() != Some(&surface) {
                continue;
            }
            let g = view.monitor.geometry();
            let (gx, gy) = if view.is_grid.get() { (view.origin.0 + sx as i64, view.origin.1 + sy as i64) } else { (g.x() as i64 + sx as i64, g.y() as i64 + sy as i64) };
            return Some((gx, gy));
        }
        None
    }

    fn on_drag_begin(self: &Rc<Self>, fullpath: &str) {
        let key = Path::new(fullpath).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(widget) = self.item(&key).map(|i| i.widget.clone()) else { return };
        if widget.parent().is_none() {
            return;
        }
        let names: Vec<String> = {
            let mut s = self.state.borrow_mut();
            s.drag_delta = (0, 0);
            s.drag_offset = (0.0, 0.0);
            s.multi_dragging = s.selected_names.contains(&key) && s.selected_names.len() > 1;
            s.dragging = true;
            s.rubber_drag_blocked = true;
            if s.multi_dragging { s.selected_names.iter().cloned().collect() } else { vec![key.clone()] }
        };
        let mut base = Vec::new();
        for name in names {
            if let Some(item) = self.item(&name) {
                let (x, y) = item.pos.get();
                base.push((name, (x as i64, y as i64)));
                item.widget.set_opacity(0.6);
            }
        }
        let start = self.pointer_global_xy();
        {
            let mut s = self.state.borrow_mut();
            s.drag_names = if s.multi_dragging { base.iter().map(|(n, _)| n.clone()).collect() } else { vec![key] };
            s.drag_base = base;
            s.drag_start_global = Some(start.unwrap_or((0, 0)));
        }
        let weak = Rc::downgrade(self);
        let path = fullpath.to_owned();
        let tick = glib::timeout_add_local(std::time::Duration::from_millis(16), move || match weak.upgrade() {
            Some(view) if view.drag_tick_step(&path) => glib::ControlFlow::Continue,
            Some(view) => {
                view.drag_tick.replace(None);
                glib::ControlFlow::Break
            }
            None => glib::ControlFlow::Break,
        });
        self.drag_tick.replace(Some(tick));
    }

    fn on_drag_update(&self, _fullpath: &str, offset_x: f64, offset_y: f64) {
        let mut s = self.state.borrow_mut();
        if !s.dragging {
            return;
        }
        s.drag_offset = (offset_x, offset_y);
    }

    fn current_delta(&self) -> (i64, i64) {
        let (start, offset) = {
            let s = self.state.borrow();
            (s.drag_start_global, s.drag_offset)
        };
        match (self.pointer_global_xy(), start) {
            (Some((gx, gy)), Some((sx, sy))) if (sx, sy) != (0, 0) || (gx, gy) != (0, 0) => (gx - sx, gy - sy),
            // No pointer position (e.g. headless): the gesture's offsets.
            _ => (offset.0 as i64, offset.1 as i64),
        }
    }

    fn drag_tick_step(&self, fullpath: &str) -> bool {
        if !self.state.borrow().dragging {
            return false;
        }
        let (dx, dy) = self.current_delta();
        let (multi, base, names) = {
            let mut s = self.state.borrow_mut();
            s.drag_delta = (dx, dy);
            (s.multi_dragging, s.drag_base.clone(), s.drag_names.clone())
        };
        if multi {
            for (name, (bx, by)) in &base {
                if let Some(item) = self.item(name) {
                    self.place(&item, (bx + dx) as f64, (by + dy) as f64);
                }
            }
        } else {
            let key = Path::new(fullpath).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (bx, by) = base.iter().find(|(n, _)| *n == key).map(|(_, p)| *p).unwrap_or((0, 0));
            if let Some(item) = self.item(&key) {
                self.place(&item, (bx + dx) as f64, (by + dy) as f64);
            }
        }
        // Ghosts on whichever grid each item lands in, by global position.
        let mut any_ghost = false;
        let Some(d) = self.desktops() else { return true };
        for name in names {
            let Some(source) = self.item(&name) else { continue };
            let (bx, by) = base.iter().find(|(n, _)| *n == name).map(|(_, p)| *p).unwrap_or((0, 0));
            let (gx, gy) = (self.origin.0 + bx + dx, self.origin.1 + by + dy);
            let Some(target) = d.find_grid_for_point(gx, gy) else { continue };
            if std::ptr::eq(Rc::as_ptr(&target), self) {
                continue;
            }
            let ghost = self.ensure_drag_ghost(&target, &name, &source);
            target.ghost_layer.move_(&ghost, (gx - target.origin.0).max(0) as f64, (gy - target.origin.1).max(0) as f64);
            ghost.set_opacity(0.6);
            any_ghost = true;
        }
        if !any_ghost {
            self.hide_all_ghosts();
        }
        true
    }

    fn ensure_drag_ghost(&self, target: &Rc<DesktopView>, name: &str, source: &Item) -> gtk::Box {
        let key = Rc::as_ptr(target) as usize;
        if let Some((_, _, ghost)) = self.state.borrow().ghosts.iter().find(|(k, n, g)| *k == key && n == name && g.parent().is_some()) {
            return ghost.clone();
        }
        let ghost = gtk::Box::new(gtk::Orientation::Vertical, 8);
        ghost.set_halign(gtk::Align::Start);
        ghost.set_valign(gtk::Align::Start);
        ghost.set_can_target(false);
        ghost.add_css_class("ghost-icon");
        let image = Self::image_for_file(&source.fullpath, 64);
        image.set_halign(gtk::Align::Center);
        let label = gtk::Label::new(Some(&source.display_name));
        label.set_wrap(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_halign(gtk::Align::Center);
        ghost.append(&image);
        ghost.append(&label);
        target.ghost_layer.put(&ghost, 0.0, 0.0);
        suite_gtk::utils::css_once(&WidgetExt::display(&self.window), "wayshell-ghost-icon", ".ghost-icon{background:transparent;border:none;box-shadow:none;}", gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        self.state.borrow_mut().ghosts.push((key, name.to_owned(), ghost.clone()));
        ghost
    }

    pub fn hide_all_ghosts(&self) {
        let ghosts = std::mem::take(&mut self.state.borrow_mut().ghosts);
        for (_, _, ghost) in ghosts {
            if let Some(parent) = ghost.parent().and_then(|p| p.downcast::<gtk::Fixed>().ok()) {
                parent.remove(&ghost);
            }
        }
    }

    pub fn stop_drag_tick(&self) {
        if let Some(id) = self.drag_tick.take() {
            id.remove();
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.state.borrow().dragging
    }

    pub fn set_dragging(&self, dragging: bool) {
        self.state.borrow_mut().dragging = dragging;
    }

    /// Tests: the state Python's test assigns before calling `on_drag_end`.
    pub fn set_drag_state(&self, multi: bool, base: Vec<(String, (i64, i64))>, delta: (i64, i64)) {
        let mut s = self.state.borrow_mut();
        s.dragging = true;
        s.multi_dragging = multi;
        s.drag_names = base.iter().map(|(n, _)| n.clone()).collect();
        s.drag_base = base;
        s.drag_delta = delta;
    }

    fn reset_drag(&self) {
        self.hide_all_ghosts();
        let mut s = self.state.borrow_mut();
        s.drag_base.clear();
        s.drag_names.clear();
        s.multi_dragging = false;
        s.dragging = false;
        s.rubber_drag_blocked = false;
        s.drag_delta = (0, 0);
    }

    /// `on_drag_end`: the target grid comes from the unsnapped global point.
    pub fn drag_end(self: &Rc<Self>, path: &str) {
        if !self.state.borrow().dragging {
            return;
        }
        let key = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(widget) = self.item(&key).map(|i| i.widget.clone()) else { return };
        if widget.parent().is_none() {
            return;
        }
        let live = self.drag_tick.borrow().is_some();
        self.stop_drag_tick();
        if live {
            let delta = self.current_delta();
            self.state.borrow_mut().drag_delta = delta;
        }
        let (multi, base, (dx, dy), preserved) = {
            let s = self.state.borrow();
            (s.multi_dragging, s.drag_base.clone(), s.drag_delta, s.selected_names.clone())
        };
        if multi {
            for (name, _) in &base {
                if let Some(item) = self.item(name) {
                    item.widget.set_opacity(1.0);
                }
            }
        } else {
            widget.set_opacity(1.0);
        }
        let d = self.desktops();
        let save = |name: &str, (bx, by): (i64, i64)| {
            let (gx, gy) = (bx + dx + self.origin.0, by + dy + self.origin.1);
            let target = d.as_ref().and_then(|d| d.find_grid_for_point(gx, gy)).unwrap_or_else(|| self.clone());
            let (mut tx, mut ty) = (gx - target.origin.0, gy - target.origin.1);
            if target.state.borrow().align_to_grid {
                (tx, ty) = target.snap_to_grid(tx, ty);
            }
            target.save_icon_position(name, tx, ty);
        };
        if multi && !base.is_empty() {
            for (name, origin) in &base {
                if self.item(name).is_some() {
                    save(name, *origin);
                }
            }
        } else {
            let current = self.item(&key).map(|i| i.pos.get()).unwrap_or_default();
            let origin = base.iter().find(|(n, _)| *n == key).map(|(_, p)| *p).unwrap_or_else(|| {
                let (x, y) = current;
                (x as i64, y as i64)
            });
            save(&key, origin);
        }
        self.reset_drag();
        let weak = self.desktops.clone();
        glib::idle_add_local_once(move || {
            let Some(d) = weak.upgrade() else { return };
            let grids = d.grid_views();
            for view in &grids {
                view.load_icons();
            }
            for view in &grids {
                for name in &preserved {
                    if let Some(item) = view.item(name) {
                        item.highlight.add_css_class("selected");
                        view.state.borrow_mut().selected_names.insert(name.clone());
                    }
                }
            }
        });
    }

    // ------------------------------------------------------------ menus

    fn on_background_right_click(self: &Rc<Self>, x: f64, y: f64) {
        if self.child_hit(x, y) {
            return;
        }
        super::menus::background_menu(self);
    }

    fn on_right_click(self: &Rc<Self>, display_name: &str, fullpath: &str) {
        self.state.borrow_mut().context_item = Some(fullpath.to_owned());
        if self.state.borrow().selected_names.len() > 1 {
            return super::menus::multi_menu(self);
        }
        if !self.state.borrow().selected_names.contains(display_name) {
            self.clear_selection();
            if let Some(item) = self.item_by_display_name(display_name) {
                self.state.borrow_mut().selected_names.insert(item.name.clone());
                self.apply_selection_visuals();
            }
        }
        super::menus::item_menu(self, fullpath);
    }

    pub(super) fn popup_menu(&self, popover: &gtk::PopoverMenu) {
        self.popup_at_pointer(popover, true);
    }

    // ------------------------------------------------------- wallpaper

    /// `set_monitor_wallpaper(monitor, file_path, mode, rgba)`.
    pub fn set_monitor_wallpaper(&self, monitor: &gdk::Monitor, file_path: Option<&str>, mode: Option<&str>, rgba: &gdk::RGBA) {
        let Some(d) = self.desktops() else { return };
        let mon_key = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("mon-{:p}", monitor.as_ptr()));
        // Solid colour via CSS (always applied, even behind videos/images).
        let css = format!("#overlay-bg-{mon_key} {{ background-color: {}; }}", rgba.to_str());
        if self.wallpaper_provider.borrow().is_none() {
            let provider = gtk::CssProvider::new();
            gtk::style_context_add_provider_for_display(&WidgetExt::display(&self.window), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            self.style_providers.borrow_mut().push(provider.clone());
            self.wallpaper_provider.replace(Some(provider));
        }
        if *self.wallpaper_css.borrow() != css {
            self.wallpaper_provider.borrow().as_ref().unwrap().load_from_string(&css);
            self.wallpaper_css.replace(css);
        }
        self.overlay.set_widget_name(&format!("overlay-bg-{mon_key}"));
        self.wallpaper_rgba.replace(Some(*rgba));
        let file_path = file_path.filter(|p| !p.is_empty());
        let Some(file_path) = file_path.filter(|_| mode != Some("Color Only")) else {
            self.wallpaper_texture.replace(None);
            d.stop_mpvpaper(&mon_key);
            self.background.set_paintable(None::<&gdk::Paintable>);
            return;
        };
        if wallpaper::is_video_path(file_path) {
            self.wallpaper_texture.replace(None);
            self.background.set_paintable(None::<&gdk::Paintable>);
            d.start_mpvpaper(&mon_key, &monitor.connector().map(|c| c.to_string()).unwrap_or_default(), file_path, mode.unwrap_or(""));
            return;
        }
        let result = d.wallpaper_texture(file_path);
        match result {
            Ok((texture, signature)) => {
                self.wallpaper_texture.replace(Some(texture.clone()));
                d.stop_mpvpaper(&mon_key);
                let current = self.background.paintable();
                if current.as_ref() != Some(texture.upcast_ref()) {
                    self.background.set_paintable(Some(&texture));
                }
                self.wallpaper_signature.replace(Some(signature));
            }
            Err(error) => return notify(&tr!("Could not set the wallpaper"), &error),
        }
        wallpaper::apply_fit(&self.background, mode);
    }

    /// `_load_saved_wallpaper`: paint this window's monitor from the config.
    pub fn load_saved_wallpaper(&self) {
        if !config::config_file().exists() {
            return;
        }
        let Ok(bytes) = std::fs::read(config::config_file()) else { return };
        let Ok(serde_json::Value::Object(data)) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return };
        let conn = self.monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("mon-{:p}", self.monitor.as_ptr()));
        let Some(cfg) = data.get(&conn).filter(|c| suite_core::pyjson::truthy(c)) else { return };
        let color = cfg.get("color").and_then(|c| c.as_str()).unwrap_or("black");
        let rgba = gdk::RGBA::parse(color).unwrap_or(gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        let file = cfg.get("file_path").and_then(|f| f.as_str());
        let mode = cfg.get("mode").and_then(|m| m.as_str());
        self.set_monitor_wallpaper(&self.monitor, file, mode, &rgba);
    }

    // ------------------------------------------------------------ destroy

    pub fn destroy(&self) {
        if self.closed.replace(true) {
            return;
        }
        if let Some(d) = self.desktops() {
            if let Some(id) = self.trash_listener.take() {
                if let Some(watcher) = d.trash_watcher_if_any() {
                    watcher.remove_listener(id);
                }
            }
            d.views.borrow_mut().retain(|v| !std::ptr::eq(Rc::as_ptr(v), self));
        }
        self.transfer.replace(None);
        if let Some(monitor) = self.desktop_monitor.take() {
            monitor.cancel();
        }
        self.stop_drag_tick();
        let items = std::mem::take(&mut *self.items.borrow_mut());
        for item in &items {
            self.release_item(item);
        }
        self.background.set_paintable(None::<&gdk::Paintable>);
        self.wallpaper_signature.replace(None);
        self.wallpaper_texture.replace(None);
        let display = WidgetExt::display(&self.window);
        for provider in self.style_providers.take() {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        self.wallpaper_provider.replace(None);
        self.window.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_round_and_helpers() {
        assert_eq!([py_round(0.5), py_round(1.5), py_round(2.5), py_round(-0.5), py_round(-1.5), py_round(0.49), py_round(2.51)], [0, 2, 2, 0, -2, 0, 3]);
        assert!(is_protected_name("/h/Desktop/Computer.desktop") && is_protected_name("trash-can.desktop"));
        assert!(!is_protected_name("/h/Desktop/notes.txt"));
        assert!(is_trash_item("Trash.desktop", "x") && is_trash_item("x", "My TRASH") && !is_trash_item("a", "b"));
        assert_eq!(wl_copy_argvs("cut", &["file:///a".into(), "file:///b".into()]), [
            vec!["wl-copy".to_string(), "-t".into(), "x-special/gnome-copied-files".into(), "cut\nfile:///a\nfile:///b".into()],
            vec!["wl-copy".to_string(), "-p".into(), "-t".into(), "text/uri-list".into(), "file:///a\nfile:///b".into()],
        ]);
    }
}
