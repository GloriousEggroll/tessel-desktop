//! Port of `~/tessel-bluetooth/tessel-shell/wayshell_prompt.py` (labwc promptCommand /
//! errorCommand dialog) and the pure parts of `install_prompts.py`.
//!
//! labwc's action protocol is 0=yes, 254=dismiss (run neither branch), and any
//! other status=no. This helper displays text only; it never executes an action.

pub mod cli;
pub mod install;
pub mod window;

pub const ACCEPT: i32 = 0;
pub const DECLINE: i32 = 1;
pub const DISMISS: i32 = 254;
pub const TEXT_LIMIT: usize = 65536;
pub const APPLICATION_ID: &str = "org.tessel.Prompt";

pub use cli::{arguments, Options, Parsed};
pub use window::PromptWindow;

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tessel_theme::WayshellTheme;

/// `main(argv)`: parse, run the GApplication, return the labwc exit status.
pub fn run(argv0: &str, args: &[String]) -> i32 {
    let prog = std::path::Path::new(argv0).file_name().map_or_else(|| argv0.to_owned(), |n| n.to_string_lossy().into_owned());
    let options = match cli::parse(&prog, args) {
        Parsed::Run(options) => options,
        Parsed::Exit { code, stdout, stderr } => {
            use std::io::Write;
            let _ = std::io::stdout().write_all(stdout.as_bytes());
            let _ = std::io::stderr().write_all(stderr.as_bytes());
            return code;
        }
    };
    let app = gtk::Application::new(Some(APPLICATION_ID), gio::ApplicationFlags::NON_UNIQUE);
    let result = Rc::new(Cell::new(if options.error { ACCEPT } else { DISMISS }));
    let theme: Rc<RefCell<Option<Rc<WayshellTheme>>>> = Rc::default();
    let window: Rc<RefCell<Option<Rc<PromptWindow>>>> = Rc::default();
    {
        let (result, theme, window) = (result.clone(), theme.clone(), window.clone());
        app.connect_activate(move |app| {
            theme.replace(Some(WayshellTheme::new(None)));
            let (result, quit_app) = (result.clone(), app.clone());
            let prompt = PromptWindow::new(app, options.clone(), move |code| {
                result.set(code);
                quit_app.quit();
            }, None);
            prompt.window.present();
            prompt.default_button.grab_focus();
            window.replace(Some(prompt));
        });
    }
    let signal = {
        let (app, window) = (app.clone(), window.clone());
        tessel_core::glib_compat::unix_signal_add_local(libc::SIGTERM, move || {
            let prompt = window.borrow().clone();
            match prompt {
                Some(prompt) => prompt.close_requested(),
                None => app.quit(),
            }
            glib::ControlFlow::Break
        })
    };
    app.run_with_args(&[argv0]);
    if let Some(theme) = theme.take() {
        theme.close();
    }
    // GLib removes the signal source itself if the handler ran.
    tessel_core::glib_compat::source_remove(signal);
    result.get()
}
