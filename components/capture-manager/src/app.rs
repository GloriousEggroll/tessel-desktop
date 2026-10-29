//! GApplication `org.waydock.Capture` (capture.py `create_application` / `main`).

use crate::args::{self, Options, Outcome};
use crate::media::{Encoder, Frame, RecordingOptions};
use crate::native::{FeedFactory, NativeFeed};
use crate::notifications::{CaptureNotifications, OpenSaved};
use crate::tray::CaptureTray;
use crate::ui::CaptureWindow;
use crate::ID;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub type EncoderChooser = Arc<dyn Fn(&RecordingOptions, &Frame) -> Result<Encoder, String> + Send + Sync>;

/// Replaceable backends (tests inject fakes; production uses the defaults).
#[derive(Clone)]
pub struct Hooks {
    /// Opens a frame source from a portal PipeWire fd (default: libcapture-pipewire.so).
    pub feed_factory: FeedFactory,
    /// Chooses the encoder for a recording (default: Vulkan probe, then software).
    pub encoder_chooser: EncoderChooser,
    /// Opens/reveals a saved file (default: `results::open_saved`).
    pub open_saved: OpenSaved,
    /// Bus for portal sessions (default: the session bus via `Gio.bus_get`).
    pub portal_bus: Option<gio::DBusConnection>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            feed_factory: NativeFeed::factory(),
            encoder_chooser: Arc::new(crate::media::choose_encoder),
            open_saved: Rc::new(crate::results::open_saved),
            portal_bus: None,
        }
    }
}

pub struct AppInner {
    pub gtk: gtk::Application,
    pub window: RefCell<Option<CaptureWindow>>,
    pub tray: RefCell<Option<CaptureTray>>,
    pub notifications: RefCell<Option<CaptureNotifications>>,
    pub hooks: Hooks,
}

#[derive(Clone)]
pub struct CaptureApp(pub Rc<AppInner>);

