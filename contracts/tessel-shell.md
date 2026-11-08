# Contract: tessel-shell

Python source: `~/tessel-bluetooth/tessel-shell/` (8340 LOC incl. tests; ~5 400 runtime LOC in
`tessel-shell` (4910) plus helpers). Refs are `file:line` relative to that directory unless prefixed.

Tessel Shell owns four things:
1. **Desktop daemon**: per-monitor wallpaper and desktop icon layer, the wallpaper settings
   window, and the `tessel-shell.sock` CLI socket.
2. **Window switcher / Activities overview**: Alt+Tab plus hot-corner overview. It runs inside
   the daemon.
3. **`tessel-prompt`**: on-demand labwc promptCommand/errorCommand dialog.
4. **`tessel-capture-picker`**: on-demand xdg-desktop-portal-wlr dmenu chooser.

`tessel-shell-lock` lives in `~/.local/bin` but belongs to tessel-lock and is out of scope here.

---

## 1. Entry points and processes

| Executable | Installed at | Started by | Notes |
|---|---|---|---|
| `tessel-shell` (python, GApplication) | `~/.local/share/tessel-shell/tessel-shell` (0755) | `tessel-shell.service` | Daemon, and also a CLI client (see §2.1). |
| `tessel_shell.py` | `~/.local/share/tessel-shell/tessel_shell.py` | labwc `A-Tab`/`A-S-Tab` keybinds, tessel-dock hot corners | Lightweight client: no GTK import. |
| `tessel-shell-toplevel` (C) | `~/.local/share/tessel-shell/tessel-shell-toplevel` | spawned by the daemon (switcher backend) | Same binary as `tessel-dock/tessel-dock-toplevel` (built from `tessel-dock/toplevel_helper.c`, install_switcher.py:84). |
| `tessel-overview` (C) | `~/.local/share/tessel-shell/tessel-overview` | spawned per Activities open | Built from `overview_capture.c` by `build_overview.py` into `$XDG_CACHE_HOME/tessel/tessel-overview`, then copied. |
| `tessel-prompt` | `~/.local/bin/tessel-prompt` = `#!/bin/sh exec /usr/bin/python3 ~/.local/share/tessel-shell/tessel_shell_prompt.py "$@"` | labwc `core/promptCommand`, `core/errorCommand` | install_prompts.py:100 |
| `tessel-capture-picker` | `~/.local/bin/tessel-capture-picker` → `capture_picker.py` | xdpw `chooser_cmd` | install_capture_picker.py:45 |

### systemd user unit `tessel-shell.service`
- Base template: `tessel-session-startup/units/tessel-shell.service`, created only if missing (configure_session.py:586-592).
  It contains `Type=simple`, `EnvironmentFile=-%h/.config/labwc/environment`,
  `ExecStart=%h/.local/share/tessel-shell/tessel-shell`, `Restart=on-failure`, `After=graphical-session.target`.
- The live `~/.config/systemd/user/tessel-shell.service` is older (`ExecStart=/usr/bin/tessel-shell`, a stale
  Sept copy). It is overridden by drop-ins:
  - `tessel-shell.service.d/palette.conf`: `ExecStart=` then `ExecStart=%h/.local/share/tessel-shell/tessel-shell`.
  - `tessel-shell.service.d/50-tessel-recovery.conf` (configure_session.py:596): `StartLimitIntervalSec=60`,
    `StartLimitBurst=5`, `OnFailure=tessel-failed@%n.service`, `Restart=on-failure`, `RestartSec=1`,
    `EnvironmentFile=-%h/.config/labwc/environment.d/90-local.env`.
- **Rust swap:** flip the ExecStart in the `palette.conf` drop-in (or a new drop-in). Do not edit the base unit.
- The unit is not enabled. labwc `autostart:57` runs `systemctl --user start tessel-shell.service`, and
  `shutdown` runs `systemctl --user stop tessel-shell.service` (install_switcher.py:125).
- Readiness probe used by installers: `tessel-shell switcher-status` / `tessel_shell.py switcher-status`,
  which must print JSON with `ready` and `can_activate` true (install_switcher.py:250, shared/install_gtk4.py:332).
- `install_switcher.verify_service_children` refuses a restart if the unit cgroup holds processes
  other than `systemd-inhibit`, `sleep`, `tessel-dock-topleve`, `tessel-shell-toplev` or glycin loaders. This means
  a Rust daemon **must launch apps outside its cgroup** (systemd-run, see §6). The helper `comm` names must
  stay ≤15-char-compatible (`tessel-shell-toplev`).
- `tessel-settings/about_page.py:16` lists `tessel-shell.service` as a shell service.
  `desktop_suite_failed.py:20` labels it "Desktop shell".

### labwc integration (`~/.config/labwc/`)
- `rc.xml` keybinds (written by install_switcher.py:99-103):
  - `A-Tab` → `/usr/bin/python3 ~/.local/share/tessel-shell/tessel_shell.py switch-window`
  - `A-S-Tab` → `… switch-window --reverse`
- `rc.xml <core>` (install_prompts.py:23-82):
  - `<promptCommand>exec "$HOME"/.local/bin/tessel-prompt --message-stdin <<'TESSEL_SHELL_PROMPT_<sha256>'\n%m\nTESSEL_SHELL_PROMPT_<sha256></promptCommand>`
  - `<errorCommand>sh -c 'exec "$HOME"/.local/bin/tessel-prompt --error'</errorCommand>`
  - The delimiter is `"TESSEL_SHELL_PROMPT_" + sha256(launcher + "\0" + "\0".join(all <prompt message>))`, with
    `_END` appended while the delimiter collides with any message line.
- `menu.xml`: `wayrunner --toggle` is replaced by the tessel-dock menu (install_switcher.py:108). That is not
  tessel-shell-owned.
- Hot corners: tessel-dock.py:428 spawns `/usr/bin/python3 ~/.local/share/tessel-shell/tessel_shell.py show-activities --monitor <connector>`.
- Wrapper `~/.config/waybar/scripts/showdesktop.sh` points to tessel-dock, not tessel-shell.

### .desktop files
- `~/.local/share/applications/org.tessel.Prompt.desktop` (install_prompts.py:113): `Name=Confirmation` plus
  30 `Name[xx]=` lines from the suite catalogs, `Icon=dialog-question-symbolic`, `Exec=true`, `NoDisplay=true`,
  `StartupNotify=false`. It names the app_id `org.tessel.Prompt` for the dock and switcher.
- The daemon and the capture picker have no .desktop file.

