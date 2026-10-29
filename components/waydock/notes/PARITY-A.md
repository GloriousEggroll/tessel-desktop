# PARITY-A — W-a core model (`components/waydock/src/core/`)

Run: `TMPDIR=/run/user/1000/cbt/rust-waydock/a cargo test -p waydock --lib core::`
→ 132 passed, 1 ignored (the ignored one is the child half of a subprocess test).
Python parity tests run `python3 -I -B` against `~/btmanager/waydock` (read-only) with
HOME/XDG in a `$TMPDIR` directory; they skip silently if the sources are missing.

| Contract item | Done | Tests |
|---|---|---|
| §8 DockConfig: all 60 fields, names, defaults, dataclass order | yes | `config_tests::field_names_follow_dataclass_order`, `legacy_config_*` |
| §8 validation order + exact `ValueError` texts | yes | `config_tests::error_messages_match_python_text`, `error_texts_match_python_from_dict` (Python parity, 22 cases), all type/range rejection tests |
| §8 migrations: frost strength ← transparency (default 20), `media` stripped, Media inserted before custom tray, missing groups appended | yes | `legacy_tint_migrates_*`, `group_layout::tests::media_migrates_*`, `missing_groups_gain_defaults_*` |
| §8 save bytes (`{"version":1,…}`, indent 2, `ensure_ascii=False`, trailing `\n`, mode 0600, atomic `.dock-settings-*` + fsync + rename) | yes | `saved_bytes_match_python_save_config` (byte-identical, 4 fixtures incl. unicode, int `hover_scale`), `round_trip_preserves_pin_order_*` |
| §8 load: never writes, missing → defaults + App Center/browser/Cairo pins, >1 MiB rejected, malformed preserved + `error` text | yes | `existing_empty_pins_*`, `bad_config_is_reported_and_preserved`, `load_error_texts_match_python` (JSON + UTF-8 error texts via `pyjson_loads`) |
| Cairo import (configparser rules, Order sort, installed only, never Exec) | yes | `cairo_import_preserves_order_*`, `cairo_import_matches_python_configparser` (parity, 17 fixtures) |
| derived: `effective_*`, `uses_separate_app_dock`, `for_app_dock`, `for_monitor` (no aliasing) | yes | `for_app_dock_uses_app_dock_appearance`, `effective_monitor_config_does_not_mutate_*`, `dock_and_bar_modes_*` |
| group_layout (LAYOUT_GROUPS, defaults per mode×display, app dock layout) | yes | `group_layout::tests::*` (test_group_positions port; GUI separator tests of test_group_layout.py belong to the UI) |
| §10.9 dock model + utility routing | yes | `model_tests::*` (test_dock_model + test_utility_groups, all cases) |
| §10.10 HoverState | yes | `model_tests::sensor_to_dock_*` … `configuration_rebuild_and_zero_delay` |
| §10.14 focused title | yes (logic) | `focused_title::tests::*` (sequences from test_focused_title GUI tests) |
| §7 tray identities | yes | `tray_prefs::tests::*` |
| §7 / §10.6 panel actions argv per applet/edge, env names only, missing script/systemd-run failure, no fallback | yes | `panel_actions::tests::*` (all 15 Python cases) + `fake_systemd_run_receives_exact_argv_and_reports_failure` (real Gio spawn of a $TMPDIR script) |
| §5 / §10.7 app launch argv (`/usr/bin/systemd-run … --unit=app-waydock-<hex>.service … -- python3 <runtime>/app_catalog.py <mode> <target>`), env via environment, service vars stripped, token in `WAYDOCK_LAUNCH_STARTUP_ID`, failure cancels notification, never direct spawn | yes | `app_catalog_tests::*launch*`, `launch_argv_and_environment_match_python` (parity of argv + env list for `_launch_separately`, `launch_process`, `launch_command`), `fake_systemd_run_receives_unit_env_names_and_values_via_environment` |
| launch helper `__main__` → `run_launch_helper` / `launch_helper_main` (`_TransferredLaunchContext` via a registered GAppLaunchContext subclass) | yes | `transferred_context_returns_token_and_x11_display_only`, `helper_missing_desktop_or_invalid_mode_*`, `helper_launches_synthetic_desktop_file_with_token` (child process, dead session bus, synthetic $TMPDIR script) |
| app catalog: resolve/search/fuzzy/categories/LaunchHistory | yes | `app_catalog_tests::*` (all test_app_catalog cases) |
| §10.21 trash: top-level only via Gio, cancel, duplicate blocked, count via metadata | yes | `trash::tests::*` (real Gio on a `file://` $TMPDIR root; `trash:///` is never touched) |
| §5 monitor naming/selection | yes | `monitors::tests::*` |

## Honest gaps
- App lifetime with a real user systemd (test_app_lifetime / test_launch_lifetime) is not run:
  argv/env/unit naming are covered with a fake `systemd-run`; survival across dock stop/SIGKILL is
  systemd's behaviour and needs the opt-in Python test.
- Trash NOT_FOUND-is-ignored branch is implemented but not exercised (needs a race or GVFS fake).
- `json.loads` corner: lone surrogates become U+FFFD, NaN/Infinity become NUL-prefixed sentinel
  strings (every DockConfig check then rejects them with Python's message), >64-bit ints become floats.
- configparser port covers the rules Cairo launchers use (no interpolation, `strict=False`,
  `[DEFAULT]` fallback); exotic forms (inline comments are off in Python too) untested beyond the parity fixtures.
- OSError texts from `save` use the config path (Python sometimes names the temporary file).
- Failure strings of panel actions / launches are English like Python (the dialog shows them raw).

## For the integrator
- `DockModel`/`utility_partition`/`FocusedTitle` are generic over `model::WindowRecord`; implement
  it for `crate::wl::windows::Window` (fields id/app_id/title/activated/minimized/parent_id) — not
  done here to avoid depending on another agent's in-flux type.
- Spawning: production uses `spawn::GioSpawner`; pass `Launcher::default()` /
  `PanelActions::new(show_error)`. GTK launch contexts: `gdk::AppLaunchContext` upcast to
  `gio::AppLaunchContext` implements `LaunchContext`.
- `ConfigStore::load` returns `DockConfig` and sets `store.error` (Python shape).
