#![allow(deprecated)]
//! Port of test_editor_ui.py (EditorTests) plus Rust-only checks. Runs only
//! inside the private nested headless labwc started by tests/run_headless.sh
//! (own XDG_RUNTIME_DIR, private D-Bus, private XDG config/cache/data/state);
//! under plain `cargo test` it skips itself. It never touches the live session.

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tessel_text_editor::document::Target;
use tessel_text_editor::recovery;
use tessel_text_editor::{EditorApplication, EditorWindow};

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_idle(window: &EditorWindow) {
    let end = Instant::now() + Duration::from_secs(4);
    while window.busy.get() && Instant::now() < end {
        settle(0.03);
    }
    assert!(!window.busy.get(), "The editor did not finish its file operation");
    settle(0.1);
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(current) = child {
        result.extend(descendants(&current));
        child = current.next_sibling();
    }
    result
}

fn buttons(window: &gtk::Window) -> HashMap<String, gtk::Button> {
    descendants(window.upcast_ref()).into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok())
        .filter_map(|b| b.label().map(|l| (l.to_string(), b))).collect()
}

struct Fixture {
    app: EditorApplication,
    window: EditorWindow,
    root: PathBuf,
}

impl Fixture {
    fn editor_windows(&self) -> Vec<EditorWindow> {
        self.app.editor_windows()
    }

    fn close_children(&self) {
        for child in self.app.app.windows() {
            if child != *self.window.window.upcast_ref::<gtk::Window>() {
                child.close();
            }
        }
        settle(0.1);
    }
}

type Check = fn(&Fixture);

fn test_standard_app_menu_quit_and_new_window_shortcuts(f: &Fixture) {
    let button = f.window.menu_button();
    assert_eq!(button.ancestor(gtk::HeaderBar::static_type()).as_ref(), Some(f.window.header.upcast_ref::<gtk::Widget>()));
    for name in ["shortcuts", "about", "quit"] {
        assert!(f.app.app.lookup_action(name).is_some(), "{name}");
    }
    assert_eq!(f.app.app.accels_for_action("app.quit"), ["<Control>q"]);
    assert_eq!(f.app.app.accels_for_action("win.new-window").len(), 2);
}

fn walk(menu: &gio::MenuModel, labels: &mut Vec<Option<String>>, actions: &mut Vec<Option<String>>) {
    for index in 0..menu.n_items() {
        let get = |name: &str| menu.item_attribute_value(index, name, Some(glib::VariantTy::STRING)).and_then(|v| v.str().map(str::to_owned));
        labels.push(get("label"));
        actions.push(get("action"));
        for link in ["section", "submenu"] {
            if let Some(child) = menu.item_link(index, link) {
                walk(&child, labels, actions);
            }
        }
    }
}

fn test_primary_menu_holds_former_menubar_entries(f: &Fixture) {
    let (mut labels, mut actions) = (Vec::new(), Vec::new());
    walk(&f.window.menu_button().menu_model().unwrap(), &mut labels, &mut actions);
    for title in ["Edit", "View", "Bookmarks", "Open Recent"] {
        assert!(labels.contains(&Some(title.into())), "{title}");
    }
    for action in ["win.open", "win.find", "win.wrap", "win.bookmark", "win.preferences", "app.quit"] {
        assert!(actions.contains(&Some(action.into())), "{action}");
    }
    assert!(!f.window.save_button.has_css_class("suggested-action"));
    assert_eq!(f.app.app.accels_for_action("win.find"), ["<Control>f"]);
    assert_eq!((tessel_text_editor::style::line_count(1), tessel_text_editor::style::line_count(3)), ("1 line".into(), "3 lines".into()));
}

