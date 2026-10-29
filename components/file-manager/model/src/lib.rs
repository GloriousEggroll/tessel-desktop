//! `files-model`: the GTK-widget-free model of Files (`wayfiles`).
//!
//! One module per Python file (`~/btmanager/file-manager`, read-only source
//! of truth). Names follow Python: classes are structs with the same name,
//! functions/methods/constants keep their snake_case / UPPER names; a leading
//! underscore is dropped for helpers other crates need.
//!
//! - [`model`] (file_model.py)
//!   - `FileItem` — a **`glib::Object` subclass** (`WayfilesFileItem`), so it
//!     goes straight into `gio::ListStore::new::<FileItem>()`.
//!     `FileItem::new(&file, &info)`, `FileItem::query(&file, cancellable) ->
//!     Result<FileItem, glib::Error>`; accessors `file() info() name()
//!     display_name() uri() path()->Option<PathBuf> is_dir() is_symlink()
//!     is_hidden() size() modified() content_type() icon() set_icon()
//!     readable() writeable() writable() deletable() trashable() renamable()
//!     thumbnail_path() thumbnail_valid() owner() group() mode()
//!     type_description()`; `data()` borrows everything at once.
//!   - `FILE_ATTRIBUTES`, `QUERY_FLAGS`, `DESKTOP_ENTRY_LIMIT`.
//!   - `archive_uri(&str) -> Result<String, String>`, `archive_uri_for_file(&gio::File)`.
//!   - `load_desktop_icon(&item, cancellable)` (sync), `load_desktop_icons_async(items,
//!     cancellable, on_done: FnOnce(Vec<FileItem>))` (GIO worker pool; synchronous when
//!     no launcher is in the batch), `is_desktop_entry`, `desktop_icon(&[u8])`.
//!   - `file_item_metadata(&item) -> FileItemMetadata` (PartialEq),
//!     `reconcile_items(&gio::ListStore, &[FileItem], remove_missing) -> bool`.
//!   - `natural_key`, `compare_items(a, b, key, descending, folders_first, natural_sort)
//!     -> std::cmp::Ordering`, `sort_items`, `name_matches`, `fnmatchcase`.
//!   - `item_count(i64)` (tr!), `format_size(u64)`, `format_modified(u64)`.
//!   - `DirectoryLoader` (cloneable handle): `new(batch_size)` / `default()` (128),
//!     `start(&dir, on_batch: Fn(Vec<FileItem>), on_done: FnOnce(Option<glib::Error>))`,
//!     `cancel()`, `is_loading()`. GIO async on the thread-default main context.
//!   - `SearchLoader` (cloneable handle): `new(batch_size, max_content_bytes)` /
//!     `default()` (64, 1 MiB), `start(&dir, query, on_batch, on_done, SearchOptions
//!     {contents, include_hidden, recursive, max_results})`, `cancel()`,
//!     `truncated()`, `skipped()`, `is_searching()`. Worker thread; delivery at idle
//!     priority on the caller's thread-default main context.
//! - [`launch`] (file_launch.py) — `launch_with(&AppInfo, &[File], context, on_done)`,
//!   `launch_terminal(&File, context, on_done)`, `terminal_command(dir, env)`,
//!   `FileLauncher` (injectable spawner/environment/which, used by the free
//!   functions), and the helper: **`helper_main(args) -> i32`**, where `args` are the
//!   arguments *after* `--launch-helper`. The lead's `main()` must call
//!   `std::process::exit(files_model::launch::helper_main(&argv[2..]))` when
//!   `argv[1] == "--launch-helper"`, before any GApplication/GTK setup. The helper argv
//!   is `[current_exe, "--launch-helper", "--application", id, uris...]` or
//!   `[current_exe, "--launch-helper", "--terminal", dir, argv...]`.
//! - [`bus`] (file_manager_bus.py) — `FileManagerBus::new(Rc<dyn FileManagerHost>,
//!   default_checker)`, `start() -> bool`, `close()`, `available()`, `last_error()`;
//!   trait `FileManagerHost { connection(), open_locations(files, select, new_window),
//!   show_properties(files) }`; `is_default_file_manager()`, `BUS_NAME`, `INTERFACE`,
//!   `OBJECT_PATH`, `DESKTOP_ID`.
//! - [`completion`] (location_completion.py logic) — `location_file(text, &current)`,
//!   `FolderQuery { parent, prefix, fragment, uri }` + `complete(name)`,
//!   `folder_query(text, &current) -> Option<FolderQuery>`, `LIMIT`, `COMPLETION_ATTRIBUTES`,
//!   `matches_fragment(&info, fragment, include_hidden)`, `sort_names`.
//! - [`goto`] (file_goto.py logic) — `fuzzy_score(query, text) -> Option<i64>`,
//!   `rank(query, &[Entry], limit) -> Vec<(i64, String, bool)>`, `RESULT_LIMIT`,
//!   `INDEX_LIMIT`, `INDEX_DEPTH`, `SKIP`, `FolderIndex::new(roots, include_hidden)`
//!   (`entries()`, `len()`, `done()`, `truncated()`, `stop()`, `join(timeout)`,
//!   `clear()`), `display_path`, `place_entries`.
//! - [`devices`] (files_devices.py data) — `Disk { path, name, device, icon }`,
//!   `local_disks()` (real mount table), `local_disks_from(&[impl MountEntryInfo])`,
//!   `capacity(&FileInfo) -> Option<(size, free, used)>`, `CAPACITY_ATTRIBUTES`.
//! - [`keys`] (file_keys.py) — `QWERTY`, `qwerty(keycode)`, `latin_keyval_with(keyval,
//!   keycode, to_lower, mapped_keyvals)`; the lead wraps it with GDK
//!   (`gdk::keyval_to_lower`, `display.map_keycode`).
//! - [`util`] — `quote`, `unquote`, `expanduser`, `which`, `shlex_split`.

pub mod bus;
pub mod completion;
pub mod devices;
pub mod goto;
pub mod keys;
pub mod launch;
pub mod model;
pub mod util;

pub use model::{
    archive_uri, archive_uri_for_file, compare_items, file_item_metadata, format_modified, format_size, item_count,
    load_desktop_icons_async, name_matches, reconcile_items, sort_items, DirectoryLoader, FileItem, SearchLoader,
    SearchOptions, FILE_ATTRIBUTES,
};

/// Test-process isolation (tools/headless.md): local VFS only (no gvfsd
/// activation for sftp:// URIs), no portals, and an unusable session bus
/// address so nothing can reach the real bus. Called first by every test
/// that touches GIO.
#[cfg(test)]
pub(crate) fn test_env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for (key, value) in [
            ("GIO_USE_VFS", "local"),
            ("GDK_DEBUG", "no-portals"),
            ("GTK_USE_PORTAL", "0"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-files-model-test-bus"),
        ] {
            std::env::set_var(key, value);
        }
    });
}
