# PARITY-D — W-d appearance publishers (`src/appearance/`)

Python sources: palette_state.py, gtk_color_scheme.py, window_buttons.py,
labwc_titlebar_theme.py, papirus_palette.py, dock_appearance.py,
wallpaper_palette.py, frosted_wallpaper.py, dock_style.py, tessel-dock.py
(`export_palette`, `sync_wallpaper_preferences`, `refresh_backgrounds`).
gtk4_support.py is widget helpers only (not appearance) — left to W-e/W-f.

Test runs (2026-10-29, crate compiled standalone through a `#[path]` shim
because other directories of the crate did not build yet):
121 unit tests (`cargo test -p tessel-dock --lib appearance`), `tests/d_portal.rs`
(1), `tests/d_python_parity.rs` (14, live `python3 -I -B` on TMPDIR fixtures) —
all pass.

## Contract rows

| Contract item | Done | Tests |
|---|---|---|
| §3 portal `ReadAll(["org.freedesktop.appearance"])`, `SettingChanged` color-scheme 1/2, else GTK fallback (prefer-dark / theme name "dark"); process-local overrides only; stale replies ignored (serials); owner loss → fallback, return → re-read | yes (gio `DBusProxy`, not zbus) | controller::tests (14, ported test_dock_appearance), d_portal::portal_color_scheme_on_private_bus (fake portal on `gio::TestDBus`) |
| (addition) icon theme override follows the user: gsettings `org.gnome.desktop.interface icon-theme` set to an installed non-Papirus theme → that theme; Papirus family, unset or not installed → Papirus-Dark/Light as before; changes are followed live (injectable `DesktopIconTheme`) | yes | controller::tests::{papirus_family_or_missing_choice_keeps_papirus_following_dark_mode, non_papirus_choice_replaces_the_papirus_override_and_follows_changes} |
| §3 GSettings `button-layout` | yes, injectable `ButtonLayoutStore` (`GioButtonLayout` real; tests use fakes, never set real gsettings) | window_buttons::tests::apply_edits_files_once_and_reloads_labwc |
| §8 palette.json: compact, sort_keys, ensure_ascii=False, trailing `\n`, atomic `.palette-*`, only when bytes differ (also across restarts), failed replace keeps old file + retry, unwritable cache non-fatal | yes | palette_state::tests (9), d_python_parity::palette_json_bytes_match_python (byte-identical incl. half-even rounding/clamping, unicode, extra keys; restart over Python's file does not rewrite) |
| §8 gtk.css import line, single backup, symlinks refused, >2 MiB untouched, BOM/line endings, concurrent edit retained, disable removes only the import; tessel-dock-colors.css byte-identical | yes | gtk_color_scheme::tests (17), d_python_parity::{tessel-dock_colors_css_matches_python, gtk_css_edits_match_python (10 originals, enable/re-enable/disable, whole directory compared)} |
| §8 labwc/environment primary block (shlex, never executed, stat-cached) | yes | gtk_color_scheme::tests::{primary_*, bad_primary_*}, d_python_parity::primary_output_selection_matches_python |
| §8 rc.xml `<theme><titlebar><layout>` / `<dropShadows>`, bytes outside `<theme>` preserved, minidom re-serialisation of `<theme>` reproduced | yes (roxmltree + own minidom writer) | rc_xml::tests (5), d_python_parity::rc_xml_edits_match_python (9 docs incl. CRLF, CDATA, PI, entities, attr normalisation, DOCTYPE/malformed errors) |
| §8 `labwc --reconfigure` async, coalesced, 5 s kill, only with `LABWC_PID` | yes (`gio::Subprocess`, injectable argv / labwc check) | window_buttons::tests::{reload_runs_off_the_main_loop_and_coalesces_requests, no_reload_outside_labwc} |
| §8 gtk-3.0/4.0 settings.ini `gtk-decoration-layout` (only existing files; Python's universal-newline rewrite kept) | yes | ini_value_replaces_or_adds, d_python_parity::{ini_edits_match_python, window_buttons_apply_writes_the_same_files_as_python} |
| §8 labwc theme themerc keys + SVG buttons (only an already managed theme), palette debounce 600 ms | yes | labwc_theme::tests (5), window_buttons::tests::palette_changes_regenerate_labwc_theme_once_per_burst, d_python_parity::{labwc_themerc_and_svg_buttons_match_python, window_buttons_apply_writes_the_same_files_as_python} |
| §8 Papirus private mirrors, marker/owner, user themes untouched, restore original colour, symlinked places refused, single debounced worker, one main-loop refresh | yes (std thread + Condvar; `IdleQueue` injectable) | papirus::tests (15, ported test_papirus_palette), d_python_parity::papirus_folder_matching_matches_python |
| §8 wallpapers.json reader (64 KiB, 64 outputs, `_` keys, Color Only, relative paths), gdk-pixbuf sampling, ffmpeg frame (same argv), 3 s poll + directory monitors (150 ms coalesce), superseded results dropped | yes | wallpaper::tests (31: test_wallpaper_palette + test_wallpaper_watching on real GFileMonitors), d_python_parity::{wallpaper_sampling_matches_python (PNG/JPEG/alpha/videos + CSS colours identical), wallpaper_reader_results_match_python} |
| §8 frosted cache ≤64 files / 16 MiB `tessel-dock-frosted-v3` | yes | frosted::tests (11), d_python_parity::frost_glass_pipeline_matches_pillow |
| dock_style CSS / LIGHT_CSS byte-identical; per-clone CSS | yes (`css/*.css` generated from the Python constants) | d_python_parity::{static_dock_css_is_byte_identical, dock_clone_css_matches_python}, style::tests |
| §10 #17–#20 | yes | as above |
| test_frost_integration (non-UI) | yes as pure functions (`export_palette`, `wallpaper_enabled`, `dock_background`) | export::tests (5) |

## Honest gaps
- Frosted textures: blur, colour-enhance, blend, alpha-composite and the
  thumbnail size maths are Pillow-exact (verified), but resizing uses
  gdk-pixbuf HYPER instead of Pillow LANCZOS(+reducing_gap) and EXIF via
  gdk-pixbuf, so textures of non-solid wallpapers differ slightly from
  Python's (different digests). The dock never enables Frosted textures
  (`set_frosted_enabled(False)`), so nothing user-visible depends on it.
- `error` strings that embed library messages differ (serde_json vs
  `json.loads`, roxmltree vs expat, gdk-pixbuf "cannot identify" text);
  errno-based OSError texts match.
- minidom namespace corner cases (xmlns attribute reordering, unbound
  prefixes inside `<theme>`) and `~user/…` in wallpaper paths (`~` and `~/`
  are handled) are not reproduced.
- labwc_titlebar_theme.py `--apply` CLI not ported (tessel-session-startup keeps
  running the Python script).
- Real `GtkSettings`/`GtkIconTheme` backends are thin wrappers not exercised
  under a headless compositor here (the logic is tested through the trait fakes).

## Integrator wiring
- Create `AppearanceController::new(Some(cb), ControllerOptions::default())`
  after GTK init; `start()`; on change reload `style::appearance_css(is_dark)`
  into the APPLICATION provider and refresh backgrounds.
- `WallpaperPalette::new(cb, WallpaperOptions::default())` and
  `Publisher::new(Some(folder_icons_changed))` on the main thread (they
  capture the thread-default context); folder_icons_changed →
  `appearance.refresh_icons()`. `export::sync_wallpaper_preferences(..)` then
  `wallpaper.start()`.
- `refresh_backgrounds` → `export::dock_background(..)` → `style::dock_css(..)`
  per clone (panel at APPLICATION+2, popup+menu in a second provider), then
  `Publisher::export(is_dark, &ExportSettings, &monitors, Some(&wallpaper))`.
- Shutdown: `wallpaper.close()`, `publisher.close()`, `appearance.close()`.
- `tests/d_python_parity.rs` and `tests/d_portal.rs` need TMPDIR set.
