//! weather_manager.py: the `org.waydock.Weather` GtkApplication. Popup mode is
//! not held (the process exits once the popup closes); `--tray` stays resident.

use crate::backend::{ConfigStore, WeatherClient};
use crate::compat::argparse::{Opt, Parsed, Parser};
use crate::settings::{Services, WeatherSettingsWindow};
use crate::tray::{TrayHost, WeatherTray};
use crate::ui::{WeatherPopup, WeatherSource};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

pub const APPLICATION_ID: &str = "org.waydock.Weather";
const EDGES: &[&str] = &["top", "bottom", "left", "right"];

pub fn parser(prog: &str) -> Parser {
    Parser { prog: prog.into(), description: Some("Weather popup and settings for Waydock; no background service required."),
             options: vec![Opt::flag("--popup", Some("show current weather and the forecast (default)"), true),
                           Opt::flag("--settings", Some("open Weather settings"), true),
                           Opt::flag("--tray", Some("stay in the system tray"), true),
                           Opt::choice("--popup-edge", EDGES), Opt::int("--popup-offset", "POPUP_OFFSET")] }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub settings: bool,
    pub tray: bool,
    pub popup_edge: String,
    pub popup_offset: i32,
}

pub fn arguments(prog: &str, argv: &[String]) -> Result<Args, (i32, String, bool)> {
    let parser = parser(prog);
    let parsed: Parsed = parser.parse(argv)?;
    let offset = parsed.int("--popup-offset").unwrap_or(76);
    if offset < 0 {
        return Err(parser.error("--popup-offset must be nonnegative"));
    }
    Ok(Args { settings: parsed.flag("--settings"), tray: parsed.flag("--tray"),
              popup_edge: parsed.value("--popup-edge").unwrap_or("bottom").into(), popup_offset: offset.min(i32::MAX as i64) as i32 })
}

pub struct WeatherApp {
    pub app: gtk::Application,
    pub store: Arc<ConfigStore>,
    pub client: Arc<dyn WeatherSource>,
    pub services: Services,
    pub popup: RefCell<Option<Rc<WeatherPopup>>>,
    pub settings: RefCell<Option<Rc<WeatherSettingsWindow>>>,
    pub tray: RefCell<Option<WeatherTray>>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    weak_self: RefCell<Weak<WeatherApp>>,
    pub prog: String,
}

impl WeatherApp {
    pub fn new(application_id: &str, client: Option<Arc<dyn WeatherSource>>, store: Option<Arc<ConfigStore>>,
               services: Services, prog: &str) -> Rc<Self> {
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        glib::set_prgname(Some(application_id));
        let store = store.unwrap_or_else(|| Arc::new(ConfigStore::new(None)));
        let client = client.unwrap_or_else(|| {
            Arc::new(WeatherClient::new(store.clone(), None, services.fetch.clone(), None)) as Arc<dyn WeatherSource>
        });
        let this = Rc::new(Self { app, store, client, services, popup: RefCell::new(None), settings: RefCell::new(None),
                                  tray: RefCell::new(None), hold: RefCell::new(None), weak_self: RefCell::new(Weak::new()),
                                  prog: prog.into() });
        this.weak_self.replace(Rc::downgrade(&this));
        this.app.connect_startup(|app| suite_gtk::theme::attach_application(app));
        let weak = Rc::downgrade(&this);
        this.app.connect_command_line(move |_, line| {
            let Some(this) = weak.upgrade() else { return glib::ExitCode::FAILURE };
            glib::ExitCode::from(this.command_line(line) as u8)
        });
        let weak = Rc::downgrade(&this);
        this.app.connect_activate(move |_| if let Some(this) = weak.upgrade() { this.show_popup("bottom", 76) });
        let weak = Rc::downgrade(&this);
        this.app.connect_shutdown(move |_| if let Some(this) = weak.upgrade() { this.shutdown() });
        this
    }

    fn weak(&self) -> Weak<WeatherApp> { self.weak_self.borrow().clone() }

