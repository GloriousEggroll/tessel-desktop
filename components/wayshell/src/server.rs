//! `$XDG_RUNTIME_DIR/wayshell.sock` server (wayshell:4016-4167, contract §2.1).
//!
//! One JSON object + `\n` per connection; one JSON line back; close. Requests
//! are read asynchronously and dispatched on the GLib main loop. The path is
//! unlinked at startup and **not** at shutdown (Python never set `_cli_sock`).

use gio::prelude::*;
use gtk::gdk;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use suite_core::pyjson::py_str;
use suite_core::tr;

/// What the socket commands need from the daemon (the app, or a test fake).
pub trait Shell {
    fn switch_window(&self, reverse: bool) -> bool;
    /// `None` before the switcher exists → `{"ok":true,"ready":false}`.
    fn switcher_status(&self) -> Option<Map<String, Value>>;
    /// `monitor`: the raw request value (absent/null = none).
    fn show_activities(&self, monitor: Option<&Value>) -> bool;
    fn monitor_keys(&self) -> Vec<String>;
    fn apply_wallpaper(&self, keys: &[String], file_path: Option<&str>, mode: &str, color: &gdk::RGBA);
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn reply(ok: bool, msg: impl Into<String>) -> Value {
    json!({"ok": ok, "msg": msg.into()})
}

/// `os.path.isfile`.
fn is_file(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// `Gdk.RGBA().parse(color)` for a request value.
fn parse_color(value: &Value) -> Result<Option<gdk::RGBA>, String> {
    match value {
        Value::String(text) => Ok(gdk::RGBA::parse(text.as_str()).ok()),
        Value::Null => Err("Argument 1 does not allow None as a value".into()),
        other => Err(format!("Must be string, not {}", type_name(other))),
    }
}

/// `req.get(key, default)` → a value usable where Python wants a str.
fn get_or<'a>(req: &'a Map<String, Value>, key: &str, default: &'a Value) -> &'a Value {
    req.get(key).unwrap_or(default)
}

/// `_serve_one` for an already-read line (without the newline).
pub fn dispatch(shell: &dyn Shell, line: Option<&str>) -> Value {
    let Some(line) = line else {
        return reply(false, "empty request");
    };
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return reply(false, format!("bad json: {}", python_json_error(line, &e))),
    };
    let Value::Object(req) = req else {
        return reply(false, format!("'{}' object has no attribute 'get'", type_name(&req)));
    };
    let cmd = req.get("cmd").cloned().unwrap_or(Value::Null);
    match cmd.as_str() {
        Some("switch-window") => {
            let reverse = req.get("reverse").cloned().unwrap_or(Value::Bool(false));
            let Value::Bool(reverse) = reverse else {
                return reply(false, tr!("Invalid switching direction"));
            };
            let ok = shell.switch_window(reverse);
            reply(ok, if ok { String::new() } else { tr!("Window switching is not ready.") })
        }
        Some("switcher-status") => {
            let mut out = Map::new();
            out.insert("ok".into(), Value::Bool(true));
            match shell.switcher_status() {
                Some(status) => out.extend(status),
                None => {
                    out.insert("ready".into(), Value::Bool(false));
                }
            }
            Value::Object(out)
        }
        Some("show-activities") => {
            let monitor = req.get("monitor").filter(|v| !v.is_null());
            let ok = shell.show_activities(monitor);
            reply(ok, if ok { String::new() } else { tr!("Activities are not ready or the requested monitor is unavailable.") })
        }
        Some("list-monitors") => json!({"ok": true, "monitors": shell.monitor_keys()}),
        Some("set") => {
            let mon = req.get("monitor").cloned().unwrap_or(Value::Null);
            // fpath = req.get("file") or ""
            let fpath = match req.get("file") {
                Some(v) if suite_core::pyjson::truthy(v) => py_str(v),
                _ => String::new(),
            };
            let fill = Value::String("Fill".into());
            let black = Value::String("black".into());
            let mode = py_str(get_or(&req, "mode", &fill));
            let color = get_or(&req, "color", &black);
            let monitors = shell.monitor_keys();
            let keys: Vec<Value> = if mon == Value::String("all".into()) {
                monitors.iter().cloned().map(Value::String).collect()
            } else {
                vec![mon.clone()]
            };
            let missing: Vec<&Value> = keys.iter().filter(|k| !k.as_str().is_some_and(|k| monitors.iter().any(|m| m == k))).collect();
            if !missing.is_empty() {
                if let Some((index, bad)) = missing.iter().enumerate().find(|(_, v)| !v.is_string()) {
                    return reply(false, format!("sequence item {index}: expected str instance, {} found", type_name(bad)));
                }
                let names: Vec<&str> = missing.iter().filter_map(|v| v.as_str()).collect();
                return reply(false, tr!("Unknown: {value1}", value1 = names.join(", ")));
            }
            let rgba = match parse_color(color) {
                Ok(Some(rgba)) => rgba,
                Ok(None) => return reply(false, tr!("Bad color '{value1}'", value1 = py_str(color))),
                Err(e) => return reply(false, e),
            };
            if mode != "Color Only" && !fpath.is_empty() && !is_file(&fpath) {
                return reply(false, tr!("Missing file '{value1}'", value1 = fpath.clone()));
            }
            let keys: Vec<String> = keys.iter().filter_map(|k| k.as_str().map(str::to_string)).collect();
            let file = if mode == "Color Only" { None } else { Some(fpath.as_str()) };
            shell.apply_wallpaper(&keys, file, &mode, &rgba);
            reply(true, "OK")
        }
        Some("apply-json") => {
            let cfg = match read_json(req.get("json_path")) {
                Ok(v) => v,
                Err(e) => return reply(false, tr!("Read error: {value1}", value1 = e)),
            };
            let Value::Object(cfg) = cfg else {
                return reply(false, format!("'{}' object has no attribute 'items'", type_name(&cfg)));
            };
            let monitors = shell.monitor_keys();
            for (key, entry) in &cfg {
                if !monitors.iter().any(|m| m == key) {
                    continue;
                }
                let Value::Object(entry) = entry else {
                    return reply(false, format!("'{}' object has no attribute 'get'", type_name(entry)));
                };
                let fill = Value::String("Fill".into());
                let black = Value::String("black".into());
                let mode = py_str(get_or(entry, "mode", &fill));
                let color = get_or(entry, "color", &black);
                let path = entry.get("file_path").filter(|v| !v.is_null()).map(py_str);
                let rgba = match parse_color(color) {
                    Ok(Some(rgba)) => rgba,
                    Ok(None) => continue,
                    Err(e) => return reply(false, e),
                };
                if mode != "Color Only" {
                    if let Some(p) = path.as_deref() {
                        if !p.is_empty() && !is_file(p) {
                            continue;
                        }
                    }
                }
                let file = if mode == "Color Only" { None } else { path.as_deref() };
                shell.apply_wallpaper(&[key.clone()], file, &mode, &rgba);
            }
            reply(true, "OK")
        }
        _ => reply(false, tr!("Unknown cmd '{value1}'", value1 = py_str(&cmd))),
    }
}

fn read_json(path: Option<&Value>) -> Result<Value, String> {
    let path = match path {
        Some(Value::String(p)) => p.clone(),
        None | Some(Value::Null) => return Err("expected str, bytes or os.PathLike object, not NoneType".into()),
        Some(other) => return Err(format!("expected str, bytes or os.PathLike object, not {}", type_name(other))),
    };
    let text = std::fs::read(&path).map_err(|e| suite_core::pyerr::os_error_path(&e, Path::new(&path)))?;
    let text = String::from_utf8(text).map_err(|e| format!("'utf-8' codec can't decode byte 0x{:02x} in position {}: invalid start byte",
                                                          e.as_bytes()[e.utf8_error().valid_up_to()], e.utf8_error().valid_up_to()))?;
    serde_json::from_str(&text).map_err(|e| python_json_error(&text, &e))
}

/// Approximate `json.JSONDecodeError` text: "Expecting value: line L column C (char N)".
fn python_json_error(text: &str, error: &serde_json::Error) -> String {
    let line = error.line();
    let column = error.column();
    let char_index: usize = text.split('\n').take(line.saturating_sub(1)).map(|l| l.chars().count() + 1).sum::<usize>() + column.saturating_sub(1);
    let kind = if text.trim().is_empty() || error.is_eof() && column <= 1 {
        "Expecting value"
    } else if error.to_string().starts_with("trailing characters") {
        "Extra data"
    } else if error.to_string().starts_with("expected value") {
        "Expecting value"
    } else {
        "Invalid JSON"
    };
    let (line, column, char_index) = if text.trim().is_empty() { (1, 1, 0) } else { (line, column.max(1), char_index) };
    format!("{kind}: line {line} column {column} (char {char_index})")
}

pub struct Server {
    listener: gio::SocketListener,
    cancel: gio::Cancellable,
    pub path: PathBuf,
}

impl Server {
    /// `_ensure_cli_socket`: unlink, listen, accept forever, chmod 0600.
    pub fn start(path: PathBuf, shell: Rc<dyn Shell>) -> Result<Rc<Server>, glib::Error> {
        if path.exists() || std::fs::symlink_metadata(&path).is_ok() {
            let _ = std::fs::remove_file(&path);
        }
        let listener = gio::SocketListener::new();
        let address = gio::UnixSocketAddress::new(&path);
        listener.add_address(&address, gio::SocketType::Stream, gio::SocketProtocol::Default, None::<&glib::Object>)?;
        let server = Rc::new(Server { listener, cancel: gio::Cancellable::new(), path: path.clone() });
        accept(&server.listener, &server.cancel, shell);
        let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
        Ok(server)
    }

