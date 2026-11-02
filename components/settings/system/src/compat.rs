//! Python behaviour the system pages rely on: exception kinds and texts,
//! `str.strip`/`splitlines`/`casefold`, `Path.resolve`, and the expat/minidom
//! view of labwc's rc.xml (built on tessel-tiling's span-keeping parser).
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use tessel_tiling::xml::{self, Element, Node};

/// A Python exception: its class (for `except ValueError`) and `str()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// `ValueError` (also `UnicodeDecodeError`).
    Value(String),
    /// `RuntimeError`.
    Runtime(String),
    /// `OSError` and subclasses.
    Os(String),
    /// `xml.parsers.expat.ExpatError`.
    Xml(String),
    /// `GLib.Error` (D-Bus failures).
    Glib(String),
}

impl Error {
    pub fn message(&self) -> &str {
        match self {
            Error::Value(m) | Error::Runtime(m) | Error::Os(m) | Error::Xml(m) | Error::Glib(m) => m,
        }
    }
    pub fn is_value(&self) -> bool { matches!(self, Error::Value(_)) }
    pub fn is_runtime(&self) -> bool { matches!(self, Error::Runtime(_)) }
    pub fn is_os(&self) -> bool { matches!(self, Error::Os(_)) }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.message()) }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

impl From<glib::Error> for Error {
    fn from(error: glib::Error) -> Self { Error::Glib(tessel_core::pyerr::python_error_text(&error)) }
}

pub fn value<T>(message: impl Into<String>) -> Result<T> { Err(Error::Value(message.into())) }
pub fn runtime<T>(message: impl Into<String>) -> Result<T> { Err(Error::Runtime(message.into())) }

/// `str(OSError)` for an operation on `path`.
pub fn os_error(error: &io::Error, path: &Path) -> Error {
    Error::Os(tessel_tiling::compat::python_os_error_text(error, Some(path)))
}

pub fn os_plain(error: &io::Error) -> Error { Error::Os(tessel_tiling::compat::python_os_error_text(error, None)) }

