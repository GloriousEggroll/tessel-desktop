//! AuthenticationWindow (authentication_manager.py:39-243): one request's
//! identity choice, PAM conversation, retry and cancellation.

use crate::agent::{Prompt, Request};
use crate::identity::{current_uid, is_password_prompt};
use crate::secret::Secret;
use crate::session::{EventHandler, Session, SessionEvent, SessionFactory};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use tessel_core::tr;
use tessel_gtk::utils::{add_css, headerbar, image, key_controller, PRIORITY_APPLICATION};

const CSS: &str = "\n                .authentication-window .authentication-lock { border-radius: 8px; }\n            ";

thread_local! {
    static INSTALLED: RefCell<Vec<(gdk::Display, &'static str)>> = const { RefCell::new(Vec::new()) };
}

/// Python: `if not getattr(display, '_suite_<key>_css', None): display._suite_<key>_css = add_css(css)`.
pub(crate) fn install_css(display: &gdk::Display, key: &'static str, css: &str) {
    let fresh = INSTALLED.with(|installed| {
        let mut installed = installed.borrow_mut();
        if installed.iter().any(|(d, k)| d == display && *k == key) {
            return false;
        }
        installed.push((display.clone(), key));
        true
    });
    if fresh {
        add_css(css, Some(display), PRIORITY_APPLICATION);
    }
}

pub struct AuthenticationWindow {
    pub window: gtk::Window,
    pub identity: gtk::DropDown,
    pub prompt: gtk::Label,
    pub entry: gtk::Entry,
    pub status: gtk::Label,
    pub cancel_button: gtk::Button,
    pub authenticate: gtk::Button,
    pub details: gtk::Expander,
    request: Rc<Request>,
    finished: RefCell<Option<Box<dyn FnOnce(bool)>>>,
    factory: SessionFactory,
    session: RefCell<Option<Rc<dyn Session>>>,
    /// Identifies the current session (Python: `session is self.session`).
    generation: Cell<u64>,
    done: Cell<bool>,
    waiting: Cell<bool>,
    attempts: Cell<u32>,
    session_completed: Cell<bool>,
    me: RefCell<Weak<AuthenticationWindow>>,
}

impl AuthenticationWindow {
    pub fn new(request: Rc<Request>, finished: Box<dyn FnOnce(bool)>, factory: SessionFactory) -> Rc<Self> {
        let title = tr!("Authentication Required");
        let window = gtk::Window::builder().title(&title).modal(true).resizable(false).build();
        window.add_css_class("authentication-window");
        window.set_titlebar(Some(&headerbar(&title)));
        window.set_default_size(460, -1);
        tessel_gtk::theme::attach(&window, None);
        install_css(&WidgetExt::display(&window), "authentication", CSS);
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(16)
            .margin_top(22).margin_bottom(20).margin_start(22).margin_end(22).build();
        window.set_child(Some(&body));
        let heading = gtk::Box::builder().spacing(14).build();
        let tile = gtk::Box::builder().halign(gtk::Align::Start).valign(gtk::Align::Start).hexpand(false).build();
        tile.add_css_class("settings-tile");
        tile.add_css_class("authentication-lock");
        tile.set_size_request(52, 52);
        // This padlock is provided by both Papirus and GTK's fallback theme.
        // Reserve its space so a long authentication message cannot squeeze it.
        let lock = image("changes-prevent-symbolic", 32);
        lock.set_halign(gtk::Align::Center);
        lock.set_valign(gtk::Align::Center);
        lock.set_hexpand(true);
        lock.set_vexpand(true);
        tile.append(&lock);
        heading.append(&tile);
        let text = gtk::Label::builder().label(&request.message).xalign(0.0).wrap(true).max_width_chars(44)
            .hexpand(true).build();
        heading.append(&text);
        body.append(&heading);

        let names: Vec<String> = request.identities.iter().map(|identity| identity.label()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let identity = gtk::DropDown::from_strings(&refs);
        let authenticate_as = tr!("Authenticate as");
        identity.set_tooltip_text(Some(&authenticate_as));
        identity.update_property(&[gtk::accessible::Property::Label(&authenticate_as)]);
        let me = current_uid();
        let selected = request.identities.iter().position(|identity| identity.uid == Some(me)).unwrap_or(0);
        identity.set_selected(selected as u32);
        if names.len() > 1 {
            body.append(&identity);
        } else {
            let first = names.first().cloned().unwrap_or_default();
            body.append(&gtk::Label::builder().label(format!("{authenticate_as} {first}")).xalign(0.0).build());
        }
        // PAM's own prompt is only shown when it asks for something other than
        // the password, which the entry's placeholder already names.
        let password = tr!("Password");
        let prompt = gtk::Label::builder().label(&password).xalign(0.0).wrap(true).visible(false).build();
        body.append(&prompt);
        // A GtkPasswordEntryBuffer keeps the typed text in non-swappable
        // memory that GTK zeroes when it is deleted. Gtk.Entry still supports
        // PAM's echo-on prompts (visibility only changes how it is drawn).
        let entry = gtk::Entry::with_buffer(&gtk::PasswordEntryBuffer::new());
        entry.set_visibility(false);
        entry.set_input_purpose(gtk::InputPurpose::Password);
        entry.set_placeholder_text(Some(&password));
        entry.update_property(&[gtk::accessible::Property::Label(&password)]);
        entry.set_sensitive(false);
        body.append(&entry);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(48).visible(false).build();
        status.add_css_class("subtitle");
        // No empty gap under the entry while there is nothing to report.
        status.connect_label_notify(|label| label.set_visible(!label.text().is_empty()));
        body.append(&status);
        let details = gtk::Expander::new(Some(&tr!("Details")));
        details.set_child(Some(&gtk::Label::builder().label(&request.action_id).xalign(0.0).wrap(true)
            .selectable(true).max_width_chars(48).build()));
        body.append(&details);
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let cancel_button = gtk::Button::with_label(&tr!("Cancel"));
        let authenticate = gtk::Button::builder().label(tr!("Authenticate")).sensitive(false).build();
        authenticate.add_css_class("suggested-action");
        buttons.append(&cancel_button);
        buttons.append(&authenticate);
        body.append(&buttons);

        let this = Rc::new(Self {
            window, identity, prompt, entry, status, cancel_button, authenticate, details, request,
            finished: RefCell::new(Some(finished)), factory, session: RefCell::new(None), generation: Cell::new(0),
            done: Cell::new(false), waiting: Cell::new(false), attempts: Cell::new(0),
            session_completed: Cell::new(false), me: RefCell::new(Weak::new()),
        });
        this.me.replace(Rc::downgrade(&this));
        let weak = Rc::downgrade(&this);
        this.entry.connect_activate(move |_| if let Some(this) = weak.upgrade() { this.submit() });
        let weak = Rc::downgrade(&this);
        this.cancel_button.connect_clicked(move |_| if let Some(this) = weak.upgrade() { this.finish(true); });
        let weak = Rc::downgrade(&this);
        this.authenticate.connect_clicked(move |_| if let Some(this) = weak.upgrade() { this.submit() });
        let weak = Rc::downgrade(&this);
        this.identity.connect_selected_notify(move |_| if let Some(this) = weak.upgrade() { this.identity_changed() });
        let weak = Rc::downgrade(&this);
        this.window.connect_close_request(move |_| {
            if let Some(this) = weak.upgrade() {
                this.finish(true);
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(&this);
        key_controller(&this.window, false, move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(this) = weak.upgrade() {
                    this.finish(true);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        this
    }

    pub fn is_done(&self) -> bool {
        self.done.get()
    }

    pub fn attempts(&self) -> u32 {
        self.attempts.get()
    }

    pub fn has_session(&self) -> bool {
        self.session.borrow().is_some()
    }

    pub fn request(&self) -> &Rc<Request> {
        &self.request
    }

    pub fn start(&self) {
        if self.done.get() {
            return;
        }
        self.drop_session();
        self.attempts.set(self.attempts.get() + 1);
        self.waiting.set(false);
        self.entry.set_text("");
        self.entry.set_sensitive(false);
        self.authenticate.set_sensitive(false);
        self.authenticate.set_label(&tr!("Authenticate"));
        let index = self.identity.selected() as usize;
        let Some(identity) = self.request.identities.get(index).or_else(|| self.request.identities.first()).cloned() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = self.me.borrow().clone();
        let handler: EventHandler = Rc::new(move |event| {
            if let Some(this) = weak.upgrade() {
                this.event(generation, event);
            }
        });
        let session = {
            let cookie = self.request.cookie.borrow();
            (self.factory)(&identity, &cookie, handler)
        };
        let session = match session {
            Ok(session) => session,
            Err(_) => {
                // Do not log request data or PAM conversation details.
                eprintln!("Could not display authentication request");
                self.finish(true);
                return;
            }
        };
        self.session_completed.set(false);
        self.session.replace(Some(session.clone()));
        session.initiate();
    }

    fn drop_session(&self) {
        let session = self.session.take();
        if let Some(session) = session {
            session.disconnect();
            if !self.session_completed.get() {
                session.cancel();
            }
        }
    }

    fn identity_changed(&self) {
        if !self.done.get() {
            self.attempts.set(0);
            self.status.set_text("");
            self.start();
        }
    }

    fn event(&self, generation: u64, event: SessionEvent) {
        let current = generation == self.generation.get() && self.session.borrow().is_some();
        match event {
            SessionEvent::Request { prompt, echo_on } => self.on_request(current, &prompt, echo_on),
            SessionEvent::Info(message) => self.info(&message),
            SessionEvent::Error(message) => self.error(&message),
            SessionEvent::Completed(authorized) => self.on_completed(current, authorized),
        }
    }

    fn on_request(&self, current: bool, prompt: &str, echo_on: bool) {
        if self.done.get() || !current {
            return;
        }
        self.waiting.set(true);
        let password = is_password_prompt(prompt);
        self.prompt.set_text(prompt);
        self.prompt.set_visible(!password && !prompt.trim().is_empty());
        let placeholder = if password { tr!("Password") } else { String::new() };
        self.entry.set_placeholder_text(Some(&placeholder));
        let label = if password { tr!("Password") } else { prompt.trim().to_owned() };
        self.entry.update_property(&[gtk::accessible::Property::Label(&label)]);
        self.entry.set_visibility(echo_on);
        self.entry.set_input_purpose(if echo_on { gtk::InputPurpose::FreeForm } else { gtk::InputPurpose::Password });
        self.entry.set_text("");
        self.entry.set_sensitive(true);
        self.authenticate.set_sensitive(true);
        self.entry.grab_focus();
        self.window.present();
    }

    fn info(&self, message: &str) {
        self.status.set_text(message);
        self.status.remove_css_class("error");
    }

    fn error(&self, message: &str) {
        self.status.set_text(message);
        self.status.add_css_class("error");
    }

    fn on_completed(&self, current: bool, authorized: bool) {
        if self.done.get() || !current {
            return;
        }
        self.entry.set_text("");
        self.session_completed.set(true);
        self.waiting.set(false);
        if authorized || self.attempts.get() >= 3 {
            self.finish(false);
        } else {
            self.drop_session();
            self.entry.set_sensitive(false);
            self.error(&tr!("Authentication failed. Please try again."));
            self.authenticate.set_label(&tr!("Try Again"));
            self.authenticate.set_sensitive(true);
        }
    }

    pub fn submit(&self) {
        if self.done.get() {
            return;
        }
        let session = self.session.borrow().clone();
        match session {
            None => self.start(),
            Some(session) if self.waiting.get() => {
                // One zeroed-on-drop copy straight out of the secure buffer.
                let response = unsafe {
                    let buffer = self.entry.buffer();
                    Secret::from_ptr(gtk::ffi::gtk_entry_buffer_get_text(buffer.as_ptr()))
                };
                self.entry.set_text("");
                self.entry.set_sensitive(false);
                self.authenticate.set_sensitive(false);
                self.waiting.set(false);
                // The response goes directly to libpolkit's PAM helper; never log it.
                session.response(response.as_c_str());
                drop(response);
            }
            Some(_) => {}
        }
    }

    pub fn finish(&self, cancelled: bool) -> bool {
        if self.done.get() {
            return true;
        }
        self.done.set(true);
        self.entry.set_text("");
        self.drop_session();
        self.request.cookie.borrow_mut().clear();
        self.window.destroy();
        if let Some(finished) = self.finished.take() {
            finished(cancelled);
        }
        true
    }

    /// The default window factory for [`crate::agent::AuthenticationAgent`].
    pub fn factory(sessions: SessionFactory) -> crate::agent::WindowFactory {
        Rc::new(move |request, finished| {
            let window = AuthenticationWindow::new(request, finished, sessions.clone());
            Ok(window as Rc<dyn Prompt>)
        })
    }
}

impl Prompt for AuthenticationWindow {
    fn set_application(&self, application: Option<&gtk::Application>) {
        self.window.set_application(application);
    }

    fn present(&self) {
        self.window.present();
    }

    fn start(&self) {
        AuthenticationWindow::start(self);
    }

    fn finish(&self, cancelled: bool) {
        AuthenticationWindow::finish(self, cancelled);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
