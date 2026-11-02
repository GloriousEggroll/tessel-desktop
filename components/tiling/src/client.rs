//! Owner-only control of the compositor instance on this Wayland display
//! (tiling_client.py). No GTK: the status stream runs inside Tessel Dock.

use crate::compat::{as_int, is_true, py_str, python_os_error_text, truthy};
use serde_json::{Map, Value};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tessel_core::tr;

/// Shown before and after changes that the running compositor cannot load.
pub const SIGN_OUT_REQUIRED: &str = "Sign out and back in to load the labwc tiling update.";

/// `Unavailable` (a RuntimeError subclass), a compositor `RuntimeError`
/// reply, or an `OSError` from the socket calls.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientError {
    Unavailable(String),
    Runtime(String),
    Os(String),
}

impl ClientError {
    pub fn message(&self) -> &str {
        match self {
            ClientError::Unavailable(m) | ClientError::Runtime(m) | ClientError::Os(m) => m,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.message()) }
}

impl std::error::Error for ClientError {}

pub type Reply = Map<String, Value>;

fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })),
    }
}

/// `$XDG_RUNTIME_DIR/tessel-tiling-$WAYLAND_DISPLAY.sock` for a valid display name.
pub fn socket_path(command: &str) -> Result<PathBuf, ClientError> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if display.is_empty() || display.contains('/') || command.len() > 255 {
        return Err(ClientError::Unavailable(tr!("Tiling requires a labwc Wayland session.")));
    }
    Ok(runtime_dir().join(format!("tessel-tiling-{display}.sock")))
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    /// `tempfile.TemporaryDirectory(prefix='wt-', dir=runtime)`.
    fn new(parent: &Path) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let mut template = parent.join("wt-XXXXXXXX").as_os_str().as_bytes().to_vec();
        template.push(0);
        let result = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        if result.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        template.pop();
        Ok(Self(PathBuf::from(std::ffi::OsStr::from_bytes(&template))))
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn os(error: std::io::Error, path: Option<&Path>) -> ClientError { ClientError::Os(python_os_error_text(&error, path)) }

/// One request/reply exchange (`request(command='status')`).
pub fn request(command: &str) -> Result<Reply, ClientError> {
    let runtime = runtime_dir();
    let target = socket_path(command)?;
    let metadata = std::fs::metadata(&target).map_err(|_| ClientError::Unavailable(tr!(SIGN_OUT_REQUIRED)))?;
    if metadata.uid() != unsafe { libc::getuid() } || !metadata.file_type().is_socket() {
        return Err(ClientError::Unavailable(tr!("The tiling control socket is not owned by this user.")));
    }
    let folder = TemporaryDirectory::new(&runtime).map_err(|e| os(e, None))?;
    let reply_path = folder.0.join("reply");
    let channel = UnixDatagram::bind(&reply_path).map_err(|e| os(e, None))?;
    let timeout = Some(Duration::from_millis(1500));
    channel.set_read_timeout(timeout).map_err(|e| os(e, None))?;
    channel.set_write_timeout(timeout).map_err(|e| os(e, None))?;
    channel.connect(&target).map_err(|e| os(e, None))?;
    let no_reply = || ClientError::Unavailable(tr!("The compositor did not respond to the tiling request."));
    channel.send(command.as_bytes()).map_err(|_| no_reply())?;
    let mut buffer = vec![0u8; 65536];
    let length = channel.recv(&mut buffer).map_err(|_| no_reply())?;
    // json.loads accepts bytes in any UTF encoding; the compositor sends UTF-8.
    let result: Value = serde_json::from_slice(&buffer[..length]).map_err(|_| no_reply())?;
    drop(channel);
    drop(folder);
    let unsupported = || ClientError::Unavailable(tr!("This compositor tiling interface is not supported."));
    // `result.get(...)` on a non-dict would raise AttributeError in Python.
    let Value::Object(result) = result else { return Err(unsupported()) };
    let version_ok = match result.get("version") {
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::Bool(true)) => true,
        _ => false,
    };
    if !version_ok || !is_true(result.get("ready")) {
        return Err(unsupported());
    }
    if truthy(result.get("error")) {
        return Err(ClientError::Runtime(py_str(&result["error"])));
    }
    Ok(result)
}

/// `status()`: the reply plus the dock fields, or the unavailable state.
pub fn status_with(call: &dyn Fn(&str) -> Result<Reply, ClientError>) -> Reply {
    match call("status") {
        Ok(result) => {
            let enabled = truthy(result.get("enabled"));
            let tooltip = if enabled { tr!("Window Tiling: On") } else { tr!("Window Tiling: Off") };
            let mut state = result;
            state.insert("text".into(), "▦".into());
            state.insert("class".into(), if enabled { "active" } else { "inactive" }.into());
            state.insert("tooltip".into(), format!("{tooltip}\n{}", tr!("Click to toggle · Right-click for shortcuts")).into());
            state.insert("sign_out_required".into(), false.into());
            state
        }
        Err(error) => {
            let message = error.message().to_owned();
            let sign_out = matches!(error, ClientError::Unavailable(_)) && message == tr!(SIGN_OUT_REQUIRED);
            let mut state = Map::new();
            state.insert("ready".into(), false.into());
            state.insert("enabled".into(), false.into());
            state.insert("text".into(), "▦".into());
            state.insert("class".into(), "unavailable".into());
            state.insert("error".into(), message.clone().into());
            state.insert("tooltip".into(), format!("{}\n{message}", tr!("Window Tiling")).into());
            state.insert("sign_out_required".into(), sign_out.into());
            state
        }
    }
}

pub fn status() -> Reply { status_with(&request) }

/// `json.dumps(status())`.
pub fn status_text(state: &Reply) -> String {
    tessel_core::pyfmt::json_dumps(&Value::Object(state.clone()), &tessel_core::pyfmt::JsonStyle::DEFAULT)
}

/// The compositor PID from a reply when it is a positive integer.
pub fn compositor_pid(state: &Reply) -> Option<i64> { as_int(state.get("compositor_pid")).filter(|pid| *pid > 0) }
