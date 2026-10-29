# tiling-manager (Waytile) — external contract

Source: `~/btmanager/tiling-manager` (Python 1,027 LOC + `native/tiling.c` 811 LOC C).
The **layout engine lives inside labwc** (patch `labwc-color/packaging/0007-add-opt-in-native-tiling.patch`,
`0009-bound-tiles-and-park-windows.patch`, `0011-query-capture-window-output.patch`, `0012-…`;
current source `labwc-color/labwc/src/tiling.c`). `tiling-manager/native/tiling.c` is an older
copy **without** `capture-output` (diff vs labwc-color: +`foreign.h`, +lines 736/779-795).
The Rust port replaces only the Python client/CLI/UI; the C compositor code is **not** ported
(it stays in the labwc RPM). The datagram protocol below is the frozen boundary.

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Launcher | `~/.local/bin/waytile` = `#!/bin/sh\nexec /usr/bin/python3 ~/.config/waybar/scripts/tiling-manager/tiling_manager.py "$@"` (0755) | install.py:39 |
| Install dir | `~/.config/waybar/scripts/tiling-manager/` (bindings, tiling_*.py + shared popup_placement, gtk4_utils, tray_popup, suite_i18n, language_data, suite_catalogs, background_blur(.so), companion_theme, global-settings/workspace_config.py) | install.py:23-37 |
| Dock files also shipped | `~/.local/share/waydock/{dock_config,panel_actions,panel_ui,status_backend,media_status,lock_media,lock_media_icons,media_controls,group_layout,dock_ui,dock_style}.py` | install.py:17 |
| .desktop | `~/.local/share/applications/org.waydock.Tiling.desktop`: `Name=Window Tiling`, `Comment=Window tiling shortcuts`, `Exec=<launcher> --settings`, `Icon=view-grid-symbolic`, `Categories=Settings;DesktopSettings;` | install.py:40-42 |
| labwc autostart | line `"$HOME/.local/bin/waytile" --restore >/dev/null 2>&1 &` preceded by comment `# Restore optional window tiling only in the labwc session.` appended once to `~/.config/labwc/autostart` (live: lines 78-79) | install.py:16,43-47 |
| systemd | **none** (deliberately; no XDG autostart either) | README |
| GApplication id | `org.waydock.Tiling` (popup only, single instance; 2nd activate → `controller.activate()`) | tiling_ui.py:92-102 |
| Status stream | `tiling_status.py` = `main()` with argv forced to `--watch` (spawned by waydock) | tiling_status.py:1-6 |
| Installer | `install.py [--apply]`; dry-run prints paths; `--apply` backs up to `~/.local/state/desktop-suite/backups/waytile-<UTCstamp>/` + `new-files.json`, restarts `waydock.service` if active, waits for `waydock --check` JSON `ready` (15 s), rolls back on failure | install.py:62-108 |

### CLI (`tiling_manager.py`, mutually exclusive) — tiling_manager.py:12-55
| Flag | Behaviour | Exit |
|---|---|---|
| `--status` | print `json.dumps(status())` one line | 0 |
| `--watch` | every 1 s: `status()`; `OutlineSync.sync()` (errors ignored); print JSON line **only when changed** (flush); ends on SIGINT/BrokenPipe | 0 |
| `--enable` / `--disable` / `--toggle` | `status()`; if not ready → stderr `error`, exit 1; else `TilingConfig().set_enabled(...)` | 0/1 |
| `--restore` | only if `config.json` `enabled is True` → `set_enabled(True, remember=False)`; never consults ready first | 0/1 (errors to stderr) |
| `--settings`, `--popup`, none | GTK popup (`tiling_ui.run(edge, offset)`) | app rc |
| `--popup-edge {top,bottom,left,right}` (default bottom), `--popup-offset INT` (default 76, clamped ≥0) | popup placement | |

Status must import no GTK (watch is long-running inside waydock).

## 2. IPC — compositor control socket (consumed)

