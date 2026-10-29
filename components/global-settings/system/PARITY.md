# gs-system parity (Global Settings system pages)

Python source: `~/btmanager/global-settings/` (read-only). Rust: `components/global-settings/system` (lib `gs_system`).

Tests:
- unit: `cargo test -p gs-system --lib` — 61 tests (no real commands spawned; runners/reloads injected; the passwd
  tests run fixture shell scripts written to scratch).
- GTK: `tests/ui_headless.rs` (harness = false) — 22 cases, run via
  `AGENT=gs-system components/global-settings/tests/headless.sh target/debug/deps/ui_headless-*`.
- Byte parity: `tests/python_parity.rs` + `tests/parity_driver.py` — the Python modules and the Rust port run the same
  operations on 33 rc.xml fixtures (plus `/usr/share/doc/labwc/rc.xml.all`), 9 environment texts, 5 keyboard saves and
  5 night-light configs; **859/859 output files byte-identical** (results and exception class + message). Python runs
  only inside headless.sh (fake HOME, `python3 -B -s`). Both binaries skip themselves outside the private session.

| Item | Done | Test |
|---|---|---|
| **labwc_settings**: `config_home`, `rc_path` (session config else `$XDG_CONFIG_HOME/labwc/rc.xml`), `environment_path`, `backup_once` (`$XDG_STATE_HOME/desktop-suite/backups/<kind>/<name>.orig`, copy2), `atomic_write` (resolve, keep mode, 0644 new), `read_bytes` (2 MiB cap), `update` (concurrent-writer check) | yes | `labwc_settings::tests::update_backs_up_once_keeps_mode_and_reports_no_change`, parity `kbsave*.rcbackup` |
| `reconfigure(done)` async `labwc --reconfigure` (silenced) | yes (injectable `Reconfigure`) | UI keyboard/shortcuts with fakes |
| environment parse/edit/update/read (all Python `splitlines` boundaries, `export`, last-wins, ordering of additions) | yes | `environment_edit_preserves_…`, parity `env*.edit/parse` |
| `read_keyboard` (expat semantics: byte offsets, empty-element end events after `/>`, BOM offsets, Waytile comment toggles incl. prolog comments, action child text, onRelease, doctype/foreign root/multiple keyboard errors) | yes | `doctype_and_foreign_root_are_rejected`, parity `*.keyboard` |
| `set_keyboard_values`, `rebind` (quoteattr, `\bkey=` regex), `update_rc` | yes | `keyboard_values_are_spliced…`, `rebind_changes_only…`, parity `*.kbvalues*`, `*.rebind*` |
| **workspace_config**: `read_state`, `with_count` (minidom toxml incl. xmlns-first attribute order, numbered names, `<initial>` repair, empty/self-closing root, naïve `data.index(b'<labwc_config')`), `session_config` (`LABWC_PID` cmdline/exe/cwd, `-c/-C/--config[=]`, merged-config refusal, `(deleted)`), `WorkspaceConfig` (fallback, timestamped backups, mode keep, reload + restore) | yes | `workspace_config::tests::*` (7), parity `*.count*`, `*.workspace` |
| **workspace_settings** `WorkspaceControls` (classes, limit text, keep unapplied edit, apply/status) | yes | UI `workspace_controls_load_apply_and_report_errors` |
| **input_config** `validate`, `read_state`, `with_profiles`, `InputConfig` (message rewrite, input backups, restore) | yes | `input_config::tests::*` (11), parity `*.input`, `*.profiles*` |
| **input_devices** `parse_devices`, `supports/default_enabled/allowed`, `discover` (libinput injectable) | yes; pyudev replaced by /sys + `/run/udev/data` | `laptop_capabilities…`, `udev_fallback_and_discovery_order` |
| **mouse_polling** PollingBackend (OpenRazer/ratbag, physical identity, Commit/Resync flow), injectable bus factory | yes (gio D-Bus) | `mouse_polling::tests::*` (15) |
| **mouse_polling_ui** PollingCard | yes | UI `input_polling_*` (3) |
| **input_page** InputControls (profiles, pending edits, switches with device default reset, choices, numbers, scroll-button capture, test area, polling card, shared defaults, Apply in a thread, Apply/Discard/Cancel close prompt, reveal_setting) | yes | UI `input_*` (11) incl. private-compositor save (labwc fake on PATH) |
| **keyboard_config** rules catalog (+ xkeyboard-config .mo), localectl fallback (injectable), state/validate/env values, `KeyboardConfig.save` | yes | `keyboard_config::tests::*`, parity `kbsave*` |
| **keyboard_page** + LayoutChooser + embedded `on_screen_keyboard::settings::KeyboardSettings` (title restyle, prefix-stripped targets) | yes (`OskFactory` injection) | UI `keyboard_layout_edits…` |
| **shortcuts_model** describe/normalize/display_key/collect/conflicts/key_from_event/ShortcutStore; tiling BINDINGS from `tiling_manager::bindings` | yes | `shortcuts_model::tests::*` (7), parity `*.shortcuts` |
| **shortcuts_page** ShortcutList, RebindDialog (system-shortcut inhibit), ShortcutsPage, CheatSheet | yes | UI `shortcuts_listing…`, `cheat_sheet_filters_and_closes` |
| **accessibility_page** (gsettings, XCURSOR_SIZE env, orca via systemd-run/systemctl runner) | yes | UI `accessibility_writes_gsettings…` (GSETTINGS_BACKEND=memory) |
| **default_apps_page** (Gio defaults → mimeapps.list via GIO itself, TERMINAL env, xdg-terminals.list, xdg-settings runner) | yes | `terminal_commands_and_list`, UI `default_apps_set_through_backend` |
| **night_light** config/validate/json (indent 2), location, hdr_policy, outputs, command, `run` plan; `SERVICE` = night-light.service bytes | yes | `night_light::tests::*`, parity `night*` |
| **night_light_page** (debounced save, systemctl enable/disable/restart runner, HDR notes) | yes | UI `night_light_*` (2) |
| **users_backend** AccountsService (system bus), local fallback, PTY `passwd` conversation | yes | `pty_conversation_success_and_rejection`, `local_account_reads_passwd` |
| **users_page** + PasswordDialog | yes | UI `users_name_and_password_flow` |
| **about_page** collectors, format_values, copy, restart (`systemctl --user --no-block try-restart …`) | yes | `about_page::tests::*`, UI `about_values_copy_and_restart_command` |
| `SECTIONS`, `build_page`, search targets (settings_search keys/targets incl. `search`, `pollingRate`, OSK `automatic/layout/size/devices`) | yes | UI `every_section_builds_with_search_targets` |

