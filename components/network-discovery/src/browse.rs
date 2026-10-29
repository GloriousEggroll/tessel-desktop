//! Read-only listing of devices and services that Avahi currently sees
//! (discovery_browse.py).
//!
//! One `avahi-browse --all --parsable --terminate --resolve` run per
//! request, spawned asynchronously; it never changes the Avahi or network
//! configuration.

use gio::prelude::*;
use regex::Regex;
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::rc::{Rc, Weak};
use std::sync::LazyLock;

pub const BROWSE_COMMAND: [&str; 5] = ["/usr/bin/avahi-browse", "--all", "--parsable", "--terminate", "--resolve"];
/// Resolution of an unresponsive device can stall; keep what was found by then.
pub const BROWSE_TIMEOUT_MS: u64 = 12000;

pub const SERVICE_NAMES: &[(&str, &str)] = &[
    ("_ipp._tcp", "Printer"), ("_ipps._tcp", "Printer"), ("_printer._tcp", "Printer"),
    ("_pdl-datastream._tcp", "Printer"), ("_scanner._tcp", "Scanner"), ("_uscan._tcp", "Scanner"),
    ("_uscans._tcp", "Scanner"), ("_airplay._tcp", "AirPlay"), ("_raop._tcp", "AirPlay speaker"),
    ("_googlecast._tcp", "Chromecast"), ("_spotify-connect._tcp", "Spotify Connect"),
    ("_smb._tcp", "Windows file sharing"), ("_afpovertcp._tcp", "Apple file sharing"),
    ("_nfs._tcp", "NFS file sharing"), ("_sftp-ssh._tcp", "SFTP file sharing"), ("_ssh._tcp", "Remote login (SSH)"),
    ("_rfb._tcp", "Remote desktop (VNC)"), ("_rdp._tcp", "Remote desktop (RDP)"), ("_http._tcp", "Web page"),
    ("_https._tcp", "Web page"), ("_workstation._tcp", "Computer"), ("_daap._tcp", "Music sharing"),
    ("_hap._tcp", "HomeKit accessory"), ("_matter._tcp", "Matter device"), ("_matterc._udp", "Matter device"),
    ("_mqtt._tcp", "MQTT broker"), ("_home-assistant._tcp", "Home Assistant"), ("_sonos._tcp", "Sonos speaker"),
    ("_amzn-wplay._tcp", "Fire TV"), ("_roku-rcp._tcp", "Roku"), ("_steam._tcp", "Steam"),
    ("_kdeconnect._udp", "KDE Connect"), ("_adb-tls-connect._tcp", "Android debugging"),
    ("_ipp-tls._tcp", "Printer"), ("_androidtvremote2._tcp", "Android TV"), ("_amzn-alexa._tcp", "Alexa"),
    ("_sftp._tcp", "SFTP file sharing"), ("_webdav._tcp", "WebDAV file sharing"),
];

/// Bookkeeping records that every Apple or Avahi host publishes add only noise.
pub const HIDDEN_TYPES: [&str; 9] = ["_device-info._tcp", "_sleep-proxy._udp", "_companion-link._tcp", "_net-assistant._udp",
                                     "_apple-mobdev2._tcp", "_rdlink._tcp", "_remotepairing._tcp", "_asquic._udp",
                                     "_googlezone._tcp"];

static MAC_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9A-Fa-f]{12}@").unwrap());
static MAC_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\[[0-9a-fA-F:]{17}\]$").unwrap());
static OPAQUE_HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[_-]*[0-9a-fA-F]{8}[0-9a-fA-F_-]{8,}$").unwrap());
static OPAQUE_LONG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{30,}$").unwrap());

/// Decode avahi-browse's `\DDD` decimal byte escapes as UTF-8 text.
pub fn unescape(value: &str) -> String {
    let raw = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        let byte = raw[index];
        if byte == b'\\' && index + 1 < raw.len() {
            let digits = &raw[index + 1..(index + 4).min(raw.len())];
            if digits.len() == 3 && digits.iter().all(u8::is_ascii_digit) {
                let number: u32 = std::str::from_utf8(digits).unwrap().parse().unwrap();
                if number < 256 {
                    out.push(number as u8);
                } else {
                    out.extend_from_slice(digits);
                }
                index += 4;
                continue;
            }
            // `\x` → `x` (regex `.`: any byte but a newline).
            if raw[index + 1] != b'\n' {
                out.push(raw[index + 1]);
                index += 2;
                continue;
            }
        }
        out.push(byte);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn service_label(kind: &str) -> String {
    if let Some((_, label)) = SERVICE_NAMES.iter().find(|(name, _)| *name == kind) {
        return (*label).into();
    }
    let plain = kind.split('.').next().unwrap_or("").trim_start_matches('_').replace('-', " ");
    let mut chars = plain.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => kind.into(),
    }
}

