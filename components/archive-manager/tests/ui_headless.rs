//! Archive Manager GUI checks. Runs only inside the private nested headless
//! labwc started by tests/run_headless.sh (own XDG_RUNTIME_DIR and Wayland
//! socket, private D-Bus without service activation, GIO_USE_VFS=local,
//! private HOME/XDG dirs); a private virtual pointer drives that compositor
//! only. Every fixture lives below $TMPDIR (= $R/tmp). Under plain
//! `cargo test` it skips itself.

use archive_manager::archive::{CreateOptions, Ctx};
use archive_manager::ops;
use archive_manager::settings::plugins::Kind;
use archive_manager::settings::{Settings, Store};
use archive_manager::ui::app::App;
use archive_manager::ui::window::ArchiveWindow;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

type Outcome = Result<(), String>;

macro_rules! ensure {
    ($cond:expr) => {
        if !$cond {
            return Err(format!("{}:{}: assertion failed: {}", file!(), line!(), stringify!($cond)));
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!("{}:{}: {}", file!(), line!(), format!($($arg)+)));
        }
    };
}

macro_rules! ensure_eq {
    ($left:expr, $right:expr) => {{
        match (&$left, &$right) { (l, r) => if l != r {
            return Err(format!("{}:{}: {} != {}\n  left: {:?}\n right: {:?}", file!(), line!(), stringify!($left), stringify!($right), l, r));
        } }
    }};
}

macro_rules! wait {
    ($pred:expr) => {
        if !wait_for(20.0, $pred) {
            return Err(format!("{}:{}: timed out waiting for {}", file!(), line!(), stringify!($pred)));
        }
    };
}

fn settle(seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let context = glib::MainContext::default();
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait_for(timeout: f64, mut predicate: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < end {
        settle(0.01);
        if predicate() {
            return true;
        }
    }
    predicate()
}

fn toplevel(name: &str) -> Option<gtk::Window> {
    gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.widget_name() == name && w.get_visible())
}

