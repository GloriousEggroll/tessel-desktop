//! The unique `org.waybar.KeepAwake` GApplication (keep_awake.py
//! `KeepAwakeApplication`). The primary is a plain `gio::Application`: GTK is
//! initialised only when the first popup is shown, so the resident daemon
//! needs no display connection or widget memory.

use crate::args::{self, Mode, Outcome};
use crate::backend::InhibitorBackend;
use crate::client::{DURATION_INTERFACE, INTERFACE};
use crate::status::status;
use crate::timed::{timer_path, Inhibit, Login, TimedBackend};
use crate::ui::KeepAwakePopup;
use gio::prelude::*;
use gio::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use suite_core::service_settings::SettingsService;

pub const APPLICATION_ID: &str = "org.waybar.KeepAwake";

/// Builds the inhibitor from the change callback (Python `backend_factory`).
pub type BackendFactory = Box<dyn FnOnce(Rc<dyn Fn()>) -> Rc<dyn Inhibit>>;

pub fn login_factory() -> BackendFactory {
    Box::new(|changed| {
        let backend = InhibitorBackend::new(changed.clone());
        Rc::new(Login { backend, on_changed: changed }) as Rc<dyn Inhibit>
    })
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct KeepAwakeApplication {
        pub factory: RefCell<Option<BackendFactory>>,
        pub timer_file: RefCell<Option<PathBuf>>,
        pub backend: RefCell<Option<TimedBackend>>,
        pub popup: RefCell<Option<KeepAwakePopup>>,
        pub position: RefCell<Option<(String, i32)>>,
        pub settings_service: RefCell<Option<SettingsService>>,
        pub duration_service: RefCell<Option<SettingsService>>,
        pub signals: RefCell<Vec<u32>>,
        pub held: RefCell<Option<gio::ApplicationHoldGuard>>,
        pub stopping: Cell<bool>,
        pub shut_down: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for KeepAwakeApplication {
        const NAME: &'static str = "KeepAwakeApplication";
        type Type = super::KeepAwakeApplication;
        type ParentType = gio::Application;
    }

    impl ObjectImpl for KeepAwakeApplication {}

    impl ApplicationImpl for KeepAwakeApplication {
        fn startup(&self) {
            self.parent_startup();
            self.obj().on_startup();
        }

        fn activate(&self) {
            self.obj().show_popup("bottom", 76);
        }

        // All parsing happens in the primary (Python returns False here).
        fn local_command_line(&self, _arguments: &mut gio::subclass::ArgumentList) -> std::ops::ControlFlow<glib::ExitCode> {
            std::ops::ControlFlow::Continue(())
        }

        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            glib::ExitCode::from(self.obj().on_command_line(command_line) as u8)
        }

        fn shutdown(&self) {
            self.obj().shutdown_now();
            self.parent_shutdown();
        }
    }
}

glib::wrapper! {
    pub struct KeepAwakeApplication(ObjectSubclass<imp::KeepAwakeApplication>)
        @extends gio::Application, @implements gio::ActionGroup, gio::ActionMap;
}

impl KeepAwakeApplication {
    /// `launcher` = never become the primary (every invocation but `--background`).
    pub fn new(application_id: &str, backend_factory: BackendFactory, launcher: bool, timer_file: Option<PathBuf>) -> Self {
        let app: Self = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", suite_core::single_instance::application_flags(launcher))
            .build();
        app.imp().factory.replace(Some(backend_factory));
        app.imp().timer_file.replace(timer_file);
        app
    }

    pub fn backend(&self) -> Option<TimedBackend> { self.imp().backend.borrow().clone() }
    pub fn popup(&self) -> Option<KeepAwakePopup> { self.imp().popup.borrow().clone() }
    pub fn snapshot(&self) -> crate::status::Snapshot {
        self.backend().map(|b| b.snapshot()).unwrap_or_default()
    }

