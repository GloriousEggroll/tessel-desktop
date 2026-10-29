//! The `org.waybar.AudioManager` GtkApplication (audio_manager.py): settings
//! window, panel popup and volume-key OSD sharing one backend. All argv of a
//! second launch are forwarded to the primary through
//! `org.gtk.Application.CommandLine`.

use crate::api::AudioApi;
use crate::backend::{AudioBackend, BackendOptions};
use crate::cli::{app_arguments, Mode};
use crate::fake::{FakeAudioBackend, FakeMode};
use crate::ui::popup::VolumePopup;
use crate::ui::settings::AudioSettingsWindow;
use crate::volume_keys::{KeyAction, Shown, VolumeKeys};
use suite_core::scheduler::GlibScheduler;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use suite_gtk::gtk;
use suite_gtk::osd::{Adjustment, AdjustmentOsd};

pub const APPLICATION_ID: &str = "org.waybar.AudioManager";

/// Test hook: an in-memory backend instead of PipeWire. Honoured only outside
/// the live session runtime directory so it can never mask real audio there.
pub const TEST_BACKEND_ENV: &str = "AUDIO_MANAGER_TEST_BACKEND";

pub type BackendFactory = Box<dyn Fn(Box<dyn Fn()>, Box<dyn Fn(&str)>) -> Rc<dyn AudioApi>>;

pub struct Inner {
    weak: RefCell<Weak<Inner>>,
    pub app: gtk::Application,
    factory: RefCell<Option<BackendFactory>>,
    pub backend: RefCell<Option<Rc<dyn AudioApi>>>,
    pub settings: RefCell<Option<Rc<AudioSettingsWindow>>>,
    pub popup: RefCell<Option<Rc<VolumePopup>>>,
    popup_position: RefCell<Option<(String, i64)>>,
    held: RefCell<Option<gio::ApplicationHoldGuard>>,
    key_held: RefCell<Option<gio::ApplicationHoldGuard>>,
    stopping: Cell<bool>,
    pub volume_keys: RefCell<Option<VolumeKeys>>,
    pub osd: RefCell<Option<AdjustmentOsd>>,
    /// Number of backends created (tests: one per process).
    pub backends_created: Cell<u32>,
}

#[derive(Clone)]
pub struct AudioApplication(pub Rc<Inner>);

fn test_backend() -> Option<FakeMode> {
    let mode = std::env::var(TEST_BACKEND_ENV).ok()?;
    let live = std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
    if suite_core::paths::runtime_dir() == live {
        return None;
    }
    match mode.as_str() {
        "populated" => Some(FakeMode::Populated),
        "empty" => Some(FakeMode::Empty),
        "unavailable" => Some(FakeMode::Unavailable),
        "keys" => Some(FakeMode::Keys),
        _ => None,
    }
}

pub fn default_factory() -> BackendFactory {
    Box::new(|changed, error| match test_backend() {
        Some(mode) => {
            let fake = FakeAudioBackend::new(mode);
            fake.set_callbacks(changed, error);
            fake
        }
        None => Rc::new(AudioBackend::new(changed, error, BackendOptions::native(true))),
    })
}

impl AudioApplication {
    pub fn new(application_id: &str, factory: Option<BackendFactory>) -> Self {
        let app = gtk::Application::new(Some(application_id), gio::ApplicationFlags::HANDLES_COMMAND_LINE);
        glib::set_prgname(Some(application_id));
        let inner = Rc::new(Inner {
            weak: RefCell::new(Weak::new()), app, factory: RefCell::new(Some(factory.unwrap_or_else(default_factory))),
            backend: RefCell::new(None), settings: RefCell::new(None), popup: RefCell::new(None), popup_position: RefCell::new(None),
            held: RefCell::new(None), key_held: RefCell::new(None), stopping: Cell::new(false), volume_keys: RefCell::new(None),
            osd: RefCell::new(None), backends_created: Cell::new(0),
        });
        *inner.weak.borrow_mut() = Rc::downgrade(&inner);
        let this = Self(inner);
        let weak = this.weak();
        this.0.app.connect_startup(move |_| Self::with(&weak, |a| a.startup()));
        let weak = this.weak();
        this.0.app.connect_activate(move |_| Self::with(&weak, |a| a.show_settings()));
        let weak = this.weak();
        this.0.app.connect_command_line(move |_, command_line| {
            let mut code = 0;
            Self::with(&weak, |a| code = a.command_line(command_line));
            glib::ExitCode::from(code as u8)
        });
        let weak = this.weak();
        this.0.app.connect_shutdown(move |_| Self::with(&weak, |a| a.shutdown()));
        this
    }

