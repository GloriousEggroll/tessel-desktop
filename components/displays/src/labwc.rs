//! labwc rc.xml lookup, the HDR overrides and adaptive sync blocks and
//! `labwc --reconfigure` (tessel-displays:61-69, 184-365).

use crate::compat::fsx::{self, Replace};
use crate::daemon::config::VrrMode;
use crate::primary::run;
use regex::Regex;
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

pub const LABWC_HDR_BEGIN: &str = "<!-- tessel-displays HDR overrides begin -->";
pub const LABWC_HDR_END: &str = "<!-- tessel-displays HDR overrides end -->";
pub const LABWC_VRR_BEGIN: &str = "<!-- tessel-displays adaptive sync begin -->";
pub const LABWC_VRR_END: &str = "<!-- tessel-displays adaptive sync end -->";
/// The markers written before the Tessel rename; read as the current ones.
const LEGACY_MARKERS: [(&str, &str); 2] = [
    ("<!-- way-displays-gui HDR overrides begin -->", LABWC_HDR_BEGIN),
    ("<!-- way-displays-gui HDR overrides end -->", LABWC_HDR_END),
];

fn read_rc(path: &Path) -> std::io::Result<String> {
    fsx::read_text(path).map(|text| crate::color_config::upgrade_markers(text, &LEGACY_MARKERS))
}
fn block_pattern(begin: &str, end: &str) -> Regex {
    Regex::new(&format!("(?s){}.*?{}", regex::escape(begin), regex::escape(end))).unwrap()
}
static HDR_PATTERN: LazyLock<Regex> = LazyLock::new(|| block_pattern(LABWC_HDR_BEGIN, LABWC_HDR_END));
static VRR_PATTERN: LazyLock<Regex> = LazyLock::new(|| block_pattern(LABWC_VRR_BEGIN, LABWC_VRR_END));
static CLOSING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</(?:labwc_config|openbox_config)\s*>").unwrap());
/// An empty root (`<labwc_config/>`), as a first rc.xml often is.
static EMPTY_ROOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<(labwc_config|openbox_config)(\s[^<>]*?)?\s*/>").unwrap());

/// Held while one of this component's writers reads and replaces rc.xml, so
/// the Displays window and the daemon never write over each other's block.
pub struct RcLock(std::fs::File);

