# wayshell-theme parity (wayshell_theme.py)

Library crate `wayshell-theme` (`wayshell_theme`), used by `wayshell` and `wayshell-prompt`.
Tests: `cargo test -p wayshell-theme` (pure + Python parity);
widgets: `components/wayshell-theme/tests/run_headless.sh` (private nested headless labwc, 2 outputs).

| Contract item | Status | Test |
|---|---|---|
| `palette_path()` = `$XDG_CACHE_HOME`(non-empty) or `~/.cache` + `/waydock/palette.json` | done | (used by `WayshellTheme::new(None)`) |
| `read_palette`: ≤131072 bytes, version `== 1` (Python equality: 1, 1.0, true), strict bool `is_dark`, `apps_transparency` int 0..100 (default 5), `apps_transparency_enabled` bool (default false), `apps_frost_strength` int 0..100 (default = transparency if key present else 50), `apps_frosted`/`apps_rounded_corners` bool, 1..64 monitors with str name, 4 int geometry with w,h>0, 3×(int/float 0..255) colours; unknown keys kept; `frost_texture: null` | done | `test_valid_snapshot_and_malformed_cache`, `version_uses_python_equality`, `python_parity` (normalized dict compared to Python) |
| `monitor_palette`: connector → geometry → first | done | `test_connector_and_geometry_select_monitor_palette` |
| `frost_texture_path` → None; `frost_background` (`%d` truncation, `%.3f`), ValueError texts | done | `python_parity` |
| `_on_accent` (`#101114` if luminance > 0.179; CPython 3.12+ compensated `sum`) | done (`on_accent`) | `python_parity` (3000 accents) |
| `palette_css` text byte-for-byte, all variants (default/desktop/switcher/backdrop, frosted, square → radius 0 + 9999px controls), half-even rounding, ValueErrors | done | `python_parity` (960 cases), `test_desktop_css_only_themes_controls_and_popovers`, `test_app_transparency_is_independent…`, `test_frosted_and_square_materials…`, `test_css_parses_in_both_scopes_and_both_appearances` (GTK parser, headless) |
| `WayshellTheme`: watches every toplevel (`items-changed`), per-window `wayshell-palette-<serial>` + provider at USER+2, providers reused on change, removed on destroy / bad cache / close | done | `test_live_updates_scope_desktop_and_remove_destroyed_windows` |
| connector/geometry/first monitor selection; Python `window.monitor` attribute = `set_window_monitor(&w, Some(&m))`; else surface monitor (or transient parent's) | done | same + `test_desktop_captions_selection_and_menu_controls_follow_each_monitor` |
| marker classes `wayshell-desktop` / `wayshell-switcher` / `wayshell-switcher-backdrop`, `notify::css-classes` re-queues only on marker change | done | `test_marker_class_change_requeues_and_switcher_blur_target`, desktop tests |
| file monitor `WATCH_MOVES`, rate limit 100 ms, "Palette monitoring unavailable:" on error; monitor `items-changed` + `notify::geometry`; `realize` + surface `enter-monitor`; idle-coalesced refresh; stat stamp (ino,size,mtime_ns) | done | `test_live_wallpaper_accent_colors…` (atomic rename picked up by the monitor) |
| blur: `suite_gtk::blur::attach(window, None, true, true)` + `configure(frosted && !backdrop, strength, rounded, target "switcher-surface"/"", include_root = !(desktop‖backdrop))`, closed on removal | done | `test_frosted_updates_keep_alpha_independent_and_foreground_opaque` (binding present; compositor-side blur not asserted) |
| `refresh()` (Rust also `refresh_now() -> bool`), `close()` | done | all UI tests |
| square setting flattens and restores local styles | done | `test_square_setting_flattens_controls_and_restores_local_styles` |

Results: pure 8/8, parity 1/1, headless UI 7/7.

Gaps / notes
- `palette_css` takes `transparency: i64`; Python's `True`/`20.5` rejections are unrepresentable (type system).
- serde_json rejects `NaN`/`Infinity` and integers beyond u64 that Python's json accepts; such a cache is "malformed" here (Waydock never writes them).
- Test-only accessors added: `themed_windows()`, `window_provider()`, `window_blur()`, `snapshot()`, `is_closed()`, `window_monitor()`.
