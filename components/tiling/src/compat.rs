//! Helpers missing from tessel-core (candidates to upstream): Python value
//! semantics over JSON, an argparse twin, mode-preserving atomic writes,
//! base64, shlex/html quoting and Python-style OS error text.

use serde_json::Value;
use std::io::{self, Write};
use std::path::Path;

/// `bool(value)`.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

/// `value is True`.
pub fn is_true(value: Option<&Value>) -> bool { matches!(value, Some(Value::Bool(true))) }

/// Python `value == flag` for a JSON value and a bool (`1 == True`, `None != False`).
pub fn eq_bool(value: Option<&Value>, flag: bool) -> bool {
    match value {
        Some(Value::Bool(b)) => *b == flag,
        Some(Value::Number(n)) => n.as_f64() == Some(flag as i64 as f64),
        _ => false,
    }
}

/// `type(value) is int`: a JSON integer (never a bool or a float).
pub fn as_int(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) if !n.is_f64() => n.as_i64(),
        _ => None,
    }
}

pub fn repr_string(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 32 || c as u32 == 127 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::String(text) => repr_string(text),
        other => py_str(other),
    }
}

/// `str(value)` for a JSON-decoded Python value.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => match (number.as_i64(), number.as_u64()) {
            (Some(int), _) => int.to_string(),
            (_, Some(int)) => int.to_string(),
            _ => tessel_core::pyfmt::float_repr(number.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(text) => text.clone(),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", repr_string(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python `int(text)` for decimal strings: surrounding whitespace, a sign,
/// single underscores between digits.
pub fn python_int(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let digit = c.to_digit(10)? as i64;
        value = value.checked_mul(10)?.checked_add(digit)?;
    }
    Some(if negative { -value } else { value })
}

/// Python's `str(OSError)`: `"[Errno N] Description"` (plus `: 'path'` when given).
pub fn python_os_error_text(error: &io::Error, path: Option<&Path>) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let text = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
            match path {
                Some(path) => format!("[Errno {code}] {text}: {}", repr_string(&path.to_string_lossy())),
                None => format!("[Errno {code}] {text}"),
            }
        }
        None => error.to_string(),
    }
}

