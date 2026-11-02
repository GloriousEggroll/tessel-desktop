# tessel-calendar — parity with ~/tessel-bluetooth/tessel-calendar

Contract: `contracts/tessel-calendar.md`. Tests: `cargo test -p tessel-calendar`
(unit + `engine_parity` + `fetch` + `private_bus`), GUI: `tests/run_headless.sh`
(27 cases in a private nested headless labwc), measurements: `tests/measure_headless.sh`.

## 1. Entry points / processes

| Contract item | Status | Test |
|---|---|---|
| Binary `tessel-calendar`; shim `shim/calendar_manager.py` execs it as `calendar_manager.py` | done | — |
| App id `org.tessel.CalendarManager`, `HANDLES_COMMAND_LINE`, prgname = app id | done (`app.rs`) | `remote_activation_reuses_application_and_routes_between_views` |
| CLI `--popup`/`--settings` (mutually exclusive), `--popup-edge {top,bottom,left,right}`, `--popup-offset INT` (negative → exit 2), argparse abbreviations/messages/usage layout | done (`args.rs`) | `args::tests::*`, `private_bus::binary_validates_arguments_locally` |
| Args validated locally before the GApplication starts; every launch forwards argv to the primary | done | `binary_validates_arguments_locally`, remote activation test |
| Remote `--popup`: different edge/offset destroys + rebuilds, same → `PopupController.activate()` toggle | done | remote activation test |
| `--settings` dismisses popup, reuses one `DateSettingsWindow`, raise via `wlrctl toplevel focus app_id:… title:…` (≤4 retries / 100 ms) | done (`suite_gtk::utils::raise_window` from idle) | `popup_settings_button_reuses_existing_window_and_backend` |
| `hold()` in startup; resident until killed | done | measure script (`exit=143` on SIGTERM, like Python) |
| `activate` → popup at bottom/76 | done | — |

## 2. IPC

| Item | Status | Test |
|---|---|---|
| timedate1 proxy on SYSTEM bus, `GET_INVALIDATED_PROPERTIES`, async (gio) | done (`backend.rs`) | `private_bus::backend_against_fake_timedated_on_private_bus` |
| Cached props + `Properties.GetAll` every 20 s; operation counter guards stale reads | done | `background_read_cannot_overwrite_a_more_recent_write` |
| `ListTimezones` (5 s, validated regex, sorted/deduped, stable list handle) | done | `timezones_load_async_…`, `invalid_timezone_lists_are_rejected`, `list_failure_…` |
| `SetNTP(bb)`, `SetTime(xbb)`, `SetTimezone(sb)` with `ALLOW_INTERACTIVE_AUTHORIZATION`, timeout 2³¹−1, never cancelled | done | backend tests + private bus (checks the message flag) |
| Manual writes preflighted with fresh `GetAll`; refused if NTP before/after | done | `ntp_prevents_manual_date_…`, `new_manual_setters_reject_ntp_…` |
| Owner loss → fail closed (`ntp=True, can_ntp=False, ready=False`), reconnect after 2500 ms | done | `service_loss_…`, private bus (release + re-own) |
| Error mapping (AccessDenied/…/Cancelled, AutomaticTimeSyncEnabled, cancelled) | done | `error_text_mapping`, private bus |
| `MAX_TIME_USEC`, date range 1970..9999, DST gap rejected, fold kept | done | `CalendarDateTests` ports |
| `GetPopupPlacement(ss)` group `clock` (via tessel-gtk `PopupController`) | done | `popup_calendar_browsing_…` (group) |
| Owned: only the GApplication name/object | done | — |

## 3. Files

| Item | Status | Test |
|---|---|---|
| `calendar.json`: sort_keys/indent 2/ensure_ascii=False + `\n`, `version:1`, atomic `.calendar-*`, 262144-char limit, lazy creation, last-valid-parse (ino/mtime_ns/size) | done (`preferences.rs`) | `sources_add_remove_and_validation` (exact bytes), `broken_file_keeps_last_valid_preferences`, `defaults_without_creating_a_file` |
| Rules: week_start values, kinds, 32 sources, duplicates, `webcal://`→`https://`, `file://` unquote (urlparse params/query), `~`/`~user`, relative rejected, `str(Path)` normalization | done | `file_uri_and_home_expansion`, prefs tests |
| Popup follows `calendar.json` via `Gio.FileMonitor(WATCH_MOVES)` | done | `week_start_preference_and_selected_day_events` (calls the handler) |
| `clock.json` via `suite_core::clock` (read-merge-write) | done | `clock_display_controls_save_live_…` |
| URL cache `$XDG_CACHE_HOME/tessel/calendar/<sha256[:32]>.ics`, atomic `.fetch-*` | done | `fetch::fetching_caching_and_source_errors` |
| `/etc/localtime` fallback zone | done (`DateSystem::now`) | — |
| Automatic week start (`locale -k LC_TIME`, 2 s timeout, cached, failure → Sunday) | done | `glibc_week_definitions`, `override_wins_over_locale` |

