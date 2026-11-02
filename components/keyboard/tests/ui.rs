//! Port of the Python test_ui.py (plus the tessel-lock
//! keyboard check). Runs only inside a private nested headless labwc started by
//! tests/run_headless.sh; under plain `cargo test` it skips itself. The input
//! helper (virtual keyboard + input method) therefore only ever connects to
//! that private compositor.
use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::LayerShell;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_gtk::gtk;
use tessel_keyboard::config::Config;
use tessel_keyboard::devices::Device;
use tessel_keyboard::keyboard::Mode;
use tessel_keyboard::manager::{Manager, Options};

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        for _ in 0..100 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn settle_until(seconds: f64, done: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end && !done() {
        settle(0.02);
    }
}

fn unique() -> String {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!("{}x{}", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
}

fn wlrctl(args: &[&str]) {
    let status = Command::new("wlrctl").args(["pointer"]).args(args).status().expect("wlrctl");
    assert!(status.success());
}

struct Fixture {
    manager: Rc<Manager>,
    target_app: gtk::Application,
    target: gtk::ApplicationWindow,
    entry: gtk::Entry,
}

impl Fixture {
    fn new() -> Self {
        let config_home = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap());
        let options = Options {
            application_id: format!("org.tessel.TestKeyboard{}", unique()),
            device_loader: Rc::new(Vec::new),
            watch_devices: false,
            config: Config::new(config_home.join(format!("keyboard-{}", unique())).join("config.json")),
            helper: Some(PathBuf::from(tessel_keyboard::BUILT_HELPER)),
        };
        let manager = Manager::new(options);
        manager.app.register(None::<&gio::Cancellable>).unwrap();
        let target_app = gtk::Application::new(Some(&format!("org.tessel.TestEntry{}", unique())), Default::default());
        target_app.register(None::<&gio::Cancellable>).unwrap();
        let target = gtk::ApplicationWindow::builder().application(&target_app).default_width(600).default_height(240).build();
        let entry = gtk::Entry::new();
        target.set_child(Some(&entry));
        target.present();
        entry.grab_focus();
        settle(0.2);
        Self { manager, target_app, target, entry }
    }

    fn show(&self) {
        self.manager.show_keyboard();
        settle_until(3.0, || self.manager.bridge_ready() && self.manager.panel().is_some_and(|p| p.is_mapped()));
        settle(0.2);
        let panel = self.manager.panel().unwrap_or_else(|| panic!("no panel: {}", self.manager.error()));
        assert!(panel.is_mapped(), "panel not mapped: {}", self.manager.error());
        assert!(self.manager.bridge_ready(), "{}", self.manager.error());
    }

    /// The target window stays active with its entry (its GtkText child) focused.
    fn focused(&self) -> bool {
        self.target.is_active() && gtk::prelude::GtkWindowExt::focus(&self.target).is_some_and(|w| w.is_ancestor(&self.entry) || w == self.entry)
    }

    fn text(&self) -> String { self.entry.text().to_string() }

    fn wait_text(&self, wanted: &str) {
        settle_until(2.0, || self.text() == wanted);
        settle(0.05);
        assert_eq!(self.text(), wanted);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.shutdown();
        for window in self.manager.app.windows() {
            window.destroy();
        }
        self.target.destroy();
        let _ = &self.target_app;
        settle(0.2);
    }
}

fn touch() -> Device { Device::new("Fixture touch", "touch", "touchscreen") }
fn keys() -> Device { Device::new("Fixture keys", "keys", "keyboard") }

// test_ui.py:41
fn keyboard_uses_pointer_output_without_moving_text_focus(f: &Fixture) {
    let monitors = gdk::Display::default().unwrap().monitors();
    assert!(monitors.n_items() >= 2, "needs two headless outputs");
    let first: gdk::Monitor = monitors.item(0).and_downcast().unwrap();
    let second: gdk::Monitor = monitors.item(1).and_downcast().unwrap();
    let focused = WidgetExt::display(&f.target).monitor_at_surface(&f.target.surface().unwrap());
    let target = if focused.as_ref() == Some(&first) { second } else { first };
    let area = target.geometry();
    wlrctl(&["move", "-100000", "-100000"]);
    wlrctl(&["move", &(area.x() + 60).to_string(), &(area.y() + 60).to_string()]);
    f.show();
    assert_eq!(f.manager.panel().unwrap().monitor().as_ref(), Some(&target));
    f.manager.keyboard().unwrap().press(30);
    f.wait_text("a");
    assert!(f.focused(), "the text field keeps keyboard focus");
}

