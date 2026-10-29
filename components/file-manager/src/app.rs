//! files_ui.FilesApplication: the unique `org.waydock.Files` GApplication,
//! shared preferences/bookmarks/operations/thumbnails/jobs, clipboard state,
//! command-line handling and session restore; plus `main`.

use crate::bridge::{Bookmarks, FilePreferences, GTKBookmarks, Prefs};
use crate::cli;
use crate::thumbnails::Thumbnails;
use crate::window::{FilesWindow, FolderTab};
use files_model::bus::{FileManagerBus, FileManagerHost};
use files_ops::operations::Operations;
use files_widgets::jobs::{JobManager, KeepAnswer};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

pub const APPLICATION_ID: &str = "org.waydock.Files";

/// Test hooks standing in for Python's `unittest.mock.patch` targets.
#[derive(Default)]
pub struct Hooks {
    /// `file_launch.launch_terminal`.
    pub launch_terminal: RefCell<Option<Rc<dyn Fn(&gio::File) -> Result<(), String>>>>,
    /// `app_catalog.launch_uri` (opening files).
    pub open_files: RefCell<Option<Rc<dyn Fn(&gio::File)>>>,
    /// `files_ui.confirm(parent, title, message, callback)`.
    pub confirm: RefCell<Option<Rc<dyn Fn(&str, &str, Box<dyn FnOnce(bool)>)>>>,
    /// `files_ui.ask_keep_running(parent, callback)`.
    pub ask_keep_running: RefCell<Option<Rc<dyn Fn(Box<dyn FnOnce(Option<KeepAnswer>)>)>>>,
    /// `patch.object(window, 'action', ...)`: return true to swallow the action.
    pub action: RefCell<Option<Rc<dyn Fn(&str) -> bool>>>,
    /// `patch.object(window, 'run_operation')`.
    pub run_operation: RefCell<Option<Rc<dyn Fn(&str, &[gio::File], Option<&gio::File>) -> Option<Rc<files_widgets::jobs::JobEntry>>>>>,
    /// `patch.object(file_goto, 'FolderIndex', ...)`.
    pub folder_index: RefCell<Option<Rc<dyn Fn() -> files_model::goto::FolderIndex>>>,
    /// Delay (ms) before a command-line location's classification is delivered.
    pub classify_delay: RefCell<Option<Rc<dyn Fn(&gio::File) -> u64>>>,
}

pub struct AppInner {
    pub app: gtk::Application,
    preferences: RefCell<Prefs>,
    bookmarks: RefCell<Bookmarks>,
    operations: RefCell<Operations>,
    pub thumbnails: Thumbnails,
    pub jobs: JobManager,
    pub clipboard_files: RefCell<Vec<gio::File>>,
    pub clipboard_cut: Cell<bool>,
    pub clipboard_provider: RefCell<Option<gdk::ContentProvider>>,
    pub clipboard_generation: Cell<u64>,
    clipboard_signal: RefCell<Option<glib::SignalHandlerId>>,
    pub session_restored: Cell<bool>,
    pub hooks: Hooks,
    appearance: RefCell<Option<waydock::appearance::AppearanceController>>,
    pub file_manager_bus: RefCell<Option<FileManagerBus>>,
}

#[derive(Clone)]
pub struct FilesApplication(pub Rc<AppInner>);

impl std::ops::Deref for FilesApplication {
    type Target = AppInner;
    fn deref(&self) -> &AppInner {
        &self.0
    }
}

#[derive(Clone)]
pub struct WeakApp(Weak<AppInner>);

impl WeakApp {
    pub fn upgrade(&self) -> Option<FilesApplication> {
        self.0.upgrade().map(FilesApplication)
    }
}

thread_local! {
    static APP: RefCell<Weak<AppInner>> = const { RefCell::new(Weak::new()) };
    static WINDOWS: RefCell<Vec<FilesWindow>> = const { RefCell::new(Vec::new()) };
}

/// The FilesWindow owning `widget` (Python `isinstance(widget.get_root(), FilesWindow)`).
pub fn files_window_for(widget: &impl IsA<gtk::Widget>) -> Option<FilesWindow> {
    let root = widget.as_ref().root()?;
    WINDOWS.with(|w| w.borrow().iter().find(|window| window.window.upcast_ref::<gtk::Widget>() == root.upcast_ref::<gtk::Widget>()).cloned())
}

