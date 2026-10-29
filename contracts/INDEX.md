
## Wave 1/2 services (keep-awake, battery, power, notifications, OSK, audio, brightness, polkitman)
| Component | Contract | Bus name / interface | Unit |
|---|---|---|---|
| keep-awake | [keep-awake.md](keep-awake.md) | `org.waybar.KeepAwake` · `.Controls` (GetState/Changed, SetEnabled/SetDuration) | keep-awake.service |
| battery-manager | [battery-manager.md](battery-manager.md) | GApplication + KAuth charge-limit helper | battery-manager.service |
| power-manager | [power-manager.md](power-manager.md) | `org.waybar.PowerManager.Controls` (GetState/Changed, SetSettings(s)) | power-manager.service (+ starts power-manager-lock.service) |
| notification-manager | [notification-manager.md](notification-manager.md) | `org.freedesktop.Notifications` (DO_NOT_QUEUE) + `org.waydock.Notifications1` | notifications.service drop-in (replaces dunst) |
| on-screen-keyboard | [on-screen-keyboard.md](on-screen-keyboard.md) | `org.waydock.OnScreenKeyboard`; `$XDG_RUNTIME_DIR/waykeys/state.json` | on-screen-keyboard.service |
| audio-manager | [audio-manager.md](audio-manager.md) | `org.waybar.AudioManager` (GApplication cmdline) | audio-manager-loopbacks.service |
| brightness-manager | [brightness-manager.md](brightness-manager.md) | `org.waybar.BrightnessManager.Controls` | brightness-manager.service |
| authentication-manager | [authentication-manager.md](authentication-manager.md) | polkit agent for LABWC_PID session; BlueZ Agent1 at `/org/wayshell/BluetoothAgent` | polkitman.service (Type=notify) |

**Cross-component interfaces**
- waydock (`status_backend.py`, `panel_actions.py`) streams JSON-line `*_status.py` scripts (power, battery, brightness, notifications, osk), reads keep-awake through `SettingsClient`, calls `wpctl` directly for volume, and starts popups with `systemd-run … <app>.py --popup --popup-edge E --popup-offset N` or `--settings` (OSK uses `--toggle`/`--settings`; keep-awake right-click runs `--toggle`).
- Global Settings imports Python modules from `~/.config/waybar/scripts/<app>/`: audio_ui/audio_backend, battery_ui, power_ui/power_backend/power_client/power_config, keep_awake_ui/client, notification_ui.SettingsPage, osk_settings. **These Python files must stay installed until Global Settings is ported.**
- Python imports between components: battery-manager imports power_backend, power_battery_ui, power_style and power_charge_limits; brightness-manager imports PowerBackend for its inhibitor check; way-displays-gui loads brightness_client.py.
- Inhibitors: keep-awake takes a logind `idle:sleep` block inhibitor. power-manager and brightness-manager find it with ListInhibitors and skip dim, lock and sleep.
- Two separate auto-dim paths can both act: power-manager's overlay and brightness-manager's swayidle. Both use a swayidle token protocol.
- lock-screen: power-manager starts `power-manager-lock.service` and waits for it to be ready before sleep. lock_settings goes through PowerClient. Lock-screen reads `org.waydock.Notifications1` GetState/StateChanged. OSK watches the lock unit's ActiveState and uses `waykeys-input --labels`.
- labwc keybinds and `desktop-suite-action` run `audio_manager.py --volume-up/--volume-down/--toggle-mute`, `brightness_manager.py --brightness-up/--brightness-down` and `power_manager.py --power-button`. Suite recognition greps the script path and source, so Rust binaries need shims at the same paths.
- brightness-manager ↔ way-displays: `$XDG_RUNTIME_DIR/way-displays.sock`. It also reads `~/.cache/waydock/palette.json`.
- These components send notifications: power alerts, desktop_suite_failed (critical, with a `restart` action), bttransfer, wayshell and capture-manager (hints `x-waydock-output` and `x-waydock-workspace`).
- polkitman's BlueZ agent serves the Bluetooth app (btmanager).