// test_ui.py:54
fn real_wayland_text_entry_and_hide_button_without_focus_theft(f: &Fixture) {
    f.show();
    let keyboard = f.manager.keyboard().unwrap();
    keyboard.press(30);
    keyboard.modifier(1);
    keyboard.press(48);
    f.wait_text("aB");
    keyboard.character("€");
    f.wait_text("aB€");
    keyboard.press(14);
    f.wait_text("aB");
    keyboard.hide_button().emit_clicked();
    settle(0.2);
    assert!(!f.manager.panel().unwrap().is_visible());
    assert!(f.manager.bridge().is_none());
    assert!(f.focused(), "no focus theft");
}

// test_ui.py:63
fn automatic_and_live_keyboard_attach_detach(f: &Fixture) {
    f.manager.hardware_changed(vec![touch()]);
    settle_until(3.0, || f.manager.panel().is_some_and(|p| p.is_visible()));
    assert!(f.manager.policy().eligible);
    assert!(f.manager.policy().active, "GTK input-method focus was not received");
    assert!(f.manager.panel().unwrap().is_visible());
    f.manager.keyboard().unwrap().hide_button().emit_clicked();
    settle(0.2);
    assert!(!f.manager.panel().unwrap().is_visible());
    let epoch = f.manager.policy().epoch;
    let event = serde_json::json!({"event": "context", "active": true, "epoch": epoch});
    f.manager.input_event(event.as_object().unwrap().clone());
    settle(0.2);
    assert!(!f.manager.panel().unwrap().is_visible(), "hide sticks within the same epoch");
    f.manager.hardware_changed(vec![touch(), keys()]);
    settle(0.2);
    assert!(f.manager.bridge().is_none());
    assert!(!f.manager.launcher().unwrap().is_visible());
    f.manager.hardware_changed(vec![touch()]);
    settle_until(3.0, || f.manager.panel().is_some_and(|p| p.is_visible()));
    assert!(f.manager.panel().unwrap().is_visible());
}

// test_ui.py:78
fn layout_switch_and_modifier_shortcuts(f: &Fixture) {
    f.show();
    let keyboard = f.manager.keyboard().unwrap();
    let bridge = f.manager.bridge().unwrap();
    bridge.send(&tessel_keyboard::backend::Command::Layout("de".into())).unwrap();
    settle(0.3);
    keyboard.press(21);
    f.wait_text("z");
    keyboard.modifier(2);
    keyboard.press(30);
    keyboard.press(14);
    f.wait_text("");
    bridge.send(&tessel_keyboard::backend::Command::Layout("fr".into())).unwrap();
    settle(0.3);
    keyboard.press(26);
    keyboard.press(18);
    f.wait_text("ê");
}

// test_ui.py:87
fn legacy_input_without_text_input_protocol(f: &Fixture) {
    f.entry.set_im_module(Some("simple"));
    f.entry.grab_focus();
    f.show();
    let keyboard = f.manager.keyboard().unwrap();
    keyboard.press(30);
    keyboard.character("€");
    f.wait_text("a€");
}

// test_ui.py:91
fn real_pointer_key_and_layout_size(f: &Fixture) {
    f.show();
    let (keyboard, panel) = (f.manager.keyboard().unwrap(), f.manager.panel().unwrap());
    let button = keyboard.character_button(30).unwrap();
    let bounds = button.compute_bounds(&panel).expect("bounds");
    let monitor = f.manager.monitor().or_else(|| WidgetExt::display(&panel).monitors().item(0).and_downcast()).unwrap();
    let area = monitor.geometry();
    let x = area.x() as f32 + (area.width() - panel.width()) as f32 / 2.0 + bounds.x() + bounds.width() / 2.0;
    let y = (area.y() + area.height() - panel.height() - 8) as f32 + bounds.y() + bounds.height() / 2.0;
    let (x, y) = ((x as i32).to_string(), (y as i32).to_string());
    for args in [vec!["move", "-100000", "-100000"], vec!["move", x.as_str(), y.as_str()], vec!["click"]] {
        wlrctl(&args);
        settle(0.05);
    }
    f.wait_text("a");
    assert!(panel.width() <= area.width());
    keyboard.root().add_css_class("keyboard-narrow");
    keyboard.root().set_size_request(0, -1);
    settle(0.2);
    let (minimum, _, _, _) = keyboard.root().measure(gtk::Orientation::Horizontal, -1);
    assert!(minimum <= 650, "narrow minimum width {minimum}");
}

