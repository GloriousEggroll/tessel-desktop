//! `color_config.py`: shared display colour settings. Reading never writes or
//! reloads labwc; writing merges into one managed rc.xml block and preserves
//! everything else byte for byte.

use crate::compat::fsx;
use crate::labwc::{element_text, local_name};
use regex::Regex;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;
use tessel_core::pyerr::os_error_path;

pub const BEGIN: &str = "<!-- tessel-displays color management begin -->";
pub const END: &str = "<!-- tessel-displays color management end -->";
/// The markers written before the Tessel rename; read as the current ones.
pub const LEGACY_MARKERS: [(&str, &str); 2] = [
    ("<!-- way-displays-gui color management begin -->", BEGIN),
    ("<!-- way-displays-gui color management end -->", END),
];
pub const MAX_PROFILE: usize = 16 * 1024 * 1024;
static BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("(?s){}.*?{}", regex::escape(BEGIN), regex::escape(END))).unwrap());
static CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)</(?:labwc_config|openbox_config)\s*>").unwrap());
static SELF_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)/\s*>").unwrap());
/// The Python lookahead `(?=(?:\s|<!--.*?-->)*$)`, applied to the rest.
static ONLY_TRAILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\A(?:\s|<!--.*?-->)*\n?\z").unwrap());

/// One output's colour settings (`DEFAULT = {"profile": "", "calibration": True, "sdrBitDepth": 8}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorSettings {
    pub profile: String,
    pub calibration: bool,
    pub sdr_bit_depth: i64,
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self { profile: String::new(), calibration: true, sdr_bit_depth: 8 }
    }
}

/// Error kinds of the Python module: ValueError, OSError, RuntimeError.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorError {
    Value(String),
    Os(String),
    Runtime(String),
}

impl std::fmt::Display for ColorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColorError::Value(t) | ColorError::Os(t) | ColorError::Runtime(t) => f.write_str(t),
        }
    }
}

fn value_of(node: roxmltree::Node, name: &str, default: &str) -> String {
    let lower = name.to_lowercase();
    let mut value = node.attributes().find(|a| a.name().to_lowercase() == lower)
        .map(|a| a.value().to_owned()).unwrap_or_else(|| default.to_owned());
    for child in node.children().filter(|c| c.is_element()) {
        if local_name(child) == lower {
            value = element_text(child).unwrap_or_default();
        }
    }
    value
}

/// Python `int(text)`.
fn py_int(text: &str) -> Result<i64, String> {
    let stripped = text.trim();
    let digits = stripped.strip_prefix(['+', '-']).unwrap_or(stripped);
    let valid = !digits.is_empty() && !digits.starts_with('_') && !digits.ends_with('_') && !digits.contains("__")
        && digits.chars().all(|c| c.is_ascii_digit() || c == '_');
    if valid {
        if let Ok(value) = stripped.replace('_', "").parse::<i64>() {
            return Ok(value);
        }
    }
    Err(format!("invalid literal for int() with base 10: '{text}'"))
}

fn parse_document(text: &str) -> Result<roxmltree::Document<'_>, ColorError> {
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    roxmltree::Document::parse_with_options(text, options)
        .map_err(|error| ColorError::Value(format!("The labwc configuration is not valid XML: {error}")))
}

pub fn parse_colors(text: &str) -> Result<BTreeMap<String, ColorSettings>, ColorError> {
    let document = parse_document(text)?;
    let root = document.root_element();
    if !matches!(local_name(root).as_str(), "labwc_config" | "openbox_config") {
        return Err(ColorError::Value("The selected file is not a labwc configuration.".into()));
    }
    let mut result = BTreeMap::new();
    for section in root.children().filter(|c| c.is_element()) {
        if local_name(section) != "colormanagement" {
            continue;
        }
        for node in section.children().filter(|c| c.is_element()) {
            if local_name(node) != "output" || value_of(node, "name", "").is_empty() {
                continue;
            }
            let depth = py_int(&value_of(node, "sdrBitDepth", "8")).map_err(ColorError::Value)?;
            result.insert(value_of(node, "name", ""), ColorSettings {
                profile: value_of(node, "profile", ""),
                calibration: value_of(node, "calibration", "true") == "true",
                sdr_bit_depth: depth,
            });
        }
    }
    Ok(result)
}