fn find(root: &impl IsA<gtk::Widget>, name: &str) -> Option<gtk::Widget> {
    let root = root.as_ref();
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if let Some(found) = find(&c, name) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

fn click(window: &gtk::Window, name: &str) -> Outcome {
    let button = find(window, name).and_downcast::<gtk::Button>().ok_or(format!("no button {name}"))?;
    ensure!(button.is_sensitive(), "{name} is insensitive");
    button.emit_clicked();
    settle(0.05);
    Ok(())
}

fn dialog(name: &str) -> Result<gtk::Window, String> {
    if !wait_for(20.0, || toplevel(name).is_some()) {
        return Err(format!("no {name}"));
    }
    Ok(toplevel(name).unwrap())
}

fn shot(name: &str) {
    let Ok(dir) = std::env::var("WAYARCHIVE_UI_SHOTS") else { return };
    if dir.is_empty() {
        return;
    }
    settle(0.6);
    let path = Path::new(&dir).join(name);
    let display = gdk::Display::default().unwrap();
    let monitor = display.monitors().item(0).and_downcast::<gdk::Monitor>().unwrap();
    let ok = std::process::Command::new("grim").arg("-o").arg(monitor.connector().unwrap().as_str()).arg(&path).status()
        .map(|s| s.success()).unwrap_or(false);
    println!("screenshot {} {}", path.display(), if ok { "saved" } else { "FAILED" });
}

fn expand_all(root: &gtk::Widget) {
    if let Some(e) = root.downcast_ref::<gtk::Expander>() {
        e.set_expanded(true);
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        expand_all(&c);
        child = c.next_sibling();
    }
}

fn have(tool: &str) -> bool {
    suite_core::shell::which(tool).is_some()
}

struct Fixture {
    app: App,
    window: ArchiveWindow,
    root: PathBuf,
}

impl Fixture {
    fn idle(&self) -> bool {
        !self.window.busy()
    }

    fn open(&self, path: &Path) -> Outcome {
        self.window.open(path);
        wait!(|| self.idle() && self.window.archive().is_some_and(|a| a.path == path));
        Ok(())
    }

    fn rows(&self) -> Vec<String> {
        let model = &self.window.selection;
        (0..model.n_items()).filter_map(|i| model.item(i)).filter_map(|o| archive_manager::ui::model::row_entry(&o)).map(|e| e.path()).collect()
    }

    fn expand_all(&self) {
        let mut i = 0;
        while i < self.window.selection.n_items() {
            if let Some(row) = self.window.selection.item(i).and_downcast::<gtk::TreeListRow>() {
                row.set_expanded(true);
            }
            i += 1;
        }
        settle(0.05);
    }

    fn select(&self, paths: &[&str]) {
        self.expand_all();
        self.window.select_paths(&paths.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        settle(0.05);
    }

    fn action(&self, name: &str) -> Outcome {
        let action = self.window.window.lookup_action(name).ok_or(format!("no action {name}"))?;
        ensure!(action.is_enabled(), "action {name} is disabled");
        action.activate(None);
        settle(0.05);
        Ok(())
    }

    fn enabled(&self, name: &str) -> bool {
        self.window.window.lookup_action(name).is_some_and(|a| a.is_enabled())
    }

    fn listing_paths(&self) -> Vec<String> {
        let mut p: Vec<String> = self.window.listing().entries.iter().map(|e| e.path.clone()).collect();
        p.sort();
        p
    }
}

/// a.txt, dir/b.txt, dir/sub/c.txt, picture.png
fn sources(dir: &Path) -> Vec<PathBuf> {
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("dir/sub")).unwrap();
    std::fs::write(src.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(src.join("dir/b.txt"), "bravo\n".repeat(200)).unwrap();
    std::fs::write(src.join("dir/sub/c.txt"), "charlie\n").unwrap();
    let pixels = glib::Bytes::from_owned(vec![255u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
    let texture = gdk::MemoryTexture::new(2, 2, gdk::MemoryFormat::R8g8b8, &pixels, 6);
    std::fs::write(src.join("picture.png"), &*texture.save_to_png_bytes()).unwrap();
    vec![src.join("a.txt"), src.join("dir"), src.join("picture.png")]
}

fn make(dir: &Path, name: &str, kind: Kind, options: CreateOptions) -> PathBuf {
    let path = dir.join(name);
    ops::create(&path, kind, &sources(dir), &options, &Settings::default(), &Ctx::new()).unwrap();
    path
}

// ------------------------------------------------------------------ checks

fn test_main_window_tree_columns_and_info_panel(f: &mut Fixture) -> Outcome {
    let zip = make(&f.root, "photos.zip", Kind::Zip, CreateOptions { comment: Some("Holiday pictures, 2026".into()), ..Default::default() });
    f.open(&zip)?;
    ensure_eq!(f.rows(), vec!["dir", "a.txt", "picture.png"]);
    let visible: Vec<&str> = f.window.columns.iter().filter(|(_, c)| c.is_visible()).map(|(k, _)| *k).collect();
    for key in ["name", "size", "compressed", "ratio", "type", "modified", "crc", "method", "version"] {
        ensure!(visible.contains(&key), "column {key} hidden: {visible:?}");
    }
    let buffer = f.window.info.comment.buffer();
    ensure_eq!(buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).as_str(), "Holiday pictures, 2026");
    f.select(&["dir/b.txt"]);
    ensure!(f.window.info.name.label() == "b.txt");
    ensure!(f.window.info.details.label().contains("1.2"), "{}", f.window.info.details.label());
    ensure!(f.window.status.label().contains("selected"), "{}", f.window.status.label());
    ensure!(f.enabled("delete") && f.enabled("rename") && f.enabled("preview"));
    // sort by size, descending: the folder stays first
    let size = f.window.columns.iter().find(|(k, _)| *k == "size").unwrap().1.clone();
    f.window.view.sort_by_column(Some(&size), gtk::SortType::Descending);
    settle(0.1);
    let rows = f.rows();
    ensure_eq!(rows[0], "dir".to_string());
    f.window.view.sort_by_column(Some(&f.window.columns[0].1), gtk::SortType::Ascending);
    f.window.window.set_default_size(1100, 640);
    f.select(&["dir/sub/c.txt"]);
    shot("main-window.png");
    Ok(())
}

fn test_filter_bar_shows_matches_with_their_folders(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "filter.tar.gz", Kind::TarGz, CreateOptions::default());
    f.open(&path)?;
    f.action("find")?;
    ensure!(f.window.search_bar.is_search_mode());
    f.window.search.set_text("c.txt");
    wait!(|| f.rows() == vec!["dir", "dir/sub", "dir/sub/c.txt"]);
    f.window.search.set_text("");
    wait!(|| f.rows().len() == 3);
    Ok(())
}

fn test_create_dialog_new_archive_with_password_and_header_encryption(f: &mut Fixture) -> Outcome {
    let src = sources(&f.root);
    f.window.new_archive(src.clone());
    let d = dialog("create-dialog")?;
    let name = find(&d, "create-name").and_downcast::<gtk::Entry>().ok_or("name")?;
    let types = find(&d, "create-type").and_downcast::<gtk::DropDown>().ok_or("type")?;
    let model = types.model().and_downcast::<gtk::StringList>().ok_or("type model")?;
    let index = (0..model.n_items()).find(|i| model.string(*i).is_some_and(|s| s.contains("(.7z)"))).ok_or("no 7z")?;
    types.set_selected(index);
    settle(0.05);
    ensure!(name.text().ends_with(".7z"), "{}", name.text());
    name.set_text("secret.7z");
    expand_all(d.upcast_ref());
    find(&d, "create-password").and_downcast::<gtk::PasswordEntry>().unwrap().set_text("hunter2");
    find(&d, "create-confirm").and_downcast::<gtk::PasswordEntry>().unwrap().set_text("hunter");
    settle(0.05);
    ensure!(!find(&d, "create-accept").unwrap().is_sensitive(), "mismatched passwords must block");
    find(&d, "create-confirm").and_downcast::<gtk::PasswordEntry>().unwrap().set_text("hunter2");
    find(&d, "create-encrypt-header").and_downcast::<gtk::CheckButton>().unwrap().set_active(true);
    shot("create-dialog.png");
    click(&d, "create-accept")?;
    let path = src[0].parent().unwrap().join("secret.7z");
    wait!(|| f.idle() && f.window.archive().is_some_and(|a| a.path == path));
    ensure!(f.listing_paths().contains(&"dir/sub/c.txt".to_string()), "{:?}", f.listing_paths());
    ensure_eq!(ops::open(&path, &Settings::default(), &Ctx::new()).err(), Some(archive_manager::archive::Error::PasswordNeeded));
    Ok(())
}

fn test_password_prompt_retries_after_a_wrong_password(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "locked.7z", Kind::SevenZip, CreateOptions { password: Some("right".into()), encrypt_header: true, ..Default::default() });
    f.window.open(&path);
    let d = dialog("password-dialog")?;
    ensure!(find(&d, "password-wrong").is_none());
    find(&d, "password-entry").and_downcast::<gtk::PasswordEntry>().unwrap().set_text("wrong");
    click(&d, "password-accept")?;
    wait!(|| toplevel("password-dialog").is_some_and(|w| find(&w, "password-wrong").is_some()));
    let d = toplevel("password-dialog").unwrap();
    find(&d, "password-entry").and_downcast::<gtk::PasswordEntry>().unwrap().set_text("right");
    click(&d, "password-accept")?;
    wait!(|| f.idle() && f.window.archive().is_some());
    let a = f.window.archive().unwrap();
    ensure!(a.listing.header_encrypted && a.password.as_deref() == Some("right"));
    // the known password is reused for extraction
    let out = f.root.join("locked-out");
    f.window.extract(None, out.clone(), ops::ExtractOptions { preserve_paths: true, ..Default::default() }, false, false);
    wait!(|| f.idle() && out.join("dir/sub/c.txt").exists());
    Ok(())
}

fn test_extract_dialog_selected_entries_into_a_subfolder(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "bundle.tar.xz", Kind::TarXz, CreateOptions::default());
    f.open(&path)?;
    f.select(&["dir/sub"]);
    f.action("extract")?;
    let d = dialog("extract-dialog")?;
    let dest = f.root.join("extract-dest");
    std::fs::create_dir_all(&dest).unwrap();
    find(&d, "extract-destination").and_downcast::<gtk::Entry>().unwrap().set_text(&dest.display().to_string());
    ensure!(find(&d, "extract-selected").and_downcast::<gtk::CheckButton>().unwrap().is_active());
    ensure!(find(&d, "extract-use-subfolder").and_downcast::<gtk::CheckButton>().unwrap().is_active(), "several top-level entries");
    ensure_eq!(find(&d, "extract-subfolder").and_downcast::<gtk::Entry>().unwrap().text().as_str(), "bundle");
    click(&d, "extract-accept")?;
    wait!(|| f.idle() && dest.join("bundle/dir/sub/c.txt").exists());
    ensure!(!dest.join("bundle/a.txt").exists());
    // Extract Here: next to the archive, in a subfolder
    f.action("extract-here")?;
    wait!(|| f.idle() && f.root.join("bundle/a.txt").exists());
    Ok(())
}

