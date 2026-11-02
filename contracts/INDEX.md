
## Wave 1/2 services (tessel-keep-awake, battery, power, notifications, OSK, audio, brightness, polkitman)
| Component | Contract | Bus name / interface | Unit |
|---|---|---|---|
| tessel-keep-awake | [tessel-keep-awake.md](tessel-keep-awake.md) | `org.tessel.KeepAwake` · `.Controls` (GetState/Changed, SetEnabled/SetDuration) | tessel-keep-awake.service |
| tessel-battery | [tessel-battery.md](tessel-battery.md) | GApplication + KAuth charge-limit helper | tessel-battery.service |
| tessel-power | [tessel-power.md](tessel-power.md) | `org.tessel.PowerManager.Controls` (GetState/Changed, SetSettings(s)) | tessel-power.service (+ starts tessel-lock.service) |
| tessel-notifications | [tessel-notifications.md](tessel-notifications.md) | `org.freedesktop.Notifications` (DO_NOT_QUEUE) + `org.tessel.Notifications1` | tessel-notifications.service drop-in (replaces dunst) |
| tessel-keyboard | [tessel-keyboard.md](tessel-keyboard.md) | `org.tessel.OnScreenKeyboard`; `$XDG_RUNTIME_DIR/tessel-keyboard/state.json` | tessel-keyboard.service |
| tessel-audio | [tessel-audio.md](tessel-audio.md) | `org.tessel.AudioManager` (GApplication cmdline) | tessel-audio-loopbacks.service |
| tessel-brightness | [tessel-brightness.md](tessel-brightness.md) | `org.tessel.BrightnessManager.Controls` | tessel-brightness.service |
| tessel-auth | [tessel-auth.md](tessel-auth.md) | polkit agent for LABWC_PID session; BlueZ Agent1 at `/org/tessel/BluetoothAgent` | tessel-auth.service (Type=notify) |

**Cross-component interfaces**
- tessel-dock (`status_backend.py`, `panel_actions.py`) streams JSON-line `*_status.py` scripts (power, battery, brightness, notifications, osk), reads tessel-keep-awake through `SettingsClient`, calls `wpctl` directly for volume, and starts popups with `systemd-run … <app>.py --popup --popup-edge E --popup-offset N` or `--settings` (OSK uses `--toggle`/`--settings`; tessel-keep-awake right-click runs `--toggle`).
- Global Settings imports Python modules from `~/.config/waybar/scripts/<app>/`: audio_ui/audio_backend, battery_ui, power_ui/power_backend/power_client/power_config, keep_awake_ui/client, notification_ui.SettingsPage, osk_settings. **These Python files must stay installed until Global Settings is ported.**
- Python imports between components: tessel-battery imports power_backend, power_battery_ui, power_style and power_charge_limits; tessel-brightness imports PowerBackend for its inhibitor check; tessel-displays loads brightness_client.py.
- Inhibitors: tessel-keep-awake takes a logind `idle:sleep` block inhibitor. tessel-power and tessel-brightness find it with ListInhibitors and skip dim, lock and sleep.
- Two separate auto-dim paths can both act: tessel-power's overlay and tessel-brightness's swayidle. Both use a swayidle token protocol.
- tessel-lock: tessel-power starts `tessel-lock.service` and waits for it to be ready before sleep. lock_settings goes through PowerClient. Lock-screen reads `org.tessel.Notifications1` GetState/StateChanged. OSK watches the lock unit's ActiveState and uses `tessel-keyboard-input --labels`.
- labwc keybinds and `tessel-action` run `audio_manager.py --volume-up/--volume-down/--toggle-mute`, `brightness_manager.py --brightness-up/--brightness-down` and `power_manager.py --power-button`. Suite recognition greps the script path and source, so Rust binaries need shims at the same paths.
- tessel-brightness ↔ way-displays: `$XDG_RUNTIME_DIR/way-displays.sock`. It also reads `~/.cache/tessel-dock/palette.json`.
- These components send notifications: power alerts, desktop_suite_failed (critical, with a `restart` action), bttransfer, tessel-shell and tessel-capture (hints `x-tessel-dock-output` and `x-tessel-dock-workspace`).
- polkitman's BlueZ agent serves the Bluetooth app (tessel-bluetooth).

