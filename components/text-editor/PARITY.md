# text-editor — parity with contracts/text-editor.md

Crate `components/text-editor`, binary `text-editor` (= the `/usr/local/bin/text-editor` command), GApplication
`org.waydock.TextEditor` (HANDLES_OPEN). Tests: `env -u TMPDIR cargo test -p text-editor` (31 unit tests; the UI
test skips itself), `tests/run_headless.sh` (21 GTK checks in a private nested headless labwc + private D-Bus +
private XDG dirs), `tests/measure_headless.sh` (Rust vs Python, same small file, 2 runs each).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## GtkSourceView linking
Only the runtime library is installed (`/usr/lib64/libgtksourceview-5.so.0`, 5.20; no `gtksourceview-5.pc`, no
unversioned `.so`). The crate uses the `sourceview5` 0.11 bindings (feature `v5_18`, matches gtk4 0.11). The
workspace file `/home/tcrider/nobara-de-rust/.cargo/config.toml` `[env]` sets, for sourceview5-sys's system-deps
probe only: `SYSTEM_DEPS_GTKSOURCEVIEW_5_NO_PKG_CONFIG=1`,
`SYSTEM_DEPS_GTKSOURCEVIEW_5_LIB=dylib:+verbatim=libgtksourceview-5.so.0` (the linker gets
`-l:libgtksourceview-5.so.0`) and `SYSTEM_DEPS_GTKSOURCEVIEW_5_SEARCH_NATIVE=/usr/lib64`. A plain `cargo build`
works; no other crate reads these variables; it keeps working if the -devel package is installed later. Chosen over a
dlopen FFI because the editor uses ~15 GtkSource classes (View, Buffer, SearchContext, CompletionWords, VimIMContext,
PrintCompositor, StyleSchemeManager …). Two binding bugs are worked around: `SearchContext::replace_all` (drops the
count and asserts on success → `compat::replace_all` calls the C function) and `VimIMContext::command_text()` /
`command_bar_text()` (panic on NULL → read the properties).

## §1 Entry points / CLI
| Item | | Test |
|---|---|---|
| App id `org.waydock.TextEditor`, HANDLES_OPEN, activate → new scratch window, open → focus existing / new window / missing path named | ✅ | `test_multiple_cli_files_get_separate_windows_and_reuse_existing`, measure_headless (app_id mapped) |
| `[-h] [--check] [files ...]`, prefixes, `--`, negative numbers, unknown → exit 2 | ✅ | `cli::tests::argparse_rules` |
| `--check` JSON `{"ready": true, "gtk": 4, "gtksourceview": 5, "tabs": false}` | ⚠ | `argparse_rules` — always ready: a missing libgtksourceview makes the dynamic loader fail (exit 127) before `main`, so the `{"ready": false, …}` form cannot occur |
| Help/usage text | ⚠ | prog is the binary name (`text-editor`), Python prints `text_editor.py`; no colour on TTYs |
| Installer / launcher / .desktop | ❌ | installer step (desktop-apps/install.py), not in the crate |

## §2 Files
| Item | | Test |
|---|---|---|
| `desktop-suite/text-editor.json` load rules (type identity, clamp, 12 recent) + `indent=2` bytes + tmp/0600/rename | ✅ | `preferences::tests::*`, `test_view_toggles_persist_preferences` |
| Recent list update + Open Recent menu in every window | ✅ | `test_view_toggles_persist_preferences` (recent array), menu walk in `test_primary_menu_holds_former_menubar_entries` |
| Recovery draft path, id validation, JSON bytes, 0700 dir / 0600 file, tmp+fsync+rename, remove | ✅ | `recovery::tests::write_pending_remove_round_trip`, `test_drafts_are_written_after_edits_and_removed_on_save_or_discard` |
| `pending()` (version==1, text str, pid liveness by cmdline, field defaults, `float(saved or 0)`, newest first) | ✅ | `write_pending_remove_round_trip`, `test_recovered_drafts_are_offered_and_restored` |
| New local file via mkstemp + link (0600), temp removed | ✅ | `test_new_local_file_is_private_like_mkstemp`, `test_new_file_creation_race_never_replaces_other_writer` |
| Set as default (`mimeapps.list`) | ✅ | `test_gio_file_open_and_set_default_text_editor` |
| palette.json via companion theme; `is_dark` → scheme | ✅ | `test_document_surface_keeps_neutral_colors_inside_wallpaper_theme` |

