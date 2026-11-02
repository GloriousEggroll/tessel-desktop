# tessel-weather — external contract

Source: `~/tessel-bluetooth/tessel-weather/` (2843 LOC incl. tests; runtime ≈ 1.9k). On-demand GTK4
popup/settings app + a JSON status streamer + optional SNI tray mode. Data from Open-Meteo.
Refs are `file:line` relative to that folder unless prefixed.

## 1. Entry points / processes

| Item | Value | Ref |
|---|---|---|
| App executable | `python3 ~/.config/waybar/scripts/tessel-weather/weather_manager.py` | ~/tessel-bluetooth/tessel-dock/panel_actions.py:24; install_weather.py:17 |
| GApplication id | `org.tessel.Weather` (session bus name; `HANDLES_COMMAND_LINE`; prgname = id) | weather_manager.py:18,37-39 |
| CLI | exactly one of `--popup` (default) \| `--settings` \| `--tray`; `--popup-edge {top,bottom,left,right}` default `bottom`; `--popup-offset INT>=0` default `76` | weather_manager.py:21-32 |
| Routing | `--tray`: first time → `hold()` + create `WeatherTray(edge, offset)`; `--settings` → dismiss popup, present single `WeatherSettingsWindow`; popup: create if none; if edge/offset differ → `controller.set_position()` + show; else `activate()` (toggle) | weather_manager.py:51-82 |
| Lifetime | Popup mode is NOT held: when the popup hides (and `controller.show_pending` false) it is destroyed on idle → process exits once no windows. Tray mode stays resident. | weather_manager.py:84-95; README "There is no weather background service" |
| Locate route | popup "Use current location" → `show_settings(locate=True)` → `settings.locate_when_ready()` (portal request only after explicit press) | weather_manager.py:75-77,97-108 |
| Status streamer | `python3 -u ~/.config/waybar/scripts/tessel-weather/weather_status.py --watch [--config PATH] [--cache-dir DIR]`; spawned and line-read by tessel-dock status_backend (stream mode) | weather_status.py:56-80; ~/tessel-bluetooth/tessel-dock/status_backend.py:31,269-273 |
| Launched by tessel-dock | `panel_actions` → `systemd-run --user … -- <python> weather_manager.py --popup --popup-edge E --popup-offset N` (left) / `--settings` (right) | ~/tessel-bluetooth/tessel-dock/panel_actions.py:28-46 |
| Tray mode install (Home Assistant screen bundle) | user unit `~/.config/systemd/user/tessel-weather.service` (`ExecStart=/usr/bin/python3 %h/.local/share/tessel-weather/weather_manager.py --tray --popup-edge top --popup-offset 40`, Restart=on-failure, RestartSec=3, `GDK_BACKEND=wayland`, Nice=5, PartOf=graphical-session.target); `~/.local/share/tessel-weather/autostart.sh` (import-environment + start unit); `~/.config/autostart/tessel-weather.desktop` and `~/.local/share/applications/tessel-weather.desktop` (Name=Weather, Icon=weather-few-clouds) | ~/tessel-bluetooth/home-assistant-screen/install_remote.py:13-80 |
| Updater | `install_weather.py [--apply]`: copies `weather_*.py` into existing `~/.config/waybar/scripts/tessel-weather/` + `tessel-settings/settings_search.py` into `~/.local/share/tessel-settings/`; refuses symlinks; backups `~/.local/state/tessel/backups/weather-location-<UTC>/` | install_weather.py:10-48 |
| Embedded elsewhere | Global Settings → Weather page uses the same `WeatherSettings` Gtk.Box; `search_targets = {location, units, forecast, refresh, current_location}` | ~/tessel-bluetooth/tessel-settings/weather_page.py:7-18; weather_settings.py:117-119 |

## 2. IPC

