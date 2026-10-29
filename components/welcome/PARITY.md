# welcome — parity with contracts/welcome.md

Crate `components/welcome` (lib `welcome`, binary `desktop-suite-welcome`, GApplication `org.waydock.Welcome`).
Tests: `cargo test -p welcome --lib` (6: CLI texts byte-compared with argparse output captured from welcome.py in the
private session, state paths, install_status, atomic setup-done, settings_command); `tests/run_headless.sh`
(11 GTK checks in the private headless labwc via ../global-settings/tests/headless.sh: port of WelcomeUITests +
Alt/Escape keys + single-instance routing through a second process + real Global Settings pages embedded).
Measured with ../global-settings/tests/measure_inner.sh (screenshot of the first page: 0 px difference to Python).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

| Item | | Test |
|---|---|---|
| CLI `--first-run`, `--page` (unique prefixes, usage/help/error texts), parsed locally first | ✅ | `cli::tests::*` |
| Command line in primary: new window if none/closed, valid `--page` shows the step, invalid ignored, present | ✅ | `single_instance_routes_page` |
| setup-done path (`XDG_STATE_HOME or ~/.local/state`), mkstemp + replace, UTC isoformat seconds | ✅ | `state::tests::*`, `walks_every_step_and_finishes` |
| install-status.json (ok/failed, `?` default, invalid → None) and the warning card | ✅ | `install_status_reads_failed_steps`, `failed_installation_is_shown_and_setup_stays_pending` |
| Steps, sidebar, Back/Next/progress/done classes, Start Using Your Desktop, finish | ✅ | `walks_every_step_and_finishes`, `sidebar_jumps_and_reuses_pages` |
| Embedded GS pages (language/keyboard/layout) with unify_header; language wizard chrome hidden, Next/Back drive `wizard_step`, label/sensitivity from the wizard | ✅ | `real_settings_pages_embed` |
| Fallback page with Open Settings | ✅ | `unavailable_page_links_to_settings` |
| Light/Dark ToggleButtons ↔ `color-scheme` (handler released on close), unavailable message | ✅ | `light_and_dark`, `missing_color_settings_disable_the_choice` |
| Shortcuts page (`display_key(key, tr)`), Show All Shortcuts / Change Shortcuts launches | ✅ | `shortcut_overview_and_cheat_sheet` |
| Busy page refuses close and shows its step; Escape; Alt+Left/Right | ✅ | `busy_page_keeps_the_window_open`, `escape_and_alt_arrows` |
| CSS verbatim, global_style.install, sizes | ☑ | screenshot 0 px vs Python |
| Installer (install.py --check/--apply/--rollback) | ⚠ | replaced by tools/install.py entry `welcome` (launcher swap only; the Python files stay) |

## Deviations ⚠
1. `settings_command` falls back to the Rust `global-settings` binary beside this executable, then to
   `/usr/bin/python3 ~/.local/share/global-settings/global_settings.py`, when `~/.local/bin/global-settings` is not
   executable (Python used `sys.executable` and its own source tree first).
2. The "Install the desktop suite … before opening Welcome." exit cannot occur: the pages are linked in.
3. A page calling `host.show_section` from inside Welcome logs a warning (Python raised AttributeError).
