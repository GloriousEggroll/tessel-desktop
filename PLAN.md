# nobara-de-rust — Rust rewrite of the ~/tessel-bluetooth Tessel

Goal: lower memory use and faster startup. Baseline (2026-10-29): resident Python
services ≈ 600 MB RSS total (tessel-dock 74, brightness 47, tessel-shell 46, notifications 32,
power 31, network 31, battery 30, OSK 27, tessel-keep-awake 27, audio loopbacks 20, …).

Source of truth for behaviour: `~/tessel-bluetooth` (Python, GTK4/PyGObject, labwc). It stays the
live desktop until each Rust component reaches parity and is swapped in.

## Rules
1. **Drop-in parity.** A Rust component keeps the exact external contract of its Python
   twin: CLI flags, D-Bus names/interfaces/signals, sockets, config/state file paths and
   formats, systemd unit names, .desktop ids, CSS class names, i18n strings. The contract is
   written first, in `contracts/<component>.md`, and tests are written against it.
2. **Shared assets are reused, not copied.** Translations: `shared/locale/<code>.json`
   (31 languages) read at runtime. Theme/palette logic is ported once into `tessel-gtk`.
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
6. **Python path shims.** Global Settings, battery/brightness managers and tessel-displays import
   modules from `~/.config/waybar/scripts/<app>/`, and keybinds / `tessel-action` call the
   `.py` paths directly (power-button check even greps `power_manager.py` for `--power-button`).
   Until every consumer is ported, a swapped component keeps a Python shim at the old path that
   execs the Rust binary (and keeps importable modules that others import).
   `tessel-action` also greps component sources for quoted flags (e.g. `'--menu'`): a
   swap must keep the shim (containing those flag strings) or update that script in the same step.
   Global Settings imports the Python UIs of many applets (often private methods) and ~25
   components import tessel-dock modules, so those Python files stay installed until wave 4.
7. **Wayland protocols** not covered by GTK go in a `crates/wayland` crate
   (wayland-client + wayland-protocols): zwlr_foreign_toplevel_management_v1 v3, ext-workspace-v1,
   zxdg_output_v1, ext_foreign_toplevel_list_v1, ext_image_copy_capture_v1 /
   ext_image_capture_source_v1, plus ext_background_effect_v1 and the private
   waydock_blur_manager_v1, which must bind on GTK's own wl_display (from gdk4-wayland).
8. **netman** has no mature Rust libnm: raw NetworkManager D-Bus + secret agent in zbus, and C FFI
   (libnm / VPN editor plugins) only where libnm-only logic is unavoidable.
9. `tessel-shell.service` ExecStart is stale; the `palette.conf` drop-in is what actually runs it —
   the swap overrides it with `zz-tessel-rust.conf` (sorts after `palette.conf`, which stays untouched).
10. Every new user-facing option still gets a Global Settings entry + 30 translations.

## Layout
- `crates/core` — i18n, paths, service settings, single-instance, logging, config IO.
- `crates/gtk` — gtk4_utils, theme/palette CSS (dock_style, companion_theme),
  tray_popup, popup_placement, app_menu, adjustment OSD, wallpaper helpers.
- `components/<name>` — one binary crate per Python component (same name).
- `contracts/<name>.md` — external contract of the Python component.