pub fn read_colors(path: &Path) -> Result<BTreeMap<String, ColorSettings>, ColorError> {
    match fsx::read_text(path) {
        Ok(text) => parse_colors(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Err(ColorError::Value(fsx::read_error(&error, path))),
        Err(error) => Err(ColorError::Os(os_error_path(&error, path))),
    }
}

type CmsOpen = unsafe extern "C" fn(*const libc::c_void, u32) -> *mut libc::c_void;
type CmsClose = unsafe extern "C" fn(*mut libc::c_void) -> libc::c_int;
type CmsIntent = unsafe extern "C" fn(*mut libc::c_void, u32, u32) -> libc::c_int;

/// `ctypes.CDLL("liblcms2.so.2")` symbol lookup.
pub fn lcms_symbol(name: &str) -> Result<*mut libc::c_void, ColorError> {
    unsafe {
        let library = libc::dlopen(c"liblcms2.so.2".as_ptr(), libc::RTLD_NOW);
        if library.is_null() {
            return Err(ColorError::Os("liblcms2.so.2: cannot open shared object file: No such file or directory".into()));
        }
        let symbol = std::ffi::CString::new(name).unwrap();
        let address = libc::dlsym(library, symbol.as_ptr());
        if address.is_null() {
            return Err(ColorError::Os(format!("liblcms2.so.2: undefined symbol: {name}")));
        }
        Ok(address)
    }
}

/// Check the same RGB display profile constraints as the compositor.
pub fn validate_profile(path: &str) -> Result<(), ColorError> {
    if path.is_empty() {
        return Ok(());
    }
    let file_path = Path::new(path);
    if !file_path.is_absolute() {
        return Err(ColorError::Value("Choose an ICC profile using an absolute file path.".into()));
    }
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(file_path)
        .map_err(|e| ColorError::Os(os_error_path(&e, file_path)))?;
    let metadata = file.metadata().map_err(|e| ColorError::Os(os_error_path(&e, file_path)))?;
    if !metadata.is_file() {
        return Err(ColorError::Value("Choose a regular ICC profile file.".into()));
    }
    let mut data = Vec::new();
    (&mut file).take(MAX_PROFILE as u64 + 1).read_to_end(&mut data).map_err(|e| ColorError::Os(os_error_path(&e, file_path)))?;
    if !(128..=MAX_PROFILE).contains(&data.len()) || &data[36..40] != b"acsp" {
        return Err(ColorError::Value("This is not a valid ICC profile (maximum size: 16 MiB).".into()));
    }
    if &data[12..16] != b"mntr" || &data[16..20] != b"RGB " || !matches!(data[8], 2 | 4) {
        return Err(ColorError::Value("Choose an ICC v2 or v4 RGB display profile.".into()));
    }
    unsafe {
        let open: CmsOpen = std::mem::transmute(lcms_symbol("cmsOpenProfileFromMem")?);
        let close: CmsClose = std::mem::transmute(lcms_symbol("cmsCloseProfile")?);
        let intent: CmsIntent = std::mem::transmute(lcms_symbol("cmsIsIntentSupported")?);
        let profile = open(data.as_ptr().cast(), data.len() as u32);
        if profile.is_null() {
            return Err(ColorError::Value("Little CMS could not read this ICC profile.".into()));
        }
        let supported = intent(profile, 1, 1) != 0;
        close(profile);
        if !supported {
            return Err(ColorError::Value("This ICC profile cannot be used for display conversion.".into()));
        }
    }
    Ok(())
}

/// `$XDG_RUNTIME_DIR/labwc-color-$WAYLAND_DISPLAY.ini`.
pub fn status_path() -> Option<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok().filter(|v| !v.is_empty())?;
    let display = std::env::var("WAYLAND_DISPLAY").ok().filter(|v| !v.is_empty())?;
    if display.contains('/') {
        return None;
    }
    Some(PathBuf::from(runtime).join(format!("labwc-color-{display}.ini")))
}

