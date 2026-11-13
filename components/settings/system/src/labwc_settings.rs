//! labwc_settings.py: safe edits of the session's labwc rc.xml keyboard
//! section and environment file.
//!
//! Edits splice only the bytes they change, so comments, formatting and
//! unrelated sections survive. Each file is backed up once, before the suite
//! first changes it.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;

use gtk::gio;

use regex::Regex;
use tessel_tiling::xml::{Element, Node};

use crate::compat::{self, bytes_rstrip, bytes_strip, casefold, local_lower, os_error, strip, tag_end, value, Error, Result};

pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const TESSEL_TILING_BEGIN: &str = "Tessel Tiling keyboard begin";
pub const TESSEL_TILING_END: &str = "Tessel Tiling keyboard end";
/// tessel-configure-session's block; the begin comment goes on with more text.
pub const TESSEL_KEYBINDINGS_BEGIN: &str = "Tessel keybindings begin";
pub const TESSEL_KEYBINDINGS_END: &str = "Tessel keybindings end";
const DOCTYPE: &str = "XML document types are not supported.";

static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());
static SETTING_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]+$").unwrap());
static COMMENT: LazyLock<regex::bytes::Regex> = LazyLock::new(|| regex::bytes::Regex::new(r"(?s-u)<!--.*?-->").unwrap());
static EMPTY_NUMLOCK: LazyLock<regex::bytes::Regex> = LazyLock::new(|| regex::bytes::Regex::new(r"(?i-u)<numlock\s*/>").unwrap());
static KEY_ATTRIBUTE: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| regex::bytes::Regex::new(r#"(?-u)\bkey\s*=\s*("[^"]*"|'[^']*')"#).unwrap());

/// `os.environ.get(name) or default`.
fn env_or(name: &str, default: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(default)
}

pub fn config_home() -> PathBuf { env_or("XDG_CONFIG_HOME", || tessel_core::paths::home().join(".config")) }

pub fn state_home() -> PathBuf { env_or("XDG_STATE_HOME", || tessel_core::paths::home().join(".local/state")) }

/// The rc.xml the running labwc reads, or the per-user default.
pub fn rc_path() -> PathBuf {
    crate::workspace_config::session_config().map(|(path, _)| path).unwrap_or_else(|_| config_home().join("labwc/rc.xml"))
}

pub fn environment_path(rc: Option<&Path>) -> PathBuf {
    let rc = rc.map(Path::to_path_buf).unwrap_or_else(rc_path);
    rc.parent().map(Path::to_path_buf).unwrap_or_default().join("environment")
}

/// Keep the file as it was before the suite first edited it.
pub fn backup_once(path: &Path, kind: &str) -> Result<PathBuf> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let target = state_home().join("tessel/backups").join(kind).join(name + ".orig");
    if target.exists() || !path.is_file() {
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| os_error(&e, parent))?;
    }
    copy2(path, &target)?;
    Ok(target)
}

/// `shutil.copy2`: contents, permission bits and timestamps.
fn copy2(source: &Path, target: &Path) -> Result<()> {
    std::fs::copy(source, target).map_err(|e| os_error(&e, source))?;
    if let Ok(metadata) = std::fs::metadata(source) {
        let times = [libc::timespec { tv_sec: 0, tv_nsec: 0 }; 2];
        let mut times = times;
        use std::os::unix::fs::MetadataExt;
        times[0] = libc::timespec { tv_sec: metadata.atime() as libc::time_t, tv_nsec: metadata.atime_nsec() as _ };
        times[1] = libc::timespec { tv_sec: metadata.mtime() as libc::time_t, tv_nsec: metadata.mtime_nsec() as _ };
        if let Ok(c) = std::ffi::CString::new(target.as_os_str().as_encoded_bytes()) {
            // SAFETY: valid NUL-terminated path and a two-element timespec array.
            unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
        }
    }
    Ok(())
}

/// `atomic_write(path, data)`: resolve, keep the mode (0644 when new).
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let path = compat::resolve(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| os_error(&e, parent))?;
    }
    let mode = compat::file_mode(&path).unwrap_or(0o644);
    compat::atomic_write_mode(&path, data, mode)
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(os_error(&error, path)),
    };
    if data.len() > MAX_BYTES {
        return value("The configuration file is too large.");
    }
    Ok(data)
}