/// `GioUnix.MountMonitor.get()`.
pub fn unix_mount_monitor() -> Option<glib::Object> {
    extern "C" {
        fn g_unix_mount_monitor_get() -> *mut glib::gobject_ffi::GObject;
    }
    unsafe {
        let pointer = g_unix_mount_monitor_get();
        if pointer.is_null() {
            None
        } else {
            Some(glib::translate::from_glib_full(pointer))
        }
    }
}

/// Ctrl+Q: one running-jobs prompt for the whole app, then close every window.
pub fn quit_files(app: &FilesApplication) {
    let windows: Vec<FilesWindow> = app.files_windows().into_iter().filter(|w| w.alive.get()).collect();
    let close_all = {
        let (app, windows) = (app.clone(), windows.clone());
        move |choice: Option<KeepAnswer>| {
            if choice == Some(KeepAnswer::Cancel) {
                app.jobs.cancel_all(Some(&app.jobs.entries()));
            }
            for window in &windows {
                window.close_confirmed.set(true);
                window.window.close();
            }
        }
    };
    if !app.jobs.entries().is_empty() && !windows.is_empty() {
        let parent = app.app.active_window().or_else(|| windows.first().map(|w| w.gtk_window()));
        app.ask_keep_running(parent.as_ref(), Box::new(move |choice| {
            if choice.is_some() {
                glib::idle_add_local_once(move || close_all(choice));
            }
        }));
    } else {
        close_all(None);
    }
}

impl FilesApplication {
    pub fn new() -> Self {
        let app = gtk::Application::builder().application_id(APPLICATION_ID).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
        let jobs = JobManager::new(&app);
        let inner = Rc::new(AppInner {
            app, preferences: RefCell::new(Rc::new(RefCell::new(FilePreferences::new(None)))),
            bookmarks: RefCell::new(Rc::new(RefCell::new(GTKBookmarks::new(None)))), operations: RefCell::new(Operations::new()),
            thumbnails: Thumbnails::new(), jobs, clipboard_files: RefCell::new(Vec::new()), clipboard_cut: Cell::new(false),
            clipboard_provider: RefCell::new(None), clipboard_generation: Cell::new(0), clipboard_signal: RefCell::new(None),
            session_restored: Cell::new(false), hooks: Hooks::default(), appearance: RefCell::new(None), file_manager_bus: RefCell::new(None),
        });
        APP.with(|a| *a.borrow_mut() = Rc::downgrade(&inner));
        let this = FilesApplication(inner);
        let weak = this.downgrade();
        this.app.connect_startup(move |_| {
            if let Some(app) = weak.upgrade() {
                app.startup()
            }
        });
        let weak = this.downgrade();
        this.app.connect_command_line(move |_, command| match weak.upgrade() {
            Some(app) => app.command_line(command),
            None => glib::ExitCode::FAILURE,
        });
        let weak = this.downgrade();
        this.app.connect_activate(move |_| {
            if let Some(app) = weak.upgrade() {
                let window = FilesWindow::new(&app);
                window.new_tab(Some(&crate::pane::home_file()), None);
                window.window.present();
            }
        });
        let weak = this.downgrade();
        this.app.connect_shutdown(move |_| {
            if let Some(app) = weak.upgrade() {
                app.shutdown()
            }
        });
        this
    }

    pub fn downgrade(&self) -> WeakApp {
        WeakApp(Rc::downgrade(&self.0))
    }

    pub fn preferences(&self) -> Prefs {
        self.preferences.borrow().clone()
    }
    pub fn set_preferences(&self, prefs: FilePreferences) {
        *self.preferences.borrow_mut() = Rc::new(RefCell::new(prefs));
    }
    pub fn bookmarks(&self) -> Bookmarks {
        self.bookmarks.borrow().clone()
    }
    pub fn set_bookmarks(&self, bookmarks: GTKBookmarks) {
        *self.bookmarks.borrow_mut() = Rc::new(RefCell::new(bookmarks));
    }
    pub fn operations(&self) -> Operations {
        self.operations.borrow().clone()
    }
    pub fn set_operations(&self, operations: Operations) {
        *self.operations.borrow_mut() = operations;
    }

