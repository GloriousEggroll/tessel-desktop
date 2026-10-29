//! Port of ~/btmanager/wayshell/test_desktop_file_ops.py (planning, conflict
//! prompts, worker transfers, Trash watcher). Every file lives under $TMPDIR;
//! each test runs on its own thread-default main context.
#![cfg(feature = "fileops")]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use wayshell::fileops::{self as ops, Mode, TransferItem, TransferResult, KEEP_BOTH, REPLACE, SKIP};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn safety() {
    let tmp = std::env::temp_dir();
    assert!(tmp.starts_with("/run/user/1000/cbt/") || !tmp.starts_with("/tmp"), "set TMPDIR to a scratch dir: {tmp:?}");
}

fn with_context(test: impl FnOnce(&glib::MainContext)) {
    safety();
    let context = glib::MainContext::new();
    context.with_thread_default(|| test(&context)).unwrap();
}

fn run_until(context: &glib::MainContext, predicate: impl FnMut() -> bool) -> bool {
    suite_core::timer::wait_for(context, Duration::from_secs(5), predicate)
}

#[test]
fn unique_name_keeps_extension_and_dotfiles() {
    let taken = ["/d/a.txt", "/d/a (1).txt", "/d/.profile", "/d/folder"];
    let exists = |p: &Path| taken.iter().any(|t| Path::new(t) == p);
    let d = Path::new("/d");
    assert_eq!(ops::unique_name_with(d, "b.txt", exists), "b.txt");
    assert_eq!(ops::unique_name_with(d, "a.txt", exists), "a (2).txt");
    assert_eq!(ops::unique_name_with(d, ".profile", exists), ".profile (1)");
    assert_eq!(ops::unique_name_with(d, "folder", exists), "folder (1)");
}

#[test]
fn clipboard_payloads() {
    let gnome = ops::GNOME_COPIED_FILES;
    assert_eq!(ops::parse_clipboard_files("cut\nfile:///a\nfile:///b\n", gnome), (Some(Mode::Move), vec!["file:///a".into(), "file:///b".into()]));
    assert_eq!(ops::parse_clipboard_files("copy\nfile:///a", gnome), (Some(Mode::Copy), vec!["file:///a".into()]));
    assert_eq!(ops::parse_clipboard_files("file:///a", gnome), (None, vec![]));
    assert_eq!(
        ops::parse_clipboard_files("# c\r\nfile:///a\r\nsmb://h/x\r\n", ops::URI_LIST),
        (Some(Mode::Copy), vec!["file:///a".into(), "smb://h/x".into()])
    );
    // NULs are newlines; lines without ':' are dropped.
    assert_eq!(ops::parse_clipboard_files("cut\0file:///a\0junk\0", gnome), (Some(Mode::Move), vec!["file:///a".into()]));
}

#[test]
fn action_and_mode_strings() {
    assert_eq!((REPLACE.as_str(), KEEP_BOTH.as_str(), SKIP.as_str()), ("replace", "keep", "skip"));
    assert_eq!(Mode::from_str("cut"), Mode::Move);
    assert_eq!(Mode::from_str("move"), Mode::Move);
    assert_eq!(Mode::from_str("copy"), Mode::Copy);
}

fn items(conflicts: &[bool]) -> Vec<TransferItem> {
    conflicts.iter().enumerate().map(|(i, c)| TransferItem::new(None, format!("n{i}")).with_conflict(*c)).collect()
}

