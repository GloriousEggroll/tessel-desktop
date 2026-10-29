//! mouse_polling.py: device-reported mouse rates, independent of
//! labwc/libinput preferences.
//!
//! Read-only discovery never starts a hardware daemon. Writes use an already
//! running OpenRazer or ratbagd instance, after rechecking the physical device
//! and its state. USB endpoint intervals and libinput defaults are not current
//! mouse polling rates.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use gtk::{gio, glib};
use regex::Regex;
use tiling_manager::xml::Node;

use crate::compat::{self, Error, Result};
use crate::input_devices::InputDevice;

pub const RAZER: &str = "org.razer";
pub const MISC: &str = "razer.device.misc";
pub const RATBAG: &str = "org.freedesktop.ratbag1";
pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// An unpacked D-Bus value (what PyGObject's `Variant.unpack()` returns).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
    List(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    pub fn as_str(&self) -> &str { if let Value::Str(s) = self { s } else { "" } }
    pub fn as_list(&self) -> &[Value] { if let Value::List(l) = self { l } else { &[] } }
    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Bool(b) => *b,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Dict(d) => !d.is_empty(),
        }
    }
}

pub type Props = Vec<(String, Value)>;

pub fn prop<'a>(props: &'a Props, key: &str) -> Option<&'a Value> { props.iter().find(|(k, _)| k == key).map(|(_, v)| v) }

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PollingState {
    pub current: Option<i64>,
    pub rates: Vec<i64>,
    pub detail: String,
    pub backend: String,
    pub owner: String,
    pub target: String,
    pub physical: String,
    pub hardware: String,
    pub profile: String,
}

impl PollingState {
    pub fn detail(detail: &str) -> PollingState { PollingState { detail: detail.into(), ..Default::default() } }

    pub fn identity(&self) -> [&str; 6] {
        [&self.backend, &self.owner, &self.target, &self.physical, &self.hardware, &self.profile]
    }

    pub fn adjustable(&self) -> bool {
        !self.owner.is_empty() && !self.target.is_empty() && self.current.is_some_and(|c| c != 0) && self.rates.len() > 1
    }
}

pub fn rate(value: &Value) -> Option<i64> {
    let number = match value {
        Value::Int(i) => *i,
        Value::Bool(b) => *b as i64,
        Value::Str(s) => compat::py_int(s).ok()?,
        _ => return None,
    };
    (0 < number && number <= 65535).then_some(number)
}

pub fn rate_text(text: &str) -> Option<i64> { rate(&Value::Str(text.into())) }

pub fn rates(values: &[Value]) -> Vec<i64> { values.iter().filter_map(rate).collect::<BTreeSet<_>>().into_iter().collect() }

pub fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).map(|t| compat::strip(&t).to_owned()).unwrap_or_default()
}

static HID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^HID_ID=0005:([0-9a-f]+):([0-9a-f]+)$").unwrap());

/// A physical root and model; composite event nodes share one root.
pub fn physical_device(path: &Path) -> Option<(String, String)> {
    let path = compat::resolve(path);
    let parents: Vec<&Path> = path.ancestors().collect();
    for parent in &parents {
        let (vendor, product) = (read_text(&parent.join("idVendor")), read_text(&parent.join("idProduct")));
        if !vendor.is_empty() && !product.is_empty() {
            return Some((parent.to_string_lossy().into_owned(), format!("usb:{}:{}", vendor.to_lowercase(), product.to_lowercase())));
        }
    }
    for parent in &parents {
        if let Some(found) = HID.captures(&read_text(&parent.join("uevent"))) {
            let vendor = i64::from_str_radix(&found[1], 16).ok()?;
            let product = i64::from_str_radix(&found[2], 16).ok()?;
            return Some((parent.to_string_lossy().into_owned(), format!("bluetooth:{vendor:04x}:{product:04x}")));
        }
    }
    None
}

/// Older OpenRazer daemons omit getSupportedPollRates; Python consulted the
/// daemon's driver classes. Without the Python package there is no safe
/// answer, so the mouse stays read-only.
pub fn legacy_razer_rates(_path: &Path) -> Vec<i64> { Vec::new() }

/// The D-Bus operations PollingBackend uses (Python `Bus`).
pub trait PollingBus: Send + Sync {
    fn call(&self, owner: &str, path: &str, interface: &str, method: &str, signature: Option<&str>, values: &[Value]) -> Result<Vec<Value>>;
    fn owner(&self, name: &str) -> Result<String>;
    fn properties(&self, owner: &str, path: &str, interface: &str) -> Result<Props>;
    fn methods(&self, owner: &str, path: &str, interface: &str) -> Result<BTreeSet<String>>;
    fn commit_rate(&self, state: &PollingState, value: i64) -> Result<()>;
}