- Path: `$XDG_RUNTIME_DIR/waytile-$WAYLAND_DISPLAY.sock` (fallback runtime `/run/user/<uid>`), `AF_UNIX SOCK_DGRAM`. tiling_client.py:28-44; tiling.c init 781.
- Client rules: reject if `WAYLAND_DISPLAY` empty or contains `/`, or command > 255 bytes; `stat` target must be owned by uid and `S_ISSOCK`; bind reply socket at `mkdtemp(prefix='wt-', dir=runtime)/reply`; timeout 1.5 s; `recv(65536)`; reply must have `version==1 && ready is True`; non-empty `error` → `RuntimeError(error)`. 
- Server: `SO_PASSCRED`, drops requests without matching `SCM_CREDENTIALS` uid, truncated (`MSG_TRUNC`) or >255 bytes; socket chmod 0600; stale same-uid socket unlinked only on `ECONNREFUSED`; reply dropped if ≥60000 bytes.
- Requests (exact strings): `status`, `windows`, `enable`, `disable`, `outline on`, `outline off`, `outline-color <output> <r> <g> <b>` (0-255, ≤64 outputs, name ≤127), `action <tiling command>`, `capture-output <foreign-toplevel identifier>` (labwc-color only). While session locked, everything except `status`/`windows`(/capture-output) returns `error:"The session is locked."`; unknown → `"Unknown tiling request."`.
- Reply JSON: `{"version":1,"ready":true,"compositor_pid":N,"enabled":B,"parking_supported":true,"tile_bounds_supported":true,"outline_supported":true,"outline_enabled":B,"error":"…","windows":[…],"capture_output_supported":true,"capture_output":{x,y,width,height}|null}`. `windows` entries (only for `windows`): `id,active,tiled,visible,output(id_bit),x,y,width,height,fullscreen,minimized,outlined,parked,current{x,y,width,height},workarea{x,y,width,height}`.
- Other consumers: `wayshell/capture_output.py:22-41` (`request('capture-output '+id)` → match Gdk monitor geometry → connector; falls back to `waydock-toplevel --snapshot`). `wayshell/install_capture_picker.py:34` ships `tiling_client.py`. So the Rust crate must expose the client as a library (suite-core) too.

### status() dict (tiling_client.py:57-68) — the waydock status-stream schema
Success: reply + `text:"▦"`, `class:"active"|"inactive"`, `tooltip: tr('Window Tiling: On'|'Window Tiling: Off') + "\n" + tr('Click to toggle · Right-click for shortcuts')`, `sign_out_required:false`.
Failure: `{ready:false, enabled:false, text:"▦", class:"unavailable", error:str, tooltip: tr('Window Tiling')+"\n"+error, sign_out_required: error == tr(SIGN_OUT_REQUIRED)}`.
Error strings (translated): `Tiling requires a labwc Wayland session.` / `Sign out and back in to load the labwc tiling update.` (socket missing) / `The tiling control socket is not owned by this user.` / `The compositor did not respond to the tiling request.` / `This compositor tiling interface is not supported.`

## 3. rc.xml keybinding overlay (tiling_config.py, bindings.py)

- Config path: `workspace_config.session_config()` (global-settings) with `LABWC_PID` temporarily set to reply `compositor_pid` → (path, fallback); fallback read if path missing; default `<labwc_config/>` expanded to `<labwc_config>\n</labwc_config>\n`. tiling_config.py:55-68,181-187.
- Managed blocks inside `<keyboard>` and `<mouse>`: `<!-- Waytile keyboard begin -->` … `<!-- Waytile keyboard end -->` (same for `mouse`), inserted before the section's closing tag, block text = begin+"\n"+content+"\n    "+end. Missing or self-closing section → replaced by `<!-- Waytile <s> section <base64 of original text> --><s><default/>BLOCK</s><!-- Waytile <s> section end -->`; on disable, unwrapped to the exact original unless the user added children (then body kept). tiling_config.py:24-76.
- Rejects: `<!DOCTYPE`/`<!ENTITY`; root ≠ `labwc_config`; unbalanced/duplicate markers (`The tiling keybinding block was edited; repair its markers first.`); multiple `<keyboard>`/`<mouse>` sections. Comments masked when searching sections.
- Keyboard content: one row per BINDINGS entry `<keybind key="K"><action name="Tiling" command="CMD"/></keybind>` (4-space indent) + `W-k` → Execute `shlex.join([launcher,'--settings'])` and `W-C-l` → Execute `~/.local/bin/wayshell-lock` (HTML-escaped). bindings.py:66-72.
- BINDINGS table (key, label, description[i18n], command) bindings.py:10-63: monitor next/previous, width-save/restore, `W-w park`, `W-z restore-parked`, `W-A-w`/`W-q close`, `W-j split`, `W-t float`, `W-p pseudo`, `W-f fullscreen`, `W-A-f maximize`, `W-C-f tiled-fullscreen`, `W-o pop`, `W-A-l layout`, scratch-show/send (`W-s`,`W-A-s`,`W-grave`,`W-S-grave`), workspace right/left/previous, group/ungroup/group-next/prev, per arrow `focus|swap|group-move|workspace-monitor l/r/u/d`, `W-<n>` workspace n, `W-S-<n>` send, `W-S-A-<n>` send-silent (n=1..10, key 0 for 10), `W-A-1..5 group-index`, resize x/y ±100/±25(A)/±300(C) with keys minus/equal. Keys must be unique; never `W-l`, never `C-A-Delete`, never `close-all`.
- Mouse content: `<context name="Frame">` with W-Left Press(Focus,Raise)/Drag(Move), W-Right Press/Drag(Resize), scroll W-Up/W-Down → `workspace left/right`, W-A-Up/Down → `group-prev/next`. bindings.py:75-85.
- `EXTRA_SHORTCUTS` (display only) bindings.py:87-94.
- global-settings `shortcuts_model.py:126-144` imports `bindings.BINDINGS` to list/describe tiling shortcuts and detect conflicts → Rust must keep a machine-readable export (or global-settings keeps reading the Python file until ported).