/// Apply `transform(bytes) -> bytes`; true when the file changed.
pub fn update(path: &Path, transform: impl FnOnce(&[u8]) -> Result<Vec<u8>>, kind: &str) -> Result<bool> {
    let original = read_bytes(path)?;
    let changed = transform(&original)?;
    if changed == original {
        return Ok(false);
    }
    // A concurrent writer (another settings page) must not be overwritten.
    if read_bytes(path)? != original {
        return compat::runtime("The configuration changed while it was being edited. Try again.");
    }
    backup_once(path, kind)?;
    atomic_write(path, &changed)?;
    Ok(true)
}

/// `done(okay, message)` of an asynchronous reload.
pub type Done = Box<dyn FnOnce(bool, String)>;

/// `reconfigure(done)` and its injectable replacements.
pub type Reconfigure = Rc<dyn Fn(Option<Done>)>;

/// Ask labwc to reload, without blocking the GTK main loop.
pub fn reconfigure(done: Option<Done>) {
    let flags = gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE;
    match gio::Subprocess::newv(&[std::ffi::OsStr::new("labwc"), std::ffi::OsStr::new("--reconfigure")], flags) {
        Err(error) => {
            if let Some(done) = done {
                done(false, error.message().to_owned());
            }
        }
        Ok(process) => {
            let child = process.clone();
            process.wait_async(gio::Cancellable::NONE, move |result| {
                let okay = result.is_ok() && child.is_successful();
                if let Some(done) = done {
                    done(okay, String::new());
                }
            });
        }
    }
}

pub fn default_reconfigure() -> Reconfigure { Rc::new(reconfigure) }

// ---- environment file ------------------------------------------------------

fn assignment(line: &str) -> Option<(String, String)> {
    let stripped = strip(line);
    if stripped.is_empty() || stripped.starts_with('#') || !stripped.contains('=') {
        return None;
    }
    let (name, value) = stripped.split_once('=').unwrap();
    let mut name = strip(name);
    if let Some(rest) = name.strip_prefix("export ") {
        name = strip(rest);
    }
    ENV_NAME.is_match(name).then(|| (name.to_owned(), strip(value).to_owned()))
}

pub fn parse_environment(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in compat::splitlines(text, false) {
        if let Some((name, value)) = assignment(&line) {
            values.insert(name, value); // labwc applies lines in order; the last wins.
        }
    }
    values
}

/// Changes for [`edit_environment`]: `Some` sets, `None` removes (dict order).
pub type EnvValues = Vec<(String, Option<String>)>;

/// Set (Some) or remove (None) variables, keeping every other line intact.
pub fn edit_environment(text: &str, values: &[(String, Option<String>)]) -> Result<String> {
    for (name, item) in values {
        if !ENV_NAME.is_match(name) {
            return value("Invalid environment variable name.");
        }
        if item.as_ref().is_some_and(|v| v.contains(['\n', '\r', '\0'])) {
            return value("Invalid environment variable value.");
        }
    }
    let lookup = |name: &str| values.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone());
    let mut pending: Vec<(String, Option<String>)> = Vec::new();
    for (name, item) in values {
        if let Some(slot) = pending.iter_mut().find(|(n, _)| n == name) {
            slot.1 = item.clone();
        } else {
            pending.push((name.clone(), item.clone()));
        }
    }
    let mut result: Vec<String> = Vec::new();
    for line in compat::splitlines(text, true) {
        if let Some((name, _)) = assignment(&line) {
            if let Some(item) = lookup(&name) {
                if let Some(item) = item {
                    let ending = if line.ends_with('\n') { "\n" } else { "" };
                    result.push(format!("{name}={item}{ending}"));
                }
                pending.retain(|(n, _)| *n != name);
                continue;
            }
        }
        result.push(line);
    }
    let additions: Vec<String> = pending.iter().filter_map(|(n, v)| v.as_ref().map(|v| format!("{n}={v}\n"))).collect();
    if !additions.is_empty() {
        if let Some(last) = result.last_mut() {
            if !last.ends_with('\n') {
                last.push('\n');
            }
        }
        result.extend(additions);
    }
    Ok(result.concat())
}

