//! GTK checks (port of test_authentication.py + PairingWindow) and end-to-end
//! runs of the real binary. Runs ONLY inside the private nested headless labwc
//! started by tests/run_headless.sh, whose own dbus-daemon is also the
//! "system" bus; a fake PolicyKit1 Authority is served on it. Conversations
//! use FakeSession (no PAM helper, no polkitd, no real credentials).

use authentication_manager::agent::{AuthenticationAgent, Prompt, WindowFactory};
use authentication_manager::bluetooth::*;
use authentication_manager::ffi;
use authentication_manager::identity::current_uid;
use authentication_manager::listener::OBJECT_PATH;
use authentication_manager::session::{FakeSession, SessionEvent};
use authentication_manager::window::AuthenticationWindow;
use gio::ffi::GAsyncResult;
use gio::prelude::*;
use glib::ffi::{gpointer, GError};
use glib::translate::*;
use gtk::prelude::*;
use std::cell::RefCell;
use std::ffi::CString;
use std::io::Read;
use std::os::unix::net::UnixDatagram;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

mod support;
use support::*;

const BINARY: &str = env!("CARGO_BIN_EXE_authentication-manager");
const CANCELLED: i32 = 19;

thread_local! {
    static RESULTS: RefCell<Vec<(bool, Result<bool, i32>)>> = const { RefCell::new(Vec::new()) };
}
static SENTINEL: u8 = 0;

unsafe extern "C" fn ready(source: *mut glib::gobject_ffi::GObject, result: *mut GAsyncResult, data: gpointer) {
    let mut error: *mut GError = std::ptr::null_mut();
    let ok = ffi::polkit_agent_listener_initiate_authentication_finish(source as *mut _, result, &mut error);
    let outcome = if error.is_null() { Ok(ok != 0) } else {
        let error: glib::Error = from_glib_full(error);
        Err(if error.matches(gio::IOErrorEnum::Cancelled) { CANCELLED } else { -1 })
    };
    RESULTS.with(|r| r.borrow_mut().push((data as *const u8 == &SENTINEL as *const u8, outcome)));
}

fn results() -> Vec<Result<bool, i32>> {
    RESULTS.with(|r| r.borrow().iter().map(|(_, o)| o.clone()).collect())
}

fn settle() {
    pump(Duration::from_millis(80));
}

struct Fx {
    app: gtk::Application,
    agent: AuthenticationAgent,
    sessions: Rc<RefCell<Vec<Rc<FakeSession>>>>,
}

impl Fx {
    fn new(name: &str) -> Self {
        RESULTS.with(|r| r.borrow_mut().clear());
        let app = gtk::Application::new(Some(&format!("org.wayshell.AuthTest.{name}")), gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gio::Cancellable>).unwrap();
        let sessions: Rc<RefCell<Vec<Rc<FakeSession>>>> = Rc::default();
        let factory: WindowFactory = AuthenticationWindow::factory(FakeSession::factory(sessions.clone()));
        let agent = AuthenticationAgent::new(Some(app.clone()), factory);
        Self { app, agent, sessions }
    }

    fn begin(&self, cancellable: Option<&gio::Cancellable>, uids: &[i32]) -> Option<Rc<dyn Prompt>> {
        unsafe {
            let mut list: *mut glib::ffi::GList = std::ptr::null_mut();
            for uid in uids {
                list = glib::ffi::g_list_append(list, ffi::polkit_unix_user_new(*uid));
            }
            let details = ffi::polkit_details_new();
            let (a, m, i, c) = (CString::new("org.example.test").unwrap(),
                                CString::new("Authentication is required to change a setting.").unwrap(),
                                CString::new("dialog-password").unwrap(), CString::new("fake-cookie").unwrap());
            ffi::polkit_agent_listener_initiate_authentication(self.agent.listener.as_ptr(), a.as_ptr(), m.as_ptr(),
                i.as_ptr(), details, c.as_ptr(), list, cancellable.map_or(std::ptr::null_mut(), |c| c.as_ptr()),
                Some(ready), &SENTINEL as *const u8 as gpointer);
            glib::gobject_ffi::g_object_unref(details as *mut _);
            let mut item = list;
            while !item.is_null() {
                glib::gobject_ffi::g_object_unref((*item).data as *mut _);
                item = (*item).next;
            }
            glib::ffi::g_list_free(list);
        }
        settle();
        self.agent.window()
    }