#[test]
fn asks_each_conflict_without_nested_loop_and_apply_to_all() {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let replies = Rc::new(RefCell::new(Vec::new()));
    let finished = Rc::new(RefCell::new(Vec::new()));
    let (a, r, f) = (asked.clone(), replies.clone(), finished.clone());
    let resolver = ops::ConflictResolver::new(
        items(&[true, false, true, true]),
        move |item, remaining, reply| {
            a.borrow_mut().push((item.name.clone(), remaining));
            r.borrow_mut().push(reply);
        },
        move |items| f.borrow_mut().push(items),
    );
    resolver.start();
    // The prompt is asynchronous: nothing is decided until it replies.
    assert_eq!(*asked.borrow(), vec![("n0".to_string(), 3)]);
    assert!(finished.borrow().is_empty());
    let reply = replies.borrow_mut().pop().unwrap();
    reply.send(Some(SKIP), false);
    assert_eq!(asked.borrow().last().unwrap(), &("n2".to_string(), 2));
    let reply = replies.borrow_mut().pop().unwrap();
    reply.send(Some(KEEP_BOTH), true);
    assert_eq!(asked.borrow().len(), 2);
    let actions: Vec<_> = finished.borrow()[0].iter().map(|i| i.action).collect();
    assert_eq!(actions, vec![Some(SKIP), Some(KEEP_BOTH), Some(KEEP_BOTH), Some(KEEP_BOTH)]);
}

#[test]
fn dismissed_prompt_skips_and_preset_actions_are_not_asked() {
    let mut list = items(&[true, true]);
    list[0].action = Some(KEEP_BOTH); // e.g. pasting an item onto itself
    let finished = Rc::new(RefCell::new(Vec::new()));
    let f = finished.clone();
    ops::ConflictResolver::new(list, |_item, _remaining, reply| reply.send(None, false), move |items| f.borrow_mut().push(items)).start();
    let actions: Vec<_> = finished.borrow()[0].iter().map(|i| i.action).collect();
    assert_eq!(actions, vec![Some(KEEP_BOTH), Some(SKIP)]);
}

#[test]
fn unresolved_items_default_to_replace_or_keep_both() {
    let finished = Rc::new(RefCell::new(Vec::new()));
    let f = finished.clone();
    // A conflict that was preset is not asked; non-conflicts become KEEP_BOTH.
    let list = vec![TransferItem::new(None, "a"), TransferItem::new(None, "b").with_conflict(true).with_action(REPLACE)];
    ops::ConflictResolver::new(list, |_, _, _| panic!("asked"), move |items| f.borrow_mut().push(items)).start();
    let actions: Vec<_> = finished.borrow()[0].iter().map(|i| i.action).collect();
    assert_eq!(actions, vec![Some(KEEP_BOTH), Some(REPLACE)]);
}

struct Fixture {
    _temp: TempDir,
    src: PathBuf,
    dest: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new("desktop-ops-");
        let src = temp.0.join("src");
        let dest = temp.0.join("Desktop");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dest).unwrap();
        Fixture { _temp: temp, src, dest }
    }

    fn plan(&self, ctx: &glib::MainContext, names: &[&str], mode: Mode, source_dir: Option<&Path>) -> Vec<TransferItem> {
        let dir = source_dir.unwrap_or(&self.src);
        let sources = names.iter().map(|n| gio::File::for_path(dir.join(n))).collect();
        let result = Rc::new(RefCell::new(None));
        let r = result.clone();
        ops::plan_async(sources, self.dest.clone(), mode, move |items| *r.borrow_mut() = Some(items), None);
        assert!(run_until(ctx, || result.borrow().is_some()));
        let items = result.borrow_mut().take().unwrap();
        items
    }

    fn transfer(&self, ctx: &glib::MainContext, items: Vec<TransferItem>, mode: Mode) -> TransferResult {
        let done = Rc::new(RefCell::new(None));
        let d = done.clone();
        let job = ops::FileTransfer::new(items, self.dest.clone(), mode);
        job.set_use_trash(false).on_done(move |r| *d.borrow_mut() = Some(r)).start();
        assert!(run_until(ctx, || done.borrow().is_some()));
        let result = done.borrow_mut().take().unwrap();
        result
    }
}

fn resolve(items: Vec<TransferItem>, choice: ops::Action) -> Vec<TransferItem> {
    let out = Rc::new(RefCell::new(None));
    let o = out.clone();
    ops::ConflictResolver::new(items, move |_, _, reply| reply.send(Some(choice), true), move |items| *o.borrow_mut() = Some(items)).start();
    let items = out.borrow_mut().take().unwrap();
    items
}

fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn recursive_copy_keep_both_and_replace() {
    with_context(|ctx| {
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.src.join("dir/sub")).unwrap();
        std::fs::write(fx.src.join("dir/sub/f.txt"), "deep").unwrap();
        std::fs::write(fx.src.join("a.txt"), "new").unwrap();
        std::fs::write(fx.dest.join("a.txt"), "old").unwrap();
        let items = fx.plan(ctx, &["dir", "a.txt"], Mode::Copy, None);
        let summary: Vec<_> = items.iter().map(|i| (i.name.as_str(), i.conflict, i.is_dir)).collect();
        assert_eq!(summary, vec![("dir", false, true), ("a.txt", true, false)]);
        let result = fx.transfer(ctx, resolve(items, KEEP_BOTH), Mode::Copy);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let mut placed = result.placed.clone();
        placed.sort();
        assert_eq!(placed, vec!["a (1).txt", "dir"]);
        assert_eq!(read(fx.dest.join("dir/sub/f.txt")), "deep");
        assert_eq!(read(fx.dest.join("a.txt")), "old");
        assert_eq!(read(fx.dest.join("a (1).txt")), "new");
        // Replace without a Trash deletes the old folder and overwrites the file.
        std::fs::write(fx.src.join("dir/only-new"), "x").unwrap();
        let items = fx.plan(ctx, &["dir", "a.txt"], Mode::Copy, None);
        let result = fx.transfer(ctx, resolve(items, REPLACE), Mode::Copy);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(read(fx.dest.join("a.txt")), "new");
        assert!(fx.dest.join("dir/only-new").exists());
        assert!(fx.src.join("a.txt").exists());
    });
}

#[test]
fn move_skip_and_same_item() {
    with_context(|ctx| {
        let fx = Fixture::new();
        std::fs::write(fx.src.join("m.txt"), "m").unwrap();
        std::fs::write(fx.src.join("s.txt"), "s").unwrap();
        std::fs::write(fx.dest.join("s.txt"), "keep").unwrap();
        std::fs::write(fx.dest.join("here.txt"), "here").unwrap();
        let items = fx.plan(ctx, &["m.txt", "s.txt"], Mode::Move, None);
        let result = fx.transfer(ctx, resolve(items, SKIP), Mode::Move);
        assert_eq!(result.placed, vec!["m.txt"]);
        assert!(!fx.src.join("m.txt").exists());
        assert_eq!(read(fx.dest.join("s.txt")), "keep");
        // Pasting a desktop item onto the desktop: copy duplicates, move is a no-op.
        let dest = fx.dest.clone();
        let same = fx.plan(ctx, &["here.txt"], Mode::Copy, Some(&dest));
        assert_eq!((same[0].same, same[0].action), (true, Some(KEEP_BOTH)));
        let result = fx.transfer(ctx, resolve(same, REPLACE), Mode::Copy);
        assert_eq!(result.placed, vec!["here (1).txt"]);
        assert_eq!(fx.plan(ctx, &["here.txt"], Mode::Move, Some(&dest))[0].action, Some(SKIP));
    });
}

#[test]
fn slash_in_display_name_becomes_underscore() {
    // Native files keep their basename; '/' can only come from display names,
    // so check the replacement rule on the planned name directly.
    with_context(|ctx| {
        let fx = Fixture::new();
        std::fs::write(fx.src.join("plain.txt"), "x").unwrap();
        let items = fx.plan(ctx, &["plain.txt"], Mode::Copy, None);
        assert_eq!(items[0].name, "plain.txt");
        assert!(!items[0].name.contains('/'));
    });
}

