//! The Tessel Dock application (tessel-dock.py `class Tessel Dock`, contract §1, §2.1,
//! §5): `Gtk.Application` `org.tessel.Dock` with `HANDLES_COMMAND_LINE`
//! (`IS_LAUNCHER` for probe/shortcut options, see [`crate::cli`]), startup
//! wiring of every backend, sd_notify readiness after startup AND the tray
//! host handshake, `org.tessel.Dock.Settings` (B's `SettingsApi`) with
//! `WorkareasChanged`, monitors → dock clones, the `--menu` output probe,
//! show desktop, trash watching, palette/gtk.css/window-button export (D's
//! `Publisher`), appearance previews, pins, launches, SIGTERM/SIGINT and the
//! orderly shutdown.
//!
//! API: [`main()`] (the binary), [`App`] (`Rc<App>`; Python method names:
//! `config`, `docks`, `backend`, `changed`, `rebuild_docks`, `monitor_entries`,
//! `save_config`, `toggle_pin`, `place_pin`, `move_pin`, `launch`,
//! `toggle_menu`, `toggle_show_desktop`, `close_popups`, `show_error`,
//! `show_settings`, `preview_appearance`, `restore_appearance`,
//! `refresh_backgrounds`, `export_palette`, `open_activities`,
//! `check_line`), [`AppRef`] (the `DockApp`/`SettingsHost`/`SettingsApp`
//! adapter holding a weak reference).

mod check;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::{json, Map, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use crate::appearance::export::{self, AppsAppearance, BarBorder, ExportSettings, Publisher, SurfaceSettings};
use crate::appearance::style::{appearance_css, CSS};
use crate::appearance::{AppearanceController, WallpaperPalette};
use crate::cli::{self, Command};
use crate::core::app_catalog::{AppCatalog, LaunchContext, LaunchHistory, Launcher};
use crate::core::config::{ConfigStore, DockConfig};
use crate::core::focused_title::FocusedTitle;
use crate::core::model::DockModel;
use crate::core::monitors::{monitor_entries, MonitorInfo, OutputInfo};
use crate::core::panel_actions::PanelActions;
use crate::core::trash::DesktopActions;
use crate::core::tray_prefs::{tray_entries, TrayEntry};
use crate::dbus::readiness::{ServiceReadiness, TrayReadiness};
use crate::dbus::settings_api::{self, DockInfo, SettingsApi, SettingsHost};
use crate::dbus::sni::StatusTrayBackend;
use crate::status::backend::{Status, StatusBackend, StatusOptions};
use crate::ui::host::{DockApp, DockView, TrayBackend};
use crate::ui::hot_corners::HotCorners;
use crate::ui::menu_probe::MenuProbe;
use crate::ui::settings::{AppearancePreview, SettingsApp, SettingsWindow};
use crate::ui::surface::{Background, DockSurface, Item};
use crate::wl::fullscreen_guard::FullscreenGuard;
use crate::wl::show_desktop::{self, ShowDesktop};
use crate::wl::{Window, WindowBackend, WorkspaceBackend};
use tessel_core::tr;

pub const APPLICATION_ID: &str = "org.tessel.Dock";

/// `_appearance_preview` & co.
#[derive(Clone, Debug, Default)]
struct Previews {
    appearance: Option<(i64, bool, bool, bool)>,
    frosted: Option<bool>,
    frost_strength: Option<i64>,
    border: Option<BarBorder>,
}

pub struct App {
    this: Weak<App>,
    application: gtk::Application,
    style_provider: OnceCell<gtk::CssProvider>,
    config: RefCell<Rc<DockConfig>>,
    store: RefCell<ConfigStore>,
    catalog: Rc<RefCell<AppCatalog>>,
    catalog_generation: Cell<u64>,
    history: RefCell<Option<Rc<RefCell<LaunchHistory>>>>,
    launcher: Launcher,
    model: RefCell<DockModel<Window>>,
    items: RefCell<Rc<Vec<Item>>>,
    docks: RefCell<Vec<Rc<DockSurface>>>,
    backend: RefCell<Option<WindowBackend>>,
    workspace_backend: RefCell<Option<WorkspaceBackend>>,
    status_backend: RefCell<Option<StatusBackend>>,
    tray_backend: RefCell<Option<Rc<StatusTrayBackend>>>,
    guard: RefCell<Option<FullscreenGuard>>,
    show_desktop: RefCell<ShowDesktop>,
    hot_corners: RefCell<Option<Rc<HotCorners>>>,
    actions: RefCell<Option<Rc<DesktopActions>>>,
    panel_actions: RefCell<Option<Rc<PanelActions>>>,
    appearance: RefCell<Option<AppearanceController>>,
    wallpaper: RefCell<Option<WallpaperPalette>>,
    publisher: RefCell<Option<Publisher>>,
    previews: RefCell<Previews>,
    settings: Rc<RefCell<Option<SettingsWindow>>>,
    settings_api: RefCell<Option<SettingsApi>>,
    readiness: ServiceReadiness,
    startup_complete: Cell<bool>,
    started: Cell<bool>,
    config_loaded: Cell<bool>,
    monitor_metadata: RefCell<HashMap<gdk::Monitor, (String, String)>>,
    monitor_signature: RefCell<Option<Vec<String>>>,
    /// The primary output Displays saved, and the watch on the file that holds it.
    primary_outputs: RefCell<Vec<String>>,
    primary_watch: RefCell<Option<(gio::FileMonitor, glib::SignalHandlerId)>>,
    trash_count: Cell<Option<u32>>,
    trash_timer: RefCell<Option<glib::SourceId>>,
    trash_refresh_timer: RefCell<Option<glib::SourceId>>,
    trash_query: RefCell<Option<gio::Cancellable>>,
    trash_monitor: RefCell<Option<(gio::FileMonitor, glib::SignalHandlerId)>>,
    catalog_timer: RefCell<Option<glib::SourceId>>,
    catalog_monitor: RefCell<Option<(gio::AppInfoMonitor, glib::SignalHandlerId)>>,
    display_signals: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    workarea_monitor_signals: RefCell<Vec<(gdk::Monitor, glib::SignalHandlerId)>>,
    signal_sources: RefCell<Vec<glib::SourceId>>,
    menu_probe: RefCell<Option<Rc<MenuProbe>>>,
    activities_request: RefCell<Option<gio::Subprocess>>,
    focused_title: Rc<RefCell<FocusedTitle>>,
    dock_app: OnceCell<Rc<AppRef>>,
}

/// Weak adapter implementing the widget host traits (no reference cycles).
pub struct AppRef(Weak<App>);

impl AppRef {
    fn app(&self) -> Option<Rc<App>> { self.0.upgrade() }
}

impl App {
    pub fn new(application: &gtk::Application) -> Rc<Self> {
        Rc::new_cyclic(|this| App {
            this: this.clone(), application: application.clone(), style_provider: OnceCell::new(),
            config: RefCell::new(Rc::new(DockConfig::default())), store: RefCell::new(ConfigStore::default()),
            catalog: Rc::new(RefCell::new(AppCatalog::from_infos(Vec::new()))), catalog_generation: Cell::new(0),
            history: RefCell::new(None), launcher: Launcher::default(), model: RefCell::new(DockModel::new()),
            items: RefCell::new(Rc::new(Vec::new())), docks: RefCell::default(), backend: RefCell::new(None),
            workspace_backend: RefCell::new(None), status_backend: RefCell::new(None), tray_backend: RefCell::new(None),
            guard: RefCell::new(None), show_desktop: RefCell::default(), hot_corners: RefCell::new(None),
            actions: RefCell::new(None), panel_actions: RefCell::new(None), appearance: RefCell::new(None),
            wallpaper: RefCell::new(None), publisher: RefCell::new(None), previews: RefCell::default(),
            settings: Rc::new(RefCell::new(None)), settings_api: RefCell::new(None), readiness: ServiceReadiness::new(),
            startup_complete: Cell::new(false), started: Cell::new(false), config_loaded: Cell::new(false), monitor_metadata: RefCell::default(),
            monitor_signature: RefCell::new(None), primary_outputs: RefCell::default(), primary_watch: RefCell::new(None), trash_count: Cell::new(None), trash_timer: RefCell::new(None),
            trash_refresh_timer: RefCell::new(None), trash_query: RefCell::new(None), trash_monitor: RefCell::new(None),
            catalog_timer: RefCell::new(None), catalog_monitor: RefCell::new(None), display_signals: RefCell::default(),
            workarea_monitor_signals: RefCell::default(), signal_sources: RefCell::default(), menu_probe: RefCell::new(None),
            activities_request: RefCell::new(None), focused_title: Rc::new(RefCell::new(FocusedTitle::new())),
            dock_app: OnceCell::new(),
        })
    }