pub type Status = BTreeMap<String, BTreeMap<String, String>>;

/// `configparser.ConfigParser(interpolation=None).read_string` (strict).
pub fn parse_ini(text: &str) -> Result<Status, String> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut defaults: Vec<(String, String)> = Vec::new();
    let mut current: Option<usize> = None; // usize::MAX = DEFAULT
    let mut last_key: Option<String> = None;
    for line in fsx::splitlines(text) {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') || stripped.starts_with(';') {
            last_key = None;
            continue;
        }
        let indented = line.starts_with([' ', '\t']);
        if indented && last_key.is_some() && current.is_some() {
            let key = last_key.clone().unwrap();
            let entries = if current == Some(usize::MAX) { &mut defaults } else { &mut sections[current.unwrap()].1 };
            if let Some(entry) = entries.iter_mut().find(|(k, _)| *k == key) {
                entry.1.push('\n');
                entry.1.push_str(stripped);
            }
            continue;
        }
        if stripped.starts_with('[') && stripped.ends_with(']') && !indented {
            let name = stripped[1..stripped.len() - 1].to_owned();
            if name == "DEFAULT" {
                current = Some(usize::MAX);
            } else {
                if sections.iter().any(|(n, _)| *n == name) {
                    return Err(format!("section {name:?} already exists"));
                }
                sections.push((name, Vec::new()));
                current = Some(sections.len() - 1);
            }
            last_key = None;
            continue;
        }
        let Some(index) = current else { return Err("File contains no section headers.".into()) };
        let split = stripped.find(['=', ':']);
        let (key, value) = match split {
            Some(at) => (stripped[..at].trim().to_lowercase(), stripped[at + 1..].trim().to_owned()),
            None => return Err(format!("Source contains parsing errors: {line:?}")),
        };
        let entries = if index == usize::MAX { &mut defaults } else { &mut sections[index].1 };
        if entries.iter().any(|(k, _)| *k == key) {
            return Err(format!("option {key:?} already exists"));
        }
        entries.push((key.clone(), value));
        last_key = Some(key);
    }
    Ok(sections.into_iter().map(|(name, entries)| {
        let mut map: BTreeMap<String, String> = defaults.iter().cloned().collect();
        map.extend(entries);
        (name, map)
    }).collect())
}

thread_local! {
    static STATUS_OVERRIDE: RefCell<Option<Rc<dyn Fn() -> Status>>> = const { RefCell::new(None) };
}

/// Test hook (Python tests patch `color_config.read_status`).
pub fn set_status_override(provider: Option<Rc<dyn Fn() -> Status>>) {
    STATUS_OVERRIDE.with(|o| o.replace(provider));
}

/// The running compositor's colour status; `{}` unless current and trusted.
pub fn read_status() -> Status {
    if let Some(provider) = STATUS_OVERRIDE.with(|o| o.borrow().clone()) {
        return provider();
    }
    read_status_file(status_path().as_deref()).unwrap_or_default()
}

