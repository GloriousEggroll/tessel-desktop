//! Private-compositor coverage of navigation, lifecycle, search and the
//! shared header (port of test_global_ui.py), plus byte/ordering parity of
//! the search index with Python. Run only through tests/run_headless.sh
//! (tests/headless.sh); outside it this binary refuses to run.
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use global_settings::gs_base::style::{self, footer, page_header, symbolic_icon, unify_header, align_page_gutter, HeaderIcon, PAGE_MARGIN};
use global_settings::gs_base::{tags, Host, SettingsPage};
use global_settings::search::SearchIndex;
use global_settings::window::{GlobalSettingsWindow, PageFactory, SECTIONS};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

fn test_app() -> gtk::Application {
    static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    gtk::Application::new(Some(&format!("org.waydock.TestSearch{n}")), gio::ApplicationFlags::NON_UNIQUE)
}

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

type Events = Rc<RefCell<Vec<(String, String)>>>;

struct Fixture {
    app: gtk::Application,
    window: GlobalSettingsWindow,
    events: Events,
    blocked: Rc<RefCell<HashSet<String>>>,
    controls: Rc<RefCell<std::collections::HashMap<String, gtk::Switch>>>,
}

fn count(events: &Events, kind: &str, section: &str) -> usize {
    events.borrow().iter().filter(|(k, s)| k == kind && s == section).count()
}

fn lifecycle(page: SettingsPage, events: &Events, blocked: &Rc<RefCell<HashSet<String>>>, section: &str) -> SettingsPage {
    let (e1, e2, e3, b) = (events.clone(), events.clone(), events.clone(), blocked.clone());
    let (s1, s2, s3, s4) = (section.to_owned(), section.to_owned(), section.to_owned(), section.to_owned());
    page.with_close(move || e1.borrow_mut().push(("close".into(), s1.clone())))
        .with_activate(move || e2.borrow_mut().push(("activate".into(), s2.clone())))
        .with_deactivate(move || e3.borrow_mut().push(("deactivate".into(), s3.clone())))
        .with_can_close(move || !b.borrow().contains(&s4))
}

fn fixture(search_pages: bool) -> Fixture {
    let app = test_app();
    app.register(gio::Cancellable::NONE).unwrap();
    let events: Events = Rc::default();
    let blocked: Rc<RefCell<HashSet<String>>> = Rc::default();
    let controls: Rc<RefCell<std::collections::HashMap<String, gtk::Switch>>> = Rc::default();
    let (e, b, c) = (events.clone(), blocked.clone(), controls.clone());
    let factory: PageFactory = Rc::new(move |section: &str, _host: &Host| {
        e.borrow_mut().push(("create".into(), section.into()));
        if !search_pages {
            return Ok(lifecycle(SettingsPage::new(gtk::Label::new(Some(section))), &e, &b, section));
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.append(&gtk::Label::new(Some(section)));
        content.append(&gtk::Box::builder().height_request(900).build());
        let label = gtk::Label::builder().label(if section == "notifications" { "Do Not Disturb" } else { "Location" })
            .xalign(0.0).build();
        let control = gtk::Switch::builder().halign(gtk::Align::Start).build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&label);
        row.append(&control);
        content.append(&row);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&content));
        let page = lifecycle(SettingsPage::new(scroll), &e, &b, section);
        page.add_target("dnd_switch", &control);
        page.add_target("location", &control);
        c.borrow_mut().insert(section.to_owned(), control);
        Ok(page)
    });
    let window = GlobalSettingsWindow::new(&app, Some(factory));
    window.show_section("audio").unwrap();
    window.window.present();
    settle(0.05);
    Fixture { app, window, events, blocked, controls }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.window.cleanup();
        self.window.window.destroy();
        self.app.quit();
        settle(0.02);
    }
}

fn search(f: &Fixture, text: &str) {
    f.window.search.grab_focus();
    f.window.set_search(text);
    settle(0.35);
}

fn rows(f: &Fixture) -> Vec<gtk::ListBoxRow> {
    let mut out = Vec::new();
    let mut child = f.window.search_results.first_child();
    while let Some(item) = child {
        child = item.next_sibling();
        out.push(item.downcast().unwrap());
    }
    out
}

