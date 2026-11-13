//! Headless GTK checks for tessel-files-widgets: ports of test_file_dialogs.py,
//! test_open_with.py and the standalone-widget parts of test_files_information.py,
//! test_files_browsing.py and test_files_devices.py (DiskUsage), plus job widgets.
//!
//! Runs only inside the private nested headless labwc started by
//! tests/run_headless.sh (private D-Bus without service activation, private HOME
//! and XDG dirs, private desktop entries, no portals, local VFS). Under plain
//! `cargo test` it skips itself. Every fixture lives under $TMPDIR (private).

use tessel_files_widgets::compat::{Item, SimpleItem};
use tessel_files_widgets::dialogs::{self, ChooserMode, OpenWithDialog, OpenWithOptions};
use tessel_files_widgets::information::{self, InformationPane, IMAGE};
use tessel_files_widgets::jobs::{self, JobManager, JobsButton};
use tessel_files_widgets::quicklook::{QuickLook, QuickLookPane};
use tessel_files_widgets::{devices::{self, DiskUsage}, poppler, print, style};
use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib};
use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Check = Result<(), String>;

macro_rules! check {
    ($cond:expr) => { if !$cond { return Err(format!("line {}: {}", line!(), stringify!($cond))); } };
    ($cond:expr, $($fmt:tt)+) => { if !$cond { return Err(format!("line {}: {}", line!(), format!($($fmt)+))); } };
}

macro_rules! eq {
    ($a:expr, $b:expr) => {{
        let (a, b) = (&$a, &$b);
        if a != b { return Err(format!("line {}: {:?} != {:?}", line!(), a, b)); }
    }};
}

fn pump(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait(timeout: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    predicate()
}

fn fixture(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("fw-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn item(path: &Path) -> Item {
    SimpleItem::for_path(path).expect("fixture item")
}

/// A small valid PDF with `pages` A4 pages (cairo-rs is built without PDF support).
fn write_pdf(path: &Path, pages: usize) {
    let mut objects: Vec<String> = Vec::new();
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));
    for page in 0..pages {
        let content = format!("BT /F1 40 Tf 80 720 Td (Page {}) Tj ET", page + 1);
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents {} 0 R \
                              /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>", 4 + page * 2));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
    }
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out += &format!("{} 0 obj\n{object}\nendobj\n", index + 1);
    }
    let xref = out.len();
    out += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        out += &format!("{offset:010} 00000 n \n");
    }
    out += &format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1);
    std::fs::write(path, out).unwrap();
}

/// A 2400×1600 JPEG with EXIF orientation, Make/Model and DateTimeOriginal.
fn write_photo(path: &Path, orientation: u16) {
    use exif::{Field, In, Tag, Value};
    let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 2400, 1600).unwrap();
    pixbuf.fill(0x9ac6ddff);
    let jpeg = pixbuf.save_to_bufferv("jpeg", &[]).unwrap();
    let fields = [
        Field { tag: Tag::Orientation, ifd_num: In::PRIMARY, value: Value::Short(vec![orientation]) },
        Field { tag: Tag::Make, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Camera".to_vec()]) },
        Field { tag: Tag::Model, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Example 1".to_vec()]) },
        Field { tag: Tag::DateTimeOriginal, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"2026:09:27 10:30:00".to_vec()]) },
    ];
    let mut writer = exif::experimental::Writer::new();
    for field in &fields {
        writer.push_field(field);
    }
    let mut tiff = std::io::Cursor::new(Vec::new());
    writer.write(&mut tiff, false).unwrap();
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff.into_inner());
    let mut out = jpeg[..2].to_vec();
    out.extend([0xff, 0xe1]);
    out.extend(((app1.len() + 2) as u16).to_be_bytes());
    out.extend(app1);
    out.extend(&jpeg[2..]);
    std::fs::write(path, out).unwrap();
}

struct Env {
    app: gtk::Application,
}

impl Env {
    fn parent(&self) -> gtk::ApplicationWindow {
        gtk::ApplicationWindow::new(&self.app)
    }
}

// ---------------------------------------------------------------- style

fn style_css_parses(_: &Env) -> Check {
    for css in [style::CSS, style::NAVIGATION_CSS] {
        let provider = gtk::CssProvider::new();
        let errors = Rc::new(Cell::new(0));
        let sink = errors.clone();
        provider.connect_parsing_error(move |_, _, error| {
            eprintln!("css: {error}");
            sink.set(sink.get() + 1);
        });
        provider.load_from_string(css);
        eq!(errors.get(), 0);
    }
    check!(style::CSS.starts_with("\n.files-window .files-toolbar { padding: 8px 0; }"));
    check!(style::NAVIGATION_CSS.ends_with("    opacity: 1;\n}\n"));
    Ok(())
}

// ---------------------------------------------------------------- dialogs (test_file_dialogs.py)

fn properties_metadata_permissions_checksums_and_symlinks(env: &Env) -> Check {
    let root = fixture("props");
    let path = root.join("example.txt");
    std::fs::write(&path, "Properties test\n").unwrap();
    let parent = env.parent();
    let window = dialogs::PropertiesWindow::new(Some(parent.upcast_ref()), &[gio::File::for_path(&path)], None, None);
    window.present();
    check!(wait(5.0, || window.hash_button().is_some()), "properties never loaded: {}", window.status.text());
    check!(dialogs::has_companion_theme(window.window()));
    check!(window.size_label().unwrap().text().contains("16 bytes"));
    window.mode().unwrap().set_text("0640");
    window.apply_mode();
    check!(wait(5.0, || !window.busy()));
    eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
    window.calculate_hashes();
    check!(wait(5.0, || window.hash_button().unwrap().is_sensitive()));
    check!(window.hash_result().unwrap().text().contains("SHA256"), "{}", window.hash_result().unwrap().text());
    window.close();
    let link = root.join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let window = dialogs::PropertiesWindow::new(Some(parent.upcast_ref()), &[gio::File::for_path(&link)], None, None);
    window.present();
    check!(wait(5.0, || window.metadata_loaded()));
    check!(window.mode().is_none());
    check!(window.hash_button().is_none());
    window.close();
    parent.destroy();
    Ok(())
}