pub fn unpack(variant: &glib::Variant) -> Value {
    use glib::VariantClass as C;
    match variant.classify() {
        C::Boolean => Value::Bool(variant.get::<bool>().unwrap_or(false)),
        C::Byte => Value::Int(variant.get::<u8>().unwrap_or(0) as i64),
        C::Int16 => Value::Int(variant.get::<i16>().unwrap_or(0) as i64),
        C::Uint16 => Value::Int(variant.get::<u16>().unwrap_or(0) as i64),
        C::Int32 => Value::Int(variant.get::<i32>().unwrap_or(0) as i64),
        C::Uint32 => Value::Int(variant.get::<u32>().unwrap_or(0) as i64),
        C::Int64 => Value::Int(variant.get::<i64>().unwrap_or(0)),
        C::Uint64 => Value::Int(variant.get::<u64>().unwrap_or(0) as i64),
        C::String | C::ObjectPath | C::Signature => Value::Str(variant.str().unwrap_or("").to_owned()),
        C::Variant => variant.as_variant().map(|v| unpack(&v)).unwrap_or(Value::List(vec![])),
        C::Array if variant.type_().element().is_dict_entry() => Value::Dict(variant.iter().map(|entry| {
            let key = entry.child_value(0);
            (key.str().map(str::to_owned).unwrap_or_else(|| format!("{key}")), unpack(&entry.child_value(1)))
        }).collect()),
        C::Array | C::Tuple => Value::List(variant.iter().map(|v| unpack(&v)).collect()),
        _ => Value::Str(variant.print(false).to_string()),
    }
}

/// The real system/session bus (`Bus(system)`).
pub struct GioBus {
    pub connection: gio::DBusConnection,
}

impl GioBus {
    pub fn new(system: bool) -> Result<GioBus> {
        let kind = if system { gio::BusType::System } else { gio::BusType::Session };
        Ok(GioBus { connection: gio::bus_get_sync(kind, gio::Cancellable::NONE)? })
    }

    fn raw(&self, owner: &str, path: &str, interface: &str, method: &str, args: Option<&glib::Variant>) -> Result<glib::Variant> {
        Ok(self.connection.call_sync(Some(owner), path, interface, method, args, None, gio::DBusCallFlags::NO_AUTO_START,
                                     2000, gio::Cancellable::NONE)?)
    }
}

fn args(signature: Option<&str>, values: &[Value]) -> Result<Option<glib::Variant>> {
    use glib::prelude::ToVariant;
    Ok(match (signature, values) {
        (None, _) => None,
        (Some("(s)"), [Value::Str(s)]) => Some((s.as_str(),).to_variant()),
        (Some("(q)"), [Value::Int(i)]) => Some(((*i as u16),).to_variant()),
        (Some(other), _) => return Err(Error::Value(format!("Unsupported D-Bus arguments: {other}"))),
    })
}

impl PollingBus for GioBus {
    fn call(&self, owner: &str, path: &str, interface: &str, method: &str, signature: Option<&str>, values: &[Value]) -> Result<Vec<Value>> {
        let reply = self.raw(owner, path, interface, method, args(signature, values)?.as_ref())?;
        Ok(match unpack(&reply) { Value::List(items) => items, other => vec![other] })
    }

    fn owner(&self, name: &str) -> Result<String> {
        match self.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner", Some("(s)"),
                        &[Value::Str(name.into())]) {
            Ok(values) => Ok(values.first().map(|v| v.as_str().to_owned()).unwrap_or_default()),
            Err(Error::Glib(message)) if message.contains("org.freedesktop.DBus.Error.NameHasNoOwner") => Ok(String::new()),
            Err(error) => Err(error),
        }
    }

    fn properties(&self, owner: &str, path: &str, interface: &str) -> Result<Props> {
        let values = self.call(owner, path, PROPERTIES, "GetAll", Some("(s)"), &[Value::Str(interface.into())])?;
        Ok(match values.into_iter().next() { Some(Value::Dict(d)) => d, _ => Vec::new() })
    }

    fn methods(&self, owner: &str, path: &str, interface: &str) -> Result<BTreeSet<String>> {
        let values = self.call(owner, path, "org.freedesktop.DBus.Introspectable", "Introspect", None, &[])?;
        introspected_methods(values.first().map(|v| v.as_str()).unwrap_or(""), interface)
    }

    #[allow(deprecated)]
    fn commit_rate(&self, state: &PollingState, value: i64) -> Result<()> {
        // ratbagd Commit queues a write: wait for IsDirty to clear and catch Resync
        // (hardware write failure) on a private worker-thread main context.
        let context = glib::MainContext::new();
        let guard = context.acquire().ok();
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = failed.clone();
        let result = context.with_thread_default(|| {
            let subscription = self.connection.signal_subscribe(Some(&state.owner), Some(&format!("{RATBAG}.Device")), Some("Resync"),
                Some(&state.target), None, gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, _| flag.store(true, std::sync::atomic::Ordering::SeqCst));
            let result = commit_flow(
                |method| {
                    use glib::prelude::ToVariant;
                    if method == "Set" {
                        let args = (format!("{RATBAG}.Profile"), "ReportRate", glib::Variant::from_variant(&(value as u32).to_variant())).to_variant();
                        self.raw(&state.owner, &state.profile, PROPERTIES, "Set", Some(&args)).map(|_| Vec::new())
                    } else {
                        self.call(&state.owner, &state.target, &format!("{RATBAG}.Device"), "Commit", None, &[])
                    }
                },
                || {
                    let props = self.properties(&state.owner, &state.profile, &format!("{RATBAG}.Profile"));
                    while context.pending() {
                        context.iteration(false);
                    }
                    props
                },
                || failed.load(std::sync::atomic::Ordering::SeqCst));
            self.connection.signal_unsubscribe(subscription);
            result
        });
        drop(guard);
        result.map_err(|_| Error::Runtime("Could not use a private main context.".into()))?
    }
}

