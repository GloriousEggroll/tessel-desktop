# Tessel Shell desktop layer — parity (Rust `desktop` module vs Python `tessel-shell`)

Source: `~/tessel-bluetooth/tessel-shell/tessel-shell` DesktopView (194-3748) + desktop half of DesktopApp,
`desktop_icon_layout.py`, `desktop_workarea.py`, `labwc_launch_environment`, launch half of
`tessel-dock/app_catalog.py`. Tests: unit (`cargo test --lib desktop::`), `tests/desktop_launch.rs`,
`tests/desktop_workarea_bus.rs` (private `gio::TestDBus`), `tests/desktop_headless.rs` via
`tests/run_headless_desktop.sh` (private nested labwc, 2 outputs, GIO_USE_VFS=local, temp HOME/XDG).

| Contract item | Status | Test |
|---|---|---|
| Layer BACKGROUND, 4 anchors, set_monitor, exclusive −1, keyboard ON_DEMAND, namespace `desktop-grid-<conn\|index>`, name `desktop-grid-win-<conn\|index>`, `.tessel-shell-desktop`, `#main-overlay`, `.desktop-grid`, transparent CSS, surface = physical monitor size | done | headless `layer_surfaces_and_workareas` |
| do_activate idempotent | done | headless |
| Icon layout maths (inset 12, cells column-major, next_slot, constrain, shift) | done, 1:1 port of test_desktop_icon_layout | unit `icon_layout::tests` |
| Workarea parser (64 KiB chars, version int 1, ≤128 monitors, names, int rects ≤2^30, w/h, containment) | done | unit `workarea::tests::rejects_malformed_payloads` |
| Workarea client: DO_NOT_AUTO_START\|DO_NOT_LOAD_PROPERTIES, GetWorkareas 2000 ms NO_AUTO_START, owner tracking, owner-only signals, serial rule, owner loss clears | done | unit `serial_owner_and_sender_rules`; bus `workarea_client_follows_the_owner_on_a_private_bus` |
| Icons avoid per-monitor panels, bar moving edges shifts arrangement (known areas only), cross-monitor drop target from unsnapped point, clamp per target workarea, removing reservation keeps placements | done | headless (port of test_desktop_workarea) |
| desktop_view_order.json: Python `json.dump` default separators, skip-unchanged writes, global coords | done | unit `byte_formats_match_python_json_dump`; headless efficiency (mtime unchanged after 100 refreshes) |
| wallpapers.json indent=2 merge-write keeping `_view_prefs` + unknown keys, colour `RGBA.to_string()`, Color Only → null | done | headless `wallpapers_and_mpvpaper` (byte-exact), settings window |
| Texture cache (realpath, ino, size, mtime_ns), weak, shared, released on destroy; one CSS provider per view | done | headless efficiency |
| Fit map + centering | done | unit `fit_map_and_video_detection`, headless |
| mpvpaper argv exact, labwc env, terminate→2 s→kill, notification when missing | done (FAKE mpvpaper only) | `mpvpaper_is_supervised_with_the_labwc_environment`, headless |
| Layer bounce BACKGROUND→BOTTOM→BACKGROUND at idle/+120/+300 ms | done | headless (3 bounces, ends BACKGROUND) |
| labwc environment ($VAR/${VAR}, every `~`, >1024 skipped, environment.d/*.env sorted, universal newlines) | done | unit (values verified against the Python function) |
| systemd-run argv exactly (§6), SERVICE_ENV stripped, --setenv names, Type=exec + --working-directory for processes, forking + helper for files/URIs/desktop ids, stderr error text | done (FAKE systemd-run) | `launches_leave_the_cgroup_through_systemd_run` |
| `_TransferredLaunchContext` + `_run_launch_helper` (startup token, DISPLAY) | done (in-binary helper `tessel-shell --launch-file\|--launch-desktop\|--open-uri <target>`) | `launch_helper_runs_the_entry_with_the_transferred_token` |
| Terminal candidates (code order: $TERMINAL, xdg-terminal, ptyxis, kgx, gnome-terminal, konsole, xfce4-terminal, tilix, alacritty, kitty, wezterm, xterm, ghostty), flags, HOME cwd, fallback `[exe]` | done | unit + launch test |
| xdg-settings browser lookup async (fileops), failures notify / confirm popover | done | headless efficiency |
| Menus: background 4 sections, Configure Displays only with tessel-displays; item (Trash: Open/Empty Trash), protected disables; multi menu | done | headless `interaction` |
| Protected items, trash detection, `Trash ({value1})` + user-trash(-full) | done | headless |
| Keyboard: Esc, Delete (confirm), Ctrl+C/X/V (blocked with protected), Ctrl+H (shortcut), arrows + Shift (second `navigate_selection`) | done | headless |
| Clipboard: gnome + uri-list providers, wl-copy ×2, paste prefers gnome, async read | done (FAKE wl-copy) | headless |
| Double click 350 ms / 8 px | done | headless `release_at` |
| Rename: .desktop edits Name=, `/ . ..` rejected, position reserved before async rename, rollback on failure | done | headless |
| Hide Icons / Show Hidden mirrored on all grids, persisted in `_view_prefs`, reveal reloads | done | headless `visibility` (port of test_desktop_visibility) |
| Refresh coalescing 100 ms, deferred during drag/rubber band, primary grid first | done | headless efficiency |
| Unchanged refresh reuses widgets + keeps selection; removed/changed (ino, mtime_ns, size, mode) released | done | headless efficiency |
| Paste/drop share async conflict prompt + worker copy, apply-to-all, failure dialog, progress toast cancel/remove | done (fileops API) | headless efficiency |
| Monitor items-changed + notify::geometry, 250 ms debounce, rebuild only on (x,y,w,h) change | done | headless efficiency |
| Sort by name (display name, lowercase) / by type groups, vertical fill | done | headless + workarea |
| Wallpaper Settings window (620×610, classes, per-monitor controls, Apply merge, Escape/close) + shared WallpaperChooser | done | headless `interaction` |

## Deliberate deviations / gaps
- Double click: Python launches on the 2nd *press* (n_press≥2) **and** again on the 2nd release
  (two launches). Rust launches once (press), resetting the release detector.
- Drag movement uses the pointer position when available, else the gesture offsets (headless has no
  pointer); Python only polls the pointer. Watchdog timer (which practically never fired) not ported.
- Item positions are tracked by the view (`Item.pos`) because an unrealized `GtkFixed` reports (0,0).
- Python's double `navigate_selection` definition: only the effective (second) one is ported.
- Key controllers merged into one handler with Python's effective order (arrows, Esc/Delete, Ctrl+C/X/V).
- Not measured: RSS/startup belong to the whole daemon (lead).
