# tessel-calendar — external contract

Source: `~/tessel-bluetooth/tessel-calendar/` (5019 LOC incl. tests; runtime ≈ 3.1k). On-demand GTK4
app (no resident service, no systemd unit). Refs are `file:line` relative to that folder
unless prefixed.

## 1. Entry points / processes

| Item | Value | Ref |
|---|---|---|
| Executable | `python3 ~/.config/waybar/scripts/tessel-calendar/calendar_manager.py` | install_waybar.py:19,69 |
| GApplication id | `org.tessel.CalendarManager` (owns that session bus name; `HANDLES_COMMAND_LINE`; prgname = app id) | calendar_manager.py:29,47-49 |
| CLI | `--popup` (default) \| `--settings` (mutually exclusive); `--popup-edge {top,bottom,left,right}` default `bottom`; `--popup-offset INT>=0` default `76` (negative → argparse error, exit 2) | calendar_manager.py:32-42 |
| Single instance | Every launch forwards argv to the primary via `do_command_line`; `do_local_command_line` returns False. Remote `--popup` with *different* edge/offset destroys and rebuilds popup; same position → `PopupController.activate()` (toggles: visible→dismiss, hidden→show; a visible child window is raised instead). `--settings` dismisses popup, reuses one `DateSettingsWindow`. | calendar_manager.py:80-136 |
| Lifetime | `hold()` in startup → process stays resident after first launch until killed (popup hidden, not destroyed). | calendar_manager.py:65-66 |
| Launched by | tessel-dock `panel_actions.APPLICATIONS["calendar"]` → `systemd-run --user --quiet --collect --service-type=exec --slice=app.slice --expand-environment=no --setenv=… -- <python> <script> --popup --popup-edge E --popup-offset N` (left click) or `… --settings` (right click) | ~/tessel-bluetooth/tessel-dock/panel_actions.py:23,28-46,74-83 |
| Legacy waybar | `custom/clock.on-click` = launcher + ` --popup --popup-edge {top for config-4k-uw, else bottom} --popup-offset 76 </dev/null >/dev/null 2>&1 &` (background so Waybar 0.15 keeps polling) | install_waybar.py:69-90 |
| Installer | `install_waybar.py [--update] [--apply]`: preview manifest `$XDG_STATE_HOME/tessel/previews/tessel-calendar.json`; copies RUNTIME list (incl. shared `gtk4_utils, companion_theme, suite_i18n, language_data, suite_catalogs, clock_preferences, background_blur.py, libwaydock-background-blur.so, popup_placement`) into `~/.config/waybar/scripts/tessel-calendar/`; backups in `~/.config/waybar/backups/calendar-<ts>/`; on update SIGTERMs running `calendar_manager.py` processes and SIGUSR2s waybar. | install_waybar.py:20-38,109-190 |
| install_all step | `Step("calendar", …, kind="waybar", update=False)` | ~/tessel-bluetooth/install_all.py:78 |
| Embedded elsewhere | Global Settings → Date & Time page imports `DateSettingsWindow` + `DateBackend` directly (adds tessel-calendar dir to sys.path; refresh tick 15 s while active) | ~/tessel-bluetooth/tessel-settings/applet_pages.py:13-21,168-200 |

No .desktop file, no keybind, no systemd unit.

## 2. IPC