## Wave 2/3 components (network, calendar, weather, media, discovery, tiling, session, way-displays, language, session-startup, bluetooth, wayshell, waydock)
| Component | Contract | Bus name / socket | Unit / launch |
|---|---|---|---|
| network-manager (netman) | [network-manager.md](network-manager.md) | GApplication; NM SecretAgent (RegisterWithCapabilities, re-register on NM restart); `--check` probe | netman.service (static, autostart) |
| calendar-manager | [calendar-manager.md](calendar-manager.md) | GApplication (resident after first launch); timedated | systemd-run popup from waydock |
| weather-manager | [weather-manager.md](weather-manager.md) | GApplication; `--tray` = SNI + dbusmenu | popup (exits on close); weather-manager.service (HA-screen bundle) |
| media-manager | [media-manager.md](media-manager.md) | `org.waydock.Media`; MPRIS client | systemd-run popup |
| network-discovery | [network-discovery.md](network-discovery.md) | `org.waydock.NetworkDiscovery`; systemd/avahi units via D-Bus, pkexec | systemd-run popup |
| session-manager | [session-manager.md](session-manager.md) | `org.waydock.Session`; logind inhibitors | desktop-suite-action (C-A-Delete, XF86PowerOff) `--menu` |
| tiling-manager | [tiling-manager.md](tiling-manager.md) | `$XDG_RUNTIME_DIR/waytile-$WAYLAND_DISPLAY.sock` (dgram JSON v1, server in labwc C) | waydock `--toggle`/`--watch` |
| way-displays | [way-displays.md](way-displays.md) | `$XDG_RUNTIME_DIR/way-displays.sock` (YAML); `org.waybar.BrightnessManager` | way-displays.service |
| language-manager | [language-manager.md](language-manager.md) | localed; writes `desktop-suite/language.json` | launcher opens Global Settings `--section language` |
| session-startup | [session-startup.md](session-startup.md) | desktop-suite-action, `desktop-suite-failed@.service`, `labwc-xdg-autostart.target` | labwc autostart |
| bluetooth | [bluetooth.md](bluetooth.md) | BlueZ, obexd (OBEX transfer + incoming Agent) | waydock popup; embedded in Global Settings |
| wayshell | [wayshell.md](wayshell.md) | owns `com.nobaraDesktopView`; `$XDG_RUNTIME_DIR/wayshell.sock` (JSON line) | wayshell.service (fix ExecStart in palette.conf drop-in) |
| waydock | [waydock.md](waydock.md) | `org.waydock.Waydock`; `/org/waydock/Settings` `org.waydock.Settings`; StatusNotifierWatcher + host | waydock.service (Type=notify; READY after tray host) |

**Cross-component interfaces (wave 2/3)**
- **Popup placement:** every tray popup (netman, calendar, weather, media, discovery, session, bluetooth=`service_apps`) calls `org.waydock.Settings.GetPopupPlacement(ss)→s` (JSON). If that fails, it falls back to `--popup-edge E --popup-offset N`. Port `shared/tray_popup` (PopupController, PointerMonitor) and `popup_placement` once, in suite-gtk.
- **waydock streams `*_status.py --watch` JSON lines** from network_status, weather_status and tiling `--watch`. The Discovery backend is a byte-identical copy that runs in-process inside waydock.
- **MPRIS client** `lock-screen/lock_media*.py` is shared by media-manager, waydock media_status/media_controls and lock-screen. Port it once.
- **Helper `waydock-toplevel`** (foreign-toplevel, JSON stdio) is used by waydock, session-manager (`close <id>`) and wayshell (`--snapshot`).
- **wayshell.sock** commands (switch-window, switcher-status, show-activities, list-monitors, set, apply-json) are called by Global Settings, waydock hot corners and the labwc Alt+Tab keybinds via `wayshell_cli.py`.
- **wayshell → waydock:** GetWorkareas and WorkareasChanged. capture-manager → waydock: the `org.gtk.Actions` actions `prepare-capture` and `finish-screenshot`. capture picker → `org.waydock.CaptureSelection`. tiling socket `capture-output` ← wayshell capture_output.
- **Shared files:**
  - `~/.cache/waydock/palette.json` is read by about 20 apps.
  - `~/.config/waydock/config.json` is also written by Global Settings when waydock is not running.
  - `~/.config/wayshell/wallpapers.json` is read by waydock, lock-screen and Global Settings.
  - `~/.config/desktop-suite/{clock,calendar,language}.json`.
  - `~/.config/weather-manager/config.json` and `~/.cache/weather-manager/*.json` + `fetch.lock` are read by lock-screen, plus night_light for lat/lon.
  - `labwc/environment` marker blocks are written by way-displays (primary output, which waydock gtk_color_scheme reads) and by language-manager.
  - labwc `rc.xml` is edited in byte-preserving blocks by session-startup, tiling-manager, way-displays and waydock appearance.
- **Flag probes:** `desktop-suite-action` greps component source for quoted flag literals (`'--menu'`, `--volume-up`, …). A Rust binary needs a shim, or the probe has to change in the same step.
- **Embedding:** Global Settings imports the Python UIs of netman, calendar DateSettings, WeatherSettings, TilingControls, WayDisplaysApp, LanguageWizard, btmanager.App and the waydock settings window. Keep the Python modules installed until Global Settings is ported. About 25 components also import waydock Python modules (companion_theme, app_catalog, window_backend, dock_style).
- **Custom wayland-client bindings needed:**
  - zwlr_foreign_toplevel_management_v1 (v3), ext-workspace-v1 and zxdg_output_v1 (waydock, wayshell switcher).
  - ext_foreign_toplevel_list_v1, ext_image_copy_capture_v1, ext_image_capture_source_v1 and wl_shm (wayshell overview).
  - ext_background_effect_v1 and the private `waydock_blur_manager_v1` (shared/protocols/waydock-blur-v1.xml). These must bind on GTK's own wl_display, in suite-gtk.
  - The X11 fullscreen probe needs x11rb.
  - Layer-shell stays on gtk4-layer-shell.