## §3 Document semantics (test_document.py → `document_tests.rs`, all 15 ported)
| Item | | Test |
|---|---|---|
| BOM + CRLF kept after edit (utf-8/16le/16be/32le) | ✅ | `test_bom_and_crlf_preserved_after_edit` |
| Unedited mixed endings byte-identical | ✅ | `test_no_edit_preserves_mixed_endings_byte_for_byte` |
| Strict decode, explicit encoding, `cp1252` name | ✅ | `test_non_utf8_is_not_silently_replaced` |
| Unrepresentable → error, file untouched | ✅ | `test_unrepresentable_encoding_does_not_touch_file` |
| Digest conflict with restored mtime; deleted file conflict; symlink kept / retarget conflict | ✅ | `test_conflict_with_modified_timestamp_restored`, `test_deleted_file_is_not_recreated_over_external_change`, `test_symlink_is_preserved_and_retarget_is_conflict` |
| Save As needs overwrite; mode kept; no final newline added | ✅ | `test_existing_save_as_requires_overwrite`, `test_file_mode_and_missing_final_newline_preserved` |
| Binary refused; newline conversion; utf-16 BOM added and re-detected | ✅ | `test_binary_input_refused`, `test_new_file_and_line_ending_conversion`, `test_conversion_to_utf16_is_detectable_on_next_open` |
| New command-line name never overwrites a later file; link race | ✅ | `test_new_command_line_path_cannot_overwrite_file_created_later`, `test_new_file_creation_race_never_replaces_other_writer` |
| GIO load/save/etag conflict, exclusive create, raced create, deleted remote, URI resolution | ✅ | `test_gio_location_load_save_and_etag_conflict`, `test_gio_new_file_is_created_exclusively_and_save_as_needs_overwrite`, `test_gio_deleted_file_is_conflict_and_uris_resolve` |
| decode ending ties, BOM choice, lookup errors, encode newline errors | ✅ | `test_decode_endings_and_bom_choice` |
| Codec names / Python error texts (`'latin-1' codec can't encode character '\U0001f5a5' in position 0: …`, `'charmap' …`, runs `position 1-2`) | ✅ | `codec::tests::*` (checked against CPython 3.14) |
| UTF-8/16/32, ISO-8859-1, cp1252 (undefined 0x81 0x8D 0x8F 0x90 0x9D) exact | ✅ | `codec::tests::single_byte_tables`, `utf16_and_utf32_are_strict` |
| koi8-r, shift_jis, gb18030, big5 | ⚠ | `encoding_rs` WHATWG tables without replacement: Shift_JIS ≈ Windows-31J and Big5 ≈ Big5-HKSCS accept some byte pairs CPython's stricter codecs reject (and vice versa); decode error texts for these codecs are approximations (never shown: a decode error opens the encoding dialog) |

