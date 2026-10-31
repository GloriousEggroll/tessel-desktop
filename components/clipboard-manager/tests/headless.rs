//! Real-protocol checks inside a private nested labwc (tests/run_headless.sh):
//! wl-copy owns a selection, is killed, and the copy must still paste.
//! Usage (inside the nested session): headless <wayclip binary> <scratch dir>
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

struct Session {
    wayclip: Child,
    settings: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.wayclip.kill();
        let _ = self.wayclip.wait();
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (binary, scratch) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    std::fs::create_dir_all(&scratch).unwrap();
    let settings = scratch.join("clipboard.json");
    std::fs::write(&settings, "{}").unwrap();
    let check = Command::new(&binary).arg("--check").output().unwrap();
    assert!(check.status.success(), "--check: {}", String::from_utf8_lossy(&check.stderr));
    println!("{}", String::from_utf8_lossy(&check.stdout).trim());
    let wayclip = Command::new(&binary).arg("--verbose").arg("--settings").arg(&settings).spawn().unwrap();
    let session = Session { wayclip, settings: settings.clone() };
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
