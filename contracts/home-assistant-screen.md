# home-assistant-screen (Kasumi idle dashboard) — external contract

Source: `~/btmanager/home-assistant-screen/` (1454 LOC of runtime: `luna_clock.py` 410,
`admin_panel.py` 113, `screen_keyboard.py` 104, `screen_levels.py` 124, `level_controls.py` 43,
`screen_timers.py` 91, `timer_countdowns.py` 45, `screen_alarms.py` 13, `alarm_indicators.py` 22,
`voice_controls.py` 73; installers `build_bundle.py`, `build_keyboard.py`, `install_remote.py`,
`install_media.py`, `install_keyboard.py`; `test_dashboard.py` 433) plus the shared lock-screen
dashboard modules (`lock_config`, `lock_dashboard`, `lock_info`, `lock_weather`, `lock_media*`,
`lock_visuals`, `lock_palette`, `voice_state` → Rust `crates/suite-lock`), the weather backend
(`weather_backend.py` → `components/weather-manager`) and the Waykeys widgets
(`osk_keyboard/backend/config/devices/state` → `components/on-screen-keyboard`).

It runs on the Kasumi Home Assistant device, not on the desktop: an idle display (not a session
lock) shown by swayidle, plus a WebKit window for the local admin web UI.

## 1. Entry points and processes

| Item | Value | Ref |
|---|---|---|
| Installed layout | `~/makima/screensaver/` = bundle `screensaver/*.py` (+ `waykeys-input` 0755, `README-gtk4.md`); `config.json` there is pre-existing and never overwritten (install refuses without it) | install_remote.py:12,21-22,55-60 |
| systemd unit | `luna-clock.service` (user unit, pre-existing on the device, not written by the installers); installers only `stop/start/restart` it and check `MainPID` owns `local.luna.Clock` | install_media.py:57-95 |
| GApplication id | `local.luna.Clock`, flags `HANDLES_COMMAND_LINE`; `hold()` in startup (never exits on window close) | luna_clock.py:291,309 |
| Program | `luna_clock.py` (Rust: `luna-clock`); `voice_controls.py` (Rust: `voice-controls`) | |
| Child process | `swayidle -w -C /dev/null timeout <idle_seconds> '<shlex.join([sys.executable, luna_clock.py, "--show"])>'`, restarted every 10 s check if it died; terminated (2 s, then kill) on shutdown | 386-394,407-413 |
| Signals | SIGTERM/SIGINT → `quit()` | 330-331 |

### CLI (`do_command_line`, exact argv match after argv[0]; runs in the primary)
| argv | Effect | Exit |
|---|---|---|
| (none) or `--show` | `window.reveal()` (if the panel is visible: `panel.reveal()` only) | 0 |
| `--hide` | `window.dismiss('command')` | 0 |
| `--panel` | create AdminPanel once, reveal clock if hidden, `panel.reveal()` | 0 |
| `--close-panel` | `panel.dismiss()` if it exists | 0 |
| `--next-wallpaper` | `wallpaper.next_rotation = 0; wallpaper.refresh()` | 0 |
| `--volume X` / `--brightness X` (exactly 2 args) | `levels.adjust_volume/brightness(X)`; ValueError/OSError → stderr `"<message>\n"` | 2 on error, else 0 |
| `--status` | one line of `json.dumps(...)` + `\n` on the caller's stdout (keys below) | 0 |
| `--quit` | `quit()` | 0 |
| anything else | stderr `Usage: luna_clock.py [--show\|--hide\|--status\|--next-wallpaper\|--quit\|--volume up/down/mute/0–100\|--brightness up/down/10–100]\n` | 2 |

Note: an unknown `--volume X` is validated by `resolve_level(0, X)` before any pactl call; volume
changes themselves run on a worker and failures show "Unavailable" on the card (exit is still 0).

### `--status` JSON (key order, Python `json.dumps` default separators, `ensure_ascii`)
`visible` (bool), `voice` (idle|listening|thinking|speaking — last polled state even when hidden),
`wallpaper` (str path or null), `size` ([w, h] of the overlay allocation), `idle_seconds`,
`last_dismissal` (null after reveal; `"escape"`/`"command"`/…), `idle_monitor_running` (bool),
`weather_visible`, `location` (config name or null), `media_visible`, `gtk_version` (4),
`media_controls` (`["previous", "play-pause", "next"]`), `media_album_visible`, `media_art_visible`,
`levels` {`volume`: null|{`sink`,`value`,`muted`}, `screen_brightness`, `brightness_mode`:
`"idle-screen dimming"`, `indicator`: null|{`kind`,`value`,`muted`,`direction`,`error`}},
`timer_countdowns` (list of rows), `compact_timers` (list), `today_alarms` (list),
`panel_visible`, `panel_focused`, `keyboard` ({`visible`:false} without a panel, else
{`visible`,`automatic_available`,`ready`,`error`}), `clock_focused`.