## Pub API (crate gs-system)
`gs_system::{SECTIONS, build_page}`; per page `<module>::build_page_with(…)` with injections:
`input_page::build_page_with(host, Option<InputConfig>, Option<DeviceLoader>, Option<Arc<dyn Polling>>)`,
`keyboard_page::build_page_with(Option<KeyboardConfig>, Option<Catalog>, Option<OskFactory>)`,
`shortcuts_page::build_page_with(Option<Rc<dyn Shortcuts>>)`, `accessibility_page::build_page_with(Option<AccessibilityBackend>)`,
`default_apps_page::build_page_with(Option<Rc<dyn DefaultApps>>)`, `night_light_page::build_page_with(store, runner, outputs, available)`,
`users_page::build_page_with(Option<Arc<dyn Accounts>>, Option<Changer>)`, `about_page::build_page_with(Option<Collector>, Option<Runner>)`.
`shortcuts_page::CheatSheet::{new, with_store, present, close, closed, window, search, lists}`;
`shortcuts_model::{display_key, display_key_with, …}`; `workspace_config::{WorkspaceConfig, WorkspaceState, read_state, with_count, session_config, session_config_at, MAX_WORKSPACES}`;
`workspace_settings::{WorkspaceControls, WorkspaceBackend}`; `labwc_settings::*`; `compat::Error` (Python exception class + str).
Controllers are reachable with `page.controller::<InputControls|KeyboardPage|ShortcutsPage|…>()`.
No pub API was added to on-screen-keyboard or tiling-manager.

## Gaps / differences (honest)
- `install_input.py` not ported: it copies Python sources into `~/.local/share/global-settings`; the Rust swap is
  `tools/install.py`'s job.
- `night-light.service` keeps Python's `ExecStart=… night_light.py --run`; `night_light::run_plan` holds the runner
  logic, but there is no Rust `night-light` binary yet (installer must keep the Python runner or add one).
- `legacy_razer_rates` (inspecting `openrazer_daemon` Python classes for daemons without `getSupportedPollRates`)
  returns no rates: such mice are shown read-only.
- XML errors for documents that are both malformed and invalid in another way may report the expat syntax error rather
  than the ValueError Python raised first (we validate well-formedness before walking). Expat error texts come from
  tiling-manager's parser; they matched Python for every fixture tested.
- Search targets: Python shared the mutable `search_targets` dict with the page. gs-base's `SettingsPage` owns its map,
  so pages whose widgets are rebuilt keep stable widgets instead: Default Apps reuses its dropdowns; Input reuses one row
  box per setting and builds the polling card inside a persistent `gs-card` box. Wanted in gs-base: a shared/dynamic
  target map (e.g. `Rc<RefCell<HashMap>>` or a provider closure).
- `AboutPage` suite VERSION lookup: `$XDG_DATA_HOME/global-settings/VERSION`, `$XDG_DATA_HOME/VERSION`, then next to
  the executable (Python used its own file location).
- `key_from_event` and the RebindDialog key capture are exercised only through `key_string` and `set()` (no synthetic
  key events are sent).