fn test_load_edit_save_and_single_document_window(f: &Fixture) {
    let path = f.root.join("hello.py");
    std::fs::write(&path, b"# Example\r\nprint('hello')\r\n").unwrap();
    f.window.load(Target::Path(path.clone()), None);
    wait_idle(&f.window);
    assert_eq!(f.window.buffer.language().map(|l| l.id().to_string()).as_deref(), Some("python3"));
    assert_eq!(*f.window.newline.borrow(), "CRLF");
    let all = descendants(f.window.window.upcast_ref());
    assert_eq!(all.iter().filter(|w| w.is::<sourceview5::View>()).count(), 1);
    assert!(!all.iter().any(|w| w.is::<gtk::Notebook>()));
    f.window.buffer.insert_at_cursor("# Added\n");
    assert!(f.window.modified());
    f.window.buffer.undo();
    assert!(!f.window.modified());
    f.window.buffer.redo();
    f.window.save(None);
    wait_idle(&f.window);
    assert!(!f.window.modified());
    assert!(std::fs::read(&path).unwrap().starts_with(b"# Added\r\n"));
    // Status texts.
    assert_eq!(f.window.window.title().unwrap(), "hello.py — Text Editor");
    assert_eq!(f.window.details.text(), "UTF-8  ·  CRLF  ·  4 lines");
    assert_eq!(f.window.path_label.text().as_str(), &*f.root.to_string_lossy());
}

fn test_unsaved_cancel_discard_and_failed_save_guard(f: &Fixture) {
    let window = &f.window;
    window.buffer.set_text("keep this");
    assert_eq!(window.title_label.text(), "● Untitled");
    assert_eq!(window.window.title().unwrap(), "*Untitled — Text Editor");
    let calls = Rc::new(RefCell::new(Vec::<&str>::new()));
    let c = calls.clone();
    window.guard(Box::new(move |_| c.borrow_mut().push("done")));
    let dialog = window.dialog.borrow().clone().expect("guard dialog");
    buttons(&dialog)["Cancel"].emit_clicked();
    assert!(calls.borrow().is_empty());
    assert_eq!(window.text(), "keep this");
    let c = calls.clone();
    window.guard(Box::new(move |_| c.borrow_mut().push("done")));
    let dialog = window.dialog.borrow().clone().unwrap();
    buttons(&dialog)["Discard"].emit_clicked();
    assert_eq!(*calls.borrow(), ["done"]);
    window.document.borrow_mut().path = Some(f.root.join("missing").join("file.txt"));
    let c = calls.clone();
    window.guard(Box::new(move |_| c.borrow_mut().push("unsafe")));
    let dialog = window.dialog.borrow().clone().unwrap();
    buttons(&dialog)["Save"].emit_clicked();
    wait_idle(window);
    assert_eq!(*calls.borrow(), ["done"]);
    assert!(window.modified());
    assert!(window.banner.is_visible());
    assert!(window.banner_label.text().starts_with("[Errno 2] No such file or directory: '"), "{}", window.banner_label.text());
}

fn test_external_conflict_preserves_both_versions(f: &Fixture) {
    let path = f.root.join("conflict.txt");
    std::fs::write(&path, "original").unwrap();
    f.window.load(Target::Path(path.clone()), None);
    wait_idle(&f.window);
    f.window.buffer.set_text("my draft");
    std::fs::write(&path, "external draft").unwrap();
    f.window.save(None);
    wait_idle(&f.window);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external draft");
    assert_eq!(f.window.text(), "my draft");
    assert!(f.window.modified());
    assert!(f.window.banner_label.text().contains("changed outside"));
}

fn test_external_change_is_reported_by_the_monitor(f: &Fixture) {
    let path = f.root.join("watched.txt");
    std::fs::write(&path, "original").unwrap();
    f.window.load(Target::Path(path.clone()), None);
    wait_idle(&f.window);
    assert!(!f.window.banner.is_visible());
    std::fs::write(&path, "changed by another program").unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    while !f.window.banner.is_visible() && Instant::now() < end {
        settle(0.05);
    }
    assert!(f.window.banner.is_visible());
    assert!(f.window.banner_reload.is_visible());
    assert_eq!(f.window.banner_label.text(), "This file changed outside Text Editor. Reload it or save your changes as another file.");
    f.window.banner_reload.emit_clicked();
    wait_idle(&f.window);
    assert_eq!(f.window.text(), "changed by another program");
    assert!(!f.window.banner.is_visible());
}

