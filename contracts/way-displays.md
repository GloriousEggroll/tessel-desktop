# way-displays (way-displays-gui + display color) — external contract

Source: `~/btmanager/way-displays` (`way-displays-gui` 3,076 LOC single script, `color_config.py` 199,
`color_controls.py` 231, `display_style.py` 70, `install_color.py` 64). The **daemon**
`/usr/bin/way-displays` (upstream C, RPM) is *not* ported; only the GUI ("Displays") and the shared
colour helpers. The GUI is also **embedded as a Python module by Global Settings**
(`global-settings/desktop_pages.py:46-215`).

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Script | `~/.local/share/way-displays/way-displays-gui` (0755) + `display_style.py`, `companion_theme.py`, `background_blur.py`, `libwaydock-background-blur.so`, `suite_i18n.py`, `language_data.py`, `suite_catalogs.py`, `color_config.py`, `color_controls.py` | brightness-manager/install_waybar.py:111-125; install_color.py:11-12 |
| Launcher | `~/.local/bin/way-displays-gui` = `#!/bin/sh\nexec /usr/bin/python3 ~/.local/share/way-displays/way-displays-gui "$@"` | install_waybar.py:126 |
| System copy | `/usr/bin/way-displays-gui` (older RPM build; not touched) | |
| .desktop | `~/.local/share/applications/way-displays-gui.desktop` (copy of `way-displays/way-displays-gui.desktop` with `Exec="<launcher>"` desktop-escaped): `Name=Display Configuration`, `GenericName=Displays Configuration`, `Icon=wdisplays`, `Categories=GTK;Settings;DesktopSettings;HardwareSettings;` | install_waybar.py:127-134 |
| Launched by | brightness-manager `AppCatalog().launch("way-displays-gui.desktop")` (brightness_manager.py:293); Global Settings Displays page (embedded) | |
| GApplication id | `com.nobara.WayDisplaysApp`; `do_activate`: `get_config_data()` then new `WayDisplaysApp` window (no single-window reuse) | way-displays-gui:3065-3076 |
| CLI | none (argv ignored) | |
| Daemon unit | `~/.config/systemd/user/way-displays.service`: `ExecStart=/usr/bin/way-displays`, `Restart=on-failure`, `After/WantedBy=graphical-session.target`; started from session-startup autostart `systemctl --user start way-displays.service` (session-startup/configure_session.py:354) | |
| Installer | `install_color.py [--apply]`: requires `~/.local/share/way-displays/display_style.py` and `global-settings/desktop_pages.py`; backup `~/.local/state/desktop-suite/backups/display-color-<stamp>/` + `manifest.json`; also installs `global-settings/settings_search.py` | install_color.py |

## 2. IPC

### way-displays daemon socket (consumed) — way-displays-gui:542-597
- Path: first existing of `$XDG_RUNTIME_DIR/way-displays.sock`, `$XDG_RUNTIME_DIR/way-displays.$XDG_VTNR.sock`; runtime default `/tmp`. Same lookup in `brightness-manager/display_scaling.py:28-31`.
- `AF_UNIX SOCK_STREAM`, timeout 5 s; send `"OP: <OP>\n<yaml payload>"`; read until EOF (8192 chunks; abort > 16 MiB); YAML response.
- Ops used: `GET` (expects dict with `STATE` dict: `STATE.HEADS[]` with `NAME`, `DESCRIPTION`, `CURRENT{…VRR…}`, `DESIRED{VRR…}`, modes; `CFG` dict), `CFG_SET`, `CFG_DEL`, `CFG_WRITE`. Payload = `yaml.safe_dump({"CFG": cfg}, sort_keys=False).rstrip()`.
- CFG keys written: `MODE{desc:{WIDTH,HEIGHT,HZ(float = refresh_mHz/1000)}}`, `SCALE{desc:float}`, `AUTO_SCALE:true`, `TRANSFORM{desc:"90|180|270|FLIPPED|FLIPPED-90…"}`, `DISABLED{desc:null}`, `VRR_OFF[desc]`, `CALLBACK_CMD`, `ORDER[names]`, `ARRANGE ROW|COLUMN`, `ALIGN TOP|MIDDLE|BOTTOM|LEFT|RIGHT`. Save sequence: `CFG_DEL del_cfg` (if any) → `CFG_SET cfg` → `CFG_WRITE` → apply XWayland primary now and again after 500 ms → refresh VRR only (save_config way-displays-gui:2669-~2990).
- `CFG_DEL` contents: TRANSFORM entries reset to STANDARD, DISABLED entries re-enabled, SCALE entries switched to auto, `VRR_OFF` entries (desc and output NAME) re-enabled, `CALLBACK_CMD:""` if callback cleared.
- `VRR_OFF` is sent only for *changed* monitors (CFG_SET adds members; sending a full list would re-disable monitors changed in another editor). Default (unedited) STANDARD transform is removed, `FLIPPED-STANDARD` → `FLIPPED`.
- Callback/notify switch: default `notify-send "way-displays ${CALLBACK_LEVEL}" "${CALLBACK_MSG}"`; immediate `CFG_SET {CALLBACK_CMD}` on edit, `CFG_DEL {CALLBACK_CMD:""}` when cleared (2515-2562).