### `config.json` (next to the program; read once at construction; tests inject a dict)
Keys read: `wallpaper_directory` (expanduser), `wallpapers` (names in it), `wallpaper_seconds`,
`blur_radius` (default 0; only stored into LockConfig — `configure(…, 0)` so the screen is sharp),
`assistant_name` (default `Kasumi`, shown upper-cased), `idle_seconds`, `voice_log` (required),
`show_voice` (default true), `levels_state` (default `~/.config/kasumi-screen/levels.json`),
`clock_24_hour` (default false), `show_weather` (default true; also gates forecast), `show_media`
(default true). Missing required keys (`wallpaper_directory`, `wallpapers`, `wallpaper_seconds`,
`idle_seconds`, `voice_log`) are fatal at start-up (Python KeyError).

## 2. IPC

### Owned
- GApplication bus name `local.luna.Clock` on the session bus (object `/local/luna/Clock`,
  `org.gtk.Application`); no custom interface.

### Consumed
| What | Detail | Ref |
|---|---|---|
| MPRIS | `LockMedia(changed)` (not readonly, never pauses on start: `start()`), toggle/previous/next buttons only | luna_clock.py:296,315 |
| Kasumi scheduler socket | `~/.local/share/kasumi-control/voice.sock` (Path.home()), AF_UNIX stream, timeout 0.7 s, send `json.dumps({"operation": op}).encode() + b"\n"`, read one line ≤ 262145 bytes, `json.loads`; `"error"` key → ValueError. Ops: `countdowns` → `response["timers"]`, `alarms_today` → `response["alarms"]`. Polled each second from a worker; never written to otherwise | timer_countdowns.py:7-21, alarm_indicators.py:6 |
| Voice log | `config.voice_log` tailed every 150 ms by `VoiceLog` (suite-lock voice_state) | luna_clock.py:295,376 |
| Weather | `LockWeather` (weather-manager config `~/.config/weather-manager/config.json` + cache), polled every 5 s, `poll(True)` | 297,375 |
| pactl | `pactl get-default-sink`, `pactl --format=json list sinks` (2 s timeouts, stderr discarded), `pactl set-sink-mute <sink> 1|0|toggle`, `pactl set-sink-volume <sink> N%`; read-back verified | level_controls.py |
| Admin web UI | `http://127.0.0.1:8765/?embedded=1` in WebKitGTK 6.0 | admin_panel.py:8 |
| Desktop browser | `Gio.AppInfo.launch_default_for_uri(uri)` for allowed external links (§6) | admin_panel.py:73 |

## 3. Files
| Path | Format | Ref |
|---|---|---|
| `levels_state` (default `~/.config/kasumi-screen/levels.json`) | `{"brightness": N}` + `\n` (json.dumps default), written to `<path>.tmp` (with_suffix) then `replace()`; parent `mkdir -p`; read on start: `max(10, min(100, int(...)))`, any error → 100 | screen_levels.py:31-34,107-110 |
| `~/.config/on-screen-keyboard/config.json` | Waykeys settings (read only here) | screen_keyboard.py:15 |
| `~/.config/weather-manager/config.json` + `$XDG_CACHE_HOME/weather-manager/` | weather (read; cache written by the backend) | |
| WebKit data | default WebKit network session: `$XDG_DATA_HOME/webkitgtk/` (storage, mediakeys salts) and `$XDG_CACHE_HOME/webkitgtk/` (WebKitCache, CacheStorage, HSTS) — independent of the program name, so Python and Rust share it | WebKit defaults (observed in the headless run) |

## 4. UI surfaces
- `ClockWindow`: `Gtk.ApplicationWindow(title='Kasumi Clock', decorated=False)`, class `lock-screen`,
  default 1280×720, `close-request` blocked, fullscreened on reveal. Overlay layers in order:
  Wallpaper (slideshow of `wallpapers`, interval `wallpaper_seconds`, blur 0) → ScrolledWindow
  (NEVER/AUTOMATIC) with WallpaperDashboard(clock `lock-clock`, date `lock-date`, weather,
  media row, assistant `lock-name`) + ScreenTimers widget after time_group + schedule summary
  (compact timers + alarms) → VoiceBand (visible = `show_voice`) → levels dimmer
  (`screen-dimmer`, opacity 1−b/100, not targetable) → levels card revealer → gear button
  (`settings-gear`, `emblem-system-symbolic` 34 px, END/END margins 24, tooltip
  `Open Kasumi control panel`) → size sensor (DrawingArea, opacity 0, `resize` →
  dashboard.configure + `lock-compact` (h<900) / `lock-narrow` (w<900)).
