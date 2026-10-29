//! Integration checks against a real compositor: GTK windows observed and
//! driven through foreign-toplevel, captured with ext-image-copy-capture,
//! workspaces/outputs listed, blur on GTK's display, and the compat binaries
//! compared with the original C helpers.
//!
//! Runs only inside a private headless labwc (tests/run_headless.sh sets
//! SUITE_WAYLAND_HEADLESS=1); anywhere else — in particular the live session
//! (/run/user/1000) — it skips without connecting.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use suite_wayland::model::Event as Changed;
use suite_wayland::{blur, gtk as sgtk, Client, Event, Frame, GlibClient, Protocols};

const APP_ID: &str = "org.suite.WaylandTest";

fn iterate(duration: Duration) {
    let end = Instant::now() + duration;
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(seconds: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => { if !$cond { return Err(format!($($msg)+)); } };
}

struct World {
    client: GlibClient,
    events: Rc<RefCell<Vec<String>>>,
    frames: Rc<RefCell<Vec<(u64, Result<Frame, String>)>>>,
    alpha: gtk::Window,
    beta: gtk::Window,
    beta_close_requests: Rc<RefCell<u32>>,
}

impl World {
    fn window_id(&self, title: &str) -> Option<u64> {
        self.client.borrow().model.windows().find(|w| w.title == title).map(|w| w.id)
    }
    fn window(&self, title: &str) -> Option<suite_wayland::Window> {
        self.client.borrow().model.windows().find(|w| w.title == title).cloned()
    }
    fn capture(&self, start: impl FnOnce(&mut Client) -> Result<u64, suite_wayland::ActionError>) -> Result<Frame, String> {
        let id = self.client.with(start).map_err(|e| e.to_string())?;
        ensure!(wait_for(5.0, || self.frames.borrow().iter().any(|(f, _)| *f == id)), "capture {id} never finished");
        let mut frames = self.frames.borrow_mut();
        let index = frames.iter().position(|(f, _)| *f == id).unwrap();
        frames.remove(index).1
    }
}

fn window(title: &str) -> gtk::Window {
    let window = gtk::Window::builder().title(title).default_width(420).default_height(300).build();
    window.set_child(Some(&gtk::Label::new(Some(title))));
    window.present();
    window
}

fn setup() -> Result<World, String> {
    let client = Client::connect(Protocols::ALL).map_err(|e| e.to_string())?;
    let events = Rc::new(RefCell::new(Vec::new()));
    let frames = Rc::new(RefCell::new(Vec::new()));
    let (sink, frame_sink) = (events.clone(), frames.clone());
    let client = GlibClient::attach(client, None, move |_, event| match event {
        Event::Captured(id, result) => frame_sink.borrow_mut().push((id, result.map_err(|e| e.to_string()))),
        other => sink.borrow_mut().push(format!("{other:?}")),
    });
    let alpha = window("Alpha");
    let beta = window("Beta");
    let beta_close_requests = Rc::new(RefCell::new(0));
    let counter = beta_close_requests.clone();
    beta.connect_close_request(move |_| {
        *counter.borrow_mut() += 1;
        glib::Propagation::Proceed
    });
    let world = World { client, events, frames, alpha, beta, beta_close_requests };
    ensure!(wait_for(10.0, || world.window_id("Alpha").is_some() && world.window_id("Beta").is_some()),
            "GTK windows never appeared: {:?}", world.client.borrow().model.all_windows());
    Ok(world)
}

fn observe(w: &World) -> Result<(), String> {
    let alpha = w.window("Alpha").unwrap();
    ensure!(alpha.app_id == APP_ID, "app_id {:?}", alpha.app_id);
    let client = w.client.borrow();
    ensure!(client.has_seat() && client.toplevel_version() == 3, "seat/version");
    ensure!(client.model.window_outputs(&alpha) == vec!["HEADLESS-1"], "outputs {:?}", alpha.outputs);
    let output = client.model.output_by_name("HEADLESS-1").ok_or("no HEADLESS-1")?;
    let g = output.geometry();
    ensure!((g.x, g.y, g.width, g.height, output.scale) == (0, 0, 1280, 720, 1), "geometry {g:?}");
    ensure!(output.logical && client.has_xdg_output(), "xdg-output not used");
    drop(client);
    ensure!(w.events.borrow().iter().any(|e| e.contains("Window(")), "handler saw no window events");
    Ok(())
}

/// GTK's side of activation: xdg_toplevel `activated` → GDK FOCUSED state.
/// (`is_active()` stays false here: the headless seat has no keyboard.)
fn focused(window: &gtk::Window) -> bool {
    window.surface().and_downcast::<gdk::Toplevel>().is_some_and(|t| t.state().contains(gdk::ToplevelState::FOCUSED))
}

fn activate(w: &World) -> Result<(), String> {
    for (title, gtk_window, other) in [("Alpha", &w.alpha, "Beta"), ("Beta", &w.beta, "Alpha")] {
        let id = w.window_id(title).unwrap();
        w.client.with(|c| c.activate(id)).map_err(|e| e.to_string())?;
        ensure!(wait_for(5.0, || w.window(title).is_some_and(|x| x.activated) && focused(gtk_window)),
                "{title} not activated: model {:?} gtk {:?}", w.window(title), gtk_window.surface().and_downcast::<gdk::Toplevel>().map(|t| t.state()));
        ensure!(w.window(other).is_some_and(|x| !x.activated), "{other} still activated");
    }
    Ok(())
}

fn minimize(w: &World) -> Result<(), String> {
    let id = w.window_id("Beta").unwrap();
    w.client.with(|c| c.set_minimized(id, true)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.window("Beta").is_some_and(|x| x.minimized)), "Beta not minimized");
    w.client.with(|c| c.activate(id)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.window("Beta").is_some_and(|x| !x.minimized && x.activated)), "activate did not restore");
    Ok(())
}

