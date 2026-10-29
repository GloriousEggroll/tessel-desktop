# PARITY-B — W-c: D-Bus (settings API, SNI, dbusmenu, readiness) + status sources

Owner: sub-agent B. Files: `src/dbus/{mod,settings_api,sni,dbusmenu,readiness}.rs`,
`src/status/{mod,backend,process,capture,html,html_entities}.rs` (+ `status/backend/tests.rs`),
`tests/b_private_bus.rs` (harness=false, gio::TestDBus also exported as the system bus),
`tests/b_python_parity.rs` (bytes vs Python run with `python3 -I`).

Run: `cargo test -p waydock --lib dbus status` and
`cargo test -p waydock --test b_private_bus --test b_python_parity`.

## Contract §2 — owned D-Bus

| Item | Done | Test |
|---|---|---|
| 2.2 `org.waydock.Settings` XML, `/org/waydock/Settings`, error domain `org.waydock.Settings.Error` | yes | b_private_bus `settings_read_apply_preview_and_disconnect_restore` |
| `Read` JSON `{config,tray_apps,monitors}` (Python `json.dumps` default separators) | yes | same |
| `Apply`: keys equal, ⊆ SETTINGS_FIELDS, Python-equality conflict check, `DockConfig::from_value`, rebuild unless only APPEARANCE/BORDER, ends caller's preview, exact texts | yes | `dbus::settings_api::tests::*` (all 9 test_settings_api SettingsPatchTests) + bus test (rebuild list `[F×7, T]`) |
| `Preview` validation (required/optional keys, int ranges, bools, border normalised via from_value), owner = sender | yes | `preview_validation`, bus test |
| `Restore` only by owner; NameOwnerChanged auto-restore; restore on close | yes | bus test |
| `GetWorkareas` compact JSON, `workarea_snapshot` clamps | yes | `all_edges_are_in_global_coordinates...`, b_python_parity |
| `GetPopupPlacement` (unknown group error, app-dock routing, layout `or` default, offset clamp 12..4096) | yes | bus test (no dock → 12/end, bar dock → 54) |
| `GetTrayApps` | yes (host supplies `tray_entries`) | bus test |
| `WorkareasChanged` idle-coalesced, only on change, cancelled by close | yes | `workareas_coalesce_and_emit_only_on_change` |
| Field sets (59 patchable = every field but `pins`) | yes | `field_sets_match_python` |
| 2.3 SNI watcher object always registered, name `BusNameOwnerFlags::NONE` (queued, never replaces) | yes | `sni_foreign_watcher_is_never_replaced...` (fake foreign watcher, GetNameOwner unchanged) |
| Watcher methods/props/signals, `PropertiesChanged` for RegisteredStatusNotifierItems | yes | `sni_discovery_actions...` (GetAll on the watcher) |
| parse_service forms, ≤1024 chars, exact errors (`InvalidArgs`) | yes | `dbus::sni::tests::service_forms...`, b_python_parity |
| External watcher: RegisterStatusNotifierHost + Get items, host_registered only after both, generation guard, signals only from owner | yes | `sni_rejected_host_registration...`, `sni_foreign_watcher...` |
| Hand-off keeps items; owner exit removes items | yes | `sni_existing_watcher_is_not_replaced_and_handoff_keeps_legacy_items` |

## Contract §3 — consumed

| Item | Done | Test |
|---|---|---|
| SNI item GetAll; icon signals/properties bump `icon_serial` (forces reload), others only refresh; coalesced GetAll | yes | `icon_signals_force_same_filename_reload...`, `icon_signature_ignores_metadata...`, bus test |
| Activate/SecondaryActivate/ContextMenu `(ii)`, Scroll `(is)`, failure callback | yes | bus test (exact argument tuples) |
| ARGB32 → RGBA, smallest ≥ size, dims ≤ 1024 | yes | `network_order_alpha_and_best_pixmap...`, b_python_parity |
| Tooltip HTML stripped (`<br>`/`</p>` → newline, full HTML5 `html.unescape`) | yes | `tooltips_are_plain_readable_text`, b_python_parity |
| `com.canonical.dbusmenu` GetLayout/AboutToShow/Event, LayoutUpdated/ItemsPropertiesUpdated, gio::Menu model (checkmark/radio/disabled/hidden/separator sections/submenu+submenu-action/icon-name/icon-data/accel), shortcuts list | yes (model side) | `dbusmenu_check_radio_disabled_hidden_submenus_and_live_updates`, `dbusmenu_shortcuts_clicks_and_retired_callbacks` |
| `org.waydock.Capture` GetState `(sb)` NO_AUTO_START 2000 ms, Changed, vanish → idle, serial guard | yes | `capture_status_follows_owner_signal_and_vanish` |
| KeepAwake via `suite_core::service_settings::SettingsClient` | yes | `keep_awake_changes_do_not_spawn...`, `status_backend_real_services_on_private_bus` |
| BlueZ GetManagedObjects every 5 s | yes | `bluetooth_multi_adapter...`, real-services bus test (unavailable on private bus) |
| systemd1 discovery → reuses `network_discovery::backend::DiscoveryBackend` in-process | yes | `discovery_observes_once...`, real-services bus test |
| MPRIS → reuses `media_manager::dock_status::MediaStatus` (wired into status `media`, dict keys as Python) | yes | real-services bus test (`Media\nNothing playing`) |

