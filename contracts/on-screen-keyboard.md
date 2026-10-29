# on-screen-keyboard (Waykeys) — external contract

Source: `~/btmanager/on-screen-keyboard/` (Python GTK4 + layer-shell, native C helper). Refs relative to that dir unless prefixed.

## 1. Entry points and processes

| Thing | Value |
|---|---|
| Main | `osk_manager.py` → installed `~/.config/waybar/scripts/on-screen-keyboard/osk_manager.py` |
| Native helper | `waykeys-input` (C, `native/input.c`, built by `Makefile`: `wayland-scanner` for `protocols/input-method-unstable-v2.xml`, `virtual-keyboard-unstable-v1.xml`; links wayland-client, xkbcommon; `-O2 -Wall -Wextra -Werror`) |
| Status stream | `osk_status.py` (spawned by waydock) |
| Launcher | `~/.local/bin/waykeys` = `#!/bin/sh\nexec /usr/bin/python3 '<target>/osk_manager.py' "$@"` (install.py:47) |
| .desktop | `~/.local/share/applications/org.waydock.OnScreenKeyboard.desktop`: `Name=On-screen Keyboard`, `Comment=Touch keyboard and automatic keyboard settings`, `Exec=<home>/.local/bin/waykeys --settings`, `Icon=input-keyboard`, `Categories=Settings;Accessibility;`, `Keywords=Keyboard;Touchscreen;Touch;Typing;` (install.py:48-52) |
| GApplication | id `org.waydock.OnScreenKeyboard`, `HANDLES_COMMAND_LINE` → **single instance via GApplication D-Bus** (owns `org.waydock.OnScreenKeyboard`, `org.gtk.Application` / `org.gtk.Actions` on `/org/waydock/OnScreenKeyboard`); secondary invocations forward argv to the primary (osk_manager.py:35) |

### CLI (osk_manager.py:63-72) — handled in primary instance
| Arg | Behaviour |
|---|---|
| `--background` | start/keep service, nothing shown (systemd) |
| `--toggle` / *(no args)* | toggle visibility (manual show / hide) |
| `--show` | manual show (clears a failed bridge first) |
| `--hide` | hide + dismiss + reset modifiers |
| `--settings` | open (or present existing) settings window |
| `--quit` | quit service |
| anything else | return code 2, nothing done |
Precedence when several: quit > settings > background > hide > show > toggle. Process holds itself (`hold()`), never exits on window close.

### systemd `~/.config/systemd/user/on-screen-keyboard.service` (on-screen-keyboard.service)
```
[Unit]
Description=Waykeys automatic touch keyboard
After=graphical-session.target
PartOf=graphical-session.target
[Service]
Type=simple
Environment=GDK_BACKEND=wayland
EnvironmentFile=-%h/.config/labwc/environment
ExecStart=/usr/bin/python3 %h/.config/waybar/scripts/on-screen-keyboard/osk_manager.py --background
Restart=on-failure
RestartSec=2
LimitCORE=0
```
No [Install]; enablement must be `static`/`disabled` (install.py:132). Plus `50-desktop-suite-recovery.conf` drop-in (OnFailure=desktop-suite-failed@%n, burst 5/60 s). Started by labwc autostart `systemctl --user start on-screen-keyboard.service >/dev/null 2>&1 &` (after waydock; session-startup/configure_session.py:385), stopped in `~/.config/labwc/shutdown` with `systemctl --user stop on-screen-keyboard.service >/dev/null 2>&1` (install.py:13-15,52-60). Friendly name "On-screen keyboard" (session-startup/desktop_suite_failed.py:28).

### Install layout (install.py:19-60; shared/install_gtk4.py:58-68)
- `~/.config/waybar/scripts/on-screen-keyboard/`: `osk_*.py`, `waykeys-input` (0755), shared `popup_placement.py gtk4_utils.py app_menu.py suite_i18n.py language_data.py suite_catalogs.py background_blur.py libwaydock-background-blur.so`, `companion_theme.py`, `window_backend.py`, `waydock-toplevel` (0755), `tray_popup.py` (install_gtk4 path).
- Lock screen copy: `~/.local/share/lock-screen/{osk_lock.py,osk_keyboard.py,osk_config.py,osk_devices.py,waykeys-input}` (also lock-screen/install_lock.py:27). Install refuses while `power-manager-lock.service` is active (install.py:80-83).
- Also refreshes waydock (`~/.local/share/waydock/…`) and global-settings (`keyboard_page.py` etc.). Readiness check: `$XDG_RUNTIME_DIR/waykeys/state.json` `pid == MainPID` and `error == ""` within 15 s (install.py:86-100).

## 2. IPC