## Wave 2/3 components (network, calendar, weather, media, discovery, tiling, session, way-displays, language, tessel-session-startup, bluetooth, tessel-shell, tessel-dock)
| Component | Contract | Bus name / socket | Unit / launch |
|---|---|---|---|
| tessel-network (netman) | [tessel-network.md](tessel-network.md) | GApplication; NM SecretAgent (RegisterWithCapabilities, re-register on NM restart); `--check` probe | tessel-network.service (static, autostart) |
| tessel-calendar | [tessel-calendar.md](tessel-calendar.md) | GApplication (resident after first launch); timedated | systemd-run popup from tessel-dock |
| tessel-weather | [tessel-weather.md](tessel-weather.md) | GApplication; `--tray` = SNI + dbusmenu | popup (exits on close); tessel-weather.service (HA-screen bundle) |
| tessel-media | [tessel-media.md](tessel-media.md) | `org.tessel.Media`; MPRIS client | systemd-run popup |
| tessel-network-discovery | [tessel-network-discovery.md](tessel-network-discovery.md) | `org.tessel.NetworkDiscovery`; systemd/avahi units via D-Bus, pkexec | systemd-run popup |
| tessel-session | [tessel-session.md](tessel-session.md) | `org.tessel.Session`; logind inhibitors | tessel-action (C-A-Delete, XF86PowerOff) `--menu` |
| tessel-tiling | [tessel-tiling.md](tessel-tiling.md) | `$XDG_RUNTIME_DIR/tessel-tiling-$WAYLAND_DISPLAY.sock` (dgram JSON v1, server in labwc C) | tessel-dock `--toggle`/`--watch` |
| way-displays | [way-displays.md](way-displays.md) | `$XDG_RUNTIME_DIR/way-displays.sock` (YAML); `org.tessel.BrightnessManager` | way-displays.service |
| tessel-language | [tessel-language.md](tessel-language.md) | localed; writes `tessel/language.json` | launcher opens Global Settings `--section language` |
| tessel-session-startup | [tessel-session-startup.md](tessel-session-startup.md) | tessel-action, `tessel-failed@.service`, `labwc-xdg-autostart.target` | labwc autostart |
| bluetooth | [bluetooth.md](bluetooth.md) | BlueZ, obexd (OBEX transfer + incoming Agent) | tessel-dock popup; embedded in Global Settings |
| tessel-shell | [tessel-shell.md](tessel-shell.md) | owns `com.nobaraDesktopView`; `$XDG_RUNTIME_DIR/tessel-shell.sock` (JSON line) | tessel-shell.service (fix ExecStart in palette.conf drop-in) |
| tessel-dock | [tessel-dock.md](tessel-dock.md) | `org.tessel.Dock`; `/org/tessel/Settings` `org.tessel.Settings`; StatusNotifierWatcher + host | tessel-dock.service (Type=notify; READY after tray host) |

**Cross-component interfaces (wave 2/3)**
- **Popup placement:** every tray popup (netman, calendar, weather, media, discovery, session, bluetooth=`service_apps`) calls `org.tessel.Settings.GetPopupPlacement(ss)→s` (JSON). If that fails, it falls back to `--popup-edge E --popup-offset N`. Port `shared/tray_popup` (PopupController, PointerMonitor) and `popup_placement` once, in tessel-gtk.
- **tessel-dock streams `*_status.py --watch` JSON lines** from network_status, weather_status and tiling `--watch`. The Discovery backend is a byte-identical copy that runs in-process inside tessel-dock.
- **MPRIS client** `tessel-lock/lock_media*.py` is shared by tessel-media, tessel-dock media_status/media_controls and tessel-lock. Port it once.
- **Helper `tessel-dock-toplevel`** (foreign-toplevel, JSON stdio) is used by tessel-dock, tessel-session (`close <id>`) and tessel-shell (`--snapshot`).
- **tessel-shell.sock** commands (switch-window, switcher-status, show-activities, list-monitors, set, apply-json) are called by Global Settings, tessel-dock hot corners and the labwc Alt+Tab keybinds via `wayshell_cli.py`.
- **tessel-shell → tessel-dock:** GetWorkareas and WorkareasChanged. tessel-capture → tessel-dock: the `org.gtk.Actions` actions `prepare-capture` and `finish-screenshot`. capture picker → `org.tessel.CaptureSelection`. tiling socket `capture-output` ← tessel-shell capture_output.
- **Shared files:**
  - `~/.cache/tessel-dock/palette.json` is read by about 20 apps.
  - `~/.config/tessel-dock/config.json` is also written by Global Settings when tessel-dock is not running.
  - `~/.config/tessel-shell/wallpapers.json` is read by tessel-dock, tessel-lock and Global Settings.
  - `~/.config/tessel/{clock,calendar,language}.json`.
  - `~/.config/tessel-weather/config.json` and `~/.cache/tessel-weather/*.json` + `fetch.lock` are read by tessel-lock, plus night_light for lat/lon.
  - `labwc/environment` marker blocks are written by way-displays (primary output, which tessel-dock gtk_color_scheme reads) and by tessel-language.
  - labwc `rc.xml` is edited in byte-preserving blocks by tessel-session-startup, tessel-tiling, way-displays and tessel-dock appearance.
