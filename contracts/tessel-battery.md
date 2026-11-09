# tessel-battery — external contract

Source: `~/tessel-bluetooth/tessel-battery/` (refs `file:line`, relative to that dir unless prefixed).
Role: display-only battery status (UPower) for system and peripheral batteries, plus explicit
charging controls (UPower `EnableChargeThreshold`, KDE KAuth charge-limit helper). Owns **no**
idle/sleep policy (tessel-power does). Live RSS ≈ 30 MB.

**Heavy dependency on tessel-power code**: the backend, battery widgets, charge-limit protocol and
style all live in `~/tessel-bluetooth/tessel-power/` (`power_backend.py` with `batteries_only=True`,
`power_battery_ui.py`, `power_charge_limits.py`, `power_style.py`) and are imported from the sibling
installed dir `~/.config/waybar/scripts/tessel-power/` (`battery_manager.py:17-18`,
`battery_status.py:15`, `install_waybar.py:22,30,150-155`). The Rust port should put these in a
shared crate used by both tessel-power and tessel-battery; see contracts/tessel-power.md for the
full `PowerBackend` contract. Only the battery-relevant subset is restated here.

## 1. Entry points & processes

| Item | Value |
|---|---|
| Main executable | `~/.config/waybar/scripts/tessel-battery/battery_manager.py` (`battery_manager.py:206-212`) |
| GApplication id / D-Bus name | `org.tessel.BatteryManager` (`:28`), `HANDLES_COMMAND_LINE`; prgname = id (`:44-50`) |
| `IS_LAUNCHER` | only when `--check` (`:207-208`) ⇒ `--check` without a running primary fails instead of starting one |
| Status stream | `battery_status.py` — own `PowerBackend(batteries_only=True)`, JSON lines on stdout |
| .desktop | `~/.local/share/applications/org.tessel.BatteryManager.desktop`: `Name=Battery Settings`, `Comment=Device battery levels and supported charging controls`, `Exec=/usr/bin/python3 <dest>/battery_manager.py --settings`, `Icon=battery-good-symbolic`, `Categories=Settings;HardwareSettings;` (`install_waybar.py:196-200`) |

### CLI (`battery_manager.py:31-41`) — mutually exclusive modes
| Flag | Behaviour |
|---|---|
| *(none)* | primary activation ⇒ popup (bottom/76) |
| `--popup [--popup-edge {top,bottom,left,right}] [--popup-offset N≥0]` | defaults bottom/76; negative ⇒ argparse error tr("Popup offset must be nonnegative") (no trailing period — differs from brightness). Different (edge,offset) ⇒ destroy & recreate; same ⇒ activate (`:106-117`) |
| `--settings` | dismiss popup; create/present singleton `BatterySettingsWindow`; then idle `wlrctl toplevel focus app_id:org.tessel.BatteryManager title:<title>` if Wayland & wlrctl on PATH (`:123-151`) |
| `--background` | hold service, no window |
| `--check` | prints `{"ready","has_battery","on_battery","percentage","batteries","charge_limit","error"}` + `\n`; exit 0 iff `backend.power_ready` (`:79-96`). Doesn't open windows |
| invalid / ambiguous | argparse `SystemExit` code returned (test_battery_app.py:120) |

### systemd user unit `tessel-battery.service` (`install_waybar.py:142-147`)
```
[Unit] Description=Battery status and charging controls
After=graphical-session.target  PartOf=graphical-session.target
[Service] Type=simple  EnvironmentFile=-%h/.config/labwc/environment
ExecStart=/usr/bin/python3 %h/.config/waybar/scripts/tessel-battery/battery_manager.py --background
Restart=on-failure  RestartSec=2
```
+ drop-in `50-tessel-recovery.conf` (StartLimitBurst=5/60 s, `OnFailure=tessel-failed@%n.service`, label "Battery" in `tessel-session-startup/desktop_suite_failed.py:24`). Autostart line `systemctl --user start tessel-battery.service >/dev/null 2>&1 &` directly after the tessel-power line (`install_waybar.py:131-139`; generated autostart from `tessel-session-startup/configure_session.py`). In `shared/install_gtk4.py:22` UNITS.
- SIGTERM/SIGINT ⇒ `quit()`; shutdown closes backend & popup (`:65-66,189-203`).