    fn session(&self) -> Rc<FakeSession> {
        self.sessions.borrow().last().cloned().unwrap()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.agent.close();
        settle();
        let _ = &self.app;
    }
}

fn auth(prompt: &Rc<dyn Prompt>) -> &AuthenticationWindow {
    prompt.as_any().downcast_ref::<AuthenticationWindow>().expect("AuthenticationWindow")
}

fn uid() -> i32 {
    current_uid() as i32
}

fn test_hidden_response_echo_prompt_and_success() {
    let fx = Fx::new("hidden");
    let prompt = fx.begin(None, &[uid()]).expect("window");
    let window = auth(&prompt);
    assert!(!EntryExt::is_visible(&window.entry));
    assert!(window.entry.buffer().is::<gtk::PasswordEntryBuffer>(), "secure buffer");
    assert!(unsafe { window.window.data::<suite_gtk::theme::WindowTheme>("waydock-companion-theme") }.is_some());
    assert!(window.window.has_css_class("authentication-window"));
    assert!(window.window.is_modal() && !window.window.is_resizable());
    assert_eq!(window.window.title().as_deref(), Some("Authentication Required"));
    let session = fx.session();
    assert_eq!(session.cookie, "fake-cookie");
    window.entry.set_text("test-response");
    window.submit();
    assert_eq!(*session.responses.borrow(), vec!["test-response".to_owned()]);
    assert_eq!(window.entry.text(), "");
    assert!(!window.entry.is_sensitive());
    session.emit(SessionEvent::Request { prompt: "Verification code:".into(), echo_on: true });
    assert!(EntryExt::is_visible(&window.entry));
    assert!(window.entry.is_sensitive());
    assert_eq!(window.entry.input_purpose(), gtk::InputPurpose::FreeForm);
    session.emit(SessionEvent::Completed(true));
    settle();
    assert_eq!(results(), vec![Ok(true)]);
    assert!(fx.agent.window().is_none());
    assert!(window.request().cookie.borrow().is_empty(), "cookie wiped");
}

fn test_password_prompt_is_the_placeholder_and_status_hides_when_empty() {
    let fx = Fx::new("placeholder");
    let prompt = fx.begin(None, &[uid()]).unwrap();
    let window = auth(&prompt);
    assert_eq!(window.entry.placeholder_text().as_deref(), Some("Password"));
    assert!(!window.prompt.is_visible());
    assert!(!window.status.is_visible());
    assert!(window.status.has_css_class("subtitle"));
    assert!(window.authenticate.has_css_class("suggested-action"));
    assert_eq!(window.authenticate.label().as_deref(), Some("Authenticate"));
    assert_eq!(window.cancel_button.label().as_deref(), Some("Cancel"));
    assert_eq!(window.details.label().as_deref(), Some("Details"));
    let session = fx.session();
    session.emit(SessionEvent::Request { prompt: "Verification code:".into(), echo_on: true });
    assert!(window.prompt.is_visible());
    assert_eq!(window.prompt.text(), "Verification code:");
    assert_eq!(window.entry.placeholder_text().as_deref(), Some(""));
    session.emit(SessionEvent::Error("Sorry, try again.".into()));
    assert!(window.status.is_visible());
    assert!(window.status.has_css_class("error"));
    session.emit(SessionEvent::Info("Touch the key".into()));
    assert!(!window.status.has_css_class("error"));
    window.finish(true);
    settle();
}

