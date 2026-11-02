//! Owned labwc bindings, preserving unrelated keys and rejecting conflicts
//! (capture_shortcuts.py). rc.xml is edited in place: everything except our
//! own `<keybind>` elements is byte-preserved.

use crate::compat::xkb;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tessel_core::tr;

pub const KEYS: [&str; 5] = ["record", "screen", "area", "window", "workspace"];

/// Ordered `{key: shortcut}` (DEFAULTS order).
pub type Values = Vec<(&'static str, String)>;

pub fn default(key: &str) -> &'static str {
    match key {
        "record" => "A-F9",
        "screen" => "Print",
        "area" => "C-Print",
        "window" => "S-Print",
        _ => "A-Print",
    }
}

pub fn defaults() -> Values {
    KEYS.iter().map(|k| (*k, default(k).to_string())).collect()
}

pub fn title(key: &str) -> &'static str {
    match key {
        "record" => "Start / stop recording",
        "screen" => "Current screen",
        "workspace" => "Current workspace",
        "window" => "Current window",
        _ => "Custom area",
    }
}

/// The installed `tessel-capture` (`tessel_core::paths::program`), shell-quoted.
pub fn program_text() -> String {
    tessel_core::shell::shell_quote(&tessel_core::paths::program("tessel-capture").to_string_lossy())
}

pub fn command(key: &str) -> String {
    if key == "record" {
        format!("{} --toggle-recording", program_text())
    } else {
        format!("{} --capture {key}", program_text())
    }
}

/// One of our bindings: `tessel-capture` (any install location, also the
/// pre-rename `waycapture`) with a screenshot, recording or capture argument.
fn owned(command_text: &str) -> bool {
    let Some((program, arguments)) = command_text.split_once(' ') else { return false };
    let name = Path::new(program.trim_matches('\'')).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name != "tessel-capture" && name != "waycapture" {
        return false;
    }
    arguments == "--screenshot" || arguments == "--toggle-recording"
        || KEYS.iter().any(|k| *k != "record" && arguments == format!("--capture {k}"))
}

pub fn config_root() -> PathBuf {
    tessel_core::paths::config_home()
}

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Canonical labwc key: `C-A-S-W-<keysym>` (raises the Python ValueError texts).
pub fn normalize(value: &str) -> Result<String, String> {
    let value = value.trim_matches(is_py_space);
    if value.is_empty() {
        return Ok(String::new());
    }
    let parts: Vec<&str> = value.split(['+', '-']).collect();
    let mut modifiers = std::collections::HashSet::new();
    for part in &parts[..parts.len() - 1] {
        let alias = match part.to_lowercase().as_str() {
            "ctrl" | "control" | "c" => "C",
            "alt" | "a" => "A",
            "shift" | "s" => "S",
            "super" | "win" | "w" => "W",
            _ => return Err(tr!("Use Ctrl, Alt, Shift or Super plus a key, for example Alt+F9.")),
        };
        modifiers.insert(alias);
    }
    let mut key = parts[parts.len() - 1].to_string();
    if matches!(key.to_lowercase().as_str(), "printscreen" | "print screen") {
        key = "Print".into();
    }
    let sym = xkb::keysym_from_name(&key)?;
    let lower = key.to_lowercase();
    if sym == 0 || ["control_l", "control_r", "alt_l", "alt_r", "shift_l", "shift_r", "super_l", "super_r"].contains(&lower.as_str()) {
        return Err(tr!("Unknown shortcut key: {key}", key = key.as_str()));
    }
    let mut name = xkb::keysym_name(sym)?;
    if name.chars().count() == 1 {
        name = name.to_lowercase();
    }
    let mut out: Vec<String> = ["C", "A", "S", "W"].iter().filter(|m| modifiers.contains(*m)).map(|m| m.to_string()).collect();
    out.push(name);
    Ok(out.join("-"))
}

pub fn display(value: &str) -> String {
    value.split('-').map(|part| match part { "C" => "Ctrl", "A" => "Alt", "S" => "Shift", "W" => "Super", other => other }.to_string())
        .collect::<Vec<_>>().join("+")
}

pub fn path_in(root: &Path) -> PathBuf {
    root.join("tessel/capture/shortcuts.json")
}

pub fn load(root: Option<&Path>) -> Values {
    let root = root.map(Path::to_path_buf).unwrap_or_else(config_root);
    let Ok(data) = std::fs::read(path_in(&root)) else { return defaults() };
    let Ok(serde_json::Value::Object(saved)) = serde_json::from_slice::<serde_json::Value>(&data) else { return defaults() };
    let mut result = Values::new();
    for key in KEYS {
        let value = match saved.get(key) {
            None => default(key).to_string(),
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(_) => return defaults(),
        };
        match normalize(&value) {
            Ok(normalized) => result.push((key, normalized)),
            Err(_) => return defaults(),
        }
    }
    result
}

