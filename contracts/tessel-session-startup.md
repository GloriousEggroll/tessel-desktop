# Contract: tessel-session-startup

Source: `~/tessel-bluetooth/tessel-session-startup/` (1885 py LOC incl. tests). Mostly **installers** that generate the
labwc session files plus two **runtime** pieces: `tessel-action` (POSIX sh) and
`desktop_suite_failed.py` (crash notifier). A Rust port only needs to replace the runtime helper
(`desktop_suite_failed`) and, optionally, the generators; the *generated files* below are the contract
other components and the user's session depend on. `tessel-action` should stay a shell script
(labwc execs it directly, no runtime cost).

Refs are `file:line` relative to `~/tessel-bluetooth/tessel-session-startup/`.

## 1. Entry points

| Executable | Installed to | CLI | Notes |
|---|---|---|---|
| `configure.py` (legacy migration) | run from tree | `[--apply]` (configure.py:136-139) | Preview writes `.migration.json` plan in source dir (16, 68); apply verifies sha256 of every file vs preview (95-96), backs up to `~/.config/waybar/backups/labwc-startup-<%Y%m%dT%H%M%S%f>/` + `enablement.json` (100-107), `systemctl --user disable` the 7 NAMES (17-18), strips `[Install]` (28-29), daemon-reload, asserts `is-enabled == static` (115-118); rolls back + re-enables on failure (122-132). Restarts polkitman instead of start (49-54). |
| `configure_xdg_autostart.py` | run from tree | `[--apply]` (56) | Plan: env/start blocks in autostart, stop block in shutdown, `XDG_CURRENT_DESKTOP` append `labwc`, install `labwc-xdg-autostart.target`, labwc-excluded copies of `nm-applet.desktop`/`blueman.desktop` (113-139). Backup `~/.local/state/tessel/backups/xdg-autostart-<ts>/` (169). Validates `sh -n` autostart+shutdown (180) and byte-for-byte re-read (181); rollback on any exception. |
| `configure_session.py` (main) | run from tree; `install_all.py` step `session` (install_all.py:111) | `--check` (default) \| `--apply` (mutually exclusive, configure_session.py:699-702) | Writes all files in §3. After apply: `systemctl --user daemon-reload`; if `$LABWC_PID` and rc.xml/menu.xml changed → `labwc --reconfigure` (738-743). Never starts/stops units. Errors → `sys.exit(str(error))` exit 1 (748-752). Backup `~/.local/state/tessel/backups/session-<ts>/` (716). |
| `tessel-action ACTION` | `~/.local/bin/tessel-action` mode 0755 (configure_session.py:628) | one positional | See §2. Exit 2 = usage/unknown action, exit 1 = no handler succeeded. |
| `desktop_suite_failed.py UNIT.service` | `~/.local/share/tessel/session/desktop_suite_failed.py` (+ `suite_i18n.py`, `language_data.py`, `suite_catalogs.py` copied from `shared/`) (configure_session.py:38-39, 616-629) | exactly one arg matching `[A-Za-z0-9:_.\\-]+\.service` else exit 2 + `usage: desktop_suite_failed.py UNIT.service` on stderr (desktop_suite_failed.py:31, 91-95) | Always exit 0 otherwise. |

`install_all.py:110-111` runs `xdg-autostart` and `session` steps; `install_all.py:191` writes a menu item using `tessel-action session-menu`; install_all imports `configure_session` by path (201-213).

## 2. `tessel-action` actions (tessel-action:78-95)

Helpers: `scripts=${XDG_CONFIG_HOME:-$HOME/.config}/waybar/scripts`, `bin=$HOME/.local/bin` (7-8).
`python_app REL args` = `exec /usr/bin/python3 $scripts/REL args` if file exists, else return 1 (16-21).
`supports FILE OPT` = grep for `"OPT"` or `'OPT'` in the installed source (14) — **a Rust replacement
binary must still contain the literal quoted option string or the dispatcher will not pass it**
(or the dispatcher must be changed to probe `--help`).