fn test_search_case_whole_word_regex_replace_and_undo(f: &Fixture) {
    let w = &f.window;
    w.buffer.set_text("Cat cat catalog\nitem1 item2\n");
    w.find_entry.set_text("cat");
    settle(0.1);
    assert_eq!(w.search_context.occurrences_count(), 3);
    w.search_settings.set_at_word_boundaries(true);
    settle(0.1);
    assert_eq!(w.search_context.occurrences_count(), 2);
    w.search_settings.set_case_sensitive(true);
    settle(0.1);
    assert_eq!(w.search_context.occurrences_count(), 1);
    assert_eq!(w.search_count.text(), "1 matches");
    w.search_settings.set_regex_enabled(true);
    w.find_entry.set_text(r"item(\d)");
    w.replace_entry.set_text(r"row\1");
    settle(0.1);
    w.replace_all();
    assert!(w.text().contains("row1 row2"));
    assert_eq!(w.search_count.text(), "2 replacements");
    w.buffer.undo();
    assert!(w.text().contains("item1 item2"));
    w.find_entry.set_text("[");
    settle(0.1);
    assert!(w.search_context.regex_error().is_some());
    w.show_search(true);
    assert!(w.search_panel.is_visible() && w.replace_row.is_visible());
    w.hide_search();
    assert!(!w.search_panel.is_visible());
}

fn test_bookmarks_preferences_and_print_export(f: &Fixture) {
    let w = &f.window;
    w.buffer.set_text("first\nsecond\nthird\n");
    w.buffer.place_cursor(&w.buffer.start_iter());
    w.toggle_bookmark();
    let position = w.buffer.iter_at_line(2).unwrap();
    w.buffer.place_cursor(&position);
    w.toggle_bookmark();
    w.next_bookmark(false);
    assert_eq!(w.buffer.iter_at_mark(&w.buffer.get_insert()).line(), 0);
    w.next_bookmark(true);
    assert_eq!(w.buffer.iter_at_mark(&w.buffer.get_insert()).line(), 2);
    w.toggle_bookmark();
    assert_eq!(w.buffer.source_marks_at_line(2, Some("bookmark")).len(), 0);
    w.preferences_dialog();
    settle(0.1);
    assert!(f.app.app.windows().iter().any(|win| win.title().as_deref() == Some("Editor Preferences")));
    f.close_children();
    w.zoom(1);
    assert_eq!(w.options.borrow().font, "Monospace 12");
    w.go_to_line();
    settle(0.1);
    assert!(f.app.app.windows().iter().any(|win| win.title().as_deref() == Some("Go to Line")));
    f.close_children();
    w.zoom(0);
    let path = f.root.join("document.pdf");
    w.print_document(Some(path.clone()));
    settle(0.3);
    assert!(std::fs::read(&path).unwrap().starts_with(b"%PDF"));
}

fn test_preferences_alone(f: &Fixture) {
    f.window.preferences_dialog();
    settle(0.1);
    f.close_children();
}

fn test_vim_write_uses_guarded_save(f: &Fixture) {
    let w = &f.window;
    let path = f.root.join("vim.txt");
    std::fs::write(&path, "old").unwrap();
    w.load(Target::Path(path.clone()), None);
    wait_idle(w);
    w.options.borrow_mut().vim = true;
    w.apply_options(false);
    assert!(w.vim_label.is_visible());
    assert_eq!(w.vim_label.text(), "Vim");
    w.buffer.set_text("saved through Vim");
    assert!(w.vim.emit_by_name::<bool>("execute-command", &[&":w"]));
    wait_idle(w);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved through Vim");
    w.buffer.set_text("unsaved draft");
    std::fs::write(&path, "external edit").unwrap();
    w.vim.emit_by_name::<bool>("execute-command", &[&":wq"]);
    wait_idle(w);
    assert!(!w.closing.get());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external edit");
    assert!(w.modified());
    assert!(!w.vim.emit_by_name::<bool>("execute-command", &[&":unknown"]));
    w.options.borrow_mut().vim = false;
    w.apply_options(false);
}

