//! labwc's HDR policy: the global `<core><hdr>` switch and per-output
//! `<hdrOutputs>` overrides in the running instance's rc.xml.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn expanduser(path: &str) -> PathBuf {
    if path == "~" {
        return crate::paths::home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return crate::paths::home().join(rest);
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
    let mut config_dir = crate::paths::config_home().join("labwc").to_string_lossy().into_owned();
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

/// Whether labwc's rc.xml text turns HDR on for `connector`.
pub fn output_hdr_enabled(text: &str, connector: &str) -> bool {
    let (global, overrides) = parse_labwc_hdr_xml(text);
    overrides.get(connector).copied().unwrap_or(global)
}

/// Whether the running labwc turns HDR on for `connector` (false when rc.xml is unreadable).
pub fn hdr_enabled(connector: &str) -> bool {
    std::fs::read_to_string(labwc_config_path()).is_ok_and(|text| output_hdr_enabled(&text, connector))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_output_overrides_win_over_the_global_switch() {
        let rc = r#"<labwc_config><core><hdr>yes</hdr></core>
            <hdrOutputs><output name="DP-2" enabled="yes"/><output name="HDMI-A-1" enabled="no"/></hdrOutputs></labwc_config>"#;
        assert!(output_hdr_enabled(rc, "DP-2"));
        assert!(!output_hdr_enabled(rc, "HDMI-A-1"));
        assert!(output_hdr_enabled(rc, "DP-1"));
        assert!(!output_hdr_enabled("<labwc_config/>", "DP-2"));
        assert!(!output_hdr_enabled("not xml", "DP-2"));
    }
}
