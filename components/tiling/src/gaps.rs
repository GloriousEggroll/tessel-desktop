//! Edit labwc's native tiling gap without rewriting other config sections
//! (tiling_gaps.py `with_gap`).

use crate::compat::{python_int, repr_string};
use crate::xml::{self, Node};

pub const DOCTYPE_REJECTED: &str = "XML document types are not supported in tiling settings.";

/// `(updated XML, previous effective gap)`; accepts `<core gap="N"/>` and
/// `<core><gap>N</gap></core>` (the last `<gap>` child wins).
pub fn with_gap(data: &[u8], gap: i64) -> Result<(Vec<u8>, i64), String> {
    if !(0..=2_147_483_647).contains(&gap) {
        return Err("Invalid window gap.".into());
    }
    if data.len() > 2 * 1024 * 1024 {
        return Err("The labwc configuration is too large.".into());
    }
    let document = xml::parse(data, Some(DOCTYPE_REJECTED)).map_err(|e| e.to_string())?;
    let root = &document.root;
    if root.name != "labwc_config" {
        return Err("Expected labwc configuration.".into());
    }
    let cores: Vec<&xml::Element> = root.elements().filter(|e| e.name == "core").collect();
    if cores.len() > 1 {
        return Err("Multiple core configuration sections are not supported.".into());
    }
    let mut core = match cores.first() {
        Some(core) => (*core).clone(),
        None => xml::Element { name: "core".into(), attributes: Vec::new(), children: Vec::new(), start: 0,
                               start_end: 0, close_start: 0, end: 0, self_closing: true },
    };
    let mut values = vec![core.attribute("gap").filter(|v| !v.is_empty()).unwrap_or("0").to_owned()];
    for child in core.elements().filter(|e| e.name == "gap") {
        let text: String = child.children.iter().filter_map(|n| match n {
            Node::Text(t) | Node::CData(t) => Some(t.as_str()),
            _ => None,
        }).collect();
        values.push(text.trim().to_owned());
    }
    let last = values.last().unwrap();
    let previous = python_int(last)
        .ok_or_else(|| format!("invalid literal for int() with base 10: {}", repr_string(last)))?;
    if !(0..=2_147_483_647).contains(&previous) {
        return Err("Invalid configured window gap.".into());
    }
    core.attributes.retain(|(name, _)| name != "gap");
    core.children.retain(|n| !matches!(n, Node::Element(e) if e.name == "gap"));
    core.children.push(Node::Element(xml::Element {
        name: "gap".into(), attributes: Vec::new(), children: vec![Node::Text(gap.to_string())],
        start: 0, start_end: 0, close_start: 0, end: 0, self_closing: false,
    }));
    let replacement = core.to_xml().into_bytes();
    let updated = if let Some(found) = cores.first() {
        [&data[..found.start], &replacement[..], &data[found.end..]].concat()
    } else if root.self_closing {
        let finish = root.start_end;
        [&data[..finish - 2], b">", &replacement[..], b"</labwc_config>", &data[finish..]].concat()
    } else {
        let closing = root.close_start;
        [&data[..closing], &replacement[..], b"\n", &data[closing..]].concat()
    };
    Ok((updated, previous))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(haystack: &[u8], needle: &[u8]) -> bool { haystack.windows(needle.len()).any(|w| w == needle) }

    #[test]
    fn edit_preserves_unrelated_sections_and_core_settings() {
        for core in [&b"<core><gap>17</gap><decoration>server</decoration><!--keep--></core>"[..],
                     b"<core gap=\"17\"><decoration>server</decoration><!--keep--></core>"] {
            let before = b"<?xml version=\"1.0\"?><labwc_config>\n<!--core example-->";
            let after = b"\n<keyboard><default/></keyboard><desktops number=\"4\"/></labwc_config>";
            let (changed, gap) = with_gap(&[&before[..], core, after].concat(), 0).unwrap();
            assert_eq!(gap, 17);
            assert!(changed.starts_with(before));
            assert!(changed.ends_with(after));
            assert!(contains(&changed, b"<decoration>server</decoration><!--keep-->"));
            assert_eq!(with_gap(&changed, 17).unwrap().1, 0);
            assert_eq!(with_gap(&with_gap(&changed, 17).unwrap().0, 0).unwrap().1, 17);
        }
    }

    #[test]
    fn missing_and_self_closing_core_and_root() {
        for data in [&b"<labwc_config/>"[..], b"<labwc_config />", b"<labwc_config><core /></labwc_config>",
                     b"<labwc_config><!-- <core gap=\"80\"/> --></labwc_config>"] {
            let (changed, gap) = with_gap(data, 0).unwrap();
            assert_eq!(gap, 0);
            assert_eq!(with_gap(&changed, 10).unwrap().1, 0);
            assert!(contains(&with_gap(&changed, 10).unwrap().0, b"<gap>10</gap>"));
        }
        // Byte-exact against the Python implementation's output.
        assert_eq!(with_gap(b"<labwc_config />", 5).unwrap().0, b"<labwc_config ><core><gap>5</gap></core></labwc_config>");
        assert_eq!(with_gap(b"<labwc_config>\n</labwc_config>\n", 5).unwrap().0,
                   b"<labwc_config>\n<core><gap>5</gap></core>\n</labwc_config>\n");
        assert_eq!(with_gap(b"<labwc_config><core>\n  <gap>3</gap>\n  <x a=\"&amp;&#9;\"/></core></labwc_config>", 0).unwrap().0,
                   b"<labwc_config><core>\n  \n  <x a=\"&amp;&#9;\"/><gap>0</gap></core></labwc_config>");
    }

    #[test]
    fn invalid_xml_and_values_do_not_generate_config() {
        for data in [&b"<other/>"[..], b"<labwc_config><core/><core/></labwc_config>",
                     b"<!DOCTYPE labwc_config><labwc_config/>", b"<labwc_config><core gap=\"broken\"/></labwc_config>",
                     b"<labwc_config><core gap=\"-1\"/></labwc_config>", b"<labwc_config><core>", b"\xff"] {
            assert!(with_gap(data, 0).is_err(), "{}", String::from_utf8_lossy(data));
        }
        assert_eq!(with_gap(b"<labwc_config><core gap=\"broken\"/></labwc_config>", 0).unwrap_err(),
                   "invalid literal for int() with base 10: 'broken'");
        assert_eq!(with_gap(b"<!DOCTYPE labwc_config><labwc_config/>", 0).unwrap_err(), DOCTYPE_REJECTED);
        // -1, True, 1.5 and '0' are rejected by the caller's type check (see config::gap_value).
        assert!(with_gap(b"<labwc_config/>", -1).is_err());
        assert!(with_gap(b"<labwc_config/>", 2_147_483_648).is_err());
        assert!(with_gap(&vec![b' '; 2 * 1024 * 1024 + 1], 0).is_err());
    }
}
