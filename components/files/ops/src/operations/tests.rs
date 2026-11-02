//! Port of test_file_operations.py; every mutation stays in a private fixture.
//! Trash is never called for real here (the hook replaces `Gio.File.trash`);
//! real Trash runs only in tests/gvfs.rs on a private bus and XDG home.

use super::*;
use crate::testutil::{names, read, write, TempDir};
use std::cell::{Cell, RefCell};
use std::os::unix::fs::symlink;
use std::rc::Rc;
use std::sync::atomic::Ordering;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    source: PathBuf,
    target: PathBuf,
    operations: Operations,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new("tessel-files-operations-");
        let root = temp.path().to_path_buf();
        let source = root.join("source");
        let target = root.join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        Fixture { _temp: temp, root, source, target, operations: Operations::new() }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.operations.cancel_all();
        for job in self.operations.jobs() {
            job.wait(Some(Duration::from_secs(5)));
        }
    }
}

fn file(path: &Path) -> gio::File {
    gio::File::for_path(path)
}

/// Run on a private thread-default context (callbacks arrive there).
fn in_context<T>(body: impl FnOnce(&glib::MainContext) -> T) -> T {
    let context = glib::MainContext::new();
    context.with_thread_default(|| body(&context)).expect("acquire private context")
}

/// Iterate the context until the job finished and its callbacks ran.
fn finish(context: &glib::MainContext, job: &Job) -> OperationResult {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        while context.iteration(false) {}
        if let Some(result) = job.result() {
            while context.iteration(false) {}
            return result;
        }
        assert!(Instant::now() < deadline, "Operation did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn run(context: &glib::MainContext, ops: &Operations, kind: &str, sources: &[&Path], destination: Option<&Path>,
       callbacks: Callbacks) -> OperationResult {
    let sources: Vec<gio::File> = sources.iter().map(|p| file(p)).collect();
    let destination = destination.map(file);
    let job = ops.start(kind, &sources, destination.as_ref(), callbacks);
    finish(context, &job)
}

fn undo(context: &glib::MainContext, ops: &Operations) -> OperationResult {
    let job = ops.undo(Callbacks::new()).expect("undo history");
    finish(context, &job)
}

fn assert_success(result: &OperationResult) {
    assert!(result.success(), "{:?}", result.failures);
}

fn resolving(choice: &'static str) -> Callbacks {
    Callbacks::new().on_conflict(move |_, resolve| resolve(choice, None))
}

#[test]
fn test_recursive_copy_keeps_symlink_and_undo() {
    let f = Fixture::new();
    in_context(|ctx| {
        let tree = f.source.join("tree");
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        write(&tree.join("nested/hello"), "hello");
        symlink(&f.source, tree.join("external")).unwrap();
        let result = run(ctx, &f.operations, "copy", &[&tree], Some(&f.target), Callbacks::new());
        assert_success(&result);
        assert!(result.undo_available);
        let copied = f.target.join("tree");
        assert_eq!(read(&copied.join("nested/hello")), "hello");
        assert!(std::fs::symlink_metadata(copied.join("external")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(copied.join("external")).unwrap(), f.source);
        assert_success(&undo(ctx, &f.operations));
        assert!(!copied.exists());
        assert!(tree.exists());
    });
}

#[test]
fn test_copy_cannot_enter_itself_through_directory_or_symlink() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("hello");
        write(&item, "hello");
        let alias = f.root.join("alias");
        symlink(&f.source, &alias).unwrap();
        for destination in [&f.source, &alias] {
            let result = run(ctx, &f.operations, "copy", &[&f.source], Some(destination), Callbacks::new());
            assert!(!result.success());
            assert_eq!(result.failures[0].message, "A folder cannot be copied or moved into itself.");
            assert_eq!(read(&item), "hello");
            assert!(!f.source.join("source").exists());
        }
    });
}

