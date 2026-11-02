//! VPN authentication helpers of secret_agent.py: the NetworkManager
//! auth-dialog stdin protocol, trusted helper discovery, response parsing and
//! `VpnProcess` (own session/process group, pipes only, 300 s timeout,
//! killpg on cancel/timeout; secrets never in argv).

use crate::libnm::{Connection, VpnPluginInfo};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const ALLOW_INTERACTION: u32 = 0x1;
pub const REQUEST_NEW: u32 = 0x2;

/// `vpn_request_payload(connection)`: never a disk file.
pub fn request_payload(connection: &Connection) -> Result<Vec<u8>, String> {
    let vpn = connection.setting("vpn").ok_or("The connection does not contain VPN settings")?;
    let mut lines = Vec::new();
    for key in vpn.vpn_data_keys() {
        lines.push(format!("DATA_KEY={}", key.replace('\n', " ")));
        lines.push(format!("DATA_VAL={}", vpn.vpn_data_item(&key).unwrap_or_default().replace('\n', " ")));
    }
    for key in vpn.vpn_secret_keys() {
        lines.push(format!("SECRET_KEY={}", key.replace('\n', " ")));
        lines.push(format!("SECRET_VAL={}", vpn.vpn_secret(&key).unwrap_or_default().replace('\n', " ")));
    }
    Ok(format!("{}\nDONE\n\nQUIT\n\n", lines.join("\n")).into_bytes())
}

/// What the agent needs from a plugin `.name` file.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginMeta {
    pub auth_dialog: Option<String>,
    pub supports_hints: bool,
    pub external_ui: bool,
}

pub fn plugin_meta(service: &str) -> Option<PluginMeta> {
    VpnPluginInfo::search(service).map(|p| meta_of(&p))
}

/// The metadata the agent reads from one plugin `.name` file.
pub fn meta_of(plugin: &VpnPluginInfo) -> PluginMeta {
    let external = plugin.lookup_property("GNOME", "supports-external-ui-mode").unwrap_or_default().to_lowercase();
    PluginMeta { auth_dialog: plugin.auth_dialog(), supports_hints: plugin.supports_hints(),
                 external_ui: ["true", "yes", "1"].contains(&external.as_str()) }
}

/// Absolute, regular, root-owned, not group/other-writable and executable.
pub fn trusted_helper(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Ok(info) = std::fs::metadata(path) else { return false };
    info.file_type().is_file() && info.uid() == 0 && info.mode() & 0o022 == 0 && tessel_core::shell::is_executable(path)
}

/// `vpn_auth_command(connection, hints, flags)` → `(argv, external_ui)`.
pub fn auth_command(connection: &Connection, hints: &[String], flags: u32, lookup: &dyn Fn(&str) -> Option<PluginMeta>,
                    trusted: &dyn Fn(&Path) -> bool) -> Result<(Vec<String>, bool), String> {
    let vpn = connection.setting("vpn").ok_or("The connection does not contain VPN settings")?;
    let service = vpn.vpn_service_type();
    let plugin = lookup(&service);
    let executable = plugin.as_ref().and_then(|p| p.auth_dialog.clone()).filter(|e| !e.is_empty())
        .ok_or("Install the authentication plugin for this VPN type.")?;
    let plugin = plugin.unwrap();
    if !trusted(Path::new(&executable)) {
        return Err("The VPN authentication helper must be a trusted system executable.".into());
    }
    let mut argv = vec![executable, "-u".into(), connection.uuid(), "-n".into(), connection.id(), "-s".into(), service];
    if flags & ALLOW_INTERACTION != 0 {
        argv.push("-i".into());
    }
    if flags & REQUEST_NEW != 0 {
        argv.push("-r".into());
    }
    if plugin.supports_hints {
        for hint in hints {
            argv.push("-t".into());
            argv.push(hint.clone());
        }
    }
    if plugin.external_ui {
        argv.push("--external-ui-mode".into());
    }
    Ok((argv, plugin.external_ui))
}

/// One requested field: `(key, label, value, masked)`.
pub type Field = (String, String, String, bool);

/// `(title, message, values, fields)`.
pub type Response = (Option<String>, Option<String>, Vec<(String, String)>, Vec<Field>);