fn row_setting(f: &Fixture, row: &gtk::ListBoxRow) -> (&'static str, &'static str) {
    let index: usize = tags::get_str(row, "setting").unwrap().parse().unwrap();
    let s = &f.window.search_index.items[index];
    (s.section, s.key)
}

fn sections_are_lazy_reused_and_cleaned_once() {
    let f = fixture(false);
    assert_eq!(*f.events.borrow(), vec![("create".to_string(), "audio".to_string()), ("activate".into(), "audio".into())]);
    assert_eq!(f.window.rows.borrow().len(), SECTIONS.len());
    for section in ["audio", "language", "audio"] {
        f.window.show_section(section).unwrap();
    }
    assert_eq!(count(&f.events, "create", "audio"), 1);
    assert_eq!(count(&f.events, "deactivate", "audio"), 1);
    assert_eq!(f.window.stack.visible_child_name().as_deref(), Some("audio"));
    f.window.window.close();
    assert!(f.window.closed.get());
    assert_eq!(count(&f.events, "close", "audio"), 1);
    assert_eq!(count(&f.events, "close", "language"), 1);
}

fn inflight_language_apply_prevents_close_and_returns_to_language() {
    let f = fixture(false);
    f.window.show_section("language").unwrap();
    f.blocked.borrow_mut().insert("language".into());
    f.window.show_section("audio").unwrap();
    f.window.window.close();
    assert!(!f.window.closed.get());
    assert_eq!(f.window.current_section().as_deref(), Some("language"));
    f.blocked.borrow_mut().clear();
    f.window.window.close();
    assert!(f.window.closed.get());
}

fn brightness_links_open_displays_without_a_duplicate_sidebar_page() {
    let f = fixture(false);
    assert!(!SECTIONS.iter().any(|s| s.0 == "brightness"));
    assert!(!f.window.rows.borrow().contains_key("brightness"));
    assert!(f.window.rows.borrow().contains_key("displays"));
    let args = global_settings::cli::parse(&["--section".into(), "brightness".into()]).unwrap();
    assert_eq!(args.section.as_deref(), Some("displays"));
    f.window.show_section("brightness").unwrap();
    assert_eq!(f.window.current_section().as_deref(), Some("displays"));
    assert_eq!(count(&f.events, "create", "displays"), 1);
    f.window.show_section("displays").unwrap();
    assert_eq!(count(&f.events, "create", "displays"), 1);
    assert!(f.window.show_section("nope").is_err());
}

fn failed_section_does_not_prevent_other_pages_and_can_retry() {
    let app = test_app();
    app.register(gio::Cancellable::NONE).unwrap();
    let fail = Rc::new(std::cell::Cell::new(true));
    let events: Events = Rc::default();
    let (fl, e) = (fail.clone(), events.clone());
    let factory: PageFactory = Rc::new(move |section: &str, _| {
        if fl.get() {
            return Err("Backend unavailable".into());
        }
        e.borrow_mut().push(("create".into(), section.into()));
        Ok(SettingsPage::new(gtk::Label::new(Some(section))))
    });
    let window = GlobalSettingsWindow::new(&app, Some(factory));
    window.show_section("network").unwrap();
    assert_eq!(window.current_section().as_deref(), Some("network"));
    let page = window.page("network").unwrap();
    let state = page.error_state.borrow().clone().unwrap();
    assert!(state.has_css_class("settings-error"));
    fail.set(false);
    // error_state is the empty-state box; its last child is the button.
    let button = state.last_child().unwrap().downcast::<gtk::Button>().unwrap();
    button.emit_clicked();
    assert_eq!(count(&events, "create", "network"), 1);
    window.show_section("audio").unwrap();
    assert_eq!(window.current_section().as_deref(), Some("audio"));
    window.cleanup();
    window.window.destroy();
}