## Contract §7 — status sources (status_backend.py)

All of `ICONS`/`STREAMS` order, exact argv (`/usr/bin/python3 -u <scripts>/<path>`, weather `--watch`,
wpctl get/set-volume `-l 1.0 … N%±`), coalesced `changed` (0 ms scheduler timer), dedup, `_failed`
error reporting once per distinct error, stream restart 3 s, one-shot interval / `min(interval, 60 s)`,
capslock hide-empty, Pango → plain text, screen_record `state` + `visible` (= `/usr/local/bin/waycapture`
is a file), discovery `visible = installed or not ready`, show desktop, audio scroll ±20 coalesced +
serialized, close cancels everything. ProcessJob: 65536 line limit, unterminated final line, timeout
force-exit, stream deadline cancelled on first line, `status command failed`. All 18 Python tests ported
(`status::backend::tests`, 24 tests incl. 5 real-child ProcessJob tests via `python3 -I -c`).

## §10 parity items 1, 3, 4, 5
1 → settings_api tests + bus test. 3 → sni tests + bus tests. 4 → `dbus::readiness::tests` (path socket,
once, no socket, abstract `@` socket, delivery failure → Err and not announced) + readiness gated on a
rejected host in `sni_rejected_host_registration...`. 5 → status backend tests.

## Honest gaps / differences
- JSON decode error texts: Python `json.JSONDecodeError` messages are reproduced for "Expecting value"
  and "Extra data"; other malformed-JSON shapes use serde_json's text (only visible in a tooltip
  "Power: <message>").
- `UnicodeDecodeError` text is approximated from `Utf8Error` (start/continuation/end-of-data cases).
- dbusmenu: GTK side (PopoverMenu, ShortcutController, `closed` → menu_state) is the UI agent's; this
  module exposes `model()`, `actions()`, `shortcuts()` (accel string — parse with
  `gtk::ShortcutTrigger::parse_string`, skip `None`), `set_on_render`, `set_on_clicked`, `popup_opened()`,
  `activate_shortcut(id, mapped)`.
- Tray icon loading (`_pixbuf`: IconThemePath private themes, absolute files, overlay compositing) is
  widget-side; backend provides `TrayItem::pixmap(prefix, size)` and `icon_signature(size, theme_serial)`
  (the exact `_update_icon` cache key) so the UI can port test_tray_refresh.
- test_tray_coordinates is GUI (screen coordinates come from the widget); backend just forwards `(x, y)`.
- Python's `item_filter` default (`Status != "Passive"`) is `TrayItem::is_passive()`.
- `tray_entries` lives in agent A's `core::tray_prefs` (not present when this was written): the host
  passes its result through `SettingsHost::tray_apps()`.

## Integrator wiring
- App: `SettingsApi::new(&app.dbus_connection(), Rc<impl SettingsHost>)` in startup; call
  `queue_workareas_changed()` once after startup and on every bar allocation/exclusive-zone/monitor change;
  `close()` on shutdown.
- `StatusTrayBackend::new(Some(notify_ready), None).start()`; `ServiceReadiness::update(startup_done,
  Some(&tray))` from that callback and when startup completes (log Err like Python's OSError).
- `StatusBackend::new(panel_changed, Some(backend_error), StatusOptions { on_show_desktop:
  Some(toggle_show_desktop), ..Default::default() }).start()`; `set_show_desktop` from the show-desktop owner.
- UI agent: `register_capture_controls` (panel_ui.py:116-152): app actions `prepare-capture` (param `b`:
  true → popdown record menu; false → keep it, autohide off with unmap/map) and `finish-screenshot`
  (restore autohide) — not implemented here.
