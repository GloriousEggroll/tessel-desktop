//! Desktop layer in a PRIVATE nested headless labwc with 2 outputs
//! (tests/run_headless_desktop.sh). Ports test_desktop_workarea,
//! test_desktop_visibility, test_desktop_efficiency and the desktop parts of
//! test_shell_integration, plus layer-shell, menu, keyboard, clipboard,
//! rename, sort, wallpaper and mpvpaper parity checks. HOME, XDG dirs and
//! ~/Desktop live under TMPDIR; wl-copy, mpvpaper, systemd-run, xdg-settings
//! and tessel-displays are logging fakes. Skips itself anywhere else.
#![cfg(feature = "desktop")]

use gio::prelude::*;
use gtk::gdk;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_shell::desktop::config;
use tessel_shell::desktop::icon_layout::{contains, point_of, Rect, EDGE_MARGIN as M};
use tessel_shell::desktop::menus;
use tessel_shell::desktop::view::{self, ConfirmRequest, DesktopView};
use tessel_shell::desktop::workarea::{Areas, WorkareaSource};
use tessel_shell::desktop::{launch, Desktops, DesktopsInner};

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while !predicate() && Instant::now() < end {
        settle(0.01);
    }
    predicate()
}

fn positions() -> Map<String, Value> {
    serde_json::from_str::<Value>(&std::fs::read_to_string(config::order_config()).unwrap()).unwrap().as_object().unwrap().clone()
}

fn write_positions(value: Value) {
    std::fs::create_dir_all(config::order_config().parent().unwrap()).unwrap();
    std::fs::write(config::order_config(), value.to_string()).unwrap();
}

fn desktop() -> PathBuf {
    config::desktop_dir()
}

fn reset_desktop(files: &[&str]) {
    let _ = std::fs::remove_dir_all(desktop());
    std::fs::create_dir_all(desktop()).unwrap();
    for name in files {
        std::fs::write(desktop().join(name), name).unwrap();
    }
    let _ = std::fs::remove_file(config::order_config());
    let _ = std::fs::remove_file(config::config_file());
}