| Action | Runs (first that works) |
|---|---|
| `terminal` | `xdg-terminal-exec` → `$TERMINAL` word-split, if argv0 exists (args kept) → first of `x-terminal-emulator ptyxis kgx gnome-terminal konsole foot alacritty kitty wezterm ghostty xfce4-terminal tilix terminator xterm` → `lab-sensible-terminal` (23-36) |
| `lock` | `exec ~/.local/bin/tessel-shell-lock` (80) |
| `session-menu` | `tessel-session/session_manager.py --menu` if source supports `--menu`, else `--popup` (72-76) |
| `power-button` | `tessel-power/power_manager.py --power-button` if supported, else session-menu (82-85) |
| `shortcuts` | `~/.local/bin/tessel-settings --shortcuts` if `~/.local/share/tessel-settings/global_settings.py` contains `'--shortcuts'`, else `tessel-settings` (86-90) |
| `play-pause` `pause` `next` `previous` `stop` | `playerctl <action>`; else first `org.mpris.MediaPlayer2.*` from `ListNames` via `gdbus call ... org.mpris.MediaPlayer2.Player.{PlayPause,Pause,Next,Previous,Stop}` on `/org/mpris/MediaPlayer2`; exit 0 if no player (38-51) |
| `volume-up` `volume-down` `toggle-mute` | `tessel-audio/audio_manager.py --<action>`; else `wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+` / `set-volume @DEFAULT_AUDIO_SINK@ 5%-` / `set-mute @DEFAULT_AUDIO_SINK@ toggle` (53-61) |
| `brightness-up` `brightness-down` | `tessel-brightness/brightness_manager.py --<action>`; else `brightnessctl set 5%+` / `5%-` (63-70) |
| other / none | stderr `usage: tessel-action ACTION`, exit 2 (94) |

Consumers: rc.xml block (§3.1), menu.xml terminal items, `install_all.py:191`, Global Settings
`tessel-settings/shortcuts_model.py:151` (decodes `tessel-action <name>` → `SUITE_ACTIONS` descriptions).
**CLI flags of other components this freezes:** tessel-audio `--volume-up --volume-down --toggle-mute`;
tessel-brightness `--brightness-up --brightness-down`; tessel-session `--menu --popup`;
tessel-power `--power-button`; tessel-settings `--shortcuts`; `tessel-shell-lock` (no args).

## 3. Generated files (the real contract)

### 3.1 `~/.config/labwc/rc.xml` keybinding block (configure_session.py:46-78, 172-235)
- One block inside the single `<keyboard>`, inserted right after the last `<default/>` (195-198); creates `<keyboard><default/></keyboard>` if missing (180-187). Markers:
  `<!-- Tessel keybindings begin: generated by tessel-session-startup/configure_session.py.` / `     Keys changed in Global Settings are kept on reinstall. -->` … `<!-- Tessel keybindings end -->` (31-32, 221-225). Rows joined by `"\n    "`.
- Must precede `<!-- Tessel Tiling keyboard begin -->…<!-- Tessel Tiling keyboard end -->` (tessel-tiling) so tiling wins.
- Rows, format `<keybind key="K"><action name="Execute" command="~/.local/bin/tessel-action NAME"/></keybind>`:

| Key | Action |
|---|---|
| `W-l` | Execute `~/.local/bin/tessel-shell-lock` |
| `C-A-Delete` | session-menu |
| `W-Return` | terminal |
| `W-slash` | shortcuts |
| `XF86PowerOff` | power-button |
| `XF86AudioPlay/Pause/Next/Prev/Stop` | play-pause/pause/next/previous/stop |
| `XF86AudioLowerVolume/RaiseVolume/Mute` | volume-down/volume-up/toggle-mute |
| `XF86MonBrightnessUp/Down` | brightness-up/brightness-down |
| `W-C-Left`,`C-A-Left` / `W-C-Right`,`C-A-Right` | `<action name="GoToDesktop" to="left"/>` / `to="right"` |
| `W-1`…`W-9`,`W-0` | `GoToDesktop to="1".."10"` |
| `W-S-1`…`W-S-0` | `SendToDesktop to="1".."10"` |

