//! workspace_config.py: persistent labwc workspace count, without rewriting
//! unrelated configuration.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tessel_tiling::xml::{Document, Element, Node};

use crate::compat::{self, element_indices, local, local_lower, os_error, read_file, tag_end, value, Error, Result};

/// UI limit, not a compositor limit.
pub const MAX_WORKSPACES: i64 = 100;
const EMPTY: &[u8] = b"<?xml version=\"1.0\"?>\n<labwc_config>\n</labwc_config>\n";

/// One parsed document with its single depth-1 `section` element (if any):
/// `(doc, section index in root.children, span, root_end)`.
pub(crate) struct Parsed {
    pub doc: Document,
    pub section: Option<usize>,
    pub span: Option<(usize, usize)>,
    pub root_end: usize,
}

/// `_parse(data)` of workspace_config / input_config.
pub(crate) fn parse_section(data: &[u8], name: &str, doctype: &str, wrong: &str) -> Result<Parsed> {
    if data.len() > 2 * 1024 * 1024 {
        return value("The labwc configuration is too large.");
    }
    if std::str::from_utf8(data).is_ok() {
        compat::check_declaration(data)?;
    }
    let mut doc = compat::parse_xml(data, doctype)?;
    let root = &doc.root;
    let root_end = compat::end_event(root);
    let found = element_indices(root, name);
    let span = found.first().map(|i| {
        let e = compat::child_element(root, *i);
        (e.start, e.end)
    });
    if local(&root.name) != "labwc_config" || found.len() > 1 {
        return value(wrong);
    }
    compat::minidom_order(&mut doc.root);
    Ok(Parsed { section: found.first().copied(), doc, span, root_end })
}

fn parse(data: &[u8]) -> Result<Parsed> {
    parse_section(data, "desktops", "XML document types are not supported in workspace settings.",
                  "Expected one labwc_config with at most one desktops section.")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceState {
    pub count: i64,
    pub signature: String,
}

pub fn read_state(data: &[u8]) -> Result<WorkspaceState> {
    let parsed = parse(data)?;
    let Some(index) = parsed.section else { return Ok(WorkspaceState { count: 1, signature: String::new() }) };
    let desktop = compat::child_element(&parsed.doc.root, index);
    let attribute = desktop.attribute("number").filter(|v| !v.is_empty()).unwrap_or("1").to_owned();
    let mut values = vec![attribute];
    values.extend(element_indices(desktop, "number").into_iter().map(|i| compat::text(compat::child_element(desktop, i))));
    // labwc's integer setting can be expressed as an attribute or a child.
    let number = compat::py_int(values.last().unwrap())?.max(1);
    let names: usize = element_indices(desktop, "names").into_iter()
        .map(|g| element_indices(compat::child_element(desktop, g), "name").len()).sum();
    Ok(WorkspaceState { count: number.max(names as i64), signature: desktop.to_xml() })
}

fn is_digit_string(text: &str) -> bool { !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) }