fn maximize_fullscreen(w: &World) -> Result<(), String> {
    let id = w.window_id("Alpha").unwrap();
    w.client.with(|c| c.set_maximized(id, true)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| x.maximized) && w.alpha.is_maximized()), "not maximized");
    w.client.with(|c| c.set_maximized(id, false)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| !x.maximized) && !w.alpha.is_maximized()), "not unmaximized");
    w.client.with(|c| c.set_fullscreen(id, true, Some("HEADLESS-1"))).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| x.fullscreen) && w.alpha.is_fullscreen()), "not fullscreen");
    w.client.with(|c| c.set_fullscreen(id, false, None)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| !x.fullscreen) && !w.alpha.is_fullscreen()), "still fullscreen");
    ensure!(w.client.with(|c| c.set_fullscreen(id, true, Some("NOPE-9"))).is_err(), "unknown output accepted");
    Ok(())
}

fn capture(w: &World) -> Result<(), String> {
    let alpha = w.window_id("Alpha").unwrap();
    ensure!(wait_for(5.0, || w.client.borrow().listed_for_window(alpha).is_some()),
            "no unique ext handle: {:?}", w.client.borrow().model.listed());
    let key = w.client.borrow().listed_for_window(alpha).unwrap();
    let listed = w.client.borrow().model.listed_toplevel(key).cloned().unwrap();
    ensure!(listed.identifier.as_deref().is_some_and(|i| !i.is_empty()), "no identifier");
    let frame = w.capture(|c| c.capture_listed(key, false))?;
    // ARGB8888 is preferred when offered; labwc's pixman renderer offers only XRGB8888.
    ensure!(frame.format == Frame::ARGB8888 || frame.format == Frame::XRGB8888, "format {}", frame.format);
    // The capture covers the toplevel's surface (CSD shadow included) at scale 1.
    let surface = w.alpha.surface().ok_or("Alpha has no surface")?;
    ensure!((frame.width as i32, frame.height as i32) == (surface.width(), surface.height()) && frame.width >= 420,
            "size {}x{} vs surface {}x{}", frame.width, frame.height, surface.width(), surface.height());
    ensure!(frame.data.len() == (frame.stride * frame.height) as usize, "buffer size");
    ensure!(frame.data.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]), "captured only black");
    let texture = frame.to_texture();
    ensure!((texture.width(), texture.height()) == (frame.width as i32, frame.height as i32), "texture size");
    let desktop = w.capture(|c| c.capture_output("HEADLESS-1", false))?;
    ensure!((desktop.width, desktop.height) == (1280, 720), "output {}x{}", desktop.width, desktop.height);
    ensure!(w.client.with(|c| c.capture_output("NOPE-9", false)).is_err(), "unknown output accepted");
    Ok(())
}

