# notification-manager parity (contract → Rust)

Tests: `cargo test -p notification-manager` (unit + `tests/dbus.rs` on a private
dbus-daemon) and `components/notification-manager/tests/run_headless.sh` (UI + daemon
inside a private nested headless labwc with its own dbus-run-session bus). The
headless file is ‹h›, the D-Bus file ‹d›, unit tests are `module::tests::name`.

## 1. Entry points / CLI
| Item | Status | Test |
|---|---|---|
| `notification-manager` = notification_manager.py; `--daemon/--check/--settings/--popup` mutually exclusive, default popup | done (argparse-compatible: `=` values, unique prefixes, usage text, exit 2) | `cli::tests::argparse_compatible_parsing` |
| `--popup-edge` choices, `--popup-offset` int, 0..4096 check only for popup (exit 2) | done | ‹d› `cli_check_settings_and_popup_against_private_bus` |
| `--check` sync GetState (NO_AUTO_START, 3 s) → `{"ready":true,"unread","settings","presentation"}` (`json.dumps` text) | done | ‹d› `cli_check…`, ‹h› `fullscreen_history_…_headless_cli` |
| errors `Notifications: <domain>: <message> (<code>)`, exit 1, never auto-start | done | ‹d› `cli_check…` |
| `--daemon` exit 1 on startup failure (name owned) | done | ‹d› `existing_server_is_never_replaced_or_queued` (service level) |
| GApplication `org.waydock.Notifications`, NON_UNIQUE, no activate window | done | ‹h› daemon run |
| `notification-status` = notification_status.py (JSONL only on change, SIGTERM/SIGINT/EPIPE exit) | done, GLib/Gio only (no GTK linked) | `status::tests::indicator_classes_and_tooltips`, ‹d› `status_stream_reports_service_changes` |
| launcher, .desktop, systemd drop-ins | installer step (parent) – see report | – |