/// The ratbag commit protocol, separated from the bus for tests:
/// `call("Set")`, `call("Commit")`, then poll `properties()` until clean.
pub fn commit_flow(call: impl Fn(&str) -> Result<Vec<Value>>, properties: impl Fn() -> Result<Props>,
                   failed: impl Fn() -> bool) -> Result<()> {
    call("Set")?;
    let result = call("Commit")?;
    if result != [Value::Int(0)] {
        return compat::runtime("The mouse could not save its polling rate.");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let props = properties()?;
        if failed() {
            return compat::runtime("The mouse rejected the polling-rate change. Refresh to read its current rate.");
        }
        if !prop(&props, "IsDirty").map(Value::truthy).unwrap_or(true) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    compat::runtime("The mouse has not confirmed the polling-rate change. Refresh to check its current rate.")
}

/// `{method names}` of `interface` in introspection XML (ElementTree findall).
pub fn introspected_methods(xml: &str, interface: &str) -> Result<BTreeSet<String>> {
    let document = tiling_manager::xml::parse(xml.as_bytes(), None).map_err(|e| Error::Value(e.to_string()))?;
    let mut found = BTreeSet::new();
    for item in document.root.elements().filter(|e| e.name == "interface" && e.attribute("name") == Some(interface)) {
        for node in item.children.iter() {
            if let Node::Element(method) = node {
                if method.name == "method" {
                    found.insert(method.attribute("name").ok_or_else(|| Error::Value("'name'".into()))?.to_owned());
                }
            }
        }
    }
    Ok(found)
}

pub type BusFactory = Arc<dyn Fn(bool) -> Result<Arc<dyn PollingBus>> + Send + Sync>;
pub type LegacyRates = Arc<dyn Fn(&Path) -> Vec<i64> + Send + Sync>;

/// What PollingCard needs (the Python tests pass a fake with read/apply).
pub trait Polling: Send + Sync {
    fn read(&self, device: &InputDevice) -> Result<PollingState>;
    fn apply(&self, device: &InputDevice, expected: &PollingState, value: i64) -> Result<PollingState>;
}

#[derive(Clone)]
pub struct PollingBackend {
    pub sysfs: PathBuf,
    pub bus_factory: BusFactory,
    pub legacy_rates: LegacyRates,
}

impl Default for PollingBackend {
    fn default() -> Self { Self::new(PathBuf::from("/sys"), None, None) }
}

impl PollingBackend {
    pub fn new(sysfs: PathBuf, bus_factory: Option<BusFactory>, legacy_rates: Option<LegacyRates>) -> PollingBackend {
        PollingBackend {
            sysfs,
            bus_factory: bus_factory.unwrap_or_else(|| Arc::new(|system| Ok(Arc::new(GioBus::new(system)?) as Arc<dyn PollingBus>))),
            legacy_rates: legacy_rates.unwrap_or_else(|| Arc::new(legacy_razer_rates)),
        }
    }

    fn physical(&self, device: &InputDevice) -> Option<(String, String)> {
        let found: BTreeSet<(String, String)> = device.paths.iter()
            .filter_map(|path| {
                let name = Path::new(path).file_name()?.to_owned();
                physical_device(&self.sysfs.join("class/input").join(name).join("device"))
            })
            .collect();
        if found.len() == 1 { found.into_iter().next() } else { None }
    }

    fn glob(&self, directory: &Path, file: Option<&str>, prefix: &str) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(directory) else { return Vec::new() };
        let mut found: Vec<PathBuf> = entries.flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .map(|e| match file { Some(f) => e.path().join(f), None => e.path() })
            .filter(|p| file.is_none() || p.exists())
            .collect();
        found.sort();
        found
    }

    fn razer(&self, physical: &str, path: &Path) -> PollingState {
        let current = rate_text(&read_text(&path.join("poll_rate")));
        let state = PollingState {
            current, backend: "OpenRazer".into(), physical: physical.into(), hardware: path.to_string_lossy().into_owned(),
            detail: "OpenRazer is not running. The current rate is read from the mouse driver.".into(), ..Default::default()
        };
        let attempt = || -> Result<PollingState> {
            let bus = (self.bus_factory)(false)?;
            let owner = bus.owner(RAZER)?;
            if owner.is_empty() {
                return Ok(state.clone());
            }
            let serial = read_text(&path.join("device_serial"));
            let devices = bus.call(&owner, "/org/razer", "razer.devices", "getDevices", None, &[])?;
            let listed = devices.first().map(|d| d.as_list().iter().any(|v| v.as_str() == serial)).unwrap_or(false);
            let valid = !serial.is_empty() && serial.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if serial.is_empty() || !listed || !valid {
                return Ok(PollingState { detail: "OpenRazer has not registered this mouse. Refresh after it connects.".into(), ..state.clone() });
            }
            let target = format!("/org/razer/device/{serial}");
            let methods = bus.methods(&owner, &target, MISC)?;
            let mut current = current;
            if current.is_none() && methods.contains("getPollRate") {
                current = bus.call(&owner, &target, MISC, "getPollRate", None, &[])?.first().and_then(rate);
            }
            let mut supported = Vec::new();
            if methods.contains("setPollRate") {
                supported = if methods.contains("getSupportedPollRates") {
                    rates(bus.call(&owner, &target, MISC, "getSupportedPollRates", None, &[])?.first().map(Value::as_list).unwrap_or(&[]))
                } else {
                    (self.legacy_rates)(path)
                };
            }
            let detail = if supported.is_empty() {
                "OpenRazer does not expose supported polling-rate adjustments for this mouse."
            } else {
                "OpenRazer · Changes apply to the physical mouse, including all of its input interfaces."
            };
            Ok(PollingState { current, rates: supported, owner, target, detail: detail.into(), ..state.clone() })
        };
        match attempt() {
            Ok(result) => result,
            Err(error @ (Error::Glib(_) | Error::Value(_) | Error::Xml(_))) => {
                PollingState { detail: format!("Could not read OpenRazer capabilities: {error}"), ..state }
            }
            Err(error) => PollingState { detail: error.to_string(), ..state },
        }
    }

    fn ratbag(&self, physical: &(String, String)) -> Result<PollingState> {
        let bus = (self.bus_factory)(true)?;
        let owner = bus.owner(RATBAG)?;
        let unavailable = PollingState::detail("This mouse does not expose its current rate through an active OpenRazer or libratbag service.");
        if owner.is_empty() {
            return Ok(unavailable);
        }
        let manager = bus.properties(&owner, "/org/freedesktop/ratbag1", &format!("{RATBAG}.Manager"))?;
        if prop(&manager, "APIVersion") != Some(&Value::Int(2)) {
            return Ok(PollingState::detail("This version of libratbag is not supported for polling-rate controls."));
        }
        // The public API provides a model ID, not a physical device ID. Only match
        // when both the daemon and sysfs contain exactly one instance of it.
        let mut matches = Vec::new();
        for target in prop(&manager, "Devices").map(Value::as_list).unwrap_or(&[]) {
            let props = bus.properties(&owner, target.as_str(), &format!("{RATBAG}.Device"))?;
            let model = prop(&props, "Model").map(Value::as_str).unwrap_or("").to_lowercase();
            if model.split(':').take(3).collect::<Vec<_>>().join(":") == physical.1 {
                matches.push((target.as_str().to_owned(), props));
            }
        }
        if matches.is_empty() {
            return Ok(unavailable);
        }
        let roots: BTreeSet<Option<(String, String)>> = self.glob(&self.sysfs.join("class/input"), Some("device"), "event")
            .iter().map(|p| physical_device(p)).collect();
        let same = roots.iter().flatten().filter(|root| root.1 == physical.1).count();
        if matches.len() != 1 || same != 1 {
            return Ok(PollingState::detail("Identical mice cannot be distinguished by libratbag; polling controls are unavailable."));
        }
        let (target, device) = matches.remove(0);
        let mut profiles = Vec::new();
        for path in prop(&device, "Profiles").map(Value::as_list).unwrap_or(&[]) {
            profiles.push((path.as_str().to_owned(), bus.properties(&owner, path.as_str(), &format!("{RATBAG}.Profile"))?));
        }
        let active: Vec<&(String, Props)> = profiles.iter()
            .filter(|(_, p)| prop(p, "IsActive").is_some_and(Value::truthy) && !prop(p, "Disabled").is_some_and(Value::truthy))
            .collect();
        if active.len() != 1 {
            return Ok(PollingState::detail("The mouse has no readable active hardware profile."));
        }
        let (profile, props) = active[0];
        let dirty = profiles.iter().any(|(_, item)| prop(item, "IsDirty").map(Value::truthy).unwrap_or(true));
        Ok(PollingState {
            current: if dirty { None } else { prop(props, "ReportRate").and_then(rate) },
            rates: if dirty { Vec::new() } else { rates(prop(props, "ReportRates").map(Value::as_list).unwrap_or(&[])) },
            detail: if dirty {
                "Another mouse configuration has unsaved changes. Save or discard them there, then refresh.".into()
            } else {
                "libratbag · Changes apply to the active hardware profile.".into()
            },
            backend: "libratbag".into(), owner, target, physical: physical.0.clone(), profile: profile.clone(), hardware: String::new(),
        })
    }
}