pub fn with_count(data: &[u8], count: i64) -> Result<Vec<u8>> {
    let current = read_state(data)?;
    let maximum = MAX_WORKSPACES.max(current.count);
    if !(1..=maximum).contains(&count) {
        return value(format!("Choose between 1 and {maximum} workspaces."));
    }
    let parsed = parse(data)?;
    let mut desktop: Element = match parsed.section {
        Some(index) => compat::child_element(&parsed.doc.root, index).clone(),
        None => compat::new_element("desktops"),
    };
    compat::set_attribute(&mut desktop, "number", &count.to_string());
    desktop.children.retain(|n| !matches!(n, Node::Element(e) if local_lower(&e.name) == "number"));
    let groups = element_indices(&desktop, "names");
    let names: Vec<(usize, usize)> = groups.iter()
        .flat_map(|g| element_indices(compat::child_element(&desktop, *g), "name").into_iter().map(move |c| (*g, c)))
        .collect();
    let old_names: Vec<String> = names.iter()
        .map(|(g, c)| compat::text(compat::child_element(compat::child_element(&desktop, *g), *c))).collect();
    for (g, c) in names.iter().skip(count as usize).rev() {
        compat::child_element_mut(&mut desktop, *g).children.remove(*c);
    }
    // Preserve the numbered names used by this desktop instead of mixing
    // "1, 2, 3, 4" with "Workspace 5" when adding another workspace.
    let numbered: Vec<String> = (1..=old_names.len()).map(|i| i.to_string()).collect();
    if !old_names.is_empty() && old_names == numbered {
        let last = *groups.last().unwrap();
        for i in names.len() as i64..count {
            let group = compat::child_element_mut(&mut desktop, last);
            group.children.push(Node::Text("\n    ".into()));
            group.children.push(Node::Element(compat::text_element("name", &(i + 1).to_string())));
            group.self_closing = false;
        }
    }
    let remaining: Vec<String> = groups.iter()
        .flat_map(|g| {
            let group = compat::child_element(&desktop, *g);
            element_indices(group, "name").into_iter().map(|c| compat::text(compat::child_element(group, c))).collect::<Vec<_>>()
        })
        .collect();
    let removed: Vec<String> = old_names.iter().skip(count as usize).cloned().collect();
    for index in element_indices(&desktop, "initial") {
        let initial = compat::child_element_mut(&mut desktop, index);
        let current = compat::text(initial);
        // Keep a valid name even if that name happens to be numeric.
        let invalid_number = is_digit_string(&current) && !compat::py_int(&current).map(|v| (1..=count).contains(&v)).unwrap_or(false);
        if !remaining.contains(&current) && (removed.contains(&current) || invalid_number) {
            initial.children.clear();
            initial.children.push(Node::Text(remaining.first().cloned().unwrap_or_else(|| "1".into())));
        }
    }
    let replacement = desktop.to_xml().into_bytes();
    if let Some((begin, end)) = parsed.span {
        return Ok([&data[..begin], &replacement[..], &data[end..]].concat());
    }
    // Expand an empty root without discarding its attributes or namespace.
    let Some(at) = data.windows(13).position(|w| w == b"<labwc_config") else {
        return value("subsection not found");
    };
    let opening = tag_end(data, at)?;
    if compat::bytes_rstrip(&data[..opening]).ends_with(b"/>") {
        return Ok([&data[..opening - 2], b">\n  ", &replacement[..], b"\n</labwc_config>", &data[opening..]].concat());
    }
    let end = parsed.root_end;
    Ok([&data[..end], b"  ", &replacement[..], b"\n", &data[end..]].concat())
}

/// `session_config()`: the running labwc's rc.xml and the system fallback.
pub fn session_config() -> Result<(PathBuf, Option<PathBuf>)> {
    session_config_at(Path::new("/proc"), &std::env::var("LABWC_PID").unwrap_or_default())
}

