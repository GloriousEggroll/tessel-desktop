# lock-screen — external contract (SECURITY-CRITICAL)

Source: `~/btmanager/lock-screen/` (refs `file:line` there): `lock_screen.py` (UI + session lock),
`pam_helper.c` (→ `waylock-pam`), `lock_auth.py`, `lock_fingerprint.py`, `lock_status.py`,
`lock_config.py` (= `crates/suite-lock::config`, already ported), `power-manager-lock.service`,
`install_lock.py`. Shared widgets (`lock_visuals`, `lock_media*`, `lock_info`, `lock_dashboard`,
`lock_weather`, `voice_state`, `lock_palette`) are ported in `crates/suite-lock` (feature `gtk`).
Global Settings → Lock Screen (`lock_settings.py`) is ported in `components/global-settings`.

## 1. Entry points

### `lock_screen.py` (Rust: `lock-screen`)
- argparse: `--preview` (store_true), `--check` (store_true), `-h/--help`. **argparse prefix
  matching** is accepted by Python (`--pre`, `--ch`); the Rust port accepts the same unambiguous
  prefixes of `--preview`, `--check`, `--help`. Unknown args → usage on stderr, exit 2.
- Before anything else after parsing (:974-976): `setrlimit(RLIMIT_CORE, (0,0))`;
  `prctl(PR_SET_DUMPABLE=4, 0)`; if prctl ≠ 0 → `SystemExit('Unable to protect the authentication
  process')` (stderr, exit 1).
- `suite_i18n.initialize()` first (:969).
- `--check` (:977-983): `Gtk.init()`, then prints one line of JSON (Python `json.dumps`, key order
  fixed): `{"session_lock": <gtk_session_lock_is_supported()>, "pam_helper": <os.access(<dir of
  program>/waylock-pam, X_OK)>, "pam_policy": <isfile("/etc/pam.d/waylock")>}`; exit 0 if all true,
  else 1. `--check` **never locks** (it only binds the registry to test for
  `ext_session_lock_manager_v1`). Rust: `waylock-pam` is looked up next to the executable.
- Normal: `LockApplication(preview).run([argv0])`; exit code = `app.result`:
  0 after unlock (or preview close), **1** if session lock unsupported, **75** if the compositor
  rejects the lock (`failed` signal, e.g. another locker holds it) (:754-756).
- GApplication id `org.wayshell.LockScreen` (lock) / `org.wayshell.LockPreview` (`--preview`),
  flags `NON_UNIQUE` (never forwards to another instance; both may run) (:593-594).

### `waylock-pam` (pam_helper.c; Rust binary of the same name, byte-compatible)
- argv: none → service `waylock`; exactly `--password-only` → `waylock-password`; exactly
  `--fingerprint` → `waylock-fingerprint`; **anything else → exit 1** without output (:80-83).
- Order (:84-89): `setrlimit(RLIMIT_CORE,{0,0})`; `prctl(PR_SET_DUMPABLE,0)`; if `getuid()≠geteuid()`
  or `getgid()≠getegid()` → exit 1 (rejects setuid/setgid installs); `getpwuid(getuid())` or exit 1.
- `pam_start(service, pw_name, &conv, &h)` (≠SUCCESS → exit 1);
  `pam_authenticate(h, PAM_DISALLOW_NULL_AUTHTOK)`; if success `pam_acct_mgmt(h, 0)`; if success
  `pam_setcred(h, PAM_REFRESH_CRED)`; `pam_end(h, status)`. Any non-success (or pam_end failure) →
  exit 1. Success → write packet `(5, "ok")`, exit 0 only if that write succeeded.
- Wire protocol on stdout (helper → client): `u8 type` + `u32 length` (network order) + UTF-8 bytes
  (no NUL, `strnlen(msg, 4096)`). Types = PAM msg styles: 1 `PAM_PROMPT_ECHO_OFF`, 2
  `PAM_PROMPT_ECHO_ON`, 3 `PAM_ERROR_MSG`, 4 `PAM_TEXT_INFO`, 5 = final success, payload `ok`.
- Responses on stdin (client → helper), one per prompt (styles 1/2): `u32 length` (network order)
  + secret bytes (no newline, no NUL). `length > 4096` → conversation error. A NUL inside → error.
- Conversation (:42-74): `count < 1 || count > PAM_MAX_NUM_MSG (32)` → `PAM_CONV_ERR`; styles outside
  1..4 → error; responses array `calloc(count, sizeof(pam_response))`, each `resp = calloc(len+1,1)`
  (libc heap: PAM frees with `free()`); on any failure every allocated response is
  `explicit_bzero`ed then freed and `PAM_CONV_ERR` returned. EOF/short read → error.
