//! Port of test_file_gvfs.py: real GVfs archive/Trash checks.
//!
//! These tests only run inside `tests/run_gvfs.sh`, which provides a private
//! D-Bus session (its servicedir holds only a generated org.gtk.vfs.Daemon
//! service), private HOME/XDG directories, and a private mount namespace in
//! which every filesystem carrying a real `.Trash`/`.Trash-<uid>` directory is
//! hidden behind an empty tmpfs. Under a plain `cargo test` every test returns
//! immediately. Trash is never called unless the fixture shares a filesystem
//! with the private XDG_DATA_HOME.

use tessel_files_ops::operations::{Callbacks, OperationResult, Operations};
use gio::prelude::*;
use std::io::Write;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Isolated {
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
}

fn isolated() -> Option<Isolated> {
    if std::env::var("TESSEL_FILES_GVFS_ISOLATED").as_deref() != Ok("1") {
        eprintln!("GVfs checks run only inside tests/run_gvfs.sh; skipping");
        return None;
    }
    let root = PathBuf::from(std::env::var_os("TESSEL_FILES_GVFS_TEST_ROOT").expect("test root"));
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let data = PathBuf::from(std::env::var_os("XDG_DATA_HOME").expect("XDG_DATA_HOME"));
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    assert!(home.starts_with(&root) && data.starts_with(&root), "Refusing to test Trash outside the private fixture");
    assert!(bus.contains(root.to_str().unwrap()), "Refusing to use a bus outside the private fixture: {bus}");
    assert_eq!(std::env::var("GIO_USE_VFS").as_deref(), Ok("gvfs"));
    Some(Isolated { root, home, data })
}

struct Fixture(PathBuf);

