# tessel-brightness — parity with contracts/tessel-brightness.md

Rust crate `components/brightness`: binaries `tessel-brightness`
(= `brightness_manager.py`) and `tessel-brightness-status` (= `brightness_status.py`, gio only, no GTK).
Tests: `cargo test -p tessel-brightness` (95 unit tests + `private_bus` end-to-end without a
display), `tests/run_headless.sh` (16 GTK checks + `private_bus` again *with* a display: keyboard
OSD and `--settings`), `tests/measure_headless.sh` (Rust vs Python time/RSS + popup screenshots).
All hardware is fake: shell fakes for `wlr-randr`/`ddcutil`/`swayidle` as the only PATH, a fake
sysfs (`BRIGHTNESS_MANAGER_SYSFS`), a fake way-displays socket, a fake logind on a private bus.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Contract item | | Test |
|---|---|---|
| GApplication `org.tessel.BrightnessManager`, HANDLES_COMMAND_LINE, prgname; IS_LAUNCHER for `--check/--idle-dim/--idle-restore/--brightness-up/--brightness-down` (absent primary ⇒ exit≠0, nothing started) | ✅ | private_bus (launchers before primary) |
| argparse: mutually exclusive modes, abbreviations, `--popup-edge` choices, `--popup-offset` int ≥ 0 with tr("Popup offset must be nonnegative."), usage/help text, exit 2 | ✅ | `args::tests::*`, private_bus |
| `--popup` refreshes hardware first, same position ⇒ activate, new position ⇒ recreate | ☑ | measure_headless (popup screenshot) |
| `--settings` dismisses popup and launches `tessel-displays.desktop` like tessel-dock `AppCatalog.launch` (systemd-run app service + tessel-dock helper); failure ⇒ `Could not open display settings: …` | ✅ (error path) / ☑ (launch) | private_bus display mode; `launch::tests::command_matches_tessel-dock_launcher` |
| `--check` JSON keys/order, exit 0 iff ready | ✅ | private_bus |
| `--brightness-up/down` ±10 through the keyboard controller, exit 0 if accepted | ✅ | private_bus display mode (75→85→75) |
| `--idle-dim/--idle-restore TOKEN` | ✅ | private_bus |
| `--background` re-entry harmless, one hardware+idle controller | ✅ | private_bus |
| SIGTERM/SIGINT: cancel keys/OSD, close idle listener, restore dimmed hardware, then quit (exit 0) | ✅ | private_bus (restores 61, journal `{}`, swayidle gone) |
| ⚠ swayidle callbacks exec the Rust binary (`current_exe`, override `BRIGHTNESS_MANAGER_EXECUTABLE`) instead of `python3 …/brightness_manager.py` | ⚠ | idle_tests |
| ⚠ `--idle-dim/--idle-restore` accept a token starting with `-` (Python argparse rejected 1/64 of tokens) | ⚠ | `args::tests::argparse_compatible` |

## §3 IPC
| Item | | Test |
|---|---|---|
| `org.tessel.BrightnessManager.Controls` at `/org/tessel/BrightnessManager`: GetState, Refresh, SetValue(ssds), SetAutoDim(b), SetSettings(s), SetScale(sds), SetAutoScale(b) → b; `.Error` replies; restarting/unknown messages translated | ✅ | private_bus (SetValue stale id ⇒ "This display is no longer connected.", invalid JSON ⇒ error, invalid settings ⇒ false) |
| `Changed(s)` only when JSON differs, re-entrancy coalesced via idle | ✅ | private_bus (signal seen) |
| State JSON shape and key order (display records, features, merged scale rows `scale:<id>`) | ✅ | private_bus, `scaling_tests::scaling_available_for_unsupported_hardware…` |
| logind `GetSession` + `Session.SetBrightness("backlight", name, u)` as the only backlight writer, revalidated in between | ✅ (fake) | `backend_tests::internal_backlight_preferred_and_logind_is_only_writer` |
| Inhibitor guard: ListInhibitors (block + block-weak `idle`), GetSession(`$XDG_SESSION_ID`/auto) + Session Active/Remote/Type; unknown ⇒ blocked (minimal; no UPower/PPD) | ✅ | `guard::tests::*`, private_bus (fake logind, held reply) |
| Client protocol (`BrightnessClient`): DO_NOT_AUTO_START proxy, owner watch, GetState(+Refresh), 15 s calls, busy for pending settings calls, error fallbacks | ✅ | private_bus (`tessel-brightness-status` follows state and loss of the service) |
| ⚠ The popup uses an in-process client with the same reply semantics (no D-Bus self proxy) | ⚠ | ui_headless (fake client), private_bus |
| way-displays IPC: socket path rules, `OP:` + YAML `CFG`, read to EOF (5 s, 16 MiB), DONE/RC/MESSAGES checks; SetScale/SetAutoScale(false) sequences; CFG_WRITE failure text; GET poll every 5 s | ✅ | `scaling_tests::*` (incl. real Unix-socket exchange), private_bus (fake daemon) |

