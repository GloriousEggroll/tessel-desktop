# tessel-language — external contract

Source: `~/tessel-bluetooth/tessel-language` (`language_manager.py` 114, `language_backend.py` 530,
`language_ui.py` 416, `language_style.py` 102). GUI-independent backend + 3-page wizard. The wizard
class is **embedded by Global Settings** (`tessel-settings/language_page.py:1-36`), and the installed
`~/.local/bin/tessel-language` launcher actually opens **Global Settings** on its language section.

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installed dir | `~/.local/share/tessel-language/` (all non-test/install/fake `*.py` + shared `suite_i18n, language_data, suite_catalogs, gtk4_utils, service_settings, companion_theme, background_blur(.so)`) | shared/install_gtk4.py:147-153 |
| Launcher `~/.local/bin/tessel-language` | `exec /usr/bin/python3 ~/.local/share/tessel-settings/global_settings.py --section language "$@"` (NOT language_manager.py) | install_gtk4.py:164-166 |
| Standalone | `python3 language_manager.py [--status] [--quit]` | language_manager.py:22-26 |
| GApplication | id `org.tessel.LanguageManager`, `HANDLES_COMMAND_LINE`, `GLib.set_prgname(id)`; all args parsed in the primary instance (`do_local_command_line` returns False) | :19,29-66 |
| `--status` | prints `json.dumps(backend.status(), ensure_ascii=False)` via remote command line, no window, exit 0 (error → stderr, 1) | :57-64 |
| `--quit` (hidden) | refuses with `A language change is still in progress.` (exit 1) while busy; else closes window and quits | :49-56 |
| no args | activate: reuse existing window (unminimize, present), then raise via `wlrctl toplevel focus app_id:<id> title:<title>` retried up to 4× every 100 ms on Wayland | :68-105 |
| systemd/.desktop/autostart | none of its own (Settings desktop entry `org.tessel.GlobalSettings.desktop` has Keywords incl. Language) | install_gtk4.py:167-172 |
| `standalone_instances()` | installer finds running `language_manager.py` processes by argv[1] | install_gtk4.py:200-215 |

## 2. IPC (consumed only; owns no bus name besides the GApplication id)

| Bus | Name / path / iface | Call | Ref |
|---|---|---|---|
| system | `org.freedesktop.locale1` `/org/freedesktop/locale1` | `Properties.Get("org.freedesktop.locale1","Locale")→as`; `org.freedesktop.locale1.SetLocale(as, b interactive=true)` with ALLOW_INTERACTIVE_AUTHORIZATION, timeout 120 s (reads 5 s) | language_backend.py:261-269 |
| system | `org.freedesktop.Accounts` `/org/freedesktop/Accounts` | `FindUserById(x uid)→o`; `Properties.Get("org.freedesktop.Accounts.User","Language")→s`; `org.freedesktop.Accounts.User.SetLanguage(s)` interactive. ServiceUnknown / NameHasNoOwner / UserDoesNotExist → account unavailable (None) | :271-289 |
| session | `org.freedesktop.systemd1` Manager | `UnsetAndSetEnvironment(as ["LC_ALL"], as ["K=V"…])` | :291-301 |
| session | `org.freedesktop.DBus` | `UpdateActivationEnvironment(a{ss} {…, "LC_ALL": ""})` | :302-308 |

Polkit prompts come from the session's authentication agent (polkitman); calls run on a worker thread.

## 3. Language catalog and values
- `LANGUAGES` from `shared/language_data.py` `LANG_NAMES` (code→native name) and `LOCALE_NAMES` (code→`xx_YY.UTF-8`); 31 codes; English names table `language_ui.py:20-28` for search.
- `language_environment(lang)` = `{LANG: locale, LANGUAGE: "code[:base][:en]", LC_MESSAGES: locale}` (region code before base, then `en`).
- System request `system_language_values(old, lang)` (:123-132): keep old categories, default every FORMAT_KEY to old LANG (or `C.UTF-8`), set `LANG=LANGUAGE=LC_MESSAGES=<locale>` (LANGUAGE must be one installed locale for localed), then `system_locale_assignments` = every key in `LOCALE_KEYS` sorted, missing ones = LANG (localed merges omissions & rejects empties).
- `LOCALE_KEYS` = LANG, LANGUAGE, LC_CTYPE, LC_MESSAGES + 10 FORMAT_KEYS (LC_NUMERIC, LC_TIME, LC_COLLATE, LC_MONETARY, LC_PAPER, LC_NAME, LC_ADDRESS, LC_TELEPHONE, LC_MEASUREMENT, LC_IDENTIFICATION). `LANGUAGE_KEYS` = LANG, LANGUAGE, LC_MESSAGES, LC_ALL (lines removed from user files).
- `equivalent_locales`: compare all LOCALE_KEYS with LANG fallback (then `C`), normalized lower-case without `-`.
- Installed locales: `locale -a` (env LC_ALL=C, timeout 10), normalized.

## 4. Files (all written atomically, original mode kept, new 0600, regular files only — symlink → `Cannot update {path}: it is not a regular file.`)

