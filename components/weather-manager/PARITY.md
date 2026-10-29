# weather-manager — parity with contracts/weather-manager.md

Crate `components/weather-manager`: binaries `weather-manager` (= `weather_manager.py`:
popup, settings, `--tray`) and `weather-status` (= `weather_status.py`, no GTK).
Tests: `cargo test -p weather-manager` (31 unit tests incl. all of test_weather_backend.py,
plus `tests/private_bus.rs` on a `gio::TestDBus`), `tests/run_headless.sh` (all of
test_weather_ui.py in a private nested headless labwc), `tests/measure_headless.sh`
(Rust vs Python timing/RSS/screenshots). `WS=<dir>` lets the scripts build from another
workspace root. No test touches the network: fixtures, fake fetchers, a fresh pre-written
cache; `fixtures/open-meteo-forecast.json` is a real reply copied from an existing cache entry.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Contract item | | Test |
|---|---|---|
| GtkApplication `org.waydock.Weather`, `HANDLES_COMMAND_LINE`, prgname = id | ✅ | ui `singleton_cli_and_shared_settings` (real binary forwards to the primary) |
| CLI `--popup`\|`--settings`\|`--tray` (exclusive), `--popup-edge`, `--popup-offset ≥ 0`; argparse text/abbreviations/exit 2, prevalidated locally and re-parsed in the primary | ✅ | `compat::argparse::tests::*`, ui singleton (`--popup-offset -1` → 2) |
| Routing: popup create / same position → toggle / other position → `set_position`+show; `--settings` dismisses popup, single window; `--tray` hold + SNI | ✅ | ui `real_click_away_escape_and_popup_settings_route`, singleton |
| Popup not held: hidden (not `show_pending`) → destroyed on idle → process exits; tray resident | ✅ | ui `pending_fetch_is_discarded_…` (no windows left), measure (tray stays) |
| "Use current location" → settings `locate_when_ready` (portal only after explicit press) | ✅ | ui `no_location_prompts_setup_without_fetching`, `settings_locate_request_waits_for_availability` |
| `weather_status.py --watch [--config] [--cache-dir]` | ✅ | private_bus `status_binary_*`; measure: Rust and Python streams byte-identical |

## §2 IPC
| Item | | Test |
|---|---|---|
| SNI `/StatusNotifierItem` props (Category, Id, Title, Status, WindowId, IconName, IconPixmap, ToolTip `(sa(iiay)ss)` html-escaped, ItemIsMenu, Menu), Activate/SecondaryActivate/ContextMenu/Scroll, NewIcon/NewToolTip/NewTitle on every update | ✅ | private_bus `tray_exports_sni_and_dbusmenu` |
| dbusmenu `/Menu` Version 3…, GetLayout rev 1 (children `av`), GetGroupProperties (filtered, unknown ids skipped), GetProperty (unknown → InvalidArgs), Event/EventGroup (unknown ids returned), AboutToShow(Group) | ✅ | same |
| Watcher registration on every appearance, path only, NO_AUTO_START 3 s | ✅ | private_bus `tray_registers_with_watcher` |
| logind PrepareForSleep(false) → reschedule in 5 s | ☑ | |
| Location portal: GeoClue2 in ListNames/ListActivatableNames + portal `version`; CreateSession(accuracy 2, `weather_<hex>` tokens) → Start; result only after Response 0 **and** a valid position; 45 s timeout; Close Request & Session, incl. late handles after cancel | ✅ | private_bus (all 5 PortalTests); `location::tests::*` |
| GetPopupPlacement via shared PopupController group `weather` | ☑ | suite-gtk |
| Open-Meteo forecast/geocoding: exact urlencode order, UA, Accept, 8 s, ≤256 KiB, NaN/Infinity rejected, HTTP code / connection messages | ✅ | `query_requests_*`, `geocoding_*`, `http_response_is_bounded_*`; one live fetch verified manually |

## §3 Files
| Item | | Test |
|---|---|---|
| config.json path (`XDG_CONFIG_HOME` only if absolute), byte-identical `json.dump` (no newline), field order, `.weather-*.tmp` atomic + fsync, dir 0700, ≤8192 B, legacy defaults, preferences-only files, all validation messages | ✅ | `configuration_atomic_roundtrip_*`, `existing_configs_*`, `invalid_config_is_not_saved`, `configured_location_*` |
| Cache key = sha256(`json.dumps(asdict, sort_keys=True)`) incl. int/float text | ✅ | `cache_key_and_file_match_python` (matches live Python-written cache file names) |
| Cache entry format/order (Python dict-merge order on failure), max 8 files, 64-hex only, `fetch.lock` flock spin 50 ms ≤10 s, early return "Weather refresh is in progress." | ✅ | `concurrent_clients_share_single_network_request`, `cache_file_count_*`, `failed_initial_request_*` |
| Fresh/TTL/same local date, 60 s back-off, 7-day stale, force reuses a newer fetch, corrupt timestamps, refresh_delay | ✅ | `shared_cache_*`, `offline_*`, `malformed_*`, `corrupt_cache_*`, `refresh_delay_*` |

## §4–5 Status JSON and UI
Status payloads (unconfigured/unavailable/ok/stale, symbols, C-locale "Updated %a %H:%M" in the
location zone, settings error suffix): ✅ `status_has_*`, private_bus, measure (identical streams).
Popup (all cards, CSS classes and CSS text verbatim, 2 s tick, 60 s re-read, worker generations,
stale/offline text, 7 `weather-day` rows + tooltips), Settings window/widget (search, results,
manual entry, units/week/refresh save-on-change with read-merge-save, current location flow,
`search_targets`), tray (one timer, file monitors with 250 ms debounce, own cache file only):
✅ ui_headless (12 checks = all WeatherUITests + CSS parse). measure_headless screenshots of the
Rust and Python popup (and tray-started popup) are **pixel-identical**.

## Deviations ⚠ / gaps ❌
1. ⚠ D-Bus via gio (like power-manager), HTTPS via `ureq` + system OpenSSL (`native-tls`,
   proxy env honoured like urllib).
2. ⚠ GtkApplication (not lazy GTK): every primary shows a window anyway; launcher processes
   forward argv without initializing GTK.
3. ⚠ `json.loads` error texts reproduced for the common cases only (`Expecting value: line L column C (char N)`,
   `Extra data…`, UTF-8 decode); other malformed-config messages differ in wording.
4. ⚠ A response with an out-of-range number (`1e400`) is "invalid data" (Python: accepted, then
   "incomplete forecast"). Integers beyond i64 in a cached response round-trip as floats.
5. ⚠ Zone keys are resolved like `ZoneInfo` (TZPATH/PYTHONTZPATH, TZif magic); the pip `tzdata`
   fallback is not consulted.
6. ❌ Global Settings' `weather_page.py` still embeds the Python `WeatherSettings`; the Rust
   widget (`settings::WeatherSettings`, same `search_targets`) is ready for the GS port.
7. ❌ `lock_weather.py` (lock screen) still imports Python `weather_backend`; keep those modules installed.
