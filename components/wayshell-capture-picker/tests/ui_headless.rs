//! test_capture_picker_ui.py and test_capture_portal.py in a private nested
//! headless labwc (tests/run_headless.sh). Under plain `cargo test` it skips.
//!
//! The portal cases run the real /usr/libexec/xdg-desktop-portal-wlr on the
//! private, service-free bus with a private minimal PipeWire core; no Start
//! call is made. Synthetic keys (wtype) go only to the private compositor.

use gio::prelude::*;
use glib::translate::IntoGlib;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use suite_gtk::gtk;
use suite_gtk::gtk::prelude::*;
use wayshell_capture_picker::sources::{parse_sources, CaptureSource, Kind};
use wayshell_capture_picker::ui::{CapturePicker, Options};

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

fn wait_for(predicate: impl Fn() -> bool, timeout: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        if predicate() {
            return true;
        }
        settle(0.02);
    }
    predicate()
}

fn source(token: &str, kind: Kind, title: &str, subtitle: &str, icon: &str) -> CaptureSource {
    CaptureSource { token: token.into(), kind, title: title.into(), subtitle: subtitle.into(), icon_name: icon.into() }
}

fn sources(count: usize) -> Vec<CaptureSource> {
    let mut values = vec![
        source("Monitor: DP-1", Kind::Monitor, "Dell UltraSharp", "DP-1 · 2560 × 1440", "video-display-symbolic"),
        source("Monitor: HDMI-A-1", Kind::Monitor, "Samsung Display", "HDMI-A-1 · 1920 × 1080", "video-display-symbolic"),
    ];
    for number in 0..count {
        let title = if number == 0 { "<b>Notes & plans</b>".to_owned() } else { format!("Project {number} — Work in progress") };
        values.push(source(&format!("Window: 0x{:x}", number + 1), Kind::Window, &title,
                           if number == 0 { "Text Editor" } else { "Terminal" },
                           if number == 0 { "accessories-text-editor-symbolic" } else { "utilities-terminal-symbolic" }));
    }
    values
}

#[derive(Clone, Debug, PartialEq)]
enum Action {
    Share(CaptureSource),
    Cancel,
}

struct Case {
    app: gtk::Application,
    actions: Rc<RefCell<Vec<Action>>>,
    view: RefCell<Option<CapturePicker>>,
}

thread_local!(static SEQUENCE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });

impl Case {
    fn new() -> Self {
        let n = SEQUENCE.with(|s| { s.set(s.get() + 1); s.get() });
        let app = gtk::Application::new(Some(&format!("org.wayshell.CapturePickerTest{n}")), gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gio::Cancellable>).unwrap();
        Self { app, actions: Rc::new(RefCell::new(Vec::new())), view: RefCell::new(None) }
    }

    fn picker_with(&self, items: Vec<CaptureSource>, options: Options) -> CapturePicker {
        let (share, cancel) = (self.actions.clone(), self.actions.clone());
        let view = CapturePicker::new(&self.app, items, Box::new(move |s| share.borrow_mut().push(Action::Share(s))),
                                      Box::new(move || cancel.borrow_mut().push(Action::Cancel)), options);
        view.present();
        settle(0.18);
        self.view.replace(Some(view.clone()));
        view
    }

    fn picker(&self, items: Option<Vec<CaptureSource>>, remote_control: u8) -> CapturePicker {
        self.picker_with(items.unwrap_or_else(|| sources(5)), Options { remote_control, icons: Some(HashMap::new()), palette: None })
    }

    fn actions(&self) -> Vec<Action> { self.actions.borrow().clone() }
}

impl Drop for Case {
    fn drop(&mut self) {
        if let Some(view) = self.view.take() {
            view.destroy();
        }
        settle(0.05);
        self.app.quit();
    }
}

fn row(view: &CapturePicker, kind: Kind, index: i32) -> gtk::ListBoxRow { view.list(kind).unwrap().row_at_index(index).unwrap() }

