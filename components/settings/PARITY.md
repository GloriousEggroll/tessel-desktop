# tessel-settings — parity with contracts/tessel-settings.md

Crates: `components/settings` (shell + binary `tessel-settings`, lib `global_settings`), `base/` (`gs-base`:
page_api, global_style, page_widgets), `applets/` (`gs-applets`), `desktop/` (`gs-desktop`), `system/` (`gs-system`).
Page groups have their own PARITY.md: [applets](applets/PARITY.md), [desktop](desktop/PARITY.md),
[system](system/PARITY.md).

Tests (shell):
- `cargo test -p tessel-settings --lib` — CLI (argparse texts byte-compared with output captured from the Python app in
  the private session: `tests/fixtures/cli_capture.out`) and the search index (port of test_settings_search.py).
- `tests/run_headless.sh` — `tests/shell_headless.rs` in a private headless labwc via `tests/headless.sh`
  (port of test_global_ui.py GlobalUITests / SearchUITests / SingletonTests + Python ranking parity).
- `tests/headless.sh <cmd>` — the shared private-session runner (private runtime dir + socket, private bus without
  service dirs, fake HOME/XDG, logging fakes for pkexec/sudo/systemctl/labwc/localectl/passwd/… on PATH; fails on any
  bus activation). Used by every page group and by Welcome.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Item | | Test |
|---|---|---|
| GApplication `org.tessel.GlobalSettings`, HANDLES_COMMAND_LINE, prgname | ✅ | `repeated_launch_focuses_and_routes_section_in_existing_window` (second process forwarded over the private bus) |
| `--section` (alias brightness→displays before choices), `--shortcuts` toggle, `--quit` (refused + message + exit 1 while a page is busy), abbreviations, help/usage/error texts byte-identical, local parse before forwarding | ✅ | `cli::tests::*` (captured Python output), singleton check |
| show(): new window if none/closed, `audio` first time, unminimize, present, `wlrctl toplevel focus` raise (≤4 retries) | ✅/☑ | singleton check (raise uses `suite_gtk::utils::raise_window`, faked wlrctl) |
| Launchers / .desktop / tessel-night-light.service | ✅ | tools/install.py entry `tessel-settings` (launchers only; tools/test_install.py) |

## §2 Window
| Item | | Test |
|---|---|---|
| Title/sizes/headerbar/`global_style.install` (CSS + PAGE_CSS verbatim, companion palette) | ✅ | `pages_share_one_header…` |
| Sidebar (classes, search entry, rows sorted by translated casefold, symbolic icons) | ✅ | `pages_share_one_header_and_sidebar_icons_are_symbolic` |
| Lazy pages, lifecycle (activate/deactivate), reuse, cleanup once, close guard | ✅ | `sections_are_lazy_reused_and_cleaned_once`, `inflight_language_apply…`, `search_preserves_busy_page_close_guard` |
| Error page + Try Again, `show_page_error` | ✅ | `failed_section_does_not_prevent…`, `async_page_errors_use_the_shared_error_state` |
| `_unify` (unify_header + align_page_gutter + 24 px icon) | ✅ | header checks |
| Search: index (178 settings + section entries), normalize, ranking, results page, summary, empty state, Down key, activate, spotlight (search_targets/target, label fallback, sub-page tabs / `page_name` rows, focus, scroll 30 %, 3.5 s highlight, 14 retries) | ✅ | `search_*`, `activating_match_opens_and_scrolls…`, `weather_uses_explicit_search_target`, `audio_result_reveals_nested_audio_page`, `search_ranking_matches_python` (28 queries, ordered results + normalize identical to Python) |
| Ctrl+F, Escape (leave search, else close) | ✅ | `no_match_and_escape…` |

## Deviations ⚠
1. Python `_build_page` caught any exception from a page module (ImportError when a component is not installed). Rust page
   builders return `Err(String)`; the error page text is that message. The release profile aborts on panic, so a page
   bug that panics is not converted into an error page.
2. `search_result_activated` and the spotlight use `SettingsPage::search_targets` (one map) instead of Python's
   `getattr` over four candidate controller objects; pages register every widget Python exposed under the key or
   `target` name.
