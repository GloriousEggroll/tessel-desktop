//! End-to-end checks over real GDBus on a private `gio::TestDBus` daemon
//! (never the session or system bus): fake `org.bluez` and `org.bluez.obex`
//! services, the manager's discovery/scan/power round trips, an outgoing
//! OBEX push over the sender's own private connection, the incoming-file
//! agent, and `btmanager --selftest` pointed at the fake BlueZ through
//! `DBUS_SYSTEM_BUS_ADDRESS`. No radio is touched.

use bluetooth_manager::backend::*;
use bluetooth_manager::dbus::{props_reply, vardict, Conn, GioConn};
use bluetooth_manager::env::FakeEnv;
use bluetooth_manager::manager::{Manager, Services};
use bluetooth_manager::testing::RecordingView;
use bluetooth_manager::transfer::*;
use gio::prelude::*;
use glib::Variant;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

type Calls = Rc<RefCell<Vec<(String, String, String)>>>;

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address, gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION, None, None::<&gio::Cancellable>).expect("private bus")
}

fn own(conn: &gio::DBusConnection, name: &str) {
    let reply = conn.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                               Some(&(name, 4u32).to_variant()), None, gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>).unwrap();
    assert_eq!(reply.get::<(u32,)>(), Some((1,)), "own {name}");
}

/// Export `xml` at `path`; `handler(method, params) -> Ok(reply) | Err((name, message))`.
fn serve(conn: &gio::DBusConnection, path: &str, xml: &str, calls: &Calls,
         handler: impl Fn(&str, &Variant) -> Result<Option<Variant>, (String, String)> + 'static) {
    let info = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let iface = info.interfaces()[0].clone();
    let calls = calls.clone();
    conn.register_object(path, &iface).method_call(move |_, _sender, path, iface, method, params, invocation| {
        calls.borrow_mut().push((path.to_owned(), iface.unwrap_or("").to_owned(), method.to_owned()));
        match handler(method, &params) {
            Ok(value) => invocation.return_value(value.as_ref()),
            Err((name, message)) => invocation.return_dbus_error(&name, &message),
        }
    }).build().unwrap();
}

const PROPS_XML: &str = r#"<node><interface name="org.freedesktop.DBus.Properties">
<method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
<method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method>
<method name="Set"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/></method>
</interface></node>"#;
const OM_XML: &str = r#"<node><interface name="org.freedesktop.DBus.ObjectManager">
<method name="GetManagedObjects"><arg type="a{oa{sa{sv}}}" direction="out"/></method></interface></node>"#;
const ADAPTER_XML: &str = r#"<node><interface name="org.bluez.Adapter1">
<method name="StartDiscovery"/><method name="StopDiscovery"/><method name="RemoveDevice"><arg type="o" direction="in"/></method>
</interface></node>"#;
const DEVICE_XML: &str = r#"<node><interface name="org.bluez.Device1">
<method name="Connect"/><method name="Disconnect"/><method name="Pair"/></interface></node>"#;
const CLIENT_XML: &str = r#"<node><interface name="org.bluez.obex.Client1">
<method name="CreateSession"><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
<method name="RemoveSession"><arg type="o" direction="in"/></method></interface></node>"#;
const PUSH_XML: &str = r#"<node><interface name="org.bluez.obex.ObjectPush1">
<method name="SendFile"><arg type="s" direction="in"/><arg type="o" direction="out"/><arg type="a{sv}" direction="out"/></method>
</interface></node>"#;
const AGENT_MANAGER_XML: &str = r#"<node><interface name="org.bluez.obex.AgentManager1">
<method name="RegisterAgent"><arg type="o" direction="in"/></method><method name="UnregisterAgent"><arg type="o" direction="in"/></method>
</interface></node>"#;

const HCI0: &str = "/org/bluez/hci0";
const DEV: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";

fn path(p: &str) -> Variant {
    bluetooth_manager::dbus::object_path(p)
}

