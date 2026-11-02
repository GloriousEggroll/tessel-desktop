# home-assistant-screen — parity with contracts/home-assistant-screen.md

Crate `components/home-assistant-screen` (lib `kasumi_screen`): binaries `luna-clock`
(= `luna_clock.py`) and `voice-controls` (= `voice_controls.py`). The dashboard widgets are
`crates/locking` (lock_dashboard/lock_info/lock_visuals/lock_weather/lock_media*/voice_state,
see `crates/locking/WIDGETS_READY`), the keyboard is Tessel Keyboard' library
(`components/keyboard`), the admin panel uses a minimal WebKitGTK 6.0 binding
(`src/webkit.rs`) that `dlopen`s the runtime-only `libwebkitgtk-6.0.so.4` on the first panel open
(the `webkit6` 0.6 crate is compatible and was tried first, but linking it eagerly cost every idle
dashboard ~7.4 MB private RSS and start-up time; no .pc files or `.cargo/config.toml` entries needed).

Tests: `cargo test -p home-assistant-screen` (unit, TMPDIR unset OK), `cargo test -p tessel-locking
--features gtk` (widget unit tests), `tests/run_headless.sh` (11 scenario checks in a private
headless labwc + private bus that fails on any activation), `tests/measure_headless.sh`.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Item | | Test |
|---|---|---|
| `local.luna.Clock`, HANDLES_COMMAND_LINE, hold, single instance (remote forwards argv) | ✅ | headless `cli_primary_status_levels_and_voice_controls` (real binary as primary + remotes) |
| `--show/--hide/--panel/--close-panel/--next-wallpaper/--quit`, exact argv matching | ✅ (`--panel` ☑ in-process: `open_panel`) | headless cli + panel checks |
| `--volume X` / `--brightness X`, errors to stderr + exit 2, usage text + exit 2 | ✅ | headless cli |
| `--status` keys/order/`json.dumps` format | ✅ | headless `dashboard_location_*` (key list), cli (prefix text) |
| swayidle child `-w -C /dev/null timeout N '<exe> --show'` (shlex.join), 10 s restart check, TERM then kill on shutdown | ✅ | headless cli (fake swayidle logs start + exit), unit `shell_join_matches_shlex` |
| config.json next to the program; required keys fatal | ✅ | unit `missing_required_config_keys_are_fatal` |
| SIGTERM/SIGINT → quit | ☑ | |

## §2 IPC / §3 Files
| Item | | Test |
|---|---|---|
| Scheduler socket protocol (request line, 256 KiB line, 0.7 s, `error` key) | ✅ | unit `snapshot_socket_protocol_and_errors`, headless `scheduler_socket_feeds_cards_and_errors_go_stale` |
| Stale snapshot (>3 s) clears cards | ✅ | headless `compact_schedules_*`, `scheduler_socket_*` |
| pactl read/set + read-back verification, mute toggle | ✅ | headless cli (fake pactl), `voice-controls volume up` |
| levels.json `{"brightness": N}\n` via `.tmp` + rename, clamp 10–100 on read | ✅ | unit `brightness_file_round_trip_and_clamp`, headless cli |
| Voice log tail (tessel-locking VoiceLog) | ✅ | tessel-locking unit `log_tail_*`, headless dashboard (`voice` = listening, band animates) |
| Weather (LockWeather), MPRIS (LockMedia) | ✅ | headless dashboard / album; tessel-locking unit weather tests |

## §4 UI
| Item | | Test |
|---|---|---|
| Overlay order, classes, CSS verbatim, no click dismissal | ✅ | headless `panel_focus_*` (no GestureClick) |
| Escape / suppress-until-release / panel Escape | ✅ | headless `panel_focus_*` |
| Media row (album/player stack, controls, sensitivity, video fallback, stop hides) | ✅ | headless `music_album_*`, `dashboard_location_*` |
| Weather location lifecycle, `Sun 27` forecast label | ✅ | headless `dashboard_location_*` |
| Timer/alarm cards (30 s split, side by side, ringing, AM/PM, midnight) | ✅ | unit `countdowns_*`, `alarms_*`; headless timers/alarms/compact |
| Levels card text/icons/auto-hide, dimmer opacity | ✅ (card text via status `indicator`) | headless cli |
| AdminPanel window props (undecorated, modal, transient, fullscreen, focus) | ✅ | headless `panel_focus_*` |
| Panel load-failed → error box | ✅ | headless `admin_panel_origin_policy_with_fake_server` (closed port) |
| Panel keyboard: typing into the web view through tessel-keyboard-input, Hide sticks, touch-only auto-show, bridge closed on dismiss | ✅ | headless `embedded_keyboard_*` |
| Clock/date text formats | ☑ | |

