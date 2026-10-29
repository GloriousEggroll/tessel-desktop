//! BlueZ pairing agent (bluetooth_agent.py): PIN, passkey and confirmation
//! prompts for the session. Registered as the default agent, it answers
//! pairing started by our Bluetooth app and by remote devices alike. BlueZ
//! calls one method at a time; each prompt keeps its D-Bus invocation until the
//! user answers, cancels, or BlueZ cancels.

use gio::prelude::*;
use glib::Variant;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use suite_core::tr;
use suite_gtk::utils::{headerbar, image, key_controller};

pub const BLUEZ: &str = "org.bluez";
pub const AGENT_PATH: &str = "/org/wayshell/BluetoothAgent";
pub const CAPABILITY: &str = "KeyboardDisplay";
pub const REJECTED: &str = "org.bluez.Error.Rejected";
/// Defined for parity; unused like in Python.
pub const CANCELED: &str = "org.bluez.Error.Canceled";
pub const XML: &str = r#"<node><interface name="org.bluez.Agent1">
<method name="Release"/>
<method name="RequestPinCode"><arg type="o" direction="in"/><arg type="s" direction="out"/></method>
<method name="DisplayPinCode"><arg type="o" direction="in"/><arg type="s" direction="in"/></method>
<method name="RequestPasskey"><arg type="o" direction="in"/><arg type="u" direction="out"/></method>
<method name="DisplayPasskey"><arg type="o" direction="in"/><arg type="u" direction="in"/><arg type="q" direction="in"/></method>
<method name="RequestConfirmation"><arg type="o" direction="in"/><arg type="u" direction="in"/></method>
<method name="RequestAuthorization"><arg type="o" direction="in"/></method>
<method name="AuthorizeService"><arg type="o" direction="in"/><arg type="s" direction="in"/></method>
<method name="Cancel"/>
</interface></node>"#;

pub fn passkey_text(passkey: u32) -> String {
    format!("{passkey:06}")
}

/// Legacy PIN codes are 1–16 characters.
pub fn valid_pin(text: &str) -> bool {
    (1..=16).contains(&text.chars().count())
}

/// Python `text.isdigit() and len(text) <= 6` (ASCII digits: the reply is `int(text)`).
pub fn valid_passkey(text: &str) -> bool {
    !text.is_empty() && text.chars().count() <= 6 && text.chars().all(|c| c.is_ascii_digit())
}

/// What a prompt's `answer` receives: `None` = rejected.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Accepted,
    Text(String),
}

pub type Validator = fn(&str) -> bool;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptOptions {
    pub code: Option<String>,
    /// Entry placeholder (and accessible label); `None` = no entry.
    pub entry: Option<String>,
    pub digits: bool,
    pub accept: Option<String>,
    pub reject: Option<String>,
}

/// One pairing prompt; `answer` is called once.
pub trait PairingPrompt {
    fn set_application(&self, application: Option<&gtk::Application>);
    fn present(&self);
    fn focus_entry(&self);
    fn set_validator(&self, validator: Validator);
    fn set_message(&self, text: &str);
    fn is_done(&self) -> bool;
    fn finish(&self, value: Option<Answer>);
    fn displayed_device(&self) -> Option<String>;
    fn set_displayed_device(&self, device: &str);
    fn as_any(&self) -> &dyn std::any::Any;
}

pub type PairingFactory = Rc<dyn Fn(&str, &str, Box<dyn FnOnce(Option<Answer>)>, PromptOptions) -> Rc<dyn PairingPrompt>>;

/// A method call's reply channel (`Gio.DBusMethodInvocation`).
pub trait Invocation {
    fn return_value(self: Box<Self>, value: Option<Variant>);
    fn return_dbus_error(self: Box<Self>, name: &str, message: &str);
}

impl Invocation for gio::DBusMethodInvocation {
    fn return_value(self: Box<Self>, value: Option<Variant>) {
        gio::DBusMethodInvocation::return_value(*self, value.as_ref());
    }

    fn return_dbus_error(self: Box<Self>, name: &str, message: &str) {
        gio::DBusMethodInvocation::return_dbus_error(*self, name, message);
    }
}

/// The bus operations the agent performs (Python's `self.bus`).
pub trait AgentBus {
    /// `Properties.Get(org.bluez.Device1, Alias)`, synchronous, 2 s.
    fn device_alias(&self, path: &str) -> Result<String, glib::Error>;
}

