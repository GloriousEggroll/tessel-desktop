# tessel-network-discovery — external contract

Source: `~/tessel-bluetooth/tessel-network-discovery/` (1174 LOC): `discovery_manager.py` 271 (app/CLI/popup),
`discovery_backend.py` 238 (systemd D-Bus state + pkexec toggle), `discovery_browse.py` 180
(avahi-browse parser), `install.py` 180, tests 305. Adapted from NDDT (MIT, `upstream/`,
LICENSE must still be installed). `tessel-dock/discovery_backend.py` is a byte-identical copy that
the dock imports in-process (status_backend.py:285-299), so the Rust backend must be a library
shared by the binary and tessel-dock.

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installed dir | `~/.config/waybar/scripts/tessel-network-discovery/` (`discovery_backend.py`, `discovery_manager.py`, `discovery_browse.py`, shared `gtk4_utils.py popup_placement.py tray_popup.py suite_i18n.py language_data.py suite_catalogs.py background_blur.py libtessel-dock-background-blur.so`, `companion_theme.py`, `LICENSE`) | install.py:15-34 |
| Launcher | `~/.local/bin/tessel-network-discovery` → `exec /usr/bin/python3 <dir>/discovery_manager.py "$@"` (0755) | install.py:38-40 |
| .desktop | `~/.local/share/applications/org.tessel.NetworkDiscovery.desktop`: `Name=Discover Network Devices`, `Comment=Enable or disable Avahi network discovery`, `Exec="<launcher>" --popup`, `Icon=network-workgroup`, `Terminal=false`, `Categories=Settings;Network;`, `Keywords=Avahi;Discovery;mDNS;Zeroconf;Network;` (validated with `desktop-file-validate`) | install.py:41-45,131 |
| Dock files also installed | `dock_config.py panel_actions.py panel_ui.py status_backend.py discovery_backend.py` → `~/.local/share/tessel-dock/` | install.py:15,35-37 |
| GApplication id | `org.tessel.NetworkDiscovery`, `HANDLES_COMMAND_LINE` | discovery_manager.py:52-53 |
| systemd unit / autostart | none (upstream `nddt.service`/autostart NOT installed) | README |
| Dock launch | tessel-dock `panel_actions.py:36-38`: both left and right click → `discovery_manager.py --popup --popup-edge E --popup-offset N` via `systemd-run --user ... --slice=app.slice` | |

### CLI (discovery_manager.py:13-23, 266-271)
Mutually exclusive: `--toggle | --popup | --settings | --status`; plus `--popup-edge
{top,bottom,left,right}` (default bottom), `--popup-offset INT` (default 76, negative → error).
- `--status`: no GTK; runs a GLib loop until backend `ready`, prints one line
  `json.dumps(snapshot())` and exits (26-36). Used by the installer health check.
- `--toggle`: hold app, wait for ready, flip `enabled`; on failure shows popup with message
  (78-82, 90-95, 116-120). Ignored if a toggle is pending or backend busy.
- `--popup`, `--settings`, none: show popup (84). Second invocation repositions the existing
  popup (136-138).

### Snapshot JSON (`summarize`, discovery_backend.py:55-64)
```json
{"text":"","tooltip":"Discover Network Devices: Enabled\nClick for discovery controls",
 "class":"active","ready":true,"installed":true,"enabled":true,"busy":false,"error":"",
 "units":{"avahi-daemon.socket":{"LoadState":"loaded","ActiveState":"active","UnitFileState":"enabled"},
          "avahi-daemon.service":{...}}}
```
- `installed` = any unit LoadState not in (None,'','not-found'); `enabled` = any ActiveState in
  (active, activating, reloading).
- label: busy → `Waiting for authentication…`; else error; else ready∧¬installed → `Avahi is not
  installed`; enabled → `Enabled`; ready → `Disabled`; else `Checking…`.
- class: `pending` (busy) | `unavailable` (error ∨ ¬ready ∨ ¬installed) | `active` | `inactive`.
- `ready` reported = ready ∧ ¬error.
- Dock: `value['visible'] = installed or not ready` (status_backend.py:293-299), title
  `Discover Network Devices`, icons `network-workgroup-symbolic`/`network-server-symbolic`
  (panel_ui.py:46). Installer polls `tessel-dock --check` for
  `panel.indicator_states.discovery ∈ [['active'],['inactive'],['unavailable']]` (install.py:144-145).

