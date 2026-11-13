# Files (tessel-files) — parity

Contract: `contracts/tessel-files.md`. Python source of truth: `~/tessel-bluetooth/tessel-files` (2026-10-29).
Each section: contract item → implemented? → test name. Gaps are listed honestly per section.

## tessel-files-ops (ops/)
<!-- owner: tessel-files-ops agent -->
Tests: `env -u TMPDIR cargo test -p tessel-files-ops` (61 unit tests; the 6 GVfs tests skip themselves) and
`components/files/ops/tests/run_gvfs.sh` (6 real GVfs tests: private dbus-daemon with only generated
`org.gtk.vfs.Daemon`/`Metadata` service files, private HOME/XDG dirs, private user+mount namespace that covers every
filesystem with a real `.Trash`/`.Trash-<uid>` with an empty tmpfs; fails on any other activation or a skip).
Python tests ported 1:1 (same names); `patch.object` cases use `#[cfg(test)]` hooks (`copy_tree`, `trash`, `notify`,
xattr errno injection). Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

### file_operations.py (`operations`) — test_file_operations.py → `operations::tests` (17/17 + 4)
| Item | | Test |
|---|---|---|
| copy (recursive, symlinks copied as links) + undo | ✅ | `test_recursive_copy_keeps_symlink_and_undo` |
| no copy into itself (prefix + realpath of destination parent) | ✅ | `test_copy_cannot_enter_itself_through_directory_or_symlink` |
| no handler = skip; rename (auto/keep-both, chosen name); replace (no undo); merge; invalid decision | ✅ | `test_default_conflict_skips_without_overwrite`, `test_replace_and_keep_both_conflicts`, `test_rename_choice_with_name_and_conflict_details` |
| replace failure restores backup | ✅ | `test_replace_failure_restores_existing_destination` |
| merge copy/move + undo (mkdir record) | ✅ | `test_directory_merge_and_safe_undo`, `test_move_and_move_merge_undo` |
| undo safety (occupied original, changed copy), failed undo re-queued | ✅ | `test_undo_never_overwrites_recreated_original`, `test_undo_never_removes_changed_copy` |
| cancel removes staging; cancel wakes pending conflict; `jobs()`/`running()` | ✅ | `test_cancel_during_copy_removes_partial_temporary`, `test_cancel_pending_conflict_does_not_block` |
| mkdir/touch (no replace), rename, link, duplicate, delete, unknown kind, "Rename one item at a time." | ✅ | `test_create_rename_link_duplicate_and_delete` |
| trash failure never deletes | ✅ | `test_failed_trash_never_calls_delete` |
| callbacks on the owning main context | ✅ | `test_default_dispatcher_delivers_callbacks_to_main_thread` |
| batch rename swap + grouped undo, collision/uniqueness/folder checks, cancel rollback | ✅ | `test_batch_rename_swap_and_grouped_undo`, `test_batch_rename_collision_changes_nothing`, `test_cancel_batch_commit_restores_all_original_names` |
| progress fields / throttle (75 ms) | ✅ | `test_progress_reports_uris_and_counts` |
| history ≤ 20, `_fingerprint` | ✅ | `test_history_is_bounded_and_fingerprint_detects_changes` |
| `_unique` names, `as_file` | ✅ | `test_unique_names_and_as_file` |
| non-local move fallback (copy + fingerprint check + delete) | ☑ | exercised by GVfs restore/undo (`run_gvfs.sh`) only when the backend lacks move |
| trash identity, undo through GVfs Trash (mount on worker context), restore | ✅ | `tests/gvfs.rs` (below) |
| `Operations(dispatch=…)` injectable dispatcher | ⚠ | not ported: callbacks always go to the thread-default context of `start()` (tests use private contexts) |
| Python `Result` | ⚠ | named `OperationResult` (avoids shadowing `std::result::Result`); `Job.result` is `Job::result()` (only once finished) |
| `start()` accepting paths/URI strings | ⚠ | takes `gio::File`; convert with `as_file()` |
| exception in `on_conflict` → cancel | ⚠ | a panic in the handler is caught and answers `cancel` |

