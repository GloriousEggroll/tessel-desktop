//! The parts of `xml.etree.ElementTree` the generators use, on top of roxmltree:
//! well-formedness (`ET.fromstring`), expanded tag names, `.text`, `findtext`, `findall`.

pub use roxmltree::{Document, Node};

/// `ET.fromstring(text)`; the error is the parser's message (Python: `ET.ParseError`).
pub fn parse(text: &str) -> Result<Document<'_>, String> {
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    Document::parse_with_options(text, options).map_err(|error| error.to_string())
}

/// ElementTree's `element.tag` (`{uri}name` for namespaced elements).
pub fn tag(node: Node) -> String {
    let name = node.tag_name();
    match name.namespace() {
        Some(uri) => format!("{{{uri}}}{}", name.name()),
        None => name.name().to_owned(),
    }
}

pub fn elements<'a, 'i: 'a>(node: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(|child| child.is_element())
}

/// `element.findall(name)` for a plain child tag.
pub fn findall<'a, 'i: 'a>(node: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    elements(node).filter(move |child| tag(*child) == name)
}

fn joined_text<'a, 'i: 'a>(nodes: impl Iterator<Item = Node<'a, 'i>>) -> Option<String> {
    let mut text: Option<String> = None;
    for node in nodes {
        if node.is_element() {
            break;
        }
        if node.is_text() {
            text.get_or_insert_with(String::new).push_str(node.text().unwrap_or(""));
        }
    }
    text
}

/// `element.text` (comments are dropped and the text around them joined, like TreeBuilder).
pub fn text(node: Node) -> Option<String> {
    joined_text(node.children())
}

/// `element.tail`.
pub fn tail(node: Node) -> Option<String> {
    joined_text(node.next_siblings().skip(1))
}

/// `element.findtext(name)`: `None` when there is no such child, `""` for an empty one.
pub fn findtext(node: Node, name: &str) -> Option<String> {
    elements(node).find(|child| tag(*child) == name).map(|child| text(child).unwrap_or_default())
}

/// `element.get(name)` for an attribute without namespace.
pub fn get<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes().find(|attribute| attribute.namespace().is_none() && attribute.name() == name).map(|a| a.value())
}

/// `sorted(element.attrib.items())` with expanded names.
pub fn sorted_attributes(node: Node) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = node
        .attributes()
        .map(|attribute| {
            let name = match attribute.namespace() {
                Some(uri) => format!("{{{uri}}}{}", attribute.name()),
                None => attribute.name().to_owned(),
            };
            (name, attribute.value().to_owned())
        })
        .collect();
    items.sort();
    items
}
