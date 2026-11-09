//! Port of test_tessel_shell_theme.py `ThemeUITests`. Runs only inside the private
//! nested headless labwc (tests/run_headless.sh, two outputs); under a plain
//! `cargo test` it skips itself.
#![allow(deprecated)]

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tessel_theme::{palette_css, set_window_monitor, Colors, CssOptions, TesselShellTheme};

fn snapshot() -> Value {
    json!({
        "version": 1, "is_dark": true, "transparency": 20,
        "monitors": [
            {"name": "HEADLESS-1", "geometry": [0, 0, 1280, 720],
             "background": [18, 25, 35], "foreground": [222, 228, 240], "accent": [105, 167, 255]},
            {"name": "HEADLESS-2", "geometry": [1280, 0, 1280, 720],
             "background": [35, 20, 25], "foreground": [235, 225, 220], "accent": [200, 100, 150]},
        ],
    })
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

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("theme-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, value: &Value) {
    std::fs::write(path, value.to_string()).unwrap();
}

/// Atomic replacement, as Tessel Dock writes the cache.
fn replace(path: &Path, value: &Value) {
    let temporary = path.with_extension("new");
    write(&temporary, value);
    std::fs::rename(&temporary, path).unwrap();
}

fn color(widget: &impl IsA<gtk::Widget>) -> (i64, i64, i64) {
    let c = widget.as_ref().color();
    let f = |v: f32| (v as f64 * 255.0).round_ties_even() as i64;
    (f(c.red()), f(c.green()), f(c.blue()))
}

fn render(widget: &impl IsA<gtk::Widget>, frame: bool) -> (Vec<u8>, usize) {
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 100, 100).unwrap();
    {
        let context = cairo::Context::new(&surface).unwrap();
        let style = widget.as_ref().style_context();
        if frame {
            gtk::render_frame(&style, &context, 0.0, 0.0, 100.0, 100.0);
        } else {
            gtk::render_background(&style, &context, 0.0, 0.0, 100.0, 100.0);
        }
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().unwrap().to_vec();
    (data, stride)
}

fn alpha(widget: &impl IsA<gtk::Widget>) -> i64 {
    let (data, stride) = render(widget, false);
    data[50 * stride + 203] as i64
}

fn unpremultiply(pixel: &[u8]) -> (i64, i64, i64) {
    let (b, g, r, a) = (pixel[0] as f64, pixel[1] as f64, pixel[2] as f64, pixel[3] as f64);
    if a == 0.0 {
        return (0, 0, 0);
    }
    let f = |v: f64| (v * 255.0 / a).round_ties_even() as i64;
    (f(r), f(g), f(b))
}

fn background_color(widget: &impl IsA<gtk::Widget>) -> (i64, i64, i64) {
    let (data, stride) = render(widget, false);
    unpremultiply(&data[50 * stride + 200..50 * stride + 204])
}

fn border_color(widget: &impl IsA<gtk::Widget>) -> (i64, i64, i64) {
    let (data, stride) = render(widget, true);
    unpremultiply(&data[50 * stride..50 * stride + 4])
}

fn corner(widget: &impl IsA<gtk::Widget>) -> i64 {
    render(widget, false).0[3] as i64
}

fn rgb(value: &Value) -> (i64, i64, i64) {
    (value[0].as_i64().unwrap(), value[1].as_i64().unwrap(), value[2].as_i64().unwrap())
}

fn monitor(name: &str) -> gdk::Monitor {
    let monitors = gdk::Display::default().unwrap().monitors();
    let found = monitors.iter::<glib::Object>().flatten().filter_map(|o| o.downcast::<gdk::Monitor>().ok()).find(|m| m.connector().as_deref() == Some(name));
    found.unwrap_or_else(|| panic!("no monitor {name}"))
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(current) = child {
        out.extend(descendants(&current));
        child = current.next_sibling();
    }
    out
}

fn test_live_wallpaper_accent_colors_controls_and_chooses_readable_text() {
    let dir = scratch("accent");
    let path = dir.join("palette.json");
    let mut data = snapshot();
    write(&path, &data);
    let window = gtk::Window::new();
    set_window_monitor(&window, Some(&monitor("HEADLESS-1")));
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.set_child(Some(&layout));
    let button = gtk::Button::with_label("Apply wallpaper");
    button.add_css_class("suggested-action");
    let switch = gtk::Switch::builder().active(true).build();
    let progress = gtk::ProgressBar::builder().fraction(0.5).build();
    layout.append(&button);
    layout.append(&switch);
    layout.append(&progress);
    window.present();
    settle(0.06);
    let popup = gtk::Popover::new();
    let confirm = gtk::Button::with_label("Confirm");
    confirm.add_css_class("suggested-action");
    popup.set_child(Some(&confirm));
    popup.set_parent(&button);
    popup.popup();
    settle(0.06);
    let theme = TesselShellTheme::new(Some(path.clone()));
    let filled = progress.first_child().unwrap().first_child().unwrap();
    assert_eq!(filled.css_name(), "progress");
    for (accent, foreground) in [(json!([220, 80, 95]), (16, 17, 20)), (json!([30, 100, 45]), (255, 255, 255))] {
        data["monitors"][0]["accent"] = accent.clone();
        replace(&path, &data);
        settle(0.5);
        for widget in [button.clone().upcast::<gtk::Widget>(), switch.clone().upcast(), filled.clone(), confirm.clone().upcast()] {
            assert_eq!(background_color(&widget), rgb(&accent), "{}", widget.css_name());
        }
        assert_eq!(color(&button.child().unwrap()), foreground);
        assert_eq!(color(&confirm.child().unwrap()), foreground);
    }
    theme.close();
    popup.unparent();
    window.destroy();
    settle(0.06);
    let _ = std::fs::remove_dir_all(dir);
}

fn test_live_updates_scope_desktop_and_remove_destroyed_windows() {
    let dir = scratch("scope");
    let path = dir.join("palette.json");
    write(&path, &snapshot());
    let desktop = gtk::Window::new();
    desktop.add_css_class("tessel-shell-desktop");
    let desktop_label = gtk::Label::new(Some("Desktop icon"));
    desktop.set_child(Some(&desktop_label));
    let settings = gtk::Window::new();
    set_window_monitor(&settings, Some(&monitor("HEADLESS-1")));
    let settings_label = gtk::Label::new(Some("Wallpaper settings"));
    settings.set_child(Some(&settings_label));
    desktop.present();
    settings.present();
    settle(0.06);
    let untouched_color = color(&desktop_label);
    let untouched_alpha = alpha(&desktop);
    let theme = TesselShellTheme::new(Some(path.clone()));
    settle(0.06);
    assert_eq!(color(&desktop_label), untouched_color);
    assert_eq!(color(&settings_label), rgb(&snapshot()["monitors"][0]["foreground"]));
    assert_eq!(theme.themed_windows().len(), 2);
    let provider = theme.window_provider(&settings).unwrap().1;
    let mut changed = snapshot();
    changed["monitors"][0]["foreground"] = json!([90, 130, 170]);
    replace(&path, &changed);
    settle(0.5);
    assert_eq!(theme.window_provider(&settings).unwrap().1, provider);
    assert_eq!(color(&settings_label), (90, 130, 170));
    assert_eq!(color(&desktop_label), untouched_color);
    for (enabled, amount, expected) in [(true, 50, 128), (true, 100, 0), (true, 0, 255), (false, 100, 255)] {
        changed["apps_transparency_enabled"] = json!(enabled);
        changed["apps_transparency"] = json!(amount);
        changed["transparency"] = json!(100 - amount);
        write(&path, &changed);
        theme.refresh();
        settle(0.06);
        assert!((alpha(&settings) - expected).abs() <= 1, "alpha {} vs {expected}", alpha(&settings));
        assert_eq!(color(&settings_label), (90, 130, 170));
        assert_eq!(alpha(&desktop), untouched_alpha);
    }
    set_window_monitor(&settings, Some(&monitor("HEADLESS-2")));
    theme.refresh();
    settle(0.06);
    assert_eq!(color(&settings_label), rgb(&snapshot()["monitors"][1]["foreground"]));
    std::fs::write(&path, "bad cache").unwrap();
    theme.refresh();
    settle(0.06);
    assert!(theme.themed_windows().is_empty());
    assert_eq!(color(&settings_label), untouched_color);
    write(&path, &snapshot());
    theme.refresh();
    settings.destroy();
    settle(0.06);
    assert!(!theme.themed_windows().iter().any(|w| *w == settings));
    theme.close();
    desktop.destroy();
    settle(0.06);
    let _ = std::fs::remove_dir_all(dir);
}

fn test_square_setting_flattens_controls_and_restores_local_styles() {
    gtk::Settings::default().unwrap().set_gtk_enable_animations(false);
    let dir = scratch("square");
    let path = dir.join("palette.json");
    let mut data = snapshot();
    write(&path, &data);
    let window = gtk::Window::new();
    let header = gtk::HeaderBar::new();
    window.set_titlebar(Some(&header));
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let button = gtk::Button::with_label("Choose wallpaper");
    layout.append(&button);
    let switch = gtk::Switch::builder().active(true).build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_value(50.0);
    layout.append(&switch);
    layout.append(&scale);
    window.set_child(Some(&layout));
    let popup = gtk::Popover::new();
    let confirm = gtk::Button::with_label("Remove wallpaper");
    popup.set_child(Some(&confirm));
    popup.set_parent(&button);
    window.present();
    popup.popup();
    settle(0.06);
    let title_button = descendants(header.upcast_ref()).into_iter().find(|w| w.is::<gtk::Button>()).unwrap();
    let widgets = [button.clone().upcast::<gtk::Widget>(), confirm.clone().upcast(), title_button.clone()];
    let mut round_widgets = vec![switch.clone().upcast::<gtk::Widget>(),
                                 descendants(switch.upcast_ref()).into_iter().find(|w| w.css_name() == "slider").unwrap()];
    round_widgets.extend(descendants(scale.upcast_ref()).into_iter().filter(|w| ["trough", "highlight", "slider"].contains(&w.css_name().as_str())));
    let local = gtk::CssProvider::new();
    local.load_from_string(".corner-probe { background: #2468ac; border-radius: 18px; }button.corner-probe:hover { border-radius: 24px; }");
    let display = WidgetExt::display(&window);
    gtk::style_context_add_provider_for_display(&display, &local, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);
    for widget in widgets.iter().chain(&round_widgets) {
        widget.add_css_class("corner-probe");
    }
    let theme = TesselShellTheme::new(Some(path.clone()));
    for enabled in [true, false, true, false] {
        data["apps_rounded_corners"] = json!(enabled);
        write(&path, &data);
        theme.refresh();
        button.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        title_button.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        settle(0.06);
        for widget in &round_widgets {
            assert!(corner(widget) < alpha(widget), "{} rounded={enabled}", widget.css_name());
        }
        for widget in &widgets {
            if enabled {
                assert!(corner(widget) < alpha(widget), "{} rounded", widget.css_name());
            } else {
                assert_eq!(corner(widget), alpha(widget), "{} square", widget.css_name());
            }
        }
    }
    theme.close();
    gtk::style_context_remove_provider_for_display(&display, &local);
    popup.unparent();
    window.destroy();
    settle(0.06);
    let _ = std::fs::remove_dir_all(dir);
}

fn test_desktop_captions_selection_and_menu_controls_follow_each_monitor() {
    // A theme can draw its own hover border on menu rows (one once leaked blue
    // through the palette). Stand in for it, whichever themes are installed.
    let native = gtk::CssProvider::new();
    native.load_from_string("popover modelbutton:hover { border: 1px solid rgb(0, 0, 255); }");
    gtk::style_context_add_provider_for_display(&gdk::Display::default().unwrap(), &native, gtk::STYLE_PROVIDER_PRIORITY_THEME);
    let dir = scratch("captions");
    let path = dir.join("palette.json");
    let mut data = snapshot();
    data["monitors"][0]["accent"] = json!([220, 80, 95]);
    data["monitors"][1]["accent"] = json!([30, 100, 45]);
    write(&path, &data);
    let mut windows = Vec::new();
    let mut popovers = Vec::new();
    let mut fixtures = Vec::new();
    for colors in data["monitors"].as_array().unwrap().clone() {
        let desktop = gtk::Window::new();
        desktop.add_css_class("tessel-shell-desktop");
        set_window_monitor(&desktop, Some(&monitor(colors["name"].as_str().unwrap())));
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        desktop.set_child(Some(&layout));
        let mut captions = Vec::new();
        for selected in [false, true] {
            let caption = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            caption.add_css_class("label-highlight");
            if selected {
                caption.add_css_class("selected");
            }
            let label = gtk::Label::new(Some("Desktop file"));
            caption.append(&label);
            layout.append(&caption);
            captions.push((caption, label));
        }
        let selection = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        selection.add_css_class("selection-box");
        layout.append(&selection);
        let entry = gtk::Entry::builder().text("Rename file").build();
        layout.append(&entry);
        let button = gtk::MenuButton::builder().label("Desktop menu").build();
        layout.append(&button);
        let menu = gio::Menu::new();
        menu.append(Some("Hide Icons"), Some("test.hide"));
        let action = gio::SimpleAction::new_stateful("hide", None, &true.to_variant());
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&action);
        desktop.insert_action_group("test", Some(&actions));
        let popup = gtk::PopoverMenu::from_model(Some(&menu));
        button.set_popover(Some(&popup));
        desktop.present();
        settle(0.06);
        popup.popup();
        settle(0.06);
        let row = descendants(popup.upcast_ref()).into_iter().find(|w| w.css_name() == "modelbutton").unwrap();
        let check = descendants(&row).into_iter().find(|w| w.css_name() == "check").unwrap();
        row.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        entry.set_state_flags(gtk::StateFlags::FOCUSED | gtk::StateFlags::FOCUS_WITHIN, false);
        let desktop_alpha = alpha(&desktop);
        windows.push(desktop.clone());
        popovers.push(popup);
        fixtures.push((desktop, captions, selection, row, check, entry, desktop_alpha));
    }
    let theme = TesselShellTheme::new(Some(path.clone()));
    for update in [false, true] {
        if update {
            data["monitors"][0]["accent"] = json!([40, 120, 180]);
            data["monitors"][0]["foreground"] = json!([240, 225, 220]);
            data["monitors"][1]["accent"] = json!([240, 190, 100]);
            data["monitors"][1]["foreground"] = json!([210, 230, 245]);
            replace(&path, &data);
        }
        settle(0.5);
        for (colors, fixture) in data["monitors"].as_array().unwrap().iter().zip(&fixtures) {
            let (desktop, captions, selection, row, check, entry, desktop_alpha) = fixture;
            let accent = rgb(&colors["accent"]);
            // Captions sit on the wallpaper: white with a shadow, whatever the palette.
            assert_eq!(color(&captions[0].1), (255, 255, 255));
            assert_eq!(background_color(&captions[1].0), accent);
            let expected_on_accent = if [(220, 80, 95), (240, 190, 100)].contains(&accent) { (16, 17, 20) } else { (255, 255, 255) };
            assert_eq!(color(&captions[1].1), expected_on_accent);
            assert_eq!(background_color(check), accent);
            assert_eq!(border_color(row), accent);
            assert_eq!(border_color(entry), accent);
            let actual = background_color(selection);
            // Premultiplied 8-bit channels lose a few levels at 22%.
            for (a, w) in [(actual.0, accent.0), (actual.1, accent.1), (actual.2, accent.2)] {
                assert!((a - w).abs() <= 5, "selection {actual:?} vs {accent:?}");
            }
            assert_eq!(alpha(desktop), *desktop_alpha);
        }
    }
    theme.close();
    for popup in &popovers {
        popup.popdown();
    }
    for window in &windows {
        window.destroy();
    }
    gtk::style_context_remove_provider_for_display(&gdk::Display::default().unwrap(), &native);
    settle(0.06);
    let _ = std::fs::remove_dir_all(dir);
}

