# session-manager — external contract

Source: `~/btmanager/session-manager/` (1211 LOC): `session_manager.py` 424 (CLI, GApplication,
popup + centered menu), `session_backend.py` 163 (lock/labwc/logind/DisplayManager actions),
`session_end.py` 179 (graceful close of all windows + inhibitor report), tests 445.
Depends on waydock's `window_backend.py` + `waydock-toplevel` helper (wlr-foreign-toplevel)
and shared `tray_popup.py` / `popup_placement.py` (`PointerMonitor`).

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installed dir | `~/.config/waybar/scripts/session-manager/` (`session_manager.py`, `session_backend.py`, `session_end.py` + shared `tray_popup.py popup_placement.py gtk4_utils.py suite_i18n.py suite_catalogs.py language_data.py background_blur.py libwaydock-background-blur.so companion_theme.py README.md`) | shared/install_gtk4.py:21,58 |
| Installer | `python3 shared/install_gtk4.py --apply` (no own installer) | README |
| GApplication id | `org.waydock.Session`, `HANDLES_COMMAND_LINE`; `do_local_command_line` returns `(False, argv, 0)` so every invocation is forwarded to the primary | session_manager.py:284,296-297 |
| systemd unit / autostart / .desktop | none; process lives only while the popup or an action is in progress | README |
| Dock | waydock session/logout button (panel_ui.py:256-258, group `session`) → `session_manager.py --popup --popup-edge E --popup-offset N` via `systemd-run --user … --slice=app.slice` (panel_actions.py:11,43-46; right-click same, never `--settings`) | |
| Keybinds | rc.xml:47 `C-A-Delete` → `desktop-suite-action session-menu`; rc.xml:50 `XF86PowerOff` → `desktop-suite-action power-button` (tries `power_manager.py --power-button` first, then session menu) | ~/.local/bin/desktop-suite-action:72-76,82-85 |
| **Feature probe** | `desktop-suite-action` runs `--menu` only if `grep -E "[\"']--menu[\"']" session_manager.py` matches (the `supports()` helper, line 14); otherwise falls back to `--popup`. The Rust swap must keep a file at `~/.config/waybar/scripts/session-manager/session_manager.py` that contains the literal `'--menu'` (e.g. a shim `exec`ing the binary) or change desktop-suite-action + configure_session.py together | desktop-suite-action:14,72-76; session-startup/test_configure_session.py:349-356 |

### CLI (session_manager.py:50-60)
`--popup` (flag), `--menu` (centered on pointer monitor, keyboard EXCLUSIVE),
`--popup-edge {top,bottom,left,right}` default bottom, `--popup-offset INT` default 76
(negative → argparse error). Validated in the launching process (line 423) and in the primary.
- `show()` while `busy`: only re-activates the popup if the end-session report is shown, else
  ignored (310-314). Switching between menu and edge mode destroys and recreates the popup (315-317).

## 2. IPC

### Owned
GApplication `org.waydock.Session` only.

### Consumed
| Bus | Call | Ref |
|---|---|---|
| system | `org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager.CanSuspend / CanReboot / CanPowerOff` (NO_AUTO_START, 3000 ms) → capability = reply ∈ {`yes`,`challenge`} | session_backend.py:43-46,68-74 |
| system | `org.freedesktop.DisplayManager /org/freedesktop/DisplayManager Properties.Get("org.freedesktop.DisplayManager","Seats")` → `ao`; switch_user enabled only if `$XDG_SEAT_PATH` (default `/org/freedesktop/DisplayManager/Seat0`) is listed | 48-66 |
| system | `org.freedesktop.DisplayManager <seat> org.freedesktop.DisplayManager.Seat.SwitchToGreeter()` 5000 ms, after locking | 133-142 |
| system | `login1.Manager.Suspend(b true) / Reboot(b true) / PowerOff(b true)` flags NONE (interactive auth allowed), 120000 ms. Refused without capability (`That action is unavailable.`) | 144-159 |
| system | `login1.Manager.ListInhibitors()` → `a(ssssuu)` (what, who, why, mode, uid, pid), 3000 ms | session_end.py:139-155 |
| helper | waydock `waydock-toplevel` (wlr-foreign-toplevel-management + xdg-output), searched in `../waydock/` (dev) then `~/.local/share/waydock/`; JSON-lines events on stdout (`ready{seat,protocol_version}`, `window{id,app_id,title,activated,fullscreen,minimized,maximized,outputs,parent}`, `closed{id}`, `output…`, `error{message}`); commands on stdin `close <native_id>\n` | session_end.py:19-29; waydock/window_backend.py:187-262 |

### Commands spawned (session_backend.py)
- Lock: `systemctl --user start power-manager-lock.service` (LOCK_COMMAND, line 8). Success
  of `systemctl start` = lock service ready.
- Log out: `labwc --exit`, only if `$LABWC_PID` > 1 and `/proc/$LABWC_PID/comm` == `labwc`
  (22-28,105-109); else `The labwc session is unavailable.`
- stdout silenced, stderr captured and appended to errors.

## 3. Action semantics
| Action | Confirm | Graceful close | Sequence |
|---|---|---|---|
| lock | no | no | lock |
| switch_user | no | no | lock → SwitchToGreeter; failed lock → `The screen could not be locked. Switch User was cancelled. <err>`; hidden when unsupported |
| logout | yes | yes | close windows → `labwc --exit` |
| sleep | no | no | lock → Suspend(true); failed lock → `… Sleep was cancelled. <err>`; disabled when CanSuspend no |
| restart | yes | yes | close windows → Reboot(true) |
| poweroff | yes | yes | close windows → PowerOff(true) |
Unknown action → `Unknown session action.`

