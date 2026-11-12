//! W-c D-Bus tests on a PRIVATE bus (gio::TestDBus; also exported as the
//! "system" bus): StatusNotifier watcher/host (test_status_tray BusTests),
//! DBusMenu model (TrayUITests, model side), org.tessel.Dock.Settings
//! (test_settings_api / test_workareas bus tests), capture status, readiness
//! and the real StatusBackend services. Never touches the desktop session.

use gio::prelude::*;
use glib::Variant;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_dock::core::config::DockConfig;
use tessel_dock::dbus::dbusmenu::RemoteMenu;
use tessel_dock::dbus::readiness::{ServiceReadiness, TrayReadiness};
use tessel_dock::dbus::settings_api::{DockInfo, MonitorInfo, SettingsApi, SettingsHost, BUS_NAME, INTERFACE, OBJECT_PATH};
use tessel_dock::dbus::sni::{Action, StatusTrayBackend, ITEM, WATCHER, WATCHER_PATH};

fn spin_until(predicate: impl Fn() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

fn settle(seconds: f64) { spin_until(|| false, seconds); }

fn connection(address: &str) -> gio::DBusConnection {
    let connection = gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None::<&gio::DBusAuthObserver>, None::<&gio::Cancellable>).unwrap();
    connection.set_exit_on_close(false);
    connection
}

fn unique(connection: &gio::DBusConnection) -> String { connection.unique_name().unwrap().to_string() }

/// Async call + main-loop spin (the API side runs on this thread).
fn call(connection: &gio::DBusConnection, name: &str, path: &str, interface: &str, method: &str, params: Option<Variant>) -> Result<Variant, glib::Error> {
    let slot: Rc<RefCell<Option<Result<Variant, glib::Error>>>> = Rc::default();
    let result = slot.clone();
    connection.call(Some(name), path, interface, method, params.as_ref(), None, gio::DBusCallFlags::NO_AUTO_START, 3000,
                    None::<&gio::Cancellable>, move |reply| { result.replace(Some(reply)); });
    assert!(spin_until(|| slot.borrow().is_some(), 5.0), "{method} did not reply");
    slot.take().unwrap()
}

fn request_name(connection: &gio::DBusConnection, name: &str) {
    let reply = call(connection, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                     Some((name, 0u32).to_variant())).unwrap();
    assert_eq!(reply.get::<(u32,)>(), Some((1,)), "{name} not acquired");
}

fn release_name(connection: &gio::DBusConnection, name: &str) {
    call(connection, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
         Some((name,).to_variant())).unwrap();
}

macro_rules! check {
    ($cond:expr) => { assert!($cond, "{}:{}: {}", file!(), line!(), stringify!($cond)) };
}

// ---------------------------------------------------------------------------
// StatusNotifier

const ITEM_XML: &str = r#"<node><interface name="org.kde.StatusNotifierItem">
<property name="Id" type="s" access="read"/>
<property name="Title" type="s" access="read"/>
<property name="Status" type="s" access="read"/>
<property name="IconName" type="s" access="read"/>
<property name="Menu" type="o" access="read"/>
<property name="ItemIsMenu" type="b" access="read"/>
<method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
<signal name="NewIcon"/><signal name="NewStatus"><arg type="s"/></signal>
</interface></node>"#;

/// One synthetic tray application on its own connection.
struct FakeItem {
    connection: gio::DBusConnection,
    path: String,
    properties: Rc<RefCell<HashMap<String, Variant>>>,
    calls: Rc<RefCell<Vec<(String, Variant)>>>,
}

impl FakeItem {
    fn new(address: &str) -> Self {
        let connection = connection(address);
        let mut initial = HashMap::new();
        initial.insert("Id".to_owned(), "test-app".to_variant());
        initial.insert("Title".to_owned(), "Test application".to_variant());
        initial.insert("Status".to_owned(), "Active".to_variant());
        initial.insert("IconName".to_owned(), "network-wired".to_variant());
        initial.insert("Menu".to_owned(), glib::variant::ObjectPath::try_from("/Menu".to_owned()).unwrap().to_variant());
        initial.insert("ItemIsMenu".to_owned(), false.to_variant());
        let properties = Rc::new(RefCell::new(initial));
        let calls: Rc<RefCell<Vec<(String, Variant)>>> = Rc::default();
        let info = gio::DBusNodeInfo::for_xml(ITEM_XML).unwrap().lookup_interface(ITEM).unwrap();
        let (recorded, props) = (calls.clone(), properties.clone());
        connection.register_object("/test/Tray", &info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                recorded.borrow_mut().push((method.to_owned(), params));
                invocation.return_value(Some(&().to_variant()));
            })
            .property(move |_, _, _, _, name| props.borrow()[name].clone())
            .build().unwrap();
        Self { connection, path: "/test/Tray".into(), properties, calls }
    }

    fn key(&self) -> String { format!("{}{}", unique(&self.connection), self.path) }

    fn register(&self) {
        call(&self.connection, WATCHER, WATCHER_PATH, WATCHER, "RegisterStatusNotifierItem", Some((self.path.as_str(),).to_variant())).unwrap();
    }

    fn set(&self, name: &str, value: Variant) { self.properties.borrow_mut().insert(name.into(), value); }

    fn emit(&self, signal: &str, params: Option<Variant>) {
        self.connection.emit_signal(None, &self.path, ITEM, signal, params.as_ref()).unwrap();
    }

    fn ready_in(&self, backend: &StatusTrayBackend) -> bool { backend.item(&self.key()).is_some_and(|item| item.ready()) }

    fn close(&self) { let _ = self.connection.close_sync(None::<&gio::Cancellable>); }
}