fn test_frosted_updates_keep_alpha_independent_and_foreground_opaque() {
    let dir = scratch("frosted");
    let path = dir.join("palette.json");
    let image = dir.join("tessel-dock/frosted").join(format!("{}.png", "e".repeat(64)));
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    std::fs::write(&image, b"legacy texture, never loaded").unwrap();
    let mut data = snapshot();
    for monitor in data["monitors"].as_array_mut().unwrap() {
        monitor["frost_texture"] = json!(image.to_string_lossy());
    }
    data["apps_frosted"] = json!(true);
    data["apps_frost_strength"] = json!(50);
    write(&path, &data);
    let window = gtk::Window::new();
    set_window_monitor(&window, Some(&monitor("HEADLESS-1")));
    let label = gtk::Label::new(Some("Readable controls"));
    window.set_child(Some(&label));
    window.present();
    settle(0.06);
    let theme = TesselShellTheme::new(Some(path.clone()));
    for strength in [0, 25, 50, 100] {
        for (enabled, amount, expected) in [(true, 0, 255), (true, 5, 242), (true, 50, 128), (true, 100, 0), (false, 100, 255)] {
            data["apps_transparency_enabled"] = json!(enabled);
            data["apps_transparency"] = json!(amount);
            data["apps_frost_strength"] = json!(strength);
            write(&path, &data);
            theme.refresh();
            settle(0.06);
            assert!((alpha(&window) - expected).abs() <= 1, "alpha {} vs {expected}", alpha(&window));
            assert_eq!(label.opacity(), 1.0);
            assert_eq!(color(&label), rgb(&data["monitors"][0]["foreground"]));
            if expected != 0 {
                // The compositor filters what is behind the window; GTK paints only its tint.
                let actual = background_color(&window);
                for (a, w) in [(actual.0, 18), (actual.1, 25), (actual.2, 35)] {
                    assert!((a - w).abs() <= 2, "tint {actual:?}");
                }
            }
            // Blur follows the frosted flag; the binding exists for the window.
            assert!(theme.window_blur(&window).is_some());
        }
    }
    theme.close();
    window.destroy();
    settle(0.06);
    let _ = std::fs::remove_dir_all(dir);
}

