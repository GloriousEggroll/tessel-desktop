# waydock — Rust port parity

Port of `~/btmanager/waydock` (contract: `contracts/waydock.md`). Library `waydock` + binary `waydock`
(≈36k lines of Rust including tests). The binary replaces only the `waydock.service` process. The Python
modules that other components import stay installed (contract §9).

The detailed contract item → status → test tables are in `notes/` (one file per porting area):

| Area (contract §12) | Module | Table |
|---|---|---|
| W-a core model: DockConfig/ConfigStore, group layouts, dock model + HoverState, tray prefs, panel actions, app catalog/search/launch argv, focused title, trash, monitor naming | `src/core/` | `notes/PARITY-A.md` |
| W-c D-Bus: org.waydock.Settings, SNI watcher/host, dbusmenu, sd_notify readiness; status sources (streams, wpctl, BlueZ, keep-awake, discovery, capture, media) | `src/dbus/`, `src/status/` | `notes/PARITY-B.md` |
| W-b Wayland: in-process foreign-toplevel/workspaces (suite-wayland), fullscreen guard (x11rb), show desktop | `src/wl/` | `notes/PARITY-C.md` |
| W-d appearance: palette.json, gtk.css/waydock-colors.css, window buttons (rc.xml/settings.ini/themerc/SVG), papirus, portal colour scheme, wallpaper sampling, frost, static + per-dock CSS | `src/appearance/` | `notes/PARITY-D.md` |
| W-e app, CLI, DockSurface, magnifier, hot corners, menu probe, `--check` | `src/app*`, `src/cli.rs`, `src/ui/surface*`, `ui/{magnifier,hot_corners,menu_probe}.rs` | `notes/PARITY-E.md` |
| W-f app menu, previews, per-app audio, drag & drop pins | `src/ui/{app_menu*,preview,app_audio}.rs` | `notes/PARITY-F1.md` |
| W-f indicators, tray widgets, tray expander, media controls, capture controls | `src/ui/{host,indicators,tray,media,capture_controls}.rs` | `notes/PARITY-F2.md` |
| W-g native Dock Settings (`--settings`) | `src/ui/{settings,group_layout_settings}.rs` | `notes/PARITY-G.md` |
| W-h install/swap | not done here (see below) | — |

## Contract musts

| Must | Status | Evidence |
|---|---|---|
| Tray watcher never replaces an existing one (BusNameOwnerFlags NONE); host registration with a foreign watcher | done | `b_private_bus` sni_* tests |
| sd_notify `READY=1` once, only after startup and tray host registered | done | `dbus::readiness::tests`, compare harness (READY via private NOTIFY_SOCKET) |
| config.json written byte-identically | done | `core` Python byte-parity tests, G apply-parity (about 40 edits) |
| rc.xml/settings.ini/gtk.css/env byte-preserving outside managed parts | done | `d_python_parity` (14 cases) |
| App launch argv (systemd-run transient unit → Python `app_catalog.py` helper) | done | `core::app_catalog` tests vs Python, fake systemd-run |
| Running badge only when count > 1 | done | e_headless |
| Pin stars on hover, bottom menus A→Z from the top | done | f1_ui_headless |
| Previews SCALE_DOWN | n/a | Waydock previews show an icon and a title, with no image. `SCALE_DOWN` exists only in wayshell's switcher (`wayshell_switcher_ui.py:312`). |
| `--check` JSON identical; IS_LAUNCHER flags never start a dock | done | `app::check`, e_headless |
| Screens identical to Python | done | `tests/compare`: 12/12 screenshots (dock and bar: panel, menu, after-menu, context, applets, preview) differ by 0 px |

## Tests (all passing)
- `cargo test -p waydock --lib`: 390 passed, 1 ignored (the child half of a subprocess test).
- `cargo test -p waydock --test b_private_bus --test b_python_parity --test d_python_parity --test d_portal`: all pass.
- Private headless labwc runners: `tests/run_e_headless.sh` 15/15 (`E_PYTHON=1` runs the same cases against Python), `run_f1_headless.sh` 17/17, `run_f2_headless.sh` 31/31, `run_g_headless.sh` 25/25, `run_wl_headless.sh` 7/7.
- `tests/compare/run.sh [dock bar]`: Python vs Rust in one private labwc, with fake HOMEs, fake status scripts and D-Bus activation disabled.

