//! login_settings_helper.py: privileged, fixed-destination appearance writer.
//! Never controls services or PAM.
//!
//! Run by `pkexec` as root with a JSON request from the logged-in user on stdin:
//! `{"expected": <current appearance>, "appearance": <new appearance>}`. Every
//! field is validated, the session must be installed and the wallpaper must be
//! a system-readable image. The only file written is
//! `/etc/waylogin/appearance.json` (atomic replace, 0644, Python `json.dump`
//! bytes). This binary links only GLib/libc (no GTK) and has no environment or flag
//! overrides; tests inject paths through `cfg(test)` code only.
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use suite_core::pyfmt::{json_dumps, JsonStyle};
use suite_core::pyjson::py_repr_value;
// The shared crates/suite-lock login model, compiled into this binary directly:
// linking the `suite_lock` crate (built with its `gtk` feature for the greeter)
// would pull GTK, media and TLS libraries into a root process.
#[path = "../../../../crates/suite-lock/src/login_model.rs"]
#[allow(dead_code)]
mod login_model;
use login_model::{default_session_roots, sessions, Appearance, CONFIG};

const MAX_REQUEST: u64 = 32768;
const FIELDS: [&str; 8] = ["picture", "placement", "background_color", "blur_strength", "transparency", "idle_seconds",
                           "clock_24h", "default_session"];
const IMAGES: [&str; 6] = [".png", ".jpg", ".jpeg", ".webp", ".bmp", ".gif"];

/// `str(OSError(errno, strerror, filename))`.
fn os_error(code: i32, filename: &[u8]) -> String {
    let text = String::from_utf8_lossy(filename).into_owned();
    // SAFETY: strerror returns a valid C string.
    let message = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
    format!("[Errno {code}] {message}: {}", py_repr_value(&Value::String(text)))
}

fn io_error(error: &std::io::Error, filename: &[u8]) -> String {
    match error.raw_os_error() {
        Some(code) => os_error(code, filename),
        None => error.to_string(),
    }
}

/// `os.path.realpath(path, strict=True)` of CPython 3.14 (posixpath), with its
/// exact error filenames: the failing component, ENOTDIR for a non-directory
/// with components left, ELOOP for a symlink loop.
fn realpath_strict(filename: &[u8]) -> Result<Vec<u8>, String> {
    enum Part {
        Name(Vec<u8>),
        Link(Vec<u8>),
    }
    let mut rest: Vec<Option<Part>> = filename.split(|b| *b == b'/').rev().map(|p| Some(Part::Name(p.to_vec()))).collect();
    let mut part_count = rest.len();
    let mut path: Vec<u8> = if filename.starts_with(b"/") {
        b"/".to_vec()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.into_os_string().into_vec()
    };
    let mut seen: std::collections::HashMap<Vec<u8>, Option<Vec<u8>>> = Default::default();
    while part_count > 0 {
        let name = match rest.pop() {
            Some(Some(Part::Name(name))) => name,
            Some(None) => {
                // The marker: a symlink's target has been resolved.
                if let Some(Some(Part::Link(link))) = rest.pop() {
                    seen.insert(link, Some(path.clone()));
                }
                continue;
            }
            _ => break,
        };
        part_count -= 1;
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            let cut = path.iter().rposition(|b| *b == b'/').unwrap_or(0);
            path.truncate(cut);
            if path.is_empty() {
                path = b"/".to_vec();
            }
            continue;
        }
        let mut newpath = path.clone();
        if path != b"/" {
            newpath.push(b'/');
        }
        newpath.extend_from_slice(&name);
        let metadata = std::fs::symlink_metadata(OsStr::from_bytes(&newpath)).map_err(|e| io_error(&e, &newpath))?;
        if !metadata.file_type().is_symlink() {
            if part_count > 0 && !metadata.is_dir() {
                return Err(os_error(libc::ENOTDIR, &newpath));
            }
            path = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            match cached {
                Some(resolved) => {
                    path = resolved.clone();
                    continue;
                }
                None => return Err(os_error(libc::ELOOP, &newpath)),
            }
        }
        let target = std::fs::read_link(OsStr::from_bytes(&newpath)).map_err(|e| io_error(&e, &newpath))?.into_os_string().into_vec();
        if target.starts_with(b"/") {
            path = b"/".to_vec();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(Part::Link(newpath)));
        rest.push(None);
        let parts: Vec<&[u8]> = target.split(|b| *b == b'/').collect();
        part_count += parts.len();
        for part in parts.into_iter().rev() {
            rest.push(Some(Part::Name(part.to_vec())));
        }
    }
    Ok(path)
}

