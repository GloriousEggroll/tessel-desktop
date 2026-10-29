//! Welcome GUI checks (port of test_welcome.py WelcomeUITests) in the private
//! headless labwc. Run only through tests/run_headless.sh.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gs_base::{tags, Host, SettingsPage};
use gtk::prelude::*;
use gtk::{gio, glib};
use welcome::state::InstallStatus;
use welcome::ui::{step_keys, ColorSettings, Options, WelcomeWindow};

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

#[derive(Default)]
struct FakeColors {
    value: RefCell<String>,
    handlers: RefCell<HashMap<u64, Rc<dyn Fn()>>>,
    next: Cell<u64>,
}

impl ColorSettings for FakeColors {
    fn get(&self) -> String { self.value.borrow().clone() }
    fn set(&self, value: &str) -> bool {
        *self.value.borrow_mut() = value.to_owned();
        let handlers: Vec<_> = self.handlers.borrow().values().cloned().collect();
        for callback in handlers {
            callback();
        }
        true
    }
    fn connect_changed(&self, callback: Rc<dyn Fn()>) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        self.handlers.borrow_mut().insert(id, callback);
        id
    }
    fn disconnect(&self, handler: u64) { self.handlers.borrow_mut().remove(&handler); }
}

type Events = Rc<RefCell<Vec<(String, String)>>>;

struct Fixture {
    app: gtk::Application,
    events: Events,
    launched: Rc<RefCell<Vec<Vec<String>>>>,
    colors: Rc<FakeColors>,
    done: PathBuf,
    window: Option<WelcomeWindow>,
}

fn app() -> gtk::Application {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let app = gtk::Application::new(Some(&format!("org.waydock.TestWelcome{n}")), gio::ApplicationFlags::NON_UNIQUE);
    app.register(gio::Cancellable::NONE).unwrap();
    app
}

fn fixture() -> Fixture {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let done = PathBuf::from(std::env::var("TMPDIR").unwrap()).join(format!("welcome-{n}/state/desktop-suite/setup-done"));
    let colors = Rc::new(FakeColors::default());
    *colors.value.borrow_mut() = "default".into();
    Fixture { app: app(), events: Rc::default(), launched: Rc::default(), colors, done, window: None }
}

impl Fixture {
    fn factory(&self) -> Rc<dyn Fn(&str, &Host) -> Result<SettingsPage, String>> {
        let events = self.events.clone();
        Rc::new(move |section: &str, _host: &Host| {
            events.borrow_mut().push(("create".into(), section.into()));
            let (e1, e2, s1, s2) = (events.clone(), events.clone(), section.to_owned(), section.to_owned());
            Ok(SettingsPage::new(gtk::Label::new(Some(section)))
                .with_close(move || e1.borrow_mut().push(("close".into(), s1.clone())))
                .with_activate(move || e2.borrow_mut().push(("activate".into(), s2.clone()))))
        })
    }

    fn make(&mut self, mut options: Options) -> WelcomeWindow {
        if options.page_factory.is_none() {
            options.page_factory = Some(self.factory());
        }
        if options.status.is_none() {
            options.status = Some(Some(InstallStatus { ok: true, failed: vec![] }));
        }
        if options.color_settings.is_none() {
            options.color_settings = Some(Some(self.colors.clone()));
        }
        options.done_path = Some(self.done.clone());
        let launched = self.launched.clone();
        options.launch = Some(Rc::new(move |argv| launched.borrow_mut().push(argv)));
        let window = WelcomeWindow::new(&self.app, options);
        window.window.present();
        settle(0.1);
        self.window = Some(window.clone());
        window
    }

    fn count(&self, kind: &str, section: &str) -> usize {
        self.events.borrow().iter().filter(|(k, s)| k == kind && s == section).count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(window) = &self.window {
            if !window.closed.get() {
                window.cleanup();
                window.window.destroy();
            }
        }
        self.app.quit();
        settle(0.02);
    }
}

