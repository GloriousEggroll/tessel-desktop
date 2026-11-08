# tessel-files (Files, `tessel-files`) — external contract

Source: `~/tessel-bluetooth/tessel-files` (Python/GTK4, ≈8.5k LOC + ≈5.6k LOC tests; current as of
2026-10-29). Independent file manager, application id **`org.tessel.Files`**. No service, no
autostart, no Global Settings page, no dock control. Rust crates (one owner each during the port):

| Crate | Dir | Python modules |
|---|---|---|
| `tessel-files-ops` | `components/files/ops` | file_operations.py, file_tools.py, file_extras.py, file_preferences.py |
| `tessel-files-model` | `components/files/model` | file_model.py, file_launch.py, file_manager_bus.py, location_completion.py (logic), file_goto.py (index/rank), files_devices.py (data) |
| `tessel-files-widgets` | `components/files/widgets` | files_style.py, files_information.py, file_print.py, file_quicklook.py, file_dialogs.py, file_jobs.py, files_devices.DiskUsage |
| `tessel-files` (bin `tessel-files`) | `components/files` | file_manager.py, files_ui.py, files_pane.py, files_columns.py, location_completion.LocationCompletion, file_goto.GoAnywhere, ark_drop.py, file_keys.py |

`media_chooser.py` is not part of the Files binary; its consumer (shared/wallpaper_chooser.py) is
already ported as `components/shell/src/wallpaper_chooser/media_chooser.rs`.

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installer | `desktop-apps/install.py [--apply] [--only tessel-files]` (root, pkexec) and the older `tessel-files/install.py [--apply]`: copy every non-test `*.py` (not `install*.py`) to `/usr/local/lib/tessel/tessel-files/`, plus shared `gtk4_utils.py suite_i18n.py language_data.py suite_catalogs.py background_blur.py libwaydock-background-blur.so app_menu.py` and tessel-dock `companion_theme.py app_catalog.py dock_appearance.py` | desktop-apps/install.py:15-60, tessel-files/install.py |
| Launcher | `/usr/local/bin/tessel-files` (0755) = `#!/bin/sh\n# Tessel standalone application\nexec /usr/bin/python3 "/usr/local/lib/tessel/tessel-files/file_manager.py" "$@"\n` | desktop-apps/install.py:48-51 |
| .desktop | `/usr/share/applications/org.tessel.Files.desktop` (`Exec=/usr/local/bin/tessel-files %U`, `Icon=system-file-manager`, `StartupWMClass=org.tessel.Files`, `MimeType=inode/directory;`, actions `NewWindow` (`--new-window`), `Home` (`--home`)) | org.tessel.Files.desktop |
| GApplication | id `org.tessel.Files`, flags `HANDLES_COMMAND_LINE` (unique; a second launch forwards its argv to the primary) | files_ui.py:65-67 |
| Wayland app_id | `org.tessel.Files` | |
| Other consumers | `shared/wallpaper_chooser.py` imports `media_chooser` (+ files_pane, location_completion, ark_drop, file_model, file_preferences, files_style, files_columns, file_keys) — copied by tessel-shell/install_switcher.py and shared/install_gtk4.py into their own dirs, so they do not depend on the Files install dir | |