### GApplication ids (the D-Bus names they own)
| App | application_id | Flags |
|---|---|---|
| daemon | `com.nobaraDesktopView` | `HANDLES_COMMAND_LINE` (unique). Owns the session-bus name `com.nobaraDesktopView` and exports `org.gtk.Application` and `org.gtk.Actions` at `/com/nobaraDesktopView` (tessel-shell:3768). |
| prompt | `org.tessel.Prompt` | `NON_UNIQUE` (tessel_shell_prompt.py:247) |
| capture picker | `org.tessel.CapturePicker` | `NON_UNIQUE` (capture_picker.py:49) |

- `tessel-login/session_transition.py:39` waits for the bus names `org.tessel.Dock` **and
  `com.nobaraDesktopView`** before it fades out the login transition.
  **The Rust daemon must own `com.nobaraDesktopView` on the session bus** (zbus `request_name` is enough).
- `CLI_XML` (`com.nobaraDesktopView.CLI`: ListMonitors/Set/ApplyJSON, tessel-shell:3822) is **defined but never
  registered**. It is dead code and need not be ported. Optionally implement it harmlessly.

---

## 2. IPC

### 2.1 `$XDG_RUNTIME_DIR/tessel-shell.sock` (owned)
Server code: tessel-shell:4136-4167 and 4016-4134.
- AF_UNIX SOCK_STREAM. Path is `GLib.get_user_runtime_dir()/tessel-shell.sock`.
- At startup the path is unlinked unconditionally if it exists, then the socket listens and is chmod `0600`.
- Shutdown does **not** unlink it: `_cli_sock` is never set (tessel-shell:4795), so a stale socket stays behind.
  Clients then get ECONNREFUSED.
- Protocol: one UTF-8 JSON object plus `\n` per connection. The server reads one line, writes one JSON
  line, and closes. Requests are dispatched on the GLib main loop (`idle_add`).
- Errors:
  - empty request → `{"ok":false,"msg":"empty request"}`
  - unparsable JSON → `{"ok":false,"msg":"bad json: …"}`
  - unknown command → `{"ok":false,"msg":tr("Unknown cmd '{value1}'")}`
  - exception → `{"ok":false,"msg":str(e)}`

| `cmd` | Request fields | Reply |
|---|---|---|
| `switch-window` | `reverse`: bool. It must be a real bool, else `{"ok":false,"msg":tr("Invalid switching direction")}`. | `{"ok":bool,"msg":"" or tr("Window switching is not ready.")}` |
| `switcher-status` | – | `{"ok":true,"ready":b,"can_activate":b,"visible":b,"mode":"hidden"\|"keyboard"\|"activities","monitor":str\|null,"windows":int,"error":str}`, or `{"ok":true,"ready":false}` before the switcher exists (tessel-shell_switcher.py:321) |
| `show-activities` | `monitor`: str or absent/null | `{"ok":bool,"msg":"" or tr("Activities are not ready or the requested monitor is unavailable.")}` |
| `list-monitors` | – | `{"ok":true,"monitors":[connector…]}`. The key is `get_connector()` or `Monitor-<i+1>`. |
| `set` | `monitor` (connector or `"all"`); `file` (str, `""` or **null**: `req.get("file") or ""`); `mode` (default `Fill`); `color` (CSS, default `black`) | Unknown monitor → `tr('Unknown: {value1}')`. Bad colour (Gdk.RGBA.parse) → `tr("Bad color '{value1}'")`. Missing file when mode≠Color Only → `tr("Missing file '{value1}'")`. Success → `{"ok":true,"msg":"OK"}`. |
| `apply-json` | `json_path` | Read error → `tr('Read error: {value1}')`. Otherwise silently skips unknown monitors, bad colours and missing files, then returns `{"ok":true,"msg":"OK"}`. |

Clients:
- `tessel_shell.py`: 2 s timeout, reply ≤ 64 KiB, reply must be a dict.
  `monitor_name` validation: non-empty, ≤128 chars, printable, no leading/trailing whitespace (tessel_shell.py:11).
- `tessel-shell` itself in client mode (`_cli_client`, tessel-shell:4801): 3 s timeout. It acts as a client only if the
  socket file exists. Otherwise it falls back to starting or contacting the GApplication.
- `tessel-settings/desktop_pages.py:106` `wallpaper_request`: 8 s timeout. It sends `list-monitors`, then one
  `set` per changed monitor with `"file": null` for Color Only.
- `tessel-dock` (hot corners via subprocess).
- Installers (`switcher-status`).

### 2.2 `tessel-shell` CLI (argv). The same grammar applies in the client path and in GApplication `do_command_line`.
```
tessel-shell                                  # start/activate daemon
tessel-shell -h|--help                        # prints usage block (exact text tessel-shell:4806-4814), exit 0
tessel-shell list-monitors                    # one connector per line
tessel-shell set --monitor|-m KEY|all [--file|-f PATH] [--mode Fill|Fit|Stretch|Center|Tile|"Color Only"] [--color|-c CSS(black)]
tessel-shell set-color --monitor|-m KEY|all --color|-c CSS [--mode (default "Color Only")]   # → {"cmd":"set","file":""}
tessel-shell apply-json PATH
tessel-shell switch-window [--reverse]
tessel-shell show-activities [--monitor NAME]
tessel-shell switcher-status                  # prints reply JSON; exit 0 iff ok
tessel-shell --gapplication-service           # never treated as CLI
```
- Client mode prints `msg` to stdout on ok (exit 0), or to stderr (exit 1).
- Bad args print `Invalid arguments. Use --help.` to stderr with argparse's exit code (2).
- Connection errors print `CLI error: …` (exit 1).
- In-daemon `do_command_line` messages differ slightly (`Unknown monitor(s): …`, `File not found: …`,
  `OK\n`, list-monitors prints `k + "\n"`, which doubles the newline). These are only reached without a socket.

`tessel_shell.py` takes the subcommands `switch-window [--reverse]`, `show-activities [--monitor M]` and
`switcher-status`. `switcher-status` prints the JSON reply. A not-ok reply prints its `msg` to stderr and
exits 1. Errors print `Tessel Shell: <err>` to stderr and exit 1.

