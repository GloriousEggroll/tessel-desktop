//! Port of test_bluetooth_agent.py: dispatch without BlueZ or a display.

use super::*;

#[derive(Default)]
struct Reply {
    answered: Cell<bool>,
    value: RefCell<Option<Variant>>,
    error: RefCell<Option<String>>,
}

struct Invoke(Rc<Reply>);

impl Invocation for Invoke {
    fn return_value(self: Box<Self>, value: Option<Variant>) {
        self.0.answered.set(true);
        self.0.value.replace(value);
    }
    fn return_dbus_error(self: Box<Self>, name: &str, _message: &str) {
        self.0.answered.set(true);
        self.0.error.replace(Some(name.to_owned()));
    }
}

/// `Bus.call_sync` raises: alias lookup falls back to the path.
struct NoBluez;

impl AgentBus for NoBluez {
    fn device_alias(&self, _path: &str) -> Result<String, glib::Error> {
        Err(glib::Error::new(gio::IOErrorEnum::Failed, "no bluez"))
    }
}

struct Window {
    title: String,
    message: RefCell<String>,
    options: PromptOptions,
    answer: RefCell<Option<Box<dyn FnOnce(Option<Answer>)>>>,
    done: Cell<bool>,
    validator: Cell<Option<Validator>>,
    displayed: RefCell<Option<String>>,
}

