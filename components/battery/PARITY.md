# tessel-battery — parity with `~/tessel-bluetooth/tessel-battery` (contract: contracts/tessel-battery.md)

Binaries: `tessel-battery` (= `battery_manager.py`), `tessel-battery-status` (= `battery_status.py`, GLib/GIO only, no GTK).
Tests: `cargo test -p tessel-battery` (unit + `tests/private_bus.rs`, private dbus-daemon) and
`tests/run_headless.sh` (private nested headless labwc; `RELEASE=1` for measurements).

Shared-with-power-manager code is isolated for upstreaming: `src/power/` (no GTK: bus, values, scheduler,
devices, charge_limits, backend) and `src/power_ui/` (style, battery_ui). Same gio `Bus` trait / `FakeBus` /
`Scheduler` shape as `components/power/src/bus.rs`, `util.rs`.

## 1. Entry points & CLI
| Contract item | Status | Test |
|---|---|---|
| GApplication `org.tessel.BatteryManager`, `HANDLES_COMMAND_LINE`, prgname = id | yes (`app.rs`) | ui_headless `application_and_measurements` |
| `IS_LAUNCHER` only for `--check`; `--check` without primary fails (exit 1, "Failed to register") | yes | private_bus `check_without_primary_fails_and_bad_arguments_exit_2` |
| local_command_line never parses (forward argv, Python `False, argv, 0`) | yes | ui_headless (remote `--popup/--settings/--background` exit 0) |
| no flag ⇒ popup bottom/76; `--popup --popup-edge --popup-offset`; same position ⇒ activate, different ⇒ destroy+recreate | yes (`show_popup`) | cli `launcher_routes_modes_and_positions`; ui_headless app run |
| `--settings`: dismiss popup, singleton window, present, idle `wlrctl toplevel focus app_id:… title:…` (single attempt) | yes | ui_headless app run (exit 0, primary survives) |
| `--background` holds service, no window | yes | ui_headless |
| `--check` JSON keys/order `ready,has_battery,on_battery,percentage,batteries,charge_limit,error` + `\n`, Python `json.dumps` separators/escapes, exit 0 iff ready | yes | ui_headless (keys, separators, exit 0) |
| argparse: mutually exclusive modes, prefix abbreviations, `--x=v`, choices, int parsing, `-h` text, exact error lines, exit 2; tr("Popup offset must be nonnegative") | yes (`cli.rs`, messages captured from Python 3.14) | cli `launcher_rejects_ambiguous_modes_and_invalid_position`, `help_matches_argparse`; private_bus |
| SIGTERM/SIGINT ⇒ quit; shutdown closes backend and destroys popup | yes | ui_headless (`SIGTERM quits cleanly`, exit 0) |

## 2–3. IPC
| Contract item | Status | Test |
|---|---|---|
| No custom D-Bus interface owned | yes | — |
| UPower `GetAll` + `EnumerateDevices` + per-device `GetAll`; `OnBattery` must be bool | yes | backend `start_async_and_read_only_battery_subset`, `failed_device_read_never_assumes_ac`, `missing_on_battery_and_stale_reads_are_ignored`; private_bus `upower_backend_follows_signals_and_owner_changes` |
| batteries_only subscriptions: NameOwnerChanged(arg0 UPower), UPower PropertiesChanged, UPower root signals (3) | yes | backend `battery_only_startup_and_refresh_use_only_upower`; private_bus (DeviceAdded/Removed, owner lost/back, property changes) |
| 100 ms coalesced re-read, 120 s fallback refresh, reconnect 5 s after connect failure / 2 s after bus close | yes | backend `start_async…` (120000), `connect_failure_reports_and_retries`, `bus_close_invalidates_callbacks_and_schedules_reconnect` |
| No "uncertain source" for charge/peripheral updates; uncertain for OnBattery change, supply identity, UPS state | yes | backend `charge_and_peripheral_signals_do_not_reset_idle_policy`, `supply_identity_and_ups_state_changes_are_uncertain`, `ups_power_source_and_hotplug_refresh` |
| `EnableChargeThreshold(b)`: revalidate identity → interactive mutation (120 s, uncancelled) → read-back; refused for KAuth-managed batteries | yes | backend `charge_limit_*` (7 tests), `refresh_reads_actual_values_without_any_helper_calls`; private_bus (real wire, interactive flag) |
| KAuth `performAction(saya{sv}aya{sh})`, QDataStream QVariantMap encode, strict reply decode, types 4/5/6 messages | yes | charge_limits `arguments_match_qt6_qdatastream`, `success_denial_busy_and_truncated_replies`, `set_parameters_…`; private_bus `kauth_end_to_end_against_fixture_sysfs` (fake helper, fixture sysfs in TMPDIR), `kauth_denial_maps_to_message` |
| `set_charge_limit`: validation, no-op when equal, getthreshold → identity check → setthreshold (start lowering) → re-inspect & confirm | yes | backend `apply_revalidates_…`, `cancel_and_unapplied_…`, `hotplug_before_authorization_…`, `validation_unchanged_and_overlap_…`, `read_transport_failure_and_close_…`, `unsupported_or_unready_limits_fail_without_calls` |
| Remote auth errors ⇒ "Authorization was canceled or denied."; other remote errors stripped | yes | backend `charge_limit_denial_…`; private_bus |
| Bluetooth shortcut: `systemd-run --user --collect --quiet --service-type=exec --slice=app.slice --expand-environment=no [--setenv=…] -- python3 <script>`; no scope/wait; spaced path one argv; three error strings | yes (`bluetooth.rs`, `app.rs`) | bluetooth `bluetooth_launch_is_independent_and_preserves_spaced_path`, `script_lookup_prefers_package_directory` |
| Bluetooth button only when script exists and systemd-run on PATH | yes | ui_headless `bluetooth_shortcut_is_gated_…` |