fn comment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").unwrap())
}

fn section_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?s)<keyboard\b[^>]*>(.*?)</keyboard\s*>").unwrap())
}

fn keybind_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?s)<keybind\b[^>]*(?:/>|>.*?</keybind\s*>)").unwrap())
}

/// Comments replaced by spaces of the same length, so offsets stay aligned.
fn mask(text: &str) -> String {
    comment_pattern().replace_all(text, |m: &regex::Captures| " ".repeat(m[0].len())).into_owned()
}

fn xml_error(error: roxmltree::Error) -> String {
    error.to_string()
}

/// ElementTree `.text` / `findtext`: text before the first child element (comments skipped).
fn et_text(node: roxmltree::Node) -> String {
    let mut out = String::new();
    for child in node.children() {
        if child.is_element() {
            break;
        }
        if child.is_text() {
            out.push_str(child.text().unwrap_or(""));
        }
    }
    out
}

fn children<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Vec<roxmltree::Node<'a, 'i>> {
    node.children().filter(|c| c.is_element() && c.tag_name().name() == name && c.tag_name().namespace().is_none()).collect()
}

fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
        .replace('\r', "&#13;").replace('\n', "&#10;").replace('\t', "&#09;")
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Rewrite rc.xml bytes with our bindings (`values` None = DEFAULTS).
pub fn shortcut(data: &[u8], values: Option<&Values>) -> Result<Vec<u8>, String> {
    let source = values.cloned().unwrap_or_else(defaults);
    let mut normalized = Values::new();
    for key in KEYS {
        let value = source.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str()).unwrap_or("");
        normalized.push((key, normalize(value)?));
    }
    let used: Vec<&str> = normalized.iter().map(|(_, v)| v.as_str()).filter(|v| !v.is_empty()).collect();
    let unique: std::collections::HashSet<&&str> = used.iter().collect();
    if unique.len() != used.len() {
        return Err(tr!("Each capture action needs a different shortcut."));
    }
    let text = std::str::from_utf8(data).map_err(|e| e.to_string())?;
    {
        let tree = roxmltree::Document::parse(text).map_err(xml_error)?;
        let root = tree.root_element();
        if root.tag_name().name() != "labwc_config" || root.tag_name().namespace().is_some() || children(root, "keyboard").len() != 1 {
            return Err(tr!("Expected a single labwc keyboard section."));
        }
    }
    let mut text = text.to_string();
    let masked = mask(&text);
    let Some(section) = section_pattern().captures(&masked).and_then(|c| c.get(1)) else {
        return Err(tr!("Missing labwc keyboard section."));
    };
    let mut replacements = Vec::new();
    for found in keybind_pattern().find_iter(&masked[section.start()..section.end()]) {
        let (start, end) = (section.start() + found.start(), section.start() + found.end());
        let snippet = &text[start..end];
        let document = roxmltree::Document::parse(snippet).map_err(xml_error)?;
        let node = document.root_element();
        let actions = children(node, "action");
        let command_text = if actions.len() == 1 {
            let attribute = actions[0].attribute("command").unwrap_or("");
            let value = if !attribute.is_empty() {
                attribute.to_string()
            } else {
                children(actions[0], "command").first().map(|c| et_text(*c)).unwrap_or_default()
            };
            value.trim_matches(is_py_space).to_string()
        } else {
            String::new()
        };
        if owned(&command_text) && actions[0].attribute("name").unwrap_or("").to_lowercase() == "execute" {
            replacements.push((start, end));
        } else {
            let Ok(key) = normalize(node.attribute("key").unwrap_or("")) else { continue };
            if used.contains(&key.as_str()) {
                return Err(tr!("{shortcut} is already assigned in labwc. Choose another shortcut.", shortcut = display(&key)));
            }
        }
    }
    for (start, end) in replacements.into_iter().rev() {
        text.replace_range(start..end, "");
    }
    let masked = mask(&text);
    let section = section_pattern().captures(&masked).and_then(|c| c.get(1)).ok_or_else(|| tr!("Missing labwc keyboard section."))?;
    let bindings: Vec<String> = normalized.iter().filter(|(_, v)| !v.is_empty()).map(|(key, value)| {
        format!("    <keybind key=\"{}\"><action name=\"Execute\"><command>{}</command></action></keybind>",
                escape_attribute(value), escape_text(&command(key)))
    }).collect();
    let end = section.end();
    let mut start = end;
    while start > section.start() {
        let previous = text[..start].chars().next_back().unwrap();
        if !is_py_space(previous) {
            break;
        }
        start -= previous.len_utf8();
    }
    Ok(format!("{}\n{}\n  {}", &text[..start], bindings.join("\n"), &text[end..]).into_bytes())
}