// test_ui.py:108
fn desktop_keyboard_suspends_while_lock_is_active(f: &Fixture) {
    f.manager.hardware_changed(vec![touch()]);
    settle_until(3.0, || f.manager.panel().is_some_and(|p| p.is_visible()));
    assert!(f.manager.panel().unwrap().is_visible());
    f.manager.session_locked(true);
    settle(0.2);
    assert!(!f.manager.panel().unwrap().is_visible());
    assert!(f.manager.bridge().is_none());
    assert!(!f.manager.launcher().unwrap().is_visible());
    f.manager.session_locked(false);
    settle_until(3.0, || f.manager.panel().is_some_and(|p| p.is_visible()));
    assert!(f.manager.panel().unwrap().is_visible());
}

// test_ui.py:117
fn pin_field_and_manual_show_without_touchscreen(f: &Fixture) {
    f.entry.set_input_purpose(gtk::InputPurpose::Pin);
    f.show();
    settle_until(2.0, || f.manager.keyboard().is_some_and(|k| k.mode() == Mode::Numbers));
    let keyboard = f.manager.keyboard().unwrap();
    assert_eq!(keyboard.mode(), Mode::Numbers);
    keyboard.character("123");
    f.wait_text("123");
    keyboard.change_mode(Mode::Letters);
    keyboard.modifier(1);
    keyboard.modifier(1);
    assert!(keyboard.shift_locked());
    assert_eq!(keyboard.modifier_button(1).unwrap().label().as_deref(), Some("⇪"));
    f.manager.hide_keyboard();
    assert_eq!(keyboard.flags(), 0);
    assert!(!keyboard.shift_locked());
}

// test_ui.py:125
fn settings_save_and_update_the_existing_keyboard(f: &Fixture) {
    f.show();
    f.manager.show_settings();
    settle(0.2);
    let window = f.manager.settings_window().unwrap();
    assert!(f.manager.app.lookup_action("about").is_some(), "the suite's primary menu is installed");
    assert_eq!(window.title().as_deref(), Some("On-screen Keyboard Settings"));
    let controls = f.manager.settings_controls().unwrap();
    controls.layout().set_selected(3); // German, after System/US/UK.
    controls.size().set_selected(2);
    controls.automatic().set_active(false);
    settle_until(2.0, || f.manager.keyboard().unwrap().label("21").is_some_and(|l| l[0] == "z"));
    assert_eq!(f.manager.config().layout, "de");
    assert_eq!(f.manager.config().size, "large");
    assert!(!f.manager.config().automatic);
    assert_eq!(f.manager.keyboard().unwrap().label("21").unwrap()[0], "z");
    assert!(f.manager.keyboard().unwrap().root().has_css_class("keyboard-large"));
    f.manager.show_settings();
    assert_eq!(f.manager.settings_window().as_ref(), Some(&window));
    window.close();
    settle(0.2);
    assert!(f.manager.settings_window().is_none());
}

// test_ui.py:139
fn failed_input_helper_can_be_reopened_in_automatic_mode(f: &Fixture) {
    f.manager.hardware_changed(vec![touch()]);
    settle_until(3.0, || f.manager.bridge_ready());
    let previous = f.manager.bridge().unwrap();
    previous.process().force_exit();
    settle_until(2.0, || f.manager.bridge_failed());
    assert!(f.manager.bridge_failed());
    assert!(!f.manager.keyboard().unwrap().body().is_sensitive());
    f.manager.hide_keyboard();
    f.show();
    assert!(!f.manager.bridge().unwrap().same(&previous));
    assert!(f.manager.bridge_ready());
    f.manager.keyboard().unwrap().press(30);
    f.wait_text("a");
}

/// State file published for Tessel Dock (osk_status.py reads it).
fn state_file_tracks_visibility(f: &Fixture) {
    let path = tessel_keyboard::status::state_path();
    f.show();
    let read = || serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap()).unwrap();
    settle_until(1.0, || read()["visible"] == true);
    let state = read();
    assert_eq!(state["pid"], std::process::id());
    assert_eq!(state["visible"], true);
    assert_eq!(state["ready"], true);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(&format!("{{\"pid\": {}, \"visible\": true, \"automatic_available\": false", std::process::id())), "{text}");
    assert!(text.ends_with("}\n"));
    let status = tessel_keyboard::status::status(Some(&path));
    assert_eq!(status["class"], "active");
    f.manager.hide_keyboard();
    settle(0.1);
    assert_eq!(read()["visible"], false);
    f.manager.shutdown();
    assert!(!path.exists(), "removed at shutdown");
}

