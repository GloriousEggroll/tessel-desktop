# picture-viewer — parity with contracts/picture-viewer.md

Crate `components/picture-viewer`, binary `waypictures` (GApplication `org.waydock.Pictures`).
Tests: `env -u TMPDIR cargo test -p picture-viewer` (15 unit tests), `tests/run_headless.sh` (10 GTK checks: all 9 Python
`PicturesUITests` + a second-process command-line check; private nested headless labwc, private D-Bus, private
XDG dirs and Pictures folder), `tests/measure_headless.sh` (Rust vs Python with the same photo open; the screenshots
differ only in 242 thumbnail-strip pixels by one level — Lanczos rounding; the window, canvas and sidebar are identical).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1/§2 Entry points and CLI
| Item | | Test |
|---|---|---|
| App id, prgname, unique instance forwarding argv, activate/unminimize/present | ✅ | `command_line_opens_files_in_the_existing_window` (real `waypictures` as 2nd process) |
| files / `-f` / `-s` / `--check` JSON / argparse prefixes, `-fs`, `--`, errors | ✅ | `cli_matches_argparse` |
| Non-local argument → message + exit 1 | ☑ | |
| Installer / launcher / .desktop (copy shipped in `data/`) | ❌ installer step; .desktop ✅ `desktop_file_lists_every_played_video_type` |

## §3 Files
| Item | | Test |
|---|---|---|
| settings.json path/format (`indent=2`, tmp + replace, unknown keys kept) | ✅ | `fullscreen_shortcuts_thumbnail_size_and_context_menu` (re-reads the file) |
| Undo snapshots (PNG level 1, 20 states / 512 MiB, ≥2, temp dir removed) | ✅ | `history_bounded_and_new_edit_discards_redo`, `edit_history_and_original_untouched_until_save` |
| Atomic save, signature checks, mode copy, link / RENAME_NOREPLACE fallback, temp cleanup, error texts | ✅ | `external_changes_and_existing_destinations_are_not_overwritten`, `failed_save_leaves_original_and_history_intact`, `new_save_on_filesystem_without_hardlinks` (+ raced destination) |
| EXIF (orientation 1, description, camera kept) + sRGB ICC on save | ✅ | `metadata_orientation_and_atomic_format_conversion`, `edit_history_…` (ICC present), `exif_data::tests` |
| Folder scan (extensions, hidden, natural/date/size order, folders first) | ✅ | `animation_not_silently_overwritten_and_folder_filtering` |
| Folder monitor (ignore own temporaries, debounce, external-change message) | ✅ | `file_monitor_distinguishes_own_save_from_external_change` |
| Copy/Move with conflict choices, Keep Both naming, results dialog, summaries | ✅ | `copy_conflicts_offer_replace_keep_both_and_skip`, `unique_target_and_transfer_outcomes` |
| Trash + restore, Open With, Open Containing Folder, Print, Set as Default Viewer | ☑ (not exercised by the Python tests either; never run against the real session) | |

## §4 Image model
| Item | | Test |
|---|---|---|
| Decode + EXIF orientation + ICC→sRGB (LittleCMS via dlopen) + 80 MP cap + pixbuf fallback | ✅ (fallback ☑) | `metadata_orientation_…`, `unknown_files_report_the_size_error` |
| Frames (GIF/APNG/WebP/TIFF count), animation playback, still-frame export | ✅ | `animation_not_silently_overwritten_…`, `animation_navigation_does_not_leave_a_running_timer` |
| Rotate/mirror/flip, adjust, grayscale, auto contrast, red-eye, alpha composite | ✅ **pixel-exact vs Pillow 12.3** | `pixel_operations_match_pillow`, `red_eye_annotations_and_adjustments_are_undoable` |
| Crop/resize/annotations/description, undo/redo, dirty | ✅ | `edit_history_…`, `annotation_tools_draw_inside_the_picture`, `crop_button_and_annotation_switching` |

## §5 UI
| Item | | Test |
|---|---|---|
| Window/header/menu/actions, icon buttons all labelled | ✅ | `header_menu_has_standard_entries_and_default_viewer_action` |
| Browse/search/grid activate/thumbnails/strip, edit + save + theme | ✅ | `browse_view_edit_save_and_theme` |
| Unsaved-changes guard (Cancel/Discard), Compare dialog | ✅ | `unsaved_navigation_and_comparison` |
| Tools panel, crop toggle ↔ tool drop-down, Shift+C, Escape | ✅ | `crop_button_and_annotation_switching` |
| Fullscreen hide/reveal, Ctrl+Shift+Z, context menu, thumbnail size | ✅ | `fullscreen_shortcuts_thumbnail_size_and_context_menu` |
| Loupe-style keys (navigate, pan when zoomed, Alt+arrows, zoom keys, typing fields) | ✅ | `viewing_keys_navigate_and_zoom_like_loupe` |
| Slideshow (interval, shuffle, loop, idle inhibit), drag-and-drop, video page | ☑ | |
| CSS verbatim, layout | ✅ | measure_headless screenshots |

## Deviations ⚠
1. Codecs are the `image` crate's (PNG, JPEG, GIF, WebP, TIFF, BMP, ICO, PNM, TGA, QOI) + GdkPixbuf/glycin for SVG, AVIF, HEIF, JXL…;
   Pillow-only formats (PSD, PCX, SGI, EPS, …) still appear in the browser but report a load error. Pages > 1 of a multi-page TIFF
   are counted but cannot be shown. CMYK JPEGs are converted without their CMYK profile.
2. WebP is saved **lossless** (the `image` crate has no lossy WebP encoder; the quality setting applies to JPEG only). TIFF saves carry
   the ICC profile but no EXIF. GIF palettes come from NeuQuant, not Pillow's quantizer.
3. Lanczos resize/thumbnails and annotation rasterisation (cairo masks: aliased shapes replace pixels, antialiased text blends,
   DejaVu Sans through cairo's toy text API) are close to, not bit-identical with, Pillow. JPEG thumbnails decode at full size (no draft mode).
4. EXIF is rewritten with kamadak-exif: PixelX/YDimension go to the Exif IFD (Pillow puts them in IFD0); if a stored field cannot be
   re-encoded only the essential tags are kept. PNG "Raw profile type exif" text chunks are not read.
5. A failure writing settings.json is shown in the status bar (Python raised inside the GTK callback).
6. The document lives behind `Arc<Mutex<…>>`; the UI reads a cached summary (`DocInfo`) so it never blocks on a worker.

## Gaps ❌
- Installer/launcher swap (`/usr/local/bin/waypictures`, .desktop) is not part of the crate.
- Python's optional `PICTURES_UI_SCREENSHOT` path is honoured by the Rust UI test, but no documentation screenshot is produced.