fn fake(bin: &Path, name: &str, body: &str) {
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[derive(Default)]
struct Fixture {
    areas: RefCell<HashMap<String, Rect>>,
    known: RefCell<Option<Areas>>,
}

impl WorkareaSource for Fixture {
    fn rectangle(&self, name: Option<&str>, geometry: Rect) -> Rect {
        name.and_then(|n| self.areas.borrow().get(n).copied()).unwrap_or(geometry)
    }
    fn known(&self) -> Areas {
        self.known.borrow().clone().unwrap_or_default()
    }
}

struct Env {
    root: PathBuf,
    bin: PathBuf,
    notes: Rc<RefCell<Vec<(String, String)>>>,
}

fn monitors_sorted() -> Vec<gdk::Monitor> {
    let mut list = tessel_shell::monitors::monitor_list(&gdk::Display::default().unwrap());
    list.sort_by_key(|m| m.geometry().x());
    list
}

fn geometry(monitor: &gdk::Monitor) -> Rect {
    let g = monitor.geometry();
    (g.x() as i64, g.y() as i64, g.width() as i64, g.height() as i64)
}

fn check_icons(views: &[Rc<DesktopView>], fixture: &Fixture) {
    settle(0.1);
    let saved = positions();
    let mut seen = std::collections::BTreeSet::new();
    for view in views {
        let full = geometry(&view.monitor);
        let workarea = fixture.rectangle(view.connector().as_deref(), full);
        for name in view.item_names() {
            assert!(seen.insert(name.clone()), "{name} shown twice");
            let point = point_of(&saved[&name]).unwrap();
            assert!(contains(workarea, point), "{name} {point:?} outside {workarea:?}");
            let (lx, ly) = view.item_position(&name).unwrap();
            assert_eq!((lx as i64 + view.origin.0, ly as i64 + view.origin.1), point, "{name}");
        }
    }
    let files: std::collections::BTreeSet<String> = std::fs::read_dir(desktop()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(seen, files);
}

// ------------------------------------------------------------ layer shell

fn layer_surfaces_and_workareas(env: &Env, app: &gtk::Application) {
    reset_desktop(&["obscured.txt", "clear.txt", "second.txt"]);
    let desktops = Desktops::new(app);
    let fixture = Rc::new(Fixture::default());
    desktops.set_workarea_source(Some(fixture.clone()));
    desktops.start();
    assert!(desktops.started());
    let count = desktops.views.borrow().len();
    desktops.start();
    assert_eq!(desktops.views.borrow().len(), count, "activate is idempotent");
    let monitors = monitors_sorted();
    assert_eq!(monitors.len(), 2, "run with WLR_HEADLESS_OUTPUTS=2");
    settle(0.3);
    let views: Vec<Rc<DesktopView>> = monitors.iter().map(|m| desktops.monitor_desktop(&m.connector().unwrap()).unwrap()).collect();
    for (view, monitor) in views.iter().zip(&monitors) {
        let conn = monitor.connector().unwrap();
        let w = &view.window;
        assert!(w.is_layer_window());
        assert_eq!(w.layer(), Layer::Background);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            assert!(w.is_anchor(edge));
        }
        assert_eq!(w.exclusive_zone(), -1);
        assert_eq!(w.keyboard_mode(), KeyboardMode::OnDemand);
        assert_eq!(w.namespace().unwrap(), format!("desktop-grid-{conn}"));
        assert_eq!(w.widget_name(), format!("desktop-grid-win-{conn}"));
        assert!(w.has_css_class("tessel-shell-desktop"));
        assert_eq!(view.overlay.widget_name(), "main-overlay");
        assert!(view.grid.has_css_class("desktop-grid"));
        assert!(!view.background.can_target() && !view.ghost_layer.can_target());
        assert_eq!(w.monitor().unwrap(), *monitor);
        let g = monitor.geometry();
        assert!(wait_until(|| (w.width(), w.height()) == (g.width(), g.height()), 3.0), "surface spans the physical monitor");
    }

    // --- test_desktop_workarea, step by step.
    let first = geometry(&monitors[0]);
    let second = geometry(&monitors[1]);
    let (x, y, width, height) = first;
    let baseline = json!({"clear.txt": [x + 256, y + 256], "second.txt": [second.0 + 128, second.1 + 128]});
    let connector = monitors[0].connector().unwrap().to_string();
    for (edge, area, obscured) in [
        ("top", (x, y + 52, width, height - 52), [x, y]),
        ("bottom", (x, y, width, height - 52), [x, y + height - 128]),
        ("left", (x + 52, y, width - 52, height), [x, y]),
        ("right", (x, y, width - 52, height), [x + width - 128, y]),
    ] {
        fixture.areas.borrow_mut().insert(connector.clone(), area);
        let mut value = baseline.clone();
        value["obscured.txt"] = json!(obscured);
        write_positions(value);
        desktops.workareas_changed();
        check_icons(&views, &fixture);
        assert_eq!(positions()["clear.txt"], baseline["clear.txt"], "{edge}");
        assert_eq!(positions()["second.txt"], baseline["second.txt"], "{edge}");
    }
    fixture.areas.borrow_mut().insert(connector.clone(), (x, y + 40, width, height - 40));
    let mut value = baseline.clone();
    value["obscured.txt"] = json!([x, y + 40]);
    write_positions(value);
    desktops.workareas_changed();
    fixture.areas.borrow_mut().insert(connector.clone(), (x, y + 52, width, height - 52));
    desktops.workareas_changed();
    assert_eq!(positions()["obscured.txt"], json!([x + M, y + 52 + M]));
    assert_eq!(positions()["second.txt"], baseline["second.txt"]);

    std::fs::write(desktop().join("new.txt"), "new file").unwrap();
    desktops.workareas_changed();
    check_icons(&views, &fixture);
    desktops.sort_all_by_name();
    check_icons(&views, &fixture);
    assert_eq!(positions()["clear.txt"], json!([x + M, y + 52 + M]));

    // A bar changing edges carries the whole arrangement on that monitor.
    let area = fixture.areas.borrow()[&connector];
    fixture.known.replace(Some([(connector.clone(), (first, area))].into()));
    desktops.workareas_changed();
    let before = positions();
    fixture.areas.borrow_mut().insert(connector.clone(), (x + 70, y, width - 70, height));
    fixture.known.replace(Some([(connector.clone(), (first, (x + 70, y, width - 70, height)))].into()));
    desktops.workareas_changed();
    let after = positions();
    for (name, value) in &before {
        let (px, py) = point_of(value).unwrap();
        let moved = tessel_shell::desktop::icon_layout::contains_sized(first, (px, py), (1, 1));
        assert_eq!(point_of(&after[name]).unwrap(), if moved { (px + 70, py - 52) } else { (px, py) }, "{name}");
    }
    check_icons(&views, &fixture);
    fixture.known.replace(None);

    // A drop on the second output is saved in global space and clamped with
    // that output's own reservation (the target comes from the unsnapped point).
    let (sx, sy, sw, sh) = second;
    let second_conn = monitors[1].connector().unwrap().to_string();
    fixture.areas.borrow_mut().insert(second_conn.clone(), (sx + 64, sy, sw - 64, sh));
    let source = &views[0];
    let (gx, gy) = point_of(&positions()["new.txt"]).unwrap();
    source.set_drag_state(false, vec![("new.txt".into(), (gx - source.origin.0, gy - source.origin.1))], (sx + 10 - gx, sy + 145 - gy));
    source.drag_end(&desktop().join("new.txt").to_string_lossy());
    check_icons(&views, &fixture);
    assert_eq!(positions()["new.txt"], json!([sx + 64 + M, sy + 128 + M]));
    assert!(source.item("new.txt").is_none());
    assert!(views[1].item("new.txt").is_some());
    views[1].save_icon_position("new.txt", -500, 99999);
    assert_eq!(positions()["new.txt"], json!([sx + 64 + M, sy + sh - 128 - M]));
    assert_eq!(views[1].snap_to_grid(-500, 99999), (64 + M, sh - 128 - M));
    desktops.workareas_changed();
    check_icons(&views, &fixture);
    assert!(views[1].item("new.txt").is_some());

    // Removing the dock reservation preserves valid placements.
    let saved = positions();
    fixture.areas.borrow_mut().clear();
    desktops.workareas_changed();
    assert_eq!(positions(), saved);
    check_icons(&views, &fixture);

    wallpapers_and_mpvpaper(env, &desktops, &monitors);
    desktops.shutdown();
    assert!(desktops.views.borrow().is_empty());
    settle(0.1);
}

// ---------------------------------------------------------- wallpaper

fn write_png(path: &Path) {
    let (w, h) = (160usize, 120usize);
    let mut data = Vec::with_capacity(w * h * 4);
    for _ in 0..w * h {
        data.extend_from_slice(&[0x33, 0x44, 0x55, 0xff]);
    }
    let texture = gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(data), w * 4);
    texture.save_to_png(path).unwrap();
}