fn backend(address: &str) -> StatusTrayBackend {
    let backend = StatusTrayBackend::new(None, Some(connection(address)));
    backend.start();
    backend
}

fn sni_discovery_actions_dynamic_icons_and_owner_exit(address: &str) {
    let item = FakeItem::new(address);
    let backend = backend(address);
    check!(spin_until(|| backend.is_watcher(), 3.0));
    check!(backend.host_registered());
    check!(backend.watcher_owner() == Some(unique(&backend.connection().unwrap())));
    item.register();
    check!(spin_until(|| item.ready_in(&backend), 3.0));
    let tray_item = backend.item(&item.key()).unwrap();
    check!(tray_item.string("Title").as_deref() == Some("Test application"));
    check!(tray_item.tooltip_text() == "Test application");
    check!(tray_item.menu_path().as_deref() == Some("/Menu"));
    // Watcher properties reflect the registration.
    let reply = call(&item.connection, WATCHER, WATCHER_PATH, "org.freedesktop.DBus.Properties", "GetAll", Some((WATCHER,).to_variant())).unwrap();
    let all = tessel_core::values::first(&reply);
    check!(all["RegisteredStatusNotifierItems"] == json!([item.key()]));
    check!(all["IsStatusNotifierHostRegistered"] == json!(true) && all["ProtocolVersion"] == json!(0));
    check!(backend.action(&tray_item, Action::Activate { x: 101, y: 203 }, None));
    check!(backend.action(&tray_item, Action::SecondaryActivate { x: 101, y: 203 }, None));
    check!(backend.action(&tray_item, Action::ContextMenu { x: 101, y: 203 }, None));
    backend.action(&tray_item, Action::Scroll { delta: -120, orientation: "horizontal".into() }, None);
    check!(spin_until(|| item.calls.borrow().len() == 4, 3.0));
    let calls: Vec<(String, String)> = item.calls.borrow().iter().map(|(m, p)| (m.clone(), p.print(false).to_string())).collect();
    check!(calls == vec![("Activate".into(), "(101, 203)".into()), ("SecondaryActivate".into(), "(101, 203)".into()),
                         ("ContextMenu".into(), "(101, 203)".into()), ("Scroll".into(), "(-120, 'horizontal')".into())]);
    let serial = tray_item.icon_serial();
    item.set("IconName", "network-offline".to_variant());
    item.emit("NewIcon", None);
    check!(spin_until(|| tray_item.string("IconName").as_deref() == Some("network-offline"), 3.0));
    check!(tray_item.icon_serial() == serial + 1);
    item.register();
    check!(backend.items().len() == 1);
    item.close();
    check!(spin_until(|| backend.items().is_empty(), 3.0));
    check!(!backend.action(&tray_item, Action::Activate { x: 0, y: 0 }, None));
    backend.close();
    println!("ok sni_discovery_actions_dynamic_icons_and_owner_exit");
}

fn sni_existing_watcher_is_not_replaced_and_handoff_keeps_legacy_items(address: &str) {
    let item = FakeItem::new(address);
    let external = backend(address);
    check!(spin_until(|| external.is_watcher(), 3.0));
    item.register();
    check!(spin_until(|| item.ready_in(&external), 3.0));
    let dock = backend(address);
    check!(spin_until(|| item.ready_in(&dock), 3.0));
    check!(spin_until(|| dock.host_registered(), 3.0));
    check!(external.is_watcher());
    check!(!dock.is_watcher());
    check!(dock.watcher_owner() == Some(unique(&external.connection().unwrap())));
    external.close();
    let _ = external.connection().unwrap().close_sync(None::<&gio::Cancellable>);
    check!(spin_until(|| dock.is_watcher(), 3.0));
    check!(dock.host_registered());
    check!(item.ready_in(&dock));
    dock.action(&dock.item(&item.key()).unwrap(), Action::Activate { x: 9, y: 7 }, None);
    check!(spin_until(|| !item.calls.borrow().is_empty(), 3.0));
    check!(item.calls.borrow()[0].0 == "Activate" && item.calls.borrow()[0].1.print(false).as_str() == "(9, 7)");
    // A later panel sees the item through the new watcher's properties even
    // though this synthetic old app never re-registered after the handoff.
    let third = backend(address);
    check!(spin_until(|| item.ready_in(&third), 3.0));
    item.close();
    check!(spin_until(|| dock.items().is_empty() && third.items().is_empty(), 3.0));
    third.close();
    dock.close();
    println!("ok sni_existing_watcher_is_not_replaced_and_handoff_keeps_legacy_items");
}

const FAKE_WATCHER_XML: &str = r#"<node><interface name="org.kde.StatusNotifierWatcher">
<method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
<method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
<property name="RegisteredStatusNotifierItems" type="as" access="read"/>
<property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
<property name="ProtocolVersion" type="i" access="read"/>
<signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
<signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
</interface></node>"#;

/// A foreign panel's watcher (not a StatusTrayBackend): optionally rejects hosts.
struct FakeWatcher {
    connection: gio::DBusConnection,
    hosts: Rc<RefCell<Vec<String>>>,
    items: Rc<RefCell<Vec<String>>>,
}