impl Polling for PollingBackend {
    fn read(&self, device: &InputDevice) -> Result<PollingState> {
        if device.kind == "touchpad" {
            return Ok(PollingState::detail("Polling-rate controls apply to mice."));
        }
        let Some(physical) = self.physical(device) else {
            return Ok(PollingState::detail("A single connected physical mouse could not be identified for this profile."));
        };
        let razer: Vec<PathBuf> = self.glob(&self.sysfs.join("bus/hid/devices"), Some("poll_rate"), "")
            .into_iter().filter_map(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| physical_device(p).as_ref() == Some(&physical)).collect();
        if razer.len() == 1 {
            return Ok(self.razer(&physical.0, &razer[0]));
        }
        if razer.len() > 1 {
            return Ok(PollingState::detail("More than one mouse is exposed by this receiver; polling controls are unavailable."));
        }
        match self.ratbag(&physical) {
            Ok(state) => Ok(state),
            Err(error @ (Error::Glib(_) | Error::Value(_))) => Ok(PollingState::detail(&format!("Could not read the mouse polling rate: {error}"))),
            Err(error) => Err(error),
        }
    }

    fn apply(&self, device: &InputDevice, expected: &PollingState, value: i64) -> Result<PollingState> {
        let fresh = self.read(device)?;
        if fresh.identity() != expected.identity() || fresh.current != expected.current {
            return compat::runtime("The mouse or its current rate changed. Refresh before applying.");
        }
        if !fresh.adjustable() || !fresh.rates.contains(&value) {
            return compat::value("This polling rate is not supported by the connected mouse.");
        }
        if Some(value) == fresh.current {
            return Ok(fresh);
        }
        let bus = (self.bus_factory)(fresh.backend == "libratbag")?;
        match fresh.backend.as_str() {
            "OpenRazer" => {
                bus.call(&fresh.owner, &fresh.target, MISC, "setPollRate", Some("(q)"), &[Value::Int(value)])?;
            }
            "libratbag" => bus.commit_rate(&fresh, value)?,
            _ => return compat::value("No polling-rate control is available for this mouse."),
        }
        let updated = self.read(device)?;
        if updated.identity() != fresh.identity() || updated.current != Some(value) {
            return compat::runtime("The mouse did not confirm the requested polling rate. Refresh to read its current rate.");
        }
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;
    use std::sync::Mutex;

    type Call = (String, String, String, String, Option<String>, Vec<Value>);

    #[derive(Default)]
    struct Inner {
        service_owner: String,
        methods_available: BTreeSet<String>,
        allowed: Vec<i64>,
        cached_rate: i64,
        calls: Vec<Call>,
        commits: Vec<(String, i64)>,
        props: Vec<(String, Props)>,
        rate_file: Option<PathBuf>,
        fail_write: bool,
        ignore_write: bool,
    }

    struct FakeBus(Mutex<Inner>);

    impl FakeBus {
        fn new() -> Arc<FakeBus> {
            Arc::new(FakeBus(Mutex::new(Inner {
                service_owner: ":1.22".into(),
                methods_available: ["getPollRate", "setPollRate", "getSupportedPollRates"].iter().map(|s| s.to_string()).collect(),
                allowed: vec![125, 500, 1000], cached_rate: 500, ..Default::default()
            })))
        }
        fn set_prop(&self, path: &str, key: &str, value: Value) {
            let mut inner = self.0.lock().unwrap();
            let props = &mut inner.props.iter_mut().find(|(p, _)| p == path).unwrap().1;
            if let Some(slot) = props.iter_mut().find(|(k, _)| k == key) { slot.1 = value } else { props.push((key.into(), value)) }
        }
    }

    impl PollingBus for FakeBus {
        fn call(&self, owner: &str, path: &str, interface: &str, method: &str, signature: Option<&str>, values: &[Value]) -> Result<Vec<Value>> {
            let mut inner = self.0.lock().unwrap();
            inner.calls.push((owner.into(), path.into(), interface.into(), method.into(), signature.map(str::to_owned), values.to_vec()));
            match method {
                "getDevices" => Ok(vec![Value::List(vec![Value::Str("TEST_MOUSE".into())])]),
                "getPollRate" => Ok(vec![Value::Int(inner.cached_rate)]),
                "getSupportedPollRates" => Ok(vec![Value::List(inner.allowed.iter().map(|v| Value::Int(*v)).collect())]),
                "setPollRate" => {
                    if inner.fail_write {
                        return compat::runtime("USB write failed");
                    }
                    if !inner.ignore_write {
                        let Value::Int(v) = values[0] else { panic!() };
                        std::fs::write(inner.rate_file.as_ref().unwrap(), v.to_string()).unwrap();
                    }
                    Ok(vec![])
                }
                other => panic!("{other}"),
            }
        }
        fn owner(&self, _name: &str) -> Result<String> { Ok(self.0.lock().unwrap().service_owner.clone()) }
        fn properties(&self, _owner: &str, path: &str, _interface: &str) -> Result<Props> {
            Ok(self.0.lock().unwrap().props.iter().find(|(p, _)| p == path).map(|(_, v)| v.clone()).unwrap_or_default())
        }
        fn methods(&self, _o: &str, _p: &str, _i: &str) -> Result<BTreeSet<String>> { Ok(self.0.lock().unwrap().methods_available.clone()) }
        fn commit_rate(&self, state: &PollingState, value: i64) -> Result<()> {
            self.0.lock().unwrap().commits.push((state.profile.clone(), value));
            self.set_prop(&state.profile, "ReportRate", Value::Int(value));
            Ok(())
        }
    }

    struct Fixture {
        root: PathBuf,
        bus: Arc<FakeBus>,
        legacy: Arc<Mutex<Vec<PathBuf>>>,
        backend: PollingBackend,
        hid: PathBuf,
        device: InputDevice,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let root = scratch(name);
            let bus = FakeBus::new();
            let legacy = Arc::new(Mutex::new(Vec::new()));
            let (b, l) = (bus.clone(), legacy.clone());
            let backend = PollingBackend::new(root.clone(), Some(Arc::new(move |_| Ok(b.clone() as Arc<dyn PollingBus>))),
                Some(Arc::new(move |path: &Path| {
                    l.lock().unwrap().push(path.to_path_buf());
                    vec![125, 500, 1000]
                })));
            let mut fixture = Fixture { root, bus, legacy, backend, hid: PathBuf::new(),
                                        device: InputDevice { paths: vec!["/dev/input/event1".into()], ..InputDevice::new("Mouse", "non-touch") } };
            fixture.hid = fixture.mouse("port1", "event1", true);
            fixture
        }

        fn mouse(&self, port: &str, event: &str, razer: bool) -> PathBuf {
            let usb = self.root.join("devices").join(port);
            std::fs::create_dir_all(&usb).unwrap();
            std::fs::write(usb.join("idVendor"), "1532").unwrap();
            std::fs::write(usb.join("idProduct"), "00cf").unwrap();
            let hid = usb.join("0003:1532:00CF.0001");
            std::fs::create_dir_all(&hid).unwrap();
            if razer {
                std::fs::write(hid.join("poll_rate"), "1000").unwrap();
                std::fs::write(hid.join("device_serial"), "TEST_MOUSE").unwrap();
                let link = self.root.join("bus/hid/devices").join(hid.file_name().unwrap());
                std::fs::create_dir_all(link.parent().unwrap()).unwrap();
                if !link.exists() {
                    std::os::unix::fs::symlink(&hid, &link).unwrap();
                }
                self.bus.0.lock().unwrap().rate_file = Some(hid.join("poll_rate"));
            }
            let node = self.root.join("class/input").join(event);
            std::fs::create_dir_all(&node).unwrap();
            std::os::unix::fs::symlink(&hid, node.join("device")).unwrap();
            hid
        }

        fn writes(&self) -> Vec<Call> { self.bus.0.lock().unwrap().calls.iter().filter(|c| c.3 == "setPollRate").cloned().collect() }

        fn read(&self) -> PollingState { self.backend.read(&self.device).unwrap() }

        fn ratbag(&self) {
            std::fs::remove_file(self.hid.join("poll_rate")).unwrap();
            let int = Value::Int;
            self.bus.0.lock().unwrap().props = vec![
                ("/org/freedesktop/ratbag1".into(), vec![("APIVersion".into(), int(2)), ("Devices".into(), Value::List(vec![Value::Str("/mouse".into())]))]),
                ("/mouse".into(), vec![("Model".into(), Value::Str("usb:1532:00cf:0".into())),
                                       ("Profiles".into(), Value::List(vec![Value::Str("/profile".into()), Value::Str("/other".into())]))]),
                ("/profile".into(), vec![("IsActive".into(), Value::Bool(true)), ("IsDirty".into(), Value::Bool(false)),
                                         ("ReportRate".into(), int(1000)), ("ReportRates".into(), Value::List(vec![int(500), int(1000)]))]),
                ("/other".into(), vec![("IsActive".into(), Value::Bool(false)), ("IsDirty".into(), Value::Bool(false)),
                                       ("ReportRate".into(), int(125)), ("ReportRates".into(), Value::List(vec![int(125)]))]),
            ];
        }
    }

