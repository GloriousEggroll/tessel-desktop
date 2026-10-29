#![allow(deprecated)]
//! The real gio transport (`LocaleServices`) and the `language-manager
//! --status` binary against FAKE localed / AccountsService / systemd user
//! manager served by this process on a private bus (gio::TestDBus), which is
//! also the "system" bus. The real localed/accountsservice are never called;
//! every file lives under TMPDIR.

use gio::prelude::*;
use language_manager::backend::{LanguageBackend, Services, LANGUAGES};
use language_manager::services::LocaleServices;
use std::cell::RefCell;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_language-manager");

fn pump(duration: Duration) {
    let end = Instant::now() + duration;
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Run `job` on another thread (the blocking calls need this thread to serve the fakes).
fn off_thread<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> T {
    let handle = std::thread::spawn(job);
    while !handle.is_finished() {
        pump(Duration::from_millis(5));
    }
    handle.join().unwrap()
}

#[derive(Default)]
struct State {
    locale: Vec<String>,
    language: String,
    calls: Vec<String>,
}

fn request_name(connection: &gio::DBusConnection, name: &str) {
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                         Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).unwrap();
}

fn interface(xml: &str, name: &str) -> gio::DBusInterfaceInfo {
    gio::DBusNodeInfo::for_xml(xml).unwrap().lookup_interface(name).unwrap()
}

fn interactive(invocation: &gio::DBusMethodInvocation) -> &'static str {
    if invocation.message().flags().contains(gio::DBusMessageFlags::ALLOW_INTERACTIVE_AUTHORIZATION) { "+i" } else { "" }
}

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>)
        .unwrap()
}