/// `PurePath(path).suffix` (os.path.splitext of the last component).
fn suffix(path: &[u8]) -> String {
    let name = path.rsplit(|b| *b == b'/').next().unwrap_or_default();
    let Some(dot) = name.iter().rposition(|b| *b == b'.') else { return String::new() };
    if name[..dot].iter().all(|b| *b == b'.') {
        return String::new();
    }
    String::from_utf8_lossy(&name[dot..]).into_owned()
}

/// `PurePath(path).parents` of an absolute, normalized path.
fn parents(path: &[u8]) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    let mut current = path.to_vec();
    while current != b"/" {
        let cut = current.iter().rposition(|b| *b == b'/').unwrap_or(0);
        current.truncate(cut.max(1));
        result.push(current.clone());
    }
    result
}

/// Python `==` between JSON-decoded values (`0 == False`, `50 == 50.0`).
fn py_eq(a: &Value, b: &Value) -> bool {
    fn number(value: &Value) -> Option<Result<i128, f64>> {
        match value {
            Value::Bool(flag) => Some(Ok(*flag as i128)),
            Value::Number(n) => Some(match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => Ok(i as i128),
                (_, Some(u)) => Ok(u as i128),
                _ => Err(n.as_f64().unwrap_or(f64::NAN)),
            }),
            _ => None,
        }
    }
    match (number(a), number(b)) {
        (Some(x), Some(y)) => {
            return match (x, y) {
                (Ok(x), Ok(y)) => x == y,
                (Err(x), Err(y)) => x == y,
                (Ok(i), Err(f)) | (Err(f), Ok(i)) => f.is_finite() && f.fract() == 0.0 && f.abs() < 1e38 && f as i128 == i,
            }
        }
        (Some(_), None) | (None, Some(_)) => return false,
        _ => {}
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| py_eq(x, y)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w))),
        _ => false,
    }
}

fn same_keys(map: &Map<String, Value>, keys: &[&str]) -> bool {
    map.len() == keys.len() && keys.iter().all(|k| map.contains_key(*k))
}

/// `validate_request(data, current, available)`.
fn validate_request(data: &Value, current: &Appearance, available: &[String]) -> Result<Appearance, String> {
    let Some(data) = data.as_object().filter(|d| same_keys(d, &["expected", "appearance"])) else {
        return Err("Invalid appearance request".into());
    };
    if !py_eq(&data["expected"], &Value::Object(current.to_map())) {
        return Err("Login settings changed elsewhere. Reload the page and try again.".into());
    }
    let value = &data["appearance"];
    if !value.as_object().is_some_and(|v| same_keys(v, &FIELDS)) {
        return Err("Unknown appearance setting".into());
    }
    let appearance = Appearance::from_dict(value)?;
    if !available.contains(&appearance.default_session) {
        return Err("Choose an installed Wayland session".into());
    }
    if !appearance.picture.is_empty() {
        let path = realpath_strict(appearance.picture.as_bytes())?;
        if !IMAGES.contains(&suffix(&path).to_lowercase().as_str()) {
            return Err("Choose a wallpaper image; login backgrounds do not support video".into());
        }
        let info = std::fs::metadata(OsStr::from_bytes(&path)).map_err(|e| io_error(&e, &path))?;
        if !info.file_type().is_file() || info.mode() & libc::S_IROTH == 0 {
            return Err("Choose a system-readable wallpaper image".into());
        }
        for parent in parents(&path) {
            let info = std::fs::metadata(OsStr::from_bytes(&parent)).map_err(|e| io_error(&e, &parent))?;
            if info.mode() & libc::S_IXOTH == 0 {
                return Err("Wallpaper folders must be accessible to the login screen".into());
            }
        }
    }
    Ok(appearance)
}

/// `json.dump(asdict(appearance), stream, indent=2); stream.write('\n')`.
fn config_bytes(appearance: &Appearance) -> String {
    json_dumps(&Value::Object(appearance.to_map()), &JsonStyle { indent: Some(2), ..JsonStyle::DEFAULT }) + "\n"
}

