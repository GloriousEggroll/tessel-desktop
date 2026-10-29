//! test_file_model.py (FileModelTests, ItemCountTests).

use super::*;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Duration;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        crate::test_env();
        let root = std::env::temp_dir().join(format!("files-model-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn item(&self, name: &str, text: &str, folder: bool) -> FileItem {
        let path = self.root.join(name);
        if folder {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, text).unwrap();
        }
        FileItem::query(&gio::File::for_path(&path), None).unwrap()
    }

    fn file(&self, name: &str) -> FileItem {
        self.item(name, "", false)
    }

    fn dir(&self) -> gio::File {
        gio::File::for_path(&self.root)
    }

    fn search(&self, query: &str, options: SearchOptions) -> (SearchLoader, Vec<FileItem>) {
        let loader = SearchLoader::new(1, 32);
        let items: Rc<RefCell<Vec<FileItem>>> = Rc::default();
        let errors: Rc<RefCell<Vec<Option<glib::Error>>>> = Rc::default();
        run_loop(|main_loop| {
            let (sink, errs) = (items.clone(), errors.clone());
            loader.start(&self.dir(), query, move |batch| sink.borrow_mut().extend(batch), move |error| {
                errs.borrow_mut().push(error);
                main_loop.quit();
            }, options);
        });
        assert_eq!(errors.borrow().len(), 1);
        assert!(errors.borrow()[0].is_none(), "{:?}", errors.borrow()[0]);
        let items = items.borrow().clone();
        (loader, items)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `run_loop(start)`: a private main context with an 8 s timeout.
fn run_loop(start: impl FnOnce(glib::MainLoop)) {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&context), false);
            let timed_out = Rc::new(Cell::new(false));
            let (flag, quit) = (timed_out.clone(), main_loop.clone());
            let timer = suite_core::timer::once(8000, move || {
                flag.set(true);
                quit.quit();
            });
            start(main_loop.clone());
            main_loop.run();
            if timer.is_pending() {
                timer.cancel();
            }
            assert!(!timed_out.get(), "Asynchronous file operation did not complete");
        })
        .unwrap();
}

fn names(items: &[FileItem]) -> Vec<String> {
    items.iter().map(FileItem::name).collect()
}

fn themed_names(icon: &gio::Icon) -> Vec<String> {
    icon.clone().downcast::<gio::ThemedIcon>().expect("themed icon").names().iter().map(|n| n.to_string()).collect()
}

fn store_of(items: &[&FileItem]) -> gio::ListStore {
    let store = gio::ListStore::new::<FileItem>();
    let items: Vec<FileItem> = items.iter().map(|i| (*i).clone()).collect();
    store.splice(0, 0, &items);
    store
}

fn store_item(store: &gio::ListStore, index: u32) -> FileItem {
    store.item(index).unwrap().downcast().unwrap()
}

fn register_hook(root: &Path, hook: impl Fn(&Path) + Send + Sync + 'static) {
    DESKTOP_HOOKS.lock().unwrap().push((root.to_path_buf(), Arc::new(hook)));
}

fn unregister_hook(root: &Path) {
    DESKTOP_HOOKS.lock().unwrap().retain(|(p, _)| p != root);
}

#[test]
fn file_info_remote_identity_and_local_metadata() {
    let fx = Fixture::new("identity");
    let item = fx.item("notes.txt", "hello", false);
    assert_eq!(item.size(), 5);
    assert_eq!(item.path().unwrap(), fx.root.join("notes.txt"));
    assert_eq!(item.uri(), gio::File::for_path(fx.root.join("notes.txt")).uri());
    assert!(item.uri().starts_with("file:///"));
    assert!(item.readable());
    assert!(item.writeable() && item.writable());
    assert!(!item.is_dir());
    assert!(item.modified() > 0);
    let _icon: gio::Icon = item.icon();
    let info = gio::FileInfo::new();
    info.set_name("Folder");
    info.set_display_name("Folder");
    info.set_file_type(gio::FileType::Directory);
    let remote = FileItem::new(&gio::File::for_uri("sftp://example.invalid/Folder"), &info);
    assert!(remote.path().is_none());
    assert_eq!(remote.uri(), "sftp://example.invalid/Folder");
    assert!(remote.is_dir());
    assert!(!remote.writeable());
    assert_eq!(remote.content_type(), "application/octet-stream");
    assert_eq!(themed_names(&remote.icon())[0], "folder");
    assert_eq!(remote.type_description(), "Folder");
}

