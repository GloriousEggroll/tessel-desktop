# tessel-power — external contract

Python source: `~/tessel-bluetooth/tessel-power/` (refs `file:line` relative to it unless prefixed).
Role: resident session power policy: power-profile popup + Power Settings window, owner of the single
`swayidle` child (dim → lock → display-off → sleep), logind lid/power-key takeover, low/critical battery
notifications + critical action, click-through dim overlay. Also ships **shared Python libraries**
(`power_backend.py`, `power_config.py`, `power_client.py`, `power_ui.py`, `power_style.py`,
`power_battery_ui.py`, `power_charge_limits.py`) imported by tessel-battery, tessel-brightness,
tessel-settings and tessel-lock (§10). Baseline RSS ≈ 31 MB (+ one `power_status.py` stream per dock).

## 1. Entry points & processes

| Item | Value |
|---|---|
| Main executable | `~/.config/waybar/scripts/tessel-power/power_manager.py` (run as `/usr/bin/python3 <path>`) |
| GApplication id | `org.tessel.PowerManager` (`power_manager.py:30`); `GLib.set_prgname(app_id)` (`:67`) → Wayland app_id `org.tessel.PowerManager` |
| Flags | `HANDLES_COMMAND_LINE`; `IS_LAUNCHER` only for `--check`, `--idle-action`, `--idle-resume`, `--power-button` (`:570-574`). **`--popup`/`--settings`/none without a running primary become the primary** (full idle manager starts). |
| Parsing | `do_local_command_line → False`: all argv parsed in the primary (`:138-160`). |
| Status stream | `power_status.py` (separate process; own `PowerBackend`; prints a JSON line whenever it changes; exits on SIGTERM/SIGINT or BrokenPipe) (`power_status.py:1-61`) |
| Main unit | `~/.config/systemd/user/tessel-power.service`: `Type=simple`, `After/PartOf=graphical-session.target`, `EnvironmentFile=-%h/.config/labwc/environment`, `ExecStart=/usr/bin/python3 %h/.config/waybar/scripts/tessel-power/power_manager.py --background`, `Restart=on-failure`, `RestartSec=2`. Static (no `[Install]`). Generated at `install_waybar.py:118-130`. |
| Recovery drop-in | `tessel-power.service.d/50-tessel-recovery.conf` (same template as other suite units: StartLimitBurst=5/60 s, `OnFailure=tessel-failed@%n.service`, `RestartSec=1`, env `labwc/environment.d/90-local.env`); failure label "Power" (`tessel-session-startup/desktop_suite_failed.py:22`). |
| Lock unit (consumed, not owned) | `tessel-lock.service` — owned by tessel-lock (`tessel-lock/tessel-lock.service`: `Type=notify`, `NotifyAccess=main`, `ExecStart=/usr/bin/python3 %h/.local/share/tessel-lock/lock_screen.py`, `SuccessExitStatus=75`, `TimeoutStartSec=30s`, `StartLimitIntervalSec=0`). Fallback text when tessel-lock absent: `Type=forking` running `%h/.config/waybar/scripts/screenlock.sh` (`install_waybar.py:118-125`). |
| Autostart | labwc autostart `systemctl --user start tessel-power.service >/dev/null 2>&1 &` (`tessel-session-startup/configure_session.py:363`; `install_waybar.py:105-115` migrates the legacy swayidle block). Battery/brightness installers anchor their lines after it. |
| .desktop | `~/.local/share/applications/org.tessel.PowerManager.desktop`: `Name=Power Settings`, `Comment=Power profiles, display-off and sleep behavior`, `Exec=/usr/bin/python3 <abs>/power_manager.py --settings`, `Icon=preferences-system-power-management`, `Categories=Settings;HardwareSettings;` (`install_waybar.py:191-195`). Also used as notification `desktop-entry` hint. |
| Signals | SIGTERM/SIGINT → quit (`:132-133,542-544`). Shutdown closes idle child, alerts, logind FDs, dim overlay, backend. |