### Owned (tray mode only), on the app's session connection (unique name; well-known `org.tessel.Weather`)
- `/StatusNotifierItem` iface `org.kde.StatusNotifierItem` (weather_tray.py:9-29,138-148):
  props `Category s="ApplicationStatus"`, `Id s="tessel-weather"`, `Title s`, `Status s="Active"`, `WindowId u=0`, `IconName s`, `IconPixmap a(iiay)=[]`, `ToolTip (sa(iiay)ss)=(icon,[],escape(title),escape(description))`, `ItemIsMenu b=false`, `Menu o="/Menu"`; methods `Activate(ii)` → popup, `SecondaryActivate(ii)`/`ContextMenu(ii)` → settings, `Scroll(is)` no-op; signals `NewIcon`, `NewToolTip`, `NewTitle` (emitted together on every update, :251-252).
- `/Menu` iface `com.canonical.dbusmenu`: props `Version u=3`, `TextDirection "ltr"`, `Status "normal"`, `IconThemePath as=[]`; `GetLayout(iias)→(u(ia{sv}av))` revision 1, root id 0 `children-display=submenu`, items 1 "Weather" (`weather-few-clouds`), 2 "Weather Settings" (`preferences-system`), 3 "Quit" (`application-exit`), each `enabled/visible=true`; `GetGroupProperties`, `GetProperty`, `Event`/`EventGroup` (`clicked` → action; unknown ids returned as errors), `AboutToShow→false`, `AboutToShowGroup→([],unknown)`; signal `LayoutUpdated(ui)` declared only. Bad args → `org.freedesktop.DBus.Error.InvalidArgs` (:150-205).
- Registration: watch `org.kde.StatusNotifierWatcher`; on each appearance call `RegisterStatusNotifierItem("/StatusNotifierItem")` (path only, so sender's unique name is used), `NO_AUTO_START`, 3 s (:59-60,125-136).

### Consumed
| Service | Use | Ref |
|---|---|---|
| system `org.freedesktop.login1.Manager.PrepareForSleep(b)` | tray: on resume (false) reschedule poll in 5 s | weather_tray.py:98-113 |
| session `org.freedesktop.portal.Desktop` `/org/freedesktop/portal/desktop` `org.freedesktop.portal.Location` | availability: system `ListNames`+`ListActivatableNames` must include `org.freedesktop.GeoClue2` (1.5 s, NO_AUTO_START) then `Properties.Get(Location, version)` (2 s). Request: `CreateSession(a{sv}{session_handle_token:'weather_<hex>', accuracy:u 2 (CITY)})→o`, `Start(o session, s '', a{sv}{handle_token})→o`; subscribe `Location.LocationUpdated(o session, a{sv})` (Latitude/Longitude), `Request.Response(u, a{sv})`, `Session.Closed`; result only after BOTH Response==0 and a position; timeout 45 s; always `Close` Request & Session (NO_AUTO_START, 1.5 s), incl. late handles after cancel | weather_location.py:12-180 |
| `org.tessel.Settings.GetPopupPlacement(ss connector, "weather")` | popup placement (tessel-gtk popup_placement) | weather_ui.py:168; ~/tessel-bluetooth/shared/popup_placement.py:69-90 |
| HTTPS Open-Meteo | forecast `https://api.open-meteo.com/v1/forecast`; geocoding `https://geocoding-api.open-meteo.com/v1/search?name=Q&count=10&language=en&format=json`; headers `User-Agent: Tessel-Weather/1.0`, `Accept: application/json`; timeout 8 s; body ≤ 256 KiB; NaN/Infinity rejected | weather_backend.py:24-31,244-299 |

Forecast query params (exact, urlencode order): `latitude, longitude, timezone, current=temperature_2m,weather_code,is_day,apparent_temperature,relative_humidity_2m,wind_speed_10m, daily=temperature_2m_min,temperature_2m_max,weather_code,precipitation_probability_max, temperature_unit=<units>, wind_speed_unit=mph|kmh, forecast_days=16`, plus `past_days=7` only for week=current (:244-255). Current-location timezone lookup: `latitude,longitude (rounded 2dp), timezone=auto, current=temperature_2m, forecast_days=1` → name "Current location" (weather_location.py:48-57).

## 3. Files

| Path | Format | Readers/writers | Ref |
|---|---|---|---|
| `$XDG_CONFIG_HOME/tessel-weather/config.json` (XDG var used only if absolute) | `json.dump(asdict(WeatherConfig), ensure_ascii=False, allow_nan=False)` — default separators, field order `name, latitude, longitude, timezone, units, refresh_minutes, week`, NO trailing newline; atomic NamedTemporaryFile `.weather-*.tmp` + fsync + replace; dir mode 0700; ≤ 8192 bytes | written by WeatherSettings (standalone + Global Settings), merged with latest file before save; read by popup (2 s tick), tray (file monitor, 250 ms debounce), weather_status (every 2 s), tessel-lock `lock_weather.py`, Global Settings night_light (lat/lon for sunrise) | weather_backend.py:161-231; ~/tessel-bluetooth/tessel-settings/night_light.py:56-83; ~/tessel-bluetooth/tessel-lock/lock_weather.py:10-30 |
| `$XDG_CACHE_HOME/tessel-weather/<sha256(json(asdict(config),sort_keys))>.json` | `{"version":1,"config":{…},"response":<raw Open-Meteo JSON>,"fetched_at":float,"attempted_at":float,"error":null|str}`; max 8 files (current + 7 newest by mtime; only 64-hex stems pruned) | shared by every process (popup, status, tray, lock screen, HA screen) | weather_backend.py:353-445 |
| `$XDG_CACHE_HOME/tessel-weather/fetch.lock` | `flock(LOCK_EX|LOCK_NB)` spin 50 ms up to 10 s; if a cached response exists return it with "Weather refresh is in progress." instead of waiting | cross-process single fetch | weather_backend.py:394-405 |

Example config (live): `{"name": "Englewood, Colorado, United States", "latitude": 39.64777, "longitude": -104.98776, "timezone": "America/Denver", "units": "fahrenheit", "refresh_minutes": 60, "week": "upcoming"}`

Config semantics (weather_backend.py:62-231):
- Defaults: `name=""` (no default city), lat/lon 0, `timezone="UTC"`, `units` from locale (LC_ALL → LC_MEASUREMENT → LANG; territory US/LR/MM → fahrenheit else celsius), `refresh_minutes=60` (15..1440), `week="upcoming"|"current"`.
- Legacy: file containing name+lat+lon but missing `timezone`/`units` → `America/Denver` / `fahrenheit`; file lacking a location keeps only preferences, `units` default `fahrenheit`.
- Validation messages are user-visible (e.g. "Enter a location name of 1 to 160 characters.", "The refresh interval must be between 15 and 1440 minutes.").
- `configured()` returns a config only if file has name/lat/lon keys and is valid.

Cache/refresh rules: fresh = `0 ≤ now−fetched < refresh_minutes·60` AND same local date (location tz); retry back-off 60 s after failed attempt; stale data shown up to 7 days; `force` refresh bypasses TTL but reuses a fetch completed after the force started. `refresh_delay` = TTL remainder (or 60 s when error/stale) capped by next local midnight +1 s, clamped 5 s..1440 min (:448-464).

## 4. weather_status.py JSON (one line per change, flush; exits 0 on BrokenPipe)
| State | Payload |
|---|---|
| no location | `{"text":"󰖐","tooltip":"Weather\nChoose a location in Weather Settings.","class":["weather","unconfigured"],"alt":"unconfigured","icon":"weather-few-clouds-symbolic"}` |
| no current | `{"text":"󰖐 —","tooltip":"<name>\n<error or Weather unavailable>","class":["weather","unavailable"],"alt":"unavailable","icon":"weather-severe-alert-symbolic"}` |
| ok | `text = "<symbol> <temp>"` (☀ ☾ ⛅ ☁ ≋ ❄ ϟ ☂ by icon), tooltip lines name / "desc · temp" / "Feels like T" / "Updated %a %H:%M" (location tz) / stale line / error; `class ["weather"]` or `["weather","stale"]`; `alt`=description; `icon` |
Store read error appends `"\nSettings could not be read: <err>"`. `--watch` loops every 2 s, prints only on change (weather_status.py:16-80). Strings in status are NOT translated (tessel-dock translates/plain-texts).

Icon/description tables for WMO codes and `format_temperature` ("—" or `"{round}°F|C"`) at weather_backend.py:467-496 — must be identical.

## 5. UI surfaces
- Popup: `Gtk.ApplicationWindow` title "Weather", class `weathermanager` + `tray-popup`, shared `PopupController` group `weather` (layer-shell OVERLAY, ns `desktop-applet`, excl −1, keyboard ON_DEMAND, click-catchers ns `desktop-applet-dismiss`, probe `suite-output-probe`; see tessel-calendar.md §5). Header: icon `weather-few-clouds-symbolic`, title, subtitle = location or "No location chosen", buttons refresh `view-refresh-symbolic` "Refresh weather" & `emblem-system-symbolic` "Weather settings". Cards: setup ("Choose a location", "Use current location", "Choose location…" suggested-action), current conditions, 7 day rows `weather-day` (tooltip "desc · Chance of precipitation: N%"), message, attribution "Weather by Open-Meteo". While visible: 2 s tick checks config; re-reads cache once per 60 s; hidden → cancel worker. (weather_ui.py:86-346)
- CSS classes: `weathermanager, popup-surface, card, row-title, row-detail, section-title, icon-button, weather-day, weather-search-results, title, subtitle, suggested-action`.
- Settings window `Gtk.ApplicationWindow` "Weather Settings" (headerbar). Widgets: location card (current-location button + Cancel + status, search entry "Search for a city" + Search button + results list, manual Expander lat/lon/tz/name "Use this location"), forecast card (units dropdown F/C, week dropdown, refresh spin). Every control change saves immediately via read-merge-save; status "Weather settings saved." (weather_settings.py:12-339).
- Worker: thread per request with generation counter; stale results dropped; closing never blocks (weather_ui.py:56-83).
- Tray (SNI) icon name follows `weather_icon`, title `"<name> · <temp>"`, tooltip description (+ "Saved weather · waiting for an update" if stale/error). Monitors config file and cache dir (only its own cache filename, ignored while its own fetch pending). One timer for next refresh (weather_tray.py:62-250).

## 6. External commands
None (pure HTTP + D-Bus). Installers only copy files.

## 7. Parity test cases
test_weather_backend.py: default config neither writes nor locates; `configured()` rejects missing/partial/invalid; atomic round-trip, bad file keeps defaults + `store.error`; no default city & units by measurement locale; legacy files keep values; refresh_delay TTL/retry/local-midnight; invalid config not saved; Sunday-first current/upcoming weeks across month/year; query has 16 days, units, tz, past_days only for current; forecast real Sunday–Saturday dates with `past` flags; week uses location date not machine tz; missing daily values → unavailable (not 0/wrong day); shared cache TTL, config change, manual refresh; offline keeps last good, stale, backs off; failed first request → unavailable, no retry every poll; malformed response preserves last good; cache count bounded, unrelated files untouched; concurrent clients → single network request (flock); geocoding explicit/bounded, distinguishable names (`name, admin1, country` de-duplicated); status payload units/day-night icon/description; HTTP body bound + non-finite JSON rejected; corrupt cache timestamps trigger refresh.
test_weather_location.py: coordinates validated & rounded to 2dp; tz from detected location; position requires Response==0, session closed after result; denial/timeout/invalid coords; timeout closes pending request; cancel during CreateSession closes late session without callback.
test_weather_ui.py: current-location success/denial/manual; unavailable/slow location doesn't override manual choice; current+7-day render, units, stale/offline; pending fetch discarded on config change & dismissal; no location → setup without fetching; tray schedules one refresh & follows config file; locate waits for availability probe; search/selection/preferences/manual validation; old search query & closed page ignored; real click-away/Escape/settings route; singleton CLI + shared settings widget.
