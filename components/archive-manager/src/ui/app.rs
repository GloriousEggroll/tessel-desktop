//! The `org.waydock.ArchiveManager` application: windows, settings (reloaded
//! when Global Settings writes them), command-line jobs (--batch, --add,
//! --add-to, --dialog) with their progress windows and exit codes.

use super::progress::{with_password, ProgressWindow, Work};
use super::window::ArchiveWindow;
use crate::archive::{CreateOptions, Error};
use crate::cli::{self, Options};
use crate::ops::{self, ExtractOptions};
use archive_settings::plugins::Kind;
use archive_settings::{Settings, Store};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use suite_core::tr;

/// Test hooks (the real ones talk to the file manager / launch applications).
#[derive(Default)]
pub struct Hooks {
    pub open_folder: RefCell<Option<Rc<dyn Fn(&Path)>>>,
    pub launch: RefCell<Option<Rc<dyn Fn(&gio::File, Option<&gio::AppInfo>)>>>,
}

pub struct AppInner {
    pub app: gtk::Application,
    pub store: Store,
    settings: RefCell<Settings>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    pub windows: RefCell<Vec<ArchiveWindow>>,
    pub hooks: Hooks,
    pub exit_code: Cell<i32>,
    pub settings_listeners: RefCell<Vec<Rc<dyn Fn()>>>,
}

#[derive(Clone)]
pub struct App(pub Rc<AppInner>);

impl std::ops::Deref for App {
    type Target = AppInner;
    fn deref(&self) -> &AppInner {
        &self.0
    }
}

thread_local! {
    static APP: RefCell<Weak<AppInner>> = const { RefCell::new(Weak::new()) };
}

pub fn current() -> Option<App> {
    APP.with(|a| a.borrow().upgrade()).map(App)
}

impl App {
    pub fn new(flags: gio::ApplicationFlags, store: Store) -> App {
        let app = gtk::Application::builder().application_id(crate::APPLICATION_ID).flags(flags).build();
        let settings = store.load();
        let inner = Rc::new(AppInner {
            app, store, settings: RefCell::new(settings), monitor: RefCell::new(None), windows: RefCell::new(Vec::new()),
            hooks: Hooks::default(), exit_code: Cell::new(0), settings_listeners: RefCell::new(Vec::new()),
        });
        APP.with(|a| *a.borrow_mut() = Rc::downgrade(&inner));
        let this = App(inner);
        let weak = Rc::downgrade(&this.0);
        this.app.connect_startup(move |_| {
            if let Some(app) = weak.upgrade().map(App) {
                app.startup();
            }
        });
        let weak = Rc::downgrade(&this.0);
        this.app.connect_command_line(move |_, command| match weak.upgrade().map(App) {
            Some(app) => app.command_line(command),
            None => glib::ExitCode::FAILURE,
        });
        let weak = Rc::downgrade(&this.0);
        this.app.connect_activate(move |_| {
            if let Some(app) = weak.upgrade().map(App) {
                app.new_window().window.present();
            }
        });
        this
    }

    pub fn settings(&self) -> Settings {
        self.settings.borrow().clone()
    }

    pub fn update_settings(&self, edit: impl FnOnce(&mut Settings)) {
        match self.store.update(edit) {
            Ok(s) => *self.settings.borrow_mut() = s,
            Err(e) => eprintln!("wayarchive: could not save settings: {e}"),
        }
        self.notify_settings();
    }

    pub fn reload_settings(&self) {
        *self.settings.borrow_mut() = self.store.load();
        self.notify_settings();
    }