- No click/tap/motion dismissal (no GestureClick on the root). Key controller (CAPTURE):
  AudioRaiseVolume/LowerVolume/Mute → adjust_volume up/down/mute; MonBrightnessUp/Down →
  adjust_brightness; Escape: ignored while `suppress_escape_until_release`; with a visible panel →
  set suppress + `panel.dismiss()`; else `dismiss('escape')`. Escape release clears suppress.
- Clock text `%H:%M` (24 h) or `%I:%M %p` with leading 0 stripped; date `"{%A}, {%B} {day}, {year}"`
  (locale strftime, not translated); tick every 1 s while visible (also refreshes wallpaper).
- Media row (`lock-media-*` classes, album cover 128 px in `album-cover` overlay with 28 px
  `album-player` badge, player icon 64 px, title ellipsized 40 chars, subtitle 48, album 44;
  buttons `lock-media-action` previous/play-pause/next with tooltips/a11y `Previous track`,
  `Play`/`Pause`, `Next track`; subtitle = error or `artist · Playing|Paused`).
  `information_changed` exact rules luna_clock.py:252-283.
- Levels card (`screen-level`, 320 px, top-right margins 28, crossfade 180 ms, auto-hide 2.5 s):
  icon `display-brightness-symbolic` / `audio-volume-muted-symbolic` / `audio-volume-high-symbolic`,
  label `Screen brightness`/`Volume`, value error or `Muted` or `"{↑|↓|} N%"` stripped.
- Timer cards (`timer-card`, `timer-title`/`timer-digits`/`timer-caption`, `timer-ringing`,
  `compact-timer-card`, `alarm-card`), FlowBox max per line = clamp(n,1,6). Caption `Time’s up`
  / `remaining` or the row caption.
- AdminPanel: `ApplicationWindow(title='Kasumi Control', decorated=False, transient_for=clock,
  modal=True)`, class `kasumi-panel`, toolbar (`panel-toolbar`, title `Kasumi control`,
  keyboard toggle after the title, `×  Close` `panel-close` tooltip `Return to the clock (Esc)`),
  error box (`panel-error`, `The control panel is unavailable. Please try again.`, `Try again`
  `panel-retry`), WebView (developer extras off, context menu suppressed, load-failed /
  web-process-terminated → error box), Escape (CAPTURE) → suppress + dismiss, close-request →
  dismiss. `dismiss()` hides, `window.reveal()`, focuses the gear. `reveal()` fullscreen +
  present + focus web view. The page/session is kept while hidden.
- PanelKeyboard: Waykeys `Keyboard` appended under the web view (viewport shrinks),
  `keyboard-narrow` when 0 < panel width < 724, toggle `input-keyboard-symbolic` (not focusable,
  tooltip `Show or hide the on-screen keyboard`, `suggested-action` while shown); bridge to
  `waykeys-input` only while needed; ready → present panel; messages exact (screen_keyboard.py).
- All CSS strings (`STYLE`, levels, timers, alarms, panel) verbatim.

## 5. voice_controls.py (CLI: `voice-controls KIND ACTION`)
Prints one JSON line (`json.dumps`). `brightness A`: `systemctl --user is-active --quiet
luna-clock` (3 s) must succeed, then `<luna-clock> --status` → `levels.screen_brightness`,
`resolve_level(cur, A, minimum=10)`, `<luna-clock> --brightness T`, re-read must equal T →
`{"success": true, "value": T, "response": "Screen brightness is T percent."}`. `volume A`:
`change_volume(A)` → `{"success": true, "response": "Volume is muted."|"Volume is N percent.",
"sink", "value", "muted"}`. `weather current`: saved location only (no fallback city), refuses
stale/no current data (`{"success": false, "response": "I could not get a current weather report
just now."}`), else `In <city before ','>, it is <round(temp)> degrees <units>[ and <description
lowercased, unless unavailable>][, feeling like <round(apparent)> degrees].` with `source:
"Open-Meteo"`, `observed_at`. Any exception / wrong argc → `{"success": false, "response": "I could
not check the weather just now."}` (kind weather) or `"I could not change that setting just now."`.
Exit status always 0. `resolve_level`: up/down ±10 clamped [min,100]; `^\d{1,3}%?$` else
`Use up, down, or a percentage`; out of range `Choose a level from {min} to 100 percent`.

## 6. Admin-panel navigation policy (admin_panel.py:62-77, verbatim)
For NAVIGATION_ACTION and NEW_WINDOW_ACTION decisions: `(scheme, netloc) == ('http',
'127.0.0.1:8765')` → allowed (WebKit default handling). Anything else → `ignore()`; then if the
action `is_user_gesture()` and (scheme `https` or (scheme `http` and netloc in
`127.0.0.1:8095`, `192.168.0.100:8095`, `raspberrypi.local:8095`)) → hand the URI to the
desktop's default handler (a full browser, for account consent/OAuth pages); never inside the
panel. Response decisions are not filtered (default). `urlsplit` netloc includes userinfo and
port verbatim (`http://user@127.0.0.1:8765/` is *not* allowed).

