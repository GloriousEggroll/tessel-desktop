# tessel-dock — external contract

Source: `~/tessel-bluetooth/tessel-dock` (≈14.7k LOC runtime Python + 726 LOC C helpers; ≈16k LOC tests, 588 test
functions in 70 files). All refs are `tessel-dock/<file>:<line>` unless noted. README.md (450 lines) is the
user-facing spec for behaviour and must stay true.

> **Biggest structural fact:** tessel-dock is not only a dock process. Its Python modules are a *library* imported
> by ~25 other components (see §9). A Rust tessel-dock binary can replace `tessel-dock.py`'s process, but the
> `.py` modules installed in `~/.local/share/tessel-dock/` (and copied beside other apps) must keep shipping
> until every consumer is ported.

---

## 1. Entry points & processes

| Item | Value | Ref |
|---|---|---|
| Runtime dir | `~/.local/share/tessel-dock/` — all `RUNTIME` modules + shared (`suite_i18n.py`, `language_data.py`, `suite_catalogs.py`, `clock_preferences.py`, `background_blur.py`, `libtessel-dock-background-blur.so`, `service_settings.py`), `icons/tessel-dock-app-menu-symbolic.svg`, `tessel-dock-toplevel`, `tessel-dock-workspaces`, `papirus-icons/` (private icon mirrors) | install.py:25-37, 239-262; papirus_palette.py:137 |
| Launcher | `~/.local/bin/tessel-dock` = `#!/bin/sh\nexec /usr/bin/python3 <runtime>/tessel-dock.py "$@"` | install.py:263-264 |
| systemd unit | `~/.config/systemd/user/tessel-dock.service`: `Type=notify`, `NotifyAccess=main`, `TimeoutStartSec=30`, `Environment=GDK_BACKEND=wayland`, `ExecStart=/usr/bin/python3 %h/.local/share/tessel-dock/tessel-dock.py`, `Restart=on-failure`, `RestartSec=1`, `After=`/`PartOf=graphical-session.target`, **no [Install]** (static; installer removes any enablement) | install.py:265-269 |
| .desktop | `~/.local/share/applications/tessel-dock.desktop`: Name=Dock Settings, `Exec="<launcher>" --settings`, Icon=preferences-desktop, Categories=Settings;DesktopSettings; | install.py:270-273 |
| labwc autostart | `systemctl --user start tessel-dock.service` (foreground; Type=notify makes it the **tray-readiness wait** before `labwc-xdg-autostart.target`). Installer's own form: `systemctl --user start tessel-dock.service >/dev/null 2>&1 &` (START_LINE); READY_LINE (no `&`, written by tessel-session-startup) counts as installed. Replaces recognized Cairo lines; unknown `cairo-dock` lines abort. | install.py:35-45,107-128; ~/.config/labwc/autostart:72; tessel-session-startup/configure_xdg_autostart.py:39 |
| labwc shutdown | ensures a `systemctl --user stop … tessel-dock.service …` line exists (accepts shared multi-unit line) | install.py:131-142 |
| rc.xml keybind | `<keybind key="Super_L" onRelease="yes"><action name="Execute" command="~/.local/bin/tessel-dock --menu-hotkey"/>`; installer rewrites `tessel-dock --menu` → `--menu-hotkey` and adds `onRelease="yes"` for Super_L/Super_R only | install.py:51-104; ~/.config/labwc/rc.xml:96-98 |
| Waybar-compat scripts | `~/.config/waybar/scripts/showdesktop.sh` → `tessel-dock --desktop-status`; `toggle_showdesktop.sh` → `tessel-dock --toggle-desktop` | tessel-shell/install_switcher.py:133-134 |
| Native helpers | `tessel-dock-toplevel`, `tessel-dock-workspaces` (C, built by build_helper.py with `wayland-scanner client-header/private-code`, `cc -std=c11 -O2 -Wall -Wextra -Werror`, pkg-config wayland-client) | build_helper.py:21-27 |
| Readiness | sd_notify `READY=1\nSTATUS=Dock and system tray ready` over `$NOTIFY_SOCKET` (abstract `@` supported) **only after** startup complete AND tray backend `ready` AND `host_registered`; sent once | service_readiness.py; tessel-dock.py:166-169 |
| Signals | SIGTERM/SIGINT → `app.quit()` (orderly shutdown closes helpers, restores previews) | tessel-dock.py:180-181,805-856 |

### CLI (`Gtk.Application`, id `org.tessel.Dock`, `HANDLES_COMMAND_LINE`) — tessel-dock.py:298-367
Second invocations forward argv to the primary via `org.gtk.Application.CommandLine`. If argv contains any of
`--check --quit --menu --menu-hotkey --toggle-desktop --desktop-status`, the app gets `IS_LAUNCHER` so it
**never starts a dock** when none is running (tessel-dock.py:1561-1566; test_dock_ui `menu_launcher_does_not_start_a_stopped_dock`).
Unknown option → stderr `Unknown option: X` exit 2.

| Flag | Behaviour / output | Exit |
|---|---|---|
| (none) | start dock (primary) | — |
| `--settings` | present (single) Dock Settings window | 0 |
| `--quit` | quit | 0 |
| `--menu` | toggle application menu on the output the compositor picks (see §5 probe) | 0 / 1 if no docks |
| `--menu-hotkey` | same, but suppressed when focused window is fullscreen and `menu_hotkey_fullscreen=false` (returns 0) | 0/1 |
| `--toggle-desktop` | minimize all / restore only windows it minimized, refocus previous focus | 0 / 1 if backend not ready |
| `--desktop-status` | prints `{"text","tooltip","class":[...],"active":bool}` (Waybar JSON) | 0 |
| `--check` | prints one JSON line: `ready, docks, mode, application_display, monitor_labels{name:label}, menu_hotkey_fullscreen, visible_groups{monitor:[...]}, app_docks[{monitor,edge,expanded,visible_groups}], hot_corners, windows, outputs[], error, icon_theme, appearance("dark"/"light"), background{transparency,frosted,frost_strength,transparency_enabled,apps_*,match_wallpaper,gtk_color_scheme_error,papirus_folders_error,rounded_corners,float_mode,colors,screens{m:{source,color}},error}, panel{workspaces_ready,workspaces,tray_ready,tray_watcher,tray_host_registered,tray_watcher_owner,tray_items,indicators[],indicator_states{},status_errors{}}` | 0 if ready && docks else 1 (installer health check uses this) |

