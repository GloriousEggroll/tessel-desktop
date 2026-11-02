//! A small non-validating XML parser with byte spans, standing in for the
//! expat + `xml.dom.minidom` use of tiling_config.py / tiling_gaps.py.
//!
//! It checks well-formedness the way expat does for the documents labwc
//! reads (tags, attributes, the five predefined entities and character
//! references, comments, CDATA, processing instructions, a DOCTYPE), keeps
//! each element's byte span so edits never re-serialize the rest of the
//! file, and serializes a subtree exactly like minidom's `toxml()`.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Element(Element),
    Text(String),
    CData(String),
    Comment(String),
    Pi(String, String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Node>,
    /// Byte index of `<`.
    pub start: usize,
    /// Byte index just after the start tag's `>`.
    pub start_end: usize,
    /// Byte index of the closing tag's `</` (= `start_end` when self-closing).
    pub close_start: usize,
    /// Byte index just after the element (closing `>`).
    pub end: usize,
    pub self_closing: bool,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub root: Element,
    pub doctype: bool,
}

/// `xml.parsers.expat.ExpatError`-like: `"<message>: line L, column C"`.
#[derive(Clone, Debug, PartialEq)]
pub struct XmlError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: line {}, column {}", self.message, self.line, self.column)
        }
    }
}

impl std::error::Error for XmlError {}

impl Element {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            _ => None,
        })
    }

    /// minidom `Element.toxml()`.
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        write_element(self, &mut out);
        out
    }
}

fn escape(text: &str, attribute: bool, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\r' if attribute => out.push_str("&#13;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\t' if attribute => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
}

fn write_node(node: &Node, out: &mut String) {
    match node {
        Node::Element(e) => write_element(e, out),
        Node::Text(t) => escape(t, false, out),
        Node::CData(t) => {
            out.push_str("<![CDATA[");
            out.push_str(t);
            out.push_str("]]>");
        }
        Node::Comment(t) => {
            out.push_str("<!--");
            out.push_str(t);
            out.push_str("-->");
        }
        Node::Pi(target, data) => {
            out.push_str("<?");
            out.push_str(target);
            out.push(' ');
            out.push_str(data);
            out.push_str("?>");
        }
    }
}

fn write_element(element: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&element.name);
    for (name, value) in &element.attributes {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        escape(value, true, out);
        out.push('"');
    }
    if element.children.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        for child in &element.children {
            write_node(child, out);
        }
        out.push_str("</");
        out.push_str(&element.name);
        out.push('>');
    }
}

struct Parser<'a> {
    data: &'a [u8],
    text: &'a str,
    pos: usize,
    doctype: bool,
    reject_doctype: Option<&'a str>,
}

fn is_name_start(c: char) -> bool { c.is_alphabetic() || c == '_' || c == ':' || (c as u32) >= 0x80 }

fn is_name_char(c: char) -> bool { is_name_start(c) || c.is_ascii_digit() || c == '-' || c == '.' || c == '\u{b7}' }

fn is_space(b: u8) -> bool { matches!(b, b' ' | b'\t' | b'\n' | b'\r') }

fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}')
}

impl<'a> Parser<'a> {
    fn error_at(&self, position: usize, message: &str) -> XmlError {
        let before = &self.data[..position.min(self.data.len())];
        let line = before.iter().filter(|b| **b == b'\n').count() + 1;
        let line_start = before.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
        XmlError { message: message.into(), line, column: position - line_start }
    }

    fn error(&self, message: &str) -> XmlError { self.error_at(self.pos, message) }

    fn starts(&self, prefix: &str) -> bool { self.data[self.pos..].starts_with(prefix.as_bytes()) }

    fn peek(&self) -> Option<u8> { self.data.get(self.pos).copied() }