fn directory_sizes_and_close_cancels_work(env: &Env) -> Check {
    let root = fixture("props-dir");
    std::fs::write(root.join("one"), "123").unwrap();
    std::fs::write(root.join("two"), "4567").unwrap();
    let parent = env.parent();
    let window = dialogs::PropertiesWindow::new(Some(parent.upcast_ref()), &[gio::File::for_path(&root)], None, None);
    window.present();
    check!(wait(5.0, || window.size_label().is_some_and(|l| l.text().contains("2 files"))));
    check!(window.size_label().unwrap().text().contains("7 bytes"));
    check!(window.hash_button().is_none());
    window.close();
    check!(window.cancelled());
    parent.destroy();
    Ok(())
}

fn prompt_close_notifies_once_and_confirm_cancel_is_false(env: &Env) -> Check {
    let parent = env.parent();
    let answers: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = answers.clone();
    let dialog = dialogs::prompt_text(Some(parent.upcast_ref()), "Name", "Choose a name", Some("Example"),
                                      move |v| sink.borrow_mut().push(format!("{v:?}")));
    dialog.close();
    eq!(*answers.borrow(), vec!["None".to_string()]);
    let sink = answers.clone();
    let dialog = dialogs::confirm(Some(parent.upcast_ref()), "Delete?", "Confirm deletion", move |v| sink.borrow_mut().push(format!("{v}")));
    dialog.close();
    eq!(*answers.borrow(), vec!["None".to_string(), "false".to_string()]);
    let error = dialogs::show_error(Some(parent.upcast_ref()), "boom");
    check!(dialogs::has_companion_theme(&error));
    error.close();
    parent.destroy();
    Ok(())
}

fn metadata_edit_is_explicit_and_persists_in_private_fixture(env: &Env) -> Check {
    use tessel_files_ops::extras::MetadataStore;
    let root = fixture("props-notes");
    let path = root.join("notes.txt");
    std::fs::write(&path, "notes").unwrap();
    let store_path = root.join("metadata.json");
    let file = gio::File::for_path(&path);
    let parent = env.parent();
    let changes = Rc::new(Cell::new(0));
    let sink = changes.clone();
    let window = dialogs::PropertiesWindow::new(Some(parent.upcast_ref()), &[file.clone()],
        Some(Rc::new(move || sink.set(sink.get() + 1))), Some(MetadataStore::new(Some(store_path.clone()))));
    window.present();
    check!(wait(5.0, || window.tags_entry().is_some()), "{}", window.status.text());
    let reader = MetadataStore::new(Some(store_path));
    check!(reader.read(&file).tags.is_empty());
    eq!(changes.get(), 0);
    window.tags_entry().unwrap().set_text("work, important");
    window.rating_choice().unwrap().set_selected(4);
    window.comment_text().unwrap().buffer().set_text("Review this document");
    check!(reader.read(&file).tags.is_empty());
    window.apply_annotations();
    check!(wait(5.0, || !window.busy()));
    let stored = reader.read(&file);
    eq!(stored.tags, vec!["work".to_string(), "important".to_string()]);
    eq!(stored.rating, 4);
    eq!(stored.comment, "Review this document".to_string());
    eq!(changes.get(), 1);
    eq!(std::fs::read_to_string(&path).unwrap(), "notes".to_string());
    window.close();
    parent.destroy();
    Ok(())
}

fn explicit_gtk4_file_chooser_and_application_chooser_cancel(env: &Env) -> Check {
    let root = fixture("chooser");
    std::fs::write(root.join("document.txt"), "chooser").unwrap();
    let parent = env.parent();
    let answers: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let sink = answers.clone();
    let dialog = dialogs::choose_file(Some(parent.upcast_ref()), "Save archive",
        move |f| sink.borrow_mut().push(f.and_then(|f| f.path()).map(|p| p.display().to_string())),
        ChooserMode::Save, Some(&gio::File::for_path(&root)), "archive.zip");
    check!(dialogs::has_companion_theme(&dialog.window));
    let target = root.join("archive.zip");
    #[allow(deprecated)]
    let ready = wait(5.0, || dialog.file_chooser.file().and_then(|f| f.path()).as_deref() == Some(target.as_path()));
    check!(ready, "chooser file never matched");
    dialog.accept_button.emit_clicked();
    eq!(answers.borrow()[0], Some(target.display().to_string()));
    check!(!target.exists());
    check!(ChooserMode::from_name("bogus").is_err());
    let chosen: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let sink = chosen.clone();
    let dialog = dialogs::choose_application(Some(parent.upcast_ref()), &gio::File::for_path(root.join("document.txt")),
                                            move |a| sink.borrow_mut().push(a.and_then(|a| a.id()).map(|s| s.to_string())));
    check!(wait(5.0, || dialog.applications_loaded()));
    dialog.window.close();
    eq!(*chosen.borrow(), vec![None]);
    parent.destroy();
    Ok(())
}

// ---------------------------------------------------------------- Open With (test_open_with.py)

struct OpenWith {
    dialog: OpenWithDialog,
    parent: gtk::ApplicationWindow,
    answers: Rc<RefCell<Vec<Option<String>>>>,
    ids: Vec<String>,
    paths: Vec<PathBuf>,
    defaults: Rc<RefCell<Vec<String>>>,
    fail_default: Rc<Cell<Option<bool>>>,
    file: gio::File,
    options: OpenWithOptions,
}

