# PARITY-E — W-e application, CLI, surfaces (`src/app.rs`, `src/app/`, `src/cli.rs`, `src/main.rs`, `src/ui/{surface,magnifier,hot_corners,menu_probe}.rs`, `src/ui/surface/`)

Sources: waydock.py `class Waydock` (1-858), dock_ui.py (everything except F1's
app menu / preview bodies), hot_corners.py, gtk4_support.py helpers (F2's
`ui::host` copies are reused), panel_ui `register_capture_controls` (F2's
`ui::capture_controls`, wired at startup).

Runs:
- `cargo test -p waydock --lib cli ui::magnifier ui::hot_corners ui::surface` (unit, pure parts)
- `tests/run_e_headless.sh [filter]` → `tests/e_headless.rs` (private labwc, 2 outputs, private bus with an
  empty `<servicedir>`, fake HOMEs, the real debug binary via `CARGO_BIN_EXE_waydock`)
- `tests/compare/run.sh dock bar` (Python vs Rust; results below)

## Contract items

| Item | Done | Test |
|---|---|---|
| §1 GApplication `org.waydock.Waydock`, `HANDLES_COMMAND_LINE`, argv forwarded to the primary | yes | e_headless `launcher_options_never_start_a_dock_and_singleton_health_shutdown` (second plain start exits 0) |
| §1 `IS_LAUNCHER` for `--check --quit --menu --menu-hotkey --toggle-desktop --desktop-status` (any argv item, as `in sys.argv`) — never starts a dock | yes | `cli::tests::launcher_options_never_start_a_dock`, e_headless (NameHasOwner stays false) |
| §1 dispatch order, `Unknown option: X\n` on stderr, exit 2 | yes | `cli::tests::dispatch_follows_python_order`, e_headless |
| §1 `--check` JSON: keys/order/separators identical (`json.dumps` defaults), exit 0 iff ready && docks | yes | e_headless key-order assertions; compare harness check_rc 0 both; `--check` output diffed by hand vs Python (same keys/values except environment-dependent strings) |
| §1 `--desktop-status` bytes | yes (C's `desktop_status_line`) | e_headless |
| §1 sd_notify `READY=1\nSTATUS=…` only after startup AND tray `ready`+`host_registered` (B's ServiceReadiness; tray `changed` → notify_ready) | yes | e_headless asserts exact datagram for every dock start |
| §1 SIGTERM/SIGINT → `quit()` → orderly shutdown (Python order) | yes (glib-unix) | e_headless `Dock::stop` exit 0 |
| §2.1 app actions `prepare-capture(b)` / `finish-screenshot` | yes (F2's `register_capture_controls`) | F2's tests |
| §2.2 `org.waydock.Settings` with `SettingsHost` (config/save/preview/restore/tray_apps/monitors/docks incl. live exclusive zones) | yes | e_headless `workareas_changed_is_emitted_only_on_change` (Apply edge → rebuild + workarea), all reservation tests use GetWorkareas |
| §2.2 `WorkareasChanged` once after startup, on reservation/monitor geometry changes, only on change | yes | e_headless `workareas_changed_is_emitted_only_on_change` |
| §5 menu target: one dock → it; several → `waydock-menu-target` probe (60 ms), fallback activated window's output → first monitor; second press cancels; other clones close; app dock → bar's menu | yes (`ui::menu_probe`) | e_headless `menu_targets_pointer_output_and_toggles`, `separate_app_dock_reserves_both_strips` |
| §5 `--menu-hotkey` suppressed on cached fullscreen unless `menu_hotkey_fullscreen` | yes (C's FullscreenGuard) | e_headless `menu_hotkey_respects_fullscreen_option` |
| §5 show desktop (C's ShowDesktop; indicator via StatusBackend) | yes | e_headless `show_desktop_restores_only_its_windows` (pre-minimized kept, maximized kept, focus restored) |
| §5 window click rules (focused → minimize, else activate; multi → preview) | yes (via F1 `activate_window`) | e_headless `dock_click_minimizes_focused_and_restores`; compare harness preview shots |
| §5 focused title minimize/restore shared across clones | yes (A's FocusedTitle, app-wide) | logic in A's tests; not GUI-tested here |
| §5 Activities (`/usr/bin/python3 ~/.local/share/wayshell/wayshell_cli.py show-activities --monitor N`, one in flight, stderr → error) | yes | e_headless hot-corner tests (fake wayshell_cli.py records argv) |
| §5 hot corners (TOP `waydock-hot-corner`, 3×3 + edge strips input region, dwell 100–2000, latch, re-entry, fullscreen block, raise after TOP promotion, status JSON) | yes | `ui::hot_corners::tests::*`, e_headless `hot_corner_dwell_latch_and_reentry`, `hot_corner_is_blocked_by_fullscreen` |
| §5 monitor naming/selection | yes (A's `monitor_entries` with `gdk::Monitor` keys) | compare harness `monitor_labels` identical |
| §5 launches (A's `Launcher`, GDK launch context timestamp 0, history) | yes (F1's `launch_app`) | A's tests; not exercised GUI-side (never launch real apps) |
| §6 `waydock` / `waydock-reveal` / `waydock-app-reservation` surfaces, anchors, margins, layers | yes | compare harness (0 px diff panel shots), e_headless reservation tests |
| §6 workarea rule: bar reserves thickness+gap even while hidden; maximized window on that monitor drops gap+rounding until the last one leaves; dock never reserves | yes | e_headless `every_edge_reserves_bar_plus_gap_even_while_hidden`, `bar_clones_reserve_only_their_own_output`, `maximized_window_drops_float_gap_per_monitor` |
| §6 hover/layer sync (BOTTOM↔TOP, no unmap), state tick only while needed | yes | e_headless `fullscreen_window_covers_the_dock` |
| §6 groups/layouts (legacy zones, custom zones, separators, availability, empty-panel tooltip), bar/dock/custom scroll limits | yes | e_headless `custom_group_positions_and_hidden_utilities`; `ui::surface::groups::tests::*` |
| §6 item buttons: running badge only when count > 1, indicator dot, magnifier (+headroom, input region) | yes | compare harness preview shots (badge "2"); `ui::magnifier::tests` |
| §6 context menus (item/background/trash, Move earlier/later sensitivity, Dock settings) | yes | compare harness context shots 0 px diff |
| §6 backgrounds: per-clone display CSS (APPLICATION+2), blur via suite_gtk::blur, popup/menu CSS | yes (D's `dock_css`) | compare harness |
| §6 separate app dock (pairing, reservation, placement under the bar) | yes | e_headless `separate_app_dock_reserves_both_strips` |
| §8 config.json via A's ConfigStore, palette/gtk.css/window buttons/papirus via D's Publisher (real default paths; tests use fake HOME/XDG) | yes | compare harness + e_headless run with fake XDG dirs |
| §3 trash watch (`trash:///` monitor, 300 ms debounce, 10 s poll fallback) | yes | not GUI-tested (no GVFS on the private bus → poll path) |
| §10.22 efficiency (menu churn) | yes (fix-up) | e_headless `menu_churn_does_not_grow_memory` (now also requires every batch < 2 MB for Rust); f1 `menu_reuses_icon_paintables` |

## Compare harness (tests/compare/run.sh dock bar, release)

| | ready ms | RSS kB | PSS kB | menu first frame ms |
|---|---|---|---|---|
| dock python | 253 | 106096 | 66402 | 145 |
| dock rust | 166 | 72076 | 42228 | 65 |
| bar python | 252 | 106912 | 66659 | 145 |
| bar rust | 170 | 74980 | 44356 | 65 |

(fix-up run; RSS/PSS after the menu: dock python 132116/88333, rust 97648/63673; bar python 133072/88881, rust 100052/65342 kB)

Screenshots: panel, menu, after-menu, context, applets, preview: 0 px differ for dock and bar (after fixing
the popup anchor extent to Python's `get_allocated_width`, the margin box). `--check` rc 0 for both.

e_headless: all pass. Every test was also run against the Python dock (`E_PYTHON=1`) to validate the
expectations.

Menu churn (fix-up): closed menus were already fully released (weak-ref probes on popup, backdrops,
listing, tiles and `MenuState` all came back dead). The growth was GTK 4.22's per-`GtkImage` gicon
loading: every open rebuilt ~all tiles with `Image::from_gicon`, and each re-load left ~0.5 MB behind
(malloc_trim does not return it; without tile icons RSS is flat; Python shows the same ~7 MB / 10 menus).
The menu now uses `popup::menu_icon_image`: an icon seen before reuses the paintable from
`IconTheme::lookup_by_gicon` (display, gicon string, size, monitor scale, direction; cleared on the theme's
`changed`); a new icon uses the lazy gicon image (first-open latency unchanged, 65 ms) and is looked up in
LOW-priority idle chunks after 200 ms. RSS per 10 menus: before 162/179/186/194/203 MB, after
116.93/116.93/116.94/116.94/116.94 MB (flat); screenshots stay 0 px (first open and cached reopen).
`E_CHURN_BATCHES=N` runs more batches.

## Honest gaps
- `hot_corner_is_blocked_by_fullscreen` is kept but not run: in the private labwc the corner fires over a
  fullscreen client with the Python dock too, so it is not a parity check.
- (fixed) `status_errors` in `--check` now comes from `StatusBackend::errors()` in Python's dict order
  (`status::backend::tests::errors_keep_python_dict_insertion_order`).
- (fixed) `drag::pin_index_at` uses `allocated_width/height` like Python (f1 `drop_index_uses_margin_box`).
- Like Python, a non-launcher unknown option with no dock running starts a dock (`hold()`), then exits 2 only for the remote.
- No Python test was ported 1:1 at mock level (test_dock_ui mocks GTK internals); ports are behavioural
  in a real compositor. Focused-title clicking, trash empty dialog, drag & drop and settings window
  through the dock are not GUI-tested here.
- Layer-shell unsupported → prints the message and exits 1 (Python raised in `do_startup`).
- The menu icon is the installed `~/.local/share/waydock/icons/…` file, else an embedded copy extracted to
  `$XDG_RUNTIME_DIR/waydock/` (GTK recolours `-symbolic.svg` files only from a path).