fn search_is_lazy_and_restores_page_lifecycle() {
    let f = fixture(true);
    assert_eq!(f.events.borrow().len(), 2);
    search(&f, "maximum charge");
    assert_eq!(row_setting(&f, &rows(&f)[0]), ("battery", "charge"));
    search(&f, "temperature");
    assert!(rows(&f).iter().any(|r| row_setting(&f, r).0 == "weather"));
    assert_eq!(f.window.stack.visible_child_name().as_deref(), Some("__search__"));
    assert_eq!(f.events.borrow().iter().filter(|e| e.0 == "create").count(), 1);
    assert_eq!(count(&f.events, "deactivate", "audio"), 1);
    search(&f, "");
    assert_eq!(f.window.stack.visible_child_name().as_deref(), Some("audio"));
    assert_eq!(count(&f.events, "activate", "audio"), 2);
}

fn activating_match_opens_and_scrolls_to_control_without_changing_it() {
    let f = fixture(true);
    search(&f, "Do Not Disturb");
    assert_eq!(f.window.search_summary.text().as_str(), format!("{} result{}", rows(&f).len(), if rows(&f).len() == 1 { "" } else { "s" }));
    f.window.search.emit_by_name::<()>("activate", &[]);
    settle(0.3);
    assert_eq!(f.window.current_section().as_deref(), Some("notifications"));
    assert_eq!(f.window.search.text(), "");
    let control = f.controls.borrow()["notifications"].clone();
    assert!(!control.is_active(), "Finding a setting must not change its value");
    assert_eq!(gtk::prelude::GtkWindowExt::focus(&f.window.window).as_ref(), Some(control.upcast_ref::<gtk::Widget>()));
    assert!(control.has_css_class("settings-search-target"));
    let scroll = f.window.page("notifications").unwrap().widget.clone().downcast::<gtk::ScrolledWindow>().unwrap();
    assert!(scroll.vadjustment().value() > 100.0, "{}", scroll.vadjustment().value());
    assert_eq!(count(&f.events, "create", "notifications"), 1);
    search(&f, "Do Not Disturb");
    let first = rows(&f)[0].clone();
    f.window.search_results.emit_by_name::<()>("row-activated", &[&first]);
    settle(0.15);
    assert_eq!(count(&f.events, "create", "notifications"), 1);
}

fn weather_uses_explicit_search_target() {
    let f = fixture(true);
    search(&f, "weather location");
    let row = rows(&f).into_iter().find(|r| row_setting(&f, r).1 == "location").unwrap();
    f.window.search_results.emit_by_name::<()>("row-activated", &[&row]);
    settle(0.2);
    assert_eq!(f.window.current_section().as_deref(), Some("weather"));
    assert!(f.controls.borrow()["weather"].has_css_class("settings-search-target"));
}

fn no_match_and_escape_do_not_close_window_or_launch_a_page() {
    let f = fixture(true);
    search(&f, "fixture-no-such-setting-zxy");
    assert!(rows(&f).is_empty());
    assert!(f.window.search_empty.is_visible());
    f.window.search.emit_by_name::<()>("activate", &[]);
    assert_eq!(f.window.current_section().as_deref(), Some("audio"));
    assert!(f.window.key(gdk::Key::Escape, gdk::ModifierType::empty()));
    assert!(!f.window.closed.get());
    assert_eq!(f.window.stack.visible_child_name().as_deref(), Some("audio"));
    assert_eq!(f.window.search.text(), "");
}