    fn weak(&self) -> Weak<App> { self.this.clone() }

    // ---------------------------------------------------------------- accessors

    pub fn application(&self) -> gtk::Application { self.application.clone() }
    pub fn config(&self) -> Rc<DockConfig> { self.config.borrow().clone() }
    pub fn docks(&self) -> Vec<Rc<DockSurface>> { self.docks.borrow().clone() }
    pub fn backend(&self) -> Option<WindowBackend> { self.backend.borrow().clone() }
    pub fn workspace_backend(&self) -> Option<WorkspaceBackend> { self.workspace_backend.borrow().clone() }
    pub fn status_backend(&self) -> Option<StatusBackend> { self.status_backend.borrow().clone() }
    pub fn tray_backend(&self) -> Option<Rc<StatusTrayBackend>> { self.tray_backend.borrow().clone() }
    pub fn hot_corners(&self) -> Option<Rc<HotCorners>> { self.hot_corners.borrow().clone() }
    pub fn catalog(&self) -> Rc<RefCell<AppCatalog>> { self.catalog.clone() }
    pub fn catalog_generation(&self) -> u64 { self.catalog_generation.get() }
    pub fn launch_history(&self) -> Option<Rc<RefCell<LaunchHistory>>> { self.history.borrow().clone() }
    pub fn focused_title(&self) -> Rc<RefCell<FocusedTitle>> { self.focused_title.clone() }
    pub fn trash_count(&self) -> Option<u32> { self.trash_count.get() }
    pub fn settings_api(&self) -> Option<SettingsApi> { self.settings_api.borrow().clone() }
    pub fn settings_window(&self) -> Option<SettingsWindow> { self.settings.borrow().clone() }
    pub fn show_desktop_active(&self) -> bool { self.show_desktop.borrow().active() }
    pub fn menu_probe_active(&self) -> bool { self.menu_probe.borrow().is_some() }
    pub fn readiness_announced(&self) -> bool { self.readiness.announced() }
    pub fn desktop_actions(&self) -> Rc<DesktopActions> {
        self.actions.borrow().clone().unwrap_or_else(|| Rc::new(DesktopActions::new(None)))
    }
    pub fn dock_app(&self) -> Rc<AppRef> { self.dock_app.get_or_init(|| Rc::new(AppRef(self.weak()))).clone() }

    pub fn open_global_settings(&self) {
        if let Some(actions) = self.panel_actions.borrow().clone() {
            actions.open_global_settings();
        }
    }

    pub fn queue_workareas_changed(&self) {
        if let Some(api) = self.settings_api() {
            api.queue_workareas_changed();
        }
    }

    // ---------------------------------------------------------------- startup