    fn notify_settings(&self) {
        let listeners: Vec<_> = self.settings_listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    fn startup(&self) {
        let display = gtk::gdk::Display::default();
        suite_gtk::utils::add_css(super::dialogs::CSS, display.as_ref(), suite_gtk::utils::PRIORITY_APPLICATION);
        suite_gtk::theme::attach_application(&self.app);
        let weak = Rc::downgrade(&self.0);
        *self.monitor.borrow_mut() = self.store.watch(move || {
            if let Some(app) = weak.upgrade().map(App) {
                app.reload_settings();
            }
        });
        let accels: &[(&str, &[&str])] = &[
            ("win.open", &["<Primary>o"]), ("win.new", &["<Primary>n"]), ("win.extract", &["<Primary>e"]),
            ("win.add-files", &["<Primary><Shift>a"]), ("win.new-folder", &["<Primary><Shift>n"]), ("win.preview", &["<Primary>p"]), ("win.find", &["<Primary>f"]),
            ("win.info-panel", &["F9"]), ("win.properties", &["<Alt>Return"]), ("win.test", &["<Primary>t"]),
            ("win.close", &["<Primary>w"]), ("win.settings", &["<Primary>comma"]),
        ];
        for (action, keys) in accels {
            self.app.set_accels_for_action(action, keys);
        }
    }

    pub fn new_window(&self) -> ArchiveWindow {
        let window = ArchiveWindow::new(self);
        self.windows.borrow_mut().push(window.clone());
        window
    }

    pub fn forget(&self, window: &ArchiveWindow) {
        self.windows.borrow_mut().retain(|w| w != window);
    }

    /// A window that shows nothing yet (reused for the next archive), else a new one.
    pub fn empty_window(&self) -> ArchiveWindow {
        let found = self.windows.borrow().iter().find(|w| w.archive().is_none() && !w.busy()).cloned();
        found.unwrap_or_else(|| self.new_window())
    }

    fn command_line(&self, command: &gio::ApplicationCommandLine) -> glib::ExitCode {
        let argv: Vec<String> = command.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let mut options = match cli::parse(&argv) {
            Ok(o) => o,
            Err(exit) => {
                command.print_literal(&exit.stdout);
                command.printerr_literal(&exit.stderr);
                return glib::ExitCode::from(exit.code as u8);
            }
        };
        let resolve = |arg: &str| -> PathBuf {
            let file = command.create_file_for_arg(std::ffi::OsStr::new(arg));
            file.path().unwrap_or_else(|| PathBuf::from(arg))
        };
        let files: Vec<PathBuf> = options.files.iter().map(|f| resolve(f)).collect();
        options.destination = options.destination.as_deref().map(|d| resolve(d).display().to_string());
        options.add_to = options.add_to.as_deref().map(|d| resolve(d).display().to_string());
        let cwd = command.cwd().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        if options.is_job() {
            if let Some(missing) = files.iter().find(|f| std::fs::symlink_metadata(f).is_err()) {
                command.printerr_literal(&format!("wayarchive: {}: {}\n", missing.display(), tr!("No such file or folder")));
                self.exit_code.set(1);
                return glib::ExitCode::from(1);
            }
            self.run_job(options, files, cwd);
            return glib::ExitCode::SUCCESS;
        }
        if files.is_empty() {
            self.new_window().window.present();
        }
        for file in files {
            let window = self.empty_window();
            window.window.present();
            window.open(&file);
        }
        glib::ExitCode::SUCCESS
    }

    pub fn fail(&self, message: &str, parent: Option<&gtk::Window>, release: impl FnOnce() + 'static) {
        self.exit_code.set(1);
        eprintln!("wayarchive: {message}");
        let w = super::dialogs::error(parent, message);
        let release = RefCell::new(Some(release));
        w.connect_destroy(move |_| {
            if let Some(r) = release.borrow_mut().take() {
                r()
            }
        });
    }

    /// --batch / --add / --add-to / --dialog: no main window; quit when done.
    pub fn run_job(&self, options: Options, files: Vec<PathBuf>, cwd: PathBuf) {
        let guard = Rc::new(RefCell::new(Some(self.app.hold())));
        let release: Rc<dyn Fn()> = Rc::new(move || {
            guard.borrow_mut().take();
        });
        if options.is_add() {
            self.run_add(options, files, release);
        } else {
            self.run_extract(options, files, cwd, 0, release);
        }
    }

    fn run_extract(&self, options: Options, files: Vec<PathBuf>, cwd: PathBuf, index: usize, release: Rc<dyn Fn()>) {
        let Some(path) = files.get(index).cloned() else {
            release();
            return;
        };
        let progress = ProgressWindow::new(&self.app, &tr!("Extracting {name}", name = file_name(&path)));
        let settings = self.settings();
        let ui = { let p = progress.clone(); Rc::new(move || p.ui()) as Rc<dyn Fn() -> crate::job::JobUi> };
        let open_path = path.clone();
        let open: Work<ops::Archive> = Arc::new(move |ctx, _| {
            let mut archive = ops::open(&open_path, &settings, ctx)?;
            archive.password = ctx.password.clone();
            Ok(archive)
        });
        let this = self.clone();
        let parent = Some(progress.window.clone());
        with_password(parent, file_name(&path), None, ui.clone(), progress.handle.clone(), open, Box::new(move |result, password| {
            let mut archive = match result {
                Ok(a) => a,
                Err(Error::Cancelled) => {
                    this.exit_code.set(1);
                    progress.window.destroy();
                    release();
                    return;
                }
                Err(e) => {
                    let r = release.clone();
                    progress.window.set_visible(false);
                    let w = progress.window.clone();
                    this.fail(&e.to_string(), None, move || {
                        w.destroy();
                        r()
                    });
                    return;
                }
            };
            if password.is_some() && archive.password.is_none() {
                archive.password = password;
            }
            let destination = options.destination.clone().map(PathBuf::from).unwrap_or_else(|| {
                if options.auto_destination { path.parent().map(Path::to_path_buf).unwrap_or(cwd.clone()) } else { cwd.clone() }
            });
            let settings = this.settings();
            let archive_copy = archive.clone();
            let auto_sub = (options.auto_subfolder && ops::needs_subfolder(&archive.listing)).then(|| ops::subfolder_name(&archive.path));
            let go = {
                let this = this.clone();
                let progress = progress.clone();
                let (options, files, cwd, release) = (options.clone(), files.clone(), cwd.clone(), release.clone());
                move |request: super::forms::ExtractRequest| {
                    let archive2 = archive.clone();
                    let extract_options = ExtractOptions { preserve_paths: request.preserve_paths, strip: None, subfolder: request.subfolder.clone() };
                    let dest = request.destination.clone();
                    let work: Work<ops::ExtractReport> = Arc::new(move |ctx, resolve| ops::extract(&archive2, None, &dest, &extract_options, ctx, resolve));
                    let ui = { let p = progress.clone(); Rc::new(move || p.ui()) as Rc<dyn Fn() -> crate::job::JobUi> };
                    let password = archive.password.clone();
                    let parent = Some(progress.window.clone());
                    let this2 = this.clone();
                    with_password(parent, archive.name(), password, ui, progress.handle.clone(), work, Box::new(move |result, _| {
                        match result {
                            Ok(report) => {
                                if request.open_destination {
                                    this2.open_folder(&report.folder);
                                }
                                progress.window.destroy();
                                this2.run_extract(options, files, cwd, index + 1, release);
                            }
                            Err(Error::Cancelled) => {
                                this2.exit_code.set(1);
                                progress.window.destroy();
                                release();
                            }
                            Err(e) => {
                                progress.window.set_visible(false);
                                let w = progress.window.clone();
                                let (this3, options, files, cwd, release) = (this2.clone(), options, files, cwd, release);
                                this2.fail(&e.to_string(), None, move || {
                                    w.destroy();
                                    this3.run_extract(options, files, cwd, index + 1, release)
                                });
                            }
                        }
                    }));
                }
            };
            if options.dialog {
                progress.window.set_visible(false);
                let p = progress.clone();
                let (this2, release2) = (this.clone(), release.clone());
                super::forms::extract_dialog(None, &archive_copy, 0,
                                             &destination, &settings, false, move |request| match request {
                    Some(request) => {
                        p.window.set_visible(true);
                        go(request)
                    }
                    None => {
                        this2.exit_code.set(1);
                        p.window.destroy();
                        release2();
                    }
                });
            } else {
                go(super::forms::ExtractRequest {
                    destination, subfolder: auto_sub, preserve_paths: true, selected_only: false,
                    open_destination: options.open_destination, close_after: false,
                });
            }
        }));
    }

    fn run_add(&self, options: Options, files: Vec<PathBuf>, release: Rc<dyn Fn()>) {
        let missing: Vec<String> = files.iter().filter(|f| std::fs::symlink_metadata(f).is_err()).map(|f| f.display().to_string()).collect();
        if !missing.is_empty() {
            let r = release.clone();
            self.fail(&tr!("These files do not exist: {files}", files = missing.join(", ")), None, move || r());
            return;
        }
        let items = cli::add_items(&files, options.change_to_first_path);
        let first = files[0].clone();
        let settings = self.settings();
        let target: Option<PathBuf> = options.add_to.clone().map(PathBuf::from)
            .or_else(|| options.auto_filename.as_deref().map(|suffix| cli::auto_archive_path(&first, suffix)));
        let this = self.clone();
        let release_start = release.clone();
        let start = move |path: PathBuf, kind: Option<Kind>, create: CreateOptions| {
            let release = release_start.clone();
            let progress = ProgressWindow::new(&this.app, &tr!("Compressing {name}", name = file_name(&path)));
            let ui = { let p = progress.clone(); Rc::new(move || p.ui()) as Rc<dyn Fn() -> crate::job::JobUi> };
            let (settings, items) = (this.settings(), items.clone());
            let work: Work<()> = Arc::new(move |ctx, _| {
                if path.exists() {
                    let archive = ops::open(&path, &settings, ctx)?;
                    ops::add_items(&archive, &items, &create, ctx)
                } else {
                    let kind = kind.or_else(|| Kind::from_name(&path.to_string_lossy()))
                        .ok_or_else(|| Error::Failed(tr!("Choose a file name with an archive extension such as .zip or .tar.gz.")))?;
                    ops::create_items(&path, kind, &items, &create, &settings, ctx)
                }
            });
            let (this2, release2) = (this.clone(), release.clone());
            let parent = Some(progress.window.clone());
            with_password(parent, String::new(), None, ui, progress.handle.clone(), work, Box::new(move |result, _| {
                match result {
                    Ok(()) => {
                        progress.window.destroy();
                        release2();
                    }
                    Err(Error::Cancelled) => {
                        this2.exit_code.set(1);
                        progress.window.destroy();
                        release2();
                    }
                    Err(e) => {
                        progress.window.set_visible(false);
                        let w = progress.window.clone();
                        this2.fail(&e.to_string(), None, move || {
                            w.destroy();
                            release2()
                        });
                    }
                }
            }));
        };
        match target {
            Some(path) => start(path, None, CreateOptions::default()),
            None => {
                let folder = first.parent().map(Path::to_path_buf).unwrap_or_default();
                let name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| tr!("Archive"));
                let name = if first.is_dir() { name } else { name.rsplit_once('.').map(|(s, _)| s.to_string()).filter(|s| !s.is_empty()).unwrap_or(name) };
                let kinds = archive_settings::plugins::writable_kinds(&|id| settings.plugin_enabled(id), &|p| p.available());
                let kinds: Vec<Kind> = if files.len() > 1 || first.is_dir() { kinds.into_iter().filter(|k| !k.is_single_file()).collect() } else { kinds };
                let (this, release) = (self.clone(), release.clone());
                super::forms::create_dialog(None, &folder, &name, kinds, None, move |request| match request {
                    Some(request) => start(request.path, Some(request.kind), request.options),
                    None => {
                        this.exit_code.set(1);
                        release();
                    }
                });
            }
        }
    }

