# wayshell-capture-picker — parity with ~/btmanager/wayshell capture_*.py

Rust port of `capture_picker.py`, `capture_picker_ui.py`, `capture_sources.py`,
`capture_output.py`, `capture_icons.py`, `capture_portal_config.py`, the
client half of `shared/capture_selection.py`, and the planning/transaction
logic of `install_capture_picker.py`. Contract: `contracts/wayshell.md` §1,
§2.3 (CaptureSelection), §4 "Capture picker", §9 "Capture", §12.

Run: `TMPDIR=/run/user/1000/cbt/rust-ws-cap cargo test -p wayshell-capture-picker`
(unit + private `gio::TestDBus` bus), `tests/run_headless.sh` (GUI and real
xdg-desktop-portal-wlr in a private nested headless labwc, service-free bus,
private PipeWire core), `tests/measure_headless.sh` (Rust vs Python).

## Contract → implementation → test

| Contract item | Status | Test |
|---|---|---|
| `len(argv) != 1` is an error (exit 1, no stdout) | yes (`entry::run`) | entry `argv_and_input_errors_exit_one_without_output`, selection_bus `binary_rejects_arguments` |
| stdin ≤ 4 MiB (+1 read), larger → error | yes | entry `argv_and_input_errors…`, sources `invalid_or_oversized…` |
| UTF-8 decode error → exit 1 | yes (Python's message text reproduced for the common cases) | entry, sources |
| Records: duplicates dropped, NUL or >4095-byte records dropped, `Monitor: <name> <desc>` (name printable), `Window: <title> (<id>)` fullmatch | yes (`sources`) | sources `*`, python_parity `parse_sources_matches_python` (4000 generated records, byte-identical with Python) |
| Labels never alter the exact record; `display_text` (Python `isprintable` + `str.split`) | yes; Python's Unicode tables generated into `printable_table.rs` (Python 3.14 / Unicode 16) | sources `readable_labels…`, `missing_labels…`, `python_printable_and_whitespace_rules`, python_parity |
| stdout = exactly one offered record + `\n` (one write ≤ PIPE_BUF) or nothing; unknown source → error | yes; fd 1 is re-pointed at stderr at process start and the record goes to a saved dup, so no GTK/GLib/helper print can reach the portal pipe | entry `logs_never_replace…`, `cancel_and_gui_errors…`; ui_headless `production_*` |
| Exit 0 / 1 (error = no output) | yes | entry, selection_bus `binary_gui_error_is_no_consent` |
| `GDK_DEBUG`: keep flags, drop any `no-portals`, add `no-portals` unless `all`; `GTK_USE_PORTAL=0`; before GTK init | yes (`entry::configure_portal_environment`, first thing in `main`) | entry `chooser_disables_recursive_portals…`; ui_headless `production_chooser_maps_while_frontend_settings_portal_is_busy` (real xdpw, busy mock Settings portal, `GDK_DEBUG=portals` in the portal env) |
| NON_UNIQUE GApplication `org.wayshell.CapturePicker`, run with argv[0] only | yes (`gui::choose`) | ui_headless (`wlrctl` sees the app id) |
| `XDPW_CHOOSER_REMOTE_DESKTOP & 3`, non-int → 3, always shows UI, never asks CaptureSelection | yes (Python `int()` incl. `_`, sign, Unicode digits) | entry `remote_control_*`; selection_bus `binary_remote_never_preselects` |
| Preselection: `Select(o, as) → (b, s)` at `':' + group.replace('_','.')`, 1200 ms, NO_AUTO_START; session regex | yes (`selection`, gio D-Bus) | selection_bus (fake SelectionBroker on TestDBus): owner derivation, exact tokens, one-shot, vanished output cancels, invalid session/unknown owner keep UI, hung requester ≤ 1200 ms, binary end-to-end |
| `Selected(o, as)` report, 1000 ms, NO_AUTO_START, only for a valid session | yes | selection_bus `selected_reported`, `binary_reports_outputs` |
| Monitor outputs = its name; window → tiling `capture-output <id>` matched to a GDK monitor geometry; fallback `waydock-toplevel --snapshot` title match only if unambiguous | yes (`output`, tiling request via `tiling_manager::client::request`) | entry `window_outputs_prefer_tiling_identifier…`, sources `capture_output_uses_selected_window…` |
| Parent-pid poll 1 s; SIGINT/SIGTERM/SIGHUP cancel | yes | ui_headless `binary_signals_and_parent_exit_cancel` |
| Window: title "Share your screen", `.capture-picker`, headerbar "Screen sharing", 640×570 default, 380×380 min, STYLE at APPLICATION and SWITCHER_STYLE at APPLICATION+31 (removed on destroy), companion theme attached | yes (`ui`, CSS byte-identical) | ui_headless `explicit_selection…`, `live_per_monitor_palette_is_attached` |
| Screens/Windows stack (`monitor`/`window`), switcher only with both kinds, search only with ≥ 8 sources, `empty` page | yes | ui_headless `tab_switch…`, `empty_source_list_cannot_share` |
| Rows: `.capture-title`, `.muted`, `.device-icon`, `.capture-selection` check (opacity), plain (no markup) titles, tooltip/accessible text, symbolic icon fallbacks, app icons from the toplevel snapshot (unique app id per title, AppCatalog rules) | yes | ui_headless `deliberate_row_activation…`, `icons_follow_the_toplevel_snapshot`; sources `window_icons_need_a_unique_app_for_the_title` |
| `.capture-remote` notice with keyboard / mouse / keyboard-and-mouse strings | yes | ui_headless `remote_control_request_is_disclosed` |
| Cancel / Share (`.primary`, insensitive until selection); emitted clicks without selection do nothing; callbacks at most once | yes | ui_headless `explicit_selection…`, `empty_source_list…` |
| Tab switch and search clear a hidden selection | yes | ui_headless `tab_switch_and_search_clear_hidden_selection` |
| Row activation deliberate (no single-click activate); keyboard navigation selects without sharing; initial focus on Cancel | yes | ui_headless `deliberate_row_activation…`, `keyboard_navigation…`, `production_chooser_keyboard_acceptance…` (Shift+Tab, Return via wtype) |
| Escape / close-request / Cancel never share | yes | ui_headless `cancel_escape_and_window_close_never_share`, `production_chooser_escape…` |
| Long list scrolls inside 720p | yes | ui_headless `long_source_list_scrolls_inside_720p` |
| Portal-side destroy is not consent | yes | ui_headless `destroy_by_portal_cancellation_does_not_consent` |
| Real xdpw: monitor/window requests return the exact record, empty output cancels | yes (real `/usr/libexec/xdg-desktop-portal-wlr`, private bus + PipeWire, no Start) | ui_headless `monitor_request…`, `window_request…`, `empty_chooser_output_cancels…` |
| xdpw INI rewrite: only chooser keys, idempotent, missing keys in a trailing `[screencast]`, inheritance chain local/desktop → local/config → system/desktop → system/config, KDE untouched, read-only plan | yes (`portal_config`, Python `splitlines` and `\s` reproduced) | portal_config `*`, python_parity `chooser_config_matches_python` |
| Installer: plan, refuse while a capture session is active, rollback on failed restart, no enable/disable, backups + manifest | logic only (`install::install` with an injected `Commands`; no CLI, never run for real) | install `*` (scratch homes, fake runner) |

## Deliberate differences
- **Stdout guard** is at the fd level for the whole process (Python only redirected Python-level prints).
- **Parent pid** is captured at process start (Python: inside `choose`, after stdin and preselection), so a portal that exits early is still noticed.
- A requester that preselects a **window** token when GTK is not initialised: Python raises (AttributeError on `Gdk.Display.get_default()`) and exits 1; Rust reports no outputs and prints the record. The real broker only ever preselects monitors.
- A non-object JSON line from `waydock-toplevel --snapshot` → no icons/outputs (Python: uncaught AttributeError → exit 1).
- `AppCatalog` is only built when a window title matches the snapshot (Python builds it whenever the helper succeeds).
- Release builds use `panic = "abort"`: an internal bug aborts (no stdout) instead of Python's exit 1.
- The toolkit's own `gtk_window_destroy` on the picker window is not hooked (the struct holds a reference); only `CapturePicker::destroy` (Python's override) and close-request matter, as in Python.
- Installer runtime set is the Rust binary + launcher instead of the `.py` bundle.

## Gaps
- None in behaviour known. `test_capture_portal.py`'s cases all run against the real xdpw headless (`run_headless.sh`); the private bus has no activatable services.

## Measurements (headless labwc, pixman, 3 runs each, 3 records on stdin, SIGTERM after 2 s)
| | Rust | Python |
|---|---|---|
| release binary | 2.27 MB (stripped) | — |
| spawn → toplevel mapped | 286 / 287 / 305 ms | 374 / 369 / 371 ms |
| RSS / PSS at 2 s | 68.5 MB / 32–36 MB | 92.5 MB / 49–52 MB |
Both include the ≤ 0.8 s `waydock-toplevel --snapshot` for icons.