    fn skip_space(&mut self) -> bool {
        let start = self.pos;
        while self.peek().is_some_and(is_space) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn name(&mut self) -> Result<String, XmlError> {
        let rest = &self.text[self.pos..];
        let mut chars = rest.char_indices();
        match chars.next() {
            Some((_, c)) if is_name_start(c) => {}
            _ => return Err(self.error("not well-formed (invalid token)")),
        }
        let mut length = rest.len();
        for (index, c) in chars {
            if !is_name_char(c) {
                length = index;
                break;
            }
        }
        let name = rest[..length].to_owned();
        self.pos += length;
        Ok(name)
    }

    fn check_chars(&self, start: usize, end: usize) -> Result<(), XmlError> {
        for (index, c) in self.text[start..end].char_indices() {
            if !is_xml_char(c) {
                return Err(self.error_at(start + index, "not well-formed (invalid token)"));
            }
        }
        Ok(())
    }

    /// Decode `&...;` at `self.pos` (which is at `&`).
    fn reference(&mut self) -> Result<String, XmlError> {
        let start = self.pos;
        let Some(length) = self.data[start..].iter().position(|b| *b == b';') else {
            return Err(self.error("not well-formed (invalid token)"));
        };
        let body = &self.text[start + 1..start + length];
        self.pos = start + length + 1;
        let value = match body {
            "lt" => "<".into(),
            "gt" => ">".into(),
            "amp" => "&".into(),
            "apos" => "'".into(),
            "quot" => "\"".into(),
            _ if body.starts_with('#') => {
                let number = match body.strip_prefix("#x") {
                    Some(hex) if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) => u32::from_str_radix(hex, 16).ok(),
                    Some(_) => None,
                    None => {
                        let decimal = &body[1..];
                        if !decimal.is_empty() && decimal.chars().all(|c| c.is_ascii_digit()) { decimal.parse().ok() } else { None }
                    }
                };
                match number.and_then(char::from_u32).filter(|c| is_xml_char(*c)) {
                    Some(c) => c.to_string(),
                    None => return Err(self.error_at(start, "reference to invalid character number")),
                }
            }
            _ if body.chars().next().is_some_and(is_name_start) && body.chars().all(is_name_char) => {
                return Err(self.error_at(start, "undefined entity"));
            }
            _ => return Err(self.error_at(start, "not well-formed (invalid token)")),
        };
        Ok(value)
    }

    fn comment(&mut self) -> Result<Node, XmlError> {
        self.pos += 4;
        let start = self.pos;
        let Some(length) = find(&self.data[start..], b"--") else { return Err(self.error_at(start - 4, "unclosed token")) };
        if !self.data[start + length..].starts_with(b"-->") {
            return Err(self.error_at(start + length, "not well-formed (invalid token)"));
        }
        self.check_chars(start, start + length)?;
        self.pos = start + length + 3;
        Ok(Node::Comment(normalize_newlines(&self.text[start..start + length])))
    }

    fn pi(&mut self) -> Result<Option<Node>, XmlError> {
        let begin = self.pos;
        self.pos += 2;
        let target = self.name()?;
        let Some(length) = find(&self.data[self.pos..], b"?>") else { return Err(self.error_at(begin, "unclosed token")) };
        let end = self.pos + length;
        if self.pos < end && !self.data[self.pos].is_ascii_whitespace() {
            return Err(self.error("not well-formed (invalid token)"));
        }
        self.check_chars(self.pos, end)?;
        let data = self.text[self.pos..end].trim_start_matches([' ', '\t', '\n', '\r']).to_owned();
        self.pos = end + 2;
        if target.eq_ignore_ascii_case("xml") {
            if begin != 0 || target != "xml" {
                return Err(self.error_at(begin, "XML or text declaration not at start of entity"));
            }
            return Ok(None);
        }
        Ok(Some(Node::Pi(target, normalize_newlines(&data))))
    }