fn walks_every_step_and_finishes() {
    let mut f = fixture();
    let window = f.make(Options::default());
    assert_eq!(window.current(), Some("welcome"));
    assert!(!window.back_button.is_sensitive());
    let mut visited = vec![window.current().unwrap()];
    for _ in 1..step_keys().len() {
        window.next_button.emit_clicked();
        settle(0.05);
        visited.push(window.current().unwrap());
    }
    assert_eq!(visited, step_keys());
    assert_eq!(tags::get_str(&window.step_list.selected_row().unwrap(), "step").as_deref(), Some("done"));
    assert_eq!(window.next_button.label().unwrap(), "Start Using Your Desktop");
    assert_eq!(window.progress.label(), "Step 7 of 7");
    let creates: Vec<String> = f.events.borrow().iter().filter(|e| e.0 == "create").map(|e| e.1.clone()).collect();
    assert_eq!(creates, vec!["language", "keyboard", "layout"]);
    window.back_button.emit_clicked();
    settle(0.05);
    assert_eq!(window.current(), Some("shortcuts"));
    window.go(1);
    assert!(!f.done.exists());
    window.next_button.emit_clicked();
    settle(0.05);
    assert!(f.done.exists());
    assert!(window.closed.get());
    let mut closes: Vec<String> = f.events.borrow().iter().filter(|e| e.0 == "close").map(|e| e.1.clone()).collect();
    closes.sort();
    assert_eq!(closes, vec!["keyboard", "language", "layout"]);
}

fn sidebar_jumps_and_reuses_pages() {
    let mut f = fixture();
    let window = f.make(Options::default());
    window.step_list.select_row(Some(&window.rows["layout"]));
    settle(0.05);
    assert_eq!(window.current(), Some("layout"));
    assert!(window.rows["keyboard"].has_css_class("welcome-step-done"));
    window.step_list.select_row(Some(&window.rows["language"]));
    window.step_list.select_row(Some(&window.rows["layout"]));
    settle(0.05);
    assert_eq!(f.count("create", "layout"), 1);
    assert_eq!(f.count("activate", "layout"), 2);
}

fn light_and_dark() {
    let mut f = fixture();
    let window = f.make(Options::default());
    window.show_step("style");
    settle(0.05);
    assert!(window.scheme_buttons.borrow()["default"].is_active());
    window.scheme_buttons.borrow()["prefer-dark"].set_active(true);
    assert_eq!(*f.colors.value.borrow(), "prefer-dark");
    f.colors.set("prefer-light");
    settle(0.05);
    assert!(window.scheme_buttons.borrow()["prefer-light"].is_active());
    window.window.close();
    assert!(f.colors.handlers.borrow().is_empty());
}

fn missing_color_settings_disable_the_choice() {
    let mut f = fixture();
    let window = f.make(Options { color_settings: Some(None), ..Options::default() });
    window.show_step("style");
    assert!(window.scheme_message.borrow().as_ref().unwrap().label().contains("unavailable"));
}

