//! Copy screenshots to the Wayland clipboard without needing a visible window
//! (capture_clipboard.py). `wl-copy` serves the data from its own background
//! process; without it the GTK clipboard is used and the application is kept
//! alive for two minutes to answer paste requests.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;

pub const FALLBACK_HOLD_SECONDS: u32 = 120;

thread_local! {
    static HOLD: RefCell<Option<(glib::SourceId, gio::ApplicationHoldGuard)>> = const { RefCell::new(None) };
}

pub fn wl_copy() -> Option<std::path::PathBuf> {
    tessel_core::shell::which("wl-copy")
}

fn run_wl_copy(program: &std::path::Path, data: &[u8], mime: &str) -> Result<(), String> {
    // wl-copy forks to serve the selection, so this returns once stdin is read.
    use std::io::Write;
    let mut child = std::process::Command::new(program).args(["--type", mime]).stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
        .map_err(|e| crate::process::spawn_error(&e, &program.to_string_lossy()))?;
    let mut stdin = child.stdin.take().unwrap();
    let written = stdin.write_all(data);
    drop(stdin);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let args = vec![program.to_string_lossy().into_owned(), "--type".into(), mime.into()];
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = crate::process::code(status);
                if code != 0 {
                    return Err(crate::process::called_process_error(&args, code));
                }
                return written.map_err(|e| tessel_core::pyerr::os_error_text(&e));
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(crate::process::timeout_text(&args, std::time::Duration::from_secs(15)));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(error) => return Err(tessel_core::pyerr::os_error_text(&error)),
        }
    }
}

pub type Done = Box<dyn FnOnce(Option<String>)>;

fn is_wayland(display: &gdk::Display) -> bool {
    display.type_().name() == "GdkWaylandDisplay"
}

/// Place encoded image bytes on the clipboard; `done(error)` runs on the GTK thread.
/// Returns the method used (`"wl-copy"` or `"gtk"`).
pub fn copy_image(application: &gtk::Application, data: Vec<u8>, mime: &str, display: Option<gdk::Display>, done: Option<Done>) -> &'static str {
    let finished: Done = done.unwrap_or_else(|| Box::new(|_| {}));
    let display = display.or_else(gdk::Display::default);
    if let (Some(program), Some(true)) = (wl_copy(), display.as_ref().map(is_wayland)) {
        let guard = application.hold();
        let application = application.clone();
        let mime = mime.to_string();
        glib::MainContext::default().spawn_local(async move {
            let payload = data.clone();
            let result = gio::spawn_blocking(move || run_wl_copy(&program, &payload, &mime)).await
                .unwrap_or_else(|_| Err("wl-copy worker failed".into()));
            drop(guard);
            let error = result.err();
            if error.is_some() {
                gtk_copy(&application, &data, display.as_ref());
            }
            finished(error);
        });
        return "wl-copy";
    }
    gtk_copy(application, &data, display.as_ref());
    finished(None);
    "gtk"
}

fn gtk_copy(application: &gtk::Application, data: &[u8], display: Option<&gdk::Display>) {
    let Some(display) = display.cloned().or_else(gdk::Display::default) else { return };
    match gdk::Texture::from_bytes(&glib::Bytes::from(data)) {
        Ok(texture) => display.clipboard().set_texture(&texture),
        Err(error) => {
            eprintln!("tessel-capture: clipboard image: {}", error.message());
            return;
        }
    }
    // Keep serving the selection for a while after the last window closes.
    let previous = HOLD.with(|hold| hold.borrow_mut().take());
    let guard = match previous {
        Some((source, guard)) => {
            source.remove();
            guard
        }
        None => application.hold(),
    };
    let source = glib::timeout_add_seconds_local_once(FALLBACK_HOLD_SECONDS, || {
        HOLD.with(|hold| hold.borrow_mut().take());
    });
    HOLD.with(|hold| *hold.borrow_mut() = Some((source, guard)));
}