impl PairingPrompt for Window {
    fn set_application(&self, _application: Option<&gtk::Application>) {}
    fn present(&self) {}
    fn focus_entry(&self) {}
    fn set_validator(&self, validator: Validator) {
        self.validator.set(Some(validator));
    }
    fn set_message(&self, text: &str) {
        self.message.replace(text.to_owned());
    }
    fn is_done(&self) -> bool {
        self.done.get()
    }
    fn finish(&self, value: Option<Answer>) {
        if !self.done.replace(true) {
            if let Some(answer) = self.answer.take() {
                answer(value);
            }
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

struct Fx {
    agent: BluetoothAgent,
    created: Rc<RefCell<Vec<Rc<Window>>>>,
}

impl Fx {
    fn new() -> Self {
        let created: Rc<RefCell<Vec<Rc<Window>>>> = Rc::default();
        let list = created.clone();
        let factory: PairingFactory = Rc::new(move |title, message, answer, options| {
            let window = Rc::new(Window { title: title.into(), message: RefCell::new(message.into()), options,
                                          answer: RefCell::new(Some(answer)), done: Cell::new(false),
                                          validator: Cell::new(None), displayed: RefCell::new(None) });
            list.borrow_mut().push(window.clone());
            window as Rc<dyn PairingPrompt>
        });
        Self { agent: BluetoothAgent::new(None, None, Some(Rc::new(NoBluez)), factory), created }
    }

    fn call(&self, method: &str, params: Variant) -> Rc<Reply> {
        let reply = Rc::new(Reply::default());
        self.agent.call(method, &params, Box::new(Invoke(reply.clone())));
        reply
    }

    fn last(&self) -> Rc<Window> {
        self.created.borrow().last().cloned().unwrap()
    }
}

fn path(text: &str) -> glib::variant::ObjectPath {
    glib::variant::ObjectPath::try_from(text).unwrap()
}

#[test]
fn confirmation_accept_and_reject() {
    let fx = Fx::new();
    let invocation = fx.call("RequestConfirmation", (path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF"), 4021u32).to_variant());
    let window = fx.last();
    assert_eq!(window.options.code.as_deref(), Some("004021"));
    assert!(window.title.contains("AA:BB:CC:DD:EE:FF"));
    assert_eq!(window.options.accept.as_deref(), Some("Pair"));
    assert_eq!(window.options.reject.as_deref(), Some("Doesn’t match"));
    assert!(!invocation.answered.get());
    window.finish(Some(Answer::Accepted));
    assert!(invocation.answered.get());
    assert!(invocation.error.borrow().is_none());
    assert!(invocation.value.borrow().is_none(), "empty reply");
    let invocation = fx.call("RequestConfirmation", (path("/org/bluez/hci0/dev_11"), 1u32).to_variant());
    fx.last().finish(None);
    assert_eq!(invocation.error.borrow().as_deref(), Some(REJECTED));
}

#[test]
fn pin_and_passkey_replies() {
    let fx = Fx::new();
    let invocation = fx.call("RequestPinCode", (path("/org/bluez/hci0/dev_11"),).to_variant());
    assert_eq!(fx.last().validator.get().map(|v| v as usize), Some(valid_pin as Validator as usize));
    assert_eq!(fx.last().options.entry.as_deref(), Some("PIN code"));
    fx.last().finish(Some(Answer::Text("0000".into())));
    assert_eq!(invocation.value.borrow().as_ref().unwrap().get::<(String,)>(), Some(("0000".to_owned(),)));
    let invocation = fx.call("RequestPasskey", (path("/org/bluez/hci0/dev_11"),).to_variant());
    assert!(fx.last().options.digits);
    fx.last().finish(Some(Answer::Text("123456".into())));
    assert_eq!(invocation.value.borrow().as_ref().unwrap().get::<(u32,)>(), Some((123456,)));
    assert!(valid_passkey("123456"));
    assert!(!valid_passkey("1234567"));
    assert!(!valid_passkey(""));
    assert!(!valid_passkey("12a"));
    assert!(!valid_pin(""));
    assert!(valid_pin("0000"));
    assert!(valid_pin(&"x".repeat(16)));
    assert!(!valid_pin(&"x".repeat(17)));
}

#[test]
fn display_passkey_updates_progress_in_one_window() {
    let fx = Fx::new();
    let device = "/org/bluez/hci0/dev_11";
    assert!(fx.call("DisplayPasskey", (path(device), 987654u32, 0u16).to_variant()).answered.get());
    assert_eq!(fx.created.borrow().len(), 1);
    assert_eq!(fx.created.borrow()[0].options.code.as_deref(), Some("987654"));
    assert_eq!(fx.created.borrow()[0].options.reject.as_deref(), Some("Close"));
    fx.call("DisplayPasskey", (path(device), 987654u32, 3u16).to_variant());
    assert_eq!(fx.created.borrow().len(), 1);
    assert!(fx.created.borrow()[0].message.borrow().contains("3 of 6"));
    // Another device (or DisplayPinCode) replaces the window.
    assert!(fx.call("DisplayPinCode", (path(device), "4321").to_variant()).answered.get());
    assert_eq!(fx.created.borrow().len(), 2);
    assert!(fx.created.borrow()[0].done.get());
    assert_eq!(fx.last().options.code.as_deref(), Some("4321"));
}

#[test]
fn cancel_and_new_request_dismiss_the_open_prompt() {
    let fx = Fx::new();
    let first = fx.call("RequestAuthorization", (path("/org/bluez/hci0/dev_11"),).to_variant());
    let second = fx.call("AuthorizeService",
                         (path("/org/bluez/hci0/dev_11"), "0000110b-0000-1000-8000-00805f9b34fb").to_variant());
    assert_eq!(first.error.borrow().as_deref(), Some(REJECTED));
    assert!(!second.answered.get());
    assert_eq!(fx.last().title, "Bluetooth connection");
    assert!(fx.call("Cancel", ().to_variant()).answered.get());
    assert_eq!(second.error.borrow().as_deref(), Some(REJECTED));
    assert!(fx.agent.window().is_none());
}

#[test]
fn release_unregisters_and_closed_agent_rejects() {
    let fx = Fx::new();
    let pending = fx.call("RequestAuthorization", (path("/org/bluez/hci0/dev_11"),).to_variant());
    assert!(fx.call("Release", ().to_variant()).answered.get());
    assert_eq!(pending.error.borrow().as_deref(), Some(REJECTED));
    assert!(!fx.agent.is_registered());
    fx.agent.close();
    let after = fx.call("RequestAuthorization", (path("/org/bluez/hci0/dev_11"),).to_variant());
    assert_eq!(after.error.borrow().as_deref(), Some(REJECTED));
    assert_eq!(fx.call("Bogus", ().to_variant()).error.borrow().as_deref(), Some(REJECTED));
}

#[test]
fn device_name_fallbacks() {
    let fx = Fx::new();
    assert_eq!(fx.agent.device_name("/org/bluez/hci0/dev_AA_BB"), "AA:BB");
    struct Named(&'static str);
    impl AgentBus for Named {
        fn device_alias(&self, _path: &str) -> Result<String, glib::Error> {
            Ok(self.0.to_owned())
        }
    }
    let agent = BluetoothAgent::new(None, None, Some(Rc::new(Named("Headphones"))), Rc::new(|_, _, _, _| unreachable!()));
    assert_eq!(agent.device_name("/org/bluez/hci0/dev_AA"), "Headphones");
    let agent = BluetoothAgent::new(None, None, Some(Rc::new(Named(""))), Rc::new(|_, _, _, _| unreachable!()));
    assert_eq!(agent.device_name("/org/bluez/hci0/dev_AA_BB"), "dev_AA_BB");
    assert_eq!(passkey_text(42), "000042");
}