    /// Show a folder in the file manager (FileManager1, else the default handler).
    pub fn open_folder(&self, folder: &Path) {
        if let Some(hook) = self.hooks.open_folder.borrow().clone() {
            hook(folder);
            return;
        }
        let uri = gio::File::for_path(folder).uri().to_string();
        let fallback = uri.clone();
        gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |bus| {
            let Ok(bus) = bus else {
                let _ = gio::AppInfo::launch_default_for_uri(&fallback, None::<&gio::AppLaunchContext>);
                return;
            };
            bus.call(Some("org.freedesktop.FileManager1"), "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1", "ShowFolders",
                     Some(&(vec![uri], String::new()).to_variant()), None, gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>,
                     move |result| {
                if result.is_err() {
                    let _ = gio::AppInfo::launch_default_for_uri(&fallback, None::<&gio::AppLaunchContext>);
                }
            });
        });
    }

    /// Open a file with its default application or `app`.
    pub fn launch(&self, file: &gio::File, app: Option<&gio::AppInfo>) -> Result<(), String> {
        if let Some(hook) = self.hooks.launch.borrow().clone() {
            hook(file, app);
            return Ok(());
        }
        match app {
            Some(app) => app.launch(&[file.clone()], None::<&gio::AppLaunchContext>).map_err(|e| e.to_string()),
            None => gio::AppInfo::launch_default_for_uri(&file.uri(), None::<&gio::AppLaunchContext>).map_err(|e| e.to_string()),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// `wayarchive` main.
pub fn main(argv: Vec<String>) -> i32 {
    let options = match cli::parse(&argv[1.min(argv.len())..]) {
        Ok(o) => o,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return exit.code;
        }
    };
    suite_core::i18n::initialize();
    if options.mimetypes {
        for mime in Kind::all_mimes() {
            println!("{mime}");
        }
        return 0;
    }
    if options.check {
        println!("{}", cli::check_json());
        return 0;
    }
    glib::set_prgname(Some(crate::APPLICATION_ID));
    if let Err(error) = gtk::init() {
        eprintln!("wayarchive: cannot start GTK: {error}");
        return 1;
    }
    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if options.is_job() {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = App::new(flags, Store::new(None));
    let status = i32::from(app.app.run_with_args(&argv).get());
    status.max(app.exit_code.get())
}