fn clean(name: &str) -> String {
    // AirPlay speakers publish "MAC@Name"; some hosts append " [MAC]".
    let name = if MAC_PREFIX.is_match(name) { name.split_once('@').map(|(_, rest)| rest).unwrap_or(name) } else { name };
    MAC_SUFFIX.replace(name, "").trim().to_owned()
}

/// Identifiers such as UUIDs or hashes, which say nothing to a person.
pub fn opaque(name: &str) -> bool { OPAQUE_HEX.is_match(name) || OPAQUE_LONG.is_match(name) }

fn casefold(text: &str) -> String { text.to_lowercase().replace('ß', "ss") }

fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(['\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}']) {
            Some(index) => {
                lines.push(&rest[..index]);
                let boundary = rest[index..].chars().next().unwrap();
                let mut skip = boundary.len_utf8();
                if boundary == '\r' && rest[index + 1..].starts_with('\n') {
                    skip += 1;
                }
                rest = &rest[index + skip..];
            }
            None => {
                lines.push(rest);
                break;
            }
        }
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub host: String,
    pub address: String,
    pub services: Vec<String>,
}

/// Prefer IPv4 addresses, which people recognize.
fn better(current: &str, candidate: &str) -> bool {
    !candidate.is_empty() && (current.is_empty() || current.contains(':') && !candidate.contains(':'))
}

