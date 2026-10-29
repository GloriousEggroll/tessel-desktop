//! users_backend.py: AccountsService queries/updates and a PTY-driven
//! `passwd` (no root helper).
use std::ffi::CStr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::LazyLock;

use gtk::{gio, glib};
use regex::bytes::Regex;

use crate::compat::{self, Error, Result};
use crate::mouse_polling::{unpack, Value};

pub const ACCOUNTS: &str = "org.freedesktop.Accounts";
pub const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
pub const USER_INTERFACE: &str = "org.freedesktop.Accounts.User";

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Account {
    pub uid: i64,
    pub user_name: String,
    pub real_name: String,
    pub icon_file: String,
    pub administrator: bool,
    pub path: String,
}

impl Account {
    pub fn display_name(&self) -> &str { if self.real_name.is_empty() { &self.user_name } else { &self.real_name } }
}

fn group_members(name: &str) -> Option<Vec<String>> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: getgrnam returns a static buffer or NULL; read immediately.
    unsafe {
        let group = libc::getgrnam(name.as_ptr());
        if group.is_null() {
            return None;
        }
        let mut members = Vec::new();
        let mut cursor = (*group).gr_mem;
        while !cursor.is_null() && !(*cursor).is_null() {
            members.push(CStr::from_ptr(*cursor).to_string_lossy().into_owned());
            cursor = cursor.add(1);
        }
        Some(members)
    }
}

/// `_local_account(uid)` from the password and group databases.
pub fn local_account(uid: u32) -> Result<Account> {
    // SAFETY: getpwuid returns a static buffer or NULL; read immediately.
    let (name, gecos) = unsafe {
        let entry = libc::getpwuid(uid);
        if entry.is_null() {
            return Err(Error::Runtime(format!("getpwuid(): uid not found: {uid}")));
        }
        (CStr::from_ptr((*entry).pw_name).to_string_lossy().into_owned(), CStr::from_ptr((*entry).pw_gecos).to_string_lossy().into_owned())
    };
    let real = gecos.split(',').next().unwrap_or("").to_owned();
    let admin = ["wheel", "sudo", "admin"].iter().any(|g| group_members(g).is_some_and(|m| m.contains(&name)));
    Ok(Account { uid: uid as i64, user_name: name, real_name: real, icon_file: String::new(), administrator: admin, path: String::new() })
}

/// What UsersPage needs (Python passes a SimpleNamespace in tests).
pub trait Accounts: Send + Sync {
    fn current(&self) -> Result<Account>;
    fn others(&self) -> Result<Vec<Account>>;
    fn set_real_name(&self, account: &Account, name: &str) -> Result<()>;
    fn set_icon(&self, account: &Account, path: &str) -> Result<()>;
}

/// Blocking D-Bus calls; use from a worker thread.
#[derive(Default)]
pub struct AccountsService {
    bus: std::sync::Mutex<Option<gio::DBusConnection>>,
}

impl AccountsService {
    pub fn with_bus(bus: gio::DBusConnection) -> Self { AccountsService { bus: std::sync::Mutex::new(Some(bus)) } }

    fn bus(&self) -> Result<gio::DBusConnection> {
        let mut slot = self.bus.lock().unwrap();
        if slot.is_none() {
            *slot = Some(gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE)?);
        }
        Ok(slot.clone().unwrap())
    }

    fn call(&self, path: &str, interface: &str, method: &str, parameters: Option<glib::Variant>, reply: Option<&str>) -> Result<Vec<Value>> {
        let reply_type = reply.map(|r| glib::VariantType::new(r).expect("reply type"));
        let result = self.bus()?.call_sync(Some(ACCOUNTS), path, interface, method, parameters.as_ref(), reply_type.as_deref(),
                                           gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION, 30000, gio::Cancellable::NONE)?;
        Ok(match unpack(&result) { Value::List(items) => items, other => vec![other] })
    }

    fn account(&self, path: &str) -> Result<Account> {
        use glib::prelude::ToVariant;
        let values = self.call(path, "org.freedesktop.DBus.Properties", "GetAll", Some((USER_INTERFACE,).to_variant()), Some("(a{sv})"))?;
        let Some(Value::Dict(props)) = values.into_iter().next() else { return Ok(Account::default()) };
        let get = |k: &str| props.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        let int = |v: Option<Value>, d: i64| match v { Some(Value::Int(i)) => i, Some(Value::Bool(b)) => b as i64, _ => d };
        let text = |v: Option<Value>| v.map(|v| v.as_str().to_owned()).unwrap_or_default();
        Ok(Account { uid: int(get("Uid"), -1), user_name: text(get("UserName")), real_name: text(get("RealName")),
                     icon_file: text(get("IconFile")), administrator: int(get("AccountType"), 0) == 1, path: path.to_owned() })
    }

    fn uid() -> u32 { unsafe { libc::getuid() } }
}

