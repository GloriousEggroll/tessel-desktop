//! Optional file annotations, Git overlays, and explicit user-defined actions
//! (file_extras.py).
//!
//! These helpers do blocking filesystem/subprocess work and belong in the
//! caller's worker thread. They have no GTK dependency and create no
//! configuration until a user explicitly saves metadata.

use crate::error::{Error, ErrorKind, Result};
use crate::{fsx, pyos, Target};
use gio::prelude::*;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tessel_core::pyfmt::{json_dumps, JsonStyle};
use tessel_core::pyjson::{casefold, py_strip};
use tessel_core::tr;

pub const METADATA_ATTRIBUTE: &str = "user.tessel.metadata";
/// The name used before the Tessel rename: still read when a file has no
/// [`METADATA_ATTRIBUTE`], and replaced by it on the next edit.
pub const LEGACY_METADATA_ATTRIBUTE: &str = "user.desktop-suite.metadata";
/// Values larger than this stored in the xattr are ignored on read.
pub const MAX_METADATA_BYTES: usize = 65536;
/// Default `status_for_directory` timeout (Python `timeout=4`).
pub const GIT_TIMEOUT: Duration = Duration::from_secs(4);

const UNSUPPORTED: [i32; 5] = [libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS, libc::E2BIG, libc::ENOSPC];
const ABSENT: [i32; 1] = [libc::ENODATA];

/// Minimal `urllib.parse.urlsplit` for `scheme://netloc/path?query#fragment`.
struct SplitUri<'a> {
    scheme: String,
    netloc: &'a str,
    path: &'a str,
    query: &'a str,
    fragment: &'a str,
}

fn urlsplit(value: &str) -> SplitUri<'_> {
    let (scheme, rest) = value.split_once(':').unwrap_or(("", value));
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let split = rest.find('/').unwrap_or(rest.len());
    SplitUri { scheme: scheme.to_ascii_lowercase(), netloc: &rest[..split], path: &rest[split..], query, fragment }
}

fn not_local() -> Error {
    Error::value(tr!("File annotations are available for local files"))
}

/// `_local_path`: an absolute local path for a file, `file://` URI or path.
pub fn local_path(value: &Target) -> Result<PathBuf> {
    let raw: PathBuf = match value {
        Target::File(file) => file.path().ok_or_else(not_local)?,
        Target::Text(text) if pyos::has_uri_scheme_slashes(text) => {
            let parts = urlsplit(text);
            if parts.scheme != "file" || !(parts.netloc.is_empty() || parts.netloc == "localhost") {
                return Err(not_local());
            }
            if !parts.query.is_empty() || !parts.fragment.is_empty() {
                return Err(Error::value(tr!("A file URI cannot contain a query or fragment")));
            }
            PathBuf::from(pyos::unquote_strict(parts.path)
                .ok_or_else(|| Error::value("'utf-8' codec can't decode the file URI"))?)
        }
        Target::Text(text) => PathBuf::from(text),
        Target::Path(path) => path.clone(),
    };
    let path = pyos::abspath(&pyos::expanduser(&raw));
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(Error::value(tr!("Invalid file path")));
    }
    Ok(path)
}

/// Tags (≤ 64 unique, ≤ 128 characters each), a 0–5 rating and a comment (≤ 8192).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub tags: Vec<String>,
    pub rating: i64,
    pub comment: String,
}

/// `EMPTY_METADATA`.
pub const EMPTY_METADATA: Metadata = Metadata { tags: Vec::new(), rating: 0, comment: String::new() };

impl Default for Metadata {
    fn default() -> Self {
        EMPTY_METADATA
    }
}

impl Metadata {
    /// `{"tags": […], "rating": n, "comment": "…"}`.
    pub fn to_value(&self) -> Value {
        json!({"tags": self.tags, "rating": self.rating, "comment": self.comment})
    }
}