### CLI (`power_manager.py:38-57`) — mutually exclusive modes
| Flag | Behaviour |
|---|---|
| `--background` | primary, no UI |
| `--popup` / none | profile popup at `--popup-edge {top,bottom,left,right}` (default bottom) `--popup-offset N` (≥0, default 76). Different position → recreate, same → activate (`:479-490`). |
| `--settings` | dismiss popup, open/raise Power Settings window; raise via `wlrctl toplevel focus app_id:org.tessel.PowerManager title:<title>`, retried up to 4× every 100 ms (`:492-540`) |
| `--check` | print health JSON, exit 0 iff `ready` (`:146-149,162-177`) |
| `--idle-action A --policy-token T` | A ∈ `dim, lock, display-off, display-on, suspend, hibernate, suspend-then-hibernate`; token required (argparse error "Idle events require --policy-token") (`:290-315`) |
| `--idle-resume A --policy-token T` | (`:317-331`) |
| `--power-button` | debounced 1.5 s power-key handler (`:345-356`) |

`--check` JSON: `{"ready","profiles_ready","power_ready","sleep_ready","idle_ready","has_battery","active_profile","batteries":[…],"charge_limit":{…},"lid_handled","power_key_handled","error"}`; `ready = profiles&&power&&sleep&&idle.ready&&config_valid`.

## 2. IPC

### 2.1 D-Bus owned (session)
- `org.tessel.PowerManager` at `/org/tessel/PowerManager` (GApplication) +
  `org.tessel.PowerManager.Controls` via `shared/service_settings.py` `SettingsService` (`power_manager.py:126-129`, `power_client.py:7`):

| Member | Sig | Semantics |
|---|---|---|
| `GetState()` | `→ s` | JSON `{**health(), "settings": <validated settings>, "ready": !stopping && backend && idle}` (`:234-239`) — NB `ready` overridden (transport readiness, not health) |
| `SetSettings(s)` | `s → (b, s)` | s = JSON object; non-dict → `org.tessel.PowerManager.Controls.Error` "Invalid power settings."; else `save()` → `(ok, self.error)` (`:241-246`) |
| signal `Changed(s)` | `s` | same JSON; on every `_changed`/`_idle_error` (`:220-258`) |

### 2.2 D-Bus consumed
System bus (`power_backend.py`, async; read timeout 10 s, mutations 120 s non-cancellable, `ALLOW_INTERACTIVE_AUTHORIZATION` when interactive, `:352-372`):

| Service | Usage |
|---|---|
| power-profiles-daemon `org.freedesktop.UPower.PowerProfiles` `/org/freedesktop/UPower/PowerProfiles`, fallback `net.hadess.PowerProfiles` `/net/hadess/PowerProfiles` (`:16-20`) | `Properties.GetAll` → `Profiles` (aa{sv} `Profile`), `ActiveProfile`, `PerformanceDegraded` or `PerformanceInhibited`; `PropertiesChanged` (debounced 100 ms); `Properties.Set(ss v)` `ActiveProfile` interactive (`:469-500,612-626`) |
| UPower `org.freedesktop.UPower` `/org/freedesktop/UPower` | `GetAll` (`OnBattery`), `EnumerateDevices`, per-device `GetAll` `org.freedesktop.UPower.Device`; `PropertiesChanged` on any path; `DeviceAdded/DeviceRemoved` (`:289-304,405-447,502-563`); `EnableChargeThreshold(b)` per device (`:628-714`) |
| logind `org.freedesktop.login1` Manager | `CanSuspend/CanHibernate/CanSuspendThenHibernate` (→ yes/challenge/no/na) (`:565-589`); `ListInhibitors` `a(ssssuu)` (`:793-807,831-879`); `GetSession(s)` with `$XDG_SESSION_ID` or `"auto"`, then Session `GetAll` (`Active`, `Remote`, `Type`) (`:809-829`); `Suspend/Hibernate/SuspendThenHibernate/PowerOff(b interactive)`; `CanPowerOff`; signal `PrepareForSleep(b)` (false → refresh) (`:449-451`); `NameOwnerChanged` for all endpoints |
| logind (`power_logind.py`) | `Inhibit(ssss)→h` `("handle-lid-switch", "Power Manager", "Applying the desktop's lid close settings", "block")` and `("handle-power-key", "Power Manager", "Applying the desktop's power button settings", "block")` (`:97-150`); `Properties.Get` `LidClosed`, `Docked`; UPower `LidIsPresent`; `PropertiesChanged` on Manager |
| KDE KAuth helper `org.kde.powerdevil.chargethresholdhelper` `/` `org.kde.kf6auth.performAction(saya{sv}aya{sh})` | actions `…getthreshold`, `…setthreshold`; QDataStream QVariantMap of qint32 `chargeStartThreshold`/`chargeStopThreshold`; reply decoding (`power_charge_limits.py:107-194`). Only when helper files exist (`:21-25`). Used from the battery UI (tessel-battery). |
Session bus: `org.freedesktop.Notifications` `Notify(susssasa{sv}i)` app_name `tr("Power")`, hints `urgency`(y), `desktop-entry="org.tessel.PowerManager"`, `category="device"`, `resident=true` if urgency 2, expire 0 for critical else -1; `CloseNotification(u)`; subscribes `ActionInvoked` (`power_alerts.py:25-91`).

