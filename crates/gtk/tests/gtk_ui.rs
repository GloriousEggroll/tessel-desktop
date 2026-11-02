//! Widget checks that need a display. Runs only inside a private headless
//! compositor (tests/run_headless.sh); on the live session it skips itself.

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_gtk::{app_menu, gtk, osd, palette, placement, popup, theme, utils};

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

fn parse_errors(css: &str) -> Vec<String> {
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = errors.clone();
    provider.connect_parsing_error(move |_, _, error| sink.borrow_mut().push(error.to_string()));
    provider.load_from_string(css);
    let result = errors.borrow().clone();
    result
}

fn labels(model: &gio::MenuModel) -> Vec<String> {
    let mut result = Vec::new();
    for index in 0..model.n_items() {
        if let Some(section) = model.item_link(index, "section") {
            result.extend(labels(&section));
        } else if let Some(label) = model.item_attribute_value(index, "label", None) {
            result.push(label.get::<String>().unwrap());
        }
    }
    result
}

fn widgets() {
    let header = utils::headerbar("Settings");
    let title = header.title_widget().and_downcast::<gtk::Label>().unwrap();
    assert_eq!(title.label(), "Settings");
    assert!(title.has_css_class("title"));
    let (first, second) = (gtk::ListBox::new(), gtk::ListBox::new());
    utils::sidebar_style(&first);
    utils::sidebar_style(&second);
    assert!(first.has_css_class(utils::SIDEBAR_CLASS));
    let display = first.display();
    assert!(utils::has_css(&display, "suite-sidebar").is_some());
    assert!(parse_errors(utils::SIDEBAR_CSS).is_empty());
    assert!(regex_free_px_fonts(utils::SIDEBAR_CSS));
    // pack(): end children stay after start children.
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let (a, b, c) = (gtk::Label::new(Some("a")), gtk::Label::new(Some("b")), gtk::Label::new(Some("c")));
    utils::pack(&row, &a, false, true, 0, false);
    utils::pack(&row, &c, false, true, 0, true);
    utils::pack(&row, &b, true, true, 0, false);
    let order: Vec<_> = utils::children(&row).iter().map(|w| w.downcast_ref::<gtk::Label>().unwrap().label().to_string()).collect();
    assert_eq!(order, ["a", "b", "c"]);
    utils::remove_children(&row);
    assert!(row.first_child().is_none());
    let image = utils::image("bluetooth-active-symbolic", 16);
    assert_eq!(image.pixel_size(), 16);
    println!("ok widgets");
}

// The suite uses pt font sizes everywhere (no px font sizes).
fn regex_free_px_fonts(css: &str) -> bool {
    !css.split("font-size:").skip(1).any(|rest| rest.trim_start().split(';').next().unwrap_or("").ends_with("px"))
}

fn generated_css_parses() {
    let entry = theme::MonitorPalette { name: "HEADLESS-1".into(), geometry: [0, 0, 1280, 720], background: [30, 15, 50],
                                        foreground: [238, 238, 238], accent: [105, 167, 255] };
    for dark in [true, false] {
        for transparency in [0, 50, 100] {
            for (frosted, rounded, colored, dialog) in [(false, true, false, false), (true, false, true, true)] {
                let css = theme::theme_css("tessel-dock-companion-9", &entry, dark, &theme::ThemeOptions {
                    transparency, frosted, rounded_corners: rounded, frost_strength: 50, colored_buttons: colored, dialog }).unwrap();
                assert_eq!(parse_errors(&css), Vec::<String>::new());
            }
        }
        let palette = palette::color_palette(dark, Some([190, 22, 66]));
        assert!(parse_errors(&palette::color_css(dark, &palette, true)).is_empty());
        assert!(parse_errors(&palette::background_css(dark, &palette::PanelStyle { border_sides: &["top", "left"], shine: true, ..Default::default() }).unwrap()).is_empty());
        assert!(parse_errors(&palette::popup_css("dock-1", dark, None, 20, false, false, 50, None).unwrap()).is_empty());
    }
    assert!(parse_errors(&theme::square_corners_css("tessel-dock-companion-9")).is_empty());
    for css in [popup::POPUP_LAYOUT_CSS, popup::POPUP_FLAT_CSS, osd::CSS] {
        assert!(parse_errors(css).is_empty());
        assert!(regex_free_px_fonts(css));
    }
    println!("ok css");
}

fn anchors() {
    let window = gtk::Window::new();
    window.init_layer_shell();
    window.set_exclusive_zone(-1);
    for edge in ["top", "bottom", "left", "right"] {
        for align in ["start", "center", "end"] {
            placement::anchor_popup(&window, edge, 53, align);
            let primary = placement::edge_from_name(edge);
            assert!(window.is_anchor(primary));
            assert_eq!(window.margin(primary), 53);
            let count = [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right].iter().filter(|e| window.is_anchor(**e)).count();
            assert_eq!(count, if align == "center" { 1 } else { 2 });
        }
    }
    assert_eq!(window.exclusive_zone(), -1);
    window.destroy();
    println!("ok anchors");
}