    fn startup(self: &Rc<Self>) {
        // Before the first docks are placed.
        self.read_primary();
        let docks = self.weak();
        crate::ui::capture_controls::register_capture_controls(&self.application, Rc::new(move || {
            docks.upgrade().map(|app| app.docks().into_iter().map(|d| d as Rc<dyn DockView>).collect()).unwrap_or_default()
        }));
        if !gtk4_layer_shell::is_supported() {
            eprintln!("{}", tr!("The dock requires a Wayland compositor with layer-shell support."));
            std::process::exit(1);
        }
        let display = gdk::Display::default().expect("display");
        let provider = self.style_provider.get_or_init(gtk::CssProvider::new);
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&display, provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let weak = self.weak();
        let appearance = AppearanceController::new(Some(Box::new(move |controller: &AppearanceController| {
            if let Some(app) = weak.upgrade() {
                app.appearance_changed(controller.is_dark());
            }
        })), Default::default());
        self.appearance.replace(Some(appearance.clone()));
        appearance.start();
        self.catalog.replace(AppCatalog::new());
        self.history.replace(Some(Rc::new(RefCell::new(LaunchHistory::new(None)))));
        let monitor = gio::AppInfoMonitor::get();
        let weak = self.weak();
        let handler = monitor.connect_changed(move |_| if let Some(app) = weak.upgrade() { app.catalog_changed() });
        self.catalog_monitor.replace(Some((monitor, handler)));
        let config = {
            let catalog = self.catalog.borrow();
            self.store.borrow_mut().load(Some(&*catalog), None)
        };
        self.config.replace(Rc::new(config));
        self.config_loaded.set(true);
        let weak = self.weak();
        let wallpaper = WallpaperPalette::new(Some(Box::new(move |_: &WallpaperPalette| {
            if let Some(app) = weak.upgrade() {
                app.refresh_backgrounds();
            }
        })), Default::default());
        self.wallpaper.replace(Some(wallpaper.clone()));
        let weak = self.weak();
        self.publisher.replace(Some(Publisher::new(Some(Box::new(move || {
            // Delivered on GTK's main loop after the worker updated every variant.
            if let Some(appearance) = weak.upgrade().and_then(|app| app.appearance.borrow().clone()) {
                appearance.refresh_icons();
            }
        })))));
        self.sync_wallpaper_preferences();
        wallpaper.start();
        let weak = self.weak();
        self.actions.replace(Some(Rc::new(DesktopActions::new(Some(Rc::new(move |error: String| {
            if let Some(app) = weak.upgrade() {
                app.show_error(&error);
            }
        }))))));
        let (changed, failed) = (self.weak(), self.weak());
        let backend = WindowBackend::new(move || if let Some(app) = changed.upgrade() { app.changed() },
                                         move |message| if let Some(app) = failed.upgrade() { app.backend_error(message) });
        self.backend.replace(Some(backend.clone()));
        let source = self.weak();
        self.guard.replace(Some(FullscreenGuard::new(move || source.upgrade().and_then(|app| app.backend()).map(|b| b.snapshot()))));
        let (activate, blocked, fullscreen) = (self.weak(), self.weak(), self.weak());
        self.hot_corners.replace(Some(HotCorners::new(
            Some(self.application.clone()),
            Rc::new(move |monitor: &str| {
                if let Some(app) = activate.upgrade() {
                    app.open_activities(monitor);
                }
                Ok(())
            }),
            Rc::new(move || Ok(blocked.upgrade().is_some_and(|app| app.focused_fullscreen_state()))),
            Rc::new(move || fullscreen.upgrade().is_some_and(|app| app.native_fullscreen_active())),
        )));
        let weak = self.weak();
        self.panel_actions.replace(Some(Rc::new(PanelActions::new(move |error| {
            if let Some(app) = weak.upgrade() {
                app.show_error(&error);
            }
        }))));
        let (panel, failed, desktop) = (self.weak(), self.weak(), self.weak());
        let status = StatusBackend::new(
            move || if let Some(app) = panel.upgrade() { app.panel_changed() },
            Some(Box::new(move |message: &str| if let Some(app) = failed.upgrade() { app.backend_error(message) })),
            StatusOptions {
                on_show_desktop: Some(Rc::new(move || desktop.upgrade().is_some_and(|app| app.toggle_show_desktop()))),
                media: self.config().media_enabled(),
                ..Default::default()
            },
        );
        self.status_backend.replace(Some(status.clone()));
        let (panel, failed) = (self.weak(), self.weak());
        let workspaces = WorkspaceBackend::new(move || if let Some(app) = panel.upgrade() { app.panel_changed() },
                                               move |message| if let Some(app) = failed.upgrade() { app.backend_error(message) });
        self.workspace_backend.replace(Some(workspaces.clone()));
        let weak = self.weak();
        let tray = Rc::new(StatusTrayBackend::new(Some(Rc::new(move || if let Some(app) = weak.upgrade() { app.notify_ready() })), None));
        self.tray_backend.replace(Some(tray.clone()));
        // Held for the whole service lifetime (Python `self.hold()`).
        std::mem::forget(self.application.hold());
        status.start();
        workspaces.start();
        tray.start();
        backend.start();
        let model = display.monitors();
        let weak = self.weak();
        let handler = model.connect_items_changed(move |_, _, _, _| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || if let Some(app) = weak.upgrade() { app.rebuild_docks() });
        });
        self.display_signals.borrow_mut().push((model.upcast(), handler));
        self.rebuild_docks();
        self.watch_trash();
        self.watch_primary();
        self.refresh_trash();
        if let Some(connection) = self.application.dbus_connection() {
            let host: Rc<dyn SettingsHost> = self.dock_app();
            match SettingsApi::new(&connection, host) {
                Ok(api) => {
                    self.settings_api.replace(Some(api.clone()));
                    // The bus name is owned before startup has finished; announce
                    // once the object exists so racing clients recover.
                    api.queue_workareas_changed();
                }
                Err(error) => eprintln!("Tessel Dock: {error}"),
            }
        }
        for signal in [libc::SIGTERM, libc::SIGINT] {
            let weak = self.weak();
            let source = glib_unix::unix_signal_add_local(signal, move || {
                if let Some(app) = weak.upgrade() {
                    app.application.quit();
                }
                glib::ControlFlow::Continue
            });
            self.signal_sources.borrow_mut().push(source);
        }
        let error = self.store.borrow().error.clone();
        if !error.is_empty() {
            let weak = self.weak();
            glib::idle_add_local_once(move || {
                if let Some(app) = weak.upgrade() {
                    app.show_error(&tr!("Could not read dock settings: {value1}", value1 = error.as_str()));
                }
            });
        }
        self.startup_complete.set(true);
        self.started.set(true);
        self.notify_ready();
        // The first frames compile GPU pipelines and scan icon themes; the
        // freed transients stay in glibc's heap until trimmed. Trim once the
        // first frames are out and again after the deferred startup work
        // (icon prefetch, palette/papirus workers) has settled.
        tessel_gtk::utils::trim_heap_after(std::time::Duration::from_secs(4));
        glib::timeout_add_local_once(std::time::Duration::from_secs(30), || {
            tessel_gtk::utils::trim_heap_after(std::time::Duration::ZERO);
        });
    }

    /// Electron tray clients may give up when started before the watcher
    /// exists: labwc waits for the actual host handshake.
    fn notify_ready(&self) {
        let tray = self.tray_backend();
        let readiness: Option<&dyn TrayReadiness> = tray.as_deref().map(|t| t as &dyn TrayReadiness);
        if let Err(error) = self.readiness.update(self.startup_complete.get(), readiness) {
            eprintln!("Tessel Dock: {error}");
        }
    }

    fn appearance_changed(&self, is_dark: bool) {
        // Restyle existing widgets; rebuilding a hovered dock would lose clicks.
        if let Some(provider) = self.style_provider.get() {
            provider.load_from_string(&appearance_css(is_dark));
        }
        self.refresh_backgrounds();
    }

    fn is_dark(&self) -> bool { self.appearance.borrow().as_ref().map(|a| a.is_dark()).unwrap_or(true) }

    pub fn refresh_backgrounds(&self) {
        if !self.config_loaded.get() {
            return;
        }
        let config = self.config();
        let previews = self.previews.borrow().clone();
        let is_dark = self.is_dark();
        let border = previews.border.clone().unwrap_or_else(|| BarBorder {
            enabled: config.bar_border_enabled, sides: config.bar_border_sides.clone(), color: config.bar_border_color.clone() });
        let apps = AppsAppearance {
            effective_apps_transparency: config.effective_apps_transparency(), effective_apps_frosted: config.effective_apps_frosted(),
            apps_frost_strength: config.apps_frost_strength, apps_rounded_corners: config.apps_rounded_corners,
        };
        let wallpaper = self.wallpaper.borrow().clone();
        for dock in self.docks() {
            let separate = dock.is_app_dock;
            let surface_config = if separate { config.for_monitor(&dock.monitor_name).for_app_dock() } else { (*config).clone() };
            let surface = SurfaceSettings {
                effective_transparency: surface_config.effective_transparency(), match_wallpaper: surface_config.match_wallpaper,
                background_color: surface_config.background_color.clone(), accent_color: surface_config.accent_color.clone(),
                effective_rounded_corners: surface_config.effective_rounded_corners(),
                effective_float_mode: surface_config.effective_float_mode(), effective_frosted: surface_config.effective_frosted(),
                frost_strength: surface_config.frost_strength,
            };
            let color = wallpaper.as_ref().and_then(|w| w.color_for(&dock.monitor_name));
            let background = export::dock_background(is_dark, &surface, separate, previews.appearance, previews.frosted,
                                                     previews.frost_strength, &border, &apps, color);
            dock.set_background(&Background {
                is_dark, transparency: background.transparency, wallpaper: background.wallpaper,
                rounded_corners: Some(background.rounded_corners),
                apps_transparency: Some(background.apps.effective_apps_transparency),
                apps_frosted: Some(background.apps.effective_apps_frosted),
                apps_rounded_corners: Some(background.apps.apps_rounded_corners), frosted: Some(background.frosted),
                frost_strength: Some(background.frost_strength), apps_frost_strength: Some(background.apps.apps_frost_strength),
                background_color: Some(background.background_color.clone()), border_sides: Some(background.border_sides.clone()),
                border_color: Some(background.border_color.clone()), accent: Some(background.accent),
            });
            dock.set_float_mode(background.float_mode);
        }
        self.export_palette();
    }

    fn export_monitors(&self) -> Vec<export::MonitorInfo> {
        self.monitor_entries().into_iter().map(|(name, _label, monitor)| {
            let g = monitor.geometry();
            export::MonitorInfo { name, geometry: [g.x() as i64, g.y() as i64, g.width() as i64, g.height() as i64] }
        }).collect()
    }

    fn sync_wallpaper_preferences(&self) {
        let Some(wallpaper) = self.wallpaper.borrow().clone() else { return };
        let config = self.config();
        let preview = self.previews.borrow().appearance.map(|p| p.1);
        export::sync_wallpaper_preferences(&wallpaper, &self.export_monitors(), preview, config.match_wallpaper,
                                           config.separate_app_dock, config.app_dock_match_wallpaper);
    }

    pub fn export_palette(&self) {
        if !self.config_loaded.get() {
            return;
        }
        let config = self.config();
        let (transparency, match_wallpaper) = match self.previews.borrow().appearance {
            Some((t, m, _, _)) => (t, m),
            None => (config.effective_transparency(), config.match_wallpaper),
        };
        let settings = ExportSettings {
            transparency: transparency as f64, match_wallpaper, saved_match_wallpaper: config.match_wallpaper,
            apps_transparency: config.apps_transparency as f64, apps_transparency_enabled: config.apps_transparency_enabled,
            apps_frosted: config.apps_frosted, apps_frost_strength: config.apps_frost_strength as f64,
            apps_rounded_corners: config.apps_rounded_corners, colored_window_buttons: config.colored_window_buttons,
            window_buttons_left: config.window_buttons_left, window_shadows: config.window_shadows,
            accent_color: config.accent_color.as_deref().and_then(tessel_gtk::palette::parse_hex),
            apps_background_color: config.apps_background_color.as_deref().and_then(tessel_gtk::palette::parse_hex),
        };
        let monitors = self.export_monitors();
        let wallpaper = self.wallpaper.borrow().clone();
        let is_dark = self.is_dark();
        if let Some(publisher) = self.publisher.borrow_mut().as_mut() {
            publisher.export(is_dark, &settings, &monitors, wallpaper.as_ref());
        }
    }

    /// `preview_appearance(...)` (live, unsaved).
    #[allow(clippy::too_many_arguments)]
    pub fn preview_appearance(&self, transparency: i64, match_wallpaper: bool, rounded_corners: Option<bool>, float_mode: Option<bool>,
                              transparency_enabled: Option<bool>, frosted: Option<bool>, frost_strength: Option<i64>,
                              border_enabled: Option<bool>, border_sides: Option<Vec<String>>, border_color: Option<String>) {
        let config = self.config();
        let enabled = transparency_enabled.unwrap_or(config.transparency_enabled);
        {
            let mut previews = self.previews.borrow_mut();
            previews.frosted = Some(frosted.unwrap_or(config.frosted));
            previews.frost_strength = Some(frost_strength.unwrap_or(config.frost_strength));
            previews.border = Some(BarBorder {
                enabled: border_enabled.unwrap_or(config.bar_border_enabled),
                sides: border_sides.unwrap_or_else(|| config.bar_border_sides.clone()),
                color: border_color.unwrap_or_else(|| config.bar_border_color.clone()),
            });
            previews.appearance = Some((if enabled { transparency } else { 0 }, match_wallpaper,
                                        rounded_corners.unwrap_or_else(|| config.effective_rounded_corners()),
                                        float_mode.unwrap_or_else(|| config.effective_float_mode())));
        }
        self.sync_wallpaper_preferences();
        self.refresh_backgrounds();
    }

    pub fn restore_appearance(&self) {
        self.previews.replace(Previews::default());
        self.sync_wallpaper_preferences();
        self.refresh_backgrounds();
    }

    fn panel_changed(&self) {
        for dock in self.docks() {
            if let Some(indicators) = dock.indicators() {
                indicators.refresh();
            }
        }
    }

    // ---------------------------------------------------------------- CLI

    fn command_line(self: &Rc<Self>, command_line: &gio::ApplicationCommandLine) -> i32 {
        let args: Vec<String> = command_line.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        match cli::parse(&args) {
            Command::Unknown(arg) => {
                command_line.printerr_literal(&cli::unknown_option_message(&arg));
                2
            }
            Command::Check => {
                let (line, status) = self.check_line();
                command_line.print_literal(&line);
                status
            }
            Command::DesktopStatus => {
                command_line.print_literal(&show_desktop::desktop_status_line(self.show_desktop_active()));
                0
            }
            Command::Quit => {
                self.application.quit();
                0
            }
            Command::Settings => {
                self.show_settings();
                0
            }
            Command::Menu { hotkey } => if self.toggle_menu(hotkey) { 0 } else { 1 },
            Command::ToggleDesktop => if self.toggle_show_desktop() { 0 } else { 1 },
            Command::None => 0,
        }
    }

    /// Last-known focused fullscreen state; never blocks the main loop.
    pub fn focused_fullscreen_state(&self) -> bool {
        self.guard.borrow().clone().is_some_and(|guard| guard.state())
    }

    /// An activated, non-minimized native fullscreen window exists.
    pub fn native_fullscreen_active(&self) -> bool {
        self.backend().is_some_and(|b| b.windows().values().any(|w| w.activated && w.fullscreen && !w.minimized))
    }

    fn menu_hotkey_blocked(&self) -> bool { !self.config().menu_hotkey_fullscreen && self.focused_fullscreen_state() }

    /// Ask the existing shell to open its mouse-driven window chooser.
    pub fn open_activities(&self, monitor_name: &str) {
        if self.activities_request.borrow().is_some() {
            return;
        }
        self.close_popups();
        let launcher = tessel_core::paths::program("tessel-shell-cli");
        let argv: Vec<&std::ffi::OsStr> =
            [launcher.as_os_str(), "show-activities".as_ref(), "--monitor".as_ref(), monitor_name.as_ref()].into();
        let process = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE) {
            Ok(process) => process,
            Err(error) => {
                self.show_error(&tr!("Could not open Activities: {value1}", value1 = error.message()));
                return;
            }
        };
        self.activities_request.replace(Some(process.clone()));
        let weak = self.weak();
        let finished = process.clone();
        process.communicate_utf8_async(None, None::<&gio::Cancellable>, move |result| {
            let message = match result {
                Ok((_out, error)) if !finished.is_successful() => {
                    let text = error.map(|e| e.trim().to_owned()).unwrap_or_default();
                    if text.is_empty() { tr!("Activities could not be opened.") } else { text }
                }
                Ok(_) => String::new(),
                Err(error) => tr!("Could not open Activities: {value1}", value1 = error.message()),
            };
            let Some(app) = weak.upgrade() else { return };
            if app.activities_request.borrow().as_ref() == Some(&finished) {
                app.activities_request.replace(None);
                if !message.is_empty() {
                    app.show_error(&message);
                }
            }
        });
    }

    /// `--menu` / `--menu-hotkey`: open the existing menu on the interacted output.
    pub fn toggle_menu(self: &Rc<Self>, from_hotkey: bool) -> bool {
        if self.menu_probe.borrow().is_some() {
            // Two quick presses still toggle while the compositor assigns
            // the temporary surface to an output.
            self.cancel_menu_probe();
            return true;
        }
        if from_hotkey && self.menu_hotkey_blocked() {
            return true;
        }
        let docks = self.docks();
        if docks.is_empty() {
            return false;
        }
        if docks.len() == 1 {
            self.toggle_menu_on_monitor(Some(docks[0].monitor.clone()));
            return true;
        }
        // Wayland exposes no global pointer coordinates: let the compositor
        // choose the current output for an invisible surface.
        let weak = self.weak();
        let probe = MenuProbe::start(Some(&self.application), from_hotkey, Box::new(move |monitor, hotkey| {
            let Some(app) = weak.upgrade() else { return };
            app.menu_probe.replace(None);
            // Focus can change meanwhile: re-read only the cached verdict.
            let blocked = app.backend().is_some_and(|b| crate::wl::fullscreen_guard::last_state(Some(&b.snapshot())));
            if !hotkey || app.config().menu_hotkey_fullscreen || !blocked {
                app.toggle_menu_on_monitor(monitor);
            }
        }));
        self.menu_probe.replace(Some(probe));
        true
    }

    fn cancel_menu_probe(&self) {
        let probe = self.menu_probe.borrow_mut().take();
        if let Some(probe) = probe {
            probe.cancel();
        }
    }

    fn toggle_menu_on_monitor(&self, monitor: Option<gdk::Monitor>) {
        let docks: Vec<Rc<DockSurface>> = self.docks().into_iter().filter(|d| !d.is_destroyed()).collect();
        if docks.is_empty() {
            return;
        }
        let mut target = docks.iter().find(|d| Some(&d.monitor) == monitor.as_ref()).cloned();
        if target.is_none() {
            if let Some(backend) = self.backend() {
                let outputs: Vec<String> =
                    backend.windows().values().filter(|w| w.activated).flat_map(|w| w.outputs.clone()).collect();
                target = docks.iter().find(|d| outputs.contains(&d.monitor_name)).cloned();
            }
        }
        let target = target.unwrap_or_else(|| {
            let primary = self.primary_monitor();
            docks.iter().find(|d| Some(&d.monitor) == primary.as_ref()).cloned().unwrap_or_else(|| docks[0].clone())
        });
        let target = docks.iter().find(|d| d.monitor == target.monitor && !d.is_app_dock).cloned().unwrap_or(target);
        for dock in &docks {
            dock.close_preview();
            if !Rc::ptr_eq(dock, &target) {
                dock.close_app_menu();
            }
        }
        target.toggle_app_menu();
    }

    // ---------------------------------------------------------------- monitors

    /// `monitor_entries()`: (name, label, monitor).
    pub fn monitor_entries(&self) -> Vec<(String, String, gdk::Monitor)> {
        let monitors = crate::ui::surface::display_monitors();
        let infos: Vec<MonitorInfo<gdk::Monitor>> = monitors.iter().map(|m| {
            let g = m.geometry();
            MonitorInfo { key: m.clone(), geometry: (g.x(), g.y(), g.width(), g.height()),
                          connector: m.connector().map(|c| c.to_string()), model: m.model().map(|c| c.to_string()) }
        }).collect();
        let outputs: Vec<OutputInfo> = self.backend().map(|b| b.outputs().values().map(|o| OutputInfo {
            name: o.name.clone(), description: o.description.clone(),
            geometry: (o.x as i32, o.y as i32, o.width as i32, o.height as i32),
        }).collect()).unwrap_or_default();
        let mut previous = self.monitor_metadata.borrow_mut();
        monitor_entries(&infos, &outputs, &mut previous, |n| tr!("Display {value1}", value1 = n as i64))
            .into_iter().map(|e| (e.name, e.label, e.key)).collect()
    }

    pub fn rebuild_docks(self: &Rc<Self>) {
        let entries = self.monitor_entries();
        for (monitor, handler) in self.workarea_monitor_signals.borrow_mut().drain(..) {
            monitor.disconnect(handler);
        }
        let signals: Vec<(gdk::Monitor, glib::SignalHandlerId)> = entries.iter().map(|(_, _, monitor)| {
            let weak = self.weak();
            (monitor.clone(), monitor.connect_geometry_notify(move |_| if let Some(app) = weak.upgrade() { app.queue_workareas_changed() }))
        }).collect();
        self.workarea_monitor_signals.replace(signals);
        self.monitor_signature.replace(Some(entries.iter().map(|e| e.0.clone()).collect()));
        let old = std::mem::take(&mut *self.docks.borrow_mut());
        for dock in old {
            dock.destroy();
        }
        let config = self.config();
        let primary = self.primary_monitor();
        let selected: Vec<_> = match config.monitor.as_str() {
            "all" => entries.clone(),
            "primary" => {
                let primary = primary.or_else(|| entries.first().map(|e| e.2.clone()));
                entries.iter().filter(|e| Some(&e.2) == primary.as_ref()).cloned().collect()
            }
            name => {
                let selected: Vec<_> = entries.iter().filter(|e| e.0 == name).cloned().collect();
                // Keep controls accessible if a selected screen is unplugged.
                if selected.is_empty() { entries.iter().take(1).cloned().collect() } else { selected }
            }
        };
        let items = self.items.borrow().clone();
        for (name, _label, monitor) in selected {
            let dock = DockSurface::new(self, &monitor, &name, false, None);
            self.docks.borrow_mut().push(dock.clone());
            dock.render(items.clone());
            if dock.config.uses_separate_app_dock() {
                let app_dock = DockSurface::new(self, &monitor, &name, true, Some(&dock));
                self.docks.borrow_mut().push(app_dock.clone());
                app_dock.render(items.clone());
            }
        }
        if let Some(corners) = self.hot_corners() {
            let pairs: Vec<(String, gdk::Monitor)> = entries.iter().map(|e| (e.0.clone(), e.2.clone())).collect();
            corners.configure(&config, &pairs);
        }
        self.refresh_backgrounds();
        self.queue_workareas_changed();
    }

    pub fn changed(self: &Rc<Self>) {
        let Some(backend) = self.backend() else { return };
        {
            let mut state = std::mem::take(&mut *self.show_desktop.borrow_mut());
            let cleared = state.windows_changed(&backend);
            *self.show_desktop.borrow_mut() = state;
            if cleared {
                if let Some(status) = self.status_backend() {
                    status.set_show_desktop(false);
                }
            }
        }
        let windows: Vec<Window> = backend.windows().values().cloned().collect();
        let pins = self.config().pins.clone();
        let items = {
            let catalog = self.catalog.borrow();
            Rc::new(self.model.borrow_mut().update(&pins, &windows, &*catalog).to_vec())
        };
        self.items.replace(items.clone());
        // Keep the shortcut's fullscreen verdict current without blocking it.
        if let Some(guard) = self.guard.borrow().clone() {
            guard.windows_changed();
        }
        let signature: Vec<String> = self.monitor_entries().into_iter().map(|e| e.0).collect();
        if Some(&signature) != self.monitor_signature.borrow().as_ref() {
            self.rebuild_docks();
        } else {
            for dock in self.docks() {
                dock.render(items.clone());
            }
            // Output geometry can change without a connector change.
            self.export_palette();
        }
        if let Some(corners) = self.hot_corners() {
            corners.refresh();
        }
    }

    fn catalog_changed(&self) {
        if self.catalog_timer.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let source = glib::timeout_add_local_once(Duration::from_millis(350), move || {
            let Some(app) = weak.upgrade() else { return };
            app.catalog_timer.replace(None);
            app.catalog.borrow_mut().refresh();
            app.catalog_generation.set(app.catalog_generation.get() + 1);
            for dock in app.docks() {
                dock.signature_reset();
            }
            app.changed();
        });
        self.catalog_timer.replace(Some(source));
    }

    fn backend_error(&self, message: &str) {
        // The backend reconnects; avoid repeated dialogs on compositor exit.
        eprintln!("Tessel Dock: {message}");
    }

    // ---------------------------------------------------------------- pins, launch

    pub fn launch(&self, desktop_id: &str, context: Option<Rc<dyn LaunchContext>>) {
        let weak = self.weak();
        let show_error: Rc<dyn Fn(&str)> = Rc::new(move |message: &str| {
            if let Some(app) = weak.upgrade() {
                app.show_error(message);
            }
        });
        let history = self.launch_history();
        let catalog = self.catalog.borrow();
        crate::ui::app_menu::launch_app(&catalog, &self.launcher, history.as_deref(), desktop_id, context, show_error);
    }

    pub fn save_config(self: &Rc<Self>, config: DockConfig, rebuild: bool) -> bool {
        let result = self.store.borrow_mut().save(&config);
        match result {
            Ok(saved) => self.config.replace(Rc::new(saved)),
            Err(error) => {
                self.show_error(&tr!("Could not save dock settings: {value1}", value1 = error.as_str()));
                return false;
            }
        };
        // The one place the Media switch takes effect: every dock surface on
        // every monitor shares this status backend.
        if let Some(status) = self.status_backend() {
            status.set_media_enabled(self.config().media_enabled());
        }
        if rebuild {
            self.rebuild_docks();
        }
        self.sync_wallpaper_preferences();
        self.refresh_backgrounds();
        self.changed();
        true
    }

    fn same_app(&self, pin: &str, desktop_id: &str, entry: Option<&crate::core::app_catalog::AppEntry>) -> bool {
        pin == desktop_id || entry.is_some_and(|entry| self.catalog.borrow().resolve(pin, None) == Some(entry))
    }

    pub fn toggle_pin(self: &Rc<Self>, desktop_id: &str) {
        let config = self.config();
        let mut pins = config.pins.clone();
        let entry = self.catalog.borrow().get(desktop_id).cloned();
        match pins.iter().position(|pin| self.same_app(pin, desktop_id, entry.as_ref())) {
            Some(index) => {
                pins.remove(index);
            }
            None => pins.push(desktop_id.to_owned()),
        }
        self.save_config(DockConfig { pins, ..(*config).clone() }, false);
    }

    /// Pin or move an application before another pin (`None`: at the end).
    pub fn place_pin(self: &Rc<Self>, desktop_id: &str, before: Option<&str>) -> bool {
        let Some(entry) = self.catalog.borrow().get(desktop_id).cloned() else { return false };
        let config = self.config();
        let mut pins = config.pins.clone();
        let existing = pins.iter().find(|pin| self.same_app(pin, desktop_id, Some(&entry))).cloned();
        let target_entry = before.and_then(|b| self.catalog.borrow().get(b).cloned());
        let anchor = before.and_then(|before| pins.iter().find(|pin| self.same_app(pin, before, target_entry.as_ref())).cloned());
        if existing.is_some() && existing == anchor {
            return false;
        }
        if let Some(existing) = &existing {
            let index = pins.iter().position(|p| p == existing).expect("pin");
            pins.remove(index);
        }
        let value = existing.unwrap_or_else(|| desktop_id.to_owned());
        let index = anchor.and_then(|a| pins.iter().position(|p| *p == a)).unwrap_or(pins.len());
        pins.insert(index, value);
        if pins == config.pins {
            return false;
        }
        self.save_config(DockConfig { pins, ..(*config).clone() }, false)
    }

    pub fn move_pin(self: &Rc<Self>, desktop_id: &str, offset: i64) {
        let config = self.config();
        let mut pins = config.pins.clone();
        let Some(index) = pins.iter().position(|p| p == desktop_id) else { return };
        let target = (index as i64 + offset).clamp(0, pins.len() as i64 - 1) as usize;
        let pin = pins.remove(index);
        pins.insert(target, pin);
        self.save_config(DockConfig { pins, ..(*config).clone() }, false);
    }

    // ---------------------------------------------------------------- primary

    /// The primary display chosen in Displays, or the first one. Wayland has no
    /// primary output: Displays saves it in labwc's `environment`.
    pub fn primary_monitor(&self) -> Option<gdk::Monitor> {
        let monitors = crate::ui::surface::display_monitors();
        let names = self.primary_outputs.borrow();
        names.iter().find_map(|name| monitors.iter().find(|m| m.connector().is_some_and(|c| c == name.as_str())).cloned())
            .or_else(|| monitors.into_iter().next())
    }

    fn primary_path() -> std::path::PathBuf {
        tessel_core::paths::config_home().join("labwc").join("environment")
    }

    /// Read the saved primary; true if it changed.
    fn read_primary(&self) -> bool {
        let names = crate::appearance::gtk_color_scheme::saved_primary_outputs(&Self::primary_path()).unwrap_or_default();
        if *self.primary_outputs.borrow() == names {
            return false;
        }
        self.primary_outputs.replace(names);
        true
    }

    /// Move a dock shown on the primary display when Displays chooses another.
    fn watch_primary(&self) {
        let file = gio::File::for_path(Self::primary_path());
        // Displays replaces the file: follow renames into its name too.
        let Ok(monitor) = file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>) else { return };
        let weak = self.weak();
        let handler = monitor.connect_changed(move |_, _, _, _| {
            let Some(app) = weak.upgrade() else { return };
            if app.read_primary() && app.config().monitor == "primary" {
                app.rebuild_docks();
            }
        });
        self.primary_watch.replace(Some((monitor, handler)));
    }

    // ---------------------------------------------------------------- trash

    /// Follow Trash changes through GIO; poll only if it cannot be watched.
    fn watch_trash(&self) {
        match gio::File::for_uri("trash:///").monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            Ok(monitor) => {
                let weak = self.weak();
                let handler = monitor.connect_changed(move |_, _, _, _| if let Some(app) = weak.upgrade() { app.trash_changed() });
                self.trash_monitor.replace(Some((monitor, handler)));
            }
            Err(_) => {
                let weak = self.weak();
                let source = glib::timeout_add_seconds_local(10, move || {
                    if let Some(app) = weak.upgrade() {
                        app.refresh_trash();
                    }
                    glib::ControlFlow::Continue
                });
                self.trash_timer.replace(Some(source));
            }
        }
    }

    fn trash_changed(&self) {
        // Emptying or restoring many files emits a burst; count once after it.
        if self.trash_refresh_timer.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let source = glib::timeout_add_local_once(Duration::from_millis(300), move || {
            if let Some(app) = weak.upgrade() {
                app.trash_refresh_timer.replace(None);
                app.refresh_trash();
            }
        });
        self.trash_refresh_timer.replace(Some(source));
    }

    pub fn refresh_trash(&self) {
        if let Some(query) = self.trash_query.borrow_mut().take() {
            query.cancel();
        }
        let Some(actions) = self.actions.borrow().clone() else { return };
        let weak = self.weak();
        let slot: Rc<RefCell<Option<gio::Cancellable>>> = Rc::new(RefCell::new(None));
        let mine = slot.clone();
        let query = actions.trash_count(Box::new(move |count, _error| {
            if mine.borrow().as_ref().is_some_and(|c| c.is_cancelled()) {
                return;
            }
            let Some(app) = weak.upgrade() else { return };
            app.trash_count.set(count);
            for dock in app.docks() {
                dock.set_trash_count(count);
            }
        }));
        slot.replace(Some(query.clone()));
        self.trash_query.replace(Some(query));
    }

    pub fn confirm_empty_trash(&self) {
        self.close_popups();
        #[allow(deprecated)]
        let dialog = gtk::MessageDialog::new(self.settings_parent().as_ref(), gtk::DialogFlags::MODAL, gtk::MessageType::Question,
                                             gtk::ButtonsType::None, &tr!("Empty Trash?"));
        self.application.add_window(&dialog);
        dialog.add_css_class("tessel-dock-settings");
        #[allow(deprecated)]
        {
            dialog.set_secondary_text(Some(&tr!("Items in the Trash will be permanently deleted.")));
            dialog.add_button(&tr!("Cancel"), gtk::ResponseType::Cancel);
            dialog.add_button(&tr!("Empty Trash"), gtk::ResponseType::Accept);
            dialog.set_default_response(gtk::ResponseType::Cancel);
        }
        let weak = self.weak();
        #[allow(deprecated)]
        dialog.connect_response(move |widget, response| {
            widget.destroy();
            if response != gtk::ResponseType::Accept {
                return;
            }
            let Some(app) = weak.upgrade() else { return };
            let done = weak.clone();
            let result = app.desktop_actions().empty_trash(Box::new(move |error| {
                if let Some(app) = done.upgrade() {
                    match error {
                        Some(error) => app.show_error(&error),
                        None => app.refresh_trash(),
                    }
                }
            }));
            if let Err(error) = result {
                app.show_error(&error);
            }
        });
        tessel_gtk::theme::attach(&dialog, None);
        dialog.present();
    }

    // ---------------------------------------------------------------- desktop

    /// Restore only windows this button hid, preserving earlier minimization.
    pub fn toggle_show_desktop(&self) -> bool {
        let Some(backend) = self.backend() else { return false };
        let mut state = std::mem::take(&mut *self.show_desktop.borrow_mut());
        let result = state.toggle(&backend, || self.close_popups());
        *self.show_desktop.borrow_mut() = state;
        match result {
            None => false,
            Some(active) => {
                if let Some(status) = self.status_backend() {
                    status.set_show_desktop(active);
                }
                true
            }
        }
    }

    pub fn close_popups(&self) {
        self.cancel_menu_probe();
        for dock in self.docks() {
            dock.close_app_menu();
            dock.close_preview();
            if let Some(expander) = dock.application_expander() {
                expander.set_expanded(false);
            }
        }
    }

    fn settings_parent(&self) -> Option<gtk::Window> { self.settings_window().map(|w| w.window().clone().upcast()) }

    pub fn show_error(&self, message: &str) {
        self.close_popups();
        crate::ui::settings::error_dialog(Some(&self.application), self.settings_parent().as_ref(), message);
    }

    pub fn show_settings(&self) {
        self.close_popups();
        let app: Rc<dyn SettingsApp> = self.dock_app();
        crate::ui::settings::show_settings(&self.settings, app);
    }

    // ---------------------------------------------------------------- shutdown

    fn shutdown(&self) {
        if let Some(api) = self.settings_api.borrow_mut().take() {
            api.close();
        }
        self.cancel_menu_probe();
        if let Some(corners) = self.hot_corners() {
            corners.close();
        }
        if let Some(request) = self.activities_request.borrow_mut().take() {
            request.force_exit();
        }
        if let Some(wallpaper) = self.wallpaper.borrow().clone() {
            wallpaper.close();
        }
        if let Some(publisher) = self.publisher.borrow().as_ref() {
            publisher.close();
        }
        if let Some(appearance) = self.appearance.borrow().clone() {
            appearance.close();
        }
        if let Some(source) = self.catalog_timer.borrow_mut().take() {
            source.remove();
        }
        if let Some((monitor, handler)) = self.catalog_monitor.borrow_mut().take() {
            monitor.disconnect(handler);
        }
        for (object, handler) in self.display_signals.borrow_mut().drain(..) {
            object.disconnect(handler);
        }
        for (monitor, handler) in self.workarea_monitor_signals.borrow_mut().drain(..) {
            monitor.disconnect(handler);
        }
        if let Some(backend) = self.backend() {
            backend.close();
        }
        let docks = std::mem::take(&mut *self.docks.borrow_mut());
        for dock in docks {
            dock.destroy();
        }
        if let Some(status) = self.status_backend() {
            status.close();
        }
        if let Some(workspaces) = self.workspace_backend() {
            workspaces.close();
        }
        if let Some(tray) = self.tray_backend() {
            tray.close();
        }
        if let Some(source) = self.trash_timer.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.trash_refresh_timer.borrow_mut().take() {
            source.remove();
        }
        if let Some((monitor, handler)) = self.trash_monitor.borrow_mut().take() {
            monitor.disconnect(handler);
            monitor.cancel();
        }
        if let Some((monitor, handler)) = self.primary_watch.borrow_mut().take() {
            monitor.disconnect(handler);
            monitor.cancel();
        }
        if let Some(query) = self.trash_query.borrow_mut().take() {
            query.cancel();
        }
        if let Some(actions) = self.actions.borrow().as_ref() {
            actions.close();
        }
        for source in self.signal_sources.borrow_mut().drain(..) {
            source.remove();
        }
    }
}

