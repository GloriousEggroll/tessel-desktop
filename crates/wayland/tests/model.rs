//! State tracking with mocked protocol events (no compositor).

use tessel_wayland::compat::{base64, parse_window_request, parse_workspace_request, thumbnail_size};
use tessel_wayland::model::{toplevel_state as ts, workspace_bits as wb, Event, Model, Render, STRING_LIMIT};

fn output(m: &mut Model, global: u32, name: &str) {
    m.output_added(global);
    m.output_name(global, name);
    m.output_description(global, &format!("Display {name}"));
    m.output_scale(global, 1);
    m.output_mode(global, true, 1920, 1080);
    m.output_geometry(global, 0, 0, 0);
}

fn window(m: &mut Model, app: &str, title: &str) -> u64 {
    let id = m.toplevel_new();
    m.toplevel_app_id(id, app);
    m.toplevel_title(id, title);
    m.toplevel_done(id);
    id
}

#[test]
fn windows_publish_on_done_newest_first() {
    let mut m = Model::new();
    let a = m.toplevel_new();
    m.toplevel_title(a, "A");
    assert!(m.take_events().is_empty());
    assert_eq!(m.windows().count(), 0, "unpublished windows are hidden");
    m.toplevel_done(a);
    let b = window(&mut m, "b", "B");
    assert_eq!((a, b), (1, 2));
    assert_eq!(m.take_events(), vec![Event::Window(1), Event::Window(2)]);
    let ids: Vec<_> = m.windows().map(|w| w.id).collect();
    assert_eq!(ids, vec![2, 1], "wl_list_insert at head");
}

#[test]
fn states_replace_previous_flags() {
    let mut m = Model::new();
    let id = window(&mut m, "app", "t");
    m.toplevel_state(id, &[ts::ACTIVATED, ts::MAXIMIZED]);
    let w = m.window(id).unwrap();
    assert!(w.activated && w.maximized && !w.minimized && !w.fullscreen);
    m.toplevel_state(id, &[ts::MINIMIZED, ts::FULLSCREEN, 99]);
    let w = m.window(id).unwrap();
    assert!(!w.activated && !w.maximized && w.minimized && w.fullscreen);
}

#[test]
fn window_json_matches_c_helper() {
    let mut m = Model::new();
    output(&mut m, 7, "HEADLESS-1");
    output(&mut m, 9, "DP-2");
    let parent = window(&mut m, "org.test", "Parent");
    let id = m.toplevel_new();
    m.toplevel_app_id(id, "org.test");
    m.toplevel_title(id, "Quote \" back\\slash\ttab\u{1}é\u{7f}");
    m.toplevel_output_enter(id, 7);
    m.toplevel_output_enter(id, 9);
    m.toplevel_output_enter(id, 9);
    m.toplevel_output_enter(id, 42); // unknown output: ignored
    m.toplevel_parent(id, Some(parent));
    m.toplevel_state(id, &[ts::ACTIVATED]);
    m.toplevel_done(id);
    assert_eq!(m.window_json(id).unwrap(),
        "{\"event\":\"window\",\"id\":\"2\",\"app_id\":\"org.test\",\"title\":\"Quote \\\" back\\\\slash\\u0009tab\\u0001é\u{7f}\",\
         \"activated\":true,\"fullscreen\":false,\"minimized\":false,\"maximized\":false,\"parent\":\"1\",\"outputs\":[\"DP-2\",\"HEADLESS-1\"]}");
    assert_eq!(m.window_json(parent).unwrap().contains("\"parent\":\"\""), true);
    m.toplevel_output_leave(id, 9);
    assert_eq!(m.window_outputs(m.window(id).unwrap()), vec!["HEADLESS-1"]);
    m.toplevel_parent(id, Some(77)); // unknown parent handle → none
    assert_eq!(m.window(id).unwrap().parent, 0);
}