    #[test]
    fn current_is_hardware_value_not_daemon_cached_or_factory_value() {
        let f = Fixture::new("poll-current");
        let state = f.read();
        assert_eq!(state.current, Some(1000)); // daemon cache deliberately says 500
        assert_eq!(state.rates, [125, 500, 1000]);
        assert!(state.adjustable());
        assert!(f.writes().is_empty());
    }

    #[test]
    fn legacy_capabilities_are_only_used_if_query_is_missing() {
        let f = Fixture::new("poll-legacy");
        f.read();
        assert!(f.legacy.lock().unwrap().is_empty());
        f.bus.0.lock().unwrap().methods_available.remove("getSupportedPollRates");
        assert_eq!(f.read().rates, [125, 500, 1000]);
        assert_eq!(f.legacy.lock().unwrap().len(), 1);
        f.bus.0.lock().unwrap().methods_available.remove("setPollRate");
        assert!(!f.read().adjustable());
    }

    #[test]
    fn valid_rate_uses_daemon_and_is_read_back() {
        let f = Fixture::new("poll-valid");
        let state = f.backend.apply(&f.device, &f.read(), 500).unwrap();
        assert_eq!(state.current, Some(500));
        assert_eq!(f.writes(), vec![(":1.22".into(), "/org/razer/device/TEST_MOUSE".into(), MISC.into(), "setPollRate".into(),
                                     Some("(q)".into()), vec![Value::Int(500)])]);
    }