/// `_atomic_write(path, data, mode)` of workspace_config.py: a temporary
/// `.<name>-XXXXXXXX` beside `path` with `mode`, fsync, `os.replace`.
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut created = None;
    for attempt in 0..100u32 {
        let random = glib::random_int() ^ attempt.wrapping_mul(2654435761);
        let candidate = parent.join(format!(".{name}-{random:08x}"));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&candidate) {
            Ok(file) => {
                created = Some((file, candidate));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let (mut file, temporary) = created.ok_or_else(|| io::Error::from_raw_os_error(libc::EEXIST))?;
    let result = (|| {
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.write_all(data)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `base64.b64encode`.
pub fn b64encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(B64[(n >> shift & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `base64.b64decode` (non-validating: other characters are discarded).
pub fn b64decode(text: &str) -> Result<Vec<u8>, String> {
    let mut bits = 0u32;
    let mut count = 0u32;
    let mut out = Vec::new();
    let mut quads = 0usize;
    let mut padding = 0usize;
    for c in text.bytes() {
        if c == b'=' {
            // Padding ends the data once the quad can be completed.
            if quads % 4 >= 2 {
                padding += 1;
                if (quads + padding) % 4 == 0 {
                    break;
                }
            }
            continue;
        }
        let Some(value) = B64.iter().position(|b| *b == c) else { continue };
        if padding > 0 {
            return Err("Discontinuous padding not allowed".into());
        }
        quads += 1;
        bits = bits << 6 | value as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count & 0xff) as u8);
        }
    }
    match quads % 4 {
        0 => Ok(out),
        1 => Err(format!("Invalid base64-encoded string: number of data characters ({quads}) cannot be 1 more than a multiple of 4")),
        _ if (quads + padding) % 4 == 0 => Ok(out),
        _ => Err("Incorrect padding".into()),
    }
}

/// `shlex.quote`.
pub fn shlex_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if text.chars().all(safe) {
        return text.into();
    }
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// `shlex.join`.
pub fn shlex_join(parts: &[&str]) -> String { parts.iter().map(|p| shlex_quote(p)).collect::<Vec<_>>().join(" ") }

/// `html.escape(text, quote=True)`.
pub fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// `datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')`.
pub fn utc_stamp() -> String {
    let now = glib::DateTime::now_utc().expect("UTC time");
    format!("{}{:06}Z", now.format("%Y%m%dT%H%M%S").expect("time format"), now.microsecond())
}

// ---------------------------------------------------------------- argparse

/// One `argparse` parser with a mutually exclusive group of `store_true`
/// modes plus `--popup-edge {top,bottom,left,right}` and `--popup-offset INT`.
pub struct ArgSpec {
    pub description: &'static str,
    pub modes: &'static [&'static str],
}

pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The chosen mode name without `--`.
    pub mode: Option<&'static str>,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Default for Parsed {
    fn default() -> Self { Self { mode: None, popup_edge: "bottom".into(), popup_offset: 76 } }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Run(Parsed),
    /// `-h/--help`: print [`ArgSpec::help`] to stdout, exit 0.
    Help,
    /// `parser.error()`: usage + `prog: error: msg` on stderr, exit 2.
    Error(String),
}

// argparse's `_negative_number_matcher`.
fn looks_negative(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('-') else { return false };
    let mut parts = rest.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match frac {
        None => digits(int),
        Some(frac) => (int.is_empty() || digits(int)) && digits(frac),
    }
}

fn is_option_like(arg: &str) -> bool {
    if !arg.starts_with('-') || arg == "-" || arg == "--" {
        return arg == "--";
    }
    !(looks_negative(arg) && !arg.contains(' '))
}

enum Resolved {
    Exact(String, Option<String>),
    Ambiguous(String),
    Unknown,
}

impl ArgSpec {
    fn options(&self) -> Vec<String> {
        let mut options = vec!["--help".to_owned()];
        options.extend(self.modes.iter().map(|m| format!("--{m}")));
        options.push("--popup-edge".into());
        options.push("--popup-offset".into());
        options
    }

    fn resolve(&self, arg: &str) -> Resolved {
        if arg == "-h" {
            return Resolved::Exact("--help".into(), None);
        }
        let (name, explicit) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg, None),
        };
        let options = self.options();
        if options.iter().any(|o| o == name) {
            return Resolved::Exact(name.into(), explicit);
        }
        if !name.starts_with("--") || name.len() < 3 {
            return Resolved::Unknown;
        }
        let matches: Vec<&String> = options.iter().filter(|o| o.starts_with(name)).collect();
        match matches.len() {
            0 => Resolved::Unknown,
            1 => Resolved::Exact(matches[0].clone(), explicit),
            _ => Resolved::Ambiguous(format!("ambiguous option: {name} could match {}",
                                             matches.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))),
        }
    }

    /// Parse like `parser.parse_args(argv)`. `nonnegative`: the discovery
    /// manager's extra `--popup-offset must be nonnegative` check.
    pub fn parse<S: AsRef<str>>(&self, argv: &[S], nonnegative: bool) -> Outcome {
        let argv: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
        let mut args = Parsed::default();
        let mut unrecognized: Vec<String> = Vec::new();
        let mut seen_mode: Option<String> = None;
        let mut index = 0;
        let mut positional_only = false;
        while index < argv.len() {
            let arg = argv[index];
            index += 1;
            if positional_only || !is_option_like(arg) {
                unrecognized.push(arg.into());
                continue;
            }
            if arg == "--" {
                positional_only = true;
                unrecognized.push(arg.into());
                continue;
            }
            let (option, explicit) = match self.resolve(arg) {
                Resolved::Exact(option, explicit) => (option, explicit),
                Resolved::Ambiguous(message) => return Outcome::Error(message),
                Resolved::Unknown => {
                    unrecognized.push(arg.into());
                    continue;
                }
            };
            if let Some(mode) = self.modes.iter().find(|m| option[2..] == ***m) {
                if let Some(value) = explicit {
                    return Outcome::Error(format!("argument {option}: ignored explicit argument '{value}'"));
                }
                if let Some(previous) = seen_mode.as_ref().filter(|previous| **previous != option) {
                    return Outcome::Error(format!("argument {option}: not allowed with argument {previous}"));
                }
                seen_mode = Some(option.clone());
                args.mode = Some(mode);
                continue;
            }
            if option == "--help" {
                if let Some(value) = explicit {
                    return Outcome::Error(format!("argument -h/--help: ignored explicit argument '{value}'"));
                }
                return Outcome::Help;
            }
            let value = match explicit {
                Some(value) => value,
                None => match argv.get(index) {
                    Some(next) if !is_option_like(next) => {
                        index += 1;
                        (*next).to_owned()
                    }
                    _ => return Outcome::Error(format!("argument {option}: expected one argument")),
                },
            };
            if option == "--popup-edge" {
                if !EDGES.contains(&value.as_str()) {
                    return Outcome::Error(format!(
                        "argument --popup-edge: invalid choice: {} (choose from 'top', 'bottom', 'left', 'right')",
                        repr_string(&value)));
                }
                args.popup_edge = value;
            } else {
                match python_int(&value) {
                    Some(number) => args.popup_offset = number,
                    None => return Outcome::Error(format!("argument --popup-offset: invalid int value: {}",
                                                          repr_string(&value))),
                }
            }
        }
        if !unrecognized.is_empty() {
            return Outcome::Error(format!("unrecognized arguments: {}", unrecognized.join(" ")));
        }
        if nonnegative && args.popup_offset < 0 {
            return Outcome::Error("--popup-offset must be nonnegative".into());
        }
        Outcome::Run(args)
    }

    /// argparse's usage block (80-column terminal, as when output is piped).
    pub fn usage(&self, prog: &str) -> String {
        let text_width = 78usize;
        let mut parts: Vec<String> = vec!["[-h]".into()];
        for (index, mode) in self.modes.iter().enumerate() {
            if index > 0 {
                parts.push("|".into());
            }
            let mut part = format!("--{mode}");
            if index == 0 {
                part.insert(0, '[');
            }
            if index == self.modes.len() - 1 {
                part.push(']');
            }
            parts.push(part);
        }
        parts.push("[--popup-edge {top,bottom,left,right}]".into());
        parts.push("[--popup-offset POPUP_OFFSET]".into());
        let prefix = "usage: ";
        let full = format!("{prog} {}", parts.join(" "));
        if prefix.len() + full.len() <= text_width {
            return format!("{prefix}{full}\n");
        }
        // argparse wraps the optional groups as units, then the group's own parts.
        let indent = " ".repeat(prefix.len() + prog.len() + 1);
        let mut all = vec![prog.to_owned()];
        all.extend(parts);
        let mut lines: Vec<String> = Vec::new();
        let mut line: Vec<String> = Vec::new();
        let mut line_len = prefix.len() - 1;
        for part in all {
            if line_len + 1 + part.len() > text_width && !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
                line.clear();
                line_len = indent.len() - 1;
            }
            line_len += part.len() + 1;
            line.push(part);
        }
        if !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
        }
        lines[0] = lines[0][indent.len()..].to_owned();
        format!("{prefix}{}\n", lines.join("\n"))
    }

    pub fn help(&self, prog: &str) -> String {
        let mut text = format!("{}\n{}\n\noptions:\n  -h, --help            show this help message and exit\n",
                               self.usage(prog), self.description);
        for mode in self.modes {
            text.push_str(&format!("  --{mode}\n"));
        }
        text.push_str("  --popup-edge {top,bottom,left,right}\n  --popup-offset POPUP_OFFSET\n");
        text
    }

    pub fn error_text(&self, prog: &str, message: &str) -> String { format!("{}{prog}: error: {message}\n", self.usage(prog)) }
}

