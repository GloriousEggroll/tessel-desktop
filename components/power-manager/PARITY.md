# power-manager — parity with contracts/power-manager.md

Rust crate `components/power-manager`: binaries `power-manager` (= `power_manager.py`)
and `power-status` (= `power_status.py`), example `fake_power_services` (measurement
fakes). Tests: `cargo test -p power-manager` (unit + private-bus end-to-end),
`tests/run_headless.sh` (GTK checks in a private nested headless labwc),
`tests/measure_headless.sh` (Rust vs Python timing/RSS + screenshots, private labwc).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Contract item | | Test |
|---|---|---|
| GApplication `org.waybar.PowerManager`, `HANDLES_COMMAND_LINE`, `IS_LAUNCHER` only for `--check/--idle-action/--idle-resume/--power-button`; prgname = app id | ✅ | private_bus (`--check` without primary fails, launchers forward) |
| All argv parsed in the primary; local pre-parse for errors (exit 2, argparse text, abbreviations, mutual exclusion, choices, `--popup-offset` ≥ 0, "Idle events require --policy-token" translated) | ✅ | `args::tests::*`, private_bus |
| `--popup` same position → activate, different → recreate; `--settings` dismisses popup, raises via `wlrctl` ×4/100 ms | ☑ | measure_headless (popup + settings screenshots, pixel-identical to Python) |
| `--check` JSON keys/order, exit 0 iff ready | ✅ | `app_tests::health_report_keys_match_python`, private_bus |
| SIGTERM/SIGINT quit; shutdown closes idle child, alerts, logind FDs, dim, backend, popup | ✅ | private_bus (clean exit, swayidle terminated) |
| `power-status` stream: JSON line on change, dedup, ``, ensure_ascii, exits on SIGTERM/SIGINT/EPIPE | ✅ | `status::tests::*`, private_bus |

## §2 IPC
| Item | | Test |
|---|---|---|
| `org.waybar.PowerManager.Controls`: `GetState` (`ready` override keeps position, `settings` last), `SetSettings(s)→(bs)`, non-object → `.Error "Invalid power settings."`, `Changed(s)` | ✅ | `app_tests::settings_editor_*`, `remote_save_rejects_non_objects`, private_bus |
| PPD (UPower.PowerProfiles + `net.hadess` fallback), GetAll, PropertiesChanged debounce 100 ms, interactive non-cancellable `Properties.Set` | ✅ | backend_tests (profile_*, subsystems_*, stale_*, service_loss_*) |
| UPower GetAll/EnumerateDevices/per-device GetAll, PropertiesChanged any path, DeviceAdded/Removed, UPS discharging | ✅ | backend_tests (batteries_*, ups_*, charge_and_peripheral_*, failed_device_read_*) |
| logind Can*, ListInhibitors (block + block-weak), GetSession(`$XDG_SESSION_ID`/auto) + Session GetAll, Suspend/Hibernate/SuspendThenHibernate/PowerOff(b), CanPowerOff, PrepareForSleep(false) → refresh, NameOwnerChanged | ✅ | backend_tests (sleep_*, idle_guard_*, explicit_*, inactive_*), private_bus |
| Reconnect 5 s / 2 s after close, fallback refresh 30 s (120 s batteries-only), read timeout 10 s, mutations 120 s | ✅ | `bus_close_invalidates_*`, `start_async_and_read_only` (9 subscriptions) |
| `power_logind`: Inhibit handle-lid-switch / handle-power-key (`block`), LidClosed edge + Docked, LidIsPresent, rc.xml regex + file monitor, late grant released | ✅ | `logind::tests::*`, private_bus (`lid_handled`, lid → suspend) |
| KDE KAuth charge helper (QDataStream encode/decode, get/set threshold, revalidation) | ✅ (backend only) | `charge_limits::tests::*`, backend_tests (charge_limit_*, apply_*, hotplug_*) |
| Notifications `Notify(susssasa{sv}i)` hints/expire, `CloseNotification`, `ActionInvoked` | ✅ | `alerts::tests::notification_hints_match_python`, private_bus |
| swayidle child protocol (§2.3): argv, shlex quoting, token_urlsafe(24), compare_digest, respawn only on signature change, dim delay, sleep only for `yes`, deps/locker checks, SIGTERM then SIGKILL after 1 s, child watch, exit error text | ✅ | idle_tests (all 13 Python cases) + private_bus |

