//! Temporarily overlay the Omarchy bindings on rc.xml; disabling restores
//! the original keys byte for byte (tiling_config.py), plus the gapless and
//! focus-outline preferences.

use crate::bindings::{keyboard_xml, mouse_xml};
use crate::client::{self, ClientError, Reply};
use crate::compat::{self, as_int, b64decode, b64encode, is_true, python_os_error_text, truthy, utc_stamp};
use crate::gaps::with_gap;
use crate::outline::{Call, OutlineSync};
use crate::xml::{self, Node};
use regex::Regex;
use serde_json::{Map, Value};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A Python exception's `str()`: what the CLI prints and the UI shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

impl std::error::Error for Error {}

impl From<ClientError> for Error {
    fn from(error: ClientError) -> Self { Error(error.message().to_owned()) }
}

impl From<String> for Error {
    fn from(message: String) -> Self { Error(message) }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self { Error(message.into()) }
}

impl From<xml::XmlError> for Error {
    fn from(error: xml::XmlError) -> Self { Error(error.to_string()) }
}

pub type Result<T> = std::result::Result<T, Error>;

fn io_error(error: io::Error, path: &Path) -> Error { Error(python_os_error_text(&error, Some(path))) }

fn read(path: &Path) -> Result<Vec<u8>> { fs::read(path).map_err(|e| io_error(e, path)) }

const KEYBOARD_BEGIN: &str = "<!-- Tessel Tiling keyboard begin -->";

fn begin_marker(section: &str) -> String { format!("<!-- Tessel Tiling {section} begin -->") }

fn end_marker(section: &str) -> String { format!("<!-- Tessel Tiling {section} end -->") }

/// `edit_bindings(data, enabled, launcher)`: add or remove the managed
/// `<keyboard>`/`<mouse>` blocks, touching no other bytes.
pub fn edit_bindings(data: &[u8], enabled: bool, launcher: &Path) -> Result<Vec<u8>> {
    let mut text = String::from_utf8(data.to_vec()).map_err(|e| {
        let at = e.utf8_error().valid_up_to();
        Error(format!("'utf-8' codec can't decode byte 0x{:02x} in position {at}: invalid start byte", data[at]))
    })?;
    if text.contains("<!DOCTYPE") || text.contains("<!ENTITY") {
        return Err("External XML entities are not supported.".into());
    }
    let document = xml::parse_str(&text)?;
    if document.root.name != "labwc_config" {
        return Err("Expected labwc configuration.".into());
    }
    let comments = Regex::new(r"(?s)<!--.*?-->").unwrap();
    for (section, content) in [("keyboard", keyboard_xml(launcher)), ("mouse", mouse_xml().to_owned())] {
        let (begin, end) = (begin_marker(section), end_marker(section));
        let pattern = Regex::new(&format!("(?s){}.*?{}", regex::escape(&begin), regex::escape(&end))).unwrap();
        let begins = text.matches(&begin).count();
        if begins != text.matches(&end).count() || begins > 1 {
            return Err("The tiling keybinding block was edited; repair its markers first.".into());
        }
        text = pattern.replace_all(&text, "").into_owned();
        // Missing/self-closing sections need defaults while enabled. Remember
        // their exact original spelling, without dropping subsequent user edits.
        let outer = Regex::new(&format!(
            r"(?s)<!-- Tessel Tiling {section} section ([A-Za-z0-9+/=]*) -->(.*?)<!-- Tessel Tiling {section} section end -->")).unwrap();
        let mut unwrapped = String::with_capacity(text.len());
        let mut last = 0;
        for captures in outer.captures_iter(&text) {
            let whole = captures.get(0).unwrap();
            unwrapped.push_str(&text[last..whole.start()]);
            let original = String::from_utf8(b64decode(&captures[1]).map_err(Error)?)
                .map_err(|_| Error("'utf-8' codec can't decode the saved section".into()))?;
            let body = &captures[2];
            let parsed = xml::parse_str(body)?;
            let significant: Vec<&Node> = parsed.root.children.iter()
                .filter(|n| matches!(n, Node::Element(_) | Node::Comment(_))).collect();
            let only_default = significant.len() == 1
                && matches!(significant[0], Node::Element(e) if e.to_xml() == "<default/>");
            unwrapped.push_str(if only_default { &original } else { body });
            last = whole.end();
        }
        unwrapped.push_str(&text[last..]);
        text = unwrapped;
        if !enabled {
            continue;
        }
        let block = format!("{begin}\n{content}\n    {end}");
        // Exclude comments from the search while retaining byte positions.
        let masked = comments.replace_all(&text, |c: &regex::Captures| " ".repeat(c[0].len())).into_owned();
        let section_pattern = Regex::new(&format!(r"(?s)<{section}\b[^>]*?(?:/>|>.*?</{section}\s*>)")).unwrap();
        let matches: Vec<regex::Match> = section_pattern.find_iter(&masked).collect();
        if matches.len() > 1 {
            return Err("Multiple input configuration sections are not supported.".into());
        }
        if let Some(found) = matches.first().filter(|m| !m.as_str().ends_with("/>")) {
            let closing = format!("</{section}");
            let position = found.start() + masked[found.start()..found.end()].rfind(&closing).unwrap();
            text.insert_str(position, &block);
        } else {
            let (start, finish) = match matches.first() {
                Some(found) => (found.start(), found.end()),
                None => match text.rfind("</labwc_config>") {
                    Some(start) => (start, start),
                    None => return Err("Expected a nonempty labwc root element.".into()),
                },
            };
            let encoded = b64encode(text[start..finish].as_bytes());
            let replacement = format!("<!-- Tessel Tiling {section} section {encoded} --><{section}><default/>{block}</{section}>\
                                       <!-- Tessel Tiling {section} section end -->");
            text.replace_range(start..finish, &replacement);
        }
    }
    xml::parse_str(&text)?;
    Ok(text.into_bytes())
}