impl FakeWatcher {
    fn new(address: &str, reject_hosts: bool, items: Vec<String>) -> Self {
        let connection = connection(address);
        let hosts: Rc<RefCell<Vec<String>>> = Rc::default();
        let items = Rc::new(RefCell::new(items));
        let info = gio::DBusNodeInfo::for_xml(FAKE_WATCHER_XML).unwrap().lookup_interface(WATCHER).unwrap();
        let (recorded, listed) = (hosts.clone(), items.clone());
        connection.register_object(WATCHER_PATH, &info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                let value = params.child_value(0).get::<String>().unwrap();
                if method == "RegisterStatusNotifierHost" {
                    recorded.borrow_mut().push(value);
                    if reject_hosts {
                        invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "Synthetic rejection");
                        return;
                    }
                }
                invocation.return_value(Some(&().to_variant()));
            })
            .property(move |_, _, _, _, name| match name {
                "RegisteredStatusNotifierItems" => listed.borrow().clone().to_variant(),
                "IsStatusNotifierHostRegistered" => true.to_variant(),
                _ => 0i32.to_variant(),
            })
            .build().unwrap();
        request_name(&connection, WATCHER);
        Self { connection, hosts, items }
    }

    fn close(&self) { let _ = self.connection.close_sync(None::<&gio::Cancellable>); }
}

fn sni_foreign_watcher_is_never_replaced_and_its_items_and_signals_are_followed(address: &str) {
    let item = FakeItem::new(address);
    let watcher = FakeWatcher::new(address, false, vec![item.key()]);
    let dock = backend(address);
    check!(spin_until(|| dock.host_registered(), 3.0));
    check!(!dock.is_watcher());
    check!(*watcher.hosts.borrow() == vec![unique(&dock.connection().unwrap())]);
    check!(dock.watcher_owner() == Some(unique(&watcher.connection)));
    check!(spin_until(|| item.ready_in(&dock), 3.0));
    // The dock's queued request never took the name.
    let owner = call(&watcher.connection, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner",
                     Some((WATCHER,).to_variant())).unwrap();
    check!(owner.get::<(String,)>().unwrap().0 == unique(&watcher.connection));
    // Unregistered from the real owner is followed; from anybody else ignored.
    item.connection.emit_signal(None, WATCHER_PATH, WATCHER, "StatusNotifierItemUnregistered", Some(&(item.key(),).to_variant())).unwrap();
    settle(0.1);
    check!(dock.item(&item.key()).is_some());
    watcher.connection.emit_signal(None, WATCHER_PATH, WATCHER, "StatusNotifierItemUnregistered", Some(&(item.key(),).to_variant())).unwrap();
    check!(spin_until(|| dock.items().is_empty(), 3.0));
    watcher.connection.emit_signal(None, WATCHER_PATH, WATCHER, "StatusNotifierItemRegistered", Some(&(item.key(),).to_variant())).unwrap();
    check!(spin_until(|| item.ready_in(&dock), 3.0));
    watcher.items.borrow_mut().clear();
    // The foreign watcher exits: the queued name is granted, items retained.
    watcher.close();
    check!(spin_until(|| dock.is_watcher() && dock.host_registered(), 3.0));
    check!(item.ready_in(&dock));
    item.close();
    dock.close();
    println!("ok sni_foreign_watcher_is_never_replaced_and_its_items_and_signals_are_followed");
}

fn sni_rejected_host_registration_does_not_report_external_host_ready(address: &str) {
    let watcher = FakeWatcher::new(address, true, vec![]);
    let dock = backend(address);
    check!(spin_until(|| dock.items_received() && !watcher.hosts.borrow().is_empty(), 3.0));
    check!(*watcher.hosts.borrow() == vec![unique(&dock.connection().unwrap())]);
    settle(0.1);
    check!(!dock.host_registered());
    let readiness = ServiceReadiness::new();
    check!(!readiness.update_with(None, true, Some(&dock as &dyn TrayReadiness)).unwrap());
    watcher.close();
    check!(spin_until(|| dock.is_watcher() && dock.host_registered(), 3.0));
    check!(readiness.update_with(None, true, Some(&dock as &dyn TrayReadiness)).unwrap());
    dock.close();
    println!("ok sni_rejected_host_registration_does_not_report_external_host_ready");
}

fn sni_passive_and_attention_status_updates_are_published(address: &str) {
    let item = FakeItem::new(address);
    let backend = backend(address);
    check!(spin_until(|| backend.is_watcher(), 3.0));
    item.register();
    check!(spin_until(|| item.ready_in(&backend), 3.0));
    let changes = Rc::new(Cell::new(0));
    let counter = changes.clone();
    let id = backend.subscribe(Rc::new(move || counter.set(counter.get() + 1)));
    for status in ["Passive", "NeedsAttention", "Active"] {
        item.set("Status", status.to_variant());
        item.emit("NewStatus", Some((status,).to_variant()));
        check!(spin_until(|| backend.item(&item.key()).unwrap().status() == status, 3.0));
    }
    check!(changes.get() >= 3);
    backend.unsubscribe(id);
    item.close();
    backend.close();
    println!("ok sni_passive_and_attention_status_updates_are_published");
}

// ---------------------------------------------------------------------------
// DBusMenu

const MENU_XML: &str = r#"<node><interface name="com.canonical.dbusmenu">
<method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/>
<arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
<method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
<method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/>
<arg type="v" direction="in"/><arg type="u" direction="in"/></method>
</interface></node>"#;