fn test_conflict_dialog_rename_and_overwrite_all(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "clash.zip", Kind::Zip, CreateOptions::default());
    f.open(&path)?;
    let dest = f.root.join("clash-dest");
    std::fs::create_dir_all(dest.join("dir")).unwrap();
    std::fs::write(dest.join("a.txt"), "existing a").unwrap();
    std::fs::write(dest.join("dir/b.txt"), "existing b").unwrap();
    f.window.extract(Some(vec!["a.txt".into()]), dest.clone(), ops::ExtractOptions { preserve_paths: true, ..Default::default() }, false, false);
    let d = dialog("conflict-dialog")?;
    ensure_eq!(find(&d, "conflict-name").and_downcast::<gtk::Entry>().unwrap().text().as_str(), "a (1).txt");
    d.set_default_size(620, -1);
    shot("conflict-dialog.png");
    click(&d, "conflict-rename")?;
    wait!(|| f.idle());
    ensure_eq!(std::fs::read_to_string(dest.join("a (1).txt")).unwrap(), "alpha\n");
    ensure_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "existing a");
    f.window.extract(None, dest.clone(), ops::ExtractOptions { preserve_paths: true, ..Default::default() }, false, false);
    let d = dialog("conflict-dialog")?;
    click(&d, "conflict-overwrite-all")?;
    wait!(|| f.idle());
    ensure_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "alpha\n");
    ensure!(std::fs::read_to_string(dest.join("dir/b.txt")).unwrap().starts_with("bravo"));
    // Cancel leaves the rest alone
    std::fs::write(dest.join("a.txt"), "again").unwrap();
    f.window.extract(Some(vec!["a.txt".into()]), dest.clone(), ops::ExtractOptions { preserve_paths: true, ..Default::default() }, false, false);
    let d = dialog("conflict-dialog")?;
    click(&d, "conflict-cancel")?;
    wait!(|| f.idle());
    ensure_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "again");
    ensure!(std::fs::read_dir(&dest).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".wayarchive")));
    Ok(())
}

fn accept_prompt(text: &str) -> Outcome {
    let d = dialog("prompt-dialog")?;
    find(&d, "prompt-entry").and_downcast::<gtk::Entry>().unwrap().set_text(text);
    click(&d, "prompt-accept")
}

fn test_edit_actions_rename_folder_delete_cut_paste_copy(f: &mut Fixture) -> Outcome {
    for (name, kind) in [("edit.zip", Kind::Zip), ("edit.tar.bz2", Kind::TarBz2)] {
        let path = make(&f.root, name, kind, CreateOptions::default());
        f.open(&path)?;
        f.select(&["a.txt"]);
        f.action("rename")?;
        accept_prompt("renamed.txt")?;
        wait!(|| f.idle() && f.listing_paths().contains(&"renamed.txt".to_string()));
        f.select(&["dir"]);
        f.action("new-folder")?;
        accept_prompt("inner")?;
        wait!(|| f.idle() && f.listing_paths().contains(&"dir/inner".to_string()));
        f.select(&["renamed.txt"]);
        f.action("cut")?;
        f.select(&["dir/inner"]);
        f.action("paste")?;
        wait!(|| f.idle() && f.listing_paths().contains(&"dir/inner/renamed.txt".to_string()) && !f.listing_paths().contains(&"renamed.txt".to_string()));
        f.select(&["picture.png"]);
        f.action("copy")?;
        f.select(&["dir"]);
        f.action("paste")?;
        wait!(|| f.idle() && f.listing_paths().contains(&"dir/picture.png".to_string()) && f.listing_paths().contains(&"picture.png".to_string()));
        f.select(&["dir/sub"]);
        f.action("delete")?;
        click(&dialog("confirm-dialog")?, "confirm-accept")?;
        wait!(|| f.idle() && !f.listing_paths().iter().any(|p| p.starts_with("dir/sub")));
        // add files into the selected folder
        let extra = f.root.join("extra.txt");
        std::fs::write(&extra, "extra").unwrap();
        f.window.add_files(vec![extra], "dir".into());
        wait!(|| f.idle() && f.listing_paths().contains(&"dir/extra.txt".to_string()));
        ensure_eq!(f.listing_paths(), vec!["dir", "dir/b.txt", "dir/extra.txt", "dir/inner", "dir/inner/renamed.txt", "dir/picture.png", "picture.png"]);
    }
    Ok(())
}

