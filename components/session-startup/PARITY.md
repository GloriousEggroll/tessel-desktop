# tessel-session-startup — parity with contracts/tessel-session-startup.md

Rust crate `components/session-startup` (library `session_startup` + 3 binaries):

| Binary | Replaces | Notes |
|---|---|---|
| `tessel-configure-session` | `configure_session.py` (`--check` default \| `--apply`) | identical stdout/stderr/exit codes and identical written trees |
| `tessel-xdg-autostart` | `configure_xdg_autostart.py` (`[--apply]`) | identical; Python tracebacks become the bare exception message |
| `tessel-failed` | `desktop_suite_failed.py UNIT.service` | gio D-Bus, `suite_core::tr!` |

`tessel-action` stays the POSIX shell script (contract §1); it is tested, not ported.
Not wired into `install_all.py` or anything else.

Shipped files (`desktop_suite_failed.py`, `shared/{suite_i18n,language_data,suite_catalogs}.py`,
`tessel-failed@.service`, `tessel-action`, `units/*.service`,
`labwc-xdg-autostart.target`) are read at run time from the suite tree:
`$DESKTOP_SUITE_SOURCE`, else the first ancestor of the executable that holds
`tessel-session-startup/tessel-action`, else `~/tessel-bluetooth` (Python: the script's own directory).
argparse's program name is `argv[0]`'s basename, so shims exec with the Python script name.

Tests: `TMPDIR=/run/user/1000/cbt/rust-session-startup cargo test -p tessel-session-startup`
(55 tests: 12 unit, 20 `configure_session`, 9 `python_parity`, 9 `shell_scripts`,
5 `failure_helper`). Every home is a fresh directory under `$TMPDIR`; external commands
(`systemctl`, `labwc`, `dbus-update-activation-environment`, `sh` for the failure case) are
logging fakes first on `PATH`; D-Bus is a private `dbus-daemon` with no service directories.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Item | | Test |
|---|---|---|
| `configure_session` argparse: `--check \| --apply` exclusive, `-h`, prefixes (`--a`), `--x=1`, `--`, unrecognized → exit 2 with argparse text | ✅ | `python_parity::failures_match_python` (argv), `configure_session::argument_errors_match_argparse` |
| Preview: unified diff per changed file (`difflib` port incl. autojunk), notes, `N files prepared…` | ✅ | `python_parity::*` (byte-identical stdout), `difflib::tests` |
| Apply: snapshot re-check, backup `~/.local/state/tessel/backups/session-<%Y%m%dT%H%M%S%f>/`, `atomic_write` (`.labwc-services-*`, chmod, replace), `sh -n` autostart(+.local), XML re-parse of rc/menu, rollback (restore / unlink new) | ✅ | `python_parity::{live_home_matches_python, failures_match_python}` (shfail: rollback tree identical) |
| After apply: `systemctl --user daemon-reload` (30 s), `labwc --reconfigure` (10 s) iff `$LABWC_PID` and rc/menu written; errors → `sys.exit(str)` text, exit 1 | ✅ | `python_parity::{live_home_inside_labwc_reconfigures, failures_match_python}` (nosystemctl), fake command logs compared |
| `configure_xdg_autostart`: plan (env/start/stop blocks, desktop identity, target, nm-applet/blueman exclusions from `~/.config` + `$XDG_CONFIG_DIRS`), mode kept, backup `xdg-autostart-<ts>`, `sh -n` autostart+shutdown, byte re-read, rollback | ✅ | `python_parity::{xdg_autostart_matches_python, xdg_failures_match_python}`, `shell_scripts::xdg_*`, `xdg::tests` |
| `desktop_suite_failed`: exactly one `[A-Za-z0-9:_.\\-]+\.service` arg else usage + exit 2; always 0 otherwise | ✅ | `failure_helper::invalid_unit_names_are_rejected`, `failed::tests` |
| `configure.py` (legacy one-time migration writing `.migration.json` into the source tree) | ❌ | not ported (historic, already applied; contract marks generators optional) |

## §2 tessel-action
Kept as shell. ActionScriptTests ported against the tessel-bluetooth script: playerctl, missing
`$TERMINAL` skipped, `$TERMINAL` with args, `--menu` only when the source mentions it,
unknown → 2 — ✅ `shell_scripts::*`.

## §3 Generated files (all byte-identical to Python, `python_parity`)
| Item | | Test |
|---|---|---|
| rc.xml block after last `<default/>`, markers, row order, rebind preservation by canonical action, suite vs user bindings outside the block (`_is_suite_binding`, casefold keys), skipped notes, comment masking, tiling block untouched/after | ✅ | `configure_session::{global_settings_rebind_is_kept, users_own_binding_is_never_replaced, old_suite_bindings_after_defaults_are_replaced, tiling_block_*, commented_bindings_*}`, `python_parity::keybinding_variants_match_python` (6 variants + rebind) |
| Missing / self-closing `<keyboard>`; DOCTYPE/ENTITY, root, multiple keyboards, marker errors (exact messages) | ✅ | `missing_keyboard_section_keeps_labwc_defaults`, `invalid_documents_are_rejected` |
| `command=`/`<command>` home → `~/` (escaped + raw), `promptCommand` → `"$HOME"/`, `errorCommand` → `sh -c 'exec …'` (shlex.quote) | ✅ | `keybindings_are_portable_*`, parity `rc-prompts` |
| Theme: cornerRadius 12, Active/InactiveWindow fonts (name/size kept, SemiBold, indent), dead `<active>/<inactive>` removed (backreference regex emulated) | ✅ | `theme_has_matching_title_fonts_*`, parity `rc-crlf` |
| menu.xml terminals → `tessel-action terminal` | ✅ | `menu_opens_configured_terminal` |
| autostart template verbatim, statement parser (continuations, nesting with Python's lookarounds, `# Begin/End` blocks), identity keys, personal lines → autostart.local once (header / newline rules) | ✅ | `personal_startup_*`, `new_personal_lines_*`, `statements_follow_shell_nesting_*`, parity `env` |
| environment template, kept Settings variables, way-displays + language blocks (exact marker lines), personal vars → `environment.d/90-local.env` (last value, comments, local de-dup) | ✅ | `environment_effective_values_are_unchanged`, parity `env` (CRLF, `export`, unterminated block, local w/o newline) |
| Base units created only when missing (not over symlinks), suite units + `*.conf` drop-ins (dotfiles included, sorted) → `%h/`, recovery drop-in with env-file iff `/.config/labwc/environment` mentioned, no drop-in for absent units | ✅ | `units_use_home_specifier_*`, `missing_base_units_*`, `units_are_portable_*`, parity `units` |
| Runtime files (helper + 3 shared modules, template unit, dispatcher 0755) | ✅ | `units_use_home_specifier_*`, parity trees |
| Symlinked targets refused (`Refusing to replace a symlink: …`) | ✅ | parity `symlink` |
| `read_text` universal newlines (CRLF files are rewritten with LF, like Python) | ✅ | parity `rc-crlf`, `env` |

## §4 desktop_suite_failed runtime
| Item | | Test |
|---|---|---|
| Skip when `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` is gone (no systemctl call) or `is-active --quiet graphical-session.target` fails | ✅ | `failure_helper::session_ending_is_not_reported` |
| Component names (11, translated via `tr!`), unknown → unit stem | ✅ | `failed::tests::component_names` |
| `Notify(susssasa{sv}i)` exact values, hints order urgency(y 2), desktop-entry, timeout 10 s | ✅ | `failed::tests::notification_parameters_match_python`; private bus: recorded parameters equal Python's |
| `ActionInvoked`/`NotificationClosed` filtered by id; `restart` → `reset-failed` + `start --no-block`; any matching signal quits; 30 min cap | ✅ | `failure_helper::{restart_action_*, closing_the_notification_*}` (Rust and Python record the same calls) |
| Notify error → `U failed; no notification could be shown: <GError message>`, exit 0 | ✅ | `failure_helper::missing_notification_service_*` (identical to Python) |

## §6 Python test cases
All of test_configure_session.py and test_configure_xdg_autostart.py are ported (above).
test_xdg_generator.py exercises `/usr/lib/systemd/user-generators/systemd-xdg-autostart-generator`
(system code, not this component) — not ported.

## Deviations ⚠
1. XML parsing is roxmltree, not expat: well-formed input behaves identically, but the text of
   XML parse errors (`ET.ParseError`) differs. `canonical` of a block row with zero/several
   child elements fails like Python but with approximate wording.
2. `html.unescape` handles numeric references and the XML/HTML core names (`amp lt gt quot apos nbsp`),
   which is everything valid XML text can contain; exotic HTML5 names are left as is.
3. Python `compile()`-checks the shipped `.py` helpers before copying; Rust copies them unchecked.
4. `configure_xdg_autostart.py` dies with a traceback on errors; the Rust binary prints the
   exception message (same exit code 1). `desktop_suite_failed.py` likewise when `systemctl`
   is missing (Rust: message + exit 1).
5. Rust-only knobs: `DESKTOP_SUITE_SOURCE` (suite tree), `DESKTOP_SUITE_FAILED_WAIT_MS` (tests).
6. UnicodeDecodeError text is reproduced for the common cases only.

## Gaps ❌
- `configure.py` legacy migration (see §1).
- `configure_session.py` must stay installed as Python: `install_all.py` (`session_module`:
  `edit_rc`, `edit_menu`, `AUTOSTART_TEMPLATE`, `ENVIRONMENT_TEMPLATE`) and
  `shared/install_gtk4.py` (`runtime_files`) import it. The Rust library exposes the same
  functions for a future Rust installer.

## Additions (no Python counterpart)
| Item | | Test |
|---|---|---|
| `tessel-icon-theme [--check]`: at login, when the configured icon theme (gsettings `org.gnome.desktop.interface icon-theme`, else `gtk-icon-theme-name` in `~/.config/gtk-4.0/settings.ini`) is not installed, set Papirus-Dark in gsettings and both GTK `settings.ini` files (other lines kept); an installed choice is never changed; nothing changes when Papirus-Dark is missing too. Run by `packaging/defaults/etc/xdg/autostart/tessel-icon-theme.desktop`. | ✅ | `icon_theme::tests::*`, `suite_core::icon_theme::tests::*` |