/// `_normal_metadata`: bound untrusted values.
pub fn normal_metadata(value: &Value) -> Metadata {
    let empty = Map::new();
    let value = value.as_object().unwrap_or(&empty);
    let mut tags = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(Value::Array(source)) = value.get("tags") {
        for tag in source.iter().take(64) {
            if let Value::String(tag) = tag {
                let cleaned = tag.replace('\0', "").replace(['\n', '\r'], " ");
                let tag = pyos::prefix_chars(py_strip(&cleaned), 128).to_string();
                if !tag.is_empty() && seen.insert(casefold(&tag)) {
                    tags.push(tag);
                }
            }
        }
    }
    let rating = match value.get("rating") {
        None => 0,
        Some(Value::Number(n)) => match (n.as_i64(), n.as_u64()) {
            (Some(v), _) => v.clamp(0, 5),
            (None, Some(_)) => 5,
            _ => 0,
        },
        _ => 0,
    };
    let comment = match value.get("comment") {
        Some(Value::String(text)) => pyos::prefix_chars(&text.replace('\0', ""), 8192).to_string(),
        _ => String::new(),
    };
    Metadata { tags, rating, comment }
}

/// `_open_item`: open without following links; regular files and folders only.
fn open_item(path: &Path) -> Result<(OwnedFd, libc::stat)> {
    let name = fsx::cstring(path)?;
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK) };
    if fd < 0 {
        return Err(Error::last_os(Some(path)));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut info) } != 0 {
        return Err(Error::last_os(Some(path)));
    }
    let kind = info.st_mode & libc::S_IFMT;
    if kind != libc::S_IFREG && kind != libc::S_IFDIR {
        return Err(Error::value(tr!("File annotations require a regular file or folder")));
    }
    Ok((fd, info))
}

fn atomic_json(path: &Path, data: &Map<String, Value>) -> Result<()> {
    let style = JsonStyle { ensure_ascii: false, sort_keys: false, indent: Some(2), item_separator: ",",
                            key_separator: ": " };
    let text = json_dumps(&Value::Object(data.clone()), &style) + "\n";
    fsx::atomic_write(path, text.as_bytes(), false)
}

/// Tags, a 0–5 rating, and a comment saved in one atomic user xattr
/// (`user.tessel.metadata`, compact JSON). Filesystems without writable
/// user xattrs use a private atomic JSON fallback bound to `[st_dev, st_ino]`.
#[derive(Clone, Debug)]
pub struct MetadataStore {
    pub path: PathBuf,
    #[cfg(test)]
    pub(crate) fail_get: Option<i32>,
    #[cfg(test)]
    pub(crate) fail_set: Option<i32>,
}