impl Accounts for AccountsService {
    fn current(&self) -> Result<Account> {
        use glib::prelude::ToVariant;
        let attempt = || -> Result<Account> {
            let values = self.call(ACCOUNTS_PATH, ACCOUNTS, "FindUserById", Some((Self::uid() as i64,).to_variant()), Some("(o)"))?;
            let path = values.first().map(|v| v.as_str().to_owned()).unwrap_or_default();
            let account = self.account(&path)?;
            let icon = if !account.icon_file.is_empty() && std::path::Path::new(&account.icon_file).is_file() { account.icon_file.clone() } else { String::new() };
            Ok(Account { icon_file: icon, path, ..account })
        };
        attempt().or_else(|_| local_account(Self::uid()))
    }

    fn others(&self) -> Result<Vec<Account>> {
        let values = self.call(ACCOUNTS_PATH, ACCOUNTS, "ListCachedUsers", None, Some("(ao)"))?;
        let paths: Vec<String> = values.first().map(|v| v.as_list().iter().map(|p| p.as_str().to_owned()).collect()).unwrap_or_default();
        let mut accounts = Vec::new();
        for path in paths {
            let account = self.account(&path)?;
            if account.uid != Self::uid() as i64 {
                accounts.push(account);
            }
        }
        accounts.sort_by_key(|a| compat::casefold(a.display_name()));
        Ok(accounts)
    }

    fn set_real_name(&self, account: &Account, name: &str) -> Result<()> {
        use glib::prelude::ToVariant;
        let name = compat::strip(name);
        if name.chars().count() > 256 || name.contains([':', '\n', '\r', '\0']) {
            return compat::value("Names cannot contain colons or line breaks.");
        }
        self.call(&account.path, USER_INTERFACE, "SetRealName", Some((name,).to_variant()), None).map(|_| ())
    }

    fn set_icon(&self, account: &Account, path: &str) -> Result<()> {
        use glib::prelude::ToVariant;
        if !path.is_empty() {
            let metadata = std::fs::metadata(path).ok().filter(|m| m.is_file());
            if metadata.map(|m| m.len() > 1024 * 1024 * 8).unwrap_or(true) {
                return compat::value("Choose an image smaller than 8 MB.");
            }
        }
        self.call(&account.path, USER_INTERFACE, "SetIconFile", Some((path,).to_variant()), None).map(|_| ())
    }
}

static CURRENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)(current|old|\(current\)).*password[^:\n]*:\s*$").unwrap());
static NEW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)new (unix )?password[^:\n]*:\s*$").unwrap());
static RETYPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)(retype|re-enter|again)[^:\n]*:\s*$").unwrap());

pub const PASSWD: &[&str] = &["setsid", "--ctty", "passwd"];

fn write_all(fd: &OwnedFd, data: &[u8]) {
    let mut offset = 0;
    while offset < data.len() {
        // SAFETY: valid fd and buffer slice.
        let written = unsafe { libc::write(fd.as_raw_fd(), data[offset..].as_ptr().cast(), data.len() - offset) };
        if written <= 0 {
            return;
        }
        offset += written as usize;
    }
}