### file_tools.py (`tools`) — test_file_tools.py → `tools::tests` (15/15 + 1)
| Item | | Test |
|---|---|---|
| zip/tar/tar.gz/tar.bz2/tar.xz (+ .tgz/.tbz2/.txz) round trip, tar record padding, `dir/` names | ✅ | `test_archive_roundtrip_formats` |
| never overwrite, not inside sources, unique roots | ✅ | `test_create_never_overwrites_and_rejects_inside_sources` |
| cancelled create removes archive (zip, tar.gz) | ✅ | `test_cancelled_create_removes_partial_archive` |
| traversal names rejected before writes | ✅ | `test_zip_traversal_rejected_before_any_writes` |
| links/specials rejected (zip symlink, tar sym/hard/fifo, create through symlink) | ✅ | `test_archive_links_rejected` |
| existing symlinked directory not followed | ✅ | `test_existing_symlink_directory_is_not_followed` (ENOTDIR, an OSError as in Python) |
| existing file kept, new entries rolled back | ✅ | `test_existing_file_preserved_and_new_files_rolled_back` |
| cancelled extract removes only new entries (zip, tar.xz) | ✅ | `test_cancelled_extract_removes_only_new_files` |
| `.` member skipped, setuid/setgid stripped, zip without Unix bits → 0644 | ✅ | `test_tar_dot_directory_and_strips_privilege_bits` |
| duplicates / file used as folder | ✅ | `test_duplicate_members_and_file_parents_rejected` |
| plan_batch_rename numbering, suffixes, errors | ✅ | `test_batch_rename_numbering_and_no_mutation`, `test_batch_rename_collision_and_invalid_names` |
| checksums, set_permissions (0000 repair), O_PATH symlink refusal, file_metadata, set_owner | ✅ | `test_metadata_checksums_and_no_symlink_permission_changes`, `test_filemode_matches_python` |
| recursive_size (links, hard links, unreadable, cancel) | ✅ | `test_recursive_size_does_not_follow_links_or_double_count_hardlinks` |
| ACL read/write via /proc/self/fd | ✅ | `test_acl_roundtrip_and_invalid_input` (runs: getfacl/setfacl installed) |
| tar.zst extraction (Python 3.14 `r:*`) | ☑ | ruzstd decoder, not tested |
| byte-identical archives | ⚠ | contents/modes/names match; ZIP/TAR framing comes from the `zip`/`tar` crates (e.g. ZIP extra fields, PAX header naming) |
| `tarfile.ReadError` / `BadZipFile` / `UnicodeDecodeError` texts | ⚠ | reported as `ErrorKind::Value` with a simplified message (`file could not be opened successfully: …`) |
| getfacl stdin | ⚠ | `/dev/null` (Python inherits stdin) |
| `hashlib.new` OpenSSL-only names (ripemd160, sm3, shake_*) and non-ASCII decimal ids in `set_owner` | ❌ | `unsupported hash type …` / treated as names |

### file_extras.py (`extras`) — test_file_extras.py → `extras::tests` (12/12 + 2)
| Item | | Test |
|---|---|---|
| reads create nothing; remote rejected on write; file URI rules | ✅ | `test_constructor_and_reads_do_not_create_files` |
| xattr round trip, partial update, contents/mode/mtime untouched | ✅ | `test_metadata_roundtrip_partial_updates_preserve_file_and_mode` |
| symlink/fifo refused | ✅ | `test_symlink_and_fifo_are_never_followed_or_modified` |
| JSON fallback (0600, inode bound), fallback beats stale xattr and is cleared later, EACCES propagates | ✅ | `test_fallback_for_unsupported_xattrs_is_atomic_and_inode_bound`, `test_fallback_overrides_old_xattr_and_clears_after_successful_write`, `test_permission_errors_are_not_silently_fallback_writes` |
| value bounds, unknown fields (TypeError), compact xattr JSON | ✅ | `test_untrusted_metadata_values_are_bounded_and_unknown_fields_rejected`, `test_xattr_payload_is_compact_json` |
| xattrs travel with Operations copy | ✅ | `test_annotations_follow_real_file_operation_copy` |
| git status (NUL rename parsing, staged labels, parents, no index write, no fsmonitor hook, timeout/remote → {}) | ✅ | `test_changed_files_subdirectories_and_rename_nul_parsing`, `test_fsmonitor_hook_never_runs_and_errors_return_empty`, `test_git_labels` |
| custom actions config, expansion, detached launch (own session) | ✅ | `test_configuration_and_expansion_preserve_literal_shell_characters`, `test_actions_launch_detached_without_shell` |
| status map keys | ⚠ | `PathBuf` (Python `str`); labels stay untranslated English as in Python |