/// tessel-lock/test_lock_ui.py: touch keyboard edits the selected lock field without IPC.
fn lock_keyboard_edits_the_selected_lock_field_without_ipc(_: &Fixture) {
    let config_home = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap());
    let store = Config::new(config_home.join(format!("lock-{}", unique())).join("config.json"));
    let window = gtk::Window::builder().default_width(800).default_height(500).build();
    let overlay = gtk::Overlay::new();
    let scroll = gtk::ScrolledWindow::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let entry = gtk::Entry::new();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&entry);
    content.append(&actions);
    scroll.set_child(Some(&content));
    overlay.set_child(Some(&scroll));
    window.set_child(Some(&overlay));
    window.present();
    let submitted = Rc::new(Cell::new(0));
    let (entry_for_host, entry_for_focus, count) = (entry.clone(), entry.clone(), submitted.clone());
    let lock = tessel_keyboard::lock::LockKeyboard::new(tessel_keyboard::lock::LockHost {
        entry: Rc::new(move || entry_for_host.clone()),
        submit: Rc::new(move || count.set(count.get() + 1)),
        is_visible: Rc::new(|| true),
        focus: Rc::new(move || { entry_for_focus.grab_focus_without_selecting(); }),
        overlay: overlay.clone(), scroll: scroll.clone().upcast(), monitor: None, button_box: actions.clone(),
        size_group: Some(gtk::SizeGroup::new(gtk::SizeGroupMode::Both)), button_class: "lock-action".into(),
        scale_factor: 1, direction: gtk::TextDirection::Ltr, helper: Some(PathBuf::from(tessel_keyboard::BUILT_HELPER)),
        store: Some(store.clone()), watch_devices: false,
    });
    assert!(lock.button().has_css_class("lock-action"));
    assert!(!lock.button().has_tooltip());
    // Touchscreen without keyboard reveals it automatically.
    lock.devices_changed(&[touch()]);
    settle(0.2);
    let widget = lock.widget().expect("lock keyboard shown");
    assert!(widget.root().is_visible());
    assert!(!widget.hide_button().has_tooltip(), "secure mode: no tooltips");
    assert!(widget.character_button(30).unwrap().tooltip_text().is_none());
    assert!(scroll.margin_bottom() > 16, "space reserved for the keyboard");
    widget.press(30);
    widget.modifier(1);
    widget.press(48);
    widget.character("€");
    assert_eq!(entry.text(), "aB€");
    widget.press(105);
    widget.press(14);
    assert_eq!(entry.text(), "a€");
    widget.modifier(2);
    widget.press(30); // Ctrl+A selects all
    widget.press(31); // replaces the selection with "s"
    assert_eq!(entry.text(), "s");
    widget.modifier(2);
    widget.press(47); // other Ctrl shortcuts (paste) are ignored
    assert_eq!(entry.text(), "s");
    widget.press(28);
    assert_eq!(submitted.get(), 1);
    widget.press(1);
    assert!(!widget.root().is_visible());
    lock.close();
    window.destroy();

    // French dead keys compose without the helper's Wayland mode.
    store.save(&serde_json::json!({"layout": "fr"})).unwrap();
    let window = gtk::Window::new();
    let overlay = gtk::Overlay::new();
    let scroll = gtk::ScrolledWindow::new();
    let entry = gtk::Entry::new();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&entry);
    column.append(&actions);
    scroll.set_child(Some(&column));
    overlay.set_child(Some(&scroll));
    window.set_child(Some(&overlay));
    window.present();
    let entry_for_host = entry.clone();
    let lock = tessel_keyboard::lock::LockKeyboard::new(tessel_keyboard::lock::LockHost {
        entry: Rc::new(move || entry_for_host.clone()), submit: Rc::new(|| {}), is_visible: Rc::new(|| true), focus: Rc::new(|| {}),
        overlay, scroll: scroll.upcast(), monitor: None, button_box: actions, size_group: None, button_class: "lock-action".into(),
        scale_factor: 1, direction: gtk::TextDirection::Ltr, helper: Some(PathBuf::from(tessel_keyboard::BUILT_HELPER)),
        store: Some(store), watch_devices: false,
    });
    lock.button().emit_clicked();
    settle(0.1);
    let widget = lock.widget().unwrap();
    widget.press(26);
    widget.press(18);
    assert_eq!(entry.text(), "ê");
    widget.press(26);
    widget.press(26);
    assert_eq!(entry.text(), "ê^");
    lock.close();
    window.destroy();
}