fn pages_share_one_header_and_sidebar_icons_are_symbolic() {
    let f = fixture(true);
    for row in f.window.rows.borrow().values() {
        let picture = row.child().unwrap().first_child().unwrap().downcast::<gtk::Image>().unwrap();
        let gicon = picture.gicon().unwrap();
        let names: Vec<String> = gicon.downcast_ref::<gio::ThemedIcon>().unwrap().names().iter().map(|n| n.to_string()).collect();
        let symbolic: Vec<&String> = names.iter().filter(|n| n.ends_with("-symbolic")).collect();
        assert!(!symbolic.is_empty(), "{names:?}");
        assert_eq!(names[..symbolic.len()].iter().collect::<Vec<_>>(), symbolic);
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tile.add_css_class("settings-tile");
    tile.append(&gtk::Image::from_icon_name("view-reveal-symbolic"));
    heading.append(&tile);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let title = gtk::Label::new(Some("Keep Awake"));
    title.add_css_class("title");
    let subtitle = gtk::Label::new(Some("Detail"));
    subtitle.add_css_class("subtitle");
    subtitle.add_css_class("muted");
    titles.append(&title);
    titles.append(&subtitle);
    heading.append(&titles);
    body.append(&heading);
    assert_eq!(unify_header(&body, None).as_ref(), Some(heading.upcast_ref::<gtk::Widget>()));
    assert_eq!(title.css_classes().iter().map(|c| c.to_string()).collect::<Vec<_>>(), vec!["gs-page-title"]);
    assert!(subtitle.has_css_class("gs-page-subtitle"));
    assert!(tile.has_css_class("gs-page-icon"));
    assert!(!tile.has_css_class("settings-tile"));
    assert_eq!(tile.first_child().unwrap().downcast::<gtk::Image>().unwrap().pixel_size(), 24);
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (text, css) in [("Notifications", "notification-heading"), ("Lead", "muted")] {
        let l = gtk::Label::new(Some(text));
        l.add_css_class(css);
        page.append(&l);
    }
    page.append(&gtk::Switch::new());
    let later = gtk::Label::new(Some("Applications"));
    later.add_css_class("muted");
    page.append(&later);
    let header = unify_header(&page, Some(HeaderIcon::Name("preferences-system-notifications-symbolic"))).unwrap();
    assert_eq!(header.parent().as_ref(), Some(page.upcast_ref::<gtk::Widget>()));
    assert_eq!(page.first_child().as_ref(), Some(&header));
    assert!(header.first_child().unwrap().is::<gtk::Image>());
    assert!(!later.has_css_class("gs-page-subtitle"));
    let status = gtk::Label::new(None);
    let connected = Rc::new(std::cell::Cell::new(false));
    let c = connected.clone();
    footer(&status, Some(Rc::new(move || c.get())));
    status.add_css_class("error");
    assert!(!status.has_css_class("error"), "not running is not shown as an error");
    connected.set(true);
    status.add_css_class("error");
    assert!(status.has_css_class("error"));
    assert!(status.has_css_class("gs-footer"));
    let shared = page_header("Title", Some("Subtitle"), Some("help-about-symbolic"));
    assert_eq!(shared.title.css_classes().iter().map(|c| c.to_string()).collect::<Vec<_>>(), vec!["gs-page-title"]);
    let breeze = std::path::Path::new("/usr/share/icons/breeze/actions/16/help-about-symbolic.svg");
    if breeze.is_file() {
        let ring = gtk::Image::from_gicon(&gio::FileIcon::new(&gio::File::for_path(breeze)));
        ring.set_pixel_size(24);
        style::symbolic_image(&ring); // re-resolves through symbolic names; the optical class follows the file
    }
    assert!(!symbolic_icon("help-about-symbolic", 16).has_css_class(style::OPTICAL_SCALE_CLASS));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_start(20).margin_end(20).build();
    root.append(&content);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    let tile = gtk::Image::from_icon_name("battery-good-symbolic");
    heading.append(&tile);
    let l = gtk::Label::new(Some("Battery"));
    l.add_css_class("title");
    heading.append(&l);
    content.append(&heading);
    let header = unify_header(&root, None);
    assert_eq!(align_page_gutter(&root, header.as_ref()), PAGE_MARGIN - 20);
    assert_eq!((content.margin_start(), content.margin_end()), (PAGE_MARGIN, PAGE_MARGIN));
    assert_eq!(align_page_gutter(&root, header.as_ref()), 0);
    tile.set_pixel_size(32);
    assert_eq!(tile.pixel_size(), 24);
    assert_eq!(shared.subtitle.unwrap().css_classes().iter().map(|c| c.to_string()).collect::<Vec<_>>(), vec!["gs-page-subtitle"]);
}

fn async_page_errors_use_the_shared_error_state() {
    let f = fixture(false);
    f.window.show_section("displays").unwrap();
    settle(0.05);
    let page = f.window.show_page_error("displays", "way-displays is not running.", Some("Displays are unavailable"),
                                        "dialog-warning-symbolic").unwrap();
    assert!(Rc::ptr_eq(&f.window.page("displays").unwrap(), &page));
    let state = page.error_state.borrow().clone().unwrap();
    assert!(state.has_css_class("gs-empty-state"));
    assert!(page.widget.first_child().unwrap().has_css_class("gs-page-header"));
    let title = state.first_child().unwrap().next_sibling().unwrap().downcast::<gtk::Label>().unwrap();
    assert_eq!(title.text(), "Displays are unavailable");
    assert!(f.events.borrow().contains(&("close".into(), "displays".into())));
    assert_eq!(f.window.stack.visible_child_name().as_deref(), Some("displays"));
    state.last_child().unwrap().downcast::<gtk::Button>().unwrap().emit_clicked();
    assert_eq!(count(&f.events, "create", "displays"), 2);
}

fn search_preserves_busy_page_close_guard() {
    let f = fixture(true);
    f.window.show_section("language").unwrap();
    f.blocked.borrow_mut().insert("language".into());
    search(&f, "volume");
    f.window.window.close();
    assert!(!f.window.closed.get());
    assert_eq!(f.window.current_section().as_deref(), Some("language"));
    assert_eq!(f.window.stack.visible_child_name().as_deref(), Some("language"));
    assert_eq!(f.window.search.text(), "");
    f.blocked.borrow_mut().clear();
    f.window.window.close();
    assert!(f.window.closed.get());
    assert_eq!(count(&f.events, "close", "language"), 1);
}

fn audio_result_reveals_nested_audio_page() {
    let app = test_app();
    app.register(gio::Cancellable::NONE).unwrap();
    let navigation = gtk::ListBox::new();
    let stack = gtk::Stack::new();
    for (key, title) in [("playback", "Playback"), ("inputs", "Input Devices")] {
        let row = gtk::ListBoxRow::new();
        tags::set_str(&row, tags::PAGE_NAME, key);
        row.set_child(Some(&gtk::Label::new(Some(title))));
        navigation.append(&row);
        stack.add_named(&gtk::Label::new(Some(title)), Some(key));
    }
    let s = stack.clone();
    navigation.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            s.set_visible_child_name(&tags::get_str(row, tags::PAGE_NAME).unwrap());
        }
    });
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.append(&navigation);
    root.append(&stack);
    let r = root.clone();
    let window = GlobalSettingsWindow::new(&app, Some(Rc::new(move |_: &str, _: &Host| Ok(SettingsPage::new(r.clone())))));
    window.show_section("audio").unwrap();
    navigation.select_row(navigation.row_at_index(0).as_ref());
    window.window.present();
    settle(0.05);
    window.search.grab_focus();
    window.set_search("Input Devices");
    settle(0.35);
    window.search.emit_by_name::<()>("activate", &[]);
    settle(0.2);
    assert_eq!(window.current_section().as_deref(), Some("audio"));
    assert_eq!(stack.visible_child_name().as_deref(), Some("inputs"));
    assert_eq!(tags::get_str(&navigation.selected_row().unwrap(), tags::PAGE_NAME).as_deref(), Some("inputs"));
    window.cleanup();
    window.window.destroy();
}

