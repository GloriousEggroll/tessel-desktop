//! The ElementTree subset calibration.py uses on its managed block:
//! `ET.fromstring` (comments/PIs dropped), `SubElement`, removal,
//! `ET.indent(space='  ')` and `ET.tostring(encoding='unicode')`.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub tag: String,
    pub attrib: Vec<(String, String)>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn new(tag: &str) -> Self {
        Self { tag: tag.into(), ..Default::default() }
    }

    pub fn sub_element(&mut self, tag: &str, attrib: &[(&str, &str)]) {
        let mut element = Element::new(tag);
        element.attrib = attrib.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        self.children.push(element);
    }
}

fn qualified(node: roxmltree::Node) -> String {
    match node.tag_name().namespace() {
        Some(ns) => format!("{{{ns}}}{}", node.tag_name().name()),
        None => node.tag_name().name().to_owned(),
    }
}

fn convert(node: roxmltree::Node) -> Element {
    let mut element = Element::new(&qualified(node));
    element.attrib = node.attributes().map(|a| {
        let name = match a.namespace() { Some(ns) => format!("{{{ns}}}{}", a.name()), None => a.name().to_owned() };
        (name, a.value().to_owned())
    }).collect();
    let mut pending: Option<String> = None;
    for child in node.children() {
        if child.is_text() {
            pending.get_or_insert_with(String::new).push_str(child.text().unwrap_or(""));
        } else if child.is_element() {
            match element.children.last_mut() {
                Some(last) => last.tail = pending.take(),
                None => element.text = pending.take(),
            }
            element.children.push(convert(child));
        }
    }
    match element.children.last_mut() {
        Some(last) => last.tail = pending.take(),
        None => element.text = pending.take(),
    }
    element
}

/// `ET.fromstring(text)`.
pub fn fromstring(text: &str) -> Result<Element, String> {
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let document = roxmltree::Document::parse_with_options(text, options).map_err(|e| e.to_string())?;
    Ok(convert(document.root_element()))
}

/// `ET.indent(tree, space='  ')`.
pub fn indent(element: &mut Element, space: &str) {
    fn walk(element: &mut Element, level: usize, space: &str) {
        if element.children.is_empty() {
            return;
        }
        let child_indent = format!("\n{}", space.repeat(level + 1));
        if element.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
            element.text = Some(child_indent.clone());
        }
        let count = element.children.len();
        for (index, child) in element.children.iter_mut().enumerate() {
            walk(child, level + 1, space);
            if child.tail.as_deref().is_none_or(|t| t.trim().is_empty()) {
                child.tail = Some(if index + 1 == count { format!("\n{}", space.repeat(level)) } else { child_indent.clone() });
            }
        }
    }
    walk(element, 0, space);
}

fn escape_cdata(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// ElementTree `_escape_attrib`.
pub fn escape_attrib(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#09;"),
            c => out.push(c),
        }
    }
    out
}

fn write(element: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&element.tag);
    for (key, value) in &element.attrib {
        out.push_str(&format!(" {key}=\"{}\"", escape_attrib(value)));
    }
    if element.text.as_deref().is_some_and(|t| !t.is_empty()) || !element.children.is_empty() {
        out.push('>');
        if let Some(text) = &element.text {
            out.push_str(&escape_cdata(text));
        }
        for child in &element.children {
            write(child, out);
        }
        out.push_str(&format!("</{}>", element.tag));
    } else {
        out.push_str(" />");
    }
    if let Some(tail) = &element.tail {
        out.push_str(&escape_cdata(tail));
    }
}

/// `ET.tostring(element, encoding='unicode')` (includes the element's tail).
pub fn tostring(element: &Element) -> String {
    let mut out = String::new();
    write(element, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_matches_elementtree() {
        let mut section = fromstring("\n<hdrCalibration>\n<output name=\"A&amp;B\" white=\"1\"/><!-- c --><x>t</x></hdrCalibration>\n").unwrap();
        section.sub_element("output", &[("name", "DP-1"), ("enabled", "false")]);
        indent(&mut section, "  ");
        assert_eq!(tostring(&section), "<hdrCalibration>\n  <output name=\"A&amp;B\" white=\"1\" />\n  <x>t</x>\n  <output name=\"DP-1\" enabled=\"false\" />\n</hdrCalibration>");
        let mut empty = Element::new("hdrCalibration");
        indent(&mut empty, "  ");
        assert_eq!(tostring(&empty), "<hdrCalibration />");
    }
}