## 2. IPC

### Owned
Only GApplication `org.tessel.NetworkDiscovery` (`org.gtk.Application`).

### Consumed: systemd (SYSTEM bus) — discovery_backend.py
| Call | Detail | Ref |
|---|---|---|
| Proxy `org.freedesktop.systemd1` `/org/freedesktop/systemd1` `org.freedesktop.systemd1.Manager` | DO_NOT_AUTO_START; watch `notify::g-name-owner` → resubscribe | 86-99,117-119 |
| `Manager.Subscribe()` | 5000 ms; `AlreadySubscribed` error tolerated; then refresh | 102-115 |
| signals `UnitNew(so)`, `UnitRemoved(so)` for the two units, `UnitFilesChanged()`, `Reloading(b)` with false → refresh (only if no refresh pending) | 121-126 |
| `Manager.LoadUnit(s)` → `(o)` for `avahi-daemon.socket`, `avahi-daemon.service` (5000 ms) | `NoSuchUnit`/`LoadFailed` → unit = `{LoadState:not-found, ActiveState:inactive}`; other errors → friendly error | 147-181 |
| Unit proxy `org.freedesktop.systemd1.Unit` at returned path | read cached `LoadState, ActiveState, UnitFileState`; `g-properties-changed` → re-read + changed | 161-186 |
| `Manager.Unsubscribe()` on close (2000 ms, fire-and-forget) | 236-237 |
- Generation counter discards stale async replies (137-138,159,169).

### Privileged change (discovery_backend.py:48-52, 194-226)
- enable: `/usr/bin/pkexec /usr/bin/systemctl enable --now avahi-daemon.socket`
- disable: `/usr/bin/pkexec /usr/bin/systemctl disable --now avahi-daemon.socket avahi-daemon.service`
- Non-bool argument → ValueError. Only one in flight (`busy`); refused when ¬ready or ¬installed.
- stdout silenced, stderr piped; exit 126/127 = auth cancelled → no error. Failure →
  `friendly_error(stderr)` or `The discovery setting could not be changed.` Always refresh after.
- Popup dismisses itself (`controller.dismiss()`) BEFORE pkexec so polkit's agent can grab the
  keyboard; app held until completion (discovery_manager.py:122-129).

### friendly_error (discovery_backend.py:19-45)
Cancelled (Gio CANCELLED, or text `operation was cancelled|cancelled|request dismissed`) → ''.
Needle table → `The system did not allow this change.` / `Avahi is not installed.` /
`The system service manager is unavailable.` / `The system service manager did not respond in time.`
Else last line with `<quark>: ` and `GDBus.Error:<name>: ` prefixes and trailing ` (N)` stripped.

## 3. Device browsing (discovery_browse.py)
- Command `/usr/bin/avahi-browse --all --parsable --terminate --resolve`, stdout piped, stderr
  silenced, single-flight; SIGTERM after 12000 ms keeping partial output (10-12,127-148).
- Spawn failure → `Avahi browsing tools are not installed.`; communicate failure → `The device
  list could not be read.`; nonzero exit with no devices → `Avahi is not running. Turn on discovery
  to look for devices.`
- Parse: `;`-separated, keep `+`/`=` with ≥6 fields; `\DDD` decimal byte escapes → UTF-8
  (`\059` → `;`); strip `^[0-9A-F]{12}@` prefix and ` [MAC]` suffix; hide types
  `_device-info._tcp _sleep-proxy._udp _companion-link._tcp _net-assistant._udp
  _apple-mobdev2._tcp _rdlink._tcp _remotepairing._tcp _asquic._udp _googlezone._tcp`;
  `=` lines with >8 fields give host (field 6, minus `.local`) and address (field 7, IPv4
  preferred over IPv6). Group by host (casefold) or name; title = shortest non-opaque name
  (opaque = UUID/hash regexes, 57-60) else host; anonymous devices whose address matches a named
  device are merged into it; sorted by (opaque, casefold name). Output
  `[{'name','host','address','services':[sorted labels]}]`.
