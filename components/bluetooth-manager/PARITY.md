# bluetooth-manager (btmanager): parity with contracts/bluetooth.md

Crate `components/bluetooth-manager`, binary `btmanager` (replaces `btmanager.py` + `btpopup.py` +
`bttransfer.py` + `translations.py` when run as a program). Library modules: `args` (argparse),
`backend` (class BT, ERROR_ADVICE), `manager` (toolkit-free half of class App), `ui` (GTK half),
`app` (BluetoothApplication), `transfer` (bttransfer.py), `settings` (settings.ini), `i18n`
(embedded translations.py catalogs), `selftest`, `dbus`/`env` (compat transport + main-loop seams),
`testing` (Python FakeBT / recording view).

Tests (all pass):
- `cargo test -p bluetooth-manager`: 154 unit tests (ports of test_backend.py, test_transfer.py and the
  behaviour checks of test_ui.py / test_popup_routing.py), `private_bus` (real GDBus on a
  `gio::TestDBus` daemon: fake org.bluez + org.bluez.obex, real OBEX push over the private
  connection, real agent, `--selftest`, launcher/gen-pot/gen-mo).
- `tests/run_headless.sh`: GTK checks in a private nested headless labwc + private dbus-daemon (also the
  "system" bus, served by `examples/fake_bluez`), including real-binary single-instance launches.
  `MEASURE=1` also measures Rust vs Python.

No test touches the radio, the real system bus or the real session bus. `test_actions.py` was never run.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Contract item | | Test |
|---|---|---|
| no flags → window 820×710, min 560×440 | ✅ | ui_headless `window_layout_rows_actions_and_reuse` |
| `--popup` (layer-shell popup, width 400, natural height, list scrolls past 420) | ✅ | ui_headless `popup_width_natural_height_and_children` |
| `--popup-edge`, `--popup-offset` (≥0; negative → exit 2), `--lang`, `-h` | ✅ | `args::tests::*` (exact Python messages, prefixes, `--x=v`, usage wrapping), private_bus launcher test |
| args validated before registering; invalid → no activation | ✅ | `args::tests`, ui_headless `single_instance_with_real_binary` |
| `--selftest` (system bus, GetManagedObjects or introspect fallback, adapters, 31 catalogs; PASS/FAIL lines, exit 0/1) | ✅ | private_bus `selftest_binary` (fallback path ☑) |
| `--gen-pot [file]`, `--gen-mo [dir]` | ✅ | byte-identical to Python (diff -r of both outputs), private_bus launcher test, `i18n::tests` |
| Embedded page for Global Settings (`App(embedded=True, host_window, backend_factory)`) | ❌ | Not ported: Global Settings keeps importing the Python module; the shim loads `btmanager_gtk.py` on import |
| Process-scan in install_gtk4.py (`argv[1] == …/btmanager.py`) | ⚠ | Rust process argv is `btmanager.py --popup …` (argv0 from shim). install_gtk4.py's list must also match the binary |

## §1 Single instance
| Item | | Test |
|---|---|---|
| `org.btmanager.Bluetooth`, HANDLES_COMMAND_LINE, prgname = app id | ✅ | ui_headless single instance |
| full argv forwarded; primary re-parses, updates popup options, `set_position`, activate | ✅ | ui_headless single instance (secondary exits 0, primary persists) |
| `--lang` honoured only before first activation | ☑ | `app::BluetoothApplication::handle_arguments` |
| popup relaunch → `PopupController.activate()` (toggle / raise owned child) | ✅ | ui_headless popup (activate with child visible) |
| window relaunch → unminimize+present+`wlrctl` raise (≤4 retries) | ☑ | suite_gtk `raise_window` |
| closing allows a fresh launch | ☑ | `existing = controller && !closing` |
| `do_local_command_line → (False, argv, 0)` | ⚠ | default GApplication local handling (unknown options pass through with HANDLES_COMMAND_LINE); only `--gapplication-*` would differ |