impl OpenWith {
    fn new(env: &Env, name: &str) -> Self {
        let root = fixture(name);
        let path = root.join("example.txt");
        std::fs::write(&path, "Generated Open With fixture\n").unwrap();
        let data = PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap()).join("applications");
        std::fs::create_dir_all(&data).unwrap();
        let names = [("Alpha Viewer", ""), ("beta Paint", ""), ("Zeta Preview", "NoDisplay=true\n"),
                     ("Handler", "NoDisplay=true\n"), ("Excluded", "Hidden=true\n"), ("Menu Helper", "NoDisplay=true\n")];
        let mut paths = Vec::new();
        for (index, (title, extra)) in names.iter().enumerate() {
            let desktop = data.join(format!("{name}-{index}.desktop"));
            std::fs::write(&desktop, format!("[Desktop Entry]\nType=Application\nName={title}\nComment=Photographs\n\
                Exec=/usr/bin/true %F\nIcon=application-x-executable\n{extra}")).unwrap();
            paths.push(desktop);
        }
        let ids = (0..names.len()).map(|i| format!("{name}-{i}.desktop")).collect();
        let strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let info = |i: usize, s: &[String]| tessel_dock::core::app_catalog::desktop_info_from_filename(&s[i]).expect("desktop info");
        // Discovery omits the hidden current default from the MIME list on purpose.
        let discovery: dialogs::Discovery = Arc::new(move |_kind| dialogs::AppSources {
            all: (0..strings.len()).map(|i| info(i, &strings)).collect(),
            all_for_type: vec![info(1, &strings), info(3, &strings), info(4, &strings)],
            recommended: vec![info(1, &strings), info(3, &strings)],
            default: Some(info(2, &strings)),
        });
        let defaults: Rc<RefCell<Vec<String>>> = Rc::default();
        let fail_default: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
        let (sink, fail) = (defaults.clone(), fail_default.clone());
        // `None` = really set the default (private XDG_CONFIG_HOME), `Some(result)` = mocked.
        let set_default: dialogs::SetDefault = Rc::new(move |app: &gio::AppInfo, kind: &str| {
            sink.borrow_mut().push(kind.to_string());
            match fail.get() {
                Some(result) => Ok(result),
                None => app.set_as_default_for_type(kind).map(|_| true),
            }
        });
        let options = OpenWithOptions { discovery: Some(discovery), set_default: Some(set_default) };
        let parent = env.parent();
        let answers: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let file = gio::File::for_path(&path);
        let dialog = Self::open(&parent, &file, &answers, &options);
        Self { dialog, parent, answers, ids, paths, defaults, fail_default, file, options }
    }

    fn open(parent: &gtk::ApplicationWindow, file: &gio::File, answers: &Rc<RefCell<Vec<Option<String>>>>,
            options: &OpenWithOptions) -> OpenWithDialog {
        let sink = answers.clone();
        let dialog = dialogs::choose_application_with(Some(parent.upcast_ref()), file,
            move |a| sink.borrow_mut().push(a.and_then(|a| a.id()).map(|s| s.to_string())), options.clone());
        assert!(wait(5.0, || dialog.applications_loaded()), "applications never loaded: {}", dialog.status.text());
        dialog
    }

    fn names(&self) -> Vec<String> {
        self.dialog.rows().iter().filter_map(dialogs::row_app_info).map(|a| a.display_name().to_string()).collect()
    }

    fn answer_ids(&self) -> Vec<Option<String>> {
        self.answers.borrow().clone()
    }

    fn id(&self, index: usize) -> Option<String> {
        Some(self.ids[index].clone())
    }
}