impl MetadataStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        MetadataStore {
            path: path.unwrap_or_else(|| tessel_core::paths::suite_data_dir().join("file-metadata.json")),
            #[cfg(test)]
            fail_get: None,
            #[cfg(test)]
            fail_set: None,
        }
    }

    fn fallback(&self) -> Map<String, Value> {
        let Ok(meta) = std::fs::metadata(&self.path) else { return Map::new() };
        if meta.len() > 16 * 1024 * 1024 {
            return Map::new();
        }
        let Ok(bytes) = std::fs::read(&self.path) else { return Map::new() };
        let Ok(text) = String::from_utf8(bytes) else { return Map::new() };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    fn identity(info: &libc::stat) -> Value {
        json!([info.st_dev as u64, info.st_ino as u64])
    }

    fn get_xattr(&self, fd: &OwnedFd) -> std::result::Result<Vec<u8>, i32> {
        #[cfg(test)]
        if let Some(errno) = self.fail_get {
            return Err(errno);
        }
        match Self::read_xattr(fd, METADATA_ATTRIBUTE) {
            Err(libc::ENODATA) => Self::read_xattr(fd, LEGACY_METADATA_ATTRIBUTE),
            result => result,
        }
    }

    fn read_xattr(fd: &OwnedFd, attribute: &str) -> std::result::Result<Vec<u8>, i32> {
        let name = std::ffi::CString::new(attribute).unwrap();
        loop {
            let size = unsafe { libc::fgetxattr(fd.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
            if size < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
            }
            let mut buffer = vec![0u8; size as usize];
            let read = unsafe { libc::fgetxattr(fd.as_raw_fd(), name.as_ptr(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if read < 0 {
                let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
                if errno == libc::ERANGE {
                    continue;
                }
                return Err(errno);
            }
            buffer.truncate(read as usize);
            return Ok(buffer);
        }
    }

    fn set_xattr(&self, fd: &OwnedFd, payload: &[u8]) -> std::result::Result<(), i32> {
        #[cfg(test)]
        if let Some(errno) = self.fail_set {
            return Err(errno);
        }
        let name = std::ffi::CString::new(METADATA_ATTRIBUTE).unwrap();
        let status = unsafe { libc::fsetxattr(fd.as_raw_fd(), name.as_ptr(), payload.as_ptr().cast(), payload.len(), 0) };
        if status != 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        Ok(())
    }

    fn read_open(&self, path: &Path, fd: &OwnedFd, info: &libc::stat) -> Result<Metadata> {
        // A size-limited xattr filesystem can retain an earlier shorter value
        // when a later edit had to use the fallback. That explicit edit wins.
        let fallback = self.fallback();
        if let Some(Value::Object(record)) = fallback.get(&pyos::as_uri(path)) {
            if record.get("identity") == Some(&Self::identity(info)) {
                return Ok(normal_metadata(record.get("metadata").unwrap_or(&Value::Null)));
            }
        }
        match self.get_xattr(fd) {
            Ok(raw) => {
                if raw.len() <= MAX_METADATA_BYTES {
                    if let Some(value) = String::from_utf8(raw).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                        return Ok(normal_metadata(&value));
                    }
                }
            }
            Err(errno) if UNSUPPORTED.contains(&errno) || ABSENT.contains(&errno) => {}
            Err(errno) => return Err(Error::from_errno(errno, None)),
        }
        Ok(EMPTY_METADATA)
    }

    /// Tolerates unsupported/remote locations: returns empty fields.
    pub fn read(&self, file: impl Into<Target>) -> Metadata {
        let target = file.into();
        (|| {
            let path = local_path(&target)?;
            let (fd, info) = open_item(&path)?;
            self.read_open(&path, &fd, &info)
        })().unwrap_or(EMPTY_METADATA)
    }

    /// Change only the supplied fields (`tags`, `rating`, `comment`); never the file contents.
    /// Rejects symlinks, remote locations, unknown fields (`TypeError`) and permission errors.
    pub fn write(&self, file: impl Into<Target>, fields: &Map<String, Value>) -> Result<Metadata> {
        let mut unknown: Vec<&String> = fields.keys().filter(|k| !["tags", "rating", "comment"].contains(&k.as_str())).collect();
        if !unknown.is_empty() {
            unknown.sort();
            let names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
            return Err(Error::type_error(format!("Unknown annotation fields: {}", names.join(", "))));
        }
        let path = local_path(&file.into())?;
        let (fd, info) = open_item(&path)?;
        let current = self.read_open(&path, &fd, &info)?;
        if fields.is_empty() {
            return Ok(current);
        }
        let mut merged = match current.to_value() {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        for (key, value) in fields {
            merged.insert(key.clone(), value.clone());
        }
        let updated = normal_metadata(&Value::Object(merged));
        let style = JsonStyle { ensure_ascii: false, ..JsonStyle::COMPACT };
        let payload = json_dumps(&updated.to_value(), &style);
        let uri = pyos::as_uri(&path);
        match self.set_xattr(&fd, payload.as_bytes()) {
            Err(errno) if !UNSUPPORTED.contains(&errno) => return Err(Error::from_errno(errno, None)),
            Err(_) => {
                fsx::locked(&self.path, || {
                    let mut data = self.fallback();
                    data.insert(uri.clone(), json!({"identity": Self::identity(&info), "metadata": updated.to_value()}));
                    atomic_json(&self.path, &data)
                })?;
            }
            Ok(()) => {
                // A formerly unsupported filesystem may now support xattrs.
                // Clear only our fallback entry after the real write succeeded.
                if self.path.exists() {
                    fsx::locked(&self.path, || {
                        let mut data = self.fallback();
                        if data.shift_remove(&uri).is_some() {
                            atomic_json(&self.path, &data)?;
                        }
                        Ok(())
                    })?;
                }
            }
        }
        Ok(updated)
    }
}

fn git_command(directory: &Path, arguments: &[&str], timeout: Duration) -> Result<(i32, Vec<u8>)> {
    let mut command = Command::new("git");
    command.args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false",
                  "-c", "core.hooksPath=/dev/null", "-C"])
        .arg(directory).args(arguments)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|e| Error::from_io(&e, Some(Path::new("git"))))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(Error::new(ErrorKind::Timeout, "git timed out"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(error.into()),
        }
    };
    let output = reader.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), output))
}