// ------------------------------------------------------------------ adapters

impl DockApp for AppRef {
    fn application(&self) -> Option<gtk::Application> { self.app().map(|app| app.application()) }
    fn docks(&self) -> Vec<Rc<dyn DockView>> {
        self.app().map(|app| app.docks().into_iter().map(|d| d as Rc<dyn DockView>).collect()).unwrap_or_default()
    }
    fn status(&self) -> Status { self.app().and_then(|app| app.status_backend()).map(|s| s.status()).unwrap_or_default() }
    fn status_action(&self, name: &str) -> bool { self.app().and_then(|app| app.status_backend()).is_some_and(|s| s.action(name)) }
    fn audio_scroll(&self, step: f64) {
        if let Some(status) = self.app().and_then(|app| app.status_backend()) {
            status.audio_scroll(step);
        }
    }
    fn media_icon(&self) -> Option<gio::Icon> { self.app().and_then(|app| app.status_backend()).and_then(|s| s.media_icon()) }
    fn tray_backend(&self) -> Option<Rc<dyn TrayBackend>> {
        self.app().and_then(|app| app.tray_backend()).map(|t| t as Rc<dyn TrayBackend>)
    }
    fn panel_open(&self, name: &str, settings: bool, edge: &str, offset: i64) -> bool {
        let Some(app) = self.app() else { return false };
        let Some(actions) = app.panel_actions.borrow().clone() else { return false };
        actions.open(name, settings, edge, offset).unwrap_or_else(|error| {
            app.show_error(&error);
            false
        })
    }
    fn panel_capture(&self, action: &str) -> bool {
        let Some(app) = self.app() else { return false };
        let Some(actions) = app.panel_actions.borrow().clone() else { return false };
        actions.capture(action).unwrap_or_else(|error| {
            app.show_error(&error);
            false
        })
    }
    fn workspaces(&self, monitor: &str) -> Option<Vec<crate::wl::Workspace>> {
        self.app().and_then(|app| app.workspace_backend()).map(|w| w.sorted_workspaces(Some(monitor)))
    }
    fn activate_workspace(&self, id: &str) -> bool { self.app().and_then(|app| app.workspace_backend()).is_some_and(|w| w.activate(id)) }
    fn workspace_error(&self) -> String { self.app().and_then(|app| app.workspace_backend()).map(|w| w.error()).unwrap_or_default() }
    fn fullscreen_active(&self) -> bool { self.app().is_some_and(|app| app.native_fullscreen_active()) }
}

