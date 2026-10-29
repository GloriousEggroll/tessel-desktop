//! `BatteryApplication` (`battery_manager.py`): unique GApplication
//! `org.waybar.BatteryManager`, `HANDLES_COMMAND_LINE`, launcher-only `--check`.

use crate::bluetooth;
use crate::cli::{self, Mode};
use suite_power::{BatteryModel, PowerBackend, PowerSystem};
use crate::ui::{AppHooks, BatteryPopup, BatterySettingsWindow};
use crate::APPLICATION_ID;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gio::prelude::ApplicationCommandLineExt;
use gtk::subclass::prelude::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::tr;
use suite_gtk::gtk;

/// Build the backend: `factory(on_changed, on_error)`.
pub type BackendFactory = Box<dyn FnOnce(Box<dyn Fn()>, Box<dyn Fn(&str)>) -> Rc<dyn BatteryBackendHandle>>;

/// A backend the application owns (model + lifecycle).
pub trait BatteryBackendHandle: BatteryModel {
    fn start(&self);
    fn close(&self);
    fn as_model(self: Rc<Self>) -> Rc<dyn BatteryModel>;
}

impl BatteryBackendHandle for PowerBackend {
    fn start(&self) { PowerSystem::start(self) }
    fn close(&self) { PowerSystem::close(self) }
    fn as_model(self: Rc<Self>) -> Rc<dyn BatteryModel> { self }
}

/// `health()`: the `--check` JSON object.
pub fn health(backend: Option<&dyn BatteryModel>, error: &str) -> Value {
    let Some(backend) = backend else {
        return json!({"ready": false, "has_battery": false, "on_battery": false, "percentage": null,
                      "batteries": [], "charge_limit": {}, "error": error});
    };
    let error = if error.is_empty() { backend.error() } else { error.to_owned() };
    json!({"ready": backend.power_ready(), "has_battery": backend.has_battery(), "on_battery": backend.on_battery(),
           "percentage": backend.percentage(), "batteries": backend.batteries(), "charge_limit": backend.charge_limit(),
           "error": error})
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BatteryApplication {
        pub factory: RefCell<Option<BackendFactory>>,
        pub backend: RefCell<Option<Rc<dyn BatteryBackendHandle>>>,
        pub popup: RefCell<Option<Rc<BatteryPopup>>>,
        pub settings: RefCell<Option<Rc<BatterySettingsWindow>>>,
        pub position: RefCell<Option<(String, i32)>>,
        pub signals: RefCell<Vec<glib::SourceId>>,
        pub stopping: Cell<bool>,
        pub error: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BatteryApplication {
        const NAME: &'static str = "BatteryManagerApplication";
        type Type = super::BatteryApplication;
        type ParentType = gtk::Application;
    }

    impl ObjectImpl for BatteryApplication {}

    impl ApplicationImpl for BatteryApplication {
        fn startup(&self) {
            self.parent_startup();
            let app = self.obj();
            std::mem::forget(app.hold());
            let factory = self.factory.take().unwrap_or_else(|| Box::new(|changed, failed| {
                Rc::new(PowerBackend::batteries_only(changed, failed)) as Rc<dyn BatteryBackendHandle>
            }));
            let (weak, weak_error) = (app.downgrade(), app.downgrade());
            let backend = factory(
                Box::new(move || if let Some(app) = weak.upgrade() { app.changed() }),
                Box::new(move |_| if let Some(app) = weak_error.upgrade() { app.changed() }),
            );
            self.backend.replace(Some(backend.clone()));
            backend.start();
            for signal in [libc::SIGTERM, libc::SIGINT] {
                let weak = app.downgrade();
                let id = suite_core::glib_compat::unix_signal_add_local(signal, move || {
                    if let Some(app) = weak.upgrade() {
                        app.quit();
                    }
                    // Kept until shutdown removes it (Python checks find_source_by_id).
                    glib::ControlFlow::Continue
                });
                self.signals.borrow_mut().push(id);
            }
        }

        fn activate(&self) {
            self.obj().show_popup("bottom", 76);
        }

        /// Never parse locally: forward argv to the primary (Python returns `False, argv, 0`).
        fn local_command_line(&self, _arguments: &mut gio::subclass::ArgumentList) -> std::ops::ControlFlow<glib::ExitCode> {
            std::ops::ControlFlow::Continue(())
        }

        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            let arguments: Vec<String> = command_line.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
            let prog = cli::prog(arguments.first().map(String::as_str));
            let args = match cli::parse(&prog, arguments.get(1..).unwrap_or(&[])) {
                Ok(args) => args,
                Err(exit) => {
                    if !exit.stdout.is_empty() {
                        command_line.print_literal(&exit.stdout);
                    }
                    if !exit.stderr.is_empty() {
                        command_line.printerr_literal(&exit.stderr);
                    }
                    return glib::ExitCode::from(exit.code as u8);
                }
            };
            let app = self.obj();
            match args.mode {
                Mode::Check => {
                    let state = app.health();
                    command_line.print_literal(&format!("{}\n", json_dumps(&state, &JsonStyle::DEFAULT)));
                    return if state["ready"] == true { glib::ExitCode::SUCCESS } else { glib::ExitCode::FAILURE };
                }
                Mode::Settings => app.show_settings(),
                Mode::Background => {}
                Mode::Popup | Mode::Default => app.show_popup(&args.popup_edge, args.popup_offset as i32),
            }
            glib::ExitCode::SUCCESS
        }

        fn shutdown(&self) {
            self.stopping.set(true);
            for id in self.signals.take() {
                id.remove();
            }
            if let Some(backend) = self.backend.borrow().clone() {
                backend.close();
            }
            let popup = self.popup.borrow().clone();
            if let Some(popup) = popup {
                popup.destroy();
            }
            self.parent_shutdown();
        }
    }

    impl GtkApplicationImpl for BatteryApplication {}
}