Other scripts with CLIs (installed into source tree only, not runtime): `install.py --check|--apply|--rollback DIR`
(install.py:521-538), `migrate_waybar.py --check|--apply|--rollback`, `install_menu_release.py [--apply]`,
`labwc_titlebar_theme.py [--apply]` (runtime copy too), `app_catalog.py <mode> <target>` launch helper
(modes `--launch-desktop`, `--launch-file`, `--open-uri`; app_catalog.py:510-517), `fullscreen_guard.py --probe`
(X11 probe subprocess, fullscreen_guard.py:37).

---

## 2. D-Bus — owned (session bus)

### 2.1 `org.tessel.Dock` (GApplication unique name)
GApplication exports at `/org/tessel/Dock`: `org.gtk.Application` (Activate/Open/CommandLine — used by CLI
forwarding), `org.gtk.Actions` with app actions:
- `prepare-capture` (param `b` recording) — releases/keeps the Screen-Record preset popover's grab (panel_ui.py:116-152). Called by tessel-capture/capture_popups.py:4-25 via `org.gtk.Actions.Activate (sava{sv})`.
- `finish-screenshot` (no param) — restores popover autohide.
Name presence is also watched by tessel-login/session_transition.py:39 (fade waits for dock).

### 2.2 `/org/tessel/Settings`, interface `org.tessel.Settings` (settings_api.py:27-36)
| Member | Signature | Semantics |
|---|---|---|
| `Read` | `() → s` | JSON `{"config": <DockConfig dict>, "tray_apps": [{id,label,running}], "monitors": [{name,label}]}` |
| `Apply` | `(s) → s` | JSON `{"changes":{k:v},"expected":{k:v}}`; keys must equal; keys ⊆ SETTINGS_FIELDS; any `current[k]!=expected[k]` → error "Dock settings changed elsewhere. Reset this page before applying again."; validated via `DockConfig.from_dict`; rebuild docks unless only APPEARANCE/BORDER fields; returns new `Read` snapshot; ends caller's preview (settings_api.py:59-71,155-162) |
| `Preview` | `(s) → ()` | JSON with required `transparency(int 0-100), match_wallpaper, rounded_corners, float_mode`, optional `transparency_enabled, frosted, frost_strength(int 0-100), bar_border_*`; non-bools rejected. Preview owner = sender (settings_api.py:163-177) |
| `Restore` | `() → ()` | drop preview if sender is preview owner |
| `GetWorkareas` | `() → s` | `{"version":1,"monitors":[{"name","geometry":[x,y,w,h],"workarea":[x,y,w,h]}]}` from live bar exclusive zones (settings_api.py:39-53) |
| `GetPopupPlacement` | `(s monitor_connector, s group) → s` | `{"edge","offset"(12..4096),"align":"start|center|end"}`; group ∈ LAYOUT_GROUPS names; offset = dock thickness + edge_gap + magnifier headroom + 8; app-dock groups resolve to the separate app dock (settings_api.py:107-124) |
| `GetTrayApps` | `() → s` | JSON list `[{id,label,running:true}]` sorted by label casefold (tray_preferences.py) |
| signal `WorkareasChanged` | `(s)` | same payload as GetWorkareas; idle-coalesced, emitted only when payload changes; also once right after startup (tessel-dock.py:177-179, settings_api.py:126-138) |
Errors: `org.tessel.Settings.Error` with human message. Preview auto-restored when owner's unique name vanishes (NameOwnerChanged) and on shutdown.
Field sets (settings_api.py:11-26): APPEARANCE_FIELDS, BORDER_FIELDS, DOCK_FIELDS (+ `separate_app_dock`, all `app_dock_*`), DESKTOP_FIELDS (`menu_hotkey_fullscreen, hot_corners, hot_corner_delay_ms`), LAYOUT_FIELDS (`edge, alignment, hide_delay_ms, reveal_delay_ms`). `pins` is never patchable.
Consumers: tessel-settings/dock_pages.py (Read/Apply/Preview/Restore/GetTrayApps, NO_AUTO_START, 2000 ms), tessel-settings/layout_presets.py (SETTINGS_FIELDS), tessel-shell/desktop_workarea.py (GetWorkareas + WorkareasChanged), shared/popup_placement.py:69-90 (GetPopupPlacement, 500 ms timeout; all tray popups), notification UI.

### 2.3 `org.kde.StatusNotifierWatcher` at `/StatusNotifierWatcher` (status_tray.py:24-41)
Registered object always; name requested with `BusNameOwnerFlags.NONE` (queued, **never replaces** an existing watcher).
Methods `RegisterStatusNotifierItem(s)`, `RegisterStatusNotifierHost(s)`; props `RegisteredStatusNotifierItems as`, `IsStatusNotifierHostRegistered b` (=true), `ProtocolVersion i` (=0); signals `StatusNotifierItemRegistered(s)`, `…Unregistered(s)`, `StatusNotifierHostRegistered()`, `…HostUnregistered()`; emits `PropertiesChanged` for RegisteredStatusNotifierItems.
Registration forms (parse_service, :44-58): `/path` (sender + path), `name/path`, `name` (→ `/StatusNotifierItem`); validated name/object path, len ≤1024; item key = `name+path`.
When another process owns the watcher: call its `RegisterStatusNotifierHost(<own unique name>)` and `Get RegisteredStatusNotifierItems`; `host_registered` only after both succeed (generation-guarded); follow its Registered/Unregistered signals only from the owner. Items retained across watcher hand-off; removed when their owner vanishes (NameOwnerChanged).

---

## 3. D-Bus / services consumed

