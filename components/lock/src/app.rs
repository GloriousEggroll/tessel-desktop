//! lock_screen.py LockApplication: the GTK application, session-lock
//! lifecycle, authentication state machine, fingerprint transaction, system
//! actions and the 1 s tick.
//!
//! SECURITY: `SessionLock::unlock` is called in exactly two places in this
//! file: `completed()` (password PAM success) and `fingerprint_completed()`
//! (fingerprint PAM success). Shutdown, close requests, Escape, signals,
//! errors and Drop never unlock (contracts/tessel-lock.md §9).
use crate::auth::{AuthFactory, Authentication, Callbacks, Transaction};
use crate::fingerprint::FingerprintDetector;
use crate::palette::PaletteState;
use crate::session_lock::{self, SessionLock};
use crate::status::LockStatus;
use crate::window::LockWindow;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use tessel_core::clock::{preferences_path, ClockPreferencesStore};
use tessel_core::tr;
use tessel_gtk::gtk;
use tessel_locking::config::{home_assistant_installed, ConfigStore, LockConfig};
use tessel_locking::media::{LockMedia, MediaIcons};
use tessel_locking::voice_state::{monotonic, VoiceLog};
use tessel_locking::weather::LockWeather;
use zeroize::Zeroizing;

/// Optional policies proposed by tessel-login/install_login.py.
pub const PASSWORD_ONLY_POLICY: &str = "/etc/pam.d/tessel-lock-password";
pub const FINGERPRINT_POLICY: &str = "/etc/pam.d/tessel-lock-fingerprint";
pub const FINGERPRINT_ATTEMPTS: u32 = 5;
/// pam_fprintd gives up after its own timeout (30 s by default); a transaction
/// that ends sooner without success was a real attempt (no match).
pub const FINGERPRINT_TIMEOUT_GUESS: f64 = 25.0;
pub const LOCK_ID: &str = "org.tessel.LockScreen";
pub const PREVIEW_ID: &str = "org.tessel.LockPreview";
/// Exit status when the compositor rejects the lock (another locker holds it).
pub const LOCK_FAILED: i32 = 75;