impl AgentBus for gio::DBusConnection {
    fn device_alias(&self, path: &str) -> Result<String, glib::Error> {
        let reply = self.call_sync(Some(BLUEZ), path, "org.freedesktop.DBus.Properties", "Get",
                                   Some(&("org.bluez.Device1", "Alias").to_variant()),
                                   Some(glib::VariantTy::new("(v)").unwrap()), gio::DBusCallFlags::NONE, 2000,
                                   None::<&gio::Cancellable>)?;
        let value = reply.child_value(0).as_variant().unwrap_or_else(|| reply.child_value(0));
        Ok(value.str().map(str::to_owned).unwrap_or_default())
    }
}

pub struct BluetoothAgent {
    inner: Rc<AgentInner>,
}

struct AgentInner {
    application: Option<gtk::Application>,
    factory: PairingFactory,
    bus: RefCell<Option<gio::DBusConnection>>,
    names: RefCell<Option<Rc<dyn AgentBus>>>,
    registration: RefCell<Option<gio::RegistrationId>>,
    /// `Gio.bus_unwatch_name(watch)` (gio's dbus WatcherId is not nameable).
    watch: RefCell<Option<Box<dyn FnOnce()>>>,
    registered: Cell<bool>,
    window: RefCell<Option<Rc<dyn PairingPrompt>>>,
    closed: Cell<bool>,
}

fn tail(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl BluetoothAgent {
    /// `bus` = the system bus (connected in `start` when `None`); `names`
    /// overrides the alias lookup (tests use a bus that has no BlueZ).
    pub fn new(application: Option<gtk::Application>, bus: Option<gio::DBusConnection>,
               names: Option<Rc<dyn AgentBus>>, factory: PairingFactory) -> Self {
        Self { inner: Rc::new(AgentInner {
            application, factory, bus: RefCell::new(bus), names: RefCell::new(names), registration: RefCell::new(None),
            watch: RefCell::new(None), registered: Cell::new(false), window: RefCell::new(None), closed: Cell::new(false),
        }) }
    }

    pub fn is_registered(&self) -> bool {
        self.inner.registered.get()
    }

    pub fn window(&self) -> Option<Rc<dyn PairingPrompt>> {
        self.inner.window.borrow().clone()
    }

    /// Register now and again whenever bluetoothd restarts.
    pub fn start(&self) -> Result<(), glib::Error> {
        let bus = match self.inner.bus.borrow().clone() {
            Some(bus) => bus,
            None => gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>)?,
        };
        self.inner.bus.replace(Some(bus.clone()));
        if self.inner.names.borrow().is_none() {
            self.inner.names.replace(Some(Rc::new(bus.clone())));
        }
        let info = gio::DBusNodeInfo::for_xml(XML)?.lookup_interface("org.bluez.Agent1").expect("Agent1 interface");
        let weak = Rc::downgrade(&self.inner);
        let id = bus.register_object(AGENT_PATH, &info)
            .method_call(move |_, _, _, _, method, params, invocation| match weak.upgrade() {
                Some(inner) => AgentInner::call(&inner, method, &params, Box::new(invocation)),
                None => invocation.return_dbus_error(REJECTED, "Unsupported request"),
            })
            .build()?;
        self.inner.registration.replace(Some(id));
        let appeared = Rc::downgrade(&self.inner);
        let vanished = Rc::downgrade(&self.inner);
        let watch = gio::bus_watch_name_on_connection(&bus, BLUEZ, gio::BusNameWatcherFlags::NONE,
            move |_, _, _| if let Some(inner) = appeared.upgrade() { AgentInner::appeared(&inner) },
            move |_, _| if let Some(inner) = vanished.upgrade() {
                inner.registered.set(false);
                AgentInner::dismiss(&inner);
            });
        self.inner.watch.replace(Some(Box::new(move || gio::bus_unwatch_name(watch))));
        Ok(())
    }

    /// `_call(…, method, params, invocation)` (also the test entry point).
    pub fn call(&self, method: &str, params: &Variant, invocation: Box<dyn Invocation>) {
        AgentInner::call(&self.inner, method, params, invocation);
    }

    pub fn device_name(&self, path: &str) -> String {
        self.inner.device_name(path)
    }

    pub fn close(&self) {
        let inner = &self.inner;
        inner.closed.set(true);
        AgentInner::dismiss(inner);
        let bus = inner.bus.borrow().clone();
        if let (Some(bus), true) = (bus.as_ref(), inner.registered.get()) {
            let _ = bus.call_sync(Some(BLUEZ), "/org/bluez", "org.bluez.AgentManager1", "UnregisterAgent",
                                  Some(&(glib::variant::ObjectPath::try_from(AGENT_PATH).unwrap(),).to_variant()), None,
                                  gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>);
        }
        if let Some(unwatch) = inner.watch.take() {
            unwatch();
        }
        if let (Some(id), Some(bus)) = (inner.registration.take(), bus) {
            let _ = bus.unregister_object(id);
        }
    }
}