fn test_failure_retry_and_identity_switch_cancel_old_session() {
    let fx = Fx::new("retry");
    let prompt = fx.begin(None, &[uid(), 0]).unwrap();
    let window = auth(&prompt);
    assert!(window.identity.parent().is_some(), "dropdown shown for >1 identity");
    let first = fx.session();
    assert_eq!(first.identity.uid, Some(current_uid()));
    first.emit(SessionEvent::Completed(false));
    assert!(!window.is_done());
    assert!(window.status.text().contains("failed"));
    assert_eq!(window.authenticate.label().as_deref(), Some("Try Again"));
    assert!(window.authenticate.is_sensitive());
    window.submit();
    let second = fx.session();
    assert!(!Rc::ptr_eq(&first, &second));
    window.identity.set_selected(1);
    assert!(second.cancelled.get());
    assert_eq!(fx.session().identity.uid, Some(0));
    assert_eq!(window.attempts(), 1, "identity change resets attempts");
    window.finish(true);
    settle();
    assert_eq!(results(), vec![Err(CANCELLED)]);
}

fn test_three_failures_finish_the_conversation() {
    let fx = Fx::new("three");
    let prompt = fx.begin(None, &[uid()]).unwrap();
    let window = auth(&prompt);
    assert!(window.identity.parent().is_none(), "single identity: label instead of dropdown");
    for attempt in 1..=3 {
        assert_eq!(window.attempts(), attempt);
        fx.session().emit(SessionEvent::Completed(false));
        if attempt < 3 {
            assert!(!window.is_done());
            window.submit();
        }
    }
    settle();
    assert!(window.is_done());
    // TRUE either way: polkitd's helper decides authorization.
    assert_eq!(results(), vec![Ok(true)]);
}

fn test_caller_cancellation_closes_prompt_and_queue() {
    let fx = Fx::new("cancel");
    let (first, queued) = (gio::Cancellable::new(), gio::Cancellable::new());
    let prompt = fx.begin(Some(&first), &[uid()]).unwrap();
    fx.begin(Some(&queued), &[uid()]);
    assert_eq!(fx.agent.requests().len(), 2);
    queued.cancel();
    settle();
    assert_eq!(fx.agent.requests().len(), 1);
    first.cancel();
    settle();
    assert!(auth(&prompt).is_done());
    assert!(fx.agent.requests().is_empty());
    assert_eq!(results(), vec![Err(CANCELLED); 2]);
}

fn test_close_and_cancel_button_cancel() {
    let fx = Fx::new("close");
    let prompt = fx.begin(None, &[uid()]).unwrap();
    auth(&prompt).window.close();
    settle();
    let prompt = fx.begin(None, &[uid()]).unwrap();
    auth(&prompt).cancel_button.emit_clicked();
    settle();
    assert_eq!(results(), vec![Err(CANCELLED); 2]);
    assert_eq!(fx.sessions.borrow().iter().filter(|s| s.cancelled.get()).count(), 2, "sessions cancelled");
}

fn test_shutdown_cancels_all_requests_once() {
    let fx = Fx::new("shutdown");
    fx.begin(None, &[uid()]);
    fx.begin(None, &[uid()]);
    fx.agent.close();
    fx.agent.close();
    settle();
    assert_eq!(results(), vec![Err(CANCELLED); 2]);
    assert!(fx.agent.requests().is_empty());
}

fn test_native_polkit_callback_preserves_opaque_user_data() {
    for cancel in [false, true] {
        let fx = Fx::new(if cancel { "native1" } else { "native0" });
        let prompt = fx.begin(None, &[uid()]).unwrap();
        if cancel { auth(&prompt).finish(true); } else { fx.session().emit(SessionEvent::Completed(true)); }
        settle();
        RESULTS.with(|r| {
            let r = r.borrow();
            assert_eq!(r.len(), 1);
            assert!(r[0].0, "Polkit must receive its original native callback context");
            assert_eq!(r[0].1, if cancel { Err(CANCELLED) } else { Ok(true) });
        });
    }
}

