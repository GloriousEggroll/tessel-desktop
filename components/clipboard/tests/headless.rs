//! Real-protocol checks inside a private nested labwc (tests/run_headless.sh):
//! wl-copy owns a selection, is killed, and the copy must still paste.
//! `headless replace <text>` is the test's own client that clears the
//! clipboard and sets a new one in the same burst, as browsers do.
//! Usage (inside the nested session): headless <tessel-clipboard binary> <scratch dir>
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

struct Session {
    tessel_clipboard: Child,
    settings: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tessel_clipboard.kill();
        let _ = self.tessel_clipboard.wait();
    }
}

fn copy(args: &[&str], data: &[u8]) -> Child {
    use std::io::Write;
    let mut child = Command::new("wl-copy").arg("--foreground").args(args).stdin(Stdio::piped()).spawn()
        .expect("wl-copy (wl-clipboard) is required");
    child.stdin.take().unwrap().write_all(data).unwrap();
    child
}

fn paste(args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("wl-paste").arg("--no-newline").args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// Poll until `paste(args)` gives `want` (or the deadline passes).
fn wait_paste(args: &[&str], want: Option<&[u8]>) -> Option<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = paste(args);
        if got.as_deref() == want || Instant::now() > deadline {
            return got;
        }
        sleep(Duration::from_millis(100));
    }
}

