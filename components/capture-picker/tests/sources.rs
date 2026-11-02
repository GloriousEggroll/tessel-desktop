//! test_capture_sources.py: portal record fidelity and cancellation.

use serde_json::{json, Map, Value};
use tessel_capture_picker::output::matching_outputs;
use tessel_capture_picker::sources::*;

fn write(source: Option<&CaptureSource>, sources: &[CaptureSource]) -> (Result<(), WriteError>, Vec<u8>) {
    let mut out = Vec::new();
    let result = write_selection(source, sources, &mut out);
    (result, out)
}

#[test]
fn readable_labels_preserve_exact_portal_records() {
    let records = "Monitor: DP-2 Dell Inc. AW3225QF (DP-2)\nMonitor: HDMI-A-1 UGREEN UHD\nWindow: OBS Studio (capture) — 日本語 (opaque:abc-123)\n";
    let sources = parse_sources(records.as_bytes()).unwrap();
    assert_eq!(sources.iter().map(|s| s.kind).collect::<Vec<_>>(), [Kind::Monitor, Kind::Monitor, Kind::Window]);
    assert_eq!(sources[0].title, "Dell Inc. AW3225QF (DP-2)");
    assert_eq!(sources[0].subtitle, "DP-2");
    assert_eq!(sources[2].title, "OBS Studio (capture) — 日本語");
    for (source, record) in sources.iter().zip(records.lines()) {
        let (result, out) = write(Some(source), &sources);
        result.unwrap();
        assert_eq!(out, format!("{record}\n").into_bytes());
    }
}

#[test]
fn duplicate_titles_remain_different_choices() {
    let sources = parse_sources(b"Window: Same title (one)\nWindow: Same title (two)\nWindow: Same title (one)\n").unwrap();
    assert_eq!(sources.len(), 2);
    assert_ne!(sources[0].subtitle, sources[1].subtitle);
}

fn objects(value: Value) -> Vec<Map<String, Value>> {
    value.as_array().unwrap().iter().map(|v| v.as_object().unwrap().clone()).collect()
}

#[test]
fn capture_output_uses_selected_window_not_focused_window() {
    let source = parse_sources(b"Window: Selected (one)\n").unwrap().remove(0);
    let mut windows = objects(json!([{"event": "window", "title": "Selected", "outputs": ["HDMI-A-1"]},
                                     {"event": "window", "title": "Other", "activated": true, "outputs": ["DP-2"]}]));
    assert_eq!(matching_outputs(Some(&source), &windows), ["HDMI-A-1"]);
    windows.extend(objects(json!([{"event": "window", "title": "Selected", "outputs": ["DP-2"]}])));
    assert!(matching_outputs(Some(&source), &windows).is_empty());
}

#[test]
fn missing_labels_and_whitespace_do_not_change_identity() {
    let sources = parse_sources(b"Monitor: DP-1 \nWindow:  (one)\nWindow: a  b\t(c) (two)\n").unwrap();
    assert_eq!(sources.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["DP-1", "one", "a b (c)"]);
    let (result, out) = write(Some(&sources[2]), &sources);
    result.unwrap();
    assert_eq!(out, b"Window: a  b\t(c) (two)\n");
}

#[test]
fn cancel_and_unknown_source_produce_no_output() {
    let sources = parse_sources(b"Monitor: DP-1 Display\n").unwrap();
    let (result, out) = write(None, &sources);
    result.unwrap();
    assert!(out.is_empty());
    let other = parse_sources(b"Monitor: DP-2 Other\n").unwrap().remove(0);
    let (result, out) = write(Some(&other), &sources);
    assert!(matches!(result, Err(WriteError::NotOffered)));
    assert!(out.is_empty());
}

