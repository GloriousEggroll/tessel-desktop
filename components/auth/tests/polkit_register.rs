//! The real libpolkit-agent registration path (`Subject` + `Listener::register`
//! as used by `app::activate`) against a FAKE PolicyKit1 Authority on a private
//! dbus-daemon that this process uses as its "system bus". The fake authority
//! then drives the agent over D-Bus (BeginAuthentication / CancelAuthentication).
//! Windows are fakes; no PAM helper is ever started. One test per process: the
//! system-bus address must be set before GDBus first opens the system bus.

use tessel_auth::agent::{AuthenticationAgent, Prompt, WindowFactory};
use tessel_auth::identity::current_uid;
use tessel_auth::listener::{session_pid, Subject, OBJECT_PATH};
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

mod support;
use support::*;

#[derive(Default)]
struct FakePrompt {
    finished: RefCell<Option<Box<dyn FnOnce(bool)>>>,
    done: Cell<bool>,
    cookie: RefCell<String>,
}

impl Prompt for FakePrompt {
    fn set_application(&self, _application: Option<&gtk::Application>) {}
    fn present(&self) {}
    fn start(&self) {}
    fn finish(&self, cancelled: bool) {
        if !self.done.replace(true) {
            if let Some(finished) = self.finished.take() {
                finished(cancelled);
            }
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

type Outcome = Rc<RefCell<Option<Result<Variant, glib::Error>>>>;

fn agent_call(connection: &gio::DBusConnection, owner: &str, method: &str, params: Variant) -> Outcome {
    let outcome: Outcome = Rc::default();
    let out = outcome.clone();
    connection.call(Some(owner), OBJECT_PATH, "org.freedesktop.PolicyKit1.AuthenticationAgent", method, Some(&params),
                    None, gio::DBusCallFlags::NONE, 10000, None::<&gio::Cancellable>,
                    move |result| { out.replace(Some(result)); });
    outcome
}

fn begin(connection: &gio::DBusConnection, owner: &str, cookie: &str) -> Outcome {
    let mut identity = HashMap::new();
    identity.insert("uid".to_owned(), current_uid().to_variant());
    let identities = vec![("unix-user".to_owned(), identity)];
    let details: HashMap<String, String> = HashMap::new();
    agent_call(connection, owner, "BeginAuthentication",
               ("org.example.test", "Authentication is required.", "dialog-password", details, cookie, identities)
                   .to_variant())
}

#[test]
fn registers_with_fake_authority_and_serves_begin_and_cancel() {
    let dir = scratch("polkit");
    let bus = PrivateBus::start(&dir);
    // SAFETY (process-wide env): this binary has exactly one test and sets
    // the variable before anything opens the system bus.
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &bus.address);
    assert!(bus.address.contains(dir.to_str().unwrap()));
    let authority = fake_authority(&bus.address);

    let windows: Rc<RefCell<Vec<Rc<FakePrompt>>>> = Rc::default();
    let created = windows.clone();
    let factory: WindowFactory = Rc::new(move |request, finished| {
        let prompt = Rc::new(FakePrompt::default());
        prompt.finished.replace(Some(finished));
        prompt.cookie.replace(request.cookie.borrow().as_str().to_owned());
        created.borrow_mut().push(prompt.clone());
        Ok(prompt as Rc<dyn Prompt>)
    });
    let agent = AuthenticationAgent::new(None, factory);
    let pid = session_pid(None).unwrap();
    let subject = match Subject::unix_session_for_process(pid) {
        Ok(subject) => subject,
        Err(error) => {
            eprintln!("SKIP: no logind session for this user ({})", error.message());
            return;
        }
    };
    let mut registration = agent.listener.register(&subject, OBJECT_PATH).expect("register with the fake authority");
    let calls = authority.calls("RegisterAuthenticationAgent");
    assert_eq!(calls.len(), 1);
    let (kind, details) = support::subject(&calls[0].params.child_value(0));
    assert_eq!(kind, "unix-session");
    assert!(details.get("session-id").and_then(|v| v.str().map(str::to_owned)).is_some_and(|s| !s.is_empty()));
    assert_eq!(calls[0].params.child_value(2).str(), Some(OBJECT_PATH));
    let owner = calls[0].sender.clone();

    // Success: the conversation completes -> BeginAuthentication returns ().
    let client = connect(&bus.address);
    let first = begin(&client, &owner, "cookie-1");
    wait_for("first window", Duration::from_secs(5), || windows.borrow().len() == 1);
    assert_eq!(*windows.borrow()[0].cookie.borrow(), "cookie-1");
    windows.borrow()[0].finish(false);
    wait_for("first reply", Duration::from_secs(5), || first.borrow().is_some());
    assert!(first.borrow().as_ref().unwrap().is_ok(), "{:?}", first.borrow());

    // polkitd cancels: the window closes and BeginAuthentication fails.
    let second = begin(&client, &owner, "cookie-2");
    wait_for("second window", Duration::from_secs(5), || windows.borrow().len() == 2);
    let cancel = agent_call(&client, &owner, "CancelAuthentication", ("cookie-2",).to_variant());
    wait_for("cancel", Duration::from_secs(5), || second.borrow().is_some() && cancel.borrow().is_some());
    assert!(windows.borrow()[1].done.get());
    assert!(second.borrow().as_ref().unwrap().is_err());
    assert!(agent.requests().is_empty());

    // Queued request then shutdown: every request is answered exactly once.
    let third = begin(&client, &owner, "cookie-3");
    let fourth = begin(&client, &owner, "cookie-4");
    wait_for("queued", Duration::from_secs(5), || agent.requests().len() == 2);
    agent.close();
    wait_for("shutdown replies", Duration::from_secs(5), || third.borrow().is_some() && fourth.borrow().is_some());
    assert!(third.borrow().as_ref().unwrap().is_err() && fourth.borrow().as_ref().unwrap().is_err());

    registration.unregister();
    let unregister = authority.calls("UnregisterAuthenticationAgent");
    assert_eq!(unregister.len(), 1);
    assert_eq!(unregister[0].params.child_value(1).str(), Some(OBJECT_PATH));
}