/// True when `data` carries the managed keyboard block.
pub fn has_bindings(data: &[u8]) -> bool { data.windows(KEYBOARD_BEGIN.len()).any(|w| w == KEYBOARD_BEGIN.as_bytes()) }

/// `workspace_config.session_config()` for the compositor `pid` (Python
/// sets `LABWC_PID` around the call): `(rc.xml path, fallback)`.
pub fn session_config(pid: Option<&str>) -> Result<(PathBuf, Option<PathBuf>)> {
    let pid = pid.map(str::to_owned).unwrap_or_else(|| std::env::var("LABWC_PID").unwrap_or_default());
    if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
        return Err("Workspace settings require a running labwc session.".into());
    }
    let process = Path::new("/proc").join(&pid);
    let cmdline = read(&process.join("cmdline"))?;
    let cmdline = String::from_utf8_lossy(&cmdline).into_owned();
    let args: Vec<&str> = cmdline.trim_end_matches('\0').split('\0').collect();
    let executable = fs::read_link(process.join("exe")).map_err(|e| io_error(e, &process.join("exe")))?;
    let executable = executable.to_string_lossy();
    let executable = executable.strip_suffix(" (deleted)").unwrap_or(&executable);
    if Path::new(executable).file_name().map(|n| n != "labwc").unwrap_or(true) {
        return Err("The labwc session could not be identified.".into());
    }
    let mut file: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut index = 1;
    while index < args.len() {
        let argument = args[index];
        if argument == "-m" || argument == "--merge-config" {
            return Err("Workspace count editing is unavailable with merged labwc configurations.".into());
        }
        if ["-c", "--config", "-C", "--config-dir"].contains(&argument) {
            index += 1;
            let value = args.get(index).ok_or_else(|| Error("list index out of range".into()))?.to_string();
            if argument == "-c" || argument == "--config" { file = Some(value) } else { dir = Some(value) }
        } else if let Some(value) = argument.strip_prefix("--config=") {
            file = Some(value.into());
        } else if let Some(value) = argument.strip_prefix("--config-dir=") {
            dir = Some(value.into());
        } else if argument.len() > 2 && argument.starts_with("-c") {
            file = Some(argument[2..].into());
        } else if argument.len() > 2 && argument.starts_with("-C") {
            dir = Some(argument[2..].into());
        }
        index += 1;
    }
    let any_option = file.is_some() || dir.is_some();
    let mut target = match (&file, &dir) {
        (Some(file), _) => PathBuf::from(file),
        (None, Some(dir)) => Path::new(dir).join("rc.xml"),
        _ => {
            let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
                .unwrap_or_else(|| tessel_core::paths::home().join(".config"));
            base.join("labwc/rc.xml")
        }
    };
    if !target.is_absolute() {
        let cwd = fs::read_link(process.join("cwd")).map_err(|e| io_error(e, &process.join("cwd")))?;
        target = cwd.join(target);
    }
    let mut fallback = None;
    if !any_option {
        let dirs = std::env::var("XDG_CONFIG_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
        for base in dirs.split(':') {
            let candidate = Path::new(base).join("labwc/rc.xml");
            if candidate.is_file() {
                fallback = Some(candidate);
                break;
            }
        }
    }
    Ok((target, fallback))
}

pub type Reload = Arc<dyn Fn() -> Result<()> + Send + Sync>;
pub type Writer = Arc<dyn Fn(&Path, &[u8], u32) -> io::Result<()> + Send + Sync>;

/// `subprocess.run(['labwc', '--reconfigure'], check=True, timeout=5,
/// capture_output=True, env={…, LABWC_PID})`.
pub fn reconfigure(compositor_pid: Option<&Value>) -> Result<()> {
    let mut command = std::process::Command::new("labwc");
    command.arg("--reconfigure").stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    if let Some(pid) = compositor_pid.filter(|p| truthy(Some(p))) {
        command.env("LABWC_PID", compat::py_str(pid));
    }
    run_checked(command, "['labwc', '--reconfigure']", 5.0)
}