fn adapter_props(powered: bool) -> Variant {
    vardict(&[("Powered", powered.to_variant()), ("Discovering", false.to_variant()), ("Alias", "Fake adapter".to_variant()),
              ("Address", "00:11:22:33:44:55".to_variant())])
}

fn managed(powered: bool) -> Variant {
    let dev = vardict(&[("Address", "AA:BB:CC:DD:EE:FF".to_variant()), ("Alias", "Headphones".to_variant()), ("Adapter", path(HCI0)),
                        ("Paired", true.to_variant()), ("Connected", false.to_variant()), ("UUIDs", vec![OBJECT_PUSH_UUID].to_variant())]);
    let entry = |p: &str, iface: &str, props: Variant| {
        Variant::from_dict_entry(&path(p), &Variant::array_from_iter_with_type(
            glib::VariantTy::new("{sa{sv}}").unwrap(), [Variant::from_dict_entry(&iface.to_variant(), &props)]))
    };
    Variant::tuple_from_iter([Variant::array_from_iter_with_type(glib::VariantTy::new("{oa{sa{sv}}}").unwrap(),
        [entry(HCI0, ADAPTER_IFACE, adapter_props(powered)), entry(DEV, DEVICE_IFACE, dev)])])
}

fn wait(context: &glib::MainContext, what: &str, predicate: impl FnMut() -> bool) {
    assert!(suite_core::timer::wait_for(context, Duration::from_secs(10), predicate), "timed out waiting for {what}");
}

fn emit_props(conn: &gio::DBusConnection, object: &str, iface: &str, props: &[(&str, Variant)]) {
    conn.emit_signal(None, object, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                     Some(&Variant::tuple_from_iter([iface.to_variant(), vardict(props), Vec::<String>::new().to_variant()]))).unwrap();
}

/// Fake BlueZ: ObjectManager, Adapter1 discovery, Properties.Set, Device1.
fn bluez_round_trips(context: &glib::MainContext, address: &str) -> gio::DBusConnection {
    let server = connect(address);
    let calls: Calls = Rc::default();
    let powered = Rc::new(Cell::new(true));
    let p = powered.clone();
    serve(&server, BLUEZ_ROOT, OM_XML, &calls, move |_, _| Ok(Some(managed(p.get()))));
    serve(&server, HCI0, ADAPTER_XML, &calls, |_, _| Ok(None));
    let (p, srv) = (powered.clone(), server.clone());
    serve(&server, HCI0, PROPS_XML, &calls, move |method, params| {
        if method == "Set" {
            let value = params.child_value(2).as_variant().unwrap().get::<bool>().unwrap();
            p.set(value);
            emit_props(&srv, HCI0, ADAPTER_IFACE, &[("Powered", value.to_variant())]);
            Ok(None)
        } else {
            Ok(Some(Variant::tuple_from_iter([adapter_props(p.get())])))
        }
    });
    serve(&server, DEV, DEVICE_XML, &calls, |method, _| match method {
        "Pair" => Err(("org.bluez.Error.AuthenticationFailed".into(), "Authentication Failed".into())),
        _ => Ok(None),
    });
    own(&server, BLUEZ);

    let bt = Bt::new(GioConn::new(connect(address)));
    let env = FakeEnv::new();
    let services = Services { load_receive_mode: Rc::new(|| "ask".into()), monitor_config: false, ..Services::default() };
    let manager = Manager::new(bt.clone(), env.clone(), false, "en", services);
    manager.set_view(Rc::new(RecordingView::default()));
    manager.start();
    wait(context, "ObjectManager discovery", || bt.has_object_manager.get() && bt.devices.borrow().len() == 1);
    env.drain();
    assert_eq!(manager.default_adapter().as_deref(), Some(HCI0));
    assert_eq!(bt.device(DEV).unwrap().alias, "Headphones");
    // PropertiesChanged from the service reaches the model.
    emit_props(&server, DEV, DEVICE_IFACE, &[("Connected", true.to_variant())]);
    wait(context, "Connected signal", || bt.device(DEV).is_some_and(|d| d.connected));
    assert_eq!(manager.event_text(), "Connected to Headphones");
    // Our own discovery session, released on close.
    manager.add_window_opened();
    wait(context, "StartDiscovery", || manager.scan_active());
    manager.add_window_destroyed();
    wait(context, "StopDiscovery", || !manager.scan_active() && !manager.scan_pending());
    // Properties.Set + the service's signal.
    manager.power_toggled(false);
    wait(context, "power off", || !bt.powered(Some(HCI0)) && manager.power_pending(HCI0).is_none());
    assert!(!powered.get());
    assert_eq!(manager.event_text(), "Bluetooth is off");
    manager.power_toggled(true);
    wait(context, "power on", || bt.powered(Some(HCI0)) && manager.power_pending(HCI0).is_none());
    env.drain();
    // A real remote error is translated into advice with details.
    bt.devices.borrow_mut().get_mut(DEV).unwrap().paired = false;
    manager.connect_clicked(DEV, &bt.device(DEV).unwrap());
    wait(context, "pair failure", || manager.event_text().contains("check the PIN"));
    let methods: Vec<String> = calls.borrow().iter().map(|c| c.2.clone()).collect();
    assert_eq!(methods, ["GetManagedObjects", "StartDiscovery", "StopDiscovery", "Set", "Set", "Pair"]);
    println!("ok bluez round trips on the private bus");
    server
}

