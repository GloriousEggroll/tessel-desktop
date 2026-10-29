//! Port of test_fullscreen_guard.py (pure policy + cache) and the dock_ui
//! "menu hotkey reads cached fullscreen state without waiting for X11" case.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn backend_with(change: impl FnOnce(&mut Window)) -> WindowsSnapshot {
    let mut window = Window {
        id: "native:1".into(), app_id: "Game".into(), title: "A game".into(), activated: true,
        outputs: vec!["DP-1".into()], ..Default::default()
    };
    change(&mut window);
    let mut output = Output::new("DP-1");
    output.width = 1920;
    output.height = 1080;
    let mut snapshot = WindowsSnapshot::default();
    snapshot.windows.insert("native:1".into(), window);
    snapshot.outputs.insert("DP-1".into(), output);
    snapshot
}
fn backend() -> WindowsSnapshot { backend_with(|_| {}) }

fn snapshot(changes: Value) -> Value {
    let mut value = json!({"focused": true, "classes": ["game", "Game"], "title": "A game",
        "fullscreen": false, "maximized": false, "border_width": 0,
        "motif": [2, 0, 0, 0, 0], "extents": [0, 0, 0, 0], "geometry": [0, 0, 1920, 1080]});
    for (key, item) in changes.as_object().unwrap() {
        value[key] = item.clone();
    }
    value
}

fn check(native: &WindowsSnapshot, x11: Value) -> bool {
    let cache = FullscreenCache::new();
    focused_fullscreen_with(native, &cache, &move || Some(x11.clone()))
}

#[test]
fn native_fullscreen_is_immediate_but_only_for_focused_unminimized_window() {
    let never = || -> Option<Value> { panic!("must not query X11") };
    let cache = FullscreenCache::new();
    assert!(focused_fullscreen_with(&backend_with(|w| w.fullscreen = true), &cache, &never));
    assert!(!focused_fullscreen_with(&backend_with(|w| { w.fullscreen = true; w.activated = false }), &cache, &never));
    let minimized = backend_with(|w| { w.fullscreen = true; w.minimized = true });
    assert!(!focused_fullscreen_with(&minimized, &cache, &|| None));
}

#[test]
fn borderless_screen_filling_window_and_negative_origin_output() {
    assert!(check(&backend(), snapshot(json!({}))));
    // Either explicit no-decoration hint or zero frame extents is enough,
    // but a missing hint alone proves nothing.
    assert!(check(&backend(), snapshot(json!({"extents": null}))));
    assert!(check(&backend(), snapshot(json!({"motif": null}))));
    assert!(!check(&backend(), snapshot(json!({"extents": null, "motif": null}))));
    let mut native = backend();
    native.outputs["DP-1"].x = -1920;
    assert!(check(&native, snapshot(json!({"geometry": [-1920, 0, 1920, 1080]}))));
    assert!(check(&native, snapshot(json!({"geometry": [-1919, 1, 1921, 1079]}))), "±1 px tolerance");
}

#[test]
fn ordinary_maximized_and_decorated_windows_remain_allowed() {
    assert!(!check(&backend(), snapshot(json!({"maximized": true}))));
    assert!(!check(&backend(), snapshot(json!({"border_width": 1}))));
    assert!(!check(&backend(), snapshot(json!({"extents": [1, 1, 28, 1], "motif": [2, 0, 1, 0, 0]}))));
    assert!(!check(&backend(), snapshot(json!({"geometry": [0, 0, 1920, 1040]}))));
    assert!(!check(&backend(), snapshot(json!({"geometry": [100, 100, 1920, 1080]}))));
    assert!(check(&backend(), snapshot(json!({"fullscreen": true, "maximized": true}))));
}

#[test]
fn stale_unfocused_or_different_application_never_blocks() {
    assert!(!check(&backend(), snapshot(json!({"focused": false}))));
    assert!(!check(&backend(), snapshot(json!({"classes": ["OtherApp"]}))));
    assert!(!check(&backend(), snapshot(json!({"title": "Old game title"}))));
    assert!(!check(&backend_with(|w| w.app_id.clear()), snapshot(json!({}))));
    assert!(!check(&backend_with(|w| w.outputs = vec!["OTHER-OUTPUT".into()]), snapshot(json!({}))));
    assert!(!check(&backend(), json!({})));
    assert!(check(&backend_with(|w| w.outputs.clear()), snapshot(json!({}))), "no membership: any output");
}