fn close(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn write_settings(path: &Path, json: &str) {
    std::fs::write(path, json).unwrap();
    // The daemon polls the file's mtime about every two seconds.
    sleep(Duration::from_millis(2600));
}

fn copy_close_paste(session: &Session, args: &[&str], data: &[u8]) -> Option<Vec<u8>> {
    let _ = &session.settings;
    let owner = copy(args, data);
    assert_eq!(wait_paste(args, Some(data)).as_deref(), Some(data), "the live owner serves the paste");
    sleep(Duration::from_millis(300));
    close(owner);
    sleep(Duration::from_millis(300));
    wait_paste(args, Some(data))
}

mod replace {
    //! A data-control client: clear the clipboard, then set `text` right away.
    use std::io::Write;
    use wayland_client::protocol::{wl_registry, wl_seat};
    use wayland_client::{Connection, Dispatch, QueueHandle};
    use wayland_protocols::ext::data_control::v1::client::{
        ext_data_control_device_v1 as device, ext_data_control_manager_v1 as manager,
        ext_data_control_offer_v1 as offer, ext_data_control_source_v1 as source,
    };

    #[derive(Default)]
    struct State {
        seat: Option<wl_seat::WlSeat>,
        manager: Option<manager::ExtDataControlManagerV1>,
        text: Vec<u8>,
        cancelled: bool,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(),
                 _: &Connection, qh: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { name, interface, .. } = event {
                match interface.as_str() {
                    "wl_seat" => state.seat = Some(registry.bind(name, 1, qh, ())),
                    "ext_data_control_manager_v1" => state.manager = Some(registry.bind(name, 1, qh, ())),
                    _ => {}
                }
            }
        }
    }
    impl Dispatch<wl_seat::WlSeat, ()> for State {
        fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<manager::ExtDataControlManagerV1, ()> for State {
        fn event(_: &mut Self, _: &manager::ExtDataControlManagerV1, _: manager::Event, _: &(), _: &Connection,
                 _: &QueueHandle<Self>) {}
    }
    impl Dispatch<device::ExtDataControlDeviceV1, ()> for State {
        fn event(_: &mut Self, _: &device::ExtDataControlDeviceV1, _: device::Event, _: &(), _: &Connection,
                 _: &QueueHandle<Self>) {}
        wayland_client::event_created_child!(State, device::ExtDataControlDeviceV1, [
            device::EVT_DATA_OFFER_OPCODE => (offer::ExtDataControlOfferV1, ()),
        ]);
    }
    impl Dispatch<offer::ExtDataControlOfferV1, ()> for State {
        fn event(_: &mut Self, _: &offer::ExtDataControlOfferV1, _: offer::Event, _: &(), _: &Connection,
                 _: &QueueHandle<Self>) {}
    }
    impl Dispatch<source::ExtDataControlSourceV1, ()> for State {
        fn event(state: &mut Self, _: &source::ExtDataControlSourceV1, event: source::Event, _: &(), _: &Connection,
                 _: &QueueHandle<Self>) {
            match event {
                source::Event::Send { fd, .. } => {
                    let _ = std::fs::File::from(fd).write_all(&state.text);
                }
                source::Event::Cancelled => state.cancelled = true,
                _ => {}
            }
        }
    }

    /// Serve until replaced (or 20 s).
    pub fn run(text: &str) {
        let conn = Connection::connect_to_env().unwrap();
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut state = State { text: text.as_bytes().to_vec(), ..Default::default() };
        conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut state).unwrap();
        let (Some(seat), Some(manager)) = (state.seat.clone(), state.manager.clone()) else { panic!("no data control") };
        let device = manager.get_data_device(&seat, &qh, ());
        let source = manager.create_data_source(&qh, ());
        for mime in ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"] {
            source.offer(mime.into());
        }
        device.set_selection(None);
        device.set_selection(Some(&source));
        conn.flush().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !state.cancelled && std::time::Instant::now() < deadline {
            queue.roundtrip(&mut state).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// Clear and set in one burst from the test's own client; returns it running.
fn clear_then_copy(text: &str) -> Child {
    Command::new(std::env::current_exe().unwrap()).args(["replace", text]).spawn().unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("replace") {
        replace::run(&args[2]);
        return;
    }
    let (binary, scratch) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    std::fs::create_dir_all(&scratch).unwrap();
    let settings = scratch.join("clipboard.json");
    std::fs::write(&settings, "{}").unwrap();
    let check = Command::new(&binary).arg("--check").output().unwrap();
    assert!(check.status.success(), "--check: {}", String::from_utf8_lossy(&check.stderr));
    println!("{}", String::from_utf8_lossy(&check.stdout).trim());
    let tessel_clipboard = Command::new(&binary).arg("--verbose").arg("--settings").arg(&settings).spawn().unwrap();
    let session = Session { tessel_clipboard, settings: settings.clone() };
    sleep(Duration::from_millis(500));
    let second = Command::new(&binary).arg("--settings").arg(&settings).output().unwrap();
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"), "single instance");
    let mut failures = 0;
    let mut check = |name: &str, ok: bool| {
        println!("{} {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    check("text survives its owner", copy_close_paste(&session, &[], b"hello world").as_deref() == Some(b"hello world"));
    check("a newer copy replaces the kept one",
          copy_close_paste(&session, &[], "second ✓".as_bytes()).as_deref() == Some("second ✓".as_bytes()));
    let image: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    check("a 3 MB image survives byte for byte",
          copy_close_paste(&session, &["--type", "image/png"], &image).as_deref() == Some(&image[..]));
    check("primary selection is not kept by default", {
        let owner = copy(&["--primary"], b"prim");
        let live = wait_paste(&["--primary"], Some(b"prim"));
        close(owner);
        sleep(Duration::from_millis(600));
        live.as_deref() == Some(b"prim") && paste(&["--primary"]).is_none()
    });
    check("the clipboard is still the image afterwards", paste(&["--type", "image/png"]).as_deref() == Some(&image[..]));
    check("clearing then copying at once keeps the new copy", {
        // tessel-clipboard owns the kept image now; a browser-style copy must win on the first try.
        let owner = clear_then_copy("fresh");
        sleep(Duration::from_millis(800));
        let got = paste(&[]);
        close(owner);
        got.as_deref() == Some(b"fresh")
    });
    check("a cleared clipboard still gets the kept copy back", {
        let owner = copy(&[], b"keep me");
        let live = wait_paste(&[], Some(b"keep me"));
        sleep(Duration::from_millis(300));
        let clear = Command::new("wl-copy").arg("--clear").status().map(|s| s.success()).unwrap_or(false);
        close(owner);
        clear && live.as_deref() == Some(b"keep me") && wait_paste(&[], Some(b"keep me")).as_deref() == Some(b"keep me")
    });

    write_settings(&settings, r#"{"keep_primary": true, "max_mb": 1}"#);
    check("primary selection kept when enabled",
          copy_close_paste(&session, &["--primary"], b"middle").as_deref() == Some(b"middle"));
    check("copies over the size limit are not kept", {
        let owner = copy(&["--type", "image/png"], &image);
        let live = wait_paste(&["--type", "image/png"], Some(&image));
        sleep(Duration::from_millis(300));
        close(owner);
        sleep(Duration::from_millis(800));
        live.as_deref() == Some(&image[..]) && paste(&[]).is_none()
    });

    write_settings(&settings, r#"{"keep_clipboard": false}"#);
    check("nothing kept when turned off", {
        let owner = copy(&[], b"gone");
        let live = wait_paste(&[], Some(b"gone"));
        sleep(Duration::from_millis(300));
        close(owner);
        sleep(Duration::from_millis(800));
        live.as_deref() == Some(b"gone") && paste(&[]).is_none()
    });

    drop(session);
    if failures > 0 {
        println!("{failures} check(s) failed");
        std::process::exit(1);
    }
    println!("all clipboard checks passed");
}