fn test_internal_preview_of_text_and_pictures(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "preview.zip", Kind::Zip, CreateOptions::default());
    f.open(&path)?;
    f.select(&["dir/sub/c.txt"]);
    f.action("preview")?;
    let p = dialog("preview-window")?;
    let view = find(&p, "preview-text").and_downcast::<gtk::TextView>().ok_or("no text view")?;
    let b = view.buffer();
    ensure_eq!(b.text(&b.start_iter(), &b.end_iter(), false).as_str(), "charlie\n");
    p.close();
    f.select(&["picture.png"]);
    f.action("preview")?;
    let p = dialog("preview-window")?;
    ensure!(find(&p, "preview-image").is_some());
    p.close();
    // the size limit refuses large entries
    f.app.update_settings(|s| s.preview_max_mb = 1);
    let big = f.root.join("big.txt");
    std::fs::write(&big, "x".repeat(2 * 1024 * 1024)).unwrap();
    f.window.add_files(vec![big], String::new());
    wait!(|| f.idle() && f.listing_paths().contains(&"big.txt".to_string()));
    f.select(&["big.txt"]);
    f.action("preview")?;
    let m = dialog("message-dialog")?;
    m.close();
    f.app.update_settings(|s| s.preview_max_mb = 50);
    Ok(())
}

fn test_open_with_external_app_and_update_after_saving(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "open.tar.gz", Kind::TarGz, CreateOptions::default());
    f.open(&path)?;
    let launched: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let sink = launched.clone();
    *f.app.hooks.launch.borrow_mut() = Some(Rc::new(move |file, _| sink.borrow_mut().push(file.path().unwrap())));
    f.select(&["dir/b.txt"]);
    f.action("open-entry")?;
    wait!(|| !launched.borrow().is_empty());
    let temp = launched.borrow()[0].clone();
    ensure!(temp.starts_with(std::env::var("XDG_RUNTIME_DIR").unwrap()), "{}", temp.display());
    settle(0.3);
    std::fs::write(&temp, "edited by an application\n").unwrap();
    let d = dialog("confirm-dialog")?;
    click(&d, "confirm-accept")?;
    wait!(|| f.idle() && f.window.listing().find("dir/b.txt").is_some_and(|e| e.size == 25));
    let out = f.root.join("open-out");
    let a = f.window.archive().unwrap();
    ops::extract(&a, Some(&["dir/b.txt".into()]), &out, &ops::ExtractOptions { preserve_paths: true, ..Default::default() }, &Ctx::new(), &mut |_| ops::Resolution::Skip).unwrap();
    ensure_eq!(std::fs::read_to_string(out.join("dir/b.txt")).unwrap(), "edited by an application\n");
    *f.app.hooks.launch.borrow_mut() = Some(record_launch());
    Ok(())
}

fn test_properties_test_and_comment(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "props.zip", Kind::Zip, CreateOptions::default());
    f.open(&path)?;
    f.action("properties")?;
    let d = dialog("properties-dialog")?;
    let sums = ops::checksums(&path, &Ctx::new()).unwrap();
    wait!(|| {
        let mut found = false;
        let mut stack = vec![d.clone().upcast::<gtk::Widget>()];
        while let Some(w) = stack.pop() {
            if w.downcast_ref::<gtk::Label>().is_some_and(|l| l.label() == sums[2]) {
                found = true;
            }
            let mut c = w.first_child();
            while let Some(x) = c {
                stack.push(x.clone());
                c = x.next_sibling();
            }
        }
        found
    });
    d.close();
    f.action("test")?;
    let m = dialog("message-dialog")?;
    m.close();
    f.action("comment")?;
    let c = dialog("comment-dialog")?;
    find(&c, "comment-text").and_downcast::<gtk::TextView>().unwrap().buffer().set_text("New comment\nline two");
    click(&c, "comment-save")?;
    wait!(|| f.idle() && f.window.listing().comment.as_deref() == Some("New comment\nline two"));
    Ok(())
}

