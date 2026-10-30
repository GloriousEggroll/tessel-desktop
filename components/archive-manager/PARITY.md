# Archive Manager (`wayarchive`, `org.waydock.ArchiveManager`) — Ark parity

New component (no Python twin): replaces KDE's Ark (24.x/25.x feature set). Tests:
`cargo test -p archive-manager` (unit: `src/**`, engine: `tests/engine.rs` against the real
tools and libarchive), `cargo test -p archive-settings`, and the GUI suite
`components/archive-manager/tests/run_headless.sh` (private nested headless labwc + private
dbus-daemon without service dirs; fails on any D-Bus activation). Test names below are
`unit::<module>::<test>`, `engine::<test>`, `ui::<test>`, `gs::<test>`
(components/global-settings/applets/tests/applets_headless.rs), `install::<test>` (tools/test_install.py).

## Command line (contract shared with Files' context menus)

`wayarchive [options] [FILES/URLS...]` — same long/short options as Ark. Exit code 0 on success,
1 on failure or cancel, 2 on bad usage.

| Option | Behaviour | Test |
|---|---|---|
| `FILES...` | open each archive in a window (unique app: later calls open windows in the running instance) | ui::test_main_window_tree_columns_and_info_panel |
| `-b, --batch` | extract each archive with a progress window (own process, non-unique), then exit; password prompt and conflict dialog when needed | ui::test_command_line_batch_and_add_modes |
| `-a, --autosubfolder` | with --batch: into a subfolder named after the archive unless it has a single top-level folder | ui::test_command_line_batch_and_add_modes |
| `-o, --destination <dir>` | extraction folder (default: current folder; `-e/--autodestination`: next to the archive) | ui::test_command_line_batch_and_add_modes |
| `-c, --add` | add the files to an archive; asks for name/type with the create dialog unless --add-to/--autofilename | unit::cli::tests::ark_options |
| `-t, --add-to <archive>` | add to that archive (created when missing; format from the extension) | ui::test_command_line_batch_and_add_modes |
| `-f, --autofilename <suffix>` | archive name from the first file/folder (`photos` → `photos.tar.gz`, then `photos_1.tar.gz` like Ark), next to it; implies --add | ui::test_command_line_batch_and_add_modes, unit::cli::tests::auto_names_and_relative_paths |
| `-p, --changetofirstpath` | archive paths relative to the first file's folder | ui::test_command_line_batch_and_add_modes |
| `-d, --dialog` | show the extract dialog for the archive first | (code path shared with the window's dialog: ui::test_extract_dialog_selected_entries_into_a_subfolder) |
| `-O, --opendestination` | open the destination in the file manager when done (FileManager1.ShowFolders, else the default handler) | ui hook (open_folder) |
| `-m, --mimetypes`, `--check`, `-h`, `-v` | list mime types / plugin JSON / help / version | unit::cli::tests::errors_and_help |

Missing input files in job modes are reported on stderr with exit code 1 (no dialog); every other
job failure shows an error dialog and exits 1.

## Formats and plugins (Ark's kerfuffle plugins; settings can disable each)

| Plugin | Formats | Access | Test |
|---|---|---|---|
| libarchive (hand-written FFI, `-l:libarchive.so.13`) | tar, tar.gz/bz2/xz/lzma/zst/lz/lzo/lz4/Z | read-write (rewrite + atomic replace) | engine::libarchive_tar_family |
| libarchive | tar.lrz (needs lrzip) ; iso, cpio, deb, rpm, ar, xar, cab, lha; unencrypted zip/7z/rar (fallback) | read-only | engine::read_only_images_and_packages, engine::rar_read_only_with_unar_and_libarchive, engine::plugins_can_be_disabled |
| libsinglefile (libarchive raw) | .gz .bz2 .xz .lzma .zst .lz .lzo .lz4 .Z (create one file; read); .lrz read | create + read | engine::single_file_compressors |
| cli7z (`7z`) | 7z, zip: add/delete/rename/move, passwords (AES-256 / ZipCrypto), 7z header encryption, split volumes; reads iso, cab, cpio, deb, ar, rpm, xar, lha, arj, wim, squashfs, AppImage | read-write / read-only | engine::sevenzip_formats_and_passwords, engine::header_encryption_needs_password_and_rejects_wrong_one, engine::split_volumes_open_read_only |
| clizip (`zip`, `unzip`, `zipinfo`, `zipnote`) | zip incl. ZipCrypto passwords, renames via zipnote | read-write | engine::infozip |
| cliunarchiver (`unar`, `lsar`) | rar/rar5 (encrypted, header-encrypted, multi-volume), arj, lha | read-only | engine::rar_read_only_with_unar_and_libarchive |

Priorities (highest enabled + available wins; writers chosen the same way): 7z/zip cli7z 180 >
clizip 160 > libarchive 50; rar cliunarchiver 120 > cli7z 90 > libarchive 50; tar libarchive 100;
images libarchive 100 > cli7z 80 (unit::plugins::tests::priorities_follow_ark). Format detection:
shared-mime-info (name + content) then magic numbers, split names (`x.7z.001`, `x.part1.rar`)
(unit::ops::tests::magic_numbers, unit::backend::tests::volume_discovery). Read-only formats,
multi-volume archives and unwritable files disable every editing action
(ui::test_read_only_formats_disable_editing).

## Main window

| Feature | Test |
|---|---|
| Entry tree with folders, mime icons, sortable columns Name, Size, Compressed, Ratio, Type, Modified, Permissions, Owner, Group, CRC, Method, Version (shown when the plugin supplies them), folders first in both directions | ui::test_main_window_tree_columns_and_info_panel, unit::model::tests::tree_and_filter |
| Information panel (icon, name, type, size, compressed, modified, owner/group, permissions, link; archive summary + comment), toggle F9, default from settings | ui::test_main_window_tree_columns_and_info_panel, ui::test_settings_dialog_and_external_changes_apply_live |
| Filter bar (Ctrl+F): matches plus their folders, expanded | ui::test_filter_bar_shows_matches_with_their_folders |
| Status bar: counts, size, selection, read-only reason; job progress + Cancel | ui::test_main_window_tree_columns_and_info_panel |
| Open, Open Recent (12, in the settings file), New archive | ui::test_create_dialog_new_archive_with_password_and_header_encryption |
| Create dialog: folder, file name (extension follows type), type, level, method, password + confirm, encrypt file list (7z), AES/ZipCrypto (zip), split volumes, comment (zip) | ui::test_create_dialog_new_archive_with_password_and_header_encryption |
| Add Files / Add Folder / New Folder into the selected folder, replace prompt for existing names | ui::test_edit_actions_rename_folder_delete_cut_paste_copy |
| Drop files on the window: add into the folder under the pointer; archive on an empty window opens; files on an empty window start a new archive | ui::test_drops_on_an_empty_window_open_or_create |
| Extract dialog: destination, subfolder (auto name, on when several top-level entries), all/selected, preserve paths, open destination, close after extraction (choices saved like Ark) | ui::test_extract_dialog_selected_entries_into_a_subfolder |
| Extract Here | ui::test_extract_dialog_selected_entries_into_a_subfolder |
| Delete (confirm), Rename, Cut/Copy/Paste (move/copy inside the archive), internal drag = move | ui::test_edit_actions_rename_folder_delete_cut_paste_copy, engine::libarchive_tar_family |
| Preview: internal text/picture viewer, size limit, external app otherwise | ui::test_internal_preview_of_text_and_pictures |
| Open / Open With (extract to a private folder below $XDG_RUNTIME_DIR, launch); saved edits → "update the archive?" | ui::test_open_with_external_app_and_update_after_saving |
| Test integrity | ui::test_properties_test_and_comment, engine::corrupt_archive_fails_test |
| Properties: name, location, type, MIME, owner, sizes, ratio, entry/file/folder counts, modified, password, encrypted list, comment, volumes, plugin, MD5/SHA-1/SHA-256 (background) | ui::test_properties_test_and_comment, engine::checksums_and_comment |
| Edit comment (zip, written in the EOCD record) | ui::test_properties_test_and_comment, unit::backend::zipcomment::tests::round_trip |
| Password prompts with wrong-password retry; known password reused | ui::test_password_prompt_retries_after_a_wrong_password |
| Overwrite conflicts: Overwrite / Skip / Rename (free name suggested) / Overwrite All / Skip All / Cancel; extraction is staged in a hidden folder on the destination's filesystem and merged | ui::test_conflict_dialog_rename_and_overwrite_all, engine::conflicts_overwrite_skip_rename_all_cancel |
| Every long job on a worker thread with progress + cancel (the GTK thread never blocks) | engine::cancel_stops_a_job, unit::backend::process::tests::run_collects_output_and_cancels |
| Archive paths that escape the destination (`../`, symlinked parents) are refused | unit::backend::tests::disk_writer_refuses_links_out_of_root, unit::archive::tests::clean_paths |
| Undo | not required (Ark has none) |

## Drag and drop with Files

| Item | Test |
|---|---|
| Drag offers `application/x-kde-ark-dndextract-service` (unique bus name) + `-path` (`/DndExtract/<n>`) and lazily produced `text/uri-list` | ui::test_ark_dnd_protocol_with_files_client |
| `org.kde.ark.DndExtract.extractSelectedFilesTo(s)` answers at once, then extracts the dragged entries (without their parent folder, like Ark) with progress and the conflict dialog; path or file:// URI accepted | ui::test_ark_dnd_protocol_with_files_client (drives Files' own `wayfiles::ark_drop::ExtractionDrop`) |
| uri-list fallback stages only when a target asks, below $XDG_RUNTIME_DIR/wayarchive | ui::test_ark_dnd_protocol_with_files_client |
| Real cross-process drag (virtual pointer) from Archive Manager into a running `wayfiles` | ui::test_real_drag_from_the_archive_into_files — Files took the Ark formats (1 D-Bus call, 0 uri-list requests) |
| Drag actions: COPY only (other apps never delete); drops on itself move entries | code |

## Settings (Ark's Configure dialog) and Global Settings

`~/.config/desktop-suite/archive-manager.json` (atomic merge-writes, unknown keys kept, reloaded
on change by every window and page). Keys: `show_info_panel`, `open_destination`,
`close_after_extraction`, `preserve_paths`, `extract_subfolder`, `disabled_plugins`,
`internal_preview`, `limit_preview_size`, `preview_max_mb`, `recent`.

| Item | Test |
|---|---|
| General / Extraction / Plugins (formats, read-only vs read-write, availability) / Previews pages | ui::test_settings_dialog_and_external_changes_apply_live, ui::test_global_settings_page_edits_the_same_file |
| Store semantics | unit (archive-settings)::tests::store_merges_and_keeps_unknown_keys, lenient_parsing_and_round_trip |
| Global Settings "Archive Manager" section embedding the same page + search entries | gs::archive_page_edits_the_archive_manager_settings_file, global-settings shell every_real_section_builds_in_one_window |

## i18n, RTL

All strings through `tr!`; 252 new strings × 30 languages in `i18n/new-strings.json` (to merge
into ~/btmanager/shared/locale). Logical layouts only (GtkBox/Grid/Paned flip in RTL; info panel
border via `:dir()`; context menu aligned to the text direction).

## Install

`tools/install.py --system` entry `archive-manager` (fresh install, rollback removes it:
install::test_archive_manager_is_a_fresh_install_and_rolls_back_to_nothing) and the user-scope
`archive-manager-mime` default-handler entry (install::test_archive_mime_defaults_only_when_named_and_revert_only_our_keys).
Packaged default: `packaging/defaults/usr/share/applications/labwc-mimeapps.list`.

## Measurements (release, private headless labwc, GSK_RENDERER=cairo as Files' measurement)

Binary 3.2 MB (stripped). Time from exec to the toplevel being listed with a 7z archive open:
≈ 45 ms warm (≈ 1.1 s first cold run); idle PSS ≈ 23 MB, RSS ≈ 57 MB.

## Gaps (honest)

- RAR creation: needs `rar` (not installed); RAR is read-only. RARLAB `unrar` CLI backend not
  implemented (this system's `unrar` is unrar-free, which wraps libarchive); RAR goes through unar,
  7-Zip (when built with the RAR codec, not on Fedora) or libarchive.
- RAR comments come from lsar's `XADComment`; not tested with a real commented RAR (no `rar` to
  make one). Multi-volume RAR and RAR5/encrypted RAR are handled by unar but untested here
  (fixtures need `rar`); stored RAR 4 is tested with a generated fixture.
- lrzip tar and `.lrz` read need the `lrzip` program (absent): listed read-only, fail with the tool's error.
- AppImage/SquashFS go through 7-Zip; SquashFS is tested, AppImage (ELF + appended squashfs) is not.
- XAR/CAB/LHA/ARJ/WIM: read paths implemented, no fixtures (no tools to create them here).
- Split zip volumes are created by 7-Zip as `x.zip.001…` (Ark's libzip makes the same kind);
  `x.z01` sets are discovered but untested.
- 7-Zip/Info-ZIP follow symlinks when files are added into a sub-folder of an archive (staged
  through links); top-level adds keep symlinks (zip -y). libarchive (tar) always stores links.
- Passwords reach 7z/unzip/unar as command-line arguments (as Ark does), visible to the same user in /proc.
- `--dialog` and `--opendestination` are not driven by a GUI test (they would start a real file
  manager in the nested session); their code paths are shared with tested window actions.
- Open With uses GTK's (deprecated) AppChooserDialog.
