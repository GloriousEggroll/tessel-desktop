//! `class BluetoothApplication`: the unique `org.btmanager.Bluetooth`
//! GtkApplication (`HANDLES_COMMAND_LINE`). Every launch forwards its full
//! argv to the primary, which re-parses it, updates the popup placement and
//! activates: first activation builds the UI and the OBEX receiver; later
//! ones toggle the popup or present and raise the window.

use crate::args;
use crate::backend::Bt;
use crate::dbus::{Conn, GioConn};
use crate::env::GlibEnv;
use crate::manager::Services;
use crate::ui::{obex_sender_factory, Options, SenderFactory, Ui};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub const APPLICATION_ID: &str = "org.btmanager.Bluetooth";

/// Test seams (Python tests patch `BT`, `ObexSender`, `ObexReceiver`, `Notifier`).
#[derive(Clone)]
pub struct Hooks {
    /// The BlueZ connection; default: the system bus (`Gio.bus_get_sync`).
    pub bluez: Rc<dyn Fn() -> Result<Rc<dyn Conn>, String>>,
    pub services: Services,
    pub sender_factory: SenderFactory,
    /// Connection for the OBEX receiver; default: the application's own.
    pub receive_bus: Option<Rc<dyn Fn(&gtk::Application) -> Option<Rc<dyn Conn>>>>,
    pub handle_sigint: bool,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            bluez: Rc::new(|| gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>)
                .map(|c| GioConn::new(c) as Rc<dyn Conn>).map_err(|e| e.message().to_owned())),
            services: Services::default(),
            sender_factory: obex_sender_factory(),
            receive_bus: None,
            handle_sigint: true,
        }
    }
}

pub struct PopupOptions {
    pub popup: bool,
    pub edge: String,
    pub offset: i32,
}

pub struct Inner {
    pub app: gtk::Application,
    pub force_lang: RefCell<Option<String>>,
    pub popup_options: RefCell<PopupOptions>,
    pub controller: RefCell<Option<Rc<Ui>>>,
    pub hooks: Hooks,
    pub prog: String,
}

#[derive(Clone)]
pub struct BluetoothApplication(pub Rc<Inner>);

/// Ask labwc to focus and raise an owned window (`_raise_window`).
pub fn raise_child(app: &gtk::Application, window: &gtk::Window) {
    let id = app.application_id().map(|s| s.to_string()).unwrap_or_else(|| APPLICATION_ID.into());
    let window = window.clone();
    glib::idle_add_local_once(move || suite_gtk::utils::raise_window(&id, &window, |w| w.get_visible()));
}

impl BluetoothApplication {
    pub fn new(application_id: &str, force_lang: Option<String>, popup: PopupOptions, hooks: Hooks, prog: &str) -> Self {
        // Keep the process name consistent with the application identity.
        glib::set_prgname(Some(application_id));
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        let this = Self(Rc::new(Inner { app: app.clone(), force_lang: RefCell::new(force_lang), popup_options: RefCell::new(popup),
                                        controller: RefCell::new(None), hooks, prog: prog.into() }));
        let weak = Rc::downgrade(&this.0);
        app.connect_command_line(move |_, line| {
            let code = weak.upgrade().map(|inner| BluetoothApplication(inner).command_line(line)).unwrap_or(0);
            glib::ExitCode::from(code as u8)
        });
        let weak = Rc::downgrade(&this.0);
        app.connect_activate(move |_| if let Some(inner) = weak.upgrade() { BluetoothApplication(inner).activate() });
        this
    }

    pub fn controller(&self) -> Option<Rc<Ui>> {
        self.0.controller.borrow().clone()
    }

    fn command_line(&self, line: &gio::ApplicationCommandLine) -> i32 {
        let words: Vec<String> = line.arguments().iter().skip(1).map(|w| w.to_string_lossy().into_owned()).collect();
        self.handle_arguments(&words)
    }

    /// `do_command_line` with the forwarded argv (minus argv[0]).
    pub fn handle_arguments(&self, words: &[String]) -> i32 {
        let parsed = match args::parse(&self.0.prog, words) {
            Ok(parsed) => parsed,
            Err((code, text, stdout)) => {
                if stdout { print!("{text}") } else { eprint!("{text}") }
                return code;
            }
        };
        if self.controller().is_none() {
            if let Some(lang) = &parsed.lang {
                self.0.force_lang.replace(Some(suite_core::i18n::normalize_language(Some(lang))));
            }
        }
        {
            let mut options = self.0.popup_options.borrow_mut();
            if parsed.popup {
                options.popup = true;
            }
            if let Some(edge) = parsed.popup_edge {
                options.edge = edge;
            }
            if let Some(offset) = parsed.popup_offset {
                options.offset = offset;
            }
        }
        if let Some(popup) = self.controller().and_then(|c| c.popup.borrow().clone()) {
            let options = self.0.popup_options.borrow();
            popup.set_position(&options.edge, options.offset);
        }
        self.0.app.activate();
        0
    }

    pub fn activate(&self) {
        let existing = self.controller().is_some_and(|c| !c.manager.closing());
        if !existing {
            let lang = self.0.force_lang.borrow().clone()
                .unwrap_or_else(|| suite_core::i18n::normalize_language(Some(&suite_core::i18n::selected_language())));
            let conn = match (self.0.hooks.bluez)() {
                Ok(conn) => conn,
                Err(message) => {
                    eprintln!("btmanager: no system bus for Bluetooth: {message}");
                    return;
                }
            };
            let (popup, edge, offset) = {
                let o = self.0.popup_options.borrow();
                (o.popup, o.edge.clone(), o.offset)
            };
            let ui = Ui::new(Options {
                application: Some(self.0.app.clone()), popup, popup_edge: edge, popup_offset: offset, lang,
                bt: Bt::new(conn), env: GlibEnv::new(), services: self.0.hooks.services.clone(),
                sender_factory: self.0.hooks.sender_factory.clone(), handle_sigint: self.0.hooks.handle_sigint,
            });
            self.0.controller.replace(Some(ui.clone()));
            // Only the primary process answers incoming OBEX pushes.
            let bus = match &self.0.hooks.receive_bus {
                Some(hook) => hook(&self.0.app),
                None => self.0.app.dbus_connection().map(|c| GioConn::new(c) as Rc<dyn Conn>),
            };
            match bus {
                Some(bus) => ui.manager.start_receiver(bus),
                None => eprintln!("btmanager: no session bus for receiving files"),
            }
        }
        let ui = self.controller().expect("controller");
        let popup = ui.popup.borrow().clone();
        if let Some(popup) = popup {
            if existing {
                popup.activate();
            }
            return;
        }
        // Forwarded startup/activation information lets the desktop focus
        // this existing window.
        let window = ui.window.clone();
        window.unminimize();
        window.present();
        if existing {
            let weak = Rc::downgrade(&ui);
            let id = self.0.app.application_id().map(|s| s.to_string()).unwrap_or_default();
            glib::idle_add_local_once(move || {
                suite_gtk::utils::raise_window(&id, &window, move |w| {
                    weak.upgrade().is_some_and(|ui| !ui.manager.closing()
                        && (*w == ui.window || ui.add.borrow().as_ref().is_some_and(|a| a.window == *w) || ui.is_tracked(w)))
                });
            });
        }
    }

    pub fn run(&self, argv: &[String]) -> glib::ExitCode {
        self.0.app.run_with_args(argv)
    }
}