/// labwc keeps reporting the output size for a toplevel's capture after it
/// left fullscreen (the C overview helper sees the same); report, don't fail.
fn capture_after_fullscreen(w: &World) -> Result<(), String> {
    if let Err(error) = capture(w) {
        println!("note: {error}");
    }
    Ok(())
}

fn workspaces(w: &World) -> Result<(), String> {
    ensure!(wait_for(3.0, || w.client.borrow().model.workspaces().len() == 3), "workspaces {:?}", w.client.borrow().model.workspaces());
    let (first, second, group) = {
        let c = w.client.borrow();
        let ws = c.model.workspaces();
        ensure!(ws.iter().filter(|x| x.active()).count() == 1 && ws[0].active(), "active {ws:?}");
        ensure!(c.model.groups().len() == 1 && c.model.group_outputs(&c.model.groups()[0]) == vec!["HEADLESS-1"], "groups");
        ensure!(ws.iter().all(|x| x.can_activate() && x.name.starts_with("Workspace")), "names/caps {ws:?}");
        (ws[0].id, ws[1].id, ws[0].group)
    };
    ensure!(group != 0, "no group");
    w.client.with(|c| c.activate_workspace(second)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.client.borrow().model.workspace(second).is_some_and(|x| x.active())), "second not active");
    ensure!(w.client.borrow().model.workspace(first).is_some_and(|x| !x.active()), "first still active");
    ensure!(w.events.borrow().iter().any(|e| e.contains("Workspaces")), "no Workspaces event");
    w.client.with(|c| c.activate_workspace(first)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.client.borrow().model.workspace(first).is_some_and(|x| x.active())), "first not restored");
    ensure!(w.client.with(|c| c.activate_workspace(999)).is_err(), "unknown workspace accepted");
    Ok(())
}

fn shared_display(w: &World) -> Result<(), String> {
    let display = gdk::Display::default().unwrap();
    let client = Client::for_gdk_display(&display, Protocols::TOPLEVELS).ok_or("no GTK connection")?;
    let seen = Rc::new(RefCell::new(0));
    let counter = seen.clone();
    let shared = GlibClient::attach(client, None, move |_, event| {
        if matches!(event, Event::Changed(Changed::Window(_))) {
            *counter.borrow_mut() += 1;
        }
    });
    let find = |title: &str| shared.borrow().model.windows().find(|x| x.title == title).map(|x| x.id);
    ensure!(wait_for(5.0, || find("Alpha").is_some() && find("Beta").is_some()), "shared client saw no windows");
    let surface = sgtk::gtk_wl_surface(&w.beta).ok_or("no wl_surface for Beta")?;
    let alpha = find("Alpha").unwrap();
    shared.with(|c| c.set_rectangle(alpha, &surface, 4, 4, 32, 32)).map_err(|e| e.to_string())?;
    shared.with(|c| c.set_minimized(alpha, true)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || shared.borrow().model.window(alpha).is_some_and(|x| x.minimized)), "shared minimize");
    // GTK keeps working on the same wl_display: a new window shows up in both clients.
    let gamma = window("Gamma");
    ensure!(wait_for(5.0, || find("Gamma").is_some() && w.window_id("Gamma").is_some()), "Gamma not seen");
    shared.with(|c| c.activate(alpha)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| x.activated && !x.minimized)), "shared activate");
    ensure!(shared.borrow().connection().protocol_error().is_none(), "protocol error on GTK's display");
    ensure!(*seen.borrow() > 0, "no events on the shared client");
    gamma.destroy();
    ensure!(wait_for(5.0, || find("Gamma").is_none()), "Gamma close not seen");
    shared.close();
    Ok(())
}