- `perform()` sets busy, `hold()`s app and dismisses popup first (so polkit/auth & app save
  prompts can take keyboard) (342-354). Success → `quit()`; failure → status text (or `The action
  could not be completed.`) and popup re-shown (400-409).

### Graceful end session (session_end.py)
- Start helper; wait ≤ `READY_TIMEOUT` 3000 ms for `ready` (else note `Open apps could not be
  checked.`); after ready wait 150 ms for initial windows, then send one `close` to every window
  present (snapshot set); wait until none of the requested ids remain or `CLOSE_TIMEOUT` 10000 ms.
- Then ListInhibitors filtered (`parse_inhibitors`, 32-45): mode ∈ {block, delay}, `what`
  (colon list) contains `shutdown`; for `logout` only inhibitors with uid == own uid.
- `blocking()` = remaining windows ∨ note ∨ any `block` inhibitor (delay-only does not block).
- Non-blocking → continue automatically. Blocking → "remaining" view. While shown, helper
  changes call `on_update`; if it becomes non-blocking (last app closed) continue automatically.
- Cancel button, Escape or outside-click dismissal → `cancel_end` (no action). Nothing is ever
  killed. Windows newly opened after the snapshot never get a close request.

## 4. Files
None read or written (no config/state). Uses env `LABWC_PID`, `XDG_SEAT_PATH`.

## 5. UI surface (session_manager.py)
- Window title `Session`, default width 380, CSS `session-manager` + `tray-popup`, CSS provider at
  priority `APPLICATION + 31` (above palette's +30). Classes: `popup-surface`, `title`, `subtitle`,
  `session-title`, `session-action`, `session-blocker`, `session-tile`, `destructive-action` (37-47).
- Edge mode: shared `PopupController(group="session")` (namespace `desktop-applet`, OVERLAY,
  exclusive −1, keyboard ON_DEMAND, anchor panel edge + side margin 12; dismiss backdrops).
- Menu mode: `MenuPopupController` (76-99): `PointerMonitor` probe (1×1 input-transparent
  layer surface namespace `suite-output-probe`, waits for `wl_surface.enter`, 1 s timeout) picks
  the monitor; NO anchors (centered), keyboard EXCLUSIVE.
- Rows (ACTIONS 18-25): `lock` `Lock Screen` `changes-prevent-symbolic`; `switch_user` `Switch
  User` `system-users-symbolic`; `logout` `Log Out` `system-log-out-symbolic`; `sleep` `Sleep`
  `system-suspend-symbolic`; `restart` `Restart` `system-reboot-symbolic`; `poweroff` `Power Off`
  `system-shutdown-symbolic`, each with detail string. Header tile `system-shutdown-symbolic` 24px.
- Confirmation (26-30) text + `Cancel` / destructive button labelled with action title.
- Remaining view: title `These apps are still open` (windows or note) else `Some apps are delaying
  this action`; detail note + `Save your work in these apps, or continue and lose unsaved changes.`;
  window rows icon (app_id → DesktopAppInfo `<id>.desktop`, lowercased, last dotted segment
  lowercased; fallback `application-x-executable-symbolic`, 28 px) + title/app_id/`Untitled
  window`; inhibitor rows `dialog-warning-symbolic` + who (`Unknown application`) + why
  (`Blocking`/`Delaying`); scroll max 320 px; buttons `Cancel` / `Log out anyway|Restart
  anyway|Power off anyway` (focused).
- Status label hidden when empty. Sensitivity: sleep/restart/poweroff by capability, logout by
  `can_logout`, switch_user visibility by DM seat.

## 6. Parity tests (test_session.py, test_session_ui.py)
1. Sleep: lock command exact argv first; Suspend only after lock success; failed lock → no suspend,
   done(False) (t:71-83).
2. Lock is exactly `systemctl --user start power-manager-lock.service`, no idle-inhibitor checks (85-87).
3. Switch user: unavailable without seat; locks first; failed lock → no D-Bus call; success →
   `SwitchToGreeter` (89-105).
4. Restart → `_power('Reboot')`, never lock; checks its own `restart` capability (not poweroff);
   argument `(True,)` (107-123). CanReboot queried on connect (125-131).
5. Logout requires `/proc/$LABWC_PID/comm == labwc` (133-140).
6. EndSession: one close per window, proceeds with `([], [], '')`; stuck windows reported after
   timeout, later close updates list without 2nd close request; no windows → proceeds; helper
   missing → note + blocking; cancel → no report and helper stopped; inhibitor filtering by action
   (158-212).
7. UI: logout closes apps first then proceeds; remaining list with cancel/anyway; auto-continue
   when last app closes; dismissing remaining list cancels; menu mode unanchored + EXCLUSIVE;
   edge move re-shows; switch user hidden without DM; destructive actions need confirmation and
   cancel is safe; restart confirmation runs only restart; lock button and error recovery
   (test_session_ui.py:78-230).
8. desktop-suite-action probe uses `--menu` only when the script text contains `'--menu'`
   (session-startup/test_configure_session.py:349-356).

## 7. Risks
- The `supports` grep-based feature detection on the Python source (see §1).
- Dependence on waydock's `waydock-toplevel` JSON helper; the Rust port can instead bind
  `zwlr_foreign_toplevel_manager_v1` directly (wayland-protocols-wlr crate) — semantics must
  match: close each handle once, ready = initial `done` roundtrip.
- Must never force shutdown (`interactive=true`, no `--force`), never kill apps.