/// Fake obexd: client sessions, object push and the agent manager.
fn obex_round_trips(context: &glib::MainContext, address: &str) {
    let server = connect(address);
    let calls: Calls = Rc::default();
    let session = "/org/bluez/obex/client/session7";
    let transfer = "/org/bluez/obex/client/session7/transfer1";
    serve(&server, ROOT, CLIENT_XML, &calls, move |method, params| match method {
        "CreateSession" => {
            assert_eq!(params.child_value(0).str(), Some("AA:BB:CC:DD:EE:FF"));
            let args = props_of(&params.child_value(1));
            assert_eq!((args["Target"].as_str(), args["Source"].as_str()), (Some("opp"), Some("00:11:22:33:44:55")));
            Ok(Some(Variant::tuple_from_iter([path(session)])))
        }
        _ => Ok(None),
    });
    let srv = server.clone();
    serve(&server, session, PUSH_XML, &calls, move |_, params| {
        assert!(params.child_value(0).str().unwrap().ends_with("hello.txt"));
        let srv = srv.clone();
        bluetooth_manager::env::idle_once(move || {
            emit_props(&srv, transfer, TRANSFER, &[("Transferred", 3u64.to_variant())]);
            emit_props(&srv, transfer, TRANSFER, &[("Status", "complete".to_variant()), ("Transferred", 5u64.to_variant())]);
        });
        Ok(Some(Variant::tuple_from_iter([path(transfer), vardict(&[("Status", "queued".to_variant()), ("Size", 5u64.to_variant())])])))
    });
    let agents: Rc<RefCell<Vec<String>>> = Rc::default();
    let a = agents.clone();
    serve(&server, ROOT, AGENT_MANAGER_XML, &calls, move |method, params| {
        a.borrow_mut().push(format!("{method} {}", params.child_value(0).str().unwrap()));
        Ok(None)
    });
    let incoming = "/org/bluez/obex/server/session3/transfer4";
    serve(&server, incoming, PROPS_XML, &calls, |_, _| Ok(Some(props_reply(&[
        ("Name", "../photo.jpg".to_variant()), ("Size", 4u64.to_variant()), ("Session", path("/org/bluez/obex/server/session3"))]))));
    serve(&server, "/org/bluez/obex/server/session3", PROPS_XML, &calls,
          |_, _| Ok(Some(props_reply(&[("Destination", "AA:BB:CC:DD:EE:FF".to_variant())]))));
    own(&server, OBEX);

    // Outgoing push over the sender's own private connection (default connector).
    let dir = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir)
        .join(format!("btmanager-private-bus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("hello.txt");
    std::fs::write(&file, "hello").unwrap();
    let progress: Rc<RefCell<Vec<Progress>>> = Rc::default();
    type Outcome = Rc<RefCell<Option<(Option<bluetooth_manager::dbus::BtError>, bool)>>>;
    let result: Outcome = Rc::default();
    let (pr, r) = (progress.clone(), result.clone());
    let sender = ObexSender::new("00:11:22:33:44:55", "AA:BB:CC:DD:EE:FF", &[file.to_string_lossy().into_owned()],
                                 Some(Rc::new(move |p| pr.borrow_mut().push(p.clone()))),
                                 Some(Box::new(move |e, c| { r.replace(Some((e, c))); })), None, None);
    sender.start();
    let ok = suite_core::timer::wait_for(context, Duration::from_secs(5), || result.borrow().is_some());
    if !ok {
        panic!("push did not complete: calls={:?} progress={:?} env={:?}", calls.borrow(), progress.borrow(),
               std::env::var("DBUS_SESSION_BUS_ADDRESS"));
    }
    assert_eq!(result.borrow().clone().unwrap(), (None, false));
    assert!(progress.borrow().iter().any(|p| p.status == "connecting"));
    assert!(progress.borrow().iter().any(|p| p.transferred == 3 && p.total == 5));
    let methods: Vec<String> = calls.borrow().iter().map(|c| c.2.clone()).collect();
    assert_eq!(methods, ["CreateSession", "SendFile", "RemoveSession"]);

    // Incoming push: register, AuthorizePush from the obexd connection, accept.
    let agent_bus = connect(address);
    let requests: Rc<RefCell<Vec<Rc<ReceiveRequest>>>> = Rc::default();
    let done: Rc<RefCell<Vec<String>>> = Rc::default();
    let status: Rc<RefCell<Vec<bool>>> = Rc::default();
    let (rq, dn, st) = (requests.clone(), done.clone(), status.clone());
    let receiver = ObexReceiver::new(ReceiverCallbacks {
        on_request: Some(Rc::new(move |r| rq.borrow_mut().push(r.clone()))),
        on_done: Some(Rc::new(move |r| dn.borrow_mut().push(r.state()))),
        on_status: Some(Rc::new(move |registered, _| st.borrow_mut().push(registered))),
        ..Default::default()
    }, Some(GioConn::new(agent_bus.clone()) as Rc<dyn Conn>), None);
    receiver.start();
    wait(context, "agent registration", || status.borrow().contains(&true));
    assert_eq!(*agents.borrow(), vec![format!("RegisterAgent {AGENT_PATH}")]);
    let target = agent_bus.unique_name().map(|n| n.to_string());
    // A foreign client is rejected.
    let stranger = connect(address);
    let refused: Rc<RefCell<Option<Result<Variant, glib::Error>>>> = Rc::default();
    let rf = refused.clone();
    stranger.call(target.as_deref(), AGENT_PATH, AGENT, "AuthorizePush", Some(&Variant::tuple_from_iter([path(incoming)])), None,
                  gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>, move |r| { rf.replace(Some(r)); });
    wait(context, "foreign rejection", || refused.borrow().is_some());
    let error = refused.borrow_mut().take().unwrap().unwrap_err();
    assert_eq!(gio::DBusError::remote_error(&error).as_deref(), Some(REJECTED));
    let reply: Rc<RefCell<Option<Result<Variant, glib::Error>>>> = Rc::default();
    let rp = reply.clone();
    server.call(target.as_deref(), AGENT_PATH, AGENT, "AuthorizePush", Some(&Variant::tuple_from_iter([path(incoming)])), None,
                gio::DBusCallFlags::NONE, 10000, None::<&gio::Cancellable>, move |r| { rp.replace(Some(r)); });
    wait(context, "incoming request", || !requests.borrow().is_empty());
    let request = requests.borrow()[0].clone();
    assert_eq!((request.name.borrow().as_str(), request.address.borrow().as_str()), ("photo.jpg", "AA:BB:CC:DD:EE:FF"));
    let saved = request.accept(&dir).unwrap();
    wait(context, "AuthorizePush reply", || reply.borrow().is_some());
    let answer = reply.borrow_mut().take().unwrap().unwrap();
    assert_eq!(answer.child_value(0).str(), Some(saved.to_str().unwrap()));
    emit_props(&server, incoming, TRANSFER, &[("Status", "complete".to_variant())]);
    wait(context, "incoming completion", || !done.borrow().is_empty());
    assert_eq!(*done.borrow(), vec!["complete"]);
    receiver.stop();
    wait(context, "unregister", || agents.borrow().len() == 2);
    assert_eq!(agents.borrow()[1], format!("UnregisterAgent {AGENT_PATH}"));
    let _ = std::fs::remove_dir_all(dir);
    println!("ok obex push, agent registration and incoming push on the private bus");
}

/// `--selftest` against the fake BlueZ (as the "system" bus), run while
/// this thread's context keeps serving it.
fn selftest_binary(context: &glib::MainContext, address: &str) {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_btmanager")).arg("--selftest")
        .env("DBUS_SYSTEM_BUS_ADDRESS", address).env("DBUS_SESSION_BUS_ADDRESS", address)
        .stdout(std::process::Stdio::piped()).spawn().unwrap();
    wait(context, "selftest exit", || child.try_wait().unwrap().is_some());
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{text}");
    assert!(text.starts_with("PASS system bus\nPASS discovery (object manager)\nPASS adapters found — 1 adapter(s)\nPASS devices listed — 1 device(s)\n"), "{text}");
    assert!(text.contains("PASS default adapter — Fake adapter powered=True discovering=False\n"), "{text}");
    assert!(text.contains("PASS i18n de — translated='"), "{text}");
    assert_eq!(text.matches("PASS i18n ").count(), 31);
    assert!(text.ends_with("\nselftest PASSED\n"));
    println!("ok --selftest against the fake BlueZ");
}

#[test]
fn private_bus_end_to_end() {
    let context = glib::MainContext::new();
    context.with_thread_default(|| {
        // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
        // and in spawned children must not look up portals or gvfs.
        for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
            std::env::set_var(key, value);
        }
        let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
        bus.up();
        let address = bus.bus_address().unwrap().to_string();
        let _bluez = bluez_round_trips(&context, &address);
        obex_round_trips(&context, &address);
        selftest_binary(&context, &address);
        bus.down();
    }).unwrap();
}

#[test]
fn launcher_validates_arguments_and_generates_catalogs() {
    let bin = env!("CARGO_BIN_EXE_btmanager");
    let run = |args: &[&str]| std::process::Command::new(bin).args(args).env("COLUMNS", "80")
        .env_remove("DBUS_SESSION_BUS_ADDRESS").env("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/nonexistent").output().unwrap();
    let bad = run(&["--popup-offset", "-1"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).ends_with("btmanager: error: --popup-offset must be a nonnegative number\n"));
    let help = run(&["-h"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("Bluetooth device manager"));
    let dir = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir)
        .join(format!("btmanager-gen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pot = dir.join("messages.po");
    let out = run(&["--gen-pot", pot.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("wrote {} (103 msgids)\n", pot.display()));
    let text = std::fs::read_to_string(&pot).unwrap();
    assert!(text.contains("msgid \"Bluetooth\"\nmsgstr \"Bluetooth\"\n"));
    let po = dir.join("po");
    let out = run(&["--gen-mo", po.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("wrote 31 catalogs under {}/\n", po.display()));
    assert!(po.join("fr/messages.mo").is_file() && po.join("zh_TW/messages.po").is_file());
    let _ = std::fs::remove_dir_all(dir);
}