- Rebind preservation: existing block rows are matched to new rows by canonical action `(tag, sorted attrs, text)` and keep their key (200-209) — this is how Global Settings' rebinding survives reinstall.
- Bindings of a block key outside the block (after `<default/>`, not in comments/tiling block) are removed if "suite" (all actions GoTo/SendToDesktop or Execute containing a `SUITE_COMMANDS` marker, 83-85, 114-126) else kept and that suite row is skipped with note `Kept your own binding for K; the suite shortcut for it is not installed.` (212-219, 646).
- Home paths in `command=` / `<command>` → `~/` (140-149); `<promptCommand>` home → `"$HOME"/`; `<errorCommand>` → `sh -c 'exec "$HOME"/…'` (152-169).
- Rejects `<!DOCTYPE`/`<!ENTITY`, non-`labwc_config` root, multiple `<keyboard>`, managed block outside keyboard, mismatched/reversed markers (94-101, 174-196).
- Theme edit (238-301): `<cornerRadius>12</cornerRadius>`; `<font place="ActiveWindow">` and `InactiveWindow` identical (name/size from existing ActiveWindow, default `Sans`/`10`, weight `SemiBold`); removes `<active>/<inactive>` containing only `titlebar`/`border`.

### 3.2 `~/.config/labwc/menu.xml` (309-319)
Every `command="X"` where X ∈ TERMINALS (86-87) → `~/.local/bin/tessel-action terminal`; home → `~`.

### 3.3 `~/.config/labwc/autostart` (template 324-392, mode kept/0755)
Verbatim template: sources `/etc/profile`, `~/.profile`; ENV_BLOCK; `systemd-run --user --quiet --collect --unit=labwc-graphical-session-anchor.service --property=Type=oneshot --property=RemainAfterExit=yes --property=Requires=graphical-session.target --property=Before=graphical-session.target --property=PartOf=graphical-session.target /usr/bin/true`; `systemctl --user start xdg-desktop-portal-wlr.service xdg-desktop-portal.service`; then backgrounded `systemctl --user start` of `way-displays netman notifications tessel-power tessel-battery tessel-brightness tessel-shell` , **`restart` polkitman**, `start tessel-audio-loopbacks tessel-keep-awake`; START_BLOCK; `"$HOME/.local/bin/tessel-tiling" --restore &`; `start tessel-keyboard.service &`; finally sources `${XDG_CONFIG_HOME:-$HOME/.config}/labwc/autostart.local`.
- ENV_BLOCK (configure_xdg_autostart.py:22-34): appends `labwc` to `XDG_CURRENT_DESKTOP` if absent; `dbus-update-activation-environment --systemd DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE PATH LABWC_PID ${XDG_CONFIG_HOME+XDG_CONFIG_HOME} ${XDG_CONFIG_DIRS+XDG_CONFIG_DIRS}`; `systemctl --user daemon-reload`.
- START_BLOCK (…:35-43): **foreground** `systemctl --user start tessel-dock.service` (tessel-dock is `Type=notify`; readiness = StatusNotifierWatcher up) then `systemctl --user --no-block start labwc-xdg-autostart.target`. → **tessel-dock Rust port must keep `Type=notify` and send `READY=1` only after the tray watcher is owned.**
- Unknown statements move once to `autostart.local` (header 393-395), with their preceding comment lines; statement parser handles `\` continuations, `case/if/for/while/until` nesting, and `# Begin X`…`# End X` blocks (399-447). Identity key for services = `systemctl --user (start|restart) U.service` (446-447).