## §3 Files
settings.json path/schema/validation/messages, 64 KiB limit, missing → defaults (not written), invalid → error + idle not started, atomic mkstemp `.power-settings-*` 0600 + fsync + rename + dir fsync, `indent=2, ensure_ascii=False` + `\n` byte-identical: ✅ `config::tests::*`, private_bus. `save()` "newly changed unsupported sleep" rule: ✅ `app_tests::unavailable_*`, `display_edit_preserves_*`. sysfs charge limits + helper-file gating: ✅.

## §4 Behaviour
Idle action/resume (token, generation, inhibitor + session guard, re-check), dim always restores, display wake serialized after in-flight display command, command de-dup, lid/power button (1.5 s debounce, menu via `systemd-run … session_manager.py --popup`), `_explicit` lock-before-sleep with 15 s timeout (failed lock cancels lid/button sleep, not critical battery), battery alerts (10 %/5 %/WarningLevel, once per cycle, 60 s countdown, cancel, plug-in reset, auto→hibernate|poweroff), dim overlay (namespace `power-manager-dim`, OVERLAY, all anchors, exclusive −1, keyboard NONE, empty input region, black 0.55), Bluetooth shortcut: ✅ `app_tests::*` (22), `alerts::tests::*`, ui_headless (dim overlay).

## §5 UI
Popup (title "Power", header icons, gear, "Power mode", ProfileSelector `profile-row`/`selected`/`profile-check`, status, "Updating power settings…"), Settings window (580×760, min 500×460, all cards, local edits until Save, Escape/Close discard, duration choices + custom value, sleep-mode suffixes untranslated and rows disabled unless `yes`, hints, "On battery" literal), CSS classes and `STYLE_CSS` verbatim, icon fallbacks, friendly_message substring table: ✅ ui_headless (14 checks = all PowerUITests) and measure_headless screenshots (popup and settings PNGs are byte-identical to the Python ones).

## §7 i18n
All strings go through `suite_core::tr!` with the Python English keys; untranslated literals kept. ☑ (catalog lookup tested in suite-core).

## Deviations (deliberate) ⚠
1. **D-Bus via gio, not zbus.** suite-core's SettingsService/Client and GApplication command-line forwarding are gio; gio keeps every call on the GLib loop with no executor and no extra binary size. Behind a `Bus` trait (fake used in unit tests).
2. **Lazy GTK.** The primary is a `gio::Application`; GTK initializes only when a popup/settings window or the dim step is first needed (Python imported/initialized GTK at startup). Windows have no `GtkApplication`; the Wayland app_id still comes from prgname `org.waybar.PowerManager`. The `org.gtk.Actions` export of GtkApplication is gone (nothing uses it).
3. **swayidle callbacks exec the Rust binary directly** (`<exe> --idle-action … --policy-token T`) instead of `<python> power_manager.py …` (no Python start per idle event). Override with `POWER_MANAGER_EXECUTABLE`.
4. **`--policy-token` accepts a value starting with `-`.** Python argparse rejected such tokens (~1/64 of `token_urlsafe` outputs), silently dropping that generation's idle actions.
5. Argparse errors from a forwarded command line go to the caller's stderr (Python printed them in the primary).
6. Status stream uses a profiles-only backend (same output, no UPower/logind traffic).
7. JSON-decode error text inside "Could not load power settings: …" is serde_json's wording, not Python's.
8. `POWER_MANAGER_SCRIPTS_DIR` (default `~/.config/waybar/scripts`) replaces `Path(__file__).parent.parent` for `session-manager/session_manager.py` and `btmanager` lookup; the Python interpreter used for those launches is `python3` from PATH.

## Gaps ❌
- `power_battery_ui.py` (BatterySection) and the shared Python libraries (`power_backend.py`, `power_config.py`, `power_client.py`, `power_ui.py`, `power_style.py`, `power_charge_limits.py`) are not replaced: Global Settings, lock-screen, battery- and brightness-manager import them, so they must stay installed (contract §12.1). The Rust backend already implements the charge-limit mutations for a future Rust battery-manager.
- Installer pieces (§8: units, .desktop, Waybar module/CSS migration, `--check` poll + rollback) are not part of this crate.
