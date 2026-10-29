//! `file_manager_bus.py`: opt-in freedesktop `org.freedesktop.FileManager1`
//! integration for the running Files app.
//!
//! No activation service is installed. The well-known name is only requested
//! while Files is the user's default directory handler, never replaces another
//! owner (`DO_NOT_QUEUE`), and is relinquished if the user selects a different
//! default file manager.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gio::prelude::*;

pub const BUS_NAME: &str = "org.freedesktop.FileManager1";
pub const INTERFACE: &str = "org.freedesktop.FileManager1";
pub const OBJECT_PATH: &str = "/org/freedesktop/FileManager1";
pub const DESKTOP_ID: &str = "org.waydock.Files.desktop";
pub const INTROSPECTION: &str = r#"
<node><interface name="org.freedesktop.FileManager1">
  <method name="ShowFolders"><arg name="URIs" type="as" direction="in"/>
    <arg name="StartupId" type="s" direction="in"/></method>
  <method name="ShowItems"><arg name="URIs" type="as" direction="in"/>
    <arg name="StartupId" type="s" direction="in"/></method>
  <method name="ShowItemProperties"><arg name="URIs" type="as" direction="in"/>
    <arg name="StartupId" type="s" direction="in"/></method>
</interface></node>
"#;

/// `handler is not None and handler.get_id() == DESKTOP_ID`.
pub fn is_default_handler(handler_id: Option<&str>) -> bool {
    handler_id == Some(DESKTOP_ID)
}

/// `is_default_file_manager()`: the `inode/directory` default is Files.
pub fn is_default_file_manager() -> bool {
    let handler = gio::AppInfo::default_for_type("inode/directory", false);
    is_default_handler(handler.as_ref().and_then(|h| h.id()).as_deref())
}

/// The application side (Python duck-typed `app`). An `Err` is reported to
/// the caller as `org.freedesktop.DBus.Error.Failed` with that text.
pub trait FileManagerHost {
    /// `app.get_dbus_connection()` (the GApplication's session connection).
    fn connection(&self) -> Option<gio::DBusConnection>;
    /// `app.open_locations(files, select=…, new_window=False)`.
    fn open_locations(&self, files: Vec<gio::File>, select: bool, new_window: bool) -> Result<(), String>;
    /// `app.show_properties(files)`.
    fn show_properties(&self, files: Vec<gio::File>) -> Result<(), String>;
}

struct Inner {
    app: Rc<dyn FileManagerHost>,
    default_checker: Box<dyn Fn() -> bool>,
    available: Cell<bool>,
    last_error: RefCell<Option<String>>,
    connection: RefCell<Option<gio::DBusConnection>>,
    registration: RefCell<Option<gio::RegistrationId>>,
    subscription: RefCell<Option<gio::SignalSubscriptionId>>,
    monitor: RefCell<Option<(gio::AppInfoMonitor, glib::SignalHandlerId)>>,
    pending: Cell<bool>,
    closed: Cell<bool>,
}

/// `FileManagerBus(app, default_checker=None)`: optional service wrapper;
/// construct/start after GApplication startup and `close()` on shutdown.
/// `start()` reports eligibility; asynchronous name acquisition is exposed
/// as [`Self::available`]. No default-association changes are made.
#[derive(Clone)]
pub struct FileManagerBus(Rc<Inner>);

fn valid_uri(uri: &str) -> bool {
    uri.chars().count() <= 8192
        && crate::util::starts_with_scheme(uri, ":")
        && !uri.chars().any(|c| crate::util::is_py_space(c) || c == '\0')
}

impl FileManagerBus {
    pub fn new(app: Rc<dyn FileManagerHost>, default_checker: Option<Box<dyn Fn() -> bool>>) -> Self {
        Self(Rc::new(Inner {
            app,
            default_checker: default_checker.unwrap_or_else(|| Box::new(is_default_file_manager)),
            available: Cell::new(false),
            last_error: RefCell::new(None),
            connection: RefCell::new(None),
            registration: RefCell::new(None),
            subscription: RefCell::new(None),
            monitor: RefCell::new(None),
            pending: Cell::new(false),
            closed: Cell::new(false),
        }))
    }