pub fn read_status_file(path: Option<&Path>) -> Option<Status> {
    use std::os::unix::fs::MetadataExt;
    let path = path?;
    if std::fs::metadata(path).ok()?.uid() != unsafe { libc::getuid() } {
        return None;
    }
    let status = parse_ini(&fsx::read_text(path).ok()?).ok()?;
    let compositor = status.get("compositor")?;
    let pid = py_int(compositor.get("pid")?).ok()?;
    if pid < 2 || py_int(compositor.get("version")?).ok()? != 1 {
        return None;
    }
    if pid > i32::MAX as i64 || unsafe { libc::kill(pid as i32, 0) } != 0 {
        return None;
    }
    if let Ok(labwc) = std::env::var("LABWC_PID") {
        if !labwc.is_empty() && py_int(&labwc).ok()? != pid {
            return None;
        }
    }
    let command = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if command.split(|b| *b == 0).any(|a| a == b"--merge-config" || a == b"-m") {
        return None;
    }
    static ESCAPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\([snrt\\])").unwrap());
    Some(status.into_iter().map(|(section, values)| {
        let values = values.into_iter().map(|(key, value)| {
            let value = ESCAPE.replace_all(&value, |c: &regex::Captures| match &c[1] {
                "s" => " ", "n" => "\n", "r" => "\r", "t" => "\t", _ => "\\",
            }.to_owned()).into_owned();
            (key, value)
        }).collect();
        (section, values)
    }).collect())
}

/// `atomic_write(path, data, mode=0o600)` (prefix `.<name>.`).
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<(), ColorError> {
    let prefix = format!(".{}.", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    fsx::atomic_write_with(path, data, mode, &prefix, &fsx::os_replace).map_err(ColorError::Os)
}

struct Lock(std::fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn locked(path: &Path) -> Result<Lock, ColorError> {
    use std::os::fd::AsRawFd;
    let parent = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(parent).map_err(|e| ColorError::Os(os_error_path(&e, parent)))?;
    let name = format!(".{}.color.lock", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let lock_path = parent.join(name);
    let file = std::fs::OpenOptions::new().append(true).create(true).open(&lock_path)
        .map_err(|e| ColorError::Os(os_error_path(&e, &lock_path)))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(ColorError::Os(tessel_core::pyerr::os_error_text(&std::io::Error::last_os_error())));
    }
    Ok(Lock(file))
}

/// `Path.resolve()` (non-strict).
fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => resolve(parent).join(name),
        _ => absolute,
    }
}

/// ElementTree `_escape_attrib`.
fn escape_attribute(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#09;"),
            c => out.push(c),
        }
    }
    out
}

/// What `rollback` needs (`{"path", "before", "after", "mode", "existed", "backup"}`).
#[derive(Clone, Debug)]
pub struct Transaction {
    pub path: PathBuf,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    pub mode: u32,
    pub existed: bool,
    pub backup: PathBuf,
}

/// Replace managed-block markers written before the Tessel rename with the current ones.
pub fn upgrade_markers(mut text: String, legacy: &[(&str, &str)]) -> String {
    for (old, new) in legacy {
        if text.contains(old) {
            text = text.replace(old, new);
        }
    }
    text
}

fn trailing_only(text: &str, from: usize) -> bool {
    ONLY_TRAILING.is_match(&text[from..])
}