### Consumed: `org.freedesktop.timedate1` (SYSTEM bus) — date_backend.py
- Proxy `org.freedesktop.timedate1` `/org/freedesktop/timedate1` iface `org.freedesktop.timedate1`, flags `GET_INVALIDATED_PROPERTIES`, async (date_backend.py:14-17,174-200).
- Properties read: `Timezone s`, `NTP b`, `CanNTP b`, `NTPSynchronized b` (cached props + explicit `org.freedesktop.DBus.Properties.GetAll(s)` every 20 s because timedated doesn't signal everything) (:277-345).
- Methods: `ListTimezones() → as` (5 s timeout, validated with regex `[A-Za-z0-9_+-]+(/…)*`, sorted/deduped, list object kept stable) (:214-250); `SetNTP(bb)` = (enabled, interactive=true); `SetTime(xbb)` = (usec_utc, relative=false, interactive=true); `SetTimezone(sb)` = (name, true). Writes use `ALLOW_INTERACTIVE_AUTHORIZATION` and timeout `2**31-1` (polkit dialog) (:348-470).
- Manual writes (`SetTime`, `SetTimezone`) are preflighted with a fresh `GetAll`; refused if NTP true (before and after preflight) (:385-433).
- Name-owner loss → `_unavailable`: state fail-closed (`ntp=True, can_ntp=False, ready=False`), reconnect after 2500 ms (:251-271).
- Error mapping (`_error_text` :152-166): remote names containing AccessDenied/NotAuthorized/InteractiveAuthorizationRequired/Cancelled/Canceled → "Authorization was canceled or denied. No change was applied."; AutomaticTimeSyncEnabled → "Turn off automatic date and time before changing the date."
- `MAX_TIME_USEC` = (((2^63-1)//1e9 − 30·365·86400)·1e6 − 1) (kernel TIME_SETTOD_SEC_MAX) (:22-23).

### Consumed: `org.tessel.Dock` (session) — shared popup_placement.py
- `org.tessel.Settings.GetPopupPlacement(ss connector, group="clock") → s JSON {"edge","offset","align"}`, path `/org/tessel/Settings`, `NO_AUTO_START`, 500 ms; invalid/absent → use CLI edge/offset. ~/tessel-bluetooth/shared/popup_placement.py:69-90; group set at calendar_popup.py:80.

### Owned
Only the GApplication name `org.tessel.CalendarManager` (+ standard `org.gtk.Application`/`org.gtk.Actions` on `/org/tessel/CalendarManager`). No custom interfaces.

## 3. Files

| Path | Format | Writer / readers | Ref |
|---|---|---|---|
| `$XDG_CONFIG_HOME/tessel/calendar.json` | JSON object, `sort_keys, indent=2, ensure_ascii=False` + `\n`; atomic mkstemp(`.calendar-*`)+fsync+replace; `version:1` added on write; max 262144 bytes; not created until changed; readers keep last valid parse (signature ino/mtime_ns/size) | calendar app (DateSettings "Calendar" section), Global Settings Date page (same widget). Popup follows via `Gio.FileMonitor(WATCH_MOVES)` → reload sources + first weekday | calendar_preferences.py:22-178; calendar_manager.py:97-112 |
| `$XDG_CONFIG_HOME/tessel/clock.json` | JSON `{stacked,date_first,use_24_hour,date_order∈mdy/dmy/ymd/ydm/myd/dym,date_separator∈/ - .,show_date,show_year,show_weekday,month_name,short_names,version:1}`; same atomic scheme (`.clock-*`), 65536 limit, read-merge-write | Owned by `shared/clock_preferences.py` (port into tessel-core). Written by DateSettings "Dock clock" section and Global Settings layout page; read by tessel-dock panel_ui clock | ~/tessel-bluetooth/shared/clock_preferences.py:14-139; date_settings.py:21,56; ~/tessel-bluetooth/tessel-dock/panel_ui.py:228,513; ~/tessel-bluetooth/tessel-settings/layout_page.py:131 |
| `$XDG_CACHE_HOME/tessel/calendar/<sha256(url)[:32]>.ics` | raw subscription bytes, atomic (`.fetch-*`) | calendar app only | calendar_events.py:32-34,393-433 |
| `/etc/localtime` | fallback zone when timedated has no name | read | date_backend.py:100-110 |

calendar.json example:
```json
{
  "sources": [{"kind": "url", "location": "https://example.org/a.ics", "name": ""},
              {"kind": "file", "location": "/home/u/cal.ics", "name": "Work"}],
  "version": 1,
  "week_start": "monday"
}
```
Rules: `week_start ∈ automatic|sunday|monday|saturday` (else automatic); `kind ∈ file|url`; max 32 sources; duplicates by `location` rejected; `webcal://` → `https://`; `file://` URI unquoted; `~` expanded; relative path rejected ("Use a full file path or a web address.") (calendar_preferences.py:16-19,55-72,159-178).

Automatic week start: `locale -k LC_TIME`, `week-1stday` (default 19971130) + `first_weekday`−1, cached per process; failure → Sunday (:26-52).

## 4. Event engine (calendar_events.py) — must be byte-for-byte equivalent in results
- Own iCalendar parser (no EDS): line unfolding, escapes, VEVENT all-day (`VALUE=DATE`) / timed, `TZID` and `Z` → local, `DURATION`, `RRULE` FREQ daily/weekly/monthly/yearly + INTERVAL, COUNT, UNTIL, BYDAY (ordinals e.g. `-1FR`), BYMONTHDAY, BYMONTH; `EXDATE`; `RECURRENCE-ID` overrides (:69-391). `MAX_PERIODS=20000`, `MAX_BYTES=16 MiB` (:27-29).
- URL fetch: cache fresh < 30 min used directly; else GET with `User-Agent: tessel-calendar/1`, timeout 20 s; body must contain `BEGIN:VCALENDAR` in first 4096 bytes (case-insens.); any failure falls back to cached copy, else raise (:409-433).
- Friendly per-source errors: "File not found", "Permission denied", "Server error N", "Could not connect", ValueError text, "Could not read the calendar" (:436-448).
- `EventStore`: worker thread load, generation guard, `range()` memo (cleared at >8 entries); app reloads every 30 min if sources and not loading (calendar_manager.py:71-72,114-118; calendar_events.py:462-532).

## 5. UI surfaces
- Popup: `Gtk.ApplicationWindow` title "Calendar"; shared `PopupController` (tessel-gtk tray_popup): layer-shell namespace `desktop-applet`, layer OVERLAY, exclusive zone −1, keyboard ON_DEMAND, anchored to panel edge with margin=offset and to the right/bottom side (align `end`, margin 12), or from dock placement; per-monitor transparent click-catcher layers namespace `desktop-applet-dismiss` (all four anchors, keyboard NONE, CSS class `desktop-applet-backdrop`); output chosen with a 1×1 input-transparent probe surface namespace `suite-output-probe` waiting for `wl_surface.enter`. Escape/outside click/close-request → hide. Monitor list change → hide. ~/tessel-bluetooth/shared/tray_popup.py:15-265, popup_placement.py:10-66.
- Popup content: header (icons `view-calendar-symbolic`/`x-office-calendar-symbolic`, title "Calendar", settings button `emblem-system-symbolic` "Date settings"), month grid, selected-day event list, status lines ("No calendars added. Add one in Date settings.", "Loading events…", "No events", "Some calendars could not be loaded.") (calendar_popup.py:35-160).
- CSS classes (window class `calendarmanager`, also `tray-popup`): `calendar-view, calendar-month, calendar-weekday, calendar-day(.today .selected .adjacent .has-events), calendar-footer, calendar-tile, icon-button, popup-surface, event-list, event-row, event-time, event-title, event-location, section-title, muted, card, status, error, title, subtitle, primary, clock-preview-inline`; colors `@calendar_accent/_surface/_muted/_line` (calendar_view.py:34-130). Theme via tessel-dock `companion_theme.attach` (calendar_view.py:26-31).
- Keyboard: arrows ±1/±7 days, PgUp/PgDn ±month, Ctrl+PgUp/PgDn ±year, Home → today (calendar_view.py:343-359).
- Settings window: `Gtk.ApplicationWindow` title "Date Settings" (headerbar), sections: date/time/timezone/NTP, Dock clock (live preview), Calendar (week start, sources) (date_settings.py:41-634). Raise on Wayland via `wlrctl toplevel focus app_id:org.tessel.CalendarManager title:<title>`, retried up to 4× every 100 ms (calendar_manager.py:187-213).
- Timers: 15 s UI tick while visible (:67,176-183).

## 6. External commands
`locale -k LC_TIME` (timeout 2 s); `wlrctl toplevel focus …` (optional); installer: SIGTERM/SIGUSR2 via /proc scan.

## 7. Parity test cases (from tests)
test_calendar_events.py: glibc week definitions (`week-1stday`+`first_weekday`); explicit override beats locale; defaults without creating file; add/remove/validate sources; broken file keeps last valid prefs; file URI + `~`; all-day/timed/folded/escaped; malformed events skipped; UTC & TZID → local; daily interval+COUNT; weekly BYDAY+UNTIL+EXDATE; COUNT counted from DTSTART not window; monthly ordinal weekday + last day; yearly birthday far from start & Feb 29; recurrence follows source zone across DST; RECURRENCE-ID replaces instance; URL cache reused while fresh and used offline; non-calendar response rejected; off-thread loading with per-source failures.
test_date_backend.py: date change keeps local wall time with target-date DST offset; DST gap rejected; repeated hour keeps fold; date type/range limits (1970..9999, MAX_TIME_USEC); entered time replaces all fields; aware/invalid time rejected; start() never writes; cached reads/external changes never write; SetNTP interactive w/o optimistic state; date write re-reads props and uses latest wall clock; NTP blocks manual date before & after preflight; denied auth leaves state; service loss blocks controls & ignores stale reply; close ignores stale preflight/write; concurrent writes rejected; missing properties fail closed; background read can't overwrite newer write (operation counter); timezone list async, stable list object; list failure keeps clock usable then recovers; stale tz-list reply after close ignored; SetTimezone `(sb)` interactive; tz rejects paths/unknown/unlisted; datetime uses entered seconds & latest tz.
test_calendar.py: month grid leap years/Dec rollover; browsing never changes system date; today tracks clock w/o losing selection; clock display controls save live, failed save reverts & reports, toggles preserve hidden format; NTP blocks manual apply; manual date/time require explicit Apply (time edit applies date+time together); unedited time tracks clock, unsaved edits survive refresh; typed time validation; pending timezone blocks time apply, survives auth failure/external changes, can be cancelled; async apply disables controls; popup settings button reuses window & backend; no-calendar hint; week start + selected-day events; remote activation routes popup/settings.
test_install_waybar.py: bundles shared clock_preferences; all companion installers ship identical shared helper; update rollback semantics.