Reconnect: system bus failure retried every 5 s (`power_backend.py:262-287`); fallback full refresh every 30 s (120 s in `batteries_only` mode) (`:453-467`).

### 2.3 swayidle child protocol (`power_idle.py`)
Spawned (Popen, stdin/stdout devnull) as:
```
swayidle -w -C /dev/null
  [timeout DIM  '<py> <dir>/power_manager.py --idle-action dim --policy-token T'
     resume '<py> …/power_manager.py --idle-resume dim --policy-token T </dev/null >/dev/null 2>&1 &']
  [timeout LOCK  …--idle-action lock…  resume …--idle-resume lock… &]
  [timeout OFF   …--idle-action display-off…  resume '…--idle-resume display-off… & /usr/bin/wlopm --on "*"']
  [timeout SLEEP …--idle-action <sleep_action>… resume …&]
  lock '/usr/bin/systemctl --user start tessel-lock.service'
  before-sleep '<same>'  after-resume '/usr/bin/wlopm --on "*"'
```
(`power_idle.py:80-145`; args are `shlex.join`ed with absolute `shutil.which` paths; `<py>` = `sys.executable`.)
- Token: `secrets.token_urlsafe(24)`, regenerated on every respawn, compared with `compare_digest` (`:104,148-165`).
- Respawn only when the policy signature changes (source known/battery/on_battery/source, lock_after, display_off, sleep_after, sleep_action, supported caps, dim) (`:51-68,80-86`).
- `dim_delay`: first of (lock_after, display_off) non-zero; dim at `first-30` if first>60 else `first//2`; 0 if disabled (`:21-30`).
- Sleep timer only when source known and capability == `"yes"` (never `challenge`) (`:57-61`).
- Requires `swayidle`, `wlopm`, `systemctl` and executable `~/.config/waybar/scripts/screenlock.sh` (exists check only) else error (`:93-102`).
- Old child: SIGTERM, SIGKILL after 1 s; reaped by `GLib.child_watch_add`; unexpected exit → error "Idle power management stopped unexpectedly (exit status N)." (`:167-245`).

### 2.4 Other
No sockets/FIFOs/lock files. Held FDs: logind handle-lid-switch / handle-power-key inhibitors.

## 3. Files
| Path | R/W | Notes |
|---|---|---|
| `${XDG_CONFIG_HOME:-~/.config}/tessel-power/settings.json` (`power_config.py:34-35`) | RW (writers: this service; GS and tessel-lock only via `SetSettings`) | missing → DEFAULTS (not written); >64 KiB or invalid → error, file untouched, idle controller not started (`:92-102`, `power_manager.py:95-100`). Write: validate, `mkstemp(".power-settings-")`, `json.dump(indent=2, ensure_ascii=False)`+`\n`, fsync, replace, fsync dir (`:105-127`). |
| `~/.config/labwc/rc.xml` | R + file monitor | regex `<keybind … key="…XF86PowerOff…"` decides holding `handle-power-key` (`power_logind.py:22-31,50-59,100-114`) |
| `~/.config/waybar/scripts/screenlock.sh` | existence/exec check | content now `exec /usr/bin/systemctl --user start tessel-lock.service` |
| `${XDG_STATE_HOME}/tessel/previews/tessel-power.json` | installer manifest | |
| `/sys/class/power_supply/*/{type,charge_control_start_threshold,charge_control_end_threshold,scope,present,manufacturer,model_name,serial_number}` | R | `power_charge_limits.py:43-100` |
| `/usr/libexec/kf6/kauth/chargethresholdhelper`, `/usr/share/dbus-1/system-services/org.kde.powerdevil.chargethresholdhelper.service`, `/usr/share/polkit-1/actions/…policy` | existence | |