fn rar4(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    let block = |body: &[u8]| -> Vec<u8> {
        let mut v = ((crc32(body) & 0xffff) as u16).to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    };
    let mut out = b"Rar!\x1a\x07\x00".to_vec();
    out.extend(block(&[0x73, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0]));
    for (name, data, dir) in files {
        let name = name.replace('/', "\\");
        let mut body = vec![0x74u8];
        body.extend_from_slice(&(0x8000u16 | if *dir { 0xe0 } else { 0 }).to_le_bytes());
        body.extend_from_slice(&((32 + name.len()) as u16).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.push(3);
        body.extend_from_slice(&crc32(data).to_le_bytes());
        body.extend_from_slice(&0x5d5d_6000u32.to_le_bytes());
        body.extend_from_slice(&[29, 0x30]);
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(&(if *dir { 0o40755u32 } else { 0o100644 }).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        out.extend(block(&body));
        out.extend_from_slice(data);
    }
    out.extend(block(&[0x7b, 0x00, 0x40, 7, 0]));
    out
}

fn test_read_only_formats_disable_editing(f: &mut Fixture) -> Outcome {
    let path = f.root.join("comic.cbr");
    std::fs::write(&path, rar4(&[("pages", b"", true), ("pages/01.txt", b"page\n", false)])).unwrap();
    f.open(&path)?;
    ensure_eq!(f.window.archive().unwrap().kind, Kind::Rar);
    f.select(&["pages/01.txt"]);
    for action in ["delete", "rename", "add-files", "add-folder", "new-folder", "cut", "comment"] {
        ensure!(!f.enabled(action), "{action} must be disabled for rar");
    }
    ensure!(f.enabled("extract") && f.enabled("preview") && f.enabled("test"));
    ensure!(f.window.status.label().contains("read-only"), "{}", f.window.status.label());
    Ok(())
}

fn test_settings_dialog_and_external_changes_apply_live(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "settings.zip", Kind::Zip, CreateOptions::default());
    f.open(&path)?;
    f.action("settings")?;
    let d = dialog("settings-dialog")?;
    let panel_visible = || f.window.paned.end_child().unwrap().get_visible();
    ensure!(panel_visible());
    // The first switch row is "Show information panel".
    let switch = {
        let mut stack = vec![d.clone().upcast::<gtk::Widget>()];
        let mut found = None;
        while let Some(w) = stack.pop() {
            if let Some(s) = w.downcast_ref::<gtk::Switch>() {
                if s.parent().and_then(|r| r.first_child()).and_then(|t| t.first_child()).and_downcast::<gtk::Label>()
                    .is_some_and(|l| l.label() == "Show information panel") {
                    found = Some(s.clone());
                }
            }
            let mut c = w.first_child();
            while let Some(x) = c {
                stack.push(x.clone());
                c = x.next_sibling();
            }
        }
        found.ok_or("no info panel switch")?
    };
    switch.set_active(false);
    wait!(|| !panel_visible());
    ensure!(!f.app.store.load().show_info_panel);
    shot("settings-dialog.png");
    click(&d, "settings-close")?;
    // Global Settings writes the same file: the window follows.
    let store = Store::new(Some(f.app.store.path.clone()));
    store.update(|s| s.show_info_panel = true).unwrap();
    wait!(|| panel_visible());
    store.update(|s| s.set_plugin_enabled("cli7z", false)).unwrap();
    wait!(|| !f.app.settings().plugin_enabled("cli7z"));
    f.open(&path)?;
    ensure_eq!(f.window.archive().unwrap().reader, "clizip");
    store.update(|s| s.set_plugin_enabled("cli7z", true)).unwrap();
    wait!(|| f.app.settings().plugin_enabled("cli7z"));
    Ok(())
}

fn test_global_settings_page_edits_the_same_file(f: &mut Fixture) -> Outcome {
    let page = archive_manager::settings::page::build_page(Some(f.app.store.path.clone()));
    ensure!(page.switches.len() == 7 && page.plugin_switches.len() == 5);
    page.switches["close_after_extraction"].set_active(true);
    ensure!(f.app.store.load().close_after_extraction);
    page.plugin_switches["clizip"].set_active(false);
    ensure!(!f.app.store.load().plugin_enabled("clizip"));
    wait!(|| !f.app.settings().plugin_enabled("clizip"));
    f.app.update_settings(|s| {
        s.close_after_extraction = false;
        s.set_plugin_enabled("clizip", true);
    });
    wait!(|| !page.switches["close_after_extraction"].is_active() && page.plugin_switches["clizip"].is_active());
    page.close();
    Ok(())
}

fn test_drops_on_an_empty_window_open_or_create(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "dropped.zip", Kind::Zip, CreateOptions::default());
    let empty = f.app.new_window();
    empty.window.present();
    empty.dropped_files(vec![path.clone()], String::new());
    wait!(|| !empty.busy() && empty.archive().is_some_and(|a| a.path == path));
    empty.window.close();
    let other = f.app.new_window();
    other.window.present();
    other.dropped_files(vec![f.root.join("src/a.txt"), f.root.join("src/dir")], String::new());
    let d = dialog("create-dialog")?;
    let name = find(&d, "create-name").and_downcast::<gtk::Entry>().unwrap();
    name.set_text("from-drop.tar.gz");
    let types = find(&d, "create-type").and_downcast::<gtk::DropDown>().unwrap();
    let model = types.model().and_downcast::<gtk::StringList>().unwrap();
    let index = (0..model.n_items()).find(|i| model.string(*i).is_some_and(|s| s.contains("(.tar.gz)"))).unwrap();
    types.set_selected(index);
    name.set_text("from-drop.tar.gz");
    click(&d, "create-accept")?;
    let created = f.root.join("src/from-drop.tar.gz");
    wait!(|| !other.busy() && other.archive().is_some_and(|a| a.path == created));
    ensure!(other.archive().unwrap().listing.find("dir/sub/c.txt").is_some());
    other.window.close();
    Ok(())
}

/// Files' own Ark drop client (file-manager/src/ark_drop.rs) against our D-Bus object.
struct ProviderDrop {
    content: gdk::ContentProvider,
    finished: Rc<RefCell<Vec<gdk::DragAction>>>,
}

impl wayfiles::ark_drop::DropSource for ProviderDrop {
    fn read(&self, mime: &'static str, _cancel: &gio::Cancellable, done: Box<dyn FnOnce(Result<(gio::InputStream, String), glib::Error>)>) {
        let content = self.content.clone();
        glib::spawn_future_local(async move {
            let out = gio::MemoryOutputStream::new_resizable();
            let result = content.write_mime_type_future(mime, &out, glib::Priority::DEFAULT).await;
            done(result.map(|_| {
                let bytes = out.steal_as_bytes();
                (gio::MemoryInputStream::from_bytes(&bytes).upcast(), mime.to_string())
            }));
        });
    }
    fn finish(&self, action: gdk::DragAction) {
        self.finished.borrow_mut().push(action);
    }
}

