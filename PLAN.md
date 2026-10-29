# nobara-de-rust — Rust rewrite of the ~/btmanager desktop suite

Goal: lower memory use and faster startup. Baseline (2026-10-29): resident Python
services ≈ 600 MB RSS total (waydock 74, brightness 47, wayshell 46, notifications 32,
power 31, network 31, battery 30, OSK 27, keep-awake 27, audio loopbacks 20, …).

Source of truth for behaviour: `~/btmanager` (Python, GTK4/PyGObject, labwc). It stays the
live desktop until each Rust component reaches parity and is swapped in.

## Rules
1. **Drop-in parity.** A Rust component keeps the exact external contract of its Python
   twin: CLI flags, D-Bus names/interfaces/signals, sockets, config/state file paths and
   formats, systemd unit names, .desktop ids, CSS class names, i18n strings. The contract is
   written first, in `contracts/<component>.md`, and tests are written against it.
2. **Shared assets are reused, not copied.** Translations: `shared/locale/<code>.json`
   (31 languages) read at runtime. Theme/palette logic is ported once into `suite-gtk`.
3. **Stack.** gtk4-rs (`gtk4` crate, feature v4_14+), `gtk4-layer-shell`, `zbus` (async,
   blocking only off the main thread), `serde`/`serde_json`, `glib` main loop (no tokio on
   GUI threads; use `glib::spawn_future_local` + `gio` async). No libadwaita unless the
   Python version uses it. Release profile: `lto = "thin"`, `strip = true`, `opt-level = 3`.
4. **Testing.** Unit tests for logic; UI tests only in a private headless labwc (recipe in
   `tools/headless.md`). Never touch the live session (wayland-0 in /run/user/1000), never
   restart labwc, never run the top-level Python `test_actions.py`.
5. **Swapping in.** Each component gets an install step (`tools/install.py --only X`) that
   installs the binary next to the Python one and flips the systemd unit / launcher, with a
   `--rollback`. Swap only after parity tests pass; measure RSS and startup before/after.
6. **Python path shims.** Global Settings, battery/brightness managers and way-displays-gui import
   modules from `~/.config/waybar/scripts/<app>/`, and keybinds / `desktop-suite-action` call the
   `.py` paths directly (power-button check even greps `power_manager.py` for `--power-button`).
   Until every consumer is ported, a swapped component keeps a Python shim at the old path that
   execs the Rust binary (and keeps importable modules that others import).
   `desktop-suite-action` also greps component sources for quoted flags (e.g. `'--menu'`): a
   swap must keep the shim (containing those flag strings) or update that script in the same step.
   Global Settings imports the Python UIs of many applets (often private methods) and ~25
   components import waydock modules, so those Python files stay installed until wave 4.
7. **Wayland protocols** not covered by GTK go in a `crates/suite-wayland` crate
   (wayland-client + wayland-protocols): zwlr_foreign_toplevel_management_v1 v3, ext-workspace-v1,
   zxdg_output_v1, ext_foreign_toplevel_list_v1, ext_image_copy_capture_v1 /
   ext_image_capture_source_v1, plus ext_background_effect_v1 and the private
   waydock_blur_manager_v1, which must bind on GTK's own wl_display (from gdk4-wayland).
8. **netman** has no mature Rust libnm: raw NetworkManager D-Bus + secret agent in zbus, and C FFI
   (libnm / VPN editor plugins) only where libnm-only logic is unavoidable.
9. `wayshell.service` ExecStart is stale; the `palette.conf` drop-in is what actually runs it —
   the swap edits that drop-in.
10. Every new user-facing option still gets a Global Settings entry + 30 translations.

## Layout
- `crates/suite-core` — i18n, paths, service settings, single-instance, logging, config IO.
- `crates/suite-gtk` — gtk4_utils, theme/palette CSS (dock_style, companion_theme),
  tray_popup, popup_placement, app_menu, adjustment OSD, wallpaper helpers.
- `components/<name>` — one binary crate per Python component (same name).
- `contracts/<name>.md` — external contract of the Python component.

## Order (waves) and status
| Wave | Component | Py LOC | Status |
|---|---|---|---|
| 0 | suite-core, suite-gtk (from shared/) | 4249 | done (wallpaper_*, application_audio, capture_selection deferred) |
| 1 | keep-awake | 1239 | ported, tests pass, not swapped |
| 1 | battery-manager | 1340 | ported, tests pass, not swapped |
| 1 | power-manager | 4564 | ported, tests pass, not swapped |
| 1 | notification-manager | 2362 | ported, tests pass, not swapped |
| 1 | on-screen-keyboard | 1068 | ported, tests pass, not swapped |
| 1 | audio-manager (loopback service first) | 4046 | ported, tests pass, not swapped |
| 2 | brightness-manager | 3797 | porting |
| 2 | network-manager (netman) | 6934 | |
| 2 | authentication-manager (polkitman) | 663 | porting |
| 2 | calendar / weather / media / discovery / tiling / session-manager | ~8.7k | porting (all but session-manager) |
| 2 | way-displays, language-manager, session-startup | ~2.9k | |
| 2 | bluetooth (btmanager.py, btpopup.py, bttransfer.py) | — | |
| 3 | wayshell | 4077 | |
| 3 | waydock | 14739 | |
| 4 | apps: calculator, text-editor, picture-viewer, system-monitor, hdr-calibration, capture-manager, file-manager | ~19k | |
| 4 | global-settings, welcome | ~9.4k | |
| 5 | lock-screen, login-manager (PAM, security review) | ~5.2k | |
| 5 | home-assistant-screen | 1454 | |