### 2.3 D-Bus consumed
| Peer | Usage |
|---|---|
| `org.tessel.Dock` `/org/tessel/Settings` `org.tessel.Settings` | `GetWorkareas() → s` (2000 ms, NO_AUTO_START). Signal `WorkareasChanged(s)`. Proxy flags DO_NOT_AUTO_START and DO_NOT_LOAD_PROPERTIES. The client tracks `g-name-owner`; on owner loss all areas are cleared. It accepts only signals whose sender is the current owner. A live signal bumps the serial so it beats an in-flight reply (desktop_workarea.py). Payload JSON: `{"version":1,"monitors":[{"name":str,"geometry":[x,y,w,h],"workarea":[x,y,w,h]}]}`. It is rejected if: >64 KiB, version≠1, >128 monitors, duplicate or empty names, non-int values or abs>2^30, w/h≤0, or a workarea outside the geometry. |
| `org.freedesktop.Notifications` | `Notify(susssasa{sv}i)`: app_name=`tr("Desktop")`, id 0, icon `dialog-error-symbolic`, hints `{"urgency": byte 1}`, timeout 8000. Async; failures are only logged (desktop_file_ops.py:365). |
| `org.tessel.CaptureSelection` at the *requesting app's unique name* (`:N.M` derived from the session path) | capture picker: `Select(o session, as tokens) → (b handled, s token)` (1200 ms) and `Selected(o session, as outputs)` (1000 ms), both NO_AUTO_START. The session must match `^/org/freedesktop/portal/desktop/session/(\d+(?:_\d+)+)/capture_[a-f0-9]+$`, and the owner is `':' + group1.replace('_','.')` (shared/capture_selection.py:58-86). The server side (`SelectionBroker`) is owned by tessel-capture or tessel-dock and is not part of tessel-shell. |
| PipeWire / WirePlumber (`Wp` 0.5) | `shared/application_audio.py`: per-app mute buttons on switcher and overview cards. |
| xdg-desktop-portal Settings | **Deliberately not used** by the capture picker (`GDK_DEBUG+=no-portals`, `GTK_USE_PORTAL=0`). |

### 2.4 Wayland protocols
| Protocol | Where | Notes |
|---|---|---|
| `zwlr_layer_shell_v1` (via gtk4-layer-shell) | desktop windows, switcher, backdrops | see §4 |
| `zwlr_foreign_toplevel_management_unstable_v1` (v≤3) + `wl_seat` (≤7) + `wl_output` (≤4) + `zxdg_output_manager_v1` (≤3) | `tessel-shell-toplevel` helper (tessel-dock/toplevel_helper.c) | Activate, minimize, unminimize and close; the window list with outputs. |
| `ext_foreign_toplevel_list_v1`, `ext_image_copy_capture_manager_v1`, `ext_foreign_toplevel_image_capture_source_manager_v1`, `ext_output_image_capture_source_manager_v1`, `wl_shm` | `tessel-overview` (overview_capture.c; XMLs in `protocols/`) | Activities previews |
| `ext_background_effect_v1` (compositor blur) | `shared/libwaydock-background-blur.so` via background_blur.py, attached by TesselShellTheme | Shared with tessel-gtk. Needs the GTK surface's `wl_surface`. |
| `wlr-data-control` | only indirectly, via the spawned `wl-copy` | |

#### Helper stdio protocols (keep them, or reimplement in-process)
- **`tessel-shell-toplevel`** (stdout JSON lines; stdin `"<action> <id>\n"`, where action ∈ `activate|minimize|unminimize|close`):
  - `{"event":"ready","seat":bool,"protocol_version":u}`
  - `{"event":"window","id":"<u64>","app_id","title","activated","fullscreen","minimized","maximized","parent":"<id|''>","outputs":[names]}`
  - `{"event":"closed","id"}`
  - `{"event":"output","id","name","description","x","y","width","height","scale"}`
  - `{"event":"output-removed","id"}`
  - `{"event":"error","message"}`
  - `--snapshot` prints the current state and exits. capture_icons and capture_output use it with a 0.8–1 s timeout.
  - Python side (tessel-dock/window_backend.py): ids become `"<session>:<native>"`, and the native id must match
    `[1-9][0-9]{0,19}`. There is an 8 s startup timeout, restart with backoff, and a write queue capped at 128.
- **`tessel-overview [connector]`** (stdout JSON lines, then exit). Each frame waits at most 1200 ms; the
  window total is 10 s; Python kills the helper after 12 s; lines are capped at 12 MiB:
  1. If a connector is given and output capture exists: `{"kind":"desktop","app_id":"","title":"","identifier":"","image":"<b64 PNG>"}`.
     The image is scaled to ≤1600×1000.
  2. `{"kind":"identities","windows":[{"app_id","title"}…]}`. This is emitted **before** window frames.
  3. Per window: `{"kind":"window","app_id","title","identifier","image"}`, scaled to ≤640×400.
  - Prefers ARGB8888, else XRGB8888. Rejects frames >16384 px or >32 Mpx. Skips non-NORMAL transforms.
    Exit code 2 when a required global is missing.

---

## 3. Files

| Path | Format | Writers | Readers |
|---|---|---|---|
| `~/.config/tessel-shell/wallpapers.json` | JSON, indent=2. `{"<connector>": {"file_path": str\|null, "mode": "Fill\|Fit\|Stretch\|Center\|Tile\|Color Only", "color": "rgb(r,g,b)"\|"rgba(...)"}, "_view_prefs": {"hide_icons": bool, "show_hidden": bool}}`. The colour is `Gdk.RGBA.to_string()`. `file_path` is null for Color Only. The writer merges with the existing file (read, update keys, rewrite, non-atomic). | daemon (tessel-shell:4328, 1430, 461) | `tessel-settings/desktop_pages.py:295` (read only; writes go via the socket), `tessel-dock/wallpaper_palette.py:339` (palette from the wallpaper, file-monitored), `tessel-lock/lock_config.py:119` (`file_path` for images). `apply-json` treats `_view_prefs` as an unknown monitor and skips it. |
| `~/.config/tessel-shell/desktop_view_order.json` (`GLib.get_user_config_dir()`) | JSON, compact. `{"<filename in ~/Desktop>": [global_x, global_y]}`. Coordinates are global compositor pixels. Example: `{"computer.desktop":[12,22],"user-home.desktop":[12,150]}`. | daemon only. `save_all_positions` skips the write when the content is unchanged, to avoid waking file monitors (tessel-shell:2732). | daemon |
| `~/Desktop/` | directory, watched with `Gio.FileMonitor` | user/daemon | |
| `$XDG_CACHE_HOME/tessel-dock/palette.json` | Written by tessel-dock. Validated (tessel_shell_theme.py:13): ≤128 KiB, `version==1`, `is_dark` bool, `apps_transparency` int 0–100 (default 5), `apps_transparency_enabled` (default False), `apps_frost_strength` 0–100, `apps_frosted`, `apps_rounded_corners` (default True), `monitors[1..64]` each with `name`, `geometry[4 ints, w,h>0]` and `background/foreground/accent` [3 × 0–255]. Malformed input means plain GTK theme. Watched with a file monitor (WATCH_MOVES, rate limit 100 ms). | tessel-dock | tessel-shell daemon, prompt, capture picker |
| `~/.config/labwc/environment` and `environment.d/*.env` (sorted) | labwc env syntax | tessel-session-startup | `labwc_launch_environment()` (tessel-shell:110-185): `$VAR` and `${VAR}` expansion, **every `~` replaced with HOME**, values >1024 bytes skipped, names with NUL skipped. A fresh environment is built for every launch. |
| `~/.config/xdg-desktop-portal-wlr/{wlroots,labwc}` | INI. `[screencast] chooser_type=dmenu`, `chooser_cmd=exec '<launcher>'` | install_capture_picker | xdpw. Inherits from the first of local/desktop, local/config, /etc/xdg/…/desktop, /etc/xdg/…/config. Only these two keys are replaced; a trailing `[screencast]` section is appended if needed (capture_portal_config.py). |
| `$XDG_CACHE_HOME/tessel/tessel-overview` | build artefact | build_overview | installers |
| `~/.local/state/tessel-shell/backups/switcher-*`, `~/.local/state/tessel/backups/overview-*` | installer backups + `manifest.json` | installers | |
| `trash:///` | GIO `trash::item-count` | | TrashWatcher. 150 ms debounce, async, shared by all monitors. |