fn wallpapers_and_mpvpaper(env: &Env, desktops: &Rc<Desktops>, monitors: &[gdk::Monitor]) {
    let conn = monitors[0].connector().unwrap().to_string();
    let png = env.root.join("wallpaper.png");
    write_png(&png);
    // Merge-write keeps _view_prefs and unknown keys; colours in GDK form.
    std::fs::create_dir_all(config::config_file().parent().unwrap()).unwrap();
    std::fs::write(config::config_file(), json!({"_view_prefs": {"hide_icons": false, "show_hidden": false}, "unknown": {"x": 1}}).to_string()).unwrap();
    let rgba = gdk::RGBA::parse("#102030").unwrap();
    desktops.apply_wallpaper_to_keys(&[conn.clone()], Some(png.to_str().unwrap()), "Fill", &rgba);
    let text = std::fs::read_to_string(config::config_file()).unwrap();
    assert_eq!(text, format!("{{\n  \"_view_prefs\": {{\n    \"hide_icons\": false,\n    \"show_hidden\": false\n  }},\n  \"unknown\": {{\n    \"x\": 1\n  }},\n  \"{conn}\": {{\n    \"file_path\": \"{}\",\n    \"mode\": \"Fill\",\n    \"color\": \"rgb(16,32,48)\"\n  }}\n}}", png.display()));
    let view = desktops.monitor_desktop(&conn).unwrap();
    assert_eq!(view.overlay.widget_name(), format!("overlay-bg-{conn}"));
    let paintable = desktops.wallpaper_paintable(&conn).expect("still image shown");
    assert_eq!(desktops.wallpaper_color(&conn).unwrap().to_str(), "rgb(16,32,48)");
    assert_eq!(view.background.content_fit(), gtk::ContentFit::Cover);
    // Same file on the second monitor shares the texture (one decode).
    let conn2 = monitors[1].connector().unwrap().to_string();
    desktops.apply_wallpaper_to_keys(&[conn2.clone()], Some(png.to_str().unwrap()), "Fit", &gdk::RGBA::parse("rgba(1,2,3,0.5)").unwrap());
    assert_eq!(desktops.wallpaper_paintable(&conn2).unwrap(), paintable);
    let view2 = desktops.monitor_desktop(&conn2).unwrap();
    assert_eq!(view2.background.content_fit(), gtk::ContentFit::Contain);
    assert_eq!(view2.background.halign(), gtk::Align::Center);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(config::config_file()).unwrap()).unwrap();
    assert_eq!(saved[&conn2]["color"], "rgba(1,2,3,0.5)");
    // Color Only persists file_path null and clears the picture.
    desktops.apply_wallpaper_to_keys(&[conn2.clone()], Some(png.to_str().unwrap()), "Color Only", &rgba);
    assert!(desktops.wallpaper_paintable(&conn2).is_none());
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(config::config_file()).unwrap()).unwrap();
    assert_eq!(saved[&conn2]["file_path"], Value::Null);
    assert_eq!(saved["_view_prefs"], json!({"hide_icons": false, "show_hidden": false}));

    // Video: mpvpaper (FAKE) with the exact argv, then the layer bounce.
    let log = env.root.join("mpvpaper.log");
    fake(&env.bin, "mpvpaper", &format!("for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nexec sleep 30", log.display()));
    let before = desktops.layer_bounces.get();
    desktops.apply_wallpaper_to_keys(&[conn.clone()], Some("/videos/loop.webm"), "Fit", &rgba);
    assert!(desktops.wallpaper_paintable(&conn).is_none());
    assert!(wait_until(|| desktops.layer_bounces.get() == before + 3, 2.0), "idle, +120 ms and +300 ms bounces");
    settle(0.05);
    assert_eq!(view.window.layer(), Layer::Background, "the bounce ends on BACKGROUND");
    assert!(wait_until(|| log.exists(), 3.0));
    assert!(wait_until(|| std::fs::read_to_string(&log).unwrap().lines().count() == 6, 3.0));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), format!("-l\nbackground\n-o\nno-audio osc=no cursor-autohide=no keep-open=yes loop-file=inf loop-playlist=inf keepaspect=yes\n{conn}\n/videos/loop.webm\n"));
    let pid = desktops.mpv.borrow_mut().running(&conn).expect("player running");
    // Back to a still image stops the player.
    desktops.apply_wallpaper_to_keys(&[conn.clone()], Some(png.to_str().unwrap()), "Fill", &rgba);
    assert!(desktops.mpv.borrow_mut().running(&conn).is_none());
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    assert!(stat.is_empty() || stat.contains(") Z "), "player stopped");
    // Missing mpvpaper notifies.
    // (PATH limited to the fake bin dir: the real /usr/bin/mpvpaper must never run.)
    std::fs::remove_file(env.bin.join("mpvpaper")).unwrap();
    let saved_path = std::env::var("PATH").unwrap();
    std::env::set_var("PATH", &env.bin);
    env.notes.borrow_mut().clear();
    desktops.apply_wallpaper_to_keys(&[conn.clone()], Some("/videos/loop.mp4"), "Fill", &rgba);
    std::env::set_var("PATH", saved_path);
    assert!(desktops.mpv.borrow_mut().running(&conn).is_none());
    assert!(env.notes.borrow().iter().any(|(s, b)| s == "Could not play the video wallpaper" && b.contains("mpvpaper")));
}

// -------------------------------------------------------------- visibility

