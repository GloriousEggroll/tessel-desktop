# wayshell — parity with ~/btmanager/wayshell (contracts/wayshell.md)

Crates: `wayshell` (this crate: daemon binary `wayshell`, keyboard client
`wayshell-cli`, library), `wayshell-theme` (wayshell_theme.py), `wayshell-prompt`
(§11 item 9) and `wayshell-capture-picker` (§11 item 10); each sibling crate has its
own PARITY.md. Desktop/wallpaper/icons: `PARITY-desktop.md`. File operations and the
wallpaper chooser: `PARITY-fileops.md`. This file covers the daemon core, the CLI,
the switcher, the overview and the measurements.

Cargo features (all default): `desktop` (implies `fileops`), `fileops`, `theme`.
They existed only to isolate parallel work; the product is built with defaults.

## ws-core
| Contract item | Status | Test |
|---|---|---|
| switcher_model MRU: append, only activation *transitions* reorder, deterministic order, frozen while active, closest-successor reselect, cancel on empty, finish/cancel never invent activation | done (`switcher::model`) | 12 unit tests = test_switcher_model.py 1:1 |
| `preview_key` / `unique_preview_target` / `overview_layout` / `layout_limits` | done (`switcher::overview`) | `ambiguous_window_images…`, `layout_keeps_previews_readable…`, `layouts_match_python_values` (values from Python), `python_round_is_half_even` |
| AppCatalog `resolve` (exact id, stem, wm-class, unique basename; reverse-DNS authoritative) | done (`app_catalog`) | `resolve_exact_class_and_unique_basename` |
| application_audio `application_key` / `matches_application` | done (`switcher::audio`) | `desktop_ids_are_authoritative_and_interpreters_never_match` |
| icon layout, workarea parser, labwc env, clipboard/unique_name/ConflictResolver, capture_sources, portal config, palette reader/CSS | done by the desktop / fileops / capture / theme ports | see their PARITY files |

