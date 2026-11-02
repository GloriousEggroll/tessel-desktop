//! Widget tests ported from ~/tessel-bluetooth/shared/test_wallpaper_chooser.py.
//! Run ONLY inside the private nested headless labwc
//! (tests/run_headless_fileops.sh sets WAYSHELL_FILEOPS_ISOLATED=1); under a
//! plain `cargo test` this binary skips itself. All media are temporary; the
//! fake pkexec never elevates. GTK lives on one thread, so the cases run
//! sequentially inside a single #[test].
#![cfg(feature = "fileops")]

use std::cell::RefCell;
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
use gtk::prelude::*;
use gtk::{gio, glib};
use tessel_shell::wallpaper_chooser::{self as gallery, Importer, Options, WallpaperChooser};

fn write_png(path: &Path, width: i32, height: i32) {
    let pixbuf = Pixbuf::new(Colorspace::Rgb, true, 8, width, height).unwrap();
    pixbuf.fill(0x334c99ff);
    pixbuf.savev(path, "png", &[]).unwrap();
}

fn settle(duration: f64) {
    tessel_core::timer::settle(&glib::MainContext::default(), Duration::from_secs_f64(duration));
}

fn wait_for(what: &str, mut predicate: impl FnMut() -> bool) {
    let ok = tessel_core::timer::wait_for(&glib::MainContext::default(), Duration::from_secs(4), &mut predicate);
    assert!(ok, "timed out waiting for {what}");
}

struct Case {
    root: PathBuf,
    folder: PathBuf,
    nested: PathBuf,
    image: PathBuf,
    parent: gtk::Window,
}

