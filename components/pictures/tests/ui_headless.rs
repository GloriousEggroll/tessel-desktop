#![allow(deprecated)]
//! Port of test_picture_ui.py (PicturesUITests). Runs only inside the private nested
//! headless labwc started by tests/run_headless.sh (own XDG_RUNTIME_DIR, D-Bus and XDG
//! dirs, fixture images in a private folder); under plain `cargo test` it skips itself.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use image::{Rgb, RgbImage, Rgba, RgbaImage};
use tessel_pictures::app::{create_application, PicturesApp};
use tessel_pictures::canvas::Zoom;
use tessel_pictures::model::Operation;
use tessel_pictures::ui::PicturesWindow;
use std::path::{Path, PathBuf};
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

fn wait_for(timeout: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait(predicate: impl FnMut() -> bool) -> bool { wait_for(8.0, predicate) }

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

fn button_labelled(root: &gtk::Widget, label: &str) -> gtk::Button {
    descendants(root).into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.label().as_deref() == Some(label)).unwrap_or_else(|| panic!("no button {label}"))
}

/// The fixture landscape (sky, sun, hill) of the Python tests.
fn landscape(color: [u8; 3]) -> RgbImage {
    RgbImage::from_fn(800, 500, |x, y| {
        let (fx, fy) = (x as f64, y as f64);
        if (fx - 565.0).powi(2) + (fy - 165.0).powi(2) <= 75.0f64.powi(2) {
            Rgb([0xf5, 0xd2, 0x9c])
        } else if fy >= 200.0 && fx >= 300.0 - (fy - 200.0) && fx <= 300.0 + (fy - 200.0) * 0.9 {
            Rgb([0x31, 0x45, 0x5a])
        } else {
            Rgb(color)
        }
    })
}

struct Case {
    window: PicturesWindow,
    folder: PathBuf,
    first: PathBuf,
    second: PathBuf,
}