glib::wrapper! {
    pub struct BatteryApplication(ObjectSubclass<imp::BatteryApplication>)
        @extends gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

struct Hooks(glib::WeakRef<BatteryApplication>);

impl AppHooks for Hooks {
    fn error(&self) -> String {
        self.0.upgrade().map(|app| app.imp().error.borrow().clone()).unwrap_or_default()
    }

    fn bluetooth_available(&self) -> bool {
        bluetooth::bluetooth_script(&bluetooth::script_roots()).is_some() && suite_core::shell::which("systemd-run").is_some()
    }

    fn open_bluetooth(&self) -> bool {
        self.0.upgrade().is_some_and(|app| app.open_bluetooth())
    }
}

impl BatteryApplication {
    pub fn new(launcher: bool, factory: Option<BackendFactory>) -> Self {
        let app: Self = glib::Object::builder()
            .property("application-id", APPLICATION_ID)
            .property("flags", suite_core::single_instance::application_flags(launcher))
            .build();
        glib::set_prgname(Some(APPLICATION_ID));
        app.imp().factory.replace(factory);
        app
    }

    fn hooks(&self) -> Rc<dyn AppHooks> {
        Rc::new(Hooks(self.downgrade()))
    }

    fn model(&self) -> Option<Rc<dyn BatteryModel>> {
        self.imp().backend.borrow().clone().map(|backend| backend.as_model())
    }

    pub fn health(&self) -> Value {
        let model = self.model();
        health(model.as_deref(), &self.imp().error.borrow())
    }

    fn changed(&self) {
        let imp = self.imp();
        if imp.stopping.get() {
            return;
        }
        let popup = imp.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.refresh();
        }
        let settings = imp.settings.borrow().clone();
        if let Some(settings) = settings {
            settings.refresh();
        }
    }

    pub fn show_popup(&self, edge: &str, offset: i32) {
        let imp = self.imp();
        let Some(model) = self.model() else { return };
        let position = (edge.to_owned(), offset);
        let existing = imp.popup.borrow().clone();
        if let Some(popup) = existing.as_ref() {
            if imp.position.borrow().as_ref() != Some(&position) {
                imp.popup.replace(None);
                popup.destroy();
            }
        }
        let current = imp.popup.borrow().clone();
        match current {
            Some(popup) => popup.activate(),
            None => {
                let weak = self.downgrade();
                let open_settings: Rc<dyn Fn()> = Rc::new(move || if let Some(app) = weak.upgrade() { app.show_settings() });
                let popup = BatteryPopup::new(Some(self.upcast_ref()), self.hooks(), model, open_settings, edge, offset);
                imp.popup.replace(Some(popup.clone()));
                imp.position.replace(Some(position));
                let weak = self.downgrade();
                let window = popup.window.downgrade();
                popup.window.connect_unrealize(move |unrealized| {
                    let (Some(app), Some(window)) = (weak.upgrade(), window.upgrade()) else { return };
                    let imp = app.imp();
                    let ours = imp.popup.borrow().as_ref().is_some_and(|p| p.window == window && &window == unrealized);
                    if ours {
                        imp.popup.replace(None);
                    }
                });
                popup.show();
            }
        }
    }

    pub fn show_settings(&self) {
        let imp = self.imp();
        let popup = imp.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.dismiss();
        }
        let Some(model) = self.model() else { return };
        if imp.settings.borrow().is_none() {
            let settings = BatterySettingsWindow::new(Some(self.upcast_ref()), self.hooks(), model, None);
            let weak = self.downgrade();
            let window = settings.window.downgrade();
            settings.window.connect_unrealize(move |_| {
                let (Some(app), Some(window)) = (weak.upgrade(), window.upgrade()) else { return };
                let imp = app.imp();
                if imp.settings.borrow().as_ref().is_some_and(|s| s.window == window) {
                    imp.settings.replace(None);
                }
            });
            imp.settings.replace(Some(settings));
        }
        let Some(settings) = imp.settings.borrow().clone() else { return };
        settings.window.set_visible(true);
        settings.refresh();
        settings.window.unminimize();
        settings.window.present();
        let weak = self.downgrade();
        let window = settings.window.downgrade();
        glib::idle_add_local_once(move || {
            if let (Some(app), Some(window)) = (weak.upgrade(), window.upgrade()) {
                app.raise_settings(&window);
            }
        });
    }

    /// One `wlrctl toplevel focus app_id:… title:…` attempt (Python `_raise_settings`).
    fn raise_settings(&self, window: &gtk::ApplicationWindow) {
        let ours = self.imp().settings.borrow().as_ref().is_some_and(|s| &s.window == window);
        if !ours || window.surface().is_none() {
            return;
        }
        if WidgetExt::display(window).type_().name() != "GdkWaylandDisplay" {
            return;
        }
        let Some(wlrctl) = suite_core::shell::which("wlrctl") else { return };
        let app_id = format!("app_id:{}", self.application_id().unwrap_or_default());
        let title = format!("title:{}", window.title().unwrap_or_default());
        let argv = [wlrctl.as_os_str(), "toplevel".as_ref(), "focus".as_ref(), app_id.as_ref(), title.as_ref()];
        if let Ok(child) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
            child.wait_async(None::<&gio::Cancellable>, |_| {});
        }
    }

    fn set_error(&self, message: String) {
        self.imp().error.replace(message);
        self.changed();
    }

    pub fn open_bluetooth(&self) -> bool {
        let script = bluetooth::bluetooth_script(&bluetooth::script_roots());
        let (Some(script), Some(launcher)) = (script, suite_core::shell::which("systemd-run")) else {
            self.set_error(tr!("Bluetooth settings are not installed."));
            return false;
        };
        let command = bluetooth::command(&launcher, &bluetooth::python(), &script, |name| std::env::var_os(name).is_some());
        let argv: Vec<&std::ffi::OsStr> = command.iter().map(|w| w.as_os_str()).collect();
        match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
            Err(error) => {
                self.set_error(tr!("Could not open Bluetooth settings: {value1}", value1 = error.message()));
                false
            }
            Ok(process) => {
                let weak = self.downgrade();
                process.wait_check_async(None::<&gio::Cancellable>, move |result| {
                    if result.is_err() {
                        if let Some(app) = weak.upgrade() {
                            app.set_error(tr!("Could not open Bluetooth settings. Try the Bluetooth icon in Waybar."));
                        }
                    }
                });
                true
            }
        }
    }

    /// Settings window (Global Settings / tests).
    pub fn settings_window(&self) -> Option<Rc<BatterySettingsWindow>> {
        self.imp().settings.borrow().clone()
    }

    pub fn popup(&self) -> Option<Rc<BatteryPopup>> {
        self.imp().popup.borrow().clone()
    }
}

/// `main()`: validate argv like argparse first, then run the application.
pub fn main() -> glib::ExitCode {
    suite_core::i18n::initialize();
    let argv: Vec<String> = std::env::args().collect();
    let prog = cli::prog(argv.first().map(String::as_str));
    let args = match cli::parse(&prog, &argv[1..]) {
        Ok(args) => args,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return glib::ExitCode::from(exit.code as u8);
        }
    };
    let app = BatteryApplication::new(args.check(), None);
    app.run_with_args(&argv)
}