fn visibility(app: &gtk::Application) {
    reset_desktop(&["left.txt", "right.txt", ".secret.txt"]);
    let before: Vec<(String, Vec<u8>)> = std::fs::read_dir(desktop()).unwrap().map(|e| e.unwrap()).map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())).collect();
    std::fs::create_dir_all(config::config_file().parent().unwrap()).unwrap();
    std::fs::write(config::config_file(), json!({"HEADLESS-9": {"file_path": null, "mode": "Color Only", "color": "rgb(0,0,0)"}, "_view_prefs": {"hide_icons": true, "show_hidden": false}}).to_string()).unwrap();
    let inner = DesktopsInner::new(app);
    let monitors = monitors_sorted();
    let mut views = Vec::new();
    let mut saved = Map::new();
    for (monitor, name) in monitors.iter().zip(["left.txt", "right.txt"]) {
        let g = geometry(monitor);
        saved.insert(name.into(), json!([g.0 + 128, g.1 + 128]));
        let view = inner.make_view(monitor, (g.0, g.1), false);
        view.is_grid.set(true);
        inner.register_grid(&monitor.connector().unwrap(), &view);
        views.push(view);
    }
    write_positions(Value::Object(saved));
    for view in &views {
        view.load_view_prefs();
        view.load_icons();
        assert!(view.item_names().is_empty());
        assert_eq!(view.act_hide_icons.state().unwrap().get::<bool>(), Some(true));
    }
    views[0].act_hide_icons.activate(None);
    assert_eq!(views[0].item_names(), ["left.txt"]);
    assert_eq!(views[1].item_names(), ["right.txt"]);
    for view in &views {
        assert!(!view.hide_icons_enabled() && !view.show_hidden());
        assert_eq!(view.act_hide_icons.state().unwrap().get::<bool>(), Some(false));
        assert!(view.items.borrow().iter().all(|i| i.widget.get_visible()));
        view.set_selected_names(view.item_names());
    }
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(config::config_file()).unwrap()).unwrap();
    assert_eq!(cfg["_view_prefs"]["hide_icons"], false);
    assert_eq!(cfg["HEADLESS-9"]["mode"], "Color Only", "other keys survive");

    views[1].act_hide_icons.activate(None);
    for view in &views {
        assert_eq!(view.act_hide_icons.state().unwrap().get::<bool>(), Some(true));
        assert!(view.selected_names().is_empty());
        assert!(view.items.borrow().iter().all(|i| !i.widget.get_visible()));
    }
    let after: Vec<(String, Vec<u8>)> = std::fs::read_dir(desktop()).unwrap().map(|e| e.unwrap()).map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())).collect();
    assert_eq!(before.len(), after.len());

    std::fs::remove_file(desktop().join("left.txt")).unwrap();
    std::fs::write(desktop().join("new.txt"), "new").unwrap();
    for view in &views {
        view.load_icons();
    }
    views[0].act_hide_icons.activate(None);
    let shown: std::collections::BTreeSet<String> = views.iter().flat_map(|v| v.item_names()).collect();
    assert_eq!(shown, ["new.txt".to_string(), "right.txt".into()].into());
    for view in &views {
        assert!(!view.show_hidden());
        assert_eq!(view.act_show_hidden.state().unwrap().get::<bool>(), Some(false));
    }
    views[1].act_show_hidden.activate(None);
    for view in &views {
        assert!(view.show_hidden());
        assert_eq!(view.act_show_hidden.state().unwrap().get::<bool>(), Some(true));
    }
    views[0].act_hide_icons.activate(None);
    views[1].act_hide_icons.activate(None);
    let shown: std::collections::BTreeSet<String> = views.iter().flat_map(|v| v.item_names()).collect();
    assert_eq!(shown, ["new.txt".to_string(), "right.txt".into(), ".secret.txt".into()].into());
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(config::config_file()).unwrap()).unwrap();
    assert_eq!(cfg["_view_prefs"], json!({"hide_icons": false, "show_hidden": true}));
    // Ctrl+H is the window's own shortcut for the same action.
    assert!(views[0].actions.lookup_action("toggle_show_hidden").is_some());
    inner.shutdown();
}

// -------------------------------------------------------------- efficiency