#[test]
fn missing_or_oversized_geometry_is_not_fullscreen() {
    for geometry in [Value::Null, json!([]), json!([0, 0, 0, 0]), json!([0, 0, "1920", 1080]), json!([0, 0, 3840, 2160]),
                     json!([0, 0, 1920.0, 1080])] {
        assert!(!check(&backend(), snapshot(json!({"geometry": geometry.clone()}))), "{geometry}");
    }
}

#[test]
fn probe_has_total_deadline_and_remote_display_is_not_contacted() {
    let calls = std::cell::Cell::new(0);
    let result = x11_snapshot_for(Some(":99"), |display, timeout| {
        calls.set(calls.get() + 1);
        assert_eq!(display, ":99");
        assert_eq!(timeout, Duration::from_millis(120));
        None
    });
    assert!(result.is_none());
    assert_eq!(calls.get(), 1);
    for bad in [json!("malformed"), Value::Null, json!([1])] {
        assert!(x11_snapshot_for(Some("unix:0.1"), |_, _| Some(bad.clone())).is_none());
    }
    let huge = json!({"focused": false, "clients": vec![vec!["x".repeat(70000)]]});
    assert!(x11_snapshot_for(Some(":0"), |_, _| Some(huge.clone())).is_none(), "65536-byte output limit");
    assert!(x11_snapshot_for(Some(":0"), |_, _| Some(json!({}))).is_some());
    for display in [None, Some(""), Some("remote.example:0"), Some("localhost:10"), Some(":0x"), Some("unix:")] {
        assert!(x11_snapshot_for(display, |_, _| panic!("must not contact {display:?}")).is_none());
    }
    assert!(is_local_display(":0") && is_local_display("unix:12.3") && !is_local_display("tcp/host:0"));
}

