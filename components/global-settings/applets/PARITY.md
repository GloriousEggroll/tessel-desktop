# gs-applets — parity with applet_pages.py, weather_page.py, language_page.py, capture_page.py

Public API used by the shell: `SECTIONS`, `build_page(section, &Host)`, plus `build_page_with(section, &Host,
&Factories)` (Python's `factories` dict) and `APPLET_SECTIONS` (`applet_pages.BUILDERS`). Each page stores a
typed handle in `SettingsPage.controller` (`AudioPage`, `BluetoothPage`, `NetworkPage`, `PowerPage`, `BatteryPage`,
`DatePage`, `KeepAwakePage`, `NotificationsPage`, `WeatherPage`, `CapturePage`; the language page stores the
`language_manager::ui::LanguageWizard` itself, as Welcome requires).

Tests: unit `cargo test -p gs-applets` (3) and the headless binary `applets_headless` (16 UI checks + 5 parity
scenarios), run with
`AGENT=gs-applets components/global-settings/tests/headless.sh target/debug/deps/applets_headless-<hash>`.

## Contract items

| Item | Done | Test |
|---|---|---|
| `_embed`: wrapper copies CSS classes except `waydock-companion-*`, `background`, `tray-popup` | yes | audio_…(`audiomanager`), keepawake_…(`keep-awake`) |
| `_embed`: window child detached, `popup-surface` removed, hexpand/vexpand | yes | keepawake_…, multiple_embedded_… |
| `_embed`: footer moved to the page bottom, `global_style.footer` + `real_error`, margins 24/16/8 | yes | audio_…, keepawake_…, notifications_… |
| `_embed`: `margin`/`scroll` (notifications 24 px in a scroller) | yes | notifications_… |
| `_embed`: hidden `close_button` (battery, power) | yes | battery_…, power_… |
| `_embed`: companion theme handover (applet theme closed) | yes | keepawake_… |
| `_embed`: close idempotent, cleanup, `_css` providers released, window destroyed; activate refreshes until closed | yes | every test closes twice in teardown |
| `window._settings_host`: dialogs transient for the host (audio loopback, bluetooth add/dialogs, network confirm/editors/wizards) | yes | audio_…, bluetooth_…, network_… |
| audio: `_drop_tab_headings`; close destroys loopback dialog, `row._destroyed()`, backend close; backend started after build | yes | audio_… |
| audio: sidebar rows carry `page_name` for search sub-pages | yes | audio_… |
| datetime: refresh on change/error, 15 s timer only while active, close stops it, `_destroyed`, backend close | yes | datetime_applies_only_explicit_interaction |
| datetime: clock preferences shared store, no system changes | yes | datetime_clock_preferences_… , parity_clock |
| battery: `PowerBackend(batteries_only=True)`, open_bluetooth → `host.show_section("bluetooth")`, `_on_destroy` | yes | battery_… |
| bluetooth: `App(embedded=True, host_window, backend_factory)`: no window shown, no SIGINT, no receiver; deactivate destroys Add window (StopDiscovery); close unwatches objects and drops ObjectManager subscriptions (`ObservedBluetooth.close`) | yes | bluetooth_… , parity_bluetooth |
| network facade: GtkApplication `org.waydock.GlobalSettings.NetworkDialogs` NON_UNIQUE, registered, no agent, no second service, no notifications, activate presents host | yes | network_controls_… |
| network: prepare_dialog → transient host / destroy_with_parent / tracked; dismiss_popup no-op; show_settings/show_popup switch tabs | yes | network_controls_…, network_overview_… |
| network: tabs Connections / Saved connections (crossfade), switcher margins, header moved above tabs, settings button hidden, saved title hidden, heading END, rows top-aligned, preferences button | yes | network_overview_…, multiple_embedded_…, parity_network |
| network close: stopping, children destroyed, editors cleared, `_on_destroy`, backend close, style released | yes | network_controls_… |
| keepawake: inner margins 24, footer = status with `real_error=client.ready`, close keeps service state | yes | keepawake_… |
| power: `PowerPageWindow` refresh (remote settings adopted only when clean and not saving; unknown durations appended; `sync_extra_controls`; save insensitive unless ready; client error shown) | yes | power_save_waits_… |
| power: `_save` waits for the service acknowledgement; failure keeps saved copy and shows the error | yes | power_save_waits_… |
| notifications: client changes refresh; close closes client and page | yes | notifications_… |
| weather: scroller, close/activate/deactivate, `search_targets` | yes | weather_and_capture_…, parity_weather |
| language: `EmbeddedWizard.close` (back to choose, reload after result), content `.languagemanager`, cancel hidden, transient host, close releases workers/style, `can_close` = not `_close_requested` | yes | language_wizard_…, multiple_embedded_… |
| capture: `_shared_card` (page margins, `gs-card`, `gs-row`, `gs-row-title`, `gs-row-subtitle`), search targets | yes | weather_and_capture_…, parity_capture |
| spotlight targets (`settings_search.py` `target=` names for these sections) | yes | search_targets_match_settings_search |
| switching pages preserves controls and resident state | yes | switching_pages_… |
| config files byte-identical to Python (clock.json + calendar.json, desktop-capture/config.json, weather-manager/config.json, btmanager/settings.ini, waybar/network-notifications.json) | yes | parity_clock/capture/weather/bluetooth/network |

## Pub APIs added to component crates (all additive; standalone behaviour unchanged)

- audio-manager `src/ui/settings.rs`: `AudioSettingsWindow::settings_host` (pub field, used as Add Loopback parent),
  `AudioSettingsWindow::css_provider()`.
- battery-manager `src/ui.rs`: `BatterySettingsWindow::on_destroy()` (stores its provider in a new private field).
- bluetooth-manager `src/backend.rs`: `Bt::objmgr_ids` (pub field, filled by `subscribe_object_manager`), `Bt::close()`;
  `src/ui.rs`: `Ui::new_embedded(options, host)`, `Ui::dialog_parent()`, `Ui::settings_host` (pub field),
  `Ui::on_window_destroyed` made pub.
- calendar-manager `src/settings.rs`: `DateSettingsWindow::destroyed()`.
- language-manager `src/ui.rs`: `Inner::close_override` (pub field), `LanguageWizard::close()` (window close unless
  overridden; the three internal `window.close()` calls now go through it), `unrealized()` made pub, `backend` made pub.
- network-manager `src/app.rs`: `AppInner::quiet` (skip notifications); `src/gui.rs`: `Embedding`, `Gui::set_embedding`,
  `embedded`, `adopt`, `children`, `editors`, `close_children` (embedded branches in `prepare_dialog`, `present`,
  `show_settings`, `show_popup`, `dismiss_popup`); `src/ui/settings.rs`: `NetworkSettingsWindow::settings_host`,
  `on_destroy` made pub; `src/ui/popup.rs`: `NetworkPopup::controller()`.
- power-manager: new `src/client.rs` (`PowerClient`, port of power_client.py); `src/ui.rs`:
  `PowerSettingsWindow::{refresh_override, save_override}` (pub fields), `refresh_base`, `save_base`, `set_message`,
  `syncing`, `set_syncing`, `on_destroy`.

After each change: `cargo test -p <crate> --lib` passes for all seven crates, every integration test target compiles,
and each crate's own headless UI binary passes when run through the GS runner (audio 21, bluetooth all, network all,
power all, battery 18, calendar 27, language 11).

## Gaps / differences (honest)

- The network facade GtkApplication is registered once per process and reused by rebuilt pages (GApplication exports
  one object per id; Python relied on the old facade being finalized). Same id/flags; `activate` presents the latest host.
- Network tests use the real `NetworkBackend` against a fake NetworkManager on the private bus (copied
  `tests/support/fake_nm.rs`, plus a recorded `RequestScan`), not Python's in-memory `FakeNetworkBackend`; the fake
  has one Wi-Fi adapter and no VPN profile, so the VPN-row and USB-adapter toggle checks became the Wi-Fi profile
  activation and per-adapter scan. The wizard draft (`hidden`, `mode=ap`, `ipv4.method=shared`) is not inspected
  (the Rust Gui keeps those wizards only as windows); title and transient parent are checked instead.
- Python's audio failed-callback test (`set_error`) and the bluetooth SIGINT check are covered only by construction
  (no SIGINT handler is installed when embedded).
- Battery parity uses a Rust fake (`BatterySource`), no Python comparison (no file is written).
- Power/notifications/keep-awake/audio/language write no files from the page (resident services or fakes), so they
  have no file-parity scenario.
