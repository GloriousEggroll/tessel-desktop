//! NotifyTests from test_desktop_file_ops.py: the notification goes out
//! asynchronously to a FAKE org.freedesktop.Notifications on a private bus
//! (gio::TestDBus — never the real session bus), and the default-browser
//! lookup runs a fake `xdg-settings` without blocking. One test function, so
//! the process-wide PATH and DBUS_SESSION_BUS_ADDRESS changes are serial.
#![cfg(feature = "fileops")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use wayshell::fileops as ops;

const XML: &str = r#"<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
<arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/>
<arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
</interface></node>"#;

#[test]
fn notification_is_async_and_browser_lookup_does_not_block() {
    let tmp = std::env::temp_dir();
    assert!(!tmp.starts_with("/tmp"), "set TMPDIR to a scratch dir");
    std::env::remove_var("DBUS_SESSION_BUS_ADDRESS");
    // tools/headless.md: GTestDBus has no service dirs (nothing can be activated); GTK/GIO here
    // and in spawned children must not look up portals or gvfs.
    for (key, value) in [("GDK_DEBUG", "no-portals"), ("GIO_USE_VFS", "local"), ("GTK_USE_PORTAL", "0")] {
        std::env::set_var(key, value);
    }
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let address = bus.bus_address().unwrap().to_string();
    assert_eq!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(), address);
    assert!(!address.contains("/run/user/1000/bus"), "private bus only");
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();

    // The fake notification service, on its own connection.
    let service = gio::DBusConnection::for_address_sync(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        gio::Cancellable::NONE,
    )
    .unwrap();
    let calls: Rc<RefCell<Vec<glib::Variant>>> = Rc::default();
    let seen = calls.clone();
    let info = gio::DBusNodeInfo::for_xml(XML).unwrap().lookup_interface(ops::NOTIFY_BUS).unwrap();
    let _id = service
        .register_object(ops::NOTIFY_PATH, &info)
        .method_call(move |_, _, _, _, method, params, invocation| {
            assert_eq!(method, "Notify");
            seen.borrow_mut().push(params);
            invocation.return_value(Some(&(7u32,).to_variant()));
        })
        .build()
        .unwrap();
    let owned = service
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(ops::NOTIFY_BUS, 4u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .unwrap();
    assert_eq!(owned.get::<(u32,)>(), Some((1,)));

    ops::notify_with("Could not open", "details", "dialog-error-symbolic", Some("Desktop"));
    assert!(calls.borrow().is_empty(), "notify never blocks for the bus");
    assert!(suite_core::timer::wait_for(&context, Duration::from_secs(5), || !calls.borrow().is_empty()));
    let (app, id, icon, summary, body, _actions, hints, timeout): (
        String,
        u32,
        String,
        String,
        String,
        Vec<String>,
        std::collections::HashMap<String, glib::Variant>,
        i32,
    ) = calls.borrow()[0].get().unwrap();
    assert_eq!((app.as_str(), summary.as_str(), body.as_str()), ("Desktop", "Could not open", "details"));
    assert_eq!((id, icon.as_str(), timeout), (0, "dialog-error-symbolic", 8000));
    assert_eq!(hints["urgency"].get::<u8>(), Some(1));
    // Default app name is tr("Desktop"); failures (no reply errors here) are only logged.
    ops::notify("Second", "");
    assert!(suite_core::timer::wait_for(&context, Duration::from_secs(5), || calls.borrow().len() == 2));
    assert_eq!(calls.borrow()[1].child_value(0).get::<String>().as_deref(), Some("Desktop"));

    // Default browser lookup via a fake xdg-settings.
    let bindir = tmp.join(format!("desktop-ops-bin-{}", std::process::id()));
    std::fs::create_dir_all(&bindir).unwrap();
    let script = bindir.join("xdg-settings");
    std::fs::write(&script, "#!/bin/sh\necho firefox.desktop\n").unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", &path);
    let environment = vec![("PATH".to_string(), bindir.display().to_string())];
    let results: Rc<RefCell<Vec<(Option<String>, Option<String>)>>> = Rc::default();
    let r = results.clone();
    ops::default_web_browser_async(environment, move |browser, error| r.borrow_mut().push((browser, error)));
    assert!(results.borrow().is_empty());
    assert!(suite_core::timer::wait_for(&context, Duration::from_secs(5), || !results.borrow().is_empty()));
    assert_eq!(*results.borrow(), vec![(Some("firefox.desktop".to_string()), None)]);
    // A failing lookup reports stderr.
    std::fs::write(&script, "#!/bin/sh\necho nope >&2\nexit 1\n").unwrap();
    let r = results.clone();
    ops::default_web_browser_async(Vec::<(String, String)>::new(), move |browser, error| r.borrow_mut().push((browser, error)));
    assert!(suite_core::timer::wait_for(&context, Duration::from_secs(5), || results.borrow().len() == 2));
    assert_eq!(results.borrow()[1], (None, Some("nope".to_string())));
    let _ = std::fs::remove_dir_all(&bindir);
    drop(service);
    bus.down();
}