    #[test]
    fn unsupported_rate_and_external_change_never_write() {
        let f = Fixture::new("poll-unsupported");
        let initial = f.read();
        assert!(f.backend.apply(&f.device, &initial, 250).unwrap_err().is_value());
        std::fs::write(f.hid.join("poll_rate"), "500").unwrap();
        assert!(f.backend.apply(&f.device, &initial, 125).unwrap_err().message().contains("changed"));
        assert!(f.writes().is_empty());
    }

    #[test]
    fn daemon_restart_and_disconnection_never_write() {
        let f = Fixture::new("poll-restart");
        let initial = f.read();
        f.bus.0.lock().unwrap().service_owner = ":1.23".into();
        assert!(f.backend.apply(&f.device, &initial, 125).unwrap_err().message().contains("changed"));
        f.bus.0.lock().unwrap().service_owner = ":1.22".into();
        std::fs::remove_file(f.root.join("class/input/event1/device")).unwrap();
        assert!(f.backend.apply(&f.device, &initial, 125).unwrap_err().message().contains("changed"));
        assert!(!f.read().adjustable());
        assert!(f.writes().is_empty());
    }

    #[test]
    fn write_failure_and_unconfirmed_write_are_not_success() {
        let f = Fixture::new("poll-failure");
        let initial = f.read();
        f.bus.0.lock().unwrap().fail_write = true;
        let error = f.backend.apply(&f.device, &initial, 500).unwrap_err();
        assert!(error.is_runtime() && error.message().contains("USB write failed"));
        f.bus.0.lock().unwrap().fail_write = false;
        f.bus.0.lock().unwrap().ignore_write = true;
        assert!(f.backend.apply(&f.device, &initial, 500).unwrap_err().message().contains("did not confirm"));
        assert_eq!(f.read().current, Some(1000));
    }