#[derive(Clone)]
struct RowOwned(i32, Vec<(&'static str, Variant)>, Vec<RowOwned>);

fn node(row: &RowOwned) -> Variant {
    let props = glib::VariantDict::new(None);
    for (key, value) in &row.1 {
        props.insert_value(key, value);
    }
    let children: Vec<Variant> = row.2.iter().map(|child| Variant::from_variant(&node(child))).collect();
    Variant::tuple_from_iter([row.0.to_variant(), props.end(), Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, children)])
}

struct FakeMenu {
    connection: gio::DBusConnection,
    rows: Rc<RefCell<Vec<RowOwned>>>,
    events: Rc<RefCell<Vec<(String, String)>>>,
    pending: Rc<RefCell<Vec<gio::DBusMethodInvocation>>>,
    delayed: Rc<Cell<bool>>,
}

impl FakeMenu {
    fn new(address: &str) -> Self {
        let connection = connection(address);
        let rows: Rc<RefCell<Vec<RowOwned>>> = Rc::new(RefCell::new(vec![RowOwned(1, vec![("label", "Test action".to_variant()),
            ("enabled", true.to_variant()), ("visible", true.to_variant()), ("shortcut", vec![vec!["Control".to_owned(), "q".to_owned()]].to_variant())], vec![])]));
        let events: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
        let pending: Rc<RefCell<Vec<gio::DBusMethodInvocation>>> = Rc::default();
        let delayed = Rc::new(Cell::new(false));
        let info = gio::DBusNodeInfo::for_xml(MENU_XML).unwrap().lookup_interface("com.canonical.dbusmenu").unwrap();
        for path in ["/Menu", "/ReplacementMenu"] {
            let (rows, events, pending, delayed) = (rows.clone(), events.clone(), pending.clone(), delayed.clone());
            connection.register_object(path, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    if method == "GetLayout" {
                        if delayed.get() {
                            pending.borrow_mut().push(invocation);
                        } else {
                            Self::reply_rows(&rows.borrow(), invocation);
                        }
                        return;
                    }
                    events.borrow_mut().push((method.to_owned(), params.print(false).to_string()));
                    if method == "AboutToShow" {
                        invocation.return_value(Some(&(false,).to_variant()));
                    } else {
                        invocation.return_value(Some(&().to_variant()));
                    }
                })
                .build().unwrap();
        }
        Self { connection, rows, events, pending, delayed }
    }

    fn reply_rows(rows: &[RowOwned], invocation: gio::DBusMethodInvocation) {
        let root = node(&RowOwned(0, vec![], rows.to_vec()));
        invocation.return_value(Some(&Variant::tuple_from_iter([1u32.to_variant(), root])));
    }

    fn reply_pending(&self, index: usize) {
        let invocation = self.pending.borrow_mut().remove(index);
        Self::reply_rows(&self.rows.borrow(), invocation);
    }

    fn clicked(&self, ident: i32) -> usize {
        self.events.borrow().iter().filter(|(m, p)| m == "Event" && p.starts_with(&format!("({ident}, 'clicked'"))).count()
    }
}