/// Regression (gtk_widget_is_visible vs get_visible): a lock window created while the
/// login is already visible (tessel-lock monitor hot-plug) reveals the touch keyboard
/// before it is mapped. osk_lock.py reserve()/toggle() read the keyboard's own
/// visible flag, so the form still gets its bottom margin and toggle() hides it;
/// WidgetExt::is_visible() (false until the window is shown) left the margin at 0
/// and made toggle() re-show instead of hiding.
fn lock_keyboard_revealed_before_the_window_is_mapped(_: &Fixture) {
    let config_home = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap());
    let store = Config::new(config_home.join(format!("lock-{}", unique())).join("config.json"));
    let window = gtk::Window::builder().default_width(800).default_height(500).build();
    let overlay = gtk::Overlay::new();
    let scroll = gtk::ScrolledWindow::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let entry = gtk::Entry::new();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&entry);
    content.append(&actions);
    scroll.set_child(Some(&content));
    overlay.set_child(Some(&scroll));
    window.set_child(Some(&overlay));
    let entry_for_host = entry.clone();
    let lock = tessel_keyboard::lock::LockKeyboard::new(tessel_keyboard::lock::LockHost {
        entry: Rc::new(move || entry_for_host.clone()), submit: Rc::new(|| {}), is_visible: Rc::new(|| true), focus: Rc::new(|| {}),
        overlay: overlay.clone(), scroll: scroll.clone().upcast(), monitor: None, button_box: actions, size_group: None,
        button_class: "lock-action".into(), scale_factor: 1, direction: gtk::TextDirection::Ltr,
        helper: Some(PathBuf::from(tessel_keyboard::BUILT_HELPER)), store: Some(store), watch_devices: false,
    });
    lock.devices_changed(&[touch()]);
    assert!(!window.is_visible(), "window not shown yet");
    let widget = lock.widget().expect("lock keyboard revealed");
    assert!(widget.root().get_visible());
    assert!(scroll.margin_bottom() > 16, "space reserved before the window is mapped: {}", scroll.margin_bottom());
    lock.toggle();
    assert!(!widget.root().get_visible(), "toggle hides the revealed keyboard");
    assert_eq!(scroll.margin_bottom(), 0);
    lock.toggle();
    assert!(widget.root().get_visible());
    window.present();
    settle(0.2);
    assert!(widget.root().is_visible());
    assert!(scroll.margin_bottom() > 16, "still reserved once mapped");
    lock.close();
    window.destroy();
}

type Case = (&'static str, fn(&Fixture));

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    // Only ever inside the private compositor (its runtime dir lives under /run/user/1000/cbt/).
    if std::env::var_os("WAYKEYS_UI").is_none() || !runtime.contains("/cbt/") || display.is_empty() {
        println!("skipped: run tests/run_headless.sh (private headless labwc only)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("GTK");
    assert!(gtk4_layer_shell::is_supported(), "layer shell");
    let only: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let cases: [Case; 13] = [
        ("keyboard_uses_pointer_output_without_moving_text_focus", keyboard_uses_pointer_output_without_moving_text_focus),
        ("real_wayland_text_entry_and_hide_button_without_focus_theft", real_wayland_text_entry_and_hide_button_without_focus_theft),
        ("automatic_and_live_keyboard_attach_detach", automatic_and_live_keyboard_attach_detach),
        ("layout_switch_and_modifier_shortcuts", layout_switch_and_modifier_shortcuts),
        ("legacy_input_without_text_input_protocol", legacy_input_without_text_input_protocol),
        ("real_pointer_key_and_layout_size", real_pointer_key_and_layout_size),
        ("desktop_keyboard_suspends_while_lock_is_active", desktop_keyboard_suspends_while_lock_is_active),
        ("pin_field_and_manual_show_without_touchscreen", pin_field_and_manual_show_without_touchscreen),
        ("settings_save_and_update_the_existing_keyboard", settings_save_and_update_the_existing_keyboard),
        ("failed_input_helper_can_be_reopened_in_automatic_mode", failed_input_helper_can_be_reopened_in_automatic_mode),
        ("state_file_tracks_visibility", state_file_tracks_visibility),
        ("lock_keyboard_edits_the_selected_lock_field_without_ipc", lock_keyboard_edits_the_selected_lock_field_without_ipc),
        ("lock_keyboard_revealed_before_the_window_is_mapped", lock_keyboard_revealed_before_the_window_is_mapped),
    ];
    let failures = RefCell::new(Vec::new());
    for (name, case) in cases {
        if !only.is_empty() && !only.iter().any(|o| name.contains(o.as_str())) {
            continue;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let fixture = Fixture::new();
            case(&fixture);
        }));
        match result {
            Ok(()) => println!("ok {name}"),
            Err(_) => {
                println!("FAILED {name}");
                failures.borrow_mut().push(name);
            }
        }
        settle(0.2);
    }
    let failures = failures.into_inner();
    println!("{} failed", failures.len());
    if !failures.is_empty() {
        std::process::exit(1);
    }
}