fn test_ark_dnd_protocol_with_files_client(f: &mut Fixture) -> Outcome {
    let path = make(&f.root, "dnd.7z", Kind::SevenZip, CreateOptions::default());
    f.open(&path)?;
    let archive = f.window.archive().unwrap();
    let content = f.window.drag_content_for(archive, vec!["dir/sub".into(), "dir/b.txt".into()]);
    ensure!(gio::dbus_is_unique_name(&content.service()), "{}", content.service());
    ensure!(content.object_path().starts_with("/DndExtract/"));
    let formats = content.upcast_ref::<gdk::ContentProvider>().formats();
    ensure!(formats.contain_mime_type(wayfiles::ark_drop::SERVICE) && formats.contain_mime_type(wayfiles::ark_drop::OBJECT));
    ensure!(formats.contain_mime_type("text/uri-list"));
    let dest = f.root.join("Drop Here");
    std::fs::create_dir_all(&dest).unwrap();
    let finished: Rc<RefCell<Vec<gdk::DragAction>>> = Rc::default();
    let errors: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = errors.clone();
    let drop = ProviderDrop { content: content.clone().upcast(), finished: finished.clone() };
    let _request = wayfiles::ark_drop::ExtractionDrop::new(Rc::new(drop), gio::File::for_path(&dest), Rc::new(move |e| sink.borrow_mut().push(e)));
    wait!(|| !finished.borrow().is_empty());
    ensure_eq!(finished.borrow().clone(), vec![gdk::DragAction::COPY]);
    ensure!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    wait!(|| f.idle() && dest.join("sub/c.txt").exists() && dest.join("b.txt").exists());
    ensure!(!dest.join("dir").exists(), "the dragged entries' parent folder is not recreated");
    ensure_eq!(f.window.dnd_calls.get(), 1);
    ensure_eq!(content.uri_list_requests(), 0);
    // a second drop asks about the existing files
    let finished2: Rc<RefCell<Vec<gdk::DragAction>>> = Rc::default();
    let drop = ProviderDrop { content: content.clone().upcast(), finished: finished2.clone() };
    let _request = wayfiles::ark_drop::ExtractionDrop::new(Rc::new(drop), gio::File::for_path(&dest), Rc::new(|_| {}));
    let d = dialog("conflict-dialog")?;
    click(&d, "conflict-skip-all")?;
    wait!(|| f.idle() && !finished2.borrow().is_empty());
    // the uri-list fallback stages lazily below XDG_RUNTIME_DIR
    let out = gio::MemoryOutputStream::new_resizable();
    let done = Rc::new(RefCell::new(None));
    let d2 = done.clone();
    let provider = content.clone().upcast::<gdk::ContentProvider>();
    let o = out.clone();
    glib::spawn_future_local(async move {
        let r = provider.write_mime_type_future("text/uri-list", &o, glib::Priority::DEFAULT).await;
        *d2.borrow_mut() = Some(r.is_ok());
    });
    wait!(|| done.borrow().is_some());
    ensure_eq!(*done.borrow(), Some(true));
    let text = String::from_utf8(out.steal_as_bytes().to_vec()).unwrap();
    let runtime = gio::File::for_path(std::env::var("XDG_RUNTIME_DIR").unwrap()).uri().to_string();
    ensure!(text.lines().count() == 2 && text.lines().all(|l| l.starts_with(&runtime)), "{text}");
    ensure!(text.contains("/sub\r\n") && text.contains("/b.txt\r\n"), "{text}");
    ensure_eq!(content.uri_list_requests(), 1);
    Ok(())
}

fn record_launch() -> Rc<dyn Fn(&gio::File, Option<&gio::AppInfo>)> {
    Rc::new(|file, _| println!("launch (not started): {}", file.uri()))
}

fn run_bin(args: &[&str], cwd: &Path) -> Result<i32, String> {
    let bin = std::env::var("WAYARCHIVE_BIN").map_err(|_| "WAYARCHIVE_BIN")?;
    let mut child = std::process::Command::new(&bin).args(args).current_dir(cwd).spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    let end = started + Duration::from_secs(60);
    let mut pressed = 0u64;
    loop {
        settle(0.05);
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(status.code().unwrap_or(-1));
        }
        // A job that failed shows an error dialog: Enter closes it (Close is the default button).
        let seconds = started.elapsed().as_secs();
        if seconds >= 4 && seconds / 2 > pressed {
            pressed = seconds / 2;
            let _ = std::process::Command::new("wtype").args(["-k", "Return"]).status();
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{args:?} did not finish"));
        }
    }
}

fn test_command_line_batch_and_add_modes(f: &mut Fixture) -> Outcome {
    let zip = make(&f.root, "cli.zip", Kind::Zip, CreateOptions::default());
    let single = f.root.join("single-folder.tar.gz");
    ops::create(&single, Kind::TarGz, &[f.root.join("src/dir")], &CreateOptions::default(), &Settings::default(), &Ctx::new()).unwrap();
    let dest = f.root.join("batch-dest");
    std::fs::create_dir_all(&dest).unwrap();
    let code = run_bin(&["--batch", "--autosubfolder", "--destination", &dest.display().to_string(), &zip.display().to_string(),
                         &single.display().to_string()], &f.root)?;
    ensure_eq!(code, 0);
    ensure!(dest.join("cli/dir/sub/c.txt").exists(), "subfolder named after the archive");
    ensure!(dest.join("dir/sub/c.txt").exists(), "a single top-level folder needs no subfolder");
    // --batch without --destination: the current folder
    let here = f.root.join("batch-here");
    std::fs::create_dir_all(&here).unwrap();
    ensure_eq!(run_bin(&["-b", &zip.display().to_string()], &here)?, 0);
    ensure!(here.join("a.txt").exists());
    // --add-to with --changetofirstpath
    let target = f.root.join("added.tar.xz");
    let code = run_bin(&["--add-to", &target.display().to_string(), "--changetofirstpath", &f.root.join("src/a.txt").display().to_string(),
                         &f.root.join("src/dir/sub").display().to_string()], &f.root)?;
    ensure_eq!(code, 0);
    let a = ops::open(&target, &Settings::default(), &Ctx::new()).unwrap();
    let mut p: Vec<String> = a.listing.entries.iter().map(|e| e.path.clone()).collect();
    p.sort();
    ensure_eq!(p, vec!["a.txt", "dir", "dir/sub", "dir/sub/c.txt"]);
    // --add --autofilename
    ensure_eq!(run_bin(&["--add", "--autofilename", "zip", &f.root.join("src/dir").display().to_string()], &f.root)?, 0);
    ensure!(f.root.join("src/dir.zip").exists());
    ensure_eq!(run_bin(&["-c", "-f", "zip", &f.root.join("src/dir").display().to_string()], &f.root)?, 0);
    ensure!(f.root.join("src/dir_1.zip").exists());
    // failures exit non-zero
    ensure!(run_bin(&["--batch", &f.root.join("missing.zip").display().to_string()], &f.root)? != 0);
    Ok(())
}

