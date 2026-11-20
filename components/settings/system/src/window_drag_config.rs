//! Window dragging: which modifier moves (left drag) and resizes (right drag)
//! windows. labwc's built-in defaults bind Super; the rc.xml template adds Alt
//! for moving. A managed block, the last thing in rc.xml's `<mouse>`, keeps only
//! one of them; with no block both work. The rest of the file is kept byte for byte.
//!
//! ```xml
//! <mouse>
//!   …
//!   <!-- Tessel window drag begin: super -->
//!   <context name="Frame">…</context>
//!   <!-- Tessel window drag end -->
//! </mouse>
//! ```
use tessel_core::session_keys::{window_drag_block, window_drag_xml, WINDOW_DRAG_MODES};

use crate::compat::{self, tag_end, value, Result};
use crate::workspace_config::{parse_section, Parsed};

/// Mode id → label (translated by the page), in page order.
pub const CHOICES: [(&str, &str); 3] = [("both", "Super or Alt"), ("super", "Super"), ("alt", "Alt")];

fn parse(data: &[u8]) -> Result<Parsed> {
    parse_section(data, "mouse", "XML document types are not supported in window drag settings.",
                  "Expected one labwc_config with at most one mouse section.")
}

/// The `<mouse>` section as text and where it starts, when rc.xml has one.
fn section<'a>(data: &'a [u8], parsed: &Parsed) -> Result<Option<(usize, &'a str)>> {
    let Some((begin, end)) = parsed.span else { return Ok(None) };
    match std::str::from_utf8(&data[begin..end]) {
        Ok(text) => Ok(Some((begin, text))),
        Err(_) => value("The mouse section of rc.xml is not valid UTF-8."),
    }
}

/// `(start, end, mode)` of the block in the whole file.
fn block(data: &[u8], parsed: &Parsed) -> Result<Option<(usize, usize, &'static str)>> {
    let Some((offset, text)) = section(data, parsed)? else { return Ok(None) };
    match window_drag_block(text) {
        Ok(found) => Ok(found.map(|(start, end, mode)| (offset + start, offset + end, mode))),
        Err(message) => value(message),
    }
}

/// The mode rc.xml has: "both" without a block.
pub fn read_mode(data: &[u8]) -> Result<String> {
    let parsed = parse(data)?;
    Ok(block(data, &parsed)?.map_or("both", |(_, _, mode)| mode).to_owned())
}

fn is_space(byte: u8) -> bool { matches!(byte, b' ' | b'\t' | b'\r' | b'\n') }