## Order (waves) and status
| Wave | Component | Py LOC | Status |
|---|---|---|---|
| 0 | tessel-core, tessel-gtk (from shared/) | 4249 | done (wallpaper_*, application_audio, capture_selection deferred) |
| 1 | tessel-keep-awake | 1239 | ported, tests pass, not swapped |
| 1 | tessel-battery | 1340 | ported, tests pass, not swapped |
| 1 | tessel-power | 4564 | ported, tests pass, not swapped |
| 1 | tessel-notifications | 2362 | ported, tests pass, not swapped |
| 1 | tessel-keyboard | 1068 | ported, tests pass, not swapped |
| 1 | tessel-audio (loopback service first) | 4046 | ported, tests pass, not swapped |
| 2 | tessel-brightness | 3797 | ported, tests pass, not swapped |
| 2 | tessel-network (netman) | 6934 | ported, gaps closed via libnm, not swapped |
| 2 | tessel-auth (polkitman) | 663 | ported, tests pass, not swapped (swap with user present) |
| 2 | calendar / weather / media / discovery / tiling / tessel-session | ~8.7k | ported except tessel-session; not swapped |
| 2 | way-displays, tessel-language, tessel-session-startup (+ tessel-session) | ~3.7k | ported, not swapped |
| 2 | bluetooth (tessel-bluetooth.py, btpopup.py, bttransfer.py) | — | ported, not swapped |
| 3 | tessel-shell | 4077 | ported (+prompt, capture-picker, theme); install.py entries ready (tessel-shell, tessel-prompt, tessel-capture-picker), not swapped (swap with user present) |
| 3 | tessel-dock | 14739 | ported, tests + 0 px screenshot parity; install.py entry ready, not swapped (swap with user present) |
| 4 | calculator | 998 | ported (Python-decimal-compatible engine), 24 unit + 9 headless pass, install.py `--system` entry ready (root only, never escalates), not swapped |
| 4 | tessel-text-editor | 1637 | ported (sourceview5 via .cargo/config.toml soname link), 31 unit + 21 headless pass, install.py `--system` entry ready (root only, never escalates), not swapped |
| 4 | tessel-pictures | 2320 | ported (image + kamadak-exif, lcms via dlopen), 15 unit + 10 headless pass, install.py `--system` entry ready (root only, never escalates), not swapped |
| 4 | tessel-system-monitor | 1519 | ported (/proc+/sys instead of psutil), 28 unit + 15 headless pass, install.py `--system` entry ready (root only, never escalates), not swapped |
| 4 | tessel-hdr-calibration | 926 | ported (hdr-pattern stays C), 19 unit + 6 headless pass, install.py entry ready, not swapped |
| 4 | tessel-capture (GUI + CLI; portal and capture_pipewire.c stay C) | 3420 | ported, 30 unit + 21 headless pass (fake portal/PipeWire; no real stream read), install.py `--system` entry ready (root only, never escalates), not swapped |
| 4 | tessel-files (tessel-files; crates tessel-files-ops/tessel-files-model/tessel-files-widgets + app) | 8448 | ported, 125 unit + 90 app headless + 25 widget headless + 6 private-GVfs checks pass; startup 343 vs 426 ms, idle PSS ≈47 vs 68 MB, after browsing ≈85 vs 115 MB; swap notes in PARITY.md, install.py `--system` entry ready (root only, never escalates), not swapped |
| 4 | tessel-settings | 8708 | ported (shell + gs-base/gs-applets/gs-desktop/gs-system; embeds the Rust applet crates), 94 unit + 113 headless checks, 859 files + 12 page scenarios byte-identical to Python, 0 px screenshot; install.py entry ready, not swapped |
| 4 | welcome | 590 | ported, 6 unit + 11 headless pass, 0 px screenshot; install.py entry ready, not swapped |
| 5 | tessel-locking (lock_config, login_model, lock dashboard widgets) | ~1.4k | done (moved from gs-desktop + widgets; shared by the three below) |
| 5 | tessel-lock (+ Rust `tessel-lock-pam` PAM helper) | ~3.1k | ported, security-reviewed (5 low fixes), 26 unit + 23 headless pass (incl. real ext-session-lock crash test in private labwc, fake libpam), install.py entry ready, not swapped (swap with user present) |
| 5 | tessel-login (Tessel Login greeter, root appearance helper) | ~1.7k | ported, security-reviewed (2 low parity fixes), 39 unit + 15 headless pass (fake greetd), install.py `--system` entry ready (root only, never escalates), not swapped (swap with user present) |
| 5 | home-assistant-screen (Kasumi luna-clock, voice-controls) | 1454 | ported, security-reviewed (5 fixes), 12 unit + 11 headless pass (fake HTTP/scheduler), install.py entry ready (`--target-root <Kasumi home>`, aarch64 builds only), not swapped |
