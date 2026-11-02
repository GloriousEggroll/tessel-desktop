//! labwc rc.xml lookup, HDR overrides block and `labwc --reconfigure`
//! (tessel-displays:61-69, 184-365).

use crate::compat::fsx::{self, Replace};
use crate::primary::run;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

pub const LABWC_HDR_BEGIN: &str = "<!-- tessel-displays HDR overrides begin -->";
pub const LABWC_HDR_END: &str = "<!-- tessel-displays HDR overrides end -->";
/// The markers written before the Tessel rename; read as the current ones.
const LEGACY_MARKERS: [(&str, &str); 2] = [
    ("<!-- way-displays-gui HDR overrides begin -->", LABWC_HDR_BEGIN),
    ("<!-- way-displays-gui HDR overrides end -->", LABWC_HDR_END),
];

fn read_rc(path: &Path) -> std::io::Result<String> {
    fsx::read_text(path).map(|text| crate::color_config::upgrade_markers(text, &LEGACY_MARKERS))
}
static HDR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("(?s){}.*?{}", regex::escape(LABWC_HDR_BEGIN), regex::escape(LABWC_HDR_END))).unwrap()
});
static CLOSING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</(?:labwc_config|openbox_config)\s*>").unwrap());

fn expanduser(path: &str) -> PathBuf {
    if path == "~" {
        return tessel_core::paths::home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return tessel_core::paths::home().join(rest);
    }
    PathBuf::from(path)
}

fn abspath(path: PathBuf) -> PathBuf {
    let absolute = std::path::absolute(&path).unwrap_or(path);
    // os.path.abspath also normalizes "." and ".." lexically.
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::ParentDir => { out.pop(); }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The rc.xml used by the running labwc instance when detectable
/// (`$LABWC_PID` command line `-c/--config`, `-C/--config-dir`).
pub fn labwc_config_path() -> PathBuf {
    let mut config_dir = tessel_core::paths::config_home().join("labwc").to_string_lossy().into_owned();
    let mut config_file: Option<String> = None;
    let pid = std::env::var("LABWC_PID").unwrap_or_default();
    if !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) {
            let args: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            let mut index = 1;
            while index < args.len() {
                let arg = &args[index];
                if (arg == "-c" || arg == "--config") && index + 1 < args.len() {
                    config_file = Some(args[index + 1].clone());
                    index += 2;
                    continue;
                }
                if let Some(value) = arg.strip_prefix("--config=") {
                    config_file = Some(value.to_owned());
                } else if (arg == "-C" || arg == "--config-dir") && index + 1 < args.len() {
                    config_dir = args[index + 1].clone();
                    index += 2;
                    continue;
                } else if let Some(value) = arg.strip_prefix("--config-dir=") {
                    config_dir = value.to_owned();
                }
                index += 1;
            }
        }
    }
    match config_file.filter(|f| !f.is_empty()) {
        Some(file) => abspath(expanduser(&file)),
        None => abspath(expanduser(&config_dir)).join("rc.xml"),
    }
}