## §2 D-Bus
| Item | | Test |
|---|---|---|
| OBEX agent `/org/btmanager/obex/agent`, `org.bluez.obex.Agent1` Release/AuthorizePush/Cancel, Rejected "Not Authorized", UnknownMethod; foreign senders rejected | ✅ | transfer_tests receive tests, private_bus (real foreign client rejected) |
| BlueZ GetManagedObjects (8 s) + InterfacesAdded/Removed | ✅ | backend_tests `object_manager_*`, private_bus |
| Introspection fallback, 2000 ms poll, never overlapping, failed replies never remove objects | ✅ | backend_tests `poll_*`, `failed_*`, `watched_objects_are_not_refetched_by_polling`; manager_tests `startup_polls_without_object_manager` |
| PropertiesChanged per object; Adapter1/Device1/Battery1 fields; other interfaces ignored | ✅ | backend_tests battery tests, manager_tests `connection_signals_*` |
| Set Powered/Trusted (15 s), Start/StopDiscovery, Pair (60 s)→Connect, Connect, Disconnect, RemoveDevice (20 s) → Unpair fallback (20 s), default 30 s | ✅ | manager_tests (timeouts asserted), private_bus |
| OBEX client: CreateSession(dest,{Target opp, Source}), SendFile, RemoveSession, signals PropertiesChanged/InterfacesRemoved/NameOwnerChanged, **private** session connection closed after cleanup | ✅ | transfer_tests (all of test_transfer.py), private_bus real push |
| AgentManager1 Register/Unregister, GetNameOwner, re-register after obexd restart | ✅ | transfer_tests, private_bus |
| Notifications: GetCapabilities, Notify(susssasa{sv}i) hints urgency/desktop-entry/value, CloseNotification, ActionInvoked/NotificationClosed | ✅ | transfer_tests `notifier_*` |
| FileManager1.ShowItems → fallback launch_default_for_uri | ☑ | `Manager::open_received` (hook tested) |
| Waydock GetPopupPlacement | ✅ | via suite_gtk PopupController/placement (shared tests) |
| Pairing agent | — | owned by authentication-manager (as in Python); nothing duplicated |

## §3 Files
| Item | | Test |
|---|---|---|
| `$XDG_CONFIG_HOME/btmanager/settings.ini` `[receive] mode = off\|ask` (default ask), atomic `.tmp`+rename, other sections kept (configparser format) | ✅ | `settings::tests` |
| primary monitors the file (WATCH_MOVES) and starts/stops the agent | ✅ | manager_tests `primary_follows_receive_policy` (monitor itself ☑) |
| Downloads (XDG, fallback ~/Downloads); O_EXCL 0600 reservation; unique sanitized `name (N).ext`; partial deleted on failure | ✅ | transfer_tests accept/failed tests, manager_tests `unsaveable_download_reports_failure` |
| note `btmanager-note-*/note.vnt` 0600 in 0700 dir, exact vNote, removed on every outcome | ✅ | ui_headless `advanced_menu_info_and_transfers` |
| language: shared language.json (no per-app override) | ✅ | Ui uses `selected_language()` |
| own catalogs from translations.py (not shared/locale) | ✅ | embedded `locale/*.json` generated from translations.py; `i18n::tests`, selftest |

## §4 UI
| Item | | Test |
|---|---|---|
| Popup (layer shell, backdrops, Escape/close/outside click/monitor change dismiss, show generation guard) | ✅ | suite_gtk PopupController (btpopup.py ≡ tray_popup.py), ui_headless popup |
| child windows suspend the popup; not transient in popup mode | ✅ | ui_headless popup |
| CSS classes / STYLE_CSS verbatim; `btmanager` + `tray-popup`; companion theme attached to every window | ✅ | ui_headless `css_parses`, screenshot identical to Python (measure run) |
| Connected / Known / Nearby (add window) lists, selected adapter only, casefold sort, counts, placeholders reused | ✅ | manager_tests grouping/sort, ui_headless |
| row reuse while unchanged (signature) | ✅ | ui_headless (same row widget after unrelated refresh), manager_tests |
| battery & device icon fallbacks | ✅/☑ | battery label tested; icon resolution ☑ |
| Advanced menu Info / Send File / Send Note, Object Push gating | ✅ | ui_headless, manager_tests |
| receive prompt: notification with Accept/Decline (urgency 2, timeout 0) else dialog; close = decline; throttled progress `value`; File received Open/Show in folder | ✅ | manager_tests receive tests, ui_headless dialog fallback |

## §6 Subtle behaviours
All test_backend.py, test_transfer.py and test_ui.py behaviour tests listed in the contract are ported
(names kept, see `backend_tests.rs`, `transfer_tests.rs`, `manager_tests.rs`), including the scan session
state machine cases (switch during start/active/stop, failed old start/stop, removed adapter before refresh,
power-off release, external Discovering, reopen during stop), stale power callbacks, forget via owner,
send revalidation, trust revert. test_popup_routing.py: argument handling ✅; anchor clearing is suite_gtk's
(`set_position`) ✅.

## Deviations / gaps
- ❌ Embedded Global Settings page (Python keeps serving it through the shim import).
- ⚠ Python 3.14 argparse colours help when stdout is a TTY; Rust prints plain text.
- ⚠ Info dialog `str()` of exotic property types (e.g. ManufacturerData) is a Python-like rendering, not
  guaranteed byte-identical.
- ⚠ A `Class` property that is not an integer is treated as 0 (BlueZ always sends `u`).
- Idle-time `Gtk-WARNING … slider reported min width -2` appears identically with the Python app (STYLE_CSS).

## Measurements (headless labwc, fake BlueZ/obexd on private buses, `--popup`)
| | Rust (release) | Python |
|---|---|---|
| binary size | 2.73 MB | — |
| time to D-Bus ready | 14 ms | 195 ms |
| second launch (forward + exit) | 28 ms | 103 ms |
| idle RSS / PSS (popup shown) | 69.9 MB / 40.3 MB | 103.0 MB / 65.8 MB |