| Path | Content | Notes |
|---|---|---|
| `$XDG_CONFIG_HOME/tessel/language.json` | `{"version":1,"language":"<code>","locale":"<xx_YY.UTF-8>", …other keys kept}` indent 2, `ensure_ascii=False` | **Read by every component** via `shared/suite_i18n.py:19-45` (valid only if version 1, code in LANG_NAMES, locale == LOCALE_NAMES[code]). Invalid JSON → error, never overwritten |
| `$XDG_CONFIG_HOME/environment.d/90-tessel-language.conf` | managed block (no `LC_ALL` line: systemd rejects empty) | |
| `$XDG_CONFIG_HOME/labwc/environment` | managed block + `LC_ALL=` | same file as way-displays primary-output block |
| `~/.i18n` | managed block + `unset LC_ALL` (shell), `NAME=value` without `export` (Fedora lang.sh parser) | |
| `$XDG_CONFIG_HOME/plasma-localerc` | only if it already exists: `[Translations] LANGUAGE=<list>` replaced/inserted; Formats untouched | :178-205 |

Managed block (:45-46,146-175): `# Tessel language — managed by Language Settings` … `# End Tessel language`, appended after a blank line, after stripping existing block and any `LANG/LANGUAGE/LC_MESSAGES/LC_ALL` assignments (with optional `export`). Values must match `[A-Za-z0-9_:.@-]+`. Format keys: user env (`LC_ALL` > key > `LANG`) else system value else `C.UTF-8`; formats already explicitly present in the file are not written. Unbalanced/duplicated markers → `The saved language section is incomplete…` (never silently deleted).

## 5. Apply transaction (`LanguageBackend.apply(code, progress)`, :427-530) → `ApplyResult(ok, language, locale, error, warnings, rolled_back, partial, relogin_required)`
1. unknown code → `Choose one of the supported languages.` (relogin false). 2. locale not installed → `{language} requires the {locale} locale. Install its language support first.` 3. read old system + account; build all file plans (validated before any prompt). 4. SetLocale; read back LANG must match. 5. account SetLanguage + verify (else warning if no AccountsService). 6. for each file: if changed since planned → `{file} changed while applying. Please retry.`; skip unchanged; write. 7. failure → rollback in reverse: files only if still our bytes; account only if it still holds our value; system only if still equivalent to our request (`set_locale(system_locale_assignments(old))`, verify; error if old had no LANG but restored has). Rollback errors → `partial=True`, `relogin_required=True`. 8. success → `update_session(...)` (failures are warnings only).
Progress strings (translated): `Checking installed language support…`, `Saving system language…`, `Saving login language…`, `Saving component preferences…`, `Restoring previous settings…`, `Updating the session environment…`.
`status()` (:348-383) → `{language, locale, system_locale, system_available, account_available, languages:[{code,name,locale,installed}], warnings:[...]}`; never writes.

## 6. UI
- `LanguageWizard(Gtk.ApplicationWindow)` title `Language Settings`, 570×730 (min 470×520), headerbar; pages `choose` (search + ListBox `.language-list`, `.profile-check`), `review` (3 info rows), `result` (`.result-icon`); footer Cancel/Back/`Continue`→`Apply Language` (`.primary`); spinner progress row; Escape closes when not busy; close refused while busy with `Please finish or cancel the authentication prompt before closing this window.`
- Host API used by Global Settings: `busy`, `page`, `_show_page`, `_message`, `_run`, `_loaded`, `_unrealized`, `_close_requested`, `_language_style_close`, `cancel_button`, `spinner`, `progress_row`, `hide_wizard_chrome()`, `wizard_action()`, `wizard_step(forward)`; GS re-parents `get_child()` and adds `.languagemanager`.
- CSS (language_style.py): window class `languagemanager`; `.title .section-title .muted .subtitle .steps .settings-tile .card .language-list .empty .primary`; extra opaque provider at APPLICATION+35 so the background is opaque above companion_theme; `companion_theme.attach`, cleanup on unrealize.

## 7. Parity test cases
Backend (`test_language_backend.py`): catalog equals previous Bluetooth names; status side-effect free; missing locale/unknown code rejected before any change; success updates system, account, prefs, session; every language uses installed UTF-8 locale for system and codes for apps; short LANGUAGE rejected by localed fixture; stale system language replaced, simplified read-back equivalent; rollback resets categories introduced by request; no false claim of restoring unset default; unrelated settings/formats/comments/permissions preserved; `.i18n` readable by Nobara profile parser; repeat apply idempotent; user formats survive inherited LC_ALL in files and new services; permission denied leaves files/account untouched; account failure restores system; write failure rolls back files+system+account; failed system restore → partial; timed-out system/account writes detected and restored; external locale change before verification not rolled back; external regional edit survives; concurrent system change not overwritten; no AccountsService → saved with warning; session failure = success with relogin warning; invalid saved config not overwritten; symlinked config not replaced; concurrent user edit preserved on rollback; system format fallbacks preserved; malformed managed block not deleted; KDE inserts `[Translations]` without touching formats; environment.d omits empty LC_ALL.
UI (`test_language_ui.py`): open/navigation never change settings; search matches native, English and locale names; missing locale explained, can't apply; missing system service explained; apply in background, window protected; host buttons drive wizard without own chrome; opaque background above companion theme; failed apply retry keeps selection; partial failure not shown as success; cancel closes without mutation and unsubscribes theme; second launch reuses window/page.

## 8. Risks
- GS embeds the Python class and calls private methods; the Rust port is effectively a GS page. Coordinate with tessel-settings porting (Wave 4).
- `suite_i18n` validity rules for language.json are shared by all 31-language components (tessel-core must implement identical validation).
- localed merge semantics (explicit full key list) and rollback equivalence logic are subtle—port tests verbatim with a fake localed.
- No Wayland protocols; raise uses external `wlrctl` (keep, or use xdg-activation).