fn efficiency(env: &Env, app: &gtk::Application) {
    let names: Vec<String> = (0..12).map(|i| format!("file-{i:02}.txt")).collect();
    reset_desktop(&[]);
    for name in &names {
        std::fs::write(desktop().join(name), "example").unwrap();
    }
    let inner = DesktopsInner::new(app);
    let monitor = monitors_sorted()[0].clone();
    let view = inner.make_view(&monitor, (0, 0), false);
    view.is_grid.set(true);
    inner.register_grid(&monitor.connector().unwrap(), &view);
    view.load_icons();
    assert_eq!(view.item_names().len(), 12);

    // Unchanged refresh: same widgets, same selection, positions not rewritten.
    let items: Vec<_> = view.items.borrow().clone();
    view.set_selected_names(["file-00.txt".to_string()]);
    let before = std::fs::metadata(config::order_config()).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    for _ in 0..100 {
        view.load_icons();
    }
    assert!(view.items.borrow().iter().zip(&items).all(|(a, b)| Rc::ptr_eq(a, b)));
    assert_eq!(view.selected_names(), ["file-00.txt".to_string()].into());
    assert!(view.item("file-00.txt").unwrap().highlight.has_css_class("selected"));
    assert_eq!(std::fs::metadata(config::order_config()).unwrap().modified().unwrap(), before);

    // Removed or changed icons are released; others survive.
    let old0 = Rc::downgrade(&view.item("file-00.txt").unwrap());
    let old1 = view.item("file-01.txt").unwrap();
    let unchanged = view.item("file-02.txt").unwrap();
    std::fs::remove_file(desktop().join("file-00.txt")).unwrap();
    std::fs::write(desktop().join("file-01.txt"), "different contents").unwrap();
    drop(items);
    view.load_icons();
    assert!(old0.upgrade().is_none(), "removed item released");
    assert!(old1.widget.parent().is_none(), "changed item's widget removed");
    assert!(!Rc::ptr_eq(&old1, &view.item("file-01.txt").unwrap()));
    drop(old1);
    assert!(Rc::ptr_eq(&unchanged, &view.item("file-02.txt").unwrap()));
    assert!(view.item("file-00.txt").is_none());

    // Event bursts coalesce and the refresh waits for the drag.
    view.set_dragging(true);
    std::fs::write(desktop().join("burst.txt"), "x").unwrap();
    for _ in 0..100 {
        view.desktop_changed(gio::FileMonitorEvent::Created);
    }
    settle(0.15);
    assert!(view.item("burst.txt").is_none(), "no reload during the drag");
    assert!(inner.refresh_pending());
    view.set_dragging(false);
    settle(0.15);
    assert!(view.item("burst.txt").is_some());
    assert!(!inner.refresh_pending());
    // Rubber band defers the same way.
    view.set_rubber_drag_active(true);
    std::fs::write(desktop().join("band.txt"), "x").unwrap();
    view.desktop_changed(gio::FileMonitorEvent::Created);
    settle(0.15);
    assert!(view.item("band.txt").is_none());
    view.set_rubber_drag_active(false);
    settle(0.15);
    assert!(view.item("band.txt").is_some());

    // Paste and drop share the async conflict prompt and the worker copy.
    let prompts: Rc<RefCell<Vec<ConfirmRequest>>> = Rc::default();
    let p = prompts.clone();
    view.confirm_hook.replace(Some(Rc::new(move |r| p.borrow_mut().push(r))));
    let source = env.root.join("src");
    let _ = std::fs::remove_dir_all(&source);
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("file-03.txt"), "pasted").unwrap();
    std::fs::write(source.join("new.txt"), "new").unwrap();
    let uris: Vec<String> = ["file-03.txt", "new.txt"].iter().map(|n| gio::File::for_path(source.join(n)).uri().to_string()).collect();
    view.do_paste_from_bytes(&format!("copy\n{}", uris.join("\n")), "x-special/gnome-copied-files");
    assert!(prompts.borrow().is_empty(), "asking never runs a nested loop");
    assert!(wait_until(|| !prompts.borrow().is_empty(), 5.0));
    let first = prompts.borrow()[0].clone();
    assert!(first.title.contains("file-03.txt"));
    assert_eq!(first.buttons, ["Skip", "Keep Both", "Replace"]);
    assert!(first.check_label.is_none() && first.on_dismiss);
    assert!(!desktop().join("new.txt").exists());
    (first.callback)(Some(1), false);
    assert!(wait_until(|| desktop().join("file-03 (1).txt").exists() && desktop().join("new.txt").exists() && view.transfer.borrow().is_none(), 5.0));
    assert_eq!(std::fs::read_to_string(desktop().join("file-03.txt")).unwrap(), "example");
    let saved = positions();
    assert!(saved.contains_key("file-03 (1).txt") && saved.contains_key("new.txt"));
    std::fs::write(source.join("dropped.txt"), "d").unwrap();
    view.set_dropped_files(vec![gio::File::for_path(source.join("dropped.txt"))]);
    view.handle_drop("move");
    assert!(wait_until(|| desktop().join("dropped.txt").exists() && !source.join("dropped.txt").exists(), 5.0), "drop → move");

    // Several conflicts offer apply-to-all; failures show a dialog.
    prompts.borrow_mut().clear();
    for name in ["file-04.txt", "file-05.txt", "file-06.txt"] {
        std::fs::write(source.join(name), "replacement").unwrap();
    }
    view.start_file_transfer(["file-04.txt", "file-05.txt", "file-06.txt"].iter().map(|n| gio::File::for_path(source.join(n))).collect(), "cut", None);
    assert!(wait_until(|| !prompts.borrow().is_empty(), 5.0));
    let first = prompts.borrow()[0].clone();
    assert!(first.check_label.as_deref().unwrap().contains('3'));
    (first.callback)(Some(2), true);
    assert!(wait_until(|| !source.join("file-06.txt").exists() && view.transfer.borrow().is_none(), 5.0));
    assert_eq!(prompts.borrow().len(), 1);
    assert_eq!(std::fs::read_to_string(desktop().join("file-05.txt")).unwrap(), "replacement");
    view.start_file_transfer(vec![gio::File::for_path(source.join("gone.txt"))], "copy", None);
    assert!(wait_until(|| prompts.borrow().len() == 2, 5.0));
    assert_eq!(prompts.borrow()[1].title, "Could not copy 'gone.txt'");
    assert_eq!(prompts.borrow()[1].buttons, ["Close"]);

    // Progress toast cancels and is removed.
    let cancelled = Rc::new(RefCell::new(false));
    let c = cancelled.clone();
    let (row, _, _) = view.add_transfer_row("Copying", Rc::new(move || *c.borrow_mut() = true));
    assert!(view.transfer_toast.borrow().is_some());
    let button = row.last_child().unwrap().downcast::<gtk::Button>().unwrap();
    button.emit_clicked();
    assert!(*cancelled.borrow());
    assert!(!button.is_sensitive());
    view.remove_transfer_row(&row);
    assert!(view.transfer_toast.borrow().is_none());

    // Browser lookup is async; failures notify.
    fake(&env.bin, "xdg-settings", "echo firefox-not-installed.desktop");
    prompts.borrow_mut().clear();
    view.launch_web_browser();
    assert!(prompts.borrow().is_empty(), "lookup is asynchronous");
    assert!(wait_until(|| !prompts.borrow().is_empty(), 5.0));
    assert_eq!(prompts.borrow()[0].title, "Could not open application");
    assert_eq!(prompts.borrow()[0].body, "That application is no longer installed.");
    fake(&env.bin, "xdg-settings", "exit 0");
    env.notes.borrow_mut().clear();
    view.launch_web_browser();
    assert!(wait_until(|| !env.notes.borrow().is_empty(), 5.0));
    assert_eq!(env.notes.borrow()[0].0, "Could not open the web browser");

    // Trash and monitor changes are signal-driven.
    let watcher = inner.trash_watcher();
    let grid = inner.make_view(&monitor, (0, 0), true);
    let id = grid.trash_listener().unwrap();
    assert!(watcher.has_listener(id));
    grid.destroy();
    assert!(!watcher.has_listener(id));
    inner.monitor_snapshot.replace(inner.snapshot_monitors());
    inner.watch_monitors();
    assert!(inner.geometry_watch_count() > 0);
    let rebuilt = Rc::new(RefCell::new(0));
    let r = rebuilt.clone();
    inner.rebuild_hook.replace(Some(Rc::new(move || *r.borrow_mut() += 1)));
    for _ in 0..5 {
        inner.monitors_changed();
    }
    assert!(inner.monitor_check_pending());
    settle(0.35);
    assert!(!inner.monitor_check_pending());
    assert_eq!(*rebuilt.borrow(), 0, "unchanged topology never rebuilds");
    inner.unwatch_monitors();
    assert_eq!(inner.geometry_watch_count(), 0);

    // The wallpaper texture is shared across views and released on destroy.
    let png = env.root.join("wall2.png");
    write_png(&png);
    let rgba = gdk::RGBA::parse("#000000").unwrap();
    view.set_monitor_wallpaper(&monitor, Some(png.to_str().unwrap()), Some("Fill"), &rgba);
    let other = inner.make_view(&monitor, (0, 0), false);
    other.set_monitor_wallpaper(&monitor, Some(png.to_str().unwrap()), Some("Fill"), &rgba);
    let texture = view.background.paintable().unwrap();
    assert_eq!(Some(&texture), other.background.paintable().as_ref());
    let provider = view.wallpaper_provider.borrow().clone().unwrap();
    view.set_monitor_wallpaper(&monitor, Some(png.to_str().unwrap()), Some("Fit"), &rgba);
    assert_eq!(view.background.paintable().as_ref(), Some(&texture));
    assert_eq!(view.wallpaper_provider.borrow().as_ref(), Some(&provider), "one provider per view");
    let weak = texture.downgrade();
    drop(texture);
    let background = view.background.clone();
    view.destroy();
    other.destroy();
    assert!(background.paintable().is_none());
    settle(0.05);
    assert!(weak.upgrade().is_none(), "texture released");
    assert_eq!(inner.textures.borrow_mut().len(), 0);
    assert!(view.style_providers.borrow().is_empty());
    inner.shutdown();
}