fn popup_controller() {
    let window = gtk::Window::builder().default_width(230).default_height(110).build();
    window.set_child(Some(&gtk::Button::with_label("Popup")));
    let applied = Rc::new(Cell::new(0));
    let counter = applied.clone();
    let controller = popup::PopupController::new(&window, move || counter.set(counter.get() + 1), "bottom", 76, "weather");
    assert!(window.has_css_class("tray-popup"));
    assert!(!controller.show_pending());
    controller.show();
    assert!(controller.show_pending(), "an asynchronous show is pending");
    controller.dismiss();
    assert!(!controller.show_pending());
    settle(1.3);
    assert!(!window.is_visible(), "dismiss cancels a pending probe and reply");
    assert!(controller.backdrops().iter().all(|b| !b.is_visible()));
    controller.show();
    settle(1.5);
    assert!(window.is_visible(), "popup shows without a dock");
    assert!(!controller.show_pending());
    assert_eq!(applied.get(), 1);
    assert!(controller.backdrops().iter().all(|b| b.is_visible()));
    assert_eq!(controller.backdrops().len() as u32, gdk::Display::default().unwrap().monitors().n_items());
    controller.activate();
    assert!(!window.is_visible(), "activate toggles a visible popup off");
    // A re-show right after a hide is not mistaken for a close by owners.
    controller.show();
    window.set_visible(false);
    assert!(controller.show_pending());
    controller.close();
    window.destroy();
    let header = popup::popup_header(&["bluetooth-active-symbolic"], "Bluetooth", None,
                                     vec![popup::HeaderAction::Button("emblem-system-symbolic".into(), "Settings".into(), None)]);
    assert!(!header.subtitle_label.is_visible());
    header.set_subtitle("2 connected");
    assert!(header.subtitle_label.is_visible());
    assert!(header.actions_box.is_visible());
    let primary = popup::footer_button("Connect", None, true, None);
    let footer = popup::popup_footer(&[Some(primary.clone().upcast()), None, Some(gtk::Button::with_label("x").upcast())]);
    assert!(primary.has_css_class("suggested-action") && !primary.has_css_class("flat"));
    assert_eq!(utils::children(&footer).len(), 2);
    assert!(utils::children(&footer)[1].has_css_class("flat"));
    let sized = gtk::Window::new();
    assert_eq!(popup::set_popup_width(&sized, 999), popup::POPUP_MAX_WIDTH);
    sized.destroy();
    println!("ok popup");
}

fn app_menu_checks(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::new(app);
    let header = gtk::HeaderBar::new();
    window.set_titlebar(Some(&header));
    let own = gio::Menu::new();
    own.append(Some("Settings"), Some("win.settings"));
    let shortcuts = vec![("Viewing".to_owned(), vec![app_menu::Shortcut::new("<Control>plus", "Zoom in"),
                                                     app_menu::Shortcut::new("Ctrl+Scroll", "Zoom")])];
    let details = app_menu::AboutDetails { name: "Pictures".into(), icon: "image-x-generic".into(), version: Some("1.0".into()), ..Default::default() };
    let button = app_menu::install_app_menu(app, &window, app_menu::MenuTarget::Model(own), details.clone(), shortcuts, Some(&header), None);
    assert!(app_menu::has_menu_icon(&button));
    assert!(button.child().and_downcast::<gtk::Image>().unwrap().gicon().unwrap().is::<gio::FileIcon>());
    assert!(button.has_css_class("flat") && button.is_primary());
    assert_eq!(button.ancestor(gtk::HeaderBar::static_type()).as_ref(), Some(header.upcast_ref::<gtk::Widget>()));
    assert_eq!(labels(&button.menu_model().unwrap()), ["Settings", "Keyboard Shortcuts", "About Pictures", "Quit"]);
    for name in ["shortcuts", "about", "quit"] {
        assert!(app.lookup_action(name).is_some());
    }
    assert_eq!(app.accels_for_action("app.quit"), ["<Control>q"]);
    assert_eq!(app.accels_for_action("app.shortcuts"), ["<Control>question"]);
    // A second window reuses the actions instead of duplicating them.
    let other = gtk::ApplicationWindow::new(app);
    let existing = gtk::MenuButton::new();
    let same = app_menu::install_app_menu(app, &other, app_menu::MenuTarget::Button(existing.clone()), details, vec![], None, None);
    assert_eq!(same, existing);
    assert_eq!(labels(&existing.menu_model().unwrap()), ["Keyboard Shortcuts", "About Pictures", "Quit"]);
    other.destroy();

    window.present();
    let dialog = app_menu::shortcuts_window(Some(window.upcast_ref()), &[("Viewing".into(), vec![
        app_menu::Shortcut::new("<Control>plus", "Zoom in"), app_menu::Shortcut::any(&["Left", "Page_Up"], "Previous picture"),
        app_menu::Shortcut::new("Ctrl+Scroll", "Zoom")])], None);
    settle(0.2);
    assert!(dialog.window.is_mapped());
    let texts: Vec<_> = dialog.rows.iter().map(|r| r.1.clone()).collect();
    assert!(texts.iter().any(|t| t.contains("zoom in") && t.contains("ctrl")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("ctrl+scroll")));
    assert!(texts.iter().any(|t| t.contains("quit")), "standard entries are always listed");
    dialog.search.set_text("previous");
    settle(0.4);
    assert_eq!(dialog.rows.iter().filter(|r| r.0.is_visible()).count(), 1);
    dialog.window.close();

    let about = app_menu::about_dialog(Some(window.upcast_ref()), &app_menu::AboutDetails {
        name: "Calculator".into(), icon: "accessories-calculator".into(), version: Some("2".into()), ..Default::default() });
    assert_eq!(about.program_name().unwrap(), "Calculator");
    assert_eq!(about.version().unwrap(), "2");
    assert!(about.license().is_none());
    about.destroy();
    let closed = Rc::new(Cell::new(0));
    let count = closed.clone();
    window.connect_close_request(move |_| {
        count.set(count.get() + 1);
        glib::Propagation::Stop
    });
    app.activate_action("quit", None);
    settle(0.1);
    assert_eq!(closed.get(), 1, "default quit closes windows through close-request");
    let called = Rc::new(Cell::new(false));
    let flag = called.clone();
    app_menu::install_app_menu(app, &window, app_menu::MenuTarget::New, app_menu::AboutDetails { name: "Capture".into(), ..Default::default() },
                               vec![], None, Some(Rc::new(move || flag.set(true))));
    app.activate_action("quit", None);
    assert!(called.get());
    assert_eq!(app_menu::accelerator_label("free text"), "free text");
    assert!(app_menu::accelerator_label("<Control>q").contains("Ctrl"));
    assert_ne!(gdk::Key::q.into_glib(), 0);
    window.destroy();
    println!("ok app_menu");
}

