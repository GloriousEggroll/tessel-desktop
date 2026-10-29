//! Desktop layer (wayshell DesktopView + the desktop half of DesktopApp).
//! OWNED BY THE DESKTOP AGENT. The application (src/app.rs) uses only the
//! `Desktops` API below; keep these signatures stable.
pub mod chooser;
pub mod config;
pub mod icon_layout;
pub mod launch;
pub mod menus;
pub mod settings;
pub mod view;
pub mod wallpaper;
pub mod workarea;

use gio::prelude::*;
use gtk::gdk;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use icon_layout::{inset, next_slot, Point, Positions, Rect, Region};
use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::{Rc, Weak};
use view::DesktopView;
use workarea::{Areas, WorkareaClient, WorkareaSource};

pub struct Desktops {
    inner: Rc<DesktopsInner>,
}

impl Deref for Desktops {
    type Target = Rc<DesktopsInner>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Desktops {
    /// Created once in `startup`; nothing is shown yet.
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Rc::new(Self { inner: DesktopsInner::new(app) })
    }
    /// do_activate's desktop part (idempotent): one grid window per monitor,
    /// saved wallpaper, workarea client, monitor hotplug watching.
    pub fn start(self: &Rc<Self>) {
        self.inner.start();
    }
    /// True once the desktop windows exist (do_activate idempotence).
    pub fn started(&self) -> bool {
        !self.inner.grids.borrow().is_empty()
    }
    /// `_apply_wallpaper_to_keys` (wallpapers.json merge-write included).
    pub fn apply_wallpaper_to_keys(&self, keys: &[String], file_path: Option<&str>, mode: &str, color: &gdk::RGBA) {
        self.inner.apply_wallpaper_to_keys(keys, file_path, mode, color);
    }
    /// The wallpaper paintable currently shown on that connector (overview backdrop).
    pub fn wallpaper_paintable(&self, connector: &str) -> Option<gdk::Paintable> {
        self.inner.monitor_desktop(connector)?.background.paintable()
    }
    /// The wallpaper colour on that connector (overview backdrop when there is no image).
    pub fn wallpaper_color(&self, connector: &str) -> Option<gdk::RGBA> {
        *self.inner.monitor_desktop(connector)?.wallpaper_rgba.borrow()
    }
    /// do_shutdown's desktop part.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}

pub struct DesktopsInner {
    pub app: gtk::Application,
    /// Every live DesktopView (Python `app.get_windows()` filtered).
    pub views: RefCell<Vec<Rc<DesktopView>>>,
    /// `monitor_grids` == `monitor_desktops`, in insertion order.
    pub grids: RefCell<Vec<(String, Rc<DesktopView>)>>,
    pub textures: RefCell<wallpaper::TextureCache>,
    pub mpv: RefCell<wallpaper::MpvSupervisor>,
    refresh_id: RefCell<Option<glib::SourceId>>,
    workarea: RefCell<Option<Rc<dyn WorkareaSource>>>,
    workarea_refresh: RefCell<Option<glib::SourceId>>,
    icon_workareas: RefCell<Areas>,
    trash: RefCell<Option<crate::fileops::TrashWatcher>>,
    monitor_list: RefCell<Option<(gio::ListModel, glib::SignalHandlerId)>>,
    geometry_handlers: RefCell<Vec<(gdk::Monitor, glib::SignalHandlerId)>>,
    monitor_check: RefCell<Option<glib::SourceId>>,
    pub monitor_snapshot: RefCell<Vec<Rect>>,
    pub wallpaper_window: RefCell<Option<Rc<settings::WallpaperWindow>>>,
    pub wallpaper_style: RefCell<Option<gtk::CssProvider>>,
    /// Number of BACKGROUND→BOTTOM→BACKGROUND bounces started (tests).
    pub layer_bounces: Cell<u32>,
    /// Tests: called instead of `rebuild_desktops` after a topology change.
    pub rebuild_hook: RefCell<Option<Rc<dyn Fn()>>>,
    me: RefCell<Weak<DesktopsInner>>,
}

fn connector_key(monitor: &gdk::Monitor, index: u32) -> String {
    monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("Monitor-{}", index + 1))
}