fn pairing(options: PromptOptions) -> (Rc<PairingWindow>, Rc<RefCell<Vec<Option<Answer>>>>) {
    let answers: Rc<RefCell<Vec<Option<Answer>>>> = Rc::default();
    let a = answers.clone();
    let window = PairingWindow::new("Pair with Phone", "Message", Box::new(move |v| a.borrow_mut().push(v)), options);
    window.window.present();
    settle();
    (window, answers)
}

fn test_pairing_window_validation_and_answers() {
    let (window, answers) = pairing(PromptOptions { entry: Some("Passkey".into()), digits: true,
                                                    accept: Some("Pair".into()), ..Default::default() });
    assert!(window.window.has_css_class("authentication-window"));
    window.set_validator(valid_passkey);
    let accept = window.accept_button.clone().unwrap();
    assert!(!accept.is_sensitive(), "disabled until valid");
    assert!(accept.has_css_class("suggested-action"));
    assert_eq!(window.reject_button.label().as_deref(), Some("Cancel"));
    let entry = window.entry.clone().unwrap();
    assert_eq!(entry.input_purpose(), gtk::InputPurpose::Digits);
    entry.set_text("12a");
    assert!(!accept.is_sensitive());
    window.accept();
    assert!(answers.borrow().is_empty(), "insensitive accept ignored");
    entry.set_text(" 123456 ");
    assert!(accept.is_sensitive());
    entry.emit_activate();
    assert_eq!(*answers.borrow(), vec![Some(Answer::Text("123456".into()))]);
    assert_eq!(entry.text(), "");

    let (window, answers) = pairing(PromptOptions { code: Some("004021".into()), accept: Some("Pair".into()),
                                                    reject: Some("Doesn’t match".into()), ..Default::default() });
    assert!(window.code.as_ref().unwrap().has_css_class("pairing-code"));
    assert_eq!(window.reject_button.label().as_deref(), Some("Doesn’t match"));
    window.window.close();
    assert_eq!(*answers.borrow(), vec![None]);
}

fn test_pairing_agent_display_passkey_updates_real_window() {
    let agent = BluetoothAgent::new(None, None, Some(Rc::new(NoBluez)), PairingWindow::factory());
    let invocation = || Box::new(Ignore) as Box<dyn Invocation>;
    let device = glib::variant::ObjectPath::try_from("/org/bluez/hci0/dev_11_22").unwrap();
    agent.call("DisplayPasskey", &(device.clone(), 7u32, 0u16).to_variant(), invocation());
    settle();
    let first = agent.window().unwrap();
    let window = first.as_any().downcast_ref::<PairingWindow>().unwrap();
    assert_eq!(window.code.as_ref().unwrap().text(), "000007");
    assert_eq!(window.window.title().as_deref(), Some("Pair with 11:22"));
    agent.call("DisplayPasskey", &(device, 7u32, 4u16).to_variant(), invocation());
    assert!(Rc::ptr_eq(&first, &agent.window().unwrap()));
    assert!(window.message.text().contains("4 of 6 digits entered."), "{}", window.message.text());
    agent.close();
    assert!(window.is_done());
}

struct NoBluez;
impl AgentBus for NoBluez {
    fn device_alias(&self, _: &str) -> Result<String, glib::Error> {
        Err(glib::Error::new(gio::IOErrorEnum::Failed, "no bluez"))
    }
}
struct Ignore;
impl Invocation for Ignore {
    fn return_value(self: Box<Self>, _: Option<glib::Variant>) {}
    fn return_dbus_error(self: Box<Self>, _: &str, _: &str) {}
}

// ---------------------------------------------------------------- end-to-end

struct Run {
    child: std::process::Child,
    ready_after: Option<Duration>,
    socket: UnixDatagram,
    started: Instant,
}