#[test]
fn test_copy_into_own_folder_asks_for_a_new_name() {
    let f = Fixture::new();
    in_context(|ctx| {
        let folder = f.source.join("folder");
        std::fs::create_dir(&folder).unwrap();
        write(&folder.join("inner"), "inner");
        std::fs::create_dir(f.source.join("folder (copy)")).unwrap();
        let alias = f.root.join("alias");
        symlink(&f.source, &alias).unwrap();
        // Pasted next to itself, directly and through a symlinked folder.
        for (destination, chosen) in [(&f.source, "chosen"), (&alias, "via alias")] {
            let seen: Rc<RefCell<Vec<(bool, bool, Vec<&'static str>, Option<String>)>>> = Rc::default();
            let record = seen.clone();
            let callbacks = Callbacks::new().on_conflict(move |conflict, resolve| {
                record.borrow_mut().push((conflict.same_item, conflict.can_merge, conflict.choices.clone(),
                                          conflict.suggested_name.clone()));
                resolve("rename", Some(chosen));
            });
            let result = run(ctx, &f.operations, "copy", &[&folder], Some(destination), callbacks);
            assert_success(&result);
            assert_eq!(seen.borrow()[0], (true, false, vec!["skip", "rename", "cancel"],
                                          Some("folder (copy 2)".to_string())));
            assert_eq!(read(&f.source.join(chosen).join("inner")), "inner");
            assert_eq!(read(&folder.join("inner")), "inner");
        }
        // Keeping the same name asks again; the automatic name needs no input.
        let asked = Rc::new(Cell::new(0));
        let count = asked.clone();
        let callbacks = Callbacks::new().on_conflict(move |_, resolve| {
            count.set(count.get() + 1);
            resolve("rename", if count.get() == 1 { Some("folder") } else { None });
        });
        let result = run(ctx, &f.operations, "copy", &[&folder], Some(&f.source), callbacks);
        assert_success(&result);
        assert_eq!(asked.get(), 2);
        assert_eq!(read(&f.source.join("folder (copy 2)").join("inner")), "inner");
        // Replace and merge are refused; with no handler the copy is skipped.
        for choice in ["replace", "merge"] {
            let result = run(ctx, &f.operations, "copy", &[&folder], Some(&f.source), resolving(choice));
            assert_eq!(result.failures[0].message, "Invalid conflict decision.");
        }
        let result = run(ctx, &f.operations, "copy", &[&folder], Some(&f.source), Callbacks::new());
        assert_success(&result);
        assert_eq!(result.skipped.len(), 1);
        // A plain file pasted next to itself works the same way.
        let item = f.source.join("note.txt");
        write(&item, "note");
        let result = run(ctx, &f.operations, "copy", &[&item], Some(&f.source), resolving("rename"));
        assert_success(&result);
        assert_eq!(read(&f.source.join("note (copy).txt")), "note");
        // Moving an item onto itself still fails without touching it.
        let result = run(ctx, &f.operations, "move", &[&folder], Some(&f.source), resolving("rename"));
        assert_eq!(result.failures[0].message, "A folder cannot be copied or moved into itself.");
        assert_eq!(read(&folder.join("inner")), "inner");
    });
}

#[test]
fn test_default_conflict_skips_without_overwrite() {
    let f = Fixture::new();
    in_context(|ctx| {
        write(&f.source.join("item"), "new");
        write(&f.target.join("item"), "old");
        let result = run(ctx, &f.operations, "copy", &[&f.source.join("item")], Some(&f.target), Callbacks::new());
        assert_success(&result);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(read(&f.target.join("item")), "old");
    });
}

#[test]
fn test_replace_and_keep_both_conflicts() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item.txt");
        write(&item, "new");
        write(&f.target.join("item.txt"), "old");
        let result = run(ctx, &f.operations, "copy", &[&item], Some(&f.target), resolving("rename"));
        assert_success(&result);
        assert_eq!(read(&f.target.join("item (copy).txt")), "new");
        assert_eq!(read(&f.target.join("item.txt")), "old");
        let result = run(ctx, &f.operations, "copy", &[&item], Some(&f.target), resolving("replace"));
        assert_success(&result);
        assert!(!result.undo_available);
        assert_eq!(read(&f.target.join("item.txt")), "new");
        assert_eq!(names(&f.target), ["item (copy).txt", "item.txt"]);
    });
}