    fn weak(&self) -> Weak<Inner> {
        self.0.weak.borrow().clone()
    }

    fn with(weak: &Weak<Inner>, f: impl FnOnce(AudioApplication)) {
        if let Some(inner) = weak.upgrade() {
            f(AudioApplication(inner));
        }
    }

    pub fn backend(&self) -> Option<Rc<dyn AudioApi>> {
        self.0.backend.borrow().clone()
    }

    pub fn key_held(&self) -> bool {
        self.0.key_held.borrow().is_some()
    }

    pub fn held(&self) -> bool {
        self.0.held.borrow().is_some()
    }

    fn startup(&self) {
        let (changed, error) = (self.weak(), self.weak());
        let factory = self.0.factory.borrow_mut().take().expect("startup runs once");
        let backend = factory(
            Box::new(move || Self::with(&changed, |a| a.changed())),
            Box::new(move |message| { let message = message.to_owned(); Self::with(&error, |a| a.error(&message)) }),
        );
        self.0.backends_created.set(self.0.backends_created.get() + 1);
        *self.0.backend.borrow_mut() = Some(backend.clone());
        let weak = self.weak();
        let keys = VolumeKeys::new(backend.clone(), move |shown| Self::with(&weak, |a| a.show_adjustment(shown)),
                                   Rc::new(GlibScheduler::default()), crate::timer::monotonic);
        *self.0.volume_keys.borrow_mut() = Some(keys);
        backend.start();
    }

    fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> i32 {
        let argv: Vec<String> = command_line.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let prog = command_line.arguments().first()
            .and_then(|a| std::path::Path::new(a).file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "audio_manager.py".into());
        let args = match app_arguments(&argv, &prog) {
            Ok(args) => args,
            Err(exit) => {
                print!("{}", exit.stdout);
                eprint!("{}", exit.stderr);
                return exit.code;
            }
        };
        match args.mode {
            Mode::VolumeUp => self.adjust_volume(KeyAction::Up),
            Mode::VolumeDown => self.adjust_volume(KeyAction::Down),
            Mode::ToggleMute => self.adjust_volume(KeyAction::Mute),
            Mode::Popup => self.show_popup(&args.popup_edge, args.popup_offset),
            Mode::Settings | Mode::Default => self.show_settings(),
        }
        0
    }

    pub fn adjust_volume(&self, action: KeyAction) {
        if self.0.key_held.borrow().is_none() {
            let guard = self.0.app.hold();
            *self.0.key_held.borrow_mut() = Some(guard);
        }
        let keys = self.0.volume_keys.borrow().clone();
        if let Some(keys) = keys {
            keys.request(action);
        }
    }

    fn show_adjustment(&self, shown: Shown) {
        if self.0.osd.borrow().is_none() {
            let weak = self.weak();
            let osd = AdjustmentOsd::new(Some(Box::new(move || Self::with(&weak, |a| a.adjustment_dismissed()))), 1500);
            *self.0.osd.borrow_mut() = Some(osd);
        }
        let osd = self.0.osd.borrow().clone().unwrap();
        osd.show(Adjustment::Volume, shown.value, shown.muted.unwrap_or(false), None, &shown.detail);
    }

    fn adjustment_dismissed(&self) {
        let pending = self.0.volume_keys.borrow().as_ref().is_some_and(|k| k.pending());
        if self.0.key_held.borrow().is_some() && !pending {
            let guard = self.0.key_held.borrow_mut().take();
            drop(guard);
        }
    }