## 4. Files
| Contract item | Status | Test |
|---|---|---|
| sysfs read-only; threshold valid only 0–100 and S_IWUSR; scope/present/start-only/unmapped fail closed; identity (resolved path, manufacturer, model, serial) | yes | charge_limits `present_system_batteries_…`, `peripherals_absent_read_only_…`, `scope_presence_and_garbage_fail_closed`, `start_only_companion_…` |
| Three KDE helper files required | yes | `missing_helper_does_not_claim_adjustable` |
| `~/.config/tessel/language.json` via suite_core i18n | yes | — |
| Never writes sysfs / never reapplies at startup | yes (only fixture dirs are written, by the fake helper in tests) | backend `refresh_reads_actual_values_without_any_helper_calls`, private_bus (`reads never activate the helper`) |

## 5. Data model
| Contract item | Status | Test |
|---|---|---|
| `batteries[]` normalization/sort/skips, coarse levels, sentinels, ChargeThresholdSettingsSupported bits, bluetooth transport | yes (`devices.rs`, same dict keys/order) | devices `devices_filter_…`, `coarse_unknown_…`, `unknown_health_…`, `firmware_charge_control_…`, `bluetooth_requires_…`, `names_kinds_and_sort_order` |
| `percentage` energy-weighted else mean, clamped; `on_battery` incl. UPS; `warning_level` from system batteries | yes | devices `invalid_battery_percentages_…`; backend `batteries_ignore_peripherals_and_weight_capacity`, `warning_level_…`, `desktop_peripherals_…` |
| `charge_limit` dict + verbatim reasons | yes | charge_limits tests |

## 6. UI
| Contract item | Status | Test |
|---|---|---|
| Popup: title, `powermanager batterymanager tray-popup`, `popup-surface`, header + gear (dismiss, open settings), source status/icon, `.status` | yes | ui_headless `live_power_source_…`, `battery_gear_dismisses_popup_…` |
| Settings window 550×560 (min 470×360), headerbar, `.title/.subtitle/.card`, Close, Escape | yes | ui_headless (settings cases) |
| BatterySection: row reuse, hotplug removal, empty texts, 224 px scroll, gauge layout | yes | ui_headless `hotplug_removal_…`, `device_battery_refresh_…`, `full_battery_gauge_…`, `coarse_and_unknown_…` |
| Battery protection switch (UPower) gating/rejection | yes | ui_headless `battery_protection_requires_support_…`, `numeric_charge_limits_hide_preset_protection_…` |
| Maximum charge SpinButton 1..100 (default 80) + Apply, pending/readback/denial/edit-back/capability-loss rules | yes | ui_headless 5 `charge_limit_*` cases |
| Details (kind, health, cycles, charge limit), time strings | yes | ui_headless `battery_details_show_health_and_cycles`; battery_ui `time_and_level_labels`, `status_labels_…` |
| Status message friendly + dedup + "Updating battery settings…" | yes | ui_headless `status_messages_are_friendly_and_deduplicated` |
| Power style CSS (verbatim), icon fallbacks, companion theme attach | yes | ui_headless `style_css_parses` |
| Indicator JSON (glyphs, classes, tooltip lines), printed only when changed, exit on closed pipe/SIGTERM | yes | status (6 tests incl. byte-exact `json.dumps`); private_bus `status_stream_prints_changes_only_and_exits_cleanly` |

## 8. i18n
All UI strings through `suite_core::tr!`; device kind/state/level/warning translated at render time. Backend
error/reason strings are shown untranslated, exactly like Python (no catalog entries exist for them).

## Gaps / differences
- Python type-confusion cases (`percentage="72"`/`False`, `set_charge_thresholds(path, 1)`) are unrepresentable in
  the typed API; `set_charge_limit` still takes a JSON value so its type checks are ported and tested.
- Global Settings imports the *Python* `battery_ui.BatterySettingsWindow` (+ tessel-power's `power_backend`); a Rust
  window cannot be embedded there, so those Python files must stay installed until Global Settings is ported.
- `install_waybar.py` (legacy Waybar edit, manifest, backups, health rollback, SIGUSR2) is not ported: installer work.
- `friendly_status` warnings log under domain `tray-popup` (Python logger `tessel-battery`).
- Not tested against real UPower/KAuth/polkit (by design); the KAuth protocol is covered with a fake helper.
- D-Bus uses gio (like tessel-power's port), not zbus: same GLib main loop, per-call timeouts/cancellables and
  `ALLOW_INTERACTIVE_AUTHORIZATION`, no executor thread.

## Measurements (private headless labwc + private bus with fake UPower, release, same conditions)
| | Rust | Python |
|---|---|---|
| binary size | tessel-battery 2.30 MB, tessel-battery-status 1.91 MB | — |
| service `--background`: time to first ready `--check` | 160–270 ms | 490–570 ms |
| service idle RSS (anon) | 46 MB (10 MB) | 70 MB (26 MB) |
| service after popup + settings (anon) | 61 MB (13 MB) | 86 MB (30 MB) |
| indicator stream RSS (anon) | 8 MB (0.8 MB) | 32 MB (15 MB) |
| indicator: first device line | 8–25 ms | 78–87 ms |
