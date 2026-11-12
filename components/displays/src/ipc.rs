//! The tessel-displays daemon socket (tessel-displays:542-594) and the module
//! global `CONFIG_DATA`.

use crate::compat::yaml;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SOCKET_CHUNK_LEN: usize = 8192;

thread_local! {
    static CONFIG_DATA: RefCell<Value> = RefCell::new(Value::Object(Map::new()));
}

/// The last `GET` reply (`{}` when unavailable).
pub fn config_data() -> Value {
    CONFIG_DATA.with(|c| c.borrow().clone())
}

pub fn set_config_data(value: Value) {
    CONFIG_DATA.with(|c| c.replace(value));
}

pub fn with_config_data<R>(f: impl FnOnce(&mut Value) -> R) -> R {
    CONFIG_DATA.with(|c| f(&mut c.borrow_mut()))
}

/// `CONFIG_DATA.get("CFG", {})` (Python's `.get` keeps a present `None`).
pub fn cfg() -> Value {
    config_data().get("CFG").cloned().unwrap_or_else(|| json!({}))
}

/// `cfg()` as the window shows it (`model::resolved_cfg`): a monitor on another
/// connector than its entries were saved under gets them, as the daemon does.
pub fn resolved_cfg() -> Value {
    crate::model::resolved_cfg(&cfg(), &crate::model::state_heads(&config_data()))
}

/// First existing of `tessel-displays.sock` and `tessel-displays.$XDG_VTNR.sock`.
pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/tmp".into());
    let runtime = PathBuf::from(runtime);
    let mut paths = vec![runtime.join("tessel-displays.sock")];
    if let Ok(vt) = std::env::var("XDG_VTNR") {
        if !vt.is_empty() {
            paths.push(runtime.join(format!("tessel-displays.{vt}.sock")));
        }
    }
    // A session started before tessel-displays had its own daemon keeps way-displays until logout.
    paths.push(runtime.join("way-displays.sock"));
    paths.iter().find(|p| p.exists()).cloned().unwrap_or_else(|| paths[0].clone())
}

/// `ipc_operation(operation, payload)`: the raw reply text. A missing socket
/// prints the Python message and yields `""`; other failures are the raised
/// exception's text.
pub fn ipc_operation(operation: &str, payload: &str) -> Result<String, String> {
    ipc_operation_at(&socket_path(), operation, payload)
}

pub fn ipc_operation_at(socket: &Path, operation: &str, payload: &str) -> Result<String, String> {
    let os = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "timed out".to_owned(),
        _ => tessel_core::pyerr::os_error_text(&e),
    };
    let mut client = match UnixStream::connect(socket) {
        Ok(client) => client,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("tessel-displays socket {} not found. Is the server running?", socket.display());
            return Ok(String::new());
        }
        Err(error) => return Err(os(error)),
    };
    client.set_read_timeout(Some(Duration::from_secs(5))).map_err(os)?;
    client.set_write_timeout(Some(Duration::from_secs(5))).map_err(os)?;
    client.write_all(format!("OP: {operation}\n{payload}").as_bytes()).map_err(os)?;
    let mut raw = Vec::new();
    let mut chunk = vec![0u8; SOCKET_CHUNK_LEN];
    loop {
        let count = client.read(&mut chunk).map_err(os)?;
        if count == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..count]);
        if raw.len() > 16 * 1024 * 1024 {
            return Err("Display service response is too large.".into());
        }
    }
    String::from_utf8(raw).map_err(|e| format!("'utf-8' codec can't decode bytes: {e}"))
}

/// `yaml.safe_dump({"CFG": cfg}, sort_keys=False).rstrip()`.
pub fn config_payload(cfg: &Value) -> String {
    let mut wrapper = Map::new();
    wrapper.insert("CFG".into(), cfg.clone());
    yaml::safe_dump(&Value::Object(wrapper)).trim_end().to_owned()
}

pub fn ipc_config_operation(operation: &str, cfg: &Value) -> Result<String, String> {
    ipc_operation(operation, &config_payload(cfg))
}

/// Refresh `CONFIG_DATA` from `GET`; true when it holds a `STATE` mapping.
pub fn get_config_data() -> bool {
    get_config_data_from(|| ipc_operation("GET", ""))
}

pub fn get_config_data_from(get: impl FnOnce() -> Result<String, String>) -> bool {
    let reply = match get() {
        Ok(reply) => reply,
        Err(error) => {
            println!("Failed to load config data: {error}");
            return false;
        }
    };
    match yaml::load(&reply) {
        Ok(data) if data.get("STATE").is_some_and(Value::is_object) => {
            set_config_data(data);
            true
        }
        Ok(_) => {
            set_config_data(json!({}));
            false
        }
        Err(error) => {
            println!("Failed to load config data: {error}");
            false
        }
    }
}