/// Python `str.isspace()` for one character.
pub fn is_py_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f' | ' ' | '\u{85}' | '\u{a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str { text.trim_matches(is_py_space) }

/// `bytes.strip()` / `bytes.rstrip()` (ASCII whitespace).
pub fn is_byte_space(b: u8) -> bool { matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c') }

pub fn bytes_strip(data: &[u8]) -> &[u8] {
    let start = data.iter().position(|b| !is_byte_space(*b)).unwrap_or(data.len());
    let end = data.iter().rposition(|b| !is_byte_space(*b)).map(|i| i + 1).unwrap_or(start);
    &data[start..end.max(start)]
}

pub fn bytes_rstrip(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|b| !is_byte_space(*b)).map(|i| i + 1).unwrap_or(0);
    &data[..end]
}

/// `str.split()` (no separator).
pub fn split_whitespace(text: &str) -> Vec<&str> { text.split(is_py_space).filter(|s| !s.is_empty()).collect() }

/// `str.split(None, 1)` after `strip()`.
pub fn split_once_whitespace(text: &str) -> (String, Option<String>) {
    let text = strip(text);
    match text.find(is_py_space) {
        None => (text.to_owned(), None),
        Some(index) => {
            let rest = text[index..].trim_start_matches(is_py_space);
            (text[..index].to_owned(), if rest.is_empty() { None } else { Some(rest.to_owned()) })
        }
    }
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `str.splitlines(keepends)`.
pub fn splitlines(text: &str, keepends: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if !is_line_break(c) {
            continue;
        }
        let mut end = index + c.len_utf8();
        if c == '\r' {
            if let Some((_, '\n')) = chars.peek() {
                chars.next();
                end += 1;
            }
        }
        lines.push(if keepends { text[start..end].to_owned() } else { text[start..index].to_owned() });
        start = end;
    }
    if start < text.len() {
        lines.push(text[start..].to_owned());
    }
    lines
}

/// `bytes.splitlines()` (only \n, \r, \r\n).
pub fn bytes_splitlines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < data.len() {
        match data[index] {
            b'\n' => {
                lines.push(&data[start..index]);
                index += 1;
                start = index;
            }
            b'\r' => {
                lines.push(&data[start..index]);
                index += if data.get(index + 1) == Some(&b'\n') { 2 } else { 1 };
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

pub fn casefold(text: &str) -> String { tessel_core::pyjson::casefold(text) }

/// Python `str.lower()` (close enough: Unicode lowercase).
pub fn lower(text: &str) -> String { text.to_lowercase() }

/// `int(text)` for decimal strings (whitespace, sign and `_` as Python).
pub fn py_int(text: &str) -> Result<i64> {
    tessel_tiling::compat::python_int(text).ok_or_else(|| {
        Error::Value(format!("invalid literal for int() with base 10: {}", tessel_tiling::compat::repr_string(text)))
    })
}

/// `float(text)`.
pub fn py_float(text: &str) -> Result<f64> {
    let stripped = strip(text).replace('_', "");
    let lowered = stripped.to_ascii_lowercase();
    let parsed = match lowered.trim_start_matches(['+', '-']) {
        "inf" | "infinity" => Some(if lowered.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY }),
        "nan" => Some(f64::NAN),
        _ if !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c)) => stripped.parse().ok(),
        _ => None,
    };
    parsed.ok_or_else(|| Error::Value(format!("could not convert string to float: {}",
                                              tessel_tiling::compat::repr_string(text))))
}

/// `Path(path).resolve()` (non-strict: resolves the existing prefix).
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(path)
    };
    if let Ok(real) = std::fs::canonicalize(&absolute) {
        return real;
    }
    let mut tail = Vec::new();
    let mut head = absolute.clone();
    loop {
        match (head.parent().map(Path::to_path_buf), head.file_name().map(|n| n.to_os_string())) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                head = parent;
                if let Ok(real) = std::fs::canonicalize(&head) {
                    let mut result = real;
                    for part in tail.iter().rev() {
                        result.push(part);
                    }
                    return result;
                }
            }
            _ => return absolute,
        }
    }
}

/// `datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')`.
pub fn utc_stamp() -> String {
    let now = glib::DateTime::now_utc().expect("UTC time");
    format!("{}{:06}Z", now.format("%Y%m%dT%H%M%S").expect("format"), now.microsecond())
}

/// `_atomic_write(path, data, mode)` of workspace_config.py (no resolve).
pub fn atomic_write_mode(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    tessel_tiling::compat::atomic_write(path, data, mode).map_err(|e| os_error(&e, path))
}

pub fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777)
}

pub fn read_file(path: &Path) -> Result<Vec<u8>> { std::fs::read(path).map_err(|e| os_error(&e, path)) }

// ---- XML ------------------------------------------------------------------

/// `_tag_end(data, start)`: the offset after the tag's `>`, quote-aware.
pub fn tag_end(data: &[u8], start: usize) -> Result<usize> {
    let mut quote = None;
    for (index, value) in data.iter().enumerate().skip(start) {
        match quote {
            Some(q) if *value == q => quote = None,
            Some(_) => {}
            None if *value == b'"' || *value == b'\'' => quote = Some(*value),
            None if *value == b'>' => return Ok(index + 1),
            None => {}
        }
    }
    value("Invalid labwc XML tag.")
}

/// Map a parse failure to its Python exception (ValueError for doctype
/// rejections and decode errors, ExpatError otherwise).
pub fn xml_error(error: xml::XmlError) -> Error {
    if error.line == 0 { Error::Value(error.message) } else { Error::Xml(error.to_string()) }
}

