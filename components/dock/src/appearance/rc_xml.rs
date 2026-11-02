//! Byte-preserving edits of labwc's `rc.xml` `<theme>` element
//! (window_buttons.py `_theme_span`, `theme_value`, `with_theme_value`).
//!
//! Python locates the first top-level `<theme>` with expat byte offsets,
//! re-serializes only that element with `xml.dom.minidom` (`toxml`), and
//! keeps every byte outside it. This module reproduces minidom's output
//! (attribute normalization, `&quot;`/`&#10;` escaping, `<x/>` for empty
//! elements, CDATA/comments/PIs kept) so edited files are byte-identical.

use super::fsx;
use std::ops::Range;

pub const DOCTYPE_ERROR: &str = "XML document types are not supported in the labwc configuration.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    Text(String),
    CData(String),
    Comment(String),
    Pi(String, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Node>,
}

fn normalize(text: &str) -> String {
    fsx::universal_newlines(text)
}

fn qname_at(source: &str, start: usize) -> String {
    source[start + 1..].chars().take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>').collect()
}

/// `tag_end`: index after the `>` closing the tag that starts at `start`.
fn tag_end(source: &[u8], start: usize) -> Result<usize, String> {
    let mut quote = None;
    for (index, value) in source.iter().enumerate().skip(start) {
        match quote {
            Some(q) => {
                if *value == q {
                    quote = None;
                }
            }
            None if *value == b'"' || *value == b'\'' => quote = Some(*value),
            None if *value == b'>' => return Ok(index + 1),
            None => {}
        }
    }
    Err("Invalid labwc XML tag.".into())
}

/// Reject `<!DOCTYPE` in the prolog (expat's StartDoctypeDeclHandler).
fn check_doctype(source: &str) -> Result<(), String> {
    let mut rest = source.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => rest = &rest[end + 3..],
                None => return Ok(()),
            }
        } else if rest.starts_with("<?") {
            match rest.find("?>") {
                Some(end) => rest = &rest[end + 2..],
                None => return Ok(()),
            }
        } else if rest.starts_with("<!DOCTYPE") {
            return Err(DOCTYPE_ERROR.into());
        } else {
            return Ok(());
        }
    }
}

fn parse(data: &[u8]) -> Result<(&str, roxmltree::Document<'_>), String> {
    let source = std::str::from_utf8(data).map_err(|e| format!("not well-formed (invalid token): byte {}", e.valid_up_to()))?;
    check_doctype(source)?;
    let options = roxmltree::ParsingOptions { allow_dtd: false, ..Default::default() };
    let document = roxmltree::Document::parse_with_options(source, options).map_err(|e| e.to_string())?;
    Ok((source, document))
}

fn find_theme<'a, 'input>(source: &str, document: &'a roxmltree::Document<'input>) -> Option<roxmltree::Node<'a, 'input>> {
    document.root_element().children().filter(|n| n.is_element())
        .find(|n| qname_at(source, n.range().start).to_lowercase() == "theme")
}

/// Byte span of the top-level `<theme>` element, or `None`.
pub fn theme_span(data: &[u8]) -> Result<Option<Range<usize>>, String> {
    let (source, document) = parse(data)?;
    Ok(find_theme(source, &document).map(|n| n.range()))
}