- stderr is unused; nothing is logged.

### `power-manager-lock.service` (owned here; started by power-manager, `wayshell-lock`, Super+L, screenlock.sh)
```
[Unit] Description=Wayshell secure screen lock
After=graphical-session.target  StartLimitIntervalSec=0
[Service] Type=notify  NotifyAccess=main  EnvironmentFile=-%h/.config/labwc/environment
ExecStart=/usr/bin/python3 %h/.local/share/lock-screen/lock_screen.py
Restart=on-failure  RestartSec=1  SuccessExitStatus=75  TimeoutStartSec=30s  LimitCORE=0
```
- No `[Install]`. `READY=1` is sent (SOCK_DGRAM to `$NOTIFY_SOCKET`, `@` → abstract) **only from the
  `locked` signal**, i.e. after the compositor confirmed `ext_session_lock_v1.locked` (:669-677).
  Power-manager waits for readiness before sleeping.
- Exit 75 is "success" (another locker already holds the session) so no restart loop; a crash →
  `Restart=on-failure` restarts the client, which re-locks the still-locked session.
- Launchers (install_lock.py:43-48): `~/.local/bin/wayshell-lock` and
  `~/.config/waybar/scripts/screenlock.sh`, both mode 0755, content exactly
  `#!/bin/sh\nexec /usr/bin/systemctl --user start power-manager-lock.service\n`.
  labwc `W-l` keybind → `wayshell-lock` added only if no `W-l` binding exists.

### Install layout (install_lock.py)
`~/.local/share/lock-screen/`: the .py modules (0644), `waylock-pam` (**0755**, never setuid),
plus osk `waykeys-input` (0755) and shared modules. Unit to `~/.config/systemd/user/` (0644).
Installer refuses while the lock unit is active, never starts the locker, runs `--check` first.

## 2. Session lock (ext-session-lock-v1 via Gtk4SessionLock)
- `Gtk4SessionLock.is_supported()` false → result 1, quit (:645-648).
- `Instance.new()`; signals: `monitor(instance, GdkMonitor)` → create a `LockWindow` for that
  monitor, `assign_window_to_monitor(window, monitor)` (also for hot-plugged outputs; the library
  emits `monitor` per existing monitor at lock and per added monitor); if the login is visible,
  focus that window's entry. `locked` → `locked=True`, MPRIS start (pause playing players once if
  `pause_media`), start fingerprint detection, sd_notify READY=1. `failed` → result 75, quit.
  `unlocked` → quit. Then `lock()`.
- **`unlock()` is called only from** `_completed(auth, True)` (password transaction, current `auth`,
  `locked`, not preview) and `_fingerprint_completed(auth, True)` (current fingerprint auth,
  `locked`, not preview, not already authorized). Never from shutdown, close-request, Escape,
  signals, errors or Drop. `do_shutdown` cancels auth, closes everything, does **not** unlock.
- Window `close-request` returns True (ignored). Monitor removal destroys only that window.
- Upstream finding (gtk4-layer-shell `src/gtk4-session-lock.c`, main, 2026-10): the instance class
  has no dispose/finalize; unref never unlocks; lock surfaces hold a ref to the instance. The only
  unlock paths are `gtk_session_lock_instance_unlock()` and the compositor's `finished` event. Rust
  additionally leaks one strong ref (`mem::forget`) so the instance is never finalized.

## 3. Authentication (lock_auth.py, lock_screen.py:766-856)
- `Authentication(on_prompt(msg, echo), on_message(msg), on_complete(success))`, fields `waiting`,
  `cancelled`, `password_only`, `fingerprint`. `start()` spawns `<dir>/waylock-pam
  [--fingerprint|--password-only]` with stdin/stdout pipes, stderr → /dev/null, close_fds; spawn
  failure → `on_complete(False)`. A reader thread parses packets: size > 4096 → invalid; types 1/2 →
  `on_prompt(msg, kind==2)`; 3/4 → `on_message(msg)`; 5 with `ok` → success iff helper exit status
  is 0 within 5 s; anything else invalid. On end: terminate if running, wait 3 s, kill; close pipes;
  `on_complete(success)` on the main loop (suppressed if cancelled). UTF-8 decoded with replacement.