// ------------------------------------------------- menus, keys, rename, sort

fn interaction(env: &Env, app: &gtk::Application) {
    reset_desktop(&["a.txt", "b.txt", "c.txt"]);
    std::fs::write(desktop().join("computer.desktop"), "[Desktop Entry]\nType=Application\nName=Computer\nExec=true\n").unwrap();
    std::fs::write(desktop().join("trash-can.desktop"), "[Desktop Entry]\nType=Link\nName=Trash\nURL=trash:///\n").unwrap();
    std::fs::write(desktop().join("app.desktop"), "[Desktop Entry]\nType=Application\nName=Old Name\nExec=true\n").unwrap();
    let inner = DesktopsInner::new(app);
    let monitor = monitors_sorted()[0].clone();
    let g = geometry(&monitor);
    let view = inner.make_view(&monitor, (g.0, g.1), false);
    view.is_grid.set(true);
    inner.register_grid(&monitor.connector().unwrap(), &view);
    view.load_icons();
    assert_eq!(view.item_names().len(), 6);
    // Trash item: icon + label.
    let trash = view.item("trash-can.desktop").unwrap();
    assert!(trash.is_trash);
    assert_eq!(trash.label.text(), "Trash");
    assert_eq!(trash.image.icon_name().unwrap(), "user-trash");
    view.update_trash_status(3);
    assert_eq!(trash.label.text(), "Trash (3)");
    assert_eq!(trash.image.icon_name().unwrap(), "user-trash-full");
    assert_eq!(view.item("app.desktop").unwrap().label.text(), "Old Name");
    // Item widget tree and classes.
    let item = view.item("a.txt").unwrap();
    assert!(item.highlight.has_css_class("label-highlight") && item.label.has_css_class("desktop-label"));
    assert_eq!((item.label.lines(), item.label.max_width_chars()), (2, 14));
    assert_eq!(item.widget.size_request(), (128, 128));

    // Menus: four sections; Configure Displays only with tessel-displays.
    let model = menus::background_model(true);
    assert_eq!(menus::labels(&model), vec![
        vec!["Open Terminal", "Open File Manager", "Open Web Browser"],
        vec!["Hide Icons", "Align to Grid", "Show Hidden", "Sort by"],
        vec!["Cut", "Copy", "Paste", "Move to Trash"],
        vec!["System Monitor", "Settings", "Change Wallpaper", "Configure Displays"],
    ]);
    assert_eq!(menus::labels(&menus::background_model(false))[3], ["System Monitor", "Settings", "Change Wallpaper"]);
    let saved_path = std::env::var("PATH").unwrap();
    std::env::set_var("PATH", &env.bin);
    assert!(!menus::has_display_settings());
    fake(&env.bin, "tessel-displays", "exit 0");
    assert!(menus::has_display_settings());
    std::env::set_var("PATH", saved_path);
    view.set_selected_names(["computer.desktop".to_string()]);
    let group = menus::background_actions(&view);
    for name in ["cut", "copy", "delete_item"] {
        assert!(!group.lookup_action(name).unwrap().is_enabled(), "{name} disabled for protected");
    }
    assert!(group.lookup_action("paste").unwrap().is_enabled());
    let (_, multi) = { view.set_selected_names(["computer.desktop".to_string(), "a.txt".into()]); menus::multi_actions(&view) };
    assert!(!multi.lookup_action("cut_items").unwrap().is_enabled());
    view.set_selected_names(["a.txt".to_string()]);
    assert!(menus::background_actions(&view).lookup_action("cut").unwrap().is_enabled());
    let (menu, group) = menus::item_actions(&view, &desktop().join("a.txt").to_string_lossy());
    assert_eq!(menus::labels(&menu), vec![vec!["Open", "Rename", "Cut", "Copy", "Move to Trash"]]);
    assert!(group.lookup_action("rename").unwrap().is_enabled());
    let (menu, _) = menus::item_actions(&view, &desktop().join("trash-can.desktop").to_string_lossy());
    assert_eq!(menus::labels(&menu), vec![vec!["Open", "Empty Trash"]]);
    let (_, group) = menus::item_actions(&view, &desktop().join("Computer.desktop").to_string_lossy());
    assert!(["rename", "cut", "copy", "delete"].iter().all(|a| !group.lookup_action(a).unwrap().is_enabled()));
    view.set_selected_names(["a.txt".to_string(), "b.txt".into()]);
    let (_, group) = menus::item_actions(&view, &desktop().join("a.txt").to_string_lossy());
    assert!(!group.lookup_action("rename").unwrap().is_enabled());

    // Keyboard: Esc clears; arrows move by cell; Shift extends; Delete asks.
    let none = gdk::ModifierType::empty();
    view.key_press(gdk::Key::Escape, none);
    assert!(view.selected_names().is_empty());
    view.key_press(gdk::Key::Down, none);
    let snapshot = view.grid_snapshot();
    let top_left = snapshot.iter().min_by_key(|(_, r, c)| (*r, *c)).unwrap().0.clone();
    assert_eq!(view.selected_names(), [top_left.clone()].into());
    view.key_press(gdk::Key::Down, none);
    assert_eq!(view.selected_names().len(), 1);
    assert_ne!(view.selected_names(), [top_left.clone()].into());
    view.key_press(gdk::Key::Down, gdk::ModifierType::SHIFT_MASK);
    assert_eq!(view.selected_names().len(), 2);
    view.key_press(gdk::Key::Up, gdk::ModifierType::SHIFT_MASK);
    assert_eq!(view.selected_names().len(), 1, "reverse shrinks");
    let prompts: Rc<RefCell<Vec<ConfirmRequest>>> = Rc::default();
    let p = prompts.clone();
    view.confirm_hook.replace(Some(Rc::new(move |r| p.borrow_mut().push(r))));
    view.key_press(gdk::Key::Delete, none);
    assert_eq!(prompts.borrow()[0].title, "Move to Trash");
    assert_eq!(prompts.borrow()[0].buttons, ["Cancel", "Move to Trash"]);
    (prompts.borrow()[0].callback.clone())(Some(0), false); // never touch any Trash

    // Clipboard: both GDK formats and wl-copy twice (FAKE wl-copy).
    let log = env.root.join("wl-copy.log");
    let _ = std::fs::remove_file(&log);
    fake(&env.bin, "wl-copy", &format!("printf '%s|' \"$@\" >> '{}'; echo >> '{}'", log.display(), log.display()));
    view.set_selected_names(["a.txt".to_string()]);
    assert!(view.key_press(gdk::Key::c, gdk::ModifierType::CONTROL_MASK));
    assert!(wait_until(|| std::fs::read_to_string(&log).map(|t| t.contains("-p|") && t.contains("gnome-copied-files")).unwrap_or(false), 3.0));
    let uri = format!("file://{}", desktop().join("a.txt").display());
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains(&format!("-t|x-special/gnome-copied-files|copy\n{uri}|")), "{text}");
    assert!(text.contains(&format!("-p|-t|text/uri-list|{uri}|")), "{text}");
    let formats = WidgetExt::display(&view.window).clipboard().formats();
    assert!(formats.contain_mime_type("x-special/gnome-copied-files") && formats.contain_mime_type("text/uri-list"));
    std::fs::remove_file(&log).unwrap();
    view.set_selected_names(["computer.desktop".to_string()]);
    view.key_press(gdk::Key::x, gdk::ModifierType::CONTROL_MASK);
    settle(0.3);
    assert!(!log.exists(), "protected items are never cut/copied");
    // Paste the copied file: same-path copy keeps both.
    view.set_selected_names(["a.txt".to_string()]);
    view.multi_copy();
    settle(0.1);
    view.multi_paste();
    assert!(wait_until(|| desktop().join("a (1).txt").exists(), 5.0), "paste reads the clipboard asynchronously");

    // Double click: 350 ms / 8 px.
    let launched: Rc<RefCell<Vec<String>>> = Rc::default();
    let l = launched.clone();
    view.launch_hook.replace(Some(Rc::new(move |p| l.borrow_mut().push(p.to_owned()))));
    let path = desktop().join("b.txt").to_string_lossy().into_owned();
    assert!(!view.release_at(1000.0, (10, 10), &path));
    assert!(view.release_at(1300.0, (14, 14), &path));
    assert!(!view.release_at(1400.0, (14, 14), &path), "a triple click does not reopen");
    assert!(!view.release_at(2000.0, (14, 14), &path));
    assert!(!view.release_at(2400.0, (14, 14), &path), "outside 350 ms");
    assert!(!view.release_at(2500.0, (30, 14), &path), "moved more than 8 px");
    assert_eq!(*launched.borrow(), [path.clone()]);

    // Rename: .desktop edits Name=, invalid names are rejected, position reserved.
    let app_item = view.item("app.desktop").unwrap();
    view.on_rename_commit("  New Name ", &app_item);
    let content = std::fs::read_to_string(desktop().join("app.desktop")).unwrap();
    assert!(content.contains("Name=New Name") && desktop().join("app.desktop").exists());
    assert_eq!(app_item.label.text(), "New Name");
    env.notes.borrow_mut().clear();
    let c_item = view.item("c.txt").unwrap();
    view.on_rename_commit("x/y", &c_item);
    assert_eq!(env.notes.borrow()[0], ("Could not rename 'c.txt'".to_string(), "A name can't contain \"/\".".to_string()));
    assert_eq!(c_item.label.text(), "c.txt");
    for bad in [".", ".."] {
        view.on_rename_commit(bad, &c_item);
    }
    assert!(desktop().join("c.txt").exists());
    let old = positions()["c.txt"].clone();
    view.on_rename_commit("renamed.txt", &c_item);
    assert_eq!(positions()["renamed.txt"], old, "position reserved before the rename finishes");
    assert!(wait_until(|| desktop().join("renamed.txt").exists() && !desktop().join("c.txt").exists(), 3.0));
    settle(0.2);
    assert_eq!(point_of(&positions()["renamed.txt"]), point_of(&old));

    // Sort by type groups folders, desktop types, then extensions.
    std::fs::create_dir(desktop().join("Folder")).unwrap();
    std::fs::write(desktop().join("pic.png"), "").unwrap();
    std::fs::write(desktop().join("doc.pdf"), "").unwrap();
    std::fs::write(desktop().join("notes.md"), "").unwrap();
    std::fs::write(desktop().join("movie.mkv"), "").unwrap();
    std::fs::write(desktop().join("weird.bin"), "").unwrap();
    inner.sort_all_by_type();
    let order: Vec<String> = positions().keys().cloned().collect();
    assert_eq!(order, ["Folder", "app.desktop", "computer.desktop", "trash-can.desktop", "a (1).txt", "a.txt", "b.txt", "notes.md", "renamed.txt", "pic.png", "doc.pdf", "movie.mkv", "weird.bin"]);
    let regions = inner.icon_regions();
    assert_eq!(point_of(&positions()["Folder"]).unwrap(), (regions[0].1 .0, regions[0].1 .1));
    inner.sort_all_by_name();
    let order: Vec<String> = positions().keys().cloned().collect();
    // Display names without extension, lowercased: "a" < "a (1)".
    assert_eq!(&order[..3], ["a.txt", "a (1).txt", "b.txt"]);
    // Display names drive name sort: "New Name" (app.desktop) sorts under N.
    let n = order.iter().position(|n| n == "app.desktop").unwrap();
    let m = order.iter().position(|n| n == "movie.mkv").unwrap();
    assert!(m < n);

    // Wallpaper Settings window: per-monitor controls, Apply merges, Escape/close clear it.
    let conn = monitor.connector().unwrap().to_string();
    std::fs::write(config::config_file(), json!({&conn: {"file_path": "/x/old.png", "mode": "Center", "color": "rgb(1,2,3)"}, "_view_prefs": {"hide_icons": false, "show_hidden": false}}).to_string()).unwrap();
    tessel_shell::desktop::settings::show_wallpaper_manager(&view);
    let window = inner.wallpaper_window.borrow().clone().unwrap();
    assert!(window.window.has_css_class("tessel-shell-wallpaper-settings"));
    assert_eq!(window.window.title().unwrap(), "Wallpaper Settings");
    assert_eq!(window.window.default_size(), (620, 610));
    assert!(window.apply.has_css_class("suggested-action") && window.apply.is_sensitive());
    assert_eq!(window.controls.len(), 2);
    let entry = window.controls.iter().find(|c| c.key == conn).unwrap();
    assert_eq!(entry.mode.selected(), 3, "Center preselected");
    assert_eq!(entry.chooser.label().unwrap(), "old.png");
    assert_eq!(entry.color.rgba().to_str(), "rgb(1,2,3)");
    tessel_shell::desktop::settings::show_wallpaper_manager(&view);
    assert!(Rc::ptr_eq(&window, inner.wallpaper_window.borrow().as_ref().unwrap()), "presented, not duplicated");
    entry.mode.set_selected(5);
    tessel_shell::desktop::settings::apply_wallpaper_settings(&view);
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(config::config_file()).unwrap()).unwrap();
    assert_eq!(cfg[&conn], json!({"file_path": null, "mode": "Color Only", "color": "rgb(1,2,3)"}));
    assert_eq!(cfg["_view_prefs"], json!({"hide_icons": false, "show_hidden": false}));
    assert_eq!(entry.chooser.label().unwrap(), "Choose image or video…");
    window.window.close();
    settle(0.05);
    assert!(inner.wallpaper_window.borrow().is_none());
    inner.shutdown();
}