fn tray_entry_list(app: &App) -> Vec<TrayEntry> {
    let Some(tray) = app.tray_backend() else { return Vec::new() };
    let infos: Vec<_> = tray.items().iter().map(|item| crate::ui::indicators::tray_info(item)).collect();
    tray_entries(infos.iter())
}

impl SettingsHost for AppRef {
    fn config(&self) -> DockConfig { self.app().map(|app| (*app.config()).clone()).unwrap_or_default() }
    fn save_config(&self, config: DockConfig, rebuild: bool) -> bool { self.app().is_some_and(|app| app.save_config(config, rebuild)) }
    fn preview_appearance(&self, values: &Map<String, Value>) {
        let Some(app) = self.app() else { return };
        let boolean = |key: &str| values.get(key).and_then(Value::as_bool);
        let int = |key: &str| values.get(key).and_then(Value::as_i64);
        app.preview_appearance(int("transparency").unwrap_or(0), boolean("match_wallpaper").unwrap_or(false),
                               boolean("rounded_corners"), boolean("float_mode"), boolean("transparency_enabled"),
                               boolean("frosted"), int("frost_strength"), boolean("bar_border_enabled"),
                               values.get("bar_border_sides").and_then(Value::as_array)
                                   .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()),
                               values.get("bar_border_color").and_then(Value::as_str).map(str::to_owned));
    }
    fn restore_appearance(&self) {
        if let Some(app) = self.app() {
            app.restore_appearance();
        }
    }
    fn tray_apps(&self) -> Value {
        let entries = self.app().map(|app| tray_entry_list(&app)).unwrap_or_default();
        Value::Array(entries.into_iter().map(|e| json!({"id": e.id, "label": e.label, "running": e.running})).collect())
    }
    fn monitors(&self) -> Vec<settings_api::MonitorInfo> {
        self.app().map(|app| app.monitor_entries().into_iter().map(|(name, label, monitor)| {
            let g = monitor.geometry();
            settings_api::MonitorInfo { name, label, geometry: (g.x() as i64, g.y() as i64, g.width() as i64, g.height() as i64) }
        }).collect()).unwrap_or_default()
    }
    fn docks(&self) -> Vec<DockInfo> {
        self.app().map(|app| app.docks().into_iter().map(|dock| {
            let (w, h) = dock.dock_dimensions();
            DockInfo {
                monitor_name: dock.monitor_name.clone(), is_app_dock: dock.is_app_dock, bar_mode: dock.bar_mode,
                config: (*dock.config).clone(), dimensions: (w as i64, h as i64), horizontal: dock.horizontal,
                edge_gap: dock.edge_gap() as i64, headroom: dock.headroom() as i64,
                exclusive_zone: gtk4_layer_shell::LayerShell::exclusive_zone(dock.reservation()) as i64,
            }
        }).collect()).unwrap_or_default()
    }
}