settings.json schema (`power_config.py:10-31`), DEFAULTS:
```json
{"version": 1, "lock_after": 300,
 "ac": {"display_off": 600, "sleep_after": 0, "sleep_action": "suspend"},
 "battery": {"display_off": 600, "sleep_after": 900, "sleep_action": "suspend"},
 "dim_before_lock": true, "lid_close": {"ac": "suspend", "battery": "suspend"},
 "power_button": "menu", "critical_action": "auto", "critical_level": 0}
```
Validation: unknown keys rejected (top & sub-dicts); version must be int 1; seconds int 0..86400 (bool rejected via `type is int`); sleep_action ∈ suspend/hibernate/suspend-then-hibernate; lid ∈ suspend/hibernate/lock/nothing/poweroff; power_button ∈ menu/suspend/hibernate/poweroff/nothing; critical_action ∈ auto/hibernate/poweroff/suspend/nothing; critical_level ∈ {0,2,3,4,5,7,10}; partial input filled from DEFAULTS. Error message strings at `:40-88` surface to UI.
`save()` extra rule: rejects *newly changed* sleep settings whose action capability ≠ "yes" ("That automatic sleep mode is not available on this system.") but preserves an unchanged unsupported saved mode (`power_manager.py:260-288`).

## 4. Behaviour
- Idle action (`:290-315`): token+policy check (`allows`) → `backend.idle_inhibited` (ListInhibitors: any `block`/`block-weak` with `idle` in `what`; errors ⇒ inhibited; then session must be Active, !Remote, Type wayland/x11) → re-check token/generation → action: sleep via `backend.sleep(interactive=False, guard)` (re-checks inhibitors incl. `sleep`), `dim` → overlay, `lock` → `systemctl --user start tessel-lock.service`, display-off/on → `wlopm --off|--on *`.
- Idle resume (`:317-331`): `dim` always restores (even revoked token); others bump the generation (cancelling in-flight checks); `display-off` resume also requests wake (`wlopm --on *`), serialized after any in-flight display command (`:445-477`).
- Commands are de-duplicated per action (`_commands` set).
- Lid close (logind `LidClosed` false→true edge, `Docked` false) → `lid_close[source]` (`:339-343`, `power_logind.py:152-162`). Lid inhibitor held unless UPower `LidIsPresent` is false.
- Power button: `menu` → `systemd-run --user --collect --quiet --service-type=exec --slice=app.slice --expand-environment=no --setenv=… -- <py> …/tessel-session/session_manager.py --popup` (`:423-443`); else `_explicit(action, interactive=True)`.
- `_explicit` (`:361-416`): `lock` → lock unit; `poweroff` → `explicit_power`; sleep actions → start lock unit and wait (≤15 s) for success, then sleep; failed lock cancels lid/button sleep ("The screen could not be locked, so the computer was not put to sleep.") but NOT critical-battery. Explicit actions ignore app idle inhibitors but need capability (`yes`, or `challenge` if interactive) + active local session.
- Battery alerts (`power_alerts.py:101-219`): only while power_ready && source known && has_battery && on_battery. Low at ≤10 % or UPower WarningLevel≥3 (urgency 1, `battery-low-symbolic`); critical ≤5 % or WL≥4 (urgency 2, `battery-caution-symbolic`); each once per discharge cycle; action due at WL≥5 (level 0) or ≤critical_level → 60 s countdown notification (`battery-empty-symbolic`, action `cancel`/"Cancel") then perform `resolve_action` (`auto` → hibernate if CanHibernate=="yes" else poweroff). Plugging in (source known, not discharging) resets everything and closes notifications; unknown source neither resets nor warns. Notifications reuse `replaces_id` per key.
- Dim overlay (`power_dim.py`): one layer-shell window per monitor, namespace `power-manager-dim`, OVERLAY, anchors all edges, exclusive -1, keyboard NONE, empty input region (click-through), black alpha 0.55, CSS class `power-dim-overlay`, title "Dim screen". Never touches backlight.
- Bluetooth shortcut (battery rows): `systemd-run … -- <py> <root>/tessel-bluetooth/tessel-bluetooth.py` (or `<root>/tessel-bluetooth.py`), gated on both existing (`:179-218`).