#[test]
fn output_geometry_logical_and_transformed() {
    let mut m = Model::new();
    m.output_added(3);
    m.output_mode(3, false, 800, 600); // not current: ignored
    m.output_mode(3, true, 3840, 2160);
    m.output_scale(3, 2);
    m.output_scale(3, 0); // invalid → 1
    m.output_scale(3, 2);
    m.output_geometry(3, 10, 20, 1); // 90°
    let o = m.output(3).unwrap();
    assert_eq!((o.geometry().width, o.geometry().height, o.geometry().x), (1080, 1920, 10));
    m.output_done(3);
    assert!(m.take_events().is_empty(), "unnamed outputs are not reported");
    m.logical_position(3, 100, 0);
    m.logical_size(3, 1600, 900);
    m.output_geometry(3, 5, 5, 0); // logical position wins once known
    m.output_name(3, "eDP-1");
    let _ = window(&mut m, "a", "a");
    m.take_events();
    m.logical_done(3);
    assert_eq!(m.take_events(), vec![Event::Output(3), Event::Window(1)]);
    assert_eq!(m.output_json(3).unwrap(),
        "{\"event\":\"output\",\"id\":3,\"name\":\"eDP-1\",\"description\":\"\",\"x\":100,\"y\":0,\"width\":1600,\"height\":900,\"scale\":2}");
    assert_eq!(m.output_by_name("eDP-1").unwrap().global, 3);
}

#[test]
fn output_removal_reports_and_reemits_windows() {
    let mut m = Model::new();
    output(&mut m, 1, "A");
    output(&mut m, 2, "B");
    let w = window(&mut m, "x", "x");
    let hidden = m.toplevel_new(); // never published
    m.toplevel_output_enter(w, 1);
    m.toplevel_output_enter(hidden, 1);
    let g = m.group_new();
    m.group_output_enter(g, 1);
    m.take_events();
    assert!(m.output_removed(1));
    assert!(!m.output_removed(1));
    assert_eq!(m.take_events(), vec![Event::OutputRemoved(1), Event::Window(w)]);
    assert!(m.window(w).unwrap().outputs.is_empty() && m.group(g).unwrap().outputs.is_empty());
    assert_eq!(m.outputs().len(), 1);
}

#[test]
fn closing_a_parent_orphans_children() {
    let mut m = Model::new();
    let parent = window(&mut m, "p", "p");
    let child = m.toplevel_new();
    m.toplevel_parent(child, Some(parent));
    m.toplevel_done(child);
    m.take_events();
    m.toplevel_closed(parent);
    assert_eq!(m.take_events(), vec![Event::WindowClosed(parent), Event::Window(child)]);
    assert_eq!(m.window(child).unwrap().parent, 0);
    assert!(m.window(parent).is_none());
    m.toplevel_closed(parent); // already gone: nothing
    assert!(m.take_events().is_empty());
}

#[test]
fn rendered_lines_capture_state_at_event_time() {
    let mut m = Model::new();
    m.render = Some(Render::Toplevel);
    let id = window(&mut m, "a", "first");
    m.toplevel_title(id, "second");
    m.toplevel_done(id);
    m.toplevel_closed(id);
    m.error("boom", false);
    let lines = m.take_lines();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].contains("\"title\":\"first\"") && lines[1].contains("\"title\":\"second\""));
    assert_eq!(lines[2], "{\"event\":\"closed\",\"id\":\"1\"}");
    assert_eq!(lines[3], "{\"event\":\"error\",\"message\":\"boom\"}");
}

#[test]
fn strings_are_limited_like_strndup() {
    let mut m = Model::new();
    let id = m.toplevel_new();
    let long = "é".repeat(STRING_LIMIT); // 2 bytes each
    m.toplevel_title(id, &long);
    let title = &m.window(id).unwrap().title;
    assert_eq!(title.len(), STRING_LIMIT);
    let odd = format!("a{}", "é".repeat(STRING_LIMIT / 2));
    m.toplevel_title(id, &odd);
    assert!(m.window(id).unwrap().title.ends_with('\u{fffd}'), "split sequence → U+FFFD");
}