/// Run with a timeout like `subprocess.run(check=True)`, with Python's messages.
pub fn run_checked(mut command: std::process::Command, display: &str, timeout: f64) -> Result<()> {
    let mut child = command.spawn().map_err(|e| {
        let program = command.get_program().to_string_lossy().into_owned();
        Error(python_os_error_text(&e, Some(Path::new(&program))))
    })?;
    // Drain the pipes so a chatty child cannot block.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let drain = |pipe: Option<Box<dyn io::Read + Send>>| std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let _ = io::copy(&mut pipe, &mut io::sink());
        }
    });
    let out = drain(stdout.map(|p| Box::new(p) as Box<dyn io::Read + Send>));
    let err = drain(stderr.map(|p| Box::new(p) as Box<dyn io::Read + Send>));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let shown = if timeout.fract() == 0.0 { format!("{}", timeout as i64) } else { tessel_core::pyfmt::float_repr(timeout) };
                return Err(Error(format!("Command '{display}' timed out after {shown} seconds")));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(error) => return Err(Error(python_os_error_text(&error, None))),
        }
    };
    let _ = out.join();
    let _ = err.join();
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(0), _) => Ok(()),
        (Some(code), _) => Err(Error(format!("Command '{display}' returned non-zero exit status {code}."))),
        (None, Some(signal)) => Err(Error(format!("Command '{display}' died with <Signals.{}: {signal}>.", signal_name(signal)))),
        _ => Err(Error(format!("Command '{display}' failed."))),
    }
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGTERM => "SIGTERM",
        libc::SIGKILL => "SIGKILL",
        libc::SIGINT => "SIGINT",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGABRT => "SIGABRT",
        libc::SIGHUP => "SIGHUP",
        _ => "SIG?",
    }
}

/// The exclusive `change.lock` held by every mutator.
struct Lock(#[allow(dead_code)] fs::File);

impl Lock {
    fn acquire(state_dir: &Path) -> Result<Self> {
        fs::create_dir_all(state_dir).map_err(|e| io_error(e, state_dir))?;
        let path = state_dir.join("change.lock");
        let file = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&path).map_err(|e| io_error(e, &path))?;
        use std::os::fd::AsRawFd;
        while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(io_error(error, &path));
            }
        }
        Ok(Self(file))
    }
}

/// The JSON text Python writes: `json.dumps(value) + '\n'`.
fn json_line(value: &Map<String, Value>) -> Vec<u8> {
    (tessel_core::pyfmt::json_dumps(&Value::Object(value.clone()), &tessel_core::pyfmt::JsonStyle::DEFAULT) + "\n").into_bytes()
}

/// `TilingConfig`: every change runs under the lock and rolls back on failure.
/// The gap between tiles when gaps are on and none was set before.
pub const DEFAULT_GAP: i64 = 10;

pub struct TilingConfig {
    /// Explicit rc.xml (tests); `None` = the running compositor's session config.
    pub path: Option<PathBuf>,
    pub state_dir: PathBuf,
    pub preferences: PathBuf,
    pub launcher: PathBuf,
    call: Call,
    reload: Option<Reload>,
    write: Writer,
    compositor_pid: Mutex<Option<Value>>,
}

pub fn default_call() -> Call { Arc::new(|command: &str| client::request(command)) }

impl Default for TilingConfig {
    fn default() -> Self { Self::new(None, None, None, None, None, None) }
}

impl TilingConfig {
    /// `state_dir` (rc.xml backups) and `config_dir` (config.json) default to
    /// `~/.local/state/tessel/tiling` and `~/.config/tessel/tiling`; `launcher` to
    /// the installed `tessel-tiling`.
    pub fn new(path: Option<PathBuf>, state_dir: Option<PathBuf>, config_dir: Option<PathBuf>, call: Option<Call>,
               reload: Option<Reload>, launcher: Option<PathBuf>) -> Self {
        Self {
            path,
            state_dir: state_dir.unwrap_or_else(|| tessel_core::paths::component_state_dir("tiling")),
            preferences: config_dir.unwrap_or_else(|| tessel_core::paths::component_config_dir("tiling")).join("config.json"),
            launcher: launcher.unwrap_or_else(|| tessel_core::paths::program("tessel-tiling")),
            call: call.unwrap_or_else(default_call),
            reload,
            write: Arc::new(compat::atomic_write),
            compositor_pid: Mutex::new(None),
        }
    }

    /// Replace `_atomic_write` (tests inject failures).
    pub fn with_writer(mut self, write: Writer) -> Self {
        self.write = write;
        self
    }

    pub fn call(&self) -> Call { self.call.clone() }

    fn call_command(&self, command: &str) -> Result<Reply> { Ok((self.call)(command)?) }

    fn write(&self, path: &Path, data: &[u8], mode: u32) -> Result<()> {
        (self.write)(path, data, mode).map_err(|e| io_error(e, path))
    }