#[test]
fn hidden_and_symlink_metadata() {
    let fx = Fixture::new("hidden");
    fx.file(".hidden");
    let directory = fx.item("target", "", true);
    std::os::unix::fs::symlink(directory.path().unwrap(), fx.root.join("link")).unwrap();
    let hidden = FileItem::query(&gio::File::for_path(fx.root.join(".hidden")), None).unwrap();
    let link = FileItem::query(&gio::File::for_path(fx.root.join("link")), None).unwrap();
    assert!(hidden.is_hidden());
    assert!(link.is_symlink());
    assert!(link.is_dir());
}

#[test]
fn desktop_entry_uses_themed_icon_without_executing_launcher() {
    let fx = Fixture::new("desktop");
    let marker = fx.root.join("must-not-run");
    let item = fx.item(
        "Terminal.desktop",
        &format!("[Desktop Entry]\nType=Application\nName=Terminal\nIcon=utilities-terminal\nExec=touch {}\n", marker.display()),
        false,
    );
    let names = themed_names(&item.icon());
    assert_eq!(names[0], "utilities-terminal");
    for name in themed_names(&item.info().icon().unwrap()) {
        assert!(names.contains(&name), "{name}");
    }
    assert!(!marker.exists());
    assert_eq!(item.display_name(), "Terminal.desktop");
}