fn test_plain_modal_lifecycle(f: &Fixture) {
    let dialog = gtk::Window::builder().application(&f.app.app).transient_for(&f.window.window).modal(true).build();
    dialog.set_child(Some(&gtk::Label::new(Some("Dialog"))));
    dialog.present();
    settle(0.1);
    dialog.close();
    settle(0.1);
}

fn rgb(value: &gdk::RGBA) -> [i64; 3] {
    [value.red(), value.green(), value.blue()].map(|c| (c as f64 * 255.0).round() as i64)
}

/// Render a widget's CSS background into a 16×16 ARGB surface and read the centre pixel.
fn background(widget: &gtk::Widget) -> (Option<[i64; 3]>, f64) {
    let snapshot = gtk::Snapshot::new();
    snapshot.render_background(&widget.style_context(), 0.0, 0.0, 16.0, 16.0);
    let node = snapshot.to_node();
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 16, 16).unwrap();
    {
        let context = cairo::Context::new(&surface).unwrap();
        if let Some(node) = node {
            node.draw(&context);
        }
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().unwrap();
    let offset = 8 * stride + 8 * 4;
    let (blue, green, red, alpha) = (data[offset] as f64, data[offset + 1] as f64, data[offset + 2] as f64, data[offset + 3] as f64);
    if alpha == 0.0 {
        return (None, 0.0);
    }
    let un = |c: f64| (c * 255.0 / alpha).round() as i64;
    (Some([un(red), un(green), un(blue)]), alpha / 255.0)
}

fn scheme_color(scheme: &sourceview5::StyleScheme, style: &str, background: bool) -> [i64; 3] {
    let style = scheme.style(style).unwrap();
    let value = if background { style.background() } else { style.foreground() }.unwrap();
    rgb(&gdk::RGBA::parse(value.as_str()).unwrap())
}