    fn reload(&self) -> Result<()> {
        match &self.reload {
            Some(reload) => reload(),
            None => {
                let pid = self.compositor_pid.lock().unwrap().clone();
                reconfigure(pid.as_ref())
            }
        }
    }

    pub fn wanted(&self) -> Result<bool> { Ok(is_true(self.load_preferences()?.get("enabled"))) }

    pub fn load_preferences(&self) -> Result<Map<String, Value>> {
        let data = match fs::read(&self.preferences) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(io_error(error, &self.preferences)),
        };
        let text = String::from_utf8(data).map_err(|_| Error("'utf-8' codec can't decode the tiling preferences".into()))?;
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err("Invalid tiling preferences.".into()),
            Err(error) => Err(Error(error.to_string())),
        }
    }

    /// `set_outline(enabled)` → whether the compositor already supports it.
    pub fn set_outline(&self, enabled: bool) -> Result<bool> {
        let _lock = Lock::acquire(&self.state_dir)?;
        let state = self.call_command("status")?;
        let preferences = self.load_preferences()?;
        let supported = truthy(state.get("outline_supported"));
        if supported {
            self.call_command(if enabled { "outline on" } else { "outline off" })?;
        }
        let mut desired = preferences;
        desired.insert("focus_outline".into(), enabled.into());
        if let Err(error) = self.write(&self.preferences, &json_line(&desired), 0o600) {
            if supported {
                self.call_command(if truthy(state.get("outline_enabled")) { "outline on" } else { "outline off" })?;
            }
            return Err(error);
        }
        Ok(supported)
    }

    pub fn set_enabled(&self, enabled: bool, remember: bool) -> Result<Reply> {
        let _lock = Lock::acquire(&self.state_dir)?;
        self.apply(enabled, remember)
    }

    fn session_path(&self, state: &Reply) -> Result<(PathBuf, Option<PathBuf>)> {
        *self.compositor_pid.lock().unwrap() = state.get("compositor_pid").cloned();
        match &self.path {
            Some(path) => Ok((path.clone(), None)),
            None => {
                let pid = as_int(state.get("compositor_pid")).filter(|p| *p > 0).map(|p| p.to_string());
                session_config(pid.as_deref())
            }
        }
    }

    fn mode_of(path: &Path) -> Result<u32> {
        Ok(fs::metadata(path).map_err(|e| io_error(e, path))?.permissions().mode() & 0o7777)
    }

    /// Persist native gap=0; switching off restores the user's prior spacing.
    pub fn set_gapless(&self, enabled: bool) -> Result<()> {
        let _lock = Lock::acquire(&self.state_dir)?;
        let preferences = self.load_preferences()?;
        if is_true(preferences.get("gapless")) == enabled {
            return Ok(());
        }
        let (path, fallback) = self.session_path(&self.call_command("status")?)?;
        let existed = path.exists();
        let before = if existed { Some(read(&path)?) } else { None };
        let original = match (&before, &fallback) {
            (Some(data), _) => data.clone(),
            (None, Some(fallback)) => read(fallback)?,
            (None, None) => b"<labwc_config/>".to_vec(),
        };
        let mode = if existed { Self::mode_of(&path)? } else { 0o600 };
        // Turning gaps on with no spacing to go back to (the saved one was 0)
        // uses the default, so there is a gap to drag between tiles.
        let gap = if enabled { Some(0) } else {
            match preferences.get("gapped_spacing") {
                None => Some(DEFAULT_GAP),
                value => as_int(value).map(|gap| if gap == 0 { DEFAULT_GAP } else { gap }),
            }
        };
        let gap = gap.ok_or_else(|| Error("Invalid window gap.".into()))?;
        let (updated, previous_gap) = with_gap(&original, gap)?;
        let mut desired = preferences;
        desired.insert("gapless".into(), enabled.into());
        if enabled {
            desired.insert("gapped_spacing".into(), previous_gap.into());
        }
        let backup = self.state_dir.join(format!("gap-{}.xml", utc_stamp()));
        self.write(&backup, &original, 0o600)?;
        if current(&path)? != before {
            return Err("The labwc configuration changed. Try again.".into());
        }
        self.write(&path, &updated, mode)?;
        let result = self.reload().and_then(|_| self.write(&self.preferences, &json_line(&desired), 0o600));
        if let Err(error) = result {
            if read(&path)? == updated {
                match &before {
                    Some(before) => self.write(&path, before, mode)?,
                    None => fs::remove_file(&path).map_err(|e| io_error(e, &path))?,
                }
                self.reload()?;
            }
            return Err(error);
        }
        Ok(())
    }

    fn apply(&self, enabled: bool, remember: bool) -> Result<Reply> {
        let before_state = self.call_command("status")?;
        let (path, fallback) = self.session_path(&before_state)?;
        let mut original = if path.exists() {
            read(&path)?
        } else if let Some(fallback) = &fallback {
            read(fallback)?
        } else {
            b"<labwc_config/>".to_vec()
        };
        // Expand an empty root before appending managed sections.
        let text = String::from_utf8(original.clone()).map_err(|_| Error("'utf-8' codec can't decode rc.xml".into()))?;
        if xml::parse_str(&text)?.root.to_xml() == "<labwc_config/>" {
            original = b"<labwc_config>\n</labwc_config>\n".to_vec();
        }
        let existed = path.exists();
        let disk_before = if existed { Some(read(&path)?) } else { None };
        let mode = if existed { Self::mode_of(&path)? } else { 0o600 };
        let updated = edit_bindings(&original, enabled, &self.launcher)?;
        fs::create_dir_all(&self.state_dir).map_err(|e| io_error(e, &self.state_dir))?;
        let backup = self.state_dir.join(format!("rc-{}.xml", utc_stamp()));
        self.write(&backup, &original, 0o600)?;
        if current(&path)? != disk_before {
            return Err("The labwc configuration changed. Try again.".into());
        }
        self.write(&path, &updated, mode)?;
        let attempt = (|| -> Result<Reply> {
            self.reload()?;
            let result = self.call_command(if enabled { "enable" } else { "disable" })?;
            if !matches!(result.get("enabled"), Some(Value::Bool(b)) if *b == enabled) {
                return Err("The compositor did not confirm the tiling change.".into());
            }
            if remember {
                let mut preferences = self.load_preferences()?;
                preferences.insert("enabled".into(), enabled.into());
                self.write(&self.preferences, &json_line(&preferences), 0o600)?;
            }
            if truthy(result.get("outline_supported")) {
                // A palette refresh is retried by the existing status watcher.
                // It must not undo an otherwise successful tiling transaction.
                let _ = OutlineSync::new(Some(self.preferences.clone()), None, self.call.clone()).sync(&result);
            }
            Ok(result)
        })();
        match attempt {
            Ok(result) => Ok(result),
            Err(error) => {
                if read(&path)? == updated {
                    match &disk_before {
                        Some(before) => self.write(&path, before, mode)?,
                        None => fs::remove_file(&path).map_err(|e| io_error(e, &path))?,
                    }
                    self.reload()?;
                }
                self.call_command(if truthy(before_state.get("enabled")) { "enable" } else { "disable" })?;
                Err(error)
            }
        }
    }
}