#[test]
fn test_rename_choice_with_name_and_conflict_details() {
    let f = Fixture::new();
    in_context(|ctx| {
        std::fs::create_dir(f.source.join("folder")).unwrap();
        std::fs::create_dir(f.target.join("folder")).unwrap();
        let seen: Rc<RefCell<Vec<(bool, bool, Vec<&'static str>)>>> = Rc::default();
        let record = seen.clone();
        let callbacks = Callbacks::new().on_conflict(move |conflict, resolve| {
            record.borrow_mut().push((conflict.is_directory, conflict.can_merge, conflict.choices.clone()));
            resolve("rename", Some("chosen"));
            resolve("skip", None); // only the first answer counts
        });
        let result = run(ctx, &f.operations, "copy", &[&f.source.join("folder")], Some(&f.target), callbacks);
        assert_success(&result);
        assert!(f.target.join("chosen").is_dir());
        assert_eq!(seen.borrow()[0], (true, true, vec!["skip", "replace", "rename", "merge", "cancel"]));
        // A bad name and an unsupported decision fail the item without changes.
        let result = run(ctx, &f.operations, "copy", &[&f.source.join("folder")], Some(&f.target),
                         Callbacks::new().on_conflict(|_, resolve| resolve("rename", Some("a/b"))));
        assert_eq!(result.failures[0].message, "Enter a file name without directory separators.");
        write(&f.source.join("plain"), "x");
        write(&f.target.join("plain"), "y");
        let result = run(ctx, &f.operations, "copy", &[&f.source.join("plain")], Some(&f.target), resolving("merge"));
        assert_eq!(result.failures[0].message, "Invalid conflict decision.");
        assert_eq!(read(&f.target.join("plain")), "y");
    });
}

#[test]
fn test_replace_failure_restores_existing_destination() {
    let f = Fixture::new();
    in_context(|ctx| {
        write(&f.source.join("item"), "new");
        write(&f.target.join("item"), "old");
        f.operations.set_hooks(TestHooks {
            copy_tree: Some(Arc::new(|_, _, _| Err(Error::os("read failed")))),
            ..Default::default()
        });
        let result = run(ctx, &f.operations, "copy", &[&f.source.join("item")], Some(&f.target), resolving("replace"));
        assert!(!result.success());
        assert_eq!(result.failures[0].message, "read failed");
        assert_eq!(read(&f.target.join("item")), "old");
        assert_eq!(names(&f.target), ["item"]);
    });
}

#[test]
fn test_directory_merge_and_safe_undo() {
    let f = Fixture::new();
    in_context(|ctx| {
        std::fs::create_dir(f.source.join("folder")).unwrap();
        write(&f.source.join("folder/new"), "new");
        std::fs::create_dir(f.target.join("folder")).unwrap();
        write(&f.target.join("folder/old"), "old");
        let result = run(ctx, &f.operations, "copy", &[&f.source.join("folder")], Some(&f.target), resolving("merge"));
        assert_success(&result);
        assert_eq!(names(&f.target.join("folder")), ["new", "old"]);
        assert_success(&undo(ctx, &f.operations));
        assert_eq!(names(&f.target.join("folder")), ["old"]);
    });
}

#[test]
fn test_move_and_move_merge_undo() {
    let f = Fixture::new();
    in_context(|ctx| {
        let folder = f.source.join("folder");
        std::fs::create_dir(&folder).unwrap();
        write(&folder.join("child"), "content");
        let result = run(ctx, &f.operations, "move", &[&folder], Some(&f.target), Callbacks::new());
        assert_success(&result);
        assert!(!folder.exists());
        assert_success(&undo(ctx, &f.operations));
        assert_eq!(read(&folder.join("child")), "content");
        std::fs::create_dir(f.target.join("folder")).unwrap();
        write(&f.target.join("folder/existing"), "keep");
        let result = run(ctx, &f.operations, "move", &[&folder], Some(&f.target), resolving("merge"));
        assert_success(&result);
        assert!(!folder.exists());
        assert_success(&undo(ctx, &f.operations));
        assert_eq!(read(&folder.join("child")), "content");
        assert_eq!(read(&f.target.join("folder/existing")), "keep");
    });
}

#[test]
fn test_undo_never_overwrites_recreated_original() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item");
        write(&item, "original");
        assert_success(&run(ctx, &f.operations, "move", &[&item], Some(&f.target), Callbacks::new()));
        write(&item, "new original");
        let result = undo(ctx, &f.operations);
        assert!(!result.success());
        assert_eq!(result.failures[0].message, "The original location is occupied; it was left untouched.");
        assert!(result.undo_available, "failed undo stays available for a retry");
        assert_eq!(read(&item), "new original");
        assert_eq!(read(&f.target.join("item")), "original");
    });
}