## 2. D-Bus
| Item | Status | Test |
|---|---|---|
| RequestName DO_NOT_QUEUE, ≠1 → "Another notification server is already running"; ReleaseName on close | done (raw RequestName, never replace) | ‹d› `existing_server_is_never_replaced_or_queued` |
| GetCapabilities / GetServerInformation | done | ‹d› `capabilities_identity_and_notify_replacement` |
| Notify (sender = caller unique name), replacement | done | ‹d› same |
| CloseNotification reason 3, InvalidId error | done | ‹d› `close_emits_standard_reason_and_invalid_id_error` |
| NotificationClosed / ActionInvoked unicast to sender | done | ‹d› `closed_and_action_signals_are_unicast_to_the_sender` |
| GetState compact JSON, ensure_ascii=False | done (Python float repr via `pyfmt`) | ‹d› `capabilities_…` |
| SetSettings (>16384 → InvalidArgs, validation → InvalidArgs), ClearHistory, MarkRead, Dismiss, ClearApp (>256 → InvalidArgs), Activate, ShowCenter, ShowCenterAt (bad edge/offset → InvalidArgs), ShowSettings | done | ‹d› `custom_settings_history_commands_and_signal`, `invalid_popup_position_is_invalid_args`, `actions_and_resident_semantics` |
| StateChanged broadcast on every change kind | done | ‹d› `custom_settings…` |
| Client: no auto-start, owner tracking, serial guard, `call` false without owner, 5 s | done (`client::NotificationClient`) | ‹d› `client_*` (3 tests) |
| JSON parse error text of SetSettings | differs (serde_json message instead of Python's `JSONDecodeError` text; same error name) | – |

## 3. Model
| Item | Status | Test |
|---|---|---|
| ids never 0, wrap, skip in-use, persisted next_id | done | `model::tests::replace_keeps_id_…` |
| text limits / NUL stripping, "Application" default | done | `model::tests::*` |
| replacement of active ids, stack tags scoped by app + desktop entry | done | `…stack_tag_replaces_per_application_and_desktop_entry` |
| urgency clamp, timeout −1/0/explicit, critical persistent | done | `…critical_default_persists_…` |
| actions ≤16 pairs, empty/duplicate keys dropped | done | `…action_list_is_bounded_and_deduplicated` |
| image-data/image_data/image-path/icon_data, validation incl. padding-less last row, base64 | done (`ay`, or integer arrays 0..255) | `…raw_images_reject_…`, `…hints_parse_from_variants` |
| progress, transient, resident, x-waydock-output/workspace | done | `…stack_tag…`, ‹h› daemon tests |
| close reasons 1–4, reason 2 marks read, transient deleted, closed before change | done | `…close_is_idempotent_…`, `…actions_only_valid_…` |
| activate / resident | done | `…actions_only_valid_on_live_notifications_resident_preserved` |
| hover pause/resume | done | `…critical_default_persists_explicit_timeout_respected_and_hover_pauses` |
| trim: ≤100 active (reason 4), history_limit, 8 MiB image budget | done | `…active_notifications_bounded_…`, `…history_private_…` |
| clear_history / clear_app single change | done | `…clear_history_…`, `…app_groups_and_clear_app` |
| state() shape/order, unread | done | `…journal_format_matches_python`, ‹d› |
| settings validation (atomic, unknown keys, dnd_until rule), dnd_deadline | done | `…invalid_settings_…`, `…timed_do_not_disturb_…`, `…dnd_deadline_choices`, `…muted_apps_…` |
| next_deadline None when idle | done | `…next_deadline_is_idle_…` |

## 4. Files
| Item | Status | Test |
|---|---|---|
| `$XDG_STATE_HOME/notification-manager/state.json`, dir 0700, file 0600 (mkstemp `.notifications-*` + rename), Python `json.dump` text, transient/images/actions never journaled, storage_error text `[Errno N] …` | done | `…history_private_…`, `…journal_format_matches_python`, `…storage_errors_are_reported_not_fatal` |
| load: >32 MiB / version≠1 / corrupt ignored; partial load stops at a bad entry like Python's `except` | done | `…invalid_settings_rejected_atomically_corrupt_history_…` |
| `~/.config/waydock/config.json` `edge` | done (`placement::dock_edge`) | `placement::tests::workspace_and_output_routing` (pure part) |

## 5. UI
| Item | Status | Test |
|---|---|---|
| Toast stack: layer TOP/OVERLAY, ns `waydock-notifications`, zone 0 (−1 after dock placement), keyboard NONE, TOP+RIGHT margin 12, width rule, ≤3 cards, set_visible never present, workspace align flip | done | ‹h› `toast_does_not_steal_focus_…`, `toasts_bound_visible_count_…`, daemon `critical_bypasses_…` (OVERLAY) |
| card reuse, plain text, progress `N%`, actions FlowBox, `primary` for `default` | done | ‹h› `replacement_reuses_card_…` |
| balanced hover | done | ‹h› `hover_lifecycle_…` |
| history center (PopupController, header + count + settings button, DND + timed buttons, groups collapse/clear, Clear All, empty state) | done | ‹h› `center_*`, `history_groups_…`, `center_and_settings_timed_dnd_options`, `dnd_choices_wrap_…` |
| Escape / click-away | done via suite-gtk PopupController; test drives `dismiss()` and checks the backdrops (no synthetic key/pointer events) | ‹h› `center_escape_clickaway_and_empty_state` |
| Settings window 650×410, headerbar, close hides | done | ‹h› `fullscreen_history_…` |
| SettingsPage (dnd_switch, fullscreen_switch, timeout_spin, dnd_options, status_label, mute switches) | done (Rust page used by the daemon window; Global Settings keeps importing the Python `notification_ui.SettingsPage`) | ‹h› `embedded_settings_sync_…`, `settings_lists_seen_and_muted_apps_…` |
| icon/picture rules, header fallback, local-only bounded images | done | ‹h› `icon_images_…`, `header_uses_…`, `images_are_bounded_…` |
| palette follows monitor (`suite_gtk::theme::attach`) | done | ‹h› `palette_tracks_selected_monitor_and_tears_down` |
| CSS (STYLE) and class names | done (verbatim) | ‹h› |

## 6. Helpers
| Item | Status | Test |
|---|---|---|
| `waydock-toplevel` events (ready/window/closed/output/output-removed/error), 8 s startup timeout, reconnect backoff | done (read-only port; activate/minimize/close requests not needed by this component) | ‹h› all daemon tests, `toast_does_not_steal_focus_…` |
| X11 probe `python3 -I -S <dir>/fullscreen_guard.py --probe`, local DISPLAY only, 120 ms, ≤64 KiB, fail-open; verdict rule; cache (2 s forced reuse, native-window votes) | done (subprocess is async on the main loop, no thread pool) | `policy::tests::*`, ‹h› `borderless_probe_holds_replacement_…` (fake probe script) |

## 7. Presentation policy
| Item | Status | Test |
|---|---|---|
| suppressed / may_present, critical bypass, muted never | done | ‹h› `critical_bypasses_dnd_and_fullscreen_but_muted_apps_never_pop_up` |
| pending→eligible after check, presentation_hold, hide on focus change, no backlog replay, blocked fullscreen destroys center, unknown state = blocked | done | ‹h› `fullscreen_history_no_backlog_…`, `borderless_probe_…` |
| toast monitor selection (focused output / connector / workspace monitor) | done | `placement::tests::workspace_and_output_routing` |
| no polling: one expiry timeout (`ceil(ms)+1`), DND timeout capped 60 s, only 2 signal sources idle; cached focus never probes | done | ‹h› `idle_daemon_schedules_no_wakeups` |
| ShowCenter* toggles + marks read, clears pending; ShowSettings closes center | done | ‹h› `fullscreen_history_…` |

## 8. i18n
All strings go through `suite_core::tr!` with the Python English keys. Note: most
notification strings (e.g. "Do Not Disturb", "You're all caught up") are **not present
in shared/locale/*.json**, so Python and Rust both show English for them today.

## Deliberate differences / gaps
- D-Bus uses GDBus (gio) instead of zbus: same main-thread semantics as Python
  (unicast `emit_signal`, raw `RequestName`), no extra thread, matches suite-core.
- The bus name is owned (interfaces exported) *before* GTK initializes; calls queue
  until the main loop runs. Python owned it after GTK init.
- SetSettings JSON syntax-error message text differs (error name identical).
- `str()` of exotic non-string hint values (e.g. a struct passed as `desktop-entry`)
  uses GVariant text instead of Python's repr.
- `test_install.py` (installer plan/rollback) has no Rust counterpart: installing is the
  parent's `tools/install.py` step.