/// `Path.mkdir(mode, parents=True, exist_ok=True)`.
fn mkdir_parents(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    let bytes = path.as_os_str().as_bytes();
    match std::fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent().filter(|p| *p != path && !p.as_os_str().is_empty()) {
                mkdir_parents(parent, 0o777)?;
            }
            match std::fs::DirBuilder::new().mode(mode).create(path) {
                Ok(()) => Ok(()),
                Err(_) if path.is_dir() => Ok(()),
                Err(error) => Err(io_error(&error, bytes)),
            }
        }
        Err(_) if path.is_dir() => Ok(()),
        Err(error) => Err(io_error(&error, bytes)),
    }
}

fn random_name() -> String {
    const CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut bytes = [0u8; 8];
    // SAFETY: getrandom into a local buffer of the given length.
    let filled = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if filled != bytes.len() as isize {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
            ^ std::process::id() as u128;
        bytes = (seed as u64).to_le_bytes();
    }
    bytes.iter().map(|b| CHARACTERS[*b as usize % CHARACTERS.len()] as char).collect()
}

/// `tempfile.mkstemp(prefix='.appearance-', dir=directory)`: 0600,
/// O_EXCL | O_NOFOLLOW | O_CLOEXEC, retried on name collisions.
fn mkstemp(directory: &Path) -> Result<(std::fs::File, PathBuf), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut last = String::new();
    for _ in 0..10000 {
        let path = directory.join(format!(".appearance-{}", random_name()));
        match std::fs::OpenOptions::new().read(true).write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last = io_error(&error, path.as_os_str().as_bytes());
            }
            Err(error) => return Err(io_error(&error, path.as_os_str().as_bytes())),
        }
    }
    Err(last)
}