### Install layout (`install_waybar.py`, `install_all.py:74` step "battery", kind=waybar, supports `--update`)
- `~/.config/waybar/scripts/tessel-battery/`: `battery_manager.py, battery_status.py, battery_ui.py, tray_popup.py, README.md, gtk4_utils.py, companion_theme.py, suite_i18n.py, language_data.py, suite_catalogs.py, background_blur.py, libtessel-dock-background-blur.so, popup_placement.py` (`:28-53`).
- Requires tessel-power installed first: `~/.config/waybar/scripts/tessel-power/{power_backend,power_battery_ui,power_charge_limits,power_style}.py` (`:30,150-155`).
- Legacy Waybar edit (only if configs exist): replace native `"battery"` with `"custom/battery"` `{exec: python3 …/battery_status.py, return-type: json, restart-interval: 3, format "{}", escape, tooltip, on-click: … --popup --popup-edge <edge> --popup-offset 76 </dev/null >/dev/null 2>&1 &, on-click-right: … --settings …}` (`:87-128`); CSS `#custom-battery{.plugged #69a7ff,.full #8fd6ab,.low #e3b66b,.critical #e05d65,.unavailable}` (`:31-44`).
- CLI `[--apply|--update]`; manifest `${XDG_STATE_HOME:-~/.local/state}/tessel/previews/tessel-battery.json`; legacy `.waybar-install.json` (sources/installed sha256 map); backup `~/.config/waybar/backups/battery-<ts>`; health = 20× `--check`; rollback; SIGUSR2 to `waybar` processes (`:206-327`).

## 2. Callers
| Caller | Command |
|---|---|
| tessel-dock click (`tessel-dock/panel_actions.py:16,46-49`) | `systemd-run --user … -- python3 …/battery_manager.py --popup --popup-edge E --popup-offset N`; right-click `--settings` |
| tessel-dock indicator (`tessel-dock/status_backend.py:27,268-273`) | `/usr/bin/python3 -u …/tessel-battery/battery_status.py` stream; restart 3 s after exit |
| Global Settings → Battery page (`tessel-settings/applet_pages.py:14-21,214-237,575`) | **Python-imports** `battery_ui.BatterySettingsWindow(app, backend, open_bluetooth=…)` from source or `~/.config/waybar/scripts/tessel-battery`, with its own `PowerBackend(batteries_only=True)`, and reparents its children (`_embed`). Calls `view._on_destroy(view)` on close. ⇒ A Rust port needs Global Settings to stop importing this (or keep the Python module installed) until GS is ported. Search entries `battery/status`, `battery/charge` (`tessel-settings/settings_search.py:97-98`) |

## 3. IPC

### 3.1 Owned
Only GApplication name `org.tessel.BatteryManager` (+ standard `org.gtk.Application` command-line forwarding). **No custom D-Bus interface.**

### 3.2 Consumed (via `PowerBackend(batteries_only=True)`, `tessel-power/power_backend.py`)
| Bus | Name / path / iface | Usage |
|---|---|---|
| system | `org.freedesktop.UPower` `/org/freedesktop/UPower` `org.freedesktop.UPower` | `Properties.GetAll` (needs bool `OnBattery`), `EnumerateDevices() → ao`; signals `DeviceAdded/DeviceRemoved`; `NameOwnerChanged` for UPower (`:289-305` — in batteries_only mode only UPower endpoint subscribed; no PPD, no logind) |
| system | each device `org.freedesktop.UPower.Device` | `GetAll`; `PropertiesChanged`; `EnableChargeThreshold(b)` for UPower charge protection (`:628-714`) |
| system | `org.kde.powerdevil.chargethresholdhelper` `/` `org.kde.kf6auth` | `performAction(s action, ay callerID=b"", a{sv} details={}, ay arguments, a{sh} fds={})` with action `org.kde.powerdevil.chargethresholdhelper.getthreshold|setthreshold`; arguments = Qt `QDataStream` QVariantMap (BE u32 count, QString UTF-16BE keys, `>IBi` type 2/null 0/int32) of `chargeStopThreshold` [+`chargeStartThreshold`]; reply blob decoded strictly (`power_charge_limits.py:18-19,123-194`). Polkit auth via helper's normal policy |
- Fallback refresh timer: 120 s in batteries_only mode (`power_backend.py:467`); updates arrive by signal, coalesced 100 ms.
- Battery levels change ⇒ no "uncertain source" flag (avoids idle-timer resets in tessel-power) (`power_backend.py:408-447`).

### 3.3 Other
No sockets, FIFOs, lock files. Bluetooth shortcut spawns (`battery_manager.py:162-187`):
`systemd-run --user --collect --quiet --service-type=exec --slice=app.slice --expand-environment=no [--setenv=WAYLAND_DISPLAY|DISPLAY|XAUTHORITY|XDG_RUNTIME_DIR|DBUS_SESSION_BUS_ADDRESS|XDG_CURRENT_DESKTOP|XDG_SESSION_TYPE (if set)] -- <sys.executable> <root>/tessel-bluetooth/tessel-bluetooth.py` (or `<root>/tessel-bluetooth.py`); no `--scope`/`--wait`; path with spaces preserved as one argv element. Missing ⇒ error tr("Bluetooth settings are not installed."); spawn GLib error ⇒ tr("Could not open Bluetooth settings: {value1}"); non-zero exit ⇒ tr("Could not open Bluetooth settings. Try the Bluetooth icon in Waybar."). Button only shown when `bluetooth_available` (script exists + systemd-run on PATH).