## Measurements (compare harness, 1280×720 headless, GSK_RENDERER=cairo, release build)
| | READY | idle RSS / PSS | after app menu RSS / PSS | menu first frame |
|---|---|---|---|---|
| dock, Python | 253 ms | 106.1 / 66.4 MB | 132.1 / 88.3 MB | 145 ms |
| dock, Rust | 166 ms | 72.1 / 42.2 MB | 97.6 / 63.7 MB | 65 ms |
| bar, Python | 252 ms | 106.9 / 66.7 MB | 133.1 / 88.9 MB | 145 ms |
| bar, Rust | 170 ms | 75.0 / 44.4 MB | 100.1 / 65.3 MB | 65 ms |

The release binary is 5.4 MB. Opening and closing the app menu repeatedly keeps RSS flat in Rust, because icon paintables are cached (test `menu_churn_does_not_grow_memory`). Most of the RSS left after the menu opens is GTK's icon and texture memory, which Python carries too.

## Known gaps / differences (details in notes/)
- Per-app audio in previews uses `pw-dump --monitor` and `wpctl set-mute`, started on demand, because there are no WirePlumber dev files to build against (F1).
- Frost textures are resized with gdk-pixbuf instead of Pillow, so pixels differ slightly. Frost textures are off by default (D).
- Error texts that come from the JSON, XML or image libraries are not reproduced word for word. These texts appear only in tooltips and logs (A, B, D).
- The X11 fullscreen probe runs in a worker thread instead of a `--probe` subprocess. Window requests are not queued, so a refused request returns false straight away (C).
- `labwc_titlebar_theme.py --apply` is not ported; session-startup keeps the Python script (D).
- Not exercised headless: Empty Trash dialog, focused-title clicks, real drag-and-drop between surfaces, modifier-key shortcuts in DBusMenu, and a real systemd app-lifetime check (app survives a dock SIGKILL).
- An unknown option with no dock running starts a dock, as Python does.

## Swap notes (W-h, for the installer — nothing was installed)
1. Install `target/release/waydock` to `~/.local/share/waydock/waydock` (beside the Python runtime). **Keep every Python module in `~/.local/share/waydock/`**: `companion_theme.py`, `app_catalog.py` (also the launch helper the Rust dock execs inside each transient unit), `window_backend.py` + `waydock-toplevel`, `dock_style.py`, `dock_config.py`, `settings_api.py`, `waydock.py` (`SettingsWindow`, embedded by Global Settings), `group_layout_settings.py`, `dock_appearance.py`, `lock_media*.py`, `application_audio.py`, `install.py`, `build_helper.py` and `labwc_titlebar_theme.py`. Also keep `icons/waydock-app-menu-symbolic.svg`.
2. Ship the locale catalogs to `~/.local/share/desktop-suite/locale/` (suite-core requirement).
3. `~/.local/bin/waydock` becomes `#!/bin/sh\nexec "$HOME/.local/share/waydock/waydock" "$@"`, at the same path. Keybinds (`--menu-hotkey`), the Waybar-compat scripts (`--desktop-status`, `--toggle-desktop`) and the `.desktop` `--settings` entry keep working unchanged.
4. In `waydock.service`, change only `ExecStart=%h/.local/share/waydock/waydock`. Keep `Type=notify`, `NotifyAccess=main`, `TimeoutStartSec=30`, `GDK_BACKEND=wayland`, `Restart=on-failure` and no `[Install]`. The labwc autostart `systemctl --user start waydock.service` line stays as the tray-readiness wait.
5. Health check before switching: `waydock --check` exits 0 (same JSON). Rollback: restore the Python ExecStart and launcher. The swap restarts the dock and tray, so do it with the user present.