fn test_document_surface_keeps_neutral_colors_inside_wallpaper_theme(f: &Fixture) {
    let w = &f.window;
    let path = tessel_gtk::theme::palette_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let surface = w.window.surface().expect("mapped window");
    let monitor = WidgetExt::display(&w.window).monitor_at_surface(&surface).unwrap();
    let g = monitor.geometry();
    let texture_path = path.parent().unwrap().join("frosted").join(format!("{}.png", "a".repeat(64)));
    std::fs::create_dir_all(texture_path.parent().unwrap()).unwrap();
    let pixels: Vec<u8> = [80u8, 145, 195].repeat(32 * 32);
    gdk::MemoryTexture::new(32, 32, gdk::MemoryFormat::R8g8b8, &glib::Bytes::from(&pixels), 96).save_to_png(&texture_path).unwrap();
    let entry = serde_json::json!({"name": monitor.connector().map(|c| c.to_string()), "geometry": [g.x(), g.y(), g.width(), g.height()],
        "background": [75, 25, 105], "foreground": [240, 205, 120], "accent": [190, 220, 60],
        "frost_texture": texture_path.to_string_lossy()});
    let client = w.app.palette.borrow().clone().unwrap();
    for (dark, enabled, transparency, frosted, rounded) in
        [(false, false, 20, false, true), (true, true, 35, false, true), (false, true, 100, false, true),
         (true, false, 70, false, true), (true, true, 25, true, true), (false, true, 100, true, false)] {
        let case = format!("dark={dark} enabled={enabled} transparency={transparency} frosted={frosted}");
        std::fs::write(&path, serde_json::json!({"version": 1, "is_dark": dark, "monitors": [entry],
            "apps_transparency_enabled": enabled, "apps_frosted": frosted, "apps_rounded_corners": rounded,
            "apps_transparency": transparency}).to_string()).unwrap();
        client.refresh();
        w.sync_style();
        settle(0.1);
        let scheme = w.buffer.style_scheme().unwrap();
        assert_eq!(scheme.id(), if dark { "Adwaita-dark" } else { "Adwaita" }, "{case}");
        let view: &gtk::Widget = w.view.upcast_ref();
        let (color, alpha) = background(view);
        assert_eq!(color, Some(scheme_color(&scheme, "text", true)), "{case}");
        assert_eq!(alpha, 1.0, "{case}");
        assert_eq!(rgb(&view.color()), scheme_color(&scheme, "text", false), "{case}");
        let gutter = sourceview5::prelude::ViewExt::gutter(&w.view, gtk::TextWindowType::Left);
        assert_eq!(background(gutter.upcast_ref()).0, Some(scheme_color(&scheme, "line-numbers", true)), "{case}");
        let expected = if enabled { 1.0 - transparency as f64 / 100.0 } else { 1.0 };
        let window_alpha = background(w.window.upcast_ref()).1;
        assert!((window_alpha - expected).abs() < 0.01, "{case}: window alpha {window_alpha}");
        assert_eq!(background(&w.window.titlebar().unwrap()).1, 0.0, "{case}");
        assert_eq!(rgb(&w.title_label.color()), [240, 205, 120], "{case}");
        let popover = w.menu_button().popover().unwrap();
        popover.popup();
        settle(0.05);
        let contents = descendants(popover.upcast_ref()).into_iter().find(|w| w.css_name() == "contents").unwrap();
        let (color, alpha) = background(&contents);
        assert!((alpha - expected).abs() < 0.01, "{case}: popover alpha {alpha}");
        if expected > 0.0 && !frosted {
            for (actual, wanted) in color.unwrap().iter().zip([75, 25, 105]) {
                assert!((actual - wanted).abs() <= 2, "{case}: popover {color:?}");
            }
        }
        assert_eq!(rgb(&popover.color()), [240, 205, 120], "{case}");
        popover.popdown();
        settle(0.05);
    }
    let _ = std::fs::remove_file(&path);
    client.refresh();
    w.sync_style();
}

fn test_document_scheme_follows_gtk_light_dark_without_a_dock_palette(f: &Fixture) {
    let settings = gtk::Settings::default().unwrap();
    let old_name = settings.gtk_theme_name();
    let old_dark = settings.is_gtk_application_prefer_dark_theme();
    let saved = f.app.palette.replace(None);
    for (name, prefer_dark, expected) in [("Adwaita", false, "Adwaita"), ("Adwaita", true, "Adwaita-dark"), ("Adwaita-dark", false, "Adwaita-dark")] {
        settings.set_gtk_theme_name(Some(name));
        settings.set_gtk_application_prefer_dark_theme(prefer_dark);
        f.window.sync_style();
        assert_eq!(f.window.buffer.style_scheme().unwrap().id(), expected, "{name} {prefer_dark}");
    }
    f.app.palette.replace(saved);
    settings.set_gtk_theme_name(old_name.as_deref());
    settings.set_gtk_application_prefer_dark_theme(old_dark);
    f.window.sync_style();
}

fn test_multiple_cli_files_get_separate_windows_and_reuse_existing(f: &Fixture) {
    let (first, second) = (f.root.join("one.txt"), f.root.join("two.txt"));
    std::fs::write(&first, "one").unwrap();
    std::fs::write(&second, "two").unwrap();
    let files = [gio::File::for_path(&first), gio::File::for_path(&second)];
    f.app.open_files(&files);
    for window in f.editor_windows() {
        wait_idle(&window);
    }
    assert_eq!(f.editor_windows().len(), 3);
    f.app.open_files(&files);
    assert_eq!(f.editor_windows().len(), 3);
    // A missing command-line file names the document without creating it.
    let missing = f.root.join("new-name.txt");
    f.app.open_files(&[gio::File::for_path(&missing)]);
    let windows = f.editor_windows();
    let named = windows.last().unwrap();
    assert_eq!(named.document.borrow().path.as_deref(), Some(missing.as_path()));
    assert_eq!(named.title_label.text(), "new-name.txt");
    assert!(!missing.exists());
}