impl SettingsApp for AppRef {
    fn config(&self) -> DockConfig { SettingsHost::config(self) }
    fn save_config(&self, config: DockConfig, rebuild: bool) -> bool { SettingsHost::save_config(self, config, rebuild) }
    fn preview_appearance(&self, preview: &AppearancePreview) {
        if let Some(app) = self.app() {
            app.preview_appearance(preview.transparency, preview.match_wallpaper, Some(preview.rounded_corners),
                                   Some(preview.float_mode), Some(preview.transparency_enabled), Some(preview.frosted),
                                   Some(preview.frost_strength), Some(preview.bar_border_enabled),
                                   Some(preview.bar_border_sides.clone()), Some(preview.bar_border_color.clone()));
        }
    }
    fn restore_appearance(&self) { SettingsHost::restore_appearance(self) }
    fn monitor_entries(&self) -> Vec<(String, String)> {
        self.app().map(|app| app.monitor_entries().into_iter().map(|(n, l, _)| (n, l)).collect()).unwrap_or_default()
    }
    fn tray_entries(&self) -> Vec<TrayEntry> { self.app().map(|app| tray_entry_list(&app)).unwrap_or_default() }
    fn subscribe_tray(&self, callback: Rc<dyn Fn()>) -> Option<u64> { self.app().and_then(|app| app.tray_backend()).map(|t| t.subscribe(callback)) }
    fn unsubscribe_tray(&self, id: u64) {
        if let Some(tray) = self.app().and_then(|app| app.tray_backend()) {
            tray.unsubscribe(id);
        }
    }
    fn appearance_is_dark(&self) -> Option<bool> { self.app().and_then(|app| app.appearance.borrow().as_ref().map(|a| a.is_dark())) }
    fn application(&self) -> Option<gtk::Application> { self.app().map(|app| app.application()) }
    fn show_error(&self, message: &str) {
        if let Some(app) = self.app() {
            app.show_error(message);
        }
    }
}

// ------------------------------------------------------------------ entry point

/// The `tessel-dock` binary.
pub fn main() -> i32 {
    tessel_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    glib::set_prgname(Some("tessel-dock"));
    glib::set_application_name("Dock");
    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if cli::is_launcher(&argv) {
        // Probes and keyboard shortcuts must never start a dock outside its
        // explicitly started desktop session.
        flags |= gio::ApplicationFlags::IS_LAUNCHER;
    }
    let application = gtk::Application::new(Some(APPLICATION_ID), flags);
    let app = App::new(&application);
    let weak = Rc::downgrade(&app);
    application.connect_startup(move |_| if let Some(app) = weak.upgrade() { app.startup() });
    let weak = Rc::downgrade(&app);
    application.connect_command_line(move |_, command_line| {
        let status = weak.upgrade().map(|app| app.command_line(command_line)).unwrap_or(1);
        glib::ExitCode::from(status as u8)
    });
    let weak = Rc::downgrade(&app);
    application.connect_shutdown(move |_| if let Some(app) = weak.upgrade() { app.shutdown() });
    let status = application.run_with_args(&argv);
    drop(app);
    i32::from(status.get())
}