pub fn parse_response(output: &[u8], external: bool) -> Result<Response, String> {
    if output.len() > 1024 * 1024 {
        return Err("The VPN authentication response is too large.".into());
    }
    let text = std::str::from_utf8(output).map_err(|_| "The VPN authentication response is invalid.".to_owned())?;
    if !external {
        // str.splitlines() semantics for \n, \r\n and \r.
        let lines: Vec<&str> = text.lines().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        let mut values: Vec<(String, String)> = Vec::new();
        let mut position = 0;
        while position < lines.len() && !lines[position].is_empty() {
            if position + 1 >= lines.len() {
                return Err("The VPN authentication response is incomplete.".into());
            }
            let key = lines[position].to_owned();
            values.retain(|(k, _)| *k != key);
            values.push((key, lines[position + 1].to_owned()));
            position += 2;
        }
        return Ok((None, None, values, Vec::new()));
    }
    let keyfile = glib::KeyFile::new();
    keyfile.load_from_data(text, glib::KeyFileFlags::NONE).map_err(|e| e.message().to_owned())?;
    let group = "VPN Plugin UI";
    if keyfile.string(group, "Version").map(|v| v.to_string()).ok().as_deref() != Some("2") {
        return Err("The VPN authentication response has an unsupported version.".into());
    }
    let title = keyfile.string(group, "Title").map_err(|e| e.message().to_owned())?.to_string();
    let message = keyfile.string(group, "Description").map_err(|e| e.message().to_owned())?.to_string();
    let mut values = Vec::new();
    let mut fields = Vec::new();
    for name in keyfile.groups().iter() {
        let name = name.to_string();
        if name == group {
            continue;
        }
        let Ok(label) = keyfile.string(&name, "Label") else { continue };
        let value = keyfile.string(&name, "Value").map(|v| v.to_string()).unwrap_or_default();
        let ask = keyfile.boolean(&name, "ShouldAsk").unwrap_or(false);
        let masked = keyfile.boolean(&name, "IsSecret").unwrap_or(false);
        values.push((name.clone(), value.clone()));
        if ask {
            fields.push((name, label.to_string(), value, masked));
        }
    }
    Ok((Some(title), Some(message), values, fields))
}

pub type Delivery = Box<dyn FnOnce(Vec<u8>, Option<String>)>;

#[derive(Default)]
struct Shared {
    pid: Option<i32>,
    cancelled: bool,
    result: Option<(Vec<u8>, Option<String>)>,
    returncode: Option<i32>,
}

/// Plugin interaction off the main thread; the whole process group dies on
/// cancel/timeout. The result is delivered on the main loop.
pub struct VpnProcess {
    shared: Arc<Mutex<Shared>>,
    thread: Option<std::thread::JoinHandle<()>>,
    source: std::cell::RefCell<Option<tessel_core::timer::Timeout>>,
}