impl Drop for RcLock {
    fn drop(&mut self) {
        // SAFETY: flock on the descriptor this lock owns.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// `rc.xml.tessel-lock` next to rc.xml.
pub fn rc_lock_path(path: &Path) -> PathBuf {
    // A linked rc.xml is locked where the file is, as the color writer finds it.
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "rc.xml".into());
    path.with_file_name(format!("{name}.tessel-lock"))
}

/// Wait for and take the advisory lock of `path`. Its folder must exist.
pub fn lock_rc(path: &Path) -> std::io::Result<RcLock> {
    let lock_path = rc_lock_path(path);
    let file = std::fs::OpenOptions::new().append(true).create(true).open(&lock_path)?;
    loop {
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(RcLock(file));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

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

/// One block of rc.xml that this component owns, between its two marker comments.
pub struct ManagedBlock<'a> {
    pub begin: &'a str,
    pub end: &'a str,
    pub pattern: &'a Regex,
    /// The element inside the markers, e.g. `hdrOutputs`.
    pub element: &'a str,
    /// `<output … />` lines without indentation.
    pub outputs: Vec<String>,
}

impl ManagedBlock<'_> {
    pub fn text(&self) -> String {
        let mut lines = vec![format!("  {}", self.begin), format!("  <{}>", self.element)];
        lines.extend(self.outputs.iter().map(|line| format!("    {line}")));
        lines.push(format!("  </{}>", self.element));
        lines.push(format!("  {}", self.end));
        lines.join("\n")
    }
}

/// What writing a managed block did.
#[derive(Debug, PartialEq, Eq)]
pub enum BlockWrite {
    /// rc.xml already had this block: nothing written.
    Unchanged,
    Written,
    /// There is no rc.xml and it was not to be created.
    NoFile,
}

/// Atomically replace only `block` in rc.xml, under the rc.xml lock, keeping
/// everything else byte for byte. A missing rc.xml is created only with `create`.
pub fn write_managed_block_with(path: &Path, block: &ManagedBlock, create: bool, replace: Replace) -> Result<BlockWrite, String> {
    if !create && !path.exists() {
        return Ok(BlockWrite::NoFile);
    }
    let directory = path.parent().unwrap_or(Path::new("."));
    if !directory.is_dir() {
        std::fs::create_dir_all(directory).map_err(|error| tessel_core::pyerr::os_error_path(&error, directory))?;
    }
    let _lock = lock_rc(path).map_err(|error| tessel_core::pyerr::os_error_path(&error, &rc_lock_path(path)))?;
    let (text, mode) = match read_rc(path) {
        Ok(text) => match fsx::mode_of(path) {
            Ok(mode) => (text, mode),
            Err(error) => return Err(fsx::read_error(&error, path)),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => return Ok(BlockWrite::NoFile),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            ("<?xml version=\"1.0\"?>\n<labwc_config>\n</labwc_config>\n".to_owned(), 0o600),
        Err(error) => return Err(fsx::read_error(&error, path)),
    };
    let block_text = block.text();
    let mut updated = if block.pattern.is_match(&text) {
        block.pattern.replacen(&text, 1, regex::NoExpand(block_text.trim())).into_owned()
    } else {
        let opened;
        let source = match (CLOSING.is_match(&text), EMPTY_ROOT.captures(&text)) {
            (false, Some(root)) => {
                let whole = root.get(0).unwrap();
                let attributes = root.get(2).map_or("", |m| m.as_str().trim_end());
                opened = format!("{}<{}{attributes}>\n</{}>{}", &text[..whole.start()], &root[1], &root[1], &text[whole.end()..]);
                &opened
            }
            _ => &text,
        };
        let Some(closing) = CLOSING.find(source) else {
            return Err("rc.xml has no closing labwc_config element".into());
        };
        let prefix = source[..closing.start()].trim_end();
        format!("{prefix}\n\n{block_text}\n{}", &source[closing.start()..])
    };
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    if updated == text {
        return Ok(BlockWrite::Unchanged);
    }
    fsx::atomic_write_with(path, updated.as_bytes(), mode, ".rc.xml.", replace)?;
    Ok(BlockWrite::Written)
}

/// Atomically replace only the HDR block managed by this application.
pub fn write_labwc_hdr_overrides_with(path: &Path, overrides: &BTreeMap<String, bool>, replace: Replace) -> (bool, Option<String>) {
    let outputs = overrides.iter()
        .map(|(name, enabled)| format!("<output name=\"{}\" enabled=\"{}\" />", html_escape(name), if *enabled { "yes" } else { "no" }))
        .collect();
    let block = ManagedBlock { begin: LABWC_HDR_BEGIN, end: LABWC_HDR_END, pattern: &HDR_PATTERN, element: "hdrOutputs", outputs };
    match write_managed_block_with(path, &block, true, replace) {
        Ok(_) => (true, None),
        Err(error) => (false, Some(error)),
    }
}

/// The adaptive sync block for these connectors (`mode` fullscreen, yes or no).
pub fn adaptive_sync_block(modes: &BTreeMap<String, VrrMode>) -> ManagedBlock<'static> {
    let outputs = modes.iter()
        .map(|(name, mode)| format!("<output name=\"{}\" mode=\"{}\" />", html_escape(name), mode.labwc()))
        .collect();
    ManagedBlock { begin: LABWC_VRR_BEGIN, end: LABWC_VRR_END, pattern: &VRR_PATTERN, element: "adaptiveSyncOutputs", outputs }
}

/// Per-screen adaptive sync from the managed block of rc.xml text.
pub fn parse_adaptive_sync_block(text: &str) -> BTreeMap<String, String> {
    let Some(found) = VRR_PATTERN.find(text) else { return BTreeMap::new() };
    let wrapped = format!("<labwc_config>{}</labwc_config>", found.as_str());
    let Ok(document) = roxmltree::Document::parse(&wrapped) else { return BTreeMap::new() };
    document.descendants().filter(|node| node.has_tag_name("output"))
        .filter_map(|node| Some((node.attribute("name")?.to_owned(), node.attribute("mode")?.to_owned())))
        .collect()
}

/// Make rc.xml's adaptive sync block say `modes`, and only when that changed
/// it, have labwc read it again. Never creates an rc.xml. `Ok(true)` when it
/// wrote the file.
pub fn sync_adaptive_sync_with(path: &Path, modes: &BTreeMap<String, VrrMode>, replace: Replace,
                               reconfigure: &dyn Fn() -> (bool, Option<String>)) -> Result<BlockWrite, String> {
    let written = write_managed_block_with(path, &adaptive_sync_block(modes), false, replace)?;
    if written == BlockWrite::Written {
        if let (false, error) = reconfigure() {
            return Err(format!("labwc did not reload rc.xml: {}", error.unwrap_or_default()));
        }
    }
    Ok(written)
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