impl AgentInner {
    fn manager_call(this: &Rc<Self>, method: &str, params: Variant, done: impl FnOnce(Result<Variant, glib::Error>) + 'static) {
        let Some(bus) = this.bus.borrow().clone() else { return };
        bus.call(Some(BLUEZ), "/org/bluez", "org.bluez.AgentManager1", method, Some(&params), None,
                 gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>, done);
    }

    fn appeared(this: &Rc<Self>) {
        let path = glib::variant::ObjectPath::try_from(AGENT_PATH).unwrap();
        let weak = Rc::downgrade(this);
        Self::manager_call(this, "RegisterAgent", (path.clone(), CAPABILITY).to_variant(), move |result| {
            if let Err(error) = result {
                eprintln!("Could not register Bluetooth pairing agent: {}", error.message());
                return;
            }
            let Some(inner) = weak.upgrade() else { return };
            inner.registered.set(true);
            Self::manager_call(&inner, "RequestDefaultAgent", (path,).to_variant(), |result| {
                if let Err(error) = result {
                    eprintln!("Could not make Bluetooth pairing agent the default: {}", error.message());
                }
            });
        });
    }

    fn device_name(&self, path: &str) -> String {
        let names = self.names.borrow().clone();
        match names.map(|names| names.device_alias(path)) {
            Some(Ok(alias)) if !alias.is_empty() => alias,
            Some(Ok(_)) => tail(path).to_owned(),
            _ => {
                let tail = tail(path);
                tail.strip_prefix("dev_").unwrap_or(tail).replace('_', ":")
            }
        }
    }