## 5. UI surfaces
- **Popup** `PowerPopup` (`power_ui.py:173-245`): `Gtk.ApplicationWindow` title "Power", `shared/tray_popup.py` `PopupController` (layer-shell OVERLAY `desktop-applet`, click-away), `popup_header` icons (`power-profile-balanced-symbolic`, `preferences-system-power-symbolic`), title "Power", subtitle "Profiles & sleep settings", gear `header_button("emblem-system-symbolic", "Power settings")` → dismiss + settings. "Power mode" section + `ProfileSelector` (buttons `profile-row`, `.selected`, check icon `profile-check`; profiles power-saver/balanced/performance with `power-profile-*-symbolic` icons) — refresh never writes; click writes. Status label (`status`), "Updating power settings…" while busy.
- **Settings window** `PowerSettingsWindow` (`power_ui.py:270-555`): normal toplevel, title "Power Settings", default 580×760, min 500×460, headerbar; cards: Power profile; Plugged in / "On battery" (literal untranslated) with Turn off display after / Sleep after / Sleep mode; Screen lock (Lock after, Dim screen before locking switch); Lid and power button; Low battery (Critical battery action, Take action); Close / Save Changes (`primary`). Edits are local until Save; Escape/Close discards. Duration choices `0,60,120,300,600,900,1800,2700,3600,7200,10800,21600,43200,86400` + any custom saved value (`:36,103-124`). Sleep-mode rows suffixed " — authorization required"/" — checking…"/" — unavailable" (untranslated) and disabled unless `yes` (`:247-267`). Attributes used by GS subclass/search: `controls[key][field]`, `lock_choice`, `dim_switch`, `save_button`, `status_label`, `settings`, `_saved`, `_syncing`, `_message`, `_destroyed`, `_validation_error()`, `sync_extra_controls()`, `_on_destroy()`.
- CSS (`power_style.py:24-107`): `@define-color power_accent #3584e4`, `power_surface`, `power_muted`, `power_line`, `power_error`; classes `powermanager` (window), `tray-popup`, `popup-surface`, `title`, `subtitle`, `section-title`, `row-title`, `muted`, `status`, `error`, `card`, `power-tile`, `profile-row`, `selected`, `profile-check`, `battery-row`, `battery-percentage`, `battery-level` (levelbar), `primary`, `icon-button`. Theme attach via `companion_theme.attach`. Icon fallback table `power_style.py:122-130`.
- Status-stream JSON (`power_status.py:18-25`): `{"text":"","tooltip":"Power profile: <name>[\nPerformance limited: <reason>]\nClick for power profiles · Right-click for settings","class":["<profile>"|"unavailable"]}`; never mentions batteries.
- Notifications: §4.

## 6. External commands
| Command | Where |
|---|---|
| `swayidle -w -C /dev/null …` | `power_idle.py:106-127` |
| `systemctl --user start tessel-lock.service` | `power_manager.py:33`, swayidle lock/before-sleep |
| `wlopm --off '*'` / `wlopm --on '*'` | `:312,452`; swayidle after-resume/resume |
| `wlrctl toplevel focus app_id:… title:…` | `:521-528` |
| `systemd-run --user --collect --quiet --service-type=exec --slice=app.slice --expand-environment=no --setenv=WAYLAND_DISPLAY,DISPLAY,XAUTHORITY,XDG_RUNTIME_DIR,DBUS_SESSION_BUS_ADDRESS,XDG_CURRENT_DESKTOP,XDG_SESSION_TYPE -- …` | session menu `--popup`, tessel-bluetooth (`:196-203,429-435`) |
| self (`<py> power_manager.py --idle-action/--idle-resume`) | from swayidle |

