//! The unique `org.waybar.CalendarManager` application (calendar_manager.py):
//! every launch forwards its command line to the primary, which shows the
//! popup (toggling it on repeated clicks) or the reused Date Settings window.

use crate::args;
use crate::backend::{DateBackend, DateSystem};
use crate::popup::CalendarPopup;
use crate::preferences::CalendarPreferencesStore;
use crate::settings::DateSettingsWindow;
use crate::store::EventStore;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const APPLICATION_ID: &str = "org.waybar.CalendarManager";

pub type BackendFactory = Rc<dyn Fn(Box<dyn Fn()>, Box<dyn Fn(&str)>) -> Rc<dyn DateSystem>>;
pub type StoreFactory = Rc<dyn Fn(Box<dyn Fn()>) -> EventStore>;

pub fn real_backend_factory() -> BackendFactory {
    Rc::new(|changed, error| Rc::new(DateBackend::new(changed, error)) as Rc<dyn DateSystem>)
}

pub fn real_store_factory() -> StoreFactory {
    Rc::new(|changed| EventStore::with_defaults(changed))
}

pub struct AppInner {
    pub app: gtk::Application,
    prog: String,
    backend_factory: BackendFactory,
    store_factory: StoreFactory,
    pub backend: RefCell<Option<Rc<dyn DateSystem>>>,
    pub popup: RefCell<Option<CalendarPopup>>,
    pub settings: RefCell<Option<DateSettingsWindow>>,
    popup_position: RefCell<Option<(String, i64)>>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    timer: RefCell<Option<glib::SourceId>>,
    events_timer: RefCell<Option<glib::SourceId>>,
    stopping: Cell<bool>,
    pub preferences: RefCell<CalendarPreferencesStore>,
    pub events: RefCell<Option<EventStore>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

#[derive(Clone)]
pub struct CalendarApp(pub Rc<AppInner>);

fn up(weak: &Weak<AppInner>) -> Option<CalendarApp> {
    weak.upgrade().map(CalendarApp)
}

impl CalendarApp {
    pub fn new(application_id: &str, prog: &str, backend_factory: BackendFactory, store_factory: StoreFactory,
               preferences: CalendarPreferencesStore) -> Self {
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        glib::set_prgname(Some(application_id));
        let this = CalendarApp(Rc::new(AppInner {
            app, prog: prog.into(), backend_factory, store_factory, backend: RefCell::new(None), popup: RefCell::new(None),
            settings: RefCell::new(None), popup_position: RefCell::new(None), hold: RefCell::new(None), timer: RefCell::new(None),
            events_timer: RefCell::new(None), stopping: Cell::new(false), preferences: RefCell::new(preferences),
            events: RefCell::new(None), monitor: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&this.0);
        this.0.app.connect_startup(move |_| if let Some(a) = up(&weak) { a.startup() });
        let weak = Rc::downgrade(&this.0);
        this.0.app.connect_activate(move |_| if let Some(a) = up(&weak) { a.show_popup("bottom", 76) });
        let weak = Rc::downgrade(&this.0);
        this.0.app.connect_command_line(move |_, line| glib::ExitCode::from(up(&weak).map_or(0, |a| a.command_line(line)) as u8));
        let weak = Rc::downgrade(&this.0);
        this.0.app.connect_shutdown(move |_| if let Some(a) = up(&weak) { a.shutdown() });
        this
    }

    fn weak(&self) -> Weak<AppInner> {
        Rc::downgrade(&self.0)
    }

    pub fn backend(&self) -> Option<Rc<dyn DateSystem>> { self.0.backend.borrow().clone() }
    pub fn popup(&self) -> Option<CalendarPopup> { self.0.popup.borrow().clone() }
    pub fn settings(&self) -> Option<DateSettingsWindow> { self.0.settings.borrow().clone() }
    pub fn events(&self) -> Option<EventStore> { self.0.events.borrow().clone() }

    fn startup(&self) {
        let (w1, w2) = (self.weak(), self.weak());
        let backend = (self.0.backend_factory)(Box::new(move || if let Some(a) = up(&w1) { a.changed() }),
                                               Box::new(move |_| if let Some(a) = up(&w2) { a.changed() }));
        self.0.backend.replace(Some(backend.clone()));
        backend.start();
        self.0.hold.replace(Some(self.0.app.hold()));
        let weak = self.weak();
        self.0.timer.replace(Some(glib::timeout_add_seconds_local(15, move || match up(&weak) {
            Some(a) if !a.0.stopping.get() => {
                a.tick();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        })));
        let weak = self.weak();
        let events = (self.0.store_factory)(Box::new(move || if let Some(a) = up(&weak) { a.events_changed() }));
        let sources = self.0.preferences.borrow_mut().load().sources;
        self.0.events.replace(Some(events.clone()));
        events.set_sources(&sources, false);
        self.watch_preferences();
        // Subscribed calendars are re-fetched when their cache is stale.
        let weak = self.weak();
        self.0.events_timer.replace(Some(glib::timeout_add_seconds_local(30 * 60, move || match up(&weak) {
            Some(a) if !a.0.stopping.get() => {
                if let Some(events) = a.events() {
                    if !events.sources().is_empty() && !events.loading() {
                        events.reload();
                    }
                }
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        })));
    }

    fn command_line(&self, line: &gio::ApplicationCommandLine) -> i32 {
        let words: Vec<String> = line.arguments().iter().skip(1).map(|w| w.to_string_lossy().into_owned()).collect();
        match args::parse(&self.0.prog, &words) {
            Ok(parsed) => {
                if parsed.settings {
                    self.show_settings();
                } else {
                    self.show_popup(&parsed.popup_edge, parsed.popup_offset);
                }
                0
            }
            Err((code, text, stdout)) => {
                if stdout { line.print_literal(&text) } else { line.printerr_literal(&text) }
                code
            }
        }
    }

    fn watch_preferences(&self) {
        let path = self.0.preferences.borrow().path.clone();
        if let Ok(monitor) = gio::File::for_path(path).monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            let weak = self.weak();
            monitor.connect_changed(move |_, _, _, _| if let Some(a) = up(&weak) { a.preferences_changed() });
            self.0.monitor.replace(Some(monitor));
        }
    }

    pub fn preferences_changed(&self) {
        if self.0.stopping.get() {
            return;
        }
        let preferences = self.0.preferences.borrow_mut().load();
        if let Some(events) = self.events() {
            events.set_sources(&preferences.sources, false);
        }
        if let Some(popup) = self.popup() {
            popup.set_first_weekday(preferences.first_weekday());
        }
    }

    fn events_changed(&self) {
        if !self.0.stopping.get() {
            if let Some(popup) = self.popup() {
                popup.events_changed();
            }
        }
    }

    pub fn show_popup(&self, edge: &str, offset: i64) {
        let position = (edge.to_owned(), offset);
        if self.popup().is_some() && self.0.popup_position.borrow().as_ref() != Some(&position) {
            if let Some(popup) = self.0.popup.take() {
                popup.destroy();
            }
        }
        if let Some(popup) = self.popup() {
            popup.activate();
            return;
        }
        let Some(backend) = self.backend() else { return };
        let weak = self.weak();
        let first_weekday = self.0.preferences.borrow_mut().load().first_weekday();
        let popup = CalendarPopup::new(&self.0.app, Rc::new(move || if let Some(a) = up(&weak) { a.show_settings() }),
                                       edge, offset.clamp(0, i32::MAX as i64) as i32,
                                       Rc::new(move || backend.now().date()), self.events(), Some(first_weekday));
        self.0.popup.replace(Some(popup.clone()));
        self.0.popup_position.replace(Some(position));
        let weak = self.weak();
        popup.window().connect_unrealize(move |window| if let Some(a) = up(&weak) {
            let same = a.popup().is_some_and(|p| p.window() == window);
            if same {
                a.0.popup.replace(None);
            }
        });
        popup.show();
    }

    pub fn show_settings(&self) {
        if let Some(popup) = self.popup() {
            popup.dismiss();
        }
        if self.settings().is_none() {
            let Some(backend) = self.backend() else { return };
            let settings = DateSettingsWindow::new(&self.0.app, backend);
            let weak = self.weak();
            settings.window().connect_unrealize(move |window| if let Some(a) = up(&weak) {
                if a.settings().is_some_and(|s| s.window() == window) {
                    a.0.settings.replace(None);
                }
            });
            self.0.settings.replace(Some(settings));
        }
        let settings = self.settings().unwrap();
        let window = settings.window().clone();
        window.set_visible(true);
        settings.refresh();
        window.unminimize();
        window.present();
        let weak = self.weak();
        glib::idle_add_local_once(move || {
            let Some(app) = up(&weak) else { return };
            let id = app.0.app.application_id().map(|s| s.to_string()).unwrap_or_default();
            if window.surface().is_none() {
                return;
            }
            let check = app.weak();
            suite_gtk::utils::raise_window(&id, window.upcast_ref(), move |w| {
                up(&check).and_then(|a| a.settings()).is_some_and(|s| s.window().upcast_ref::<gtk::Window>() == w)
            });
        });
    }

    fn changed(&self) {
        if self.0.stopping.get() {
            return;
        }
        if let Some(popup) = self.popup() {
            popup.refresh();
        }
        if let Some(settings) = self.settings() {
            settings.refresh();
        }
    }

    fn tick(&self) {
        if let Some(popup) = self.popup() {
            if popup.window().is_visible() {
                popup.refresh();
            }
        }
        if let Some(settings) = self.settings() {
            if settings.window().is_visible() {
                settings.refresh();
            }
        }
    }

    fn shutdown(&self) {
        self.0.stopping.set(true);
        for timer in [&self.0.timer, &self.0.events_timer] {
            if let Some(source) = timer.take() {
                crate::view::remove_source(source);
            }
        }
        if let Some(monitor) = self.0.monitor.take() {
            monitor.cancel();
        }
        if let Some(events) = self.events() {
            events.close();
        }
        if let Some(backend) = self.backend() {
            backend.close();
        }
        if let Some(popup) = self.0.popup.take() {
            popup.destroy();
        }
        self.0.hold.take();
    }

    pub fn run(&self, argv: &[String]) -> glib::ExitCode {
        self.0.app.run_with_args(argv)
    }
}