---

## 4. UI surfaces

### Desktop layer (one per monitor; tessel-shell:4470 `_make_grid_window`)
- `Gtk.Window` with class `tessel-shell-desktop`, name `desktop-grid-win-<connector|index>`.
- Layer-shell settings:
  - namespace `desktop-grid-<connector|index>`
  - **layer BACKGROUND**, anchored on all four edges, `set_monitor(mon)`
  - **exclusive_zone −1**, so the surface covers panel strips
  - keyboard **ON_DEMAND**
- One surface combines wallpaper and icons. The legacy separate `desktop-bg-<conn>` window (tessel-shell:4442)
  is unused; separate windows raced each other.
- Widget tree: `Gtk.Overlay #main-overlay` → it is renamed `#overlay-bg-<conn>` when a wallpaper is set.
  - child: `Gtk.Picture` (background, can_target False, COVER)
  - overlay: `Gtk.Fixed.desktop-grid` (icons)
  - overlay: ghost `Gtk.Fixed` (drag ghosts `.ghost-icon`, can_target False)
- The solid colour is per-display CSS `#overlay-bg-<conn> { background-color: <rgba>; }`.
- Fit map: Fill→COVER, Fit→CONTAIN (centered), Stretch→FILL, Center→SCALE_DOWN (centered), Tile→FILL,
  Color Only→no paintable.
- Stills: `Gdk.Texture.new_from_filename`, cached app-wide by (realpath, ino, size, mtime_ns) in a weak
  dictionary. The explicit decode avoids a glycin WebP 200×200 fallback bug (tessel-shell:1393).
- Video (`.mp4 .m4v .mov .webm .gif`): spawns `mpvpaper -l background -o "no-audio osc=no cursor-autohide=no
  keep-open=yes loop-file=inf loop-playlist=inf [keepaspect=yes for Fit/Fill]" <connector> <path>` with the
  labwc environment.
  - Afterwards the grids bounce BACKGROUND→BOTTOM→BACKGROUND at idle, +120 ms and +300 ms (tessel-shell:4513)
    so they stay above mpvpaper.
  - mpvpaper is stopped with terminate, wait 2 s, then kill.
  - If mpvpaper is missing, a notification is sent.
- Icon cell is 128×128.
  - Item: `Box`[`Box`[`Image` 64 px, `Box.label-highlight`[`Label.desktop-label`]]].
  - Label: 2 lines, 14 chars max, END ellipsis, WORD_CHAR wrap, centered.
  - Selected: `.label-highlight.selected`. Rubber band: `.selection-box`. Progress toast: `.transfer-toast`.
- CSS provider at USER priority (tessel-shell:365-411). Scrollbars are hidden globally on the display; the
  wallpaper settings window re-enables them.
- Wallpaper Settings window: `Gtk.Window` titled `tr("Wallpaper Settings")`, 620×610, class
  `tessel-shell-wallpaper-settings`. Classes used: `.wallpaper-title/-subtitle/-tile/-card/-section-title/-detail/-display-icon/-body`.
  - Per monitor: mode dropdown, colour button, and an image/video chooser button (ellipsize MIDDLE, 28 chars).
  - Apply button `.suggested-action`.
  - Chooser = `shared/wallpaper_chooser.WallpaperChooser`. Tiles are 224×126. The start folder is the first of
    `/usr/share/applications/wallpapers`, `/usr/share/wallpapers`, Pictures, home. Videos are validated with
    `ffprobe`. System-folder imports use one `pkexec --disable-internal-agent cp --update=none-fail …` per batch.
    The chooser is shared with Global Settings.

#### Desktop menus
Background right-click (tessel-shell:889) is a `PopoverMenu` with no arrow, in four sections:
1. Open Terminal / Open File Manager / Open Web Browser
2. Hide Icons (toggle) / Align to Grid (toggle) / Show Hidden (toggle) / Sort by ▸ By Name, By Type
3. Cut / Copy / Paste / Move to Trash. Cut, Copy and Delete are disabled if the selection contains a protected item.
4. System Monitor (`org.tessel.SystemMonitor.desktop`) / Settings (`org.tessel.GlobalSettings.desktop`) /
   Change Wallpaper / Configure Displays. Configure Displays appears only if `tessel-displays` is on PATH.

Item menu (tessel-shell:3063):
- Trash: Open / Empty Trash
- Everything else: Open / Rename / Cut / Copy / Move to Trash.
- **Protected** items (never cut/copied/moved/deleted): `computer.desktop`, `network.desktop`,
  `trash-can.desktop`, `user-home.desktop` (case-insensitive basename, tessel-shell:3048).
- An item is "trash" if its name starts with `trash` or its display name contains `trash`. Its icon is
  `user-trash` or `user-trash-full`, and its label is `tr("Trash")` or `tr('Trash ({value1})')`.

#### Keyboard on the desktop
- Esc clears the selection.
- Delete/KP_Delete moves the selection to Trash (with confirmation).
- Ctrl+C, Ctrl+X, Ctrl+V work as clipboard shortcuts.
- Ctrl+H toggles Show Hidden.
- Arrow keys navigate by grid cell; Shift extends the selection.
- Double-click window is 350 ms within 8 px (tessel-shell:219).