    pub fn register_window(&self, window: &FilesWindow) {
        WINDOWS.with(|w| w.borrow_mut().push(window.clone()));
    }

    pub fn unregister_window(&self, window: &FilesWindow) {
        WINDOWS.with(|w| w.borrow_mut().retain(|other| other != window));
    }

    /// Every FilesWindow of the application (Python filters `get_windows()`).
    pub fn files_windows(&self) -> Vec<FilesWindow> {
        WINDOWS.with(|w| w.borrow().clone())
    }

    pub fn ask_keep_running(&self, parent: Option<&gtk::Window>, callback: Box<dyn FnOnce(Option<KeepAnswer>)>) {
        if let Some(hook) = self.hooks.ask_keep_running.borrow().clone() {
            hook(callback);
            return;
        }
        files_widgets::jobs::ask_keep_running(parent, callback);
    }

    fn startup(&self) {
        let appearance = waydock::appearance::AppearanceController::new(None, Default::default());
        appearance.start();
        *self.appearance.borrow_mut() = Some(appearance);
        let display = gdk::Display::default();
        suite_gtk::utils::add_css(files_widgets::style::CSS, display.as_ref(), suite_gtk::utils::PRIORITY_APPLICATION);
        // Override only navigation chrome; colors still inherit the suite palette.
        suite_gtk::utils::add_css(files_widgets::style::NAVIGATION_CSS, display.as_ref(), suite_gtk::utils::PRIORITY_APPLICATION + 31);
        suite_gtk::theme::attach_application(&self.app);
        if let Some(display) = &display {
            let weak = self.downgrade();
            let id = display.clipboard().connect_changed(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.clipboard_generation.set(app.clipboard_generation.get() + 1);
                }
            });
            *self.clipboard_signal.borrow_mut() = Some(id);
        }
        let shortcuts: &[(&str, &[&str])] = &[
            ("new-tab", &["<Primary>t"]), ("new-window", &["<Primary>n"]), ("close-tab", &["<Primary>w"]),
            ("reopen-tab", &["<Primary><Shift>t"]), ("split", &["F3"]), ("location", &["<Primary>l"]),
            ("back", &["<Alt>Left"]), ("forward", &["<Alt>Right"]), ("up", &["<Alt>Up"]), ("refresh", &["F5"]),
            ("home", &["<Alt>Home"]), ("search", &["<Primary>f"]), ("filter", &["<Primary>i"]),
            ("hidden", &["<Primary>h", "<Alt>period"]), ("copy", &["<Primary>c"]), ("cut", &["<Primary>x"]),
            ("paste", &["<Primary>v"]), ("select-all", &["<Primary>a"]), ("invert-selection", &["<Primary><Shift>a"]),
            ("trash", &["Delete"]), ("delete", &["<Shift>Delete"]), ("rename", &["F2"]),
            ("new-folder", &["<Primary><Shift>n"]), ("undo", &["<Primary>z"]), ("properties", &["<Alt>Return"]),
            ("bookmark", &["<Primary>d"]), ("zoom-in", &["<Primary>plus", "<Primary>equal"]),
            ("zoom-out", &["<Primary>minus"]), ("focus-other", &["<Primary>Tab"]), ("information", &["F11"]), ("sidebar", &["F9"]),
            ("icons", &["<Primary>1"]), ("compact", &["<Primary>2"]), ("details", &["<Primary>3"]), ("columns", &["<Primary>4"]),
            ("go-anywhere", &["<Primary>k"]), ("print", &["<Primary>p"]),
        ];
        for (name, keys) in shortcuts {
            self.app.set_accels_for_action(&format!("win.{name}"), keys);
        }
        let bus = FileManagerBus::new(Rc::new(BusHost(self.downgrade())), None);
        bus.start();
        *self.file_manager_bus.borrow_mut() = Some(bus);
    }

    fn command_line(&self, command: &gio::ApplicationCommandLine) -> glib::ExitCode {
        let arguments: Vec<String> = command.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let options = match cli::parse(&arguments, false) {
            Ok(options) => options,
            Err(exit) => {
                command.printerr_literal(&exit.stderr);
                return glib::ExitCode::from(exit.code as u8);
            }
        };
        let mut locations: Vec<gio::File> = options.locations.iter().map(|arg| command.create_file_for_arg(std::ffi::OsStr::new(arg))).collect();
        if options.home {
            locations.insert(0, crate::pane::home_file());
        }
        self.open_locations(locations, options.new_window, options.select, options.split);
        glib::ExitCode::SUCCESS
    }

    pub fn open_locations(&self, locations: Vec<gio::File>, new_window: bool, select: bool, split: bool) -> FilesWindow {
        let active = self.app.active_window().and_then(|w| files_window_for(&w));
        let mut restored: Vec<(String, FolderTab)> = Vec::new();
        let window = match active {
            Some(window) if !new_window => window,
            _ => {
                let window = FilesWindow::new(self);
                // Launchers such as the dock's Home button name a folder. The first
                // window still reopens the saved tabs, then shows that folder.
                if !locations.is_empty() && !new_window && self.restore_tabs(&window) {
                    restored = window.pages().into_iter().map(|p| (p.left.current().uri().to_string(), p)).collect();
                }
                window
            }
        };
        if !locations.is_empty() {
            let resolved: Rc<RefCell<Vec<Option<(gio::File, Vec<String>)>>>> = Rc::new(RefCell::new(vec![None; locations.len()]));
            let ready = {
                let (resolved, window, restored) = (resolved.clone(), window.clone(), restored.clone());
                Rc::new(move |index: usize, directory: gio::File, selected: Vec<String>| {
                    resolved.borrow_mut()[index] = Some((directory, selected));
                    if !window.alive.get() || resolved.borrow().iter().any(Option::is_none) {
                        return;
                    }
                    let resolved: Vec<(gio::File, Vec<String>)> = resolved.borrow().iter().flatten().cloned().collect();
                    // Preserve command-line order despite asynchronous remote queries.
                    if split {
                        let page = window.new_tab(Some(&resolved[0].0), Some(resolved[0].1.clone()));
                        let second = resolved.get(1).unwrap_or(&resolved[0]);
                        page.set_split(true, Some(&second.0));
                        if let Some(right) = page.right() {
                            right.set_pending_selection(second.1.clone());
                        }
                        for (directory, selected) in &resolved[2.min(resolved.len())..] {
                            window.new_tab(Some(directory), Some(selected.clone()));
                        }
                    } else {
                        let mut groups: Vec<(String, FolderTab)> = Vec::new();
                        for (directory, selected) in &resolved {
                            let uri = directory.uri().to_string();
                            if let Some((_, page)) = groups.iter().find(|(u, _)| *u == uri).filter(|_| !selected.is_empty()) {
                                page.left.extend_pending_selection(selected.clone());
                            } else if let Some((_, page)) = restored.iter().find(|(u, _)| *u == uri).filter(|_| selected.is_empty()) {
                                if let Some(number) = window.notebook.page_num(&page.paned) {
                                    window.notebook.set_current_page(Some(number));
                                }
                            } else {
                                let page = window.new_tab(Some(directory), Some(selected.clone()));
                                groups.retain(|(u, _)| *u != uri);
                                groups.push((uri, page));
                            }
                        }
                    }
                    window.window.present();
                })
            };
            for (index, location) in locations.iter().enumerate() {
                if select {
                    ready(index, location.parent().unwrap_or_else(|| location.clone()), vec![location.uri().to_string()]);
                    continue;
                }
                // Classify asynchronously: remote paths must not stall GTK.
                let (ready, delay) = (ready.clone(), self.hooks.classify_delay.borrow().clone());
                let source = location.clone();
                location.query_info_async("standard::type", gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT, None::<&gio::Cancellable>,
                    move |result| {
                        let key = source.clone();
                        let deliver = move || match result {
                            Ok(info) if info.file_type() == gio::FileType::Directory => ready(index, source.clone(), Vec::new()),
                            Ok(_) => ready(index, source.parent().unwrap_or_else(|| source.clone()), vec![source.uri().to_string()]),
                            Err(_) => ready(index, source.clone(), Vec::new()),
                        };
                        match delay.map(|d| d(&key)).filter(|ms| *ms > 0) {
                            Some(ms) => {
                                glib::timeout_add_local_once(std::time::Duration::from_millis(ms), deliver);
                            }
                            None => deliver(),
                        }
                    });
            }
        } else if window.pages().is_empty() && !self.restore_tabs(&window) {
            window.new_tab(Some(&crate::pane::home_file()), None);
        }
        self.session_restored.set(true);
        if split && locations.is_empty() {
            if let Some(page) = window.current_page() {
                page.set_split(true, None);
            }
        }
        window.window.present();
        window
    }

    /// Reopen the previous session's tabs once per session; report whether any were.
    pub fn restore_tabs(&self, window: &FilesWindow) -> bool {
        let prefs = self.preferences();
        let tabs: Vec<String> = if crate::bridge::pbool(&prefs, "restore_session") && !self.session_restored.get() {
            crate::bridge::pget(&prefs, "tabs").as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
        } else {
            Vec::new()
        };
        self.session_restored.set(true);
        let splits: Vec<String> = crate::bridge::pget(&prefs, "tab_splits").as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect()).unwrap_or_default();
        for (index, uri) in tabs.iter().enumerate() {
            let page = window.new_tab(Some(&crate::pane::as_file(uri)), None);
            if let Some(split) = splits.get(index).filter(|s| !s.is_empty()) {
                page.set_split(true, Some(&crate::pane::as_file(split)));
            }
        }
        if !tabs.is_empty() {
            let active = crate::bridge::pget(&prefs, "active_tab").as_i64().unwrap_or(0).max(0) as u32;
            window.notebook.set_current_page(Some(active.min(window.notebook.n_pages().saturating_sub(1))));
        }
        !tabs.is_empty()
    }

    pub fn show_properties(&self, files: Vec<gio::File>) {
        let window = match self.app.active_window().and_then(|w| files_window_for(&w)) {
            Some(window) => window,
            None => self.open_locations(Vec::new(), false, false, false),
        };
        let weak = window.downgrade();
        let properties = files_widgets::dialogs::PropertiesWindow::new(Some(window.window.upcast_ref()), &files, Some(Rc::new(move || {
            if let Some(window) = weak.upgrade() {
                FilesWindow(window).refresh_all()
            }
        })), None);
        properties.present();
    }

    fn shutdown(&self) {
        if let Some(appearance) = self.appearance.borrow_mut().take() {
            appearance.close();
        }
        if let Some(bus) = self.file_manager_bus.borrow_mut().take() {
            bus.close();
        }
        if let (Some(display), Some(id)) = (gdk::Display::default(), self.clipboard_signal.borrow_mut().take()) {
            display.clipboard().disconnect(id);
        }
        self.thumbnails.close();
    }
}