impl VpnProcess {
    pub fn spawn(argv: Vec<String>, payload: Vec<u8>, callback: Delivery, timeout: Duration) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker = shared.clone();
        let thread = std::thread::spawn(move || run(argv, payload, worker, timeout));
        let callback = std::rc::Rc::new(std::cell::RefCell::new(Some(callback)));
        let poll = shared.clone();
        let source = tessel_core::timer::repeat(20, move || {
            let mut state = poll.lock().unwrap();
            if state.cancelled {
                return false;
            }
            if let Some((output, error)) = state.result.take() {
                drop(state);
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(output, error);
                }
                return false;
            }
            true
        });
        Self { shared, thread: Some(thread), source: std::cell::RefCell::new(Some(source)) }
    }

    pub fn cancel(&self) {
        let mut state = self.shared.lock().unwrap();
        state.cancelled = true;
        if let Some(pid) = state.pid {
            if state.returncode.is_none() {
                unsafe { libc::killpg(pid, libc::SIGKILL) };
            }
        }
        drop(state);
        if let Some(source) = self.source.borrow_mut().take() {
            source.cancel();
        }
    }

    pub fn returncode(&self) -> Option<i32> {
        self.shared.lock().unwrap().returncode
    }

    pub fn join(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

fn run(argv: Vec<String>, payload: Vec<u8>, shared: Arc<Mutex<Shared>>, timeout: Duration) {
    let finish = |output: Vec<u8>, error: Option<String>, code: Option<i32>| {
        let mut state = shared.lock().unwrap();
        state.returncode = code;
        if !state.cancelled {
            state.result = Some((output, error));
        }
    };
    let Some(program) = argv.first() else { return finish(Vec::new(), Some("The VPN authentication helper could not be started.".into()), None) };
    let mut command = Command::new(program);
    command.args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).env_remove("G_MESSAGES_DEBUG");
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = {
        let mut state = shared.lock().unwrap();
        if state.cancelled {
            return;
        }
        match command.spawn() {
            Ok(child) => {
                state.pid = Some(child.id() as i32);
                child
            }
            Err(_) => {
                drop(state);
                return finish(Vec::new(), Some("The VPN authentication helper could not be started.".into()), None);
            }
        }
    };
    let pid = child.id() as i32;
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(&payload);
        }
        drop(stdin);
    });
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = (&mut stdout).take(1024 * 1024 + 1).read_to_end(&mut output);
        let mut sink = [0u8; 4096];
        while matches!(stdout.read(&mut sink), Ok(n) if n > 0) {}
        output
    });
    let end = Instant::now() + timeout;
    let mut error = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if shared.lock().unwrap().cancelled {
            unsafe { libc::killpg(pid, libc::SIGKILL) };
            break child.wait().ok();
        }
        if Instant::now() >= end {
            unsafe { libc::killpg(pid, libc::SIGKILL) };
            error = Some("VPN authentication timed out.".to_owned());
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // Grandchildren in the session may still hold the pipe; never wait forever.
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    let _ = writer.join();
    let output = reader.join().unwrap_or_default();
    let code = status.and_then(|s| s.code()).or(Some(-1));
    if error.is_none() && code != Some(0) {
        error = Some("VPN authentication was cancelled or could not be completed.".into());
    }
    finish(output, error, code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn vpn_connection() -> Connection {
        let connection = Connection::new().unwrap();
        let general = crate::libnm::Setting::new("connection").unwrap();
        general.set_str("id", Some("Synthetic credential test"));
        general.set_str("uuid", Some("11111111-2222-4333-8444-555555555557"));
        general.set_str("type", Some("vpn"));
        connection.add_setting(&general);
        let vpn = crate::libnm::Setting::new("vpn").unwrap();
        vpn.set_str("service-type", Some("org.freedesktop.NetworkManager.openvpn"));
        vpn.vpn_add_data_item("remote", "test.invalid");
        connection.add_setting(&vpn);
        connection
    }

    fn iterate_until(mut done: impl FnMut() -> bool, timeout: Duration) {
        let end = Instant::now() + timeout;
        let context = glib::MainContext::ref_thread_default();
        while !done() && Instant::now() < end {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn protocol_escapes_newlines_and_finishes_handshake() {
        let connection = vpn_connection();
        let vpn = connection.setting("vpn").unwrap();
        vpn.vpn_add_data_item("synthetic", "line1\nline2");
        vpn.vpn_add_secret("password", "fakevalue");
        let payload = String::from_utf8(request_payload(&connection).unwrap()).unwrap();
        assert!(payload.contains("DATA_KEY=synthetic\nDATA_VAL=line1 line2\n"), "{payload}");
        assert!(payload.contains("SECRET_KEY=password\nSECRET_VAL=fakevalue\n"));
        assert!(payload.ends_with("DONE\n\nQUIT\n\n"));
    }

    #[test]
    fn plugin_discovery_never_puts_password_in_arguments() {
        let connection = vpn_connection();
        connection.setting("vpn").unwrap().vpn_add_secret("password", "synthetic-never-in-argv");
        let meta = |_: &str| Some(PluginMeta { auth_dialog: Some("/usr/libexec/nm-openvpn-auth-dialog".into()), supports_hints: true, external_ui: true });
        let (argv, external) = auth_command(&connection, &["password".into()], 3, &meta, &|_| true).unwrap();
        assert_eq!(argv[0], "/usr/libexec/nm-openvpn-auth-dialog");
        assert_eq!(argv[1..7], ["-u", "11111111-2222-4333-8444-555555555557", "-n", "Synthetic credential test", "-s",
                                "org.freedesktop.NetworkManager.openvpn"]);
        assert!(argv.contains(&"-i".into()) && argv.contains(&"-r".into()));
        assert!(argv.windows(2).any(|w| w == ["-t", "password"]));
        assert_eq!(argv.last().unwrap(), "--external-ui-mode");
        assert!(external);
        assert!(!argv.iter().any(|a| a.contains("synthetic-never-in-argv")));
        // The real plugin metadata of an installed plugin, when present.
        if let Some(real) = plugin_meta("org.freedesktop.NetworkManager.openvpn") {
            assert!(real.auth_dialog.is_some());
        }
    }

    #[test]
    fn plugin_trust_rejects_user_owned_executable() {
        let dir = std::env::temp_dir().join(format!("vpn-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let helper = dir.join("auth-dialog");
        std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&helper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let path = helper.display().to_string();
        let meta = move |_: &str| Some(PluginMeta { auth_dialog: Some(path.clone()), supports_hints: false, external_ui: false });
        let error = auth_command(&vpn_connection(), &[], 1, &meta, &trusted_helper).unwrap_err();
        assert!(error.contains("trusted system"), "{error}");
        assert!(!trusted_helper(Path::new("relative/helper")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real installed plugin `.name` file (read-only), else a fixture in its format.
    #[test]
    fn real_plugin_name_file_drives_the_helper_command() {
        let installed = ["/usr/lib/NetworkManager/VPN", "/usr/lib64/NetworkManager/VPN", "/etc/NetworkManager/VPN"].iter()
            .filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten().map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "name")
                  && std::fs::read_to_string(p).is_ok_and(|t| t.contains("auth-dialog=")));
        let fixture;
        let path = match installed {
            Some(path) => path,
            None => {
                let dir = std::env::temp_dir().join(format!("vpn-name-{}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                fixture = dir.join("nm-fixture-service.name");
                std::fs::write(&fixture, "[VPN Connection]\nname=fixture\nservice=org.freedesktop.NetworkManager.fixture\n\
                    program=/usr/libexec/nm-fixture-service\n\n[GNOME]\nauth-dialog=/usr/libexec/nm-fixture-auth-dialog\n\
                    supports-external-ui-mode=true\nsupports-hints=true\n").unwrap();
                fixture.clone()
            }
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let keyfile = glib::KeyFile::new();
        keyfile.load_from_data(&text, glib::KeyFileFlags::NONE).unwrap();
        let service = keyfile.string("VPN Connection", "service").unwrap().to_string();
        let dialog = keyfile.string("GNOME", "auth-dialog").unwrap().to_string();
        let hints = keyfile.boolean("GNOME", "supports-hints").unwrap_or(false);
        let external = keyfile.string("GNOME", "supports-external-ui-mode").map(|v| ["true", "yes", "1"].contains(&v.to_lowercase().as_str())).unwrap_or(false);
        // libnm's own parser sees the same metadata (the installed copy is also found by service name).
        let meta = match crate::libnm::VpnPluginInfo::from_file(&path.display().to_string()) {
            Ok(info) => meta_of(&info),
            Err(_) => plugin_meta(&service).expect("installed plugin found by service"),
        };
        assert_eq!(meta, PluginMeta { auth_dialog: Some(dialog.clone()), supports_hints: hints, external_ui: external });
        let connection = vpn_connection();
        connection.setting("vpn").unwrap().set_str("service-type", Some(&service));
        connection.setting("vpn").unwrap().vpn_add_secret("password", "synthetic-never-in-argv");
        let lookup = move |s: &str| (s == service).then(|| meta.clone());
        let (argv, ext) = auth_command(&connection, &["password".into()], 1, &lookup, &|_| true).unwrap();
        assert_eq!(argv[0], dialog);
        assert_eq!(ext, external);
        assert_eq!(argv.windows(2).any(|w| w == ["-t", "password"]), hints);
        assert!(!argv.iter().any(|a| a.contains("synthetic")));
        if Path::new(&dialog).exists() {
            assert!(trusted_helper(Path::new(&dialog)), "the installed root-owned helper passes the trust check");
        }
    }

    #[test]
    fn legacy_plugin_reply() {
        let (_, _, values, fields) = parse_response(b"password\nfakevalue\notp\n123456\n\n", false).unwrap();
        assert_eq!(values, [("password".into(), "fakevalue".into()), ("otp".into(), "123456".into())]);
        assert!(fields.is_empty());
        assert!(parse_response(b"password", false).is_err());
    }

    #[test]
    fn external_ui_supports_multiple_masked_and_visible_fields() {
        let output = "[VPN Plugin UI]\nVersion=2\nTitle=VPN test\nDescription=Authenticate\n\
                      [username]\nLabel=Username\nValue=test\nIsSecret=false\nShouldAsk=true\n\
                      [password]\nLabel=Password\nValue=\nIsSecret=true\nShouldAsk=true\n\
                      [cookie]\nLabel=Cookie\nValue=saved\nIsSecret=true\nShouldAsk=false\n";
        let (title, message, values, fields) = parse_response(output.as_bytes(), true).unwrap();
        assert_eq!((title.as_deref(), message.as_deref()), (Some("VPN test"), Some("Authenticate")));
        assert_eq!(fields, [("username".into(), "Username".into(), "test".into(), false), ("password".into(), "Password".into(), "".into(), true)]);
        assert!(values.contains(&("cookie".into(), "saved".into())));
        assert!(parse_response(b"[VPN Plugin UI]\nVersion=1\n", true).is_err());
    }

    #[test]
    fn vpn_child_uses_pipe_and_is_reaped() {
        let _context = crate::testing::TestContext::new();
        let replies: Rc<RefCell<Vec<(Vec<u8>, Option<String>)>>> = Rc::default();
        let r = replies.clone();
        let mut worker = VpnProcess::spawn(vec!["/bin/sh".into(), "-c".into(), "cat >/dev/null; printf 'password\\nfake\\n\\n'".into()],
            b"synthetic secret over a pipe".to_vec(), Box::new(move |o, e| r.borrow_mut().push((o, e))), Duration::from_secs(300));
        iterate_until(|| !replies.borrow().is_empty(), Duration::from_secs(3));
        assert_eq!(replies.borrow().len(), 1);
        assert_eq!(replies.borrow()[0].1, None);
        assert_eq!(replies.borrow()[0].0, b"password\nfake\n\n");
        assert!(worker.join(Duration::from_secs(2)));
        assert_eq!(worker.returncode(), Some(0));
    }

    #[test]
    fn vpn_cancel_kills_child_and_suppresses_late_result() {
        let _context = crate::testing::TestContext::new();
        let replies: Rc<RefCell<Vec<(Vec<u8>, Option<String>)>>> = Rc::default();
        let r = replies.clone();
        let mut worker = VpnProcess::spawn(vec!["/bin/sleep".into(), "30".into()], Vec::new(),
                                           Box::new(move |o, e| r.borrow_mut().push((o, e))), Duration::from_secs(300));
        let end = Instant::now() + Duration::from_secs(2);
        while worker.shared.lock().unwrap().pid.is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        worker.cancel();
        assert!(worker.join(Duration::from_secs(2)));
        iterate_until(|| false, Duration::from_millis(100));
        assert!(worker.returncode().is_some());
        assert!(replies.borrow().is_empty());
    }

    #[test]
    fn vpn_timeout_reaps_child_and_delivers_error() {
        let _context = crate::testing::TestContext::new();
        let replies: Rc<RefCell<Vec<(Vec<u8>, Option<String>)>>> = Rc::default();
        let r = replies.clone();
        let mut worker = VpnProcess::spawn(vec!["/bin/sleep".into(), "30".into()], Vec::new(),
                                           Box::new(move |o, e| r.borrow_mut().push((o, e))), Duration::from_millis(50));
        assert!(worker.join(Duration::from_secs(2)));
        iterate_until(|| !replies.borrow().is_empty(), Duration::from_secs(1));
        assert_eq!(replies.borrow().len(), 1);
        assert!(replies.borrow()[0].1.as_deref().unwrap().contains("timed out"));
    }
}
