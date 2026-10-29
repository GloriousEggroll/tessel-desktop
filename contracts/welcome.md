# welcome — external contract

Source: `~/btmanager/welcome` (`welcome.py` 69, `welcome_ui.py` 521, `install.py` 151). Rust:
`components/welcome` (lib `welcome`, binary `desktop-suite-welcome`).

## 1. Entry points
| Item | Value | Ref |
|---|---|---|
| Installed tree | `~/.local/share/desktop-suite-welcome/{welcome.py,welcome_ui.py}` (0644) | install.py:15-26 |
| Launcher | `~/.local/bin/desktop-suite-welcome` = `#!/bin/sh\nexec /usr/bin/python3 '<…/welcome.py>' "$@"\n` (0755) | install.py:28-29 |
| .desktop | `~/.local/share/applications/org.waydock.Welcome.desktop`: `Name=Welcome`, `Comment=Set up language, keyboard, desktop layout and colors`, `Exec=<launcher>` (absolute, unquoted), `Icon=user-home`, `Terminal=false`, `Categories=Settings;DesktopSettings;`, `StartupNotify=true`, `StartupWMClass=org.waydock.Welcome`, `Keywords=Setup;First run;Getting started;Tour;` | install.py:30-35 |
| Installer | `install.py [--check|--apply|--rollback BACKUP]`: backup `~/.local/state/desktop-suite/backups/welcome-<UTC stamp>/files/…` + `manifest.json` `{"version":1,"home":…,"files":[{"path","existed"}]}` 0600; refuses symlinks; desktop-file-validate | install.py |
| First login | `install_all.py` autostart: if `$STATE/setup-done` is missing, run `~/.local/bin/desktop-suite-welcome --first-run >>"$LOG" 2>&1 &` (else `python3 welcome/welcome.py --first-run`) | install_all.py:483-490 |
| Menu | labwc root menu `<item label="Welcome"><action name="Execute" command="~/.local/bin/desktop-suite-welcome"/></item>` | install_all.py:188 |
| GApplication | `org.waydock.Welcome`, `HANDLES_COMMAND_LINE`, `set_prgname(id)`; `run([argv0, *argv])`; each command line: new `WelcomeWindow(first_run=…)` if none/closed, `--page KEY` shows that step if valid (else ignored), `present()` | welcome.py |
| CLI | argparse (`allow_abbrev`): `--first-run`, `--page PAGE`; parsed locally before contacting the primary; prog `welcome.py`; texts byte-identical (tests/fixtures/welcome_capture.out) | |
| Missing GS | Python: `SystemExit('Install the desktop suite (install_all.py --apply) before opening Welcome.')` when `global-settings/runtime_paths.py` is absent. Rust links the Global Settings pages, so this cannot happen. | |

## 2. Files
- `$XDG_STATE_HOME/desktop-suite/setup-done` (`XDG_STATE_HOME or ~/.local/state`): written on Finish unless the
  install status is not ok; `mkstemp(prefix='.setup-done-')` in the directory (0600), content
  `datetime.now(timezone.utc).isoformat(timespec='seconds') + '\n'` (`2026-10-29T12:00:00+00:00`), `os.replace`.
- Reads `$XDG_STATE_HOME/desktop-suite/install-status.json`: `ok = bool(data['ok']) and not failed`, failed =
  `step` (default `?`) of every step whose `status` is `failed` or `not run`; unreadable/invalid/non-object → None.
- Light/Dark writes `org.gnome.desktop.interface color-scheme` (`prefer-light|prefer-dark|default`) through GSettings.
- Embedded pages write exactly what their Global Settings pages write (language wizard, keyboard, layout presets).

## 3. Window
`WelcomeWindow`: title `Welcome`, default 1000×740, min 760×560, `headerbar`, `global_style.install`, class `welcome`,
CSS (verbatim) at APPLICATION priority. Sidebar ListBox `.welcome-steps` (220 px) rows: symbolic icon 16 + step name;
steps `welcome, language, keyboard, layout, style, shortcuts, done` (names Welcome, Language, Keyboard, Desktop Layout,
Light or Dark, Shortcuts, Done). Footer `.welcome-actions`: Back, `gs-muted` progress "Step {number} of {total}", Next
(`suggested-action`; "Start Using Your Desktop" on the last step). Completed rows get `.welcome-step-done`.
Escape closes; Alt+Left/Right = Back/Next. `language/keyboard/layout` are the Global Settings pages
(`unify_header` with the step icon; the language wizard's own stepper/footer hidden via `hide_wizard_chrome`, Next/Back
drive `wizard_step`, label = `wizard_action()`, insensitive while busy). A page that fails to build shows a fallback
(header + "This step could not be shown here…", error text, "Open Settings" → `global-settings --section KEY`).
Welcome page: 64 px `user-home-symbolic`, `.welcome-hero`, `.welcome-lead`, and a `.gs-card.welcome-warning` listing
failed install steps + log path `$STATE/first-login.log`. Style page: three grouped ToggleButtons `.welcome-scheme`
following `changed::color-scheme`; disabled with "The shared color scheme setting is unavailable on this system." when the
schema/key is missing. Shortcuts page: 7 default shortcuts (`display_key(key, tr)` in `.welcome-key`), "Show All
Shortcuts" (`global-settings --shortcuts`), "Change Shortcuts" (`--section shortcuts`). Done page: "You're all set",
"Open Settings" (`--section about`). Close is refused while a page's `can_close()` is False (that step is shown).
Launches use the installed `~/.local/bin/global-settings` when executable, else Python `global_settings.py`.

## 4. Tests to port
test_welcome.py: StateTests (paths, install_status, mark_setup_done, settings_command), InstallerTests (installer —
handled by tools/install.py in the Rust swap), WelcomeUITests (walk/finish, sidebar reuse, light/dark, missing colour
settings, shortcuts + cheat sheet, fallback, failed installation, busy page, real pages embed).