fn test_css_parses_in_both_scopes_and_both_appearances() {
    let modes = [CssOptions::default(), CssOptions { desktop: true, ..Default::default() },
                 CssOptions { switcher: true, ..Default::default() }, CssOptions { backdrop: true, ..Default::default() }];
    for mode in modes {
        for (background, foreground) in [([20.0, 25.0, 30.0], [238.0, 238.0, 238.0]), ([240.0, 243.0, 249.0], [35.0, 38.0, 45.0])] {
            let colors = Colors { background, foreground, accent: [70.0, 130.0, 210.0] };
            for (amount, frosted, rounded) in [(0, false, true), (50, false, true), (100, false, true), (50, true, false), (100, true, true)] {
                let provider = gtk::CssProvider::new();
                let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
                let sink = errors.clone();
                provider.connect_parsing_error(move |_, _, error| sink.borrow_mut().push(error.to_string()));
                let text = palette_css("test", &colors, amount, &CssOptions { frosted, rounded_corners: rounded, ..mode }).unwrap();
                provider.load_from_string(&text);
                assert_eq!(*errors.borrow(), Vec::<String>::new());
            }
        }
    }
}

fn test_marker_class_change_requeues_and_switcher_blur_target() {
    // A window that gains the switcher marker after theming is restyled at idle.
    let dir = scratch("markers");
    let path = dir.join("palette.json");
    let mut data = snapshot();
    data["apps_frosted"] = json!(true);
    write(&path, &data);
    let window = gtk::Window::new();
    window.present();
    settle(0.06);
    let theme = TesselShellTheme::new(Some(path.clone()));
    settle(0.06);
    let (class, _) = theme.window_provider(&window).unwrap();
    assert!(class.starts_with("tessel-shell-palette-") && window.has_css_class(&class));
    window.add_css_class("tessel-shell-switcher-backdrop");
    settle(0.06);
    // The backdrop variant is transparent and keeps the same class/provider.
    assert_eq!(theme.window_provider(&window).unwrap().0, class);
    assert_eq!(alpha(&window), 0);
    theme.close();
    assert!(!window.has_css_class(&class));
    assert!(theme.themed_windows().is_empty());
    window.destroy();
    settle(0.06);
    let _ = std::fs::remove_dir_all(dir);
}