fn explicit_selection_and_share_returns_original_source_once() {
    let case = Case::new();
    let items = sources(5);
    let view = case.picker(Some(items.clone()), 0);
    assert!(view.window.has_css_class("capture-picker"));
    assert_eq!(view.window.title().as_deref(), Some("Share your screen"));
    assert!(view.share_button.has_css_class("primary"));
    assert!(view.selected().is_none());
    assert!(!view.share_button.is_sensitive());
    assert!(case.actions().is_empty());
    // Even an emitted disabled-button signal cannot consent without selection.
    view.share_button.emit_clicked();
    assert!(case.actions().is_empty());
    let second = row(&view, Kind::Monitor, 1);
    view.list(Kind::Monitor).unwrap().select_row(Some(&second));
    assert!(view.share_button.is_sensitive());
    assert!(case.actions().is_empty());
    assert_eq!(view.row_for(&second).unwrap().check.opacity(), 1.0);
    assert_eq!(view.selection_label.label(), "Samsung Display");
    view.share_button.emit_clicked();
    view.share_button.emit_clicked();
    view.cancel();
    assert_eq!(case.actions(), [Action::Share(items[1].clone())]);
    assert!(view.theme().unwrap().is_closed());
    assert!(view.is_finished());
}

fn notices(view: &CapturePicker) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut stack = vec![view.window.child().unwrap()];
    while let Some(widget) = stack.pop() {
        if widget.has_css_class("capture-remote") {
            found.push(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
    found
}

fn remote_control_request_is_disclosed() {
    let case = Case::new();
    assert!(notices(&case.picker(None, 0)).is_empty());
    case.view.borrow().clone().unwrap().cancel();
    for (devices, word) in [(3, "keyboard and mouse."), (1, "keyboard."), (2, "mouse.")] {
        let view = case.picker(None, devices);
        let found = notices(&view);
        assert_eq!(found.len(), 1);
        let label = found[0].last_child().unwrap().downcast::<gtk::Label>().unwrap();
        assert!(label.label().ends_with(&format!("control your {word}")), "{}", label.label());
        assert_eq!(case.actions().last(), Some(&Action::Cancel));
        view.cancel();
    }
}

fn tab_switch_and_search_clear_hidden_selection() {
    let case = Case::new();
    let view = case.picker(Some(sources(18)), 0);
    assert!(view.search.is_visible());
    assert!(view.switcher.is_visible());
    view.list(Kind::Monitor).unwrap().select_row(Some(&row(&view, Kind::Monitor, 0)));
    assert!(view.share_button.is_sensitive());
    view.stack.set_visible_child_name("window");
    assert!(view.selected().is_none());
    assert!(!view.share_button.is_sensitive());
    let first = row(&view, Kind::Window, 0);
    view.list(Kind::Window).unwrap().select_row(Some(&first));
    assert!(view.share_button.is_sensitive());
    view.search.set_text("PROJECT 2");
    view.search_changed();
    assert!(!view.share_button.is_sensitive());
    assert!(!first.is_child_visible());
    assert!(row(&view, Kind::Window, 2).is_child_visible());
    assert!(case.actions().is_empty());
}

fn cancel_escape_and_window_close_never_share() {
    for action in ["button", "escape", "close"] {
        let case = Case::new();
        let view = case.picker(None, 0);
        view.list(Kind::Monitor).unwrap().select_row(Some(&row(&view, Kind::Monitor, 0)));
        match action {
            "button" => view.cancel_button.emit_clicked(),
            "escape" => {
                let keys = view.keys.borrow().clone().unwrap();
                let handled: bool = keys.emit_by_name("key-pressed", &[&gtk::gdk::Key::Escape.into_glib(), &0u32, &gtk::gdk::ModifierType::empty()]);
                assert!(handled);
            }
            _ => view.window.close(),
        }
        settle(0.05);
        view.cancel();
        assert_eq!(case.actions(), [Action::Cancel], "{action}");
    }
}

fn deliberate_row_activation_and_plain_window_titles() {
    let case = Case::new();
    let view = case.picker(None, 0);
    view.stack.set_visible_child_name("window");
    let listing = view.list(Kind::Window).unwrap();
    assert!(!listing.activates_on_single_click());
    let first = row(&view, Kind::Window, 0);
    let entry = view.row_for(&first).unwrap();
    assert!(!entry.title_label.uses_markup());
    assert_eq!(entry.title_label.label(), "<b>Notes & plans</b>");
    assert!(entry.title_label.has_css_class("capture-title") && entry.subtitle_label.has_css_class("muted"));
    assert!(entry.check.has_css_class("capture-selection"));
    listing.emit_by_name::<()>("row-activated", &[&first]);
    assert_eq!(case.actions(), [Action::Share(view.sources[entry.source].clone())]);
}

fn keyboard_navigation_selects_without_sharing() {
    let case = Case::new();
    let view = case.picker(None, 0);
    let listing = view.list(Kind::Monitor).unwrap();
    let (first, second) = (row(&view, Kind::Monitor, 0), row(&view, Kind::Monitor, 1));
    first.grab_focus();
    settle(0.05);
    assert!(listing.selected_row().is_none(), "Keyboard focus alone must not select a source");
    listing.emit_by_name::<()>("move-cursor", &[&gtk::MovementStep::DisplayLines, &1i32, &false, &false]);
    settle(0.05);
    assert_eq!(listing.selected_row().as_ref(), Some(&second));
    assert_eq!(view.selected(), Some(&view.sources[1]));
    assert!(case.actions().is_empty());
    listing.emit_by_name::<()>("activate-cursor-row", &[]);
    assert_eq!(case.actions(), [Action::Share(view.sources[1].clone())]);
}

fn empty_source_list_cannot_share() {
    let case = Case::new();
    let view = case.picker(Some(Vec::new()), 0);
    assert!(!view.switcher.is_visible());
    assert!(!view.search.is_visible());
    assert_eq!(view.stack.visible_child_name().as_deref(), Some("empty"));
    assert!(!view.share_button.is_sensitive());
    view.share_button.emit_clicked();
    assert!(case.actions().is_empty());
    view.cancel_button.emit_clicked();
    assert_eq!(case.actions(), [Action::Cancel]);
}

fn long_source_list_scrolls_inside_720p() {
    let case = Case::new();
    let view = case.picker(Some(sources(80)), 0);
    view.stack.set_visible_child_name("window");
    settle(0.12);
    assert!(view.window.height() <= 660, "height {}", view.window.height());
    assert!(view.window.width() <= 700, "width {}", view.window.width());
    let scroller = view.stack.visible_child().unwrap().first_child().unwrap().downcast::<gtk::ScrolledWindow>().unwrap();
    let adjustment = scroller.vadjustment();
    assert!(adjustment.upper() > adjustment.page_size(), "{} <= {}", adjustment.upper(), adjustment.page_size());
    assert!(view.cancel_button.is_mapped());
    assert!(view.share_button.is_mapped());
}

fn destroy_by_portal_cancellation_does_not_consent() {
    let case = Case::new();
    let view = case.picker(None, 0);
    view.destroy();
    view.destroy();
    view.cancel();
    assert!(case.actions().is_empty());
    assert!(view.theme().unwrap().is_closed());
}

fn live_per_monitor_palette_is_attached() {
    let case = Case::new();
    let path = std::env::temp_dir().join("capture-palette.json");
    let _ = std::fs::remove_file(&path);
    let client = suite_gtk::theme::PaletteClient::new(Some(path.clone()));
    let view = case.picker_with(sources(5), Options { icons: Some(HashMap::new()), palette: Some(client.clone()), ..Default::default() });
    let display = WidgetExt::display(&view.window);
    let monitor = display.monitor_at_surface(&view.window.surface().unwrap()).unwrap();
    let g = monitor.geometry();
    let write = |extra: serde_json::Value| {
        let mut palette = serde_json::json!({"version": 1, "is_dark": true, "monitors": [
            {"name": "Test monitor", "geometry": [g.x(), g.y(), g.width(), g.height()], "background": [39, 31, 47],
             "foreground": [243, 239, 250], "accent": [186, 159, 235]}]});
        for (k, v) in extra.as_object().unwrap() {
            palette[k] = v.clone();
        }
        std::fs::write(&path, serde_json::to_vec(&palette).unwrap()).unwrap();
        client.refresh();
        view.theme().unwrap().refresh();
    };
    write(serde_json::json!({}));
    let theme = view.theme().unwrap();
    assert_eq!(theme.monitor_name().as_deref(), Some("Test monitor"));
    assert!(view.window.has_css_class(theme.scope()));
    let css = theme.provider().expect("palette provider").to_str().to_string();
    assert!(css.contains("rgb(39,31,47)"), "{css}");
    write(serde_json::json!({"apps_transparency_enabled": true, "apps_transparency": 45}));
    let css = view.theme().unwrap().provider().unwrap().to_str().to_string();
    assert!(css.contains("rgba(39,31,47,0.55"), "{css}");
    let _ = std::fs::remove_file(&path);
}

fn icons_follow_the_toplevel_snapshot() {
    // `Options.icons = None` runs the real `waydock-toplevel --snapshot`
    // (the suite-wayland helper, on this private display only).
    let Some(helper) = std::env::var_os("WAYSHELL_CAPTURE_TOPLEVEL_HELPER") else {
        println!("skip icons_follow_the_toplevel_snapshot (no helper)");
        return;
    };
    assert!(Path::new(&helper).is_file());
    let case = Case::new();
    let started = Instant::now();
    let view = case.picker_with(sources(2), Options::default());
    assert!(started.elapsed() < Duration::from_secs(3), "snapshot timeout bounds construction");
    assert_eq!(view.rows().len(), 4);
}

// ----- binary / real portal cases -----

fn toplevel_mapped() -> bool {
    Command::new("wlrctl").args(["toplevel", "list"]).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("org.wayshell.CapturePicker")).unwrap_or(false)
}

fn binary() -> &'static str { env!("CARGO_BIN_EXE_wayshell-capture-picker") }

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn wait_child(child: &mut Child, timeout: f64) -> Option<std::process::ExitStatus> {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() > end {
            return None;
        }
        settle(0.02);
    }
}