/// Run passwd on a private PTY. Returns `(success, message)`.
pub fn change_password(current: &str, new: &str, command: &[&str], timeout: f64) -> (bool, String) {
    if current.contains('\n') || new.contains('\n') || new.is_empty() {
        return (false, "Invalid password.".into());
    }
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty fills two descriptors.
    if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) } != 0 {
        return (false, compat::os_plain(&std::io::Error::last_os_error()).to_string());
    }
    // SAFETY: freshly opened descriptors we own.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let stdio = |fd: &OwnedFd| fd.try_clone().map(std::process::Stdio::from);
    let mut process = std::process::Command::new(command[0]);
    process.args(&command[1..]).env("LC_ALL", "C").env("LANG", "C");
    match (stdio(&slave), stdio(&slave), stdio(&slave)) {
        (Ok(a), Ok(b), Ok(c)) => {
            process.stdin(a).stdout(b).stderr(c);
        }
        _ => return (false, "Could not open a terminal.".into()),
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return (false, tiling_manager::compat::python_os_error_text(&error, Some(std::path::Path::new(command[0])))),
    };
    drop(slave);
    let mut transcript: Vec<u8> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let (mut sent_new, mut sent_current) = (0, 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
    while std::time::Instant::now() < deadline {
        let mut poll = libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, 250) };
        if ready > 0 {
            let mut buffer = [0u8; 4096];
            // SAFETY: valid fd and buffer.
            let count = unsafe { libc::read(master.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if count <= 0 {
                break;
            }
            let chunk = &buffer[..count as usize];
            transcript.extend_from_slice(chunk);
            pending.extend_from_slice(chunk);
            let lines = compat::bytes_splitlines(&pending);
            let last = lines.last().map(|l| l.to_vec()).unwrap_or_default();
            if RETYPE.is_match(&last) {
                write_all(&master, format!("{new}\n").as_bytes());
                pending.clear();
            } else if NEW.is_match(&last) {
                sent_new += 1;
                if sent_new > 1 {
                    break; // the first new password was rejected
                }
                write_all(&master, format!("{new}\n").as_bytes());
                pending.clear();
            } else if CURRENT.is_match(&last) || (compat::bytes_rstrip(&last).ends_with(b"assword:") && sent_current == 0) {
                sent_current += 1;
                if sent_current > 1 {
                    break;
                }
                write_all(&master, format!("{current}\n").as_bytes());
                pending.clear();
            }
        } else if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
    }
    if matches!(child.try_wait(), Ok(None)) {
        // SAFETY: signalling our own child.
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    }
    let finish = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= finish => {
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    drop(master);
    let text = String::from_utf8_lossy(&transcript).into_owned();
    let lines: Vec<String> = compat::splitlines(&text, false).iter().map(|l| compat::strip(l).to_owned())
        .filter(|l| !l.is_empty() && !l.to_lowercase().ends_with("password:") && !l.to_lowercase().starts_with("changing password"))
        .collect();
    if status.is_some_and(|s| s.code() == Some(0)) {
        return (true, String::new());
    }
    let message = lines.iter().rev().find(|l| l.to_uppercase().starts_with("BAD PASSWORD")).cloned();
    (false, message.or_else(|| lines.last().cloned()).unwrap_or_else(|| "The password could not be changed.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    fn fake(folder: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = folder.join(name);
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    #[test]
    fn pty_conversation_success_and_rejection() {
        let folder = scratch("passwd");
        let command = fake(&folder, "passwd", "printf 'Changing password for user u.\\nCurrent password: '; read old
printf 'New password: '; read new
printf 'Retype new password: '; read again
[ \"$old\" = old ] && [ \"$new\" = \"$again\" ] && [ \"$new\" = new ] && echo ok && exit 0
echo 'passwd: Authentication token manipulation error'; exit 1
");
        assert_eq!(change_password("old", "new", &[&command], 10.0), (true, String::new()));
        let (okay, message) = change_password("bad", "new", &[&command], 10.0);
        assert!(!okay);
        assert!(message.contains("Authentication token manipulation error"), "{message}");
        let weak = fake(&folder, "weak", "printf 'Current password: '; read old
printf 'New password: '; read new
printf 'BAD PASSWORD: The password is shorter than 8 characters\\nNew password: '; read new
exit 1
");
        assert_eq!(change_password("old", "x", &[&weak], 10.0), (false, "BAD PASSWORD: The password is shorter than 8 characters".into()));
        assert!(!change_password("a\nb", "x", &[&command], 10.0).0);
        assert!(!change_password("a", "x", &["/nonexistent/passwd"], 1.0).0);
    }

    #[test]
    fn local_account_reads_passwd() {
        let account = local_account(unsafe { libc::getuid() }).unwrap();
        assert!(!account.user_name.is_empty());
        assert!(account.path.is_empty());
    }
}