/// expat parse + doctype rejection (`StartDoctypeDeclHandler` raising).
pub fn parse_xml(data: &[u8], doctype: &str) -> Result<xml::Document> {
    if std::str::from_utf8(data).is_err() {
        // `data.decode('utf-8')` runs before expat.
        let error = std::str::from_utf8(data).unwrap_err();
        let position = error.valid_up_to();
        let reason = match error.error_len() {
            None => "unexpected end of data",
            Some(_) if (0x80..0xc2).contains(&data[position]) || data[position] >= 0xf5 => "invalid start byte",
            Some(_) => "invalid continuation byte",
        };
        return value(format!("'utf-8' codec can't decode byte 0x{:02x} in position {position}: {reason}", data[position]));
    }
    // tessel-tiling's parser rejects a declaration after a BOM; expat skips
    // the BOM but keeps counting its bytes in CurrentByteIndex.
    if let Some(body) = data.strip_prefix(b"\xef\xbb\xbf") {
        let mut document = xml::parse(body, Some(doctype)).map_err(|mut e| {
            if e.line == 1 {
                e.column += 1;
            }
            xml_error(e)
        })?;
        shift(&mut document.root, 3);
        return Ok(document);
    }
    xml::parse(data, Some(doctype)).map_err(xml_error)
}

fn shift(element: &mut Element, by: usize) {
    element.start += by;
    element.start_end += by;
    element.close_start += by;
    element.end += by;
    for child in &mut element.children {
        if let Node::Element(e) = child {
            shift(e, by);
        }
    }
}

/// expat's `CurrentByteIndex` in an EndElementHandler: the `</` of the end
/// tag, or just after `/>` for an empty element.
pub fn end_event(element: &Element) -> usize { if element.self_closing { element.end } else { element.close_start } }

/// Local name as expat/minidom report it, lowercased (`name.rsplit(':')[-1].lower()`).
pub fn local_lower(name: &str) -> String { lower(local(name)) }

/// minidom `localName`.
pub fn local(name: &str) -> &str { name.rsplit(':').next().unwrap_or(name) }