#[test]
fn test_undo_never_removes_changed_copy() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item");
        write(&item, "original");
        assert_success(&run(ctx, &f.operations, "copy", &[&item], Some(&f.target), Callbacks::new()));
        write(&f.target.join("item"), "edited content");
        let result = undo(ctx, &f.operations);
        assert!(!result.success());
        assert_eq!(result.failures[0].message, "The item changed since the operation; it was left untouched.");
        assert_eq!(read(&f.target.join("item")), "edited content");
    });
}

#[test]
fn test_cancel_during_copy_removes_partial_temporary() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item");
        write(&item, "original");
        f.operations.set_hooks(TestHooks {
            copy_tree: Some(Arc::new(|job: &Job, _source: &gio::File, temporary: &gio::File| {
                std::fs::write(temporary.path().unwrap(), "partial").unwrap();
                job.cancel();
                job.cancellable().set_error_if_cancelled().map_err(Error::from)
            })),
            ..Default::default()
        });
        let result = run(ctx, &f.operations, "copy", &[&item], Some(&f.target), Callbacks::new());
        assert!(result.cancelled);
        assert!(names(&f.target).is_empty());
        assert_eq!(read(&item), "original");
    });
}

#[test]
fn test_cancel_pending_conflict_does_not_block() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item");
        write(&item, "original");
        write(&f.target.join("item"), "existing");
        let asked = Rc::new(Cell::new(false));
        let flag = asked.clone();
        let job = f.operations.start("copy", &[file(&item)], Some(&file(&f.target)),
                                     Callbacks::new().on_conflict(move |_, _| flag.set(true)));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !asked.get() {
            ctx.iteration(false);
            assert!(Instant::now() < deadline, "conflict was not asked");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(job.running());
        assert_eq!(f.operations.jobs(), vec![job.clone()]);
        job.cancel();
        assert!(job.wait(Some(Duration::from_secs(2))).unwrap().cancelled);
        assert_eq!(read(&f.target.join("item")), "existing");
        assert!(f.operations.jobs().is_empty());
    });
}

