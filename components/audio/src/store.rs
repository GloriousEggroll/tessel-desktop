//! Atomic, versioned storage for the virtual outputs managed by Audio Settings
//! (loopback_store.py). Error strings are English and untranslated, as in Python.

use crate::model::Loopback;
use tessel_core::pyjson::{casefold, py_strip};
use crate::timer::{self, Timeout};
use gio::prelude::*;
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const MAX_ENTRIES: usize = 64;
pub const MAX_CHARS: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(message: impl Into<String>) -> Result<T, StoreError> {
    Err(StoreError(message.into()))
}

pub type Entries = IndexMap<String, Loopback>;

enum Failure {
    Store(StoreError),
    Io(io::Error),
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        Failure::Store(error)
    }
}

use tessel_core::pyerr::strerror;

fn text(value: Option<&Value>, field: &str, limit: usize) -> Result<String, StoreError> {
    let Some(Value::String(value)) = value else { return err(format!("The loopback {field} must be text.")) };
    let value = py_strip(value);
    if value.is_empty() || value.chars().count() > limit || value.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
        return err(format!("The loopback {field} must contain 1–{limit} characters without control characters."));
    }
    Ok(value.to_owned())
}

/// `str(uuid.UUID(value))`, or None where Python raises.
pub fn canonical_uuid(value: &str) -> Option<String> {
    let hex = value.replace("urn:", "").replace("uuid:", "");
    let hex = hex.trim_matches(['{', '}']).replace('-', "");
    if hex.chars().count() != 32 {
        return None;
    }
    // int(hex, 16): whitespace, sign, 0x prefix and single underscores are accepted.
    let mut digits = py_strip(&hex);
    if let Some(rest) = digits.strip_prefix('+') {
        digits = rest;
    } else if digits.starts_with('-') {
        let rest = digits[1..].trim_start_matches("0x").replace('_', "");
        return (!rest.is_empty() && rest.chars().all(|c| c.is_ascii_hexdigit()) && rest.chars().all(|c| c == '0'))
            .then(|| "00000000-0000-0000-0000-000000000000".into());
    }
    let digits = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")).unwrap_or(digits);
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let digits: String = digits.chars().filter(|&c| c != '_').collect();
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let number = u128::from_str_radix(&digits, 16).ok()?;
    let h = format!("{number:032x}");
    Some(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

/// A random version-4 UUID (`uuid.uuid4()`).
pub fn uuid4() -> String {
    let mut bytes = [0u8; 16];
    let mut filled = 0;
    while filled < 16 {
        let n = unsafe { libc::getrandom(bytes[filled..].as_mut_ptr().cast(), 16 - filled, 0) };
        if n > 0 {
            filled += n as usize;
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    canonical_uuid(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()).unwrap()
}

/// `validate_entry(value)`.
pub fn validate_entry(value: &Value) -> Result<Loopback, StoreError> {
    let keys = ["id", "name", "target_name", "target_description"];
    let Value::Object(map) = value else { return err("The saved loopback has missing or unknown fields.") };
    if map.len() != keys.len() || !keys.iter().all(|k| map.contains_key(*k)) {
        return err("The saved loopback has missing or unknown fields.");
    }
    let original = map["id"].as_str();
    let Some(identifier) = original.and_then(canonical_uuid) else {
        return err("The saved loopback has an invalid identifier.");
    };
    if Some(identifier.as_str()) != original {
        return err("The saved loopback identifier is not canonical.");
    }
    Ok(Loopback {
        id: identifier,
        name: text(map.get("name"), "name", 120)?,
        target_name: text(map.get("target_name"), "target", 1024)?,
        target_description: text(map.get("target_description"), "target description", 512)?,
    })
}

pub fn entry_json(entry: &Loopback) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), entry.id.clone().into());
    map.insert("name".into(), entry.name.clone().into());
    map.insert("target_name".into(), entry.target_name.clone().into());
    map.insert("target_description".into(), entry.target_description.clone().into());
    Value::Object(map)
}

/// `Path.with_suffix(suffix)`.
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = match name.rfind('.') {
        Some(0) | None => name.as_str(),
        Some(i) if i == name.len() - 1 => name.as_str(),
        Some(i) => &name[..i],
    };
    path.with_file_name(format!("{stem}{suffix}"))
}

