//! Synchronous Gio D-Bus transport (language_backend.py `LocaleServices`),
//! constructed lazily inside the worker thread. `gio::bus_get_sync` honours
//! `DBUS_SYSTEM_BUS_ADDRESS` / `DBUS_SESSION_BUS_ADDRESS`, which the tests
//! point at a private dbus-daemon.

use crate::backend::{Env, Services};
use glib::variant::ToVariant;
use std::sync::Mutex;
use tessel_core::pyerr::python_error_text;
use tessel_core::tr;

const LOCALE1: &str = "org.freedesktop.locale1";
const LOCALE1_PATH: &str = "/org/freedesktop/locale1";
const ACCOUNTS: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
const ACCOUNTS_USER: &str = "org.freedesktop.Accounts.User";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const ABSENT_ACCOUNT_ERRORS: [&str; 3] = [
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
    "org.freedesktop.Accounts.Error.UserDoesNotExist",
];

#[derive(Default)]
pub struct LocaleServices {
    system_bus: Mutex<Option<gio::DBusConnection>>,
}

impl LocaleServices {
    pub fn new() -> Self {
        Self::default()
    }

    fn system_bus(&self) -> Result<gio::DBusConnection, glib::Error> {
        let mut bus = self.system_bus.lock().unwrap();
        if let Some(bus) = bus.as_ref() {
            return Ok(bus.clone());
        }
        let connection = gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>)?;
        *bus = Some(connection.clone());
        Ok(connection)
    }

    fn call(&self, name: &str, path: &str, interface: &str, method: &str, parameters: Option<glib::Variant>,
            interactive: bool) -> Result<glib::Variant, glib::Error> {
        let flags = if interactive { gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION } else { gio::DBusCallFlags::NONE };
        self.system_bus()?.call_sync(Some(name), path, interface, method, parameters.as_ref(), None,
                                     flags, if interactive { 120_000 } else { 5_000 }, None::<&gio::Cancellable>)
    }

    fn property(&self, name: &str, path: &str, interface: &str, property: &str) -> Result<glib::Variant, glib::Error> {
        let reply = self.call(name, path, PROPERTIES, "Get", Some((interface, property).to_variant()), false)?;
        let boxed = reply.child_value(0);
        Ok(boxed.as_variant().unwrap_or(boxed))
    }

    fn account(&self) -> Result<(String, String), glib::Error> {
        let uid = unsafe { libc::getuid() } as i64;
        let reply = self.call(ACCOUNTS, ACCOUNTS_PATH, ACCOUNTS, "FindUserById", Some((uid,).to_variant()), false)?;
        let path = reply.child_value(0).str().unwrap_or("").to_owned();
        let language = self.property(ACCOUNTS, &path, ACCOUNTS_USER, "Language")?;
        Ok((path, language.str().unwrap_or("").to_owned()))
    }
}

fn text(error: glib::Error) -> String {
    python_error_text(&error)
}

impl Services for LocaleServices {
    fn get_locale(&self) -> Result<Vec<String>, String> {
        let value = self.property(LOCALE1, LOCALE1_PATH, LOCALE1, "Locale").map_err(text)?;
        Ok(value.get::<Vec<String>>().unwrap_or_default())
    }

    fn set_locale(&self, values: &[String]) -> Result<(), String> {
        self.call(LOCALE1, LOCALE1_PATH, LOCALE1, "SetLocale", Some((values.to_vec(), true).to_variant()), true)
            .map(|_| ()).map_err(text)
    }

    fn get_account(&self) -> Result<Option<(String, String)>, String> {
        match self.account() {
            Ok(account) => Ok(Some(account)),
            Err(error) => {
                let name = gio::DBusError::remote_error(&error).map(|n| n.to_string()).unwrap_or_default();
                if ABSENT_ACCOUNT_ERRORS.contains(&name.as_str()) { Ok(None) } else { Err(text(error)) }
            }
        }
    }

    fn set_account_language(&self, path: &str, language: &str) -> Result<(), String> {
        self.call(ACCOUNTS, path, ACCOUNTS_USER, "SetLanguage", Some((language,).to_variant()), true)
            .map(|_| ()).map_err(text)
    }

    fn update_session(&self, values: &Env) -> Result<Vec<String>, String> {
        let bus = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).map_err(text)?;
        let mut warnings = Vec::new();
        let unset = vec!["LC_ALL".to_owned()];
        if let Err(error) = bus.call_sync(Some("org.freedesktop.systemd1"), "/org/freedesktop/systemd1",
                                          "org.freedesktop.systemd1.Manager", "UnsetAndSetEnvironment",
                                          Some(&(unset, values.assignments()).to_variant()), None,
                                          gio::DBusCallFlags::NONE, 5_000, None::<&gio::Cancellable>) {
            warnings.push(tr!("Background services will use the new language after login: {error}", error = error.message()));
        }
        let mut activation = values.clone();
        activation.set("LC_ALL", "");
        let entries = activation.0.iter().map(|(k, v)| glib::Variant::from_dict_entry(&k.to_variant(), &v.to_variant()));
        let dict = glib::Variant::array_from_iter_with_type(glib::VariantTy::new("{ss}").unwrap(), entries);
        if let Err(error) = bus.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                          "UpdateActivationEnvironment", Some(&glib::Variant::tuple_from_iter([dict])),
                                          None, gio::DBusCallFlags::NONE, 5_000, None::<&gio::Cancellable>) {
            warnings.push(tr!("Some newly opened applications need a new login: {error}", error = error.message()));
        }
        Ok(warnings)
    }
}