fn git_label(code: &str) -> String {
    if ["DD", "AU", "UD", "UA", "DU", "AA", "UU"].contains(&code) {
        return "Conflict".into();
    }
    if code == "??" {
        return "Untracked".into();
    }
    if code == "!!" {
        return "Ignored".into();
    }
    let label = "RDAMTC".chars().find(|c| code.contains(*c)).map_or("Changed", |c| match c {
        'R' => "Renamed",
        'C' => "Copied",
        'D' => "Deleted",
        'A' => "Added",
        'M' => "Modified",
        _ => "Type changed",
    });
    let chars: Vec<char> = code.chars().collect();
    if chars.len() == 2 && chars[0] != ' ' && chars[1] == ' ' { format!("{label} (staged)") } else { label.into() }
}

/// Read Git status without hooks, index refresh writes, or a shell.
///
/// Returns absolute-path keys for changed files and containing subdirectories
/// (labels are untranslated English, as in Python). Non-repositories, absent
/// Git, untrusted ownership, and timeouts return an empty map.
pub fn status_for_directory(path: impl Into<Target>, timeout: Duration) -> HashMap<PathBuf, String> {
    let target = path.into();
    (|| -> Result<HashMap<PathBuf, String>> {
        let directory = pyos::realpath(&local_path(&target)?);
        if !directory.is_dir() {
            return Ok(HashMap::new());
        }
        let (code, root_bytes) = git_command(&directory, &["rev-parse", "--show-toplevel"], timeout)?;
        if code != 0 {
            return Ok(HashMap::new());
        }
        let root_bytes = root_bytes.strip_suffix(b"\n").unwrap_or(&root_bytes);
        let root = PathBuf::from(OsStr::from_bytes(root_bytes));
        let (code, status) = git_command(&directory, &["status", "--porcelain=v1", "-z", "--untracked-files=normal",
                                                      "--ignored=no", "--", "."], timeout)?;
        if code != 0 {
            return Ok(HashMap::new());
        }
        let mut result = HashMap::new();
        let mut parts = status.split(|&b| b == 0);
        while let Some(part) = parts.next() {
            if part.len() < 4 {
                continue;
            }
            let code: String = part[..2].iter().map(|&b| if b.is_ascii() { b as char } else { '\u{fffd}' }).collect();
            let relative = &part[3..];
            if code.contains('R') || code.contains('C') {
                parts.next(); // The second path is the original rename/copy.
            }
            let joined = if relative.starts_with(b"/") {
                relative.to_vec()
            } else {
                let mut bytes = root.as_os_str().as_bytes().to_vec();
                if !bytes.ends_with(b"/") {
                    bytes.push(b'/');
                }
                bytes.extend_from_slice(relative);
                bytes
            };
            let full = PathBuf::from(OsString::from_vec(pyos::normpath(&joined)));
            if pyos::commonpath(&directory, &full) != directory {
                continue;
            }
            result.insert(full.clone(), git_label(&code));
            let mut parent = pyos::dirname(&full);
            while parent != directory && pyos::commonpath(&directory, &parent) == directory {
                result.entry(parent.clone()).or_insert_with(|| "Contains changes".to_string());
                let next = pyos::dirname(&parent);
                if next == parent {
                    break;
                }
                parent = next;
            }
        }
        Ok(result)
    })().unwrap_or_default()
}

/// One validated entry of `file-manager-actions.json`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CustomAction {
    pub name: String,
    pub argv: Vec<String>,
    pub directories: bool,
    pub files: bool,
}

/// Explicit argv actions from the user's own optional JSON configuration.
///
/// File format: `[{"name":"Action", "argv":["program","%F"], "directories":true, "files":true}]`.
/// There is no shell parsing. `%f`/`%u`/`%%` may occur within an argument;
/// `%F`/`%U` must occupy a whole argument. Unsupported codes are rejected.
#[derive(Clone, Debug)]
pub struct CustomActions {
    pub path: PathBuf,
}