/// Merge changed outputs into our block; preserve unrelated XML byte for byte.
pub fn write_colors(path: &Path, changes: &BTreeMap<String, ColorSettings>) -> Result<Transaction, ColorError> {
    for (name, value) in changes {
        if name.is_empty() || !matches!(value.sdr_bit_depth, 8 | 10) {
            return Err(ColorError::Value("Invalid display color settings.".into()));
        }
        validate_profile(&value.profile)?;
    }
    let path = resolve(path);
    let _lock = locked(&path)?;
    let existed = path.exists();
    let before = if existed {
        std::fs::read(&path).map_err(|e| ColorError::Os(os_error_path(&e, &path)))?
    } else {
        b"<labwc_config>\n</labwc_config>\n".to_vec()
    };
    let mode = if existed {
        std::fs::metadata(&path).map_err(|e| ColorError::Os(os_error_path(&e, &path)))?.permissions().mode() & 0o7777
    } else {
        0o600
    };
    let mut text = String::from_utf8(before.clone())
        .map_err(|e| ColorError::Value(format!("'utf-8' codec can't decode byte: {e}")))?;
    text = upgrade_markers(text, &LEGACY_MARKERS);
    parse_colors(&text)?; // Reject malformed XML before any write.
    let matches: Vec<_> = BLOCK.find_iter(&text).map(|m| m.as_str().to_owned()).collect();
    if matches.len() > 1 || text.matches(BEGIN).count() != matches.len() || text.matches(END).count() != matches.len() {
        return Err(ColorError::Value("The managed color settings block is malformed.".into()));
    }
    let mut managed = match matches.first() {
        Some(block) => parse_colors(&format!("<labwc_config>{block}</labwc_config>"))?,
        None => BTreeMap::new(),
    };
    managed.extend(changes.iter().map(|(k, v)| (k.clone(), v.clone())));
    let section = if managed.is_empty() {
        "<colorManagement />".to_owned()
    } else {
        let mut section = String::from("<colorManagement>");
        for (name, value) in &managed {
            section.push_str(&format!("\n  <output name=\"{}\" profile=\"{}\" calibration=\"{}\" sdrBitDepth=\"{}\" />",
                escape_attribute(name), escape_attribute(&value.profile),
                if value.calibration { "true" } else { "false" }, value.sdr_bit_depth));
        }
        section.push_str("\n</colorManagement>");
        section
    };
    let block = format!("{BEGIN}\n{section}\n{END}");
    // Append last so explicit GUI choices override any earlier manual section.
    text = BLOCK.replace_all(&text, "").into_owned();
    let find_close = |text: &str| CLOSE.find_iter(text).find(|m| trailing_only(text, m.end())).map(|m| m.start());
    let close = match find_close(&text) {
        Some(close) => close,
        None => {
            // Empty self-closing roots are common on first use.
            let (has_children, tag) = {
                let document = parse_document(&text)?;
                let root = document.root_element();
                (root.children().any(|c| c.is_element()), root.tag_name().name().to_owned())
            };
            let positions: Vec<(usize, usize)> = SELF_CLOSE.find_iter(&text)
                .filter(|m| trailing_only(&text, m.end())).map(|m| (m.start(), m.end())).collect();
            if has_children || positions.is_empty() {
                return Err(ColorError::Value("Could not locate the labwc configuration root.".into()));
            }
            let mut replaced = String::new();
            let mut offset = 0;
            for (start, end) in positions {
                replaced.push_str(&text[offset..start]);
                replaced.push_str(&format!(">\n</{tag}>\n"));
                offset = end;
            }
            replaced.push_str(&text[offset..]);
            text = replaced;
            find_close(&text).ok_or_else(|| ColorError::Value("Could not locate the labwc configuration root.".into()))?
        }
    };
    let after = format!("{}{block}\n{}", &text[..close], &text[close..]).into_bytes();
    parse_colors(std::str::from_utf8(&after).unwrap())?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let backup = tessel_core::paths::suite_state_dir().join("backups/color").join(format!("{nanos}-rc.xml"));
    atomic_write(&backup, &before, 0o600)?;
    atomic_write(&path, &after, mode)?;
    Ok(Transaction { path, before, after, mode, existed, backup })
}

pub fn rollback(transaction: &Transaction) -> Result<(), ColorError> {
    let _lock = locked(&transaction.path)?;
    let current = std::fs::read(&transaction.path).map_err(|e| ColorError::Os(os_error_path(&e, &transaction.path)))?;
    if current != transaction.after {
        return Err(ColorError::Runtime(format!("Configuration changed again; restore the backup manually: {}",
                                               transaction.backup.display())));
    }
    if transaction.existed {
        atomic_write(&transaction.path, &transaction.before, transaction.mode)
    } else {
        std::fs::remove_file(&transaction.path).map_err(|e| ColorError::Os(os_error_path(&e, &transaction.path)))
    }
}