fn current(path: &Path) -> Result<Option<Vec<u8>>> { if path.exists() { Ok(Some(read(path)?)) } else { Ok(None) } }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CONFIG: &[u8] = b"<labwc_config><keyboard><default/><!-- retained -->
<keybind key=\"W-l\"><action name=\"Execute\" command=\"existing-lock\"/></keybind>
</keyboard><mouse><default/></mouse><libinput><device category=\"non-touch\"><pointerSpeed>0.5</pointerSpeed></device></libinput></labwc_config>";

    fn launcher() -> &'static Path { Path::new("/tmp/tessel-tiling") }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool { haystack.windows(needle.len()).any(|w| w == needle) }

    fn reply(value: Value) -> Reply { value.as_object().unwrap().clone() }

    #[test]
    fn enable_disable_preserves_original_configuration() {
        let on = edit_bindings(CONFIG, true, launcher()).unwrap();
        assert!(contains(&on, b"command=\"layout\""));
        assert!(contains(&on, b"existing-lock"));
        assert_eq!(edit_bindings(&on, false, launcher()).unwrap(), CONFIG);
        assert_eq!(edit_bindings(&on, true, launcher()).unwrap(), on);
    }

    #[test]
    fn missing_and_self_closing_sections_restore_exactly() {
        for data in [&b"<labwc_config></labwc_config>"[..], b"<labwc_config><keyboard /><mouse/></labwc_config>"] {
            let on = edit_bindings(data, true, launcher()).unwrap();
            assert!(contains(&on, b"<default/>"));
            assert_eq!(edit_bindings(&on, true, launcher()).unwrap(), on);
            assert_eq!(edit_bindings(&on, false, launcher()).unwrap(), data);
        }
    }

    #[test]
    fn user_added_binding_in_created_section_is_kept() {
        let on = edit_bindings(b"<labwc_config></labwc_config>", true, launcher()).unwrap();
        let on = String::from_utf8(on).unwrap()
            .replace("</keyboard>", "<keybind key=\"W-z\"><action name=\"Close\"/></keybind></keyboard>");
        let off = edit_bindings(on.as_bytes(), false, launcher()).unwrap();
        assert!(contains(&off, b"key=\"W-z\""));
        assert!(!contains(&off, b"Tessel Tiling"));
    }

    #[test]
    fn rejected_documents() {
        for (data, message) in [
            (&b"<!DOCTYPE x><labwc_config/>"[..], "External XML entities are not supported."),
            (b"<openbox_config/>", "Expected labwc configuration."),
            (b"<labwc_config><!-- Tessel Tiling keyboard begin --></labwc_config>",
             "The tiling keybinding block was edited; repair its markers first."),
            (b"<labwc_config><keyboard/><keyboard/></labwc_config>", "Multiple input configuration sections are not supported."),
        ] {
            assert_eq!(edit_bindings(data, true, launcher()).unwrap_err().0, message);
        }
        assert!(edit_bindings(b"<labwc_config>", true, launcher()).unwrap_err().0.starts_with("no element found"));
        // Comments never count as sections; markers inside other text stay intact.
        let data = b"<labwc_config><!-- <keyboard/> --><keyboard><default/></keyboard></labwc_config>";
        let on = edit_bindings(data, true, launcher()).unwrap();
        assert!(on.starts_with(b"<labwc_config><!-- <keyboard/> --><keyboard><default/><!-- Tessel Tiling keyboard begin -->"));
        assert_eq!(edit_bindings(&on, false, launcher()).unwrap(), data);
    }

    struct Fake {
        active: Arc<Mutex<bool>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    fn fake(fail_enable: bool) -> (Call, Fake) {
        let active = Arc::new(Mutex::new(false));
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let (a, c) = (active.clone(), calls.clone());
        let call: Call = Arc::new(move |command: &str| {
            c.lock().unwrap().push(command.into());
            if command == "enable" && fail_enable {
                return Err(ClientError::Runtime("Not applied".into()));
            }
            if command != "status" {
                *a.lock().unwrap() = command == "enable";
            }
            Ok(reply(json!({"ready": true, "enabled": *a.lock().unwrap()})))
        });
        (call, Fake { active, calls })
    }

    fn noop_reload() -> Option<Reload> { Some(Arc::new(|| Ok(()))) }

    #[test]
    fn transaction_failure_restores_config_and_compositor_state() {
        let root = crate::testing::scratch("config-failure");
        let path = root.join("rc.xml");
        fs::write(&path, CONFIG).unwrap();
        let (call, state) = fake(true);
        let controller = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call),
                                           noop_reload(), Some(launcher().into()));
        assert_eq!(controller.set_enabled(true, true).unwrap_err().0, "Not applied");
        assert_eq!(fs::read(&path).unwrap(), CONFIG);
        assert!(!controller.preferences.exists());
        assert!(!*state.active.lock().unwrap());
        assert_eq!(*state.calls.lock().unwrap(), ["status", "enable", "disable"]);
    }

    #[test]
    fn saved_preference_and_reversible_overlay() {
        let root = crate::testing::scratch("config-saved");
        let path = root.join("rc.xml");
        fs::write(&path, CONFIG).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let (call, _state) = fake(false);
        let controller = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call),
                                           noop_reload(), Some(launcher().into()));
        controller.set_enabled(true, true).unwrap();
        assert!(controller.wanted().unwrap());
        assert!(has_bindings(&fs::read(&path).unwrap()));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640, "the file mode is kept");
        assert_eq!(fs::read_to_string(&controller.preferences).unwrap(), "{\"enabled\": true}\n");
        let backups: Vec<_> = fs::read_dir(&root).unwrap().filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("rc-")).collect();
        assert_eq!(backups.len(), 1);
        controller.set_enabled(false, true).unwrap();
        assert!(!controller.wanted().unwrap());
        assert_eq!(fs::read(&path).unwrap(), CONFIG);
        // remember=False (--restore) leaves the preference alone.
        controller.set_enabled(true, false).unwrap();
        assert!(!controller.wanted().unwrap());
    }

    #[test]
    fn missing_config_uses_expanded_default_and_is_removed_on_failure() {
        let root = crate::testing::scratch("config-missing");
        let path = root.join("labwc/rc.xml");
        let (call, _) = fake(false);
        let controller = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call),
                                           noop_reload(), Some(launcher().into()));
        controller.set_enabled(true, true).unwrap();
        let on = fs::read(&path).unwrap();
        assert!(on.starts_with(b"<labwc_config>\n<!-- Tessel Tiling keyboard section "));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        controller.set_enabled(false, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"<labwc_config>\n</labwc_config>\n");
        fs::remove_file(&path).unwrap();
        let (call, _) = fake(true);
        let failing = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call),
                                        noop_reload(), Some(launcher().into()));
        assert!(failing.set_enabled(true, true).is_err());
        assert!(!path.exists(), "a file that did not exist is removed again");
    }

    #[test]
    fn concurrent_edit_is_refused() {
        let root = crate::testing::scratch("config-race");
        let path = root.join("rc.xml");
        fs::write(&path, CONFIG).unwrap();
        let target = path.clone();
        let writer: Writer = Arc::new(move |p: &Path, data: &[u8], mode: u32| {
            if p.file_name().unwrap().to_string_lossy().starts_with("rc-") {
                fs::write(&target, b"<labwc_config/>")?; // someone else saves rc.xml meanwhile
            }
            compat::atomic_write(p, data, mode)
        });
        let (call, _) = fake(false);
        let controller = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call),
                                           noop_reload(), Some(launcher().into())).with_writer(writer);
        assert_eq!(controller.set_enabled(true, true).unwrap_err().0, "The labwc configuration changed. Try again.");
        assert_eq!(fs::read(&path).unwrap(), b"<labwc_config/>");
    }

    #[test]
    fn unconfirmed_change_rolls_back() {
        let root = crate::testing::scratch("config-unconfirmed");
        let path = root.join("rc.xml");
        fs::write(&path, CONFIG).unwrap();
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = calls.clone();
        let call: Call = Arc::new(move |command: &str| {
            log.lock().unwrap().push(command.into());
            Ok(reply(json!({"ready": true, "enabled": false})))
        });
        let reloads = Arc::new(Mutex::new(0));
        let counter = reloads.clone();
        let reload: Reload = Arc::new(move || {
            *counter.lock().unwrap() += 1;
            Ok(())
        });
        let controller = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call),
                                           Some(reload), Some(launcher().into()));
        assert_eq!(controller.set_enabled(true, true).unwrap_err().0, "The compositor did not confirm the tiling change.");
        assert_eq!(fs::read(&path).unwrap(), CONFIG);
        assert_eq!(*reloads.lock().unwrap(), 2);
        assert_eq!(*calls.lock().unwrap(), ["status", "enable", "disable"]);
    }

    // ---- gapless (test_gaps.py)

    #[test]
    fn persistence_restores_custom_spacing_and_keeps_other_preferences() {
        let root = crate::testing::scratch("gap-persist");
        let path = root.join("rc.xml");
        fs::write(&path, b"<labwc_config><core gap=\"17\"/></labwc_config>").unwrap();
        let reloads = Arc::new(Mutex::new(0));
        let counter = reloads.clone();
        let reload: Reload = Arc::new(move || {
            *counter.lock().unwrap() += 1;
            Ok(())
        });
        let call: Call = Arc::new(|_: &str| Ok(reply(json!({"compositor_pid": 42}))));
        let config = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call.clone()),
                                       Some(reload.clone()), None);
        fs::create_dir_all(config.preferences.parent().unwrap()).unwrap();
        fs::write(&config.preferences, r#"{"enabled":true,"focus_outline":false}"#).unwrap();
        config.set_gapless(true).unwrap();
        assert_eq!(with_gap(&fs::read(&path).unwrap(), 0).unwrap().1, 0);
        assert_eq!(Value::Object(config.load_preferences().unwrap()),
                   json!({"enabled": true, "focus_outline": false, "gapless": true, "gapped_spacing": 17}));
        assert_eq!(fs::read_to_string(&config.preferences).unwrap(),
                   "{\"enabled\": true, \"focus_outline\": false, \"gapless\": true, \"gapped_spacing\": 17}\n");
        config.set_gapless(true).unwrap();
        assert_eq!(*reloads.lock().unwrap(), 1);
        let next = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call), Some(reload), None);
        next.set_gapless(false).unwrap();
        assert_eq!(with_gap(&fs::read(&path).unwrap(), 0).unwrap().1, 17);
        assert_eq!(next.load_preferences().unwrap()["gapless"], json!(false));
    }

    #[test]
    fn reload_or_preference_save_failure_rolls_back() {
        for failure in ["reload", "preferences"] {
            let root = crate::testing::scratch(&format!("gap-fail-{failure}"));
            let path = root.join("rc.xml");
            let original = b"<labwc_config><core><gap>13</gap></core></labwc_config>";
            fs::write(&path, original).unwrap();
            let reloads = Arc::new(Mutex::new(0));
            let counter = reloads.clone();
            let reload: Reload = Arc::new(move || {
                let mut count = counter.lock().unwrap();
                *count += 1;
                if failure == "reload" && *count == 1 {
                    return Err("Reload failed".into());
                }
                Ok(())
            });
            let preferences = root.join("config.json");
            let writer: Writer = Arc::new(move |target: &Path, data: &[u8], mode: u32| {
                if failure == "preferences" && target == preferences {
                    return Err(io::Error::other("Disk full"));
                }
                compat::atomic_write(target, data, mode)
            });
            let call: Call = Arc::new(|_: &str| Ok(Map::new()));
            let config = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call),
                                           Some(reload), None).with_writer(writer);
            assert!(config.set_gapless(true).is_err());
            assert_eq!(fs::read(&path).unwrap(), original, "{failure}");
            assert!(!config.preferences.exists());
        }
    }

    #[test]
    fn turning_gaps_on_after_a_zero_gap_uses_the_default() {
        let root = crate::testing::scratch("gap-zero");
        let path = root.join("rc.xml");
        fs::write(&path, b"<labwc_config><core><gap>0</gap></core></labwc_config>").unwrap();
        let call: Call = Arc::new(|_: &str| Ok(reply(json!({"compositor_pid": 42}))));
        let config = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call), noop_reload(), None);
        config.set_gapless(true).unwrap();
        assert_eq!(config.load_preferences().unwrap()["gapped_spacing"], json!(0));
        config.set_gapless(false).unwrap();
        assert_eq!(with_gap(&fs::read(&path).unwrap(), 0).unwrap().1, DEFAULT_GAP);
    }

    #[test]
    fn invalid_saved_spacing_is_rejected() {
        let root = crate::testing::scratch("gap-invalid");
        let path = root.join("rc.xml");
        fs::write(&path, b"<labwc_config/>").unwrap();
        let call: Call = Arc::new(|_: &str| Ok(Map::new()));
        let config = TilingConfig::new(Some(path.clone()), Some(root.clone()), Some(root.clone()), Some(call), noop_reload(), None);
        fs::create_dir_all(config.preferences.parent().unwrap()).unwrap();
        for value in ["-1", "true", "1.5", "\"0\""] {
            fs::write(&config.preferences, format!("{{\"gapless\": true, \"gapped_spacing\": {value}}}")).unwrap();
            assert_eq!(config.set_gapless(false).unwrap_err().0, "Invalid window gap.", "{value}");
        }
        assert_eq!(fs::read(&path).unwrap(), b"<labwc_config/>");
    }

    // ---- outline toggle (test_outline.py)

    #[test]
    fn toggle_preserves_other_settings_and_supports_pending_compositor_update() {
        for supported in [false, true] {
            let root = crate::testing::scratch(&format!("outline-toggle-{supported}"));
            let calls: Arc<Mutex<Vec<String>>> = Arc::default();
            let log = calls.clone();
            let call: Call = Arc::new(move |command: &str| {
                log.lock().unwrap().push(command.into());
                Ok(reply(json!({"outline_supported": supported, "outline_enabled": true})))
            });
            let config = TilingConfig::new(None, Some(root.clone()), Some(root.clone()), Some(call), None, None);
            fs::create_dir_all(config.preferences.parent().unwrap()).unwrap();
            fs::write(&config.preferences, r#"{"enabled":true,"other":"retained"}"#).unwrap();
            assert_eq!(config.set_outline(false).unwrap(), supported);
            assert_eq!(serde_json::from_str::<Value>(&fs::read_to_string(&config.preferences).unwrap()).unwrap(),
                       json!({"enabled": true, "other": "retained", "focus_outline": false}));
            let expected: Vec<&str> = if supported { vec!["status", "outline off"] } else { vec!["status"] };
            assert_eq!(*calls.lock().unwrap(), expected);
        }
    }

    #[test]
    fn failed_preference_write_restores_native_outline() {
        let root = crate::testing::scratch("outline-write");
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = calls.clone();
        let call: Call = Arc::new(move |command: &str| {
            log.lock().unwrap().push(command.into());
            Ok(reply(json!({"outline_supported": true, "outline_enabled": true})))
        });
        let writer: Writer = Arc::new(|_: &Path, _: &[u8], _: u32| Err(io::Error::from_raw_os_error(libc::ENOSPC)));
        let config = TilingConfig::new(None, Some(root.clone()), Some(root.clone()), Some(call), None, None).with_writer(writer);
        assert!(config.set_outline(false).unwrap_err().0.starts_with("[Errno 28] No space left on device"));
        assert_eq!(*calls.lock().unwrap(), ["status", "outline off", "outline on"]);
    }

    #[test]
    fn invalid_preferences_raise() {
        let root = crate::testing::scratch("prefs-invalid");
        let config = TilingConfig::new(None, Some(root.clone()), Some(root.clone()), None, None, None);
        assert!(config.load_preferences().unwrap().is_empty());
        fs::create_dir_all(config.preferences.parent().unwrap()).unwrap();
        fs::write(&config.preferences, "[]").unwrap();
        assert_eq!(config.load_preferences().unwrap_err().0, "Invalid tiling preferences.");
        fs::write(&config.preferences, "{").unwrap();
        assert!(config.wanted().is_err());
    }

    #[test]
    fn session_config_follows_the_compositor_command_line() {
        assert_eq!(session_config(Some("")).unwrap_err().0, "Workspace settings require a running labwc session.");
        assert_eq!(session_config(Some("12a")).unwrap_err().0, "Workspace settings require a running labwc session.");
        // This test process is not labwc.
        let own = std::process::id().to_string();
        assert_eq!(session_config(Some(&own)).unwrap_err().0, "The labwc session could not be identified.");
    }

    #[test]
    fn reconfigure_reports_python_messages() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 3"]);
        assert_eq!(run_checked(command, "['labwc', '--reconfigure']", 5.0).unwrap_err().0,
                   "Command '['labwc', '--reconfigure']' returned non-zero exit status 3.");
        let mut command = std::process::Command::new("sleep");
        command.arg("5");
        assert_eq!(run_checked(command, "['labwc', '--reconfigure']", 0.2).unwrap_err().0,
                   "Command '['labwc', '--reconfigure']' timed out after 0.2 seconds");
        let command = std::process::Command::new("/nonexistent/labwc");
        assert_eq!(run_checked(command, "x", 1.0).unwrap_err().0,
                   "[Errno 2] No such file or directory: '/nonexistent/labwc'");
    }
}