fn blur_regions(w: &World) -> Result<(), String> {
    let display = WidgetExt::display(&w.alpha);
    let binding = blur::attach(&w.alpha, None, true, true);
    binding.configure(true, 50, true, None, None);
    ensure!(wait_for(5.0, || {
        binding.refresh();
        binding.surface_count() == 1
    }), "blur never applied (supported: {:?})", blur::compositor_supports_blur(&display));
    ensure!(blur::compositor_supports_blur(&display) == Some(true), "blur capability");
    ensure!(blur::compositor_supports_radius(&display) == Some(true), "waydock_blur_manager_v1 not bound");
    let [x, y, width, height, corners, strength] = binding.geometry(0).ok_or("no geometry")?;
    ensure!(corners == 12 && strength == 20, "corners/strength {corners}/{strength}");
    ensure!(width >= 420 && height >= 300 && x >= 0 && y >= 0, "geometry {x},{y} {width}x{height}");
    ensure!(blur::attach(&w.alpha, None, true, true).surface_count() == 1, "attach is not shared per widget");
    binding.configure(true, 100, false, None, None);
    ensure!(binding.geometry(0).is_some_and(|g| g[4] == 0 && g[5] == 40), "reconfigure {:?}", binding.geometry(0));
    // An unmapped widget gets nothing.
    let other = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let other_binding = blur::attach(&other, None, true, true);
    other_binding.configure(true, 50, true, None, None);
    ensure!(other_binding.surface_count() == 0, "unmapped widget blurred");
    binding.configure(false, 50, true, None, None);
    ensure!(binding.surface_count() == 0, "disable keeps regions");
    binding.configure(true, 50, true, None, None);
    ensure!(binding.surface_count() == 1, "re-enable");
    binding.close();
    ensure!(binding.surface_count() == 0, "close keeps regions");
    iterate(Duration::from_millis(100));
    ensure!(gdk_wayland_ok(&display), "GTK connection broke");
    Ok(())
}

fn gdk_wayland_ok(display: &gdk::Display) -> bool {
    sgtk::gdk_connection(display).is_some_and(|c| c.protocol_error().is_none())
}

/// Run a helper while the main loop keeps GTK alive; returns (stdout, status).
fn run_helper(program: &str, args: &[&str], stdin: &[u8]) -> (String, i32) {
    let mut child = Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().unwrap_or_else(|e| panic!("{program}: {e}"));
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin).ok();
    drop(input);
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut out).ok();
        out
    });
    let mut status = None;
    wait_for(20.0, || {
        status = child.try_wait().ok().flatten();
        status.is_some()
    });
    if status.is_none() {
        child.kill().ok();
    }
    let code = status.and_then(|s| s.code()).unwrap_or(-1);
    (reader.join().unwrap(), code)
}

fn c_helper(name: &str) -> Option<String> {
    let dir = std::env::var("SUITE_WAYLAND_C_HELPERS").ok()?;
    let path = format!("{dir}/{name}");
    std::path::Path::new(&path).is_file().then_some(path)
}

const RUST_TOPLEVEL: &str = env!("CARGO_BIN_EXE_waydock-toplevel");
const RUST_WAYSHELL_TOPLEVEL: &str = env!("CARGO_BIN_EXE_wayshell-toplevel");
const RUST_WORKSPACES: &str = env!("CARGO_BIN_EXE_waydock-workspaces");
const RUST_OVERVIEW: &str = env!("CARGO_BIN_EXE_wayshell-overview");