    /// Whether Files currently owns the name.
    pub fn available(&self) -> bool {
        self.0.available.get()
    }
    pub fn last_error(&self) -> Option<String> {
        self.0.last_error.borrow().clone()
    }
    /// A `RequestName` call is in flight.
    pub fn pending(&self) -> bool {
        self.0.pending.get()
    }
    pub fn connection(&self) -> Option<gio::DBusConnection> {
        self.0.connection.borrow().clone()
    }
    pub fn is_closed(&self) -> bool {
        self.0.closed.get()
    }

    /// Watch default-handler changes; request the name if eligible.
    pub fn start(&self) -> bool {
        if self.0.closed.get() {
            return false;
        }
        if self.0.monitor.borrow().is_none() {
            let monitor = gio::AppInfoMonitor::get();
            let weak = Rc::downgrade(&self.0);
            let handler = monitor.connect_changed(move |_| {
                if let Some(inner) = weak.upgrade() {
                    FileManagerBus(inner).default_changed();
                }
            });
            self.0.monitor.replace(Some((monitor, handler)));
        }
        let eligible = (self.0.default_checker)();
        if eligible {
            self.request_name();
        }
        eligible
    }

    fn bind(&self) -> Result<bool, glib::Error> {
        if self.0.connection.borrow().is_none() {
            let connection = self.0.app.connection();
            self.0.connection.replace(connection);
        }
        let Some(connection) = self.connection().filter(|c| !c.is_closed()) else { return Ok(false) };
        if self.0.registration.borrow().is_none() {
            let node = gio::DBusNodeInfo::for_xml(INTROSPECTION)?;
            let info = node.interfaces()[0].clone();
            let weak = Rc::downgrade(&self.0);
            let registration = connection
                .register_object(OBJECT_PATH, &info)
                .method_call(move |_connection, _sender, _path, _interface, method, parameters, invocation| {
                    match weak.upgrade() {
                        Some(inner) => FileManagerBus(inner).method_call(method, &parameters, invocation),
                        None => invocation.return_dbus_error("org.freedesktop.DBus.Error.NotSupported", "Files is not the active default file manager"),
                    }
                })
                .build()?;
            self.0.registration.replace(Some(registration));
            let weak: Weak<Inner> = Rc::downgrade(&self.0);
            #[allow(deprecated)]
            let subscription = connection.signal_subscribe(
                Some("org.freedesktop.DBus"),
                Some("org.freedesktop.DBus"),
                Some("NameOwnerChanged"),
                Some("/org/freedesktop/DBus"),
                Some(BUS_NAME),
                gio::DBusSignalFlags::NONE,
                move |_connection, _sender, _path, _interface, _signal, parameters| {
                    if let Some(inner) = weak.upgrade() {
                        FileManagerBus(inner).name_changed(parameters);
                    }
                },
            );
            self.0.subscription.replace(Some(subscription));
        }
        Ok(true)
    }