pub fn parse_labwc_bool(value: Option<&str>, default: bool) -> bool {
    match value.unwrap_or("").trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

pub fn local_name(node: roxmltree::Node) -> String {
    node.tag_name().name().to_lowercase()
}

/// ElementTree's `element.text`: character data before the first child element.
pub fn element_text(node: roxmltree::Node) -> Option<String> {
    let mut text: Option<String> = None;
    for child in node.children() {
        if child.is_element() {
            break;
        }
        if child.is_text() {
            text.get_or_insert_with(String::new).push_str(child.text().unwrap_or(""));
        }
    }
    text
}

fn child_value(element: roxmltree::Node, name: &str) -> Option<String> {
    if let Some(value) = element.attribute(name) {
        return Some(value.to_owned());
    }
    let name = name.to_lowercase();
    element.children().filter(|c| c.is_element()).find(|c| local_name(*c) == name).map(element_text)?
}

/// Global HDR policy and per-output overrides from rc.xml text.
pub fn parse_labwc_hdr_xml(text: &str) -> (bool, BTreeMap<String, bool>) {
    let mut global = false;
    let mut overrides = BTreeMap::new();
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let Ok(document) = roxmltree::Document::parse_with_options(text, options) else { return (global, overrides) };
    for element in document.root_element().descendants().filter(|n| n.is_element()) {
        match local_name(element).as_str() {
            "core" => {
                for child in element.children().filter(|c| c.is_element()) {
                    if local_name(child) == "hdr" {
                        if let Some(text) = element_text(child).filter(|t| !t.is_empty()) {
                            global = parse_labwc_bool(Some(&text), global);
                        }
                    }
                }
            }
            "hdroutputs" => {
                for output in element.children().filter(|c| c.is_element()) {
                    if local_name(output) != "output" {
                        continue;
                    }
                    let name = child_value(output, "name").unwrap_or_default().trim().to_owned();
                    if let (false, Some(enabled)) = (name.is_empty(), child_value(output, "enabled")) {
                        overrides.insert(name, parse_labwc_bool(Some(&enabled), false));
                    }
                }
            }
            _ => {}
        }
    }
    (global, overrides)
}

pub struct HdrConfig {
    pub global: bool,
    pub overrides: BTreeMap<String, bool>,
    pub managed: BTreeMap<String, bool>,
    pub text: String,
}

pub fn read_labwc_hdr_config(path: &Path) -> HdrConfig {
    let Ok(text) = read_rc(path) else {
        return HdrConfig { global: false, overrides: BTreeMap::new(), managed: BTreeMap::new(), text: String::new() };
    };
    let (global, overrides) = parse_labwc_hdr_xml(&text);
    let managed = HDR_PATTERN.find(&text)
        .map(|m| parse_labwc_hdr_xml(&format!("<labwc_config>{}</labwc_config>", m.as_str())).1)
        .unwrap_or_default();
    HdrConfig { global, overrides, managed, text }
}

/// `html.escape(text, quote=True)`.
pub fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

pub fn write_labwc_hdr_overrides(path: &Path, overrides: &BTreeMap<String, bool>) -> (bool, Option<String>) {
    write_labwc_hdr_overrides_with(path, overrides, &fsx::os_replace)
}

/// Atomically replace only the HDR block managed by this application.
pub fn write_labwc_hdr_overrides_with(path: &Path, overrides: &BTreeMap<String, bool>, replace: Replace) -> (bool, Option<String>) {
    let (text, mode) = match read_rc(path) {
        Ok(text) => match fsx::mode_of(path) {
            Ok(mode) => (text, mode),
            Err(error) => return (false, Some(fsx::read_error(&error, path))),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            ("<?xml version=\"1.0\"?>\n<labwc_config>\n</labwc_config>\n".to_owned(), 0o600),
        Err(error) => return (false, Some(fsx::read_error(&error, path))),
    };
    let mut lines = vec![format!("  {LABWC_HDR_BEGIN}"), "  <hdrOutputs>".to_owned()];
    for (name, enabled) in overrides {
        lines.push(format!("    <output name=\"{}\" enabled=\"{}\" />", html_escape(name), if *enabled { "yes" } else { "no" }));
    }
    lines.push("  </hdrOutputs>".into());
    lines.push(format!("  {LABWC_HDR_END}"));
    let block = lines.join("\n");
    let mut updated = if HDR_PATTERN.is_match(&text) {
        HDR_PATTERN.replacen(&text, 1, regex::NoExpand(block.trim())).into_owned()
    } else {
        let Some(closing) = CLOSING.find(&text) else {
            return (false, Some("rc.xml has no closing labwc_config element".into()));
        };
        let prefix = text[..closing.start()].trim_end();
        format!("{prefix}\n\n{block}\n{}", &text[closing.start()..])
    };
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    if updated == text {
        return (true, None);
    }
    match fsx::atomic_write_with(path, updated.as_bytes(), mode, ".rc.xml.", replace) {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error)),
    }
}

/// `labwc --reconfigure` (timeout 5 s).
pub fn reconfigure_labwc() -> (bool, Option<String>) {
    match run(&["labwc", "--reconfigure"], Duration::from_secs(5)) {
        Err(None) => (false, Some("labwc is not installed".into())),
        Err(Some(error)) => (false, Some(error)),
        Ok(result) if result.status != 0 => {
            let detail = [result.stderr.trim(), result.stdout.trim()].into_iter().find(|s| !s.is_empty())
                .map(String::from).unwrap_or_else(|| format!("labwc exited with status {}", result.status));
            (false, Some(detail))
        }
        Ok(_) => (true, None),
    }
}