### Transaction `set_enabled(enabled, remember=True)` (tiling_config.py:122-222)
1. `flock(~/.local/state/waytile/change.lock)`. 2. `status` → path. 3. backup `~/.local/state/waytile/rc-<UTC %Y%m%dT%H%M%S%fZ>.xml` (0600). 4. if file changed since read → `The labwc configuration changed. Try again.` 5. atomic write (keep mode; new 0600). 6. `labwc --reconfigure` (env `LABWC_PID=compositor_pid`, timeout 5, check). 7. send `enable`/`disable`; reply `enabled` must match else `The compositor did not confirm the tiling change.` 8. if remember: merge `{"enabled": B}` into config.json. 9. OutlineSync (errors swallowed). On any exception: restore rc.xml only if still our bytes (unlink if it didn't exist), reconfigure, send previous enable state, re-raise.

### Gapless `set_gapless(B)` (tiling_config.py:143-180, tiling_gaps.py:20-90)
- No-op if preference already equals B (no reload). gap = 0 if on else `gapped_spacing` (default 10). `with_gap` replaces `<core gap=".."/>` attribute and all `<gap>` children with a single trailing `<gap>N</gap>` in the one `<core>` (creates `<core>` if absent, expands self-closing root), byte-preserving everything else; returns previous effective gap (last `<gap>` child wins over attribute). Rejects DOCTYPE, >2 MiB, non-`labwc_config` root, multiple `<core>`, non-int gap, gap outside 0..2^31-1, bool/float/str gap.
- Backup `gap-<stamp>.xml`; on enable store `gapped_spacing=previous`; write rc.xml, reload, then write prefs; on reload/pref failure restore rc.xml (+ reload).

## 4. Files

| Path | Format | Writers / readers |
|---|---|---|
| `$XDG_CONFIG_HOME/waytile/config.json` (0600) | `{"enabled":bool,"focus_outline":bool(default true),"gapless":bool,"gapped_spacing":int,...}` unknown keys preserved | tiling_config; global-settings `layout_page.py:117-164`, `layout_presets.py:124-127` (TilingConfig.load_preferences/set_gapless); `dock_pages.py:257-282` embeds `TilingControls` |
| `$XDG_STATE_HOME/waytile/change.lock` | flock | all mutators |
| `$XDG_STATE_HOME/waytile/{rc,gap}-<stamp>.xml` | backups | |
| `$XDG_CACHE_HOME/waydock/palette.json` | read: `monitors[].{name, accent:[r,g,b]}` (≤64, name `[A-Za-z0-9_.-]{1,120}`, ints 0-255) | written by waydock palette_state |
| rc.xml (session config) | see §3 | labwc, global-settings |

### OutlineSync (tiling_outline.py:24-43)
Skip unless `outline_supported`. enabled = prefs `focus_outline` is not False. Snapshot `(compositor_pid, enabled, colors)`; if equal to previous **and** `outline_enabled==enabled` → no calls. Else send `outline on|off` if differs, then `outline-color NAME R G B` for every valid monitor. Files >1 MiB or invalid → `{}`.
`set_outline(B)`: lock, `status`, if supported send `outline on|off`, write prefs `focus_outline`; on write failure revert native outline to previous `outline_enabled`; returns `outline_supported`.

## 5. UI surfaces

- Popup window (tiling_ui.py:15-88): `Gtk.ApplicationWindow` title `Window Tiling`, CSS class `tiling-shortcuts`, size `set_popup_width()`×620, child `.popup-surface`; uses shared `tray_popup.PopupController(edge, offset)` (layer-shell placement shared with all applets), `popup_header('view-grid-symbolic', 'Window Tiling', ' ')`, `companion_theme.attach`. CSS classes: `.shortcut-row`, `.key`, `.detail`. Subtitle: `On · New windows tile automatically.` / `Off · Windows move and resize normally.` (hidden if not ready). Hint text, `Gtk.SearchEntry` (`Search shortcuts`), rows filtered by all casefolded words in `shortcut + tr(description)`; scrolled list min 120 / max 480 px. Refresh every 2 s while visible; on unrealize remove timer & CSS provider.
- `TilingControls` (tiling_settings.py:176-287), shared with Global Settings: rows `Window tiling`, `Gapless`, `Focused window outline` (switches; `AsyncSwitch` worker-thread apply, insensitive while busy, revert on error), notices, `.error` label, `Sign out now` button (AlertDialog `Log Out` / `Log out of this session? Save your work before continuing.`, buttons Cancel/Log Out, runs `labwc --exit` only if `/proc/$LABWC_PID/comm == labwc`), `Keyboard shortcuts…` button spawning `waytile --settings`. CSS classes `settings-muted detail gs-row-subtitle`, `error`. Notice strings: `Takes effect after you sign out and back in.`, `Sign out and back in to load the window parking update.`, `Saved. Sign out and back in to load the compositor outline update.`
- Waydock hosting: dock item id `tiling` (`dock_config.py:41`, label "Window Tiling", Always-visible group, sits after Bluetooth), glyph `▦` (`status_backend.py:25`, `panel_ui.py:28`), icons `view-grid-symbolic`/`view-app-grid-symbolic` (`panel_ui.py:48`), status stream `tiling-manager/tiling_status.py` (`status_backend.py:32`, JSON lines `text/class/tooltip`), left-click → `tiling_manager.py --toggle`, right-click → `--settings --popup-edge E --popup-offset N` (`panel_actions.py:41-42`).

## 6. External commands
`labwc --reconfigure` (env LABWC_PID), `labwc --exit`, `systemctl --user is-active/restart waydock.service` + `~/.local/bin/waydock --check` (installer only), `waytile --settings`.

## 7. Parity test cases
From `tests/test_config.py`, `test_gaps.py`, `test_outline.py` (pure logic; port 1:1):
1. BINDINGS never contain `W-l`, `C-A-Delete`, `close-all`; `W-A-l`→`layout`; keys unique; block contains `.local/bin/wayshell-lock`.
2. enable→disable is byte-identical to original; enable is idempotent.
3. Missing / self-closing `<keyboard/>`/`<mouse/>` restore exactly (base64 wrapper); user binding added inside created section survives disable, markers removed.
4. Compositor `enable` failure → rc.xml byte-restored, no prefs file, compositor left disabled.
5. `wanted()` tracks set_enabled; disable restores bytes.
6. with_gap keeps prefix/suffix bytes, other `<core>` children and comments, for both `<gap>` child and `gap=` attribute; round-trips previous gap.
7. Missing/self-closing core/root, commented-out core → previous 0.
8. Invalid XML/values raise and generate nothing (`<other/>`, 2×core, DOCTYPE, `gap="broken"`, gap -1/True/1.5/'0').
9. Gapless persistence keeps other prefs, stores `gapped_spacing`, repeat call = no reload, new controller restores 17.
10. Reload failure or prefs write failure → rc.xml restored, no prefs.
11. OutlineSync: per-monitor colors, names with newline rejected, idle sync silent, palette change resends, `focus_outline:false` → `outline off`, `{}` state → no calls, new compositor_pid → resend.
12. set_outline preserves other keys; unsupported → only `status` call; write failure → `outline off` then `outline on`.
13. `test_native.py` (headless labwc, compositor behaviour; keep as integration suite for the C side, not the Rust port): oversized clients cropped, min sizes, gapless reflow, mixed scale, XWayland min/fullscreen output, park/restore LIFO, scratchpad/groups/scrolling, outline never on fullscreen, toggle churn, real config overlay + keys, popup uses shared theme.
14. Waydock: `test_panel_actions.py` command lines for `tiling` (toggle vs settings+edge/offset).

## 8. Risks
- Popup + Global Settings share `TilingControls` widget in-process; GS embedding requires either a Rust GS or keeping the Python module installed.
- `--watch` diff semantics (print only on change, first line always) and 1 s cadence; waydock parses these lines.
- Byte-exact XML editing (no DOM re-serialization of the whole file) — needs a span-preserving parser (quick-xml with byte offsets), not a DOM writer.
- Wayland protocols: none on the client side (control via private socket). Popup needs layer-shell via shared `tray_popup` (gtk4-layer-shell).