fn spawn(program: &str, args: &[&str], tag: &str, extra: &[(&str, &str)]) -> Run {
    let dir = scratch(tag);
    let notify = dir.join("n");
    let socket = UnixDatagram::bind(&notify).unwrap();
    socket.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let mut command = Command::new(program);
    command.args(args).env("NOTIFY_SOCKET", &notify).stdout(Stdio::null()).stderr(Stdio::piped());
    for (k, v) in extra {
        command.env(k, v);
    }
    let started = Instant::now();
    Run { child: command.spawn().unwrap(), ready_after: None, socket, started }
}

impl Run {
    fn wait_ready(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        let mut buffer = [0u8; 64];
        while Instant::now() < end {
            if let Ok(n) = self.socket.recv(&mut buffer) {
                assert_eq!(&buffer[..n], b"READY=1");
                self.ready_after = Some(self.started.elapsed());
                return true;
            }
            if let Ok(Some(_)) = self.child.try_wait() {
                return false;
            }
        }
        false
    }

    fn rss_kb(&self) -> u64 {
        std::fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap_or_default().lines()
            .find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0)
    }

    fn terminate(mut self) -> (Option<i32>, String) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let end = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                break status.code();
            }
            if Instant::now() > end {
                let _ = self.child.kill();
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        let _ = self.child.stderr.take().unwrap().read_to_string(&mut stderr);
        (status, stderr)
    }
}

fn name_has_owner(name: &str) -> bool {
    let bus = connect(&std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap());
    bus.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner",
                  Some(&(name,).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>)
        .ok().and_then(|v| v.get::<(bool,)>()).is_some_and(|(b,)| b)
}

fn registrations(authority: &Service) -> Vec<Call> {
    authority.calls("RegisterAuthenticationAgent")
}

/// Register → READY → second instance is a no-op → SIGTERM unregisters.
fn e2e(authority: &Service, program: &str, args: &[&str], label: &str) -> (Call, Duration, u64) {
    authority.calls.lock().unwrap().clear();
    let mut run = spawn(program, args, label, &[]);
    assert!(run.wait_ready(Duration::from_secs(15)), "{label}: READY");
    let register = registrations(authority);
    assert_eq!(register.len(), 1, "{label}: one registration");
    assert!(register[0].at <= Instant::now());
    assert!(run.started + run.ready_after.unwrap() >= register[0].at, "{label}: READY only after registration");
    assert!(name_has_owner("org.wayshell.Authentication"));
    std::thread::sleep(Duration::from_secs(3));
    let rss = run.rss_kb();
    // A second launch only activates the first.
    let second = Command::new(program).args(args).stderr(Stdio::null()).status().unwrap();
    assert_eq!(second.code(), Some(0));
    assert_eq!(registrations(authority).len(), 1, "{label}: second instance did not register");
    let ready = run.ready_after.unwrap();
    let (status, stderr) = run.terminate();
    assert_eq!(status, Some(0), "{label}: {stderr}");
    assert_eq!(authority.calls("UnregisterAuthenticationAgent").len(), 1, "{label}: unregistered on SIGTERM");
    (register[0].clone(), ready, rss)
}

fn test_e2e_binary_registers_then_ready_and_parity_with_python() {
    let authority = fake_authority(&std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap());
    let (rust, ready, rss) = e2e(&authority, BINARY, &[], "rust-debug");
    let (kind, details) = subject(&rust.params.child_value(0));
    assert_eq!(kind, "unix-session");
    assert_eq!(rust.params.child_value(2).str(), Some(OBJECT_PATH));
    println!("MEASURE rust-debug ready_ms={} rss_kb={rss}", ready.as_millis());
    if let Ok(release) = std::env::var("AUTH_RELEASE_BIN") {
        for _ in 0..3 {
            let (_, ready, rss) = e2e(&authority, &release, &[], "rust-release");
            println!("MEASURE rust-release ready_ms={} rss_kb={rss}", ready.as_millis());
        }
    }
    if let Ok(script) = std::env::var("AUTH_PYTHON_SCRIPT") {
        for _ in 0..3 {
            let (python, ready, rss) = e2e(&authority, "/usr/bin/python3", &[&script], "python");
            println!("MEASURE python ready_ms={} rss_kb={rss}", ready.as_millis());
            let (pkind, pdetails) = subject(&python.params.child_value(0));
            assert_eq!((pkind.as_str(), pdetails.get("session-id").map(|v| v.str().map(str::to_owned))),
                       (kind.as_str(), details.get("session-id").map(|v| v.str().map(str::to_owned))),
                       "same subject as Python");
            assert_eq!(python.params.child_value(1).str(), rust.params.child_value(1).str(), "same locale");
            assert_eq!(python.params.child_value(2).str(), rust.params.child_value(2).str(), "same object path");
        }
    }
}