## 4. Event engine — byte-for-byte

| Item | Status | Test |
|---|---|---|
| Parser (unfolding, quoted colons, escapes, nested components, STATUS:CANCELLED, DTSTART/DTEND/DURATION, mixed types, aware/naive mixing, Python `int()`/`\d`/`strip()` Unicode semantics, ZoneInfo key validation) | done (`events.rs`, `py.rs`) | `engine_parity` parse dumps |
| RRULE FREQ/INTERVAL/COUNT/UNTIL/BYDAY(ordinals)/BYMONTHDAY/BYMONTH, EXDATE, RECURRENCE-ID, window skipping, `MAX_PERIODS`, sort order | done | `engine_parity` |
| CPython zoneinfo semantics (TZif v1–3, POSIX footer, fold, gap/repeat lookups, C `fromutc`), glibc local zone for `astimezone()` | done (`tz.rs`) | `tz::tests`, `engine_parity` (8 local zones × 18 source zones) |
| `OverflowError` exactly where Python raises (parse → "Could not read the calendar"; expand → empty) | done (`civil.rs` checked arithmetic) | `engine_parity` overflow cases |
| Fixtures: `tests/gen_fixtures.py` runs the Python engine: 3176 cases / 9701 expansions in-repo (curated full output + seeded fuzz as SHA-256); two extra 15 000-case fuzz runs (45 701 expansions each) also identical | done | `engine_matches_python_fixtures` (`CALENDAR_FIXTURES=` for extra sets) |
| URL fetch (fresh < 30 min, UA `tessel-calendar/1`, 20 s, `BEGIN:VCALENDAR` in first 4096 bytes, cache fallback), friendly errors | done (`http.rs`: gio sockets + glib-networking TLS) | `fetch` (local 127.0.0.1 server: length, chunked, close-delimited, 302, 404, loop, refused) |
| `EventStore` worker thread, generation guard, `range()` memo (>8 clears), reload every 30 min when idle | done (`store.rs`) | `store::tests::*`, GUI events test |

## 5. UI

| Item | Status | Test |
|---|---|---|
| Popup window (title, `calendarmanager` + `tray-popup`, layer shell via tessel-gtk `PopupController`, backdrops, probe), header icons/title/subtitle/settings button | done (`popup.rs`) | `popup_calendar_browsing_…`; screenshot identical to Python (`measure_headless.sh`) |
| Month grid, events list, status lines, all CSS classes/colors (`STYLE_CSS`, `SETTINGS_CSS` verbatim), companion theme attach | done (`view.rs`) | `month_grid_…`, `week_start_preference_and_selected_day_events`, `popup_without_calendars_…` |
| Keyboard: arrows, PgUp/PgDn, Ctrl+PgUp/PgDn, Home, Alt/Super ignored | done | `keyboard_navigation_moves_days_months_and_years` |
| Date Settings window (date/time/timezone/NTP, Dock clock live preview, Calendar card, file chooser) | done (`settings.rs`) | 19 settings cases ported from test_calendar.py; screenshot matches Python |
| Timers: 15 s tick, 30 s popup refresh | done | — |
| Locale names (`calendar.day_name`, `month_abbr`, `strftime`) | done (libc `strftime`) | GUI tests compare with `strftime` |

## Gaps / differences (honest list)
- HTTP: gio `GProxyResolver` decides proxies (urllib reads `*_proxy` env only); urllib's
  exotic `ValueError`s for malformed URLs (e.g. `user:pass@host` → "nonnumeric port")
  become "Could not connect". No `ftp:` redirects are fetched (urllib would).
- Corrupt `calendar.json`/`clock.json`: the JSON error *text* shown in the settings error
  label is serde_json's, not Python's `json` message (same as tessel-core's clock store).
- Argument errors forwarded to an already-running primary are printed on the
  launcher's stderr (Python printed them on the primary's stderr).
- Python type errors that Rust's types make impossible (`set_ntp("yes")`, `set_date("…")`,
  aware `time`) have no runtime equivalent; those test lines are dropped.
- `install_waybar.py` (installer, update/rollback) is not ported here — the parent's
  install step owns it; `test_install_waybar.py` therefore has no Rust counterpart.
- Expansion results containing integers beyond i64 (e.g. INTERVAL ≥ 2⁶³) saturate instead
  of Python big ints; every such case overflows the same way in both.

## Compat helpers implemented locally (upstream candidates)
`tz` (CPython-exact zoneinfo + glibc local zone), `civil` (Python date/datetime/timedelta
arithmetic with OverflowError points), `py` (Python `int()`, `isdigit`/`isdecimal`/`isspace`
tables from CPython 3.14), `http` (small gio HTTP GET), `view::strftime` (libc, locale-aware;
`ClockTime` impls), `preferences::os_error_text` (overlaps the new `suite_core::pyerr`),
`view::remove_source` (overlaps `suite_core::glib_compat::source_remove`).