- Service labels table `SERVICE_NAMES` (14-29, translated in UI); unknown → `_foo-bar._tcp` →
  `Foo bar`.
- Browse runs when popup is open and `enabled` changes to true, and on "Search again"
  (discovery_manager.py:96-110). Browser closed when the popup closes.

## 4. Files
- No config/state of its own; Avahi state lives in systemd. Installer backups in
  `~/.local/state/tessel/backups/network-discovery-<UTC ts>/` + `new-files.json`; refuses
  symlinked targets; atomic tempfile writes `.discovery-install-*` (install.py:49-67,117-122).
- Always-visible option lives in tessel-dock config (`dock_config.py:42` group entry `discovery`)
  and Global Settings → Dock/Panel.

## 5. UI surface (discovery_manager.py:161-261)
- Window title `Discover Network Devices`, CSS `discovery-manager` + `tray-popup` (classes added
  after construction so theme's `tessel-dock-companion-*` class survives), inner `popup-surface`,
  `list.discovered` boxed list, `row-title`, `row-detail`, `section-title`. Width ≤ 420.
- Shared `PopupController` (default group `service_apps`): layer-shell namespace
  `desktop-applet`, OVERLAY, exclusive −1, keyboard ON_DEMAND, backdrop `desktop-applet-dismiss`.
- Header `network-workgroup-symbolic` `Network Discovery`, subtitle = `Waiting for
  authentication… | Checking… | Avahi is not installed. | Discovery is on. | Discovery is off.`;
  switch `Discover network devices` (tooltip `Enable network device discovery`), insensitive
  unless ready∧installed∧¬busy; detail line only for problems; `Found on this network` +
  `view-refresh-symbolic` `Search again` (sensitive iff enabled ∧ ¬searching); list rows
  name + `services, … · address`; scroll max 260 px; status messages `Turn on discovery to see
  devices on this network.`, `Searching…`, browser error, `No devices or shared services were
  found.`; footer about admin authentication.

## 6. External commands
`pkexec systemctl …` (above), `avahi-browse …`. Installer: `systemctl --user is-active/show/
restart tessel-dock.service`, `desktop-file-validate`, `discovery_manager.py --status`,
`~/.local/bin/tessel-dock --check`; `verify_dock_children` walks tessel-dock's cgroup and refuses to
restart if any non-helper/non-status child process exists (install.py:74-105).

## 7. Parity tests (test_discovery.py, test_popup.py, test_install.py)
1. Socket OR service active → enabled; loaded+inactive → installed, not enabled (d:10-17).
2. not-found → ready ∧ ¬installed; `{}`+error → ¬ready, error in tooltip (19-25).
3. Exact pkexec argv; non-bool (`'true'`, `'avahi;exit'`, 1, None) raises (27-31).
4. Toggle async; second call while busy returns False, one spawn; completion → done(True,'') (40-55).
5. Exit 126 with `Not authorized` stderr → done(False,'') (no error shown) (57-68).
6. ¬ready → no spawn; spawn GLib.Error → busy false, message contains error (70-80).
7. friendly_error mappings incl. pass-through `Something odd happened`; tooltip never contains
   `GDBus` (83-96).
8. Parser fixture → `['Office Printer','Thomas’s Mac']`, address 192.0.2.2 (IPv4 over fe80::1),
   services `['Chromecast','Printer','Web page']` (opaque Chromecast merged by address),
   `['Windows file sharing']`; `unescape('a\\059b')=='a;b'` (98-112).
9. Browser single-flight, argv contains `--terminate`, SSH label `Remote login (SSH)`; missing
   binary error text (115-135).
10. Popup: classes, switch state, `Discovery is on.`, detail hidden, width ≤420, one browse on
    open, rows rendered, opening never toggles; switch off hides window before command
    (test_popup.py:53-82).
11. Re-show on edge change, real dismissal clears `app.popup` (84-106).
12. Installer: plan copies only runtime + dock files; failed health check restores originals
    (test_install.py:17-49).

## 8. Risks
- Async stale-reply handling (generation) and resubscribe on systemd owner change.
- Must remain usable without Avahi installed (control hidden in dock).
- No Wayland protocols beyond layer-shell.