#[test]
fn workspaces_snapshot_matches_c_helper() {
    let mut m = Model::new();
    output(&mut m, 5, "HEADLESS-1");
    output(&mut m, 6, "HEADLESS-2");
    let g = m.group_new();
    let w1 = m.workspace_new();
    let w2 = m.workspace_new();
    assert_eq!((g, w1, w2), (1, 2, 3), "groups and workspaces share one counter");
    m.group_output_enter(g, 5);
    m.group_output_enter(g, 6);
    m.group_workspace_enter(g, w1);
    m.group_workspace_enter(g, w2);
    m.workspace_name(w1, "1");
    m.workspace_name(w2, "Two");
    m.workspace_state(w1, wb::ACTIVE | wb::URGENT);
    m.workspace_capabilities(w1, wb::CAN_ACTIVATE);
    m.workspace_state(w2, wb::HIDDEN);
    m.workspace_coordinates(w1, &[1, 0, 0, 0, 2, 0, 0, 0]);
    m.workspaces_done();
    assert!(m.take_events().is_empty(), "nothing before publication");
    m.publish_workspaces();
    assert_eq!(m.take_events(), vec![Event::Workspaces]);
    assert_eq!(m.workspaces_json(),
        "{\"event\":\"state\",\"workspaces\":[\
         {\"id\":\"2\",\"name\":\"1\",\"active\":true,\"urgent\":true,\"hidden\":false,\"can_activate\":true,\"group_id\":\"1\",\"coordinates\":[1,2],\"outputs\":[\"HEADLESS-2\",\"HEADLESS-1\"]},\
         {\"id\":\"3\",\"name\":\"Two\",\"active\":false,\"urgent\":false,\"hidden\":true,\"can_activate\":false,\"group_id\":\"1\",\"coordinates\":[],\"outputs\":[\"HEADLESS-2\",\"HEADLESS-1\"]}],\
         \"groups\":[{\"id\":\"1\",\"outputs\":[\"HEADLESS-2\",\"HEADLESS-1\"]}]}");
    m.group_workspace_leave(99, w1); // other group: no effect
    assert_eq!(m.workspace(w1).unwrap().group, g);
    m.group_removed(g);
    assert_eq!(m.workspace(w1).unwrap().group, 0);
    assert!(m.workspaces_json().contains("\"group_id\":\"\",\"coordinates\":[1,2],\"outputs\":[]"));
    m.workspace_removed(w2);
    assert_eq!(m.workspaces().len(), 1);
    m.workspaces_done();
    assert_eq!(m.take_events(), vec![Event::Workspaces]);
}

#[test]
fn malformed_coordinates_are_fatal() {
    let mut m = Model::new();
    let w = m.workspace_new();
    m.workspace_coordinates(w, &[1, 2, 3]);
    m.workspace_coordinates(w, &[0; 1028]);
    let errors: Vec<_> = m.take_events();
    assert_eq!(errors.len(), 2);
    assert!(errors.iter().all(|e| *e == Event::Error { message: "Invalid workspace coordinates.".into(), fatal: true }));
    m.render = Some(Render::Workspaces);
    m.workspace_coordinates(w, &[1]);
    assert_eq!(m.take_lines(), vec!["{\"event\":\"error\",\"message\":\"Invalid workspace coordinates.\"}".to_string()]);
}

#[test]
fn listed_toplevels_track_identity_and_closure() {
    let mut m = Model::new();
    let k = m.listed_new();
    m.listed_identifier(k, "abc");
    m.listed_app_id(k, "org.app");
    m.listed_title(k, "T");
    m.listed_done(k);
    m.listed_closed(k);
    assert_eq!(m.take_events(), vec![Event::Listed(k), Event::ListedClosed(k)]);
    let t = m.listed_toplevel(k).unwrap();
    assert!(t.closed && t.identifier.as_deref() == Some("abc"));
    m.listed_forget(k);
    assert!(m.listed().is_empty());
}

#[test]
fn window_requests_parse_like_sscanf() {
    assert_eq!(parse_window_request(b"activate 12"), Some(("activate".into(), 12)));
    assert_eq!(parse_window_request(b"  close\t7  "), Some(("close".into(), 7)));
    assert_eq!(parse_window_request(b"close +7"), Some(("close".into(), 7)));
    assert_eq!(parse_window_request(b"close -1"), Some(("close".into(), u64::MAX)));
    assert_eq!(parse_window_request(b"close 99999999999999999999999"), Some(("close".into(), u64::MAX)));
    assert_eq!(parse_window_request(b"close 7 x"), None);
    assert_eq!(parse_window_request(b"close"), None);
    assert_eq!(parse_window_request(b""), None);
    assert_eq!(parse_window_request(b"close x7"), None);
    assert_eq!(parse_window_request(b"close 7\r"), Some(("close".into(), 7)));
    assert_eq!(parse_window_request(b"close 7\0 junk"), Some(("close".into(), 7)), "C string ends at NUL");
    // %19s: a longer word continues into the number conversion.
    assert_eq!(parse_window_request(b"abcdefghijklmnopqrs5"), Some(("abcdefghijklmnopqrs".into(), 5)));
    assert_eq!(parse_window_request(b"abcdefghijklmnopqrstu 5"), None);
}

