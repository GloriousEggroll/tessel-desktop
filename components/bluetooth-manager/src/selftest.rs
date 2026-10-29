//! `--selftest`: system bus, GetManagedObjects (or the blocking introspection
//! fallback), adapters ≥ 1 and complete catalogs. Read-only BlueZ calls only.
//! Prints `PASS/FAIL name — extra` lines and `selftest PASSED/FAILED`.

use crate::backend::*;
use crate::dbus::BtError;
use crate::i18n;
use glib::Variant;
use serde_json::Value;

struct Checks {
    ok: bool,
    out: String,
}

impl Checks {
    fn check(&mut self, name: &str, cond: bool, extra: &str) {
        let suffix = if extra.is_empty() { String::new() } else { format!(" — {extra}") };
        self.out.push_str(&format!("{} {name}{suffix}\n", if cond { "PASS" } else { "FAIL" }));
        print!("{} {name}{suffix}\n", if cond { "PASS" } else { "FAIL" });
        if !cond {
            self.ok = false;
        }
    }
}

fn call_sync(conn: &gio::DBusConnection, path: &str, iface: &str, method: &str, params: Option<Variant>, timeout: i32)
             -> Result<Variant, BtError> {
    let params = params.unwrap_or_else(|| ().into());
    conn.call_sync(Some(BLUEZ), path, iface, method, Some(&params), None, gio::DBusCallFlags::NONE, timeout,
                   None::<&gio::Cancellable>).map_err(|e| BtError::from_glib(&e))
}

fn introspect_children(conn: &gio::DBusConnection, path: &str) -> Vec<String> {
    match call_sync(conn, path, INTROSPECT_IFACE, "Introspect", None, 10000) {
        Ok(reply) => node_names(reply.child_value(0).str().unwrap_or_default()),
        Err(_) => Vec::new(),
    }
}

fn get_all(conn: &gio::DBusConnection, path: &str, iface: &str) -> Option<Props> {
    call_sync(conn, path, PROPS_IFACE, "GetAll", Some((iface,).into()), 10000).ok().map(|r| first_props(&r))
}

/// Python `repr(str)` / `repr(list)` for the i18n lines.
fn repr(value: &Value) -> String {
    suite_core::pyjson::py_repr_value(value)
}

pub fn run() -> i32 {
    let mut checks = Checks { ok: true, out: String::new() };
    let conn = match gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>) {
        Ok(conn) => {
            checks.check("system bus", true, "");
            conn
        }
        Err(error) => {
            checks.check("system bus", false, error.message());
            return 1;
        }
    };
    let bt = Bt::new(crate::dbus::GioConn::new(conn.clone()));
    let mut om_available = true;
    match call_sync(&conn, BLUEZ_ROOT, OBJMGR_IFACE, "GetManagedObjects", None, DEFAULT_TIMEOUT) {
        Ok(reply) => {
            bt.apply_managed_objects(&reply);
            checks.check("discovery (object manager)", true, "");
        }
        Err(error) => {
            // Not fatal: some BlueZ builds lack ObjectManager entirely.
            om_available = false;
            println!("INFO object manager unavailable ({}) — using introspection fallback", short_err(&error));
            bt.has_object_manager.set(false);
            for child in introspect_children(&conn, BLUEZ_ROOT) {
                let ad_path = format!("{BLUEZ_ROOT}/{child}");
                let Some(props) = get_all(&conn, &ad_path, ADAPTER_IFACE) else { continue };
                bt.adapters.borrow_mut().insert(&ad_path, adapter_from_props(&props));
                for dev in introspect_children(&conn, &ad_path) {
                    let dev_path = format!("{ad_path}/{dev}");
                    if bt.devices.borrow().contains(&dev_path) {
                        continue;
                    }
                    if let Some(props) = get_all(&conn, &dev_path, DEVICE_IFACE) {
                        bt.devices.borrow_mut().insert(&dev_path, dev_from_props(&props));
                    }
                }
            }
        }
    }
    let n_ad = bt.adapters.borrow().len();
    let n_dev = bt.devices.borrow().len();
    if !om_available {
        checks.check("discovery (introspect fallback)", n_ad >= 1, &format!("{n_ad} adapter(s)"));
    }
    checks.check("adapters found", n_ad >= 1, &format!("{n_ad} adapter(s)"));
    checks.check("devices listed", true, &format!("{n_dev} device(s)"));
    let first = bt.adapters.borrow().0.first().cloned();
    if let Some((_, ad)) = first {
        let name = if ad.alias.is_empty() { ad.address.clone() } else { ad.alias.clone() };
        let py = |b: bool| if b { "True" } else { "False" };
        checks.check("default adapter", true, &format!("{name} powered={} discovering={}", py(ad.powered), py(ad.discovering)));
    }
    let ids = i18n::messids();
    for lang in i18n::available_languages() {
        let catalog: std::collections::HashMap<String, String> = i18n::catalog_for(lang).into_iter().collect();
        let missing: Vec<Value> = ids.iter().filter(|m| catalog.get(*m).map(|s| s.trim()).unwrap_or("").is_empty())
            .map(|m| Value::String(m.clone())).collect();
        let sample = i18n::tr_in(lang, "Scan");
        let extra = if missing.is_empty() { format!("translated={}", repr(&Value::String(sample))) }
                    else { format!("missing={}", repr(&Value::Array(missing.clone()))) };
        checks.check(&format!("i18n {lang}"), missing.is_empty(), &extra);
    }
    println!("\n{}", if checks.ok { "selftest PASSED" } else { "selftest FAILED" });
    let _ = &checks.out;
    if checks.ok { 0 } else { 1 }
}