- **Flag probes:** `tessel-action` greps component source for quoted flag literals (`'--menu'`, `--volume-up`, …). A Rust binary needs a shim, or the probe has to change in the same step.
- **Embedding:** Global Settings imports the Python UIs of netman, calendar DateSettings, WeatherSettings, TilingControls, WayDisplaysApp, LanguageWizard, tessel-bluetooth.App and the tessel-dock settings window. Keep the Python modules installed until Global Settings is ported. About 25 components also import tessel-dock Python modules (companion_theme, app_catalog, window_backend, dock_style).
- **Custom wayland-client bindings needed:**
  - zwlr_foreign_toplevel_management_v1 (v3), ext-workspace-v1 and zxdg_output_v1 (tessel-dock, tessel-shell switcher).
  - ext_foreign_toplevel_list_v1, ext_image_copy_capture_v1, ext_image_capture_source_v1 and wl_shm (tessel-shell overview).
  - ext_background_effect_v1 and the private `waydock_blur_manager_v1` (shared/protocols/waydock-blur-v1.xml). These must bind on GTK's own wl_display, in tessel-gtk.
  - The X11 fullscreen probe needs x11rb.
  - Layer-shell stays on gtk4-layer-shell.

## Wave 4 apps (calculator, tessel-text-editor, tessel-pictures, tessel-system-monitor, tessel-hdr-calibration, tessel-capture)
| Component | Contract | Bus name / files | Unit / launch |
|---|---|---|---|
| tessel-text-editor | [tessel-text-editor.md](tessel-text-editor.md) | GApplication `org.tessel.TextEditor` (HANDLES_OPEN); `$XDG_STATE_HOME/tessel-text-editor/recovery/*.json`; `tessel/tessel-text-editor.json` | `/usr/local/bin/tessel-text-editor` (desktop-apps/install.py), `org.tessel.TextEditor.desktop` |
| tessel-system-monitor | [tessel-system-monitor.md](tessel-system-monitor.md) | GApplication `org.tessel.SystemMonitor` (HANDLES_COMMAND_LINE; `--check`/`--page`/`--quit`), no D-Bus service; /proc + /sys | launcher `/usr/local/bin/tessel-system-monitor` |
| tessel-hdr-calibration | [tessel-hdr-calibration.md](tessel-hdr-calibration.md) | GApplication `org.desktop.HDRCalibration` (NON_UNIQUE; `--output`/`--sdr-editing`/`--smoke-test`); SIGHUP to labwc; rc.xml tessel-hdr-calibration block, `~/.config/tessel-hdr-calibration/` profiles + corrections; C `hdr-pattern` helper kept | launcher `~/.local/bin/tessel-hdr-calibration`; shim at `hdr_calibration.py` (way-displays color_controls.py) |
| calculator | [calculator.md](calculator.md) | GApplication `org.tessel.Calculator` (HANDLES_COMMAND_LINE); state `$XDG_STATE_HOME/tessel-dock-calculator/state.json` | launcher `/usr/local/bin/calculator` |
| tessel-pictures | [tessel-pictures.md](tessel-pictures.md) | GApplication `org.tessel.Pictures` (HANDLES_COMMAND_LINE); `$XDG_CONFIG_HOME/tessel-pictures/settings.json` | launcher `/usr/local/bin/tessel-pictures` |
| tessel-files | [tessel-files.md](tessel-files.md) | GApplication `org.tessel.Files` (HANDLES_COMMAND_LINE; `--check`/`--new-window`/`--select`/`--split`/`--home`); optional `org.freedesktop.FileManager1` (only while default `inode/directory` handler, never replaces an owner); `tessel/tessel-files.json`, `file-manager-actions.json`, `gtk-3.0/bookmarks`, `$XDG_DATA_HOME/tessel/file-metadata.json` + `user.tessel.metadata` xattr | `/usr/local/bin/tessel-files` (desktop-apps/install.py, root), `org.tessel.Files.desktop` |
| tessel-capture | [tessel-capture.md](tessel-capture.md) | GApplication `org.tessel.Capture` (actions show, stop, lock-shot(d), open/reveal/copy/edit-saved(s)); `/org/tessel/Capture/Status`; CaptureSelection broker; ScreenCast portal client; C `libcapture-pipewire.so` dlopen'ed | `/usr/local/bin/tessel-capture` (keybinds, tessel-dock, .desktop); Python capture_settings/capture_shortcuts stay installed (Global Settings, install_session.py) |

