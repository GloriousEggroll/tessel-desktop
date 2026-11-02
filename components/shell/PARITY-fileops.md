# tessel-shell fileops + wallpaper chooser parity

Sources: `tessel-shell/desktop_file_ops.py`, `shared/wallpaper_chooser.py`, `shared/wallpaper_import.py`,
`tessel-files/media_chooser.py` (Browse dialog, compact port). Modules: `src/fileops/`, `src/wallpaper_chooser/`
(feature `fileops`).

Run: `TMPDIR=/run/user/1000/cbt/rust-ws-fops cargo test -p tessel-shell --no-default-features --features fileops`;
widgets: `components/shell/tests/run_headless_fileops.sh` (private nested headless labwc, temp HOME/XDG).

## File operations (contract §7, §9 "File ops")
| Item | Status | Test |
|---|---|---|
| `unique_name` `base (n).ext`, dotfiles keep whole name | done | fileops::unique_name_keeps_extension_and_dotfiles, unit splitext |
| clipboard parse: NUL→newline, copy/cut, gnome needs verb, `#`/no-`:` dropped, Python `splitlines` | done | fileops::clipboard_payloads, unit splitlines |
| planning off GTK thread, same-path copy→KEEP_BOTH / move→SKIP, `/`→`_` | done | recursive_copy_keep_both_and_replace, move_skip_and_same_item |
| ConflictResolver: asked in turn, apply-to-all, dismissed=SKIP, presets not asked, no nested loop, defaults REPLACE/KEEP_BOTH | done | asks_each_conflict…, dismissed_prompt…, unresolved_items_default… |
| Gio copy/move worker, recursive, Replace (Trash → delete fallback), cross-fs move fallback | done | recursive_copy_keep_both_and_replace |
| folder into itself, missing source → errors | done | folder_into_itself_and_missing_source_report_errors |
| cancel removes partial copy (before start and mid-copy) | done | cancel_stops_and_removes_partial_copy, cancel_mid_copy_removes_partial_file |
| progress throttled 100 ms, marshalled to main loop | done | progress_is_reported_on_the_main_loop |
| notify `(susssasa{sv}i)`, app `tr("Desktop")`, `dialog-error-symbolic`, urgency y 1, 8000, async, failures logged | done | notification_parameters_match_python, fileops_bus (fake service on gio::TestDBus) |
| `xdg-settings get default-web-browser` async | done | fileops_bus (fake xdg-settings) |
| TrashWatcher: shared, async `trash::item-count`, coalesced queries, 150 ms debounce, monitor-driven, close | done | trash_count_is_queried_asynchronously_and_shared, trash_changes_are_debounced_150ms, real_directory_monitor_drives_the_count |
| rename rules (`/`, `.`, `..`), `.desktop` Name=, async rename/trash/empty trash | done (helpers; DesktopView wires them) | rename_rules, rename_and_empty_trash_are_async |
| Toast after 400 ms, failure dialog, position reservation | DesktopView (desktop agent) | — |

## Wallpaper import
All 24 Python cases ported 1:1 (`tests/wallpaper_chooser_import.rs`) with a fake pkexec in $TMPDIR:
snapshot + one `pkexec --disable-internal-agent cp --update=none-fail --no-preserve=mode,ownership -- … folder` per batch,
126 cancel / 127 deny, race keeps others' files, numbered `(2)` suffixes, symlinks never followed, safe filenames,
removal via `unlink --` with identity checks. Test seams on `Importer`: `deny_unprivileged`, `after_copy`, paths.

## Wallpaper chooser
| Item | Status | Test |
|---|---|---|
| listing filter/sort, ffprobe argv + 4 s timeout, default folder order, 224×126 previews, validation messages | done | wallpaper_chooser.rs (5) |
| Add single → selects copy; invalid; multiple keeps gallery open; validation error correctable; partial batch failure | done | wallpaper_chooser_ui |
| Browse: single/multi selection, type filter keeps folders, navigation/back, accept, Escape/parent close | done (compact) | wallpaper_chooser_ui |
| Removal confirm (cancel default, Escape, close ≠ confirm), removal updates/keeps selection, auth cancel/failure | done | wallpaper_chooser_ui |
| Preview frame/trash inset at two column widths; scroll-bounded decode, cache ≤64, pending ≤8, navigation | done | wallpaper_chooser_ui |
| callback exactly once (close, parent unrealize, cancellable); single-instance staging like DesktopView | done | wallpaper_chooser_ui |

## Gaps
- Browse dialog is a compact MediaChooser: no icons/compact/details views, zoom, thumbnails toggle, bookmarks,
  devices, filter entry, content-type check (suffix only). The real one belongs to the tessel-files port.
- Python attaches the companion theme to the removal prompt only when the parent has one; Rust always calls
  `suite_gtk::theme::attach` (idempotent, no palette ⇒ no change).
- Python's `Global Settings` chooser tests belong to tessel-settings.
- Video thumbnail lookup hashes `g_file_get_uri` (Python used `Path.as_uri`; differs only for exotic characters).