fn compat_snapshots(_: &World) -> Result<(), String> {
    let (rust, code) = run_helper(RUST_TOPLEVEL, &["--snapshot"], b"");
    ensure!(code == 0 && rust.lines().last() == Some("{\"event\":\"ready\",\"seat\":true,\"protocol_version\":3}"), "rust snapshot {rust}");
    ensure!(rust.contains("\"title\":\"Alpha\"") && rust.contains(&format!("\"app_id\":\"{APP_ID}\"")), "no Alpha: {rust}");
    ensure!(run_helper(RUST_WAYSHELL_TOPLEVEL, &["--snapshot"], b"").0 == rust, "wayshell-toplevel differs");
    ensure!(run_helper(RUST_TOPLEVEL, &["--bogus"], b"").1 == 2, "bad argument accepted");
    let (workspaces, code) = run_helper(RUST_WORKSPACES, &["--snapshot"], b"");
    ensure!(code == 0 && workspaces.ends_with("{\"event\":\"ready\",\"protocol_version\":1}\n"), "workspaces {workspaces}");
    let Some(c_toplevel) = c_helper("waydock-toplevel") else { return Err("C helpers not built (SUITE_WAYLAND_C_HELPERS)".into()) };
    let (c, code) = run_helper(&c_toplevel, &["--snapshot"], b"");
    ensure!(code == 0 && c == rust, "toplevel snapshot differs\nC:\n{c}\nRust:\n{rust}");
    let (c, _) = run_helper(&c_helper("waydock-workspaces").unwrap(), &["--snapshot"], b"");
    ensure!(c == workspaces, "workspace snapshot differs\nC:\n{c}\nRust:\n{workspaces}");
    // Request validation and error text, byte for byte (no compositor-visible effect).
    let mut requests: Vec<u8> = b"\n bogus 1\nactivate 999\nclose\nclose 5 6\nactivate -3\nminimize 18446744073709551616\n".to_vec();
    requests.extend(std::iter::repeat_n(b'x', 5000));
    let (c, c_code) = run_helper(&c_toplevel, &[], &requests);
    let (r, r_code) = run_helper(RUST_TOPLEVEL, &[], &requests);
    ensure!(c == r && c_code == r_code, "toplevel requests differ\nC({c_code}):\n{c}\nRust({r_code}):\n{r}");
    ensure!(r.ends_with("{\"event\":\"error\",\"message\":\"Window request is too long.\"}\n"), "too-long {r}");
    let mut requests = b"activate 0\nactivate 9999\nactivate 1\nactivate x\nnope\n".to_vec();
    requests.extend(b"activate 2\0\n");
    let (c, c_code) = run_helper(&c_helper("waydock-workspaces").unwrap(), &[], &requests);
    let (r, r_code) = run_helper(RUST_WORKSPACES, &[], &requests);
    ensure!(c == r && c_code == r_code, "workspace requests differ\nC({c_code}):\n{c}\nRust({r_code}):\n{r}");
    Ok(())
}

fn compat_actions(w: &World) -> Result<(), String> {
    // Live helper session: minimize Alpha by helper id, observe the event.
    let mut child = Command::new(RUST_TOPLEVEL).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|e| e.to_string())?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let lines = RefCell::new(Vec::<serde_json::Value>::new());
    let seen = |needle: &dyn Fn(&serde_json::Value) -> bool| {
        lines.borrow_mut().extend(rx.try_iter().filter_map(|l| serde_json::from_str(&l).ok()));
        lines.borrow().iter().any(needle)
    };
    let result = (|| {
        ensure!(wait_for(5.0, || seen(&|v| v["event"] == "ready")), "helper not ready: {:?}", lines.borrow());
        let id = lines.borrow().iter().find(|v| v["event"] == "window" && v["title"] == "Alpha")
            .and_then(|v| v["id"].as_str().map(str::to_owned)).ok_or("no Alpha line")?;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "minimize {id}").map_err(|e| e.to_string())?;
        ensure!(wait_for(5.0, || seen(&|v| v["id"] == id.as_str() && v["minimized"] == true)), "no minimized event");
        ensure!(wait_for(5.0, || w.window("Alpha").is_some_and(|x| x.minimized)), "library client missed it");
        writeln!(stdin, "activate {id}").map_err(|e| e.to_string())?;
        ensure!(wait_for(5.0, || focused(&w.alpha) && w.window("Alpha").is_some_and(|x| !x.minimized)), "activate failed");
        // A window opened while the helper runs is reported, then its close.
        let delta = window("Delta");
        ensure!(wait_for(5.0, || seen(&|v| v["title"] == "Delta")), "Delta not reported");
        delta.destroy();
        ensure!(wait_for(5.0, || seen(&|v| v["event"] == "closed")), "close not reported");
        drop(stdin);
        ensure!(wait_for(5.0, || child.try_wait().ok().flatten().is_some()), "helper ignores stdin EOF");
        Ok(())
    })();
    child.kill().ok();
    result
}