fn row(id: i32, props: Vec<(&'static str, Variant)>, children: Vec<RowOwned>) -> RowOwned { RowOwned(id, props, children) }

fn dbusmenu_check_radio_disabled_hidden_submenus_and_live_updates(address: &str) {
    let fake = FakeMenu::new(address);
    let client = connection(address);
    let name = unique(&fake.connection);
    *fake.rows.borrow_mut() = vec![
        row(1, vec![("label", "Checked".to_variant()), ("toggle-type", "checkmark".to_variant()), ("toggle-state", 1i32.to_variant())], vec![]),
        row(2, vec![("label", "Radio".to_variant()), ("toggle-type", "radio".to_variant()), ("toggle-state", 1i32.to_variant())], vec![]),
        row(3, vec![("label", "Disabled".to_variant()), ("enabled", false.to_variant())], vec![]),
        row(4, vec![("label", "Hidden".to_variant()), ("visible", false.to_variant())], vec![]),
        row(7, vec![("type", "separator".to_variant())], vec![]),
        row(5, vec![("label", "More".to_variant()), ("children-display", "submenu".to_variant())], vec![row(6, vec![("label", "Nested".to_variant())], vec![])]),
        row(-8, vec![("label", "Negative".to_variant()), ("icon-name", "edit-copy".to_variant())], vec![]),
    ];
    let menu = RemoteMenu::new(&client, &name, "/Menu");
    let renders = Rc::new(Cell::new(0));
    let counter = renders.clone();
    menu.set_on_render(Some(Rc::new(move || counter.set(counter.get() + 1))));
    check!(spin_until(|| menu.layout().is_some(), 3.0));
    let actions = menu.actions();
    check!(actions.lookup_action("item1").unwrap().state().unwrap().get::<bool>() == Some(true));
    check!(actions.lookup_action("item2").unwrap().state().unwrap().get::<String>().as_deref() == Some("2"));
    check!(!actions.lookup_action("item3").unwrap().is_enabled());
    check!(actions.lookup_action("item4").is_none());
    check!(actions.lookup_action("item6").is_some());
    check!(actions.lookup_action("itemn8").is_some());
    // Model: section 1 = Checked/Radio/Disabled, section 2 = More (submenu) + Negative.
    let model = menu.model();
    check!(model.n_items() == 2);
    let first = model.item_link(0, "section").unwrap();
    let second = model.item_link(1, "section").unwrap();
    check!(first.n_items() == 3 && second.n_items() == 2);
    check!(second.item_attribute_value(0, "submenu-action", None).unwrap().get::<String>().as_deref() == Some("remote.submenu5"));
    check!(second.item_link(0, "submenu").is_some());
    check!(first.item_attribute_value(1, "target", None).unwrap().get::<String>().as_deref() == Some("2"));
    check!(second.item_attribute_value(1, "icon", None).is_some());
    actions.change_action_state("submenu5", &true.to_variant());
    check!(spin_until(|| fake.events.borrow().iter().any(|(m, p)| m == "AboutToShow" && p == "(5,)"), 3.0));
    check!(spin_until(|| fake.events.borrow().iter().any(|(m, p)| m == "Event" && p.starts_with("(5, 'opened'")), 3.0));
    actions.activate_action("item6", None);
    check!(spin_until(|| fake.clicked(6) == 1, 3.0));
    let before = renders.get();
    fake.rows.borrow_mut()[0].1[2] = ("toggle-state", 0i32.to_variant());
    fake.connection.emit_signal(None, "/Menu", "com.canonical.dbusmenu", "LayoutUpdated", Some(&(2u32, 0i32).to_variant())).unwrap();
    check!(spin_until(|| menu.actions().lookup_action("item1").unwrap().state().unwrap().get::<bool>() == Some(false), 3.0));
    check!(renders.get() == before + 1);
    // An identical layout does not re-render.
    fake.connection.emit_signal(None, "/Menu", "com.canonical.dbusmenu", "ItemsPropertiesUpdated", None).unwrap();
    settle(0.1);
    check!(renders.get() == before + 1);
    menu.close();
    let _ = fake.connection.close_sync(None::<&gio::Cancellable>);
    println!("ok dbusmenu_check_radio_disabled_hidden_submenus_and_live_updates");
}

fn dbusmenu_shortcuts_clicks_and_retired_callbacks(address: &str) {
    let fake = FakeMenu::new(address);
    let client = connection(address);
    let name = unique(&fake.connection);
    fake.delayed.set(true);
    let old = RemoteMenu::new(&client, &name, "/Menu");
    check!(spin_until(|| fake.pending.borrow().len() == 1, 3.0));
    // Coalesced: refreshes while one GetLayout is in flight are queued once.
    old.refresh();
    old.refresh();
    settle(0.05);
    check!(fake.pending.borrow().len() == 1);
    old.close();
    fake.reply_pending(0);
    settle(0.1);
    check!(old.layout().is_none()); // retired callbacks must not mutate the menu
    fake.delayed.set(false);
    let menu = RemoteMenu::new(&client, &name, "/ReplacementMenu");
    check!(spin_until(|| menu.layout().is_some(), 3.0));
    let shortcuts = menu.shortcuts();
    check!(shortcuts.len() == 1 && shortcuts[0].accel == "<Control>q" && shortcuts[0].ident == 1);
    let first = menu.model().item_link(0, "section").unwrap();
    check!(first.item_attribute_value(0, "accel", None).unwrap().get::<String>().as_deref() == Some("<Control>q"));
    let popdowns = Rc::new(Cell::new(0));
    let counter = popdowns.clone();
    menu.set_on_clicked(Some(Rc::new(move || counter.set(counter.get() + 1))));
    menu.popup_opened();
    check!(spin_until(|| fake.events.borrow().iter().any(|(m, p)| m == "Event" && p.starts_with("(0, 'opened'")), 3.0));
    check!(fake.events.borrow().iter().any(|(m, p)| m == "AboutToShow" && p == "(0,)"));
    check!(!menu.activate_shortcut(1, false)); // not mapped
    check!(menu.activate_shortcut(1, true));
    check!(spin_until(|| fake.clicked(1) == 1, 3.0));
    check!(popdowns.get() == 1);
    menu.actions().activate_action("item1", None);
    check!(spin_until(|| fake.clicked(1) == 2, 3.0));
    check!(fake.events.borrow().iter().any(|(_, p)| p == "(1, 'clicked', <0>, 0)"));
    menu.close();
    let _ = fake.connection.close_sync(None::<&gio::Cancellable>);
    println!("ok dbusmenu_shortcuts_clicks_and_retired_callbacks");
}

// ---------------------------------------------------------------------------
// org.tessel.Dock.Settings

#[derive(Default)]
struct FakeHost {
    config: RefCell<DockConfig>,
    preview: RefCell<Option<Map<String, Value>>>,
    rebuilds: RefCell<Vec<bool>>,
    docks: RefCell<Vec<(String, bool, bool, (i64, i64), bool, i64, i64, i64)>>,
}

impl SettingsHost for FakeHost {
    fn config(&self) -> DockConfig { self.config.borrow().clone() }
    fn save_config(&self, config: DockConfig, rebuild: bool) -> bool {
        self.config.replace(config);
        self.rebuilds.borrow_mut().push(rebuild);
        true
    }
    fn preview_appearance(&self, values: &Map<String, Value>) { self.preview.replace(Some(values.clone())); }
    fn restore_appearance(&self) { self.preview.replace(None); }
    fn tray_apps(&self) -> Value { json!([]) }
    fn monitors(&self) -> Vec<MonitorInfo> {
        vec![MonitorInfo { name: "DP-1".into(), label: "DP-1 · Test display".into(), geometry: (0, 0, 1920, 1080) }]
    }
    fn docks(&self) -> Vec<DockInfo> {
        let config = self.config();
        self.docks.borrow().iter().map(|(monitor, app_dock, bar_mode, dimensions, horizontal, gap, headroom, zone)| DockInfo {
            monitor_name: monitor.clone(), is_app_dock: *app_dock, bar_mode: *bar_mode, config: config.clone(),
            dimensions: *dimensions, horizontal: *horizontal, edge_gap: *gap, headroom: *headroom, exclusive_zone: *zone,
        }).collect()
    }
}

fn settings_call(client: &gio::DBusConnection, method: &str, payload: Option<Value>) -> Result<Option<Value>, glib::Error> {
    let params = payload.map(|p| (p.to_string(),).to_variant());
    let reply = call(client, BUS_NAME, OBJECT_PATH, INTERFACE, method, params)?;
    Ok(reply.try_child_value(0).and_then(|v| v.get::<String>()).map(|text| serde_json::from_str(&text).unwrap()))
}

fn settings_read_apply_preview_and_disconnect_restore(address: &str) {
    let bus = connection(address);
    request_name(&bus, BUS_NAME);
    let host = Rc::new(FakeHost::default());
    host.config.replace(DockConfig::from_value(&json!({"pins": ["org.example.App.desktop"]})).unwrap());
    let api = SettingsApi::new(&bus, host.clone()).unwrap();
    let client = connection(address);
    let read = settings_call(&client, "Read", None).unwrap().unwrap();
    check!(read["monitors"][0]["label"] == "DP-1 · Test display");
    check!(read["tray_apps"] == json!([]));
    check!(read["config"]["pins"] == json!(["org.example.App.desktop"]));
    check!(settings_call(&client, "GetTrayApps", None).unwrap() == Some(json!([])));
    let applied = settings_call(&client, "Apply", Some(json!({"changes": {"transparency": 42}, "expected": {"transparency": 5}}))).unwrap().unwrap();
    check!(applied["config"]["transparency"] == 42);
    check!(applied["config"]["pins"] == json!(["org.example.App.desktop"]));
    for (changes, expected) in [
        (json!({"match_wallpaper": true}), json!({"match_wallpaper": false})),
        (json!({"apps_transparency_enabled": true}), json!({"apps_transparency_enabled": false})),
        (json!({"apps_frosted": true, "apps_rounded_corners": false}), json!({"apps_frosted": false, "apps_rounded_corners": true})),
        (json!({"frosted": true}), json!({"frosted": false})),
        (json!({"frost_strength": 30, "apps_frost_strength": 80}), json!({"frost_strength": 50, "apps_frost_strength": 50})),
        (json!({"bar_border_enabled": true, "bar_border_sides": ["bottom"]}),
         json!({"bar_border_enabled": false, "bar_border_sides": ["top", "bottom", "left", "right"]})),
    ] {
        settings_call(&client, "Apply", Some(json!({"changes": changes, "expected": expected}))).unwrap();
    }
    check!(!*host.rebuilds.borrow().last().unwrap()); // border changes must not rebuild the dock
    settings_call(&client, "Apply", Some(json!({"changes": {"mode": "bar"}, "expected": {"mode": "dock"}}))).unwrap();
    // Errors carry the Settings error domain and Python's text.
    let error = settings_call(&client, "Apply", Some(json!({"changes": {"transparency": 1}, "expected": {"transparency": 99}}))).unwrap_err();
    check!(gio::DBusError::remote_error(&error).as_deref() == Some("org.tessel.Dock.Settings.Error"));
    check!(error.message().ends_with("Dock settings changed elsewhere. Reset this page before applying again."));
    let error = call(&client, BUS_NAME, OBJECT_PATH, INTERFACE, "Apply", Some(("not json",).to_variant())).unwrap_err();
    check!(error.message().ends_with("Expecting value: line 1 column 1 (char 0)"));
    let error = settings_call(&client, "Apply", Some(json!({"changes": {"pins": []}, "expected": {"pins": []}}))).unwrap_err();
    check!(error.message().ends_with("This setting cannot be changed here."));
    settings_call(&client, "Preview", Some(json!({"transparency": 60, "match_wallpaper": true, "rounded_corners": true, "float_mode": false,
        "transparency_enabled": false, "frosted": true, "frost_strength": 45, "bar_border_enabled": true,
        "bar_border_sides": ["right"], "bar_border_color": "#AABBCC"}))).unwrap();
    {
        let preview = host.preview.borrow();
        let preview = preview.as_ref().unwrap();
        check!(preview["bar_border_color"] == "#aabbcc" && preview["bar_border_sides"] == json!(["right"]));
    }
    check!(api.preview_owner() == Some(unique(&client)));
    for (field, invalid) in [("bar_border_enabled", json!(1)), ("bar_border_sides", json!(["bad"])), ("bar_border_sides", json!("top")),
                             ("bar_border_color", json!("#abcdef;"))] {
        let mut payload = json!({"transparency": 0, "match_wallpaper": false, "rounded_corners": false, "float_mode": false});
        payload[field] = invalid;
        check!(settings_call(&client, "Preview", Some(payload)).is_err());
    }
    for invalid in [json!(true), json!(-1), json!(101), json!(12.5)] {
        let payload = json!({"transparency": 60, "match_wallpaper": true, "rounded_corners": true, "float_mode": false, "frost_strength": invalid});
        let error = settings_call(&client, "Preview", Some(payload)).unwrap_err();
        check!(error.message().ends_with("Invalid Blur strength."));
    }
    // Restore from someone else does nothing.
    let other = connection(address);
    settings_call(&other, "Restore", None).unwrap();
    check!(host.preview.borrow().is_some());
    let _ = client.close_sync(None::<&gio::Cancellable>);
    check!(spin_until(|| host.preview.borrow().is_none(), 3.0));
    check!(api.preview_owner().is_none());
    check!(*host.rebuilds.borrow() == vec![false, false, false, false, false, false, false, true]);
    let config = host.config();
    check!((config.frost_strength, config.apps_frost_strength) == (30, 80));
    check!(config.frosted && config.apps_frosted && !config.apps_rounded_corners);
    // Apply ends the caller's own preview.
    settings_call(&other, "Preview", Some(json!({"transparency": 1, "match_wallpaper": false, "rounded_corners": false, "float_mode": false}))).unwrap();
    check!(host.preview.borrow().is_some());
    settings_call(&other, "Apply", Some(json!({"changes": {"gap": 9}, "expected": {"gap": config.gap}}))).unwrap();
    check!(host.preview.borrow().is_none());
    // Popup placement.
    let placement = settings_call(&other, "GetPopupPlacement", None).err();
    check!(placement.is_some()); // wrong signature
    let reply = call(&other, BUS_NAME, OBJECT_PATH, INTERFACE, "GetPopupPlacement", Some(("DP-1", "system_tray").to_variant())).unwrap();
    let text = reply.child_value(0).get::<String>().unwrap();
    let edge = host.config().edge;
    check!(text == format!("{{\"edge\": \"{edge}\", \"offset\": 12, \"align\": \"end\"}}"));
    host.docks.borrow_mut().push(("DP-1".into(), false, true, (1920, 40), true, 6, 0, 46));
    let reply = call(&other, BUS_NAME, OBJECT_PATH, INTERFACE, "GetPopupPlacement", Some(("DP-1", "system_tray").to_variant())).unwrap();
    let placement: Value = serde_json::from_str(&reply.child_value(0).get::<String>().unwrap()).unwrap();
    check!(placement["offset"] == 54);
    let error = call(&other, BUS_NAME, OBJECT_PATH, INTERFACE, "GetPopupPlacement", Some(("DP-1", "nope").to_variant())).unwrap_err();
    check!(error.message().ends_with("Unknown popup group"));
    // Workareas from the live bar reservation.
    let reply = call(&other, BUS_NAME, OBJECT_PATH, INTERFACE, "GetWorkareas", None).unwrap();
    check!(reply.child_value(0).get::<String>().unwrap()
           == format!(r#"{{"version":1,"monitors":[{{"name":"DP-1","geometry":[0,0,1920,1080],"workarea":{}}}]}}"#,
                      if edge == "top" { "[0,46,1920,1034]" } else if edge == "bottom" { "[0,0,1920,1034]" } else if edge == "left" { "[46,0,1874,1080]" } else { "[0,0,1874,1080]" }));
    // close() restores an active preview.
    settings_call(&other, "Preview", Some(json!({"transparency": 1, "match_wallpaper": false, "rounded_corners": false, "float_mode": false}))).unwrap();
    api.close();
    check!(host.preview.borrow().is_none());
    check!(call(&other, BUS_NAME, OBJECT_PATH, INTERFACE, "Read", None).is_err());
    release_name(&bus, BUS_NAME);
    let _ = other.close_sync(None::<&gio::Cancellable>);
    let _ = bus.close_sync(None::<&gio::Cancellable>);
    println!("ok settings_read_apply_preview_and_disconnect_restore");
}

fn workareas_coalesce_and_emit_only_on_change(address: &str) {
    let bus = connection(address);
    request_name(&bus, BUS_NAME);
    let host = Rc::new(FakeHost::default());
    let api = SettingsApi::new(&bus, host).unwrap();
    let listener = connection(address);
    let payloads: Rc<RefCell<Vec<String>>> = Rc::default();
    let seen = payloads.clone();
    let _subscription = listener.subscribe_to_signal(None, Some(INTERFACE), Some("WorkareasChanged"), Some(OBJECT_PATH), None,
        gio::DBusSignalFlags::NONE, move |signal| {
            seen.borrow_mut().push(signal.parameters.child_value(0).get::<String>().unwrap());
        });
    settle(0.05);
    let payload = Rc::new(RefCell::new(r#"{"version":1,"monitors":[{"name":"DP-1","geometry":[0,0,1920,1080],"workarea":[0,48,1920,1032]}]}"#.to_owned()));
    let source = payload.clone();
    api.set_workareas_override(Some(Box::new(move || source.borrow().clone())));
    for _ in 0..20 {
        api.queue_workareas_changed();
    }
    check!(spin_until(|| payloads.borrow().len() == 1, 2.0));
    for _ in 0..20 {
        api.queue_workareas_changed();
    }
    settle(0.2);
    check!(payloads.borrow().len() == 1);
    *payload.borrow_mut() = r#"{"version":1,"monitors":[{"name":"DP-1","geometry":[0,0,1920,1080],"workarea":[64,0,1856,1080]}]}"#.to_owned();
    for _ in 0..20 {
        api.queue_workareas_changed();
    }
    check!(spin_until(|| payloads.borrow().len() == 2, 2.0));
    check!(payloads.borrow()[1].contains("[64,0,1856,1080]"));
    // A pending idle is cancelled by close.
    *payload.borrow_mut() = "{}".into();
    api.queue_workareas_changed();
    api.close();
    settle(0.1);
    check!(payloads.borrow().len() == 2);
    release_name(&bus, BUS_NAME);
    let _ = bus.close_sync(None::<&gio::Cancellable>);
    println!("ok workareas_coalesce_and_emit_only_on_change");
}

// ---------------------------------------------------------------------------
// Capture status + real StatusBackend services on the private bus.

const CAPTURE_XML: &str = r#"<node><interface name="org.tessel.Capture.Status">
<method name="GetState"><arg type="s" direction="out"/><arg type="b" direction="out"/></method>
<signal name="Changed"><arg type="s"/></signal>
</interface></node>"#;

fn capture_status_follows_owner_signal_and_vanish(address: &str) {
    use tessel_dock::status::capture::{CaptureStatus, INTERFACE as CAPTURE_INTERFACE, NAME, PATH};
    let states: Rc<RefCell<Vec<String>>> = Rc::default();
    let seen = states.clone();
    let status = CaptureStatus::new(move |state| seen.borrow_mut().push(state.to_owned()));
    let service = connection(address);
    let info = gio::DBusNodeInfo::for_xml(CAPTURE_XML).unwrap().lookup_interface(CAPTURE_INTERFACE).unwrap();
    service.register_object(PATH, &info)
        .method_call(|_, _, _, _, _, _, invocation| invocation.return_value(Some(&("recording", true).to_variant())))
        .build().unwrap();
    check!(spin_until(|| *states.borrow() == ["idle"], 2.0)); // initial vanished (no owner)
    request_name(&service, NAME);
    check!(spin_until(|| states.borrow().last().map(String::as_str) == Some("recording"), 3.0));
    service.emit_signal(None, PATH, CAPTURE_INTERFACE, "Changed", Some(&("stopping",).to_variant())).unwrap();
    check!(spin_until(|| states.borrow().last().map(String::as_str) == Some("stopping"), 3.0));
    release_name(&service, NAME);
    check!(spin_until(|| states.borrow().last().map(String::as_str) == Some("idle"), 3.0));
    status.close();
    request_name(&service, NAME);
    settle(0.1);
    check!(states.borrow().last().map(String::as_str) == Some("idle"));
    let _ = service.close_sync(None::<&gio::Cancellable>);
    println!("ok capture_status_follows_owner_signal_and_vanish");
}

fn status_backend_real_services_on_private_bus() {
    use tessel_dock::status::backend::{Services, StatusBackend, StatusOptions};
    use tessel_dock::status::process::{JobFactory, StatusJob};
    struct Idle;
    impl StatusJob for Idle {
        fn start(&self) {}
        fn close(&self) {}
    }
    let factory: JobFactory = Rc::new(|_, _, _, _| Rc::new(Idle) as Rc<dyn StatusJob>);
    let errors: Rc<RefCell<Vec<String>>> = Rc::default();
    let seen = errors.clone();
    let backend = StatusBackend::new(|| {}, Some(Box::new(move |error: &str| seen.borrow_mut().push(error.into()))),
        StatusOptions { program: std::rc::Rc::new(|name| std::path::Path::new("/nonexistent").join(name)), job_factory: factory, scheduler: Rc::new(tessel_core::scheduler::GlibScheduler::default()),
                        services: Services::real(), on_show_desktop: None, media: true });
    backend.start();
    let tooltip = |name: &str| backend.get(name).unwrap()["tooltip"].as_str().unwrap().to_owned();
    check!(spin_until(|| tooltip("idle_inhibitor") == "Keep Awake is not running in this session.", 5.0));
    check!(spin_until(|| tooltip("bluetooth").starts_with("Bluetooth: "), 5.0));
    check!(backend.get("bluetooth").unwrap()["classes"] == json!(["unavailable"]));
    check!(spin_until(|| backend.get("media").unwrap().get("title").is_some(), 5.0));
    check!(backend.get("media").unwrap()["tooltip"] == "Media\nNothing playing");
    // The Media switch stops and restarts the real MPRIS watcher live.
    for _ in 0..2 {
        backend.set_media_enabled(false);
        check!(!backend.media_running() && backend.get("media").unwrap().get("title").is_none());
        settle(0.1);
        check!(backend.get("media").unwrap().get("title").is_none());
        backend.set_media_enabled(true);
        check!(spin_until(|| backend.get("media").unwrap().get("title").is_some(), 5.0));
    }
    check!(backend.get("screen_record").unwrap()["state"] == "idle");
    check!(spin_until(|| tooltip("discovery") != "Discover Network Devices: waiting for status", 5.0));
    backend.close();
    settle(0.1);
    println!("ok status_backend_real_services_on_private_bus (discovery: {})", tooltip("discovery").replace('\n', " | "));
}

fn main() {
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    assert_eq!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(), address, "TestDBus did not take over the session bus");
    // SAFETY: single-threaded test main, before any other thread starts.
    unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };
    unsafe { std::env::remove_var("NOTIFY_SOCKET") };
    sni_discovery_actions_dynamic_icons_and_owner_exit(&address);
    sni_existing_watcher_is_not_replaced_and_handoff_keeps_legacy_items(&address);
    sni_foreign_watcher_is_never_replaced_and_its_items_and_signals_are_followed(&address);
    sni_rejected_host_registration_does_not_report_external_host_ready(&address);
    sni_passive_and_attention_status_updates_are_published(&address);
    dbusmenu_check_radio_disabled_hidden_submenus_and_live_updates(&address);
    dbusmenu_shortcuts_clicks_and_retired_callbacks(&address);
    settings_read_apply_preview_and_disconnect_restore(&address);
    workareas_coalesce_and_emit_only_on_change(&address);
    capture_status_follows_owner_signal_and_vanish(&address);
    status_backend_real_services_on_private_bus();
    settle(0.1);
    bus.down();
    println!("b_private_bus: all passed");
}