fn binary_signals_and_parent_exit_cancel() {
    let dir = scratch("cap-bin");
    let input = dir.join("in");
    std::fs::write(&input, b"Monitor: HEADLESS-1 Headless output\n").unwrap();
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let mut child = Command::new(binary()).stdin(std::fs::File::open(&input).unwrap()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().unwrap();
        assert!(wait_for(toplevel_mapped, 8.0), "picker did not map");
        unsafe { libc::kill(child.id() as i32, signal) };
        let status = wait_child(&mut child, 5.0).expect("signal did not end the picker");
        let mut out = Vec::new();
        std::io::Read::read_to_end(child.stdout.as_mut().unwrap(), &mut out).unwrap();
        assert_eq!((status.code(), out.as_slice()), (Some(0), &b""[..]), "signal {signal}");
        assert!(wait_for(|| !toplevel_mapped(), 3.0));
    }
    // The portal (our parent) going away cancels within the 1 s poll.
    // The shell stays alive (blocked on its stdin) until the picker mapped.
    let script = format!("{} < {} > {} 2>/dev/null & echo $! > {}; read line", suite_core::shell::shell_quote(binary()),
                         input.display(), dir.join("out").display(), dir.join("pid").display());
    let mut shell = Command::new("sh").arg("-c").arg(script).stdin(Stdio::piped()).spawn().unwrap();
    assert!(wait_for(toplevel_mapped, 8.0), "orphan picker did not map");
    drop(shell.stdin.take());
    assert!(wait_child(&mut shell, 3.0).is_some());
    let pid: i32 = std::fs::read_to_string(dir.join("pid")).unwrap().trim().parse().unwrap();
    let gone = || std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| s.contains(") Z ")).unwrap_or(true);
    assert!(wait_for(gone, 4.0), "orphaned picker kept running");
    assert_eq!(std::fs::read(dir.join("out")).unwrap(), b"");
}

