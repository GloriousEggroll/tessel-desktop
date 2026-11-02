//! The unique `org.tessel.LanguageManager` GApplication
//! (language_manager.py `LanguageApplication`). The primary is a plain
//! `gio::Application`: GTK is initialised when the wizard is first shown, so
//! `--status` works (and stays cheap) without a display.

use crate::args::{self, Outcome};
use crate::backend::LanguageBackend;
use crate::ui::{LanguageWizard, WizardBackend};
use gio::prelude::*;
use gio::subclass::prelude::*;
use std::cell::RefCell;
use std::sync::Arc;
use tessel_gtk::gtk;
use tessel_gtk::gtk::prelude::{GtkWindowExt, WidgetExt};

pub const APPLICATION_ID: &str = "org.tessel.LanguageManager";

/// Python `backend_factory` (called in `do_startup`).
pub type BackendFactory = Box<dyn FnOnce() -> Arc<dyn WizardBackend>>;

pub fn default_factory() -> BackendFactory {
    Box::new(|| Arc::new(LanguageBackend::default()) as Arc<dyn WizardBackend>)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LanguageApplication {
        pub factory: RefCell<Option<BackendFactory>>,
        pub backend: RefCell<Option<Arc<dyn WizardBackend>>>,
        pub window: RefCell<Option<LanguageWizard>>,
        pub held: RefCell<Option<gio::ApplicationHoldGuard>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LanguageApplication {
        const NAME: &'static str = "LanguageManagerApplication";
        type Type = super::LanguageApplication;
        type ParentType = gio::Application;
    }

    impl ObjectImpl for LanguageApplication {}

    impl ApplicationImpl for LanguageApplication {
        fn startup(&self) {
            self.parent_startup();
            if let Some(factory) = self.factory.take() {
                self.backend.replace(Some(factory()));
            }
        }

        fn activate(&self) {
            self.obj().show_window();
        }

        // All parsing happens in the primary (Python returns False here).
        fn local_command_line(&self, _arguments: &mut gio::subclass::ArgumentList) -> std::ops::ControlFlow<glib::ExitCode> {
            std::ops::ControlFlow::Continue(())
        }

        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            glib::ExitCode::from(self.obj().on_command_line(command_line) as u8)
        }

        fn shutdown(&self) {
            if let Some(wizard) = self.window.take() {
                wizard.window.destroy();
            }
            drop(self.held.take());
            self.parent_shutdown();
        }
    }
}

glib::wrapper! {
    pub struct LanguageApplication(ObjectSubclass<imp::LanguageApplication>)
        @extends gio::Application, @implements gio::ActionGroup, gio::ActionMap;
}

impl LanguageApplication {
    pub fn new(application_id: &str, backend_factory: BackendFactory) -> Self {
        glib::set_prgname(Some(application_id));
        let app: Self = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", gio::ApplicationFlags::HANDLES_COMMAND_LINE)
            .build();
        app.imp().factory.replace(Some(backend_factory));
        app
    }

    pub fn window(&self) -> Option<LanguageWizard> {
        self.imp().window.borrow().clone()
    }

    pub fn backend(&self) -> Option<Arc<dyn WizardBackend>> {
        self.imp().backend.borrow().clone()
    }

    fn on_command_line(&self, command_line: &gio::ApplicationCommandLine) -> i32 {
        let argv: Vec<String> = command_line.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let prog = args::prog(argv.first().map(String::as_str).unwrap_or("language_manager.py"));
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
        if parsed.quit {
            if self.window().is_some_and(|w| w.busy.get()) {
                command_line.printerr_literal("A language change is still in progress.\n");
                return 1;
            }
            if let Some(wizard) = self.window() {
                wizard.window.close();
            }
            self.quit();
            return 0;
        }
        if parsed.status {
            // Status queries do not display a window or change a preference.
            let Some(backend) = self.backend() else { return 1 };
            return match backend.status() {
                Ok(status) => {
                    let style = tessel_core::pyfmt::JsonStyle { ensure_ascii: false, ..tessel_core::pyfmt::JsonStyle::DEFAULT };
                    command_line.print_literal(&(tessel_core::pyfmt::json_dumps(&status, &style) + "\n"));
                    0
                }
                Err(error) => {
                    command_line.printerr_literal(&(error + "\n"));
                    1
                }
            };
        }
        self.activate();
        0
    }

    /// `do_activate`: reuse the open window (unminimize, present), then raise it.
    pub fn show_window(&self) {
        let Some(backend) = self.backend() else { return };
        if !gtk::is_initialized_main_thread() {
            if let Err(error) = gtk::init() {
                eprintln!("tessel-language: cannot open the window: {error}");
                return;
            }
        }
        let wizard = match self.window() {
            Some(wizard) if !wizard.closed.get() => wizard,
            _ => {
                let wizard = LanguageWizard::new(None, backend);
                let weak = self.downgrade();
                let target = wizard.downgrade();
                wizard.window.connect_unrealize(move |_| {
                    let Some(app) = weak.upgrade() else { return };
                    let mine = app.window().is_some_and(|w| target.upgrade().is_some_and(|t| std::rc::Rc::ptr_eq(&w.0, &t)));
                    if mine {
                        app.imp().window.replace(None);
                        drop(app.imp().held.take());
                    }
                });
                self.imp().window.replace(Some(wizard.clone()));
                if self.imp().held.borrow().is_none() {
                    self.imp().held.replace(Some(self.hold()));
                }
                wizard
            }
        };
        wizard.window.unminimize();
        wizard.window.present();
        let weak = self.downgrade();
        let window = wizard.window.clone();
        let id = self.application_id().map(|id| id.to_string()).unwrap_or_else(|| APPLICATION_ID.to_owned());
        glib::idle_add_local_once(move || {
            let Some(app) = weak.upgrade() else { return };
            let check = app.downgrade();
            tessel_gtk::utils::raise_window(&id, &window, move |w| {
                check.upgrade().and_then(|app| app.window()).is_some_and(|wizard| &wizard.window == w)
            });
        });
    }
}