/// `XmlDeclHandler`: refuse a non-UTF-8 encoding declaration.
pub fn check_declaration(data: &[u8]) -> Result<()> {
    let body = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    if !body.starts_with(b"<?xml") || !body.get(5).is_some_and(|b| is_byte_space(*b) || *b == b'?') {
        return Ok(());
    }
    let Some(end) = body.windows(2).position(|w| w == b"?>") else { return Ok(()) };
    let decl = String::from_utf8_lossy(&body[..end]);
    let pattern = regex::Regex::new(r#"encoding\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap();
    if let Some(found) = pattern.captures(&decl) {
        let encoding = found.get(1).or_else(|| found.get(2)).map(|m| m.as_str()).unwrap_or("");
        if !encoding.is_empty() && encoding.to_lowercase().replace('-', "") != "utf8" {
            return value("The labwc configuration must use UTF-8.");
        }
    }
    Ok(())
}

/// minidom puts namespace declarations before the other attributes.
pub fn minidom_order(element: &mut Element) {
    let (mut declarations, others): (Vec<_>, Vec<_>) = std::mem::take(&mut element.attributes).into_iter()
        .partition(|(name, _)| name == "xmlns" || name.starts_with("xmlns:"));
    declarations.extend(others);
    element.attributes = declarations;
    for child in &mut element.children {
        if let Node::Element(e) = child {
            minidom_order(e);
        }
    }
}

/// `_elements(node, name)`: indices of element children with that local name (lowercased compare).
pub fn element_indices(element: &Element, name: &str) -> Vec<usize> {
    element.children.iter().enumerate()
        .filter_map(|(i, n)| match n {
            Node::Element(e) if local_lower(&e.name) == name => Some(i),
            _ => None,
        })
        .collect()
}

pub fn child_element(element: &Element, index: usize) -> &Element {
    match &element.children[index] {
        Node::Element(e) => e,
        _ => unreachable!("not an element"),
    }
}

pub fn child_element_mut(element: &mut Element, index: usize) -> &mut Element {
    match &mut element.children[index] {
        Node::Element(e) => e,
        _ => unreachable!("not an element"),
    }
}

/// `_text(node)`: direct text and CDATA children, stripped.
pub fn text(element: &Element) -> String {
    let joined: String = element.children.iter().filter_map(|n| match n {
        Node::Text(t) | Node::CData(t) => Some(t.as_str()),
        _ => None,
    }).collect();
    strip(&joined).to_owned()
}

/// All character data below `element` (expat's buffered `characters`).
pub fn descendant_text(element: &Element) -> String {
    let mut out = String::new();
    fn walk(element: &Element, out: &mut String) {
        for node in &element.children {
            match node {
                Node::Text(t) | Node::CData(t) => out.push_str(t),
                Node::Element(e) => walk(e, out),
                _ => {}
            }
        }
    }
    walk(element, &mut out);
    out
}

/// minidom `setAttribute`: replace in place or append.
pub fn set_attribute(element: &mut Element, name: &str, value: &str) {
    if let Some(slot) = element.attributes.iter_mut().find(|(n, _)| n == name) {
        slot.1 = value.to_owned();
    } else {
        element.attributes.push((name.to_owned(), value.to_owned()));
    }
}

pub fn new_element(name: &str) -> Element {
    Element { name: name.to_owned(), attributes: Vec::new(), children: Vec::new(), start: 0, start_end: 0,
              close_start: 0, end: 0, self_closing: true }
}

pub fn text_element(name: &str, value: &str) -> Element {
    let mut element = new_element(name);
    element.children.push(Node::Text(value.to_owned()));
    element
}

/// Comments in the prolog (before the root start tag), as expat reports them.
pub fn prolog_comments(data: &[u8], root_start: usize) -> Vec<String> {
    let mut found = Vec::new();
    let mut index = 0;
    let head = &data[..root_start];
    while index < head.len() {
        if head[index..].starts_with(b"<!--") {
            let start = index + 4;
            let Some(length) = head[start..].windows(3).position(|w| w == b"-->") else { break };
            found.push(String::from_utf8_lossy(&head[start..start + length]).replace("\r\n", "\n").replace('\r', "\n"));
            index = start + length + 3;
        } else if head[index..].starts_with(b"<?") {
            match head[index..].windows(2).position(|w| w == b"?>") {
                Some(length) => index += length + 2,
                None => break,
            }
        } else {
            index += 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_string_helpers() {
        assert_eq!(splitlines("a\nb\r\nc\rd", true), ["a\n", "b\r\n", "c\r", "d"]);
        assert_eq!(splitlines("a\n", false), ["a"]);
        assert_eq!(strip("\x1c x \u{a0}"), "x");
        assert_eq!(bytes_strip(b" \n"), b"");
        assert_eq!(split_once_whitespace("  us   English (US) "), ("us".into(), Some("English (US)".into())));
        assert_eq!(py_int(" 12 ").unwrap(), 12);
        assert_eq!(py_int("x").unwrap_err().message(), "invalid literal for int() with base 10: 'x'");
        assert!(py_float("nan").unwrap().is_nan());
        assert_eq!(py_float("0.25").unwrap(), 0.25);
        assert!(py_float("abc").is_err());
        assert_eq!(tag_end(b"<a b='>'>", 0).unwrap(), 9);
        assert!(check_declaration(b"<?xml version='1.0' encoding='latin-1'?><a/>").is_err());
        assert!(check_declaration(b"<?xml version='1.0' encoding='UTF-8'?><a/>").is_ok());
        assert_eq!(utc_stamp().len(), "20260101T000000000000Z".len());
    }
}

/// `shlex.split(text)` (POSIX mode, no comments); Err on unbalanced quotes.
pub fn shlex_split(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(q) => current.push(q),
                        None => return value("No closing quotation"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') if matches!(chars.peek(), Some('"' | '\\')) => current.push(chars.next().unwrap()),
                        Some(q) => current.push(q),
                        None => return value("No closing quotation"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(next) => current.push(next),
                    None => return value("No escaped character"),
                }
            }
            c if c == ' ' || c == '\t' || c == '\n' || c == '\r' => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}