#[test]
fn test_create_rename_link_duplicate_and_delete() {
    let f = Fixture::new();
    in_context(|ctx| {
        let folder = f.target.join("newfolder");
        assert_success(&run(ctx, &f.operations, "mkdir", &[], Some(&folder), Callbacks::new()));
        let item = folder.join("item");
        assert_success(&run(ctx, &f.operations, "touch", &[], Some(&item), Callbacks::new()));
        let again = run(ctx, &f.operations, "touch", &[], Some(&item), Callbacks::new());
        assert!(!again.success(), "touch never replaces existing items");
        write(&item, "text");
        let renamed = folder.join("renamed");
        assert_success(&run(ctx, &f.operations, "rename", &[&item], Some(&renamed), Callbacks::new()));
        assert!(!item.exists());
        assert_success(&run(ctx, &f.operations, "link", &[&renamed], Some(&f.source), Callbacks::new()));
        assert!(std::fs::symlink_metadata(f.source.join("renamed")).unwrap().file_type().is_symlink());
        assert_success(&run(ctx, &f.operations, "duplicate", &[&renamed], None, Callbacks::new()));
        assert_eq!(read(&folder.join("renamed (copy)")), "text");
        assert_success(&run(ctx, &f.operations, "delete", &[&f.source.join("renamed")], None, Callbacks::new()));
        assert!(renamed.exists());
        let unknown = run(ctx, &f.operations, "explode", &[&renamed], None, Callbacks::new());
        assert_eq!(unknown.failures[0].message, "Unknown operation: explode");
        let two = run(ctx, &f.operations, "rename", &[&renamed, &item], Some(&item), Callbacks::new());
        assert_eq!(two.failures[0], Failure { source: String::new(), message: "Rename one item at a time.".into() });
    });
}

#[test]
fn test_failed_trash_never_calls_delete() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item");
        write(&item, "keep");
        let hooks = TestHooks {
            trash: Some(Arc::new(|_| Err(Error::os("Trash not supported")))),
            ..Default::default()
        };
        let deleted = hooks.delete_called.clone();
        f.operations.set_hooks(hooks);
        let result = run(ctx, &f.operations, "trash", &[&item], None, Callbacks::new());
        assert!(!result.success());
        assert_eq!(result.failures[0].message, "Trash not supported");
        assert!(!deleted.load(Ordering::SeqCst));
        assert_eq!(read(&item), "keep");
    });
}

#[test]
fn test_default_dispatcher_delivers_callbacks_to_main_thread() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item");
        write(&item, "text");
        let called: Rc<RefCell<Vec<std::thread::ThreadId>>> = Rc::default();
        let record = called.clone();
        let progress = Rc::new(Cell::new(0));
        let count = progress.clone();
        let job = f.operations.start("copy", &[file(&item)], Some(&file(&f.target)), Callbacks::new()
            .on_progress(move |_| count.set(count.get() + 1))
            .on_done(move |_| record.borrow_mut().push(std::thread::current().id())));
        assert!(job.wait(Some(Duration::from_secs(5))).is_some());
        assert!(called.borrow().is_empty(), "callbacks only run on the owning context");
        while ctx.iteration(false) {}
        assert_eq!(*called.borrow(), vec![std::thread::current().id()]);
        assert!(progress.get() >= 1);
    });
}

#[test]
fn test_batch_rename_swap_and_grouped_undo() {
    let f = Fixture::new();
    in_context(|ctx| {
        let first = f.source.join("first");
        let second = f.source.join("second");
        write(&first, "one");
        write(&second, "two");
        let job = f.operations.rename_many(vec![(file(&first), file(&second)), (file(&second), file(&first))],
                                           Callbacks::new());
        assert_success(&finish(ctx, &job));
        assert_eq!(read(&first), "two");
        assert_eq!(read(&second), "one");
        assert_success(&undo(ctx, &f.operations));
        assert_eq!(read(&first), "one");
        assert_eq!(read(&second), "two");
        assert_eq!(names(&f.source), ["first", "second"]);
    });
}

#[test]
fn test_batch_rename_collision_changes_nothing() {
    let f = Fixture::new();
    in_context(|ctx| {
        let first = f.source.join("first");
        let second = f.source.join("second");
        write(&first, "one");
        write(&second, "two");
        let result = finish(ctx, &f.operations.rename_many(vec![(file(&first), file(&second))], Callbacks::new()));
        assert!(!result.success());
        assert!(result.failures[0].message.starts_with("The new name is already in use: "));
        let same = f.source.join("same");
        let result = finish(ctx, &f.operations.rename_many(vec![(file(&first), file(&same)), (file(&second), file(&same))],
                                                           Callbacks::new()));
        assert!(!result.success());
        assert_eq!(result.failures[0].message, "Every source and destination in a rename batch must be unique.");
        let elsewhere = f.target.join("moved");
        let result = finish(ctx, &f.operations.rename_many(vec![(file(&first), file(&elsewhere))], Callbacks::new()));
        assert_eq!(result.failures[0].message, "Batch rename keeps each item in its existing folder.");
        assert_eq!(read(&first), "one");
        assert_eq!(read(&second), "two");
        assert_eq!(names(&f.source), ["first", "second"]);
    });
}