fn test_new_opens_a_window_and_never_replaces_the_document(f: &Fixture) {
    f.window.buffer.set_text("unsaved scratch");
    let before = f.editor_windows().len();
    WidgetExt::activate_action(&f.window.window, "win.new", None).unwrap();
    settle(0.1);
    assert!(f.window.dialog.borrow().is_none());
    assert_eq!(f.window.text(), "unsaved scratch");
    assert_eq!(f.editor_windows().len(), before + 1);
    assert_eq!(f.app.app.accels_for_action("win.new-window")[0], "<Control>n");
}

fn test_drafts_are_written_after_edits_and_removed_on_save_or_discard(f: &Fixture) {
    use std::os::unix::fs::PermissionsExt;
    let folder = recovery::recovery_dir();
    let w = &f.window;
    w.draft_delay.set(0.05);
    w.buffer.set_text("draft text");
    settle(0.3);
    f.app.drafts.wait();
    let draft = folder.join(format!("{}.json", w.draft_id.borrow()));
    assert!(draft.exists());
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&draft).unwrap()).unwrap();
    assert_eq!(value["text"], "draft text");
    assert_eq!(std::fs::metadata(&draft).unwrap().permissions().mode() & 0o777, 0o600);
    let path = f.root.join("saved.txt");
    w.save_to(Some(Target::Path(path.clone())), None, true);
    wait_idle(w);
    f.app.drafts.wait();
    assert!(!draft.exists());
    w.buffer.insert_at_cursor(" more");
    settle(0.3);
    f.app.drafts.wait();
    let draft = folder.join(format!("{}.json", w.draft_id.borrow()));
    assert!(draft.exists());
    w.guard(Box::new(|w| w.finish_close()));
    let dialog = w.dialog.borrow().clone().unwrap();
    buttons(&dialog)["Discard"].emit_clicked();
    settle(0.1);
    f.app.drafts.wait();
    assert!(!draft.exists());
}

fn test_destroy_without_guard_keeps_the_draft(f: &Fixture) {
    let w = &f.window;
    w.buffer.set_text("crash me");
    let draft = recovery::recovery_dir().join(format!("{}.json", w.draft_id.borrow()));
    w.window.destroy();
    settle(0.1);
    assert!(w.closing.get());
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&draft).unwrap()).unwrap();
    assert_eq!(value["text"], "crash me");
    assert_eq!(value["location"], serde_json::Value::Null);
    std::fs::remove_file(&draft).unwrap();
}

fn test_recovered_drafts_are_offered_and_restored(f: &Fixture) {
    let target = f.root.join("notes.txt");
    std::fs::write(&target, "on disk\n").unwrap();
    let (first, second) = (recovery::new_identifier(), recovery::new_identifier());
    recovery::write(&first, "recovered edit\n", Some(&target.to_string_lossy()), "utf-8", "LF", None).unwrap();
    recovery::write(&second, "scratch idea", None, "utf-8", "LF", None).unwrap();
    for identifier in [&first, &second] {
        // Pretend a crashed process wrote them.
        let path = recovery::recovery_dir().join(format!("{identifier}.json"));
        let mut value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        value["pid"] = 0.into();
        std::fs::write(&path, value.to_string()).unwrap();
    }
    f.app.recovered.replace(Some(recovery::pending(None)));
    f.app.recovery_offered.set(false);
    f.app.offer_recovery(Some(&f.window));
    assert!(f.window.recovery_bar.is_visible());
    assert_eq!(f.window.recovery_label.text(), "Recovered unsaved documents: 2 from a previous session can be restored.");
    f.window.recovery_restore.emit_clicked();
    for window in f.editor_windows() {
        wait_idle(&window);
    }
    let texts: HashMap<String, EditorWindow> = f.editor_windows().into_iter().map(|w| (w.text(), w)).collect();
    assert!(texts.contains_key("recovered edit\n"));
    assert!(texts.contains_key("scratch idea"));
    let restored = &texts["recovered edit\n"];
    assert_eq!(restored.document.borrow().path.as_deref(), Some(target.as_path()));
    assert!(restored.modified());
    assert_eq!(*restored.draft_id.borrow(), first);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "on disk\n");
    restored.save(None);
    wait_idle(restored);
    f.app.drafts.wait();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "recovered edit\n");
    assert!(!recovery::recovery_dir().join(format!("{first}.json")).exists());
    texts["scratch idea"].finish_close();
    f.app.drafts.wait();
    assert_eq!(recovery::pending(None), vec![]);
}

