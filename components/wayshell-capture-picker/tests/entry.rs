//! test_capture_picker_entry.py: only the deliberately accepted portal record
//! may reach stdout. Plus the env hardening and remote-control parsing.

use serde_json::{json, Map, Value};
use std::io::Write;
use wayshell_capture_picker::entry::*;
use wayshell_capture_picker::output::{selected_outputs_with, tiling_outputs, Resolvers};
use wayshell_capture_picker::sources::{parse_sources, CaptureSource};

#[test]
fn chooser_disables_recursive_portals_without_losing_debug_flags() {
    for (existing, expected) in [
        ("", "no-portals"),
        ("portals", "portals,no-portals"),
        ("frames:settings;portals no-portals", "frames,settings,portals,no-portals"),
        ("all,NO-PORTALS,frames", "all,frames"),
        ("ALL\tframes", "ALL,frames"),
    ] {
        assert_eq!(portal_debug_flags(existing), expected);
        assert_eq!(portal_debug_flags(&portal_debug_flags(existing)), expected, "idempotent");
    }
    // The process-level setter (this test binary is single-threaded here).
    std::env::set_var("GDK_DEBUG", "portals");
    std::env::set_var("GTK_USE_PORTAL", "1");
    configure_portal_environment();
    assert_eq!(std::env::var("GDK_DEBUG").unwrap(), "portals,no-portals");
    assert_eq!(std::env::var("GTK_USE_PORTAL").unwrap(), "0");
}

#[test]
fn remote_control_devices_parse_like_python_int() {
    for (value, expected) in [(None, 0), (Some("0"), 0), (Some("3"), 3), (Some("1"), 1), (Some("2"), 2), (Some("7"), 3),
                              (Some(" 6 "), 2), (Some("bogus"), 3), (Some(""), 3), (Some("0x3"), 3), (Some("1_0"), 2),
                              (Some("-1"), 3), (Some("+5"), 1), (Some("١"), 1)] {
        assert_eq!(remote_control_devices_from(value), expected, "{value:?}");
    }
}

struct Outcome {
    status: i32,
    stdout: Vec<u8>,
    stderr: String,
    chosen_with: Vec<u8>,
    preselect_calls: usize,
    reports: Vec<(String, Vec<String>)>,
}

fn run_entry(env: &[(&str, &str)], argv_len: usize, input: &[u8], preselect: Option<(bool, usize)>,
             choose: &mut dyn FnMut(&[CaptureSource], u8, &mut dyn Write) -> Result<Option<CaptureSource>, String>) -> Outcome {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut chosen_with = Vec::new();
    let mut preselect_calls = 0;
    let mut reports = Vec::new();
    let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let lookup = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let mut requested = |_: &str, sources: &[CaptureSource]| {
        preselect_calls += 1;
        match preselect {
            Some((handled, index)) => (handled, sources.get(index).cloned()),
            None => (false, None),
        }
    };
    let mut wrapped = |sources: &[CaptureSource], remote: u8, err: &mut dyn Write| {
        chosen_with.push(remote);
        choose(sources, remote, err)
    };
    let mut report = |session: &str, outputs: &[String]| reports.push((session.to_owned(), outputs.to_vec()));
    let mut outputs = |source: &CaptureSource| vec![source.subtitle.clone()];
    let status = run(argv_len, &mut &input[..], &mut stdout, &mut stderr, &lookup, Hooks {
        requested_selection: &mut requested, choose: &mut wrapped, report_selection: &mut report, selected_outputs: &mut outputs,
    });
    Outcome { status, stdout, stderr: String::from_utf8(stderr).unwrap(), chosen_with, preselect_calls, reports }
}

const ORIGINAL: &[u8] = b"Monitor: DP-1 Test display\n";

#[test]
fn logs_never_replace_the_selected_source() {
    let outcome = run_entry(&[], 1, ORIGINAL, None, &mut |sources, _, err| {
        writeln!(err, "an incidental helper message").unwrap();
        Ok(Some(sources[0].clone()))
    });
    assert_eq!(outcome.status, 0);
    assert_eq!(outcome.stdout, ORIGINAL);
    assert!(outcome.stderr.contains("incidental helper message"));
}

#[test]
fn cancel_and_gui_errors_cannot_grant_access() {
    let outcome = run_entry(&[], 1, ORIGINAL, None, &mut |_, _, _| Ok(None));
    assert_eq!((outcome.status, outcome.stdout.as_slice()), (0, &b""[..]));
    assert!(outcome.reports.is_empty());
    let outcome = run_entry(&[], 1, ORIGINAL, None, &mut |_, _, err| {
        writeln!(err, "diagnostic").unwrap();
        Err("GUI unavailable".into())
    });
    assert_eq!((outcome.status, outcome.stdout.as_slice()), (1, &b""[..]));
    assert!(outcome.stderr.contains("Wayshell capture picker: GUI unavailable"));
}

#[test]
fn remote_control_always_shows_the_picker() {
    let session = "/org/freedesktop/portal/desktop/session/1_9/s1";
    for (value, expected) in [("3", 3), ("1", 1), ("2", 2), ("bogus", 3)] {
        let outcome = run_entry(&[("XDPW_CHOOSER_REMOTE_DESKTOP", value), ("XDPW_CHOOSER_SESSION", session)], 1, ORIGINAL,
                                Some((true, 0)), &mut |sources, _, _| Ok(Some(sources[0].clone())));
        assert_eq!(outcome.preselect_calls, 0, "requester answered for itself");
        assert_eq!((outcome.status, outcome.stdout.as_slice(), outcome.chosen_with.as_slice()), (0, ORIGINAL, &[expected][..]));
    }
}