const BACKEND: &str = "/usr/libexec/xdg-desktop-portal-wlr";
const NAME: &str = "org.freedesktop.impl.portal.desktop.wlr";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENCAST: &str = "org.freedesktop.impl.portal.ScreenCast";

/// `(o handle, o session_handle, s app_id, a{sv} options)`.
fn request(kind: &str, options: glib::Variant) -> glib::Variant {
    glib::Variant::tuple_from_iter([
        glib::variant::ObjectPath::try_from(format!("{PORTAL_PATH}/request/test/{kind}")).unwrap().to_variant(),
        glib::variant::ObjectPath::try_from(format!("{PORTAL_PATH}/session/test/capture")).unwrap().to_variant(),
        "org.wayshell.CapturePortalTest".to_variant(),
        options,
    ])
}

struct Portal {
    dir: PathBuf,
    bus: gio::DBusConnection,
    env: Vec<(String, String)>,
    pipewire: Option<Child>,
    backend: Option<Child>,
    keyboard: Option<Child>,
    window: gtk::Window,
    response: Rc<RefCell<Option<Result<u32, String>>>>,
}

impl Portal {
    fn new() -> Self {
        let dir = scratch("cap-portal");
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
        std::fs::write(dir.join("pipewire.conf"), "context.properties = {\n    core.daemon = true\n    core.name = pipewire-0\n    support.dbus = false\n}\n\
context.spa-libs = { support.* = support/libspa-support }\ncontext.modules = [\n    { name = libpipewire-module-protocol-native }\n\
    { name = libpipewire-module-client-node }\n    { name = libpipewire-module-metadata }\n    { name = libpipewire-module-access }\n]\n").unwrap();
        let env = vec![("PIPEWIRE_RUNTIME_DIR".to_owned(), runtime.clone()), ("PIPEWIRE_REMOTE".to_owned(), "pipewire-0".to_owned())];
        let log = std::fs::File::create(dir.join("backend.log")).unwrap();
        let pipewire = Command::new("pipewire").arg("-c").arg(dir.join("pipewire.conf")).envs(env.iter().cloned())
            .stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap();
        let socket = Path::new(&runtime).join("pipewire-0");
        assert!(wait_for(|| socket.exists(), 5.0), "private PipeWire core failed to start");
        let window = gtk::Window::builder().title("Portal fixture — only the private desktop").default_width(300).default_height(180).build();
        window.set_child(Some(&gtk::Label::new(Some("Synthetic capture source"))));
        window.present();
        assert!(wait_for(|| window.is_mapped(), 5.0));
        settle(0.15);
        let bus = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).unwrap();
        Self { dir, bus, env, pipewire: Some(pipewire), backend: None, keyboard: None, window, response: Rc::new(RefCell::new(None)) }
    }

    fn owner(&self) -> bool {
        self.bus.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner",
                           Some(&(NAME,).to_variant()), None, gio::DBusCallFlags::NONE, 1000, None::<&gio::Cancellable>)
            .ok().and_then(|v| v.get::<(bool,)>()).is_some_and(|(b,)| b)
    }

    fn record(&self) -> PathBuf { self.dir.join("records") }

    fn start_request(&mut self, types: u32, mode: &str, extra_env: &[(&str, &str)]) {
        assert!(!self.owner(), "private bus unexpectedly already contains a portal backend");
        let wrapper = self.dir.join("chooser.sh");
        std::fs::write(&wrapper, format!("#!/bin/sh\n\
rec=\"$1\"; cat > \"$rec.tmp\"; mv \"$rec.tmp\" \"$rec\"\n\
case \"$2\" in\n  select) head -n 1 \"$rec\" ;;\n  cancel) ;;\n\
  ui) {bin} < \"$rec\" > \"$rec.response\"; status=$?; cat \"$rec.response\"; exit $status ;;\n  *) exit 2 ;;\nesac\n",
            bin = suite_core::shell::shell_quote(binary()))).unwrap();
        std::fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let command = suite_core::shell::shell_join(&[wrapper.to_string_lossy().into_owned(), self.record().to_string_lossy().into_owned(), mode.into()]);
        let config = self.dir.join("portal.conf");
        std::fs::write(&config, format!("[screencast]\nchooser_type=dmenu\nchooser_cmd={command}\n")).unwrap();
        let log = std::fs::OpenOptions::new().append(true).open(self.dir.join("backend.log")).unwrap();
        use std::os::unix::process::CommandExt;
        let backend = Command::new(BACKEND).args(["-l", "DEBUG", "-c"]).arg(&config).envs(self.env.iter().cloned())
            .envs(extra_env.iter().map(|(k, v)| (k.to_string(), v.to_string())))
            .stdout(log.try_clone().unwrap()).stderr(log).process_group(0).spawn().unwrap();
        self.backend = Some(backend);
        assert!(wait_for(|| self.owner(), 5.0), "private backend failed to start: {}", self.log());
        let create = self.bus.call_sync(Some(NAME), PORTAL_PATH, SCREENCAST, "CreateSession",
            Some(&request("create", glib::VariantDict::new(None).end())),
            None, gio::DBusCallFlags::NONE, 3000, None::<&gio::Cancellable>).unwrap();
        assert_eq!(create.child_value(0).get::<u32>(), Some(0));
        let options = glib::VariantDict::new(None);
        options.insert_value("types", &types.to_variant());
        let response = self.response.clone();
        response.replace(None);
        self.bus.call(Some(NAME), PORTAL_PATH, SCREENCAST, "SelectSources",
            Some(&request("select", options.end())),
            None, gio::DBusCallFlags::NONE, 15000, None::<&gio::Cancellable>, move |result| {
                response.replace(Some(result.map(|v| v.child_value(0).get::<u32>().unwrap_or(99)).map_err(|e| e.to_string())));
            });
        let record = self.record();
        assert!(wait_for(|| record.exists(), 8.0), "backend did not invoke the configured chooser: {}", self.log());
    }

    fn finish_request(&self, status: u32) {
        assert!(wait_for(|| self.response.borrow().is_some(), 10.0), "the backend did not return from source selection");
        assert_eq!(self.response.borrow().clone().unwrap(), Ok(status), "{}", self.log());
    }

    fn log(&self) -> String { std::fs::read_to_string(self.dir.join("backend.log")).unwrap_or_default() }

    fn choices(&self) -> Vec<CaptureSource> { parse_sources(&std::fs::read(self.record()).unwrap()).unwrap() }

    fn ui_request(&mut self, extra_env: &[(&str, &str)]) {
        self.keyboard = Some(Command::new("wtype").args(["-s", "25000"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
        settle(0.1);
        self.start_request(1, "ui", extra_env);
        assert!(wait_for(toplevel_mapped, 8.0), "production chooser did not map its GTK4 window");
        settle(0.2);
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            // Own process group: also closes its shell/chooser children.
            unsafe { libc::killpg(backend.id() as i32, libc::SIGTERM) };
            if wait_child(&mut backend, 3.0).is_none() {
                unsafe { libc::killpg(backend.id() as i32, libc::SIGKILL) };
                let _ = backend.wait();
            }
        }
        for child in [self.keyboard.take(), self.pipewire.take()].into_iter().flatten() {
            let mut child = child;
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            let _ = wait_child(&mut child, 3.0);
        }
        self.window.destroy();
        settle(0.05);
        let _ = wait_for(|| !self.owner(), 3.0);
    }
}

fn monitor_request_supplies_only_real_headless_monitors_and_accepts_exact_record() {
    let mut portal = Portal::new();
    portal.start_request(1, "select", &[]);
    portal.finish_request(0);
    let choices = portal.choices();
    assert_eq!(choices.len(), 2, "{choices:?}");
    assert!(choices.iter().all(|c| c.kind == Kind::Monitor && c.subtitle.starts_with("HEADLESS-")));
}

fn window_request_supplies_eligible_window_identifier_and_accepts_exact_record() {
    let mut portal = Portal::new();
    portal.start_request(2, "select", &[]);
    portal.finish_request(0);
    let choices = portal.choices();
    assert_eq!(choices.len(), 1, "{choices:?}");
    assert_eq!(choices[0].kind, Kind::Window);
    assert_eq!(Some(choices[0].title.as_str()), portal.window.title().as_deref());
    assert!(!choices[0].subtitle.is_empty());
}

fn empty_chooser_output_cancels_instead_of_selecting_a_default() {
    let mut portal = Portal::new();
    portal.start_request(3, "cancel", &[]);
    portal.finish_request(1);
    let kinds: std::collections::BTreeSet<_> = portal.choices().iter().map(|c| c.kind.name()).collect();
    assert_eq!(kinds, ["monitor", "window"].into());
}

fn wtype(args: &[&str]) {
    assert!(Command::new("wtype").args(args).status().unwrap().success());
}

fn production_chooser_escape_returns_portal_cancellation() {
    let mut portal = Portal::new();
    portal.ui_request(&[]);
    wtype(&["-s", "100", "-k", "Escape"]);
    portal.finish_request(1);
    assert_eq!(std::fs::read(portal.record().with_extension("response")).unwrap(), b"");
}

fn production_chooser_keyboard_acceptance_returns_original_monitor_record() {
    let mut portal = Portal::new();
    portal.ui_request(&[]);
    // Cancel owns initial focus. Navigate backward into the source list;
    // activating the row is an explicit share action, like double-click.
    wtype(&["-s", "100", "-M", "shift", "-k", "Tab", "-m", "shift", "-s", "100", "-k", "Return"]);
    portal.finish_request(0);
    let selected = std::fs::read(portal.record().with_extension("response")).unwrap();
    let records = std::fs::read(portal.record()).unwrap();
    assert!(records.split(|b| *b == b'\n').any(|line| !line.is_empty() && [line, b"\n"].concat() == selected),
            "{selected:?} not among {records:?}");
}

fn production_chooser_maps_while_frontend_settings_portal_is_busy() {
    let mut portal = Portal::new();
    // A pending Settings.ReadAll on the private bus reproduces the frontend
    // dependency cycle; the chooser must never contact it.
    let node = gio::DBusNodeInfo::for_xml(r#"<node><interface name="org.freedesktop.portal.Settings">
      <method name="ReadAll"><arg name="namespaces" type="as" direction="in"/><arg name="settings" type="a{sa{sv}}" direction="out"/></method>
      <method name="Read"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
      <property name="version" type="u" access="read"/></interface></node>"#).unwrap();
    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let pending = Rc::new(RefCell::new(Vec::<gio::DBusMethodInvocation>::new()));
    let (c1, c2, p1) = (calls.clone(), calls.clone(), pending.clone());
    let registration = portal.bus.register_object(PORTAL_PATH, &node.lookup_interface("org.freedesktop.portal.Settings").unwrap())
        .method_call(move |_, _, _, _, method, _, invocation| {
            c1.borrow_mut().push(method.to_owned());
            p1.borrow_mut().push(invocation);
        })
        .property(move |_, _, _, _, name| {
            c2.borrow_mut().push(name.to_owned());
            2u32.to_variant()
        })
        .build().unwrap();
    let reply = portal.bus.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        Some(&("org.freedesktop.portal.Desktop", 0u32).to_variant()), None, gio::DBusCallFlags::NONE, 1000, None::<&gio::Cancellable>).unwrap();
    assert_eq!(reply.get::<(u32,)>(), Some((1,)), "the private portal name was unexpectedly occupied");
    let started = Instant::now();
    // Force GTK to look for portals; the chooser must override this locally.
    portal.ui_request(&[("GDK_DEBUG", "portals")]);
    assert!(started.elapsed() < Duration::from_secs(5), "the chooser waited on a frontend portal before showing");
    assert!(calls.borrow().is_empty(), "the chooser recursively contacted the frontend portal: {:?}", calls.borrow());
    wtype(&["-s", "100", "-k", "Escape"]);
    portal.finish_request(1);
    assert_eq!(std::fs::read(portal.record().with_extension("response")).unwrap(), b"");
    for invocation in pending.take() {
        invocation.return_dbus_error("org.freedesktop.portal.Error.Cancelled", "The private test portal is closing");
    }
    let _ = portal.bus.unregister_object(registration);
    let _ = portal.bus.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
        Some(&("org.freedesktop.portal.Desktop",).to_variant()), None, gio::DBusCallFlags::NONE, 1000, None::<&gio::Cancellable>);
}