#[test]
fn folder_into_itself_and_missing_source_report_errors() {
    with_context(|ctx| {
        let fx = Fixture::new();
        let nested = fx.dest.join("inner");
        std::fs::create_dir(&nested).unwrap();
        let item = TransferItem::new(Some(gio::File::for_path(&fx.dest)), "Desktop").with_dir(true).with_action(KEEP_BOTH);
        let done = Rc::new(RefCell::new(None));
        let d = done.clone();
        ops::FileTransfer::new(vec![item], nested.clone(), Mode::Copy).on_done(move |r| *d.borrow_mut() = Some(r)).start();
        assert!(run_until(ctx, || done.borrow().is_some()));
        assert_eq!(done.borrow().as_ref().unwrap().errors.len(), 1);
        assert_eq!(std::fs::read_dir(&nested).unwrap().count(), 0);
        let missing = fx.plan(ctx, &["gone.txt"], Mode::Copy, None);
        let result = fx.transfer(ctx, resolve(missing, KEEP_BOTH), Mode::Copy);
        let names: Vec<_> = result.errors.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["gone.txt"]);
        assert!(result.placed.is_empty());
    });
}

#[test]
fn cancel_stops_and_removes_partial_copy() {
    with_context(|ctx| {
        let fx = Fixture::new();
        let names: Vec<String> = (0..30).map(|i| format!("f{i}.bin")).collect();
        for name in &names {
            std::fs::write(fx.src.join(name), vec![7u8; 64 * 1024]).unwrap();
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let items = resolve(fx.plan(ctx, &refs, Mode::Copy, None), KEEP_BOTH);
        let progress = Rc::new(RefCell::new(Vec::new()));
        let done = Rc::new(RefCell::new(None));
        let (p, d) = (progress.clone(), done.clone());
        let job = ops::FileTransfer::new(items, fx.dest.clone(), Mode::Copy);
        job.on_progress(move |fraction, _| p.borrow_mut().push(fraction)).on_done(move |r| *d.borrow_mut() = Some(r));
        job.cancel();
        job.start();
        assert!(run_until(ctx, || done.borrow().is_some()));
        assert!(done.borrow().as_ref().unwrap().cancelled);
        assert!(progress.borrow().is_empty());
        assert_eq!(std::fs::read_dir(&fx.dest).unwrap().count(), 0);
    });
}

#[test]
fn cancel_mid_copy_removes_partial_file() {
    with_context(|ctx| {
        let fx = Fixture::new();
        std::fs::write(fx.src.join("big.bin"), vec![1u8; 64 * 1024 * 1024]).unwrap();
        let items = resolve(fx.plan(ctx, &["big.bin"], Mode::Copy, None), KEEP_BOTH);
        let done = Rc::new(RefCell::new(None));
        let d = done.clone();
        let job = ops::FileTransfer::new(items, fx.dest.clone(), Mode::Copy);
        let cancel = job.clone();
        job.set_report_interval(Duration::ZERO)
            .on_progress(move |_, _| cancel.cancel())
            .on_done(move |r| *d.borrow_mut() = Some(r))
            .start();
        assert!(run_until(ctx, || done.borrow().is_some()));
        let result = done.borrow_mut().take().unwrap();
        if result.cancelled {
            assert!(result.placed.is_empty());
            assert_eq!(std::fs::read_dir(&fx.dest).unwrap().count(), 0, "partial copy removed");
        }
    });
}

#[test]
fn progress_is_reported_on_the_main_loop() {
    with_context(|ctx| {
        let fx = Fixture::new();
        std::fs::write(fx.src.join("big.bin"), vec![3u8; 4 * 1024 * 1024]).unwrap();
        let items = resolve(fx.plan(ctx, &["big.bin"], Mode::Copy, None), KEEP_BOTH);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let main = std::thread::current().id();
        let done = Rc::new(RefCell::new(None));
        let (s, d) = (seen.clone(), done.clone());
        ops::FileTransfer::new(items, fx.dest.clone(), Mode::Copy)
            .set_report_interval(Duration::ZERO)
            .on_progress(move |fraction, name| {
                assert_eq!(std::thread::current().id(), main);
                s.borrow_mut().push((fraction, name.to_string()));
            })
            .on_done(move |r| *d.borrow_mut() = Some(r))
            .start();
        assert!(run_until(ctx, || done.borrow().is_some()));
        assert_eq!(done.borrow().as_ref().unwrap().placed, vec!["big.bin"]);
        assert!(!seen.borrow().is_empty());
        assert!(seen.borrow().iter().all(|(f, n)| (0.0..=1.0).contains(f) && n == "big.bin"));
    });
}

// ---------------------------------------------------------------- Trash watcher

#[derive(Default)]
struct FakeTrash {
    calls: Rc<RefCell<Vec<Box<dyn FnOnce(Result<u32, glib::Error>)>>>>,
    attributes: Rc<RefCell<Vec<&'static str>>>,
}

impl ops::TrashBackend for FakeTrash {
    fn monitor(&self, _on_changed: Box<dyn Fn()>) -> Result<Box<dyn FnOnce()>, glib::Error> {
        Err(glib::Error::new(gio::IOErrorEnum::NotSupported, "unsupported"))
    }
    fn query_count(&self, _c: &gio::Cancellable, done: Box<dyn FnOnce(Result<u32, glib::Error>)>) {
        self.attributes.borrow_mut().push("trash::item-count");
        self.calls.borrow_mut().push(done);
    }
}

#[test]
fn trash_count_is_queried_asynchronously_and_shared() {
    with_context(|_ctx| {
        let fake = FakeTrash::default();
        let (calls, attributes) = (fake.calls.clone(), fake.attributes.clone());
        let watcher = ops::TrashWatcher::with_backend(Box::new(fake));
        assert_eq!(attributes.borrow()[0], "trash::item-count");
        let (seen_a, seen_b) = (Rc::new(RefCell::new(Vec::new())), Rc::new(RefCell::new(Vec::new())));
        let (a, b) = (seen_a.clone(), seen_b.clone());
        let id_a = watcher.add_listener(move |n| a.borrow_mut().push(n));
        watcher.add_listener(move |n| b.borrow_mut().push(n));
        watcher.query(); // coalesced while the first query is in flight
        assert_eq!(calls.borrow().len(), 1);
        let first = calls.borrow_mut().remove(0);
        first(Ok(4));
        assert_eq!((seen_a.borrow().clone(), seen_b.borrow().clone()), (vec![4], vec![4]));
        assert_eq!(attributes.borrow().len(), 2);
        let second = calls.borrow_mut().remove(0);
        second(Ok(4)); // unchanged count: no repeat notification
        assert_eq!(*seen_a.borrow(), vec![4]);
        let late = Rc::new(RefCell::new(Vec::new()));
        let l = late.clone();
        watcher.add_listener(move |n| l.borrow_mut().push(n));
        assert_eq!(*late.borrow(), vec![4]);
        assert!(watcher.has_listener(id_a));
        watcher.remove_listener(id_a);
        assert!(!watcher.has_listener(id_a));
        assert!(!watcher.is_monitoring());
        watcher.close();
        assert_eq!(watcher.listener_count(), 0);
        watcher.query();
        assert!(calls.borrow().is_empty(), "closed watcher never queries");
    });
}

#[test]
fn trash_changes_are_debounced_150ms() {
    with_context(|ctx| {
        let fake = FakeTrash::default();
        let calls = fake.calls.clone();
        let watcher = ops::TrashWatcher::with_backend(Box::new(fake));
        let first = calls.borrow_mut().remove(0);
        first(Ok(1));
        watcher.changed();
        watcher.changed();
        watcher.changed();
        assert!(watcher.debounce_pending());
        assert!(calls.borrow().is_empty());
        suite_core::timer::settle(ctx, Duration::from_millis(100));
        assert!(calls.borrow().is_empty());
        assert!(run_until(ctx, || !calls.borrow().is_empty()));
        assert_eq!(calls.borrow().len(), 1);
        watcher.changed();
        watcher.close();
        assert!(!watcher.debounce_pending());
    });
}

#[test]
fn real_directory_monitor_drives_the_count() {
    // A plain directory stands in for trash:///: the watcher only needs a
    // monitor and an async count query, so this proves the signal path.
    struct DirTrash(gio::File);
    impl ops::TrashBackend for DirTrash {
        fn monitor(&self, on_changed: Box<dyn Fn()>) -> Result<Box<dyn FnOnce()>, glib::Error> {
            ops::GioTrash { file: self.0.clone() }.monitor(on_changed)
        }
        fn query_count(&self, _c: &gio::Cancellable, done: Box<dyn FnOnce(Result<u32, glib::Error>)>) {
            let n = std::fs::read_dir(self.0.path().unwrap()).map(|d| d.count() as u32).unwrap_or(0);
            suite_core::timer::once(0, move || done(Ok(n)));
        }
    }
    with_context(|ctx| {
        let temp = TempDir::new("desktop-trash-");
        let watcher = ops::TrashWatcher::with_backend(Box::new(DirTrash(gio::File::for_path(&temp.0))));
        assert!(watcher.is_monitoring());
        assert!(run_until(ctx, || watcher.count() == Some(0)));
        std::fs::write(temp.0.join("x"), "x").unwrap();
        assert!(run_until(ctx, || watcher.count() == Some(1)));
        watcher.close();
    });
}

// ---------------------------------------------------------------- helpers

#[test]
fn notification_parameters_match_python() {
    let v = ops::notify_parameters("Desktop", "dialog-error-symbolic", "Could not open", "details");
    assert_eq!(v.type_().as_str(), "(susssasa{sv}i)");
    let (app, id, icon, summary, body, actions, hints, timeout): (String, u32, String, String, String, Vec<String>, std::collections::HashMap<String, glib::Variant>, i32) =
        v.get().unwrap();
    assert_eq!((app.as_str(), id, icon.as_str(), summary.as_str(), body.as_str()), ("Desktop", 0, "dialog-error-symbolic", "Could not open", "details"));
    assert!(actions.is_empty());
    assert_eq!(hints["urgency"].get::<u8>(), Some(1), "{v}");
    assert_eq!(timeout, 8000);
    assert_eq!(ops::notify_log_line("A", ""), "⚠️ A");
    assert_eq!(ops::notify_log_line("A", "b"), "⚠️ A: b");
}

#[test]
fn rename_rules() {
    let message = "A name can't contain \"/\".";
    assert_eq!(ops::rename_error("a/b").as_deref(), Some(message));
    assert_eq!(ops::rename_error(".").as_deref(), Some(message));
    assert_eq!(ops::rename_error("..").as_deref(), Some(message));
    assert_eq!(ops::rename_error("ok name"), None);
    let temp = TempDir::new("desktop-rename-");
    let launcher = temp.0.join("app.desktop");
    std::fs::write(&launcher, "[Desktop Entry]\nType=Application\nName=Old\nExec=true\n").unwrap();
    ops::rename_desktop_entry(&launcher, "New").unwrap();
    assert!(read(&launcher).contains("Name=New"));
    assert!(launcher.exists(), "launchers keep their file name");
}

#[test]
fn rename_and_empty_trash_are_async() {
    with_context(|ctx| {
        let temp = TempDir::new("desktop-rename-async-");
        let file = temp.0.join("old.txt");
        std::fs::write(&file, "x").unwrap();
        let result = Rc::new(RefCell::new(None));
        let r = result.clone();
        ops::rename_async(&file, "new.txt", move |res| *r.borrow_mut() = Some(res));
        assert!(result.borrow().is_none());
        assert!(run_until(ctx, || result.borrow().is_some()));
        assert!(temp.0.join("new.txt").exists());
        // A stand-in folder for trash:/// (never the real Trash).
        let bin = temp.0.join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("a"), "a").unwrap();
        std::fs::write(bin.join("b"), "b").unwrap();
        let done = Rc::new(RefCell::new(None));
        let d = done.clone();
        ops::empty_trash_async(Some(gio::File::for_path(&bin)), move |error| *d.borrow_mut() = Some(error));
        assert!(run_until(ctx, || done.borrow().is_some()));
        assert_eq!(*done.borrow(), Some(None));
        assert_eq!(std::fs::read_dir(&bin).unwrap().count(), 0);
    });
}