## §5 voice-controls
| Item | | Test |
|---|---|---|
| volume / brightness (systemctl is-active + luna-clock --status/--brightness) JSON | ✅ | headless cli |
| weather current: no configured location → failure JSON; argc/unknown errors | ✅ | headless cli, unit `bad_arguments_answer_in_json` |
| weather success sentence (round/units/description/apparent) | ☑ | (no fake Open-Meteo in this run) |

## §6 / §7 Navigation policy + threat model
| Item | | Test |
|---|---|---|
| Only `http://127.0.0.1:8765` in the panel; https / local-music hosts only on user gesture → external browser | ✅ | unit `navigation_policy_matches_admin_panel_py` (45 cases), headless fake servers: other-origin `location.href`, `window.open` (http + https) all refused, nothing launched |
| Panel URL injectable only through `Options::panel_url` (library constructor), no env/CLI | ✅ | code review: `luna-clock` main uses `Options::new` |
| Developer extras off, context menu suppressed | ☑ | |
| Microphone never opened | ✅ by construction (no audio capture code) | |

## Deviations ⚠ / gaps ❌
1. ⚠ Malformed scheduler rows: Python raised inside the GLib callback (which also removed the
   tick source); Rust keeps the previous cards and keeps ticking.
2. ⚠ `urlsplit` raising (unbalanced `[`) let Python fall through to WebKit's default (allow);
   Rust ignores such URIs (stricter).
3. ⚠ `PanelKeyboard`: Python's `notify::width` handler never fires (GtkWindow has no `width`
   property); Rust likewise only resizes the keyboard class on reconcile.
4. ⚠ Wallpaper decoding uses gdk-pixbuf/glycin instead of PIL (Hyper ≈ Lanczos; webp/avif need
   the system loaders); blur is PIL's 3-pass extended box algorithm in float.
5. ☑ The weather tray test of test_dashboard.py belongs to tessel-weather.
6. ❌ Not measured on the Kasumi device itself (aarch64 Debian, WebKit/Tessel Keyboard helper there must be
   built for the target); the install/bundle scripts (`build_bundle.py`, `install_*.py`) are not
   ported (installer work).
7. ⚠ Security hardening beyond Python (contract §7): WebKit data dirs
   `$XDG_DATA_HOME/webkitgtk` / `$XDG_CACHE_HOME/webkitgtk` made 0700 before the first view
   (unit `webkit_data_dirs_become_user_only`, headless `admin_panel_origin_policy_with_fake_server`);
   explicit `permission-request` deny handler (headless: `Notification.requestPermission()` →
   `denied`, `AdminPanel::denied_permissions`); scheduler reply read under one overall 0.7 s
   deadline (unit `hostile_scheduler_replies_are_bounded_and_never_panic`); tessel-locking `VoiceLog`
   ignores non-regular files (O_NONBLOCK open), bounds the initial skip and over-long partial lines
   (tessel-locking unit `log_tail_bounds_overlong_lines_and_ignores_fifos`). Origin-confusion cases
   (userinfo, `#@`, suffix hosts, ports, IPv6/decimal IPv4, `blob:`/`view-source:`) added to
   `navigation_policy_matches_admin_panel_py` (all Ignore, same as Python's urlsplit check).

## Measurements (`tests/measure_headless.sh`: private headless labwc, fake HOME, same config,
ready = primary answers `--status` with `"visible": true`; RSS/PSS from smaps_rollup 5 s later;
runs 2–3, run 1 is a cold shader cache)
| | ready | RSS | PSS |
|---|---|---|---|
| Rust `luna-clock` (default GSK renderer, Vulkan) | 132–145 ms | 178–182 MB | 126 MB |
| Python `luna_clock.py` (default renderer) | 176–213 ms | 211 MB | 150 MB |
| Rust, `GSK_RENDERER=cairo` | 42–44 ms | 55 MB | 26.7 MB |
| Python, `GSK_RENDERER=cairo` | 106–124 ms | 88 MB | 50.5 MB |

The GPU driver mappings dominate the default-renderer numbers. WebKit is `dlopen`ed on the first
`--panel` (like Python's lazy import); linking it eagerly cost ~7.4 MB private RSS.
Release binaries: `luna-clock` 3.18 MB, `voice-controls` 1.19 MB (Python screen sources 51 KB +
shared modules, interpreter not counted).