#[test]
fn test_cancel_batch_commit_restores_all_original_names() {
    let f = Fixture::new();
    in_context(|ctx| {
        let first = f.source.join("first");
        let second = f.source.join("second");
        write(&first, "one");
        write(&second, "two");
        f.operations.set_hooks(TestHooks { notify: Some(Arc::new(|job: &Job| job.cancel())), ..Default::default() });
        let job = f.operations.rename_many(vec![(file(&first), file(&f.source.join("newfirst"))),
                                                (file(&second), file(&f.source.join("newsecond")))], Callbacks::new());
        let result = finish(ctx, &job);
        assert!(result.cancelled);
        assert_eq!(read(&first), "one");
        assert_eq!(read(&second), "two");
        assert_eq!(names(&f.source), ["first", "second"]);
    });
}

#[test]
fn test_progress_reports_uris_and_counts() {
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join("item");
        write(&item, "data");
        let seen: Rc<RefCell<Vec<Progress>>> = Rc::default();
        let record = seen.clone();
        let result = run(ctx, &f.operations, "copy", &[&item], Some(&f.target),
                         Callbacks::new().on_progress(move |p| record.borrow_mut().push(p.clone())));
        assert_success(&result);
        let seen = seen.borrow();
        let first = &seen[0];
        assert_eq!(first.kind, "copy");
        assert_eq!(first.source, file(&item).uri().as_str());
        assert_eq!(first.destination, file(&f.target.join("item")).uri().as_str());
        assert_eq!((first.completed_items, first.total_items), (0, 1));
        let last = seen.last().unwrap();
        assert_eq!((last.completed_items, last.destination.as_str()), (1, ""));
    });
}

#[test]
fn test_history_is_bounded_and_fingerprint_detects_changes() {
    let f = Fixture::new();
    in_context(|ctx| {
        for index in 0..(HISTORY_LIMIT + 3) {
            let path = f.target.join(format!("dir{index}"));
            assert_success(&run(ctx, &f.operations, "mkdir", &[], Some(&path), Callbacks::new()));
        }
        assert_eq!(f.operations.inner.history.lock().unwrap().len(), HISTORY_LIMIT);
        let item = f.source.join("item");
        write(&item, "a");
        let before = fingerprint(&file(&item)).unwrap();
        assert_eq!(before, fingerprint(&file(&item)).unwrap());
        write(&item, "ab");
        assert_ne!(before, fingerprint(&file(&item)).unwrap());
        assert!(f.operations.can_undo());
    });
}

#[test]
fn test_unique_names_and_as_file() {
    assert_eq!(splitext_bytes(b"item.txt"), (&b"item"[..], &b".txt"[..]));
    assert_eq!(splitext_bytes(b".bashrc"), (&b".bashrc"[..], &b""[..]));
    assert_eq!(as_file("sftp://host/x").uri(), "sftp://host/x");
    assert_eq!(as_file("/tmp/a b").path().unwrap(), PathBuf::from("/tmp/a b"));
    let f = Fixture::new();
    in_context(|ctx| {
        let item = f.source.join(".hidden");
        write(&item, "x");
        assert_success(&run(ctx, &f.operations, "duplicate", &[&item], None, Callbacks::new()));
        assert_success(&run(ctx, &f.operations, "duplicate", &[&item], None, Callbacks::new()));
        assert_eq!(names(&f.source), [".hidden", ".hidden (copy 2)", ".hidden (copy)"]);
    });
}