## 4. Files
| Path | R/W | Notes |
|---|---|---|
| `/sys/class/power_supply/*/{type,charge_control_start_threshold,charge_control_end_threshold,scope,present,manufacturer,model_name,serial_number}` | R only | charge-limit inspection; threshold valid only if 0–100 and owner-writable bit (S_IWUSR) set — **not** `os.access(W_OK)` (`power_charge_limits.py:33-100`) |
| `/usr/libexec/kf6/kauth/chargethresholdhelper`, `/usr/share/dbus-1/system-services/org.kde.powerdevil.chargethresholdhelper.service`, `/usr/share/polkit-1/actions/org.kde.powerdevil.chargethresholdhelper.policy` | existence check | all three required for "supported" (`:21-25,96-98`) |
| `~/.config/tessel/language.json` | R | i18n |
No config/state files of its own. Never writes sysfs, never reapplies limits at startup.

## 5. Data model (backend fields the UI/stream/--check expose)
- `power_ready`, `power_source_known`, `on_battery` (UPower `OnBattery` OR any UPS (Type 3) supply with State 2/3), `has_battery`, `percentage` (energy-weighted over system supplies Type 2/3 with PowerSupply && IsPresent, else mean Percentage; clamped 0–100), `warning_level`, `error`, `busy`.
- `batteries[]` (`power_backend.py:55-116`), sorted (non-supply last, name casefold, path); skips DisplayDevice, Type 0/1, IsPresent false: `{path,name (Vendor+Model unless model starts with vendor; else model/vendor/basename(NativePath) for Type 2; else kind),kind,type,icon_name,percentage (None if BatteryLevel≠1 i.e. coarse),level ("Low","Critical","Normal","High","Full","Unknown"),state ("Unknown","Charging","Discharging","Empty","Fully charged","Waiting to charge","Waiting to discharge"),state_code,power_supply,rechargeable,time_to_empty,time_to_full,health (Capacity 0.01–100),cycles,warning,bluetooth (Type 28 or NativePath /org/bluez/ or path tail bluez_),charge_threshold_supported,charge_threshold_enabled,charge_start_threshold,charge_end_threshold (honour ChargeThresholdSettingsSupported bits 1/2),busy,charge_limit_adjustable}`.
- `charge_limit` = `{"supported","maximum" (common stop or null),"battery_count","busy","error","reason"}` (`power_charge_limits.py:43-100`). Reasons (verbatim English, tr'd in UI): "No adjustable laptop charge limit is available.", "The available charge-limit helper cannot safely control this battery combination.", "Charge limits are available only for system batteries.", "A laptop battery is not currently available.", "The hardware charge limit could not be read.", "Adjusting charge limits requires the KDE charge-limit helper.", "Applies to all laptop batteries with adjustable charge limits."
- `set_charge_limit(max)` (`power_backend.py:716-791`): int 1–100; no-op if all stops already equal; getthreshold ⇒ verify identity unchanged ⇒ setthreshold with `arguments_for` (lowers start to `min(min(starts), max-1)` only if some start ≥ max; refuse if mixed None starts) ⇒ re-inspect & confirm every battery stop == max else "The requested charge limit could not be confirmed on every laptop battery." KAuth reply types 4/5 ⇒ "Authorization was canceled or denied.", 6 ⇒ busy.
- `set_charge_thresholds(path, enabled)`: UPower `EnableChargeThreshold`, then read-back; refused for KAuth-managed batteries ("Use Maximum charge to adjust the laptop battery limits.").

## 6. UI surfaces
- **Popup** `BatteryPopup` (`battery_ui.py:67-138`): `Gtk.ApplicationWindow` title tr("Battery"), `tray_popup.PopupController` (layer-shell overlay `desktop-applet`, click-away/Escape), `set_popup_width`; CSS classes window `powermanager batterymanager` (+`tray-popup`), `popup-surface`; header `popup_header("battery-good-symbolic", tr("Battery"), " ")` with gear `header_button("emblem-system-symbolic", tr("Battery settings"))`; subtitle = `source_status` and icon = `source_icon` (24 px): `battery-missing-symbolic` (unknown/invalid), `ac-adapter-symbolic` (plugged), `battery-caution-symbolic` ≤10, `battery-low-symbolic` ≤25, `battery-full-symbolic` ≥95, else `battery-good-symbolic` (`:31-56`). Body `BatterySection` (power_battery_ui.py) + `.status` label.
- **Settings** `BatterySettingsWindow` (`:141-211`): normal xdg toplevel, 550×560 (min 470×360), headerbar tr("Battery Settings"), title tr("Battery & Charging") `.title`, `.subtitle`, scrolled `.card` with `BatterySection(detailed=True)`, `.status`, Close button; Escape closes. Constructor kwarg `open_bluetooth` (used by Global Settings).
- `BatterySection` (power_battery_ui.py): rows `.battery-row`, `.row-title`, `.status`, `.battery-percentage`, `Gtk.LevelBar.battery-level`, `.icon-button` Bluetooth (tooltip tr("Bluetooth device settings")), "Battery protection" `Gtk.Switch` (UPower), "Maximum charge" `SpinButton(1..100, default 80)` + `.primary` tr("Apply") (`:195-215`), `.error`, `.section-title` tr("Batteries"); empty tr("No battery devices detected.") / tr("Battery information is unavailable."). Remaining time strings tr("{hours} hr {minutes} min") etc.
- Status message: `friendly_status(item, tr("Some battery details could not be read. See the log for details."))` deduped over (local, app.error, backend.error); else tr("Updating battery settings…") while busy (`battery_ui.py:59-64`).
- Style: `power_style.install_style` (classes `powermanager`, `title`, `subtitle`, `section-title`, `row-title`, `status`, `muted`, `error`, `card`, `primary`, `icon-button`, `battery-row`, `battery-level`, `battery-percentage`, `popup-surface`, `tray-popup`, …), companion_theme.
- **Indicator JSON** (`battery_status.py:18-43`), printed only when changed, exit on BrokenPipe:
  - unknown source: text ``, class `["unavailable"]`, first line tr("Power source unavailable")
  - AC: ``, `["plugged"]`, tr("Plugged in") + ` · NN%` if a system battery has a valid %
  - battery valid %: glyph by ≤10/≤25/≤50/≤80/else = `    `; class critical ≤10, low ≤25, full ≥95, else `battery`; tr("On battery · {percent:.0f}%")
  - battery invalid %: ``, `unavailable`, tr("On battery · Level unavailable")
  - then one line per device `"<name>: <NN%|tr(level)|tr('Level unavailable')> · <tr(state)|tr('Status unavailable')>"`, last line tr("Click for batteries · Right-click for charging settings").

## 7. External commands
`wlrctl toplevel focus app_id:org.tessel.BatteryManager title:<title>`; `systemd-run … tessel-bluetooth.py` (above). Everything else is D-Bus.

## 8. i18n
`suite_i18n` (`shared/locale/<code>.json`, language pref `~/.config/tessel/language.json`), initialized at import in every module. Device kind/state/level names are passed through `tr()` at render time (English keys from `power_backend.py:28-39`).

## 9. Parity test cases
test_battery_app.py: AC ⇒ plug glyph, class plugged, tooltip starts "Plugged in · 72%\n", contains "Right-click for charging settings" (:22); charge ranges select glyphs/classes (:29); unknown/invalid % never invents a percentage (:43); unknown source never reuses stale AC/battery state, no "%" (:56); peripheral-only desktop shows plug + "Controller: 100% · Fully charged", "Mouse: Low · Discharging", "Headphones: Level unavailable · Status unavailable" (:67); health reports batteries without idle controller; not ready ⇒ false (:82); `--check` opens no window, exit 0/1 (:93); routing popup("top",30)/settings/background (:104); ambiguous modes & negative offset rejected (:120); Bluetooth launch independent (`--service-type=exec`, no `--scope`/`--wait`, spaced path last argv) (:125); unavailable Bluetooth ⇒ error, no spawn (:138).
tessel-power tests that cover the shared backend (port with it): `test_power_charge_limits.py` (sysfs inspection, KAuth encode/decode, start-lowering rules), `test_power_backend.py` (device normalization, OnBattery/UPS, no idle reset on charge updates), `test_power_ui.py` battery sections.

## 10. Memory / startup
- Two processes per session: service (≈30 MB) + `battery_status.py` stream per tessel-dock (another full Python+GLib+PowerBackend). GTK imported in the service at start (gtk4_utils), UI modules lazily. Rust: one tiny binary for the stream (or a D-Bus/state broadcast) and a GTK service; backend needs only UPower (+ KAuth on demand).
- Startup: one UPower GetAll + EnumerateDevices + GetAll per device; sysfs scan of power_supply on every publish.