### CLI (`file_manager.py`, argparse)
`usage: file_manager.py [-h] [--check] [--new-window] [--select] [--split] [--home] [locations ...]`,
description `Browse files with Tessel.`; `--select` help `Select the supplied files in their
parent folders`, `--split` help `Open two folders side by side`, `--home` help `Open your home folder`.
- `--check`: print `json.dumps({"ready": true, "gtk": 4, "kde_components": false, "embedded_terminal": false, "uri_schemes": [sorted GIO schemes]})`, exit 0 (or `{"ready": false, "error": …}`, exit 1).
- Otherwise `FilesApplication().run([argv0, *argv])`; `do_command_line` re-parses `--new-window --select --split --home locations*` (no help), files via `create_file_for_arg` (relative to the caller's cwd); `--home` inserts Home first.
- Rust: an internal helper mode (`tessel-files --launch-helper …`, see §Model) replaces `python3 file_launch.py --application|--terminal …`.

## 2. Files and D-Bus
Preferences `$XDG_CONFIG_HOME/tessel/tessel-files.json`, GTK bookmarks `$XDG_CONFIG_HOME/gtk-3.0/bookmarks`,
custom actions `$XDG_CONFIG_HOME/tessel/file-manager-actions.json`, annotations `user.tessel.metadata` xattr +
`$XDG_DATA_HOME/tessel/file-metadata.json` (§3); `org.freedesktop.FileManager1` (§4); palette
`$XDG_CACHE_HOME/tessel-dock/palette.json` (read, suite_gtk::theme); `~/.config/tessel/language.json` (read, i18n).
Settings ▸ "Use Files as the Default File Manager" writes `inode/directory` and `x-scheme-handler/trash` defaults
to `$XDG_CONFIG_HOME/mimeapps.list` via GIO.

## 3. Operations (tessel-files-ops)
<!-- owner: tessel-files-ops agent -->
Crate `files_ops` (no GTK). API summary in `ops/src/lib.rs`. Python error texts below are what
`str(exception)` shows; Rust `Error`'s `Display` is the same (`ErrorKind` = Python class). The
messages are untranslated in Python (no catalog entries exist); Rust routes the fixed ones through
`tr!`, so they stay English until a catalog adds them.

### 3.1 file_operations.py → `files_ops::operations`
- Kinds (`Operations::start(kind, &[File], Option<&File>, Callbacks)`): `copy move link` (into a
  destination folder), `duplicate` (beside the original, `"<stem> (copy)<ext>"`, then `(copy 2)`…;
  splitext rules: `.bashrc` has no extension), `rename` (exactly one source, exact new path), `mkdir`
  `touch` (destination only; never replace), `trash` `delete`, `restore` (reads `trash::orig-path`).
  Internal kinds `undo`, `batch_rename`.
- GIO flags: query `NOFOLLOW_SYMLINKS`; copy `NOFOLLOW_SYMLINKS|ALL_METADATA`; moves add
  `NO_FALLBACK_FOR_MOVE` (on `NOT_SUPPORTED`/`WOULD_RECURSE` → fingerprint, staged copy, re-fingerprint,
  delete source). `INFO` = `standard::name,standard::type,standard::size,standard::symlink-target,time::modified,time::modified-usec,etag::value,id::file`.
- Staging names: `<parent>/.tessel-files-copy-<32 hex>`, `.tessel-files-replaced-<hex>`, `.tessel-files-rename-<hex>`.
  Copies are built in the temporary and promoted with a non-overwriting move; temporaries are removed
  on failure/cancel. Replace moves the old item to `.tessel-files-replaced-…`, restores it on failure (or
  reports it), deletes it on success; a replacing job never offers undo.
- Conflicts: `Conflict{source, destination, is_directory, can_merge, choices}`; choices
  `skip replace rename cancel` (+`merge` when both are directories, inserted before `cancel`). No handler
  ⇒ skip (added to `skipped`). First answer wins; `cancel` cancels the job; a dismissed dialog must
  answer `cancel`; `Job::cancel()` wakes a pending conflict; a panicking handler answers cancel.
- Progress: `Progress{kind, source, destination (URIs or ""), current_bytes, total_bytes (per file),
  completed_items, total_items}`; ≥ 75 ms apart unless forced (start of each item, after each item,
  each batch-rename commit).
- Result: `OperationResult{kind, completed (URIs), failures [Failure{source URI or "", message}],
  skipped, cancelled, undo_available}`, `success()` = no failures and not cancelled.
  GLib errors are reported as PyGObject `str(GLib.Error)`: `"g-io-error-quark: <message> (<code>)"`.
- Undo: history of ≤ 20 groups (`HISTORY_LIMIT`), one per job; failed/cancelled undo records are
  re-queued. Records: copy/mkdir/touch → remove if fingerprint unchanged; move/rename → move back if
  unchanged and the original path is free; merge-move → recreate the emptied source folder; batch →
  reverse batch if all fingerprints unchanged; trash → find the exact entry in `trash:///` by original
  path + (`id::file`, type, size, mtime, usec) and move it back (mounting Trash on a private worker
  context, 10 s limit). Restore and replace record no undo; delete has none.
- Batch rename: pairs must stay in their folder; identical pairs dropped; stage all, then commit
  without overwrite; on error/cancel vacate committed names, then restore originals.
- Callbacks run on the GLib main context that was thread-default at `start()` (the default context on
  the GTK thread) via an async channel — `start` must be called on the thread owning that context when
  any callback is given. Workers are named `Tessel Files-<kind>` (std threads, joinable via `Job::wait`).
- Error strings: `A folder cannot be copied or moved into itself.` · `The filesystem root cannot be replaced.` ·
  `The filesystem root cannot be duplicated.` · `Enter a file name without directory separators.` ·
  `Invalid conflict decision.` · `Symbolic links require a local source.` ·
  `The source changed while moving; both copies were kept.` ·
  `The previous destination is preserved here; restore it manually.` (failure on the backup URI) ·
  `Every source and destination in a rename batch must be unique.` · `Batch rename keeps each item in its existing folder.` ·
  `The source no longer exists: {name}` · `The new name is already in use: {name}` (parse names) ·
  `Could not roll back this renamed item: {error}` · `Original item is preserved here; its name could not be restored: {error}` ·
  `Choose a destination folder.` · `Choose a new file name.` · `Choose a file name.` · `Rename one item at a time.` ·
  `The trash entry has no original location.` · `Unknown operation: {kind}` ·
  `The original location is occupied; the deleted item remains in Trash.` ·
  `This Trash backend cannot identify the deleted item. Restore it from Trash.` ·
  `The deleted item is no longer uniquely identifiable in Trash; nothing was restored.` ·
  `An item changed since the batch rename; it was left untouched.` · `The original folder was recreated; undo stopped for it.` ·
  `The item changed since the operation; it was left untouched.` · `The original location is occupied; it was left untouched.`

### 3.2 file_tools.py → `files_ops::tools`
- `create_archive`: suffix (case-insensitive) `.zip` · `.tar.gz`/`.tgz` · `.tar.bz2`/`.tbz2` · `.tar.xz`/`.txz` · `.tar`;
  else `Use .zip, .tar, .tar.gz, .tar.bz2, or .tar.xz`. Destination opened `O_EXCL` (`[Errno 17] File exists: '<path>'`),
  removed on any failure/cancel. Walk: no symlinks (`Archive creation does not follow symbolic links: <path>`), no
  specials (`Unsupported special file: <path>`), unique top names (`Archive source names must be unique`), not inside
  a source (`The archive cannot be created inside a selected source folder`, `Choose an archive destination outside the selected folders`),
  `Select at least one file or folder`. Members in sorted pre-order. ZIP: deflate (level 6), zip64 for files, mode bits,
  local-time DOS dates (`ZIP does not support timestamps before 1980`). TAR: ustar/PAX (`path` record when non-ASCII
  or > 100 bytes), uid/gid 0, empty user/group, mode `& 0o777`, integer mtime, directories with trailing `/`, padded to
  10240-byte records; gzip level 9 (header name = archive name minus `.gz`, OS 255), bzip2 9, xz preset 6.
- `extract_archive`: ZIP if a central directory is found, else tar (plain/gzip/bzip2/xz/zstd by magic). All members are
  validated first: `Unsafe archive member: <name>` (NUL, `\`, leading `/`, `X:`, `..`), ZIP type bits other than
  0/regular/dir or encrypted → `Unsupported archive link, special file or encrypted entry: <name>`, tar links/devices/fifos →
  `Archive links and special files are not extracted: <name>`, `Duplicate archive member: <name>` (directories may repeat),
  `An archive file is also used as a folder`; `.`/`./` directory entries are skipped. The destination folder is created
  (not its parents); everything is created through `O_NOFOLLOW` directory fds (dirs 0755, files `O_EXCL` 0600 then
  `fchmod(mode & 0o777)`; ZIP entries without Unix bits → 0644). Failure/cancel removes exactly what was created.
  Returns created paths. Cancel error `Operation cancelled` (`ErrorKind::Cancelled`); progress `(copied, member size)`
  per 1 MiB chunk.
- `plan_batch_rename(paths, pattern, start)`: `#…#` → zero-padded counter, file suffix (Python 3.14 `Path.suffix`) kept,
  folders get none. Errors `Select one or more distinct files`, `Enter a file name without path separators`,
  `Include # in the name to number multiple files`, `The starting number must not be negative`, `Invalid file name`,
  `A file already exists at <target>` (FileExistsError).
- `file_metadata` (lstat: name, absolute path, size, mode `& 0o7777`, `stat.filemode` text, uid/gid, owner/group name or id,
  float mtime/atime, is_directory, is_symlink, link_target), `recursive_size` (no links followed, hard links once,
  unreadable count), `checksums(path, algorithms)` (md5 sha1 sha224 sha256 sha384 sha512 sha512_224 sha512_256 sha3_* blake2b
  blake2s; else `unsupported hash type <name>`; regular files only, `O_NOFOLLOW`; dialog uses sha256, sha512, md5).
- `set_permissions(path, mode)` (`Permissions must be an octal mode between 0000 and 7777`), `set_owner(path, uid, gid)`
  (number, decimal string or name; `KeyError` `getpwnam(): name not found: '<name>'`); both through an `O_PATH|O_NOFOLLOW`
  fd + `/proc/self/fd/N` (`Permissions can only be edited for regular files and folders`).
- ACL: `acl_available()` = `getfacl` and `setfacl` on PATH; `read_acl` = `getfacl --omit-header --absolute-names -- /proc/self/fd/N`,
  `write_acl` = `setfacl --set-file=- -- /proc/self/fd/N` (text on stdin; `Enter a valid access control list`); 15 s timeout;
  non-zero exit → OSError(stderr stripped or `The access control list could not be updated`); missing tools →
  `Install the acl package to manage access control lists`.

### 3.3 file_extras.py → `files_ops::extras`
- Metadata xattr `user.tessel.metadata` = compact UTF-8 JSON `{"tags":[…],"rating":n,"comment":"…"}` (≤ 64 KiB read).
  Normalization: ≤ 64 tags, NUL removed, CR/LF → space, stripped, ≤ 128 chars, case-insensitively unique; rating int clamped
  0–5 (non-int → 0); comment NUL removed, ≤ 8192 chars. Opened `O_RDONLY|O_NOFOLLOW|O_NONBLOCK`; regular files and folders
  only (`File annotations require a regular file or folder`); local only (`File annotations are available for local files`,
  `A file URI cannot contain a query or fragment`). Unknown fields → TypeError `Unknown annotation fields: a, b`.
- Fallback (xattr errno ENOTSUP/EOPNOTSUPP/ENOSYS/E2BIG/ENOSPC): `$XDG_DATA_HOME/tessel/file-metadata.json`
  (absolute XDG value only, else `~/.local/share`), `{"file:///…": {"identity": [st_dev, st_ino], "metadata": {…}}}`,
  `indent=2`, non-ASCII kept, trailing newline, mkstemp `.file-metadata.json.XXXXXXXX` (0600) + fsync + rename, under
  `flock` of `file-metadata.json.lock` (0600); ignored above 16 MiB. A matching fallback record wins over the xattr; a
  successful xattr write removes the record. Other errnos (EACCES …) propagate. Reads never create files.
- `status_for_directory(path, timeout=4 s)`: `git --no-optional-locks -c core.fsmonitor=false -c core.untrackedCache=false
  -c core.hooksPath=/dev/null -C <realpath> rev-parse --show-toplevel` then `status --porcelain=v1 -z --untracked-files=normal
  --ignored=no -- .`; labels `Conflict` `Untracked` `Ignored` `Renamed|Deleted|Added|Modified|Type changed|Copied|Changed`
  (+` (staged)` when only the index column is set); parents up to the folder get `Contains changes`. Any failure/timeout → empty.
- Custom actions: `$XDG_CONFIG_HOME/tessel/file-manager-actions.json` (≤ 1 MiB, ≤ 128 records):
  `[{"name", "argv": [...], "directories": true, "files": true}]`; name non-blank ≤ 128; argv 1–128 strings ≤ 8192 chars, no
  NUL, `argv[0]` non-empty without `%`. Expansion: whole-argument `%F` (local paths; `This action requires local files`) and
  `%U` (URIs); inline `%f` `%u` `%%`; others `Unsupported action placeholder %<c>`; `Select at least one file`. Paths become
  URIs with Python's `Path.as_uri()` quoting. Invoked with null stdio in a new session (`setsid`), optional cwd.

### 3.4 file_preferences.py → `files_ops::preferences`
- `$XDG_CONFIG_HOME/tessel/tessel-files.json` (absolute XDG value only): `json.dumps(indent=2, ensure_ascii=False)`
  + `\n`, keys in `DEFAULTS` order, unknown keys dropped. Writes: `flock` on `<file>.lock` (0600), re-read + merge, mkstemp
  `.<name>.XXXXXXXX` (0600) in the symlink-resolved directory, fsync, rename (a symlinked file keeps its link). Files > 8 MiB,
  invalid UTF-8/JSON → defaults (file untouched). Unknown keys → KeyError `'Unknown file manager preferences: a, b'` /
  `'Unknown folder options: …'`; `A folder URI is required`.
- `DEFAULTS`: `view icons, icon_size 64, sort name, descending false, folders_first true, natural_sort true, group_by none,
  show_hidden false, show_previews true, show_statusbar true, show_sidebar true, show_information true, remember_folders false,
  split_view false, location_editable false, single_click false, confirm_delete true, restore_session true, tabs [], tab_splits [],
  active_tab 0, window_size [1100, 760], bookmarks [], folders {}, search_saved [], information_width 360, folder_peek true,
  vim_keys false`. Choices: view `icons compact details columns`, sort `name size modified type`, group_by `none name type size
  modified`; icon_size int 16–256; information_width int 300–800; tabs ≤ 64 valid URIs (split per tab or `""`); active_tab
  clamped; window_size ints clamped to 640–7680 × 400–4320; bookmarks ≤ 512 unique `{uri, label}`; folders ≤ 4096 URI →
  FOLDER_KEYS subset; search_saved ≤ 128 `{name (label or query[:128]), uri, query[:4096], contents bool}`.
  `valid_uri`: 1–8192 chars, no whitespace (`str.isspace`) or NUL, `^[A-Za-z][A-Za-z0-9+.-]*:`. Labels: NUL removed,
  CR/LF → space, stripped, ≤ 1024 chars.
- GTK bookmarks `$XDG_CONFIG_HOME/gtk-3.0/bookmarks`: lines `<uri>[ <label>]`; other lines kept verbatim; `add` appends
  (existing URI unchanged; `A valid bookmark URI is required`), `rename`, `remove`; written `"\n".join(lines) + "\n"`
  (no trailing newline only when `remove` empties the file).
- Reused by `media_chooser` (`tessel/media-chooser.json` via an explicit path).

## 4. Model (tessel-files-model)
<!-- owner: tessel-files-model agent -->
Crate `tessel-files-model` (lib `files_model`), one module per Python file; API summary in `model/src/lib.rs`.

**file_model.py → `model`**
- `FILE_ATTRIBUTES` = `standard::*,time::modified,time::modified-usec,time::modified-nsec,access::can-read,access::can-write,access::can-delete,access::can-trash,access::can-rename,thumbnail::*,unix::mode,unix::uid,unix::gid,owner::*,trash::orig-path,trash::deletion-date`; query flags NONE (links resolved).
- `FileItem` is a GObject (`TesselFilesFileItem`) for `gio::ListStore`. Identity = URI. `name` = `standard::name` byte string or basename; `display_name` falls back to name; `is_hidden` = is-hidden or leading `.`; content type default `application/octet-stream`; icon = `standard::icon` or themed `folder`/`text-x-generic`; permissions default false; `type_description` = `Folder` (untranslated, as Python) or `g_content_type_get_description`.
- Launcher icons: only local regular `*.desktop` files of 1..=256 KiB, bounded reads, UTF-8 key file, `Type` ∈ {Application, Link, Directory}, stripped `Icon` without control chars; absolute path → `GFileIcon` only if the file exists; bare name (no `/ \ :`) → themed icon with the file's own icon names appended as fallbacks; Exec is never read. Async enrichment runs on GIO's worker pool; completes synchronously when a batch has no launcher, otherwise always calls back on the caller's main context (also after cancellation).
- `file_item_metadata`: name, display name, is_dir, is_symlink, is_hidden, standard::type, size, modified, modified-usec, modified-nsec, content type, icon (`g_icon_to_string`, else serialized print, else object identity), 5 permissions, mode, owner, group, thumbnail path/valid, symlink target, trash orig-path, deletion-date. Access time excluded.
- `reconcile_items(store, incoming, remove_missing)`: reverse walk; remove missing (if `remove_missing`); `splice(i,1,[new])` when metadata differs; new URIs appended in first-seen order (last duplicate wins) in one splice; returns changed.
- Sorting: folders first (not reversed by `descending`), key `name|size|modified|type`, natural case-folded key (`\d+` runs numeric, text before numbers), ties by display name then URI. `name_matches`: casefold; empty → true; `*?[` → `fnmatchcase`; else substring.
- `item_count`: `tr("1 item")` / `tr("{count} items")`. `format_size`: `<1024` → `"{:,} B"`, else `"{:,.1f} KiB…EiB"`. `format_modified`: local `strftime("%b %d, %Y %H:%M")`, `—` for 0/invalid. `archive_uri`: `archive://` + quote(quote(uri, safe=""), safe="") + `/`; empty → `Choose an archive file`.
- `DirectoryLoader(batch_size=128, clamp 1..512)`: `enumerate_children_async` + `next_files_async(batch)` at DEFAULT priority; each batch enriched before `on_batch`; `on_done(None|error)` once; `cancel()`/restart suppress every old callback; enumerators closed async.
- `SearchLoader(batch_size=64, max_content_bytes=1 MiB, clamp 1..512 / 1..16 MiB)`: worker thread `file-manager-search`, depth-first stack, depth < 128, symlinked dirs never queued, hidden skipped unless `include_hidden`, `recursive`, `max_results` (default 10000, clamp 1..100000); content search only for regular non-symlink files, 64 KiB reads up to the cap, any NUL → no match, UTF-8 replace + casefold substring; unreadable subfolders/contents → `skipped`; root failure → `on_done(error)`; `truncated` = reached max. Delivery at DEFAULT_IDLE on the caller's main context.

**file_launch.py → `launch`** (strings untranslated, as Python)
- `launch_with(app_info, files, context, on_done)`: no desktop id → `The selected application does not have a desktop entry.`; Tessel Dock `launch_environment` + `launch_command(…, forking)` (`systemd-run --user --collect --quiet --service-type=forking --property=GuessMainPID=no --property=ExitType=cgroup --property=TimeoutStartSec=90s --slice=app.slice --expand-environment=no --unit=app-tessel-dock-<hex>.service --setenv=NAME… --`), environment via `set_environ`, stdout silenced, stderr piped. Failure: stripped stderr or `Could not launch the selected application.` / `Could not open a terminal in this folder.`; communicate error → its message; the activation token is retired (`launch_failed`) on spawn or service failure.
- Helper argv (replaces `python3 file_launch.py …`): `[tessel-files, "--launch-helper", "--application", <id>, <uris>…]` / `[tessel-files, "--launch-helper", "--terminal", <dir>, <argv>…]`. **The lead's `main()` must call `files_model::launch::helper_main(&argv[2..])` and exit with its status when `argv[1] == "--launch-helper"`, before GApplication/GTK.** Helper: `--terminal` (≥3 args) → SubprocessLauncher (stdout+stderr silenced), cwd = dir, no wait; 0, or 1 + message on stderr. `--application` (≥3 args) → `DesktopAppInfo.new(id)` (else `Application no longer installed.`, 1), token from `TESSEL_DOCK_LAUNCH_STARTUP_ID` (removed) in Tessel Dock's transferred launch context, `launch_uris_async`; false → `The selected application could not be launched.`; anything else → 2.
- `launch_terminal(dir)`: no local path → `This location has no local folder path. Mount it locally before opening it in a terminal.`. Terminal command: `xdg-terminal-exec --dir=<dir>` first, else `shlex.split($TERMINAL)` (error `The configured terminal command is invalid.`), then x-terminal-emulator, ptyxis, kgx, gnome-terminal, konsole, xfce4-terminal, tilix, alacritty, kitty, wezterm, foot, ghostty, xterm (`which` on the launch environment's PATH); directory flags by the resolved executable name (ptyxis `--new-window --working-directory`; kgx/gnome-terminal/xfce4-terminal/tilix/alacritty/foot/footclient `--working-directory`; konsole `--workdir`; kitty `--directory`; wezterm `start --cwd`; ghostty `--working-directory=`); none → `No terminal application was found. Install a terminal or set the TERMINAL environment variable.`

**file_manager_bus.py → `bus`**: name/interface `org.freedesktop.FileManager1`, path `/org/freedesktop/FileManager1`, methods `ShowFolders`, `ShowItems`, `ShowItemProperties` (`as URIs, s StartupId`). Only while the `inode/directory` default is `org.tessel.Files.desktop` (re-checked on `GAppInfoMonitor::changed` and on every call): `RequestName(name, 4 = DO_NOT_QUEUE)` (2 s); owner on reply 1|4, else `last_error` = `Another file manager owns FileManager1`; NameOwnerChanged to "" → retry; default lost / `close()` → `ReleaseName`. Calls when not active → `org.freedesktop.DBus.Error.NotSupported` `Files is not the active default file manager`; >128 URIs, or a URI >8192 chars / without `scheme:` / with whitespace or NUL → `…InvalidArgs` `Expected at most 128 valid file URIs`; host error → `…Failed`; empty list → no-op success. `ShowItems` = open with select, `ShowFolders` = open, `ShowItemProperties` = properties; never a new window. No activation file is installed. App side: trait `FileManagerHost`.

**location_completion.py (logic) → `completion`**: `location_file` (expanduser; absolute or URI scheme → commandline arg, else relative to the viewed folder); `folder_query` (`~` → `~/`; `scheme://` without a `/` after the authority → none; split at the last `/`; URI fragments unquoted, completions quoted with `safe=""` plus `/`); `LIMIT` 128; attributes `standard::name,standard::type,standard::is-hidden`; directory + prefix + hidden rule; sort by `(casefold, name)`.

**file_goto.py (logic) → `goto`**: `INDEX_LIMIT` 80 000, `INDEX_DEPTH` 10, `RESULT_LIMIT` 60, `SKIP`; `fuzzy_score`; `rank` (last word vs names, a `/` in it falls back to the path at −500, other words must occur in the path, +bonus, +40 dirs, −4 per `/`, sort (−score, path)); `FolderIndex` (thread `files-goto-index`, BFS, no symlink follow); `display_path` (`~`); places `(path, name or path, true, 400)`.

**files_devices.py (data) → `devices`**: `local_disks` (mount table; `/` first as `System`; skip non-`/dev/` and `/dev/loop*`, squashfs/tmpfs/devtmpfs, the `/boot /proc /sys /dev /var/lib /usr` trees, `/run/*` except `/run/media/*`, repeated devices); `capacity` (needs size and free, size > 0, clamped, used defaults to size − free).

**file_keys.py → `keys`**: QWERTY table by XKB keycode; `latin_keyval_with` (GDK lowering and `map_keycode` supplied by the lead).

## 5. Widgets (tessel-files-widgets)
<!-- owner: tessel-files-widgets agent -->
No files, D-Bus names or CLI of their own. External surface = CSS classes, strings, GTK behaviour.
Strings: files_information/file_print/file_quicklook/file_dialogs/files_devices use **untranslated English
literals** (kept so in Rust); file_jobs.py and `item_count` use `tr()`.

| Item | Value | Ref |
|---|---|---|
| Stylesheet | `CSS`, `NAVIGATION_CSS` byte-identical; Python installs CSS at APPLICATION, NAVIGATION_CSS above the palette theme (files_ui.py) | files_style.py |
| CSS classes | `files-image-preview files-text-preview files-info files-info-header files-info-content heading title muted files-quicklook files-disk-usage low-space files-jobs files-job files-jobs-indicator files-window flat destructive-action suggested-action boxed-list dim-label title-2 monospace` | |
| Preview kinds | `image/*`→image, ⊂`application/pdf`→pdf, `video/* audio/*`→media, ⊂`text/plain`→text, dirs/others → none | files_information.preview_kind |
| Image decode | streamed 64 KiB, > 128 MiB → "This image is too large to preview.", scaled to ≤ 1200 px (round), EXIF orientation applied (5–8 swap dimensions); details `Dimensions` "`2,400 × 1,600 pixels`", `Format` (upper), then photo headers `Color mode, Camera (Make Model deduped), Date taken, Lens, ISO, Exposure (1/N s or n:g s), Aperture f/n:g, Focal length n:g mm`, values stripped, NUL removed, ≤ 512 chars; GLib errors → "Preview unavailable" | decode_preview/photo_details |
| Text | ≤ 1 MiB (`TEXT_LIMIT`), NUL in first 8192 bytes → "This file contains binary data.", truncated text ends at the last `\n`; UTF-8 with replacement; `Lines` "`1,234`" + "`+ (first 1 MiB shown)`"; GtkSourceView 5 language guess, `Adwaita`/`Adwaita-dark` scheme by foreground luminance > .5; line numbers only in Quick Look; wrap WORD_CHAR (pane) / NONE (large) | read_text/decode_text/_show_text |
| PDF | ≤ 256 MiB, first page on white at 1200 px (B8G8R8A8 premultiplied); `Pages`, `Page size` "`210 × 297 mm`" (.0f), `Title`, `Author`; poppler-glib 0.18 (`libpoppler-glib.so.8`) | decode_pdf |
| Media | `Gtk.Video(autoplay=False)` + `Gtk.MediaFile`; never plays by itself; released (`clear()`) on selection change/close; `Duration` m:ss / h:mm:ss, `Contents` Video/Audio | _show_media |
| PreviewLoader | one running decode, one replaceable pending request (no backlog), results ignored after cancel/close | PreviewLoader |
| InformationPane | width 300, header "Preview", print button (`printer-symbolic`, "Print…", only when printable), close button "Hide preview pane — F11"; selection key = folder URI + (uri, mtime, size, usec, nsec); none → folder name, "Select a file to preview"; many → `item_count`, "Select one file to preview", `Total size`; one → `Type`, `File size` (files only), `Modified` (+ preview rows first) | InformationPane |
| Printing | GtkPrintOperation (job name = display name, embed page setup, async, not full page); pdf via `render_for_printing` scaled/centred, image via pixbuf (orientation), text: Monospace 9 WORD_CHAR paged by layout lines; other → "This file type cannot be printed from Files."; `export_to` = synchronous EXPORT | file_print.py |
| Quick Look | title "Quick Preview", ≥ 640×480 or 80 %/85 % of parent, non-modal, header Open + Print…; keys (no Ctrl/Alt): Space/Escape close, Return/KP_Enter open, Left/Right step, Up/Down step unless text preview; closing releases the preview | file_quicklook.py |
| Dialogs | modal `.files-window`, suite headerbar, dock theme, Escape closes; `show_error` "Could not complete operation"; `confirm` Cancel/Confirm (destructive), close = False; `prompt_text` Cancel/OK, Enter accepts, close = None; one answer per dialog | file_dialogs.py |
| choose_file | GtkFileChooserWidget (no portal), 840×580, modes open/save/folder (Open/Save/Select Folder), "Choose a file"/"Choose a folder" when nothing chosen, never creates the file | choose_file |
| Open With | 560×560, search ("Search applications…"), A–Z/Z–A, all apps + MIME handlers (NoDisplay handlers shown, Hidden never), rows show "Current default"/"Recommended"/comment, sort key (casefold name, casefold id, id); Enter/Open rebuild with the current query first; "Use as default for {description} ({type})" changes the MIME default **only when checked and confirmed**; failure keeps the window open with "Could not set the default application: …" | choose_application |
| Properties | 590×560 (min 420×320), pages General / Tags & Notes / Permissions / Open With / Checksums (+ Access Control for one local path); size "`16 B (16 bytes)`" + recursive "`n files, m folders`"; octal mode entry ↔ 9 check boxes; ownership (empty = unchanged); ACL via getfacl/setfacl; tags/rating/comment via `MetadataStore` only on "Save tags and notes"; association via "Set as default"; SHA-256/SHA-512/MD5; closing cancels work | PropertiesWindow |
| Jobs | each job holds the GApplication; notification id `files-job-<n>`, title "File operation finished"/"File operation failed", icon `system-file-manager`, only when no Files window is alive and not cancelled; button tooltip "{count} file operations running"/"1 file operation running", 250 ms pulse; `ask_keep_running` "File Operations Running", Cancel Operations / Keep Running in Background (default), close = None | file_jobs.py |
| DiskUsage | "Checking space…", one async `filesystem::size,free,used` query at LOW priority (never queued), "X free\nY total", tooltip "path\nU used · F free · S total", `.low-space` under 10 % free, "Space unavailable" | files_devices.DiskUsage |

## 6. Application window and panes (tessel-files)
Source: files_ui.py, files_pane.py, files_columns.py, location_completion.LocationCompletion, file_goto.GoAnywhere,
ark_drop.py, file_keys.latin_keyval. Rust: `components/files/src/{app,window,pane,columns,completion,goto,ark_drop,keys,thumbnails,tile,cli}.rs`.

- **Application** (`FilesApplication`): unique `org.tessel.Files`, `HANDLES_COMMAND_LINE`. Startup: Tessel Dock
  `AppearanceController` (Papirus light/dark), `files_style.CSS` at APPLICATION, `NAVIGATION_CSS` at APPLICATION+31,
  companion theme on every window, clipboard `changed` → generation counter, accelerators (`win.*` table of files_ui.py:94-105),
  FileManager1 bus. `command_line`: add_help=False parser (`--new-window --select --split --home locations*`), files via
  `create_file_for_arg` (caller's cwd). `open_locations(files, new_window, select, split)`: reuses the active window
  unless `new_window`; a new first window restores saved tabs (once per session) before showing a named folder, reusing a
  restored tab with the same URI; each location is classified asynchronously (`standard::type`), results applied in argument
  order; `--select`/files group by parent folder (selection merged); `--split` puts the second location in the right pane.
  No locations: restore tabs or Home. `activate` → new window on Home.
- **Window** (`FilesWindow`, `.files-window`, title `<folder> — Files`): header (New tab, Places toggle `win.sidebar`, split,
  app menu with Keyboard Shortcuts/About/Quit (Ctrl+Q asks once about running jobs), jobs button); toolbar (back, forward,
  parent, reload chevrons; address host; Icons/Compact/Details/Columns toggles; Search; preview toggle `win.information`);
  error row `.files-error`; Places sidebar `.files-sidebar` (Places/Bookmarks/Devices with DiskUsage/Network Locations/Saved
  Searches) refreshed on mount/volume changes, usage every 15 s while visible; tabs (`Gtk.Notebook`, tabs shown only with ≥2
  pages, new tabs open next to the active one, ≤20 reopenable closed tabs); content split with the information pane whose
  width (`information_width`, ≥300) is saved 250 ms after resizing and restored after one allocation tick. Session (`tabs`,
  `tab_splits`, `active_tab`, `window_size` ≥720×480) saved 300 ms after changes and on close. Closing a window with its
  jobs running asks Keep Running (default) / Cancel Operations; jobs keep the app alive.
- **Actions** (all names of files_ui.py:384-390; stateful: hidden, previews, information, sidebar, descending, folder-first).
  Editable focus routes copy/cut/paste/select-all/invert-selection/undo/trash/delete to the text field. Delete = Trash,
  Shift+Delete = confirmed permanent delete. Clipboard: `GdkFileList` + `x-special/gnome-copied-files` (`copy|cut\nuri…`, ≤1 MiB read);
  a completed cut clears the clipboard when unchanged since. Messages and strings exactly as files_ui.py (mostly untranslated, as in Python).
- **Context menus**: file menu (Open, Open in New Tab/Terminal for folders, Open With…, Rename…, Quick Preview/Print… for one
  file, Cut/Copy/Duplicate/Copy To…/Move To…/Create Link…, Compress…/Extract…/Browse Archive, Move to Trash or Restore/Permanently
  Delete… in Trash, Custom Actions, Properties…), blank menu (Open in Terminal, New Folder…, New Text File…, Paste, Undo, Reload,
  Bookmark This Folder, Empty Trash… in Trash, Properties…), location menu for breadcrumbs/places (`location.tab|window|terminal`,
  `location.empty-trash`, `bookmark.remove`; terminal disabled for locations without a local path). **Presentation (changed
  2026-10-29):** the popover is parented to the window's child (not the row), `has_arrow=False`, pointing at the click point,
  `position=RIGHT`, and after `popup()` its vertical offset is `measure(VERTICAL).natural // 2` so its top sits at the pointer —
  the compositor slides it vertically instead of GTK shrinking it into a scrolled list. It is detached on close (after the
  action dispatch, via idle) and when its anchor unmaps.
- **Pane** (`BrowserPane`, `.files-pane`, `.active-pane`): breadcrumbs (≤6 ancestors, Home labelled "Home", `.files-crumb`) or
  location entry with folder completion (`.files-completion`, 100 ms debounce, ≤128, Tab/Down/Up/Return/Escape); search box
  (recursive name/contents, ≤10,000 results) and filter; views icons/compact (GridView), details (ColumnView over a
  TreeListModel; Name/Size/Type/Modified/Git columns, header sort persisted globally or per folder), columns (Miller,
  ≤6 parents, stops at Home). Only the displayed view binds models; hidden tabs keep data, expansion, selection and scroll.
  Live monitor updates are coalesced (250 ms, ≤32 queries per round, >1024 events → one rescan), hidden-file events ignored
  while hidden files are hidden. Git labels refreshed at most every 2 s. Folder peek popover after 700 ms (≤40 entries,
  `folder_peek`). Type-ahead (1.2 s). Keys (capture phase): Space = Quick Preview (not while typing); **with nothing
  selected any arrow key selects the first item (changed 2026-10-29)**; Vim j/k/h/l (`vim_keys`, by key position);
  Left/Right in Columns. **After Trash or permanent delete the selection moves to the item before the first deleted item, or
  index 0 when nothing precedes it (changed 2026-10-29)**; partially failed removals keep the selection. Drops: internal
  drags use the drag's action; external sources are offered COPY only, a plain drop from a MOVE-capable source asks
  Copy Here/Move Here/Link Here/Cancel, Shift = move, Ctrl = copy, Ctrl+Shift = link; Ark drops call
  `org.kde.ark.DndExtract.extractSelectedFilesTo(s)` on the source's unique name (path `/DndExtract/…`, 10 s timeout).
- **Thumbnails**: two decodes at a time, 96-texture cache, cached thumbnails only when valid, originals ≤32 MiB, embedded
  orientation applied, scrolled-away requests cancelled.
- **Rust-only**: `tessel-files --launch-helper …` (helper modes, see §4; `--launch-helper --app-catalog <mode> <target>` runs
  Tessel Dock's app_catalog helper for opening files); an argparse error in the primary's command-line handler is reported to the
  caller's stderr with exit 2 (Python raised SystemExit inside the vfunc).