// ---------------------------------------------------- real cross-process DnD

mod pointer {
    use wayland_client::protocol::{wl_registry, wl_seat};
    use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
    use wayland_protocols_wlr::virtual_pointer::v1::client::{zwlr_virtual_pointer_manager_v1 as manager, zwlr_virtual_pointer_v1 as vp};

    #[derive(Default)]
    struct State {
        manager: Option<manager::ZwlrVirtualPointerManagerV1>,
        seat: Option<wl_seat::WlSeat>,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { name, interface, version } = event {
                match interface.as_str() {
                    "zwlr_virtual_pointer_manager_v1" => state.manager = Some(registry.bind(name, version.min(2), qh, ())),
                    "wl_seat" if state.seat.is_none() => state.seat = Some(registry.bind(name, version.min(5), qh, ())),
                    _ => {}
                }
            }
        }
    }
    delegate_noop!(State: ignore wl_seat::WlSeat);
    delegate_noop!(State: manager::ZwlrVirtualPointerManagerV1);
    delegate_noop!(State: vp::ZwlrVirtualPointerV1);

    pub struct Pointer {
        connection: Connection,
        pointer: vp::ZwlrVirtualPointerV1,
        extent: (u32, u32),
        time: u32,
    }

    impl Pointer {
        pub fn new(extent: (u32, u32)) -> Result<Pointer, String> {
            let connection = Connection::connect_to_env().map_err(|e| e.to_string())?;
            let mut queue = connection.new_event_queue();
            let qh = queue.handle();
            connection.display().get_registry(&qh, ());
            let mut state = State::default();
            queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
            let manager = state.manager.clone().ok_or("no zwlr_virtual_pointer_manager_v1")?;
            let pointer = manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());
            queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
            Ok(Pointer { connection, pointer, extent, time: 1 })
        }
        fn tick(&mut self) -> u32 {
            self.time += 16;
            self.time
        }
        pub fn move_to(&mut self, x: f64, y: f64) {
            let t = self.tick();
            self.pointer.motion_absolute(t, x as u32, y as u32, self.extent.0, self.extent.1);
            self.pointer.frame();
            let _ = self.connection.flush();
        }
        pub fn button(&mut self, pressed: bool) {
            let t = self.tick();
            self.pointer.button(t, 0x110, if pressed { wayland_client::protocol::wl_pointer::ButtonState::Pressed } else { wayland_client::protocol::wl_pointer::ButtonState::Released });
            self.pointer.frame();
            let _ = self.connection.flush();
        }
    }
}

fn test_real_drag_from_the_archive_into_files(f: &mut Fixture) -> Outcome {
    let Ok(wayfiles) = std::env::var("WAYFILES_BIN") else { return Err("WAYFILES_BIN".into()) };
    let path = make(&f.root, "dnd-source.zip", Kind::Zip, CreateOptions::default());
    let display = gdk::Display::default().unwrap();
    let monitor = display.monitors().item(0).and_downcast::<gdk::Monitor>().unwrap();
    let geometry = monitor.geometry();
    ensure!(geometry.width() >= 1270, "output too small: {}x{}", geometry.width(), geometry.height());
    let window = f.app.new_window();
    window.window.set_title(Some("dnd-source"));
    window.window.set_default_size(620, 700);
    window.window.present();
    window.open(&path);
    wait!(|| !window.busy() && window.archive().is_some());
    let dest = f.root.join("Files Destination");
    std::fs::create_dir_all(&dest).unwrap();
    let mut files = std::process::Command::new(&wayfiles).arg(&dest).spawn().map_err(|e| e.to_string())?;
    let result = (|| -> Outcome {
        wait!(|| std::process::Command::new("wlrctl").args(["toplevel", "find", "app_id:org.waydock.Files"]).status().is_ok_and(|s| s.success()));
        settle(2.0);
        // the row of a.txt in our window
        let rows = (0..window.selection.n_items()).filter_map(|i| window.selection.item(i)).filter_map(|o| archive_manager::ui::model::row_entry(&o))
            .position(|e| e.path() == "a.txt").ok_or("no a.txt row")?;
        let _ = rows;
        let label = {
            let mut stack = vec![window.view.clone().upcast::<gtk::Widget>()];
            let mut found = None;
            while let Some(w) = stack.pop() {
                if w.downcast_ref::<gtk::Label>().is_some_and(|l| l.label() == "a.txt" && l.is_mapped()) {
                    found = Some(w.clone());
                }
                let mut c = w.first_child();
                while let Some(x) = c {
                    stack.push(x.clone());
                    c = x.next_sibling();
                }
            }
            found.ok_or("a.txt label not mapped")?
        };
        let bounds = label.compute_bounds(&window.window).ok_or("no bounds")?;
        let (x, y) = ((bounds.x() + 10.0) as f64, (bounds.y() + bounds.height() / 2.0) as f64);
        // Start on an unselected row and move right away, as a user does: the
        // drag must not need a first click to select the row.
        window.selection.unselect_all();
        let mut pointer = pointer::Pointer::new((geometry.width() as u32, geometry.height() as u32))?;
        pointer.move_to(x, y);
        settle(0.3);
        pointer.button(true);
        settle(0.01);
        // Small first steps like a real mouse, then the long way to Files.
        for step in 1..=12 {
            pointer.move_to(x + step as f64, y);
            settle(0.01);
        }
        for step in 1..=30 {
            pointer.move_to(x + (960.0 - x) * step as f64 / 30.0, y + (350.0 - y) * step as f64 / 30.0);
            settle(0.03);
        }
        settle(0.5);
        shot("drag-into-files.png");
        pointer.button(false);
        let content = window.drag.borrow().clone();
        wait!(|| dest.join("a.txt").exists() && !window.busy());
        let fallback = content.map(|c| c.uri_list_requests()).unwrap_or(0);
        println!("dnd: Files used {} (D-Bus calls {}, uri-list requests {fallback})",
                 if window.dnd_calls.get() > 0 { "the Ark protocol" } else { "the uri-list fallback" }, window.dnd_calls.get());
        ensure_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "alpha\n");
        ensure!(window.dnd_calls.get() == 1 && fallback == 0, "Files took the file list instead of the Ark formats");
        Ok(())
    })();
    let _ = files.kill();
    let _ = files.wait();
    window.window.close();
    result
}