fn decode(data: &[u8]) -> Result<&str> {
    std::str::from_utf8(data).map_err(|error| {
        let position = error.valid_up_to();
        let reason = match error.error_len() {
            None => "unexpected end of data",
            Some(_) if (0x80..0xc2).contains(&data[position]) || data[position] >= 0xf5 => "invalid start byte",
            Some(_) => "invalid continuation byte",
        };
        Error::Value(format!("'utf-8' codec can't decode byte 0x{:02x} in position {position}: {reason}", data[position]))
    })
}

pub fn update_environment(values: &[(String, Option<String>)], path: Option<&Path>) -> Result<bool> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(|| environment_path(None));
    update(&path, |data| Ok(edit_environment(decode(data)?, values)?.into_bytes()), "labwc-environment")
}

pub fn read_environment(path: Option<&Path>) -> Result<HashMap<String, String>> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(|| environment_path(None));
    Ok(parse_environment(&String::from_utf8_lossy(&read_bytes(&path)?)))
}

/// Convenience: `{name: value}` pairs in order.
pub fn env_values(pairs: &[(&str, Option<&str>)]) -> EnvValues {
    pairs.iter().map(|(n, v)| (n.to_string(), v.map(str::to_owned))).collect()
}

// ---- rc.xml keyboard section ----------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Action {
    pub name: String,
    /// `(name, value)` pairs, including child `<command>` text.
    pub attributes: Vec<(String, String)>,
}