fn shortcut_overview_and_cheat_sheet() {
    let mut f = fixture();
    let window = f.make(Options::default());
    window.show_step("shortcuts");
    settle(0.05);
    let keys: Vec<String> = window.shortcut_rows.borrow().iter().map(|r| r.0.clone()).collect();
    for expected in ["Super + L", "Super + /", "Ctrl + Alt + Delete", "Print Screen", "Super", "Alt + Tab", "Super + Enter"] {
        assert!(keys.contains(&expected.to_owned()), "{expected} in {keys:?}");
    }
    let mut buttons = Vec::new();
    fn walk(widget: &gtk::Widget, out: &mut Vec<gtk::Button>) {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if !widget.is::<gtk::ToggleButton>() {
                out.push(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            walk(&item, out);
            child = item.next_sibling();
        }
    }
    walk(&window.page("shortcuts").unwrap().widget, &mut buttons);
    buttons[0].emit_clicked();
    assert_eq!(f.launched.borrow().last().unwrap().last().unwrap(), "--shortcuts");
    buttons[1].emit_clicked();
    let last = f.launched.borrow().last().unwrap().clone();
    assert_eq!(&last[last.len() - 2..], &["--section".to_string(), "shortcuts".to_string()]);
}

fn unavailable_page_links_to_settings() {
    let mut f = fixture();
    let window = f.make(Options { page_factory: Some(Rc::new(|_: &str, _: &Host| Err("keyboard_page is not installed".into()))),
                                  ..Options::default() });
    window.show_step("keyboard");
    settle(0.05);
    assert!(window.is_fallback("keyboard"));
    window.window.close();
    assert!(window.closed.get());
}

fn failed_installation_is_shown_and_setup_stays_pending() {
    let mut f = fixture();
    let window = f.make(Options { status: Some(Some(InstallStatus { ok: false, failed: vec!["gtk4".into()] })), ..Options::default() });
    assert!(window.warning.borrow().as_ref().unwrap().is_visible());
    window.show_step("done");
    window.go(1);
    assert!(window.closed.get());
    assert!(!f.done.exists());
}

fn busy_page_keeps_the_window_open() {
    let mut f = fixture();
    let busy = Rc::new(Cell::new(true));
    let b = busy.clone();
    let window = f.make(Options { page_factory: Some(Rc::new(move |section: &str, _: &Host| {
        let b = b.clone();
        Ok(SettingsPage::new(gtk::Label::new(Some(section))).with_can_close(move || !b.get()))
    })), ..Options::default() });
    window.show_step("language");
    window.show_step("done");
    window.window.close();
    settle(0.05);
    assert!(!window.closed.get());
    assert_eq!(window.current(), Some("language"));
    busy.set(false);
    window.window.close();
    assert!(window.closed.get());
}

fn escape_and_alt_arrows() {
    let mut f = fixture();
    let window = f.make(Options::default());
    assert!(window.key(gtk::gdk::Key::Right, gtk::gdk::ModifierType::ALT_MASK));
    assert_eq!(window.current(), Some("language"));
    assert!(window.key(gtk::gdk::Key::Left, gtk::gdk::ModifierType::ALT_MASK));
    assert_eq!(window.current(), Some("welcome"));
    assert!(!window.key(gtk::gdk::Key::Right, gtk::gdk::ModifierType::empty()));
    assert!(window.key(gtk::gdk::Key::Escape, gtk::gdk::ModifierType::empty()));
    assert!(window.closed.get());
}

/// A second launch is forwarded to the primary and routes --page; invalid
/// --page values are ignored; argparse errors print in the launcher.
fn single_instance_routes_page() {
    use welcome::app::{WelcomeApplication, WindowFactory};
    let colors = Rc::new(FakeColors::default());
    *colors.value.borrow_mut() = "default".into();
    let first_runs = Rc::new(RefCell::new(Vec::new()));
    let (c, fr) = (colors.clone(), first_runs.clone());
    let factory: WindowFactory = Rc::new(move |app: &gtk::Application, first_run: bool| {
        fr.borrow_mut().push(first_run);
        WelcomeWindow::new(app, Options {
            page_factory: Some(Rc::new(|s: &str, _: &Host| Ok(SettingsPage::new(gtk::Label::new(Some(s)))))),
            color_settings: Some(Some(c.clone())), status: Some(None), first_run,
            done_path: Some(PathBuf::from(std::env::var("TMPDIR").unwrap()).join("welcome-si/setup-done")),
            launch: Some(Rc::new(|_| {})) })
    });
    let app = WelcomeApplication::new(welcome::ui::APPLICATION_ID, Some(factory));
    app.app.register(gio::Cancellable::NONE).unwrap();
    let hold = app.app.hold();
    assert_eq!(app.command_line(&["--first-run".into()]), 0);
    settle(0.05);
    let window = app.window.borrow().clone().unwrap();
    let binary = env!("CARGO_BIN_EXE_desktop-suite-welcome");
    let run = |args: &[&str]| -> (i32, String) {
        let mut child = std::process::Command::new(binary).args(args).stderr(std::process::Stdio::piped()).spawn().unwrap();
        for _ in 0..200 {
            settle(0.03);
            if child.try_wait().unwrap().is_some() {
                break;
            }
        }
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into())
    };
    assert_eq!(run(&["--page", "style"]).0, 0);
    assert_eq!(window.current(), Some("style"));
    assert_eq!(run(&["--pa", "nonsense"]).0, 0);
    assert_eq!(window.current(), Some("style"));
    let (code, err) = run(&["--page"]);
    assert_eq!(code, 2);
    assert!(err.ends_with("welcome.py: error: argument --page: expected one argument\n"), "{err}");
    assert!(Rc::ptr_eq(&app.window.borrow().clone().unwrap().0, &window.0));
    assert_eq!(*first_runs.borrow(), vec![true]);
    window.window.close();
    drop(hold);
    settle(0.05);
}