fn push_text(raw: &str, children: &mut Vec<Node>) {
    let mut rest = raw;
    while !rest.is_empty() {
        let (text, after) = match rest.find("<![CDATA[") {
            Some(start) => (&rest[..start], Some(&rest[start + 9..])),
            None => (rest, None),
        };
        if !text.is_empty() {
            let text = unescape(&normalize(text));
            match children.last_mut() {
                Some(Node::Text(previous)) => previous.push_str(&text),
                _ => children.push(Node::Text(text)),
            }
        }
        let Some(after) = after else { break };
        let end = after.find("]]>").unwrap_or(after.len());
        children.push(Node::CData(normalize(&after[..end])));
        rest = after.get(end + 3..).unwrap_or("");
    }
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let value = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") => u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32),
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match value {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn build(source: &str, node: roxmltree::Node) -> Result<Element, String> {
    let range = node.range();
    let name = qname_at(source, range.start);
    let attributes = node.attributes().map(|a| (source[a.range_qname()].to_owned(), a.value().to_owned())).collect();
    let mut element = Element { name, attributes, children: Vec::new() };
    let open_end = tag_end(source.as_bytes(), range.start)?;
    if open_end >= range.end || source[range.start..open_end].trim_end().ends_with("/>") {
        return Ok(element);
    }
    let close_start = source[..range.end].rfind('<').unwrap_or(range.end);
    let mut cursor = open_end;
    for child in node.children() {
        if child.is_text() {
            continue;
        }
        let child_range = child.range();
        if child_range.start > cursor {
            push_text(&source[cursor..child_range.start], &mut element.children);
        }
        let raw = &source[child_range.clone()];
        if child.is_element() {
            element.children.push(Node::Element(build(source, child)?));
        } else if child.is_comment() {
            let inner = raw.strip_prefix("<!--").and_then(|r| r.strip_suffix("-->")).unwrap_or("");
            element.children.push(Node::Comment(normalize(inner)));
        } else if child.is_pi() {
            let inner = raw.strip_prefix("<?").and_then(|r| r.strip_suffix("?>")).unwrap_or("");
            let target: String = inner.chars().take_while(|c| !c.is_whitespace()).collect();
            let data = inner[target.len()..].trim_start_matches([' ', '\t', '\r', '\n']);
            element.children.push(Node::Pi(target, normalize(data)));
        }
        cursor = child_range.end.max(cursor);
    }
    if close_start > cursor {
        push_text(&source[cursor..close_start], &mut element.children);
    }
    Ok(element)
}

fn write_data(out: &mut String, text: &str, attribute: bool) {
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

/// minidom `Element.toxml()` (no XML declaration, no added whitespace).
pub fn to_xml(element: &Element) -> String {
    let mut out = String::new();
    write_element(&mut out, element);
    out
}

fn write_element(out: &mut String, element: &Element) {
    out.push('<');
    out.push_str(&element.name);
    for (name, value) in &element.attributes {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        write_data(out, value, true);
        out.push('"');
    }
    if element.children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in &element.children {
        match child {
            Node::Element(child) => write_element(out, child),
            Node::Text(text) => write_data(out, text, false),
            Node::CData(text) => {
                out.push_str("<![CDATA[");
                out.push_str(text);
                out.push_str("]]>");
            }
            Node::Comment(text) => {
                out.push_str("<!--");
                out.push_str(text);
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
    out.push_str("</");
    out.push_str(&element.name);
    out.push('>');
}

fn child_index(element: &Element, name: &str) -> Option<usize> {
    let wanted = name.to_lowercase();
    element.children.iter().position(|c| matches!(c, Node::Element(e) if e.name.to_lowercase() == wanted))
}

fn theme_element(data: &[u8]) -> Result<Option<(Range<usize>, Element)>, String> {
    let (source, document) = parse(data)?;
    let Some(theme) = find_theme(source, &document) else { return Ok(None) };
    // minidom re-parses the fragment on its own: it must be well formed alone.
    let range = theme.range();
    let fragment = &source[range.clone()];
    let options = roxmltree::ParsingOptions { allow_dtd: false, ..Default::default() };
    let fragment_document = roxmltree::Document::parse_with_options(fragment, options).map_err(|e| e.to_string())?;
    Ok(Some((range, build(fragment, fragment_document.root_element())?)))
}

/// Text of a `<theme>` descendant such as `["titlebar", "layout"]`, or `None`.
pub fn theme_value(data: &[u8], path: &[&str]) -> Result<Option<String>, String> {
    let Some((_, theme)) = theme_element(data)? else { return Ok(None) };
    let mut node = &theme;
    for name in path {
        match child_index(node, name) {
            Some(index) => match &node.children[index] {
                Node::Element(child) => node = child,
                _ => unreachable!(),
            },
            None => return Ok(None),
        }
    }
    let text: String = node.children.iter().filter_map(|c| match c { Node::Text(t) => Some(t.as_str()), _ => None }).collect();
    Ok(Some(tessel_core::pyjson::py_strip(&text).to_owned()))
}

/// Set a `<theme>` descendant, keeping the rest of rc.xml byte for byte.
pub fn with_theme_value(data: &[u8], path: &[&str], value: &str) -> Result<Vec<u8>, String> {
    let Some((range, mut theme)) = theme_element(data)? else {
        let root = fsx::rfind_bytes(data, b"</labwc_config>").ok_or("subsection not found")?;
        let mut inner = value.to_owned();
        for name in path.iter().rev() {
            inner = format!("<{name}>{inner}</{name}>");
        }
        return Ok([&data[..root], format!("  <theme>\n    {inner}\n  </theme>\n").as_bytes(), &data[root..]].concat());
    };
    let mut node = &mut theme;
    for (depth, name) in path.iter().enumerate() {
        let index = match child_index(node, name) {
            Some(index) => index,
            None => {
                let child = Node::Element(Element { name: (*name).to_owned(), attributes: Vec::new(), children: Vec::new() });
                if depth == 0 {
                    node.children.push(Node::Text("  ".into()));
                    node.children.push(child);
                    node.children.push(Node::Text("\n  ".into()));
                    node.children.len() - 2
                } else {
                    node.children.push(child);
                    node.children.len() - 1
                }
            }
        };
        node = match &mut node.children[index] {
            Node::Element(child) => child,
            _ => unreachable!(),
        };
    }
    node.children = vec![Node::Text(value.to_owned())];
    Ok([&data[..range.start], to_xml(&theme).as_bytes(), &data[range.end..]].concat())
}

pub fn title_layout(data: &[u8]) -> Result<Option<String>, String> {
    theme_value(data, &["titlebar", "layout"])
}

pub fn with_title_layout(data: &[u8], layout: &str) -> Result<Vec<u8>, String> {
    with_theme_value(data, &["titlebar", "layout"], layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const RC: &[u8] = b"<?xml version=\"1.0\"?>
<labwc_config>
  <!-- keep me -->
  <theme>
    <name>GTK</name>
    <titleLayout>NLIMC</titleLayout>
    <active>
      <titlebar color=\"#282a36\"/>
    </active>
  </theme>
  <desktops number=\"4\"/>
</labwc_config>
";

    fn contains(data: &[u8], needle: &[u8]) -> bool {
        data.windows(needle.len()).any(|w| w == needle)
    }

    fn count(data: &[u8], needle: &[u8]) -> usize {
        data.windows(needle.len()).filter(|w| *w == needle).count()
    }

    #[test]
    fn sets_direct_theme_titlebar_and_keeps_everything_else() {
        assert_eq!(title_layout(RC).unwrap(), None);
        let updated = with_title_layout(RC, "close,iconify,max:").unwrap();
        assert_eq!(title_layout(&updated).unwrap().as_deref(), Some("close,iconify,max:"));
        assert!(contains(&updated, b"<titlebar color=\"#282a36\"/>"));
        assert!(contains(&updated, b"<!-- keep me -->"));
        assert!(contains(&updated, b"<desktops number=\"4\"/>"));
        let again = with_title_layout(&updated, "icon:iconify,max,close").unwrap();
        assert_eq!(title_layout(&again).unwrap().as_deref(), Some("icon:iconify,max,close"));
        assert_eq!(count(&again, b"<layout>"), 1);
        assert_eq!(fsx::replace_bytes(&again, b"icon:iconify,max,close", b"close,iconify,max:"), updated);
    }

    #[test]
    fn adds_theme_when_missing() {
        let data = b"<labwc_config>\n</labwc_config>\n";
        let updated = with_title_layout(data, "close,iconify,max:").unwrap();
        assert_eq!(title_layout(&updated).unwrap().as_deref(), Some("close,iconify,max:"));
        assert!(with_title_layout(b"<other/>", "x").unwrap_err().contains("subsection not found"));
    }

    #[test]
    fn drop_shadows_switch_edits_only_the_theme() {
        assert_eq!(theme_value(RC, &["dropShadows"]).unwrap(), None);
        let on = with_theme_value(RC, &["dropShadows"], "yes").unwrap();
        assert_eq!(theme_value(&on, &["dropShadows"]).unwrap().as_deref(), Some("yes"));
        let off = with_theme_value(&on, &["dropShadows"], "no").unwrap();
        assert_eq!(count(&off, b"<dropShadows>"), 1);
        assert_eq!(fsx::replace_bytes(&off, b"<dropShadows>no<", b"<dropShadows>yes<"), on);
        assert!(contains(&off, b"<titlebar color=\"#282a36\"/>"));
    }

    #[test]
    fn minidom_serialization_details() {
        let data = "<labwc_config><theme a='1\"' b=\"x&#10;y\tz\">t&amp;\"q\"&gt;&#x41;<!-- c --><?pi data?><x  /><y></y><![CDATA[c<d]]>\r\n</theme></labwc_config>".as_bytes();
        let updated = with_theme_value(data, &["y"], "v&<").unwrap();
        assert_eq!(String::from_utf8(updated).unwrap(),
                   "<labwc_config><theme a=\"1&quot;\" b=\"x&#10;y z\">t&amp;\"q\"&gt;A<!-- c --><?pi data?><x/><y>v&amp;&lt;</y><![CDATA[c<d]]>\n</theme></labwc_config>");
    }

    #[test]
    fn doctype_and_malformed_documents_are_rejected() {
        assert_eq!(theme_span(b"<!DOCTYPE labwc_config><labwc_config/>").unwrap_err(), DOCTYPE_ERROR);
        assert!(theme_span(b"<labwc_config><theme></labwc_config>").is_err());
        assert_eq!(theme_span(b"<labwc_config><THEME/></labwc_config>").unwrap(), Some(14..22));
        assert_eq!(theme_span(b"<labwc_config><a><theme/></a></labwc_config>").unwrap(), None);
    }
}