#[test]
fn workspace_requests_are_strict() {
    assert_eq!(parse_workspace_request(b"activate 3"), Ok(3));
    assert_eq!(parse_workspace_request(b"activate 0"), Err("Invalid workspace ID."));
    assert_eq!(parse_workspace_request(b"activate 03"), Err("Invalid workspace ID."));
    assert_eq!(parse_workspace_request(b"activate 3 "), Err("Invalid workspace ID."));
    assert_eq!(parse_workspace_request(b"activate +3"), Err("Invalid workspace ID."));
    assert_eq!(parse_workspace_request(b"activate 99999999999999999999"), Err("Invalid workspace ID."));
    assert_eq!(parse_workspace_request(b"activate"), Err("Invalid workspace request."));
    assert_eq!(parse_workspace_request(b" activate 3"), Err("Invalid workspace request."));
}

#[test]
fn overview_helpers() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64(&[0xff, 0xfe, 0xfd, 0x00]), "//79AA==");
    assert_eq!(thumbnail_size(1280, 800, (640., 400.)), (640, 400));
    assert_eq!(thumbnail_size(300, 200, (640., 400.)), (300, 200), "never upscaled");
    assert_eq!(thumbnail_size(4000, 10, (640., 400.)), (640, 2));
    assert_eq!(thumbnail_size(10000, 1, (640., 400.)), (640, 1), "at least one pixel");
    assert_eq!(thumbnail_size(1920, 1080, (1600., 1000.)), (1600, 900));
}

#[cfg(feature = "capture-png")]
#[test]
fn thumbnails_are_scaled_pngs() {
    use tessel_wayland::Frame;
    let mut data = Vec::new();
    for _ in 0..(1280 * 800) {
        data.extend_from_slice(&[0x20, 0x40, 0x80, 0xff]);
    }
    let frame = Frame { width: 1280, height: 800, stride: 1280 * 4, format: Frame::ARGB8888, transform: 0, data };
    let png = tessel_wayland::compat::thumbnail_png(&frame, true).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 640);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 400);
    let rotated = Frame { transform: 1, ..frame };
    assert!(tessel_wayland::compat::thumbnail_png(&rotated, true).is_none(), "fails closed");
}

#[test]
fn rounded_regions_stay_inside_the_outline() {
    #[cfg(feature = "gtk")]
    {
        use tessel_wayland::blur::rounded_region_rects;
        assert_eq!(rounded_region_rects(1, 2, 30, 40, 0), vec![[1, 2, 30, 40]]);
        let rects = rounded_region_rects(0, 0, 100, 50, 12);
        assert_eq!(rects[0], [0, 12, 100, 26]);
        assert_eq!(rects.len(), 1 + 2 * 12);
        assert!(rects[1][0] > 0 && rects[1][2] < 100, "top row is inset");
        assert_eq!(rounded_region_rects(0, 0, 10, 10, 64)[0], [0, 5, 10, 0], "radius clamps to half the size");
    }
}

#[test]
fn argb_is_preferred_over_xrgb() {
    use tessel_wayland::client::prefer_format;
    use tessel_wayland::Frame;
    let fold = |offers: &[u32]| offers.iter().fold(None, |c, f| prefer_format(c, *f));
    assert_eq!(fold(&[Frame::XRGB8888, Frame::ARGB8888]), Some(Frame::ARGB8888));
    assert_eq!(fold(&[Frame::ARGB8888, Frame::XRGB8888]), Some(Frame::ARGB8888));
    assert_eq!(fold(&[0x34324241, Frame::XRGB8888]), Some(Frame::XRGB8888));
    assert_eq!(fold(&[0x34324241]), None, "other formats are never used");
}