/// Group parsable records into devices.
pub fn parse(output: &str) -> Vec<Device> {
    // (name, kind) → (host, address), in first-seen order.
    let mut records: Vec<((String, String), (String, String))> = Vec::new();
    for line in splitlines(output) {
        let fields: Vec<&str> = line.split(';').collect();
        if fields.len() < 6 || !(fields[0] == "+" || fields[0] == "=") {
            continue;
        }
        let (name, kind) = (clean(&unescape(fields[3])), fields[4].to_owned());
        if HIDDEN_TYPES.contains(&kind.as_str()) || name.is_empty() {
            continue;
        }
        let key = (name, kind);
        let index = match records.iter().position(|(k, _)| *k == key) {
            Some(index) => index,
            None => {
                records.push((key, (String::new(), String::new())));
                records.len() - 1
            }
        };
        if fields[0] == "=" && fields.len() > 8 {
            let record = &mut records[index].1;
            if record.0.is_empty() {
                let host = unescape(fields[6]);
                record.0 = host.strip_suffix(".local").unwrap_or(&host).to_owned();
            }
            if better(&record.1, fields[7]) {
                record.1 = fields[7].to_owned();
            }
        }
    }
    struct Group {
        key: String,
        names: BTreeSet<String>,
        host: String,
        address: String,
        services: BTreeSet<String>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for ((name, kind), (host, address)) in records {
        // Several services usually share one host; unresolved ones group by name.
        let key = casefold(if host.is_empty() { &name } else { &host });
        let index = match groups.iter().position(|g| g.key == key) {
            Some(index) => index,
            None => {
                groups.push(Group { key, names: BTreeSet::new(), host: host.clone(), address: String::new(), services: BTreeSet::new() });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        group.names.insert(name);
        if better(&group.address, &address) {
            group.address = address;
        }
        group.services.insert(service_label(&kind));
    }
    let mut result: Vec<(Device, bool)> = groups.into_iter().map(|group| {
        let mut names: Vec<&String> = group.names.iter().filter(|n| !opaque(n)).collect();
        names.sort_by(|a, b| (a.chars().count(), a.as_str()).cmp(&(b.chars().count(), b.as_str())));
        let title = match names.first() {
            Some(name) => (*name).clone(),
            None if !group.host.is_empty() => group.host.clone(),
            None => group.names.iter().next().cloned().unwrap_or_default(),
        };
        let anonymous = names.is_empty();
        (Device { name: title, host: group.host, address: group.address, services: group.services.into_iter().collect() }, anonymous)
    }).collect();
    // Opaque per-app hosts (Chromecast, Spotify) often share a named device's address.
    let mut named: Vec<(String, usize)> = Vec::new();
    for (index, (device, anonymous)) in result.iter().enumerate() {
        if !device.address.is_empty() && !anonymous {
            match named.iter_mut().find(|(address, _)| *address == device.address) {
                Some(entry) => entry.1 = index,
                None => named.push((device.address.clone(), index)),
            }
        }
    }
    let mut absorbed = vec![false; result.len()];
    for index in 0..result.len() {
        if !result[index].1 {
            continue;
        }
        let address = result[index].0.address.clone();
        if let Some(&(_, owner)) = named.iter().find(|(a, _)| *a == address) {
            let services: BTreeSet<String> = result[owner].0.services.iter().chain(result[index].0.services.iter()).cloned().collect();
            result[owner].0.services = services.into_iter().collect();
            absorbed[index] = true;
        }
    }
    let mut merged: Vec<Device> = result.into_iter().zip(absorbed).filter(|(_, a)| !a).map(|((d, _), _)| d).collect();
    merged.sort_by_cached_key(|device| (opaque(&device.name), casefold(&device.name)));
    merged
}

pub type Finished = Rc<dyn Fn(&ServiceBrowser)>;

struct Inner {
    finished: Finished,
    command: Vec<String>,
    process: RefCell<Option<gio::Subprocess>>,
    cancel: RefCell<Option<gio::Cancellable>>,
    timeout: RefCell<Option<glib::SourceId>>,
    closed: Cell<bool>,
    devices: RefCell<Vec<Device>>,
    error: RefCell<String>,
}

/// Run one asynchronous browse at a time and report a list or a friendly error.
#[derive(Clone)]
pub struct ServiceBrowser(Rc<Inner>);

impl ServiceBrowser {
    pub fn new(finished: Finished) -> Self { Self::with_command(finished, BROWSE_COMMAND.iter().map(|s| s.to_string()).collect()) }

    pub fn with_command(finished: Finished, command: Vec<String>) -> Self {
        Self(Rc::new(Inner { finished, command, process: RefCell::new(None), cancel: RefCell::new(None),
                             timeout: RefCell::new(None), closed: Cell::new(false), devices: RefCell::default(),
                             error: RefCell::default() }))
    }

    pub fn command(&self) -> &[String] { &self.0.command }
    pub fn running(&self) -> bool { self.0.process.borrow().is_some() }
    pub fn devices(&self) -> Vec<Device> { self.0.devices.borrow().clone() }
    pub fn error(&self) -> String { self.0.error.borrow().clone() }

    fn weak(&self) -> Weak<Inner> { Rc::downgrade(&self.0) }

    pub fn start(&self) -> bool {
        if self.0.closed.get() || self.running() {
            return false;
        }
        self.0.error.replace(String::new());
        let argv: Vec<&OsStr> = self.0.command.iter().map(OsStr::new).collect();
        let process = match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) {
            Ok(process) => process,
            Err(_) => {
                self.0.error.replace("Avahi browsing tools are not installed.".into());
                (self.0.finished)(self);
                return false;
            }
        };
        self.0.process.replace(Some(process.clone()));
        let cancel = gio::Cancellable::new();
        self.0.cancel.replace(Some(cancel.clone()));
        let weak = self.weak();
        let timeout = glib::timeout_add_local_once(std::time::Duration::from_millis(BROWSE_TIMEOUT_MS), move || {
            if let Some(inner) = weak.upgrade() {
                inner.timeout.replace(None);
                if let Some(process) = inner.process.borrow().as_ref() {
                    // Ends output; communicate then returns what was printed so far.
                    process.send_signal(15);
                }
            }
        });
        self.0.timeout.replace(Some(timeout));
        let weak = self.weak();
        let owner = process.clone();
        process.communicate_utf8_async(None, Some(&cancel), move |result| {
            let Some(inner) = weak.upgrade() else { return };
            ServiceBrowser(inner).done(&owner, result.map(|(out, _)| out.map(|o| o.to_string())));
        });
        true
    }

    fn done(&self, owner: &gio::Subprocess, result: Result<Option<String>, glib::Error>) {
        if let Some(timeout) = self.0.timeout.take() {
            timeout.remove();
        }
        if self.0.process.borrow().as_ref() != Some(owner) {
            return;
        }
        self.0.process.replace(None);
        let output = result.ok().map(|o| o.unwrap_or_default());
        if self.0.closed.get() {
            return;
        }
        match output {
            None => {
                self.0.error.replace("The device list could not be read.".into());
            }
            Some(output) => {
                let devices = parse(&output);
                let empty = devices.is_empty();
                self.0.devices.replace(devices);
                if owner.has_exited() && owner.exit_status() != 0 && empty {
                    self.0.error.replace("Avahi is not running. Turn on discovery to look for devices.".into());
                }
            }
        }
        (self.0.finished)(self);
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        if let Some(timeout) = self.0.timeout.take() {
            timeout.remove();
        }
        if let Some(cancel) = self.0.cancel.borrow().as_ref() {
            cancel.cancel();
        }
        if let Some(process) = self.0.process.take() {
            process.force_exit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_output_groups_services_by_device_with_friendly_names() {
        let output = [
            "+;eth0;IPv4;Office\\032Printer;_ipp._tcp;local",
            "=;eth0;IPv4;Office\\032Printer;_ipp._tcp;local;BRW1234.local;192.0.2.2;631;\"x\"",
            "=;eth0;IPv6;Office\\032Printer;_http._tcp;local;BRW1234.local;fe80::1;80;",
            "=;eth0;IPv4;Office\\032Printer;_http._tcp;local;BRW1234.local;192.0.2.2;80;",
            "+;eth0;IPv4;Living\\032Room;_device-info._tcp;local",
            "=;eth0;IPv4;3ca2bc5d-55f1-d0e9-c00b-1693109b5b41;_googlecast._tcp;local;3ca2.local;192.0.2.2;8009;",
            "+;eth0;IPv4;Thomas\\226\\128\\153s\\032Mac;_smb._tcp;local", "garbage"].join("\n");
        let devices = parse(&output);
        assert_eq!(devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Office Printer", "Thomas\u{2019}s Mac"]);
        assert_eq!(devices[0].address, "192.0.2.2");
        assert_eq!(devices[0].host, "BRW1234");
        assert_eq!(devices[0].services, ["Chromecast", "Printer", "Web page"]);
        assert_eq!(devices[1].services, ["Windows file sharing"]);
        assert_eq!(unescape("a\\059b"), "a;b");
    }

    #[test]
    fn names_labels_and_opaque_identifiers() {
        assert_eq!(service_label("_ssh._tcp"), "Remote login (SSH)");
        assert_eq!(service_label("_foo-bar._tcp"), "Foo bar");
        assert_eq!(service_label("_device-info._tcp"), "Device info");
        assert_eq!(service_label("._tcp"), "._tcp");
        assert_eq!(clean("A1B2C3D4E5F6@Kitchen"), "Kitchen");
        assert_eq!(clean("Laptop [aa:bb:cc:dd:ee:ff]"), "Laptop");
        assert!(opaque("3ca2bc5d-55f1-d0e9-c00b-1693109b5b41"));
        assert!(opaque("Chromecast-Ultra-0123456789abcdef0123"));
        assert!(!opaque("Office Printer"));
        assert_eq!(unescape("x\\999y\\\\z"), "x999y\\z");
        assert_eq!(unescape("bad\\255"), "bad\u{fffd}");
    }

    #[test]
    fn anonymous_hosts_and_sorting() {
        let output = "=;e;IPv4;zeta;_ssh._tcp;local;zeta.local;10.0.0.9;22;\n\
                      =;e;IPv4;Alpha;_http._tcp;local;alpha.local;fe80::2;80;\n\
                      =;e;IPv4;0123456789abcdef0123456789abcdef;_spotify-connect._tcp;local;0123456789abcdef0123456789abcdef.local;10.0.0.200;1;\n\
                      +;e;IPv4;Beta;_ipp._tcp;local";
        let devices = parse(output);
        assert_eq!(devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
                   ["Alpha", "Beta", "zeta", "0123456789abcdef0123456789abcdef"]);
        assert_eq!(devices[3].services, ["Spotify Connect"]);
        assert_eq!(devices[1].host, "");
    }
}