/// `tempfile.mkstemp(prefix='.' + name)` + chmod + replace.
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&parent).map_err(|e| tessel_core::pyerr::os_error_path(&e, &parent))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = crate::media::mkstemp(&parent, &format!(".{name}"), "").map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
    let result = std::fs::write(&temporary, data)
        .and_then(|_| std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode)))
        .and_then(|_| std::fs::rename(&temporary, path));
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| tessel_core::pyerr::os_error_text(&e))
}

fn timestamp() -> String {
    let now = glib::DateTime::now_local().expect("local time");
    format!("{}{:06}", now.format("%Y%m%dT%H%M%S").unwrap(), now.microsecond())
}

/// `labwc --reconfigure` (check=True, 10 s).
pub fn reconfigure() -> Result<(), String> {
    let args = vec!["labwc".to_string(), "--reconfigure".to_string()];
    let output = crate::process::run(&args, b"", std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;
    if output.status != 0 {
        return Err(crate::process::called_process_error(&args, output.status));
    }
    Ok(())
}

/// Save the bindings and reload labwc; any failure restores rc.xml and shortcuts.json.
pub fn apply(values: &Values, root: Option<&Path>, reload: Option<&dyn Fn() -> Result<(), String>>) -> Result<Values, String> {
    use std::os::unix::fs::PermissionsExt;
    let root = root.map(Path::to_path_buf).unwrap_or_else(config_root);
    let configured = root.join("labwc/rc.xml");
    let rc = std::fs::canonicalize(&configured).unwrap_or(configured);
    let before = std::fs::read(&rc).map_err(|e| tessel_core::pyerr::os_error_path(&e, &rc))?;
    let mode = std::fs::metadata(&rc).map_err(|e| tessel_core::pyerr::os_error_path(&e, &rc))?.permissions().mode() & 0o777;
    let mut normalized = Values::new();
    for key in KEYS {
        let value = values.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str()).unwrap_or("");
        normalized.push((key, normalize(value)?));
    }
    let after = shortcut(&before, Some(&normalized))?;
    let path = path_in(&root);
    let previous = std::fs::read(&path).ok();
    if after != before {
        let backup = root.join("tessel/capture/backups").join(format!("rc-{}.xml", timestamp()));
        atomic_write(&backup, &before, 0o600)?;
    }
    if std::fs::read(&rc).map_err(|e| tessel_core::pyerr::os_error_path(&e, &rc))? != before {
        return Err(tr!("Keyboard configuration changed. Please try again."));
    }
    let map: serde_json::Map<String, serde_json::Value> =
        normalized.iter().map(|(k, v)| (k.to_string(), serde_json::Value::String(v.clone()))).collect();
    let result = (|| {
        atomic_write(&rc, &after, mode)?;
        atomic_write(&path, (crate::compat::json_indent2(&serde_json::Value::Object(map)) + "\n").as_bytes(), 0o600)?;
        match reload {
            Some(reload) => reload(),
            None => reconfigure(),
        }
    })();
    if let Err(error) = result {
        let _ = atomic_write(&rc, &before, mode);
        match previous {
            None => {
                let _ = std::fs::remove_file(&path);
            }
            Some(previous) => {
                let _ = atomic_write(&path, &previous, 0o600);
            }
        }
        return Err(error);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &[u8] = br#"<labwc_config><keyboard><default/><!-- keep this -->
<keybind key="W-r"><action name="Execute" command="other-app"/></keybind>
<keybind key="Print"><action name="Execute"><command>/usr/local/bin/waycapture --screenshot</command></action></keybind>
</keyboard><mouse><default/></mouse></labwc_config>"#;

    fn with(changes: &[(&'static str, &str)]) -> Values {
        defaults().into_iter().map(|(k, v)| (k, changes.iter().find(|(c, _)| *c == k).map(|(_, x)| x.to_string()).unwrap_or(v))).collect()
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        String::from_utf8_lossy(haystack).contains(needle)
    }

    #[test]
    fn test_defaults_migrate_old_print_and_are_idempotent() {
        let new = shortcut(CONFIG, None).unwrap();
        assert_eq!(shortcut(&new, None).unwrap(), new);
        assert!(contains(&new, "<!-- keep this -->"));
        assert!(contains(&new, "command=\"other-app\""));
        assert!(contains(&new, "--capture screen"));
        assert!(!contains(&new, "--screenshot"));
        assert_eq!(String::from_utf8_lossy(&new).matches(&format!("{} --", program_text())).count(), 5);
        assert!(contains(&new, "key=\"A-F9\""));
        // Byte-identical to Python's capture_shortcuts.shortcut(CONFIG) (with the installed program).
        assert_eq!(String::from_utf8(new).unwrap(), concat!(
            "<labwc_config><keyboard><default/><!-- keep this -->\n",
            "<keybind key=\"W-r\"><action name=\"Execute\" command=\"other-app\"/></keybind>\n",
            "    <keybind key=\"A-F9\"><action name=\"Execute\"><command>/usr/local/bin/tessel-capture --toggle-recording</command></action></keybind>\n",
            "    <keybind key=\"Print\"><action name=\"Execute\"><command>/usr/local/bin/tessel-capture --capture screen</command></action></keybind>\n",
            "    <keybind key=\"C-Print\"><action name=\"Execute\"><command>/usr/local/bin/tessel-capture --capture area</command></action></keybind>\n",
            "    <keybind key=\"S-Print\"><action name=\"Execute\"><command>/usr/local/bin/tessel-capture --capture window</command></action></keybind>\n",
            "    <keybind key=\"A-Print\"><action name=\"Execute\"><command>/usr/local/bin/tessel-capture --capture workspace</command></action></keybind>\n",
            "  </keyboard><mouse><default/></mouse></labwc_config>").replace("/usr/local/bin/tessel-capture", &program_text()));
    }

    #[test]
    fn test_disabled_and_custom_keys_remove_owned_bindings() {
        let new = shortcut(&shortcut(CONFIG, None).unwrap(), Some(&with(&[("record", ""), ("screen", "Ctrl+Alt+F6")]))).unwrap();
        assert!(!contains(&new, "--toggle-recording"));
        assert!(contains(&new, "key=\"C-A-F6\""));
        assert_eq!(normalize("Alt+Ctrl+f6").unwrap(), "C-A-F6");
        assert_eq!(normalize("Print Screen").unwrap(), "Print");
        assert_eq!(normalize("super+A").unwrap(), "W-a");
        assert_eq!(display("C-A-F6"), "Ctrl+Alt+F6");
    }

    #[test]
    fn test_conflicts_and_invalid_input_leave_configuration_untouched() {
        for values in [with(&[("record", "Super+r")]), with(&[("record", "Print")]), with(&[("record", "Ctrl+not_a_key")]),
                       with(&[("record", "Ctrl+Shift_L")]), with(&[("record", "Hyper+x")])] {
            assert!(shortcut(CONFIG, Some(&values)).is_err(), "{values:?}");
        }
        assert_eq!(shortcut(CONFIG, Some(&with(&[("record", "Super+r")]))).unwrap_err(),
                   "Super+r is already assigned in labwc. Choose another shortcut.");
        assert_eq!(shortcut(b"<labwc_config/>", None).unwrap_err(), "Expected a single labwc keyboard section.");
    }

    #[test]
    fn test_transaction_and_reload_failure_rollback() {
        let root = std::env::temp_dir().join(format!("capture-shortcuts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let rc = root.join("labwc/rc.xml");
        std::fs::create_dir_all(rc.parent().unwrap()).unwrap();
        std::fs::write(&rc, CONFIG).unwrap();
        apply(&defaults(), Some(&root), Some(&|| Ok(()))).unwrap();
        let before = std::fs::read(&rc).unwrap();
        assert_eq!(load(Some(&root)), defaults());
        let error = apply(&with(&[("record", "Ctrl+F9")]), Some(&root), Some(&|| Err("reload failed".into()))).unwrap_err();
        assert_eq!(error, "reload failed");
        assert_eq!(std::fs::read(&rc).unwrap(), before);
        assert_eq!(load(Some(&root)), defaults());
        assert!(std::fs::read_dir(root.join("tessel/capture/backups")).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".xml")));
        assert_eq!(std::fs::read_to_string(path_in(&root)).unwrap(),
                   "{\n  \"record\": \"A-F9\",\n  \"screen\": \"Print\",\n  \"area\": \"C-Print\",\n  \"window\": \"S-Print\",\n  \"workspace\": \"A-Print\"\n}\n");
        std::fs::write(path_in(&root), "{\"record\": 5}").unwrap();
        assert_eq!(load(Some(&root)), defaults());
        std::fs::write(path_in(&root), "{\"record\": \"alt+f10\", \"screen\": \"\"}").unwrap();
        assert_eq!(load(Some(&root)), with(&[("record", "A-F10"), ("screen", "")]));
        let _ = std::fs::remove_dir_all(root);
    }
}