## 7. i18n
`shared/suite_i18n.py`, catalogs `shared/locale/<code>.json` (English keys). `tr()` in `power_manager.py`, `power_ui.py:29-44,73-112,300-555`, `power_status.py:13-21`, `power_alerts.py:77,151-182`, `power_client.py:13`. Untranslated literals to keep verbatim: "On battery" card title (`power_ui.py:303`), sleep-mode suffixes, internal error strings from backend/idle/config (mapped by `friendly_message` substring table `power_ui.py:83-90` — substrings "screen lock command is missing", "requires swayidle", "Idle power management stopped", "Could not start idle power management" must stay stable).

## 8. Install layout (`install_waybar.py`)
- Dest `~/.config/waybar/scripts/tessel-power/`: `power_manager.py power_backend.py power_config.py power_idle.py power_ui.py power_battery_ui.py power_charge_limits.py power_style.py power_status.py tray_popup.py README.md power_alerts.py power_logind.py power_dim.py gtk4_utils.py companion_theme.py suite_i18n.py language_data.py suite_catalogs.py service_settings.py power_client.py background_blur.py libwaydock-background-blur.so popup_placement.py` (`:27-61`). `shared/install_gtk4.py:41-52` refreshes all non-test `*.py` + `service_settings.py`.
- Units `tessel-power.service`, `tessel-lock.service` (new-only staging), `.desktop`, autostart migration; legacy Waybar configs get `custom/power` module (exec `power_status.py`, on-click `--popup --popup-edge <bar edge> --popup-offset 76`, right-click `--settings`) + `#custom-power.{performance,balanced,power-saver,unavailable}` CSS (`:41-53,79-102`).
- Apply: start/restart unit, poll `--check` up to 15×0.3 s; rollback on failure; then SIGTERM the exact legacy swayidle argv; SIGUSR2 to `waybar` processes. `install_gtk4` child audit tolerates a `swayidle` child of the unit (`shared/install_gtk4.py:265`).

## 9. Consumers / cross-component
| Who | How |
|---|---|
| tessel-dock `power` indicator | streams `/usr/bin/python3 -u …/tessel-power/power_status.py`; restart 3 s after exit (`tessel-dock/status_backend.py:26,272-327`) |
| tessel-dock clicks | `systemd-run … -- <py> power_manager.py --popup --popup-edge E --popup-offset N` / `--settings` (`tessel-dock/panel_actions.py:14,46-50`) |
| labwc XF86PowerOff | `~/.local/bin/tessel-action power-button` → **greps power_manager.py source for `"--power-button"`** then `python3 power_manager.py --power-button`, else session menu (`tessel-action:82-85`, `tessel-session-startup/configure_session.py:54`) |
| Global Settings Power page | imports `power_client.PowerClient`, `power_config`, `power_ui.PowerSettingsWindow` (subclassed) and its own `power_backend.PowerBackend` (`tessel-settings/applet_pages.py:458-545`); saves via `SetSettings`; also `applet_pages.py:224` uses PowerBackend; search entries `tessel-settings/settings_search.py:92-96,150-152` reference labels/attrs |
| tessel-lock settings | `power_client.PowerClient` read `state.settings.lock_after`, write via `set_settings` (`tessel-lock/lock_settings.py:9,219-277`) |
| tessel-battery | imports `power_backend.PowerBackend(batteries_only=True)`, `power_battery_ui.BatterySection`, `power_style`, `power_charge_limits` (`tessel-battery/install_waybar.py:30`, `battery_ui.py:22-23`) |
| tessel-brightness | `power_backend.PowerBackend` as read-only logind/inhibitor/session guard (`tessel-brightness/brightness_manager.py:80-90`) |
| tessel-keep-awake | its logind `idle:sleep` block inhibitor blocks automatic dim/lock/display-off/sleep here |
| tessel-lock | `tessel-lock.service` Type=notify readiness = "session locked"; restarting tessel-power must not kill it |
| tessel-session | launched for power-button `menu` |