fn test_e2e_registration_failure_quits_without_ready() {
    let authority = fake_authority(&std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap());
    let mut run = spawn(BINARY, &[], "fail", &[("LABWC_PID", "999999999")]);
    assert!(!run.wait_ready(Duration::from_secs(10)), "no READY");
    let (status, stderr) = run.terminate();
    assert_eq!(status, Some(0), "GApplication quit (systemd sees exit before READY)");
    assert!(stderr.lines().any(|l| l.starts_with("Could not register authentication agent: No session for pid")),
            "{stderr}");
    assert!(registrations(&authority).is_empty());
}

fn main() {
    let isolated = std::env::var("AUTH_UI_ISOLATED_TEST").as_deref() == Ok("1")
        && std::env::var("XDG_RUNTIME_DIR").is_ok_and(|d| d != "/run/user/1000")
        && std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| !d.is_empty())
        && std::env::var("DBUS_SYSTEM_BUS_ADDRESS").ok() == std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()
        && std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok_and(|a| a.contains("/run/user/1000/cbt/"));
    if !isolated {
        println!("ui_headless: skipped (run tests/run_headless.sh: private nested labwc + private buses)");
        return;
    }
    gtk::init().expect("private display");
    let tests: &[(&str, fn())] = &[
        ("hidden_response_echo_prompt_and_success", test_hidden_response_echo_prompt_and_success),
        ("password_prompt_is_the_placeholder_and_status_hides_when_empty",
         test_password_prompt_is_the_placeholder_and_status_hides_when_empty),
        ("failure_retry_and_identity_switch_cancel_old_session", test_failure_retry_and_identity_switch_cancel_old_session),
        ("three_failures_finish_the_conversation", test_three_failures_finish_the_conversation),
        ("caller_cancellation_closes_prompt_and_queue", test_caller_cancellation_closes_prompt_and_queue),
        ("close_and_cancel_button_cancel", test_close_and_cancel_button_cancel),
        ("shutdown_cancels_all_requests_once", test_shutdown_cancels_all_requests_once),
        ("native_polkit_callback_preserves_opaque_user_data", test_native_polkit_callback_preserves_opaque_user_data),
        ("pairing_window_validation_and_answers", test_pairing_window_validation_and_answers),
        ("pairing_agent_display_passkey_updates_real_window", test_pairing_agent_display_passkey_updates_real_window),
        ("e2e_registration_failure_quits_without_ready", test_e2e_registration_failure_quits_without_ready),
        ("e2e_binary_registers_then_ready_and_parity_with_python", test_e2e_binary_registers_then_ready_and_parity_with_python),
    ];
    let mut failed = 0;
    for (name, test) in tests {
        let outcome = std::panic::catch_unwind(test);
        settle();
        match outcome {
            Ok(()) => println!("test {name} ... ok"),
            Err(error) => {
                failed += 1;
                let message = error.downcast_ref::<String>().cloned()
                    .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                println!("test {name} ... FAILED: {message}");
            }
        }
    }
    println!("ui_headless: {} passed, {failed} failed", tests.len() - failed);
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
