# Tessel file layout

Every Tessel part uses `tessel_core::paths` for its directories and for finding
other Tessel programs. No part hardcodes `~/.config/waybar/scripts`, `~/.local/share/<part>`,
`/usr/local/lib/...` or Python entry points (`*.py`) any more: the Python suite is retired.

## Directories

| What | Function | Path |
|---|---|---|
| Shared preferences | `suite_config_dir()` | `~/.config/tessel/` (`language.json`, `clock.json`, `calendar.json`, `clipboard.json`, `archive-manager.json`, `text-editor.json`, `lock-screen.json`, `file-manager.json`, …: the file names did not change) |
| A part's own config | `component_config_dir("dock")` | `~/.config/tessel/dock/` |
| Shared state | `suite_state_dir()` | `~/.local/state/tessel/` (`backups/…`) |
| A part's state | `component_state_dir("dock")` | `~/.local/state/tessel/dock/` |
| Cache | `suite_cache_dir()`, `component_cache_dir("dock")` | `~/.cache/tessel/`, `~/.cache/tessel/dock/` |
| Shared data | `suite_data_dir()` | `~/.local/share/tessel/` (`locale/`, `keyring/`, `session/`) |

Part names are the component directory names (`components/<part>`): `dock`, `shell`, `tiling`,
`power`, `keyboard`, `hdr-calibration`, `audio`, `weather`, `network`, `capture`, `settings`,
`notifications`, `keep-awake`, `text-editor`, `calculator`, …  `component_*_dir` also accepts
`tessel-dock` (the prefix is dropped).

## Migration (old → new, done once at login by `tessel-session-startup`)

| Old | New |
|---|---|
| `~/.config/desktop-suite/*` | `~/.config/tessel/*` |
| `~/.config/way-displays/cfg.yaml` | `~/.config/tessel/displays/cfg.yaml` (read until the first save; the old file stays) |
| `~/.config/waydock/` | `~/.config/tessel/dock/` |
| `~/.config/wayshell/` | `~/.config/tessel/shell/` |
| `~/.config/waytile/` | `~/.config/tessel/tiling/` |
| `~/.config/power-manager/` | `~/.config/tessel/power/` |
| `~/.config/on-screen-keyboard/` | `~/.config/tessel/keyboard/` |
| `~/.config/hdr-calibration/` | `~/.config/tessel/hdr-calibration/` |
| `~/.config/audio-manager/` | `~/.config/tessel/audio/` |
| `~/.config/weather-manager/` | `~/.config/tessel/weather/` |
| `~/.config/desktop-capture/` | `~/.config/tessel/capture/` |
| `~/.config/night-light/` | `~/.config/tessel/night-light/` |
| `~/.config/btmanager/` | `~/.config/tessel/bluetooth/` |
| `~/.config/picture-viewer/` | `~/.config/tessel/pictures/` |
| `~/.config/environment.d/90-desktop-suite-language.conf` | `~/.config/environment.d/90-tessel-language.conf` (markers renamed) |
| `~/.config/waybar/network-notifications.json` | `~/.config/tessel/network/notifications.json` |
| `~/.local/state/desktop-suite/` | `~/.local/state/tessel/` |
| `~/.local/state/{waydock,wayshell,waytile,global-settings,notification-manager,keep-awake,hdr-calibration,text-editor,waydock-calculator}/` | `~/.local/state/tessel/{dock,shell,tiling,settings,notifications,keep-awake,hdr-calibration,text-editor,calculator}/` |
| `~/.cache/desktop-suite/`, `~/.cache/waydock/`, `~/.cache/weather-manager/` | `~/.cache/tessel/`, `~/.cache/tessel/dock/`, `~/.cache/tessel/weather/` |
| `~/.local/share/desktop-suite/{keyring,session,file-metadata.json}` | `~/.local/share/tessel/{keyring,session,file-metadata.json}` (the keyring also copies itself on first open) |
| `~/.local/share/waydock/icons` | `~/.local/share/tessel/dock/icons` |

Text inside migrated files, `~/.config/labwc/rc.xml` and `~/.config/mimeapps.list` gets the
old ids replaced (`org.waydock.Files.desktop` → `org.tessel.Files.desktop`, old program
names → `tessel-*`). Old files are copied, never deleted.

## Programs

All executables (apps, daemons, status streams, helpers) are installed flat into one directory:

| Scope | Directory | On `PATH` |
|---|---|---|
| user | `~/.local/lib/tessel/` | symlinks in `~/.local/bin/` |
| system | `/usr/local/lib/tessel/` | symlinks in `/usr/local/bin/` |
| package (`distribution/fedora`) | `/usr/lib/tessel/` | `/usr/bin/` |

Find another program with `tessel_core::paths::program("tessel-power-status")`: beside the
running executable first, then those directories, else the bare name (`PATH`). Status streams
the dock runs are programs too (`tessel-power-status`, `tessel-battery-status`, …).

## Other installed files

| What | User scope | System scope |
|---|---|---|
| Units | `~/.config/systemd/user/tessel-*.service` | `/usr/local/lib/systemd/user/` |
| Desktop files | `~/.local/share/applications/org.tessel.*.desktop` | `/usr/local/share/applications/` |
| D-Bus activation | `~/.local/share/dbus-1/services/` | `/usr/local/share/dbus-1/services/` |
| Translations | `~/.local/share/tessel/locale/<lang>.json` | `/usr/local/share/tessel/locale/` |
| Logo | `~/.local/share/icons/hicolor/scalable/apps/tessel.svg` (+ `tessel-symbolic.svg`) | `/usr/local/share/icons/hicolor/…` |