#[test]
fn desktop_entry_absolute_icon_and_malformed_fallbacks() {
    let fx = Fixture::new("absolute");
    let picture = fx.root.join("custom icon.svg");
    std::fs::write(&picture, r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"/>"#).unwrap();
    let item = fx.item("Custom.desktop", &format!("[Desktop Entry]\nType=Application\nIcon={}\n", picture.display()), false);
    let file_icon = item.icon().downcast::<gio::FileIcon>().expect("file icon");
    assert_eq!(file_icon.file().path().unwrap(), picture);
    let cases = [
        "not an entry".to_owned(),
        "[Desktop Entry]\nType=Application\n".to_owned(),
        "[Other Group]\nType=Application\nIcon=folder\n".to_owned(),
        "[Desktop Entry]\nType=Other\nIcon=folder\n".to_owned(),
        "[Desktop Entry]\nType=Application\nIcon=relative/icon.svg\n".to_owned(),
        format!("[Desktop Entry]\nType=Application\nIcon={}.missing\n", picture.display()),
    ];
    for (index, data) in cases.iter().enumerate() {
        let malformed = fx.item(&format!("bad{index}.desktop"), data, false);
        assert!(malformed.icon().equal(Some(&malformed.info().icon().unwrap())), "{data}");
    }
}

#[test]
fn desktop_entry_reads_are_bounded_and_non_launchers_unchanged() {
    let fx = Fixture::new("bounded");
    let text = "[Desktop Entry]\nType=Application\nIcon=utilities-terminal\n";
    let oversized = fx.item("huge.desktop", &format!("{text}{}", "#".repeat(DESKTOP_ENTRY_LIMIT)), false);
    assert!(oversized.icon().equal(Some(&oversized.info().icon().unwrap())));
    let plain = fx.item("regular.txt", text, false);
    assert!(plain.icon().equal(Some(&plain.info().icon().unwrap())));
    let info = plain.info().dup();
    info.set_name("remote.desktop");
    info.set_size(text.len() as i64);
    let remote = FileItem::new(&gio::File::for_uri("sftp://example.invalid/remote.desktop"), &info);
    load_desktop_icon(&remote, None);
    assert!(remote.icon().equal(Some(&remote.info().icon().unwrap())));
    // The pure parser on its own.
    assert_eq!(desktop_icon(text.as_bytes()), Some(DesktopIcon::Themed("utilities-terminal".into())));
    assert_eq!(desktop_icon(b"[Desktop Entry]\nType=Link\nIcon=  web  \n"), Some(DesktopIcon::Themed("web".into())));
    assert_eq!(desktop_icon(b"[Desktop Entry]\nType=Application\nIcon=a:b\n"), None);
    assert_eq!(desktop_icon(b"\xff\xfe"), None);
}

#[test]
fn desktop_icon_enrichment_runs_off_main_loop_and_search_matches() {
    let fx = Fixture::new("offmain");
    fx.item("Example.desktop", "[Desktop Entry]\nType=Application\nIcon=utilities-terminal\n", false);
    let main_thread = std::thread::current().id();
    let workers: Arc<Mutex<Vec<std::thread::ThreadId>>> = Arc::default();
    let sink = workers.clone();
    register_hook(&fx.root, move |_| sink.lock().unwrap().push(std::thread::current().id()));
    let items: Rc<RefCell<Vec<FileItem>>> = Rc::default();
    let errors: Rc<RefCell<Vec<Option<glib::Error>>>> = Rc::default();
    run_loop(|main_loop| {
        let (sink, errs) = (items.clone(), errors.clone());
        DirectoryLoader::default().start(&fx.dir(), move |batch| sink.borrow_mut().extend(batch), move |error| {
            errs.borrow_mut().push(error);
            main_loop.quit();
        });
    });
    unregister_hook(&fx.root);
    assert_eq!(errors.borrow().len(), 1);
    assert!(errors.borrow()[0].is_none());
    let workers = workers.lock().unwrap().clone();
    assert!(!workers.is_empty());
    assert!(!workers.contains(&main_thread));
    assert_eq!(themed_names(&items.borrow()[0].icon())[0], "utilities-terminal");
    let (_, searched) = fx.search("Example", SearchOptions::default());
    assert_eq!(themed_names(&searched[0].icon())[0], "utilities-terminal");
}

#[test]
fn desktop_icon_edits_replace_only_affected_entry() {
    let fx = Fixture::new("edits");
    let prefix = "[Desktop Entry]\nType=Application\nIcon=";
    let launcher = fx.item("Edit.desktop", &format!("{prefix}folder-documents\n"), false);
    let untouched = fx.file("unchanged.txt");
    let store = store_of(&[&launcher, &untouched]);
    assert!(!reconcile_items(&store, &[FileItem::query(&launcher.file(), None).unwrap()], false));
    std::fs::write(launcher.path().unwrap(), format!("{prefix}utilities-terminal\n")).unwrap();
    let replacement = FileItem::query(&launcher.file(), None).unwrap();
    assert!(reconcile_items(&store, &[replacement], false));
    assert_eq!(themed_names(&store_item(&store, 0).icon())[0], "utilities-terminal");
    assert_eq!(store_item(&store, 1), untouched);
    std::fs::write(launcher.path().unwrap(), "broken desktop entry").unwrap();
    let replacement = FileItem::query(&launcher.file(), None).unwrap();
    reconcile_items(&store, std::slice::from_ref(&replacement), false);
    assert!(store_item(&store, 0).icon().equal(Some(&replacement.info().icon().unwrap())));
}

#[test]
fn navigation_cancels_inflight_desktop_icon_metadata() {
    let fx = Fixture::new("navigate");
    let (first, second) = (fx.root.join("first"), fx.root.join("second"));
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    std::fs::write(first.join("Old.desktop"), "[Desktop Entry]\nType=Application\nIcon=folder\n").unwrap();
    std::fs::write(second.join("New.txt"), "new folder").unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (s, r) = (started.clone(), release.clone());
    register_hook(&fx.root, move |_| {
        s.store(true, AtomicOrdering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !r.load(AtomicOrdering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let loader = DirectoryLoader::default();
    let old_items: Rc<RefCell<Vec<FileItem>>> = Rc::default();
    let old_done: Rc<Cell<u32>> = Rc::default();
    let new_items: Rc<RefCell<Vec<FileItem>>> = Rc::default();
    run_loop(|main_loop| {
        let (sink, done) = (old_items.clone(), old_done.clone());
        loader.start(&gio::File::for_path(&first), move |batch| sink.borrow_mut().extend(batch), move |_| done.set(done.get() + 1));
        let (loader, new_items, started, release, second) = (loader.clone(), new_items.clone(), started.clone(), release.clone(), second.clone());
        suite_core::timer::repeat(10, move || {
            if !started.load(AtomicOrdering::SeqCst) {
                return true;
            }
            let sink = new_items.clone();
            let quit = main_loop.clone();
            loader.start(&gio::File::for_path(&second), move |batch| sink.borrow_mut().extend(batch), move |_| {
                suite_core::timer::once(50, move || quit.quit());
            });
            release.store(true, AtomicOrdering::SeqCst);
            false
        });
    });
    release.store(true, AtomicOrdering::SeqCst);
    unregister_hook(&fx.root);
    assert!(old_items.borrow().is_empty());
    assert_eq!(old_done.get(), 0);
    assert_eq!(names(&new_items.borrow()), ["New.txt"]);
}

#[test]
fn sorting_natural_case_insensitive_and_folders_first() {
    let fx = Fixture::new("sorting");
    let items = [fx.file("file10"), fx.file("File2"), fx.file("alpha"), fx.item("z-folder", "", true)];
    assert_eq!(names(&sort_items(&items, "name", false, true, true)), ["z-folder", "alpha", "File2", "file10"]);
    assert_eq!(names(&sort_items(&items, "name", true, true, true)), ["z-folder", "file10", "File2", "alpha"]);
    assert_eq!(sort_items(&items, "name", false, false, true).last().unwrap().name(), "z-folder");
    // Plain (non-natural) order compares digits as text.
    assert_eq!(names(&sort_items(&items, "name", false, true, false)), ["z-folder", "alpha", "file10", "File2"]);
}

#[test]
fn sort_by_size_and_modified() {
    let fx = Fixture::new("size");
    let small = fx.item("small.txt", "s", false);
    let large = fx.item("large.txt", "123456789", false);
    assert_eq!(sort_items(&[large.clone(), small.clone()], "size", false, true, true), [small.clone(), large.clone()]);
    let file = std::fs::OpenOptions::new().write(true).open(small.path().unwrap()).unwrap();
    file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(100)).unwrap();
    let small = FileItem::query(&small.file(), None).unwrap();
    assert_eq!(small.modified(), 100);
    assert_eq!(sort_items(&[large.clone(), small.clone()], "modified", false, true, true), [small, large]);
}

#[test]
fn natural_keys_and_globs() {
    assert_eq!(natural_key("File10b"), [
        NaturalPart::Text("file".into()),
        NaturalPart::Number("10".into()),
        NaturalPart::Text("b".into())
    ]);
    assert!(natural_key("a2") < natural_key("a10"));
    assert_eq!(natural_key("a007").cmp(&natural_key("a7")), Ordering::Equal);
    assert!(natural_key("a99999999999999999999999") > natural_key("a9999999999999999999999"));
    // Non-ASCII decimal digits (Arabic-Indic ٣ = 3, fullwidth ２ = 2).
    assert_eq!(natural_key("x٣"), natural_key("x3"));
    assert_eq!(natural_key("x２"), natural_key("x2"));
    assert!(fnmatchcase("notes.txt", "*.txt"));
    assert!(fnmatchcase("a1", "a[0-9]"));
    assert!(!fnmatchcase("ab", "a[!b]"));
    assert!(fnmatchcase("a]", "a[]]"));
    assert!(fnmatchcase("a[", "a["));
    assert!(fnmatchcase("abc", "a?c"));
    assert!(fnmatchcase("aXbYc", "a*b*c"));
    assert!(!fnmatchcase("aXbY", "a*b*c"));
    assert!(fnmatchcase("", "*"));
}

#[test]
fn search_matching_and_human_units() {
    assert!(name_matches("Notes.TXT", "*.txt"));
    assert!(name_matches("OBS Studio", "obs"));
    assert!(name_matches("anything", ""));
    assert!(!name_matches("notes.jpg", "*.txt"));
    assert!(name_matches("Straße", "STRASSE"));
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(1023), "1,023 B");
    assert_eq!(format_size(1024), "1.0 KiB");
    assert_eq!(format_size(1024u64.pow(3)), "1.0 GiB");
    assert_eq!(format_size(1536 * 1024u64.pow(2)), "1.5 GiB");
    assert_eq!(format_size(u64::MAX), "16.0 EiB");
    assert_eq!(format_size(2 * 1024u64.pow(6)), "2.0 EiB");
    assert_eq!(format_modified(0), "—");
    assert_ne!(format_modified(1), "—");
    assert_eq!(format_modified(u64::MAX), "—");
}

#[test]
fn archive_uri_preserves_spaces_unicode_and_remote_identity() {
    crate::test_env();
    let path = "/tmp/My file ☃.zip";
    let expected = "archive://file%253A%252F%252F%252Ftmp%252FMy%252520file%252520%2525E2%252598%252583.zip/";
    assert_eq!(archive_uri(path).unwrap(), expected);
    assert_eq!(archive_uri(&gio::File::for_path(path).uri()).unwrap(), expected);
    assert_eq!(archive_uri_for_file(&gio::File::for_path(path)), expected);
    assert_eq!(archive_uri("sftp://server/work/archive.zip").unwrap(), "archive://sftp%253A%252F%252Fserver%252Fwork%252Farchive.zip/");
    assert_eq!(archive_uri("").unwrap_err(), "Choose an archive file");
}

type Changes = Rc<RefCell<Vec<(u32, u32, u32)>>>;

fn watch(store: &gio::ListStore) -> Changes {
    let changes: Changes = Rc::default();
    let sink = changes.clone();
    store.connect_items_changed(move |_, position, removed, added| sink.borrow_mut().push((position, removed, added)));
    changes
}

#[test]
fn reconciliation_retains_unchanged_identity_and_ignores_enumeration_order() {
    let fx = Fixture::new("reconcile-order");
    let (first, second) = (fx.file("first.txt"), fx.file("second.txt"));
    let store = store_of(&[&first, &second]);
    let changes = watch(&store);
    let incoming = [FileItem::query(&second.file(), None).unwrap(), FileItem::query(&first.file(), None).unwrap()];
    assert!(!reconcile_items(&store, &incoming, true));
    assert_eq!(store_item(&store, 0), first);
    assert_eq!(store_item(&store, 1), second);
    assert!(changes.borrow().is_empty());
}

#[test]
fn reconciliation_inserts_removes_and_replaces_only_changed_rows() {
    let fx = Fixture::new("reconcile-change");
    let unchanged = fx.item("unchanged.txt", "stay", false);
    let changed = fx.item("changed.log", "old", false);
    let removed = fx.item("removed.txt", "gone", false);
    let store = store_of(&[&unchanged, &changed, &removed]);
    std::fs::write(changed.path().unwrap(), "new log data").unwrap();
    let replacement = FileItem::query(&changed.file(), None).unwrap();
    let added = fx.item("added.txt", "new", false);
    let changes = watch(&store);
    assert!(reconcile_items(&store, &[replacement.clone(), FileItem::query(&unchanged.file(), None).unwrap(), added.clone()], true));
    let listed: Vec<String> = (0..store.n_items()).map(|i| store_item(&store, i).name()).collect();
    assert_eq!(listed, ["unchanged.txt", "changed.log", "added.txt"]);
    assert_eq!(store_item(&store, 0), unchanged);
    assert_eq!(store_item(&store, 1), replacement);
    assert_eq!(store_item(&store, 2), added);
    assert_eq!(*changes.borrow(), [(2, 1, 0), (1, 1, 1), (2, 0, 1)]);
}

#[test]
fn partial_reconciliation_keeps_unrelated_items_and_empty_updates_do_nothing() {
    let fx = Fixture::new("reconcile-partial");
    let log = fx.item("active.log", "old", false);
    let untouched = fx.item("document.txt", "original", false);
    let store = store_of(&[&log, &untouched]);
    std::fs::write(log.path().unwrap(), "Growing log contents").unwrap();
    let replacement = FileItem::query(&log.file(), None).unwrap();
    assert!(reconcile_items(&store, std::slice::from_ref(&replacement), false));
    assert_eq!(store.n_items(), 2);
    assert_eq!(store_item(&store, 0), replacement);
    assert_eq!(store_item(&store, 1), untouched);
    assert!(!reconcile_items(&store, &[], false));
    assert_eq!(store.n_items(), 2);
    let added = fx.file("new.txt");
    assert!(reconcile_items(&store, &[added], false));
    assert_eq!(store.n_items(), 3);
    assert_eq!(store_item(&store, 1), untouched);
    // A repeated URI: the last snapshot wins, in first-seen position.
    let store = gio::ListStore::new::<FileItem>();
    let a = fx.item("dup.txt", "1", false);
    std::fs::write(a.path().unwrap(), "22").unwrap();
    let b = FileItem::query(&a.file(), None).unwrap();
    assert!(reconcile_items(&store, &[a, b.clone()], true));
    assert_eq!(store.n_items(), 1);
    assert_eq!(store_item(&store, 0), b);
}

#[test]
fn cached_metadata_ignores_atime_but_tracks_subseconds_icons_and_permissions() {
    let fx = Fixture::new("metadata");
    let original = fx.item("metadata.txt", "value", false);
    let info = original.info().dup();
    info.set_attribute_uint64("time::access", 1234567);
    let accessed = FileItem::new(&original.file(), &info);
    assert_eq!(file_item_metadata(&original), file_item_metadata(&accessed));
    for (attribute, value) in [("time::modified-usec", 456789u32), ("unix::mode", 0o100600)] {
        let info = original.info().dup();
        let old = info.attribute_uint32(attribute);
        info.set_attribute_uint32(attribute, if value != old { value } else { value + 1 });
        let changed = FileItem::new(&original.file(), &info);
        assert_ne!(file_item_metadata(&original), file_item_metadata(&changed), "{attribute}");
    }
    let info = original.info().dup();
    info.set_icon(&gio::ThemedIcon::new("dialog-warning"));
    assert_ne!(file_item_metadata(&original), file_item_metadata(&FileItem::new(&original.file(), &info)));
    let info = original.info().dup();
    info.set_file_type(gio::FileType::Special);
    assert_ne!(file_item_metadata(&original), file_item_metadata(&FileItem::new(&original.file(), &info)));
}

#[test]
fn directory_load_is_batched_and_complete() {
    let fx = Fixture::new("batched");
    for number in 0..13 {
        fx.file(&format!("entry{number}"));
    }
    let loader = DirectoryLoader::new(4);
    let items: Rc<RefCell<Vec<FileItem>>> = Rc::default();
    let batches: Rc<RefCell<Vec<usize>>> = Rc::default();
    let errors: Rc<RefCell<Vec<Option<glib::Error>>>> = Rc::default();
    run_loop(|main_loop| {
        let (sink, sizes, errs) = (items.clone(), batches.clone(), errors.clone());
        loader.start(
            &fx.dir(),
            move |batch| {
                sizes.borrow_mut().push(batch.len());
                sink.borrow_mut().extend(batch);
            },
            move |error| {
                errs.borrow_mut().push(error);
                main_loop.quit();
            },
        );
        assert!(loader.is_loading());
    });
    assert_eq!(items.borrow().len(), 13);
    assert_eq!(*batches.borrow(), [4, 4, 4, 1]);
    assert_eq!(errors.borrow().len(), 1);
    assert!(errors.borrow()[0].is_none());
    assert!(!loader.is_loading());
    assert_eq!(DirectoryLoader::new(0).batch_size(), 1);
    assert_eq!(DirectoryLoader::new(9999).batch_size(), 512);
}

#[test]
fn directory_restart_drops_old_batches_and_completion() {
    let fx = Fixture::new("restart");
    let (first, second) = (fx.root.join("first"), fx.root.join("second"));
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    for number in 0..20 {
        std::fs::write(first.join(format!("old{number}")), "").unwrap();
    }
    std::fs::write(second.join("new"), "").unwrap();
    let loader = DirectoryLoader::new(1);
    let old_batches: Rc<RefCell<Vec<FileItem>>> = Rc::default();
    let new_items: Rc<RefCell<Vec<FileItem>>> = Rc::default();
    let old_done: Rc<Cell<u32>> = Rc::default();
    run_loop(|main_loop| {
        let (restart, old, new, second) = (loader.clone(), old_batches.clone(), new_items.clone(), second.clone());
        let done = old_done.clone();
        loader.start(
            &gio::File::for_path(&first),
            move |items| {
                old.borrow_mut().extend(items);
                let (sink, quit) = (new.clone(), main_loop.clone());
                restart.start(&gio::File::for_path(&second), move |items| sink.borrow_mut().extend(items), move |_| quit.quit());
            },
            move |_| done.set(done.get() + 1),
        );
    });
    assert_eq!(old_batches.borrow().len(), 1);
    assert_eq!(old_done.get(), 0);
    assert_eq!(names(&new_items.borrow()), ["new"]);
}

#[test]
fn missing_directory_reports_error() {
    let fx = Fixture::new("missing");
    let errors: Rc<RefCell<Vec<Option<glib::Error>>>> = Rc::default();
    run_loop(|main_loop| {
        let errs = errors.clone();
        DirectoryLoader::default().start(&gio::File::for_path(fx.root.join("missing")), |_| panic!("Unexpected files"), move |error| {
            errs.borrow_mut().push(error);
            main_loop.quit();
        });
    });
    assert!(errors.borrow()[0].as_ref().unwrap().matches(gio::IOErrorEnum::NotFound));
}

#[test]
fn recursive_search_hidden_and_no_symlink_traversal() {
    let fx = Fixture::new("recursive");
    let visible = fx.root.join("visible");
    std::fs::create_dir(&visible).unwrap();
    std::fs::write(visible.join("needle.txt"), "").unwrap();
    let hidden = fx.root.join(".hidden");
    std::fs::create_dir(&hidden).unwrap();
    std::fs::write(hidden.join("needle-secret.txt"), "").unwrap();
    // A symlink cycle would make an unrestricted recursive search repeat.
    std::os::unix::fs::symlink(&fx.root, visible.join("cycle")).unwrap();
    let (_, results) = fx.search("needle", SearchOptions::default());
    assert_eq!(names(&results), ["needle.txt"]);
    let (_, results) = fx.search("needle", SearchOptions { include_hidden: true, ..Default::default() });
    let mut found = names(&results);
    found.sort();
    assert_eq!(found, ["needle-secret.txt", "needle.txt"]);
    let (_, results) = fx.search("needle", SearchOptions { recursive: false, ..Default::default() });
    assert!(results.is_empty());
}

#[test]
fn content_search_reads_bounded_text_and_ignores_binary() {
    let fx = Fixture::new("contents");
    fx.item("one.txt", "The NEEDLE is here", false);
    fx.item("late.txt", &format!("{}needle", "x".repeat(64)), false);
    fx.item("binary.dat", "\0needle", false);
    fx.item("needle-name.txt", "unrelated", false);
    let (_, results) = fx.search("needle", SearchOptions { contents: true, ..Default::default() });
    let mut found = names(&results);
    found.sort();
    assert_eq!(found, ["needle-name.txt", "one.txt"]);
}

#[test]
fn search_limit_and_cancel_discard_scheduled_batches() {
    let fx = Fixture::new("limit");
    for number in 0..20 {
        fx.file(&format!("needle{number}"));
    }
    let (loader, items) = fx.search("needle", SearchOptions { max_results: 3, ..Default::default() });
    assert_eq!(items.len(), 3);
    assert!(loader.truncated());
    assert_eq!(loader.skipped(), 0);
    let loader = SearchLoader::new(1, 1024 * 1024);
    let delivered: Rc<RefCell<Vec<FileItem>>> = Rc::default();
    let done: Rc<Cell<u32>> = Rc::default();
    run_loop(|main_loop| {
        let (cancel, sink, finished) = (loader.clone(), delivered.clone(), done.clone());
        loader.start(
            &fx.dir(),
            "needle",
            move |items| {
                sink.borrow_mut().extend(items);
                cancel.cancel();
                let quit = main_loop.clone();
                suite_core::timer::once(100, move || quit.quit());
            },
            move |_| finished.set(finished.get() + 1),
            SearchOptions::default(),
        );
    });
    assert_eq!(delivered.borrow().len(), 1);
    assert_eq!(done.get(), 0);
    assert!(!loader.is_searching());
}

#[test]
fn search_missing_root_reports_error() {
    let fx = Fixture::new("search-missing");
    let loader = SearchLoader::default();
    let errors: Rc<RefCell<Vec<Option<glib::Error>>>> = Rc::default();
    run_loop(|main_loop| {
        let errs = errors.clone();
        loader.start(&gio::File::for_path(fx.root.join("absent")), "x", |_| panic!("no batch"), move |error| {
            errs.borrow_mut().push(error);
            main_loop.quit();
        }, SearchOptions::default());
    });
    assert!(errors.borrow()[0].is_some());
    assert!(!loader.truncated());
}

// ItemCountTests.test_singular_and_plural_item_counts
#[test]
fn singular_and_plural_item_counts() {
    assert_eq!(item_count(1), "1 item");
    assert_eq!(item_count(0), "0 items");
    assert_eq!(item_count(7), "7 items");
}