type Check = fn(&mut Fixture) -> Outcome;

fn checks() -> Vec<(&'static str, Check)> {
    vec![
        ("test_main_window_tree_columns_and_info_panel", test_main_window_tree_columns_and_info_panel),
        ("test_filter_bar_shows_matches_with_their_folders", test_filter_bar_shows_matches_with_their_folders),
        ("test_create_dialog_new_archive_with_password_and_header_encryption", test_create_dialog_new_archive_with_password_and_header_encryption),
        ("test_password_prompt_retries_after_a_wrong_password", test_password_prompt_retries_after_a_wrong_password),
        ("test_extract_dialog_selected_entries_into_a_subfolder", test_extract_dialog_selected_entries_into_a_subfolder),
        ("test_conflict_dialog_rename_and_overwrite_all", test_conflict_dialog_rename_and_overwrite_all),
        ("test_edit_actions_rename_folder_delete_cut_paste_copy", test_edit_actions_rename_folder_delete_cut_paste_copy),
        ("test_internal_preview_of_text_and_pictures", test_internal_preview_of_text_and_pictures),
        ("test_open_with_external_app_and_update_after_saving", test_open_with_external_app_and_update_after_saving),
        ("test_properties_test_and_comment", test_properties_test_and_comment),
        ("test_read_only_formats_disable_editing", test_read_only_formats_disable_editing),
        ("test_settings_dialog_and_external_changes_apply_live", test_settings_dialog_and_external_changes_apply_live),
        ("test_global_settings_page_edits_the_same_file", test_global_settings_page_edits_the_same_file),
        ("test_drops_on_an_empty_window_open_or_create", test_drops_on_an_empty_window_open_or_create),
        ("test_ark_dnd_protocol_with_files_client", test_ark_dnd_protocol_with_files_client),
        ("test_command_line_batch_and_add_modes", test_command_line_batch_and_add_modes),
        ("test_real_drag_from_the_archive_into_files", test_real_drag_from_the_archive_into_files),
    ]
}

fn labels(root: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(l) = root.downcast_ref::<gtk::Label>() {
        if !l.label().is_empty() {
            out.push(l.label().to_string());
        }
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        labels(&c, out);
        child = c.next_sibling();
    }
}

fn dump_windows() {
    for w in gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok()) {
        if w.get_visible() && w.widget_name() != "archive-window" {
            let mut out = Vec::new();
            labels(w.upcast_ref(), &mut out);
            println!("  open window {}: {:?}", w.widget_name(), out.iter().take(8).collect::<Vec<_>>());
        }
    }
}

fn close_strays() {
    for w in gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok()) {
        if w.get_visible() && w.widget_name() != "archive-window" {
            w.close();
        }
    }
    settle(0.1);
}

fn main() {
    if std::env::var("WAYARCHIVE_UI_ISOLATED_TEST").as_deref() != Ok("1") {
        println!("ui_headless: skipped (run tests/run_headless.sh for the private compositor)");
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if runtime == "/run/user/1000" || display.is_empty() || !std::env::temp_dir().to_string_lossy().contains("/cbt/rust-archive-manager/") {
        eprintln!("ui_headless: refusing to run outside the private compositor");
        std::process::exit(1);
    }
    let filters: Vec<String> = std::env::args().skip(1).collect();
    suite_core::i18n::initialize();
    gtk::init().expect("gtk");
    let config = std::env::temp_dir().join("config/archive-manager.json");
    let app = App::new(gio::ApplicationFlags::HANDLES_COMMAND_LINE, Store::new(Some(config)));
    app.app.register(None::<&gio::Cancellable>).expect("register on the private bus");
    assert!(!app.app.is_remote());
    let _hold = app.app.hold();
    // A persistent virtual keyboard so the seat has one.
    let mut keyboard = std::process::Command::new("wtype").args(["-s", "600000"]).spawn().ok();
    // Never start a real application from a test.
    *app.hooks.launch.borrow_mut() = Some(record_launch());
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let sink = opened.clone();
    *app.hooks.open_folder.borrow_mut() = Some(Rc::new(move |p| sink.borrow_mut().push(p.to_path_buf())));
    let (mut failures, mut ran) = (0, 0);
    for (name, check) in checks() {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        if name.contains("sevenzip") && !have("7z") {
            continue;
        }
        ran += 1;
        let root = std::env::temp_dir().join(format!("case-{ran}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let window = app.new_window();
        window.window.present();
        wait_for(5.0, || window.window.is_mapped());
        settle(0.3);
        let mut fixture = Fixture { app: app.clone(), window, root };
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&mut fixture)));
        let outcome = match result {
            Ok(Ok(())) => "ok".to_string(),
            Ok(Err(error)) => {
                failures += 1;
                dump_windows();
                format!("FAILED: {error}")
            }
            Err(panic) => {
                failures += 1;
                format!("PANICKED: {:?}", panic.downcast_ref::<String>().cloned().or(panic.downcast_ref::<&str>().map(|s| s.to_string())))
            }
        };
        println!("test {name} ... {outcome} ({:.1}s)", started.elapsed().as_secs_f64());
        close_strays();
        fixture.window.window.close();
        settle(0.1);
    }
    if let Some(k) = keyboard.as_mut() {
        let _ = k.kill();
        let _ = k.wait();
    }
    println!("ui_headless: {} passed, {failures} failed", ran - failures);
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