## §4 Files
| Item | | Test |
|---|---|---|
| `settings.json` exact keys/types/ranges, ≤128 KiB, invalid ⇒ defaults + error, atomic `.brightness-*` + fsync + dir fsync, `indent=2` format | ✅ | `core_tests::invalid_settings_*`, private_bus (file text) |
| `restore.json` journal validation, durable before dim write, emptied after restore | ✅ | `core_tests::*`, private_bus |
| sysfs DRM/EDID/i2c/ddc and backlight ranking | ✅ | `backend_tests::*` |
| tessel-dock `palette.json` keyboard fallback (≤256 KiB, version 1) | ✅ | `keys_tests::optional_palette_fallback…` |

## §5 Auto-dim / §6 hardware / §7 keys
All test_brightness_idle / test_brightness_backend / test_brightness_core / test_brightness_keys
cases are ported (`idle_tests`, `backend_tests`, `core_tests`, `keys_tests`), plus: child poll every
1 s, timeout + "Could not run" subprocess errors, `LC_ALL=C`. Hotplug: GDK `items-changed`,
`notify::geometry`, `notify::scale-factor` ⇒ one 750 ms settled non-full refresh (`backend_tests::monitor_hotplug…`).
Keyboard OSD: shared `suite_gtk::osd` (namespace `desktop-adjustment-osd`, keyboard mode NONE,
"85%"/"75%") — ui_headless `keyboard_brightness_uses_the_shared_osd_without_focus`.

## §8 UI
| Item | | Test |
|---|---|---|
| Popup: title "Displays", `brightnessmanager` + `tray-popup`, `desktop-applet` layer, header + gear "Display settings", DisplayControls/FeatureRow/ScalingRow/AutoScaleRow/AutoDimRow, status label, CSS classes, 200 ms debounce, drag guards, busy/submitted logic, cancel on hide/dismiss/destroy | ✅ | ui_headless (14 ported cases) |
| CSS (`brightness_style.py`) verbatim, provider per window removed on unrealize, companion theme attach | ✅ | ui_headless `css_parses_without_warnings`, screenshots identical to Python |
| Indicator JSON (tooltip, classes, dedup, BrokenPipe exit) | ✅ | `status::tests`, private_bus |
| ❌ `BrightnessSettingsWindow` (unreachable from the app per contract) and its 4 tests | ❌ | — |
| ❌ Native virtual-pointer drag test (pywayland) — gesture handlers tested via `pressed()/released()` | ❌ | — |

## Other deviations
- GTK is initialised at startup (GDK monitors drive hotplug refreshes); widgets, CSS and the OSD
  are created only when needed. Without a display the service still runs (no hotplug/OSD/popup).
- `BRIGHTNESS_MANAGER_SYSFS` (test-only) re-roots `/sys/class/{drm,backlight}`.
- YAML requests are emitted in block style with double-quoted keys (same document as `safe_dump`).
- `wlrctl` raise path of the old settings window is not ported (dead code in Python).