### file_preferences.py (`preferences`) — test_file_preferences.py → `preferences::tests` (8/8 + 1)
| Item | | Test |
|---|---|---|
| defaults, XDG rule, no files on read | ✅ | `test_read_only_defaults_create_no_files_and_use_xdg` (pure `config_home_from`; the real env is never read) |
| validation/normalization, key order | ✅ | `test_invalid_config_is_safely_normalized` |
| locked atomic merge, 0600, `.lock` file, KeyError, exact JSON bytes | ✅ | `test_atomic_update_persists_and_instances_do_not_clobber` |
| broken JSON → defaults, copies from get | ✅ | `test_invalid_json_defaults_and_defensive_get` |
| pane widths, folder options, tabs/splits, saved searches | ✅ | `test_pane_visibility_and_width_are_persistent_and_validated`, `test_folder_overrides_and_session_and_saved_searches`, `test_save_writes_explicit_edits` |
| symlinked preferences file | ✅ | `test_preferences_honor_symlink_target` |
| GTK bookmarks keep unrelated lines | ✅ | `test_gtk_bookmarks_preserve_unrelated_lines_and_labels` |
| integers beyond u64 in JSON | ⚠ | parsed as floats → default (Python clamps big ints) |

### test_file_gvfs.py → `tests/gvfs.rs` via `tests/run_gvfs.sh` (6/6)
| Item | | Test |
|---|---|---|
| archive:// mount, browse, copy nested folder out | ✅ | `test_archive_uri_mount_browse_and_copy_from_nested_folder` |
| trash → private Trash → restore exact entry | ✅ | `test_trash_and_restore_only_private_fixture` |
| undo trash without opening Trash first | ✅ | `test_trash_00_undo_rpm_without_opening_trash_first` |
| older versions at the same path never substituted | ✅ | `test_trash_undo_distinguishes_older_versions_at_same_original_path` |
| folder + symlink group undo | ✅ | `test_trash_undo_group_preserves_directory_and_symlink` |
| missing/occupied → error, nothing restored | ✅ | `test_trash_undo_refuses_missing_item_and_keeps_existing_replacement` |
| `DirectoryLoader`/`archive_uri` from file_model | ⚠ | replaced by a synchronous enumerate and a local `archive_uri` copy (tessel-files-model owns the real ones) |

## tessel-files-model (model/)
<!-- owner: tessel-files-model agent -->
`cargo test -p tessel-files-model` (TMPDIR unset): 51 unit tests + 1 private-bus integration test, all pass.
Tests use temp dirs only, `GIO_USE_VFS=local`, and an unusable `DBUS_SESSION_BUS_ADDRESS` (`test_env()`);
the bus test runs on `gio::TestDBus` (own dbus-daemon, no service dirs).