| Service | Use | Ref |
|---|---|---|
| `org.kde.StatusNotifierItem` (each item) | `GetAll` props; signals `NewIcon/NewAttentionIcon/NewOverlayIcon` force icon reload, others refresh props; methods `Activate(ii)`, `SecondaryActivate(ii)`, `ContextMenu(ii)` with **screen** coords, `Scroll(is)` delta=±120·steps, `"vertical"/"horizontal"`; `ItemIsMenu` → left click opens menu; Activate failure → fall back to DBusMenu | status_tray.py:300-420,578-615 |
| `com.canonical.dbusmenu` | `GetLayout(iias)(0,-1,[])`, `AboutToShow(i)`, `Event(isvu)(id,"clicked",<i 0>,0)`; signals `LayoutUpdated`, `ItemsPropertiesUpdated`; rendered as native `Gio.Menu` + `Gtk.PopoverMenu` (checkmark/radio/disabled/hidden/submenu/icon-name/icon-data/shortcut) | dbusmenu.py |
| `org.freedesktop.portal.Desktop` `/org/freedesktop/portal/desktop` `org.freedesktop.portal.Settings` | `ReadAll(["org.freedesktop.appearance"])`, `SettingChanged` for `color-scheme` (1 dark, 2 light, else GTK fallback `gtk-application-prefer-dark-theme`/theme name) | dock_appearance.py:13-16,156-210 |
| MPRIS `org.mpris.MediaPlayer2.*` `/org/mpris/MediaPlayer2` | ListNames + NameOwnerChanged, Properties GetAll/PropertiesChanged; methods `PlayPause/Previous/Next` on `.Player`, `Raise` on root; NO_AUTO_START 1500 ms; never launches players | lock_media.py, media_status.py |
| `org.tessel.Capture` `/org/tessel/Capture/Status` `org.tessel.Capture.Status` | `GetState() → (sb)`, signal `Changed(s)` state ∈ idle/preparing/recording/stopping (owned by tessel-capture/capture_tray.py) | capture_status.py |
| `org.tessel.KeepAwake` (`/org/tessel/KeepAwake`, iface `org.tessel.KeepAwake.Controls`) | `GetState() → s` (JSON indicator) + `Changed` signal via shared service_settings.SettingsClient | status_backend.py:279-285; shared/service_settings.py:53-110 |
| system bus `org.bluez` `/` ObjectManager | `GetManagedObjects` every 5 s → bluetooth indicator (Adapter1.Powered, Device1.Connected/Alias/Name) | status_backend.py:342-370,61-71 |
| system bus `org.freedesktop.systemd1` | Manager `Subscribe`, load units `avahi-daemon.socket`/`.service`, watch `ActiveState/LoadState`; toggle with `pkexec systemctl enable --now avahi-daemon.socket` / `disable --now avahi-daemon.socket avahi-daemon.service` | discovery_backend.py:9-60,75-238 |
| WirePlumber (`Wp` 0.5 GI, `libwireplumber-module-mixer-api`) | per-application volume/mute in window previews (`get-volume`/`set-volume` on mixer-api), on-demand only | application_audio.py:32-160 |
| GVFS `trash:///` | `monitor_directory` (fallback poll 10 s; 300 ms burst debounce), `trash::item-count`, empty = delete top-level children via GVFS only | tessel-dock.py:700-730; desktop_actions.py:140-182 |
| GSettings `org.gnome.desktop.wm.preferences button-layout` | window-button side | window_buttons.py:18,288-297 |
| Gio AppInfoMonitor | catalog refresh debounce 350 ms | tessel-dock.py:621-631 |

---

## 4. Wayland protocols

| Protocol | Where | Notes |
|---|---|---|
| `zwlr_layer_shell_v1` (via gtk4-layer-shell) | every surface | see §6 |
| `zwlr_foreign_toplevel_manager_v1` (≤v3), `wl_seat` (≤v7), `wl_output` (≤v4), `zxdg_output_manager_v1` (≤v3) | `tessel-dock-toplevel` helper | window list/activate/minimize/unminimize/close |
| `ext_workspace_manager_v1` v1 (+wl_output, xdg-output) | `tessel-dock-workspaces` helper | workspaces per output group |
| `ext_background_effect_manager_v1` + custom `tessel_blur_manager_v1` (`shared/protocols/tessel-blur-v1.xml`, `set_radius`) | `libtessel-dock-background-blur.so` (shared/background_blur.c), shares GTK's wl_display | blur under dock/popups; bundled labwc patch |
| X11 (Xwayland) via libX11 ctypes in subprocess | fullscreen_guard.py `--probe` | `_NET_ACTIVE_WINDOW`, `_NET_CLIENT_LIST`, `_NET_WM_STATE(_FULLSCREEN/_MAXIMIZED_*)`, `_NET_FRAME_EXTENTS`, `_MOTIF_WM_HINTS`, `_NET_WM_NAME`; only local `DISPLAY` (`:N`/`unix:N`), total deadline, ≤512 clients |

### 4.1 Helper stdio protocol (must be preserved if the Rust port keeps the helpers, or re-implemented in-process)
`tessel-dock-toplevel [--snapshot]` (any other arg → exit 2). stdout line-buffered JSON events:
- `{"event":"ready","seat":bool,"protocol_version":N}`
- `{"event":"output","id":<wl_registry name>,"name","description","x","y","width","height","scale"}` — logical size from xdg-output, else pixel/scale with 90/270 transform swap; re-emits all windows after
- `{"event":"output-removed","id":N}`
- `{"event":"window","id":"<u64>","app_id","title","activated","fullscreen","minimized","maximized","parent":"<u64 or empty>","outputs":[names]}` (strings capped 16384, control chars `\u00XX`)
- `{"event":"closed","id":"<u64>"}`, `{"event":"error","message"}`
stdin: exactly `<action> <id>\n`, action ∈ `activate|minimize|unminimize|close`; `activate` unminimizes first; needs seat. Bad → error event. Line >4096 → exit. (toplevel_helper.c:66-100,282-321)

`tessel-dock-workspaces [--snapshot]`: `{"event":"state","workspaces":[{"id","name","active","urgent","hidden","can_activate","group_id","coordinates":[..],"outputs":[..]}],"groups":[{"id","outputs"}]}`, then `{"event":"ready","protocol_version":1}`; `{"event":"unsupported","message"}` (exit 0, **no restart**); stdin `activate <id>\n` (decimal, non-zero, must be activatable & not hidden) → `activate` + `commit`. (workspace_helper.c:80-110,302-345)

Python side (window_backend.py, workspace_backend.py): IDs namespaced `"<session>:<native>"` so stale clicks after helper restart are rejected; startup timeout 8 s; retry backoff from 1 s; writes serialized via queue; helper loss clears state. WindowBackend is also used by tessel-shell, tessel-session/session_end.py, tessel-notifications, tessel-keyboard.

---

## 5. Behaviour: windows, menu, hotkeys

