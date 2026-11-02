# PARITY-C — W-b Wayland backends (`components/dock/src/wl/`)

Sources: window_backend.py, workspace_backend.py, fullscreen_guard.py, tessel-dock.py
(`toggle_show_desktop`, `changed()`, `--desktop-status`, `focused_fullscreen_state`,
`refresh_fullscreen_state`, `_menu_hotkey_blocked`), status_backend.py `set_show_desktop`.

## Design
- In-process, own `wl_display` per backend (tessel-wayland `Client` + `GlibClient`). The
  helpers' JSON line protocol stays the internal event format: the model renders the exact
  helper lines (`model.render`), and the backends consume them with the Python validation.
  So every Python branch (ids, validation, error texts, session handling) is ported 1:1.
- Startup (connect plus the helpers' 3 roundtrips) runs on a `gio::spawn_blocking` worker. The
  8 s startup timeout aborts it by `shutdown()`-ing the socket. The client then moves to the
  main thread (`SendClient`, documented `unsafe impl Send`: no capture protocols are bound).
- `link::fake::{FakeConnector, FakeLink}` (public) drive backends from a real `Model` with
  mocked protocol events. Other agents can use them in UI tests.

## Contract items
| Item | Status | Tests |
|---|---|---|
| §4/§4.1 window events, ids `"<session>:<native>"`, `[1-9][0-9]{0,19}` | done | `windows::tests::exact_ids_*`, `malformed_ids_*` |
| session bump on (re)connect / loss / close; stale ids rejected | done | `connection_loss_clears_state_invalidates_ids_and_reconnects_with_backoff`, `unavailable_or_stale_actions_never_send_requests` |
| startup timeout 8 s (aborts attempt) | done | `startup_timeout_aborts_the_attempt`, workspaces `startup_timeout_and_not_started_messages` |
| reconnect backoff 1 s ×2 → 15 s, reset on ready | done | `backoff_is_capped_at_fifteen_seconds` |
| connection loss clears state | done | same as above, `fatal_protocol_error_ends_the_session` |
| missing seat refuses activate | done | `missing_seat_refuses_activation` |
| maximized ≠ fullscreen | done | `maximized_state_is_distinct_from_fullscreen_and_resets`, headless |
| output rename / unplug | done | `output_name_replacement_*`, `logical_output_geometry_*`, `output_rename_and_unplug_through_the_model` |
| malformed events fail cleanly ("…invalid…") | done | `invalid_helper_line_fails_cleanly_*`, workspaces `malformed_*` |
| close suppresses callbacks/reconnect | done | `close_suppresses_late_callbacks_and_reconnect` |
| error texts identical | done (English, as in Python, which does not translate them either; no `tr!`) | asserted in tests |
| workspaces: atomic snapshot, validation, duplicate ids, groups, hidden, per-output filter | done | `workspaces::tests::*` (port of test_workspace_backend.py) |
| `sorted_workspaces` coordinates → natural name → group → native id | done | `coordinate_order_*`, `natural_name_matches_python` (runs Python) |
| unsupported compositor → no restart, no on_error | done | `unsupported_compositor_does_not_restart_or_error_spam` |
| §5 fullscreen guard: native exact, worker refresh, cache by focus signature, read-only on hotkey, fail open | done | `fullscreen_guard::tests::*`, `menu_hotkey_reads_cached_fullscreen_state_without_waiting_for_x11` |
| §10.16 X11 probe: borderless + negative-origin outputs, maximized/decorated allowed, remote DISPLAY never contacted, 120 ms deadline, ≤512 clients, rate limit (2 s), native after two client lists | done (pure-Rust x11rb client with a deadline-aware stream) | `borderless_*`, `ordinary_maximized_*`, `probe_has_total_deadline_*`, `stalled_x_server_hits_the_deadline`, `forced_refresh_is_rate_limited`, `native_wayland_window_stops_probing_after_two_client_lists`; headless "Xwayland borderless" (real Xwayland + GTK X11 fixture) |
| §5/§10.13 show desktop: own set + focus, restore before minimize ack, pre-existing minimization and maximized state kept, cleared on close | done | `show_desktop::tests::*`, headless "show desktop" |
| `--desktop-status` JSON bytes | done (pyfmt `json_dumps` DEFAULT) | `desktop_status_bytes_match_python` (runs Python status_backend.indicator) |
| headless integration (test_workspace_native, FullscreenGuardNativeTests, show desktop native) | done | `tests/wl_headless.rs` via `tests/run_wl_headless.sh` (7/7 passing, 3 runs) |

## Honest gaps / differences
- No write queue. Requests go straight to the in-process client (flushed at once), so
  "Too many window requests are pending.", "The window request could not be sent.",
  "The connection to the window helper was interrupted." and "…helper stopped." can no longer
  happen. If the client refuses a request, the action now returns `false` and reports the
  error at once. Python returned `true` and the error arrived later as a helper event.
- `status_backend.indicator()`'s `plain_text` (strip tags + HTML unescape) is not applied to
  the show-desktop tooltip. The translated "Show desktop" / "Restore windows" texts contain no
  markup.
- X11 probe runs in-process (a thread with a socket deadline), not as a `--probe` subprocess.
  32-bit properties are read as unsigned (Xlib sign-extension of `long` is not reproduced; it
  does not matter for atoms, extents or motif flags). Display numbers that do not fit in a
  u16 are refused. No server answers them anyway.
- Python's 65536-byte probe-output limit is reproduced by measuring `json.dumps` length.

## Integrator wiring (W-e/W-f/app)
- `WindowBackend::new(on_changed, on_error)` + `start()`, and the same for `WorkspaceBackend`.
  Both use the thread-default main context. Call `close()` on shutdown.
- In the app's `changed()`: if `show_desktop.windows_changed(&backend)` returns true, then
  `set_show_desktop(false)`. Then `guard.windows_changed()`.
- `FullscreenGuard::new(move || Some(backend.snapshot()))`. `guard.state()` covers hot corners
  and the hotkey. `guard.menu_hotkey_blocked(config.menu_hotkey_fullscreen)`.
- `--toggle-desktop`: `show_desktop.toggle(&backend, close_popups)`. `None` → exit 1.
  `Some(active)` → the status indicator gets `show_desktop::indicator(active)`, exit 0.
- `--desktop-status`: print `show_desktop::desktop_status_line(show_desktop.active())`.
- Cargo.toml additions: `indexmap = "2"`, `x11rb = { version = "0.13", default-features = false }`,
  `[[test]] wl_headless` (harness = false).