    #[test]
    fn without_daemon_driver_value_is_read_only() {
        let f = Fixture::new("poll-nodaemon");
        f.bus.0.lock().unwrap().service_owner = String::new();
        let state = f.read();
        assert_eq!(state.current, Some(1000));
        assert!(!state.adjustable());
        assert!(f.bus.0.lock().unwrap().calls.is_empty());
    }

    #[test]
    fn composite_interfaces_share_rate_but_identical_devices_are_not_merged() {
        let mut f = Fixture::new("poll-composite");
        f.mouse("port1", "event2", true);
        f.device.paths.push("/dev/input/event2".into());
        assert_eq!(f.read().current, Some(1000));
        f.mouse("port2", "event3", true);
        f.device.paths.push("/dev/input/event3".into());
        assert!(!f.read().adjustable());
    }

    #[test]
    fn ratbag_reads_active_profile_and_does_not_commit_other_edits() {
        let f = Fixture::new("poll-ratbag-read");
        f.ratbag();
        let state = f.read();
        assert_eq!(state.current, Some(1000));
        assert!(state.adjustable());
        f.bus.set_prop("/other", "IsDirty", Value::Bool(true));
        assert_eq!(f.read().current, None);
        assert!(f.backend.apply(&f.device, &state, 500).unwrap_err().is_runtime());
        assert!(f.bus.0.lock().unwrap().calls.is_empty());
        assert!(f.bus.0.lock().unwrap().commits.is_empty());
    }

