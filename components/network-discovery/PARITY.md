# tessel-network-discovery — parity with contracts/tessel-network-discovery.md

Rust crate `components/network-discovery`: binary `tessel-network-discovery`
(= `discovery_manager.py`), library `network_discovery` (`backend` = the
systemd D-Bus state + pkexec toggle Tessel Dock embeds, `browse` = avahi-browse
parser/runner, `app` = GTK popup), example `fake_systemd` (measurement fake).

Tests: `cargo test -p tessel-network-discovery` (unit + `private_bus`: fake systemd on
a private dbus-daemon used as the system bus, recording shell script instead of
pkexec), `tests/run_headless.sh` (popup in a private nested headless labwc),
`tests/measure_headless.sh` (Rust vs Python). Avahi/systemd/pkexec are never
touched.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Contract item | | Test |
|---|---|---|
| `--toggle|--popup|--settings|--status`, `--popup-edge` choices, `--popup-offset` int, negative → `--popup-offset must be nonnegative`; argparse texts, exit 2 | ✅ | `compat` argparse (shared twin, tested in tessel-tiling), ui_headless `toggle_and_command_line` (exit 2 for -1) |
| GApplication `org.tessel.NetworkDiscovery`, `HANDLES_COMMAND_LINE`; second invocation repositions the popup | ✅ | ui_headless `toggle_and_command_line`, `moving_to_another_edge_*` |
| `--status`: no GTK, loop until ready, one `json.dumps(snapshot())` line | ✅ | private_bus `status_command_prints_one_json_line`; measure: byte-identical to Python |
| `--toggle`: hold, wait for ready, flip; failure → popup with message; ignored while pending/busy | ✅ | ui_headless `toggle_and_command_line`, `toggle_without_avahi_shows_the_message` |
| Snapshot JSON keys/order/labels/classes, `` text | ✅ | `backend::tests::*` (incl. `snapshot_json_matches_python`) |

## §2 IPC (systemd, system bus)
| Item | | Test |
|---|---|---|
| Manager proxy DO_NOT_AUTO_START; `notify::g-name-owner` → resubscribe; no owner → "service manager is unavailable" | ✅ | private_bus `service_manager_restart_resubscribes` |
| `Subscribe` (AlreadySubscribed tolerated) then refresh | ✅ | private_bus (Subscribe counted) |
| UnitNew/UnitRemoved (our units only), UnitFilesChanged, Reloading(false) → refresh only if none pending | ✅ | private_bus `units_are_observed_through_systemd_signals` |
| LoadUnit ×2, NoSuchUnit/LoadFailed → not-found, other errors → friendly | ✅ | private_bus `missing_avahi_and_errors` |
| Unit proxies, cached LoadState/ActiveState/UnitFileState, PropertiesChanged → re-read; old proxies dropped on refresh | ✅ | private_bus (one update per signal after refresh) |
| Generation counter discards stale replies | ☑ | implemented as in Python, not separately provoked |
| Unsubscribe on close (2 s, fire-and-forget), closed backend ignores signals | ✅ | private_bus |
| pkexec argv (enable/disable), non-bool refused, single flight, refused when not ready/not installed, stdout silenced/stderr piped, 126/127 = cancelled, friendly failure / default text, always refresh | ✅ | `backend::tests::commands_*`, private_bus `privileged_changes_are_async_single_flight` |
| Popup dismissed before pkexec, app held until done | ✅ | ui_headless `popup_style_state_and_dismissal_before_authentication` |
| `friendly_error` table, quark/GDBus prefix and ` (N)` stripping, cancellation | ✅ | `backend::tests::errors_are_friendly_*` |

## §3 Browsing
argv, 12 s SIGTERM keeping partial output, single flight, error texts, `\DDD` unescape, MAC prefix/suffix, hidden types, grouping, IPv4 preference, opaque titles, anonymous merge by address, sort, SERVICE_NAMES + fallback label: ✅ `browse::tests::*` (Python fixture + extra cases cross-checked against Python), private_bus `browser_is_async_single_flight_*`. The 12 s timeout path is ☑ (not waited for in tests).

## §5 UI
Title, `discovery-manager`/`tray-popup` (added after construction; theme class kept), `.popup-surface`, CSS verbatim, width ≤420, header `network-workgroup-symbolic` + subtitles, explanation, switch + tooltip + accessible label + sensitivity, detail only for problems, `Found on this network`, `Search again` sensitivity, `list.discovered.boxed-list` rows (name, `services · address`), scroll max 260, status messages, footer, browse once on open/enable, browser closed with the popup, re-show on edge change, real dismissal destroys: ✅ ui_headless (4 cases); measure_headless popup screenshot is pixel-identical to Python's.

## Deviations ⚠
1. **D-Bus via gio, not zbus** (like tessel-power): GDBusProxy gives the same DO_NOT_AUTO_START, cached-property and owner-tracking semantics as the Python code, on the GLib loop.
2. Python shim execs the binary as `[python3, discovery_manager.py, …]`; the binary drops the interpreter argument (argparse prog stays `discovery_manager.py`).
3. A malformed LoadUnit reply (no object path) reports "Unexpected reply from the system service manager." (Python raised inside the callback).

## Gaps ❌
- Tessel Dock imports the **Python** `discovery_backend.py` in-process; it must stay installed (`~/.local/share/tessel-dock/`) until Tessel Dock is ported (the Rust `backend` module is its replacement).
- Installer (`install.py`, dock files, health check, rollback) not part of this crate; upstream `LICENSE` must still be installed next to the binary.
