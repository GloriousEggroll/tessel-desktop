//! Port of test_calculator_ui.py (CalculatorUITests). Runs only inside the
//! private nested headless labwc started by tests/run_headless.sh (own
//! XDG_RUNTIME_DIR, D-Bus and XDG dirs); under plain `cargo test` it skips itself.

use calculator::app::create_application;
use calculator::backend::CalculatorStore;
use calculator::ui::CalculatorWindow;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

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

fn wait(mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(4);
    while Instant::now() < end {
        settle(0.02);
        if predicate() {
            return true;
        }
    }
    false
}

fn d(text: &str) -> calculator::decimal::Decimal { calculator::decimal::Decimal::parse(text).unwrap() }

thread_local!(static SEQUENCE: Cell<u32> = const { Cell::new(0) });

struct Case {
    app: gtk::Application,
    window: CalculatorWindow,
    directory: PathBuf,
}

impl Case {
    fn new() -> Self {
        let sequence = SEQUENCE.with(|s| {
            s.set(s.get() + 1);
            s.get()
        });
        let directory = std::env::temp_dir().join(format!("calculator-ui-{}-{sequence}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let store = CalculatorStore::new(Some(directory.join("state.json")));
        let app = gtk::Application::new(Some(&format!("org.tessel.CalculatorTest{sequence}")), gio::ApplicationFlags::NON_UNIQUE);
        app.register(gio::Cancellable::NONE).unwrap();
        let window = CalculatorWindow::new(&app, Some(store));
        window.window.present();
        settle(0.12);
        Self { app, window, directory }
    }

    fn state(&self) -> PathBuf { self.directory.join("state.json") }
}

impl Drop for Case {
    fn drop(&mut self) {
        self.window.destroy();
        settle(0.05);
        self.app.quit();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn press(window: &CalculatorWindow, key: gdk::Key, state: gdk::ModifierType) -> bool {
    window.keys.emit_by_name::<bool>("key-pressed", &[&key.into_glib(), &0u32, &state])
}

fn primary_menu_and_standard_actions() {
    let case = Case::new();
    let w = &case.window;
    let header = w.menu_button.ancestor(gtk::HeaderBar::static_type()).unwrap();
    assert_eq!(header, w.header.clone().upcast::<gtk::Widget>());
    assert!(tessel_gtk::app_menu::has_menu_icon(&w.menu_button));
    assert!(w.menu_button.has_css_class("flat"));
    for name in ["shortcuts", "about", "quit"] {
        assert!(case.app.lookup_action(name).is_some(), "{name}");
    }
    assert_eq!(case.app.accels_for_action("app.quit").iter().map(|s| s.to_string()).collect::<Vec<_>>(), ["<Control>q"]);
    case.app.activate_action("shortcuts", None);
    settle(0.15);
    let parent: gtk::Window = w.window.clone().upcast();
    let dialogs: Vec<gtk::Window> = gtk::Window::list_toplevels().into_iter().filter_map(|t| t.downcast::<gtk::Window>().ok())
        .filter(|t| t.transient_for().as_ref() == Some(&parent) && t.is_visible()).collect();
    assert_eq!(dialogs.len(), 1);
    dialogs[0].destroy();
    println!("ok primary_menu_and_standard_actions");
}

fn standard_buttons_previous_answer_and_error_recovery() {
    let case = Case::new();
    let w = &case.window;
    for key in ["2", "+", "3", "*", "4", "evaluate"] {
        w.buttons[key].emit_clicked();
    }
    assert_eq!(w.result_label.text(), "14");
    w.input("+");
    w.input("6");
    w.calculate();
    assert_eq!(w.result_label.text(), "20");
    w.input("7");
    assert_eq!(w.expression.text(), "7");
    w.set_expression("1/0");
    assert!(!w.calculate());
    assert!(w.status_label.text().contains("zero"));
    assert!(w.status_label.has_css_class("error"));
    w.input("clear");
    w.insert("sqrt(81)");
    assert!(w.calculate());
    assert_eq!(w.result_label.text(), "9");
    println!("ok standard_buttons_previous_answer_and_error_recovery");
}

fn keyboard_and_selection_editing() {
    let case = Case::new();
    let w = &case.window;
    for key in [gdk::Key::KP_1, gdk::Key::KP_Add, gdk::Key::KP_2, gdk::Key::KP_Enter] {
        assert!(press(w, key, gdk::ModifierType::empty()), "{key:?}");
    }
    assert_eq!(w.result_label.text(), "3");
    w.set_expression("12345");
    w.expression.select_region(1, 4);
    w.input("backspace");
    assert_eq!(w.expression.text(), "15");
    press(w, gdk::Key::Escape, gdk::ModifierType::empty());
    assert_eq!(w.expression.text(), "");
    assert_eq!(w.result_label.text(), "0");
    // Ctrl+H toggles the history; Alt/Super keys are left alone.
    assert!(press(w, gdk::Key::h, gdk::ModifierType::CONTROL_MASK));
    assert!(w.history_panel.is_visible());
    assert!(!press(w, gdk::Key::_1, gdk::ModifierType::ALT_MASK));
    println!("ok keyboard_and_selection_editing");
}

fn scientific_mode_angles_and_layout() {
    let case = Case::new();
    let w = &case.window;
    w.mode_dropdown.set_selected(1);
    settle(0.12);
    assert!(w.scientific_box.is_visible());
    assert!(w.angle_dropdown.is_visible());
    w.set_expression("sin(30)");
    w.calculate();
    assert_eq!(w.result_label.text(), "0.5");
    w.angle_dropdown.set_selected(1);
    w.set_expression("sin(pi/2)");
    w.calculate();
    assert_eq!(w.result_label.text(), "1");
    assert!(wait(|| w.window.height() > 0));
    assert!(w.window.height() <= 700, "height {}", w.window.height());
    assert!(w.buttons["evaluate"].is_mapped());
    let saved = CalculatorStore::new(Some(case.state()));
    assert!(!saved.degrees);
    assert!(saved.scientific);
    println!("ok scientific_mode_angles_and_layout");
}

fn memory_controls_recall_and_clear_even_with_invalid_input() {
    let case = Case::new();
    let w = &case.window;
    w.set_expression("12");
    w.memory_buttons["MS"].emit_clicked();
    assert_eq!(w.store.borrow().memory, Some(d("12")));
    w.set_expression(".5");
    w.memory_buttons["M+"].emit_clicked();
    assert_eq!(w.store.borrow().memory, Some(d("12.5")));
    w.memory_buttons["M−"].emit_clicked();
    assert_eq!(w.store.borrow().memory, Some(d("12")));
    assert_eq!(w.memory_status.text(), "Memory: 12");
    w.input("clear");
    w.memory_buttons["MR"].emit_clicked();
    assert_eq!(w.expression.text(), "12");
    w.set_expression("1/0");
    w.memory_buttons["MC"].emit_clicked();
    assert_eq!(w.store.borrow().memory, None);
    assert!(!w.memory_buttons["MR"].is_sensitive());
    println!("ok memory_controls_recall_and_clear_even_with_invalid_input");
}

fn history_reuse_preserves_previous_answer_context_and_clears() {
    let case = Case::new();
    let w = &case.window;
    w.set_expression("5+5");
    w.calculate();
    w.input("clear");
    w.insert("ans+3");
    w.calculate();
    let entry = w.store.borrow().history[0].clone();
    assert_eq!(entry.result, "13");
    assert!(!entry.expression.contains("ans"));
    assert_eq!(entry.expression, "(10)+3");
    w.set_expression("100");
    w.calculate();
    w.use_history(&entry);
    assert!(w.calculate());
    assert_eq!(w.result_label.text(), "13");
    w.history_button.set_active(true);
    assert!(w.history_panel.is_visible());
    assert!(w.history_scroll.is_visible());
    w.clear_history_button.emit_clicked();
    assert!(w.store.borrow().history.is_empty());
    assert!(w.history_empty.is_visible());
    println!("ok history_reuse_preserves_previous_answer_context_and_clears");
}

fn clipboard_result_and_paste_expression() {
    let case = Case::new();
    let w = &case.window;
    w.set_expression("6*7");
    w.calculate();
    w.copy_result();
    assert_eq!(w.status_label.text(), "Copied to clipboard");
    let values: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = values.clone();
    w.window.clipboard().read_text_async(gio::Cancellable::NONE, move |r| {
        sink.borrow_mut().push(r.ok().flatten().map(|s| s.to_string()).unwrap_or_default());
    });
    assert!(wait(|| !values.borrow().is_empty()));
    assert_eq!(*values.borrow(), ["42"]);
    w.window.clipboard().set_text("(8+2)*3");
    w.input("clear");
    w.paste();
    assert!(wait(|| w.expression.text() == "(8+2)*3"));
    assert!(w.calculate());
    assert_eq!(w.result_label.text(), "30");
    println!("ok clipboard_result_and_paste_expression");
}

fn palette_live_binding_and_close_cleanup() {
    let case = Case::new();
    let w = &case.window;
    let surface = w.window.surface().unwrap();
    let monitor = WidgetExt::display(&w.window).monitor_at_surface(&surface).unwrap();
    let g = monitor.geometry();
    let palette = tessel_gtk::theme::palette_path();
    assert!(!palette.starts_with(tessel_core::paths::home().join(".cache")), "palette must be private: {}", palette.display());
    std::fs::create_dir_all(palette.parent().unwrap()).unwrap();
    std::fs::write(&palette, serde_json::json!({"version": 1, "is_dark": true, "monitors": [
        {"name": "Test screen", "geometry": [g.x(), g.y(), g.width(), g.height()], "background": [39, 31, 47],
         "foreground": [243, 239, 250], "accent": [186, 159, 235]}]}).to_string()).unwrap();
    let theme = w.theme.borrow().clone().unwrap();
    assert!(wait(|| theme.monitor_name().as_deref() == Some("Test screen")));
    let css = theme.provider().unwrap().to_str().to_string();
    assert!(css.contains("39,31,47") || css.contains("39, 31, 47"), "{css}");
    w.window.close();
    assert!(w.closed.get());
    assert!(theme.is_closed());
    let _ = std::fs::remove_file(&palette);
    println!("ok palette_live_binding_and_close_cleanup");
}

fn second_process_reuses_existing_window() {
    let directory = std::env::temp_dir().join(format!("calculator-ui-{}-single", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let state = directory.join("state.json");
    let app = create_application(calculator::APPLICATION_ID, Some(Rc::new(move |app: &gtk::Application| {
        CalculatorWindow::new(app, Some(CalculatorStore::new(Some(state.clone()))))
    })));
    app.app.register(gio::Cancellable::NONE).unwrap();
    assert!(!app.app.is_remote());
    app.show(None);
    let window = app.window().unwrap();
    settle(0.15);
    let run = |argument: &str| {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_tessel-calculator")).arg(argument)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut status = None;
        while Instant::now() < deadline {
            settle(0.02);
            if let Some(code) = child.try_wait().unwrap() {
                status = Some(code);
                break;
            }
        }
        if status.is_none() {
            let _ = child.kill();
        }
        let output = child.wait_with_output().unwrap();
        assert!(status.is_some_and(|s| s.success()), "{status:?} {}", String::from_utf8_lossy(&output.stderr));
    };
    run("7*8");
    assert!(Rc::ptr_eq(&app.window().unwrap().0, &window.0));
    assert_eq!(app.app.windows().len(), 1);
    assert_eq!(window.result_label.text(), "56");
    run("--scientific");
    settle(0.1);
    assert_eq!(window.mode_dropdown.selected(), 1);
    assert!(Rc::ptr_eq(&app.window().unwrap().0, &window.0));
    window.destroy();
    app.app.quit();
    let _ = std::fs::remove_dir_all(&directory);
    println!("ok second_process_reuses_existing_window");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none()
        || std::env::var_os("CALCULATOR_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/calculator/tests/run_headless.sh for the private compositor)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("display");
    primary_menu_and_standard_actions();
    standard_buttons_previous_answer_and_error_recovery();
    keyboard_and_selection_editing();
    scientific_mode_angles_and_layout();
    memory_controls_recall_and_clear_even_with_invalid_input();
    history_reuse_preserves_previous_answer_context_and_clears();
    clipboard_result_and_paste_expression();
    palette_live_binding_and_close_cleanup();
    second_process_reuses_existing_window();
    println!("ui_headless: all checks passed");
}