impl Drop for OpenWith {
    fn drop(&mut self) {
        self.dialog.window.close();
        self.parent.destroy();
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn search_icon_has_space_before_placeholder(env: &Env) -> Check {
    let fixture = OpenWith::new(env, "ow-icon");
    let search = &fixture.dialog.application_search;
    let nodes = tessel_gtk::utils::children(search);
    let text = nodes.iter().find(|n| n.css_name() == "text").ok_or("no text node")?.clone();
    let icon = nodes.iter().find(|n| n.css_name() == "image").ok_or("no image node")?.clone();
    check!(wait(5.0, || text.width() > 0));
    let text_bounds = text.compute_bounds(search).ok_or("text bounds")?;
    let icon_bounds = icon.compute_bounds(search).ok_or("icon bounds")?;
    let gap = text_bounds.x() - icon_bounds.x() - icon_bounds.width();
    check!(gap >= 8.0, "gap {gap}");
    Ok(())
}

fn all_apps_and_hidden_handlers_sorted_and_filtered_in_both_directions(env: &Env) -> Check {
    let f = OpenWith::new(env, "ow-sort");
    let d = &f.dialog;
    eq!(f.names(), vec!["Alpha Viewer", "beta Paint", "Handler", "Zeta Preview"]);
    check!(!d.default_check.is_active());
    check!(d.default_label.text().contains("text/plain"), "{}", d.default_label.text());
    d.application_chooser.select_row(Some(&d.rows()[1]));
    let selected = d.application_chooser.selected_row().and_then(|r| dialogs::row_app_id(&r));
    d.application_sort.set_selected(1);
    eq!(f.names(), vec!["Zeta Preview", "Handler", "beta Paint", "Alpha Viewer"]);
    eq!(d.application_chooser.selected_row().and_then(|r| dialogs::row_app_id(&r)), selected);
    d.application_search.set_text("pHoToGrApHs");
    check!(wait(5.0, || d.rows().len() == 4));
    d.application_search.set_text("VIEW");
    check!(wait(5.0, || f.names() == ["Zeta Preview", "Alpha Viewer"]), "{:?}", f.names());
    d.application_sort.set_selected(0);
    eq!(f.names(), vec!["Alpha Viewer", "Zeta Preview"]);
    d.application_search.set_text("no possible app match");
    check!(wait(5.0, || d.rows().is_empty()));
    check!(!d.accept_button.is_sensitive());
    check!(!d.default_check.is_sensitive());
    d.accept_button.emit_clicked();
    check!(f.answer_ids().is_empty());
    Ok(())
}

fn immediate_search_activation_opens_latest_match_without_setting_default(env: &Env) -> Check {
    let f = OpenWith::new(env, "ow-activate");
    f.fail_default.set(Some(true));
    f.dialog.application_search.set_text("bETA pAINT");
    // No main-loop wait: the debounce must not let Enter open the old Alpha Viewer.
    f.dialog.application_search.emit_by_name::<()>("activate", &[]);
    eq!(f.answer_ids(), vec![f.id(1)]);
    check!(f.defaults.borrow().is_empty());
    f.dialog.window.close();
    eq!(f.answer_ids().len(), 1);
    Ok(())
}

fn immediate_open_button_uses_latest_search_and_row_activation_works(env: &Env) -> Check {
    let mut f = OpenWith::new(env, "ow-open");
    f.dialog.application_search.set_text("ZETA");
    f.dialog.accept_button.emit_clicked();
    eq!(f.answer_ids(), vec![f.id(2)]);
    f.answers.borrow_mut().clear();
    f.dialog = OpenWith::open(&f.parent, &f.file, &f.answers, &f.options);
    let row = f.dialog.rows()[1].clone();
    f.dialog.application_chooser.emit_by_name::<()>("row-activated", &[&row]);
    eq!(f.answer_ids(), vec![f.id(1)]);
    Ok(())
}

fn confirmed_checkbox_updates_private_mime_default_for_selected_type_only(env: &Env) -> Check {
    let config = PathBuf::from(std::env::var("XDG_CONFIG_HOME").map_err(|_| "no private XDG_CONFIG_HOME")?);
    let private = std::env::var("FILES_WIDGETS_PRIVATE_ROOT").unwrap_or_else(|_| "/nonexistent".into());
    check!(config.starts_with(&private), "XDG_CONFIG_HOME is not private: {}", config.display());
    let f = OpenWith::new(env, "ow-default");
    f.dialog.application_search.set_text("beta");
    f.dialog.default_check.set_active(true);
    f.dialog.accept_button.emit_clicked();
    eq!(f.answer_ids(), vec![f.id(1)]);
    let associations = config.join("mimeapps.list");
    check!(associations.is_file());
    let values = glib::KeyFile::new();
    values.load_from_file(&associations, glib::KeyFileFlags::NONE).map_err(|e| e.to_string())?;
    let list: Vec<String> = values.string_list("Default Applications", "text/plain").map_err(|e| e.to_string())?
        .iter().map(|s| s.to_string()).collect();
    eq!(list, vec![f.ids[1].clone()]);
    let keys: Vec<String> = values.keys("Default Applications").map_err(|e| e.to_string())?.iter().map(|s| s.to_string()).collect();
    eq!(keys, vec!["text/plain".to_string()]);
    Ok(())
}

fn cancel_does_not_apply_checked_default(env: &Env) -> Check {
    let f = OpenWith::new(env, "ow-cancel");
    f.fail_default.set(Some(true));
    f.dialog.default_check.set_active(true);
    f.dialog.window.close();
    eq!(f.answer_ids(), vec![None]);
    check!(f.defaults.borrow().is_empty());
    Ok(())
}

fn association_failure_stays_open_and_can_retry_without_default(env: &Env) -> Check {
    let f = OpenWith::new(env, "ow-fail");
    f.dialog.default_check.set_active(true);
    f.fail_default.set(Some(false));
    f.dialog.accept_button.emit_clicked();
    eq!(*f.defaults.borrow(), vec!["text/plain".to_string()]);
    check!(f.answer_ids().is_empty());
    check!(f.dialog.window.is_visible());
    check!(f.dialog.status.text().contains("Could not set the default application"));
    f.dialog.default_check.set_active(false);
    f.dialog.accept_button.emit_clicked();
    eq!(f.answer_ids(), vec![f.id(0)]);
    eq!(f.defaults.borrow().len(), 1);
    Ok(())
}

// ---------------------------------------------------------------- information (test_files_information.py)

struct PaneFixture {
    window: gtk::ApplicationWindow,
    pane: InformationPane,
    root: PathBuf,
}

impl PaneFixture {
    fn new(env: &Env, name: &str) -> Self {
        let root = fixture(name);
        std::fs::write(root.join("Alpha.txt"), "alpha\n").unwrap();
        std::fs::write(root.join("Beta.txt"), "beta\n").unwrap();
        let window = env.parent();
        window.set_default_size(900, 700);
        let pane = InformationPane::new(|| {}, Some(Rc::new(|_item: Item| {})));
        window.set_child(Some(pane.widget()));
        window.present();
        Self { window, pane, root }
    }

    fn select(&self, names: &[&str]) {
        let items: Vec<Item> = names.iter().map(|n| item(&self.root.join(n))).collect();
        self.pane.show_selection(&items, &gio::File::for_path(&self.root));
    }

    fn row(&self, title: &str) -> Option<String> {
        self.pane.row_value(title)
    }
}

impl Drop for PaneFixture {
    fn drop(&mut self) {
        self.pane.close();
        self.window.destroy();
    }
}

fn large_preview_and_image_details_follow_selection_and_orientation(env: &Env) -> Check {
    let f = PaneFixture::new(env, "info-photo");
    write_photo(&f.root.join("Coast.jpg"), 1);
    f.select(&["Coast.jpg"]);
    check!(wait(10.0, || f.pane.picture.paintable().is_some()), "hint {}", f.pane.hint.text());
    pump(0.3);
    let texture = f.pane.picture.paintable().unwrap();
    eq!((texture.intrinsic_width(), texture.intrinsic_height()), (1200, 800));
    check!(f.pane.picture.width() >= 300 && f.pane.picture.height() >= 300, "{}x{}", f.pane.picture.width(), f.pane.picture.height());
    eq!(f.row("Dimensions"), Some("2,400 × 1,600 pixels".to_string()));
    eq!(f.row("Format"), Some("JPEG".to_string()));
    eq!(f.row("Camera"), Some("Camera Example 1".to_string()));
    eq!(f.row("Date taken"), Some("2026:09:27 10:30:00".to_string()));
    eq!(f.row("Color mode"), Some("RGB".to_string()));
    check!(f.row("File size").is_some());
    if let Ok(path) = std::env::var("TESSEL_FILES_PREVIEW_SCREENSHOT") {
        let _ = std::process::Command::new("grim").arg(path).status();
    }
    write_photo(&f.root.join("Portrait.jpg"), 6);
    f.select(&["Portrait.jpg"]);
    check!(wait(10.0, || f.pane.picture.paintable().is_some()));
    let texture = f.pane.picture.paintable().unwrap();
    eq!((texture.intrinsic_width(), texture.intrinsic_height()), (800, 1200));
    eq!(f.row("Dimensions"), Some("1,600 × 2,400 pixels".to_string()));
    f.select(&["Alpha.txt"]);
    check!(f.pane.picture.paintable().is_none());
    check!(f.row("Dimensions").is_none());
    eq!(f.pane.title.text().to_string(), "Alpha.txt".to_string());
    f.select(&[]);
    eq!(f.pane.title.text().to_string(), f.root.file_name().unwrap().to_string_lossy().to_string());
    Ok(())
}

fn invalid_image_and_multi_selection_clear_old_preview(env: &Env) -> Check {
    let f = PaneFixture::new(env, "info-broken");
    write_photo(&f.root.join("Coast.jpg"), 1);
    std::fs::write(f.root.join("Broken.jpg"), b"broken jpeg image").unwrap();
    f.select(&["Coast.jpg"]);
    check!(wait(10.0, || f.pane.picture.paintable().is_some()));
    f.select(&["Broken.jpg"]);
    check!(wait(5.0, || f.pane.hint.text() == "Preview unavailable"), "{}", f.pane.hint.text());
    check!(f.pane.picture.paintable().is_none());
    f.select(&["Alpha.txt", "Beta.txt", "Coast.jpg"]);
    check!(f.pane.title.text().contains("items"));
    eq!(f.pane.rows().into_iter().map(|(t, _)| t).collect::<Vec<_>>(), vec!["Total size".to_string()]);
    Ok(())
}

fn pending_image_does_not_replace_new_selection(env: &Env) -> Check {
    let f = PaneFixture::new(env, "info-pending");
    write_photo(&f.root.join("Coast.jpg"), 1);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (e, r) = (entered.clone(), release.clone());
    information::set_decoder(IMAGE, Some(Arc::new(move |file: &gio::File, _token: &gio::Cancellable| {
        e.store(true, Ordering::SeqCst);
        let end = Instant::now() + Duration::from_secs(3);
        while !r.load(Ordering::SeqCst) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        information::decode_preview(file, &gio::Cancellable::new()) // completes despite cancellation
    })));
    f.select(&["Coast.jpg"]);
    let result = (|| {
        check!(wait(5.0, || entered.load(Ordering::SeqCst)));
        f.select(&["Beta.txt"]);
        release.store(true, Ordering::SeqCst);
        check!(wait(5.0, || !f.pane.loader().running()));
        Ok(())
    })();
    release.store(true, Ordering::SeqCst);
    information::set_decoder(IMAGE, None);
    result?;
    eq!(f.pane.title.text().to_string(), "Beta.txt".to_string());
    check!(f.pane.picture.paintable().is_none());
    f.select(&["Coast.jpg"]);
    check!(wait(10.0, || f.pane.picture.paintable().is_some()));
    Ok(())
}

fn worker_keeps_only_latest_pending_request(_: &Env) -> Check {
    let loader = information::PreviewLoader::new();
    let release = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (r, e, c) = (release.clone(), entered.clone(), calls.clone());
    information::set_decoder("test-kind", Some(Arc::new(move |file: &gio::File, _token: &gio::Cancellable| {
        c.lock().unwrap().push(file.basename().unwrap().to_string_lossy().into_owned());
        e.store(true, Ordering::SeqCst);
        let end = Instant::now() + Duration::from_secs(3);
        while !r.load(Ordering::SeqCst) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        (None, Vec::new(), String::new())
    })));
    let results: Rc<RefCell<Vec<&str>>> = Rc::default();
    let sink = results.clone();
    loader.request(&gio::File::for_path("/nonexistent-fw/first"), move |_| sink.borrow_mut().push("stale"), "test-kind");
    let outcome = (|| {
        check!(wait(5.0, || entered.load(Ordering::SeqCst)));
        for index in 0..100 {
            let sink = results.clone();
            loader.request(&gio::File::for_path(format!("/nonexistent-fw/{index}")), move |_| sink.borrow_mut().push("latest"), "test-kind");
        }
        release.store(true, Ordering::SeqCst);
        check!(wait(5.0, || !loader.running() && !loader.pending()));
        Ok(())
    })();
    release.store(true, Ordering::SeqCst);
    information::set_decoder("test-kind", None);
    loader.close();
    outcome?;
    eq!(*calls.lock().unwrap(), vec!["first".to_string(), "99".to_string()]);
    eq!(*results.borrow(), vec!["latest"]);
    Ok(())
}

// ---------------------------------------------------------------- browsing (test_files_browsing.py)

fn text_pdf_and_binary_previews(env: &Env) -> Check {
    use sourceview5::prelude::*;
    let f = PaneFixture::new(env, "browse-previews");
    std::fs::write(f.root.join("script.py"), "def hello():\n    return 42\n").unwrap();
    std::fs::write(f.root.join("blob.txt"), b"text\x00binary").unwrap();
    std::fs::create_dir(f.root.join("Documents")).unwrap();
    write_pdf(&f.root.join("Guide.pdf"), 2);
    let preview = &f.pane.preview;
    f.select(&["script.py"]);
    check!(wait(5.0, || preview.stack.visible_child_name().as_deref() == Some("text")));
    let buffer = &preview.buffer;
    eq!(buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string(), "def hello():\n    return 42\n".to_string());
    eq!(f.row("Lines"), Some("2".to_string()));
    check!(preview.source());
    check!(buffer.language().is_some_and(|l| l.id().starts_with("python")));
    check!(f.pane.print_button.is_visible());
    f.select(&["Guide.pdf"]);
    check!(poppler::available(), "poppler-glib is not loadable");
    check!(wait(10.0, || f.pane.picture.paintable().is_some()), "hint {}", f.pane.hint.text());
    let texture = f.pane.picture.paintable().unwrap();
    eq!(texture.intrinsic_width().max(texture.intrinsic_height()), 1200);
    eq!(f.row("Pages"), Some("2".to_string()));
    eq!(f.row("Page size"), Some("210 × 297 mm".to_string()));
    f.select(&["blob.txt"]);
    check!(wait(5.0, || f.pane.hint.text() == "This file contains binary data."), "{}", f.pane.hint.text());
    check!(!f.pane.print_button.is_visible());
    f.select(&["Documents"]);
    eq!(preview.stack.visible_child_name().map(|s| s.to_string()), Some("placeholder".to_string()));
    Ok(())
}

fn text_preview_is_capped_on_a_line_boundary(_: &Env) -> Check {
    let root = fixture("browse-cap");
    let big = root.join("big.log");
    std::fs::write(&big, ("x".repeat(99) + "\n").repeat(20)).unwrap();
    let (text, truncated) = information::read_text(&gio::File::for_path(&big), None, 1050).map_err(|e| e.to_string())?;
    check!(truncated);
    eq!(text, ("x".repeat(99) + "\n").repeat(10));
    Ok(())
}

fn media_preview_uses_a_player_and_releases_it(env: &Env) -> Check {
    if tessel_core::shell::which("ffmpeg").is_none() {
        println!("  (skipped: ffmpeg creates the media fixture)");
        return Ok(());
    }
    let f = PaneFixture::new(env, "browse-media");
    let clip = f.root.join("tone.ogg");
    let status = std::process::Command::new("ffmpeg").args(["-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
        .arg(&clip).status().map_err(|e| e.to_string())?;
    check!(status.success());
    f.select(&["tone.ogg"]);
    let preview = &f.pane.preview;
    eq!(preview.stack.visible_child_name().map(|s| s.to_string()), Some("media".to_string()));
    let stream = preview.video.media_stream().ok_or("no media stream")?;
    check!(!stream.is_playing());
    pump(0.5);
    check!(!stream.is_playing());
    f.select(&["Alpha.txt"]);
    check!(preview.video.media_stream().is_none());
    Ok(())
}

struct FakePane {
    items: Vec<Item>,
    index: Cell<usize>,
    activated: RefCell<Vec<String>>,
}

impl QuickLookPane for FakePane {
    fn closed(&self) -> bool {
        false
    }
    fn move_selection(&self, delta: i32) -> Option<Item> {
        let next = (self.index.get() as i32 + delta).clamp(0, self.items.len() as i32 - 1) as usize;
        self.index.set(next);
        Some(self.items[next].clone())
    }
    fn activate(&self, item: &Item) {
        self.activated.borrow_mut().push(item.name());
    }
}

fn space_quick_preview_steps_opens_and_closes(env: &Env) -> Check {
    let root = fixture("quicklook");
    std::fs::write(root.join("Alpha.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("Beta.txt"), "beta\n").unwrap();
    let pane = Rc::new(FakePane { items: vec![item(&root.join("Alpha.txt")), item(&root.join("Beta.txt"))],
                                  index: Cell::new(0), activated: RefCell::default() });
    let parent = env.parent();
    parent.set_default_size(800, 600);
    parent.present();
    let printed = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = printed.clone();
    let look = QuickLook::new(&parent, pane.clone(), Rc::new(move |item: Item| sink.borrow_mut().push(item.name())));
    look.show_item(&pane.items[0]);
    look.present();
    check!(wait(5.0, || look.preview.stack.visible_child_name().as_deref() == Some("text")));
    eq!(look.heading.text().to_string(), "Alpha.txt".to_string());
    check!(wait(2.0, || look.print_button.is_visible()));
    look.print_button.emit_clicked();
    eq!(*printed.borrow(), vec!["Alpha.txt".to_string()]);
    check!(look.key_pressed(gdk::Key::Right, gdk::ModifierType::empty()));
    eq!(pane.index.get(), 1);
    eq!(look.heading.text().to_string(), "Beta.txt".to_string());
    // Up/Down scroll a text preview instead of stepping.
    check!(wait(5.0, || look.preview.kind() == Some(information::TEXT)));
    check!(!look.key_pressed(gdk::Key::Up, gdk::ModifierType::empty()));
    check!(!look.key_pressed(gdk::Key::Left, gdk::ModifierType::CONTROL_MASK));
    check!(look.key_pressed(gdk::Key::Left, gdk::ModifierType::empty()));
    eq!(look.heading.text().to_string(), "Alpha.txt".to_string());
    check!(look.key_pressed(gdk::Key::Return, gdk::ModifierType::empty()));
    eq!(*pane.activated.borrow(), vec!["Alpha.txt".to_string()]);
    check!(wait(2.0, || !look.window().is_visible()));
    let look = QuickLook::new(&parent, pane.clone(), Rc::new(|_| {}));
    look.show_item(&pane.items[1]);
    look.present();
    check!(look.key_pressed(gdk::Key::space, gdk::ModifierType::empty()));
    check!(wait(2.0, || !look.window().is_visible()));
    check!(look.preview.item().is_none(), "closing must release the preview");
    parent.destroy();
    Ok(())
}

fn printing_renders_pdf_text_and_images(env: &Env) -> Check {
    check!(poppler::available(), "poppler-glib is required to count exported pages");
    let root = fixture("print");
    write_pdf(&root.join("Guide.pdf"), 3);
    std::fs::write(root.join("long.txt"), (0..400).map(|n| format!("line {n}\n")).collect::<String>()).unwrap();
    let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 300, 200).unwrap();
    pixbuf.fill(0);
    pixbuf.savev(root.join("shot.png"), "png", &[]).unwrap();
    std::fs::create_dir(root.join("Documents")).unwrap();
    let parent = env.parent();
    let errors: Rc<RefCell<Vec<String>>> = Rc::default();
    for (name, pages) in [("Guide.pdf", Some(3)), ("long.txt", None), ("shot.png", Some(1))] {
        let target = item(&root.join(name));
        check!(print::printable(Some(&target)));
        let output = root.join(format!("{name}.out.pdf"));
        let sink = errors.clone();
        let result = print::print_file(Some(parent.upcast_ref()), &target, Rc::new(move |e| sink.borrow_mut().push(e)), Some(&output));
        eq!(result, Some(gtk::PrintOperationResult::Apply));
        let uri = gio::File::for_path(&output).uri();
        let count = poppler::Document::from_uri(&uri).map_err(|e| e.to_string())?.n_pages();
        match pages {
            None => check!(count > 1, "400 lines need several pages, got {count}"),
            Some(pages) => eq!(count, pages),
        }
    }
    eq!(*errors.borrow(), Vec::<String>::new());
    check!(!print::printable(Some(&item(&root.join("Documents")))));
    let sink = errors.clone();
    std::fs::write(root.join("blob.bin"), [0u8, 1, 2]).unwrap();
    let result = print::print_file(None, &item(&root.join("blob.bin")), Rc::new(move |e| sink.borrow_mut().push(e)), None);
    eq!(result, None);
    eq!(*errors.borrow(), vec!["This file type cannot be printed from Files.".to_string()]);
    parent.destroy();
    Ok(())
}

// ---------------------------------------------------------------- devices (test_files_devices.py)

fn disk_usage_bar_shows_free_and_total(_: &Env) -> Check {
    let usage = DiskUsage::new(&gio::File::for_path(std::env::temp_dir()));
    check!(wait(5.0, || usage.pending().is_none()));
    check!(usage.showing_capacity(), "detail {}", usage.detail.text());
    check!(usage.detail.text().contains("free\n"));
    check!(usage.detail.text().contains("total"));
    check!((0.0..=1.0).contains(&usage.bar.fraction()));
    check!(usage.widget().tooltip_text().is_some_and(|t| t.contains(" used · ")));
    usage.close();
    check!(usage.closed());
    Ok(())
}

fn slow_capacity_query_does_not_queue_and_ignores_stale_reply(_: &Env) -> Check {
    devices::forget_last_capacities();
    let usage = DiskUsage::new(&gio::File::for_path(std::env::temp_dir()));
    let token = usage.pending().ok_or("no query started")?;
    usage.refresh();
    check!(usage.pending().as_ref() == Some(&token), "refresh must not queue a second query");
    usage.close();
    check!(token.is_cancelled());
    usage.finished(&token, Some((1000, 10, 990)));
    pump(0.2);
    check!(!usage.showing_capacity());
    eq!(usage.detail.text().lines().next().map(str::to_string), Some("Checking space…".to_string()));
    Ok(())
}

/// The Files stylesheet on the display while `body` runs, around a `.files-window`.
fn in_files_window(env: &Env, usage: &DiskUsage, body: impl FnOnce(&gtk::ApplicationWindow) -> Check) -> Check {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(style::CSS);
    let display = gdk::Display::default().ok_or("no display")?;
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    let window = env.parent();
    window.add_css_class("files-window");
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).width_request(176).build();
    column.append(usage.widget());
    window.set_child(Some(&column));
    window.present();
    let result = body(&window);
    usage.close();
    window.destroy();
    gtk::style_context_remove_provider_for_display(&display, &provider);
    result
}

fn size_of(usage: &DiskUsage) -> (i32, i32, i32) {
    let (min_width, _, _, _) = usage.widget().measure(gtk::Orientation::Horizontal, -1);
    let (min_height, natural_height, _, _) = usage.widget().measure(gtk::Orientation::Vertical, 176);
    (min_width, min_height, natural_height)
}

// The Places pane jumped when the replies came: the bar appeared (wider, taller) and one line became two.
fn disk_usage_keeps_its_size_when_the_reply_arrives(env: &Env) -> Check {
    devices::forget_last_capacities();
    let usage = DiskUsage::new(&gio::File::for_path(std::env::temp_dir()));
    in_files_window(env, &usage, |_| {
        let token = usage.pending().ok_or("no query started")?;
        check!(!usage.showing_capacity());
        let before = size_of(&usage);
        check!(before.0 > 0 && before.1 > 0);
        usage.finished(&token, Some((8_000_000_000_000, 1_100_000_000_000, 6_900_000_000_000)));
        check!(usage.showing_capacity());
        check!(usage.detail.text().contains(" free\n"), "detail {}", usage.detail.text());
        eq!(size_of(&usage), before);
        // A later reply with other numbers, and the rare unavailable outcome, keep it too.
        usage.refresh();
        let token = usage.pending().ok_or("no second query")?;
        usage.finished(&token, Some((500_000_000, 1_000_000, 499_000_000)));
        eq!(size_of(&usage), before);
        usage.refresh();
        let token = usage.pending().ok_or("no third query")?;
        usage.finished(&token, None);
        check!(!usage.showing_capacity());
        eq!(usage.detail.text().lines().next().map(str::to_string), Some("Space unavailable".to_string()));
        eq!(size_of(&usage), before);
        Ok(())
    })
}

fn disk_usage_starts_with_the_last_known_numbers(env: &Env) -> Check {
    devices::forget_last_capacities();
    let location = gio::File::for_path(std::env::temp_dir());
    let seeded = DiskUsage::seeded(&location, Some((1000, 50, 950)));
    check!(seeded.showing_capacity());
    check!(seeded.detail.text().ends_with(" total"), "detail {}", seeded.detail.text());
    check!(seeded.bar.has_css_class("low-space"));
    check!(seeded.pending().is_some(), "a seeded row still asks for fresh numbers");
    seeded.close();
    // A reply is remembered: the next row for that place shows it before its own query returns.
    let first = DiskUsage::new(&location);
    check!(!first.showing_capacity());
    check!(wait(5.0, || first.pending().is_none()));
    let shown = first.detail.text().to_string();
    first.close();
    let rebuilt = DiskUsage::new(&location);
    check!(rebuilt.pending().is_some());
    check!(rebuilt.showing_capacity());
    eq!(rebuilt.detail.text().to_string(), shown);
    in_files_window(env, &rebuilt, |_| {
        let before = size_of(&rebuilt);
        check!(wait(5.0, || rebuilt.pending().is_none()));
        eq!(size_of(&rebuilt), before);
        Ok(())
    })
}

// ---------------------------------------------------------------- jobs (file_jobs.py)

struct FakeJob(Rc<Cell<u32>>);
impl jobs::JobHandle for FakeJob {
    fn cancel(&self) {
        self.0.set(self.0.get() + 1);
    }
}

fn jobs_button_lists_cancels_and_notifies(env: &Env) -> Check {
    let manager = JobManager::new(&env.app);
    let notes: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = notes.clone();
    manager.set_notifier(Some(Rc::new(move |id: &str, _n: &gio::Notification| sink.borrow_mut().push(id.to_string()))));
    let window = env.parent();
    jobs::set_window_alive(&window, true);
    let button = JobsButton::new(&manager);
    check!(!button.widget().is_visible());
    let entry = manager.add("Copying", Some(window.upcast_ref()), None);
    let cancels = Rc::new(Cell::new(0));
    entry.set_job(Some(Rc::new(FakeJob(cancels.clone()))));
    check!(button.widget().is_visible());
    eq!(button.idents(), vec![entry.ident]);
    check!(button.pulsing());
    eq!(button.widget().tooltip_text().map(|s| s.to_string()), Some("1 file operation running".to_string()));
    manager.update(&entry, Some("a.txt"), Some(0.5));
    let (_, title, progress, cancel) = button.row(entry.ident).unwrap();
    eq!(title.text().to_string(), "Copying · a.txt".to_string());
    eq!(progress.fraction(), 0.5);
    cancel.emit_clicked();
    eq!(cancels.get(), 1);
    check!(!cancel.is_sensitive());
    check!(title.text().ends_with(" · Cancelling…"));
    eq!(manager.owned_by(window.upcast_ref()).len(), 1);
    let second = manager.add("Moving", None, None);
    eq!(button.widget().tooltip_text().map(|s| s.to_string()), Some("2 file operations running".to_string()));
    manager.finish(&entry, Some("done"), false);
    // A Files window is still alive: no notification.
    check!(notes.borrow().is_empty());
    jobs::set_window_alive(&window, false);
    manager.finish(&second, Some("Moved 1 item"), false);
    eq!(*notes.borrow(), vec![format!("files-job-{}", second.ident)]);
    check!(manager.is_empty());
    check!(!button.widget().is_visible());
    check!(!button.pulsing());
    button.close();
    let answers: Rc<RefCell<Vec<Option<jobs::KeepAnswer>>>> = Rc::default();
    let sink = answers.clone();
    let dialog = jobs::ask_keep_running(Some(window.upcast_ref()), move |a| sink.borrow_mut().push(a));
    dialog.close();
    eq!(*answers.borrow(), vec![None]);
    window.destroy();
    Ok(())
}

// ---------------------------------------------------------------- main

type Test = (&'static str, fn(&Env) -> Check);

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let private = std::env::var("FILES_WIDGETS_PRIVATE_ROOT").unwrap_or_default();
    if std::env::var("FILES_WIDGETS_ISOLATED_TEST").as_deref() != Ok("1") || runtime == "/run/user/1000" || runtime.is_empty()
        || private.is_empty() || !runtime.starts_with(&private)
        || std::env::var("WAYLAND_DISPLAY").map(|d| d.is_empty()).unwrap_or(true) {
        println!("ui_headless: skipped (run tests/run_headless.sh for the private headless compositor)");
        return;
    }
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    tessel_core::i18n::initialize();
    gtk::init().expect("GTK in the private compositor");
    let app = gtk::Application::new(Some("org.tessel.FilesWidgetsTest"), gio::ApplicationFlags::NON_UNIQUE);
    app.register(gio::Cancellable::NONE).expect("register on the private bus");
    let env = Env { app };
    let tests: &[Test] = &[
        ("style_css_parses", style_css_parses),
        ("properties_metadata_permissions_checksums_and_symlinks", properties_metadata_permissions_checksums_and_symlinks),
        ("directory_sizes_and_close_cancels_work", directory_sizes_and_close_cancels_work),
        ("prompt_close_notifies_once_and_confirm_cancel_is_false", prompt_close_notifies_once_and_confirm_cancel_is_false),
        ("metadata_edit_is_explicit_and_persists_in_private_fixture", metadata_edit_is_explicit_and_persists_in_private_fixture),
        ("explicit_gtk4_file_chooser_and_application_chooser_cancel", explicit_gtk4_file_chooser_and_application_chooser_cancel),
        ("search_icon_has_space_before_placeholder", search_icon_has_space_before_placeholder),
        ("all_apps_and_hidden_handlers_sorted_and_filtered_in_both_directions", all_apps_and_hidden_handlers_sorted_and_filtered_in_both_directions),
        ("immediate_search_activation_opens_latest_match_without_setting_default", immediate_search_activation_opens_latest_match_without_setting_default),
        ("immediate_open_button_uses_latest_search_and_row_activation_works", immediate_open_button_uses_latest_search_and_row_activation_works),
        ("confirmed_checkbox_updates_private_mime_default_for_selected_type_only", confirmed_checkbox_updates_private_mime_default_for_selected_type_only),
        ("cancel_does_not_apply_checked_default", cancel_does_not_apply_checked_default),
        ("association_failure_stays_open_and_can_retry_without_default", association_failure_stays_open_and_can_retry_without_default),
        ("large_preview_and_image_details_follow_selection_and_orientation", large_preview_and_image_details_follow_selection_and_orientation),
        ("invalid_image_and_multi_selection_clear_old_preview", invalid_image_and_multi_selection_clear_old_preview),
        ("pending_image_does_not_replace_new_selection", pending_image_does_not_replace_new_selection),
        ("worker_keeps_only_latest_pending_request", worker_keeps_only_latest_pending_request),
        ("text_pdf_and_binary_previews", text_pdf_and_binary_previews),
        ("text_preview_is_capped_on_a_line_boundary", text_preview_is_capped_on_a_line_boundary),
        ("media_preview_uses_a_player_and_releases_it", media_preview_uses_a_player_and_releases_it),
        ("space_quick_preview_steps_opens_and_closes", space_quick_preview_steps_opens_and_closes),
        ("printing_renders_pdf_text_and_images", printing_renders_pdf_text_and_images),
        ("disk_usage_bar_shows_free_and_total", disk_usage_bar_shows_free_and_total),
        ("slow_capacity_query_does_not_queue_and_ignores_stale_reply", slow_capacity_query_does_not_queue_and_ignores_stale_reply),
        ("disk_usage_keeps_its_size_when_the_reply_arrives", disk_usage_keeps_its_size_when_the_reply_arrives),
        ("disk_usage_starts_with_the_last_known_numbers", disk_usage_starts_with_the_last_known_numbers),
        ("jobs_button_lists_cancels_and_notifies", jobs_button_lists_cancels_and_notifies),
    ];
    let (mut passed, mut failed) = (0, Vec::new());
    for (name, test) in tests {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test(&env)))
            .unwrap_or_else(|panic| Err(format!("panic: {}", panic.downcast_ref::<String>().cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default())));
        pump(0.05);
        match result {
            Ok(()) => {
                passed += 1;
                println!("ok   {name} ({:.2}s)", started.elapsed().as_secs_f64());
            }
            Err(error) => {
                println!("FAIL {name}: {error}");
                failed.push(*name);
            }
        }
    }
    println!("\n{passed} passed, {} failed {:?}", failed.len(), failed);
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