impl CustomActions {
    pub fn new(path: Option<PathBuf>) -> Self {
        CustomActions { path: path.unwrap_or_else(|| tessel_core::paths::suite_config_dir().join("file-manager-actions.json")) }
    }

    pub fn list(&self) -> Vec<CustomAction> {
        let Ok(meta) = std::fs::metadata(&self.path) else { return Vec::new() };
        if meta.len() > 1024 * 1024 {
            return Vec::new();
        }
        let Some(Value::Array(records)) = std::fs::read(&self.path).ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return Vec::new() };
        let mut actions = Vec::new();
        for record in records.iter().take(128) {
            let Value::Object(record) = record else { continue };
            let (Some(Value::String(name)), Some(Value::Array(argv))) = (record.get("name"), record.get("argv")) else {
                continue;
            };
            if py_strip(name).is_empty() || name.chars().count() > 128 || argv.is_empty() || argv.len() > 128 {
                continue;
            }
            let argv: Option<Vec<String>> = argv.iter().map(|arg| match arg {
                Value::String(text) if !text.contains('\0') && text.chars().count() <= 8192 => Some(text.clone()),
                _ => None,
            }).collect();
            let Some(argv) = argv else { continue };
            if argv[0].is_empty() || argv[0].contains('%') {
                continue;
            }
            actions.push(CustomAction {
                name: py_strip(name).to_string(),
                argv,
                directories: record.get("directories").is_none_or(|v| v == &Value::Bool(true)),
                files: record.get("files").is_none_or(|v| v == &Value::Bool(true)),
            });
        }
        actions
    }

    /// Expand `action.argv` for `files` without any shell.
    pub fn expand(action: &CustomAction, files: &[Target]) -> Result<Vec<OsString>> {
        if files.is_empty() {
            return Err(Error::value(tr!("Select at least one file")));
        }
        let mut uris: Vec<String> = Vec::new();
        let mut paths: Vec<Option<PathBuf>> = Vec::new();
        for file in files {
            match file {
                Target::File(file) => {
                    uris.push(file.uri().to_string());
                    paths.push(file.path());
                }
                Target::Text(text) if pyos::has_uri_scheme_slashes(text) => {
                    uris.push(text.clone());
                    match local_path(file) {
                        Ok(path) => paths.push(Some(path)),
                        Err(error) if error.is_value_error() => paths.push(None),
                        Err(error) => return Err(error),
                    }
                }
                _ => {
                    let path = local_path(file)?;
                    uris.push(pyos::as_uri(&path));
                    paths.push(Some(path));
                }
            }
        }
        let requires_local = || Error::value(tr!("This action requires local files"));
        let mut result = Vec::new();
        for argument in &action.argv {
            if argument == "%F" {
                for path in &paths {
                    result.push(path.clone().ok_or_else(requires_local)?.into_os_string());
                }
            } else if argument == "%U" {
                result.extend(uris.iter().map(OsString::from));
            } else {
                let mut out: Vec<u8> = Vec::new();
                let mut chars = argument.chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch == '%' {
                        if let Some(&code) = chars.peek() {
                            if code != '\n' {
                                chars.next();
                                match code {
                                    '%' => out.push(b'%'),
                                    'u' => out.extend_from_slice(uris[0].as_bytes()),
                                    'f' => out.extend_from_slice(paths[0].as_ref().ok_or_else(requires_local)?
                                        .as_os_str().as_bytes()),
                                    other => return Err(Error::value(
                                        tr!("Unsupported action placeholder %{code}", code = other.to_string()))),
                                }
                                continue;
                            }
                        }
                    }
                    let mut buffer = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                }
                result.push(OsString::from_vec(out));
            }
        }
        Ok(result)
    }

    /// Start the expanded argv detached (new session, null stdio) in `directory`.
    pub fn invoke(action: &CustomAction, files: &[Target], directory: Option<&Target>) -> Result<Child> {
        let argv = Self::expand(action, files)?;
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(directory) = directory {
            command.current_dir(local_path(directory)?);
        }
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().map_err(|e| Error::from_io(&e, Some(Path::new(&argv[0]))))
    }
}

#[cfg(test)]
mod tests;