### 3.4 `~/.config/labwc/environment` (477-490, 521-573)
Template: `XDG_CURRENT_DESKTOP=wlroots:labwc`, `MOZ_ENABLE_WAYLAND=1`, `_JAVA_AWT_WM_NONREPARENTING=1`. Kept in-file: variables `XDG_CURRENT_DESKTOP XKB_DEFAULT_LAYOUT XKB_DEFAULT_VARIANT XKB_DEFAULT_OPTIONS` (under `# Settings written by Global Settings`) and blocks
`# tessel-displays primary output environment begin/end` (way-displays) and
`# Tessel language — managed by Language Settings` … `# End Tessel language` (tessel-language) (495-497). Other vars (last value only) move to `~/.config/labwc/environment.d/90-local.env`. **way-displays & tessel-language ports must keep writing exactly these marker lines.**

### 3.5 `~/.config/labwc/shutdown` (live; from configure.py:61-66 and configure_xdg_autostart.py:44-48)
Blocks `# Begin labwc app service cleanup` (stop the 7 units) and `# Begin labwc XDG session cleanup` (`systemctl --user stop labwc-xdg-autostart.target graphical-session.target`); live file also stops tessel-shell, tessel-keep-awake, tessel-keyboard.

### 3.6 systemd user units (`~/.config/systemd/user/`)
| Unit | Source | Content |
|---|---|---|
| `tessel-shell.service` | `units/tessel-shell.service` (created only if missing, 586-593) | `Type=simple`, `After=graphical-session.target`, `EnvironmentFile=-%h/.config/labwc/environment`, `ExecStart=%h/.local/share/tessel-shell/tessel-shell`, `Restart=on-failure`, no `[Install]` |
| `tessel-notifications.service` | `units/tessel-notifications.service` | `ExecStart=/usr/bin/python3 "%h/.config/waybar/scripts/tessel-notifications/notification_manager.py" --daemon` + env file; live drop-in `tessel.conf` overrides ExecStart, `RestartSec=2` |
| `way-displays.service` | `units/way-displays.service` | `ExecStart=/usr/bin/way-displays`, `Restart=on-failure` (no env file) |
| `tessel-failed@.service` | repo root | `CollectMode=inactive-or-failed`; `ExecStart=/usr/bin/python3 %h/.local/share/tessel/session/desktop_suite_failed.py %i`; `TimeoutStopSec=5` |
| `labwc-xdg-autostart.target` | repo root | `Requires/After=graphical-session.target`, `Wants=xdg-desktop-autostart.target`, `PartOf=graphical-session.target` |
| `<U>.d/50-tessel-recovery.conf` for each existing SUITE_UNIT (579-582: tessel-dock tessel-shell netman tessel-power notifications tessel-battery tessel-brightness polkitman tessel-keep-awake tessel-keyboard tessel-audio-loopbacks) | generated (596-604) | `[Unit] StartLimitIntervalSec=60 StartLimitBurst=5 OnFailure=tessel-failed@%n.service` `[Service] Restart=on-failure RestartSec=1` + `EnvironmentFile=-%h/.config/labwc/environment.d/90-local.env` iff unit/drop-ins mention `/.config/labwc/environment` |
- Units and their non-recovery drop-ins rewritten with `%h/` for home in `ExecStart* ExecStop ExecReload EnvironmentFile WorkingDirectory` (607-613).
- **Swap-in note:** a Rust `tools/install.py` that changes `ExecStart` must keep `%h`, keep unit names, and leave the recovery drop-in (or regenerate it).

### 3.7 XDG autostart overrides
`~/.config/autostart/{nm-applet,blueman}.desktop` copied from first of `~/.config`, `$XDG_CONFIG_DIRS` (default `/etc/xdg`) with labwc excluded (configure_xdg_autostart.py:75-110, 133-138): append `labwc` to `NotShowIn`; or remove from `OnlyShowIn` (sole entry → drop key + `Hidden=true`); `OnlyShowIn=wlroots` → error; other groups (`[Desktop Action …]`) untouched.

