//! labwc rc.xml lookup, HDR overrides block and `labwc --reconfigure`
//! (tessel-displays:61-69, 184-365).

use crate::compat::fsx::{self, Replace};
use crate::primary::run;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;
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

pub use tessel_core::labwc_hdr::{element_text, labwc_config_path, local_name, parse_labwc_bool, parse_labwc_hdr_xml};

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