#[test]
fn desktop_headless() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || runtime.is_empty() || std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var_os("DESKTOP_UI_ISOLATED_TEST").is_none() {
        eprintln!("desktop_headless: skipped (run tests/run_headless_desktop.sh)");
        return;
    }
    let root = std::env::temp_dir().join(format!("ws-desk-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bin = root.join("bin");
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    std::env::set_var("XDG_CACHE_HOME", home.join(".cache"));
    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    assert!(config::config_file().starts_with(&home) && config::order_config().starts_with(&home) && config::desktop_dir().starts_with(&home));
    fake(&bin, "systemd-run", "exit 0");
    launch::set_systemd_run_path(Some(bin.join("systemd-run")));
    let notes: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let n = notes.clone();
    view::set_notify_hook(Some(Rc::new(move |s, b| n.borrow_mut().push((s.into(), b.into())))));
    gtk::init().unwrap();
    let app = gtk::Application::new(None::<&str>, gio::ApplicationFlags::NON_UNIQUE);
    let env = Env { root: root.clone(), bin, notes };
    layer_surfaces_and_workareas(&env, &app);
    visibility(&app);
    efficiency(&env, &app);
    interaction(&env, &app);
    println!("desktop_headless: all scenarios passed");
    let _ = std::fs::remove_dir_all(&root);
}