| Contract item | Status | Test |
|---|---|---|
| FileItem GObject, identity, local/remote metadata | yes | `model::tests::file_info_remote_identity_and_local_metadata` |
| hidden / symlink-to-folder | yes | `hidden_and_symlink_metadata` |
| launcher themed icon + fallbacks, Exec never run | yes | `desktop_entry_uses_themed_icon_without_executing_launcher` |
| absolute icon, malformed entries keep file icon | yes | `desktop_entry_absolute_icon_and_malformed_fallbacks` |
| bounded reads, remote/non-launcher untouched | yes | `desktop_entry_reads_are_bounded_and_non_launchers_unchanged` |
| enrichment off the main thread (dir load + search) | yes | `desktop_icon_enrichment_runs_off_main_loop_and_search_matches` |
| edited launcher replaces only its row | yes | `desktop_icon_edits_replace_only_affected_entry` |
| navigation cancels in-flight enrichment | yes | `navigation_cancels_inflight_desktop_icon_metadata` |
| natural / folders-first / descending / size / modified sort | yes | `sorting_natural_case_insensitive_and_folders_first`, `sort_by_size_and_modified`, `natural_keys_and_globs` |
| name_matches, fnmatchcase, format_size/format_modified | yes | `search_matching_and_human_units`, `natural_keys_and_globs` |
| archive_uri double escaping | yes | `archive_uri_preserves_spaces_unicode_and_remote_identity` |
| reconcile_items (identity, exact items-changed sequence, partial, duplicates) | yes | `reconciliation_*`, `partial_reconciliation_*` |
| file_item_metadata (atime excluded, usec/mode/icon/type tracked) | yes | `cached_metadata_ignores_atime_but_tracks_subseconds_icons_and_permissions` |
| DirectoryLoader batches, restart, error | yes | `directory_load_is_batched_and_complete`, `directory_restart_drops_old_batches_and_completion`, `missing_directory_reports_error` |
| SearchLoader hidden/recursive/symlink, contents, limit, cancel, root error | yes | `recursive_search_hidden_and_no_symlink_traversal`, `content_search_reads_bounded_text_and_ignores_binary`, `search_limit_and_cancel_discard_scheduled_batches`, `search_missing_root_reports_error` |
| item_count | yes | `singular_and_plural_item_counts` |
| launch_with / launch_terminal argv, env, completion, token retirement | yes | `launch::tests::*` (13 tests, FakeSpawner — nothing is launched) |
| terminal command selection + directory flags + errors | yes | `xdg_terminal_*`, `missing_configured_*`, `terminal_directory_flags_*`, `default_terminal_alternatives_*`, `terminal_failures_*` |
| helper `--terminal` / `--application` / usage codes | yes | `terminal_helper_*`, `helper_preserves_*`, `synchronous_completion_*`, `bad_or_missing_*`, `source_helper_rejects_unknown_operation` |
| FileManager1 default-only, open/select/properties, validation, NotSupported, existing owner, default change, close while pending | yes | `bus::tests::*`, `tests/bus_private.rs` |
| location_file / folder_query / completion filter + sort | yes | `completion::tests::*` |
| fuzzy_score / rank / FolderIndex / display_path / places | yes | `goto::tests::*` |
| local_disks / capacity | yes | `devices::tests::*` |
| QWERTY / latin_keyval | yes (pure; GDK glue is the lead's) | `keys::tests::keys_by_position` |
| quote/unquote/expanduser/which/shlex | yes | `util::tests::quote_unquote_expanduser_shlex` |

Gaps / differences:
- `latin_keyval(display, …)` needs GDK: the crate exposes `latin_keyval_with(keyval, keycode, to_lower, mapped)`; the lead supplies `gdk::keyval_to_lower` and `display.map_keycode`.
- Helper: the env-var token pop (`TESSEL_DOCK_LAUNCH_STARTUP_ID` removed) is only in `helper_main`; tests inject the token instead of mutating the process environment. The real `--application` launch path (DesktopAppInfo + transferred context) is not exercised by tests (Python mocks it too).
- `launch_with` takes `&gio::AppInfo`; tests use `launch_with_id` (a desktop id without a real AppInfo).
- Paths and names are carried as UTF-8 (`to_string_lossy`) in the terminal helper argv and `FolderIndex` entries; Python keeps surrogate-escaped bytes. Non-UTF-8 folder names would be mangled there.
- `FileItem` holds no extra per-row attributes (Python widgets set ad-hoc attributes on rows/list items, not on FileItem); `set_icon` is the only mutation.
- Unicode `\d` digit values in `natural_key` use the block-of-ten rule (correct for all `Nd` runs) instead of a digit table.
- `SearchLoader`'s Python catch-all (`except Exception` → `on_done(exc)`): a worker that ends without a result calls `on_done` with a Rust-only error text `The search stopped unexpectedly.` (untested).
- No Python test was ported for the `FolderIndex` limit (80 000 entries) or `INDEX_DEPTH` cut-off; both are implemented.

## tessel-files-widgets (widgets/)
<!-- owner: tessel-files-widgets agent -->
Unit: `cargo test -p tessel-files-widgets` (9, TMPDIR unset OK). UI: `widgets/tests/run_headless.sh` (25 checks in a
private headless labwc; private bus without service dirs, fails on "Activating service"; private HOME/XDG,
private desktop entries; mimeapps.list only under the private XDG_CONFIG_HOME). Last run: 9/9 + 25/25, no activation.

| Contract item | Status | Test |
|---|---|---|
| CSS / NAVIGATION_CSS byte-identical | yes | `style::tests::matches_python_source`, ui `style_css_parses` |
| preview_kind | yes | `information::tests::kinds_follow_content_types` |
| Image decode ≤1200 px, orientation, details, camera/date/mode | yes | unit `image_decoding_scales_and_reports_dimensions`, ui `large_preview_and_image_details_follow_selection_and_orientation` |
| Broken image → "Preview unavailable", multi-selection Total size | yes | ui `invalid_image_and_multi_selection_clear_old_preview` |
| Text cap on line boundary, binary refusal, Lines, GtkSourceView language | yes | unit `text_is_capped…`, ui `text_pdf_and_binary_previews`, `text_preview_is_capped_on_a_line_boundary` |
| PDF first page 1200 px, Pages / Page size (poppler via dlopen) | yes | ui `text_pdf_and_binary_previews` |
| Media never autoplays, released on change | yes | ui `media_preview_uses_a_player_and_releases_it` (ffmpeg fixture; PipeWire/Pulse pointed at nonexistent paths) |
| PreviewLoader: one running + latest pending only; stale result ignored | yes | ui `worker_keeps_only_latest_pending_request`, `pending_image_does_not_replace_new_selection` |
| Print PDF/text/image, export_to, unprintable error | yes | ui `printing_renders_pdf_text_and_images` |
| Quick Look keys, step, open, print, release on close | yes | ui `space_quick_preview_steps_opens_and_closes` (fake pane) |
| show_error / confirm / prompt_text one-shot answers, theme attached | yes | ui `prompt_close_notifies_once_and_confirm_cancel_is_false` |
| choose_file (GTK widget, no portal, no file created) | yes | ui `explicit_gtk4_file_chooser_and_application_chooser_cancel` |
| Open With: catalog + NoDisplay handlers, sort both ways, search, debounce-safe Enter/Open, row activation, explicit default only, failure retry, search icon spacing | yes | ui `search_icon_has_space…`, `all_apps_and_hidden_handlers…`, `immediate_search_activation…`, `immediate_open_button…`, `confirmed_checkbox_updates_private_mime_default…`, `cancel_does_not_apply_checked_default`, `association_failure_stays_open…`; unit `mime_handlers_become_visible…` |
| Properties: size, permissions (octal ↔ checks), checksums, symlinks, dir sizes, close cancels, tags/notes persistence | yes | ui `properties_metadata_permissions_checksums_and_symlinks`, `directory_sizes_and_close_cancels_work`, `metadata_edit_is_explicit_and_persists_in_private_fixture` |
| Properties: ownership, ACL page, "Set as default" association | implemented, not exercised (would need root / real setfacl on fixtures / a real default change) | — |
| Jobs: hold/release, notification only with no live window, button rows/tooltip/pulse, cancel, ask_keep_running | yes | ui `jobs_button_lists_cancels_and_notifies` (notifier hook) |
| DiskUsage free/total bar, no queueing, stale reply ignored | yes | ui `disk_usage_bar_shows_free_and_total`, `slow_capacity_query_does_not_queue_and_ignores_stale_reply`; unit `capacity_bounds` |
| DiskUsage keeps its size (Rust-only fix for a jumping Places pane): the bar always takes its space (transparent until known, trough pinned at 150 px) and the text is always two lines; the last capacity per location is remembered for the app's life and seeds new rows; unchanged numbers leave the text alone | yes | ui `disk_usage_keeps_its_size_when_the_reply_arrives`, `disk_usage_starts_with_the_last_known_numbers`; app ui `test_places_pane_keeps_its_layout_when_disk_space_arrives_and_on_unchanged_refreshes` |

Gaps / differences (honest):
- Pillow is replaced by kamadak-exif + a header sniffer for `Color mode`: modes for PNG/JPEG/GIF/WebP/BMP only
  (TIFF and other Pillow formats return no photo rows; Pillow's exact mode for e.g. 16-bit/LA PNG variants may differ).
- Python `self.source` fallback (no GtkSourceView → plain TextView) does not exist: sourceview5 is linked.
- `FilePreview`/`InformationPane`/`QuickLook`/`PropertiesWindow` are handles, not GTK subclasses (`widget()`/fields instead of `isinstance`).
- Open With discovery runs on a GLib worker with `AppInfo`s moved across threads (same as Python); tests inject sources
  through `OpenWithOptions` instead of `patch.object(Gio.AppInfo, …)`.
- Python `hasattr(window, …)` checks map to `Option` accessors; `window.alive` maps to `jobs::set_window_alive`.
- `compat::capacity` re-exports `files_model::devices::capacity`; `compat::ensure_search_css`
  re-adds the search-field margins Python's `headerbar()` installs through `add_css` (suite_gtk's headerbar does not).
- Media "prepared"/Duration rows are not asserted (headless GStreamer timing); print-dialog (non-export) path untested.

## tessel-files application (src/, tests/)
Tests: `cargo test -p tessel-files` (unit: CLI argparse, key positions) and `tests/run_headless.sh` (95 checks in a
private headless labwc; `tests/ui_headless.rs`). Measurements: `tests/measure_headless.sh`.

| Contract item | Status | Test |
|---|---|---|
| CLI parse/help/errors, `--check` JSON | done | `cli::tests::flags_prefixes_and_errors`, `test_dependency_check_does_not_open_a_window` |
| Unique app, secondary forwards argv, relative paths from caller cwd, `--home`, `--new-window --split` | done | `test_real_secondary_invocations_reuse_primary_and_preserve_arguments`, `test_home_shortcut_opens_home_instead_of_reusing_current_folder` |
| open_locations: split, reveal grouping, split reveal, async classification order, reveal scroll | done | `test_split_opens_one_tab_with_two_distinct_folders`, `test_reveal_*`, `test_split_reveal_keeps_independent_selections`, `test_async_classification_preserves_argument_order` |
| Session restore, tab order, Home launcher reuses restored tab | done | `test_new_tabs_open_next_to_the_active_tab` |
| Views, hidden files, selection, information pane | done | `test_folder_views_hidden_files_selection_and_information`, `test_only_displayed_layout_binds_rows` |
| Tabs/split/history/bookmark/reopen, tab close button | done | `test_tabs_split_history_and_bookmark`, `test_tab_close_button_closes_its_tab` |
| Address bar: tabs below it, alignment, per-pane address, blank click edit, Escape, editable mode, Home crumbs | done | `test_tabs_only_appear_*`, `test_address_aligns_*`, `test_tab_changes_*`, `test_split_pane_changes_*`, `test_blank_address_*`, `test_escape_discards_*`, `test_always_editable_*`, `test_home_breadcrumbs_*` |
| Filter, recursive and contents search | done | `test_filter_recursive_name_search_and_contents_search` |
| Clipboard copy/cut/paste/undo, details nested copy, entry shortcuts stay in text | done | `test_clipboard_*`, `test_details_tree_*`, `test_entry_shortcuts_*`, `test_file_context_copy_after_location_entry_focus` |
| Jobs: concurrent, shared popover, keep running + notification, cancel on close, Ctrl+Q asks once | done | `test_jobs_*`, `test_closing_window_*`, `test_ctrl_q_*` |
| New folder/file dialogs, statusbar/single-click prefs | done | `test_new_folder_dialog_accept_and_cancel`, `test_statusbar_*` |
| Context menus: folder vs file actions, blank menu clears selection, details non-name cell, breadcrumb/places/bookmark/virtual/trash menus, keyboard activation, release on navigation/anchor unmap/tab close/Escape | done | all `test_files_context.py` names |
| Places pane rebuilds only when its rows would differ (places, bookmarks, mounts, volumes, disks, saved searches; Rust-only); mount/volume signal bursts are folded into one refresh 100 ms after the first; direct `refresh_sidebar()` stays synchronous | done | `test_places_pane_keeps_its_layout_when_disk_space_arrives_and_on_unchanged_refreshes` |
| **Menus show fully without a scrollbar (window-child parent, RIGHT, offset after popup)** | done | `test_context_menus_show_every_item_without_scrolling`, `test_context_menu_on_a_short_output_shows_every_item` (Rust-only; fails with the old BOTTOM placement: verified by mutation) |
| **No selection + any arrow selects first item (all four views)** | done | `test_arrow_key_without_selection_selects_first_item` |
| **Selection after delete/trash moves to the item before (or index 0)** | done | `test_deleting_selects_the_item_before_the_deleted_items`, `test_trashing_selects_the_item_before_the_trashed_item` (private XDG_DATA_HOME, same-filesystem check) |
| Quick Preview space/steps, Go Anywhere, Miller columns, folder peek, Vim keys + layout shortcuts, widened Name column, text/binary preview | done | `test_files_browsing.py` names |
| Live monitor: busy log, create/rename/delete, hidden writes, details expansion, navigation drops queue, desktop icons | done | `test_files_monitor.py` names (navigate/refresh counters replace `patch.object(wraps=)`) |
| Header sorting root + children, per-folder sort sync | done | `test_header_sorts_*`, `test_folder_header_*` |
| Virtualization / release of closed tabs, panes, rows; hidden tab state; reveal on return; split release | done | `test_files_memory.py` names (Rc/WeakRef instead of gc) |
| Thumbnails: all views + info, large batch, broken cache fallback, orientation, cancel backlog, corrupt image | done | `test_files_thumbnails.py` names (Thumbnails::request_key/set_decoder replace attribute patching) |
| Location completion | done | `test_location_completion.py` names |
| Preview width/pane visibility persistence, title for folder/multi-selection | done | `test_both_panes_and_preview_width_restore_after_reopening`, `test_selection_title_follows_folder_and_multi_selection` |
| Drops: COPY-only external, choice menu, modifiers, internal kinds, rubberband suspension, Ark negotiation + D-Bus request | done | `test_external_sources_*`, `test_external_drop_menu_*`, `test_item_press_*`, `test_ark_drop_*` |
| Git labels + throttled refresh | done | `test_git_status_labels_appear_and_refresh_is_throttled` (real git in a temp repo; test_git_refresh.py's mocked throttling internals not ported 1:1) |
| **Ark-style archive service menus** (Rust-only, `src/service_menu.rs`): with `tessel-archive` on PATH (looked up once, cached) the context menu shows an "Extract" submenu (every selected item a local archive: Ark's mime set, exact/alias match, name guess for unknown types) — here `--batch --destination <folder>`, to… `--batch --dialog`, autodetect subfolder `--batch --autosubfolder --destination <folder>` — and a "Compress" submenu (every item local) — `--add --changetofirstpath --autofilename tar.gz\|zip`, to… `--add --changetofirstpath --dialog`; local paths in view order; hidden for non-local selections; "here" items disabled in read-only folders. Runs through the `systemd-run --user` launcher (`--launch-helper --program <cwd> <argv>`); the folder monitor shows the result. Without `tessel-archive` the internal Compress…/Extract… stay unchanged | done | `service_menu::tests::*`, `launch::tests::program_*`, `test_archive_service_menus_follow_the_selection`, `test_without_tessel-archive_the_internal_archive_items_remain`, `test_archive_service_actions_run_tessel-archive_and_the_folder_updates` (fake `$R/bin/tessel-archive`, real helper), `test_archive_service_submenus_screenshot` |

Gaps (app crate):
- `test_open_files_window_follows_live_transparency_and_blur` and `test_preview_screenshot` (live palette/blur render, grim
  screenshot) not ported; theme attach is checked only for presence. `test_file_icons.py` (Papirus resolution) not ported.
- `test_file_dnd.py` cases that emit `drop` on a real `Gtk.DropTarget` with a faked `get_current_drop` are tested through
  `BrowserPane::drop_files` (the handler body) instead; GTK drag protocol itself untested headless.
- `test_install.py` (installer planning) not ported: no Rust installer entry yet (see swap notes).
- Real terminal/app launches, mounting, unmounting, Connect to Server, Empty Trash execution, archive dialogs, templates
  and saved searches are wired but only exercised through hooks/fakes or not at all.
- `media_chooser.py` is not part of this binary (Tessel Shell's Rust wallpaper chooser has its own port).
- GTK's `GtkGizmo (progress) reported min width -2` for the DiskUsage bars (the theme's -1 px progress margin) is gone:
  `.files-disk-usage progress` has `margin: 0`.

## Swap notes (not installed; do this with the user present)
Python today: `desktop-apps/install.py --apply` (root, pkexec) copies `tessel-files/*.py` + shared/tessel-dock modules to
`/usr/local/lib/tessel/tessel-files/` and writes the marker launcher `/usr/local/bin/tessel-files`
(`#!/bin/sh` / `# Tessel standalone application` / `exec /usr/bin/python3 ".../file_manager.py" "$@"`) and
`/usr/share/applications/org.tessel.Files.desktop` (`Exec=/usr/local/bin/tessel-files %U`).
1. Build `cargo build --release -p tessel-files`; install `target/release/tessel-files` (≈4.7 MB, 0755) as
   `/usr/local/lib/tessel/tessel-files/tessel-files` (a stable path: the launch helper re-executes `current_exe()` from
   inside `systemd-run --user` services, so the binary must not move while apps are being started).
2. Keep the marker launcher but point it at the binary: `exec /usr/local/lib/tessel/tessel-files/tessel-files "$@"`.
   desktop-apps/install.py regenerates this launcher, so its `APPS['tessel-files']` entry must be switched in the same step
   (otherwise the next `--apply` reverts to Python). The .desktop file is unchanged (same Exec, actions `--new-window`/`--home`).
3. Ship translations: `shared/locale/*.json` to `/usr/local/lib/tessel/tessel-files/locale/` (catalog dir = `<exe dir>/locale`).
4. Leave the Python files in place: rollback = restore the Python launcher line; `shared/wallpaper_chooser.py` and the
   Tessel Shell/gtk4 installers copy `media_chooser.py`, `files_pane.py` … from the repo, not from this directory.
5. Runtime deps: GTK 4.22, gtk4-layer-shell (link), GtkSourceView 5, `libpoppler-glib.so.8` (dlopen, optional), gvfs for
   Trash/Network/remote, `git`/`getfacl`/`setfacl` optional. Preferences/bookmarks/metadata files are format-compatible;
   no migration. FileManager1 is still only claimed while Files is the default `inode/directory` handler.
6. Verify after swap: `tessel-files --check`, open Home, right-click a file near the screen middle (menu unscrolled), Ctrl+Q.