impl Case {
    fn new(app: &PicturesApp, name: &str) -> Self {
        let folder = std::env::temp_dir().join(format!("pictures-ui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let (first, second) = (folder.join("01-sunset.png"), folder.join("02-coast.jpg"));
        landscape([0xba, 0x6d, 0x8d]).save(&first).unwrap();
        landscape([0x51, 0x80, 0xa7]).save(&second).unwrap();
        std::fs::write(folder.join("not-a-picture.txt"), "excluded").unwrap();
        let window = app.activate();
        window.open_paths(&[first.clone()], false);
        assert!(wait(|| !window.busy.get()), "Initial image did not load");
        assert!(window.doc_info().is_some(), "{}", window.status.text());
        Self { window, folder, first, second }
    }

    fn size(&self) -> (u32, u32) {
        let info = self.window.doc_info().unwrap();
        (info.width, info.height)
    }

    fn idle(&self) -> bool { wait(|| !self.window.busy.get()) }
}

impl Drop for Case {
    fn drop(&mut self) {
        wait(|| !self.window.busy.get());
        self.window.shutdown();
        for window in gtk::Window::list_toplevels() {
            if let Ok(window) = window.downcast::<gtk::Window>() {
                window.destroy();
            }
        }
        wait_for(3.0, || self.window.thumbnail_pending.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

fn browse_view_edit_save_and_theme(app: &PicturesApp) {
    let case = Case::new(app, "browse");
    let w = &case.window;
    assert_eq!(case.size(), (800, 500));
    assert_eq!(w.stack.visible_child_name().as_deref(), Some("view"));
    assert_eq!(w.thumb_store.n_items(), 2);
    assert!(w.theme.borrow().is_some());
    let original = std::fs::read(&case.first).unwrap();
    w.edit(Operation::Right);
    assert!(case.idle());
    assert_eq!(case.size(), (500, 800));
    assert!(w.edit_bar.is_visible());
    assert_eq!(std::fs::read(&case.first).unwrap(), original);
    w.undo(false);
    assert!(case.idle());
    assert_eq!(case.size(), (800, 500));
    assert!(!w.doc_info().unwrap().dirty);
    w.edit(Operation::Resize(400.0, 250.0));
    assert!(case.idle());
    let output = case.folder.join("edited.png");
    w.save_to(&output, false, None);
    assert!(case.idle());
    assert_eq!(image::image_dimensions(&output).unwrap(), (400, 250));
    assert!(!w.doc_info().unwrap().dirty);
    w.browse_folder(&case.folder, false);
    assert!(case.idle());
    assert_eq!(w.store.n_items(), 3);
    w.search.set_text("COAST");
    w.filter_entries();
    assert_eq!(w.store.n_items(), 1);
    w.grid.emit_by_name::<()>("activate", &[&0u32]);
    assert!(case.idle());
    assert_eq!(w.doc_info().unwrap().path, case.second);
    w.tools.set_visible(true);
    assert!(wait(|| w.canvas.area.is_mapped()));
    if let Some(path) = std::env::var_os("PICTURES_UI_SCREENSHOT") {
        wait(|| w.thumbnail_pending.borrow().is_empty());
        settle(0.3);
        let monitor = WidgetExt::display(&w.window).monitor_at_surface(&w.window.surface().unwrap()).unwrap();
        let status = std::process::Command::new("grim").arg("-o").arg(monitor.connector().unwrap().as_str()).arg(path).status().unwrap();
        assert!(status.success());
    }
    println!("ok browse_view_edit_save_and_theme");
}

fn unsaved_navigation_and_comparison(app: &PicturesApp) {
    let case = Case::new(app, "unsaved");
    let w = &case.window;
    let original = std::fs::read(&case.first).unwrap();
    w.edit(Operation::Mirror);
    assert!(case.idle());
    w.open_paths(&[case.second.clone()], false);
    let confirm = w.confirm.borrow().clone().expect("unsaved-changes prompt");
    button_labelled(confirm.upcast_ref(), "Cancel").emit_clicked();
    assert_eq!(w.doc_info().unwrap().path, case.first);
    assert!(w.doc_info().unwrap().dirty);
    w.open_paths(&[case.second.clone()], false);
    let confirm = w.confirm.borrow().clone().expect("unsaved-changes prompt");
    button_labelled(confirm.upcast_ref(), "Discard").emit_clicked();
    assert!(case.idle());
    assert_eq!(w.doc_info().unwrap().path, case.second);
    assert_eq!(std::fs::read(&case.first).unwrap(), original);
    let comparison = tessel_pictures::dialogs::compare(&w.window, &[case.first.clone(), case.second.clone()]);
    assert!(wait(|| comparison.is_mapped()));
    comparison.close();
    println!("ok unsaved_navigation_and_comparison");
}

fn crop_button_and_annotation_switching(app: &PicturesApp) {
    let case = Case::new(app, "crop");
    let w = &case.window;
    w.tools.set_visible(true);
    w.crop_button.emit_clicked();
    assert_eq!(w.canvas.tool(), "crop");
    assert!(w.selection_options.is_visible());
    assert!(!w.annotation_options.is_visible());
    for (widget, value) in w.coordinates.iter().zip([20.0, 30.0, 300.0, 200.0]) {
        widget.set_value(value);
    }
    w.apply_selection();
    assert!(case.idle());
    assert_eq!(case.size(), (300, 200));
    w.undo(false);
    assert!(case.idle());
    assert_eq!(case.size(), (800, 500));
    // Leaving crop must clear the toggle and keep annotation choices mapped
    // to the correct tools after removing Crop from the drop-down.
    w.tool.set_selected(3);
    assert_eq!(w.canvas.tool(), "arrow");
    assert!(!w.crop_button.is_active());
    assert!(w.annotation_options.is_visible());
    w.canvas.area.grab_focus();
    w.key_pressed(gdk::Key::c, gdk::ModifierType::SHIFT_MASK);
    assert_eq!(w.canvas.tool(), "crop");
    w.key_pressed(gdk::Key::Escape, gdk::ModifierType::empty());
    assert_eq!(w.canvas.tool(), "pan");
    assert!(!w.crop_button.is_active());
    w.set_edit_tool("stamp");
    assert_eq!(w.tool.selected(), 9);
    assert_eq!(w.canvas.tool(), "stamp");
    println!("ok crop_button_and_annotation_switching");
}

fn animation_navigation_does_not_leave_a_running_timer(app: &PicturesApp) {
    let case = Case::new(app, "animation");
    let w = &case.window;
    let path = case.folder.join("animation.gif");
    {
        let mut file = std::fs::File::create(&path).unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut file);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            encoder.encode_frame(image::Frame::from_parts(RgbaImage::from_pixel(40, 40, Rgba(color)), 0, 0,
                                                         image::Delay::from_numer_denom_ms(60, 1))).unwrap();
        }
    }
    w.open_paths(&[path], false);
    assert!(case.idle());
    w.toggle_animation();
    assert!(wait(|| w.animation_index.get() == 1));
    w.open_paths(&[case.first.clone()], false);
    assert!(case.idle());
    assert_eq!(w.doc_info().unwrap().path, case.first);
    assert!(!w.animation_playing.get());
    assert!(!w.animation_timer_pending());
    println!("ok animation_navigation_does_not_leave_a_running_timer");
}

fn fullscreen_shortcuts_thumbnail_size_and_context_menu(app: &PicturesApp) {
    let case = Case::new(app, "fullscreen");
    let w = &case.window;
    w.edit(Operation::Right);
    assert!(case.idle());
    w.undo(false);
    assert!(case.idle());
    assert!(w.key_pressed(gdk::Key::Z, gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK));
    assert!(case.idle());
    assert_eq!(case.size(), (500, 800));
    w.undo(false);
    assert!(case.idle());
    let controllers = w.canvas.area.observe_controllers();
    let gesture = (0..controllers.n_items()).filter_map(|i| controllers.item(i))
        .filter_map(|o| o.downcast::<gtk::GestureClick>().ok()).find(|g| g.button() == 3).unwrap();
    gesture.emit_by_name::<()>("pressed", &[&1i32, &40f64, &40f64]);
    assert!(wait(|| w.context_menu.borrow().as_ref().is_some_and(|m| m.is_mapped())));
    let menu = w.context_menu.borrow().clone().unwrap();
    menu.popdown();
    assert!(wait(|| w.context_menu.borrow().is_none()));
    w.window.fullscreen();
    assert!(wait(|| w.window.is_fullscreen()));
    assert!(!w.sidebar.is_visible());
    w.fullscreen_motion(10.0);
    assert!(w.toolbar.is_visible());
    w.window.unfullscreen();
    assert!(wait(|| !w.window.is_fullscreen()));
    assert!(w.sidebar.is_visible());
    w.browse_folder(&case.folder, false);
    assert!(case.idle());
    w.thumbnail_size.set_selected(2);
    assert_eq!(w.preferences.borrow()["thumbnail_size"], serde_json::json!(2));
    assert!(wait(|| w.thumbnail_pending.borrow().is_empty()));
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&w.config_path).unwrap()).unwrap();
    assert_eq!(saved["thumbnail_size"], 2);
    println!("ok fullscreen_shortcuts_thumbnail_size_and_context_menu");
}

fn file_monitor_distinguishes_own_save_from_external_change(app: &PicturesApp) {
    let case = Case::new(app, "monitor");
    let w = &case.window;
    let file = gio::File::for_path(&case.first);
    w.message("Saved picture");
    w.folder_changed(&file, gio::FileMonitorEvent::Changed);
    assert!(wait(|| !w.monitor_pending()));
    assert_eq!(w.status.text(), "Saved picture");
    RgbImage::from_pixel(12, 12, Rgb([255, 255, 255])).save(&case.first).unwrap();
    w.folder_changed(&file, gio::FileMonitorEvent::Changed);
    assert!(wait(|| !w.monitor_pending()));
    assert!(w.status.text().contains("changed on disk"), "{}", w.status.text());
    assert_eq!(case.size(), (800, 500));
    println!("ok file_monitor_distinguishes_own_save_from_external_change");
}

fn viewing_keys_navigate_and_zoom_like_loupe(app: &PicturesApp) {
    let case = Case::new(app, "keys");
    let w = &case.window;
    let none = gdk::ModifierType::empty();
    w.canvas.area.grab_focus();
    let at = |path: &Path| { let path = path.to_path_buf(); move || !w.busy.get() && w.current_path.borrow().as_ref() == Some(&path) };
    assert!(w.key_pressed(gdk::Key::Right, none));
    assert!(wait(at(&case.second)));
    for (key, expected) in [(gdk::Key::BackSpace, &case.first), (gdk::Key::space, &case.second), (gdk::Key::Page_Up, &case.first),
                            (gdk::Key::End, &case.second), (gdk::Key::Home, &case.first)] {
        assert!(w.key_pressed(key, none), "{key:?}");
        assert!(wait(at(expected)), "{key:?}");
    }
    // Zoomed beyond the viewport, plain arrows pan; Alt+arrows still navigate.
    assert!(w.key_pressed(gdk::Key::plus, gdk::ModifierType::CONTROL_MASK));
    assert_eq!(w.canvas.zoom_mode(), "custom");
    w.canvas.set_zoom(Zoom::Value(8.0));
    assert!(wait(|| w.can_pan(true)));
    let before = w.image_scroll.hadjustment().value();
    assert!(w.key_pressed(gdk::Key::Right, none));
    assert!(w.image_scroll.hadjustment().value() > before);
    assert_eq!(w.current_path.borrow().as_ref(), Some(&case.first));
    assert!(w.key_pressed(gdk::Key::Right, gdk::ModifierType::ALT_MASK));
    assert!(wait(at(&case.second)));
    assert!(w.key_pressed(gdk::Key::minus, gdk::ModifierType::CONTROL_MASK));
    assert!(w.key_pressed(gdk::Key::_0, gdk::ModifierType::CONTROL_MASK));
    assert_eq!(w.canvas.zoom_mode(), "fit");
    // Typing in a field keeps its own keys.
    w.tools.set_visible(true);
    w.set_edit_tool("text");
    assert!(wait(|| w.annotation_text.is_mapped()));
    assert!(w.annotation_text.grab_focus());
    assert!(wait(|| GtkWindowExt::focus(&w.window).is_some_and(|f| f.is::<gtk::Editable>())));
    assert!(!w.navigation_key(gdk::Key::space, none));
    println!("ok viewing_keys_navigate_and_zoom_like_loupe");
}

fn header_menu_has_standard_entries_and_default_viewer_action(app: &PicturesApp) {
    let case = Case::new(app, "menu");
    let w = &case.window;
    assert_eq!(w.menu_button.ancestor(gtk::HeaderBar::static_type()), Some(w.header.clone().upcast()));
    assert!(tessel_gtk::app_menu::has_menu_icon(&w.menu_button));
    for name in ["shortcuts", "about", "quit"] {
        assert!(app.app.lookup_action(name).is_some(), "{name}");
    }
    assert!(w.window.lookup_action("set-default").is_some());
    let areas: [gtk::Widget; 6] = [w.header.clone().upcast(), w.toolbar.clone().upcast(), w.edit_bar.clone().upcast(),
                                   w.zoom_bar.clone().upcast(), w.tools.clone().upcast(), w.sidebar.clone().upcast()];
    let unlabeled: Vec<gtk::Widget> = areas.iter().flat_map(descendants).filter(|widget| {
        let kind = widget.type_();
        (kind == gtk::Button::static_type() || kind == gtk::ToggleButton::static_type())
            && widget.downcast_ref::<gtk::Button>().is_some_and(|b| b.label().is_none() && b.child().is_some_and(|c| c.is::<gtk::Image>()))
            && widget.tooltip_text().is_none()
            && [gtk::WindowControls::static_type(), gtk::MenuButton::static_type(), gtk::DropDown::static_type(),
                gtk::SpinButton::static_type()].iter().all(|t| widget.ancestor(*t).is_none())
    }).collect();
    assert!(unlabeled.is_empty(), "icon-only buttons need a label: {unlabeled:?}");
    println!("ok header_menu_has_standard_entries_and_default_viewer_action");
}

fn answer(values: &[(&str, bool)]) {
    for (label, every) in values {
        let dialogs = || gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
            .filter(|w| w.title().as_deref() == Some("File Already Exists") && w.is_visible()).collect::<Vec<_>>();
        assert!(wait(|| !dialogs().is_empty()), "{label}");
        let dialog = dialogs()[0].clone();
        if *every {
            descendants(dialog.upcast_ref()).into_iter().find_map(|w| w.downcast::<gtk::CheckButton>().ok()).unwrap().set_active(true);
        }
        button_labelled(dialog.upcast_ref(), label).emit_clicked();
        wait_for(2.0, || !dialog.is_visible());
    }
}

fn copy_conflicts_offer_replace_keep_both_and_skip(app: &PicturesApp) {
    let case = Case::new(app, "conflicts");
    let w = &case.window;
    let target = case.folder.join("target");
    std::fs::create_dir(&target).unwrap();
    let name = |p: &Path| p.file_name().unwrap().to_owned();
    std::fs::write(target.join(name(&case.first)), b"old-first").unwrap();
    std::fs::write(target.join(name(&case.second)), b"old-second").unwrap();
    w.transfer(&[case.first.clone(), case.second.clone()], &target, false, false);
    answer(&[("Keep Both", false), ("Replace", false)]);
    assert!(wait(|| !w.busy.get() && w.transfer_results.borrow().is_some()));
    assert_eq!(std::fs::read(target.join(name(&case.first))).unwrap(), b"old-first");
    assert_eq!(std::fs::read(target.join("01-sunset (2).png")).unwrap(), std::fs::read(&case.first).unwrap());
    assert_eq!(std::fs::read(target.join(name(&case.second))).unwrap(), std::fs::read(&case.second).unwrap());
    w.transfer_results.take().unwrap().destroy();
    // Skip with "apply to all" leaves every existing file untouched.
    assert!(wait(|| !w.busy.get()));
    std::fs::write(target.join(name(&case.second)), b"kept").unwrap();
    w.transfer(&[case.first.clone(), case.second.clone()], &target, false, false);
    answer(&[("Skip", true)]);
    assert!(wait(|| !w.busy.get() && w.transfer_results.borrow().is_some()));
    assert_eq!(std::fs::read(target.join(name(&case.second))).unwrap(), b"kept");
    assert!(w.status.text().contains("2 not completed"), "{}", w.status.text());
    w.transfer_results.take().unwrap().destroy();
    println!("ok copy_conflicts_offer_replace_keep_both_and_skip");
}

fn command_line_opens_files_in_the_existing_window(app: &PicturesApp) {
    let case = Case::new(app, "cli");
    let w = &case.window;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_tessel-pictures")).arg(&case.second).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = None;
    while Instant::now() < deadline && status.is_none() {
        settle(0.02);
        status = child.try_wait().unwrap();
    }
    if status.is_none() {
        let _ = child.kill();
    }
    assert!(status.is_some_and(|s| s.success()), "{status:?}");
    assert!(wait(|| !w.busy.get() && w.current_path.borrow().as_ref() == Some(&case.second)));
    assert_eq!(app.app.windows().len(), 1);
    println!("ok command_line_opens_files_in_the_existing_window");
}

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if runtime == "/run/user/1000" || std::env::var_os("WAYLAND_DISPLAY").is_none()
        || std::env::var_os("PICTURES_UI_ISOLATED_TEST").is_none() {
        println!("ui_headless: skipped (run components/pictures/tests/run_headless.sh for the private compositor)");
        return;
    }
    tessel_core::i18n::initialize();
    gtk::init().expect("display");
    gtk::Settings::default().unwrap().set_gtk_application_prefer_dark_theme(true);
    let app = create_application();
    app.app.register(gio::Cancellable::NONE).unwrap();
    browse_view_edit_save_and_theme(&app);
    unsaved_navigation_and_comparison(&app);
    crop_button_and_annotation_switching(&app);
    animation_navigation_does_not_leave_a_running_timer(&app);
    fullscreen_shortcuts_thumbnail_size_and_context_menu(&app);
    file_monitor_distinguishes_own_save_from_external_change(&app);
    viewing_keys_navigate_and_zoom_like_loupe(&app);
    header_menu_has_standard_entries_and_default_viewer_action(&app);
    copy_conflicts_offer_replace_keep_both_and_skip(&app);
    command_line_opens_files_in_the_existing_window(&app);
    println!("ui_headless: all checks passed");
}