#### Clipboard
Copy and cut publish both `x-special/gnome-copied-files` (`copy|cut\n` + URIs) and `text/uri-list`. They also
spawn `wl-copy -t x-special/gnome-copied-files <payload>` and `wl-copy -p -t text/uri-list <uris>`
asynchronously (tessel-shell:1757). Paste prefers the gnome format.

### Switcher (`tessel-shell_switcher_ui.py`)
- Window class `tessel-shell-switcher` (plus `activities` in overview mode), title `tr("Open windows")`.
- Layer-shell: namespace `tessel-shell-switcher`, layer **OVERLAY**, exclusive_zone −1, keyboard **EXCLUSIVE**.
  - Keyboard mode: centered, no anchors.
  - Activities mode: anchored on all four edges (tessel-shell_switcher_ui.py:215).
- Backdrops: one per monitor, class `tessel-shell-switcher-backdrop`, namespace `tessel-shell-switcher-backdrop`,
  OVERLAY, all anchors, keyboard NONE. They catch outside clicks, which cancel.
- CSS classes: `.switcher-surface .switcher-heading .switcher-count .switcher-hint .switcher-grid`,
  `button.switcher-card(.selected,.minimized)`, `.switcher-tile`, `button.switcher-card-close` (also used for
  mute), `.switcher-name .switcher-title .window-preview`. Card actions use opacity 0 unless hovered or selected.