## §4 UI (test_editor_ui.py → `tests/ui_headless.rs`, all 16 ported + 5 Rust-only)
| Item | | Test |
|---|---|---|
| App menu in the header, app.shortcuts/about/quit, Ctrl+Q, 2 new-window accels | ✅ | `test_standard_app_menu_quit_and_new_window_shortcuts` |
| Menu tree (Edit/View/Bookmarks/Open Recent, actions), Save not suggested-action, Ctrl+F, line_count | ✅ | `test_primary_menu_holds_former_menubar_entries` |
| Load (language guess python3, CRLF), single SourceView, no notebook, undo/redo/save, title/status/subtitle texts | ✅ | `test_load_edit_save_and_single_document_window` |
| Guard Cancel/Discard/Save, failed save keeps edits + banner (`[Errno 2] …`), "● Untitled" / "*Untitled — Text Editor" | ✅ | `test_unsaved_cancel_discard_and_failed_save_guard` |
| Save conflict keeps both versions | ✅ | `test_external_conflict_preserves_both_versions` |
| Directory monitor → 300 ms → banner + Reload → reloaded | ✅ | `test_external_change_is_reported_by_the_monitor` (Rust-only) |
| Search count/case/whole word/regex, replace all + count text, undo, regex error, panel show/hide | ✅ | `test_search_case_whole_word_regex_replace_and_undo` |
| Bookmarks next/previous/toggle off, Preferences dialog, zoom font string, Go to Line, PDF export via PrintCompositor | ✅ | `test_bookmarks_preferences_and_print_export`, `test_preferences_alone` |
| Vim `:w`, `:wq` guarded by conflict, unknown command, vim label | ✅ | `test_vim_write_uses_guarded_save` |
| Plain modal lifecycle | ✅ | `test_plain_modal_lifecycle` |
| Document surface keeps scheme colours (view/gutter bg + fg), window/popover alpha & colours follow palette, 6 palette cases | ✅ | `test_document_surface_keeps_neutral_colors_inside_wallpaper_theme` |
| Scheme follows GTK light/dark without a palette | ✅ | `test_document_scheme_follows_gtk_light_dark_without_a_dock_palette` |
| `win.new` opens a window, never replaces | ✅ | `test_new_opens_a_window_and_never_replaces_the_document` |
| Drafts written/removed (save, discard); destroy without guard keeps the draft | ✅ | `test_drafts_are_written_after_edits_and_removed_on_save_or_discard`, `test_destroy_without_guard_keeps_the_draft` (Rust-only) |
| Recovery bar text, Restore into windows (reuse untitled), draft id kept, save removes, pending empty | ✅ | `test_recovered_drafts_are_offered_and_restored` |
| GIO file open; Set as default + message | ✅ | `test_gio_file_open_and_set_default_text_editor` |
| Non-UTF-8 → "Choose File Encoding" → cp1252, status `CP1252`, format change marks modified, converted save | ✅ | `test_non_utf8_asks_for_an_encoding` (Rust-only) |
| View toggles persist preferences | ✅ | `test_view_toggles_persist_preferences` (Rust-only) |
| CSS classes / CSS text verbatim, per-view font rule (`11.0pt`, int bounds `6pt`/`36pt`) | ✅ | `style::tests::font_rules_follow_python` |
| Remote focus re-check (≥5 s), drag-and-drop → `Application.open`, Open/Save file dialogs, interactive print dialog, Preferences widgets' handlers, encoding/line-ending dropdowns, completion, Escape key | ☑ | no input devices in the headless session; handlers port the Python code 1:1 |

## Deviations ⚠
1. Destroy handling runs from GtkApplication `window-removed` (always emitted by `gtk_window_destroy`) as well as
   `destroy`: GTK 4 emits `destroy` only at dispose, which the window's own Rust references delay.
2. In-flight external checks are discarded by a document serial number (Python compares object identity + etag);
   a save produces a new document value instead of mutating it in place on the worker.
3. CJK/KOI8-R codec tables (above); `--check` / help prog name (above).
4. `test_editor_ui.py --screenshot` (demo screenshot mode) is not ported.

## Gaps ❌
- Installer/swap (desktop-apps/install.py) is not part of the crate.
- Live GVfs (sftp/smb) is not exercised; the GIO code path is tested with local `Gio.File`s exactly as the Python tests do.

## compat (upstream candidates)
`compat::expanduser` (`os.path.expanduser`), `compat::resolve` (`Path.resolve()` non-strict), `compat::named_temporary`
(`NamedTemporaryFile(delete=False)`), `compat::uuid4_hex`, `compat::attach_application(app, &PaletteClient)` (suite-gtk
keeps its shared palette client private; the editor needs `palette.is_dark`), `compat::replace_all` (sourceview5
binding fix), `worker::Worker` (`ThreadPoolExecutor(max_workers=1)` + futures oneshot), `codec` (Python codec names /
strict errors).
