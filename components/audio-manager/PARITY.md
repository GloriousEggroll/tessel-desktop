# audio-manager — parity with `~/btmanager/audio-manager` (contracts/audio-manager.md)

Binaries: `audio-manager` (= audio_manager.py) and `audio-manager-loopbacks` (= loopback_service.py).
Tests: `cargo test -p audio-manager` (71 unit tests + private-PipeWire integration
`tests/loopback_integration.rs`), UI: `tests/run_headless.sh` (21 cases, private headless labwc,
in-memory fake backend). Test names below are `module::test` (unit), `integration::x`, `ui::x`.

## §1 Entry points
| Contract item | Status | Test |
|---|---|---|
| Modes `--popup/--settings/--volume-up/--volume-down/--toggle-mute` mutually exclusive; default = settings | done (`cli.rs`) | `cli::defaults_modes_and_values`, `cli::errors_match_python_argparse` |
| `--popup-edge {top,bottom,left,right}` (bottom), `--popup-offset N` (76, negative → error); argparse prefix matching, `--x=v`, messages, exit 2 | done | same |
| Options validated before any audio connection; primary re-parses forwarded argv (errors on the primary's stderr, exit code returned to the launcher) | done | `ui::popup_resident_keeps_backend` (launcher exits 2 locally) |
| All argv forwarded to the primary via `org.gtk.Application.CommandLine`; second launches reuse one backend | done (HANDLES_COMMAND_LINE, no GOptions) | `ui::remote_launch_reuses_backend_and_routes_between_windows`, `ui::key_only_launch_forwarding_and_osd_lifetime` |
| prgname / app_id `org.waybar.AudioManager`; `activate` → settings | done | `ui::remote_launch_…` (name owned on bus) |
| Raise settings with `wlrctl toplevel focus app_id:… title:…`, 4 retries / 100 ms | done (suite_gtk `raise_window`) | — (not observable headless) |
| `_held` for popup (released at shutdown), `_key_held` released when OSD dismisses and keys idle | done | `ui::popup_resident_keeps_backend`, `ui::key_only_…` (exit 1–3 s after last key) |
| Popup with different (edge, offset) is destroyed and recreated | done | `ui::popup_controls_follow_default_and_open_settings` |
| Service `[--config PATH]`, default `${XDG_CONFIG_HOME}/audio-manager/loopbacks.json` | done | `cli::service_arguments_match_python`, `integration::service_restores…` |
| Single instance: `flock(EX|NB)` on `<store>.service.lock` (0600); held → `INFO: The loopback service is already running for this configuration.`, exit 0 | done | `integration::service_restores…` (second instance) |
| SIGINT/SIGTERM quit, children terminated ≤2 s then killed | done | `supervisor::shutdown_cleans_all_owned_children`, `integration::service_restores…` (cleanup after SIGTERM) |
| `LEVEL: message` logging to stderr/journald, logger texts | done (`log!`) | integration log check |
| Missing `pw-loopback` → error, non-zero exit | done (exit 1) | — |
| Unit/drop-in/autostart/install layout | unchanged; installer's job (see report) | — |

## §2 IPC
| Item | Status | Test |
|---|---|---|
| GApplication name/object path, `org.gtk.Application`/`org.gtk.Actions` (GIO provided) | done | `ui::remote_launch_…` |
| Wayland app_id | done | — |
| WirePlumber 0.5: `Wp.init(PIPEWIRE|SPA_TYPES)`, Core props `application.name="Audio Settings"`, icon; ObjectManager interests Node 0x11, Device 0x291, Link 0x11, Metadata 0x10011; mixer-api + default-nodes-api modules | done (`wp.rs`, libwireplumber `dlopen`ed at runtime) | `integration::single_monitor_process_tracks_writes_and_external_changes`, `integration::service_restores…` |
| mixer `scale=1`, `changed(id)`, `get-volume`, `set-volume` a{sv} (`volume`,`mute`,`channelVolumes{"i":{volume}}`) | done | `integration::single_monitor…` (drag, mute, external wpctl), `backend::volume_coalesces…`, `backend::channel_changes_merge…`, `backend::mixer_volume_is_read_in_channel_order` |
| default-nodes `set-default-configured-node-name` (Virtual → Audio/Source) | done | `backend::default_uses_stable_node_name`, `integration::single_monitor…` (real default switch) |
| Metadata `set(stream, "target.object", "Spa:Id", serial)` | done | `backend::stream_move_uses_target_serial…` (real path not exercised: no streams in private graph) |
| Device `set_param` Profile/Route with `save:true` via SpaPodBuilder | done | `backend::routes_and_profiles_build_native_saved_parameters` (FFI pod building not exercised: private graph has no devices) |
| `pw-dump --monitor --no-colors`, one child, batches split at `\n]\n`, incremental merge, `{"id":N,"info":null}` removals, props-only change-mask | done | `graph::*`, `backend::recorded_pw_dump_stream_matches_python_reference` (real capture vs Python output), `backend::chunked_monitor_feed…`, `integration::single_monitor…` (exactly 1 pw-dump) |
| GStreamer `pipewiresrc ! audioconvert ! level ! fakesink`, target-object=serial, client-name, stream props, 60 ms, state changes on 1 worker thread | done (`level.rs`, libgstreamer `dlopen`ed lazily) | `integration::input_level_meter_is_hidden_and_stops`, `level::peak_levels…` |

## §3 Files
| Item | Status | Test |
|---|---|---|
| dir 0700, `loopbacks.json` format/validation (exact keys, canonical UUID, lengths, control chars, ≤64, case-insensitive unique names, ≤256 KiB chars), all English error strings | done | `store::*` |
| `loopbacks.lock` flock around read-modify-write (0600) | done | `store::concurrent_writers_do_not_lose_saved_entries` |
| `.loopbacks-*.json` temp → fsync → rename → fsync dir; failure keeps old, no leftovers | done | `store::atomic_replace_failure_keeps_previous_config` |
| Written `indent=2, ensure_ascii=False` + `\n`, Python key order | done | `store::file_format_matches_python_json_dumps` |
| Directory monitor (WATCH_MOVES), basename filter, 100 ms debounce | done | `store::config_monitor_observes_atomic_replacement_and_deletion` |
| Invalid file: service keeps last valid config, logs once per distinct error; app shows error, never overwrites | done | `service::service_retains_last_valid_config…`, `backend::invalid_external_config_keeps_last_valid_view_then_recovers`, `store::malformed_or_unsupported_config_is_not_overwritten` |

## §4 Data model / PipeWire semantics
All of §4 is implemented in `graph.rs`/`backend.rs` with Python value semantics (`pyjson.rs`:
`int()`/`float()`/`str()`/truthiness/`==`/`in (True,"true",1)`).
| Item | Test |
|---|---|
| media classes, description/icon fallbacks, cbrt volumes, max(channels), defaults by name, target by name then serial, links override, routes by direction/device/profile, hidden internal/meter nodes ("false" not truthy) | `graph::*` (ported fixture), `backend::recorded_pw_dump_stream_matches_python_reference` |
| writes check `object.serial` + bound id ("This audio device changed…"), range 0–1.5 | `backend::stale_snapshot_serial_is_rejected_before_write`, `backend::invalid_volume_channel…`, `backend::queued_volume_cannot_touch_replacement_node` |
| 45 ms coalescing, master drops channels, mute flushes; 0.35 s echo window; 25/20/120 ms timers | `backend::volume_coalesces…`, `backend::master_volume_drops_pending_channels`, `backend::volume_echo_updates_model_without_refreshing_views`, `integration::single_monitor…` (≤3 refreshes per 20-step drag) |
| reconnect timings (5 s startup → 2.5 s, disconnect → 2 s, pw-dump backoff min(10 s, 0.5·2^n), error after 3) | `backend::startup_failure_reports_and_schedules_reconnect`, `backend::disconnect_cancels_connection_timers…`, `backend::connection_loss_discards_queued_actions`, `backend::retired_registry_and_mixer_callbacks…` |
| `target_available` | `model::suspended_is_available_but_unavailable_route_is_not` |

## §5 UI
| Surface | Status | Test |
|---|---|---|
| Volume popup (title "Volume", layer `desktop-applet` via suite_gtk PopupController, header + gear, output combo = set default, 0–150 scale with "100%" mark, mute, Applications/Input sections + input combo, meters only for default input while shown, 360 px stream list, `.popup-status[.error]`, 80 ms writes, stale-drag guards) | done | `ui::popup_*`, `ui::missing_output…`, `ui::pending_popup_drag…`, `ui::input_level_meters_run_only_while_visible`, `ui::popup_device_names…`, `ui::mute_buttons…` |
| Audio Settings 980×740, headerbar, `.navigation` sidebar, crossfade stack, 6 pages, rows kept across refreshes (focus/menus preserved) | done | `ui::five_settings_pages_and_external_state` |
| NodeRow (mute, 70 ms volume, routing/port/default, individual channels + lock `changes-prevent/allow-symbolic`, pending edits, meters), DeviceRow (profile), LoopbackRow states, Add Loopback dialog (460 wide) | done | `ui::volume_mute…`, `ui::stream_routing…`, `ui::channel_lock…`, `ui::virtual_input…`, `ui::pending_settings_edits…`, `ui::disconnected_backend…`, `ui::loopback_*`, `ui::managed_loopbacks…` |
| Volume OSD (suite_gtk AdjustmentOsd: `desktop-adjustment-osd`, keyboard NONE, 1.5 s, not attached to the app) | done | `ui::key_only_launch_forwarding_and_osd_lifetime` |
| CSS (STYLE_CSS, POPUP_CSS byte-identical), classes, colors, theme via `suite_gtk::theme::attach` | done | covered by UI cases (classes asserted) |
| Strings via `tr!` (backend/store/keys strings English) | done | — (31 of the 66 UI strings have no entry in shared/locale; same as Python) |

## §6 External commands
`pw-dump --monitor --no-colors` (1 child), `pw-loopback --name … --capture-props … --playback-props …`
(argv, never shell, exact JSON text/key order), `wlrctl`. Tests: `supervisor::target_pinning…` (byte-exact
JSON), `supervisor::names_remain_literal_argv…`, `integration::builder_never_falls_back…`.
Supervisor (§6): `supervisor::*` (all Python SupervisorTests + spawn-failure + 30 s reset).

## §8 Volume keys
All 12 test_volume_keys.py cases (`volume_keys::*`), test_volume_cli (`cli::*`, `ui::key_only…`),
test_volume_app (`ui::key_only…`, `ui::popup_resident…`).

## Gaps / deliberate differences
- JSON error *texts* in "Cannot read PipeWire devices: …" / store messages come from serde_json, not
  Python's `json` (the fixed English prefixes match). serde_json rejects `NaN`/`Infinity` tokens and lone
  surrogates that Python accepts.
- `str.casefold()` is approximated (lowercase + ß/ſ/ς/ligature folding).
- Python bug kept: after a pw-dump batch fails to parse the monitor is force-exited but never restarted
  until the next reconnect (`backend.rs monitor_data`). Candidate fix for both.
- Malformed shapes Python would raise on (e.g. non-list `channelVolumes`) are treated as empty instead
  of failing the whole graph.
- `Cannot start loopback X: [Errno n] …` omits Python's trailing `: 'path'` filename.
- Global Settings embeds the *Python* `audio_ui` in-process; there is no Rust equivalent of that embedding
  (the Python modules must stay installed).
- Not exercised for real (no hardware allowed): Device Profile/Route `set_param` pod FFI and stream
  `target.object` metadata writes (unit-tested with fakes only); the level meter on a real microphone.
- Icon-fallback resolution test (test_popup_mixer `test_input_icons_fall_back…`) not ported (icon names
  list identical to Python; `ui::*` only checks names).