/// rc.xml with `mode`. Only the block (and the whitespace before it) changes; a
/// missing or self-closing `<mouse>` becomes one with labwc's defaults. A mode
/// changed elsewhere since `expected` was read is a conflict.
pub fn with_mode(data: &[u8], mode: &str, expected: &str) -> Result<Vec<u8>> {
    if !WINDOW_DRAG_MODES.contains(&mode) {
        return value(format!("Unknown window drag mode: {mode}"));
    }
    let parsed = parse(data)?;
    let found = block(data, &parsed)?;
    let current = found.map_or("both", |(_, _, m)| m);
    if current != expected {
        return compat::runtime("Window dragging changed elsewhere. Reload before applying.");
    }
    let wanted = window_drag_xml(mode);
    let Some(index) = parsed.section else {
        let Some(block) = wanted else { return Ok(data.to_vec()) };
        let mouse = format!("<mouse>\n    <default/>\n    {block}\n  </mouse>").into_bytes();
        let opening = tag_end(data, parsed.doc.root.start)?;
        if compat::bytes_rstrip(&data[..opening]).ends_with(b"/>") {
            let name = parsed.doc.root.name.as_bytes();
            return Ok([&data[..opening - 2], b">\n  ", &mouse[..], b"\n</", name, b">", &data[opening..]].concat());
        }
        let end = parsed.root_end;
        return Ok([&data[..end], b"  ", &mouse[..], b"\n", &data[end..]].concat());
    };
    let element = compat::child_element(&parsed.doc.root, index);
    if element.self_closing {
        let Some(block) = wanted else { return Ok(data.to_vec()) };
        let mouse = format!("<mouse>\n    <default/>\n    {block}\n  </mouse>").into_bytes();
        let (begin, end) = parsed.span.unwrap();
        return Ok([&data[..begin], &mouse[..], &data[end..]].concat());
    }
    // Where the block goes: after the section's last content, before the
    // whitespace that indents the closing tag.
    let close = element.close_start;
    let mut text = data.to_vec();
    if let Some((start, end, _)) = found {
        // Already last and the same: nothing to change.
        if wanted.is_some() && current == mode && data[end..close].iter().all(|b| is_space(*b)) {
            return Ok(text);
        }
        // The block goes with the whitespace that put it on its own line.
        let from = data[..start].iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
        text = [&data[..from], &data[end..]].concat();
    }
    if let Some(block) = wanted {
        let close = text.len() - (data.len() - close);
        let at = text[..close].iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
        text = [&text[..at], format!("\n    {block}").as_bytes(), &text[at..]].concat();
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rc.xml template's mouse section, with the tiling block added to it.
    const CONFIG: &str = r#"<?xml version="1.0"?>
<!-- Keep the rest of this file byte for byte. -->
<labwc_config>
  <touchpad/>
  <mouse>
    <default />
    <!-- Move any window, including fullscreen windows, with Alt+left-drag. -->
    <context name="Frame">
      <mousebind button="A-Left" action="Press">
        <action name="Focus"/>
        <action name="Raise"/>
      </mousebind>
      <mousebind button="A-Left" action="Drag">
        <action name="Move"/>
      </mousebind>
    </context>
    <context name="Root">
      <mousebind button="Right" action="Press">
        <action name="ShowMenu" menu="right-click-menu"/>
      </mousebind>
    </context>
  <!-- Tessel Tiling mouse begin -->
    <context name="Frame">
      <mousebind button="W-Left" action="Drag"><action name="Move"/></mousebind>
    </context>
    <!-- Tessel Tiling mouse end --></mouse>
  <hdrOutputs><output name="DP-2"/></hdrOutputs>
</labwc_config>
"#;

    fn apply(data: &str, mode: &str) -> String {
        let data = data.as_bytes();
        String::from_utf8(with_mode(data, mode, &read_mode(data).unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn every_mode_round_trips_and_only_the_block_changes() {
        assert_eq!(read_mode(CONFIG.as_bytes()).unwrap(), "both");
        for mode in ["super", "alt"] {
            let text = apply(CONFIG, mode);
            assert_eq!(read_mode(text.as_bytes()).unwrap(), mode);
            let block = window_drag_xml(mode).unwrap();
            // Last in <mouse>, after the tiling block, on its own line.
            assert!(text.contains(&format!("<!-- Tessel Tiling mouse end -->\n    {block}</mouse>")), "{text}");
            assert_eq!(text.replace(&format!("\n    {block}"), ""), CONFIG);
            // Same mode again: no change. Back to both: the original file.
            assert_eq!(apply(&text, mode), text);
            assert_eq!(apply(&text, "both"), CONFIG);
            let other = if mode == "super" { "alt" } else { "super" };
            let switched = apply(&text, other);
            assert_eq!(read_mode(switched.as_bytes()).unwrap(), other);
            assert_eq!(switched, apply(CONFIG, other));
        }
        assert_eq!(apply(CONFIG, "both"), CONFIG);
    }

    #[test]
    fn a_block_that_is_no_longer_last_moves_to_the_end() {
        let block = window_drag_xml("alt").unwrap();
        let first = CONFIG.replace("  <!-- Tessel Tiling mouse begin -->", &format!("    {block}\n  <!-- Tessel Tiling mouse begin -->"));
        assert_eq!(read_mode(first.as_bytes()).unwrap(), "alt");
        let text = apply(&first, "alt");
        assert!(text.ends_with(&format!("<!-- Tessel Tiling mouse end -->\n    {block}</mouse>\n  <hdrOutputs><output name=\"DP-2\"/></hdrOutputs>\n</labwc_config>\n")), "{text}");
        assert_eq!(text.matches(tessel_core::session_keys::WINDOW_DRAG_BEGIN).count(), 1);
        assert_eq!(text, apply(CONFIG, "alt"));
        assert_eq!(apply(&first, "both"), CONFIG);
    }

    #[test]
    fn indented_closing_tag_and_one_line_sections() {
        let indented = "<labwc_config>\n  <mouse>\n    <default/>\n  </mouse>\n</labwc_config>\n";
        let text = apply(indented, "super");
        assert_eq!(text, format!("<labwc_config>\n  <mouse>\n    <default/>\n    {}\n  </mouse>\n</labwc_config>\n", window_drag_xml("super").unwrap()));
        assert_eq!(apply(&text, "both"), indented);
        let line = "<labwc_config><mouse><default/></mouse></labwc_config>";
        let text = apply(line, "alt");
        assert_eq!(read_mode(text.as_bytes()).unwrap(), "alt");
        assert_eq!(apply(&text, "both"), line);
    }

    #[test]
    fn missing_or_self_closing_sections_get_labwc_defaults() {
        let expected = |mode: &str| format!("<mouse>\n    <default/>\n    {}\n  </mouse>", window_drag_xml(mode).unwrap());
        assert_eq!(apply("<labwc_config>\n  <keyboard/>\n</labwc_config>\n", "super"),
                   format!("<labwc_config>\n  <keyboard/>\n  {}\n</labwc_config>\n", expected("super")));
        assert_eq!(apply("<labwc_config/>", "alt"), format!("<labwc_config>\n  {}\n</labwc_config>", expected("alt")));
        assert_eq!(apply("<labwc_config><mouse /><x/></labwc_config>", "alt"), format!("<labwc_config>{}<x/></labwc_config>", expected("alt")));
        for data in ["<labwc_config/>", "<labwc_config><mouse/></labwc_config>"] {
            assert_eq!(read_mode(data.as_bytes()).unwrap(), "both");
            assert_eq!(apply(data, "both"), data);
        }
        // Back to both keeps the section, with its defaults.
        let text = apply("<labwc_config><mouse/></labwc_config>", "super");
        assert_eq!(apply(&text, "both"), "<labwc_config><mouse>\n    <default/>\n  </mouse></labwc_config>");
    }

    #[test]
    fn markers_outside_the_mouse_section_are_not_the_block() {
        let block = window_drag_xml("super").unwrap();
        let data = format!("<labwc_config><!-- {} --><mouse><default/></mouse></labwc_config>", block.replace("-->", "- ->").replace("<!--", "<!- -"));
        assert_eq!(read_mode(data.as_bytes()).unwrap(), "both");
        let outside = format!("<labwc_config>{block}<mouse><default/></mouse></labwc_config>");
        assert_eq!(read_mode(outside.as_bytes()).unwrap(), "both");
    }

    #[test]
    fn broken_blocks_unknown_modes_and_conflicts_are_rejected() {
        let block = window_drag_xml("super").unwrap();
        for data in [format!("<labwc_config><mouse>{}</mouse></labwc_config>", block.replace("<!-- Tessel window drag end -->", "")),
                     format!("<labwc_config><mouse>{block}{block}</mouse></labwc_config>"),
                     "<labwc_config><mouse/><mouse/></labwc_config>".into(), "<oops/>".into()] {
            assert!(read_mode(data.as_bytes()).unwrap_err().is_value(), "{data}");
            assert!(with_mode(data.as_bytes(), "both", "both").is_err(), "{data}");
        }
        assert!(with_mode(CONFIG.as_bytes(), "ctrl", "both").unwrap_err().is_value());
        let error = with_mode(CONFIG.as_bytes(), "alt", "super").unwrap_err();
        assert!(error.is_runtime() && error.message().contains("changed elsewhere"));
    }
}