fn test_gio_file_open_and_set_default_text_editor(f: &Fixture) {
    let path = f.root.join("remote-like.toml");
    std::fs::write(&path, "key = 1\n").unwrap();
    f.window.load(Target::File(gio::File::for_uri(&gio::File::for_path(&path).uri())), None);
    wait_idle(&f.window);
    assert_eq!(f.window.text(), "key = 1\n");
    let applications = PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap()).join("applications");
    std::fs::create_dir_all(&applications).unwrap();
    let desktop = std::fs::read_to_string("/home/tcrider/tessel-bluetooth/tessel-text-editor/org.tessel.TextEditor.desktop").unwrap();
    std::fs::write(applications.join("org.tessel.TextEditor.desktop"), desktop.replace("Exec=tessel-text-editor", "Exec=true")).unwrap();
    WidgetExt::activate_action(&f.window.window, "win.set-default", None).unwrap();
    settle(0.1);
    let mimeapps = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("mimeapps.list");
    assert!(std::fs::read_to_string(mimeapps).unwrap().contains("text/plain=org.tessel.TextEditor.desktop"));
    assert!(f.window.banner.is_visible());
    assert_eq!(f.window.banner_label.text(), "Text Editor is now the default application for text files.");
}

fn test_non_utf8_asks_for_an_encoding(f: &Fixture) {
    let path = f.root.join("latin.txt");
    std::fs::write(&path, b"caf\xe9\n").unwrap();
    f.window.load(Target::Path(path.clone()), None);
    wait_idle(&f.window);
    let dialog = f.app.app.windows().into_iter().find(|w| w.title().as_deref() == Some("Choose File Encoding")).expect("encoding dialog");
    let drop = descendants(dialog.upcast_ref()).into_iter().find_map(|w| w.downcast::<gtk::DropDown>().ok()).unwrap();
    drop.set_selected(6); // windows-1252
    buttons(&dialog)["Open"].emit_clicked();
    wait_idle(&f.window);
    assert_eq!(f.window.text(), "café\n");
    assert_eq!(*f.window.encoding.borrow(), "cp1252");
    assert!(f.window.details.text().starts_with("CP1252  ·  LF"));
    // Encoding/line-ending changes mark the document modified until saved.
    f.window.format(Some("utf-8"), Some("CRLF"));
    assert!(f.window.modified());
    f.window.save(None);
    wait_idle(&f.window);
    assert_eq!(std::fs::read(&path).unwrap(), "café\r\n".as_bytes());
    assert!(!f.window.modified());
}

fn test_view_toggles_persist_preferences(f: &Fixture) {
    WidgetExt::activate_action(&f.window.window, "win.wrap", None).unwrap();
    assert_eq!(f.window.view.wrap_mode(), gtk::WrapMode::None);
    let config = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tessel/tessel-text-editor.json");
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(value["wrap"], false);
    assert!(value["recent"].is_array());
    WidgetExt::activate_action(&f.window.window, "win.wrap", None).unwrap();
    assert_eq!(f.window.view.wrap_mode(), gtk::WrapMode::WordChar);
}

fn isolated() -> bool {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    std::env::var("TEXT_EDITOR_UI_ISOLATED_TEST").as_deref() == Ok("1")
        && runtime != "/run/user/1000" && !runtime.is_empty()
        && std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| !d.is_empty() && !d.starts_with('/'))
        && ["XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"].iter()
            .all(|k| std::env::var(k).is_ok_and(|v| v.starts_with("/run/user/1000/cbt/") && !v.starts_with(&home)))
}