    fn on_startup(&self) {
        let Some(factory) = self.imp().factory.take() else { return };
        let weak = self.downgrade();
        let changed: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(app) = weak.upgrade() {
                app.changed();
            }
        });
        let path = self.imp().timer_file.borrow().clone().unwrap_or_else(timer_path);
        let backend = TimedBackend::new(factory(changed), Some(path), None);
        self.imp().backend.replace(Some(backend.clone()));
        backend.start();
        let snapshot = |weak: glib::WeakRef<Self>| move || {
            weak.upgrade().map(|app| status(&app.snapshot())).unwrap_or_default()
        };
        let weak = self.downgrade();
        match SettingsService::new(self, INTERFACE, "SetEnabled", "b", snapshot(self.downgrade()), move |value| {
            let app = weak.upgrade().ok_or("Keep Awake is stopping.")?;
            Ok(app.remote_enable(value.get::<bool>().ok_or("Invalid argument.")?))
        }) {
            Ok(service) => { self.imp().settings_service.replace(Some(service)); }
            Err(error) => eprintln!("keep-awake: could not export {INTERFACE}: {error}"),
        }
        // A second interface on the same object, so the shared transport stays unchanged.
        let weak = self.downgrade();
        match SettingsService::new(self, DURATION_INTERFACE, "SetDuration", "u", snapshot(self.downgrade()), move |value| {
            let app = weak.upgrade().ok_or("Keep Awake is stopping.")?;
            app.remote_duration(value.get::<u32>().ok_or("Invalid argument.")?)
        }) {
            Ok(service) => { self.imp().duration_service.replace(Some(service)); }
            Err(error) => eprintln!("keep-awake: could not export {DURATION_INTERFACE}: {error}"),
        }
        self.imp().held.replace(Some(self.hold()));
        for number in [libc::SIGTERM, libc::SIGINT] {
            let weak = self.downgrade();
            let id = crate::compat::unix_signal_add_local(number, move || {
                if let Some(app) = weak.upgrade() {
                    app.quit();
                }
                glib::ControlFlow::Break
            });
            self.imp().signals.borrow_mut().push(id);
        }
    }

    fn on_command_line(&self, command_line: &gio::ApplicationCommandLine) -> i32 {
        let argv: Vec<String> = command_line.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let prog = args::prog(argv.first().map(String::as_str).unwrap_or("keep_awake.py"));
        let parsed = match args::parse(argv.get(1..).unwrap_or(&[])) {
            Outcome::Run(parsed) => parsed,
            Outcome::Help => {
                print!("{}", args::help(&prog));
                return 0;
            }
            Outcome::Error(message) => {
                eprint!("{}", args::error_text(&prog, &message));
                return 2;
            }
        };
        let Some(backend) = self.backend() else { return 1 };
        match parsed.mode {
            Some(Mode::Status) => command_line.print_literal(&(crate::status::status_text(&backend.snapshot()) + "\n")),
            Some(Mode::Enable) => { backend.set_enabled(true); }
            Some(Mode::Disable) => { backend.set_enabled(false); }
            Some(Mode::Toggle) => { backend.toggle(); }
            Some(Mode::Background) => {}
            Some(Mode::Popup) | None => self.show_popup(&parsed.popup_edge, parsed.popup_offset as i32),
        }
        0
    }

    /// Show (or toggle) the singleton popup at `edge`/`offset`.
    pub fn show_popup(&self, edge: &str, offset: i32) {
        let Some(backend) = self.backend() else { return };
        if !gtk::is_initialized_main_thread() {
            if let Err(error) = gtk::init() {
                eprintln!("keep-awake: cannot show the popup: {error}");
                return;
            }
        }
        let position = (edge.to_owned(), offset);
        let current = self.popup();
        if current.is_some() && self.imp().position.borrow().as_ref() != Some(&position) {
            if let Some(popup) = self.imp().popup.take() {
                popup.destroy();
            }
        }
        match self.popup() {
            Some(popup) => popup.activate(),
            None => {
                let popup = KeepAwakePopup::new(Rc::new(backend), edge, offset, false);
                let weak = self.downgrade();
                let window = popup.window().clone();
                window.connect_destroy(move |destroyed| {
                    if let Some(app) = weak.upgrade() {
                        let mine = app.popup().is_some_and(|p| p.window() == destroyed);
                        if mine {
                            app.imp().popup.replace(None);
                        }
                    }
                });
                self.imp().popup.replace(Some(popup.clone()));
                self.imp().position.replace(Some(position));
                popup.show();
            }
        }
    }

    fn changed(&self) {
        if self.imp().stopping.get() {
            return;
        }
        if let Some(popup) = self.popup() {
            popup.refresh();
        }
        if let Some(service) = self.imp().settings_service.borrow().clone() {
            service.changed();
        }
    }

    fn remote_duration(&self, minutes: u32) -> Result<(bool, String), String> {
        let backend = self.backend().ok_or("Keep Awake is stopping.")?;
        backend.set_duration(minutes)?;
        self.changed();
        Ok((true, String::new()))
    }

    fn remote_enable(&self, enabled: bool) -> (bool, String) {
        let Some(backend) = self.backend() else { return (false, String::new()) };
        backend.set_enabled(enabled);
        let error = backend.error();
        (error.is_empty(), error)
    }

    /// Python `do_shutdown` (idempotent; also run by GApplication shutdown).
    pub fn shutdown_now(&self) {
        let imp = self.imp();
        if imp.shut_down.replace(true) {
            return;
        }
        imp.stopping.set(true);
        if let Some(service) = imp.settings_service.take() {
            service.close();
        }
        if let Some(service) = imp.duration_service.take() {
            service.close();
        }
        for id in imp.signals.take() {
            crate::compat::source_remove(id);
        }
        if let Some(backend) = imp.backend.borrow().clone() {
            backend.close();
        }
        if let Some(popup) = imp.popup.take() {
            popup.destroy();
        }
        drop(imp.held.take());
    }
}

use suite_gtk::gtk;
use suite_gtk::gtk::prelude::WidgetExt;