impl Default for FilesApplication {
    fn default() -> Self {
        Self::new()
    }
}

struct BusHost(WeakApp);

impl FileManagerHost for BusHost {
    fn connection(&self) -> Option<gio::DBusConnection> {
        self.0.upgrade()?.app.dbus_connection()
    }
    fn open_locations(&self, files: Vec<gio::File>, select: bool, new_window: bool) -> Result<(), String> {
        let app = self.0.upgrade().ok_or("Files is closing.")?;
        app.open_locations(files, new_window, select, false);
        Ok(())
    }
    fn show_properties(&self, files: Vec<gio::File>) -> Result<(), String> {
        let app = self.0.upgrade().ok_or("Files is closing.")?;
        app.show_properties(files);
        Ok(())
    }
}

/// `file_manager.py main(argv)`.
pub fn main(argv: Vec<String>) -> i32 {
    // Internal launch helpers run inside an independent app service (no GTK).
    if argv.get(1).map(String::as_str) == Some("--launch-helper") {
        if argv.get(2).map(String::as_str) == Some("--app-catalog") {
            return waydock::core::app_catalog::launch_helper_main(&argv[3..]);
        }
        return files_model::launch::helper_main(&argv[2..]);
    }
    let options = match cli::parse(&argv[1.min(argv.len())..], true) {
        Ok(options) => options,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return exit.code;
        }
    };
    suite_core::i18n::initialize();
    if options.check {
        println!("{}", cli::check_json());
        return 0;
    }
    if let Err(error) = gtk::init() {
        eprintln!("Files requires GTK4 and PyGObject: {error}");
        return 1;
    }
    let app = FilesApplication::new();
    let args: Vec<String> = argv.clone();
    i32::from(app.app.run_with_args(&args).get())
}

/// The program file path for helper commands (`sys.executable` + script).
pub fn helper_program() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/local/bin/wayfiles"))
}