/// `session_config` against a fake procfs (tests).
pub fn session_config_at(proc: &Path, pid: &str) -> Result<(PathBuf, Option<PathBuf>)> {
    if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
        return compat::runtime("Workspace settings require a running labwc session.");
    }
    let process = proc.join(pid);
    let cmdline = read_file(&process.join("cmdline"))?;
    let cmdline = String::from_utf8(cmdline).map_err(|_| Error::Value("'utf-8' codec can't decode the command line".into()))?;
    let args: Vec<&str> = cmdline.trim_end_matches('\0').split('\0').collect();
    // An RPM upgrade unlinks the old binary while the current compositor
    // continues running it. procfs adds this suffix to the executable link.
    let exe = process.join("exe");
    let executable = std::fs::read_link(&exe).map_err(|e| os_error(&e, &exe))?;
    let executable = executable.to_string_lossy();
    let executable = executable.strip_suffix(" (deleted)").unwrap_or(&executable);
    if Path::new(executable).file_name().map(|n| n != "labwc").unwrap_or(true) {
        return compat::runtime("The labwc session could not be identified.");
    }
    let (mut file, mut dir): (Option<String>, Option<String>) = (None, None);
    let mut index = 1;
    while index < args.len() {
        let argument = args[index];
        if argument == "-m" || argument == "--merge-config" {
            return compat::runtime("Workspace count editing is unavailable with merged labwc configurations.");
        }
        if ["-c", "--config", "-C", "--config-dir"].contains(&argument) {
            index += 1;
            let item = args.get(index).ok_or_else(|| Error::Value("list index out of range".into()))?.to_string();
            if argument == "-c" || argument == "--config" { file = Some(item) } else { dir = Some(item) }
        } else if argument.starts_with("--config=") || argument.starts_with("--config-dir=") {
            let (key, item) = argument.split_once('=').unwrap();
            if key == "--config" { file = Some(item.into()) } else { dir = Some(item.into()) }
        } else if (argument.starts_with("-c") || argument.starts_with("-C")) && argument.len() > 2 {
            if argument.starts_with("-c") { file = Some(argument[2..].into()) } else { dir = Some(argument[2..].into()) }
        }
        index += 1;
    }
    let options = file.is_some() || dir.is_some();
    let mut target = match (&file, &dir) {
        (Some(file), _) => PathBuf::from(file),
        (None, Some(dir)) => Path::new(dir).join("rc.xml"),
        _ => crate::labwc_settings::config_home().join("labwc/rc.xml"),
    };
    if !target.is_absolute() {
        let cwd = process.join("cwd");
        target = std::fs::read_link(&cwd).map_err(|e| os_error(&e, &cwd))?.join(target);
    }
    let mut fallback = None;
    if !options {
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

/// `reload()`: blocking, may run on a worker thread.
pub type Reload = Arc<dyn Fn() -> std::result::Result<(), String> + Send + Sync>;

/// `subprocess.run(['labwc', '--reconfigure'], check=True, capture_output=True, timeout=5)`.
pub fn labwc_reconfigure() -> std::result::Result<(), String> {
    let mut command = std::process::Command::new("labwc");
    command.arg("--reconfigure").stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    tessel_tiling::config::run_checked(command, "['labwc', '--reconfigure']", 5.0).map_err(|e| e.0)
}

pub fn default_reload() -> Reload { Arc::new(labwc_reconfigure) }

#[derive(Clone)]
pub struct WorkspaceConfig {
    pub path: PathBuf,
    pub fallback: Option<PathBuf>,
    pub backup_dir: PathBuf,
    pub reload: Reload,
}

impl WorkspaceConfig {
    /// `WorkspaceConfig(path=None, *, fallback=None, reload=None, backup_dir=None)`.
    pub fn new(path: Option<PathBuf>, fallback: Option<PathBuf>, reload: Option<Reload>,
               backup_dir: Option<PathBuf>) -> Result<WorkspaceConfig> {
        let (path, fallback) = match path {
            Some(path) => (path, fallback),
            None => session_config()?,
        };
        Ok(Self::with_backup(path, fallback, reload,
                             backup_dir.unwrap_or_else(|| crate::labwc_settings::state_home().join("tessel/backups/workspaces"))))
    }

    pub(crate) fn with_backup(path: PathBuf, fallback: Option<PathBuf>, reload: Option<Reload>, backup_dir: PathBuf) -> Self {
        WorkspaceConfig { path: compat::resolve(&path), fallback, backup_dir, reload: reload.unwrap_or_else(default_reload) }
    }

    /// The running session's configuration.
    pub fn session() -> Result<WorkspaceConfig> { Self::new(None, None, None, None) }

    /// `WorkspaceConfig(path, backup_dir=…, reload=…)` for tests and embedding.
    pub fn at(path: impl Into<PathBuf>, backup_dir: impl Into<PathBuf>, reload: Reload) -> WorkspaceConfig {
        Self::with_backup(path.into(), None, Some(reload), backup_dir.into())
    }

    pub(crate) fn read(&self) -> Result<Vec<u8>> {
        if self.path.exists() {
            return read_file(&self.path);
        }
        if let Some(fallback) = &self.fallback {
            return read_file(fallback);
        }
        Ok(EMPTY.to_vec())
    }

    pub fn load(&self) -> Result<WorkspaceState> { read_state(&self.read()?) }

    /// Write `updated` over `original`, back up, reload, restore on failure.
    pub(crate) fn commit(&self, original: &[u8], updated: &[u8], restored: &str) -> Result<()> {
        let existed = self.path.exists();
        let mode = if existed { compat::file_mode(&self.path).unwrap_or(0o600) } else { 0o600 };
        std::fs::create_dir_all(&self.backup_dir).map_err(|e| os_error(&e, &self.backup_dir))?;
        compat::atomic_write_mode(&self.backup_dir.join(compat::utc_stamp() + "-rc.xml"), original, mode)?;
        if self.read()? != original {
            return compat::runtime("The labwc configuration changed while saving. Please try again.");
        }
        compat::atomic_write_mode(&self.path, updated, mode)?;
        if (self.reload)().is_err() {
            // Never overwrite an edit made by another application during reload.
            if read_file(&self.path)? == updated {
                if existed {
                    compat::atomic_write_mode(&self.path, original, mode)?;
                } else {
                    std::fs::remove_file(&self.path).map_err(|e| os_error(&e, &self.path))?;
                }
                let _ = (self.reload)();
                return compat::runtime(restored);
            }
            return compat::runtime("Could not reload labwc, and its configuration changed elsewhere.");
        }
        Ok(())
    }

    pub fn save(&self, count: i64, expected: &WorkspaceState) -> Result<WorkspaceState> {
        let original = self.read()?;
        let state = read_state(&original)?;
        if state.signature != expected.signature {
            return compat::runtime("Workspace settings changed elsewhere. Reset this page before applying.");
        }
        let updated = with_count(&original, count)?;
        if count == state.count {
            return Ok(state);
        }
        self.commit(&original, &updated, "Could not reload labwc; the previous configuration was restored.")?;
        read_state(&updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;
    use std::sync::Mutex;

    const CONFIG: &[u8] = br#"<?xml version="1.0"?>
<!-- Preserve all unrelated formatting. -->
<labwc_config>
  <keyboard><keybind key='W-r'><action name='Execute' command='example &amp; test'/></keybind></keyboard>
  <desktops popupTime="500"><names><name>1</name><name>2</name><name>3</name><name>4</name></names></desktops>
  <theme><name>GTK</name></theme>
</labwc_config>
"#;

    fn split<'a>(data: &'a [u8], marker: &[u8], after: bool) -> &'a [u8] {
        let at = data.windows(marker.len()).position(|w| w == marker).unwrap();
        if after { &data[at + marker.len()..] } else { &data[..at] }
    }

    fn contains(data: &[u8], needle: &str) -> bool { data.windows(needle.len()).any(|w| w == needle.as_bytes()) }

    #[test]
    fn session_detection_survives_compositor_package_update() {
        let proc = scratch("proc");
        for executable in ["/usr/bin/labwc", "/usr/bin/labwc (deleted)"] {
            let process = proc.join("123");
            let _ = std::fs::remove_dir_all(&process);
            std::fs::create_dir_all(&process).unwrap();
            std::fs::write(process.join("cmdline"), b"labwc\0--config\0/tmp/session.xml\0").unwrap();
            std::os::unix::fs::symlink(executable, process.join("exe")).unwrap();
            assert_eq!(session_config_at(&proc, "123").unwrap(), (PathBuf::from("/tmp/session.xml"), None));
        }
        let process = proc.join("124");
        std::fs::create_dir_all(&process).unwrap();
        std::fs::write(process.join("cmdline"), b"other\0").unwrap();
        std::os::unix::fs::symlink("/usr/bin/other (deleted)", process.join("exe")).unwrap();
        let error = session_config_at(&proc, "124").unwrap_err();
        assert!(error.is_runtime() && error.message().contains("could not be identified"));
        assert!(session_config_at(&proc, "").unwrap_err().message().contains("require a running labwc session"));
        let process = proc.join("125");
        std::fs::create_dir_all(&process).unwrap();
        std::fs::write(process.join("cmdline"), b"labwc\0-m\0").unwrap();
        std::os::unix::fs::symlink("/usr/bin/labwc", process.join("exe")).unwrap();
        assert!(session_config_at(&proc, "125").unwrap_err().message().contains("merged"));
        let process = proc.join("126");
        std::fs::create_dir_all(&process).unwrap();
        std::fs::write(process.join("cmdline"), b"labwc\0-Cconf\0").unwrap();
        std::os::unix::fs::symlink("/usr/bin/labwc", process.join("exe")).unwrap();
        std::os::unix::fs::symlink("/home/x", process.join("cwd")).unwrap();
        assert_eq!(session_config_at(&proc, "126").unwrap().0, PathBuf::from("/home/x/conf/rc.xml"));
    }

    #[test]
    fn named_workspaces_increase_and_decrease_preserve_other_xml() {
        for count in [1, 2, 6, MAX_WORKSPACES] {
            let updated = with_count(CONFIG, count).unwrap();
            assert_eq!(read_state(&updated).unwrap().count, count);
            assert_eq!(split(&updated, b"<desktops", false), split(CONFIG, b"<desktops", false));
            assert_eq!(split(&updated, b"</desktops>", true), split(CONFIG, b"</desktops>", true));
            assert!(contains(&updated, "popupTime=\"500\""));
            assert!(contains(&updated, &format!("<name>{count}</name>")));
            assert!(!contains(&updated, &format!("<name>{}</name>", count + 1)));
        }
    }

    #[test]
    fn minimum_number_defaults_empty_root_namespace_and_initial() {
        for data in [&b"<labwc_config/>"[..], b"<labwc_config><desktops/></labwc_config>",
                     b"<labwc_config><desktops number=\"4\"><number>6</number></desktops></labwc_config>",
                     b"<labwc_config xmlns=\"http://openbox.org/3.4/rc\"><desktops number=\"8\"/></labwc_config>"] {
            assert_eq!(read_state(&with_count(data, 2).unwrap()).unwrap().count, 2, "{}", String::from_utf8_lossy(data));
        }
        assert_eq!(with_count(b"<labwc_config/>", 2).unwrap(), b"<labwc_config>\n  <desktops number=\"2\"/>\n</labwc_config>");
        let data = b"<labwc_config><desktops number=\"8\"><names><name>Work</name><name>Games</name></names><initial>Games</initial><prefix>Desk</prefix></desktops></labwc_config>";
        let updated = with_count(data, 1).unwrap();
        assert!(contains(&updated, "<name>Work</name>"));
        assert!(contains(&updated, "<initial>Work</initial>"));
        assert!(contains(&updated, "<prefix>Desk</prefix>"));
    }

    #[test]
    fn invalid_counts_and_xml_are_rejected() {
        for count in [0, -1, 101] {
            assert!(with_count(CONFIG, count).unwrap_err().is_value());
        }
        for data in [&b"<!DOCTYPE labwc_config><labwc_config/>"[..], b"<oops/>", b"<labwc_config><desktops/><desktops/></labwc_config>"] {
            assert!(with_count(data, 3).unwrap_err().is_value());
        }
        // Existing manually configured counts above the UI default remain valid.
        let data = b"<labwc_config><desktops number=\"120\"/></labwc_config>";
        assert_eq!(read_state(&with_count(data, 110).unwrap()).unwrap().count, 110);
        assert!(with_count(b"<?xml version='1.0' encoding='latin-1'?><labwc_config/>", 2).unwrap_err().message().contains("UTF-8"));
    }

    #[test]
    fn save_merges_other_edits_backs_up_reloads_and_keeps_permissions() {
        let folder = scratch("ws-save");
        let path = folder.join("rc.xml");
        std::fs::write(&path, CONFIG).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let counter = calls.clone();
        let config = WorkspaceConfig::at(&path, folder.join("backup"), Arc::new(move || {
            *counter.lock().unwrap() += 1;
            Ok(())
        }));
        let state = config.load().unwrap();
        let changed = String::from_utf8_lossy(CONFIG).replace("<name>GTK</name>", "<name>Another Theme</name>");
        std::fs::write(&path, &changed).unwrap();
        config.save(6, &state).unwrap();
        assert_eq!(config.load().unwrap().count, 6);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(contains(&std::fs::read(&path).unwrap(), "Another Theme"));
        assert_eq!(compat::file_mode(&path), Some(0o640));
        let backup = std::fs::read_dir(&config.backup_dir).unwrap().next().unwrap().unwrap().path();
        assert_eq!(std::fs::read_to_string(backup).unwrap(), changed);
        let error = config.save(2, &state).unwrap_err();
        assert!(error.is_runtime() && error.message().contains("changed elsewhere"));
    }

    #[test]
    fn failed_reload_restores_config_and_no_change_does_not_reload() {
        let folder = scratch("ws-fail");
        let path = folder.join("rc.xml");
        std::fs::write(&path, CONFIG).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let counter = calls.clone();
        let config = WorkspaceConfig::at(&path, folder.join("backup"), Arc::new(move || {
            *counter.lock().unwrap() += 1;
            Err("fixture".into())
        }));
        let state = config.load().unwrap();
        config.save(4, &state).unwrap();
        assert_eq!(*calls.lock().unwrap(), 0);
        assert!(config.save(2, &state).unwrap_err().message().contains("restored"));
        assert_eq!(std::fs::read(&path).unwrap(), CONFIG);
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[test]
    fn missing_file_uses_fallback_then_empty_default() {
        let folder = scratch("ws-missing");
        let fallback = folder.join("system.xml");
        std::fs::write(&fallback, b"<labwc_config><desktops number=\"3\"/></labwc_config>").unwrap();
        let mut config = WorkspaceConfig::at(folder.join("rc.xml"), folder.join("b"), Arc::new(|| Ok(())));
        assert_eq!(config.load().unwrap().count, 1);
        config.fallback = Some(fallback);
        assert_eq!(config.load().unwrap().count, 3);
        let state = config.load().unwrap();
        config.save(5, &state).unwrap();
        assert_eq!(read_state(&std::fs::read(folder.join("rc.xml")).unwrap()).unwrap().count, 5);
        assert_eq!(compat::file_mode(&folder.join("rc.xml")), Some(0o600));
    }
}