    fn command_line(&self, line: &gio::ApplicationCommandLine) -> i32 {
        let words: Vec<String> = line.arguments().iter().skip(1).map(|w| w.to_string_lossy().into_owned()).collect();
        let args = match arguments(&self.prog, &words) {
            Ok(args) => args,
            Err((code, text, stdout)) => {
                if stdout { line.print_literal(&text) } else { line.printerr_literal(&text) }
                return code;
            }
        };
        if args.tray {
            self.start_tray(&args.popup_edge, args.popup_offset);
        } else if args.settings {
            self.show_settings(false);
        } else {
            self.show_popup(&args.popup_edge, args.popup_offset);
        }
        0
    }

    pub fn start_tray(&self, edge: &str, offset: i32) {
        if self.tray.borrow().is_some() {
            return;
        }
        let Some(bus) = self.app.dbus_connection() else { return };
        self.hold.replace(Some(self.app.hold()));
        let host: Weak<dyn TrayHost> = self.weak();
        let tray = WeatherTray::new(host, bus, self.store.clone(), self.client.clone(), edge, offset);
        self.tray.replace(Some(tray));
    }

    pub fn popup(&self) -> Option<Rc<WeatherPopup>> { self.popup.borrow().clone() }

    pub fn show_popup(&self, edge: &str, offset: i32) {
        if self.popup().is_some_and(|p| p.closed.get()) {
            self.popup.replace(None);
        }
        match self.popup() {
            None => {
                let weak = self.weak();
                let open_settings: Rc<dyn Fn()> = Rc::new(move || if let Some(this) = weak.upgrade() { this.show_settings(false) });
                let weak = self.weak();
                let locate: Rc<dyn Fn()> = Rc::new(move || if let Some(this) = weak.upgrade() { this.show_settings(true) });
                let popup = WeatherPopup::new(&self.app, self.client.clone(), self.store.clone(), open_settings, edge, offset,
                                              Some(locate));
                let weak = self.weak();
                popup.window.connect_visible_notify(move |window| if let Some(this) = weak.upgrade() { this.visibility(window) });
                self.popup.replace(Some(popup.clone()));
                popup.show();
            }
            Some(popup) => {
                let controller = popup.controller();
                if (controller.edge(), controller.offset()) != (edge.to_owned(), offset) {
                    controller.set_position(edge, offset);
                    popup.show();
                } else {
                    popup.activate();
                }
            }
        }
    }

    fn visibility(&self, window: &gtk::ApplicationWindow) {
        let current = self.popup().is_some_and(|p| &p.window == window);
        if !window.get_visible() && current {
            let weak = self.weak();
            let window = window.clone();
            glib::idle_add_local_once(move || if let Some(this) = weak.upgrade() { this.close_hidden_popup(&window) });
        }
    }

    fn close_hidden_popup(&self, window: &gtk::ApplicationWindow) {
        // show_pending: set_position() hid it only to reappear on another edge.
        let Some(popup) = self.popup() else { return };
        if &popup.window == window && !window.get_visible() && !popup.controller().show_pending() {
            self.popup.replace(None);
            popup.destroy();
        }
    }

    pub fn show_settings(&self, locate: bool) {
        if let Some(popup) = self.popup() {
            popup.dismiss();
        }
        if self.settings.borrow().is_none() {
            let window = WeatherSettingsWindow::new(&self.app, Some(self.store.clone()), self.services.clone());
            let weak = self.weak();
            window.window.connect_unrealize(move |w| if let Some(this) = weak.upgrade() {
                let current = this.settings.borrow().as_ref().is_some_and(|s| &s.window == w);
                if current {
                    this.settings.replace(None);
                }
            });
            self.settings.replace(Some(window));
        }
        let Some(settings) = self.settings.borrow().clone() else { return };
        settings.settings.activate();
        if locate {
            // Starts the portal request only after the explicit button press.
            settings.settings.locate_when_ready();
        }
        settings.window.unminimize();
        settings.window.present();
    }

    pub fn shutdown(&self) {
        if let Some(tray) = self.tray.take() {
            tray.close();
        }
        if let Some(popup) = self.popup.take() {
            popup.destroy();
        }
        if let Some(settings) = self.settings.borrow().clone() {
            settings.settings.close();
        }
        self.hold.take();
    }
}

impl TrayHost for WeatherApp {
    fn show_popup(&self, edge: &str, offset: i32) { WeatherApp::show_popup(self, edge, offset) }
    fn show_settings(&self) { WeatherApp::show_settings(self, false) }
    fn quit(&self) { self.app.quit() }
}
