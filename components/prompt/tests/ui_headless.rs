//! Port of test_tessel_shell_prompt.py `PromptUITests`. Runs only inside the
//! private nested headless labwc (tests/run_headless.sh); under a plain
//! `cargo test` it skips itself.
#![allow(deprecated)]

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_prompt::{arguments, PromptWindow, ACCEPT, DECLINE, DISMISS, TEXT_LIMIT};
use tessel_theme::TesselShellTheme;

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

thread_local! {
    static SEQUENCE: Cell<u32> = const { Cell::new(0) };
}

struct Case {
    app: gtk::Application,
    results: Rc<RefCell<Vec<i32>>>,
    windows: RefCell<Vec<Rc<PromptWindow>>>,
}

impl Case {
    fn new() -> Self {
        let sequence = SEQUENCE.with(|s| { s.set(s.get() + 1); s.get() });
        let app = gtk::Application::new(Some(&format!("org.tessel.TestPrompt{sequence}")), gio::ApplicationFlags::NON_UNIQUE);
        app.register(gio::Cancellable::NONE).expect("register on the private bus");
        Self { app, results: Rc::default(), windows: RefCell::default() }
    }

    fn window(&self, args: &[&str], stdin_fd: Option<RawFd>) -> Rc<PromptWindow> {
        let results = self.results.clone();
        let window = PromptWindow::new(&self.app, arguments(args), move |code| results.borrow_mut().push(code), stdin_fd);
        self.windows.borrow_mut().push(window.clone());
        window.window.present();
        settle(0.05);
        window
    }

    /// An unmapped window: 64 KiB in the message labels would grow a mapped,
    /// non-resizable window past cairo's surface limit (same in Python).
    fn hidden_window(&self, args: &[&str], stdin_fd: Option<RawFd>) -> Rc<PromptWindow> {
        let results = self.results.clone();
        let window = PromptWindow::new(&self.app, arguments(args), move |code| results.borrow_mut().push(code), stdin_fd);
        self.windows.borrow_mut().push(window.clone());
        window
    }

    fn results(&self) -> Vec<i32> {
        self.results.borrow().clone()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        for window in self.windows.borrow().iter() {
            window.stop_input();
            window.window.destroy();
        }
        self.app.quit();
        settle(0.05);
    }
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut child = widget.first_child();
    while let Some(current) = child {
        out.push(current.clone());
        out.extend(descendants(&current));
        child = current.next_sibling();
    }
    out
}

fn pipe() -> (RawFd, RawFd) {
    let mut fds = [0; 2];
    // SAFETY: plain pipe(2) into a two-element array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    (fds[0], fds[1])
}

fn write_fd(fd: RawFd, data: &[u8]) {
    let mut offset = 0;
    while offset < data.len() {
        // SAFETY: writing from a borrowed slice.
        let count = unsafe { libc::write(fd, data[offset..].as_ptr().cast(), data.len() - offset) };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock || error.kind() == std::io::ErrorKind::Interrupted {
                settle(0.005);
                continue;
            }
            panic!("write: {error}");
        }
        offset += count as usize;
    }
}

fn close_fd(fd: RawFd) {
    // SAFETY: closing a descriptor this test owns.
    unsafe { libc::close(fd) };
}

fn nonblocking(fd: RawFd) {
    // SAFETY: plain fcntl on a test-owned descriptor.
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
}

fn buffer_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string()
}

fn same<A: IsA<gtk::Widget>, B: IsA<gtk::Widget>>(a: Option<A>, b: &B) -> bool {
    a.is_some_and(|a| a.upcast_ref::<gtk::Widget>() == b.upcast_ref::<gtk::Widget>())
}

fn test_confirmation_default_and_each_response_preserve_labwc_protocol() {
    let case = Case::new();
    for (action, code) in [("decline", DECLINE), ("accept", ACCEPT)] {
        let window = case.window(&["--message", "Perform fixture action?", "--decline-label", "Later"], None);
        assert!(window.cancel.is_none(), "A decline button replaces the extra Cancel");
        let (decline, accept) = (window.decline.clone().unwrap(), window.accept.clone().unwrap());
        assert_eq!(decline.label().as_deref(), Some("Later"));
        assert!(same(window.window.default_widget(), &accept));
        assert!(same(RootExt::focus(&window.window), &accept));
        assert!(accept.has_css_class("suggested-action"));
        if action == "decline" { decline.emit_clicked() } else { accept.emit_clicked() }
        assert_eq!(*case.results().last().unwrap(), code);
        let before = case.results();
        window.finish(ACCEPT);
        assert_eq!(case.results(), before);
    }
    assert_eq!(case.results(), vec![1, 0]);
}

