# tessel-pictures (Pictures, `tessel-pictures`) — external contract

Source: `~/tessel-bluetooth/tessel-pictures` (`picture_viewer.py` 76 LOC launcher, `picture_model.py` 370 documents/undo/saves,
`picture_canvas.py` 168, `picture_dialogs.py` 235, `picture_files.py` 324, `picture_ui.py` 1147). Standalone app; no
D-Bus API of its own; independent of Capture / Screen Recorder. Nothing else in the suite imports its modules.

## 1. Entry points
| Item | Value | Ref |
|---|---|---|
| Installer | `desktop-apps/install.py --only tessel-pictures [--apply]` (root via pkexec): copies the `picture_*.py` modules + shared `gtk4_utils.py suite_i18n.py language_data.py suite_catalogs.py background_blur.py libtessel-dock-background-blur.so app_menu.py` + `companion_theme.py` to `/usr/local/lib/tessel/tessel-pictures/` | desktop-apps/install.py |
| Launcher | `/usr/local/bin/tessel-pictures` = `#!/bin/sh\n# Tessel standalone application\nexec /usr/bin/python3 "/usr/local/lib/tessel/tessel-pictures/picture_viewer.py" "$@"` | install.py:47-50 |
| .desktop | `/usr/share/applications/org.tessel.Pictures.desktop`: `Name=Pictures`, `GenericName=Picture Viewer`, `Comment=Browse, compare and edit images`, `Exec=/usr/local/bin/tessel-pictures %U`, `Icon=image-x-generic`, `Categories=Graphics;Viewer;Photography;`, Keywords, `MimeType=` 13 image + 8 video types (copy in `components/pictures/data/`), `StartupNotify=true`, **`StartupWMClass=org.tessel.Pictures`** | org.tessel.Pictures.desktop |
| GApplication | id **`org.tessel.Pictures`**, `HANDLES_COMMAND_LINE` (unique; a second launch forwards argv), `GLib.set_prgname(id)` | picture_viewer.py:25-58 |
| activate | create window if none/closed, `unminimize()`, `present()` | |
| command-line | parse argv; activate; files → `create_file_for_arg(arg).get_path()` (relative to the caller's cwd); any non-local → status "Open a local file or a mounted network folder." and exit 1; else `open_paths(paths, slideshow)`; no files + `-s` → toggle slideshow; `-f` → fullscreen | |

## 2. CLI (argparse, prog `picture_viewer.py`)
`picture_viewer.py [-h] [-f] [-s] [--check] [files ...]`; `-f/--fullscreen`, `-s/--slideshow`, `--check` prints
`{"ready": true, "gtk": 4, "application_id": "org.tessel.Pictures", "save_formats": ["BMP", "GIF", "JPEG", "PNG", "TIFF", "WEBP"]}`
(exit 0; `{"ready": false, "error": …}` exit 1 on import failure). argparse rules: prefixes, combined `-fs`, `--`,
`--x=v` explicit-value error, unrecognized arguments → exit 2 with usage on stderr.

## 3. Files
- Preferences `$XDG_CONFIG_HOME/tessel-pictures/settings.json` (`os.environ.get('XDG_CONFIG_HOME', ~/.config)` — an empty value is used as is): JSON object, keys `interval` (s, ≥1, default 5), `quality` (1–100, 95), `loop` (true), `shuffle` (false), `hidden` (false), `keep_zoom` (false), `thumbnail_size` (0–2, 1); written `json.dumps(indent=2)` to `settings.tmp` then replaced; unknown keys preserved.
- Undo snapshots: `tempfile.TemporaryDirectory(prefix='tessel-pictures-undo-')` in `$TMPDIR`, `<id>.png` (compress level 1), ≤20 states and ≤512 MiB (≥2 kept), removed on close.
- Saves: `mkstemp(prefix='.tessel-pictures-', suffix=<ext>, dir=<destination dir>)` (0600), encode, fsync; existing destination: signature `(st_dev, st_ino, st_size, st_mtime_ns)` re-checked, mode copied, `os.replace`; new destination: hard link (never overwrites), fallback `renameat2(RENAME_NOREPLACE)` on EPERM/EOPNOTSUPP/ENOSYS/EXDEV. Formats by extension: PNG, JPEG, WEBP, TIFF, BMP, GIF; JPEG/WEBP `quality`; EXIF (orientation 1, description, pixel size) + sRGB ICC for JPEG/PNG/WEBP/TIFF; JPEG/BMP flattened on white. Messages: "A file with that name already exists.", "This file changed outside Pictures. Use Save As to keep both versions.", "The destination changed while saving. Choose a different name.", "Use Save As to export a still frame without replacing the animation.", "Choose a regular file rather than replacing a symbolic link.", "Save as PNG, JPEG, WebP, TIFF, BMP or GIF.".
- Folder scanning: images = Pillow's registered extensions + `.svg .svgz .avif .heic .heif .jxl`; videos `.mp4 .m4v .webm .mkv .mov .avi .ogv`; hidden files optional; natural name / date / size sort, folders first. Folder monitor ignores `.tessel-pictures-*` and non-picture names, debounced 450 ms.
- Trash: `Gio.File.trash`; restore enumerates `trash:///` (`trash::orig-path`, newest `trash::deletion-date`).
- Default viewer: `Gio.DesktopAppInfo('org.tessel.Pictures.desktop').set_as_default_for_type` for the 13 image types only (installation changes no MIME defaults).

## 4. Image model
Decoding: Pillow (EXIF orientation applied once, embedded ICC → sRGB perceptual with alpha kept, 80 MP cap
"Image size must be positive and at most 80 megapixels."), GdkPixbuf fallback for other system decoders. Frames:
`n_frames` (GIF/APNG/WebP/TIFF); animations cannot be edited ("Save a still-frame copy before editing an animation or
multi-page image."); Save As exports the displayed frame and resets history. Operations: rotate left/right, mirror, flip,
crop / red-eye (rectangle inside image, "Select a rectangle inside the image."), resize (LANCZOS, checked size),
adjust (ImageEnhance Brightness/Contrast/Color + gamma LUT 0.1–5, alpha kept), grayscale, auto contrast, annotations
(rectangle, ellipse, line, arrow, freehand, text DejaVuSans `max(12, width·5)`, image stamp; width 1–100), description
(EXIF 270). Undo/redo, dirty = current snapshot ≠ saved snapshot.

## 5. UI
- Window `Gtk.ApplicationWindow`, title "Pictures" / "Pictures — <folder>" / "[• ]<name> — Pictures", class `pictures-window`, 1180×800, min 740×460, CSS `STYLE` at APPLICATION+31, `companion_theme.attach`.
- Header bar (the only toolbar): Open, Open folder, Parent, Browse, Previous, Next | caption | Save, Undo, Redo, Editing tools, Slideshow, Fullscreen, app menu (Save As…, Copy Image, Compare Selected Pictures, Open With…, Open Containing Folder, Rename…, Copy To…, Move To…, Move to Trash…, Restore Last Trashed Pictures, Print…, Information Sidebar, Thumbnail Strip, Set as Default Image Viewer, Settings… as `win.*` actions + Keyboard Shortcuts/About Pictures/Quit). Actions disabled while busy; save-as/copy-image/print need a document.
- Edit bar (`edit-bar`, "Unsaved changes", Save / Save a copy / Reload), sidebar (`pictures-sidebar`, Places Pictures/Home/Downloads, Information with size, format, MB, date `%x %X`, frames, Camera/Model/Taken/Exposure/Aperture/ISO/Focal length/Lens, description; Edit description…), browser (search, sort, thumbnail size, location entry, GridView with MultiSelection, thumbnails 116/168/236 px, 2 workers, ≤100 pending, 96-texture LRU), viewer (canvas on a neutral background with checkerboard, Gtk.Video, zoom bar Fit/Fill/100%/−/+ / animation, thumbnail strip), tools panel (`tools`, `edit-tool` buttons, tool drop-down, crop ratios, X/Y/Width/Height, Apply selection, color, size, text, stamp), status row with spinner.
- Dialogs: themed modal windows (unsaved-changes Cancel/Discard/Save, forms with error label, Open With, Compare (≤4 panes, sync zoom), conflict Cancel/Skip/Keep Both/Replace + "Apply to all", results list).
- Keys: see README table (Ctrl+O/Shift+O, arrows/Page/Space/BackSpace/Home/End navigate (arrows pan when zoomed past the viewport, Alt+arrows always navigate, video keeps Space/BackSpace), Enter / Alt+Up browse, Ctrl+S/Shift+S, Ctrl+Z/Shift+Z/Y, Ctrl+R/Shift+R, Shift+C crop, Shift+R resize, F / Shift+F, Ctrl+±/0, + / −, Ctrl+wheel, F5, F11, Escape, F2, Delete, Ctrl+C, Ctrl+P; app accels Ctrl+Q, Ctrl+?). Typing fields keep their keys. Fullscreen hides header/sidebar/tools/strip/zoom bar/status; pointer within 65 px of the top reveals the header for 2 s. Slideshow inhibits idle ("Picture slideshow"), shuffle/loop. Drag-and-drop of files opens them. Right-click context menu on canvas and thumbnails.

## 6. i18n
All visible strings above via `suite_i18n.translate` (`tr`), including the formatted ones (`{count} pictures and videos`, `Opening {name}…`, `Saved {path}`, `{width} × {height} pixels`, `“{name}” already exists in {folder}.`, `Apply to all {count} remaining conflicts`, transfer outcomes and summaries).