- **Menu target output** (tessel-dock.py:451-529): one dock → that monitor. Multiple → map an invisible 1×1 OVERLAY layer surface (`tessel-dock-menu-target`, empty input region, no keyboard), after 60 ms read `get_monitor_at_surface`; fallback: output of activated window → first monitor. A second press while probing cancels (toggle). Other clones close their menus first. Separate app dock: bar's menu used.
- **Fullscreen guard**: native foreign-toplevel `fullscreen` of activated+unminimized window is exact; Xwayland borderless detection runs in a worker thread, verdict cached by focus signature, read-only on hotkey (never blocks). Unknown X11 fails open.
- **Show desktop**: remembers the set it minimized + previously focused id; restore only those (even before minimize ack), refocus. Cleared when those windows close.
- **Window click**: focused & not minimized → minimize, else activate (restores maximized state). Single-window apps act directly; multi → preview popup. Close button per preview row closes only that window. "Close All" snapshots ids.
- **Focused title** (title mode): click minimizes and remembers id across compositor's auto focus hand-off; next click restores same window (focused_title.py).
- **Activities**: `/usr/bin/python3 ~/.local/share/tessel-shell/tessel_shell.py show-activities --monitor <name>` (one in flight; stderr shown as error) (tessel-dock.py:423-449). Used by Activities button & hot corners.
- **Hot corners** (hot_corners.py): per `(monitor, corner)` TOP-layer sensor `tessel-dock-hot-corner`, 3×3 corner + 48 px guard strip input region, dwell `hot_corner_delay_ms` (100–2000, default 150), latched until pointer leaves (no re-open after Escape), blocked by fullscreen, raised above bar when dock goes TOP.
- **Monitor naming** (tessel-dock.py:531-557, monitor_label :55-66): match GDK monitor to helper output by exact geometry; name = wl_output name (connector); label `"<name> · <description minus ' (name)' suffix or model>"`, ignore unknown/(null)/none/n/a; duplicates suffixed `-<index>`; fallback `monitor-N` / "Display N".
- **Monitor selection** `config.monitor`: `all` | `primary` (first GDK monitor) | name (missing → first).
- **App launch** (app_catalog.py:34-120): every app runs in its own transient unit: `/usr/bin/systemd-run --user --collect --quiet --service-type=forking --property=GuessMainPID=no --property=ExitType=cgroup --property=TimeoutStartSec=90s --slice=app.slice --expand-environment=no --unit=app-tessel-dock-<uuid hex>.service [--working-directory=] --setenv=NAME… -- python3 app_catalog.py --launch-desktop|--launch-file|--open-uri <target>`; env values passed via environment not argv; strips service vars (`INVOCATION_ID, JOURNAL_STREAM, NOTIFY_SOCKET, …, TESSEL_DOCK_LAUNCH_STARTUP_ID`); startup token forwarded in `TESSEL_DOCK_LAUNCH_STARTUP_ID`; never parses Exec itself; never falls back to direct spawn. `launch_process()` variant uses `--service-type=exec`.
- **Panel applet popups** (panel_actions.py): `systemd-run --user --quiet --collect --service-type=exec --slice=app.slice --expand-environment=no --setenv=WAYLAND_DISPLAY,DISPLAY,XAUTHORITY,XDG_RUNTIME_DIR,DBUS_SESSION_BUS_ADDRESS,XDG_CURRENT_DESKTOP,XDG_SESSION_TYPE,LABWC_PID -- <sys.executable> ~/.config/waybar/scripts/<folder>/<script> <args>`; args table §7.

---

## 6. UI surfaces (gtk4-layer-shell)

`surface()` helper (dock_ui.py:115-126): undecorated `Gtk.Window`, css `tessel-dock-surface`, exclusive zone −1, keyboard NONE (or ON_DEMAND).

| Namespace | Layer | Anchors / zone | Keyboard | Purpose |
|---|---|---|---|---|
| `tessel-dock` | BOTTOM ↔ TOP (hover promotes to TOP; fullscreen stays above) | edge (+both sides in bar mode, + alignment side in dock), margin = gap if float | NONE | visual dock/bar (dock_ui.py:208,559-572) |
| `tessel-dock-reveal` | TOP | edge (+sides in bar) ; **exclusive zone = thickness+gap in bar mode**, −1 otherwise | NONE | hover sensor + workarea reservation (stays mapped while auto-hidden) (:199,615-656) |
| `tessel-dock-app-reservation` | BOTTOM | edge + both sides, empty input region | NONE | separate expanded app dock reservation (:214-226) |
| `tessel-dock-applications` | OVERLAY | positioned by `position_popup` | **EXCLUSIVE** while open | app menu (:1752-1791) |
| `tessel-dock-menu-dismiss` | OVERLAY | all 4 edges, every monitor; dismiss on release | NONE | outside-click catchers |
| `tessel-dock-windows` | TOP | popup | NONE | window preview |
| `tessel-dock-tray-dismiss` / `tessel-dock-<kind>-dismiss` | TOP | full monitor, input region with holes for dock/preview | NONE | tray/applications expander dismissal (status_tray.py:579-640,787) |
| `tessel-dock-hot-corner` | TOP | corner, partial input region | NONE | hot corners |
| `tessel-dock-menu-target` | OVERLAY | 1×1, empty input | NONE | menu output probe |
Context menus / tray menus = `Gtk.Popover` (`tessel-dock-context-menu`, positioned away from edge). Errors = `Gtk.MessageDialog` with `tessel-dock-settings` class. Magnifier: transparent headroom beside panel, input region restricted to panel (dock_ui.py:62-113,1314-1327).
Workarea rule: bar reserves even while hidden; when any non-minimized window on that monitor is maximized, bar temporarily drops float gap and rounding (reservation = thickness only) — per monitor.

**CSS classes** (dock_style.py CSS 29 KB + popup_css/background_css generated per clone scope `tessel-dock-palette-<id hex>` and `…-applications`): `tessel-dock-surface tessel-dock-panel tessel-dock-vertical tessel-dock-compact-bar tessel-dock-compact-item tessel-dock-separator tessel-dock-indicators tessel-dock-bar-start tessel-dock-status status-<name> tessel-dock-status-glyph tessel-dock-status-icon tessel-dock-indicator tessel-dock-tray tessel-dock-tray-item tessel-dock-tray-expander tessel-dock-applications-expander tessel-dock-workspaces tessel-dock-workspace(.active .urgent) tessel-dock-clock tessel-dock-clock-time tessel-dock-clock-date tessel-dock-clock-weekday tessel-dock-media tessel-dock-media-control tessel-dock-media-song tessel-dock-focused-title tessel-dock-focused-title-button tessel-dock-activities-icon tessel-dock-count tessel-dock-popup tessel-dock-app-menu tessel-dock-app-menu-header tessel-dock-app-menu-settings tessel-dock-app-categories tessel-dock-app-section tessel-dock-app-name tessel-dock-app-pin tessel-dock-app-frequent(-list,-tile) tessel-dock-empty-cell tessel-dock-title tessel-dock-subtitle tessel-dock-window-title tessel-dock-window-close tessel-dock-context-menu tessel-dock-settings settings-card settings-tile settings-muted settings-hint settings-section-heading settings-section-title`; indicator state classes from scripts (lower-cased `[A-Za-z0-9_-]`). Global Settings re-styles `.tessel-settings .tessel-dock-settings` (tessel-settings/global_style.py:190).
Drag & drop: string `tessel-dock-app:<desktop-id>` (dock_ui.py:30) between menu and dock for pin/reorder.