impl Fixture {
    fn new(env: &Isolated) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = env.home.join(format!("fixture-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir(&path).unwrap();
        Fixture(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Never trash unless the fixture is on the private XDG_DATA_HOME's filesystem
/// (GIO would otherwise use `<mountpoint>/.Trash-<uid>`).
fn same_filesystem(env: &Isolated, folder: &Path) -> bool {
    std::fs::create_dir_all(&env.data).unwrap();
    let same = std::fs::metadata(folder).unwrap().dev() == std::fs::metadata(&env.data).unwrap().dev();
    if !same {
        eprintln!("fixture and XDG_DATA_HOME are on different filesystems; skipping");
    }
    same
}

fn quote(text: &str) -> String {
    text.bytes().map(|b| if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) { (b as char).to_string() }
                     else { format!("%{b:02X}") }).collect()
}

/// file_model.archive_uri: two escaping layers around the archive's URI.
fn archive_uri(path: &Path) -> String {
    format!("archive://{}/", quote(&quote(gio::File::for_path(path).uri().as_str())))
}

fn mount(location: &gio::File) {
    let context = glib::MainContext::new();
    let main_loop = glib::MainLoop::new(Some(&context), false);
    let result: std::rc::Rc<std::cell::RefCell<Option<Result<(), glib::Error>>>> = Default::default();
    context.with_thread_default(|| {
        let done = result.clone();
        let quit = main_loop.clone();
        location.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&gio::MountOperation::new()),
                                        None::<&gio::Cancellable>, move |outcome| {
            *done.borrow_mut() = Some(outcome);
            quit.quit();
        });
        let quit = main_loop.clone();
        let timer = glib::timeout_source_new_seconds(10, None, glib::Priority::DEFAULT, move || {
            quit.quit();
            glib::ControlFlow::Break
        });
        timer.attach(Some(&context));
        if result.borrow().is_none() {
            main_loop.run();
        }
        timer.destroy();
    }).unwrap();
    let outcome = result.borrow_mut().take();
    match outcome {
        Some(Ok(())) => {}
        Some(Err(error)) if error.matches(gio::IOErrorEnum::AlreadyMounted) => {}
        Some(Err(error)) => panic!("mount failed: {error}"),
        None => panic!("GVfs operation did not finish"),
    }
}

struct Item {
    file: gio::File,
    name: String,
    orig_path: Option<String>,
}

fn entries(directory: &gio::File) -> Vec<Item> {
    let enumerator = directory.enumerate_children("standard::name,trash::orig-path", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                                                  None::<&gio::Cancellable>).unwrap();
    let mut items = Vec::new();
    while let Some(info) = enumerator.next_file(None::<&gio::Cancellable>).unwrap() {
        items.push(Item {
            file: directory.child(info.name()),
            name: info.name().to_string_lossy().into_owned(),
            orig_path: info.attribute_byte_string("trash::orig-path").map(|s| s.to_string()),
        });
    }
    items
}

fn operation(ops: &Operations, kind: &str, sources: &[gio::File], destination: Option<&gio::File>) -> OperationResult {
    let result = ops.start(kind, sources, destination, Callbacks::new()).wait(Some(Duration::from_secs(10)))
        .expect("GVfs file operation did not finish");
    assert!(result.success(), "{kind}: {:?}", result.failures);
    assert!(result.skipped.is_empty());
    result
}

fn undo(ops: &Operations) -> OperationResult {
    ops.undo(Callbacks::new()).expect("Trash must add an undo record").wait(Some(Duration::from_secs(10)))
        .expect("Undo did not finish")
}

fn file(path: &Path) -> gio::File {
    gio::File::for_path(path)
}

fn trashinfo_count(env: &Isolated, prefix: &str) -> usize {
    std::fs::read_dir(env.data.join("Trash/info")).map(|entries| entries
        .filter_map(|e| e.ok())
        .filter(|e| { let n = e.file_name().to_string_lossy().into_owned(); n.starts_with(prefix) && n.ends_with(".trashinfo") })
        .count()).unwrap_or(0)
}

fn ours(source: &Path) -> Vec<Item> {
    let trash = gio::File::for_uri("trash:///");
    mount(&trash);
    entries(&trash).into_iter().filter(|item| item.orig_path.as_deref() == source.to_str()).collect()
}

#[test]
fn test_archive_uri_mount_browse_and_copy_from_nested_folder() {
    let Some(env) = isolated() else { return };
    let fixture = Fixture::new(&env);
    let archive_path = fixture.0.join("Archive with spaces.zip");
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
    archive.start_file("Read me.txt", zip::write::SimpleFileOptions::default()).unwrap();
    archive.write_all(b"Root archive file\n").unwrap();
    archive.start_file("Nested folder/Second note.txt", zip::write::SimpleFileOptions::default()).unwrap();
    archive.write_all(b"Nested archive file\n").unwrap();
    archive.finish().unwrap();
    let root = gio::File::for_uri(&archive_uri(&archive_path));
    mount(&root);
    let mut names: Vec<String> = entries(&root).into_iter().map(|i| i.name).collect();
    names.sort();
    assert_eq!(names, ["Nested folder", "Read me.txt"]);
    let nested = root.child("Nested folder");
    let children: Vec<String> = entries(&nested).into_iter().map(|i| i.name).collect();
    assert_eq!(children, ["Second note.txt"]);
    let destination = fixture.0.join("Copied out");
    std::fs::create_dir(&destination).unwrap();
    let ops = Operations::new();
    operation(&ops, "copy", &[nested], Some(&file(&destination)));
    assert_eq!(std::fs::read_to_string(destination.join("Nested folder/Second note.txt")).unwrap(), "Nested archive file\n");
}

#[test]
fn test_trash_00_undo_rpm_without_opening_trash_first() {
    let Some(env) = isolated() else { return };
    let fixture = Fixture::new(&env);
    if !same_filesystem(&env, &fixture.0) {
        return;
    }
    let ops = Operations::new();
    let source = fixture.0.join("package-1.0.x86_64.rpm");
    std::fs::write(&source, b"private RPM fixture\x00\xff").unwrap();
    let result = operation(&ops, "trash", &[file(&source)], None);
    assert!(result.undo_available);
    assert!(ops.can_undo());
    assert!(!source.exists());
    let result = undo(&ops);
    assert!(result.success(), "{:?}", result.failures);
    assert_eq!(std::fs::read(&source).unwrap(), b"private RPM fixture\x00\xff");
    assert!(!ops.can_undo());
    assert_eq!(trashinfo_count(&env, "package-1.0.x86_64.rpm"), 0);
}

#[test]
fn test_trash_and_restore_only_private_fixture() {
    let Some(env) = isolated() else { return };
    let fixture = Fixture::new(&env);
    if !same_filesystem(&env, &fixture.0) {
        return;
    }
    let ops = Operations::new();
    let source = fixture.0.join("Trash fixture.txt");
    let contents = b"Only this private fixture may be trashed and restored\n";
    std::fs::write(&source, contents).unwrap();
    operation(&ops, "trash", &[file(&source)], None);
    assert!(!source.exists());
    assert!(env.data.join("Trash").is_dir(), "Trash must use the private XDG data home");
    assert!(trashinfo_count(&env, "Trash fixture") >= 1);
    let found = ours(&source);
    assert_eq!(found.len(), 1, "Never restore an entry that is not our exact private fixture");
    assert!(Path::new(found[0].orig_path.as_deref().unwrap()).starts_with(&env.root));
    operation(&ops, "restore", &[found[0].file.clone()], None);
    assert_eq!(std::fs::read(&source).unwrap(), contents);
    assert!(ours(&source).is_empty());
}

#[test]
fn test_trash_undo_distinguishes_older_versions_at_same_original_path() {
    let Some(env) = isolated() else { return };
    let fixture = Fixture::new(&env);
    if !same_filesystem(&env, &fixture.0) {
        return;
    }
    let ops = Operations::new();
    let source = fixture.0.join("duplicate-name.rpm");
    std::fs::write(&source, b"older package").unwrap();
    operation(&ops, "trash", &[file(&source)], None);
    std::fs::write(&source, b"newer package").unwrap();
    operation(&ops, "trash", &[file(&source)], None);
    let result = undo(&ops);
    assert!(result.success(), "{:?}", result.failures);
    assert_eq!(std::fs::read(&source).unwrap(), b"newer package");
    // The older operation cannot replace the version just restored.
    let result = undo(&ops);
    assert!(!result.success());
    assert!(result.undo_available);
    assert_eq!(result.failures[0].message, "The original location is occupied; the deleted item remains in Trash.");
    assert_eq!(std::fs::read(&source).unwrap(), b"newer package");
    std::fs::remove_file(&source).unwrap();
    let result = undo(&ops);
    assert!(result.success(), "{:?}", result.failures);
    assert_eq!(std::fs::read(&source).unwrap(), b"older package");
}

#[test]
fn test_trash_undo_group_preserves_directory_and_symlink() {
    let Some(env) = isolated() else { return };
    let fixture = Fixture::new(&env);
    if !same_filesystem(&env, &fixture.0) {
        return;
    }
    let ops = Operations::new();
    let folder = fixture.0.join("packages");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("one.rpm"), b"rpm data").unwrap();
    let link = fixture.0.join("link to packages");
    symlink(&folder, &link).unwrap();
    operation(&ops, "trash", &[file(&folder), file(&link)], None);
    assert!(!folder.exists());
    assert!(std::fs::symlink_metadata(&link).is_err());
    let result = undo(&ops);
    assert!(result.success(), "{:?}", result.failures);
    assert_eq!(std::fs::read(folder.join("one.rpm")).unwrap(), b"rpm data");
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), folder);
}

#[test]
fn test_trash_undo_refuses_missing_item_and_keeps_existing_replacement() {
    let Some(env) = isolated() else { return };
    let fixture = Fixture::new(&env);
    if !same_filesystem(&env, &fixture.0) {
        return;
    }
    let ops = Operations::new();
    let source = fixture.0.join("missing.rpm");
    std::fs::write(&source, b"original").unwrap();
    operation(&ops, "trash", &[file(&source)], None);
    std::fs::write(&source, b"replacement").unwrap();
    let result = undo(&ops);
    assert!(!result.success());
    assert_eq!(std::fs::read(&source).unwrap(), b"replacement");
    std::fs::remove_file(&source).unwrap();
    let found = ours(&source);
    assert_eq!(found.len(), 1);
    operation(&ops, "delete", &[found[0].file.clone()], None);
    let result = undo(&ops);
    assert!(!result.success());
    assert_eq!(result.failures[0].message,
               "The deleted item is no longer uniquely identifiable in Trash; nothing was restored.");
    assert!(!source.exists());
}
