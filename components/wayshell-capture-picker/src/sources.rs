//! xdg-desktop-portal-wlr dmenu records, kept separate from their UI labels
//! (capture_sources.py).
//!
//! Only the portal's supplied choices are eligible. The selected record must be
//! returned unchanged: window identifiers are opaque and monitor descriptions are
//! part of the dmenu matching protocol, even when they look redundant in the UI.

use std::io::Write;
use std::sync::OnceLock;

pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
/// xdpw waits for the chooser to exit before reading its stdout. Keep our one
/// response below PIPE_BUF so a very long window title cannot deadlock that pipe.
pub const MAX_RECORD_BYTES: usize = 4095;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Monitor,
    Window,
}

impl Kind {
    /// The stack page id (`monitor` / `window`).
    pub fn name(self) -> &'static str {
        match self {
            Kind::Monitor => "monitor",
            Kind::Window => "window",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CaptureSource {
    pub token: String,
    pub kind: Kind,
    pub title: String,
    pub subtitle: String,
    pub icon_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLarge,
    /// Python's UnicodeDecodeError text.
    Unicode(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TooLarge => f.write_str("The portal supplied too many capture sources."),
            ParseError::Unicode(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for ParseError {}

/// `str.isprintable()` for one character (Python's Unicode tables).
pub fn is_printable(ch: char) -> bool {
    let code = ch as u32;
    let table = crate::printable_table::NON_PRINTABLE;
    match table.binary_search_by(|&(start, end)| {
        if end < code {
            std::cmp::Ordering::Less
        } else if start > code {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(_) => false,
        Err(_) => true,
    }
}

/// `str.isprintable()` (true for the empty string).
pub fn is_printable_str(text: &str) -> bool {
    text.chars().all(is_printable)
}

/// Flatten title whitespace for display, never for the returned token.
pub fn display_text(value: &str) -> String {
    let replaced: String = value.chars().map(|c| if is_printable(c) { c } else { ' ' }).collect();
    replaced.split(suite_core::pyjson::is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

fn window_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    // Python's `\s` for str also covers U+001C..U+001F.
    PATTERN.get_or_init(|| regex::Regex::new(r"^Window: (.*) \(([^()\s\x1C-\x1F]+)\)$").unwrap())
}

/// Python's `bytes.decode('utf-8')` error text.
fn decode_error(data: &[u8], error: std::str::Utf8Error) -> String {
    let position = error.valid_up_to();
    let byte = data[position];
    match error.error_len() {
        None => format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: unexpected end of data"),
        Some(_) => {
            let lead = matches!(byte, 0xC2..=0xF4);
            if lead {
                let end = position + error.error_len().unwrap_or(1);
                if end - position > 1 {
                    format!("'utf-8' codec can't decode bytes in position {position}-{}: invalid continuation byte", end - 1)
                } else {
                    format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: invalid continuation byte")
                }
            } else {
                format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: invalid start byte")
            }
        }
    }
}

pub fn parse_sources(data: &[u8]) -> Result<Vec<CaptureSource>, ParseError> {
    if data.len() > MAX_INPUT_BYTES {
        return Err(ParseError::TooLarge);
    }
    let text = std::str::from_utf8(data).map_err(|e| ParseError::Unicode(decode_error(data, e)))?;
    let mut choices = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for token in text.split('\n') {
        if token.is_empty() || !seen.insert(token) {
            continue;
        }
        if token.contains('\0') || token.len() > MAX_RECORD_BYTES {
            continue;
        }
        if let Some(rest) = token.strip_prefix("Monitor: ") {
            let (name, description) = rest.split_once(' ').unwrap_or((rest, ""));
            if name.is_empty() || !is_printable_str(name) {
                continue;
            }
            let label = display_text(description);
            choices.push(CaptureSource {
                token: token.to_owned(), kind: Kind::Monitor,
                title: if label.is_empty() { name.to_owned() } else { label },
                subtitle: name.to_owned(), icon_name: "video-display-symbolic".into(),
            });
        } else if token.starts_with("Window: ") {
            let Some(captures) = window_pattern().captures(token) else { continue };
            // Empty titles still need a distinguishable label. The identifier is
            // useful when multiple windows have identical captions.
            let identifier = captures[2].to_owned();
            let label = display_text(&captures[1]);
            choices.push(CaptureSource {
                token: token.to_owned(), kind: Kind::Window,
                title: if label.is_empty() { identifier.clone() } else { label },
                subtitle: identifier, icon_name: "application-x-executable-symbolic".into(),
            });
        }
    }
    Ok(choices)
}

#[derive(Debug)]
pub enum WriteError {
    NotOffered,
    Io(std::io::Error),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::NotOffered => f.write_str("The selected source was not offered by the portal."),
            WriteError::Io(error) => write!(f, "{}", suite_core::pyerr::os_error_text(error)),
        }
    }
}

impl std::error::Error for WriteError {}

/// A closed dialog or invalid selection never falls back to another source.
pub fn write_selection(source: Option<&CaptureSource>, choices: &[CaptureSource], stream: &mut dyn Write) -> Result<(), WriteError> {
    let Some(source) = source else { return Ok(()) };
    if !choices.contains(source) {
        return Err(WriteError::NotOffered);
    }
    let mut record = Vec::with_capacity(source.token.len() + 1);
    record.extend_from_slice(source.token.as_bytes());
    record.push(b'\n');
    // One write(2) of ≤ PIPE_BUF bytes is atomic on a pipe.
    stream.write_all(&record).map_err(WriteError::Io)?;
    stream.flush().map_err(WriteError::Io)
}