/// Atomic replacement of `config`.
fn write_config(config: &Path, appearance: &Appearance) -> Result<(), String> {
    let directory = config.parent().ok_or("Invalid destination")?;
    mkdir_parents(directory, 0o755)?;
    let (mut file, temporary) = mkstemp(directory)?;
    let temporary_bytes = temporary.as_os_str().as_bytes().to_vec();
    let result = (|| {
        file.write_all(config_bytes(appearance).as_bytes()).map_err(|e| io_error(&e, &temporary_bytes))?;
        file.flush().map_err(|e| io_error(&e, &temporary_bytes))?;
        file.sync_all().map_err(|e| io_error(&e, &temporary_bytes))?;
        // fchmod on the open descriptor: the same 0644 without re-resolving the path.
        file.set_permissions(std::fs::Permissions::from_mode(0o644)).map_err(|e| io_error(&e, &temporary_bytes))?;
        drop(file);
        std::fs::rename(&temporary, config).map_err(|error| match error.raw_os_error() {
            // str(OSError) with two filenames: "… 'src' -> 'dst'".
            Some(code) => format!("{} -> {}", os_error(code, &temporary_bytes),
                                  py_repr_value(&Value::String(config.to_string_lossy().into_owned()))),
            None => error.to_string(),
        })
    })();
    // `finally: if os.path.exists(temporary): os.unlink(temporary)`
    if std::fs::metadata(&temporary).is_ok() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Python's `main()`, with the destination and session roots as parameters.
/// Production passes the fixed `/etc/waylogin/appearance.json`, the system
/// session roots and the real effective UID; only `cfg(test)` code passes others.
fn run(config: &Path, roots: &[PathBuf], euid: u32, argc: usize, stdin: &mut dyn Read) -> Result<(), String> {
    if euid != 0 || argc != 1 {
        return Err("Administrator authorization is required".into());
    }
    let mut raw = Vec::new();
    stdin.take(MAX_REQUEST + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    if raw.len() as u64 > MAX_REQUEST {
        return Err("Appearance request is too large".into());
    }
    if std::fs::symlink_metadata(config).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Refusing to replace a symbolic link".into());
    }
    let data: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let current = Appearance::load(Some(config))?;
    let available: Vec<String> = sessions(roots).into_iter().map(|s| s.ident).collect();
    let appearance = validate_request(&data, &current, &available)?;
    write_config(config, &appearance)
}

fn main() {
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    let argc = std::env::args_os().count();
    if let Err(message) = run(Path::new(CONFIG), &default_session_roots(), euid, argc, &mut std::io::stdin().lock()) {
        let _ = writeln!(std::io::stderr(), "{message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        root: PathBuf,
        config: PathBuf,
        roots: Vec<PathBuf>,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let root = std::env::temp_dir().join(format!("wl-helper-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let sessions = root.join("sessions");
            std::fs::create_dir_all(&sessions).unwrap();
            std::fs::write(sessions.join("labwc.desktop"), "[Desktop Entry]\nName=labwc\nExec=sh\n").unwrap();
            Fixture { config: root.join("etc/waylogin/appearance.json"), roots: vec![sessions], root }
        }

        /// The write path with an injected euid of 0 — this test process is
        /// unprivileged and only ever writes inside its own temporary directory.
        fn run(&self, request: &[u8]) -> Result<(), String> {
            run(&self.config, &self.roots, 0, 1, &mut &request[..])
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn current() -> Appearance {
        Appearance { picture: String::new(), ..Appearance::default() }
    }

    fn request(appearance: Value) -> Value {
        json!({"expected": Value::Object(Appearance::default().to_map()), "appearance": appearance})
    }

    fn sessions() -> Vec<String> {
        vec!["labwc".into()]
    }

    /// Python: `json.dumps(asdict(Appearance(...)), indent=2) + "\n"` (CPython 3.14).
    const PY_CONFIG: &str = "{\n  \"picture\": \"/usr/share/backgrounds/caf\\u00e9 \\ud83d\\ude00.png\",\n  \"placement\": \"center\",\n  \"background_color\": \"#ABCdef\",\n  \"blur_strength\": 70,\n  \"transparency\": 20,\n  \"idle_seconds\": 60,\n  \"clock_24h\": true,\n  \"default_session\": \"labwc\"\n}\n";
    const PY_DEFAULT: &str = "{\n  \"picture\": \"/usr/share/wallpapers/nobara-winds1.webp\",\n  \"placement\": \"fill\",\n  \"background_color\": \"#161820\",\n  \"blur_strength\": 50,\n  \"transparency\": 5,\n  \"idle_seconds\": 30,\n  \"clock_24h\": false,\n  \"default_session\": \"labwc\"\n}\n";

    #[test]
    fn config_bytes_match_python_json_dump() {
        assert_eq!(config_bytes(&Appearance::default()), PY_DEFAULT);
        let appearance = Appearance { picture: "/usr/share/backgrounds/café 😀.png".into(), placement: "center".into(),
            background_color: "#ABCdef".into(), blur_strength: 70, transparency: 20, idle_seconds: 60, clock_24h: true,
            default_session: "labwc".into() };
        assert_eq!(config_bytes(&appearance), PY_CONFIG);
    }

    #[test]
    fn allowed_appearance_roundtrip() {
        let mut data = json!({"expected": Value::Object(current().to_map()), "appearance": Value::Object(current().to_map())});
        for (key, value) in [("blur_strength", json!(70)), ("transparency", json!(20)), ("idle_seconds", json!(60)), ("clock_24h", json!(true))] {
            data["appearance"][key] = value;
        }
        let value = validate_request(&data, &current(), &sessions()).unwrap();
        assert_eq!((value.blur_strength, value.transparency, value.idle_seconds, value.clock_24h), (70, 20, 60, true));
        // Python equality for `expected`: 0 == False, 50 == 50.0.
        let mut loose = data.clone();
        loose["expected"]["clock_24h"] = json!(0);
        loose["expected"]["blur_strength"] = json!(50.0);
        assert!(validate_request(&loose, &current(), &sessions()).is_ok());
    }

    #[test]
    fn rejects_unknown_fields_concurrent_edits_and_invalid_values() {
        let base = Value::Object(current().to_map());
        for (key, value, message) in [
            ("autologin", json!(true), "Unknown appearance setting"),
            ("blur_strength", json!(-1), "Invalid blur_strength"),
            ("transparency", json!(true), "Invalid transparency"),
            ("idle_seconds", json!(0), "Invalid idle_seconds"),
            ("clock_24h", json!("true"), "Invalid clock or session setting"),
            ("default_session", json!("/bin/sh"), "Choose an installed Wayland session"),
            ("background_color", json!("url(file:///etc/passwd)"), "Invalid background color"),
            ("placement", json!("other"), "Invalid wallpaper placement"),
            ("blur_strength", json!(50.0), "Invalid blur_strength"),
        ] {
            let mut appearance = base.clone();
            appearance[key] = value;
            let data = json!({"expected": base.clone(), "appearance": appearance});
            assert_eq!(validate_request(&data, &current(), &sessions()).unwrap_err(), message, "{key}");
        }
        let data = json!({"expected": base.clone(), "appearance": base.clone()});
        assert_eq!(validate_request(&data, &Appearance { idle_seconds: 80, ..current() }, &sessions()).unwrap_err(),
                   "Login settings changed elsewhere. Reload the page and try again.");
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove("picture");
        for (data, message) in [
            (json!([]), "Invalid appearance request"),
            (json!({"expected": base.clone()}), "Invalid appearance request"),
            (json!({"expected": base.clone(), "appearance": base.clone(), "x": 1}), "Invalid appearance request"),
            (json!({"expected": [], "appearance": base.clone()}), "Login settings changed elsewhere. Reload the page and try again."),
            (json!({"expected": base.clone(), "appearance": []}), "Unknown appearance setting"),
            (json!({"expected": base.clone(), "appearance": missing}), "Unknown appearance setting"),
        ] {
            assert_eq!(validate_request(&data, &current(), &sessions()).unwrap_err(), message);
        }
    }

    #[test]
    fn rejects_private_or_non_image_wallpapers() {
        let fixture = Fixture::new("pictures");
        let private = fixture.root.join("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let picture = private.join("image.jpg");
        std::fs::write(&picture, b"fixture").unwrap();
        std::fs::set_permissions(&picture, std::fs::Permissions::from_mode(0o644)).unwrap();
        let public = fixture.root.join("public");
        std::fs::create_dir_all(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        let unreadable = public.join("secret.png");
        std::fs::write(&unreadable, b"x").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o640)).unwrap();
        let video = public.join("clip.webm");
        std::fs::write(&video, b"x").unwrap();
        std::os::unix::fs::symlink(&video, public.join("disguised.png")).unwrap();
        std::os::unix::fs::symlink(public.join("loop2.png"), public.join("loop1.png")).unwrap();
        std::os::unix::fs::symlink(public.join("loop1.png"), public.join("loop2.png")).unwrap();
        std::fs::create_dir_all(public.join("folder.png")).unwrap();
        let check = |picture: String| {
            let mut appearance = Value::Object(current().to_map());
            appearance["picture"] = picture.into();
            validate_request(&json!({"expected": Value::Object(current().to_map()), "appearance": appearance}), &current(), &sessions())
        };
        let text = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(check(text(&picture)).unwrap_err(), "Wallpaper folders must be accessible to the login screen");
        assert_eq!(check("/etc/passwd".into()).unwrap_err(), "Choose a wallpaper image; login backgrounds do not support video");
        assert_eq!(check(text(&unreadable)).unwrap_err(), "Choose a system-readable wallpaper image");
        assert_eq!(check(text(&public.join("disguised.png"))).unwrap_err(),
                   "Choose a wallpaper image; login backgrounds do not support video");
        assert_eq!(check(text(&public.join("folder.png"))).unwrap_err(), "Choose a system-readable wallpaper image");
        assert_eq!(check("/nonexistent-waylogin/x.png".into()).unwrap_err(), "[Errno 2] No such file or directory: '/nonexistent-waylogin'");
        assert_eq!(check("/etc/passwd/x.png".into()).unwrap_err(), "[Errno 20] Not a directory: '/etc/passwd'");
        assert_eq!(check(text(&public.join("loop1.png"))).unwrap_err(),
                   format!("[Errno 40] Too many levels of symbolic links: '{}'", text(&public.join("loop1.png"))));
        assert_eq!(check("relative.png".into()).unwrap_err(), "Wallpaper must be an absolute, system-readable path");
        assert_eq!(check(String::new()).unwrap().picture, "");
    }

    #[test]
    fn python_path_rules() {
        for (name, expected) in [("a.", "."), ("a.PNG", ".PNG"), (".png", ""), ("..png", ""), ("a..png", ".png"), ("a.tar.gz", ".gz"),
                                 ("a", ""), (".a.png", ".png")] {
            assert_eq!(suffix(format!("/x/{name}").as_bytes()), expected, "{name}");
        }
        assert_eq!(parents(b"/usr/share/a.png"), [b"/usr/share".to_vec(), b"/usr".to_vec(), b"/".to_vec()]);
        assert_eq!(realpath_strict(b"/usr/./bin/../bin//").unwrap(), b"/usr/bin");
    }

    #[test]
    fn refuses_without_root_or_with_arguments() {
        let fixture = Fixture::new("refuse");
        let body = serde_json::to_vec(&request(Value::Object(Appearance::default().to_map()))).unwrap();
        for (euid, argc) in [(1000, 1), (0, 2), (1000, 2), (0, 0)] {
            assert_eq!(run(&fixture.config, &fixture.roots, euid, argc, &mut &body[..]).unwrap_err(),
                       "Administrator authorization is required");
        }
        assert!(!fixture.config.parent().unwrap().exists());
    }

    fn umask() -> u32 {
        // SAFETY: read and restore the process umask.
        unsafe {
            let mask = libc::umask(0o022);
            libc::umask(mask);
            mask
        }
    }

    #[test]
    fn writes_atomically_with_python_bytes_and_mode() {
        let fixture = Fixture::new("write");
        let mut appearance = Appearance::default().to_map();
        appearance.insert("picture".into(), "".into());
        appearance.insert("idle_seconds".into(), 45.into());
        fixture.run(&serde_json::to_vec(&request(Value::Object(appearance))).unwrap()).unwrap();
        let written = std::fs::read_to_string(&fixture.config).unwrap();
        assert_eq!(written, PY_DEFAULT.replace("/usr/share/wallpapers/nobara-winds1.webp", "").replace("30", "45"));
        assert_eq!(std::fs::metadata(&fixture.config).unwrap().mode() & 0o777, 0o644);
        let directory = fixture.config.parent().unwrap();
        assert_eq!(std::fs::metadata(directory).unwrap().mode() & 0o777, 0o755 & !umask());
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 1, "no temporary left behind");
        // A second edit must name the new state as `expected`.
        let stale = serde_json::to_vec(&request(Value::Object(Appearance::default().to_map()))).unwrap();
        assert_eq!(fixture.run(&stale).unwrap_err(), "Login settings changed elsewhere. Reload the page and try again.");
    }

    #[test]
    fn size_limit_symlink_and_json_errors() {
        let fixture = Fixture::new("limits");
        assert_eq!(fixture.run(&vec![b' '; 32769]).unwrap_err(), "Appearance request is too large");
        // Exactly 32768 bytes is read, parsed and written.
        let mut appearance = Appearance::default().to_map();
        appearance.insert("picture".into(), "".into());
        let mut body = serde_json::to_vec(&request(Value::Object(appearance))).unwrap();
        body.resize(32768, b' ');
        fixture.run(&body).unwrap();
        assert!(fixture.run(b"{").is_err());
        assert!(fixture.run(b"\xff").is_err());
        std::fs::remove_file(&fixture.config).unwrap();
        let target = fixture.root.join("elsewhere.json");
        std::os::unix::fs::symlink(&target, &fixture.config).unwrap();
        assert_eq!(fixture.run(&body).unwrap_err(), "Refusing to replace a symbolic link");
        assert!(!target.exists());
        // A corrupt current file is reported and never overwritten.
        std::fs::remove_file(&fixture.config).unwrap();
        std::fs::write(&fixture.config, "{\"blur_strength\": 500}").unwrap();
        assert_eq!(fixture.run(&body).unwrap_err(), "Invalid blur_strength");
        assert_eq!(std::fs::read_to_string(&fixture.config).unwrap(), "{\"blur_strength\": 500}");
        // A directory squatting on the destination (Python: read_text's IsADirectoryError).
        std::fs::remove_file(&fixture.config).unwrap();
        std::fs::create_dir_all(&fixture.config).unwrap();
        assert_eq!(fixture.run(&body).unwrap_err(), format!("[Errno 21] Is a directory: '{}'", fixture.config.display()));
    }

    #[test]
    fn failed_replace_removes_the_temporary() {
        let fixture = Fixture::new("replace");
        std::fs::create_dir_all(fixture.config.join("busy")).unwrap();
        let error = write_config(&fixture.config, &Appearance::default()).unwrap_err();
        assert!(error.starts_with("[Errno 21] Is a directory: '") || error.starts_with("[Errno 39] Directory not empty: '"), "{error}");
        assert!(error.ends_with(&format!(" -> '{}'", fixture.config.display())), "{error}");
        assert_eq!(std::fs::read_dir(fixture.config.parent().unwrap()).unwrap().count(), 1);
    }
}