pub type ReplaceFn = fn(&Path, &Path) -> io::Result<()>;

#[derive(Clone, Debug)]
pub struct LoopbackStore {
    pub path: PathBuf,
    /// `os.replace` (tests substitute a failing one).
    pub(crate) replace: ReplaceFn,
}

impl Default for LoopbackStore {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LoopbackStore {
    /// Default `~/.config/tessel/audio/loopbacks.json`.
    pub fn new(path: Option<PathBuf>) -> Self {
        let path = path.unwrap_or_else(|| tessel_core::paths::component_config_dir("audio").join("loopbacks.json"));
        Self { path, replace: |a, b| fs::rename(a, b) }
    }

    fn directory(&self) -> PathBuf {
        match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_owned(),
            _ => PathBuf::from("."),
        }
    }

    pub fn ensure_directory(&self) -> io::Result<()> {
        let directory = self.directory();
        match fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory) {
            Ok(()) => Ok(()),
            Err(_) if directory.is_dir() => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn load(&self) -> Result<Entries, StoreError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entries::new()),
            Err(error) => return err(format!("Cannot read saved loopbacks: {}", strerror(&error))),
        };
        // Bound malformed files before parsing them on the main loop: Python
        // reads at most 256 KiB + 1 *characters*.
        let mut bytes = Vec::new();
        if let Err(error) = Read::by_ref(&mut file).take(((MAX_CHARS + 1) * 4) as u64).read_to_end(&mut bytes) {
            return err(format!("Cannot read saved loopbacks: {}", strerror(&error)));
        }
        let content = match std::str::from_utf8(&bytes) {
            Ok(text) => text.to_owned(),
            Err(error) => {
                let valid = std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap();
                if valid.chars().count() > MAX_CHARS || error.error_len().is_none() && bytes.len() as u64 == ((MAX_CHARS + 1) * 4) as u64 {
                    valid.to_owned()
                } else {
                    return err("The saved loopback file is not valid UTF-8 JSON.");
                }
            }
        };
        if content.chars().count() > MAX_CHARS {
            return err("The saved loopback file is too large.");
        }
        let data: Value = match serde_json::from_str(&content) {
            Ok(data) => data,
            Err(_) => return err("The saved loopback file is not valid JSON."),
        };
        let valid_format = match &data {
            Value::Object(map) => {
                map.len() == 2
                    && map.contains_key("loopbacks")
                    && map.get("version").and_then(|v| v.as_i64().filter(|_| v.is_i64() || v.is_u64())) == Some(1)
            }
            _ => false,
        };
        if !valid_format {
            return err("The saved loopback file has an unsupported format or version.");
        }
        let Some(Value::Array(entries)) = data.get("loopbacks") else {
            return err("The saved loopback file must contain at most 64 loopbacks.");
        };
        if entries.len() > MAX_ENTRIES {
            return err("The saved loopback file must contain at most 64 loopbacks.");
        }
        let mut result = Entries::new();
        let mut names = std::collections::HashSet::new();
        for value in entries {
            let entry = validate_entry(value)?;
            let folded = casefold(&entry.name);
            if result.contains_key(&entry.id) || names.contains(&folded) {
                return err("The saved loopback file contains duplicate loopbacks.");
            }
            names.insert(folded);
            result.insert(entry.id.clone(), entry);
        }
        Ok(result)
    }

    fn locked<T>(&self, body: impl FnOnce() -> Result<T, Failure>) -> Result<T, StoreError> {
        let wrap = |error: io::Error| StoreError(format!("Cannot save loopbacks: {}", strerror(&error)));
        self.ensure_directory().map_err(wrap)?;
        let lock = OpenOptions::new().create(true).read(true).write(true).append(true).mode(0o600)
            .open(with_suffix(&self.path, ".lock")).map_err(wrap)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(wrap(io::Error::last_os_error()));
        }
        // OSErrors raised inside the locked block become "Cannot save loopbacks".
        body().map_err(|failure| match failure {
            Failure::Store(error) => error,
            Failure::Io(error) => wrap(error),
        })
    }

    fn write(&self, entries: &Entries) -> io::Result<()> {
        let mut data = Map::new();
        data.insert("version".into(), 1.into());
        data.insert("loopbacks".into(), Value::Array(entries.values().map(entry_json).collect()));
        let style = tessel_core::pyfmt::JsonStyle { ensure_ascii: false, indent: Some(2), ..tessel_core::pyfmt::JsonStyle::DEFAULT };
        let payload = tessel_core::pyfmt::json_dumps(&Value::Object(data), &style) + "\n";
        let directory = self.directory();
        let (temporary, mut file) = loop {
            let name = format!(".loopbacks-{}.json", &uuid4().replace('-', "")[..8]);
            let path = directory.join(name);
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
                Ok(file) => break (path, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let result = (|| {
            file.write_all(payload.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            (self.replace)(&temporary, &self.path)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        File::open(&directory)?.sync_all()
    }

    pub fn add(&self, name: &str, target_name: &str, target_description: &str) -> Result<Loopback, StoreError> {
        let mut map = Map::new();
        map.insert("id".into(), uuid4().into());
        map.insert("name".into(), name.into());
        map.insert("target_name".into(), target_name.into());
        map.insert("target_description".into(), target_description.into());
        let entry = validate_entry(&Value::Object(map))?;
        self.locked(|| {
            let mut entries = self.load()?;
            if entries.len() >= MAX_ENTRIES {
                return Err(StoreError("At most 64 loopbacks can be saved.".into()).into());
            }
            let folded = casefold(&entry.name);
            if entries.values().any(|current| casefold(&current.name) == folded) {
                return Err(StoreError("A loopback with that name already exists. Choose another name.".into()).into());
            }
            entries.insert(entry.id.clone(), entry.clone());
            self.write(&entries).map_err(Failure::Io)
        })?;
        Ok(entry)
    }

    pub fn remove(&self, identifier: &str) -> Result<(), StoreError> {
        self.locked(|| {
            let mut entries = self.load()?;
            if entries.shift_remove(identifier).is_none() {
                return Err(StoreError("This saved loopback no longer exists.".into()).into());
            }
            self.write(&entries).map_err(Failure::Io)
        })
    }
}

/// Watch the directory so atomic file replacements also trigger a reload;
/// events are debounced 100 ms.
pub struct LoopbackMonitor {
    monitor: gio::FileMonitor,
    handler: RefCell<Option<glib::SignalHandlerId>>,
    source: Rc<RefCell<Option<Timeout>>>,
}

impl LoopbackMonitor {
    pub fn new(store: &LoopbackStore, changed: impl Fn() + 'static) -> Result<Self, String> {
        store.ensure_directory().map_err(|e| strerror(&e))?;
        let monitor = gio::File::for_path(store.directory())
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>)
            .map_err(|e| e.message().to_owned())?;
        let name = store.path.file_name().map(|n| n.to_owned()).unwrap_or_default();
        let source: Rc<RefCell<Option<Timeout>>> = Rc::default();
        let pending = source.clone();
        let changed = Rc::new(changed);
        let handler = monitor.connect_changed(move |_, file, other, _| {
            let matches = |f: Option<&gio::File>| f.and_then(|f| f.basename()).is_some_and(|b| b.as_os_str() == name.as_os_str());
            if !(matches(Some(file)) || matches(other)) {
                return;
            }
            if pending.borrow().is_none() {
                let (slot, changed) = (pending.clone(), changed.clone());
                *pending.borrow_mut() = Some(timer::once(100, move || {
                    slot.borrow_mut().take();
                    changed();
                }));
            }
        });
        Ok(Self { monitor, handler: RefCell::new(Some(handler)), source })
    }

    pub fn close(&self) {
        if let Some(source) = self.source.borrow_mut().take() {
            source.cancel();
        }
        if let Some(handler) = self.handler.borrow_mut().take() {
            self.monitor.disconnect(handler);
        }
        self.monitor.cancel();
    }
}

impl Drop for LoopbackMonitor {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!("am-test-{tag}-{}-{}", std::process::id(), uuid4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn store(tag: &str) -> (PathBuf, LoopbackStore) {
        let dir = temp_dir(tag);
        let store = LoopbackStore::new(Some(dir.join("audio").join("loopbacks.json")));
        (dir, store)
    }

    #[test]
    fn add_roundtrip_and_remove_survive_new_store_instance() {
        let (dir, store) = store("roundtrip");
        assert!(store.load().unwrap().is_empty());
        let first = store.add("Headset", "bluez_output.headset", "Bluetooth Headset").unwrap();
        let second = store.add("Speakers", "alsa_output.speakers", "Speakers").unwrap();
        let fresh = LoopbackStore::new(Some(store.path.clone())).load().unwrap();
        assert_eq!(fresh, Entries::from([(first.id.clone(), first.clone()), (second.id.clone(), second.clone())]));
        LoopbackStore::new(Some(store.path.clone())).remove(&first.id).unwrap();
        assert_eq!(store.load().unwrap(), Entries::from([(second.id.clone(), second)]));
        assert_eq!(fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(store.path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(with_suffix(&store.path, ".lock").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_format_matches_python_json_dumps() {
        let (dir, store) = store("format");
        let entry = store.add("Ünïcode “mix”", "sink", "Speakers").unwrap();
        let text = fs::read_to_string(&store.path).unwrap();
        assert_eq!(text, format!("{{\n  \"version\": 1,\n  \"loopbacks\": [\n    {{\n      \"id\": \"{}\",\n      \"name\": \"Ünïcode “mix”\",\n      \"target_name\": \"sink\",\n      \"target_description\": \"Speakers\"\n    }}\n  ]\n}}\n", entry.id));
        assert_eq!(&entry.id[14..15], "4");
        store.remove(&entry.id).unwrap();
        assert_eq!(fs::read_to_string(&store.path).unwrap(), "{\n  \"version\": 1,\n  \"loopbacks\": []\n}\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_names_invalid_names_and_unknown_removals_preserve_file() {
        let (dir, store) = store("dupes");
        store.add("Headset", "headset", "Headset").unwrap();
        let original = fs::read(&store.path).unwrap();
        for name in ["HEADSET", "", "  ", "bad\nname", &"x".repeat(121)] {
            assert!(store.add(name, "headset", "Headset").is_err(), "{name:?}");
        }
        assert_eq!(store.add("HEADSET", "h", "h").unwrap_err().0, "A loopback with that name already exists. Choose another name.");
        assert_eq!(store.add("bad\nname", "h", "h").unwrap_err().0, "The loopback name must contain 1–120 characters without control characters.");
        assert_eq!(store.remove("foreign-node-name").unwrap_err().0, "This saved loopback no longer exists.");
        assert_eq!(fs::read(&store.path).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_or_unsupported_config_is_not_overwritten() {
        let (dir, store) = store("malformed");
        store.ensure_directory().unwrap();
        for content in ["{\"version\": 2, \"loopbacks\": []}", "{\"version\": 1, \"loopbacks\": [null]}",
                        "{\"version\": true, \"loopbacks\": []}", "{\"version\": 1.0, \"loopbacks\": []}", "{unfinished", "[]"] {
            fs::write(&store.path, content).unwrap();
            assert!(store.load().is_err(), "{content}");
            assert!(store.add("Headset", "headset", "Headset").is_err());
            assert_eq!(fs::read_to_string(&store.path).unwrap(), content);
        }
        fs::write(&store.path, b"\xff").unwrap();
        assert_eq!(store.load().unwrap_err().0, "The saved loopback file is not valid UTF-8 JSON.");
        fs::write(&store.path, "{unfinished").unwrap();
        assert_eq!(store.load().unwrap_err().0, "The saved loopback file is not valid JSON.");
        fs::write(&store.path, " ".repeat(MAX_CHARS + 1)).unwrap();
        assert_eq!(store.load().unwrap_err().0, "The saved loopback file is too large.");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn entry_validation_messages() {
        let ok = serde_json::json!({"id": "532aa4b1-bd34-4d0c-8a4e-34580ebf1228", "name": " A ", "target_name": "t", "target_description": "d"});
        assert_eq!(validate_entry(&ok).unwrap().name, "A");
        let mut upper = ok.clone();
        upper["id"] = "532AA4B1-BD34-4D0C-8A4E-34580EBF1228".into();
        assert_eq!(validate_entry(&upper).unwrap_err().0, "The saved loopback identifier is not canonical.");
        let mut bad = ok.clone();
        bad["id"] = "nope".into();
        assert_eq!(validate_entry(&bad).unwrap_err().0, "The saved loopback has an invalid identifier.");
        bad["id"] = 5.into();
        assert_eq!(validate_entry(&bad).unwrap_err().0, "The saved loopback has an invalid identifier.");
        let mut extra = ok.clone();
        extra["x"] = 1.into();
        assert_eq!(validate_entry(&extra).unwrap_err().0, "The saved loopback has missing or unknown fields.");
        let mut target = ok.clone();
        target["target_name"] = 3.into();
        assert_eq!(validate_entry(&target).unwrap_err().0, "The loopback target must be text.");
        assert_eq!(canonical_uuid("{532AA4B1BD344D0C8A4E34580EBF1228}").as_deref(), Some("532aa4b1-bd34-4d0c-8a4e-34580ebf1228"));
        assert_eq!(with_suffix(Path::new("/a/loopbacks.json"), ".service.lock"), Path::new("/a/loopbacks.service.lock"));
        assert_eq!(with_suffix(Path::new("/a/config"), ".lock"), Path::new("/a/config.lock"));
    }

    #[test]
    fn atomic_replace_failure_keeps_previous_config() {
        let (dir, mut store) = store("replace");
        store.add("Headset", "headset", "Headset").unwrap();
        let original = fs::read(&store.path).unwrap();
        store.replace = |_, _| Err(io::Error::other("simulated full disk"));
        assert_eq!(store.add("Speakers", "speakers", "Speakers").unwrap_err().0, "Cannot save loopbacks: simulated full disk");
        assert_eq!(fs::read(&store.path).unwrap(), original);
        let leftovers: Vec<_> = fs::read_dir(store.path.parent().unwrap()).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".loopbacks-")).collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_writers_do_not_lose_saved_entries() {
        let (dir, store) = store("concurrent");
        let path = store.path.clone();
        let handles: Vec<_> = (0..12).map(|index| {
            let path = path.clone();
            std::thread::spawn(move || LoopbackStore::new(Some(path)).add(&format!("Output {index}"), &format!("sink.{index}"), &format!("Sink {index}")).unwrap())
        }).collect();
        let entries: Vec<Loopback> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 12);
        for entry in entries {
            assert_eq!(loaded[&entry.id], entry);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_monitor_observes_atomic_replacement_and_deletion() {
        let context = glib::MainContext::new();
        context.with_thread_default(|| {
            let (dir, store) = store("monitor");
            let observed: Rc<RefCell<Vec<Entries>>> = Rc::default();
            let (sink, reader) = (observed.clone(), store.clone());
            let monitor = LoopbackMonitor::new(&store, move || sink.borrow_mut().push(reader.load().unwrap())).unwrap();
            let first = store.add("Headset", "headset", "Headset").unwrap();
            assert!(timer::wait_for(&context, std::time::Duration::from_secs(2), || !observed.borrow().is_empty()));
            assert_eq!(observed.borrow().last().unwrap(), &Entries::from([(first.id.clone(), first)]));
            fs::remove_file(&store.path).unwrap();
            assert!(timer::wait_for(&context, std::time::Duration::from_secs(2), || observed.borrow().len() >= 2));
            assert!(observed.borrow().last().unwrap().is_empty());
            monitor.close();
            fs::remove_dir_all(dir).unwrap();
        }).unwrap();
    }
}