fn main() {
    if !isolated() {
        println!("ui_headless: skipped (run tests/run_headless.sh: needs a private headless labwc)");
        return;
    }
    gtk::init().expect("GTK");
    tessel_core::i18n::initialize();
    let app = EditorApplication::new();
    app.app.register(None::<&gio::Cancellable>).expect("register on the private bus");
    settle(0.2);
    let checks: Vec<(&str, Check)> = vec![
        ("test_standard_app_menu_quit_and_new_window_shortcuts", test_standard_app_menu_quit_and_new_window_shortcuts),
        ("test_primary_menu_holds_former_menubar_entries", test_primary_menu_holds_former_menubar_entries),
        ("test_load_edit_save_and_single_document_window", test_load_edit_save_and_single_document_window),
        ("test_unsaved_cancel_discard_and_failed_save_guard", test_unsaved_cancel_discard_and_failed_save_guard),
        ("test_external_conflict_preserves_both_versions", test_external_conflict_preserves_both_versions),
        ("test_external_change_is_reported_by_the_monitor", test_external_change_is_reported_by_the_monitor),
        ("test_search_case_whole_word_regex_replace_and_undo", test_search_case_whole_word_regex_replace_and_undo),
        ("test_bookmarks_preferences_and_print_export", test_bookmarks_preferences_and_print_export),
        ("test_preferences_alone", test_preferences_alone),
        ("test_vim_write_uses_guarded_save", test_vim_write_uses_guarded_save),
        ("test_plain_modal_lifecycle", test_plain_modal_lifecycle),
        ("test_document_surface_keeps_neutral_colors_inside_wallpaper_theme", test_document_surface_keeps_neutral_colors_inside_wallpaper_theme),
        ("test_document_scheme_follows_gtk_light_dark_without_a_dock_palette", test_document_scheme_follows_gtk_light_dark_without_a_dock_palette),
        ("test_multiple_cli_files_get_separate_windows_and_reuse_existing", test_multiple_cli_files_get_separate_windows_and_reuse_existing),
        ("test_new_opens_a_window_and_never_replaces_the_document", test_new_opens_a_window_and_never_replaces_the_document),
        ("test_drafts_are_written_after_edits_and_removed_on_save_or_discard", test_drafts_are_written_after_edits_and_removed_on_save_or_discard),
        ("test_destroy_without_guard_keeps_the_draft", test_destroy_without_guard_keeps_the_draft),
        ("test_recovered_drafts_are_offered_and_restored", test_recovered_drafts_are_offered_and_restored),
        ("test_gio_file_open_and_set_default_text_editor", test_gio_file_open_and_set_default_text_editor),
        ("test_non_utf8_asks_for_an_encoding", test_non_utf8_asks_for_an_encoding),
        ("test_view_toggles_persist_preferences", test_view_toggles_persist_preferences),
    ];
    let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let (mut passed, mut failed) = (0, Vec::new());
    for (index, (name, check)) in checks.iter().enumerate() {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let root = std::env::temp_dir().join(format!("te-ui-{index}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let window = EditorWindow::new(&app);
        window.present();
        settle(0.1);
        let fixture = Fixture { app: app.clone(), window, root: std::fs::canonicalize(&root).unwrap() };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&fixture)));
        // tearDown
        for window in app.editor_windows() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait_idle(&window)));
            window.finish_close();
        }
        for window in app.app.windows() {
            window.destroy();
        }
        settle(0.1);
        let _ = std::fs::remove_dir_all(&root);
        match result {
            Ok(()) => {
                passed += 1;
                println!("ok   {name}");
            }
            Err(error) => {
                let message = error.downcast_ref::<String>().cloned()
                    .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                println!("FAIL {name}: {message}");
                failed.push(*name);
            }
        }
    }
    println!("ui_headless: {passed} passed, {} failed {:?}", failed.len(), failed);
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
