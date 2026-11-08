# tessel-capture (tessel-capture: Screenshot & Screen Recorder) — external contract

Source: `~/tessel-bluetooth/tessel-capture` (`capture.py` 129, `capture_ui.py` 956, `capture_media.py` 408,
`capture_editor.py` 331, `capture_portal.py` 185, `capture_area.py` 161, `capture_shortcuts.py` 159,
`capture_results.py` 123, `capture_settings.py` 104, `capture_notifications.py` 72, `capture_clipboard.py` 70,
`capture_vulkan.py` 70, `capture_native.py` 61, `capture_tray.py` 32, `capture_popups.py` 25) plus
`shared/capture_selection.py` 84 (server side `SelectionBroker`; the client half is already in
`components/capture-picker/src/selection.rs`). **Not ported:** `capture_pipewire.c`
(`libcapture-pipewire.so`, stays C, loaded with dlopen), `portal/` (patched xdg-desktop-portal-wlr, C),
- Screen-share portal: the distribution's `xdg-desktop-portal-wlr` (Nobara 0.8.4-4 or newer: colorimetry, RemoteDesktop and the chooser-session patch, which exports `XDPW_CHOOSER_SESSION` to the picker). The suite no longer builds its own portal; `tools/install.py` `retire-old` removes the old `tessel-capture-portal-wlr` override and binary once that package is installed.

## 1. Entry points and launchers