## Wave 4 (second half): Global Settings and Welcome
| Component | Contract | Bus name / files | Unit / launch |
|---|---|---|---|
| tessel-settings | [tessel-settings.md](tessel-settings.md) | GApplication `org.tessel.GlobalSettings` (HANDLES_COMMAND_LINE; `--section`/`--shortcuts`/`--quit`); embeds every applet's Rust page; rc.xml/environment/mimeapps/night-light/tessel-dock config writers | `~/.local/bin/tessel-settings`, `~/.local/bin/tessel-language` (`--section language`), `org.tessel.GlobalSettings.desktop`; Python modules stay (tiling_config imports workspace_config) |
| welcome | [welcome.md](welcome.md) | GApplication `org.tessel.Welcome` (`--first-run`, `--page`); `$XDG_STATE_HOME/tessel/setup-done`, reads `install-status.json` | `~/.local/bin/tessel-welcome` (first-login autostart, labwc menu) |

## Wave 5: security components (tessel-lock, tessel-login) and home-assistant-screen
Each contract has a **Threat model** section. Shared code: `crates/locking` (lock_config, login_model, and with feature `gtk` the lock dashboard widgets used by all three).
| Component | Contract | Bus name / files | Unit / launch |
|---|---|---|---|
| tessel-lock | [tessel-lock.md](tessel-lock.md) | GApplication `org.tessel.LockScreen` / `org.tessel.LockPreview` (NON_UNIQUE; `--preview`, `--check`); ext-session-lock-v1 via Gtk4SessionLock; `tessel-lock-pam` helper (pipes: type byte + BE u32 length; PAM services `tessel-lock`, `tessel-lock-password`, `tessel-lock-fingerprint`); reads UPower, `org.tessel.Notifications1`, fprintd, logind, DisplayManager; `$XDG_CONFIG_HOME/tessel/tessel-lock.json` | `tessel-lock.service` (Type=notify, READY after `locked`, exit 75 = already locked); `tessel-shell-lock`, `screenlock.sh` |
| tessel-login | [tessel-login.md](tessel-login.md) | greetd IPC over `$GREETD_SOCK` (native-endian u32 + JSON); runs as `greetd`; root `tessel-login-helper` (pkexec, writes only `/etc/tessel-login/appearance.json` 0644) | `tessel-login.service` → greetd `/etc/tessel-login/greetd.toml` → `/usr/local/lib/tessel-login/start-greeter` (root-owned) |
| home-assistant-screen | [home-assistant-screen.md](home-assistant-screen.md) | GApplication (luna-clock `--show/--hide/--status/--next-wallpaper/--quit/--panel/--close-panel/--volume/--brightness`); admin panel WebKit restricted to `http://127.0.0.1:8765`; Kasumi scheduler socket; `~/.config/kasumi-screen/levels.json`; voice log (read only) | `luna-clock.service` on the Kasumi device (`~/makima/screensaver/`); shims at `luna_clock.py`, `voice_controls.py` |
