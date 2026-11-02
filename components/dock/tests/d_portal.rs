//! AppearanceController against a FAKE settings portal on a private
//! `gio::TestDBus` bus (never the real session bus): initial ReadAll,
//! SettingChanged, portal loss → GTK fallback, portal return → ReadAll again.

use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_dock::appearance::controller::{self, AppearanceController, ControllerOptions, IconThemeBackend, PortalSource, SettingsBackend};

const XML: &str = r#"<node><interface name="org.freedesktop.portal.Settings">
<method name="ReadAll"><arg type="as" direction="in"/><arg type="a{sa{sv}}" direction="out"/></method>
<signal name="SettingChanged"><arg type="s"/><arg type="s"/><arg type="v"/></signal></interface></node>"#;

#[derive(Default)]
struct Settings {
    prefer_dark: Cell<bool>,
    icon: RefCell<Option<String>>,
}

impl SettingsBackend for Settings {
    fn prefer_dark(&self) -> bool { self.prefer_dark.get() }
    fn set_prefer_dark(&self, value: bool) { self.prefer_dark.set(value) }
    fn string(&self, name: &str) -> Option<String> {
        if name == controller::ICON_THEME { self.icon.borrow().clone() } else { Some("Adwaita".into()) }
    }
    fn set_string(&self, name: &str, value: &str) {
        if name == controller::ICON_THEME {
            *self.icon.borrow_mut() = Some(value.into());
        }
    }
    fn reset(&self, name: &str) {
        if name == controller::PREFER_DARK {
            self.prefer_dark.set(false);
        }
    }
    fn connect_notify(&self, _: &str, _: Box<dyn Fn()>) -> u64 { 1 }
    fn disconnect(&self, _: u64) {}
}

struct Icons(Vec<PathBuf>);

impl IconThemeBackend for Icons {
    fn search_path(&self) -> Vec<PathBuf> { self.0.clone() }
    fn set_search_path(&self, _: &[PathBuf]) {}
    fn connect_changed(&self, _: Box<dyn Fn()>) -> u64 { 1 }
    fn disconnect(&self, _: u64) {}
}

fn settle(context: &glib::MainContext, seconds: f64, done: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    done()
}

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None, None::<&gio::Cancellable>).unwrap()
}

fn bus_call(connection: &gio::DBusConnection, method: &str, name: &str, extra: Option<u32>) {
    let parameters = match extra {
        Some(flags) => (name, flags).to_variant(),
        None => (name,).to_variant(),
    };
    connection.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", method,
                         Some(&parameters), None, gio::DBusCallFlags::NONE, 2000, None::<&gio::Cancellable>).unwrap();
}

#[test]
fn portal_color_scheme_on_private_bus() {
    let scratch = PathBuf::from(std::env::var_os("TMPDIR").expect("TMPDIR"));
    let icons = scratch.join(format!("d-portal-icons-{}", std::process::id()));
    for theme in ["Papirus-Dark", "Papirus-Light"] {
        std::fs::create_dir_all(icons.join(theme)).unwrap();
        std::fs::write(icons.join(theme).join("index.theme"), "").unwrap();
    }
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
        assert!(!address.contains("/run/user/1000/bus"), "must be a private bus");

        let portal = connect(&address);
        let scheme = Rc::new(Cell::new(1u32));
        let reads = Rc::new(Cell::new(0u32));
        let info = gio::DBusNodeInfo::for_xml(XML).unwrap().lookup_interface(controller::PORTAL_INTERFACE).unwrap();
        let (value, counter) = (scheme.clone(), reads.clone());
        let _registration = portal.register_object(controller::PORTAL_PATH, &info).method_call(move |_, _, _, _, method, parameters, invocation| {
            assert_eq!(method, "ReadAll");
            assert_eq!(parameters.get::<(Vec<String>,)>().unwrap().0, [controller::APPEARANCE]);
            counter.set(counter.get() + 1);
            let inner: HashMap<String, glib::Variant> = [("color-scheme".to_owned(), value.get().to_variant())].into_iter().collect();
            let outer: HashMap<String, HashMap<String, glib::Variant>> = [(controller::APPEARANCE.to_owned(), inner)].into_iter().collect();
            invocation.return_value(Some(&(outer,).to_variant()));
        }).build().unwrap();
        bus_call(&portal, "RequestName", controller::PORTAL_NAME, Some(4));

        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let settings = Rc::new(Settings::default());
        let client = connect(&address);
        let appearance = AppearanceController::new(
            Some(Box::new(move |c: &AppearanceController| sink.borrow_mut().push((c.is_dark(), c.icon_theme_name())))),
            ControllerOptions { settings: Some(settings.clone()), icon_theme: Some(Rc::new(Icons(vec![icons.clone()]))),
                                portal: PortalSource::Connection(client), desktop_icon_theme: None, no_gtk_defaults: true });
        appearance.start();
        assert_eq!(events.borrow()[0], (false, Some("Papirus-Light".into())), "GTK fallback first");
        assert!(settle(&context, 3.0, || appearance.is_dark()), "initial ReadAll applies portal dark");
        assert_eq!(appearance.icon_theme_name().as_deref(), Some("Papirus-Dark"));
        assert!(settings.prefer_dark.get(), "process-local prefer-dark override");
        assert_eq!(reads.get(), 1);

        let emit = |value: u32| {
            portal.emit_signal(None, controller::PORTAL_PATH, controller::PORTAL_INTERFACE, "SettingChanged",
                               Some(&(controller::APPEARANCE, "color-scheme", value.to_variant()).to_variant())).unwrap();
            portal.flush_sync(None::<&gio::Cancellable>).unwrap();
        };
        emit(2);
        assert!(settle(&context, 3.0, || !appearance.is_dark()), "SettingChanged light");
        // Unrelated keys are ignored.
        portal.emit_signal(None, controller::PORTAL_PATH, controller::PORTAL_INTERFACE, "SettingChanged",
                           Some(&(controller::APPEARANCE, "accent-color", 1u32.to_variant()).to_variant())).unwrap();
        emit(1);
        assert!(settle(&context, 3.0, || appearance.is_dark()));

        // Portal loss: back to the GTK fallback (override removed).
        bus_call(&portal, "ReleaseName", controller::PORTAL_NAME, None);
        assert!(settle(&context, 3.0, || !appearance.is_dark()), "portal loss restores GTK");
        assert!(!settings.prefer_dark.get());
        // Portal returns: ReadAll again.
        scheme.set(1);
        bus_call(&portal, "RequestName", controller::PORTAL_NAME, Some(4));
        assert!(settle(&context, 3.0, || appearance.is_dark() && reads.get() == 2), "reconnect reads again");

        appearance.close();
        emit(2);
        settle(&context, 0.3, || false);
        assert!(appearance.is_dark(), "closed controllers ignore signals");
        bus.down();
    }).unwrap();
}