fn main() {
    if std::env::var("CAPTURE_UI").as_deref() != Ok("1") {
        eprintln!("ui_headless: skipped (run tests/run_headless.sh)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    assert!(runtime.starts_with("/run/user/1000/cbt/") && !display.is_empty() && bus.contains("/run/user/1000/cbt/"),
            "refusing to run outside the private headless session");
    suite_core::i18n::initialize();
    // The picker binary hardens its own environment; this process (a portal
    // client stand-in) must not use portals either.
    std::env::set_var("GDK_DEBUG", "no-portals");
    gtk::init().unwrap();
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    let only = std::env::var("CAPTURE_UI_CASE").ok().filter(|c| !c.is_empty());
    let cases: &[(&str, fn())] = &[
        ("explicit_selection_and_share_returns_original_source_once", explicit_selection_and_share_returns_original_source_once),
        ("remote_control_request_is_disclosed", remote_control_request_is_disclosed),
        ("tab_switch_and_search_clear_hidden_selection", tab_switch_and_search_clear_hidden_selection),
        ("cancel_escape_and_window_close_never_share", cancel_escape_and_window_close_never_share),
        ("deliberate_row_activation_and_plain_window_titles", deliberate_row_activation_and_plain_window_titles),
        ("keyboard_navigation_selects_without_sharing", keyboard_navigation_selects_without_sharing),
        ("empty_source_list_cannot_share", empty_source_list_cannot_share),
        ("long_source_list_scrolls_inside_720p", long_source_list_scrolls_inside_720p),
        ("destroy_by_portal_cancellation_does_not_consent", destroy_by_portal_cancellation_does_not_consent),
        ("live_per_monitor_palette_is_attached", live_per_monitor_palette_is_attached),
        ("icons_follow_the_toplevel_snapshot", icons_follow_the_toplevel_snapshot),
        ("binary_signals_and_parent_exit_cancel", binary_signals_and_parent_exit_cancel),
        ("monitor_request_supplies_only_real_headless_monitors_and_accepts_exact_record",
         monitor_request_supplies_only_real_headless_monitors_and_accepts_exact_record),
        ("window_request_supplies_eligible_window_identifier_and_accepts_exact_record",
         window_request_supplies_eligible_window_identifier_and_accepts_exact_record),
        ("empty_chooser_output_cancels_instead_of_selecting_a_default", empty_chooser_output_cancels_instead_of_selecting_a_default),
        ("production_chooser_escape_returns_portal_cancellation", production_chooser_escape_returns_portal_cancellation),
        ("production_chooser_maps_while_frontend_settings_portal_is_busy", production_chooser_maps_while_frontend_settings_portal_is_busy),
        ("production_chooser_keyboard_acceptance_returns_original_monitor_record",
         production_chooser_keyboard_acceptance_returns_original_monitor_record),
    ];
    let mut failed = 0;
    for (name, case) in cases {
        if only.as_deref().is_some_and(|o| o != *name) {
            continue;
        }
        let result = std::panic::catch_unwind(case);
        settle(0.1);
        match result {
            Ok(()) => println!("ok {name}"),
            Err(_) => {
                println!("FAILED {name}");
                failed += 1;
            }
        }
    }
    println!("{} failed", failed);
    if failed > 0 {
        std::process::exit(1);
    }
}