    fn request_name(&self) {
        let inner = &self.0;
        if inner.closed.get() || inner.pending.get() || inner.available.get() || !(inner.default_checker)() {
            return;
        }
        match self.bind() {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                inner.last_error.replace(Some(suite_core::pyerr::python_error_text(&error)));
                return;
            }
        }
        let Some(connection) = self.connection() else { return };
        inner.pending.set(true);
        let this = self.clone();
        // DBUS_NAME_FLAG_DO_NOT_QUEUE (4): no replacement and no queued takeover.
        connection.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(BUS_NAME, 4u32).to_variant()),
            Some(glib::VariantTy::new("(u)").expect("(u)")),
            gio::DBusCallFlags::NONE,
            2000,
            None::<&gio::Cancellable>,
            move |result| this.name_requested(result),
        );
    }

    fn name_requested(&self, result: Result<glib::Variant, glib::Error>) {
        let inner = &self.0;
        inner.pending.set(false);
        let reply = match result {
            Ok(reply) => reply.child_value(0).get::<u32>().unwrap_or(0),
            Err(error) => {
                inner.last_error.replace(Some(suite_core::pyerr::python_error_text(&error)));
                return;
            }
        };
        let acquired = matches!(reply, 1 | 4); // PRIMARY_OWNER or ALREADY_OWNER.
        if acquired && (inner.closed.get() || !(inner.default_checker)()) {
            self.release_name();
            return;
        }
        inner.available.set(acquired);
        inner.last_error.replace(if acquired { None } else { Some("Another file manager owns FileManager1".into()) });
    }

    fn release_name(&self) {
        self.0.available.set(false);
        if let Some(connection) = self.connection().filter(|c| !c.is_closed()) {
            connection.call(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ReleaseName",
                Some(&(BUS_NAME,).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                None::<&gio::Cancellable>,
                |_| {},
            );
        }
    }

    /// `_default_changed`: the `inode/directory` default changed.
    pub fn default_changed(&self) {
        if self.0.closed.get() {
            return;
        }
        if (self.0.default_checker)() {
            self.request_name();
        } else if self.0.available.get() {
            self.release_name();
        }
    }

    fn name_changed(&self, parameters: &glib::Variant) {
        let Some((_name, old_owner, new_owner)) = parameters.get::<(String, String, String)>() else { return };
        if self.0.closed.get() {
            return;
        }
        let unique = self.connection().and_then(|c| c.unique_name()).map(|s| s.to_string());
        if unique.as_deref() == Some(old_owner.as_str()) && new_owner != old_owner {
            self.0.available.set(false);
        }
        if new_owner.is_empty() {
            self.request_name();
        }
    }

    fn method_call(&self, method: &str, parameters: &glib::Variant, invocation: gio::DBusMethodInvocation) {
        let inner = &self.0;
        if inner.closed.get() || !inner.available.get() || !(inner.default_checker)() {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.NotSupported", "Files is not the active default file manager");
            return;
        }
        let Some((uris, _startup_id)) = parameters.get::<(Vec<String>, String)>() else {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "Expected at most 128 valid file URIs");
            return;
        };
        if uris.len() > 128 || !uris.iter().all(|uri| valid_uri(uri)) {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "Expected at most 128 valid file URIs");
            return;
        }
        let files: Vec<gio::File> = uris.iter().map(|uri| gio::File::for_uri(uri)).collect();
        let result = match method {
            "ShowItemProperties" => {
                if files.is_empty() {
                    Ok(())
                } else {
                    inner.app.show_properties(files)
                }
            }
            "ShowItems" | "ShowFolders" => {
                if files.is_empty() {
                    Ok(())
                } else {
                    inner.app.open_locations(files, method == "ShowItems", false)
                }
            }
            _ => {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
                return;
            }
        };
        match result {
            Ok(()) => invocation.return_value(None),
            Err(error) => invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &error),
        }
    }

    /// Release the name, unregister and stop watching (idempotent).
    pub fn close(&self) {
        let inner = &self.0;
        if inner.closed.get() {
            return;
        }
        inner.closed.set(true);
        if inner.available.get() {
            self.release_name();
        }
        if let Some(connection) = self.connection() {
            if let Some(registration) = inner.registration.borrow_mut().take() {
                let _ = connection.unregister_object(registration);
            }
            if let Some(subscription) = inner.subscription.borrow_mut().take() {
                #[allow(deprecated)]
                connection.signal_unsubscribe(subscription);
            }
        }
        if let Some((monitor, handler)) = inner.monitor.borrow_mut().take() {
            monitor.disconnect(handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoBus;
    impl FileManagerHost for NoBus {
        fn connection(&self) -> Option<gio::DBusConnection> {
            panic!("Unexpected bus access")
        }
        fn open_locations(&self, _: Vec<gio::File>, _: bool, _: bool) -> Result<(), String> {
            unreachable!()
        }
        fn show_properties(&self, _: Vec<gio::File>) -> Result<(), String> {
            unreachable!()
        }
    }

    // DefaultHandlerTests.test_only_our_explicit_default_enables_integration
    #[test]
    fn only_our_explicit_default_enables_integration() {
        assert!(!is_default_handler(None));
        assert!(!is_default_handler(Some("org.kde.dolphin.desktop")));
        assert!(is_default_handler(Some("org.waydock.Files.desktop")));
    }

    // DefaultHandlerTests.test_nondefault_never_gets_bus_or_registers
    #[test]
    fn nondefault_never_gets_bus_or_registers() {
        crate::test_env();
        let service = FileManagerBus::new(Rc::new(NoBus), Some(Box::new(|| false)));
        assert!(!service.start());
        assert!(!service.available());
        service.default_changed();
        service.close();
        assert!(!service.start());
    }

    #[test]
    fn uri_validation() {
        assert!(valid_uri("file:///home/test"));
        assert!(valid_uri("sftp://host/work"));
        assert!(!valid_uri("not a URI"));
        assert!(!valid_uri("/plain/path"));
        assert!(!valid_uri("file:///a\u{1c}b"));
        assert!(!valid_uri(&format!("file:///{}", "a".repeat(8192))));
    }
}