- `respond(text)`: ignored unless waiting and not cancelled; secret > 4096 bytes or containing NUL →
  `cancel()` + `on_complete(False)`; else writes `u32 len + bytes`; the bytearray is zeroed after.
- `cancel()`: `cancelled=True`, `waiting=False`, SIGTERM the helper.
- App: `authenticate(window, text)` ignored when preview / not locked / login hidden / retry timer
  pending. If an auth exists: respond only if it is the same window's and waiting (then
  insensitive). Else new transaction: `initial_secret` = text bytes, all entries/unlock buttons
  insensitive, status `Checking…`, `password_only = fingerprint transaction running and
  /etc/pam.d/waylock-password is a file`. First ECHO_OFF prompt is answered from `initial_secret`
  (then zeroed); later prompts show `window.set_prompt(message)` (standard "password"/`Password`
  prompts stay hidden; others visible and become the entry's accessible label), set echo
  visibility and input purpose (FREE_FORM/PASSWORD), re-enable, focus. Messages go to the auth
  window's status. Failure → status `Authentication failed. Please try again.` on all windows, 2 s
  retry timer then re-enable. Stale completions (auth not current) are ignored.
- `cancel_authentication()`: drop auth, zero initial secret, cancel helper, re-enable (unless retry
  pending), clear every entry, password toggles off, visibility off, PASSWORD purpose, prompt reset,
  status '' (preview: `Preview — your session is not locked.`).

## 4. Fingerprint (lock_fingerprint.py, lock_screen.py:679-752)
- Only if not preview and `/etc/pam.d/waylock-fingerprint` is a file. Detection on the **system bus**:
  `net.reactivated.Fprint` `/net/reactivated/Fprint/Manager` `Manager.GetDefaultDevice()→(o)`
  (auto-start allowed), then `Device.ListEnrolledFingers(s username)→(as)` and
  `Properties.Get(Device, "scan-type")→(v)` (NO_AUTO_START), 5 s timeouts. Any error → silent
  password-only. Non-empty fingers → `found("Swipe your finger on the fingerprint reader")` if
  scan-type `swipe`, else `"Touch the fingerprint reader"`. Never Claim/Verify.
- Then `waylock-pam --fingerprint` runs in parallel (after `locked`). A prompt from it → cancel,
  hint ''. Messages become the hint (status row + form label). Success → cancel the password auth,
  clear entries, `authorized`, `unlock()`. Failure: count a non-match if it ended < 25 s after
  start; at 5 → hint `Too many fingerprint attempts. Use your password.` and stop; else hint
  `Fingerprint not recognized. Try again.` and restart after 1 s. Password success cancels it.

## 5. lock_status.py (D-Bus reads only)
- UPower (system bus, DO_NOT_AUTO_START proxy): `/org/freedesktop/UPower/devices/DisplayDevice`
  `org.freedesktop.UPower.Device` cached `IsPresent`,`Type`,`Percentage`,`State`, refreshed on
  `g-properties-changed`. `battery_summary(present, kind, pct, state)`: None unless present and
  kind==2; level clamp(round); FULL(4) `Fully charged`, CHARGING(1) `Charging`, PENDING_CHARGE(5)
  `Not charging`; icon `battery-{full≥90|good≥50|low≥20|caution≥6|empty}[-charging]-symbolic`
  (`-charging` also for FULL); text `"{level}%"[ " · " detail]`.
- Notifications (session bus, DO_NOT_AUTO_START|DO_NOT_LOAD_PROPERTIES):
  `org.freedesktop.Notifications` `/org/freedesktop/Notifications` iface `org.waydock.Notifications1`;
  `GetState()→(s json)` (3 s, NO_AUTO_START) on owner, `StateChanged(s)` signal. Owner gone → {}.
  `notification_summary(state, names)`: only `unread` count: `1 new notification` /
  `{count} new notifications`; with names: unread, non-transient items' `app_name` or
  `desktop_entry` (≤64 chars, unique, first 4, ` …` if more) appended ` · a, b`. Never summaries or
  bodies.
- `StatusRow`: battery (icon 22 + label, accessible `Battery <text>`), notifications
  (`preferences-system-notifications-symbolic`), fingerprint (`auth-fingerprint-symbolic`); CSS
  `lock-information lock-status`; visible iff any child visible.

## 6. System actions (system bus, NO_AUTO_START)
- `login1.Manager.CanSuspend()` (1.5 s) == `yes` → Sleep button visible; `Suspend(b false)` (10 s).
- `org.freedesktop.DisplayManager` Properties.Get(`Seats`) (1.5 s): offered only if
  `$XDG_SEAT_PATH` (default `/org/freedesktop/DisplayManager/Seat0`) is in the list; Switch User →
  cancel auth, `Seat.SwitchToGreeter()` (5 s). Errors → status `That action is unavailable. The
  session remains locked.`
- Print key: session bus `org.waydock.Capture` `/org/waydock/Capture` `org.gtk.Actions.Activate("lock-shot",
  [d <lock start monotonic>], {})` NO_AUTO_START 1.5 s; never reveals the login.

## 7. UI behaviour (README.md, lock_screen.py:106-578)
- Window title `Screen Locked`, undecorated, CSS `lock-screen` + scope `waydock-companion-<id>`.
  Root overlay: Wallpaper; wallpaper content (scrolled dashboard: clock `lock-clock family-clock`,
  date `lock-date family-date`, weather, media row `lock-information`, assistant `lock-name`,
  status row; voice band overlay) hidden until settled; LoginLayer (blurred) with login content:
  avatar (Avatar, margin-bottom 8), user label (`lock-user family-user`, gecos first field or
  login name), prompt label (hidden), password overlay (entry: visibility off, max 1024,
  PASSWORD purpose, no undo, `lock-password family-password`, accessible `Password`; eye toggle
  `lock-password-eye family-eye` inside trailing end), caps label `Caps Lock is on`, fingerprint
  hint (`lock-caption`), status (`lock-caption`), action row Unlock (submit icon, `suggested-action`),
  Sleep (Adwaita `weather-clear-night-symbolic`), Switch User (`system-users-symbolic`), touch
  keyboard button. Buttons: `lock-action family-action`, 44×44, share one BOTH size group (media
  button excluded, 58×58, `lock-media-action`).
- Hints: native tooltips disabled (`has_tooltip=False`); hover/focus after 200 ms shows the
  `lock-hint` label in an in-surface `Gtk.Fixed` overlay below (or above) the button; no new
  toplevel/popup is ever created. Media hint only when login hidden.
- Starts with login hidden (`login_visible=False`, layer progress 0). Any key / motion (not a
  stationary/synthetic re-enter at the same position; the first `enter` is recorded only) / click
  / scroll → `activity(window)` reveals the login on **all** monitors and focuses that window's
  entry; the waking key/click is swallowed (claimed). Clicks/motion over the preview close button
  or the visible wallpaper media button never wake. 30 s without activity → cancel auth, hide
  login (entries cleared). Escape → cancel auth + clear (preview: quit). Print → capture only.
- Key forwarding: key controller in capture phase; key presses/releases on a window that is not
  the selected `input_window` are `controller.forward()`ed to the selected window's focus widget
  (or entry delegate) — no text is copied between windows; re-entrancy guard `forwarding_input`.
  Clicking a password field selects it as `input_window`.
- `submit`: ignored in preview or when the entry is insensitive; reads text, clears the entry,
  password toggle off, `authenticate`.
- Tick 1 s: idle hide; reload config (changes → refresh windows, start voice log if needed);
  weather poll; status start; each window: dashboard configure(width,height), `lock-narrow`
  (<900 wide), `family-compact` (h<900 or w<900), `lock-compact` (h<900, spacing 10/8/margin 8 vs
  18/12/20), clock via clock preferences, `long_date`, caps lock, background refresh, Sleep/Switch
  visibility.
- `refresh`: appearance(config, palette) → background configure(blur), dock palette CSS
  (`theme_css(scope, color_palette(is_dark, wallpaper color), …)` + lock overrides incl. accent
  alpha `(100-transparency)/100` with 3 decimals), tint, assistant name upper-cased (visible only
  with a detected Home Assistant install and setting), voice band likewise.
- Preview: id LockPreview, one window on monitor 0, 1100×760 resizable, status `Preview — your
  session is not locked.`, entry/eye/unlock insensitive, `Preview · Close` button
  (`family-preview`), MPRIS read-only, no lock instance, no system actions, no fingerprint.
- Media: shown only while playing/paused and `show_media`; title translated, subtitle `artist ·
  Playing|Paused` or error; button tooltip `Pause`/`Play`, insensitive unless controllable, not
  busy, not preview; toggles only when locked, not preview, login hidden, wallpaper shown.
- Broken config never prevents locking (defaults used).
- Touch keyboard: on-screen-keyboard `lock` module (`LockKeyboard`) edits the lock entry directly;
  never the waykeys injection IPC.

## 8. Files
| Path | Use |
|---|---|
| `$XDG_CONFIG_HOME/desktop-suite/lock-screen.json` | appearance config (suite-lock::config), polled each second |
| `$XDG_CONFIG_HOME/desktop-suite/clock.json` | 12/24 h (suite_core::clock) |
| `$XDG_CONFIG_HOME/wayshell/wallpapers.json` | desktop wallpaper per connector |
| `~/.config/waydock/palette.json` via PaletteClient | dock palette |
| `/etc/pam.d/waylock`, `waylock-password`, `waylock-fingerprint` | existence checks only |
| voice log (`~/.hermes/logs/agent.log` default) | read-only, only if Home Assistant detected |
| weather-manager config/cache | read-only (suite-lock) |

## 9. Threat model
1. **Lock bypass.** The only call sites of `unlock()` are the two PAM-success completions (§2). A
   success requires: the `(5,"ok")` packet **and** helper exit status 0 (within 5 s) **and** the
   transaction being the current one (not cancelled, not stale) **and** `locked && !preview`.
   Closing windows, Escape, idle hide, SIGTERM/SIGINT, D-Bus errors, monitor removal, shutdown and
   Rust `Drop` never unlock. There is no test-only unlock entry point in the production binary.
2. **Crash = still locked.** ext-session-lock-v1: if the client dies while locked, the compositor
   keeps outputs locked (labwc shows opaque/black) until a new client locks. The service restarts
   the locker. Release profile is `panic = "abort"` workspace-wide: a panic aborts the process with
   the lock still held (safe); a panic must never be caught and turned into an unlock.
3. **Input leaks.** Secrets never appear in argv, environment, files, logs, stderr, D-Bus or
   GSettings. The password travels: GtkEntry → zeroizing buffer → helper stdin pipe. The helper
   gets no username/service from the UI. Key forwarding between monitors forwards events, not text.
   The touch keyboard edits the GtkEntry directly (never the waykeys injection IPC). Hints are
   labels inside the lock surface; GTK tooltips/popovers (xdg popups, cannot be lock surfaces) are
   disabled. No clipboard: GTK refuses copy/cut while the text is hidden; **Rust deviation
   (security fix)**: `copy-clipboard`/`cut-clipboard` are also blocked on the entry's GtkText
   after the eye toggle revealed the password (Python lets Ctrl+C / the context menu copy it,
   where wlr-data-control clipboard managers record it). Residual (both versions): selecting
   *revealed* text still exports the PRIMARY selection (GTK has no switch for it). The entry
   text is read with `gtk_editable_get_text` straight into a `Zeroizing<String>` (no GString
   copy). The helper is spawned with every fd > 2 close-on-exec (Python `close_fds=True`).
4. **PAM conversation.** Message count 1..32; styles 1..4 only; outgoing text truncated at 4096;
   response length ≤ 4096, NUL rejected, short reads fail; responses libc-calloc'ed (PAM frees),
   all response buffers zeroed (`explicit_bzero`) on failure; Rust-side copies are `Zeroizing`.
   Client side rejects packet sizes > 4096 and unknown types (fail closed).
5. **Helper privilege.** `waylock-pam` is unprivileged (0755, no setuid/caps); it refuses to run if
   uid≠euid or gid≠egid; authenticates only `getuid()`'s account; services are compile-time fixed
   (`waylock`, `waylock-password`, `waylock-fingerprint`). Rate limiting (faillock) lives in PAM.
6. **Core dumps / ptrace.** Both processes: `RLIMIT_CORE 0` and `PR_SET_DUMPABLE 0` (also blocks
   same-uid ptrace and /proc/<pid>/mem reads); unit `LimitCORE=0`.
7. **Files.** Helper 0755; config written 0600 by Global Settings (atomic replace); the locker
   only reads config files. Lock-screen never writes files.
8. **Rust-specific.** `panic=abort` keeps the lock (desired). `Drop`/`dispose` of windows, the app
   or the session-lock wrapper never calls unlock; the lock instance is intentionally leaked.
   `unsafe` FFI is confined to `session_lock.rs` (gtk4-layer-shell), `bin/waylock-pam.rs`
   (libpam + libc), `window.rs::secret_text` (borrowing the entry text) and small libc calls
   in `auth.rs`/`cli.rs` (kill, close_range in pre_exec, setrlimit/prctl). `waylock-pam` resets
   SIGPIPE to SIG_DFL (the Rust runtime ignores it; C dies on a vanished client). No async runtime; the helper reader runs on a std thread and posts to the main
   loop via a channel.