### D-Bus (consumed): `org.waybar.BrightnessManager` (session)
Object `/org/waybar/BrightnessManager`, iface `org.waybar.BrightnessManager.Controls`: `GetState()→s(JSON)`, `Refresh()→b`, `SetValue(s connector, s feature, d value, s expected_id)→b`, `SetAutoDim(b)→b`, `SetScale(s,d,s)→b`, `SetAutoScale(b)→b`, `SetSettings(s JSON)→b`, signal `Changed(s JSON)`. Via `brightness-manager/brightness_client.py` loaded by path (repo sibling, then `~/.config/waybar/scripts/brightness-manager/brightness_client.py`) (way-displays-gui:741-760). Features used: `brightness`, `contrast`; auto-dim switch + settings (dim %, durations). Slider sends debounced 180 ms (`GLib.timeout_add(180)`), keyed by display `id` (expected_id) so hotplug identity change cancels (BrightnessControls 761-998).

### Wayland
- `zwlr_layer_shell_v1` via gtk4-layer-shell (`CDLL('libgtk4-layer-shell.so', RTLD_GLOBAL)` before GTK): `MonitorOverlay` identify labels — layer **OVERLAY**, exclusive zone **-1** (ignore panels; sits at output 0,0), anchor TOP+LEFT, keyboard NONE, `set_monitor(monitor)`, empty input region (`cairo.Region()`) → click-through (600-740, 2177-2232). Monitor matched by `desc in GdkMonitor.description`.
- No wlr-output-management: all output changes go through the daemon.

## 3. Files written / read

| Path | Format | Notes / other users |
|---|---|---|
| `$XDG_CONFIG_HOME/labwc/environment` | managed block `# way-displays-gui primary output environment begin` / `…end` with `SDL_VIDEO_DISPLAY_PRIORITY=`, `WAYLANDDRV_PRIMARY_MONITOR=`, `PROTON_WAYLAND_MONITOR=` (= connector). Added after a blank line; removed when no primary; atomic, mode preserved (new 0600); returns `(ok, err, changed)` | 72-182. Read by **waydock `gtk_color_scheme.py:23-24,216`** (primary monitor ordering) and language-manager also rewrites this file (other keys). Change → `labwc --reconfigure` |
| `$XDG_CONFIG_HOME/labwc/xinitrc` (same dir as environment) | block `# way-displays-gui primary output begin` / `# output: <name>` / `if command -v xrandr …; then\n  xrandr --output '<quoted>' --primary >/dev/null 2>&1 \|\| true\nfi` / `# way-displays-gui primary output end`. Adopts/removes standalone `xrandr --output X --primary [&|>/dev/null 2>&1 [|| true]]` lines; new file `#!/bin/sh\n` mode 0700; not created when empty | 368-476 |
| rc.xml (`LABWC_PID` cmdline `-c/--config`, `-C/--config-dir`, else `$XDG_CONFIG_HOME/labwc/rc.xml`) | HDR block `<!-- way-displays-gui HDR overrides begin -->` `<hdrOutputs><output name=".." enabled="yes|no" /></hdrOutputs>` `<!-- … end -->` before `</labwc_config>`; reads global `<core><hdr>` and all `<hdrOutputs>` | 184-346 |
| rc.xml colour block (color_config.py) | `<!-- way-displays-gui color management begin -->` `<colorManagement><output name profile calibration="true|false" sdrBitDepth="8|10"/>…` `<!-- … end -->`, appended last before root close (expands self-closing root); merges managed outputs; byte-preserves the rest; lock `.<rc>.color.lock` (flock); backup `$XDG_STATE_HOME/desktop-suite/backups/color/<ns>-rc.xml`; `rollback(tx)` refuses if file changed | color_config.py:14-199 |
| `$XDG_RUNTIME_DIR/labwc-color-$WAYLAND_DISPLAY.ini` (read) | keyfile written by labwc patch 0008: `[compositor] pid, version=1, icc_supported, hdr_calibration_supported, hdr_correction_supported`; per-output `[NAME] display_id, description, enabled, hdr, hdr_supported, hdr_peak_nits, hdr_max_frame_average_nits, hdr_reference_white_nits, hdr_target_*_nits, hdr_calibration_active, hdr_correction_*, pending, icc_active, profile, error, bit_depth`. Valid only if owned by uid, pid alive, version 1, matches `$LABWC_PID`, compositor not started with `-m/--merge-config`; values unescape `\s \n \r \t \\` | color_config.py:92-119 |
| `$XDG_CACHE_HOME/waydock/palette.json` | via `companion_theme.attach` (live per-monitor palette) | display_style.py |