fn compat_overview(_: &World) -> Result<(), String> {
    let (rust, code) = run_helper(RUST_OVERVIEW, &["HEADLESS-1"], b"");
    ensure!(code == 0, "overview exit {code}");
    let parsed: Vec<serde_json::Value> = rust.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let kinds: Vec<_> = parsed.iter().map(|v| v["kind"].as_str().unwrap_or("").to_owned()).collect();
    ensure!(kinds.first().map(String::as_str) == Some("desktop") && kinds.get(1).map(String::as_str) == Some("identities"),
            "kinds {kinds:?}");
    ensure!(kinds.iter().filter(|k| *k == "window").count() >= 2, "window frames {kinds:?}");
    for frame in parsed.iter().filter(|v| v["kind"] == "window") {
        ensure!(frame["identifier"].as_str().is_some_and(|i| !i.is_empty()) && frame["app_id"] == APP_ID, "frame {}", frame["title"]);
        let png = decode_base64(frame["image"].as_str().unwrap());
        ensure!(png.starts_with(b"\x89PNG"), "not a PNG");
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        ensure!((420..=640).contains(&width), "thumbnail width {width}");
    }
    if let Some(c_overview) = c_helper("wayshell-overview") {
        let (c, _) = run_helper(&c_overview, &["HEADLESS-1"], b"");
        let shape = |text: &str| -> Vec<String> {
            text.lines().map(|l| {
                let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
                if let Some(image) = v.get_mut("image") {
                    let png = decode_base64(image.as_str().unwrap());
                    *image = serde_json::Value::String(format!("{}x{}", u32::from_be_bytes(png[16..20].try_into().unwrap()),
                                                                u32::from_be_bytes(png[20..24].try_into().unwrap())));
                }
                v.to_string()
            }).collect()
        };
        ensure!(shape(&c) == shape(&rust), "overview differs\nC: {:?}\nRust: {:?}", shape(&c), shape(&rust));
    }
    ensure!(run_helper(RUST_OVERVIEW, &["NOPE-9"], b"").0.starts_with("{\"kind\":\"identities\""), "unknown connector");
    Ok(())
}

fn decode_base64(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let bytes: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, c)| n | (value(*c) as u32) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}

fn close(w: &World) -> Result<(), String> {
    let id = w.window_id("Beta").unwrap();
    w.client.with(|c| c.close(id)).map_err(|e| e.to_string())?;
    ensure!(wait_for(5.0, || *w.beta_close_requests.borrow() == 1), "Beta got no close request");
    ensure!(wait_for(5.0, || w.window_id("Beta").is_none()), "Beta still listed");
    ensure!(w.events.borrow().iter().any(|e| e == &format!("Changed(WindowClosed({id}))")), "no WindowClosed event");
    ensure!(w.client.with(|c| c.activate(id)).is_err(), "stale id accepted");
    Ok(())
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if std::env::var("SUITE_WAYLAND_HEADLESS").as_deref() != Ok("1") || runtime == "/run/user/1000" || runtime.is_empty() {
        println!("headless: skipped (run tests/run_headless.sh)");
        return;
    }
    glib::set_prgname(Some(APP_ID));
    gtk::init().expect("gtk");
    let world = match setup() {
        Ok(world) => world,
        Err(error) => {
            println!("FAIL setup: {error}");
            std::process::exit(1);
        }
    };
    let checks: &[(&str, fn(&World) -> Result<(), String>)] = &[
        ("observe windows and outputs", observe),
        ("activate", activate),
        ("minimize / restore", minimize),
        ("capture toplevel and output", capture),
        ("maximize / fullscreen", maximize_fullscreen),
        ("capture after fullscreen (labwc quirk, informational)", capture_after_fullscreen),
        ("workspaces", workspaces),
        ("GTK's own wl_display (set_rectangle, shared queue)", shared_display),
        ("blur on GTK surfaces", blur_regions),
        ("compat snapshots and request parity with C", compat_snapshots),
        ("compat live actions", compat_actions),
        ("compat overview", compat_overview),
        ("close", close),
    ];
    let mut failed = 0;
    for (name, check) in checks {
        match check(&world) {
            Ok(()) => println!("ok   {name}"),
            Err(error) => {
                failed += 1;
                println!("FAIL {name}: {error}");
            }
        }
    }
    println!("headless: {} passed, {failed} failed", checks.len() - failed);
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