---

## 7. How tessel-dock hosts applets (indicators) — status_backend.py, panel_ui.py, panel_actions.py

Indicator names (DEFAULT_APPS, dock_config.py:38-45): `idle_inhibitor screen_record notifications power battery capslock bluetooth tiling keyboard network discovery audio brightness showdesktop` (+ `media`, `weather` groups). ALWAYS_VISIBLE_APPS default `idle_inhibitor capslock audio brightness showdesktop`; the others are hidden in the system tray until "More apps" is clicked (Tessel change, see **System tray** below).

**Status sources** (one shared monitor for all clones):
| Name | Source | Ref |
|---|---|---|
| power, battery, brightness, network, capslock, notifications, weather, keyboard, tiling | long-running `/usr/bin/python3 -u ~/.config/waybar/scripts/<STREAMS path>` (weather adds `--watch`), one JSON line per update `{"text":str,"tooltip"?:str,"class"?:str|[str]}`; Pango markup stripped; capslock hidden when text empty; exit/invalid → "unavailable" + restart after 3 s; line >65536 rejected; startup deadline 10 s until first line | status_backend.py:30-36,90-187,259-330 |
| audio | `/usr/bin/wpctl get-volume @DEFAULT_AUDIO_SINK@` every 2 s (`Volume: 0.42 [MUTED]`); scroll → `wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ N%+/-` (5%/step, coalesced ±20, serialized) | :61-74,277,404-414 |
| bluetooth | BlueZ GetManagedObjects / 5 s | :342-370 |
| idle_inhibitor | KeepAwake D-Bus; click → `python3 ~/.config/waybar/scripts/tessel-keep-awake/keep_awake.py --toggle` | :279,394 |
| discovery | systemd1 watch (above) | :287 |
| screen_record | org.tessel.Capture; visible only if `/usr/local/bin/tessel-capture` exists | :240-256 |
| media | MPRIS (inline controls + popup); only while the Media group is on (off by default) | media_status.py |
| showdesktop | internal | :400-403 |

STREAMS paths: `tessel-power/power_status.py, tessel-battery/battery_status.py, tessel-brightness/brightness_status.py, tessel-network/network_status.py, capslock.py, tessel-notifications/notification_status.py, tessel-weather/weather_status.py, tessel-keyboard/osk_status.py, tessel-tiling/tiling_status.py`.
Glyph→themed symbolic icon mapping and class semantics (wifi strength glyphs `󰤟󰤢󰤥󰤨`, `vpn` badge, battery `..`, `plugged`, power profile classes, `dnd`…) in panel_ui.py:53-105 — must be ported verbatim. Status icon size: compact `clamp(round(icon*.4),12,20)` else `clamp(round(icon*.5),14,32)`; tray 16 px compact else 22 px.

**Clicks** → `application_command(name, settings, edge, offset)` (panel_actions.py:9-48), script under `~/.config/waybar/scripts/<folder>/<script>`, offset = `max(40, thickness+gap+8)`:
- left: `--popup --popup-edge <edge> --popup-offset <n>`; right (settings): `--settings` except bluetooth/idle_inhibitor/session/media (they get the popup again; bluetooth has no right-click handler; idle_inhibitor right-click toggles).
- discovery: both → popup. keyboard: `--toggle` / `--settings`. tiling: `--toggle` / `--settings --popup-edge E --popup-offset N`.
- folders: media→tessel-media/media_manager.py, session→tessel-session/session_manager.py, notifications, idle_inhibitor→tessel-keep-awake/keep_awake.py, power, brightness, battery, network→tessel-network/network_manager.py, discovery→tessel-network-discovery/discovery_manager.py, bluetooth→tessel-bluetooth/tessel-bluetooth.py, tiling, keyboard→tessel-keyboard/osk_manager.py, audio, calendar (clock click; right-click `--settings`), weather.
- screen_record: left = preset popover (`tessel-capture --start-recording --audio-preset none|desktop|desktop-mic`), during recording `--stop`, gear/right = `--record`.
- Settings utility button → `~/.local/bin/tessel-settings`; System Monitor → `/usr/local/bin/tessel-system-monitor`; logout → `labwc --exit` (only after confirmation); Home/Trash → `launch_uri` (default handler for `file://`/`trash:///`).

**System tray** (Tessel change: one group, `service_apps`, labelled "System tray"; the separate `system_tray` arrow expander and the hover expansion of the applets are gone). Order on every edge, in dock and bar mode: regular StatusNotifier items, always-shown items, the applets (DEFAULT_APPS order), the "More apps" button (`view-more-symbolic`, tooltip and accessible name "More apps", accessible state *expanded*). Collapsed: always-shown items + always-visible applets + More apps; expanded: also the regular (non-passive) items and the other available applets. More apps is hidden when nothing is hidden. Only More apps expands it (click; Enter/Space from the keyboard); it never expands on hover or focus and never collapses on a timer or pointer leave. It collapses on a second click, a click outside every dock (`tessel-dock-tray-dismiss` catchers, holes for every visible dock), Esc inside the tray, a popup opened from outside the tray (calendar, session, weather, media), a dock menu opened outside the tray, and when a menu opened inside the tray (item DBusMenu, application-drawn ContextMenu, screen-record menu) closes; while such a menu is open it stays expanded without catchers. An applet popup opened from the tray keeps it expanded (popups are OVERLAY with their own dismissal). Expanded, it holds the dock's `tray-expanded` token (keeps an auto-hidden dock shown), released on every collapse and on close. StatusNotifier: main tray + "always" tray filtered by `always_show_tray` identities (`id:` / `desktop:` / `name:` (non-unique) / `title:` prefix, ≤500 chars; tray_preferences.py). Pixmaps ARGB32 network order → RGBA, pick smallest ≥ size, dims ≤1024. `IconThemePath` gets a private IconTheme per path. Tooltip HTML stripped.