/// The program name argparse shows: basename of argv[0].
pub fn prog(argv0: &str) -> String {
    Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| argv0.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_like_python() {
        for (raw, encoded) in [("", ""), ("a", "YQ=="), ("ab", "YWI="), ("abc", "YWJj"), ("<keyboard />", "PGtleWJvYXJkIC8+")] {
            assert_eq!(b64encode(raw.as_bytes()), encoded);
            assert_eq!(b64decode(encoded).unwrap(), raw.as_bytes());
        }
        assert!(b64decode("YQ").is_err());
        assert!(b64decode("Y").is_err());
    }

    #[test]
    fn quoting_matches_shlex_and_html() {
        assert_eq!(shlex_join(&["/home/u/.local/bin/tessel-tiling", "--settings"]), "/home/u/.local/bin/tessel-tiling --settings");
        assert_eq!(shlex_quote("/home/a b/it's"), "'/home/a b/it'\"'\"'s'");
        assert_eq!(html_escape("'a' & \"b\" <c>"), "&#x27;a&#x27; &amp; &quot;b&quot; &lt;c&gt;");
    }

    #[test]
    fn python_values() {
        assert!(eq_bool(Some(&serde_json::json!(1)), true));
        assert!(!eq_bool(None, false));
        assert_eq!(as_int(Some(&serde_json::json!(true))), None);
        assert_eq!(as_int(Some(&serde_json::json!(1.0))), None);
        assert_eq!(python_int(" 1_7 "), Some(17));
        assert_eq!(python_int("broken"), None);
        assert_eq!(utc_stamp().len(), 22);
    }
}
