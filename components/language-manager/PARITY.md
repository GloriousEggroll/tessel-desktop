# language-manager — parity with contracts/language-manager.md

Rust crate `components/language-manager`: binary `language-manager` (= `language_manager.py`),
library (`backend` = language_backend.py, `services` = `LocaleServices`, `ui` = language_ui.py,
`style` = language_style.py, `args`, `app`, `worker`), example `fake_locale_services`
(measurement fakes). Shim: `shim/language_manager.py`.

Tests: `cargo test -p language-manager` (args unit tests, `tests/backend.rs` = every
test_language_backend.py case + Python byte-parity, `tests/private_bus.rs` = real gio transport and
binary on gio::TestDBus with fake localed/Accounts/systemd1), `tests/run_headless.sh` (all
test_language_ui.py cases in a private nested headless labwc), `tests/measure_headless.sh`
(Rust vs Python timing/RSS/screenshots, fake services only).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Contract item | | Test |
|---|---|---|
| GApplication `org.waydock.LanguageManager`, `HANDLES_COMMAND_LINE`, prgname = id, all args parsed in the primary | ✅ | ui `test_second_launch_reuses_current_page_and_window` |
| argparse: `--status`, hidden `--quit`, `-h/--help` text, abbreviations, `unrecognized arguments`, `ignored explicit argument`, exit 2, prog = basename(argv[0]) | ✅ | `args::tests::*` (texts captured from Python 3.14) |
| `--status` → `json.dumps(status, ensure_ascii=False)` via the command line, no window; error → stderr, 1 | ✅ | private_bus (binary, byte-checked fields), measure_headless (`cmp` with Python output: identical) |
| `--quit` refused while busy (`A language change is still in progress.`, exit 1), else close + quit | ✅ | ui singleton test |
| no args → reuse window (unminimize, present), raise via `wlrctl` ×4 / 100 ms | ✅ / ☑ (raise) | ui singleton test |
| No unit / .desktop / autostart of its own | ✅ | — |

## §2 IPC (consumed)
| Item | | Test |
|---|---|---|
| locale1 `Properties.Get Locale`, `SetLocale(as, true)` with ALLOW_INTERACTIVE_AUTHORIZATION, 120 s / reads 5 s | ✅ | private_bus (`SetLocale+i(...)`) |
| Accounts `FindUserById(x)`, `User.Language`, `SetLanguage(s)` interactive; ServiceUnknown/NameHasNoOwner/UserDoesNotExist → absent | ✅ | private_bus (no Accounts → `account_available: false`) |
| systemd1 `UnsetAndSetEnvironment(["LC_ALL"], [K=V…])`, `UpdateActivationEnvironment({…, LC_ALL: ""})`, failures → warnings | ✅ | private_bus (exact argument text) |
| Error text = PyGObject `str(GLib.Error)` | ✅ | private_bus (`g-dbus-error-quark: GDBus.Error:…ServiceUnknown…`) |
| Calls run on a worker thread | ✅ | ui (`threads` never the main thread) |

## §3–§5 Values, files, apply transaction
All of §3 (`LANGUAGES`, `language_environment`, `system_language_values`, `system_locale_assignments`,
`equivalent_locales`, `locale -a` with `LC_ALL=C`, 10 s timeout), §4 (four files + language.json,
atomic mkstemp `.<name>.*` + fsync + chmod(original mode, new 0600) + rename, regular-file check,
managed block rules, `.i18n` shell form, `plasma-localerc` only if present, invalid JSON never
overwritten, other keys kept in order, `indent=2, ensure_ascii=False`) and §5 (order of operations,
progress strings, verification, reverse rollback with ownership checks, `partial`/`relogin_required`,
session update warnings) — ✅ `tests/backend.rs`: all 31 Python cases plus
`python_parity_on_fixtures` (Python and Rust backends on identical fixtures produce byte-identical
files, identical service requests, results and warnings; includes CRLF, `export`, KDE sections,
non-ASCII and float preference values, and a malformed managed block).

## §6 UI
Window (title, 570×730, min 470×520, headerbar), pages choose/review/result, search (native, English,
locale names, casefold), check marks, review rows, result page, spinner/progress row, footer,
Escape, close refused while busy, host API (`busy`, `page`, `show_page`, `message`, `run`,
`loaded`, `close_requested`, `language_style_close`, `cancel_button`, `spinner`, `progress_row`,
`hide_wizard_chrome`, `wizard_action`, `wizard_step`, `on_wizard_changed`), CSS verbatim incl.
opaque provider at APPLICATION+35, companion theme attach/close on unrealize: ✅ ui (11/11 Python
cases). The choose page screenshot is byte-identical to Python's (measure_headless).

## §7 i18n
Every string goes through `suite_core::tr!` with the Python English key, including the second
`tr(message)` on progress text. ☑

## Deviations ⚠
1. **D-Bus via gio, not zbus** (as power-manager): gio's sync calls are what `LocaleServices` used;
   the worker thread keeps them off the GTK loop.
2. **Lazy GTK**: the primary is a `gio::Application`; GTK initializes when the wizard is first
   shown, so `--status` needs no display. Windows are plain `GtkWindow`s (app held while open), app
   id from prgname. The `org.gtk.Actions` export of GtkApplication is gone (unused).
3. `XDG_CONFIG_HOME=""` is treated as unset (suite_core::paths rule, same as suite_i18n's reader);
   Python would have used `Path("")` = the working directory.
4. JSON-decode error text inside "Cannot read the saved component language: …" is serde_json's;
   Python json accepts `NaN`/`Infinity` literals, serde does not. Signal deaths of `locale -a`
   read `died with signal N.` instead of `<Signals.SIGKILL: 9>`.

## Gaps ❌
- Global Settings embeds the **Python** `LanguageWizard` (language_page.py) and calls private
  methods; language_backend.py / language_ui.py / language_style.py must stay installed until
  global-settings is ported (wave 4). The Rust `ui::LanguageWizard` exposes the same host API for
  that port.
- The installed `~/.local/bin/language-manager` launcher opens Global Settings, not this binary;
  unchanged here.