fn test_destructive_accept_is_styled_but_not_default() {
    let case = Case::new();
    let window = case.window(&["--message", "Delete fixture?", "--destructive"], None);
    let accept = window.accept.clone().unwrap();
    assert!(accept.has_css_class("destructive-action"));
    assert!(!same(window.window.default_widget(), &accept));
    assert!(same(window.window.default_widget(), window.decline.as_ref().unwrap()));
}

fn test_message_is_heading_and_icon_is_symbolic() {
    let case = Case::new();
    let window = case.window(&["--message", "Restart now?\nUpdates finish installing after restart."], None);
    assert_eq!(window.heading.text(), "Restart now?");
    assert_eq!(window.body.text(), "Updates finish installing after restart.");
    assert_ne!(Some(window.heading.text()), window.window.title());
    let single = case.window(&["--message", "Exit the session?"], None);
    assert_eq!(single.heading.text(), "Exit the session?");
    assert!(!single.body.is_visible());
    let icon = descendants(single.window.upcast_ref()).into_iter()
        .filter_map(|w| w.downcast::<gtk::Image>().ok()).find(|w| w.has_css_class("prompt-icon")).unwrap();
    let names = icon.gicon().unwrap().downcast::<gio::ThemedIcon>().unwrap().names();
    assert!(names.iter().any(|n| n == "dialog-question-symbolic"));
}

fn test_escape_and_window_close_never_approve() {
    let case = Case::new();
    let window = case.window(&["--message", "A <b>literal</b> message"], None);
    assert!(!window.heading.uses_markup());
    assert_eq!(window.heading.text(), "A <b>literal</b> message");
    let controller = window.key_controller.borrow().clone().unwrap();
    let handled: bool = controller.emit_by_name("key-pressed", &[&gdk::Key::Escape.into_glib(), &0u32, &gdk::ModifierType::empty()]);
    assert!(handled);
    let window = case.window(&["--message", "Second prompt"], None);
    let _: bool = window.window.emit_by_name("close-request", &[]);
    assert_eq!(case.results(), vec![DISMISS, DISMISS]);
}

fn test_stdin_message_keeps_confirmation_disabled_until_complete() {
    let case = Case::new();
    let (read_fd, writer) = pipe();
    let window = case.window(&["--message-stdin"], Some(read_fd));
    let accept = window.accept.clone().unwrap();
    assert!(!accept.is_sensitive());
    write_fd(writer, "Apostrophe ' and $(literal)\nnext line\n".as_bytes());
    settle(0.05);
    assert!(!accept.is_sensitive());
    close_fd(writer);
    settle(0.05);
    assert_eq!(window.message_text(), "Apostrophe ' and $(literal)\nnext line");
    assert!(accept.is_sensitive());
    assert!(same(RootExt::focus(&window.window), &accept));
    assert!(!window.input_watched());
    drop(case);
    close_fd(read_fd);
}

fn test_stdin_truncated_message_stays_disabled() {
    let case = Case::new();
    let (read_fd, writer) = pipe();
    let window = case.hidden_window(&["--message-stdin"], Some(read_fd));
    nonblocking(writer);
    for _ in 0..(TEXT_LIMIT / 4096 + 4) {
        write_fd(writer, &b"fixture action line\n".repeat(4096 / 20));
        settle(0.01);
    }
    close_fd(writer);
    settle(0.1);
    assert!(window.input_truncated());
    assert_eq!(window.input_len(), TEXT_LIMIT);
    assert!(!window.accept.as_ref().unwrap().is_sensitive());
    assert!(!window.decline.as_ref().unwrap().is_sensitive());
    assert!(window.message_text().ends_with("Additional text was omitted."));
    drop(case);
    close_fd(read_fd);
}

fn test_error_stdin_streams_and_retains_only_bounded_text() {
    let case = Case::new();
    let (read_fd, writer) = pipe();
    let window = case.window(&["--error"], Some(read_fd));
    nonblocking(writer);
    write_fd(writer, b"Configuration fixture error\n");
    settle(0.05);
    let details = window.details.clone().unwrap();
    assert!(buffer_text(&details).contains("fixture error"));
    let mut line = b"fixture diagnostic line\n".to_vec();
    line.resize(32, b' ');
    let block: Vec<u8> = line.repeat(128);
    for _ in 0..(TEXT_LIMIT / 4096 + 4) {
        write_fd(writer, &block);
        settle(0.01);
    }
    settle(0.05);
    assert_eq!(window.input_len(), TEXT_LIMIT);
    assert!(window.input_truncated());
    assert!(buffer_text(&details).contains("Additional text"));
    window.cancel.clone().unwrap().emit_clicked();
    assert_eq!(case.results(), vec![ACCEPT]);
    assert!(!window.input_watched());
    drop(case);
    close_fd(read_fd);
    close_fd(writer);
}