## ws-cli
| Item | Status | Test |
|---|---|---|
| `wayshell.sock`: unlink at start, listen, chmod 0600, one JSON line in/out, dispatch on main loop, **not** unlinked at shutdown | done (`server`) — reads asynchronously (Python blocked the main loop on a slow client) | `real_daemon_starts_switcher…` (stale socket at start, socket left after SIGTERM) |
| Errors: empty request, `bad json: …`, `Unknown cmd '…'`, Python exception texts for non-dict requests / `monitor: null` (`sequence item 0: expected str instance, NoneType found`) / unreadable `json_path` (`[Errno 2] …: '…'`) | done | `errors_and_switch_direction`, `set_validates_and_accepts_null_file`, `apply_json_skips_unknown_bad_and_missing` |
| `switch-window` real-bool `reverse`; `switcher-status` field order; `show-activities` optional/validated monitor; `list-monitors` connector or `Monitor-<i+1>`; `set` (`file: null`, mode passthrough, GDK colour parse, missing file); `apply-json` silent skips | done | same + `show_activities_forwards_optional_monitor` |
| argv grammar (client path and `do_command_line`): argparse prefixes, `--opt=v`, `-mX`, choices, required, usage + error text byte-identical, exit 2, `Invalid arguments. Use --help.`; `-h` block; `--gapplication-service`; `CLI error: …`; client prints msg/OK/monitors/status JSON | done (`cli`) | `grammar_matches_argparse`, `argparse_error_text_is_python_text` (13 cases captured from Python), `requests_match_python_payloads` |
| `do_command_line` messages (`Unknown monitor(s): …`, `File not found: …`, `OK\n\n`, doubled list-monitors newline, printed on the daemon's stdout) | done (`app`) | integration (`wayshell list-monitors` via the binary) |
| wayshell_cli.py: monitor_name rules, payload omits absent monitor, 2 s / 64 KiB / dict reply, `Wayshell: <err>`, help/usage text | done (`wayshell-cli` binary, `cli::keyboard_main`) | `monitor_name_validation`, `keyboard_client_payloads_and_errors` |
| Owns `com.nobaraDesktopView` (unique GApplication, HANDLES_COMMAND_LINE) | done (gtk::Application) | integration (private bus) |
| `CLI_XML` interface | not ported (dead code in Python, per contract) | — |

## ws-toplevel
In-process wlr-foreign-toplevel client (`switcher::backend::NativeBackend`, suite-wayland
`GlibClient` on its own connection) instead of spawning `wayshell-toplevel`: same
`ready`/`can_activate` (seat)/`error`/ordered windows, ids `"<session>:<native>"`, ids
invalidated on loss, reconnect 1 s→15 s backoff, 8 s startup timeout, helper error
strings. The helper binaries still exist in suite-wayland for other consumers
(capture picker, waydock). Tests: all native tests below drive it.

## ws-switcher UI (`switcher::ui`, `switcher` controller)
| Item | Status | Test |
|---|---|---|
| Layer OVERLAY, ns `wayshell-switcher`, exclusive −1, keyboard EXCLUSIVE; activities = all anchors | done | `title_updates_keep_widgets…`, `real_window_capture…` (full-output size) |
| Backdrops per monitor (`wayshell-switcher-backdrop`, keyboard NONE, Cairo buffer, click = cancel) | done | `title_updates…` (outside click), `real_daemon…` (real pointer click on the other output dismisses) |
| STYLE CSS verbatim at USER+1; classes, card structure, close/mute overlays, tooltips, fallbacks | done | `fallback_css_parses`, `real_close_click…`, `card_actions_follow_audio…`, `mute_click_does_not_activate_or_close` |
| Cards survive title/state updates; reparent only on order change; manual scroll kept, selection revealed; many windows scroll; surfaces follow the monitor | done | `title_updates…`, `manual_scroll…`, `many_windows_scroll…` |
| Alt release: modifier-state notify, Alt_L/R release → idle, focus-in → idle, 50 ms poll; `_gesture_closed` until Alt re-press; `display.sync()` before activation | done | native: `hold_tab_repeat_reverse_release_and_fast_tap` (incl. fast tap), `escape_cancels_and_ignores_more_tabs…` |
| Minimized restore, closed selection safe, activities persistent then Alt+Tab release, close targets without activation and respects refusal, fullscreen geometry kept | done | native: `minimized_window_restores…`, `activities_stays_open…`, `activities_close_targets…`, `fullscreen_geometry_survives_alt_tab` |
| PipeWire per-app mute (WirePlumber mixer via dlopen, core closed with the switcher) | done (`switcher::audio`) | unit match tests + mute UI test; real PipeWire streams not exercised headless |
| Theme refresh / `set_window_monitor` | done (wayshell-theme) | theme crate's headless tests |

## ws-overview
In-process ext-image-copy-capture (`OverviewCapture`) replacing the `wayshell-overview`
helper process: desktop frame first, then identities, then windows; 1200 ms per frame,
10 s for windows, 12 s overall; thumbnails ≤1600×1000 / ≤640×400 (cairo bilinear, straight
to a `GdkMemoryTexture`, no PNG/base64 round trip); NORMAL transform only; closing drops the
connection (= killing the helper). Animated `Stage` (280 ms ease-out cubic, desktop shrinks
16 %, content from 1.10, honours gtk-enable-animations, shares the desktop's wallpaper
paintable). Tests: `real_window_capture_and_overview_cleanup`, `overview_visual_layout`.

## Gaps / deliberate differences
- The Python native test `test_activities_stays_open_then_alt_tab_uses_normal_release` types Escape
  before the overlay maps; the **Python original fails that line** in the same runner. The Rust
  port waits for overlay focus first (comment in the test).
- `bad json:` messages approximate Python's `JSONDecodeError` text for the common
  `Expecting value` / `Extra data` cases only.
- The fullscreen test's adaptive dock half (DockSurface) belongs to waydock.
- `test_live_palette_updates_without_replacing_cards` is covered by wayshell-theme's headless suite.
- WirePlumber mute is not exercised against a real PipeWire graph in tests.

## Measurements (private headless labwc, release builds, same harness for both)
| | Rust | Python |
|---|---|---|
| Daemon ready (spawn → `switcher-status` ready), 2 outputs, temp HOME | 186–206 ms | 301 ms |
| Daemon idle RSS / PSS (desktop layers on 2 outputs + switcher warm) | 67 MB / 37 MB | 104 MB / 67 MB |
| … after two Activities openings | 100 MB / 58 MB | 140 MB / 91 MB |
| Alt+Tab open latency (XTEST key → labwc keybind → CLI → switcher focused), median of 12 | 18 ms (bimodal 18/33 = frame) with `wayshell-cli` | 50 ms (python CLI + Python switcher) |
| Same, Rust switcher but `python3 wayshell_cli.py` keybind | 49 ms | — |
| Overview open (show → mapped / first preview), in-process capture | 9–11 ms / 16–18 ms | 32 ms / 32 ms |
| Release binaries | wayshell 2.5 MB (full), wayshell-cli 1.6 MB (libc only) | — |

Runners: `tests/run_headless_switcher.sh` (`DAEMON=python` for the Python daemon,
`EXTRA=…/tests/measure/py_overview.py`), `tests/run_switcher_native.sh` (`latency`, `python-latency`).

## Swap notes (installer step; nothing is installed by the port)
- Binaries → `~/.local/share/wayshell/`: `wayshell-rs` (daemon; or replace `wayshell` itself — no
  consumer imports it), `wayshell-cli`, `wayshell-prompt`, `wayshell-capture-picker`. Ship
  `~/btmanager/shared/locale/*.json` to `~/.local/share/desktop-suite/locale/`.
- systemd: edit only the `wayshell.service.d/palette.conf` drop-in (authoritative; the base
  unit's ExecStart is stale): `ExecStart=` + `ExecStart=%h/.local/share/wayshell/wayshell-rs`.
  Keep `50-desktop-suite-recovery.conf`. Rollback = restore the drop-in line.
  The daemon spawns no helper processes (toplevel + capture are in-process), launched apps go
  through `systemd-run --user … app-waydock-<uuid>.service`, so `verify_service_children` passes.
- labwc rc.xml: repoint `A-Tab` / `A-S-Tab` to `~/.local/share/wayshell/wayshell-cli switch-window
  [--reverse]` (no Python startup: ~50 → ~18-33 ms open latency). install_switcher.py rewrites these
  keybinds to the Python path when re-run — update it in the same step.
- Replace `~/.local/share/wayshell/wayshell_cli.py` with `shim/wayshell_cli.py` (execs
  `wayshell-cli` when run, keeps `monitor_name`/`request`/`main` importable for
  wayshell_switcher.py and waydock hot corners).
- Readiness probe unchanged: `wayshell-cli switcher-status` / `wayshell_cli.py switcher-status`
  print JSON with `ready` and `can_activate`.
- Prompt / capture picker: see their PARITY.md (launchers in `~/.local/bin` exec the Rust
  binaries; the prompt here-doc delimiter and xdpw `chooser_cmd` are unchanged).