    #[test]
    fn ratbag_change_commits_and_requeries() {
        let f = Fixture::new("poll-ratbag-commit");
        f.ratbag();
        let state = f.backend.apply(&f.device, &f.read(), 500).unwrap();
        assert_eq!(state.current, Some(500));
        assert_eq!(f.bus.0.lock().unwrap().commits, vec![("/profile".to_string(), 500)]);
    }

    #[test]
    fn ratbag_refuses_unknown_api_duplicate_models_and_active_profile_change() {
        let f = Fixture::new("poll-ratbag-refuse");
        f.ratbag();
        let original = f.read();
        f.bus.set_prop("/profile", "IsActive", Value::Bool(false));
        f.bus.set_prop("/other", "IsActive", Value::Bool(true));
        assert!(f.backend.apply(&f.device, &original, 500).unwrap_err().is_runtime());
        f.bus.set_prop("/org/freedesktop/ratbag1", "APIVersion", Value::Int(3));
        assert!(!f.read().adjustable());
        f.bus.set_prop("/org/freedesktop/ratbag1", "APIVersion", Value::Int(2));
        f.mouse("port2", "event2", false);
        assert!(f.read().detail.contains("Identical"));
    }

    #[test]
    fn ratbag_unknown_rate_is_not_filled_in_from_supported_rates() {
        let f = Fixture::new("poll-ratbag-zero");
        f.ratbag();
        f.bus.set_prop("/profile", "ReportRate", Value::Int(0));
        let state = f.read();
        assert_eq!(state.current, None);
        assert!(!state.adjustable());
    }

    fn exercise(fail: bool) -> Result<Vec<String>> {
        let calls = Mutex::new(Vec::new());
        let failed = std::cell::Cell::new(false);
        let result = commit_flow(|method| {
            calls.lock().unwrap().push(method.to_owned());
            if method == "Commit" {
                if fail {
                    failed.set(true);
                }
                return Ok(vec![Value::Int(0)]);
            }
            Ok(vec![])
        }, || Ok(vec![("IsDirty".into(), Value::Bool(false))]), || failed.get());
        result.map(|_| calls.into_inner().unwrap())
    }

    #[test]
    fn commit_success_sets_then_commits() { assert_eq!(exercise(false).unwrap(), ["Set", "Commit"]); }

    #[test]
    fn resync_signal_reports_failed_hardware_write() {
        assert!(exercise(true).unwrap_err().message().contains("rejected"));
    }

    #[test]
    fn introspection_methods_and_physical_bluetooth() {
        let xml = "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\" \"x\">\n<node><interface name=\"razer.device.misc\"><method name=\"getPollRate\"/><method name=\"setPollRate\"><arg type=\"q\"/></method></interface><interface name=\"other\"><method name=\"x\"/></interface></node>";
        assert_eq!(introspected_methods(xml, MISC).unwrap().into_iter().collect::<Vec<_>>(), ["getPollRate", "setPollRate"]);
        let root = scratch("poll-bt");
        let dev = root.join("bt/hid/input5");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(root.join("bt/hid/uevent"), "DRIVER=x\nHID_ID=0005:0000046D:0000B023\n").unwrap();
        let found = physical_device(&dev).unwrap();
        assert_eq!(found.1, "bluetooth:046d:b023");
        assert!(found.0.ends_with("bt/hid"));
    }
}