fn test_unreadable_stdin_reports_error() {
    let case = Case::new();
    let window = case.window(&["--message-stdin"], Some(987));
    assert!(window.heading.text().starts_with("Could not read message: [Errno 9]"), "{}", window.heading.text());
    assert!(!window.accept.as_ref().unwrap().is_sensitive());
}

fn background_rgb(widget: &impl IsA<gtk::Widget>) -> (f64, f64, f64) {
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 100, 100).unwrap();
    {
        let context = cairo::Context::new(&surface).unwrap();
        gtk::render_background(&widget.as_ref().style_context(), &context, 0.0, 0.0, 100.0, 100.0);
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().unwrap();
    let pixel = &data[50 * stride + 200..50 * stride + 204];
    let a = pixel[3] as f64;
    if a == 0.0 {
        return (0.0, 0.0, 0.0);
    }
    (pixel[2] as f64 * 255.0 / a, pixel[1] as f64 * 255.0 / a, pixel[0] as f64 * 255.0 / a)
}

fn test_prompt_follows_live_shell_palette() {
    let case = Case::new();
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("prompt-palette-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("palette.json");
    let mut data = json!({"version": 1, "is_dark": true, "monitors": [{
        "name": "HEADLESS-1", "geometry": [0, 0, 1280, 720],
        "background": [25, 20, 30], "foreground": [225, 230, 235], "accent": [190, 50, 90]}]});
    std::fs::write(&path, data.to_string()).unwrap();
    let window = case.window(&["--message", "Themed fixture"], None);
    let theme = TesselShellTheme::new(Some(path.clone()));
    for accent in [[190, 50, 90], [40, 120, 80]] {
        data["monitors"][0]["accent"] = json!(accent);
        std::fs::write(&path, data.to_string()).unwrap();
        theme.refresh();
        settle(0.2);
        assert!(theme.themed_windows().iter().any(|w| w == window.window.upcast_ref::<gtk::Window>()));
        let color = background_rgb(window.accept.as_ref().unwrap());
        for (actual, expected) in [(color.0, accent[0]), (color.1, accent[1]), (color.2, accent[2])] {
            assert!((actual - expected as f64).abs() <= 1.0, "{color:?} vs {accent:?}");
        }
    }
    theme.close();
    let _ = std::fs::remove_dir_all(dir);
}

fn main() {
    if std::env::var("TESSEL_SHELL_UI_ISOLATED").as_deref() != Ok("1") {
        println!("ui_headless: skipped (run tests/run_headless.sh for the private compositor)");
        return;
    }
    gtk::init().expect("gtk init in the private compositor");
    let tests: [(&str, fn()); 9] = [
        ("test_confirmation_default_and_each_response_preserve_labwc_protocol", test_confirmation_default_and_each_response_preserve_labwc_protocol),
        ("test_destructive_accept_is_styled_but_not_default", test_destructive_accept_is_styled_but_not_default),
        ("test_message_is_heading_and_icon_is_symbolic", test_message_is_heading_and_icon_is_symbolic),
        ("test_escape_and_window_close_never_approve", test_escape_and_window_close_never_approve),
        ("test_stdin_message_keeps_confirmation_disabled_until_complete", test_stdin_message_keeps_confirmation_disabled_until_complete),
        ("test_stdin_truncated_message_stays_disabled", test_stdin_truncated_message_stays_disabled),
        ("test_error_stdin_streams_and_retains_only_bounded_text", test_error_stdin_streams_and_retains_only_bounded_text),
        ("test_unreadable_stdin_reports_error", test_unreadable_stdin_reports_error),
        ("test_prompt_follows_live_shell_palette", test_prompt_follows_live_shell_palette),
    ];
    let mut failed = 0;
    let filter = std::env::var("UI_TEST_FILTER").unwrap_or_default();
    for (name, test) in tests {
        if !name.contains(&filter) {
            continue;
        }
        let result = std::panic::catch_unwind(test);
        println!("test {name} ... {}", if result.is_ok() { "ok" } else { "FAILED" });
        failed += usize::from(result.is_err());
    }
    println!("ui_headless: {} passed, {failed} failed", tests.len() - failed);
    std::process::exit(i32::from(failed > 0));
}