    fn doctype(&mut self) -> Result<(), XmlError> {
        if let Some(message) = self.reject_doctype {
            return Err(XmlError { message: message.into(), line: 0, column: 0 });
        }
        let begin = self.pos;
        let mut depth = 0i32;
        let mut quote = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match (quote, b) {
                (Some(q), _) if b == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(b),
                (None, b'[') => depth += 1,
                (None, b']') => depth -= 1,
                (None, b'>') if depth <= 0 => {
                    self.doctype = true;
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(self.error_at(begin, "unclosed token"))
    }

    fn misc(&mut self, after_root: bool) -> Result<bool, XmlError> {
        self.skip_space();
        if self.pos >= self.data.len() {
            return Ok(false);
        }
        if self.starts("<!--") {
            self.comment()?;
            return Ok(true);
        }
        if self.starts("<?") {
            self.pi()?;
            return Ok(true);
        }
        if !after_root && self.starts("<!DOCTYPE") {
            if self.doctype {
                return Err(self.error("syntax error"));
            }
            self.doctype()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn element(&mut self) -> Result<Element, XmlError> {
        let start = self.pos;
        self.pos += 1;
        let name = self.name()?;
        let mut attributes: Vec<(String, String)> = Vec::new();
        loop {
            let spaced = self.skip_space();
            match self.peek() {
                None => return Err(self.error_at(start, "unclosed token")),
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                Some(b'/') => {
                    if self.data.get(self.pos + 1) != Some(&b'>') {
                        return Err(self.error("not well-formed (invalid token)"));
                    }
                    self.pos += 2;
                    return Ok(Element { name, attributes, children: Vec::new(), start, start_end: self.pos,
                                        close_start: self.pos, end: self.pos, self_closing: true });
                }
                Some(_) if !spaced => return Err(self.error("not well-formed (invalid token)")),
                Some(_) => {
                    let attribute_start = self.pos;
                    let key = self.name()?;
                    self.skip_space();
                    if self.peek() != Some(b'=') {
                        return Err(self.error("not well-formed (invalid token)"));
                    }
                    self.pos += 1;
                    self.skip_space();
                    let quote = match self.peek() {
                        Some(q @ (b'"' | b'\'')) => q,
                        _ => return Err(self.error("not well-formed (invalid token)")),
                    };
                    self.pos += 1;
                    let mut value = String::new();
                    loop {
                        match self.peek() {
                            None => return Err(self.error_at(start, "unclosed token")),
                            Some(b) if b == quote => {
                                self.pos += 1;
                                break;
                            }
                            Some(b'<') => return Err(self.error("not well-formed (invalid token)")),
                            Some(b'&') => value.push_str(&self.reference()?),
                            Some(b'\r') => {
                                self.pos += 1;
                                if self.peek() == Some(b'\n') {
                                    self.pos += 1;
                                }
                                value.push(' ');
                            }
                            Some(b'\t' | b'\n') => {
                                self.pos += 1;
                                value.push(' ');
                            }
                            Some(_) => {
                                let c = self.text[self.pos..].chars().next().unwrap();
                                if !is_xml_char(c) {
                                    return Err(self.error("not well-formed (invalid token)"));
                                }
                                value.push(c);
                                self.pos += c.len_utf8();
                            }
                        }
                    }
                    if attributes.iter().any(|(n, _)| *n == key) {
                        return Err(self.error_at(attribute_start, "duplicate attribute"));
                    }
                    attributes.push((key, value));
                }
            }
        }
        let start_end = self.pos;
        let mut children: Vec<Node> = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, children: &mut Vec<Node>| {
            if !text.is_empty() {
                children.push(Node::Text(std::mem::take(text)));
            }
        };
        loop {
            match self.peek() {
                None => return Err(self.error_at(self.data.len(), "no element found")),
                Some(b'<') => {
                    if self.starts("</") {
                        flush(&mut text, &mut children);
                        let close_start = self.pos;
                        self.pos += 2;
                        let close = self.name()?;
                        if close != name {
                            return Err(self.error_at(close_start + 2, "mismatched tag"));
                        }
                        self.skip_space();
                        if self.peek() != Some(b'>') {
                            return Err(self.error("not well-formed (invalid token)"));
                        }
                        self.pos += 1;
                        return Ok(Element { name, attributes, children, start, start_end, close_start, end: self.pos,
                                            self_closing: false });
                    } else if self.starts("<!--") {
                        flush(&mut text, &mut children);
                        children.push(self.comment()?);
                    } else if self.starts("<![CDATA[") {
                        flush(&mut text, &mut children);
                        let begin = self.pos + 9;
                        let Some(length) = find(&self.data[begin..], b"]]>") else {
                            return Err(self.error("unclosed CDATA section"));
                        };
                        self.check_chars(begin, begin + length)?;
                        children.push(Node::CData(normalize_newlines(&self.text[begin..begin + length])));
                        self.pos = begin + length + 3;
                    } else if self.starts("<?") {
                        flush(&mut text, &mut children);
                        match self.pi()? {
                            Some(node) => children.push(node),
                            None => unreachable!(),
                        }
                    } else if self.starts("<!") {
                        return Err(self.error("not well-formed (invalid token)"));
                    } else {
                        flush(&mut text, &mut children);
                        children.push(Node::Element(self.element()?));
                    }
                }
                Some(b'&') => text.push_str(&self.reference()?),
                Some(b'\r') => {
                    self.pos += 1;
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    text.push('\n');
                }
                Some(_) => {
                    if self.starts("]]>") {
                        return Err(self.error("not well-formed (invalid token)"));
                    }
                    let c = self.text[self.pos..].chars().next().unwrap();
                    if !is_xml_char(c) {
                        return Err(self.error("not well-formed (invalid token)"));
                    }
                    text.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }
}

fn normalize_newlines(text: &str) -> String { text.replace("\r\n", "\n").replace('\r', "\n") }

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> { haystack.windows(needle.len()).position(|w| w == needle) }

/// Parse a whole document. `reject_doctype`: the error message to raise when
/// a DOCTYPE is present (tiling_gaps' `StartDoctypeDeclHandler`).
pub fn parse(data: &[u8], reject_doctype: Option<&str>) -> Result<Document, XmlError> {
    let text = std::str::from_utf8(data).map_err(|error| XmlError {
        message: format!("'utf-8' codec can't decode byte 0x{:02x} in position {}: invalid start byte",
                         data[error.valid_up_to()], error.valid_up_to()),
        line: 0, column: 0,
    })?;
    let mut parser = Parser { data, text, pos: 0, doctype: false, reject_doctype };
    if parser.starts("\u{feff}") {
        parser.pos += 3;
    }
    if parser.starts("<?xml") && parser.data.get(5).is_some_and(|b| is_space(*b) || *b == b'?') {
        // The declaration must be the very first thing.
        parser.pi()?;
    }
    while parser.misc(false)? {}
    if parser.pos >= data.len() {
        return Err(parser.error("no element found"));
    }
    if parser.peek() != Some(b'<') {
        return Err(parser.error("syntax error"));
    }
    let root = parser.element()?;
    while parser.misc(true)? {}
    if parser.pos < data.len() {
        return Err(parser.error("junk after document element"));
    }
    Ok(Document { root, doctype: parser.doctype })
}

/// `minidom.parseString(text)` as a well-formedness check.
pub fn parse_str(text: &str) -> Result<Document, XmlError> { parse(text.as_bytes(), None) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toxml_matches_minidom() {
        let doc = parse(b"<a><core x=\"1&amp;\t2&apos;&quot;&gt;\" gap=\"5\">\n <![CDATA[a<b]]> &#65;<?pi data?><!--c--><gap> 3 </gap>\n</core></a>", None).unwrap();
        let core = doc.root.elements().next().unwrap();
        assert_eq!(core.to_xml(), "<core x=\"1&amp; 2'&quot;&gt;\" gap=\"5\">\n <![CDATA[a<b]]> A<?pi data?><!--c--><gap> 3 </gap>\n</core>");
        assert_eq!(parse(b"<labwc_config></labwc_config>", None).unwrap().root.to_xml(), "<labwc_config/>");
        assert_eq!(parse(b"<?xml version=\"1.0\"?>\n<labwc_config />\n", None).unwrap().root.to_xml(), "<labwc_config/>");
    }

    #[test]
    fn spans_point_at_tags() {
        let data = b"<r><core a='1'><gap>2</gap></core><x/></r>";
        let doc = parse(data, None).unwrap();
        let core = doc.root.elements().next().unwrap();
        assert_eq!(&data[core.start..core.start_end], b"<core a='1'>");
        assert_eq!(&data[core.close_start..core.end], b"</core>");
        let x = doc.root.elements().nth(1).unwrap();
        assert!(x.self_closing);
        assert_eq!(&data[x.start..x.end], b"<x/>");
        assert_eq!(&data[doc.root.close_start..], b"</r>");
    }

    #[test]
    fn malformed_documents_are_rejected() {
        for bad in [&b"<a><b></a>"[..], b"<a>", b"", b"<a/><b/>", b"<a x='1' x='2'/>", b"<a>&nope;</a>", b"<a x='<'/>",
                    b"text", b"<a><!-- -- --></a>", b"<a>\x01</a>", b" <?xml version='1.0'?><a/>"] {
            assert!(parse(bad, None).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        assert_eq!(parse(b"<a><b></a>", None).unwrap_err().to_string(), "mismatched tag: line 1, column 8");
        assert!(parse(b"<!DOCTYPE a [<!ENTITY x 'y'>]><a/>", None).unwrap().doctype);
        assert_eq!(parse(b"<!DOCTYPE a><a/>", Some("no")).unwrap_err().to_string(), "no");
    }
}