fn live_theme(cache: &std::path::Path) {
    let path = cache.join("tessel/dock/palette.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let display = gdk::Display::default().unwrap();
    let monitor = display.monitors().item(0).and_downcast::<gdk::Monitor>().unwrap();
    let g = monitor.geometry();
    let palette = serde_json::json!({"version": 1, "is_dark": true, "monitors": [
        {"name": monitor.connector().map(|c| c.to_string()).unwrap_or_default(), "geometry": [g.x(), g.y(), g.width(), g.height()],
         "background": [30, 15, 50], "foreground": [238, 238, 238], "accent": [105, 167, 255]}]});
    std::fs::write(&path, palette.to_string()).unwrap();
    let client = theme::PaletteClient::new(Some(path.clone()));
    let window = gtk::Window::new();
    window.set_child(Some(&gtk::Button::with_label("Themed")));
    let binding = theme::attach(&window, Some(&client));
    assert!(window.has_css_class(binding.scope()));
    assert!(theme::attach(&window, Some(&client)).scope() == binding.scope(), "attach is idempotent");
    window.present();
    settle(0.5);
    assert!(binding.monitor_name().is_some(), "window follows its monitor's palette");
    assert!(binding.provider().is_some());
    std::fs::remove_file(&path).unwrap();
    settle(0.5);
    client.refresh();
    assert!(binding.provider().is_none(), "palette removal restores the application's CSS");
    binding.close();
    assert!(!window.has_css_class(binding.scope()));
    window.destroy();
    println!("ok theme");
}

fn osd_checks() {
    let dismissed = Rc::new(Cell::new(false));
    let flag = dismissed.clone();
    let indicator = osd::AdjustmentOsd::new(Some(Box::new(move || flag.set(true))), 300);
    indicator.show(osd::Adjustment::Volume, Some(42.0), false, None, "Speakers");
    settle(0.2);
    assert!(indicator.window().unwrap().is_visible());
    settle(0.4);
    assert!(indicator.window().is_none() && dismissed.get(), "indicator closes itself after its duration");
    let resolved = Rc::new(Cell::new(false));
    let flag = resolved.clone();
    let indicator = osd::AdjustmentOsd::new(None, 3000);
    indicator.resolve_monitor(move |_| flag.set(true));
    settle(0.3);
    assert!(resolved.get());
    indicator.close();
    println!("ok osd");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var_os("SUITE_GTK_UI").is_none() {
        println!("gtk_ui: skipped (run crates/gtk/tests/run_headless.sh for the private compositor)");
        return;
    }
    let cache = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap()).join("cache");
    gtk::init().expect("display");
    widgets();
    generated_css_parses();
    anchors();
    popup_controller();
    let app = gtk::Application::new(Some("org.tessel.AppMenuTest"), gio::ApplicationFlags::NON_UNIQUE);
    app.register(None::<&gio::Cancellable>).unwrap();
    app_menu_checks(&app);
    live_theme(&cache);
    osd_checks();
    println!("gtk_ui: all checks passed");
}
