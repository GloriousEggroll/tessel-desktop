//! calculator.py: the unique GApplication and `main()`.

use crate::cli::{self, Options};
use crate::ui::CalculatorWindow;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub type WindowFactory = Rc<dyn Fn(&gtk::Application) -> CalculatorWindow>;

/// `CalculatorApplication`: one window, reused (and raised) by later launches.
#[derive(Clone)]
pub struct CalculatorApp {
    pub app: gtk::Application,
    pub window: Rc<RefCell<Option<CalculatorWindow>>>,
    factory: WindowFactory,
}

impl CalculatorApp {
    pub fn window(&self) -> Option<CalculatorWindow> { self.window.borrow().clone() }

    pub fn show(&self, options: Option<&Options>) {
        let current = self.window().filter(|w| !w.closed.get());
        let window = match current {
            Some(window) => window,
            None => {
                let window = (self.factory)(&self.app);
                self.window.replace(Some(window.clone()));
                window
            }
        };
        if let Some(options) = options {
            if options.scientific {
                window.mode_dropdown.set_selected(1);
            }
            if let Some(expression) = &options.expression {
                window.set_expression(expression);
                window.calculate();
            }
        }
        window.window.unminimize();
        window.window.present();
        let (slot, id) = (Rc::downgrade(&self.window), self.app.application_id().map(|s| s.to_string()).unwrap_or_default());
        let target = window.window.clone();
        glib::idle_add_local_once(move || {
            let valid = move |candidate: &gtk::Window| {
                slot.upgrade().and_then(|s| s.borrow().clone())
                    .is_some_and(|w| !w.closed.get() && w.window.upcast_ref::<gtk::Window>() == candidate)
            };
            suite_gtk::utils::raise_window(&id, target.upcast_ref(), valid);
        });
    }
}

pub fn create_application(application_id: &str, factory: Option<WindowFactory>) -> CalculatorApp {
    suite_core::i18n::initialize();
    let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
    glib::set_prgname(Some(application_id));
    let factory = factory.unwrap_or_else(|| Rc::new(|app: &gtk::Application| CalculatorWindow::new(app, None)));
    let this = CalculatorApp { app: app.clone(), window: Rc::default(), factory };
    let handle = this.clone();
    app.connect_activate(move |_| handle.show(None));
    let handle = this.clone();
    app.connect_command_line(move |app, command_line| {
        let arguments: Vec<String> = command_line.arguments().iter().skip(1)
            .map(|a| a.to_string_lossy().into_owned()).collect();
        let options = match cli::arguments(&arguments) {
            Ok(options) => options,
            Err(exit) => {
                // argparse prints in the primary instance, like the Python version.
                print!("{}", exit.stdout);
                eprint!("{}", exit.stderr);
                return glib::ExitCode::from(exit.code as u8);
            }
        };
        if options.quit {
            if let Some(window) = handle.window().filter(|w| !w.closed.get()) {
                window.window.close();
            }
            app.quit();
        } else {
            handle.show(Some(&options));
        }
        glib::ExitCode::SUCCESS
    });
    this
}

pub fn main(argv: Vec<String>) -> i32 {
    let options = match cli::arguments(&argv[1.min(argv.len())..]) {
        Ok(options) => options,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return exit.code;
        }
    };
    if options.check {
        let status = cli::check();
        println!("{status}");
        return if status.starts_with("{\"ready\": true") { 0 } else { 1 };
    }
    let app = create_application(crate::APPLICATION_ID, None);
    i32::from(app.app.run_with_args(&argv).get())
}