#[test]
fn unreachable_local_display_fails_fast() {
    // A display number nobody serves: the real probe returns None quickly.
    let start = Instant::now();
    assert!(probe(":64999", PROBE_TIMEOUT).is_none());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn stalled_x_server_hits_the_deadline() {
    // A listening socket that never answers the X11 setup.
    let dir = std::env::temp_dir().join(format!("wl-x11-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("X0");
    let _ = std::fs::remove_file(&path);
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let address = x11rb::reexports::x11rb_protocol::parse_display::ConnectAddress::Socket(path.display().to_string());
    let (stream, _) = x11rb::rust_connection::DefaultStream::connect(&address).unwrap();
    let start = Instant::now();
    let deadline = start + Duration::from_millis(120);
    let stream = x11::DeadlineStreamForTest::new(stream, deadline);
    let result = x11rb::rust_connection::RustConnection::connect_to_stream(stream, 0);
    assert!(result.is_err());
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_millis(110) && elapsed < Duration::from_millis(600), "{elapsed:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

struct Clock(Arc<Mutex<f64>>);
impl Clock {
    fn new() -> (Self, FullscreenCache) {
        let now = Arc::new(Mutex::new(100.0));
        let read = now.clone();
        (Self(now), FullscreenCache::with_clock(move || *read.lock().unwrap()))
    }
    fn advance(&self, seconds: f64) { *self.0.lock().unwrap() += seconds }
}

fn fresh(cache: &FullscreenCache, native: &WindowsSnapshot, x11: Value) -> (bool, usize) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let result = focused_fullscreen_with(native, cache, &move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Some(x11.clone())
    });
    (result, calls.load(Ordering::SeqCst))
}

#[test]
fn last_state_never_probes_and_tracks_signature() {
    let (_clock, cache) = Clock::new();
    let native = backend();
    assert!(!cache.last_state(Some(&native)));
    assert!(cache.last_state(Some(&backend_with(|w| w.fullscreen = true))));
    assert!(!cache.last_state(Some(&backend_with(|w| w.activated = false))));
    assert!(cache.needs_probe(&native, false));
    assert_eq!(fresh(&cache, &native, snapshot(json!({}))), (true, 1));
    assert!(cache.last_state(Some(&native)));
    assert!(cache.last_state(None));
    assert!(!cache.needs_probe(&native, false));
    // A different focus/title signature is unknown until re-checked: fail open.
    let menu = backend_with(|w| w.title = "Menu".into());
    assert!(!cache.last_state(Some(&menu)));
    assert!(cache.needs_probe(&menu, false));
    cache.invalidate();
    assert!(cache.needs_probe(&native, false));
}

#[test]
fn forced_refresh_is_rate_limited() {
    let (clock, cache) = Clock::new();
    let native = backend();
    fresh(&cache, &native, snapshot(json!({})));
    assert!(!cache.needs_probe(&native, true));
    clock.advance(PROBE_MAX_AGE);
    assert!(cache.needs_probe(&native, true));
    assert!(!cache.needs_probe(&native, false));
}

#[test]
fn native_wayland_window_stops_probing_after_two_client_lists() {
    let (_clock, cache) = Clock::new();
    let files = |title: &str| backend_with(|w| { w.app_id = "org.gnome.Nautilus".into(); w.title = title.into() });
    let unfocused = json!({"focused": false, "clients": [["steam", "Steam"]]});
    assert_eq!(fresh(&cache, &files("Files"), unfocused.clone()), (false, 1));
    assert!(cache.needs_probe(&files("Home"), false));
    assert_eq!(fresh(&cache, &files("Home"), unfocused.clone()), (false, 1));
    let renamed = files("Downloads");
    assert!(!cache.needs_probe(&renamed, true));
    assert_eq!(fresh(&cache, &renamed, unfocused), (false, 0));
    cache.forget(std::iter::empty());
    assert!(cache.needs_probe(&files("Music"), false));
}

#[test]
fn xwayland_and_appless_windows() {
    let (clock, cache) = Clock::new();
    let listed = snapshot(json!({"clients": [["game", "Game"]]}));
    for _ in 0..3 {
        fresh(&cache, &backend(), listed.clone());
    }
    clock.advance(PROBE_MAX_AGE);
    assert!(cache.needs_probe(&backend(), true), "listed X11 clients stay probe candidates");
    let appless = backend_with(|w| w.app_id.clear());
    assert!(!cache.needs_probe(&appless, false));
    assert_eq!(fresh(&cache, &appless, listed), (false, 0));
    assert!(!cache.needs_probe(&backend_with(|w| w.fullscreen = true), false));
}

#[test]
fn snapshot_without_x11_focus_is_never_fullscreen() {
    let native = backend();
    assert!(!matching_x11_fullscreen(&native.windows["native:1"], &native.outputs, Some(&json!({"focused": false, "clients": []}))));
    assert!(!matching_x11_fullscreen(&native.windows["native:1"], &native.outputs, None));
}

#[test]
fn signature_is_sorted_and_ignores_unfocused_windows() {
    let mut state = backend();
    state.windows.insert("a:0".into(), Window { id: "a:0".into(), activated: true, ..Default::default() });
    state.windows.insert("z:9".into(), Window { id: "z:9".into(), activated: false, ..Default::default() });
    let ids: Vec<_> = focus_signature(&state).into_iter().map(|s| s.0).collect();
    assert_eq!(ids, ["a:0", "native:1"]);
}

fn spin(context: &glib::MainContext, timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    done()
}

#[test]
fn menu_hotkey_reads_cached_fullscreen_state_without_waiting_for_x11() {
    let context = glib::MainContext::new();
    context.with_thread_default(|| {
        let state = Rc::new(RefCell::new(backend()));
        let source = state.clone();
        let cache = Arc::new(FullscreenCache::new());
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let gate = release.clone();
        let probes = Arc::new(AtomicUsize::new(0));
        let counter = probes.clone();
        let guard = FullscreenGuard::with_parts(move || Some(source.borrow().clone()), cache.clone(), Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let (lock, wake) = &*gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
            Some(snapshot(json!({})))
        }));
        let start = Instant::now();
        assert!(!guard.menu_hotkey_blocked(false), "unknown X11 verdict fails open");
        assert!(start.elapsed() < Duration::from_millis(50), "the hotkey never waits for X11");
        assert!(guard.probe_running());
        // Repeated presses while probing coalesce into one rerun.
        assert!(!guard.state());
        assert!(!guard.state());
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        assert!(spin(&context, Duration::from_secs(3), || !guard.probe_running()));
        assert!(guard.menu_hotkey_blocked(false), "the cached Xwayland verdict blocks the hotkey");
        assert!(!guard.menu_hotkey_blocked(true), "allowed in fullscreen");
        assert_eq!(probes.load(Ordering::SeqCst), 1, "the rerun found nothing to re-probe");
        // Native fullscreen is exact without probing; closed windows are forgotten.
        state.borrow_mut().windows["native:1"].fullscreen = true;
        assert!(guard.state());
        state.borrow_mut().windows.clear();
        guard.windows_changed();
        assert!(!guard.state());
        assert!(spin(&context, Duration::from_millis(50), || !guard.probe_running()));
    }).unwrap();
}