impl Action {
    pub fn new(name: &str, attributes: &[(&str, &str)]) -> Action {
        Action { name: name.into(), attributes: attributes.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    pub fn get(&self, key: &str, default: &str) -> String {
        let key = casefold(key);
        self.attributes.iter().find(|(name, _)| casefold(name) == key).map(|(_, v)| v.clone())
            .unwrap_or_else(|| default.to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keybind {
    pub key: String,
    pub actions: Vec<Action>,
    /// Byte offset of the `<keybind` start tag.
    pub start: usize,
    pub end: usize,
    /// Inside the tiling manager's managed block.
    pub tiling: bool,
    /// Inside tessel-configure-session's keybinding block.
    pub suite: bool,
    pub on_release: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyboardSection {
    /// `(start, end)` of `<keyboard>`, None if absent.
    pub span: Option<(usize, usize)>,
    /// End of the `<keyboard ...>` start tag.
    pub open_end: usize,
    pub self_closing: bool,
    /// Offset of `</labwc_config>`.
    pub root_end: usize,
    /// Child name (casefolded) → (text, (start, end)).
    pub values: HashMap<String, (String, (usize, usize))>,
    pub has_default: bool,
    pub keybinds: Vec<Keybind>,
}

struct Walk<'a> {
    section: &'a mut KeyboardSection,
    tiling: bool,
    suite: bool,
}

impl Walk<'_> {
    fn comment(&mut self, text: &str) {
        match strip(text) {
            TESSEL_TILING_BEGIN => self.tiling = true,
            TESSEL_TILING_END => self.tiling = false,
            TESSEL_KEYBINDINGS_END => self.suite = false,
            text if text.starts_with(TESSEL_KEYBINDINGS_BEGIN) => self.suite = true,
            _ => {}
        }
    }

    /// Visit comments in document order (keybinds see only earlier ones).
    fn comments_in(&mut self, element: &Element) {
        for node in &element.children {
            match node {
                Node::Comment(c) => self.comment(c),
                Node::Element(e) => self.comments_in(e),
                _ => {}
            }
        }
    }

    fn keybind(&mut self, element: &Element, data: &[u8]) -> Result<()> {
        let key = element.attribute("key").unwrap_or("").to_owned();
        let release = element.attribute("onRelease").unwrap_or("").to_lowercase() == "yes";
        let (tiling, suite) = (self.tiling, self.suite);
        let mut actions = Vec::new();
        for node in &element.children {
            match node {
                Node::Comment(c) => self.comment(c),
                Node::Element(action) => {
                    let is_action = local_lower(&action.name) == "action";
                    let mut attributes: Vec<(String, String)> = if is_action { action.attributes.clone() } else { Vec::new() };
                    for child in &action.children {
                        match child {
                            Node::Comment(c) => self.comment(c),
                            Node::Element(inner) => {
                                if is_action {
                                    let name = local_lower(&inner.name);
                                    if !attributes.iter().any(|(n, _)| *n == name) {
                                        attributes.push((name, strip(&compat::descendant_text(inner)).to_owned()));
                                    }
                                }
                                self.comments_in(inner);
                            }
                            _ => {}
                        }
                    }
                    if is_action {
                        let name = action.attribute("name").unwrap_or("").to_owned();
                        attributes.retain(|(n, _)| n != "name");
                        actions.push(Action { name, attributes });
                    }
                }
                _ => {}
            }
        }
        // `_tag_end(data, position)` when the end event is at `</` — for an
        // empty keybind directly before a closing tag, that tag's end.
        let position = compat::end_event(element);
        let end = if data[position..].starts_with(b"</") { tag_end(data, position)? } else { tag_end(data, element.start)? };
        self.section.keybinds.push(Keybind { key, actions, start: element.start, end, tiling, suite, on_release: release });
        Ok(())
    }
}

pub fn read_keyboard(data: &[u8]) -> Result<KeyboardSection> {
    if data.len() > MAX_BYTES {
        return value("The labwc configuration is too large.");
    }
    let mut section = KeyboardSection::default();
    if bytes_strip(data).is_empty() {
        return Ok(section);
    }
    let document = compat::parse_xml(data, DOCTYPE)?;
    let root = &document.root;
    if local_lower(&root.name) != "labwc_config" {
        return value("Expected a labwc_config document.");
    }
    section.root_end = compat::end_event(root);
    let mut walk = Walk { section: &mut section, tiling: false, suite: false };
    for comment in compat::prolog_comments(data, root.start) {
        walk.comment(&comment);
    }
    for node in &root.children {
        let keyboard = match node {
            Node::Comment(c) => {
                walk.comment(c);
                continue;
            }
            Node::Element(e) => e,
            _ => continue,
        };
        if local_lower(&keyboard.name) != "keyboard" {
            walk.comments_in(keyboard);
            continue;
        }
        if walk.section.span.is_some() {
            return value("Multiple keyboard sections are not supported.");
        }
        walk.section.self_closing = keyboard.self_closing;
        walk.section.span = Some((keyboard.start, keyboard.end));
        walk.section.open_end = keyboard.start_end;
        for child in &keyboard.children {
            let element = match child {
                Node::Comment(c) => {
                    walk.comment(c);
                    continue;
                }
                Node::Element(e) => e,
                _ => continue,
            };
            let local = local_lower(&element.name);
            if local == "default" {
                walk.section.has_default = true;
                walk.comments_in(element);
            } else if local == "keybind" {
                walk.keybind(element, data)?;
            } else {
                // Python records the value when the end event is followed by `</`
                // (for an empty element: the parent's end tag, an empty span).
                let position = compat::end_event(element);
                if data[position..].starts_with(b"</") {
                    let text = strip(&compat::descendant_text(element)).to_owned();
                    walk.section.values.insert(casefold(&local), (text, (element.start_end, position)));
                }
                walk.comments_in(element);
            }
        }
    }
    Ok(section)
}

/// `xml.sax.saxutils.escape`.
pub fn escape(text: &str) -> String { text.replace('&', "&amp;").replace('>', "&gt;").replace('<', "&lt;") }

/// `xml.sax.saxutils.quoteattr`.
pub fn quoteattr(text: &str) -> String {
    let data = escape(text).replace('\n', "&#10;").replace('\r', "&#13;").replace('\t', "&#9;");
    if data.contains('"') {
        if data.contains('\'') { format!("\"{}\"", data.replace('"', "&quot;")) } else { format!("'{data}'") }
    } else {
        format!("\"{data}\"")
    }
}

fn rfind(data: &[u8], byte: u8, end: usize) -> Option<usize> { data[..end].iter().rposition(|b| *b == byte) }

/// Set `<keyboard>` child values such as repeatRate; None removes the element.
pub fn set_keyboard_values(data: &[u8], values: &[(String, Option<String>)]) -> Result<Vec<u8>> {
    for (name, _) in values {
        if !SETTING_NAME.is_match(name) {
            return value("Invalid keyboard setting.");
        }
    }
    let mut data = if bytes_strip(data).is_empty() {
        b"<?xml version=\"1.0\"?>\n<labwc_config>\n</labwc_config>\n".to_vec()
    } else {
        data.to_vec()
    };
    for (name, item) in values {
        let section = read_keyboard(&data)?;
        let encoded = item.as_ref().map(|v| escape(v).into_bytes());
        if let Some((_old, (begin, finish))) = section.values.get(&casefold(name)).cloned() {
            match encoded {
                None => {
                    let mut element = rfind(&data, b'<', begin).map(|i| i as isize).unwrap_or(-1);
                    let mut closing = tag_end(&data, finish)?;
                    let line_start = (if element < 0 { None } else { rfind(&data, b'\n', element as usize) })
                        .map(|i| i + 1).unwrap_or(0);
                    let line_end = data[closing..].iter().position(|b| *b == b'\n').map(|i| i + closing);
                    let before = if element < 0 { &data[line_start..0.max(line_start)] } else { &data[line_start..element as usize] };
                    if bytes_strip(before).is_empty() {
                        if let Some(line_end) = line_end {
                            if bytes_strip(&data[closing..line_end]).is_empty() {
                                element = line_start as isize;
                                closing = line_end + 1;
                            }
                        }
                    }
                    // Python's data[:-1] when rfind fails: never happens for a parsed element.
                    let element = element.max(0) as usize;
                    data = [&data[..element], &data[closing..]].concat();
                }
                Some(encoded) => data = [&data[..begin], &encoded[..], &data[finish..]].concat(),
            }
            continue;
        }
        let Some(encoded) = encoded else { continue };
        let element = [b"<", name.as_bytes(), b">", &encoded[..], b"</", name.as_bytes(), b">"].concat();
        match section.span {
            None => {
                let insert = section.root_end;
                data = [&data[..insert], b"  <keyboard>\n    ", &element[..], b"\n  </keyboard>\n", &data[insert..]].concat();
            }
            Some((begin, finish)) if section.self_closing => {
                let tag = bytes_rstrip(&data[begin..finish]);
                let tag = bytes_rstrip(&tag[..tag.len().saturating_sub(2)]).to_vec();
                data = [&data[..begin], &tag[..], b">\n    ", &element[..], b"\n  </keyboard>", &data[finish..]].concat();
            }
            Some(_) => {
                let insert = section.open_end;
                data = [&data[..insert], b"\n    ", &element[..], &data[insert..]].concat();
            }
        }
    }
    read_keyboard(&data)?;
    Ok(data)
}

/// labwc's `parse_bool` words; None for anything else.
pub fn labwc_bool(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Some(true),
        "no" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Num Lock at sign-in: on unless `<numlock>` says off. A missing element is
/// on because `tessel-configure-session` adds `<numlock>on</numlock>` then.
pub fn numlock_enabled(section: &KeyboardSection) -> bool {
    section.values.get("numlock").is_none_or(|(text, _)| labwc_bool(text) != Some(false))
}

/// Write `<numlock>on|off</numlock>` into `<keyboard>`.
pub fn set_numlock(data: &[u8], on: bool) -> Result<Vec<u8>> {
    let word = if on { "on" } else { "off" };
    let section = read_keyboard(data)?;
    if let Some((start, end)) = section.span.filter(|_| !section.values.contains_key("numlock")) {
        // An empty `<numlock/>` has no value labwc reads; replace it rather than add a second element.
        let masked = COMMENT.replace_all(&data[start..end], |c: &regex::bytes::Captures| vec![b' '; c[0].len()]).into_owned();
        if let Some(found) = EMPTY_NUMLOCK.find(&masked) {
            let element = format!("<numlock>{word}</numlock>");
            let data = [&data[..start + found.start()], element.as_bytes(), &data[start + found.end()..]].concat();
            read_keyboard(&data)?;
            return Ok(data);
        }
    }
    set_keyboard_values(data, &[("numlock".to_owned(), Some(word.to_owned()))])
}

/// Change the key of the user keybind whose start tag is at byte offset `start`.
pub fn rebind(data: &[u8], start: usize, old_key: &str, new_key: &str) -> Result<Vec<u8>> {
    rebind_many(data, &[(start, old_key.to_owned(), new_key.to_owned())])
}

/// `rebind` for several keybinds in one pass: `(start, old key, new key)`.
pub fn rebind_many(data: &[u8], changes: &[(usize, String, String)]) -> Result<Vec<u8>> {
    for (_, _, new_key) in changes {
        if new_key.is_empty() || new_key.contains(['<', '>', '&', '"', '\'', '\n']) || new_key.chars().count() > 128 {
            return value("Invalid key combination.");
        }
    }
    let section = read_keyboard(data)?;
    for (start, old_key, _) in changes {
        match section.keybinds.iter().find(|item| item.start == *start) {
            Some(bind) if bind.key == *old_key => {
                if bind.tiling {
                    return value("Tiling shortcuts are managed by the tiling settings.");
                }
            }
            _ => return compat::runtime("The shortcut changed while it was being edited. Try again."),
        }
    }
    // From the end of the file backwards, so earlier offsets stay valid.
    let mut ordered: Vec<&(usize, String, String)> = changes.iter().collect();
    ordered.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    ordered.dedup_by_key(|(start, _, _)| *start);
    let mut data = data.to_vec();
    for (start, _, new_key) in ordered {
        let start = *start;
        let end = tag_end(&data, start)?;
        let tag = &data[start..end];
        let Some(found) = KEY_ATTRIBUTE.find(tag) else { return value("The shortcut has no key attribute.") };
        let replacement = format!("key={}", quoteattr(new_key));
        let tag = [&tag[..found.start()], replacement.as_bytes(), &tag[found.end()..]].concat();
        data = [&data[..start], &tag[..], &data[end..]].concat();
    }
    read_keyboard(&data)?;
    Ok(data)
}

pub fn update_rc(transform: impl FnOnce(&[u8]) -> Result<Vec<u8>>, path: Option<&Path>) -> Result<bool> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(rc_path);
    update(&path, transform, "labwc-rc")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::{scratch, EnvGuard};

    pub const RC: &[u8] = br#"<?xml version="1.0"?>
<!-- user comment -->
<labwc_config>
  <core><hdr>no</hdr></core>
  <keyboard>
    <default/>
    <repeatRate>30</repeatRate>
    <keybind key="W-d"><action name="ToggleShowDesktop"/></keybind>
    <keybind key="W-l"><action name="Execute"><command>/home/u/.local/bin/tessel-shell-lock</command></action></keybind>
    <keybind key="A-F4"/>
    <!-- Tessel Tiling keyboard begin -->
    <keybind key="W-t"><action name="Tiling" command="float"/></keybind>
    <!-- Tessel Tiling keyboard end -->
  </keyboard>
  <mouse><default/></mouse>
</labwc_config>
"#;

    fn replace(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        String::from_utf8_lossy(data).replace(std::str::from_utf8(from).unwrap(), std::str::from_utf8(to).unwrap()).into_bytes()
    }

    #[test]
    fn environment_edit_preserves_comments_duplicates_and_unrelated_lines() {
        let text = "# keep\nXKB_DEFAULT_LAYOUT=se\nFOO=1\n#XKB_DEFAULT_LAYOUT=fr\nXKB_DEFAULT_LAYOUT=us\nXKB_DEFAULT_OPTIONS=x:y";
        let result = edit_environment(text, &env_values(&[("XKB_DEFAULT_LAYOUT", Some("us,de")), ("XKB_DEFAULT_OPTIONS", None),
                                                          ("XKB_DEFAULT_VARIANT", Some(",nodeadkeys"))])).unwrap();
        assert_eq!(result, "# keep\nXKB_DEFAULT_LAYOUT=us,de\nFOO=1\n#XKB_DEFAULT_LAYOUT=fr\n\
                            XKB_DEFAULT_LAYOUT=us,de\nXKB_DEFAULT_VARIANT=,nodeadkeys\n");
        assert_eq!(parse_environment(&result)["XKB_DEFAULT_LAYOUT"], "us,de");
        assert!(edit_environment("", &env_values(&[("A", Some("x\nB=y"))])).unwrap_err().is_value());
    }

    #[test]
    fn update_backs_up_once_keeps_mode_and_reports_no_change() {
        let root = scratch("labwc-update");
        let _env = EnvGuard::set(&[("XDG_STATE_HOME", root.join("state").to_str().unwrap()),
                                   ("XDG_CONFIG_HOME", root.join("config").to_str().unwrap())]);
        let path = root.join("environment");
        std::fs::write(&path, "A=1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(update_environment(&env_values(&[("A", Some("2"))]), Some(&path)).unwrap());
        assert!(update_environment(&env_values(&[("A", Some("3"))]), Some(&path)).unwrap());
        assert!(!update_environment(&env_values(&[("A", Some("3"))]), Some(&path)).unwrap());
        let backup = root.join("state/tessel/backups/labwc-environment/environment.orig");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "A=1\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=3\n");
        assert_eq!(compat::file_mode(&path), Some(0o640));
    }

    #[test]
    fn keyboard_values_are_spliced_without_touching_other_xml() {
        let data = set_keyboard_values(RC, &env_values(&[("repeatRate", Some("40")), ("repeatDelay", Some("300"))])).unwrap();
        let text = String::from_utf8(data.clone()).unwrap();
        assert!(text.contains("<repeatRate>40</repeatRate>"));
        assert!(text.contains("<repeatDelay>300</repeatDelay>"));
        assert_eq!(text.replace("40", "30").replace("\n    <repeatDelay>300</repeatDelay>", "").as_bytes(), RC);
        let removed = set_keyboard_values(&data, &env_values(&[("repeatDelay", None)])).unwrap();
        assert!(!String::from_utf8_lossy(&removed).contains("repeatDelay"));
        let created = set_keyboard_values(b"<labwc_config>\n</labwc_config>\n", &env_values(&[("repeatRate", Some("9"))])).unwrap();
        assert_eq!(read_keyboard(&created).unwrap().values["repeatrate"].0, "9");
        let selfclosed = set_keyboard_values(b"<labwc_config><keyboard/></labwc_config>", &env_values(&[("repeatRate", Some("9"))])).unwrap();
        assert!(String::from_utf8_lossy(&selfclosed).contains("<keyboard>"));
        assert!(set_keyboard_values(RC, &env_values(&[("bad-name", Some("1"))])).unwrap_err().is_value());
    }

    #[test]
    fn numlock_is_spliced_into_the_keyboard_section() {
        assert!(numlock_enabled(&read_keyboard(RC).unwrap()), "missing reads as on");
        let off = set_numlock(RC, false).unwrap();
        assert_eq!(off, replace(RC, b"  <keyboard>\n", b"  <keyboard>\n    <numlock>off</numlock>\n"), "inserted, the rest kept");
        assert!(!numlock_enabled(&read_keyboard(&off).unwrap()));
        let on = set_numlock(&off, true).unwrap();
        assert_eq!(on, replace(&off, b"<numlock>off</numlock>", b"<numlock>on</numlock>"), "replaced in place");
        assert_eq!(set_numlock(&on, true).unwrap(), on, "idempotent");
        assert!(numlock_enabled(&read_keyboard(&on).unwrap()));
        let spaced = b"<labwc_config><keyboard><!-- mine --><NumLock> No </NumLock><repeatRate>9</repeatRate></keyboard></labwc_config>";
        assert!(!numlock_enabled(&read_keyboard(spaced).unwrap()));
        assert_eq!(set_numlock(spaced, true).unwrap(),
                   b"<labwc_config><keyboard><!-- mine --><NumLock>on</NumLock><repeatRate>9</repeatRate></keyboard></labwc_config>");
        let selfclosed = set_numlock(b"<labwc_config><keyboard/></labwc_config>", false).unwrap();
        assert_eq!(selfclosed, b"<labwc_config><keyboard>\n    <numlock>off</numlock>\n  </keyboard></labwc_config>");
        let missing = set_numlock(b"<labwc_config>\n</labwc_config>\n", false).unwrap();
        assert_eq!(missing, b"<labwc_config>\n  <keyboard>\n    <numlock>off</numlock>\n  </keyboard>\n</labwc_config>\n");
        let created = set_numlock(b"", true).unwrap();
        assert!(numlock_enabled(&read_keyboard(&created).unwrap()));
        assert!(String::from_utf8_lossy(&created).contains("<numlock>on</numlock>"));
        // labwc reads no value from `<numlock/>`: it becomes the written element.
        let empty = b"<labwc_config><keyboard><!-- <numlock/> --><NumLock /><default/></keyboard></labwc_config>";
        assert_eq!(set_numlock(empty, false).unwrap(),
                   b"<labwc_config><keyboard><!-- <numlock/> --><numlock>off</numlock><default/></keyboard></labwc_config>");
        for word in ["off", "OFF", "no", "false", "0"] {
            let data = format!("<labwc_config><keyboard><numlock>{word}</numlock></keyboard></labwc_config>");
            assert!(!numlock_enabled(&read_keyboard(data.as_bytes()).unwrap()), "{word}");
        }
        for word in ["on", "Yes", "true", "1", "maybe"] {
            let data = format!("<labwc_config><keyboard><numlock>{word}</numlock></keyboard></labwc_config>");
            assert!(numlock_enabled(&read_keyboard(data.as_bytes()).unwrap()), "{word}");
        }
    }

    #[test]
    fn rebind_changes_only_the_key_attribute_and_refuses_tiling_or_stale_edits() {
        let section = read_keyboard(RC).unwrap();
        let lock = section.keybinds.iter().find(|b| b.key == "W-l").unwrap();
        let data = rebind(RC, lock.start, "W-l", "W-C-l").unwrap();
        assert_eq!(data, replace(RC, b"key=\"W-l\"", b"key=\"W-C-l\""));
        let tiling = section.keybinds.iter().find(|b| b.tiling).unwrap();
        assert_eq!(tiling.key, "W-t");
        assert!(rebind(RC, tiling.start, "W-t", "W-y").unwrap_err().is_value());
        assert!(rebind(&data, lock.start, "W-x", "W-y").unwrap_err().is_runtime());
        assert!(rebind(RC, lock.start, "W-l", "W-\"").unwrap_err().is_value());
        let lock_action = &lock.actions[0];
        assert_eq!(lock_action.get("command", ""), "/home/u/.local/bin/tessel-shell-lock");
        assert!(section.has_default);
        assert_eq!(section.keybinds.iter().find(|b| b.key == "A-F4").unwrap().actions, vec![]);
    }

    #[test]
    fn suite_block_is_tracked_and_several_keys_change_in_one_pass() {
        let data = br#"<labwc_config><keyboard><keybind key="W-x"/><!-- Tessel keybindings begin: generated.
     More text. --><keybind key="W-l"><action name="Close"/></keybind><keybind key="XF86AudioPlay"><action name="Iconify"/></keybind>
<!-- Tessel keybindings end --><keybind key="W-y"><action name="Close"/></keybind></keyboard></labwc_config>"#;
        let section = read_keyboard(data).unwrap();
        let suite: Vec<(&str, bool)> = section.keybinds.iter().map(|b| (b.key.as_str(), b.suite)).collect();
        assert_eq!(suite, [("W-x", false), ("W-l", true), ("XF86AudioPlay", true), ("W-y", false)]);
        let at = |key: &str| section.keybinds.iter().find(|b| b.key == key).unwrap().start;
        // Longer and shorter keys in one pass; the offsets refer to the original bytes.
        let changes = [(at("W-l"), "W-l".to_owned(), "W-C-S-l".to_owned()), (at("XF86AudioPlay"), "XF86AudioPlay".to_owned(), "F8".to_owned()),
                       (at("W-y"), "W-y".to_owned(), "W-S-y".to_owned())];
        let changed = rebind_many(data, &changes).unwrap();
        let expected = replace(&replace(&replace(data, b"\"W-l\"", b"\"W-C-S-l\""), b"\"XF86AudioPlay\"", b"\"F8\""), b"\"W-y\"", b"\"W-S-y\"");
        assert_eq!(changed, expected);
        // Stale: one changed key refuses the whole edit.
        assert!(rebind_many(&changed, &changes).unwrap_err().is_runtime());
        assert!(rebind_many(data, &[(at("W-l"), "W-l".to_owned(), "W-<".to_owned())]).unwrap_err().is_value());
    }

    #[test]
    fn doctype_and_foreign_root_are_rejected() {
        assert!(read_keyboard(b"<!DOCTYPE x [<!ENTITY a \"b\">]><labwc_config/>").unwrap_err().is_value());
        assert!(read_keyboard(b"<other/>").unwrap_err().is_value());
        assert!(read_keyboard(b"<labwc_config><keyboard/><keyboard/></labwc_config>").unwrap_err().is_value());
        assert!(matches!(read_keyboard(b"<labwc_config>").unwrap_err(), Error::Xml(_)));
        assert_eq!(read_keyboard(b"  \n").unwrap(), KeyboardSection::default());
    }

    #[test]
    fn quoteattr_matches_saxutils() {
        assert_eq!(quoteattr("a"), "\"a\"");
        assert_eq!(quoteattr("a\"b"), "'a\"b'");
        assert_eq!(quoteattr("a\"b'c"), "\"a&quot;b'c\"");
        assert_eq!(quoteattr("a\tb"), "\"a&#9;b\"");
    }
}