impl CaptureApp {
    pub fn new(hooks: Hooks) -> Self {
        glib::set_prgname(Some(ID));
        let gtk = gtk::Application::builder().application_id(ID).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
        let this = Self(Rc::new(AppInner { gtk: gtk.clone(), window: RefCell::default(), tray: RefCell::default(),
                                           notifications: RefCell::default(), hooks }));
        let weak = Rc::downgrade(&this.0);
        gtk.connect_startup(move |application| {
            let Some(inner) = weak.upgrade() else { return };
            CaptureApp(inner).startup(application);
        });
        let weak = Rc::downgrade(&this.0);
        gtk.connect_shutdown(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            if let Some(notifications) = inner.notifications.borrow().as_ref() {
                notifications.close();
            }
            let tray = inner.tray.borrow_mut().take();
            if let Some(tray) = tray {
                tray.close();
            }
        });
        let weak = Rc::downgrade(&this.0);
        gtk.connect_activate(move |_| {
            if let Some(inner) = weak.upgrade() {
                CaptureApp(inner).activate();
            }
        });
        let weak = Rc::downgrade(&this.0);
        gtk.connect_command_line(move |_, command| {
            let Some(inner) = weak.upgrade() else { return glib::ExitCode::FAILURE };
            let argv: Vec<String> = command.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
            let prog = args::prog_name(argv.first().map(String::as_str));
            match args::parse(&prog, argv.get(1..).unwrap_or(&[])) {
                Outcome::Parsed(options) => glib::ExitCode::from(CaptureApp(inner).command_line(&options) as u8),
                Outcome::Help(text) => {
                    command.print_literal(&text);
                    glib::ExitCode::SUCCESS
                }
                Outcome::Error(text) => {
                    command.printerr_literal(&text);
                    glib::ExitCode::from(2)
                }
            }
        });
        this
    }

    fn startup(&self, application: &gtk::Application) {
        crate::results::register_actions(application, self.0.hooks.open_saved.clone());
        *self.0.notifications.borrow_mut() = Some(CaptureNotifications::new(application, self.0.hooks.open_saved.clone()));
        *self.0.tray.borrow_mut() = Some(CaptureTray::new(application.dbus_connection()));
        let locked = gio::SimpleAction::new("lock-shot", Some(glib::VariantTy::DOUBLE));
        let weak = Rc::downgrade(&self.0);
        locked.connect_activate(move |_, value| {
            let Some(inner) = weak.upgrade() else { return };
            let window = inner.window.borrow().clone();
            if let Some(window) = window {
                window.locked_shot(value.and_then(|v| v.get::<f64>()));
            }
        });
        application.add_action(&locked);
        let show = gio::SimpleAction::new("show", None);
        let weak = Rc::downgrade(&self.0);
        show.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                CaptureApp(inner).activate();
            }
        });
        application.add_action(&show);
        let stop = gio::SimpleAction::new("stop", None);
        let weak = Rc::downgrade(&self.0);
        stop.connect_activate(move |_, _| {
            let Some(inner) = weak.upgrade() else { return };
            let window = inner.window.borrow().clone();
            if let Some(window) = window {
                window.stop_clicked();
            }
        });
        application.add_action(&stop);
    }

    pub fn window(&self) -> Option<CaptureWindow> {
        self.0.window.borrow().clone()
    }

    pub fn ensure_window(&self) -> CaptureWindow {
        let current = self.window();
        match current {
            Some(window) if !window.0.closed.get() => window,
            _ => {
                let window = CaptureWindow::new(&self.0);
                *self.0.window.borrow_mut() = Some(window.clone());
                window
            }
        }
    }

    pub fn activate(&self) {
        let window = self.ensure_window();
        window.0.quick_capture.set(false);
        window.window().unminimize();
        window.window().present();
    }

    /// `do_command_line` in the primary instance; returns the exit status.
    pub fn command_line(&self, options: &Options) -> i32 {
        if options.lock_shot {
            return match self.window() {
                Some(window) if window.locked_shot(None) => 0,
                _ => 1,
            };
        }
        let busy = self.window().is_some_and(|w| w.0.busy.get());
        if options.stop || (options.toggle_recording && busy) {
            if let Some(window) = self.window() {
                if window.0.busy.get() && window.0.capture_recording.get() {
                    window.stop_clicked();
                    return 0;
                }
            }
            return 1;
        }
        if options.start_recording || options.toggle_recording || options.capture.is_some() {
            let window = self.ensure_window();
            if window.0.busy.get() {
                return 1;
            }
            window.0.quick_capture.set(true);
            if let Some(target) = &options.capture {
                window.prepare(false, Some(target.clone()), false);
            } else {
                window.start_recording(options.audio_preset.clone(), !options.toggle_recording);
            }
            return 0;
        }
        self.activate();
        if let Some(window) = self.window() {
            if !window.0.busy.get() {
                if options.record {
                    window.0.w.mode.set_selected(1);
                } else if options.screenshot {
                    window.0.w.mode.set_selected(0);
                }
            }
        }
        0
    }
}

pub fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let prog = args::prog_name(argv.first().map(String::as_str));
    let options = match args::parse(&prog, argv.get(1..).unwrap_or(&[])) {
        Outcome::Parsed(options) => options,
        Outcome::Help(text) => {
            print!("{text}");
            return std::process::ExitCode::SUCCESS;
        }
        Outcome::Error(text) => {
            eprint!("{text}");
            return std::process::ExitCode::from(2);
        }
    };
    if options.check {
        let result = crate::check::check();
        println!("{}", crate::check::text(&result));
        return if result["ready"] == serde_json::json!(true) { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::from(1) };
    }
    suite_core::i18n::initialize();
    let app = CaptureApp::new(Hooks::default());
    let code = app.0.gtk.run_with_args(&argv);
    std::process::ExitCode::from(code.get())
}