/// Python's SearchIndex on the same queries (driver run inside this private
/// session) must give the same ordered (section, key) lists.
/// Search entries added after the port (no Python counterpart).
const RUST_ONLY: &[(&str, &str)] = &[("theme", "icons")];
/// Sections the Python suite does not have at all (Archive Manager replaces KDE's Ark; Clipboard, Klipper).
const RUST_ONLY_SECTIONS: &[&str] = &["archive", "clipboard", "passwords"];

fn search_ranking_matches_python() {
    let Ok(btmanager) = std::env::var("BTMANAGER") else { panic!("BTMANAGER unset") };
    let queries = ["volume", "display", "lock", "wifi", "Wi-Fi", "24 hour", "dark", "  maximum   Charge ", "é", "ü ber",
                   "blur", "keyboard layout", "a", "screen", "power sleep", "notifications", "dock", "time", "x", "über",
                   "panel", "transparency", "mouse speed", "theme", "zzz", "temperature", "night", "users password"];
    let driver = format!("import sys, json\nsys.path.insert(0, {:?})\nfrom settings_search import SearchIndex, normalize\nimport ast\nsrc=open({:?}).read()\ntree=ast.parse(src)\nsections=next(ast.literal_eval(n.value) for n in tree.body if isinstance(n, ast.Assign) and any(getattr(t,'id',None)=='SECTIONS' for t in n.targets))\nindex=SearchIndex(sections)\nq=json.loads(sys.argv[1])\nprint(json.dumps({{'results': [[[i.section, i.key] for i in index.search(x)] for x in q], 'norm': [normalize(x) for x in q]}}))\n",
        format!("{btmanager}/global-settings"), format!("{btmanager}/global-settings/global_ui.py"));
    let out = std::process::Command::new("python3").args(["-I", "-B", "-c", &driver, &serde_json::to_string(&queries).unwrap()])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let index = SearchIndex::new(SECTIONS, &|t: &str| t.to_owned());
    for (i, query) in queries.iter().enumerate() {
        // Settings the Python suite does not have are left out of the comparison.
        let rust: Vec<serde_json::Value> = index.search(query).iter()
            .filter(|s| !RUST_ONLY.contains(&(s.section, s.key)) && !RUST_ONLY_SECTIONS.contains(&s.section))
            .map(|s| serde_json::json!([s.section, s.key])).collect();
        assert_eq!(serde_json::Value::Array(rust), value["results"][i], "query {query:?}");
        assert_eq!(global_settings::search::normalize(query), value["norm"][i].as_str().unwrap(), "{query:?}");
    }
}