#[test]
fn preselection_skips_the_ui_and_reports_outputs() {
    let session = "/org/freedesktop/portal/desktop/session/1_9/capture_ab12";
    let outcome = run_entry(&[("XDPW_CHOOSER_SESSION", session)], 1, ORIGINAL, Some((true, 0)), &mut |_, _, _| panic!("no UI"));
    assert_eq!((outcome.status, outcome.stdout.as_slice()), (0, ORIGINAL));
    assert_eq!(outcome.reports, [(session.to_owned(), vec!["DP-1".to_owned()])]);
    // Handled without a token: a vanished output cancels (no UI, no output).
    let outcome = run_entry(&[("XDPW_CHOOSER_SESSION", session)], 1, ORIGINAL, Some((true, 99)), &mut |_, _, _| panic!("no UI"));
    assert_eq!((outcome.status, outcome.stdout.as_slice()), (0, &b""[..]));
    // Not handled: the picker opens.
    let outcome = run_entry(&[("XDPW_CHOOSER_SESSION", session)], 1, ORIGINAL, Some((false, 0)), &mut |_, _, _| Ok(None));
    assert_eq!((outcome.chosen_with.as_slice(), outcome.stdout.as_slice()), (&[0u8][..], &b""[..]));
}

#[test]
fn argv_and_input_errors_exit_one_without_output() {
    let outcome = run_entry(&[], 2, ORIGINAL, None, &mut |_, _, _| panic!("no UI"));
    assert_eq!((outcome.status, outcome.stdout.as_slice()), (1, &b""[..]));
    assert!(outcome.stderr.contains("This chooser takes its portal source list on standard input."));
    let big = vec![b'x'; 4 * 1024 * 1024 + 10];
    let outcome = run_entry(&[], 1, &big, None, &mut |_, _, _| panic!("no UI"));
    assert_eq!((outcome.status, outcome.stdout.as_slice()), (1, &b""[..]));
    assert!(outcome.stderr.contains("too many capture sources"));
    let outcome = run_entry(&[], 1, b"Window: \xff (id)\n", None, &mut |_, _, _| panic!("no UI"));
    assert_eq!(outcome.status, 1);
    assert!(outcome.stderr.contains("'utf-8' codec can't decode byte 0xff in position 8: invalid start byte"), "{}", outcome.stderr);
    // A choice not offered by the portal is an error, never output.
    let outcome = run_entry(&[], 1, ORIGINAL, None, &mut |_, _, _| Ok(parse_sources(b"Monitor: DP-2 x\n").unwrap().pop()));
    assert_eq!((outcome.status, outcome.stdout.as_slice()), (1, &b""[..]));
}

fn object(value: Value) -> Map<String, Value> { value.as_object().unwrap().clone() }

#[test]
fn window_outputs_prefer_tiling_identifier_then_unambiguous_title() {
    let source = parse_sources(b"Window: Editor (ext-42)\n").unwrap().remove(0);
    let monitors = || vec![("DP-1".to_owned(), 0, 0, 2560, 1440), ("HDMI-A-1".to_owned(), 2560, 0, 1920, 1080)];
    let snapshot = || Some(vec![object(json!({"event": "window", "title": "Editor", "outputs": ["DP-1"]}))]);
    let commands = std::cell::RefCell::new(Vec::new());
    let commands = &commands;
    let reply = |value: Value| {
        move |command: &str| {
            commands.borrow_mut().push(command.to_owned());
            Ok::<_, String>(object(value.clone()))
        }
    };
    let supported = reply(json!({"capture_output_supported": true, "capture_output": {"x": 2560, "y": 0, "width": 1920, "height": 1080}}));
    assert_eq!(selected_outputs_with(Some(&source), &Resolvers { tiling: &supported, monitors: &monitors, snapshot: &snapshot }), ["HDMI-A-1"]);
    assert_eq!(commands.borrow().as_slice(), ["capture-output ext-42"]);
    // Supported but no box (or no matching monitor): nothing, no title guess.
    let none = reply(json!({"capture_output_supported": true, "capture_output": null}));
    assert!(selected_outputs_with(Some(&source), &Resolvers { tiling: &none, monitors: &monitors, snapshot: &snapshot }).is_empty());
    // Unsupported or failing compositor: the helper title match.
    let old = reply(json!({"capture_output_supported": false}));
    assert_eq!(selected_outputs_with(Some(&source), &Resolvers { tiling: &old, monitors: &monitors, snapshot: &snapshot }), ["DP-1"]);
    let failing = |_: &str| Err::<Map<String, Value>, _>("Sign out".to_owned());
    assert_eq!(selected_outputs_with(Some(&source), &Resolvers { tiling: &failing, monitors: &monitors, snapshot: &snapshot }), ["DP-1"]);
    let missing = || None;
    assert!(selected_outputs_with(Some(&source), &Resolvers { tiling: &failing, monitors: &monitors, snapshot: &missing }).is_empty());
    // A box missing a key (Python KeyError) falls back too.
    assert_eq!(tiling_outputs(&object(json!({"capture_output_supported": true, "capture_output": {"x": 1}})), &monitors()), None);
    // Monitors never query the compositor.
    let monitor = parse_sources(b"Monitor: DP-1 Dell\n").unwrap().remove(0);
    let never = |_: &str| -> Result<Map<String, Value>, String> { panic!("no tiling request for monitors") };
    assert_eq!(selected_outputs_with(Some(&monitor), &Resolvers { tiling: &never, monitors: &monitors, snapshot: &missing }), ["DP-1"]);
}