ICC validation (color_config.py:62-89): absolute path, regular file (O_NONBLOCK so FIFOs don't block), 128 B…16 MiB, `acsp` @36, class `mntr`, `RGB `, version byte 2|4, liblcms2 `cmsOpenProfileFromMem` + `cmsIsIntentSupported(…,1,1)`.

Primary output at load: `CALLBACK_CMD` managed block → environment block → `xrandr --query --current` `connected primary` (2270-2282). Primary only allowed on enabled monitors; single-select.

## 4. UI
- Main window `WayDisplaysApp(application, brightness_client_factory=None, *, embedded=False)` — Global Settings constructs it with `embedded=True`, steals `get_child()`, hides the heading found via `global_style._find_title`, changes inner margins 30→24, relies on module globals `CONFIG_DATA`, `get_config_data()`. Title `Displays`, default ≤980 wide, headerbar; `.page-title`, `.subtitle`, `Save`/Apply button `.primary` (insensitive until a change). When not embedded, applies XWayland primary on idle.
- Widgets: `MonitorLayout(Gtk.Fixed)` drag-arrange with edge snapping (threshold 30, recenter, flash), notebook tabs per monitor (`MonitorWidget`: enable, primary, resolution/refresh dropdown, scale + auto switch, rotation buttons `.orientation-button`, flip, VRR, HDR (per-output labwc override), `ColorControls`, `BrightnessControls`).
- CSS classes (display_style.py:13-60): window `way-displays` / `way-displays-overlay`; `.settings-card .brightness-controls .monitor-title .heading .settings-row .orientation-button .hdr-details .hdr-details-title .hdr-value .monitor-frame .overlay-label .panel-bar .snap-flash .overlay-box .dim-label .error`; overlay CSS `window#overlay-window`, `.connector-label .resolution-label .device-label`. companion_theme special-cases `way-displays-overlay` (waydock/companion_theme.py:316,584).
- `Calibrate HDR…` spawns `python3 ../hdr-calibration/hdr_calibration.py --output <connector>` (start_new_session) (color_controls.py:105-113). Colour status refreshed by Gio directory monitor on rc.xml parent.

## 5. External commands
`labwc --reconfigure` (timeout 5), `xrandr --query --current`, `xrandr --output X --primary|--noprimary` (timeout 2, verified by re-query: `XRandR reports {value1} after applying {value2}`), `notify-send` (via daemon callback), hdr_calibration.py.

## 6. Parity test cases
`test_primary_xinitrc.py`: create (0700) / change / repeat save is no-op (mtime unchanged, `(True,None,False)`); adopts existing `xrandr --output "old" --primary`, keeps other lines and mode 0750, ignores commented examples; removes duplicate primary commands and other managed blocks; clearing removes; empty selection doesn't create file; connector with `$(…)` stays a literal argument; failed `os.replace` keeps original and leaves no `.xinitrc.*` temp.
`test_color_config.py`: read side-effect free, last override wins; apply preserves comments/HDR/unrelated XML then rollback restores; second editor merges other monitors & escapes names; malformed XML / missing profile writes nothing; rollback refuses to clobber later edit; real ICC validated and FIFO rejected without blocking; stale status file → `{}`; child-element syntax and trailing comments preserved.
`test_monitor_overlay.py` (headless labwc): overlay sits at output corner despite reserved TOP 72 / LEFT 64 panels, and stays above fullscreen windows.
`test_brightness_integration.py`: connector matching/refresh/unsupported contrast; Apply runs primary command & updates startup script; slider coalescing on exact display; pending slider cancelled on service loss/busy/close; missing monitor/write failure shows no fake success; hotplug identity change cancels; busy feature defers pending value; loading ≠ unsupported; shared auto-dim refresh w/o extra writes; busy policy not overwritten; local policy waits for async reply and reverts on failure; colour changes pending & shared with embedded editor; unsupported compositor explained; HDR report distinguishes metadata/white target/measurement; **VRR in two stale editors preserves other monitor** (only changed VRR sent); **auto-scale deletes only that display's SCALE** (`CFG_DEL SCALE{first}`, `CFG_SET AUTO_SCALE:true, SCALE{second}`); VRR uses DESIRED when runtime off; brightness changes keep Apply state; follows live dock palette; rebuilt tabs/close cancel debounce and client.

## 7. Risks
- Global Settings imports the Python module and pokes internals (`CONFIG_DATA`, `WayDisplaysApp(embedded=True)`, title widget tree). Until GS is Rust, keep the Python GUI installed or expose an embeddable widget from a Rust GS.
- Exact managed-block text (env, xinitrc, rc.xml HDR/colour) is parsed by waydock and by the next GUI run — byte-compatible output required.
- YAML round-trip with the daemon (`sort_keys=False`, `HZ` float, `DISABLED{desc: null}`).
- Protocols: layer-shell only (gtk4-layer-shell crate is enough); empty input region via `gdk::Surface::set_input_region`.