/// SingletonTests: a second launch is forwarded to this primary (private bus)
/// and routes its --section into the existing window; --quit is refused
/// while a page is busy; --shortcuts toggles the overview.
fn repeated_launch_focuses_and_routes_section_in_existing_window() {
    use global_settings::app::{GlobalSettingsApplication, WindowFactory, APPLICATION_ID};
    let blocked = Rc::new(std::cell::Cell::new(false));
    let b = blocked.clone();
    let factory: WindowFactory = Rc::new(move |app: &gtk::Application| {
        let b = b.clone();
        GlobalSettingsWindow::new(app, Some(Rc::new(move |key: &str, _: &Host| {
            let b = b.clone();
            let page = SettingsPage::new(gtk::Label::new(Some(key)));
            if key == "language" {
                page.set_can_close(move || !b.get());
            }
            Ok(page)
        })))
    });
    let app = GlobalSettingsApplication::new(APPLICATION_ID, Some(factory), None);
    app.app.register(gio::Cancellable::NONE).unwrap();
    let hold = app.app.hold();
    app.show(Some("language"));
    settle(0.05);
    let window = app.window.borrow().clone().unwrap();
    let binary = env!("CARGO_BIN_EXE_global-settings");
    let run = |args: &[&str]| -> (i32, String, String) {
        let mut child = std::process::Command::new(binary).args(args)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        for _ in 0..200 {
            settle(0.03);
            if child.try_wait().unwrap().is_some() {
                break;
            }
        }
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
    };
    for (args, section) in [(&[][..], "language"), (&["--section", "audio"][..], "audio"), (&["--sec", "brightness"][..], "displays")] {
        let (code, _out, err) = run(args);
        assert_eq!(code, 0, "{err}");
        assert!(Rc::ptr_eq(&app.window.borrow().clone().unwrap().0, &window.0));
        assert_eq!(window.current_section().as_deref(), Some(section));
    }
    let (code, _, err) = run(&["--section", "bogus"]);
    assert_eq!(code, 2);
    assert!(err.contains("invalid choice: 'bogus'"), "{err}");
    let (code, _, _) = run(&["--shortcuts"]);
    assert_eq!(code, 0);
    assert!(app.cheat_sheet.borrow().as_ref().is_some_and(|s| !s.closed()));
    let (code, _, _) = run(&["--shortcuts"]);
    assert_eq!(code, 0);
    settle(0.05);
    assert!(app.cheat_sheet.borrow().as_ref().is_some_and(|s| s.closed()));
    window.show_section("language").unwrap();
    blocked.set(true);
    let (code, _, err) = run(&["--quit"]);
    assert_eq!((code, err.as_str()), (1, "A settings operation is still in progress.\n"));
    assert!(!window.closed.get());
    blocked.set(false);
    let (code, _, _) = run(&["--quit"]);
    assert_eq!(code, 0);
    assert!(window.closed.get());
    drop(hold);
    settle(0.05);
}