pub const STYLE: &str = "
window.tessel-lock { background: #161820; border-radius: 0; }
.tessel-lock .lock-name { font-size: 13.5pt; font-weight: 600; letter-spacing: 5px; color: white; }
.tessel-lock .lock-panel { background: transparent; border: none; box-shadow: none; }
.tessel-lock entry { padding: 10px 14px; border-radius: 9999px; min-height: 22px; }
.tessel-lock .lock-caption { font-size: 10pt; }
.tessel-lock .lock-information { font-size: 15pt; }
.tessel-lock .lock-location { font-size: 18pt; font-weight: 600; }
.tessel-lock .lock-current-weather { font-size: 24pt; }
.tessel-lock .lock-forecast-heading { font-size: 15pt; }
.tessel-lock .lock-forecast-day { font-size: 15pt; }
.tessel-lock .lock-forecast-high { font-size: 18pt; font-weight: 600; }
.tessel-lock .lock-information .lock-caption { font-size: 13.5pt; }
.tessel-lock .lock-media-title { font-size: 19.5pt; font-weight: 600; }
.tessel-lock .lock-information label { text-shadow: 0 2px 6px #000; }
.tessel-lock .lock-group-heading { font-size: 13.5pt; font-weight: 500; letter-spacing: 1px; }
.tessel-lock .lock-temperature { font-size: 36pt; font-weight: 600; }
.tessel-lock .lock-condition { font-size: 16.5pt; }
.tessel-lock .lock-status { font-size: 13.5pt; }
";

/// Construction options (Python keyword arguments and test patches).
pub struct Options {
    pub preview: bool,
    pub auth_factory: AuthFactory,
    pub store: ConfigStore,
    /// `home_assistant_installed()` unless overridden (tests patch it).
    pub has_assistant: Option<bool>,
    pub fingerprint_policy: PathBuf,
    pub password_only_policy: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Self { preview: false, auth_factory: Authentication::factory(), store: ConfigStore::default(), has_assistant: None,
               fingerprint_policy: FINGERPRINT_POLICY.into(), password_only_policy: PASSWORD_ONLY_POLICY.into() }
    }
}

/// The current PAM transaction and its identity (stale completions are ignored).
type Current = RefCell<Option<(u64, Rc<dyn Transaction>)>>;

pub struct App {
    pub application: gtk::Application,
    pub preview: bool,
    pub auth_factory: RefCell<AuthFactory>,
    pub store: ConfigStore,
    pub config: RefCell<LockConfig>,
    pub palette: PaletteState,
    pub clock_store: RefCell<ClockPreferencesStore>,
    pub windows: RefCell<Vec<Rc<LockWindow>>>,
    pub has_assistant: Cell<bool>,
    voice_log: RefCell<Option<VoiceLog>>,
    pub auth: Current,
    pub auth_window: RefCell<Option<Weak<LockWindow>>>,
    initial_secret: RefCell<Zeroizing<Vec<u8>>>,
    lock: RefCell<Option<SessionLock>>,
    pub locked: Cell<bool>,
    pub authorized: Cell<bool>,
    pub can_sleep: Cell<bool>,
    pub greeter: RefCell<Option<String>>,
    tick: RefCell<Option<glib::SourceId>>,
    voice_tick: RefCell<Option<glib::SourceId>>,
    pub retry: RefCell<Option<glib::SourceId>>,
    pub result: Cell<i32>,
    pub login_visible: Cell<bool>,
    pub input_window: RefCell<Option<Weak<LockWindow>>>,
    pub forwarding_input: Cell<bool>,
    pub last_activity: Cell<f64>,
    pub weather: LockWeather,
    pub media: LockMedia,
    pub media_icons: MediaIcons,
    pub status: LockStatus,
    pub fingerprint: RefCell<Option<FingerprintDetector>>,
    pub fingerprint_auth: Current,
    pub fingerprint_prompt: RefCell<String>,
    pub fingerprint_hint: RefCell<String>,
    pub fingerprint_failures: Cell<u32>,
    pub fingerprint_started: Cell<f64>,
    pub fingerprint_restart: RefCell<Option<glib::SourceId>>,
    pub fingerprint_policy: RefCell<PathBuf>,
    pub password_only_policy: RefCell<PathBuf>,
    system_bus: RefCell<Option<gio::DBusConnection>>,
    capture_started: Cell<f64>,
    pub capture_requests: Cell<u32>,
    next_id: Cell<u64>,
    style: RefCell<Option<gtk::CssProvider>>,
    shut_down: Cell<bool>,
    activated: Cell<bool>,
}

fn same(a: &Weak<LockWindow>, b: &Rc<LockWindow>) -> bool { std::ptr::eq(a.as_ptr(), Rc::as_ptr(b)) }

impl App {
    pub fn new(options: Options) -> Rc<Self> {
        let id = if options.preview { PREVIEW_ID } else { LOCK_ID };
        let application = gtk::Application::new(Some(id), gio::ApplicationFlags::NON_UNIQUE);
        // Broken appearance must never prevent locking.
        let config = options.store.load().unwrap_or_default();
        let has_assistant = options.has_assistant.unwrap_or_else(home_assistant_installed);
        let voice_log = (has_assistant && config.show_voice).then(|| VoiceLog::new(&config.voice_log));
        let app = Rc::new_cyclic(|weak: &Weak<App>| {
            let changed = {
                let weak = weak.clone();
                move || if let Some(app) = weak.upgrade() { app.information_changed() }
            };
            App {
                application: application.clone(), preview: options.preview, auth_factory: RefCell::new(options.auth_factory),
                store: options.store, config: RefCell::new(config), palette: PaletteState::new(None),
                clock_store: RefCell::new(ClockPreferencesStore::new(preferences_path())), windows: RefCell::new(Vec::new()),
                has_assistant: Cell::new(has_assistant), voice_log: RefCell::new(voice_log), auth: RefCell::new(None),
                auth_window: RefCell::new(None), initial_secret: RefCell::new(Zeroizing::new(Vec::new())),
                lock: RefCell::new(None), locked: false.into(), authorized: false.into(), can_sleep: false.into(),
                greeter: RefCell::new(None), tick: RefCell::new(None), voice_tick: RefCell::new(None), retry: RefCell::new(None),
                result: 0.into(), login_visible: false.into(), input_window: RefCell::new(None), forwarding_input: false.into(),
                last_activity: monotonic().into(),
                weather: LockWeather::new(changed.clone(), None, None),
                media: LockMedia::new(changed.clone(), options.preview),
                media_icons: MediaIcons::default(),
                status: LockStatus::new(changed),
                fingerprint: RefCell::new(None), fingerprint_auth: RefCell::new(None),
                fingerprint_prompt: RefCell::new(String::new()), fingerprint_hint: RefCell::new(String::new()),
                fingerprint_failures: 0.into(), fingerprint_started: 0.0.into(), fingerprint_restart: RefCell::new(None),
                fingerprint_policy: RefCell::new(options.fingerprint_policy),
                password_only_policy: RefCell::new(options.password_only_policy),
                system_bus: RefCell::new(None), capture_started: monotonic().into(), capture_requests: 0.into(),
                next_id: 1.into(), style: RefCell::new(None), shut_down: false.into(), activated: false.into(),
            }
        });
        let weak = Rc::downgrade(&app);
        application.connect_activate(move |_| if let Some(app) = weak.upgrade() { app.activate() });
        let weak = Rc::downgrade(&app);
        application.connect_shutdown(move |_| if let Some(app) = weak.upgrade() { app.shutdown() });
        app
    }

    fn next_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    /// Read-only view of the compositor lock (tests and status only).
    pub fn is_locked(&self) -> bool { self.lock.borrow().as_ref().is_some_and(SessionLock::is_locked) }
    pub fn has_lock(&self) -> bool { self.lock.borrow().is_some() }

    pub fn quit(&self) { self.application.quit() }

    pub fn window_list(&self) -> Vec<Rc<LockWindow>> { self.windows.borrow().clone() }

    pub fn activate(self: &Rc<Self>) {
        if self.activated.replace(true) {
            return;
        }
        // `hold()` for the process lifetime (the guard is leaked on purpose).
        std::mem::forget(self.application.hold());
        *self.style.borrow_mut() = Some(tessel_gtk::utils::add_css(&(STYLE.to_string() + &tessel_locking::visuals::family_css("tessel-lock")),
                                                                  None, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION));
        let config = self.config.borrow().clone();
        self.weather.poll(config.show_weather || config.show_forecast);
        self.status.start(config.show_battery, config.show_notifications);
        if self.preview {
            let Some(monitor) = gdk::Display::default().and_then(|d| d.monitors().item(0)).and_then(|m| m.downcast::<gdk::Monitor>().ok())
            else {
                self.result.set(1);
                self.quit();
                return;
            };
            let window = LockWindow::new(self, &monitor);
            self.windows.borrow_mut().push(window.clone());
            window.window.set_default_size(1100, 760);
            window.window.set_resizable(true);
            window.window.present();
            self.media.start(false);
        } else {
            if !session_lock::is_supported() {
                self.result.set(1);
                self.quit();
                return;
            }
            let lock = SessionLock::new();
            let weak = Rc::downgrade(self);
            lock.connect_monitor(move |lock, monitor| if let Some(app) = weak.upgrade() { app.monitor_added(lock, &monitor) });
            let weak = Rc::downgrade(self);
            lock.connect_locked(move || if let Some(app) = weak.upgrade() { app.on_locked() });
            let weak = Rc::downgrade(self);
            lock.connect_failed(move || if let Some(app) = weak.upgrade() { app.on_failed() });
            let weak = Rc::downgrade(self);
            lock.connect_unlocked(move || if let Some(app) = weak.upgrade() { app.quit() });
            *self.lock.borrow_mut() = Some(lock.clone());
            self.capture_started.set(monotonic());
            lock.lock();
        }
        let weak = Rc::downgrade(self);
        *self.tick.borrow_mut() = Some(glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(app) => { app.tick_windows(); glib::ControlFlow::Continue }
            None => glib::ControlFlow::Break,
        }));
        if self.voice_log.borrow().is_some() {
            self.start_voice_tick();
        }
        if !self.preview {
            self.load_actions();
        }
    }

    fn start_voice_tick(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(std::time::Duration::from_millis(150), move || match weak.upgrade() {
            Some(app) => { app.poll_voice(); glib::ControlFlow::Continue }
            None => glib::ControlFlow::Break,
        });
        if let Some(old) = self.voice_tick.replace(Some(id)) {
            old.remove();
        }
    }

    fn monitor_added(self: &Rc<Self>, lock: &SessionLock, monitor: &gdk::Monitor) {
        let window = LockWindow::new(self, monitor);
        self.windows.borrow_mut().push(window.clone());
        lock.assign_window_to_monitor(&window.window, monitor);
        if self.login_visible.get() {
            window.entry.grab_focus();
        }
    }

    fn on_locked(self: &Rc<Self>) {
        self.locked.set(true);
        let pause = self.config.borrow().pause_media;
        self.media.start(pause);
        self.start_fingerprint();
        crate::notify::ready();
    }

    fn on_failed(&self) {
        self.result.set(LOCK_FAILED);
        self.quit();
    }

    pub fn capture_visible_lock_screen(&self) {
        self.capture_requests.set(self.capture_requests.get() + 1);
        let started = self.capture_started.get();
        gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |result| {
            let Ok(bus) = result else { return };
            let parameters = ("lock-shot", vec![started.to_variant()], std::collections::HashMap::<String, glib::Variant>::new())
                .to_variant();
            // Capture has not been enabled: locking is unaffected.
            bus.call(Some("org.tessel.Capture"), "/org/tessel/Capture", "org.gtk.Actions", "Activate", Some(&parameters),
                     None, gio::DBusCallFlags::NO_AUTO_START, 1500, None::<&gio::Cancellable>, |_| {});
        });
    }

    // ----- fingerprint -------------------------------------------------

    pub fn start_fingerprint(self: &Rc<Self>) {
        // Detection only: fprintd says whether a finger is enrolled.
        if self.preview || self.fingerprint.borrow().is_some() || !self.fingerprint_policy.borrow().is_file() {
            return; // Fallback: the tessel-lock policy's own pam_fprintd.
        }
        let weak = Rc::downgrade(self);
        let detector = FingerprintDetector::new(move |prompt| if let Some(app) = weak.upgrade() { app.fingerprint_found(prompt) },
                                                None, None);
        *self.fingerprint.borrow_mut() = Some(detector.clone());
        detector.start();
    }

    pub fn fingerprint_found(self: &Rc<Self>, prompt: &str) {
        *self.fingerprint_prompt.borrow_mut() = prompt.to_string();
        self.fingerprint_begin();
    }

    pub fn fingerprint_begin(self: &Rc<Self>) {
        self.fingerprint_restart.borrow_mut().take();
        if self.preview || !self.locked.get() || self.authorized.get() || self.fingerprint_auth.borrow().is_some()
            || self.fingerprint_failures.get() >= FINGERPRINT_ATTEMPTS {
            return;
        }
        let id = self.next_id();
        let (w1, w2, w3) = (Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self));
        let factory = self.auth_factory.borrow().clone();
        let auth = factory(Callbacks {
            prompt: Box::new(move |_, _| if let Some(app) = w1.upgrade() { app.fingerprint_prompted(id) }),
            message: Box::new(move |message| if let Some(app) = w2.upgrade() { app.fingerprint_message(id, message) }),
            complete: Box::new(move |success| if let Some(app) = w3.upgrade() { app.fingerprint_completed(id, success) }),
        });
        auth.set_fingerprint(true);
        *self.fingerprint_auth.borrow_mut() = Some((id, auth.clone()));
        self.fingerprint_started.set(monotonic());
        let hint = if self.fingerprint_failures.get() > 0 { self.fingerprint_hint.borrow().clone() }
                   else { self.fingerprint_prompt.borrow().clone() };
        self.fingerprint_state(&hint);
        auth.start();
    }

    fn is_current(current: &Current, id: u64) -> bool { current.borrow().as_ref().is_some_and(|(i, _)| *i == id) }

    fn fingerprint_prompted(self: &Rc<Self>, id: u64) {
        // The fingerprint policy never asks for typed input; fail closed.
        if Self::is_current(&self.fingerprint_auth, id) {
            self.fingerprint_cancel();
            self.fingerprint_state("");
        }
    }

    fn fingerprint_message(&self, id: u64, message: &str) {
        if Self::is_current(&self.fingerprint_auth, id) && !message.is_empty() {
            self.fingerprint_state(message);
        }
    }

    fn fingerprint_completed(self: &Rc<Self>, id: u64, success: bool) {
        if !Self::is_current(&self.fingerprint_auth, id) {
            return;
        }
        self.fingerprint_auth.borrow_mut().take();
        if success && self.locked.get() && !self.preview && !self.authorized.get() {
            // PAM (pam_fprintd, faillock preauth, account) succeeded: stop the password path.
            let auth = self.auth.borrow_mut().take();
            self.auth_window.borrow_mut().take();
            self.clear_initial();
            if let Some((_, auth)) = auth {
                auth.cancel();
            }
            for window in self.window_list() {
                window.entry.set_text("");
            }
            self.authorized.set(true);
            let lock = self.lock.borrow().clone();
            if let Some(lock) = lock {
                lock.unlock(); // SECURITY: verified fingerprint PAM success only.
            }
            return;
        }
        if self.authorized.get() || !self.locked.get() {
            return;
        }
        if monotonic() - self.fingerprint_started.get() < FINGERPRINT_TIMEOUT_GUESS {
            self.fingerprint_failures.set(self.fingerprint_failures.get() + 1);
        }
        if self.fingerprint_failures.get() >= FINGERPRINT_ATTEMPTS {
            self.fingerprint_state("Too many fingerprint attempts. Use your password.");
            return;
        }
        if self.fingerprint_failures.get() > 0 {
            self.fingerprint_state("Fingerprint not recognized. Try again.");
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
            if let Some(app) = weak.upgrade() {
                app.fingerprint_restart.borrow_mut().take();
                app.fingerprint_begin();
            }
        });
        *self.fingerprint_restart.borrow_mut() = Some(id);
    }

    pub fn fingerprint_cancel(&self) {
        if let Some(source) = self.fingerprint_restart.borrow_mut().take() {
            source.remove();
        }
        let auth = self.fingerprint_auth.borrow_mut().take();
        if let Some((_, auth)) = auth {
            auth.cancel();
        }
    }

    fn fingerprint_state(&self, message: &str) {
        *self.fingerprint_hint.borrow_mut() = message.to_string();
        self.information_changed();
    }

    pub fn information_changed(&self) {
        for window in self.window_list() {
            window.update_information();
        }
    }

    fn clear_initial(&self) {
        // Zeroizing<Vec<u8>>: replacing drops (and zeroes) the old buffer.
        let mut secret = self.initial_secret.borrow_mut();
        zeroize::Zeroize::zeroize(&mut **secret);
        *secret = Zeroizing::new(Vec::new());
    }

    // ----- password ----------------------------------------------------

    pub fn authenticate(self: &Rc<Self>, window: &Rc<LockWindow>, text: Zeroizing<String>) {
        if self.preview || !self.locked.get() || !self.login_visible.get() || self.retry.borrow().is_some() {
            return;
        }
        let current = self.auth.borrow().clone();
        if let Some((_, auth)) = current {
            let same_window = self.auth_window.borrow().as_ref().is_some_and(|w| same(w, window));
            if same_window && auth.waiting() {
                self.set_sensitive(false);
                auth.respond(&text);
            }
            return;
        }
        *self.auth_window.borrow_mut() = Some(Rc::downgrade(window));
        *self.initial_secret.borrow_mut() = Zeroizing::new(text.as_bytes().to_vec());
        drop(text);
        self.set_sensitive(false);
        window.status.set_text(&tr!("Checking…"));
        let id = self.next_id();
        let (w1, w2, w3) = (Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self));
        let factory = self.auth_factory.borrow().clone();
        let auth = factory(Callbacks {
            prompt: Box::new(move |message, echo| if let Some(app) = w1.upgrade() { app.prompt(id, message, echo) }),
            message: Box::new(move |message| if let Some(app) = w2.upgrade() { app.message(id, message) }),
            complete: Box::new(move |success| if let Some(app) = w3.upgrade() { app.completed(id, success) }),
        });
        // While the fingerprint transaction runs, a password must not wait on pam_fprintd.
        auth.set_password_only(self.fingerprint_auth.borrow().is_some() && self.password_only_policy.borrow().is_file());
        *self.auth.borrow_mut() = Some((id, auth.clone()));
        auth.start();
    }

    pub fn set_sensitive(&self, enabled: bool) {
        let enabled = enabled && !self.preview;
        for window in self.window_list() {
            window.entry.set_sensitive(enabled);
            window.unlock_button.set_sensitive(enabled);
        }
        if enabled {
            if let Some(window) = self.input_window() {
                if !window.closed.get() {
                    window.entry.grab_focus();
                }
            }
        }
    }

    pub fn input_window(&self) -> Option<Rc<LockWindow>> { self.input_window.borrow().as_ref().and_then(Weak::upgrade) }
    pub fn auth_window(&self) -> Option<Rc<LockWindow>> { self.auth_window.borrow().as_ref().and_then(Weak::upgrade) }
    pub fn current_auth(&self) -> Option<Rc<dyn Transaction>> { self.auth.borrow().as_ref().map(|(_, a)| a.clone()) }
    pub fn current_fingerprint(&self) -> Option<Rc<dyn Transaction>> {
        self.fingerprint_auth.borrow().as_ref().map(|(_, a)| a.clone())
    }

    fn prompt(&self, id: u64, message: &str, echo: bool) {
        if !Self::is_current(&self.auth, id) {
            return;
        }
        let auth = self.current_auth().expect("current auth");
        if !self.initial_secret.borrow().is_empty() && !echo {
            let response: Zeroizing<String> = Zeroizing::new(String::from_utf8_lossy(&self.initial_secret.borrow()).into_owned());
            self.clear_initial();
            auth.respond(&response);
            return;
        }
        self.clear_initial();
        let window = self.auth_window().filter(|w| !w.closed.get());
        let Some(window) = window else {
            self.cancel_authentication();
            return;
        };
        window.set_prompt(message);
        window.status.set_text("");
        window.entry.set_visibility(echo);
        window.entry.set_input_purpose(if echo { gtk::InputPurpose::FreeForm } else { gtk::InputPurpose::Password });
        window.entry.set_sensitive(true);
        window.unlock_button.set_sensitive(true);
        *self.input_window.borrow_mut() = Some(Rc::downgrade(&window));
        window.entry.grab_focus();
    }

    fn message(&self, id: u64, message: &str) {
        if Self::is_current(&self.auth, id) {
            if let Some(window) = self.auth_window().filter(|w| !w.closed.get()) {
                window.status.set_text(message);
            }
        }
    }

    fn completed(self: &Rc<Self>, id: u64, success: bool) {
        if !Self::is_current(&self.auth, id) {
            return;
        }
        self.auth.borrow_mut().take();
        self.auth_window.borrow_mut().take();
        self.clear_initial();
        for window in self.window_list() {
            window.entry.set_text("");
        }
        if success && self.locked.get() && !self.preview {
            self.fingerprint_cancel();
            self.authorized.set(true);
            let lock = self.lock.borrow().clone();
            if let Some(lock) = lock {
                lock.unlock(); // SECURITY: verified password PAM success only.
            }
        } else {
            for window in self.window_list() {
                window.status.set_text(&tr!("Authentication failed. Please try again."));
            }
            let weak = Rc::downgrade(self);
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(2000), move || {
                if let Some(app) = weak.upgrade() {
                    app.retry.borrow_mut().take();
                    app.ready();
                }
            });
            if let Some(old) = self.retry.replace(Some(source)) {
                old.remove();
            }
        }
    }

    /// `_ready` (also called by tests after removing the retry timer).
    pub fn ready(&self) {
        if let Some(source) = self.retry.borrow_mut().take() {
            source.remove();
        }
        self.set_sensitive(true);
    }

    pub fn cancel_authentication(&self) {
        let auth = self.auth.borrow_mut().take();
        self.auth_window.borrow_mut().take();
        self.clear_initial();
        if let Some((_, auth)) = auth {
            auth.cancel();
        }
        if self.retry.borrow().is_none() {
            self.set_sensitive(true);
        }
        for window in self.window_list() {
            window.entry.set_text("");
            window.show_password.set_active(false);
            window.entry.set_visibility(false);
            window.entry.set_input_purpose(gtk::InputPurpose::Password);
            window.set_prompt("");
            window.status.set_text(&if self.preview { tr!("Preview — your session is not locked.") } else { String::new() });
        }
    }

    // ----- tick / activity ---------------------------------------------

    pub fn tick_windows(self: &Rc<Self>) {
        if self.login_visible.get() && monotonic() - self.last_activity.get() >= 30.0 {
            self.cancel_authentication();
            self.login_visible.set(false);
            for window in self.window_list() {
                window.reveal_login(false);
            }
        }
        if let Ok(config) = self.store.load() {
            if config != *self.config.borrow() {
                *self.config.borrow_mut() = config.clone();
                if self.has_assistant.get() && config.show_voice && self.voice_log.borrow().is_none() {
                    *self.voice_log.borrow_mut() = Some(VoiceLog::new(&config.voice_log));
                    self.start_voice_tick();
                }
                for window in self.window_list() {
                    window.refresh();
                }
            }
        }
        let config = self.config.borrow().clone();
        self.weather.poll(config.show_weather || config.show_forecast);
        self.status.start(config.show_battery, config.show_notifications);
        for window in self.window_list() {
            window.tick();
        }
    }

    pub fn activity(&self, window: &Rc<LockWindow>) {
        self.last_activity.set(monotonic());
        if !self.login_visible.get() {
            self.login_visible.set(true);
            *self.input_window.borrow_mut() = Some(Rc::downgrade(window));
            for item in self.window_list() {
                item.reveal_login(true);
            }
            window.entry.grab_focus();
        }
    }

    fn poll_voice(&self) {
        let show = self.config.borrow().show_voice && !self.login_visible.get();
        let state = match self.voice_log.borrow_mut().as_mut() {
            Some(log) if show => log.poll(),
            _ => "idle",
        };
        for window in self.window_list() {
            window.voice.update(state);
        }
    }

    // ----- system actions ----------------------------------------------

    fn load_actions(self: &Rc<Self>) {
        let Ok(bus) = gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>) else { return };
        *self.system_bus.borrow_mut() = Some(bus.clone());
        let weak = Rc::downgrade(self);
        bus.call(Some("org.freedesktop.login1"), "/org/freedesktop/login1", "org.freedesktop.login1.Manager", "CanSuspend", None,
                 None, gio::DBusCallFlags::NO_AUTO_START, 1500, None::<&gio::Cancellable>, move |result| {
            if let (Some(app), Ok(value)) = (weak.upgrade(), result) {
                if value.try_child_value(0).and_then(|v| v.str().map(str::to_owned)).as_deref() == Some("yes") {
                    app.can_sleep.set(true);
                }
            }
        });
        let weak = Rc::downgrade(self);
        bus.call(Some("org.freedesktop.DisplayManager"), "/org/freedesktop/DisplayManager", "org.freedesktop.DBus.Properties",
                 "Get", Some(&("org.freedesktop.DisplayManager", "Seats").to_variant()), None, gio::DBusCallFlags::NO_AUTO_START,
                 1500, None::<&gio::Cancellable>, move |result| {
            let (Some(app), Ok(value)) = (weak.upgrade(), result) else { return };
            let seats = value.try_child_value(0).map(|v| v.as_variant().unwrap_or(v));
            let paths: Vec<String> = seats.map(|s| s.iter().filter_map(|p| p.str().map(str::to_owned)).collect()).unwrap_or_default();
            // Only offer the current local seat; never send another seat away.
            let expected = std::env::var("XDG_SEAT_PATH").ok().filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/org/freedesktop/DisplayManager/Seat0".into());
            if paths.contains(&expected) {
                *app.greeter.borrow_mut() = Some(expected);
            }
        });
    }

    fn action_result(weak: Weak<App>) -> impl FnOnce(Result<glib::Variant, glib::Error>) {
        move |result| {
            if result.is_err() {
                if let Some(app) = weak.upgrade() {
                    for window in app.window_list() {
                        window.status.set_text(&tr!("That action is unavailable. The session remains locked."));
                    }
                }
            }
        }
    }

    pub fn sleep(self: &Rc<Self>) {
        if self.can_sleep.get() && self.locked.get() {
            if let Some(bus) = self.system_bus.borrow().clone() {
                bus.call(Some("org.freedesktop.login1"), "/org/freedesktop/login1", "org.freedesktop.login1.Manager", "Suspend",
                         Some(&(false,).to_variant()), None, gio::DBusCallFlags::NO_AUTO_START, 10000, None::<&gio::Cancellable>,
                         Self::action_result(Rc::downgrade(self)));
            }
        }
    }

    pub fn switch_user(self: &Rc<Self>) {
        let greeter = self.greeter.borrow().clone();
        if let (Some(greeter), true) = (greeter, self.locked.get()) {
            self.cancel_authentication();
            if let Some(bus) = self.system_bus.borrow().clone() {
                bus.call(Some("org.freedesktop.DisplayManager"), &greeter, "org.freedesktop.DisplayManager.Seat", "SwitchToGreeter",
                         None, None, gio::DBusCallFlags::NO_AUTO_START, 5000, None::<&gio::Cancellable>,
                         Self::action_result(Rc::downgrade(self)));
            }
        }
    }

    /// `do_shutdown`. Never unlocks: crashes, close requests and signals must
    /// leave the compositor locked. Only PAM success may unlock.
    pub fn shutdown(&self) {
        if self.shut_down.replace(true) {
            return;
        }
        self.cancel_authentication();
        self.fingerprint_cancel();
        if let Some(detector) = self.fingerprint.borrow().as_ref() {
            detector.close();
        }
        self.status.close();
        self.media.close();
        self.media_icons.close();
        self.weather.close();
        for slot in [&self.tick, &self.voice_tick, &self.retry] {
            if let Some(source) = slot.borrow_mut().take() {
                source.remove();
            }
        }
        for window in self.window_list() {
            window.cleanup();
        }
        self.palette.close();
    }
}