## 7. Threat model
- **Admin panel origin.** The embedded view is a privileged surface (local admin UI with its own
  login). Only `http://127.0.0.1:8765` documents may load in it, including redirects, form posts,
  `window.open` and target=_blank (all arrive as navigation/new-window decisions). Everything
  else is ignored; only an explicit user gesture can send an https or local-music URL to the
  external browser — scripts cannot open the browser without a tap, and `file:`, `data:`,
  `javascript:`, plain-http other hosts and custom schemes are never opened anywhere.
  Developer extras and the context menu are disabled. The URL is a constant; the Rust port
  injects a different base only through a library constructor argument used by tests (no
  environment variable or CLI flag can redirect production).
- **Credentials / cookies.** The panel never sees or stores credentials itself; the admin UI's
  login lives in WebKit's default network session. No persistent cookie store is configured
  (neither port calls `set_persistent_storage`), so session cookies live in the network process
  for the lifetime of the dashboard; WebKit's own website data goes to `$XDG_DATA_HOME/webkitgtk/`
  and `$XDG_CACHE_HOME/webkitgtk/` (directories created by WebKit under the process umask — 0755
  with umask 022; salts 0600; HSTS db 0644 — i.e. not user-only in Python). **Rust hardening:**
  before the first view is created, both `webkitgtk` directories are created 0700 (or an existing
  one owned by the user has its group/other bits removed; symlinks and foreign-owned directories
  are left alone), so WebKit's 0755 subdirectories are no longer reachable by other users.
  Permission requests (`permission-request`: microphone/camera, geolocation, notifications,
  clipboard, website-data access, …) are denied by an explicit handler (Python relied on WebKit's
  default deny). The Rust port adds no other storage. `--status`
  reports only visibility/readiness, never URLs, page content or typed text.
- **Scheduler socket trust.** `voice.sock` is a per-user socket under `~/.local/share`; replies are
  untrusted JSON: bounded to one 256 KiB line, 0.7 s timeout (Rust: an overall deadline for the
  whole reply, not per recv as in Python, so a trickling peer cannot hold the worker), worker thread, and only listed
  fields are used (strings shown as plain GTK labels, never markup). Malformed rows must not
  crash the screen; the socket is never written beyond the fixed request.
- **Voice log.** Parsed as untrusted text (lossy UTF-8, NUL stripped, fixed substrings only,
  256 KiB per poll); nothing from it is displayed. Rust hardening (shared suite-lock `VoiceLog`):
  the log is opened `O_NONBLOCK` and read only when it is a regular file (a FIFO could block the
  GTK thread in Python), the initial partial-line skip is bounded to 256 KiB, and a partial line
  longer than 256 KiB is dropped up to its newline instead of buffering without bound. The screen **never opens the microphone**;
  the voice band only reflects the log.
- **External commands.** `pactl` / `swayidle` / `systemctl` are run with argv lists (no shell);
  the volume target is a validated integer. `voice-controls` arguments are validated before use.
- **Tests.** Only fake endpoints: a fake HTTP server on a random 127.0.0.1 port or `load_html`
  with base `http://127.0.0.1:8765/` (no request is made), fake pactl/wpctl/systemctl on PATH,
  fake scheduler socket and voice log in private dirs, fake MPRIS players on a private bus.
  Never a real Home Assistant, admin endpoint or token.

## 8. Parity test cases (test_dashboard.py → Rust)
1. music album/controls/video fallback/player exit (fake MPRIS player, local cover file).
2. embedded keyboard types into the web panel and hides without losing the field.
3. panel focus / Escape / click lifecycle (no click dismissal, modal + transient, Escape twice).
4. today's alarms AM/PM side by side with timers; midnight rollover; empty → hidden.
5. timer countdowns side by side, 30 s urgent threshold, ringing, removal.
6. compact schedules share space with urgent timers; stale (>3 s) snapshot hides cards.
7. dashboard location lifecycle (fake weather client) and media row.
8. (weather tray test belongs to weather-manager.)
Plus: navigation policy table, levels file/clamp/resolve_level, pactl read-back, status JSON keys,
CLI usage/exit codes, voice-controls outputs.

## 9. Memory / startup
Python baseline measured in the private headless session (see components/home-assistant-screen/PARITY.md):
`luna_clock.py` ready 176–213 ms, RSS 211 MB / PSS 150 MB with the default renderer (88 / 50 MB with
`GSK_RENDERER=cairo`). WebKit is imported only when the panel is first opened.