## 10. Parity test cases (from tests)
- `test_power_config.py`: defaults detached copies; round-trip w/ restrictive perms; reject bad types/versions/actions/bounds; existing file keeps battery sleep 0; lid/button/critical validation; failed replace keeps old file and removes temp; malformed file reported, not overwritten; XDG_CONFIG_HOME respected.
- `test_power_idle.py`: default argv (`dim 270, lock 300, display-off 600`, prefix `swayidle -w -C /dev/null`, token suffix, lock/before-sleep/after-resume exact); every timeout has resume forwarding with ` </dev/null >/dev/null 2>&1 &` (+wake for display-off); unchanged refresh doesn't respawn; source switch respawns + old token rejected; unknown source / non-`yes` caps never arm sleep; disabling timers keeps lock/before-sleep; missing deps/locker → error w/o spawn; spawn fail/child exit revoke token; close idempotent; replacement never blocks, hung child SIGKILLed after 1 s; child watch reaping; dim before lock/off & disableable; quoting.
- `test_power_app.py`: settings editor works w/o profiles; stale policy no action; inhibited display stays on; resume cancels pending sleep/lock/display-off; display wake waits for in-flight off and requeues; lock via independent service; Keep Awake inhibitor blocks auto-lock; unavailable auto-sleep not saved; repairing invalid config starts idle; unsupported saved sleep choice preserved; dim respects inhibitors, restore works with revoked token; lid uses source policy + lock before sleep; failed lock cancels lid sleep not critical; power button menu/action + 1.5 s debounce; idle flags need token; tooltip excludes batteries; bluetooth launch w/o shell, gated.
- `test_power_backend.py`: async read-only start; subsystems independent + `net.hadess` fallback; profile write = interactive Properties.Set; batteries ignore peripherals, weight by energy; warning level only from system batteries; explicit actions ignore idle inhibitors but need capability+session; peripherals don't create battery policy; charge-limit revalidate/readback/hotplug/service-loss/denial/unchanged; UPS power source; charge/peripheral signals don't reset idle; failed device read never assumes AC; stale profile reply ignored; service loss invalidates; sleep re-reads inhibitors; auto sleep disallows `challenge` and query failure; logind loss/activity cancels sleep; idle guard fail-closed, only `idle` blocks display; inactive/remote/unknown sessions block; bus close → reconnect; close suppresses callbacks; device filtering & sentinel values; inhibitor subjects/modes (`block` and `block-weak`).
- `test_power_logind.py`: lid inhibitor unless LidIsPresent false; power-key follows rc.xml keybind, late grant after unbind released; lid edge reports docked.
- `test_power_alerts.py`: low→critical once/cycle; warning levels w/o percentage; countdown then act; cancel/plug-in stop countdown; configured percentage & `nothing`; unknown source no reset; auto prefers unattended hibernate; power-key inhibitor needs compositor binding.
- `test_power_charge_limits.py`: QDataStream encoding; reply decoding (success/denied/busy/truncated); start-threshold adjustment; helper-file gating; fail-closed mappings; start-only companion blocks writes.
- `test_power_ui.py`: header icons; popup refresh read-only; busy/rejection honest; local edits until Save; hidden battery policy & custom seconds preserved; challenge/unsupported modes disabled w/o normalizing; profile change preserves unsaved edits; save failure keeps edits; dim overlay covers outputs, passes input; remote reload of older files; battery widgets (shared with tessel-battery).

## 11. Memory / startup
- Primary imports Gtk at top; `power_backend`, `power_idle`, `power_alerts`, `power_logind`, `power_dim` (cairo + layer-shell) at startup; UI modules lazily. Each dock spawns its own `power_status.py` Python process (≈ +15–20 MB each) with a full PowerBackend — a Rust port should make the stream binary tiny (gio only, no GTK) and keep its stdout protocol.
- All D-Bus I/O async; `--check` may be `ready:false` for the first seconds.

## 12. Riskiest parity points
1. **Python-coupled consumers**: GS, tessel-lock, battery- and tessel-brightness import power_*.py modules directly; callers run `/usr/bin/python3 power_manager.py`, and `tessel-action` greps its *source* for `"--power-button"`. The Rust swap must keep these Python modules/shims (or port consumers simultaneously).
2. swayidle token/resume protocol and fail-closed inhibitor/session checks (security: never auto-sleep/lock when inhibited, never with stale tokens; display wake must work even if the primary is busy/dead).
3. Exact settings.json schema/validation + `GetState`/`SetSettings` JSON (`ready` override, `settings` key) and lock-before-sleep semantics with `tessel-lock.service` readiness.