/// The real Global Settings pages embed (fake HOME, private bus): the
/// language wizard is driven by Welcome's buttons and its own chrome is hidden.
#[cfg(feature = "pages")]
fn real_settings_pages_embed() {
    let mut f = fixture();
    let window = f.make(Options { page_factory: Some(Rc::new(welcome::ui::default_page_factory)), ..Options::default() });
    for key in ["language", "keyboard", "layout"] {
        window.show_step(key);
        settle(0.3);
        assert!(!window.is_fallback(key), "{key} fell back");
    }
    let wizard = window.page("language").unwrap().controller::<language_manager::ui::LanguageWizard>().expect("wizard");
    assert!(!wizard.step_label.is_visible());
    assert!(!wizard.footer.is_visible());
    assert!(wizard.on_wizard_changed.borrow().is_some());
    let title = gs_base::style::find_title(&window.page("language").unwrap().widget, 9).unwrap();
    assert!(title.has_css_class("gs-page-title"));
    window.show_step("layout");
    window.window.close();
    settle(0.2);
    assert!(window.closed.get());
}

fn main() {
    if std::env::var("GS_HEADLESS").as_deref() != Ok("1") {
        eprintln!("welcome_headless: run through tests/run_headless.sh (private compositor only); skipping");
        return;
    }
    suite_core::i18n::initialize();
    gtk::init().expect("gtk");
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let checks: &[(&str, fn())] = &[
        ("walks_every_step_and_finishes", walks_every_step_and_finishes),
        ("sidebar_jumps_and_reuses_pages", sidebar_jumps_and_reuses_pages),
        ("light_and_dark", light_and_dark),
        ("missing_color_settings_disable_the_choice", missing_color_settings_disable_the_choice),
        ("shortcut_overview_and_cheat_sheet", shortcut_overview_and_cheat_sheet),
        ("unavailable_page_links_to_settings", unavailable_page_links_to_settings),
        ("failed_installation_is_shown_and_setup_stays_pending", failed_installation_is_shown_and_setup_stays_pending),
        ("busy_page_keeps_the_window_open", busy_page_keeps_the_window_open),
        ("escape_and_alt_arrows", escape_and_alt_arrows),
        ("single_instance_routes_page", single_instance_routes_page),
        #[cfg(feature = "pages")]
        ("real_settings_pages_embed", real_settings_pages_embed),
    ];
    let mut failed = 0;
    let mut ran = 0;
    for (name, check) in checks {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        match std::panic::catch_unwind(*check) {
            Ok(()) => println!("ok   {name}"),
            Err(error) => {
                failed += 1;
                let message = error.downcast_ref::<String>().cloned()
                    .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                println!("FAIL {name}: {message}");
            }
        }
    }
    println!("{ran} checks, {failed} failed");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
