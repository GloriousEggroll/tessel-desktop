//! `global_settings.py`: the GApplication (`org.waydock.GlobalSettings`).
use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::cli;
use crate::window::{GlobalSettingsWindow, PageFactory};

pub const APPLICATION_ID: &str = "org.waydock.GlobalSettings";

/// The Keyboard Shortcuts overview (`shortcuts_page.CheatSheet`).
pub trait CheatSheet {
    fn present(&self);
    fn close(&self);
    fn closed(&self) -> bool;
    fn window(&self) -> gtk::Window;
}

pub type WindowFactory = Rc<dyn Fn(&gtk::Application) -> GlobalSettingsWindow>;
pub type CheatSheetFactory = Rc<dyn Fn(&gtk::Application) -> Rc<dyn CheatSheet>>;

#[derive(Clone)]
pub struct GlobalSettingsApplication {
    pub app: gtk::Application,
    pub window: Rc<RefCell<Option<GlobalSettingsWindow>>>,
    pub cheat_sheet: Rc<RefCell<Option<Rc<dyn CheatSheet>>>>,
    window_factory: WindowFactory,
    cheat_factory: CheatSheetFactory,
}

pub fn default_window_factory(page_factory: Option<PageFactory>) -> WindowFactory {
    Rc::new(move |app: &gtk::Application| GlobalSettingsWindow::new(app, page_factory.clone()))
}

impl GlobalSettingsApplication {
    pub fn new(application_id: &str, window_factory: Option<WindowFactory>,
               cheat_factory: Option<CheatSheetFactory>) -> GlobalSettingsApplication {
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        glib::set_prgname(Some(application_id));
        let this = GlobalSettingsApplication {
            app: app.clone(),
            window: Rc::default(),
            cheat_sheet: Rc::default(),
            window_factory: window_factory.unwrap_or_else(|| default_window_factory(None)),
            cheat_factory: cheat_factory.unwrap_or_else(|| Rc::new(crate::default_cheat_sheet)),
        };
        let handle = this.clone();
        app.connect_activate(move |_| handle.show(None));
        let handle = this.clone();
        app.connect_command_line(move |_app, command_line| {
            let arguments: Vec<String> = command_line.arguments().iter().skip(1)
                .map(|a| a.to_string_lossy().into_owned()).collect();
            glib::ExitCode::from(handle.command_line(&arguments, Some(command_line)) as u8)
        });
        this
    }

    /// `do_command_line`; returns the exit status.
    pub fn command_line(&self, arguments: &[String], command_line: Option<&gio::ApplicationCommandLine>) -> i32 {
        let args = match cli::parse(arguments) {
            Ok(args) => args,
            Err((code, out, err)) => {
                // argparse prints in the primary instance, like the Python version.
                print!("{out}");
                eprint!("{err}");
                return code;
            }
        };
        if args.quit {
            let window = self.window.borrow().clone();
            if let Some(window) = window.filter(|w| !w.closed.get()) {
                window.window.close();
                if !window.closed.get() {
                    if let Some(line) = command_line {
                        line.printerr_literal("A settings operation is still in progress.\n");
                    } else {
                        eprint!("A settings operation is still in progress.\n");
                    }
                    return 1;
                }
            }
            self.app.quit();
        } else if args.shortcuts {
            self.toggle_shortcuts();
        } else {
            self.show(args.section.as_deref());
        }
        0
    }

    pub fn toggle_shortcuts(&self) {
        let sheet = self.cheat_sheet.borrow().clone();
        if let Some(sheet) = sheet.filter(|s| !s.closed()) {
            // Pressing Super+/ again dismisses the overview.
            sheet.close();
            return;
        }
        let sheet = (self.cheat_factory)(&self.app);
        *self.cheat_sheet.borrow_mut() = Some(sheet.clone());
        sheet.present();
        self.raise(sheet.window());
    }

    pub fn show(&self, section: Option<&str>) {
        let existing = self.window.borrow().clone();
        let window = match existing.filter(|w| !w.closed.get()) {
            Some(window) => window,
            None => {
                let window = (self.window_factory)(&self.app);
                *self.window.borrow_mut() = Some(window.clone());
                window
            }
        };
        if section.is_some() || window.current_section().is_none() {
            let _ = window.show_section(section.unwrap_or("audio"));
        }
        window.window.unminimize();
        window.window.present();
        self.raise(window.window.clone().upcast());
    }

    fn raise(&self, target: gtk::Window) {
        let window = Rc::downgrade(&self.window);
        let sheet = Rc::downgrade(&self.cheat_sheet);
        let id = self.app.application_id().map(|s| s.to_string()).unwrap_or_default();
        glib::idle_add_local_once(move || {
            let valid = move |candidate: &gtk::Window| {
                let is_window = window.upgrade().and_then(|w| w.borrow().clone())
                    .is_some_and(|w| w.window.upcast_ref::<gtk::Window>() == candidate);
                let is_sheet = sheet.upgrade().and_then(|s| s.borrow().clone())
                    .is_some_and(|s| &s.window() == candidate);
                (is_window || is_sheet) && candidate.is_visible()
            };
            if valid(&target) {
                suite_gtk::utils::raise_window(&id, &target, valid);
            }
        });
    }
}

pub fn main(argv: Vec<String>) -> i32 {
    if let Err((code, out, err)) = cli::parse(&argv[1.min(argv.len())..]) {
        print!("{out}");
        eprint!("{err}");
        return code;
    }
    suite_core::i18n::initialize();
    let app = GlobalSettingsApplication::new(APPLICATION_ID, None, None);
    i32::from(app.app.run_with_args(&argv).get())
}