---

## 8. Files

| Path | Format | Writers / readers |
|---|---|---|
| `$XDG_CONFIG_HOME/tessel-dock/config.json` | `{"version":1, …all DockConfig fields}` indent 2, atomic (mkstemp `.dock-settings-*` + fsync + replace); >1 MiB rejected; missing → defaults + Cairo import (never writes on load); malformed → error dialog, file preserved | W: tessel-dock (save_config), installer (first run, mode 0600), tessel-settings via D-Bus Apply, or **directly via ConfigStore when dock offline** (dock_pages.py:31-90); R: tessel-notifications/notification_placement.py:17 (`edge`), tessel-shell/install_switcher.py:34, tessel-media/install.py |
| `$XDG_CACHE_HOME/tessel-dock/palette.json` | `{"version":1,"is_dark","transparency","apps_transparency","apps_transparency_enabled","apps_frosted","apps_frost_strength","apps_rounded_corners","colored_window_buttons","monitors":[{"name","geometry":[x,y,w,h],"background":[r,g,b],"foreground":[r,g,b],"accent":[r,g,b]}]}` compact, `sort_keys`, trailing `\n`, atomic `.palette-*`; rewritten only when bytes differ (incl. across restarts) | R: companion_theme.py (all suite apps, dir monitor), brightness_keys.py, tiling_outline.py, labwc_titlebar_theme.py, tessel-lock |
| `$XDG_STATE_HOME/tessel-dock/launch-counts.json` | `{desktop_id: int}` ≤200 | tessel-dock (Frequent row) |
| `$XDG_CACHE_HOME/tessel-dock/frosted/` | ≤64 files/16 MiB textures `tessel-dock-frosted-v3` | frosted_wallpaper.py (legacy) |
| `$XDG_CONFIG_HOME/gtk-4.0/gtk.css` | appends exact line `@import url("tessel-dock-colors.css"); /* Tessel Dock wallpaper colors */`; backup `gtk.css.tessel-dock-original` once; generated `tessel-dock-colors.css` (header `/* Tessel Dock managed GTK4 wallpaper colors. */`) with GTK named colors + libadwaita vars + window controls; refuses symlinks, >2 MiB; BOM/line-endings preserved | gtk_color_scheme.py:18-25,183-190 |
| `$XDG_CONFIG_HOME/labwc/environment` (read) | block `# tessel-displays primary output environment begin/end`, keys `SDL_VIDEO_DISPLAY_PRIORITY, WAYLANDDRV_PRIMARY_MONITOR, PROTON_WAYLAND_MONITOR` → primary display for global palette; parsed with shlex, never executed | gtk_color_scheme.py:23-25,196-230 (written by tessel-displays) |
| `~/.config/labwc/rc.xml` | `<theme><titlebar><layout>` (left/right buttons), `<theme><dropShadows>yes/no` ; then async `labwc --reconfigure` (coalesced, only if `LABWC_PID`) | window_buttons.py:160-330 |
| labwc active theme `…/themes/<name>/openbox-3/` | `themerc` (only button/padding/title/menu/OSD color keys) + SVG buttons `{close,iconify,max,max_toggled…}-{active,inactive}.svg`, `_hover-*`; only if theme already managed; CLI backups `~/.local/state/tessel/backups/labwc-theme-<stamp>` | labwc_titlebar_theme.py |
| `~/.config/gtk-3.0/settings.ini`, `gtk-4.0/settings.ini` | `gtk-decoration-layout` (only if file exists) | window_buttons.py:299-309 |
| `$XDG_DATA_HOME/icons/<Papirus*>` symlink → `~/.local/share/tessel-dock/papirus-icons/theme-<name>` + marker `.tessel-dock-papirus.json` (owner `tessel-dock-papirus-palette-v1`) | private Papirus mirrors recoloured with `papirus-folders`, `gtk-update-icon-cache` | papirus_palette.py:22-24,137,300-330 |
| `~/.config/tessel-shell/wallpapers.json` + referenced files (read) | per-output wallpaper selection (tessel-shell's contract); 3 s poll fallback + file monitors; video → `ffmpeg` representative frame | wallpaper_palette.py:30,100-130,339 |
| `$XDG_CONFIG_HOME/tessel/clock.json` (read) | clock format/order (shared/clock_preferences.py:14) | also calendar, tessel-keep-awake, tessel-lock, GS |
| `~/.config/cairo-dock/current_theme/launchers/*.desktop` (read, first run only) | Container=_MainDock_, Order, Origin; Exec never used | dock_config.py:300-336 |
| Installer backups `~/.local/state/tessel/…/tessel-dock/backups` | manifest.json | install.py, migrate_waybar.py |

### DockConfig schema (dock_config.py:49-114, validation :160-287) — exact names/defaults/ranges are contract
`pins[]` (desktop ids, `*.desktop`, no `/`, ≤512, dedup) · `mode` dock|bar · `separate_app_dock` false · `app_dock_edge` left|right|bottom ("left") · `app_dock_expand` true · `app_dock_icon_size` 40 (16-128) · `app_dock_gap` 8 (0-48) · `app_dock_float_mode` false · `app_dock_rounded_corners` false · `app_dock_transparency` 5 · `app_dock_transparency_enabled` true · `app_dock_frosted` false · `app_dock_frost_strength` 50 · `app_dock_match_wallpaper` false · `app_dock_background_color` "#38373a" · `app_dock_auto_hide` false · `app_dock_shine` false · `app_dock_group_layout` {} · `application_display` icons|title · `always_show_tray[]` (≤512 ids) · `always_visible_apps[]` (DEFAULT_APPS names; "media" stripped on load) · `group_layouts{dock|bar:{start,center,end:[groups]}}` · `edge` bottom · `alignment` start|center|end · `monitor` "all" · `monitor_overrides{name:{edge?,alignment?}}` · `icon_size` 40 · `gap` 8 · `hover_scale` 1.0 (1-2 float) · `magnification` false · `compact_bar` false · `bar_islands` false (Tessel addition: draw the bar's start, center and end as separate islands; bars only, joined again while a window is maximized) · `transparency` 5 · `transparency_enabled` true · `frosted` false · `frost_strength` 50 · `apps_transparency` 5 · `apps_transparency_enabled` false · `apps_frosted` false · `apps_frost_strength` 50 · `apps_rounded_corners` true · `window_buttons_left` false · `colored_window_buttons` false · `window_shadows` true · `match_wallpaper` false · `background_color` null|#rrggbb · `rounded_corners` true · `float_mode` true · `bar_rounded_corners` false · `bar_float_mode` false · `bar_border_enabled` false · `bar_border_sides` [top,bottom,left,right] (canonical order) · `bar_border_color` "#777777" · `hide_on_fullscreen` true (legacy name = "keep behind windows") · `menu_hotkey_fullscreen` false · `group_visibility{12 GROUPS:bool}` (Tessel change: `system_tray` is no longer a group; a saved `system_tray` key is accepted and merged: `service_apps` = saved `service_apps` (default true) OR saved `system_tray`; it is never written again. Missing groups default true, except `media`, which defaults false (Tessel change: the Media switch is off by default and also stops the dock following MPRIS players; a legacy `"media"` in `always_visible_apps` without a saved `media` key means on)) · `hot_corners{monitor:[top_left|top_right|bottom_left|bottom_right]}` · `hot_corner_delay_ms` 150 (100-2000) · `auto_hide` false · `reveal_delay_ms` 120 (0-5000) · `hide_delay_ms` 500 (0-5000). Colors lower-cased. Ints must be real ints (bool rejected). Migration: missing `*frost_strength` with `*frosted=true` ← `*transparency` (default 20). `version` must be 1.
Derived: `effective_rounded_corners/float_mode` pick `bar_*` in bar mode; `uses_separate_app_dock = separate_app_dock && mode==bar && edge==top`; `for_monitor()` deep-copies; `for_app_dock()`.
Group layouts (group_layout.py): LAYOUT_GROUPS (12; Tessel change: `system_tray` merged into `service_apps`, labelled "System tray"), POSITIONS start/center/end, defaults per mode×display (:15-30), validation adds missing groups to defaults, inserts `media` before `system_tray` (else `service_apps`) in old custom layouts, then drops `system_tray` when the layout also has `service_apps` (the tray stays where `service_apps` was) or renames it to `service_apps`; `GetPopupPlacement(_, "system_tray")` answers for `service_apps`; app dock groups `app_menu pinned running files`.

---

## 9. Library surface used by other components (must remain importable Python until they are ported)
`companion_theme.py` (attach/attach_application/PaletteClient/theme_css — ~20 apps; copied beside entrypoints by their installers), `app_catalog.py` (AppCatalog, launch_uri, launch_process, DesktopAppInfo, launch_command, `_launch_environment`, `_TransferredLaunchContext` — tessel-files, brightness, tessel-shell, capture), `window_backend.py` + `tessel-dock-toplevel` (tessel-shell switcher, session_end, tessel-notifications, OSK), `dock_style.py` (color_palette, CSS — tessel-lock, GS), `dock_config.py`/`settings_api.py`/`tessel-dock.SettingsWindow`/`monitor_label`/`group_layout_settings.py` (Global Settings embeds the real settings window: tessel-settings/dock_pages.py:13-21), `dock_appearance.AppearanceController` (tessel-files, desktop-apps), `lock_media*.py` (tessel-media, tessel-lock), `application_audio.py` (tessel-shell switcher), `install.py` helpers `atomic_write/snapshot/restore_file/menu_hotkey_text/HOME_LAUNCHER/RUNTIME` (≈10 installers), `build_helper.py` (tessel-shell), `labwc_titlebar_theme.py` (tessel-session-startup). → Port plan: keep these modules installed; Rust binary only replaces the `tessel-dock.service` process and must keep every file/D-Bus contract they rely on (palette.json, org.tessel.Settings, config.json).

---

## 10. Parity test cases (from tests; port as Rust/integration tests)
1. Settings API: Apply merges only changed fields, never touches pins or other pages' fields; conflicting `expected` → error; internal/unknown fields rejected; preview restored when previewing client disconnects (test_settings_api).
2. Config: load never writes; missing file → defaults+Cairo import (order by Order, installed only); empty pin list preserved; malformed preserved & reported; all type/range rejections; legacy migrations (frost strength, media in always_visible_apps, center alignment); dock vs bar independent corners/float; `for_monitor` no aliasing (test_dock_config, 32 cases).
3. StatusNotifier: existing watcher not replaced; hand-off keeps legacy items; host_registered only after RegisterStatusNotifierHost + items read; item removed on owner exit; service forms; ARGB→RGBA & best pixmap; HTML tooltips plain; NewIcon forces same-filename reload, title signals don't (test_status_tray, test_tray_refresh).
4. Readiness: READY=1 only after startup + tray host; once; abstract socket; no socket ok (test_service_readiness).
5. Status backend: one process per stream; dedup + coalesced callbacks; stream exit → unavailable then restart in 3 s; timeout kills silent child; oversized/unterminated/invalid JSON handling; final unterminated line drained; audio scroll coalesce/serialize; tessel-keep-awake via D-Bus without polling; close cancels all (test_status_backend, 18).
6. Panel actions: exact argv per applet/edge; detached via systemd-run with only listed env; missing script → error, no fallback; systemd-run failure reported, never direct child (test_panel_actions, 15).
7. App launch: unique unit per launch; literal args; env not in argv; startup token forwarded; failure cancels notification; app survives dock stop/SIGKILL (test_app_launch, test_app_lifetime).
8. Window backend: ids namespaced per helper session; stale/unknown never written; missing seat refuses activate; maximized ≠ fullscreen; output rename/unplug; helper loss clears & reconnects; malformed events fail cleanly (test_window_backend, test_workspace_backend incl. "unsupported compositor does not restart").
9. Dock model: pins join alias windows (StartupWMClass), no duplicates on unpin/close/repin, first-seen order stable, empty app_id windows separate, dialogs inherit parent, missing pins hidden but retained; utility routing of Home/Settings/Trash windows (conservative trash title match, child dialogs follow parent) (test_dock_model, test_utility_groups).
10. Hover: sensor→dock grace period, menu/preview hold, stale timer cannot hide new hover, per-output independence, zero delay (test_dock_model HoverState).
11. Bar reservation: every edge reserves bar+gap even while hidden; dock never reserves; maximized window drops gap & rounding per monitor until last maximized leaves; WorkareasChanged coalesced and only on change (test_bar_reservation, test_workareas).
12. Menu hotkey: uses current monitor, toggles, cancels pending repeat, closes other clones; launcher never starts dock; fullscreen guard reads cached state, ignores unfocused fullscreen; click/`--menu` still work (test_dock_ui).
13. Show desktop restores only its own windows, preserves pre-existing minimization and maximized state, works before minimize ack (test_dock_ui, test_native_integration).
14. Focused title minimize/restore through focus hand-off, shared across clones (test_focused_title).
15. Hot corners: dwell/cancel/re-entry, occlusion latch, fullscreen blocks, hotplug, coexist with full-width bar (test_hot_corners).
16. Fullscreen guard X11 probe: borderless screen-sized + negative-origin outputs; maximized/decorated allowed; remote DISPLAY never contacted; rate limit; stops probing native Wayland windows after two client lists (test_fullscreen_guard).
17. Palette file: unchanged payload & restart don't rewrite; failed replace keeps old snapshot; unwritable cache non-fatal (test_palette_state).
18. GTK css: exact user bytes preserved, single backup, symlinks refused, oversized untouched, concurrent edits retained, disable removes only managed import (test_gtk_color_scheme, 18).
19. Window buttons: edits rc.xml only when different, reload off main loop & coalesced, no reload outside labwc (test_window_buttons).
20. Papirus: user themes untouched, restore original colour on disable (test_papirus_palette, 15).
21. Trash: empty only top-level via GVFS, cancel, duplicate requests blocked, count via metadata only (test_desktop_actions).
22. Tray expander: one dismissal set across clones, holes keep preview clickable, catchers never cover fullscreen, released when hidden/destroyed (test_tray_expander).
23. Installer: preview writes nothing; health check (`--check`) before retiring Cairo; rollback restores files/enablement/keybinds; idempotent autostart/shutdown edits (test_install, 24).
24. Native integration in private headless labwc (test_native_integration.py, 15; recipe there: `labwc --config-dir`, `WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2`, private bus) — fullscreen game receives edge clicks, per-monitor reservation, float gap tracking, singleton/health/shutdown.

---

## 11. Module map (runtime, LOC)

| File | LOC | Responsibility |
|---|---|---|
| tessel-dock.py | 1568 | Gtk.Application, CLI, startup wiring, monitors, menu probe, show-desktop, trash watch, palette export, preview state, **SettingsWindow** (~700 LOC, embedded by GS) |
| dock_ui.py | 2273 | DockSurface: layer surfaces, hover/layer sync, groups & layout, bar sizing/reservation, buttons, previews, context menus, app menu (search, categories, frequent, DnD pins), magnifier |
| status_tray.py | 871 | SNI watcher+host backend, StatusTray widget, TrayExpander + dismissal surfaces |
| companion_theme.py | 671 | palette.json client + per-monitor CSS for companion apps (library) |
| panel_ui.py | 621 | DockIndicators: indicator buttons, glyph→icon map, clock, workspaces, session, capture popover |
| install.py | 541 | installer (also helper library) |
| wallpaper_palette.py | 530 | wallpaper sampling worker (Pillow/ffmpeg), watchers |
| app_catalog.py | 517 | desktop catalog, search/categories, LaunchHistory, systemd-run launches + helper main |
| dock_style.py | 513 | static CSS, light CSS, palette maths, background/popup CSS |
| status_backend.py | 434 | indicator monitors (scripts, wpctl, BlueZ, D-Bus clients) |
| dock_config.py | 401 | DockConfig schema/validation, ConfigStore, Cairo import |
| toplevel_helper.c / workspace_helper.c | 352 / 374 | Wayland helpers |
| papirus_palette.py, window_buttons.py, fullscreen_guard.py, workspace_backend.py, window_backend.py, gtk_color_scheme.py, lock_media.py, migrate_waybar.py, dock_model.py | 351, 351, 350, 341, 312, 309, 294, 276, 287 | as named in §3-8 |
| discovery_backend.py, hot_corners.py, frosted_wallpaper.py, dock_appearance.py, labwc_titlebar_theme.py, settings_api.py, dbusmenu.py, desktop_actions.py, application_audio.py | 238, 234, 222, 210, 205, 199, 188, 182, 178 | |
| gtk4_support.py, group_layout_settings.py, media_controls.py, panel_actions.py, lock_media_icons.py, group_layout.py, palette_state.py, build_helper.py, media_status.py, capture_status.py, focused_title.py, install_menu_release.py, tray_preferences.py, service_readiness.py | 146, 131, 126, 117, 95, 93, 73, 66, 54, 53, 39, 30, 25, 24 | |

## 12. Suggested porting split
1. **W-a core model (pure Rust, no GTK)**: DockConfig serde + validation + group_layout + ConfigStore atomic IO; dock_model (grouping, utility partition, HoverState); tray_preferences; status indicator JSON parsing; panel_actions argv builder; app launch argv (`launch_command`). Tests 2,7(argv),9,10.
2. **W-b Wayland backends**: in-process `wayland-client` foreign-toplevel + ext-workspace + xdg-output on a separate `wl_display` connection (or keep the C helpers and port only the stdio client). Keep helper binaries available for tessel-shell/tessel-session meanwhile. Tests 8.
3. **W-c D-Bus (zbus)**: org.tessel.Settings, SNI watcher/host + dbusmenu→GMenu, GApplication actions/command-line (gtk4-rs `ApplicationFlags::HANDLES_COMMAND_LINE`), sd_notify readiness, MPRIS/Capture/KeepAwake/BlueZ/systemd1 clients. Tests 1,3,4,5.
4. **W-d appearance publishers**: palette.json, gtk.css/tessel-dock-colors.css, window_buttons (rc.xml/settings.ini/gsettings/themerc), papirus mirrors, portal colour-scheme, wallpaper sampler. Tests 17-20. (Reuse tessel-gtk palette maths.)
5. **W-e surfaces**: DockSurface layer windows, bar reservation/workareas, hover layer switching, magnifier, hot corners, menu probe, dismissal catchers, blur via tessel-gtk background_blur. Tests 11,12,15,22.
6. **W-f widgets**: app menu (search ranking/categories/frequent/DnD), previews (+WirePlumber per-app audio), indicators/clock/workspaces/media inline, tray widgets, context menus, Trash/Home. Tests 6,13,14,21.
7. **W-g settings window**: native Rust Dock Settings for `--settings`; Global Settings keeps embedding the Python SettingsWindow until GS is ported (so Python modules stay installed).
8. **W-h install/swap**: `tools/install.py --only tessel-dock` flips `ExecStart` in tessel-dock.service, keeps launcher path `~/.local/bin/tessel-dock`, keeps Type=notify; integration tests in headless labwc (test 24).