## 4. `desktop_suite_failed` runtime contract (desktop_suite_failed.py)
- Skip (exit 0) if `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` socket missing or `systemctl --user is-active --quiet graphical-session.target` fails (40-45).
- Component names (18-30, translated via `shared/locale` `tr`): tessel-dock→Dock, tessel-shell→Desktop shell, netman→Network, tessel-power→Power, notifications→Notifications, tessel-battery→Battery, tessel-brightness→Brightness, polkitman→Authentication agent, tessel-keep-awake→Keep Awake, tessel-keyboard→On-screen keyboard, tessel-audio-loopbacks→Audio loopbacks; unknown → unit minus `.service`.
- D-Bus (session): `org.freedesktop.Notifications.Notify(susssasa{sv}i)` = (`tr('Tessel')`, 0, `dialog-warning`, `tr('{name} stopped working')`, `tr('{name} stopped repeatedly and was not restarted automatically.')`, `['restart', tr('Restart')]`, `{urgency: y 2, desktop-entry: s 'tessel-settings'}`, 0), timeout 10 s (72-82). Subscribes `ActionInvoked`/`NotificationClosed` on `/org/freedesktop/Notifications`; ignores other ids; `restart` → `systemctl --user reset-failed U` + `systemctl --user start --no-block U` (48-50); any matching signal quits; hard quit after 30 min (33, 84).
- GLib.Error from Notify (e.g. notifications is the failed unit) → stderr `U failed; no notification could be shown: …`, exit 0 (99-103).

## 5. External commands spawned
systemctl --user (disable/enable/is-enabled/show/daemon-reload/reset-failed/start/stop), systemd-run, dbus-update-activation-environment, labwc --reconfigure, sh -n, playerctl, gdbus, wpctl, brightnessctl, terminals, python3 component scripts, tessel-settings, tessel-shell-lock, tessel-tiling.

## 6. Parity test cases (port these)
test_configure_session.py: personal startup moves to autostart.local and still runs, sh -n valid (64); env effective values unchanged incl. environment.d (83); reinstall idempotent, local files kept (99); new personal lines appended, never rewritten (110); keybindings portable, waybar-era commands replaced, errorCommand `sh -c 'exec "$HOME"/.local/bin/tessel-prompt --error'` (121-141); menu terminal (144); units use `%h`, recovery drop-in, env-file iff unit reads labwc env, no drop-in for power-manager-lock/tessel-dock when unit absent (151-168); base units created only when missing (170); Global Settings rebind kept (191); user binding never replaced + skipped list (198); tiling block untouched & after suite block (205); missing keyboard section keeps `<default/>` first (214); commented / pre-default bindings untouched (219); theme fonts/corners (229); invalid unit names → 2 (261); session ending not reported (266); restart action only for matching notification id → reset-failed+start (272); action script: playerctl (334), missing $TERMINAL skipped (339), $TERMINAL with args (344), `--menu` only when source mentions it (349), unknown → 2 (358); preview writes nothing (363).
test_configure_xdg_autostart.py: desktop identity idempotent & order-preserving (54); malformed managed block raises (61); app exclusion keeps other desktops & action groups (67); optional XDG paths imported only when set (87); preview/install idempotent, no activation (122); **autostart waits for tessel-dock start before target** (147); failed validation restores files and removes new target (172).
test_xdg_generator.py (uses `/usr/lib/systemd/user-generators/systemd-xdg-autostart-generator`): user overrides system (74), Hidden masks (84), missing TryExec excludes (89), desktop match on colon list (94), GNOME phase/`X-systemd-skip` (105), Exec literal (110), unit bound to graphical-session (120).

## 7. Wayland
None used directly (only checks `$WAYLAND_DISPLAY` socket existence).