#[test]
fn invalid_or_oversized_records_are_never_selectable() {
    let mut data = b"garbage\nWindow: no identifier\nMonitor: \nMonitor: DP-1 bad\0value\n".to_vec();
    data.extend(format!("Window: {} (id)\n", "x".repeat(MAX_RECORD_BYTES)).into_bytes());
    assert!(parse_sources(&data).unwrap().is_empty());
    assert_eq!(parse_sources(&vec![b'x'; MAX_INPUT_BYTES + 1]), Err(ParseError::TooLarge));
    assert!(matches!(parse_sources(b"Window: \xff (id)\n"), Err(ParseError::Unicode(_))));
    // Exactly at the limit is still a record.
    let token = format!("Window: {} (id)", "x".repeat(MAX_RECORD_BYTES - "Window:  (id)".len()));
    assert_eq!(token.len(), MAX_RECORD_BYTES);
    assert_eq!(parse_sources(token.as_bytes()).unwrap().len(), 1);
}

#[test]
fn python_printable_and_whitespace_rules() {
    // Cf (LRM, ZWSP) and Zs are not printable in Python; they flatten to spaces.
    assert_eq!(display_text("a\u{200e}b\u{200b}c\u{3000}d\x1fe"), "a b c d e");
    assert!(is_printable(' ') && !is_printable('\u{a0}') && !is_printable('\u{e000}') && is_printable('日'));
    // Monitor names must be printable; the description is only a label.
    assert!(parse_sources("Monitor: DP\u{200b}1 x\n".as_bytes()).unwrap().is_empty());
    // A carriage return is part of the record, never stripped.
    let sources = parse_sources(b"Window: t (id)\r\nMonitor: DP-1 desc\r\n").unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].token, "Monitor: DP-1 desc\r");
    // Identifiers cannot contain Python whitespace (U+001C..U+001F included).
    assert!(parse_sources(b"Window: t (a\x1cb)\n").unwrap().is_empty());
}

#[test]
fn window_icons_need_a_unique_app_for_the_title() {
    use gio::prelude::*;
    use tessel_capture_picker::compat::{AppCatalog, AppRecord};
    use tessel_capture_picker::icons::icons_from_snapshot;
    let sources = parse_sources(b"Window: Editor (a)\nWindow: Shared (b)\nWindow: Unknown (c)\nMonitor: Editor x\n").unwrap();
    let windows = objects(json!([
        {"event": "window", "title": "Editor", "app_id": "org.example.Editor"},
        {"event": "window", "title": "Shared", "app_id": "one"},
        {"event": "window", "title": "Shared", "app_id": "two"},
        {"event": "window", "title": "Unknown", "app_id": ""},
        {"event": "output", "title": "Editor x", "app_id": "org.example.Editor"},
    ]));
    let catalog = || AppCatalog::with_records(vec![
        AppRecord { id: "org.example.Editor.desktop".into(), icon: Some(gio::ThemedIcon::new("accessories-text-editor").upcast()), ..Default::default() },
        AppRecord { id: "one.desktop".into(), ..Default::default() },
        AppRecord { id: "two.desktop".into(), ..Default::default() },
    ]);
    let icons = icons_from_snapshot(&sources, &windows, &catalog);
    assert_eq!(icons.len(), 1);
    let icon = icons["Window: Editor (a)"].downcast_ref::<gio::ThemedIcon>().unwrap().names();
    assert_eq!(icon[0], "accessories-text-editor");
    // Reverse-DNS ids are authoritative: no basename collapse; hidden entries skipped.
    let catalog = AppCatalog::with_records(vec![AppRecord { id: "org.other.Editor.desktop".into(), ..Default::default() },
                                                AppRecord { id: "firefox.desktop".into(), startup_wm_class: "firefox-esr".into(), ..Default::default() },
                                                AppRecord { id: "hidden.desktop".into(), hidden: true, ..Default::default() }]);
    assert!(catalog.resolve("org.example.Editor").is_none());
    assert_eq!(catalog.resolve("editor").unwrap().id, "org.other.Editor.desktop");
    assert_eq!(catalog.resolve("FIREFOX-ESR").unwrap().id, "firefox.desktop");
    assert!(catalog.resolve("hidden").is_none());
    // Missing icons fall back like Python.
    assert!(catalog.resolve("firefox").unwrap().icon.is_some());
}