    pub fn show_popup(&self, edge: &str, offset: i64) {
        if self.0.held.borrow().is_none() {
            // Keep one backend alive when the menu is dismissed.
            let guard = self.0.app.hold();
            *self.0.held.borrow_mut() = Some(guard);
        }
        let position = (edge.to_owned(), offset);
        let existing = self.0.popup.borrow().clone();
        if let Some(popup) = &existing {
            if self.0.popup_position.borrow().as_ref() != Some(&position) {
                popup.destroy();
                self.0.popup.replace(None);
            }
        }
        let existing = self.0.popup.borrow().clone();
        match existing {
            Some(popup) => popup.activate(),
            None => {
                let Some(backend) = self.backend() else { return };
                let weak = self.weak();
                let popup = VolumePopup::new(&self.0.app, backend, move || Self::with(&weak, |a| a.show_settings()), edge, offset.min(i32::MAX as i64) as i32);
                *self.0.popup_position.borrow_mut() = Some(position);
                let weak = self.weak();
                popup.window.connect_unrealize(move |window| Self::with(&weak, |a| {
                    let same = a.0.popup.borrow().as_ref().is_some_and(|p| &p.window == window);
                    if same {
                        a.0.popup.replace(None);
                    }
                }));
                *self.0.popup.borrow_mut() = Some(popup.clone());
                popup.show();
            }
        }
    }

    pub fn show_settings(&self) {
        let popup = self.0.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.dismiss();
        }
        if self.0.settings.borrow().is_none() {
            let Some(backend) = self.backend() else { return };
            let settings = AudioSettingsWindow::new(&self.0.app, backend);
            let weak = self.weak();
            settings.window.connect_unrealize(move |window| Self::with(&weak, |a| {
                let same = a.0.settings.borrow().as_ref().is_some_and(|s| &s.window == window);
                if same {
                    a.0.settings.replace(None);
                }
            }));
            *self.0.settings.borrow_mut() = Some(settings);
        }
        let settings = self.0.settings.borrow().clone().unwrap();
        settings.window.set_visible(true);
        settings.refresh();
        settings.window.unminimize();
        settings.window.present();
        let (weak, window) = (self.weak(), settings.window.clone());
        glib::idle_add_local_once(move || {
            let still = weak.clone();
            suite_gtk::utils::raise_window(APPLICATION_ID, window.upcast_ref(), move |window| {
                still.upgrade().is_some_and(|inner| inner.settings.borrow().as_ref().is_some_and(|s| s.window.upcast_ref::<gtk::Window>() == window))
            });
        });
    }

    fn changed(&self) {
        if self.0.stopping.get() {
            return;
        }
        let keys = self.0.volume_keys.borrow().clone();
        if let Some(keys) = keys {
            keys.changed();
        }
        let settings = self.0.settings.borrow().clone();
        if let Some(settings) = settings {
            settings.refresh();
        }
        let popup = self.0.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.refresh();
        }
    }

    fn error(&self, message: &str) {
        let keys = self.0.volume_keys.borrow().clone();
        if let Some(keys) = keys {
            keys.error(message);
        }
        let settings = self.0.settings.borrow().clone();
        if let Some(settings) = settings {
            crate::ui::common::View::set_error(&*settings, message);
        }
        // Backend errors live in backend.error; refreshing reads that state so
        // a recovered connection also clears the popup's status message.
        let popup = self.0.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.refresh();
        }
    }

    fn shutdown(&self) {
        self.0.stopping.set(true);
        let keys = self.0.volume_keys.borrow().clone();
        if let Some(keys) = keys {
            keys.close();
        }
        let osd = self.0.osd.borrow().clone();
        if let Some(osd) = osd {
            osd.close();
        }
        let guard = self.0.key_held.borrow_mut().take();
        drop(guard);
        if let Some(backend) = self.backend() {
            backend.close();
        }
        let popup = self.0.popup.borrow().clone();
        if let Some(popup) = popup {
            popup.destroy();
        }
        let guard = self.0.held.borrow_mut().take();
        drop(guard);
    }

    pub fn run(&self, argv: &[String]) -> i32 {
        i32::from(self.0.app.run_with_args(argv).get())
    }
}

/// `audio_manager.main()`.
pub fn main(argv: Vec<String>) -> i32 {
    suite_core::i18n::initialize();
    let prog = argv.first().and_then(|a| std::path::Path::new(a).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "audio_manager.py".into());
    // Validate local help/options before starting an audio connection.
    if let Err(exit) = app_arguments(argv.get(1..).unwrap_or_default(), &prog) {
        print!("{}", exit.stdout);
        eprint!("{}", exit.stderr);
        return exit.code;
    }
    AudioApplication::new(APPLICATION_ID, None).run(&argv)
}