fn rect_of(monitor: &gdk::Monitor) -> Rect {
    let g = monitor.geometry();
    (g.x() as i64, g.y() as i64, g.width() as i64, g.height() as i64)
}

impl DesktopsInner {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let inner = Rc::new(Self {
            app: app.clone(),
            views: RefCell::default(),
            grids: RefCell::default(),
            textures: RefCell::default(),
            mpv: RefCell::default(),
            refresh_id: RefCell::default(),
            workarea: RefCell::default(),
            workarea_refresh: RefCell::default(),
            icon_workareas: RefCell::default(),
            trash: RefCell::default(),
            monitor_list: RefCell::default(),
            geometry_handlers: RefCell::default(),
            monitor_check: RefCell::default(),
            monitor_snapshot: RefCell::default(),
            wallpaper_window: RefCell::default(),
            wallpaper_style: RefCell::default(),
            layer_bounces: Cell::new(0),
            rebuild_hook: RefCell::default(),
            me: RefCell::default(),
        });
        inner.me.replace(Rc::downgrade(&inner));
        inner
    }

    fn weak(&self) -> Weak<Self> {
        self.me.borrow().clone()
    }

    pub fn monitors(&self) -> Vec<(String, gdk::Monitor)> {
        gdk::Display::default().map(|d| crate::monitors::monitors_by_key(&d)).unwrap_or_default()
    }

    // ---------------------------------------------------------------- start

    pub fn start(&self) {
        if !self.grids.borrow().is_empty() {
            return;
        }
        for (index, (_, monitor)) in self.monitors().into_iter().enumerate() {
            self.make_grid_window(&monitor, index as u32);
        }
        if self.workarea.borrow().is_none() {
            let weak = self.weak();
            let client = WorkareaClient::new(move || if let Some(d) = weak.upgrade() { d.workareas_changed() });
            client.start();
            self.workarea.replace(Some(Rc::new(client)));
        }
        self.workareas_changed();
        self.monitor_snapshot.replace(self.snapshot_monitors());
        self.watch_monitors();
    }

    pub fn workarea_source(&self) -> Option<Rc<dyn WorkareaSource>> {
        self.workarea.borrow().clone()
    }

    pub fn set_workarea_source(&self, source: Option<Rc<dyn WorkareaSource>>) {
        self.workarea.replace(source);
    }

    // -------------------------------------------------------------- windows

    /// A view without layer-shell setup (Python `DesktopView(app, ...)`).
    pub fn make_view(&self, monitor: &gdk::Monitor, origin: (i64, i64), is_grid: bool) -> Rc<DesktopView> {
        let me = self.me.borrow().upgrade().expect("desktops alive");
        DesktopView::new(&me, monitor, origin, is_grid)
    }

    /// `monitor_grids[conn] = view` (and `monitor_desktops`).
    pub fn register_grid(&self, key: &str, view: &Rc<DesktopView>) {
        let mut grids = self.grids.borrow_mut();
        match grids.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = view.clone(),
            None => grids.push((key.to_owned(), view.clone())),
        }
    }

    pub fn monitor_desktop(&self, key: &str) -> Option<Rc<DesktopView>> {
        self.grids.borrow().iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    /// Live grid views (`get_windows()` DesktopViews with is_grid).
    pub fn grid_views(&self) -> Vec<Rc<DesktopView>> {
        self.views.borrow().iter().filter(|v| v.is_grid.get() && !v.closed.get()).cloned().collect()
    }

    /// `_make_grid_window`: the combined wallpaper + icon layer surface.
    pub fn make_grid_window(&self, monitor: &gdk::Monitor, index: u32) -> Rc<DesktopView> {
        let conn = connector_key(monitor, index);
        if let Some(existing) = self.monitor_desktop(&conn) {
            return existing;
        }
        let g = monitor.geometry();
        let view = self.make_view(monitor, (g.x() as i64, g.y() as i64), true);
        let label = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty()).unwrap_or_else(|| index.to_string());
        let window = &view.window;
        window.set_widget_name(&format!("desktop-grid-win-{label}"));
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!("\n            #{} {{ background-color: transparent; }}\n            #main-overlay {{ background-color: transparent; }}\n        ", window.widget_name()));
        gtk::style_context_add_provider_for_display(&WidgetExt::display(window), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        view.style_providers.borrow_mut().push(css);
        if self.app.is_registered() {
            window.set_application(Some(&self.app));
        }
        window.init_layer_shell();
        window.set_namespace(Some(&format!("desktop-grid-{label}")));
        window.set_layer(Layer::Background);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_monitor(Some(monitor));
        // Also the wallpaper surface: cover reserved panel strips too.
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.set_focusable(true);
        window.present();
        self.register_grid(&conn, &view);
        view.load_saved_wallpaper();
        view
    }

    pub fn rebuild_desktops(&self) {
        let views = self.views.borrow().clone();
        for view in views {
            if view.is_dragging() {
                view.set_dragging(false);
                view.stop_drag_tick();
                view.hide_all_ghosts();
            }
        }
        let views = self.views.borrow().clone();
        for view in views {
            view.destroy();
        }
        self.grids.borrow_mut().clear();
        for (index, (_, monitor)) in self.monitors().into_iter().enumerate() {
            self.make_grid_window(&monitor, index as u32);
        }
        self.workareas_changed();
    }

    pub fn raise_grids_over_wallpaper(&self) {
        self.layer_bounces.set(self.layer_bounces.get() + 1);
        for view in self.grid_views() {
            if !view.window.is_layer_window() {
                continue;
            }
            view.window.set_layer(Layer::Bottom);
            // Bounce back on idle so it ends where it started.
            let weak = Rc::downgrade(&view);
            glib::idle_add_local_once(move || {
                if let Some(view) = weak.upgrade() {
                    if !view.closed.get() {
                        view.window.set_layer(Layer::Background);
                    }
                }
            });
        }
    }

    // ------------------------------------------------------------ wallpaper

    pub fn wallpaper_texture(&self, path: &str) -> Result<(gdk::Texture, wallpaper::TextureKey), String> {
        self.textures.borrow_mut().get(path)
    }

    pub fn start_mpvpaper(&self, mon_key: &str, connector: &str, path: &str, mode: &str) -> bool {
        let environment = launch::labwc_launch_environment();
        let result = self.mpv.borrow_mut().start(mon_key, connector, path, mode, &environment);
        match result {
            Ok(()) => {
                // Raise the grids over the video a few times to beat mapping races.
                let weak = self.weak();
                glib::idle_add_local_once(move || if let Some(d) = weak.upgrade() { d.raise_grids_over_wallpaper() });
                for ms in [120u64, 300] {
                    let weak = self.weak();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || if let Some(d) = weak.upgrade() { d.raise_grids_over_wallpaper() });
                }
                true
            }
            Err(wallpaper::StartError::Missing) => {
                view::notify(&suite_core::tr!("Could not play the video wallpaper"), &suite_core::tr!("The video wallpaper player (mpvpaper) is not installed."));
                false
            }
            Err(wallpaper::StartError::Spawn(error)) => {
                view::notify(&suite_core::tr!("Could not play the video wallpaper"), &error);
                false
            }
        }
    }

    pub fn stop_mpvpaper(&self, mon_key: &str) {
        self.mpv.borrow_mut().stop(mon_key);
    }

    pub fn apply_wallpaper_to_keys(&self, keys: &[String], file_path: Option<&str>, mode: &str, color: &gdk::RGBA) {
        let monitors = self.monitors();
        for key in keys {
            let view = self.monitor_desktop(key);
            let monitor = monitors.iter().find(|(k, _)| k == key).map(|(_, m)| m.clone());
            if let (Some(view), Some(monitor)) = (view, monitor) {
                view.set_monitor_wallpaper(&monitor, file_path, Some(mode), color);
            }
        }
        if let Err(error) = config::merge_wallpapers(keys, file_path, mode, &color.to_str()) {
            eprintln!("Failed to save wallpapers: {error}");
        }
    }

    // --------------------------------------------------------------- trash

    pub fn trash_watcher(&self) -> crate::fileops::TrashWatcher {
        self.trash.borrow_mut().get_or_insert_with(crate::fileops::TrashWatcher::new).clone()
    }

    pub fn trash_watcher_if_any(&self) -> Option<crate::fileops::TrashWatcher> {
        self.trash.borrow().clone()
    }

    pub fn trash_count(&self) -> u32 {
        self.trash_watcher().count().unwrap_or(0)
    }

    // ------------------------------------------------------------- refresh

    /// Folder-monitor events coalesce for 100 ms; the refresh waits while a
    /// drag or rubber band is active.
    pub fn queue_desktop_refresh(&self) {
        if self.refresh_id.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(100), move || match weak.upgrade() {
            Some(d) if d.refresh_desktops() => glib::ControlFlow::Continue,
            _ => glib::ControlFlow::Break,
        });
        self.refresh_id.replace(Some(id));
    }

    pub fn refresh_pending(&self) -> bool {
        self.refresh_id.borrow().is_some()
    }

    /// Returns true to retry later.
    fn refresh_desktops(&self) -> bool {
        let views = self.grid_views();
        if views.iter().any(|v| {
            let s = v.state.borrow();
            s.dragging || s.rubber_drag_active
        }) {
            return true;
        }
        self.refresh_id.replace(None);
        // The primary grid assigns new files a global position: visit it first.
        let primary = self.primary_grid();
        let mut ordered = views;
        ordered.sort_by_key(|v| !primary.as_ref().is_some_and(|p| Rc::ptr_eq(p, v)));
        for view in ordered {
            view.load_icons();
        }
        false
    }

    // ------------------------------------------------------------- geometry

    pub fn desktop_bounds(&self) -> Rect {
        let (mut min_x, mut min_y, mut max_r, mut max_b) = (1_000_000_000i64, 1_000_000_000i64, -1_000_000_000i64, -1_000_000_000i64);
        for (_, monitor) in self.monitors() {
            let (x, y, w, h) = rect_of(&monitor);
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_r = max_r.max(x + w);
            max_b = max_b.max(y + h);
        }
        (min_x, min_y, max_r - min_x, max_b - min_y)
    }

    /// The leftmost, then topmost monitor's grid places new files.
    pub fn primary_grid(&self) -> Option<Rc<DesktopView>> {
        let mut best: Option<((i64, i64), Rc<DesktopView>)> = None;
        for (key, monitor) in self.monitors() {
            let Some(grid) = self.monitor_desktop(&key) else { continue };
            let (x, y, _, _) = rect_of(&monitor);
            if best.as_ref().map_or(true, |(t, _)| (x, y) < *t) {
                best = Some(((x, y), grid));
            }
        }
        best.map(|(_, g)| g)
    }

    pub fn icon_regions(&self) -> Vec<Region> {
        let source = self.workarea_source();
        let mut regions: Vec<Region> = self
            .monitors()
            .into_iter()
            .map(|(_, monitor)| {
                let full = rect_of(&monitor);
                let connector = monitor.connector().map(|c| c.to_string());
                let usable = match &source {
                    Some(s) => s.rectangle(connector.as_deref(), full),
                    None => full,
                };
                (full, inset(usable))
            })
            .collect();
        regions.sort_by_key(|r| (r.0 .0, r.0 .1));
        regions
    }

    /// Fill actual monitor workspaces, never panel strips or screen gaps.
    pub fn next_global_slot(&self, positions: &Positions) -> Point {
        next_slot(positions, &self.icon_regions())
    }

    pub fn find_grid_for_point(&self, gx: i64, gy: i64) -> Option<Rc<DesktopView>> {
        for (key, monitor) in self.monitors() {
            let (x, y, w, h) = rect_of(&monitor);
            if x <= gx && gx < x + w && y <= gy && gy < y + h {
                return self.monitor_desktop(&key);
            }
        }
        None
    }

    // ------------------------------------------------------------ workareas

    pub fn workareas_changed(&self) {
        if let Some(id) = self.workarea_refresh.take() {
            id.remove();
        }
        if self.grids.borrow().iter().any(|(_, v)| v.is_dragging()) {
            let weak = self.weak();
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                if let Some(d) = weak.upgrade() {
                    d.workarea_refresh.replace(None);
                    d.workareas_changed();
                }
            });
            self.workarea_refresh.replace(Some(id));
            return;
        }
        self.follow_workarea_moves();
        let grids: Vec<_> = self.grids.borrow().iter().map(|(_, v)| v.clone()).collect();
        for view in grids {
            view.load_icons();
        }
    }

    pub fn workarea_refresh_pending(&self) -> bool {
        self.workarea_refresh.borrow().is_some()
    }

    /// Carry icons along when a bar or dock changes edge (known areas only).
    fn follow_workarea_moves(&self) {
        let known = self.workarea_source().map(|s| s.known()).unwrap_or_default();
        let previous = self.icon_workareas.borrow().clone();
        let moves: Vec<(Rect, (i64, i64))> = known
            .iter()
            .filter_map(|(name, (geometry, area))| {
                let (pg, pa) = previous.get(name)?;
                (pg == geometry && (pa.0, pa.1) != (area.0, area.1)).then(|| (*geometry, (area.0 - pa.0, area.1 - pa.1)))
            })
            .collect();
        let mut merged = previous;
        merged.extend(known);
        self.icon_workareas.replace(merged);
        if !moves.is_empty() && !self.grids.borrow().is_empty() {
            let positions = config::load_positions();
            if !positions.is_empty() {
                config::save_all_positions(&icon_layout::shift_positions(&positions, &moves));
            }
        }
    }

    // ------------------------------------------------------------- monitors

    pub fn snapshot_monitors(&self) -> Vec<Rect> {
        self.monitors().iter().map(|(_, m)| rect_of(m)).collect()
    }

    pub fn watch_monitors(&self) {
        let Some(display) = gdk::Display::default() else { return };
        let list = display.monitors();
        let weak = self.weak();
        let handler = list.connect_items_changed(move |_, _, _, _| if let Some(d) = weak.upgrade() { d.monitors_changed() });
        self.monitor_list.replace(Some((list, handler)));
        self.watch_monitor_geometry();
    }

    fn watch_monitor_geometry(&self) {
        for (monitor, handler) in self.geometry_handlers.take() {
            monitor.disconnect(handler);
        }
        let Some(list) = self.monitor_list.borrow().as_ref().map(|(l, _)| l.clone()) else { return };
        let mut handlers = Vec::new();
        for index in 0..list.n_items() {
            let Some(monitor) = list.item(index).and_downcast::<gdk::Monitor>() else { continue };
            let weak = self.weak();
            let handler = monitor.connect_notify_local(Some("geometry"), move |_, _| if let Some(d) = weak.upgrade() { d.monitors_changed() });
            handlers.push((monitor, handler));
        }
        self.geometry_handlers.replace(handlers);
    }

    pub fn geometry_watch_count(&self) -> usize {
        self.geometry_handlers.borrow().len()
    }

    pub fn monitor_check_pending(&self) -> bool {
        self.monitor_check.borrow().is_some()
    }

    pub fn unwatch_monitors(&self) {
        if let Some(id) = self.monitor_check.take() {
            id.remove();
        }
        for (monitor, handler) in self.geometry_handlers.take() {
            monitor.disconnect(handler);
        }
        if let Some((list, handler)) = self.monitor_list.take() {
            list.disconnect(handler);
        }
    }

    /// One output change emits several signals; compare once they settle (250 ms).
    pub fn monitors_changed(&self) {
        if self.monitor_check.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(d) = weak.upgrade() {
                d.monitor_check.replace(None);
                d.check_monitor_changes();
            }
        });
        self.monitor_check.replace(Some(id));
    }

    fn check_monitor_changes(&self) {
        self.watch_monitor_geometry();
        let snapshot = self.snapshot_monitors();
        if snapshot != *self.monitor_snapshot.borrow() {
            self.monitor_snapshot.replace(snapshot);
            let weak = self.weak();
            glib::idle_add_local_once(move || {
                let Some(d) = weak.upgrade() else { return };
                let hook = d.rebuild_hook.borrow().clone();
                match hook {
                    Some(hook) => hook(),
                    None => d.rebuild_desktops(),
                }
            });
        }
    }

    // -------------------------------------------------------- view toggles

    pub fn set_hide_icons_all(&self, state: bool) {
        for view in self.grid_views() {
            view.state.borrow_mut().hide_icons_enabled = state;
            view.act_hide_icons.set_state(&state.to_variant());
            // Visibility only: forget invisible selections as well.
            view.clear_selection();
            if state {
                for item in view.items.borrow().iter() {
                    item.widget.set_visible(false);
                }
            } else {
                // A hidden desktop skips reloads: rebuild it on reveal.
                view.load_icons();
            }
        }
    }

    pub fn set_show_hidden_all(&self, state: bool) {
        for view in self.grid_views() {
            view.state.borrow_mut().show_hidden = state;
            view.act_show_hidden.set_state(&state.to_variant());
            view.load_icons();
        }
    }

    // ---------------------------------------------------------------- sort

    fn list_desktop_files(&self, show_hidden: bool) -> Vec<String> {
        let folder = gio::File::for_path(config::desktop_dir());
        let Ok(enumerator) = folder.enumerate_children("standard::name,standard::type", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) else { return vec![] };
        let mut files = Vec::new();
        while let Ok(Some(info)) = enumerator.next_file(gio::Cancellable::NONE) {
            let name = info.name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            files.push(name);
        }
        let _ = enumerator.close(gio::Cancellable::NONE);
        files
    }

    pub fn display_name_for(name: &str) -> String {
        let path = config::desktop_dir().join(name);
        if name.ends_with(".desktop") && path.is_file() {
            if let Some(display) = view::desktop_key(&path.to_string_lossy(), "Name").filter(|n| !n.is_empty()) {
                return display;
            }
        }
        crate::fileops::splitext(name).0.to_owned()
    }

    pub fn type_key_for(name: &str) -> (String, String) {
        let path = config::desktop_dir().join(name);
        let lower = name.to_lowercase();
        if path.is_dir() {
            return ("0_folder".into(), lower);
        }
        if name.ends_with(".desktop") && path.is_file() {
            let kind = view::desktop_key(&path.to_string_lossy(), "Type").filter(|t| !t.is_empty()).unwrap_or_else(|| "desktop".into());
            let group = match kind.as_str() {
                "Application" => "1_application",
                "Link" => "2_link",
                "Directory" => "3_directory",
                "Service" => "4_service",
                "desktop" => "9_desktop_unknown",
                _ => "9_other",
            };
            return (group.into(), lower);
        }
        let ext = crate::fileops::splitext(name).1.to_lowercase();
        let group = match ext.as_str() {
            ".txt" | ".md" | ".rtf" => "5_text",
            ".png" | ".jpg" | ".jpeg" | ".webp" => "6_image",
            ".pdf" => "7_pdf",
            ".mp4" | ".mkv" | ".avi" => "8_video",
            _ => "9_other",
        };
        (group.into(), lower)
    }

    fn write_positions_vertical_fill(&self, sorted: &[String]) {
        let regions = self.icon_regions();
        let mut positions = Positions::new();
        for name in sorted {
            let slot = next_slot(&positions, &regions);
            positions.insert(name.clone(), icon_layout::point_value(slot));
        }
        config::write_all_positions(&positions);
        for view in self.grid_views() {
            view.load_icons();
        }
    }

    fn sort_show_hidden(&self) -> bool {
        self.primary_grid().map(|p| p.show_hidden()).unwrap_or(false)
    }

    pub fn sort_all_by_name(&self) {
        let mut files = self.list_desktop_files(self.sort_show_hidden());
        files.sort_by_cached_key(|n| Self::display_name_for(n).to_lowercase());
        self.write_positions_vertical_fill(&files);
    }

    pub fn sort_all_by_type(&self) {
        let mut files = self.list_desktop_files(self.sort_show_hidden());
        files.sort_by_cached_key(|n| Self::type_key_for(n));
        self.write_positions_vertical_fill(&files);
    }

    // ------------------------------------------------------------ shutdown

    pub fn shutdown(&self) {
        if let Some(id) = self.refresh_id.take() {
            id.remove();
        }
        let views = self.views.borrow().clone();
        for view in views {
            view.destroy();
        }
        self.grids.borrow_mut().clear();
        if let Some(id) = self.workarea_refresh.take() {
            id.remove();
        }
        if let Some(source) = self.workarea.take() {
            source.close();
        }
        self.unwatch_monitors();
        if let Some(trash) = self.trash.take() {
            trash.close();
        }
        if let Some(window) = self.wallpaper_window.take() {
            window.window.destroy();
        }
    }
}