impl Case {
    fn new(n: usize) -> Case {
        let root = std::env::temp_dir().join(format!("wallpaper-ui-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("Wallpapers");
        let nested = folder.join("Another Folder");
        fs::create_dir_all(&nested).unwrap();
        let image = folder.join("Wallpaper.png");
        write_png(&image, 640, 360);
        fs::copy(&image, nested.join("Alternate.PNG")).unwrap();
        let parent = gtk::Window::new();
        parent.present();
        settle(0.08);
        Case { root, folder, nested, image, parent }
    }

    fn picker(&self) -> (WallpaperChooser, Rc<RefCell<Vec<Option<PathBuf>>>>) {
        self.picker_with(Options::default())
    }

    fn picker_with(&self, mut options: Options) -> (WallpaperChooser, Rc<RefCell<Vec<Option<PathBuf>>>>) {
        let results: Rc<RefCell<Vec<Option<PathBuf>>>> = Rc::default();
        let r = results.clone();
        options.default_folder = Some(self.folder.clone());
        let picker = WallpaperChooser::with_options(Some(&self.parent), move |p| r.borrow_mut().push(p), options);
        picker.present();
        (picker, results)
    }

    fn choose(&self, picker: &WallpaperChooser, name: &str) {
        wait_for("listing", || !picker.is_loading());
        let index = picker.paths().iter().position(|p| p.file_name().unwrap() == name).expect("tile");
        picker.selection().set_selected(index as u32);
        picker.select_button().emit_clicked();
        settle(0.08);
    }

    fn deletion_picker(&self) -> (WallpaperChooser, Rc<RefCell<Vec<Option<PathBuf>>>>) {
        let (picker, results) = self.picker();
        wait_for("bound tile", || !picker.is_loading() && picker.tile(&self.image).is_some());
        wait_for("preview cache", || !picker.cache_paths().is_empty());
        (picker, results)
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        self.parent.destroy();
        settle(0.05);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fake_pkexec(root: &Path, code: Option<i32>) -> PathBuf {
    let script = root.join("fake-pkexec");
    let tail = code.map(|c| format!("echo denied >&2; exit {c}")).unwrap_or_else(|| "shift\nexec \"$@\"".into());
    fs::write(&script, format!("#!/bin/sh\n{tail}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

// ---------------------------------------------------------------- cases

fn add_copies_external_wallpaper_and_selects_copy(c: &Case) {
    let source = c.root.join("Imported wallpaper.PNG");
    fs::copy(&c.image, &source).unwrap();
    let (picker, results) = c.picker();
    // Navigating elsewhere must not change the import collection.
    picker.navigate(&c.nested, None, None);
    picker.add_button().emit_clicked();
    let dialog = picker.browse_dialog().expect("dialog");
    assert!(dialog.is_multiple());
    assert_eq!(dialog.accept_button().label().unwrap(), "Add");
    dialog.navigate(&c.root);
    wait_for("dialog listing", || !dialog.is_loading());
    dialog.open_files(vec![gio::File::for_path(&source)]);
    wait_for("closed", || picker.is_closed());
    let target = c.folder.join(source.file_name().unwrap());
    assert_eq!(*results.borrow(), vec![Some(target.clone())]);
    assert_eq!(fs::read(&target).unwrap(), fs::read(&source).unwrap());
    assert!(source.is_file());
    assert!(!c.nested.join(source.file_name().unwrap()).exists());
    picker.close();
    assert_eq!(results.borrow().len(), 1);
}

fn add_rejects_invalid_image_and_remains_usable(c: &Case) {
    let source = c.root.join("Broken.png");
    fs::write(&source, b"not an image").unwrap();
    let (picker, results) = c.picker();
    wait_for("listing", || !picker.is_loading());
    picker.start_import(vec![source.clone()]);
    wait_for("import", || !picker.is_import_busy());
    assert!(picker.message().text().contains("Could not add wallpaper"), "{}", picker.message().text());
    assert!(picker.add_button().is_sensitive());
    assert!(!c.folder.join("Broken.png").exists());
    assert!(results.borrow().is_empty());
    c.choose(&picker, "Wallpaper.png");
    assert_eq!(*results.borrow(), vec![Some(c.image.clone())]);
}

fn add_multiple_files_keeps_gallery_open_and_preserves_existing_files(c: &Case) {
    let sources = [c.root.join("First.png"), c.root.join("Second.PNG")];
    for s in &sources {
        fs::copy(&c.image, s).unwrap();
    }
    let existing = c.folder.join("First.png");
    fs::copy(&c.image, &existing).unwrap();
    let (picker, results) = c.picker();
    picker.add_button().emit_clicked();
    let dialog = picker.browse_dialog().unwrap();
    dialog.navigate(&c.root);
    wait_for("dialog listing", || !dialog.is_loading());
    assert!(dialog.selection().is::<gtk::MultiSelection>());
    for s in &sources {
        assert!(dialog.select_name(s.file_name().unwrap().to_str().unwrap(), false));
    }
    assert_eq!(dialog.selected().len(), 2);
    dialog.accept_button().emit_clicked();
    wait_for("import + reload", || dialog.is_closed() && !picker.is_import_busy() && !picker.is_loading());
    assert!(!picker.is_closed());
    assert!(results.borrow().is_empty());
    let targets = [c.folder.join("First (2).png"), c.folder.join("Second.PNG")];
    for (s, t) in sources.iter().zip(&targets) {
        assert!(s.is_file());
        assert_eq!(fs::read(t).unwrap(), fs::read(s).unwrap());
    }
    assert_eq!(fs::read(&existing).unwrap(), fs::read(&c.image).unwrap());
    assert!(picker.message().text().contains("Added 2 wallpapers"), "{}", picker.message().text());
    assert_eq!(picker.selected_path(), Some(targets[0].clone()));
    c.choose(&picker, "Second.PNG");
    assert_eq!(*results.borrow(), vec![Some(targets[1].clone())]);
}

fn add_multiple_validation_error_allows_correcting_selection(c: &Case) {
    let broken = c.root.join("Broken.png");
    fs::write(&broken, b"not an image").unwrap();
    let source = c.root.join("Good.png");
    fs::copy(&c.image, &source).unwrap();
    let (picker, results) = c.picker();
    picker.add_button().emit_clicked();
    let dialog = picker.browse_dialog().unwrap();
    dialog.open_files(vec![gio::File::for_path(&source), gio::File::for_path(&broken)]);
    wait_for("validation", || !dialog.is_busy());
    assert!(!dialog.is_closed());
    assert!(dialog.status().text().contains("Broken.png"), "{}", dialog.status().text());
    assert!(!c.folder.join("Good.png").exists());
    assert!(results.borrow().is_empty());
    dialog.open_files(vec![gio::File::for_path(&source)]);
    wait_for("closed", || picker.is_closed());
    assert_eq!(*results.borrow(), vec![Some(c.folder.join("Good.png"))]);
}

fn batch_import_reports_partial_failure_and_continues(c: &Case) {
    let sources: Vec<PathBuf> = ["First.png", "Broken.png", "Last.png"].iter().map(|n| c.root.join(n)).collect();
    for s in &sources {
        fs::copy(&c.image, s).unwrap();
    }
    let validate: gallery::Validator = Arc::new(|p: &Path| {
        if p.ends_with("Broken.png") {
            Err("Source disappeared".to_string())
        } else {
            gallery::validate_media(p)
        }
    });
    let (picker, results) = c.picker_with(Options { validate: Some(validate), ..Options::default() });
    wait_for("listing", || !picker.is_loading());
    picker.start_import(sources);
    wait_for("import", || !picker.is_import_busy() && !picker.is_loading());
    assert!(results.borrow().is_empty());
    assert!(!picker.is_closed());
    assert!(c.folder.join("First.png").is_file());
    assert!(c.folder.join("Last.png").is_file());
    assert!(!c.folder.join("Broken.png").exists());
    let text = picker.message().text();
    assert!(text.contains("Added 2 wallpapers"), "{text}");
    assert!(text.contains("Broken.png: Source disappeared"), "{text}");
    assert!(picker.add_button().is_sensitive());
}

fn browse_uses_navigation_and_filters(c: &Case) {
    fs::write(c.folder.join("notes.txt"), "not a wallpaper").unwrap();
    let (picker, results) = c.picker();
    picker.browse(false);
    let dialog = picker.browse_dialog().unwrap();
    assert!(dialog.selection().is::<gtk::SingleSelection>());
    assert!(dialog.window().has_css_class("files-window"));
    wait_for("dialog listing", || !dialog.is_loading());
    assert_eq!(dialog.visible_names(), vec!["Another Folder", "Wallpaper.png"]);
    dialog.type_filter().set_selected(2);
    assert_eq!(dialog.visible_names().len(), 1, "Folders remain navigable in video-only view");
    dialog.type_filter().set_selected(0);
    dialog.navigate(&c.nested);
    wait_for("nested", || !dialog.is_loading());
    assert_eq!(dialog.visible_names()[0], "Alternate.PNG");
    dialog.back_button().emit_clicked();
    wait_for("back", || !dialog.is_loading());
    assert_eq!(dialog.current(), c.folder);
    assert!(dialog.select_name("Wallpaper.png", true));
    dialog.accept_button().emit_clicked();
    wait_for("closed", || picker.is_closed());
    assert_eq!(*results.borrow(), vec![Some(c.image.clone())]);
}

fn browse_cancel_and_parent_close_never_select_media(c: &Case) {
    let (picker, results) = c.picker();
    picker.browse(false);
    let dialog = picker.browse_dialog().unwrap();
    dialog.close(); // Escape
    assert!(picker.browse_dialog().is_none());
    assert!(results.borrow().is_empty());
    picker.browse(false);
    let dialog = picker.browse_dialog().unwrap();
    picker.close();
    assert!(dialog.is_closed());
    assert_eq!(*results.borrow(), vec![None]);
}

fn trash_confirmation_cancel_preserves_file_and_selection(c: &Case) {
    let (picker, results) = c.deletion_picker();
    let tile = picker.tile(&c.image).unwrap();
    assert_eq!(tile.remove_button.halign(), gtk::Align::Start);
    assert_eq!(tile.remove_button.valign(), gtk::Align::End);
    assert_eq!(tile.remove_button.icon_name().unwrap(), "user-trash-symbolic");
    tile.remove_button.emit_clicked();
    let dialog = picker.removal_dialog().expect("confirmation");
    assert!(dialog.message.text().contains("Wallpaper.png"));
    assert!(dialog.window.titlebar().and_downcast::<gtk::HeaderBar>().is_some());
    assert_eq!(dialog.window.transient_for().as_ref(), Some(picker.window()));
    assert!(dialog.window.is_modal());
    assert_eq!(dialog.window.default_widget().as_ref(), Some(dialog.cancel_button.upcast_ref::<gtk::Widget>()));
    assert!(picker.selected_path().is_none(), "Trash must not choose a wallpaper");
    assert!(!picker.select_button().is_sensitive());
    dialog.response(gtk::ResponseType::Cancel);
    settle(0.08);
    assert!(picker.removal_dialog().is_none());
    assert!(c.image.is_file());
    assert_eq!(picker.model().n_items(), 1);
    assert!(results.borrow().is_empty());
}

fn confirmed_removal_updates_gallery_and_clears_deleted_selection(c: &Case) {
    let (picker, results) = c.deletion_picker();
    picker.selection().set_selected(0);
    assert!(picker.select_button().is_sensitive());
    picker.tile(&c.image).unwrap().remove_button.emit_clicked();
    picker.removal_dialog().unwrap().response(gtk::ResponseType::Accept);
    assert!(!picker.select_button().is_sensitive());
    wait_for("removal", || !picker.is_removal_busy());
    assert!(!c.image.exists());
    assert_eq!(picker.model().n_items(), 0);
    assert!(picker.selected_path().is_none());
    assert!(!picker.select_button().is_sensitive());
    assert!(!picker.cache_paths().contains(&c.image));
    assert!(results.borrow().is_empty());
    assert!(!picker.is_closed());
}

fn remove_other_tile_keeps_existing_selection(c: &Case) {
    let other = c.folder.join("Second.png");
    fs::copy(&c.image, &other).unwrap();
    let (picker, results) = c.deletion_picker();
    let index = picker.paths().iter().position(|p| *p == other).unwrap();
    picker.selection().set_selected(index as u32);
    picker.tile(&c.image).unwrap().remove_button.emit_clicked();
    picker.removal_dialog().unwrap().response(gtk::ResponseType::Accept);
    wait_for("removal", || !picker.is_removal_busy());
    assert_eq!(picker.selected_path(), Some(other));
    assert!(picker.select_button().is_sensitive());
    assert!(results.borrow().is_empty());
}

fn removal_auth_cancel_and_failure_leave_picker_usable(c: &Case) {
    let (picker, results) = c.deletion_picker();
    picker.selection().set_selected(0);
    for (code, message) in [(126, "not removed"), (1, "denied")] {
        picker.set_importer(Importer {
            system_folders: vec![c.folder.clone()],
            pkexec: fake_pkexec(&c.root, Some(code)),
            deny_unprivileged: true,
            ..Importer::default()
        });
        picker.tile(&c.image).unwrap().remove_button.emit_clicked();
        picker.removal_dialog().unwrap().response(gtk::ResponseType::Accept);
        wait_for("removal", || !picker.is_removal_busy());
        assert!(picker.message().text().contains(message), "{}", picker.message().text());
        assert!(picker.navigation().is_sensitive());
        assert!(picker.grid().is_sensitive());
        assert!(picker.select_button().is_sensitive());
        assert!(c.image.is_file());
        assert_eq!(picker.model().n_items(), 1);
        assert!(results.borrow().is_empty());
    }
}

fn closing_picker_with_confirmation_never_deletes(c: &Case) {
    let (picker, results) = c.deletion_picker();
    picker.tile(&c.image).unwrap().remove_button.emit_clicked();
    assert!(picker.removal_dialog().is_some());
    picker.close();
    settle(0.08);
    assert!(c.image.is_file());
    assert!(picker.removal_dialog().is_none());
    assert_eq!(*results.borrow(), vec![None]);
}

fn removal_prompt_escape_closes_only_prompt_and_close_never_confirms(c: &Case) {
    let (picker, results) = c.deletion_picker();
    picker.tile(&c.image).unwrap().remove_button.emit_clicked();
    let dialog = picker.removal_dialog().unwrap();
    assert_eq!(dialog.key_pressed(gtk::gdk::Key::Escape), glib::Propagation::Stop);
    assert!(picker.removal_dialog().is_none());
    assert!(!picker.is_closed());
    assert!(c.image.is_file());
    picker.tile(&c.image).unwrap().remove_button.emit_clicked();
    picker.removal_dialog().unwrap().close();
    settle(0.08);
    assert!(picker.removal_dialog().is_none());
    assert!(!picker.is_closed());
    assert!(c.image.is_file());
    assert!(results.borrow().is_empty());
}

fn preview_frame_keeps_trash_inset(c: &Case) {
    for (name, w, h) in [("Portrait", 120, 480), ("Square", 240, 240), ("Wide", 800, 100)] {
        write_png(&c.folder.join(format!("{name}.png")), w, h);
    }
    let (picker, _results) = c.deletion_picker();
    wait_for("4 previews", || picker.cache_paths().len() == 4);
    let mut column_widths = Vec::new();
    for window_width in [800, 1200] {
        picker.window().set_default_size(window_width, 610);
        wait_for("resize", || picker.window().width() == window_width);
        settle(0.15);
        for path in picker.bound_paths() {
            let tile = picker.tile(&path).unwrap();
            assert_eq!(tile.frame.width(), gallery::PREVIEW_WIDTH);
            assert_eq!(tile.frame.height(), gallery::PREVIEW_HEIGHT);
            assert!(tile.picture.paintable().is_some());
            assert_eq!(tile.picture.content_fit(), gtk::ContentFit::Contain);
            let bounds = tile.frame.compute_bounds(&tile.child).unwrap();
            assert!((bounds.x() - (tile.child.width() - gallery::PREVIEW_WIDTH) as f32 / 2.0).abs() <= 1.0);
            let button = tile.remove_button.compute_bounds(&tile.frame).unwrap();
            assert!(button.x() >= 6.0);
            assert!(button.y() >= 0.0);
            assert!(button.x() + button.width() <= tile.frame.width() as f32 - 6.0);
            assert!((button.y() + button.height() - (tile.frame.height() - 6) as f32).abs() <= 1.0);
        }
        column_widths.push(picker.tile(&picker.bound_paths()[0]).unwrap().child.width());
    }
    assert_ne!(column_widths[0], column_widths[1], "Exercise different allocated column widths");
}

fn large_previews_load_on_scroll_and_folder_navigation_cancels_once(c: &Case) {
    for index in 0..100 {
        fs::copy(&c.image, c.folder.join(format!("Fixture {index:03}.png"))).unwrap();
    }
    let (picker, results) = c.picker();
    wait_for("101 tiles", || picker.model().n_items() == 101);
    wait_for("some previews", || !picker.cache_paths().is_empty());
    assert!(picker.cache_paths().len() < 40, "{}", picker.cache_paths().len());
    let first: std::collections::HashSet<PathBuf> = picker.cache_paths().into_iter().collect();
    for path in picker.bound_paths() {
        let tile = picker.tile(&path).unwrap();
        if tile.picture.paintable().is_some() {
            assert!(tile.stack.width() >= 224 && tile.stack.height() >= 126);
        }
    }
    let adjustment = picker.scroll().vadjustment();
    adjustment.set_value(adjustment.upper() - adjustment.page_size());
    wait_for("last tile preview", || picker.cache_paths().contains(&c.image));
    assert!(picker.cache_paths().iter().any(|p| !first.contains(p)));
    assert!(picker.cache_paths().len() <= gallery::CACHE_LIMIT);
    assert!(picker.pending_len() <= gallery::PENDING_LIMIT);
    picker.location().set_text(&c.nested.to_string_lossy());
    picker.location().emit_activate();
    assert_eq!(picker.folder(), c.nested);
    assert!(results.borrow().is_empty());
    wait_for("nested", || picker.model().n_items() == 1);
    assert_eq!(picker.paths()[0].file_name().unwrap(), "Alternate.PNG");
    picker.up_button().emit_clicked();
    wait_for("back to 101", || picker.model().n_items() == 101);
    picker.close();
    picker.close();
    c.parent.destroy();
    settle(0.08);
    assert_eq!(*results.borrow(), vec![None]);
}

fn parent_unrealize_and_cancellable_finish_once(c: &Case) {
    let (picker, results) = c.picker();
    c.parent.destroy();
    settle(0.08);
    picker.close();
    assert_eq!(*results.borrow(), vec![None]);
    assert!(picker.is_closed());
    let parent = gtk::Window::new();
    parent.present();
    let cancellable = gio::Cancellable::new();
    let r: Rc<RefCell<Vec<Option<PathBuf>>>> = Rc::default();
    let rr = r.clone();
    let picker = WallpaperChooser::with_options(
        Some(&parent),
        move |p| rr.borrow_mut().push(p),
        Options { default_folder: Some(c.folder.clone()), cancellable: Some(cancellable.clone()), ..Options::default() },
    );
    picker.present();
    cancellable.cancel();
    wait_for("cancel", || picker.is_closed());
    assert_eq!(*r.borrow(), vec![None]);
    parent.destroy();
}

fn staged_selection_like_tessel_shell(c: &Case) {
    // DesktopView.choose_image_for_monitor: one chooser at a time, the result
    // only stages the path (nothing applied).
    let staged: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let current: Rc<RefCell<Option<WallpaperChooser>>> = Rc::default();
    let open = {
        let staged = staged.clone();
        let current = current.clone();
        let parent = c.parent.clone();
        let folder = c.folder.clone();
        move || {
            if let Some(existing) = current.borrow().as_ref() {
                existing.present();
                return;
            }
            let (s, cur) = (staged.clone(), current.clone());
            let picker = WallpaperChooser::with_options(
                Some(&parent),
                move |path| {
                    cur.borrow_mut().take();
                    if let Some(path) = path {
                        *s.borrow_mut() = Some(path);
                    }
                },
                Options { default_folder: Some(folder.clone()), ..Options::default() },
            );
            picker.present();
            *current.borrow_mut() = Some(picker);
        }
    };
    open();
    let picker = current.borrow().clone().unwrap();
    open();
    assert!(current.borrow().as_ref().is_some_and(|p| p.window() == picker.window()));
    c.choose(&picker, "Wallpaper.png");
    picker.close();
    assert!(current.borrow().is_none());
    assert_eq!(*staged.borrow(), Some(c.image.clone()));
    open();
    let again = current.borrow().clone().unwrap();
    again.close();
    assert!(current.borrow().is_none());
    assert_eq!(*staged.borrow(), Some(c.image.clone()));
}

#[test]
fn wallpaper_chooser_widgets() {
    if std::env::var("WAYSHELL_FILEOPS_ISOLATED").as_deref() != Ok("1") {
        eprintln!("skipped: requires tests/run_headless_fileops.sh (private headless labwc)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(!runtime.is_empty() && runtime != "/run/user/1000", "never the live session");
    tessel_core::i18n::initialize();
    gtk::init().unwrap();
    let cases: Vec<(&str, fn(&Case))> = vec![
        ("add_copies_external_wallpaper_and_selects_copy", add_copies_external_wallpaper_and_selects_copy),
        ("add_rejects_invalid_image_and_remains_usable", add_rejects_invalid_image_and_remains_usable),
        ("add_multiple_files_keeps_gallery_open", add_multiple_files_keeps_gallery_open_and_preserves_existing_files),
        ("add_multiple_validation_error_allows_correcting", add_multiple_validation_error_allows_correcting_selection),
        ("batch_import_reports_partial_failure", batch_import_reports_partial_failure_and_continues),
        ("browse_uses_navigation_and_filters", browse_uses_navigation_and_filters),
        ("browse_cancel_and_parent_close_never_select", browse_cancel_and_parent_close_never_select_media),
        ("trash_confirmation_cancel_preserves_file", trash_confirmation_cancel_preserves_file_and_selection),
        ("confirmed_removal_updates_gallery", confirmed_removal_updates_gallery_and_clears_deleted_selection),
        ("remove_other_tile_keeps_selection", remove_other_tile_keeps_existing_selection),
        ("removal_auth_cancel_and_failure", removal_auth_cancel_and_failure_leave_picker_usable),
        ("closing_picker_with_confirmation_never_deletes", closing_picker_with_confirmation_never_deletes),
        ("removal_prompt_escape", removal_prompt_escape_closes_only_prompt_and_close_never_confirms),
        ("preview_frame_keeps_trash_inset", preview_frame_keeps_trash_inset),
        ("large_previews_load_on_scroll", large_previews_load_on_scroll_and_folder_navigation_cancels_once),
        ("parent_unrealize_and_cancellable", parent_unrealize_and_cancellable_finish_once),
        ("staged_selection_like_tessel_shell", staged_selection_like_tessel_shell),
    ];
    let mut failures = Vec::new();
    for (index, (name, case)) in cases.into_iter().enumerate() {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let c = Case::new(index);
            case(&c);
        }));
        match outcome {
            Ok(()) => println!("ok   {name}"),
            Err(error) => {
                let text = error.downcast_ref::<String>().cloned().or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                println!("FAIL {name}: {text}");
                failures.push(name);
            }
        }
        settle(0.05);
    }
    assert!(failures.is_empty(), "failed: {failures:?}");
}