### waykeys-input line protocol (native/input.c; osk_backend.py)
Spawned as `waykeys-input <layout>` with stdin/stdout pipes, stderr silenced; one persistent process while the keyboard is "needed".
- **stdin** (≤63 chars per line, queue 128, else `error` + exit):
  - `key <evdev 0..247> <flags 0..31>` — flags bit 1 Shift, 2 Ctrl, 4 Alt, 8 Super(Logo), 16 AltGr(Mod5).
  - `text <unicode scalar 1..0x10FFFF, not surrogate>`.
  - `layout <xkb name [a-z0-9_]{1,39}>` (Python side: `isalnum` after removing `_`, ≤39).
  - EOF → reset modifiers, destroy virtual keyboard + IM, exit 0.
- **stdout** JSON lines:
  - `{"event":"ready","input_method":bool}` (IM v2 manager present)
  - `{"event":"layout","keys":{"<evdev>":[base,shift,altgr,shift+altgr],…},"dead":{"<evdev>":["dead_x"|"",…4]}}` for keys 2-13,16-27,30-41,43-53,57; dead keys labelled ´ ` ^ ¨ ~.
  - `{"event":"context","active":bool,"purpose":u,"epoch":u}` on each IM `done`; epoch increments on every `activate`.
  - `{"event":"ime-unavailable"}`; `{"event":"error","message":"…"}` (messages: "Cannot connect to the Wayland display.", "This compositor does not support virtual keyboards.", "Could not load the keyboard layout.", "That keyboard layout is unavailable.", "Keyboard input queue overflow.", "Invalid input command.").
- `waykeys-input --labels <layout>` (no Wayland): prints one `layout` event and exits 0/1 — used by lock screen (osk_lock.py:13-18, 2 s timeout, lru_cache 8).
- Typing semantics: when IM active+available and no Ctrl/Alt/Super: keysym → compose (locale table) → `commit_string`+`commit(serial)`, then waits for next `done` (or 250 ms) before processing more commands (WebKit fix, input.c:127-133, 211). Backspace during compose cancels compose. Otherwise raw virtual-keyboard press+release with modifiers set then cleared. `text` without IM: temporary keymap with `<I255>` = `U%04X`, press keycode 247, restore keymap.
- Python bridge: queue cap 128 (drops), exit → `failed('The keyboard input connection closed. Hide and reopen the keyboard to reconnect.')`; line >32768 → failure; close = close stdin, force_exit after 2 s.

### D-Bus consumed
- `org.freedesktop.systemd1` (session): `Get(org.freedesktop.systemd1.Unit, ActiveState)` on `/org/freedesktop/systemd1/unit/power_2dmanager_2dlock_2eservice` (NO_AUTO_START, 1.5 s) + `PropertiesChanged` subscription (osk_session.py:3-31). Locked = `active|activating|deactivating` (initial), signal also treats `failed` as locked (stays suspended after a failed locker).
- GApplication own-name (above). No D-Bus API exported beyond GApplication/actions (`about` action from app_menu).

### Wayland protocols
- Main process: layer-shell (gtk4-layer-shell). Helper: `wl_seat` v1, `zwp_virtual_keyboard_manager_v1` v1 (required), `zwp_input_method_manager_v2` v1 (optional). Never grabs hardware keys; ignores surrounding text.

### udev
`pyudev` netlink monitor `subsystem=input`, debounce 180 ms (osk_devices.py:49-67). Classification (osk_devices.py:15-24): skip other `ID_SEAT` (vs `$XDG_SEAT`, default seat0), `/sys/devices/virtual/input/*`, `LIBINPUT_IGNORE_DEVICE=1`; `ID_INPUT_TOUCHSCREEN=1` → touchscreen; `ID_INPUT_KEYBOARD=1` and not mouse/touchpad/joystick and name not matching `\b(mouse|trackball|touchpad|touchscreen|hotkeys|consumer control|system control)\b` (i) → keyboard. Only `event*` nodes; name from parent `input` attr `name`; identity = `ID_SERIAL` (else `ID_PATH`) + `|` + name; dedup by (identity, kind); sorted (kind, casefold name). `automatic_available = automatic && any touchscreen && no keyboard whose identity ∉ ignore_keyboards`.

## 3. Files

| Path | Format | Writer / readers |
|---|---|---|
| `$XDG_CONFIG_HOME/on-screen-keyboard/config.json` (default `~/.config/…`) | `{"automatic":true,"layout":"system","size":"medium","ignore_keyboards":[]}`, `indent=2`, trailing `\n`, mode 0600, mkstemp `.keyboard-*` + fsync + rename (osk_config.py:26-41) | written by `osk_settings.KeyboardSettings` (standalone window and Global Settings Keyboard page, global-settings/keyboard_page.py:129-145); read by daemon (directory monitor, 120 ms debounce) and lock screen |
| validation (osk_config.py:13-22) | unknown keys dropped; `automatic` bool; `layout` ∈ system,us,gb,de,fr,es,it,pt,br,ru,ua,tr,se,no,dk; `size` ∈ small/medium/large; `ignore_keyboards` list ≤128 str ≤512 | invalid → ValueError (save rejects, load error shown) |
| `$XDG_CONFIG_HOME/labwc/environment` | reads last `XKB_DEFAULT_LAYOUT=` line (optional `export `, quotes stripped), comma list, each `[A-Za-z0-9_]{1,39}` else dropped; fallback env `XKB_DEFAULT_LAYOUT`, then `['us']`. `system` layout = first entry | written by Global Settings keyboard page; file monitor triggers reload |
| `$XDG_RUNTIME_DIR/waykeys/state.json` (dir 0700, file 0600, `.tmp` + rename) | `{"pid":int,"visible":bool,"automatic_available":bool,"ready":bool,"error":str}\n`, written only on change; removed at shutdown if pid matches (osk_manager.py:202-208,236-239) | read by `osk_status.py` (waydock), install.py readiness |

## 4. Visibility policy (osk_state.py — pure, port verbatim)
`visible = manual || (eligible && active && !dismissed)`; `needed = eligible || manual`. `context(active, epoch)`: new epoch clears `dismissed`. `show()`: manual, undismissed. `hide()`: not manual, dismissed. `hardware(e)`: losing eligibility hides; gaining resets dismissed and epoch=-1.
Manager (osk_manager.py):
- Bridge (helper) exists iff `needed && !suspended`; tearing down also sets `active=False`. Lock → hide, `active=False`, `epoch=-1`, suspend; unlock re-reconciles.
- `ready` without IM → error text "Automatic text-field detection is unavailable. Use the keyboard button to open it."; `ime-unavailable` → "Another input method is active. The keyboard still works manually."
- Context change (epoch or purpose differs) resets modifiers and sets mode: purposes 2,3,4,9 (digits, number, phone, PIN) → numeric pad.
- Bridge failure: error shown, keys insensitive; next `show` recreates bridge (also in automatic mode).
- Layout change in config/environment → `layout <name>` sent only if changed; size applied live.
- Layer shell not supported → error "On-screen keyboard requires a compatible Wayland session.", no windows.
- Placement: panel goes to the monitor under the **pointer** at show time (`popup_placement.PointerMonitor`), never follows keyboard focus; width `min(1100, max(300, mon_w-24))`; `<700` → `keyboard-narrow`. Monitor geometry changes reposition.
- Launcher button visible when `eligible && !shown && !suspended`.

## 5. UI surfaces
| Surface | Kind |
|---|---|
| Keyboard panel | `Gtk.ApplicationWindow` undecorated, title "On-screen Keyboard", css `waykeys-window`; layer-shell namespace `waykeys-keyboard`, layer OVERLAY, keyboard mode NONE, anchor BOTTOM margin 8, `auto_exclusive_zone_enable` while visible (reserves space), zone 0 when hidden; close-request → hide |
| Touch launcher | namespace `waykeys-launcher`, layer TOP, kbd NONE, anchor BOTTOM+RIGHT margins 12, zone 0; 52×48 `input-keyboard-symbolic` button, tooltip "Show keyboard" |
| Settings window | normal window 640×580, title "On-screen Keyboard Settings", `headerbar("On-screen Keyboard")`, `app_menu.install_app_menu` (name/icon/comment; quit closes window only) wrapping `osk_settings.KeyboardSettings` |
| Lock-screen keyboard | `osk_lock.LockKeyboard` embedded in lock surface (lock-screen/lock_screen.py:43-46,240); no IPC, edits the GtkEntry directly |
All windows `companion_theme.attach`. Keyboard widget (osk_keyboard.py) CSS: `waykeys`, `popup-surface`, `keyboard-heading`, `keyboard-key`, `latched`, `keyboard-utility`, `keyboard-status`, `keyboard-small|medium|large`, `keyboard-narrow`. Settings CSS: `keyboard-settings`, `detail`, `gs-page-title`, `gs-card`, `gs-row-subtitle` (from global_style.PAGE_CSS if importable); `search_targets` keys `keyboard-automatic`, `keyboard-layout`, `keyboard-size`, `keyboard-devices` (global-settings/settings_search.py:32-40). Icon file `/usr/share/icons/Adwaita/symbolic/devices/input-keyboard-symbolic.svg` preferred.
Layouts: letters (4 rows + bottom row 123/#+=/Ctrl/Alt/Super/Space/AltGr/arrows), numbers (789/456/123 pad with ⌫,-,+, ABC/0/./↵), symbols (4 fixed rows + ABC/Esc/Space/Delete/⌫/↵). All buttons `focusable=False`. Repeat (⌫ 14, Delete 111, ← 105, → 106): first fire on press, repeat after 420 ms every 65 ms. Shift double-tap within 0.4 s → caps lock (⇪). One-shot modifiers reset after each key unless shift locked. Labels from helper `layout` event; empty label → `·` and insensitive. Secure mode: no tooltips.
Lock keyboard (osk_lock.py:83-128): Enter→submit, Esc→hide, arrows/Home(102)/End(107) move cursor, Ctrl+A select all, other Ctrl/Alt/Super ignored (no clipboard), dead keys compose via combining chars + NFC, dead+dead → spacing accent; auto shows when touchscreen-without-keyboard.

### Waydock integration
- Status: waydock runs `python3 -u osk_status.py` (waydock/status_backend.py:32) — polls state.json every 1 s, checks `kill(pid,0)`, prints on change `{"text":"","class":"active"|"inactive","tooltip":"On-screen Keyboard: On|Off\n<error | Automatic: touchscreen without a keyboard | Click to show · Right-click for settings>"}` (untranslated).
- Click: `osk_manager.py --toggle`; right-click `--settings` (waydock/panel_actions.py:21,42-43) via `systemd-run --user --collect --slice=app.slice`.

## 6. External commands
`waykeys-input <layout>` (persistent), `waykeys-input --labels <layout>` (lock screen). `waydock-toplevel`/`window_backend` copied but **unused at runtime** (only `.close()` on a never-set attribute) — port need not spawn it. Install: `systemctl --user show/restart/is-enabled`, `sh -n`.

## 7. i18n
`suite_i18n.translate` (`shared/locale/<code>.json`). Strings: On-screen Keyboard, On-screen Keyboard Settings, Show keyboard, Hide keyboard, Hide, Keyboard settings, Space, Backspace/Enter/Tab/Escape/Left/Right/Up/Down/Delete (tooltips), the three error messages in §4, "Keyboard input failed.", settings page strings (Show automatically, Keyboard layout, System keyboard layout, Key size, Small/Medium/Large, Changes apply immediately., Connected input devices, "Touchscreens: {value1}", None detected, Count this device as a physical keyboard, No physical keyboards detected., "Follows your physical keyboard layout in Keyboard settings (now: {layout}).", …), layout names. Untranslated: osk_status tooltip, bridge failure messages from osk_backend, lock button "Show keyboard".

## 8. Parity test cases
Policy (tests/test_policy.py): system layout from labwc env, quoted value, `$(evil)` rejected → `us` (:12); hardware combos + ignore list + automatic off (:24); virtual inputs, mouse receivers, mouse/joystick flags, other seats don't block; uhid BT keyboard counts (:33); hide sticks across repeated commits of same epoch, new epoch reopens, inactive hides (:42); manual use and hotplug (:48); config atomic roundtrip, invalid layout rejected without changing file (:53).
UI (tests/test_ui.py, private headless labwc with 2 outputs): panel on pointer's output not focus's (:41); real typing `aB`, `€`, backspace, Hide destroys bridge, no focus theft (:54); automatic attach/detach, hide sticks within same epoch, keyboard plug hides launcher + bridge (:63); layout switch de (`z` on 21), Ctrl+A then ⌫ clears, fr dead-key `^`+`e` → `ê` (:78); legacy `im-module=simple` path (:87); real pointer click, narrow min width ≤650 (:91); lock suspends/restores (:108); PIN → numbers, shift double-tap locks, hide resets flags (:117); settings save applies live, single settings window, `about` action present (:125); killed helper → reopen works (:139).
Lock: lock-screen/test_lock_ui.py `LockTests.test_touch_keyboard_edits_the_selected_lock_field_without_ipc`.

## 9. Memory / startup
- Idle (no touchscreen, not manual): no windows, no helper — but Python still imports GTK4, layer shell, pyudev, window_backend (~27 MB RSS). Rust: keep udev monitor + GApplication only; create windows lazily.
- `osk_status.py` is a 1 s polling Python process per waydock — replace with inotify or a Rust flag but keep the path waydock hard-codes (waydock/status_backend.py:32) or change waydock simultaneously.
- `waykeys-input` C helper is already native (30 KB); keep it unchanged and reuse its protocol (lock screen and home-assistant-screen bundle depend on it: home-assistant-screen/build_bundle.py:23-46).