- Keyboard layout `layout_limits` (tessel-shell_switcher_ui.py:20):
  - columns = min(count, 4, (W−48−36)//176)
  - width = min(800, W−48, max(360, cols·176+36))
  - content_height = min(444, max(32, H−48−118)); the grid scrolls inside it
- Activities layout `overview_layout` (overview.py:20): preview height 90–320, ≤4 columns, ≥300 px per column.
- Activities show sequence:
  1. Take the desktop's existing wallpaper paintable.
  2. Start `tessel-overview <connector>`.
  3. Present on the desktop frame or after a 220 ms deadline.
  4. Animate over 280 ms: ease-out cubic, desktop shrinks 16 %, content scales from 1.10. It respects
     `gtk-enable-animations`.
- Previews are assigned only when the (app_id, title) pair is unique in both the identities frame and the item
  list (`unique_preview_target`).
- Keys:
  - Esc cancels.
  - Enter chooses the selection, or cancels if nothing is selected.
  - Tab, ISO_Left_Tab and arrows move the selection. Up/Down move by one row (`columns` steps). Shift or
    Left/Up reverse.
  - Alt+Tab while in activities switches to keyboard mode.
- Card tooltip: `"{name}\n{title}"` plus `"\nMinimized"`. Fallbacks: name → `app_id` or `"Application"`; title →
  `"Untitled window"`; icon → `application-x-executable`.

### Theme (`tessel_shell_theme.py`)
`TesselShellTheme` watches every GTK toplevel. Per window it:
- adds class `tessel-shell-palette-<serial>` and a provider at USER+2 priority
- picks a monitor palette by connector, then by geometry, else uses the first entry
- applies a variant based on marker classes:
  - `tessel-shell-desktop`: popovers, entries, captions and selection only; **never the wallpaper or root background**
  - `tessel-shell-switcher`: the switcher variant
  - `tessel-shell-switcher-backdrop`: transparent
  - default: the full window variant
- on_accent text is `#101114` when sRGB luminance >0.179, else `#ffffff`.
- With rounded corners off, every border-radius becomes 0, except switch, scale and scrollbar parts, which
  keep 9999px.
- Blur is attached via background_blur: off for the backdrop, `target_class='switcher-surface'` for the switcher,
  `include_root` false for the desktop and the backdrop.
- The exact CSS text is in tessel_shell_theme.py:90-226. Port it verbatim into tessel-gtk.

### Prompt (`tessel_shell_prompt.py`)
- `Gtk.ApplicationWindow` with class `tessel-prompt`.
  - Title: `tr("Configuration Error")` with `--error`, otherwise `tr("Confirm Action")`.
  - Size 540 (error, resizable) or 440 (not resizable). Headerbar decoration `:close`.
- Content:
  - icon `dialog-error-symbolic` or `dialog-question-symbolic`, 32 px, `.prompt-icon`
  - first line of the message → `.prompt-heading`; the rest → `.prompt-body`
  - buttons in `.prompt-actions`
- Error mode: a monospace TextView (200–420 px) holds stdin. The only button is `tr("Close")`, which returns 0.
- Exit codes: Yes=0, No=1, dismiss=254. Escape and window close dismiss (254), or return 0 in error mode.
  SIGTERM acts as close.
- CLI:
  - exactly one of `--message TEXT`, `--message-stdin` or `--error`
  - `--accept-label` (default `tr("Yes")`), `--decline-label` (default `tr("No")`)
  - `--destructive`: accept gets `.destructive-action` and Decline becomes the default; otherwise accept has
    `.suggested-action` and is the default
- Stdin handling:
  - read non-blocking, 8×8 KiB per iteration, keeping ≤65 536 bytes
  - on truncation, append `"\n\n"+tr("Additional text was omitted.")`
  - `--message-stdin` strips one trailing `\n`
  - **Yes/No stay insensitive until EOF and when truncated**

### Capture picker (`capture_picker.py`, `capture_picker_ui.py`)
- `Gtk.ApplicationWindow` titled `tr("Share your screen")`, class `capture-picker`, headerbar
  `tr("Screen sharing")`. It is a normal toplevel, not layer-shell.
- Content: a Screens/Windows stack (`add_titled` ids `monitor` and `window`), a search entry, rows (`.capture-title`,
  `.muted`, `.device-icon`, `.capture-selection` check), a `.capture-remote` notice, `.capture-empty`, and
  Cancel / Share (`.primary`, insensitive until a source is selected).
- Protocol (xdpw dmenu):
  - stdin: newline-separated records `Monitor: <name> <description>` and `Window: <title> (<identifier>)`
  - up to 4 MiB; duplicates dropped; records with NUL or >4095 bytes dropped
  - stdout: exactly the chosen record + `\n`, or nothing (cancel); everything else goes to stderr
  - exit 0, or 1 on error (error = no output)
- `len(argv)!=1` is an error.
- `XDPW_CHOOSER_REMOTE_DESKTOP & 3` (bit 1 keyboard, bit 2 pointer; non-int → 3) shows the notice and **always
  shows the UI**.
- Otherwise `XDPW_CHOOSER_SESSION` is first offered to `CaptureSelection.Select` (preselection by the requesting
  app).
- After selection: `Selected(session, outputs)`.
  - Outputs for a monitor: its name.
  - Outputs for a window: tessel-tiling `capture-output <identifier>` (tiling_client socket), matched to a GDK
    monitor by geometry. Fallback: `tessel-dock-toplevel --snapshot` title match, used only if unambiguous.
- Exits when the parent pid changes (1 s poll) or on SIGINT, SIGTERM or SIGHUP.

---

## 5. Switcher semantics (`switcher_model.py`, `tessel-shell_switcher.py`)
- MRU: new ids are appended. Only a *transition* to activated moves an id to the front. Unchanged activation
  flags never reorder. If several windows are activated, they are processed in deterministic id order.
- `begin`:
  - The current (activated) window goes first.
  - Forward selects index 1 (or 0 if there is no current window).
  - Reverse selects the last.
  - `select_current` (activities) selects 0.
- While active the order is frozen.
  - New windows are appended.
  - If the selected window closes, the selection moves to the closest surviving successor, or to the previous
    one at the end.
  - If everything disappears, the switch is cancelled.
- Alt release is detected by modifier-state notify, key-release of Alt_L/Alt_R (deferred to idle), focus-in
  (deferred to idle), and a 50 ms poll fallback. If Alt is no longer down → `choose`.
- `_gesture_closed`: after Esc in a held-Alt gesture, further `switch-window` calls return ok without reopening.
  A fresh Alt press clears it.
- `choose`:
  1. `model.finish`
  2. hide: dismiss, then `display.sync()` **before** activation
  3. `backend.activate(id)`
- Close button: sends `close` and keeps the card until the compositor reports the window closed. A refusal
  (save prompt) keeps the overview open.
- Mute toggles PipeWire app streams and never activates the window.
- `show_activities(monitor)`:
  - An invalid or unknown connector returns False.
  - With no monitor: the current view's monitor if visible, else `_active_monitor`. `_active_monitor` is the
    first output of the activated window, else the monitor of an active GTK toplevel.
  - The layer surface cannot change output after its first configure, so it is dismissed and re-shown on a
    monitor change.
- `step` and `show_activities` return False unless the backend is `ready` and `can_activate`.

## 6. External commands spawned
| Command | When |
|---|---|
| `systemd-run --user --collect --quiet --service-type=forking --property=GuessMainPID=no --property=ExitType=cgroup --property=TimeoutStartSec=90s --slice=app.slice --expand-environment=no --unit=app-tessel-dock-<uuid>.service [--working-directory=] --setenv=NAME… -- <helper>` | All launches: .desktop files, URIs, terminal, file manager, tessel-displays, suite apps (tessel-dock/app_catalog.py:34). Launched apps survive a tessel-shell restart. |
| Terminal candidates, in order | `$TERMINAL`, `xdg-terminal --working-directory`, `ptyxis --new-window --working-directory`, `kgx`/`gnome-terminal`/`xfce4-terminal`/`tilix`/`alacritty --working-directory`, `konsole --workdir`, `kitty --directory`, `wezterm start --cwd`, `xterm`, `ghostty +new-window`. cwd is HOME (tessel-shell:1073). |
| `xdg-settings get default-web-browser` | async, then launch the reported desktop id |
| `~/.local/bin/tessel-displays` or `tessel-displays` on PATH | Configure Displays |
| `mpvpaper …` | video wallpaper |
| `wl-copy` ×2 | copy/cut |
| `tessel-shell-toplevel`, `tessel-overview <conn>` | switcher / activities |
| `ffprobe`, `pkexec cp` | wallpaper chooser and import (shared) |
| `tessel-dock-toplevel --snapshot`, tiling socket `capture-output` | capture picker |

## 7. File operations (`desktop_file_ops.py`)
- `unique_name`: `base (n).ext`. Dotfiles keep the whole name as base (`.profile (1)`).
- `parse_clipboard_files`:
  - NULs are treated as newlines.
  - The first line `copy` or `cut` gives the mode (cut→move).
  - The gnome format requires the verb, otherwise the payload is invalid.
  - Lines starting with `#` and lines without `:` are dropped.
- Planning happens off the GTK thread. Same-path paste: copy → KEEP_BOTH, move → SKIP. `/` in names is replaced
  by `_`.
- Conflicts: each asks **Skip / Keep Both / Replace** with "apply to all". A dismissed prompt means SKIP.
  Unresolved items become REPLACE if conflicting, otherwise KEEP_BOTH. There is no nested main loop.
- Copy and move run on a worker thread with Gio and a cancellable.
  - Cancelling removes a partial copy.
  - Copying a folder into itself is reported as an error.
  - Progress is marshalled to the main loop.
  - A toast appears if the transfer outlasts a delay; failures show a dialog.
- Trash and rename are async (`trash_async`, `set_display_name_async`).
  - Renaming a `.desktop` edits `Name=` and keeps the filename.
  - Other files are renamed via display name. Names with `/`, `.` or `..` are rejected with
    `tr("A name can't contain \"/\".")`.
  - The old position is reserved under the new name before the rename.
- Desktop refresh: folder-monitor events are coalesced for 100 ms. The refresh is deferred while a drag or
  rubber band is active. The primary grid (leftmost, then topmost monitor) is visited first because it
  assigns slots to new files.

## 8. Icon layout (`desktop_icon_layout.py`)
- Each monitor's usable region is its tessel-dock workarea (fallback: full geometry) inset by `EDGE_MARGIN=12`.
- `next_slot` scans monitors sorted by (x, y), column-major with 128-px cells. A cell overlaps another if
  |dx|<128 and |dy|<128.
  - When the desktop is full, it uses the clamped bottom-right of the last region.
- `constrain_positions`:
  - Non-finite or malformed points are dropped.
  - Points already inside a usable area stay.
  - Others are clamped into their own monitor (or the nearest one), and moved to `next_slot` if the clamped
    cell overlaps.
- `shift_positions`: when a monitor's workarea origin moves with unchanged geometry (a bar changed edge), every
  icon on that monitor shifts by the delta. Only *known* areas are compared, so a dock restart does not shift
  icons (tessel-shell:4411).
- Sort by Name uses the display name, lowercased. Sort by Type groups as:
  folders, Application, Link, Directory, Service, unknown desktop, text (.txt .md .rtf), image (.png .jpg .jpeg .webp),
  .pdf, video (.mp4 .mkv .avi), other.
  Positions are rewritten with `next_slot` fill.
- Drag:
  - The target grid is chosen from the global point **before** snapping, so icons can be dragged across screens.
  - Snapping is `snap_to_grid` when Align to Grid is on.
  - A drag watchdog and ghosts exist for cross-surface feedback.
- Monitor topology: `items-changed` and per-monitor `notify::geometry` are debounced 250 ms. Desktops are
  rebuilt only if the (x, y, w, h) snapshot changed. The switcher is never destroyed by a rebuild.
- Hide Icons only changes visibility and clears the selection. Turning it off triggers `load_icons()`, which
  picks up files added while hidden. Show Hidden reloads with dotfiles. Both states are mirrored across all
  monitors and persisted in `_view_prefs`.

---

## 9. Parity test cases (from tests; port as Rust tests)
**CLI and socket**
- shell_integration: a daemon started with a temp HOME answers `tessel_shell.py switcher-status` with ready
  true; socket commands are forwarded.
- activities:20: `--monitor` is optional and validated (empty, whitespace or non-printable → argparse error).
  The forwarded payload omits `monitor` when absent.
- `switch-window` with non-bool `reverse` → not ok. `set` with `file: null` works (Global Settings).
- Stale socket left after shutdown; startup unlinks it (no crash).

**Switcher model** (test_switcher_model)
- Forward cycle wraps. Reverse starts at the end.
- Activities selects current, then advances normally.
- A single window or an empty model returns None and never crashes.
- With no focused window, `begin` starts at the MRU head.
- Focus and title churn during a switch never reorder it. finish and cancel never invent activation.
- When the selected window closes, the selection goes to the closest successor. Removal before the selection
  keeps the selected id. A new window is appended.
- Minimized windows and empty title or app_id remain selectable.
- A backend clear cancels. Session-prefixed ids from replaced helper sessions never return a stale selection.
- Mutating backend objects in place still registers activation transitions.

**Switcher native/UI**
- Hold Alt, repeat Tab, reverse, release activates. A fast tap (Alt+Tab released immediately) switches to the
  previous window.
- A fullscreen window keeps its geometry across Alt+Tab with an adaptive top bar (exclusive_zone −1, no
  reservation).
- Esc cancels, and further Tabs are ignored until Alt is pressed again.
- A minimized window is restored when chosen. A closed selection is safe.
- Activities stays open without Alt. Alt+Tab then follows normal release semantics.
- The close button targets the window without activating it and respects refusal.
- The grid and its height stay bounded; many windows scroll; surfaces follow the selected monitor.
- Title updates keep card widgets and callbacks (no hover flicker). The order is reparented only when the order
  changes.
- Manual scroll survives updates; keyboard selection is scrolled into view.
- Palette updates live without replacing cards. Fallback and palette CSS parse in light and dark.
- The overlay and backdrop stay transparent; only `.switcher-surface` is painted.
- A pointer click outside on either monitor dismisses without choosing. A card click selects instead of
  dismissing.
- Mute click neither activates nor closes.

**Overview**
- Ambiguous (app_id, title) duplicates get no preview.
- Layout is readable from small to large monitors.
- The real capture helper is cleaned up (killed) on dismiss.

**Theme** (test_tessel_shell_theme)
- Malformed cache → no palette.
- Connector selects the monitor first, then geometry, then the first entry.
- The desktop scope themes only controls, popovers and captions; **never the wallpaper**.
- App transparency is independent of the dock and does not fade the wallpaper.
- Frosted and square materials never paint wallpaper or backdrops. Foreground stays opaque while frosted.
- Accent drives on_accent readability. The square setting flattens corners and restoring it brings back local
  styles.
- Destroyed windows are removed. Per-monitor captions follow their monitor.

**Desktop**
- An unchanged refresh reuses widgets, keeps the selection, and does not rewrite the positions file.
- Launches (terminal with HOME cwd, files, shortcuts) go through independent systemd-run services.
- Removed or changed icons (by inode, mtime, size, mode signature) are released; others survive.
- Event bursts coalesce, and the refresh waits for the drag to end.
- Paste and drop share the async conflict prompt and worker copy. Multiple conflicts offer apply-to-all.
  Failures show a dialog.
- The progress toast cancels and is removed.
- Failures notify, and the browser lookup is async.
- Trash and monitor changes are signal-driven (no polling).
- The wallpaper texture is shared across views and released on destroy.
- Hide/show restores icons after a hidden startup and file changes (desktop_visibility).
- Existing, new, sorted and dragged icons avoid per-monitor panels (desktop_workarea). A malformed workarea
  payload is ignored.

**Icon layout** (test_desktop_icon_layout)
- 12 px edge margin.
- A bar moving edges carries the whole arrangement.
- Obscured cells reflow without touching other monitors. A larger floating gap reflows only newly obscured cells.
- New cells fill real workareas, not gaps between monitors.
- Reflow avoids existing cells and prefers the original monitor.
- Offscreen or invalid saved positions are tolerated.

**File ops**
- unique_name handles extensions and dotfiles.
- Clipboard payloads (gnome without verb → invalid).
- Each conflict is asked in turn, with apply-to-all and no nested loop. A dismissed prompt skips; preset actions
  are not asked.
- Recursive copy works with keep-both and replace. Move with skip and same item.
- Folder into itself and a missing source report errors.
- Cancel removes the partial copy.
- Progress is reported on the main loop.
- Trash count is async and shared.
- Notifications are async.

**Prompt** (test_tessel_shell_prompt)
- The installer changes only the two commands and is idempotent. A home launcher is written as `"$HOME"/…`.
  `<core/>` is created or expanded.
- Punctuation (`'`, `$`, backticks, multi-line) stays literal through `sh`.
- A failed apply restores existing files and removes new ones. The plan is read-only and installs no services.
- Exit codes 0/1/254. The destructive accept is styled but not the default. The message first line is the
  heading. The icon is symbolic.
- Escape and close never approve.
- Stdin message: buttons stay disabled until EOF; truncated input stays disabled.
- Error stdin streams with bounded text.
- The live palette is followed.

**Capture**
- Labels never alter the exact record. Duplicate titles remain distinct.
- Output comes from the *selected* window, not the focused one.
- Whitespace and missing labels do not change identity.
- Cancel or an unknown source → no output. Invalid or oversized records are not selectable.
- `GDK_DEBUG` keeps existing flags and adds `no-portals` unless `all` is present. `GTK_USE_PORTAL=0`.
- Logs never reach stdout. Cancel or a GUI error never grants access.
- Remote control always shows the picker and discloses it (keyboard / mouse / both strings).
- Tab switch and search clear a hidden selection. Row activation is deliberate. Keyboard navigation selects
  without sharing.
- An empty list cannot share. A long list scrolls inside 720p. A portal-side destroy is not consent.
- Real xdpw: monitor and window requests return the exact record. Empty output cancels. The picker maps while
  the Settings portal is busy.
- Portal config: only the chooser keys are updated, idempotently. Missing keys are added. The effective fallback
  is inherited and KDE is untouched.
- Installer: refuses while capture is active; rolls back on a failed restart without enabling services.

---

## 10. Module map
| File | LOC | Responsibility |
|---|---|---|
| `tessel-shell` | 4910 | Daemon. DesktopView: icons, selection, rubber band, drag, rename, clipboard, trash, menus, wallpaper settings window. DesktopApp: per-monitor layer windows, mpvpaper, socket server, GApplication CLI, monitor hotplug, workarea following, sort. `_cli_client`. |
| `tessel-shell_switcher.py` | 334 | Switcher controller: Alt tracking, modes, backend wiring, status. |
| `tessel-shell_switcher_ui.py` | 590 | Switcher/overview layer surfaces, backdrops, cards, layout, capture-driven presentation. |
| `switcher_model.py` | 98 | Pure MRU and selection model. |
| `overview.py` | 178 | Overview helper process client, preview matching, layout, animated Stage. |
| `overview_capture.c` | 184 | ext-image-copy-capture snapshot helper (stdout JSON + base64 PNG). |
| `build_overview.py` | 41 | wayland-scanner + cc build. |
| `tessel_shell_theme.py` | 419 | Palette cache reader, per-toplevel CSS, blur attach. |
| `tessel_shell.py` | 70 | Socket client and monitor-name validator. |
| `desktop_workarea.py` | 141 | Tessel Dock workarea D-Bus client and validator. |
| `desktop_icon_layout.py` | 92 | Pure grid placement maths. |
| `desktop_file_ops.py` | 498 | Transfer planning, conflicts, worker copy, notify, browser lookup, TrashWatcher. |
| `tessel_shell_prompt.py` | 264 | labwc prompt and error dialog. |
| `capture_picker.py` | 125 | xdpw dmenu entry, env hardening, preselect/report. |
| `capture_picker_ui.py` | 338 | Picker window. |
| `capture_sources.py` | 66 | Record parse and write. |
| `capture_output.py` | 53 | Selected source → output names. |
| `capture_icons.py` | 31 | Window record → app icon. |
| `capture_portal_config.py` | 50 | xdpw INI rewrite. |
| `install_switcher.py` | 294 | rc.xml, menu, autostart and shutdown migration; Wayrunner retirement. |
| `install_overview.py` | 88 | Overview runtime install. |
| `install_prompts.py` | 195 | rc.xml core commands, launcher, Prompt .desktop. |
| `install_capture_picker.py` | 112 | Picker runtime, xdpw config, portal restart. |

External modules loaded at runtime from `~/.local/share/tessel-shell/`:
- from tessel-dock: `app_catalog`, `window_backend`, `companion_theme`
- from shared: `application_audio`, `wallpaper_chooser`, `wallpaper_import`, `gtk4_utils`, `suite_i18n` (+ catalogs), `background_blur` + `.so`, `capture_selection`
- from tessel-files: `media_chooser`, `files_pane`, `file_model`, …
- from tessel-tiling: `tiling_client`

## 11. Suggested porting split
1. **ws-core (no GUI):** port `switcher_model`, `desktop_icon_layout`, the `desktop_workarea` parser,
   `capture_sources`, `capture_portal_config`, `parse_clipboard_files`/`unique_name`/ConflictResolver,
   `labwc_launch_environment` and the palette reader + `palette_css` (into tessel-gtk). Each unit test above
   maps 1:1.
2. **ws-cli:** add `tessel-shell.sock` server and client, plus the argv grammar. Own `com.nobaraDesktopView` on the
   bus. Ship a `tessel_shell_cli` drop-in (keep the Python path in rc.xml, or have the installer repoint it).
3. **ws-toplevel:** reuse the tessel-dock Rust foreign-toplevel module, in-process (wayland-client
   `zwlr_foreign_toplevel_manager_v1`) or as the same helper binary. Keep the `switcher-status` fields.
4. **ws-switcher UI:** layer-shell OVERLAY surface, backdrops, cards, keyboard/Alt policy, PipeWire mute
   (wireplumber-rs or pw D-Bus).
5. **ws-overview:** a Rust ext-image-copy-capture client (in-process thread or the helper with the same JSON),
   plus the animated stage.
6. **ws-desktop:** per-monitor BACKGROUND layer, wallpaper (texture cache, fit modes, colour CSS, mpvpaper
   supervision), wallpaper settings window (reuses the tessel-gtk WallpaperChooser).
7. **ws-icons:** icon grid, selection, rubber band, keyboard nav, drag across monitors, rename, menus, positions
   file, monitor hotplug, workarea following.
8. **ws-fileops:** Gio async transfer worker, conflict dialog, toast, trash watcher, clipboard and `wl-copy`,
   notifications.
9. **ws-prompt:** standalone binary `tessel-prompt` (exit codes, stdin streaming).
10. **ws-capture-picker:** standalone binary (dmenu stdio, CaptureSelection client, tiling `capture-output`).

Do 1–4 first: Alt+Tab is latency-critical and is the main RSS win. Desktop parity (6–8) is the largest piece.
9 and 10 are independent and small.

## 12. Riskiest parity points
- **Alt-release detection:** race between `wl_keyboard.enter` modifiers, the release event and the 50 ms poll.
  `display.sync()` must complete before activation, or the exclusive layer keeps focus.
- **Layer order versus mpvpaper:** the BACKGROUND→BOTTOM→BACKGROUND bounce. Exclusive zone −1 on desktops,
  with the icon area constrained by D-Bus workareas instead.
- **Desktop positions format:** global coordinates keyed by filename. A changed write would trigger tessel-dock and
  other monitors. Skip unchanged writes.
- **wallpapers.json:** the merge-write must keep `_view_prefs` and unknown keys. Colour strings must stay in
  GDK `to_string()` form (`rgb(0,0,0)`), because tessel-dock's palette and tessel-lock read them.
- **Capture picker:** stdout must carry exactly one record ≤4095 bytes, or nothing. Never use portals inside
  the chooser (deadlock).
- **Prompt here-doc delimiter algorithm:** it must match byte-for-byte, or the installer rewrites rc.xml.
- **Launched apps must leave tessel-shell's cgroup** (systemd-run), or the installers refuse restarts.