fn serve_locale(connection: &gio::DBusConnection, state: &Rc<RefCell<State>>) {
    request_name(connection, "org.freedesktop.locale1");
    request_name(connection, "org.freedesktop.systemd1");
    let s = state.clone();
    let p = state.clone();
    connection.register_object("/org/freedesktop/locale1", &interface(r#"<node><interface name="org.freedesktop.locale1">
        <method name="SetLocale"><arg type="as" direction="in"/><arg type="b" direction="in"/></method>
        <property name="Locale" type="as" access="read"/></interface></node>"#, "org.freedesktop.locale1"))
        .method_call(move |_, _, _, _, method, params, invocation| {
            let mut state = s.borrow_mut();
            state.calls.push(format!("{method}{}{}", interactive(&invocation), params.print(false)));
            let (values, _): (Vec<String>, bool) = params.get().unwrap();
            // localed drops entries equal to LANG in its read-back.
            let lang = values.iter().find(|v| v.starts_with("LANG=")).cloned();
            state.locale = values.iter().filter(|v| v.starts_with("LANG=")
                || lang.as_ref().map(|l| &l[5..]) != v.split_once('=').map(|x| x.1)).cloned().collect();
            invocation.return_value(None);
        })
        .property(move |_, _, _, _, _| p.borrow().locale.to_variant())
        .build().unwrap();
    let s = state.clone();
    connection.register_object("/org/freedesktop/systemd1", &interface(r#"<node><interface name="org.freedesktop.systemd1.Manager">
        <method name="UnsetAndSetEnvironment"><arg type="as" direction="in"/><arg type="as" direction="in"/></method>
        </interface></node>"#, "org.freedesktop.systemd1.Manager"))
        .method_call(move |_, _, _, _, method, params, invocation| {
            s.borrow_mut().calls.push(format!("{method}{}{}", interactive(&invocation), params.print(false)));
            invocation.return_value(None);
        }).build().unwrap();
}

fn serve_accounts(connection: &gio::DBusConnection, state: &Rc<RefCell<State>>) {
    request_name(connection, "org.freedesktop.Accounts");
    let s = state.clone();
    connection.register_object("/org/freedesktop/Accounts", &interface(r#"<node><interface name="org.freedesktop.Accounts">
        <method name="FindUserById"><arg type="x" direction="in"/><arg type="o" direction="out"/></method>
        </interface></node>"#, "org.freedesktop.Accounts"))
        .method_call(move |_, _, _, _, method, params, invocation| {
            s.borrow_mut().calls.push(format!("{method}{}{}", interactive(&invocation), params.print(false)));
            invocation.return_value(Some(&(glib::variant::ObjectPath::try_from("/org/freedesktop/Accounts/User1000").unwrap(),).to_variant()));
        }).build().unwrap();
    let s = state.clone();
    let p = state.clone();
    connection.register_object("/org/freedesktop/Accounts/User1000", &interface(r#"<node><interface name="org.freedesktop.Accounts.User">
        <method name="SetLanguage"><arg type="s" direction="in"/></method>
        <property name="Language" type="s" access="read"/></interface></node>"#, "org.freedesktop.Accounts.User"))
        .method_call(move |_, _, _, _, method, params, invocation| {
            let mut state = s.borrow_mut();
            state.calls.push(format!("{method}{}{}", interactive(&invocation), params.print(false)));
            state.language = params.child_value(0).str().unwrap().to_owned();
            invocation.return_value(None);
        })
        .property(move |_, _, _, _, _| p.borrow().language.to_variant())
        .build().unwrap();
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("lm-bus-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn status_binary(address: &str, home: &PathBuf) -> (i32, String, String) {
    let mut child = Command::new(BIN).arg("--status")
        .env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY")
        .env("DBUS_SESSION_BUS_ADDRESS", address).env("DBUS_SYSTEM_BUS_ADDRESS", address)
        .env("HOME", home).env("XDG_CONFIG_HOME", home.join(".config")).env("XDG_RUNTIME_DIR", home)
        .env("DESKTOP_SUITE_LOCALE_DIR", "/nonexistent").env("LANG", "en_US.UTF-8").env_remove("LC_ALL").env_remove("LANGUAGE")
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "--status did not finish");
        pump(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned(),
     String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn real_transport_against_fake_services_on_a_private_bus() {
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    // Both "buses" are the private daemon; the real system bus is never contacted.
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address);
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address);
    let state = Rc::new(RefCell::new(State {
        locale: vec!["LANG=en_US.UTF-8".into(), "LC_TIME=en_GB.UTF-8".into()],
        language: "en_US.UTF-8".into(), calls: Vec::new(),
    }));
    let services_connection = connect(&address);
    serve_locale(&services_connection, &state);

    // 1. No AccountsService: ServiceUnknown means "absent", not an error.
    let home = scratch("status");
    let (code, stdout, stderr) = status_binary(&address, &home);
    assert_eq!(code, 0, "{stderr}");
    let status: serde_json::Value = serde_json::from_str(stdout.trim_end()).unwrap();
    assert_eq!(status["system_locale"], "en_US.UTF-8");
    assert_eq!(status["system_available"], true);
    assert_eq!(status["account_available"], false);
    assert_eq!(status["languages"].as_array().unwrap().len(), 31);
    assert!(stdout.contains("\"name\": \"简体中文\""), "ensure_ascii=False: {stdout}");
    assert!(std::fs::read_dir(&home).unwrap().next().is_none(), "--status must not write");

    let accounts_connection = connect(&address);
    serve_accounts(&accounts_connection, &state);
    let (code, stdout, _) = status_binary(&address, &home);
    assert_eq!(code, 0);
    let status: serde_json::Value = serde_json::from_str(stdout.trim_end()).unwrap();
    assert_eq!(status["account_available"], true);
    assert_eq!(status["language"], "en");
    assert!(state.borrow().calls.iter().all(|c| !c.starts_with("Set")), "status called a setter");

    // 2. A full apply through the real gio transport.
    let home = scratch("apply");
    let backend_home = home.clone();
    let result = off_thread(move || {
        let services: Arc<dyn Services> = Arc::new(LocaleServices::new());
        let mut backend = LanguageBackend::new(Some(backend_home), None, Some(services));
        backend.locale_provider = Box::new(|| Ok(LANGUAGES.iter().map(|l| l.locale.to_owned()).collect()));
        backend.environment = Default::default();
        backend.apply("de", None)
    });
    assert!(result.ok, "{result:?}");
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    let calls = state.borrow().calls.clone();
    let set_locale = calls.iter().find(|c| c.starts_with("SetLocale")).unwrap();
    assert!(set_locale.starts_with("SetLocale+i(["), "{set_locale}");
    assert!(set_locale.contains("'LANGUAGE=de_DE.UTF-8'") && set_locale.ends_with("], true)"), "{set_locale}");
    assert!(calls.contains(&"SetLanguage+i('de_DE.UTF-8',)".to_owned()), "{calls:?}");
    let environment = calls.iter().find(|c| c.starts_with("UnsetAndSetEnvironment")).unwrap();
    assert!(environment.starts_with("UnsetAndSetEnvironment(['LC_ALL'], ['LC_NUMERIC=en_GB.UTF-8'") == false);
    assert!(environment.starts_with("UnsetAndSetEnvironment(['LC_ALL'], ['LC_NUMERIC=en_US.UTF-8', 'LC_TIME=en_GB.UTF-8'"), "{environment}");
    assert!(environment.ends_with("'LANG=de_DE.UTF-8', 'LANGUAGE=de:en', 'LC_MESSAGES=de_DE.UTF-8'])"), "{environment}");
    assert!(calls.iter().filter(|c| c.starts_with("FindUserById")).all(|c| !c.contains("+i")));
    assert_eq!(state.borrow().language, "de_DE.UTF-8");
    assert!(home.join(".config/desktop-suite/language.json").exists());

    // 3. Errors from a missing service carry PyGObject's str(GLib.Error) text.
    drop(services_connection);
    pump(Duration::from_millis(100));
    let error = off_thread(|| LocaleServices::new().get_locale().unwrap_err());
    assert!(error.starts_with("g-dbus-error-quark: GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown:"), "{error}");
    drop(accounts_connection);
    bus.down();
}