fn main() {
    if std::env::var("TESSEL_SHELL_UI_ISOLATED").as_deref() != Ok("1") {
        println!("ui_headless: skipped (run tests/run_headless.sh for the private compositor)");
        return;
    }
    gtk::init().expect("gtk init in the private compositor");
    let tests: [(&str, fn()); 7] = [
        ("test_live_wallpaper_accent_colors_controls_and_chooses_readable_text", test_live_wallpaper_accent_colors_controls_and_chooses_readable_text),
        ("test_live_updates_scope_desktop_and_remove_destroyed_windows", test_live_updates_scope_desktop_and_remove_destroyed_windows),
        ("test_square_setting_flattens_controls_and_restores_local_styles", test_square_setting_flattens_controls_and_restores_local_styles),
        ("test_desktop_captions_selection_and_menu_controls_follow_each_monitor", test_desktop_captions_selection_and_menu_controls_follow_each_monitor),
        ("test_frosted_updates_keep_alpha_independent_and_foreground_opaque", test_frosted_updates_keep_alpha_independent_and_foreground_opaque),
        ("test_css_parses_in_both_scopes_and_both_appearances", test_css_parses_in_both_scopes_and_both_appearances),
        ("test_marker_class_change_requeues_and_switcher_blur_target", test_marker_class_change_requeues_and_switcher_blur_target),
    ];
    let mut failed = 0;
    for (name, test) in tests {
        let result = std::panic::catch_unwind(test);
        println!("test {name} ... {}", if result.is_ok() { "ok" } else { "FAILED" });
        failed += usize::from(result.is_err());
    }
    println!("ui_headless: {} passed, {failed} failed", tests.len() - failed);
    std::process::exit(i32::from(failed > 0));
}