/// CombinedPagesTests with the real page crates: every section builds in one
/// window (private bus, fake HOME, no daemons), gets the shared header, and
/// closing releases everything. Error pages are allowed only where a
/// resident service is required and absent.
#[cfg(feature = "pages")]
fn every_real_section_builds_in_one_window() {
    let app = test_app();
    app.register(gio::Cancellable::NONE).unwrap();
    let window = GlobalSettingsWindow::new(&app, None);
    window.window.present();
    let mut errors = Vec::new();
    for (key, ..) in SECTIONS {
        window.show_section(key).unwrap();
        settle(0.25);
        let page = window.page(key).unwrap();
        assert_eq!(page.widget.root().as_ref(), Some(window.window.upcast_ref::<gtk::Root>()), "{key}");
        if page.error_state.borrow().is_some() {
            errors.push(key.to_string());
        }
        let title = style::find_title(&page.widget, 9).unwrap_or_else(|| panic!("{key}: no title"));
        let mut header = title.parent();
        while let Some(h) = header.clone() {
            if h.has_css_class("gs-page-header") { break; }
            header = h.parent();
        }
        assert!(header.is_some(), "{key}: no shared header");
    }
    println!("     error pages: {errors:?}");
    window.window.close();
    settle(0.3);
    assert!(window.closed.get());
}

fn main() {
    if std::env::var("GS_HEADLESS").as_deref() != Ok("1") {
        eprintln!("shell_headless: run through tests/run_headless.sh (private compositor only); skipping");
        return;
    }
    suite_core::i18n::initialize();
    gtk::init().expect("gtk");
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let checks: &[(&str, fn())] = &[
        ("sections_are_lazy_reused_and_cleaned_once", sections_are_lazy_reused_and_cleaned_once),
        ("inflight_language_apply_prevents_close_and_returns_to_language", inflight_language_apply_prevents_close_and_returns_to_language),
        ("brightness_links_open_displays_without_a_duplicate_sidebar_page", brightness_links_open_displays_without_a_duplicate_sidebar_page),
        ("failed_section_does_not_prevent_other_pages_and_can_retry", failed_section_does_not_prevent_other_pages_and_can_retry),
        ("search_is_lazy_and_restores_page_lifecycle", search_is_lazy_and_restores_page_lifecycle),
        ("activating_match_opens_and_scrolls_to_control_without_changing_it", activating_match_opens_and_scrolls_to_control_without_changing_it),
        ("weather_uses_explicit_search_target", weather_uses_explicit_search_target),
        ("no_match_and_escape_do_not_close_window_or_launch_a_page", no_match_and_escape_do_not_close_window_or_launch_a_page),
        ("pages_share_one_header_and_sidebar_icons_are_symbolic", pages_share_one_header_and_sidebar_icons_are_symbolic),
        ("async_page_errors_use_the_shared_error_state", async_page_errors_use_the_shared_error_state),
        ("search_preserves_busy_page_close_guard", search_preserves_busy_page_close_guard),
        ("audio_result_reveals_nested_audio_page", audio_result_reveals_nested_audio_page),
        ("search_ranking_matches_python", search_ranking_matches_python),
        ("repeated_launch_focuses_and_routes_section_in_existing_window", repeated_launch_focuses_and_routes_section_in_existing_window),
        #[cfg(feature = "pages")]
        ("every_real_section_builds_in_one_window", every_real_section_builds_in_one_window),
    ];
    let mut failed = 0;
    for (name, check) in checks {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
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
    println!("{} checks, {failed} failed", checks.len());
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
