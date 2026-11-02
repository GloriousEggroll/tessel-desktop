//! The GApplication (`org.tessel.Welcome`, HANDLES_COMMAND_LINE).
use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::cli;
use crate::ui::{step_keys, Options, WelcomeWindow, APPLICATION_ID};

pub type WindowFactory = Rc<dyn Fn(&gtk::Application, bool) -> WelcomeWindow>;

#[derive(Clone)]
pub struct WelcomeApplication {
    pub app: gtk::Application,
    pub window: Rc<RefCell<Option<WelcomeWindow>>>,
    factory: WindowFactory,
}

impl WelcomeApplication {
    pub fn new(application_id: &str, factory: Option<WindowFactory>) -> WelcomeApplication {
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        glib::set_prgname(Some(application_id));
        let factory = factory.unwrap_or_else(|| Rc::new(|app: &gtk::Application, first_run: bool| {
            WelcomeWindow::new(app, Options { first_run, ..Options::default() })
        }));
        let this = WelcomeApplication { app: app.clone(), window: Rc::default(), factory };
        let handle = this.clone();
        app.connect_command_line(move |_app, line| {
            let arguments: Vec<String> = line.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
            glib::ExitCode::from(handle.command_line(&arguments) as u8)
        });
        this
    }

    pub fn command_line(&self, arguments: &[String]) -> i32 {
        let args = match cli::parse(arguments) {
            Ok(args) => args,
            Err((code, out, err)) => {
                print!("{out}");
                eprint!("{err}");
                return code;
            }
        };
        let existing = self.window.borrow().clone();
        let window = match existing.filter(|w| !w.closed.get()) {
            Some(window) => window,
            None => {
                let window = (self.factory)(&self.app, args.first_run);
                *self.window.borrow_mut() = Some(window.clone());
                window
            }
        };
        if let Some(page) = args.page.as_deref().filter(|p| step_keys().contains(p)) {
            window.show_step(page);
        }
        window.window.present();
        0
    }
}

pub fn main(argv: Vec<String>) -> i32 {
    if let Err((code, out, err)) = cli::parse(&argv[1.min(argv.len())..]) {
        print!("{out}");
        eprint!("{err}");
        return code;
    }
    tessel_core::i18n::initialize();
    let app = WelcomeApplication::new(APPLICATION_ID, None);
    i32::from(app.app.run_with_args(&argv).get())
}
