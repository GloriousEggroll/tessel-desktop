# tessel-session — parity with contracts/tessel-session.md

Rust crate `components/session`, binary `tessel-session` (= `session_manager.py`).
Tests: `cargo test -p tessel-session` (unit + fake-helper + private-bus), `tests/run_headless.sh`
(the 11 Python UITests + 1 extra in a private nested headless labwc), `tests/measure_headless.sh`
(Rust vs Python timing/RSS + screenshots, private labwc, nothing clicked).
**Safety:** every test uses a fake backend, a recording command runner and/or fake logind on a
private bus. Nothing can lock, log out, suspend, reboot or power off.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Item | | Test |
|---|---|---|
| GtkApplication `org.tessel.Session`, `HANDLES_COMMAND_LINE`, every invocation forwarded to the primary (no IS_LAUNCHER) | ✅ | ui_headless (registration), measure_headless (`--menu` forwarded to the running popup) |
| `--popup`, `--menu`, `--popup-edge {top,bottom,left,right}` (bottom), `--popup-offset INT` (76, negative → error); argparse abbreviations, `int()` parsing, usage/help/error text byte-identical; validated in the launcher (exit 2) and the primary | ✅ | `args::tests::*`, `private_bus::binary_validates_arguments_locally` |
| `show()` while busy only re-activates when the end-session report is shown; menu ↔ edge switch destroys + recreates | ✅ | ui_headless `menu_mode_*`, `dismissing_remaining_list_cancels` |
| tessel-action probe: the installed `session_manager.py` must contain `'--menu'` | ✅ | `shim/session_manager.py`, `args::tests::shim_keeps_the_menu_flag_*` |

## §2 IPC
| Item | | Test |
|---|---|---|
| login1 `CanSuspend/CanReboot/CanPowerOff` (NO_AUTO_START, 3000 ms, cancellable) → `yes`/`challenge`; errors → status text | ✅ | `backend_tests::restart_capability_is_queried_on_connect`, `capability_errors_*`, private_bus |
| DisplayManager `Properties.Get(Seats)` → switch user only for `$XDG_SEAT_PATH` (default Seat0) | ✅ | same + private_bus |
| `Seat.SwitchToGreeter` 5000 ms NO_AUTO_START after a successful lock | ✅ | `switch_user_locks_before_greeter`, private_bus |
| `Suspend/Reboot/PowerOff(b true)`, flags NONE, 120000 ms, refused without capability (`That action is unavailable.`) | ✅ | `sleep_locks_first_*`, `restart_checks_its_own_capability_*`, private_bus |
| `ListInhibitors` 3000 ms → `parse_inhibitors` (block/delay, `shutdown` in the colon list, logout = own uid) | ✅ | `end_tests::inhibitors_filtered_by_action`, private_bus |
| Lock = `systemctl --user start tessel-lock.service`; logout = `labwc --exit` only if `/proc/$LABWC_PID/comm` == `labwc`; stdout silenced, stripped stderr as error | ✅ | `lock_is_manual_*`, `logout_requires_our_compositor_session`, `command_runner_reports_status_and_stderr` |
| `tessel-dock-toplevel` helper protocol (window_backend.py subset): JSON events ready/window/closed/output/output-removed/error, ids `<session>:<native>`, `close N\n` writes (queue ≤128), 8 s startup timeout, relaunch 1 s doubling to 15 s, invalid output → lost | ✅ | `helper_windows::*` (fake helper script) |

## §3 Action semantics / graceful end session
All rows of the contract table (confirm, graceful close, lock-first for sleep/switch user,
cancel messages, `Unknown session action.`), `perform` (busy, hold, dismiss first), success →
quit, failure → status (or `The action could not be completed.`) and re-show: ✅
backend_tests (11), ui_headless (`destructive_*`, `restart_confirmation_*`, `lock_button_*`).
EndSession: READY 3000 ms / 150 ms initial-window wait / one close per snapshot window /
CLOSE 10000 ms, note `Open apps could not be checked.`, `blocking()`, `on_update`
auto-continue, cancel = no report + helper stopped: ✅ end_tests (6), helper_windows
(`end_session_through_the_helper`), ui_headless (`list_updates_*`, `remaining_apps_*`).

## §4 Files
None (env `LABWC_PID`, `XDG_SEAT_PATH` only). ✅

## §5 UI
Title `Session`, width 380, classes `tessel-session tray-popup popup-surface title subtitle
session-title session-action session-blocker session-tile destructive-action`, CSS verbatim at
APPLICATION+31 (removed on unrealize), rows/icons/details, confirmation, remaining view (window
icon lookup via DesktopAppInfo `<id>`/lowercase/last segment, `Untitled window`, inhibitor rows
`Unknown application`/`Blocking`/`Delaying`, scroller max 320), status hidden when empty, capability
sensitivity, switch-user visibility, edge popup (shared PopupController group `session`), menu
mode (no anchors, keyboard EXCLUSIVE, pointer-monitor probe, click-away backdrops): ✅ ui_headless
(12) and measure_headless — Rust and Python popup and menu screenshots are **byte-identical PNGs**.

## §7 i18n
All strings via `suite_core::tr!` with the Python keys (incl. the trailing-space lock-failure
prefixes; the stored note is translated where shown). ☑

## Deviations ⚠
1. D-Bus via gio (behind a local `SystemBus` trait with per-call flags/timeouts), not zbus —
   same reason as tessel-power; tessel-core's `Bus` has fixed 10 s/120 s timeouts.
2. The window helper is located as `<exe dir>/../tessel-dock/tessel-dock-toplevel`, then
   `~/.local/share/tessel-dock/tessel-dock-toplevel` (Python looked for `window_backend.py` in those
   folders); `SESSION_MANAGER_WINDOW_HELPER` overrides. Missing helper → the same note.
3. `--help`/argparse errors arriving in the primary are printed to the *caller's* stdout/stderr
   (Python printed them in the primary). The launcher validates first, so this is unreachable in
   practice.
4. The Rust `WindowSource` keeps only what EndSession needs (no activate/minimize consumers),
   although `activate/minimize/unminimize` are ported.

## Gaps ❌
None known for this component. Not ported: Tessel Dock's `window_backend.py` as a shared module
(other consumers stay Python until tessel-dock is ported).