    pub fn close(&self) {
        self.cancel.cancel();
        self.listener.close();
    }
}

fn accept(listener: &gio::SocketListener, cancel: &gio::Cancellable, shell: Rc<dyn Shell>) {
    let again = listener.clone();
    let cancel2 = cancel.clone();
    listener.accept_async(Some(cancel), move |result| {
        if cancel2.is_cancelled() {
            return;
        }
        if let Ok((connection, _)) = result {
            serve(connection, shell.clone());
        }
        accept(&again, &cancel2, shell);
    });
}

fn serve(connection: gio::SocketConnection, shell: Rc<dyn Shell>) {
    let input = gio::DataInputStream::new(&connection.input_stream());
    input.set_newline_type(gio::DataStreamNewlineType::Lf);
    let input2 = input.clone();
    input.read_line_utf8_async(glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |result| {
        let _keep = &input2;
        match result {
            Ok(line) => {
                // Dispatch at idle on the main loop like Python's idle_add.
                glib::idle_add_local_once(move || {
                    let answer = dispatch(&*shell, line.as_deref());
                    let payload = format!("{}\n", crate::cli::dumps(&answer));
                    let output = connection.output_stream();
                    let conn = connection.clone();
                    output.write_all_async(payload.into_bytes(), glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |_| {
                        let _ = conn.close(None::<&gio::Cancellable>);
                    });
                });
            }
            Err(_) => {
                // Invalid UTF-8 or a read error: Python raised before replying.
                let _ = connection.close(None::<&gio::Cancellable>);
            }
        }
    });
}

/// `GLib.get_user_runtime_dir()/wayshell.sock`.
pub fn socket_path() -> PathBuf {
    glib::user_runtime_dir().join("wayshell.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        applied: RefCell<Vec<(Vec<String>, Option<String>, String, String)>>,
        switched: RefCell<Vec<bool>>,
        activities: RefCell<Vec<Option<Value>>>,
    }

    impl Shell for Fake {
        fn switch_window(&self, reverse: bool) -> bool {
            self.switched.borrow_mut().push(reverse);
            true
        }
        fn switcher_status(&self) -> Option<Map<String, Value>> {
            None
        }
        fn show_activities(&self, monitor: Option<&Value>) -> bool {
            self.activities.borrow_mut().push(monitor.cloned());
            monitor.is_none_or(|m| m.as_str() == Some("HDMI-A-1"))
        }
        fn monitor_keys(&self) -> Vec<String> {
            vec!["HDMI-A-1".into(), "DP-1".into()]
        }
        fn apply_wallpaper(&self, keys: &[String], file: Option<&str>, mode: &str, color: &gdk::RGBA) {
            self.applied.borrow_mut().push((keys.to_vec(), file.map(str::to_string), mode.to_string(), color.to_str().to_string()));
        }
    }

    fn run(fake: &Fake, line: &str) -> String {
        crate::cli::dumps(&dispatch(fake, Some(line)))
    }

    #[test]
    fn errors_and_switch_direction() {
        let f = Fake::default();
        assert_eq!(crate::cli::dumps(&dispatch(&f, None)), r#"{"ok": false, "msg": "empty request"}"#);
        assert!(run(&f, "{").starts_with(r#"{"ok": false, "msg": "bad json: "#));
        assert_eq!(run(&f, ""), r#"{"ok": false, "msg": "bad json: Expecting value: line 1 column 1 (char 0)"}"#);
        assert_eq!(run(&f, r#"{"cmd": "nope"}"#), r#"{"ok": false, "msg": "Unknown cmd 'nope'"}"#);
        assert_eq!(run(&f, r#"{}"#), r#"{"ok": false, "msg": "Unknown cmd 'None'"}"#);
        assert_eq!(run(&f, r#"[1]"#), r#"{"ok": false, "msg": "'list' object has no attribute 'get'"}"#);
        assert_eq!(run(&f, r#"{"cmd": "switch-window", "reverse": 1}"#), r#"{"ok": false, "msg": "Invalid switching direction"}"#);
        assert_eq!(run(&f, r#"{"cmd": "switch-window", "reverse": "true"}"#), r#"{"ok": false, "msg": "Invalid switching direction"}"#);
        assert_eq!(run(&f, r#"{"cmd": "switch-window"}"#), r#"{"ok": true, "msg": ""}"#);
        assert_eq!(run(&f, r#"{"cmd": "switch-window", "reverse": true}"#), r#"{"ok": true, "msg": ""}"#);
        assert_eq!(*f.switched.borrow(), vec![false, true]);
        assert_eq!(run(&f, r#"{"cmd": "switcher-status"}"#), r#"{"ok": true, "ready": false}"#);
        assert_eq!(run(&f, r#"{"cmd": "list-monitors"}"#), r#"{"ok": true, "monitors": ["HDMI-A-1", "DP-1"]}"#);
    }

    #[test]
    fn show_activities_forwards_optional_monitor() {
        let f = Fake::default();
        assert_eq!(run(&f, r#"{"cmd": "show-activities"}"#), r#"{"ok": true, "msg": ""}"#);
        assert_eq!(run(&f, r#"{"cmd": "show-activities", "monitor": null}"#), r#"{"ok": true, "msg": ""}"#);
        assert_eq!(run(&f, r#"{"cmd": "show-activities", "monitor": "DP-9"}"#),
                   r#"{"ok": false, "msg": "Activities are not ready or the requested monitor is unavailable."}"#);
        assert_eq!(*f.activities.borrow(), vec![None, None, Some(json!("DP-9"))]);
    }

    #[test]
    fn set_validates_and_accepts_null_file() {
        let dir = std::env::temp_dir().join(format!("ws-server-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("a.png");
        std::fs::write(&image, b"x").unwrap();
        let f = Fake::default();
        assert_eq!(run(&f, r#"{"cmd": "set", "monitor": "X"}"#), r#"{"ok": false, "msg": "Unknown: X"}"#);
        assert_eq!(run(&f, r#"{"cmd": "set", "monitor": null}"#), r#"{"ok": false, "msg": "sequence item 0: expected str instance, NoneType found"}"#);
        assert_eq!(run(&f, r#"{"cmd": "set", "monitor": "DP-1", "color": "nocolor"}"#), r#"{"ok": false, "msg": "Bad color 'nocolor'"}"#);
        assert_eq!(run(&f, r#"{"cmd": "set", "monitor": "DP-1", "file": "/nonexistent/x.png"}"#), r#"{"ok": false, "msg": "Missing file '/nonexistent/x.png'"}"#);
        // Global Settings sends "file": null for Color Only.
        assert_eq!(run(&f, r#"{"cmd": "set", "monitor": "DP-1", "file": null, "mode": "Color Only", "color": "rgb(1,2,3)"}"#), r#"{"ok": true, "msg": "OK"}"#);
        let line = format!(r#"{{"cmd": "set", "monitor": "all", "file": "{}", "mode": "Fit", "color": "red"}}"#, image.display());
        assert_eq!(run(&f, &line), r#"{"ok": true, "msg": "OK"}"#);
        // A missing file is fine for Color Only.
        assert_eq!(run(&f, r#"{"cmd": "set", "monitor": "DP-1", "file": "/nonexistent", "mode": "Color Only"}"#), r#"{"ok": true, "msg": "OK"}"#);
        let applied = f.applied.borrow();
        assert_eq!(applied[0], (vec!["DP-1".to_string()], None, "Color Only".to_string(), "rgb(1,2,3)".to_string()));
        assert_eq!(applied[1], (vec!["HDMI-A-1".to_string(), "DP-1".to_string()], Some(image.display().to_string()), "Fit".to_string(), "rgb(255,0,0)".to_string()));
        assert_eq!(applied[2].1, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_json_skips_unknown_bad_and_missing() {
        let dir = std::env::temp_dir().join(format!("ws-apply-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("a.png");
        std::fs::write(&image, b"x").unwrap();
        let cfg = dir.join("w.json");
        std::fs::write(&cfg, format!(r#"{{"_view_prefs": {{"hide_icons": true}}, "DP-1": {{"file_path": "{}", "mode": "Fill", "color": "rgb(0,0,0)"}},
            "HDMI-A-1": {{"file_path": "/missing.png", "mode": "Fill"}}, "Other": {{}}}}"#, image.display())).unwrap();
        let f = Fake::default();
        assert_eq!(run(&f, &format!(r#"{{"cmd": "apply-json", "json_path": "{}"}}"#, cfg.display())), r#"{"ok": true, "msg": "OK"}"#);
        assert_eq!(f.applied.borrow().len(), 1);
        assert_eq!(f.applied.borrow()[0].0, vec!["DP-1".to_string()]);
        assert_eq!(run(&f, r#"{"cmd": "apply-json", "json_path": "/nonexistent.json"}"#),
                   r#"{"ok": false, "msg": "Read error: [Errno 2] No such file or directory: '/nonexistent.json'"}"#);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