    fn call(this: &Rc<Self>, method: &str, params: &Variant, invocation: Box<dyn Invocation>) {
        if method == "Release" {
            this.registered.set(false);
            Self::dismiss(this);
            return invocation.return_value(None);
        }
        if method == "Cancel" {
            Self::dismiss(this);
            return invocation.return_value(None);
        }
        let device = if params.n_children() > 0 { params.child_value(0).str().unwrap_or("").to_owned() } else { String::new() };
        let name = this.device_name(&device);
        let pair = || tr!("Pair with {device}", device = name.as_str());
        match method {
            "RequestPinCode" => Self::prompt(this, invocation, &pair(),
                &tr!("Enter the PIN code for {device}. It is often 0000 or 1234, or shown on the device.", device = name.as_str()),
                PromptOptions { entry: Some(tr!("PIN code")), accept: Some(tr!("Pair")), ..Default::default() },
                Some(valid_pin), Some(Box::new(|text: String| (text,).to_variant()))),
            "RequestPasskey" => Self::prompt(this, invocation, &pair(),
                &tr!("Enter the six-digit passkey shown on {device}.", device = name.as_str()),
                PromptOptions { entry: Some(tr!("Passkey")), digits: true, accept: Some(tr!("Pair")), ..Default::default() },
                Some(valid_passkey), Some(Box::new(|text: String| (text.parse::<u32>().unwrap_or(0),).to_variant()))),
            "RequestConfirmation" => {
                let passkey = params.child_value(1).get::<u32>().unwrap_or(0);
                Self::prompt(this, invocation, &pair(), &tr!("Confirm that {device} shows this code.", device = name.as_str()),
                    PromptOptions { code: Some(passkey_text(passkey)), accept: Some(tr!("Pair")),
                                    reject: Some(tr!("Doesn’t match")), ..Default::default() }, None, None)
            }
            "RequestAuthorization" => Self::prompt(this, invocation, &pair(),
                &tr!("{device} wants to pair with this computer.", device = name.as_str()),
                PromptOptions { accept: Some(tr!("Allow")), reject: Some(tr!("Deny")), ..Default::default() }, None, None),
            "AuthorizeService" => Self::prompt(this, invocation, &tr!("Bluetooth connection"),
                &tr!("{device} wants to connect to this computer.", device = name.as_str()),
                PromptOptions { accept: Some(tr!("Allow")), reject: Some(tr!("Deny")), ..Default::default() }, None, None),
            "DisplayPinCode" | "DisplayPasskey" => {
                let code = if method == "DisplayPinCode" {
                    params.child_value(1).str().unwrap_or("").to_owned()
                } else {
                    passkey_text(params.child_value(1).get::<u32>().unwrap_or(0))
                };
                let current = this.window.borrow().clone();
                let repeat = method == "DisplayPasskey" && current.as_ref().is_some_and(|window| {
                    !window.is_done() && window.displayed_device().as_deref() == Some(device.as_str())
                });
                if repeat {
                    // BlueZ repeats DisplayPasskey as digits are typed on the keyboard.
                    let typed = params.child_value(2).get::<u16>().unwrap_or(0);
                    current.unwrap().set_message(&tr!(
                        "Type this code on {device}, then press Enter. {typed} of {total} digits entered.",
                        device = name.as_str(), typed = typed as i64, total = code.chars().count() as i64));
                } else {
                    Self::show(this, &pair(), &tr!("Type this code on {device}, then press Enter.", device = name.as_str()),
                               Box::new(|_| {}),
                               PromptOptions { code: Some(code), reject: Some(tr!("Close")), ..Default::default() });
                    if let Some(window) = this.window.borrow().as_ref() {
                        window.set_displayed_device(&device);
                    }
                }
                invocation.return_value(None);
            }
            _ => invocation.return_dbus_error(REJECTED, "Unsupported request"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prompt(this: &Rc<Self>, invocation: Box<dyn Invocation>, title: &str, message: &str, options: PromptOptions,
              validator: Option<Validator>, reply: Option<Box<dyn FnOnce(String) -> Variant>>) {
        let answered = Box::new(move |value: Option<Answer>| match (value, reply) {
            (None, _) => invocation.return_dbus_error(REJECTED, "Rejected by user"),
            (Some(Answer::Text(text)), Some(reply)) => invocation.return_value(Some(reply(text))),
            (Some(Answer::Accepted), Some(reply)) => invocation.return_value(Some(reply(String::new()))),
            (Some(_), None) => invocation.return_value(None),
        });
        Self::show(this, title, message, answered, options);
        if let Some(validator) = validator {
            if let Some(window) = this.window.borrow().clone() {
                window.set_validator(validator);
            }
        }
    }

    fn show(this: &Rc<Self>, title: &str, message: &str, answered: Box<dyn FnOnce(Option<Answer>)>, options: PromptOptions) {
        // A new request replaces a stale one; BlueZ already cancelled the old pairing.
        Self::dismiss(this);
        if this.closed.get() {
            return answered(None);
        }
        let window = (this.factory)(title, message, answered, options);
        this.window.replace(Some(window.clone()));
        window.set_application(this.application.as_ref());
        window.present();
        window.focus_entry();
    }

    fn dismiss(this: &Rc<Self>) {
        let window = this.window.take();
        if let Some(window) = window {
            if !window.is_done() {
                window.finish(None);
            }
        }
    }
}

const PAIRING_CSS: &str = ".authentication-window .pairing-code { font-size: 30px; font-weight: 700; \
letter-spacing: 6px; font-feature-settings: \"tnum\"; }";

/// One prompt window (bluetooth_agent.py:45-128).
pub struct PairingWindow {
    pub window: gtk::Window,
    pub message: gtk::Label,
    pub code: Option<gtk::Label>,
    pub entry: Option<gtk::Entry>,
    pub reject_button: gtk::Button,
    pub accept_button: Option<gtk::Button>,
    answer: RefCell<Option<Box<dyn FnOnce(Option<Answer>)>>>,
    validator: Cell<Option<Validator>>,
    done: Cell<bool>,
    displayed: RefCell<Option<String>>,
}

impl PairingWindow {
    pub fn new(title: &str, message: &str, answer: Box<dyn FnOnce(Option<Answer>)>, options: PromptOptions) -> Rc<Self> {
        let window = gtk::Window::builder().title(title).modal(true).resizable(false).build();
        window.add_css_class("authentication-window");
        window.set_titlebar(Some(&headerbar(title)));
        window.set_default_size(420, -1);
        suite_gtk::theme::attach(&window, None);
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(16)
            .margin_top(22).margin_bottom(20).margin_start(22).margin_end(22).build();
        window.set_child(Some(&body));
        let heading = gtk::Box::builder().spacing(14).build();
        let tile = gtk::Box::builder().halign(gtk::Align::Start).valign(gtk::Align::Start).hexpand(false).build();
        tile.add_css_class("settings-tile");
        tile.add_css_class("authentication-lock");
        tile.set_size_request(52, 52);
        let icon = image("bluetooth-active-symbolic", 30);
        icon.set_size_request(52, 52);
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::Center);
        tile.append(&icon);
        heading.append(&tile);
        let label = gtk::Label::builder().label(message).xalign(0.0).wrap(true).max_width_chars(40).hexpand(true).build();
        heading.append(&label);
        body.append(&heading);
        let code = options.code.as_ref().map(|code| {
            let label = gtk::Label::new(Some(code));
            label.add_css_class("pairing-code");
            body.append(&label);
            label
        });
        let entry = options.entry.as_ref().map(|placeholder| {
            let entry = gtk::Entry::builder().placeholder_text(placeholder)
                .input_purpose(if options.digits { gtk::InputPurpose::Digits } else { gtk::InputPurpose::FreeForm }).build();
            entry.update_property(&[gtk::accessible::Property::Label(placeholder)]);
            body.append(&entry);
            entry
        });
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        let reject_button = gtk::Button::with_label(&options.reject.clone().unwrap_or_else(|| tr!("Cancel")));
        buttons.append(&reject_button);
        let accept_button = options.accept.as_ref().map(|accept| {
            let button = gtk::Button::with_label(accept);
            button.add_css_class("suggested-action");
            buttons.append(&button);
            button
        });
        body.append(&buttons);
        let this = Rc::new(Self { window, message: label, code, entry, reject_button, accept_button,
                                  answer: RefCell::new(Some(answer)), validator: Cell::new(None), done: Cell::new(false),
                                  displayed: RefCell::new(None) });
        let weak: Weak<Self> = Rc::downgrade(&this);
        if let Some(entry) = &this.entry {
            let w = weak.clone();
            entry.connect_activate(move |_| if let Some(this) = w.upgrade() { this.accept() });
            let w = weak.clone();
            entry.connect_changed(move |_| if let Some(this) = w.upgrade() { this.validate() });
        }
        let w = weak.clone();
        this.reject_button.connect_clicked(move |_| if let Some(this) = w.upgrade() { this.finish(None) });
        if let Some(button) = &this.accept_button {
            let w = weak.clone();
            button.connect_clicked(move |_| if let Some(this) = w.upgrade() { this.accept() });
        }
        let w = weak.clone();
        this.window.connect_close_request(move |_| {
            if let Some(this) = w.upgrade() {
                this.finish(None);
            }
            glib::Propagation::Stop
        });
        let w = weak;
        key_controller(&this.window, false, move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(this) = w.upgrade() {
                    this.finish(None);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        crate::window::install_css(&WidgetExt::display(&this.window), "pairing", PAIRING_CSS);
        this
    }

    pub fn factory() -> PairingFactory {
        Rc::new(|title, message, answer, options| PairingWindow::new(title, message, answer, options) as Rc<dyn PairingPrompt>)
    }

    pub fn validate(&self) {
        if let (Some(button), Some(entry), Some(validator)) = (&self.accept_button, &self.entry, self.validator.get()) {
            button.set_sensitive(validator(entry.text().trim()));
        }
    }

    pub fn accept(&self) {
        if self.accept_button.as_ref().is_some_and(|button| !button.is_sensitive()) {
            return;
        }
        let value = match &self.entry {
            Some(entry) => Answer::Text(entry.text().trim().to_owned()),
            None => Answer::Accepted,
        };
        self.finish(Some(value));
    }
}

impl PairingPrompt for PairingWindow {
    fn set_application(&self, application: Option<&gtk::Application>) {
        self.window.set_application(application);
    }

    fn present(&self) {
        self.window.present();
    }

    fn focus_entry(&self) {
        if let Some(entry) = &self.entry {
            entry.grab_focus();
        }
    }

    fn set_validator(&self, validator: Validator) {
        self.validator.set(Some(validator));
        self.validate();
    }

    fn set_message(&self, text: &str) {
        self.message.set_text(text);
    }

    fn is_done(&self) -> bool {
        self.done.get()
    }

    fn finish(&self, value: Option<Answer>) {
        if self.done.replace(true) {
            return;
        }
        let answer = self.answer.take();
        if let Some(entry) = &self.entry {
            entry.set_text("");
        }
        self.window.destroy();
        if let Some(answer) = answer {
            answer(value);
        }
    }

    fn displayed_device(&self) -> Option<String> {
        self.displayed.borrow().clone()
    }

    fn set_displayed_device(&self, device: &str) {
        self.displayed.replace(Some(device.to_owned()));
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
#[path = "bluetooth_tests.rs"]
mod tests;