| Item | Value | Ref |
|---|---|---|
| Launcher | `/usr/local/bin/tessel-capture` (0755, root) = `#!/bin/sh\n# Tessel standalone application\nexec /usr/bin/python3 "/usr/local/lib/tessel/tessel-capture/capture.py" "$@"` | desktop-apps/install.py:46-50 |
| Install dir | `/usr/local/lib/tessel/tessel-capture/`: all non-test, non-install `*.py`, `gtk4_utils.py suite_i18n.py language_data.py suite_catalogs.py background_blur.py libwaydock-background-blur.so app_menu.py companion_theme.py popup_placement.py capture_selection.py libcapture-pipewire.so` | desktop-apps/install.py:31-60 |
| .desktop | `/usr/share/applications/org.tessel.Capture.desktop`: `Name=Screenshot`, `Exec=/usr/local/bin/tessel-capture --screenshot`, `Icon=camera-photo-symbolic`, `StartupNotify=true`, `StartupWMClass=org.tessel.Capture`, `Categories=Graphics;`, `Keywords=Screenshot;Capture;Print;Screen;HDR;`, `Actions=Record;` → `[Desktop Action Record] Name=Screen Recorder Exec=/usr/local/bin/tessel-capture --record Icon=media-record-symbolic` | org.tessel.Capture.desktop |
| .desktop | `/usr/share/applications/org.tessel.ScreenRecorder.desktop`: `Name=Screen Recorder`, `Exec=/usr/local/bin/tessel-capture --record`, `Icon=media-record-symbolic`, `StartupWMClass=org.tessel.Capture`, `Categories=AudioVideo;Recorder;`, `Keywords=Recording;Capture;Video;Screen;HDR;`, `Actions=Stop;` → `[Desktop Action Stop] Name=Stop and Save Recording Exec=/usr/local/bin/tessel-capture --stop Icon=media-playback-stop-symbolic` | org.tessel.ScreenRecorder.desktop |
| GApplication | id `org.tessel.Capture`, flags `HANDLES_COMMAND_LINE` (unique; remote instances forward argv, primary's return value is the exit code), `GLib.set_prgname(ID)` → Wayland app_id `org.tessel.Capture`. Object path `/org/tessel/Capture`. Not autostarted at login | capture.py:46-123 |
| Who launches it | Tessel Dock `panel_actions.capture()` via `systemd-run --user --quiet --collect --service-type=exec --slice=app.slice --expand-environment=no --setenv=… -- /usr/local/bin/tessel-capture <args>` with `settings → --record`, `stop → --stop`, `none/desktop/desktop-mic → --start-recording --audio-preset X`; screen_record indicator right-click/gear `--record`; visible only if `/usr/local/bin/tessel-capture` exists. labwc keybinds (§7). The app menu (.desktop files). | tessel-dock/panel_actions.py:55-75, status_backend.py:252-262 |
| Who imports it | Global Settings `capture_page.py` imports **`capture_settings.build_page(path)`** (Python, from the install dir) and restyles `controls.switches` rows → the Python `capture_settings.py` must stay installed until Global Settings is ported. `install_session.py` imports `capture_shortcuts.shortcut/load`. | tessel-settings/capture_page.py |

## 2. CLI (argparse; prog = basename of argv[0])

`--check` is handled in the launching process before the GApplication exists; everything else goes through
`Application.run(argv)` → primary `do_command_line`.

| Flag | Behaviour (primary) | Exit |
|---|---|---|
| `--check` | prints `json.dumps(result)`: `{"ready": b, "ffmpeg": which(ffmpeg), "pipewire": lib loaded, "video": which(ffprobe), "images": PIL}`; on exception only `{"ready": false, "error": "<str>"}` (missing `libcapture-pipewire.so` ⇒ dlerror text). | 0 if ready else 1 |
| `--lock-shot` | `window.locked_shot()` (no `not_before`) | 0 if started, 1 if no window / busy / not armed |
| `--stop` | if window busy **and** `capture_recording`: `stop_clicked()` → 0; else 1 | 0/1 |
| `--toggle-recording` | busy → as `--stop`; idle → `ensure_window`, `quick_capture=True`, `start_recording(audio_preset, hide_window=False)` (window visibility unchanged) | 0 (1 if busy) |
| `--start-recording [--audio-preset none\|desktop\|desktop-mic]` | `ensure_window`; busy → 1; `quick_capture=True`; `start_recording(preset, hide_window=True)` | 0/1 |
| `--capture workspace\|screen\|window\|area` | `ensure_window`; busy → 1; `quick_capture=True`; `prepare(target=X, hide_window=False)` (never presents/hides the window; saves to the *screenshot* folder even while the window shows recorder settings) | 0/1 |
| `--record` / `--screenshot` / none | `do_activate` (ensure window, `quick_capture=False`, unminimize, present); if not busy select mode 1 (`--record`) or 0 (`--screenshot`) | 0 |
| `-h/--help` | argparse help ("Portal-backed Screenshot and Screen Recorder."), exit 0. Errors: usage + `prog: error: …`, exit 2 (abbreviations `--rec`, `--capture=area` accepted; ambiguous `--st`). | |

## 3. GApplication actions (also exported as `org.gtk.Actions` on `/org/tessel/Capture`)

| Action | Param | Behaviour |
|---|---|---|
| `show` | — | `do_activate()` |
| `stop` | — | `window.stop_clicked()` if a window exists |
| `lock-shot` | `d` (CLOCK_MONOTONIC seconds when locking began) | `window.locked_shot(value)` — called by tessel-lock on Print: `org.tessel.Capture /org/tessel/Capture org.gtk.Actions.Activate("lock-shot", [<d>], {})`, NO_AUTO_START, 1500 ms (tessel-lock/lock_screen.py:581-590) |
| `open-saved` / `reveal-saved` | `s` file URI | `open_saved(app, uri, reveal)` (§9) — never creates the capture window |
| `copy-saved` / `edit-saved` | `s` file URI | copy file as PNG to clipboard / open the editor — never creates the capture window |
| `shortcuts`, `about`, `quit` | — | suite app menu (`<Control>question`, `<Control>q`); quit → `quit_requested` (refuses while recording/busy: present + error "Stop the current capture before quitting.") |

## 4. D-Bus

### Owned
| Name / path / iface | Members | Ref |
|---|---|---|
| `org.tessel.Capture` (GApplication) `/org/tessel/Capture/Status` `org.tessel.Capture.Status` | `GetState() → (s state, b armed)`; signal `Changed(s, b)` emitted only when `(state, armed)` changes. state ∈ `idle`, `preparing` (busy and window.capture_recording), `recording`, `stopping`. Registered in `startup` on the app connection, unregistered at shutdown. Consumer: tessel-dock `capture_status.py` (bus_watch_name, GetState NO_AUTO_START 2000 ms, Changed subscription by owner). | capture_tray.py |
| `/org/tessel/CaptureSelection` `org.tessel.CaptureSelection` on the **same connection, unique name** (registered lazily on the first PortalSession, once per connection) | `Select(o session, as tokens) → (b handled, s token)`: pops `targets[session]`; chosen = first token starting with `"Monitor: "` whose `token[9:].partition(' ')[0] == target` (only if ≤ 8192 tokens); returns `(target is not None, chosen or "")` — a vanished output answers `(true, "")` = cancel. `Selected(o session, as outputs)`: > 16 outputs or any > 128 chars → `org.freedesktop.DBus.Error.InvalidArgs "Invalid capture outputs"`; else pops `observers[session]` and calls it (sets `session.outputs`). Called by the Tessel Shell capture picker (the patched xdpw passes `XDPW_CHOOSER_SESSION`). | shared/capture_selection.py |

### Consumed
| Peer | Calls | Ref |
|---|---|---|
| `org.freedesktop.portal.Desktop` `/org/freedesktop/portal/desktop` `org.freedesktop.portal.ScreenCast` | `CreateSession({handle_token, session_handle_token})` → `SelectSources(o, {types u (1 monitor / 2 window), multiple b false, cursor_mode u (2 embedded if pointer on and not arming, else 1 hidden), persist_mode u 0, handle_token})` → `Start(o, "", {handle_token})` → `OpenPipeWireRemote(o, {}) → h` (fd list, async 10 s). Tokens `capture_<uuid4 hex>`; expected request path `/org/freedesktop/portal/desktop/request/<sender>/<token>` (sender = unique name without `:` and `.`→`_`), session path `…/session/<sender>/<token>` — required format for CaptureSelection. Calls 10 s timeout; renamed request handles followed. Subscribes `org.freedesktop.portal.Request.Response` (code 1 → cancelled, "Screen access was cancelled."; other ≠0 → "The portal could not start capture.") and `org.freedesktop.portal.Session.Closed` ("Screen sharing ended. The portal session was closed."). 120 s selection timeout ("Screen selection timed out."). Empty `streams` → "The portal returned no capture sources.". `remote_fd()` (worker thread) = sync `OpenPipeWireRemote` 5 s, a fresh fd per consumer. Close → `org.freedesktop.portal.Request.Close` on the pending request and `org.freedesktop.portal.Session.Close` (2 s, no reply needed). Stream props used: `position (ii)`, `size (ii)`, `source_type u`. | capture_portal.py |
| `org.freedesktop.Notifications` | `Notify(app "Screenshot"/"Screen Recorder" (translated), 0, icon camera-photo-symbolic / media-record-symbolic, title, body=path, actions, hints, 10000)`, 3000 ms, reply `(u)`. actions `["default","Open","folder","Open Folder"]` + screenshots `["copy","Copy","edit","Edit"]` (translated labels). hints `desktop-entry s "org.tessel.Capture"`, **`x-tessel-dock-output s`** (connector of the captured output; for window/workspace the pointer's monitor via PointerMonitor, then the first `Selected` output), **`x-tessel-dock-workspace b`** (kind == workspace). App `hold()` per pending id; released on `NotificationClosed`, 60 s expiry or shutdown. `ActionInvoked default/folder` → open/reveal; `copy`/`edit` → copy/edit saved. On Notify error → GNotification fallback `saved_notification` (id `capture`). | capture_notifications.py |
| `org.tessel.Dock` `/org/tessel/Dock` `org.gtk.Actions.Activate` | `("prepare-capture", [<b recording>], {})` before every capture (then 250 ms → choose target); `("finish-screenshot", [], {})` from `release_sources` when the dock popup was held (screenshots only). NO_AUTO_START, 1000 ms; errors ignored. | capture_popups.py |
| `org.freedesktop.FileManager1` | indirectly via `Gtk.FileLauncher.open_containing_folder` (ShowItems) when the default `inode/directory` handler is not `org.tessel.Files.desktop` | capture_results.py |
| GNotification (app.send_notification) | ids `capture` (failures "Capture failed", fallback saved notice: default `app.open-saved(uri)`, buttons `Open Folder → app.reveal-saved`, screenshots `Copy → app.copy-saved`, `Edit → app.edit-saved`; recording notices button `Stop & save → app.stop`, default `app.show`), `capture-open-error` ("Could not open saved capture", "Could not copy the screenshot"), `capture-editor-error` ("Could not open the screenshot editor"). `withdraw_notification('capture')` after a recording finishes. | capture_ui.py:907-918, capture_results.py |

## 5. Files

| Path | Format / rules |
|---|---|
| `$XDG_CONFIG_HOME/desktop-capture/config.json` (`XDG_CONFIG_HOME` env, else `~/.config`; empty value used verbatim) | Window preferences, written on every capture start and on close: `json.dumps(prefs, indent=2) + "\n"` via `config.tmp` + rename (umask mode). Key order: `copy_to_clipboard, open_editor, mode_preferences{record|screenshot: {directory, target}}, audio{system, microphone}, codec, color, encoder, quality, container, format` (dropdown indices) `, delay, fps, peak, bitrate, duration` (ints) `, cursor` (bool) `, encoder_backend` (`auto|cpu|vulkan`). Unknown keys are dropped by this writer. Load: only int/bool types accepted, indices range-checked; `encoder_backend` wins, else legacy `encoder` 2/3 → 2 (Vulkan). |
| same file, keys `copy_to_clipboard` (default true), `open_editor` (default false) | `capture_settings.set_value`: read-merge one key, `mkstemp(prefix=".config-")` + `json.dumps(data, indent=2) + "\n"` + replace (mode 0600 from mkstemp); non-bool → ValueError. Read at use time (`get` → type-checked bool). Global Settings and the window both write. |
| `$XDG_CONFIG_HOME/desktop-capture/shortcuts.json` | `{"record": "A-F9", "screen": "Print", "area": "C-Print", "window": "S-Print", "workspace": "A-Print"}` (normalized, `""` = disabled), indent 2 + newline, 0600. Load errors / invalid → defaults. |
| `$XDG_CONFIG_HOME/desktop-capture/backups/rc-%Y%m%dT%H%M%S%f.xml` | rc.xml before a changing apply, 0600 |
| `$XDG_CONFIG_HOME/labwc/rc.xml` (resolved symlink) | §7 |
| Output folders | defaults `GLib.get_user_special_dir(PICTURES)/Screenshots` and `…(VIDEOS)/Recordings` (fallback `~`); must be absolute after `expanduser` ("Choose an absolute save folder."); per-mode folder/target remembered. Names `Screenshot_%Y-%m-%d_%H-%M-%S_%f.png|jpg`, `Recording_…mkv|mp4`. **Never overwrite**: screenshots `open(path, 'xb')` + chmod 0600 (FileExistsError on collision); recordings: ffmpeg `-n` writes `.recording-XXXXXXXX.<ext>` (mkstemp name, unlinked first) under umask 077, then `os.link` → final name (fails if it exists), temp unlinked; on error the partial path is reported ("Partial recording: …"). Editor Save replaces the file atomically with mode 0600 (`.<name>-XXXX` temp). |
| `$XDG_CACHE_HOME/tessel-dock/palette.json` | companion_theme live palette (every window) |

Images: screenshots are SDR sRGB, **PNG with an embedded lcms2 sRGB ICC profile** (`iCCP` "ICC Profile", 588-byte `ImageCms.createProfile('sRGB')`, creation date = now UTC) or **JPEG quality 95, 4:4:4, same ICC (APP2)**. Workspace composition: outputs placed at portal `position`, resized with Lanczos to logical `size`, RGB canvas ≤ 100 Mpx, PNG + ICC.

## 6. External commands
| Command | Use |
|---|---|
| `ffmpeg` (zscale, tonemap, png, libopenh264/libx265/libsvtav1, `*_vulkan`) | screenshot conversion (`-f rawvideo … -vf <color_filter> -frames:v 1 -c:v png -f image2pipe pipe:1`, 45 s), encoder probes (one frame, `-f null -`, 15 s), recording (`-n`, `-use_wallclock_as_timestamps 1`, pulse inputs, `-fps_mode cfr`, tags; stdin non-blocking writes; SIGINT then kill on error; 30 s finish timeout). Exact argument lists: capture_media.py `color_filter / raw_input / encoder_arguments / recording_command / encoder_probe_command`. |
| `ffprobe` | only `--check` presence |
| `pactl --format=json list sources` (3 s), `pactl get-default-sink`, `pactl get-default-source` | audio source list (`.monitor` = system audio), presets |
| `wl-copy --type image/png` | clipboard (15 s, forks to serve); fallback GTK clipboard + 120 s app hold |
| `labwc --reconfigure` | after shortcut apply (check, 10 s; failure rolls back) |
| default `inode/directory` handler `org.tessel.Files.desktop` | `<executable> --select <uri>` (new session, std streams /dev/null) |
| default handler for the file's type | `Gio.AppInfo.launch_default_for_uri_async` with the display's launch context |
| `libcapture-pipewire.so` (dlopen) | `capture_open(fd, node, fps=120)`, `capture_read`, `capture_release`, `capture_close`; struct `{u32 width,height,size,depth; double received (CLOCK_MONOTONIC, ≤ PTS); char pixel_format[24], transfer[24], primaries[24]}` |
| `libvulkan.so.1` (dlopen) | `encoding_devices(codec)`: device indices with `VK_KHR_video_queue`, `VK_KHR_video_encode_queue`, `VK_KHR_video_encode_<h264|h265|av1>` |
| `libxkbcommon.so.0` (dlopen) | shortcut key validation (`xkb_keysym_from_name(.., CASE_INSENSITIVE)`, `xkb_keysym_get_name`) |

## 7. labwc keyboard shortcuts (capture_shortcuts.py)
- Keys: `record` "Start / stop recording" → `/usr/local/bin/tessel-capture --toggle-recording`; `screen` "Current screen", `area` "Custom area", `window` "Current window", `workspace` "Current workspace" → `/usr/local/bin/tessel-capture --capture <key>`. OWNED commands = these + `/usr/local/bin/tessel-capture --screenshot` (legacy Print binding, migrated).
- `normalize`: split on `+`/`-`; modifiers ctrl/control/c→C, alt/a→A, shift/s→S, super/win/w→W (else "Use Ctrl, Alt, Shift or Super plus a key, for example Alt+F9."); `printscreen`/`print screen`→Print; keysym must exist and not be a bare modifier ("Unknown shortcut key: {key}"); canonical name, single chars lower-cased; order `C-A-S-W-key`. `display`: `C→Ctrl, A→Alt, S→Shift, W→Super` joined by `+`.
- `shortcut(rc_bytes, values)`: duplicates → "Each capture action needs a different shortcut."; root must be `labwc_config` with exactly one `<keyboard>` ("Expected a single labwc keyboard section."); comments masked; each `<keybind>` in the section: exactly one `<action name="Execute">` whose `command` attribute or `<command>` text (stripped) is OWNED → removed; any other binding whose normalized key is one of ours → "{shortcut} is already assigned in labwc. Choose another shortcut."; new bindings `    <keybind key="K"><action name="Execute"><command>CMD</command></action></keybind>` appended before `</keyboard>` (trailing whitespace replaced by `\n` + lines + `\n  `). Everything else byte-preserved; idempotent.
- `apply(values)`: resolve rc.xml, backup if changed, re-check unchanged ("Keyboard configuration changed. Please try again."), atomic write rc (mode kept) + shortcuts.json (0600), `labwc --reconfigure`; any failure restores both files.
- The window's "Keyboard shortcuts" card: entries per key (display form), "Defaults", "Apply shortcuts" → "Keyboard shortcuts updated." and refreshes the app-menu shortcut list.

## 8. UI (GTK4, gtk4-layer-shell)
- `CaptureWindow(Gtk.ApplicationWindow)` class `capture-window`, default 650×760, headerbar title "Capture" (`suite-headerbar`), window title "Screenshot"/"Screen Recorder" by mode; CSS at APPLICATION+31 (`.subtitle .card .record-status .capture-mode button(:checked) .preview .error`); `companion_theme.attach`. Mode switch `.linked.capture-mode` toggle buttons "Screenshot" / "Screen recording". Cards (`.card`): Area dropdown (screenshot: workspace·all screens / screen / window / area, default screen; recording: screen / window / area), hint, Delay (0–3600), Include pointer, HDR peak (100–10000 step 100, 1000); screenshot card: Image format PNG/JPEG, arm button "Enable/Disable Print Screen while locked" + hint, the two `capture_settings` switch rows; recording card: codec H.264/H.265/AV1 (HDR forces ≥H.265), Color SDR/HDR, FPS 1–120 (30), Encoder Automatic/Software/Vulkan, Quality High/Balanced/Small (18/23/28; default Balanced), Bitrate 0–200, Container MKV/MP4, Stop after 0–86400, System audio / Microphone dropdowns (pactl); Save-to entry + folder chooser; Keyboard shortcuts card. Footer: main button `.suggested-action` "Take screenshot"/"Start recording", Cancel / "Stop & save", status label `.record-status` (`.error`), preview `Gtk.Picture.preview` (180 px), result row Copy image / Edit… / Open file / Open folder (only for the current mode's last result; recordings: open only). App menu (`suite-app-menu`, "Screenshot & Screen Recorder", icon camera-photo, shortcuts = saved desktop shortcuts + editor shortcuts).
- Close while busy/armed → hide (keeps running). Otherwise save prefs, release, disarm.
- Flow: `prepare` (save prefs, hide unless quick/`hide_window=False`, popdown popovers, `prepare-capture`, 250 ms) → `choose_target`: window/workspace first resolve the pointer monitor (PointerMonitor, namespace `suite-output-probe`); workspace = one portal session per connected monitor (CaptureSelection target = connector); screen = `ScreenSelection` probe (1×1 layer surface, namespace **`tessel-capture-selection`**, OVERLAY, exclusive −1, keyboard NONE, empty input region; `enter-monitor` → monitor; 3 s timeout = cancel); area = per-monitor fullscreen layer surfaces (`tessel-capture-selection`, OVERLAY, keyboard EXCLUSIVE, class `capture-selection`, CSS at USER+100, crosshair, 38 % dim with clear selection + accent outline, drag ≥2 px, Escape/right-click cancel, 120 s timeout); window = portal source type 2 with the picker. → countdown (≥ 0.35 s, "Capturing in {seconds} seconds…") → feeds (PipeWire, after the delay) → first frame (area → pixel crop `pixel_rectangle`) → screenshot or Recorder.
- Screenshot result: `last_file`, preview, release sources, present unless quick, "Saved {path}", auto-copy (if enabled; never for lock shots), notification "Screenshot saved and copied"/"Screenshot saved", optional editor.
- Cancel (portal code 1 or selector cancel) → "Capture cancelled.", no notification, window not presented. Errors: quick capture → GNotification "Capture failed"; else present the window with the error (never for lock shots).
- Recording: tray states preparing → recording → stopping → idle; "Recording · {detail}" elapsed mm:ss; saved → notification "Recording saved" (recording hints/actions); closing the window keeps recording.
- Lock screen ("Enable Print Screen while locked"): arm = workspace sessions without pointer, read one frame from each, keep sessions (no feeds) → `armed_sources`, "Ready for Print Screen on the lock screen. Keep Capture running; it can be minimized."; `locked_shot(not_before)` reopens each armed session with a new `OpenPipeWireRemote` (no new consent), rejects frames received before `not_before`, saves without clipboard/notification/preview. Disarm closes the sessions ("Locked screenshot access is disabled.").
- Editor `EditorWindow` "Edit Screenshot" 980×700: tools Pen/Arrow/Rectangle/Text/Crop (toggle group), colour `#e5484d`, size 1–40 (4), text entry, Undo/Copy/Save As buttons, Save `.suggested-action` in the header; Ctrl+Z, Ctrl+C, Ctrl+S, Ctrl+Shift+S, Escape (ignored while an editable has focus); one editor per path (raised). Render: cairo ARGB32, crop = last crop op, PNG, JPEG re-encode for .jpg/.jpeg.
- `capture_settings.build_page(path)`: `Gtk.Box` (vertical, spacing 14, margins 20), heading "Screenshots" `.title-2`, the two switch rows (labels + `.subtitle .dim-label` hints), `page.switches`.

## 9. Opening results (capture_results.py)
`open_saved(app, uri, reveal)`: non-local URI → "The saved capture must be a local file."; missing → "The saved capture no longer exists: {path}" (GNotification `capture-open-error`, `dialog-error-symbolic`); hold app during async launch. Reveal: default `inode/directory` app `org.tessel.Files.desktop` → `<exe> --select <uri>`; otherwise `Gtk.FileLauncher.open_containing_folder`. Open: `launch_default_for_uri_async` with the display's launch context. `copy_saved`: decode the file (PNG/JPEG) → PNG → clipboard.

## 10. i18n
All UI strings through `suite_i18n.translate` (shared catalogs `shared/locale/<code>.json`), including the `RecordingOptions.validate`, portal, shortcut and editor messages listed above.

## 11. Lock-screen interplay
Lock screen never launches Capture: it only calls `lock-shot(d)` (NO_AUTO_START) when Capture is already running and armed. Lock shots never touch the clipboard, notifications or the window.

## 12. Installers (not ported, documented only)
- `install_session.py [--apply]` (desktop user): migrates/adds the rc.xml shortcuts (`shortcut(rc, load())`), updates tessel-lock files if installed, refuses when locked, backups under `~/.local/state/tessel/backups/capture-<stamp>`, `labwc --reconfigure`, rollback.
- `install_portal.py [--system] [--apply]`: installs `/usr/local/libexec/tessel-capture-portal-wlr` (needs `XDPW_CHOOSER_SESSION`) and the user drop-in `~/.config/systemd/user/xdg-desktop-portal-wlr.service.d/50-tessel-capture.conf` (`ExecStart=` override); refuses while a screencast session is active; restarts only the portal.
