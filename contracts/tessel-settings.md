# tessel-settings — external contract

Source: `~/tessel-bluetooth/tessel-settings` (8,708 LOC without tests; 38 modules + `layouts/`), plus every
applet UI it imports. Rust: `components/settings` (shell + binary) with page crates
`gs-base` (shared API), `gs-applets`, `gs-desktop`, `gs-system` under it.

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installed tree | `~/.local/share/tessel-settings/*.py` (every non-test, non-install module) + shared `suite_i18n.py language_data.py suite_catalogs.py gtk4_utils.py service_settings.py companion_theme.py wallpaper_chooser.py wallpaper_import.py capture_settings.py background_blur.py libtessel-dock-background-blur.so media_chooser.py files_pane.py …` + `VERSION` + `layouts/{gnome,kde,mac,win}.{layout,svg}` | shared/install_gtk4.py:147-191 |
| Launchers | `~/.local/bin/tessel-settings` = `#!/bin/sh\nexec /usr/bin/python3 '<~/.local/share/tessel-settings/global_settings.py>' "$@"\n`; `~/.local/bin/tessel-language` = same + `--section language ` before `"$@"` | install_gtk4.py:163-166 |
| .desktop | `~/.local/share/applications/org.tessel.GlobalSettings.desktop`: `Name=Settings`, `Comment=Configure your desktop, devices, and system language`, `Icon=preferences-system`, `Categories=Settings;DesktopSettings;`, `StartupNotify=true`, `StartupWMClass=org.tessel.GlobalSettings`, `Keywords=Audio;Bluetooth;Network;Displays;Power;Battery;Wallpaper;Dock;Language;` | install_gtk4.py:167-173 |
| User unit | `~/.config/systemd/user/tessel-night-light.service` (copied from `tessel-settings/tessel-night-light.service`) | install_gtk4.py:83 |
| GApplication | `org.tessel.GlobalSettings`, `HANDLES_COMMAND_LINE`, `do_local_command_line` returns False (so the primary parses argv), `GLib.set_prgname(app id)` | global_settings.py |
| Launched by | tessel-dock gear (`panel_actions.open_global_settings`: `~/.local/bin/tessel-settings`), applets' "Settings" buttons with `--section X`, keybind Super+/ → `--shortcuts`, Welcome (`tessel-welcome`), desktop_suite_failed notification (`desktop-entry: tessel-settings`), tessel-session-startup keeps the `tessel-settings` rebind | |
| Importers of GS Python modules | `tessel-tiling/tiling_config.py` imports `workspace_config` from `~/.local/share/tessel-settings`; `welcome/welcome.py` imports `runtime_paths`, `global_style`, `desktop_pages.interface_settings`, `shortcuts_model.display_key` and builds GS pages; installers of weather/lock/osk/notification copy `settings_search.py`/pages into the GS tree | |

### CLI (argparse, `allow_abbrev` — unique prefixes of the long options are accepted)
- `--section KEY` (`type` alias applied before `choices`: `brightness` → `displays`); keys = §2 order.
  Invalid → argparse usage error (exit 2, message on the remote stderr).
- `--shortcuts` toggles the Keyboard Shortcuts cheat sheet (`shortcuts_page.CheatSheet`): a second call closes it.
- Keyboard Shortcuts page: a shortcut in rc.xml's "Tessel keybindings" block whose key differs from
  `tessel-configure-session`'s table (`tessel_core::session_keys`) gets a reset button; "Reset All Shortcuts…" (after confirmation) rewrites every
  changed block key in one rc.xml write and one `labwc --reconfigure`, leaving rows whose default key a keybind of the
  user's own outside the block uses. Keybinds outside the block, labwc defaults and tiling shortcuts have no default.
- `--quit` (suppressed in help): closes the window; if a page refuses (`can_close`), prints
  `A settings operation is still in progress.\n` and returns 1; else quits.
- no flags: show the window (section `audio` the first time), unminimize, present, then
  `wlrctl toplevel focus app_id:org.tessel.GlobalSettings title:<title>` (≤4 retries/100 ms) on Wayland.
- `main()` parses argv locally first (so `--help`/errors print in the launching process too).

## 2. Window (global_ui.py)
- `GlobalSettingsWindow`: title `Settings`, default 1090×810, min 830×580, `headerbar(tr('Settings'))`,
  `global_style.install` (class `tessel-settings`, CSS+PAGE_CSS, companion palette).
- Sidebar `.settings-navigation.suite-sidebar` 230 px: `.navigation-title` "Settings", `Gtk.SearchEntry.settings-search`
  ("Search settings"), ListBox rows sorted by `tr(name).casefold()`; row = `.navigation-item` box (icon 16 + label).
- Sections (key, name, icon, module): audio Audio audio-volume-high-symbolic applet_pages · bluetooth Bluetooth
  bluetooth-active-symbolic applet_pages · network Network network-wired-symbolic applet_pages · displays Displays
  video-display-symbolic desktop_pages · input "Mouse & Touchpad" input-mouse-symbolic input_page · keyboard Keyboard
  input-keyboard-symbolic keyboard_page · shortcuts "Keyboard Shortcuts" preferences-desktop-keyboard-shortcuts-symbolic
  shortcuts_page · power Power power-profile-balanced-symbolic applet_pages · battery Battery battery-good-symbolic ·
  keepawake "Keep Awake" view-reveal-symbolic · notifications Notifications preferences-system-notifications-symbolic ·
  weather Weather weather-clear-symbolic weather_page · theme "Theme & Wallpaper" preferences-desktop-wallpaper-symbolic
  desktop_pages · layout "Desktop Layout" view-dual-symbolic layout_page · behavior "Desktop Behavior"
  preferences-desktop-symbolic dock_pages · lock "Lock Screen" changes-prevent-symbolic lock_page · login "Login Screen"
  system-users-symbolic login_page · dock Dock/Panel preferences-desktop-display-symbolic dock_pages · datetime
  "Date & Time" preferences-system-time-symbolic applet_pages · language Language preferences-desktop-locale-symbolic
  language_page · accessibility Accessibility preferences-desktop-accessibility-symbolic · defaultapps "Default Apps"
  applications-system-symbolic · nightlight "Night Light" weather-clear-night-symbolic · users Users avatar-default-symbolic
  · about About help-about-symbolic · capture Screenshots camera-photo-symbolic capture_page.
- Pages are built lazily on first selection; a builder exception → `_error_page` (header + `empty_state`
  "Settings are unavailable", error text, "Try Again" rebuilds; class `settings-error`). `_unify` restyles every
  page header (`unify_header` + `align_page_gutter`, 24 px section icon). Stack transition NONE.
- Lifecycle: previous page `deactivate()`, new page `activate()`; search hides the current page (deactivate) and
  restores it (activate). `close-request`: first page whose `can_close()` is False is shown and close is refused;
  else `cleanup()` closes every page and the style. Ctrl+F focuses search; Escape leaves search, else closes.
- Search (settings_search.py, 178 `Setting`s + one `__section__` entry per section "Open {section} settings."):
  NFKD + casefold + strip combining, tokens `[^\W_]+`; every word must occur in the terms; rank 0 exact title,
  1 prefix, 2 token prefixes, 3 substrings, 4 term-token prefixes, 5 other; sort `(rank, title, section.casefold(), key)`.
  Results page `.settings-search-page` with header "Search results" + subtitle "{n} result(s)", `.settings-search-results`
  rows (icon, `.search-setting-title`, `.search-section`, `.search-detail`, go-next), empty state "No settings found".
  Activating shows the section, calls `reveal_setting`, then spotlights the target (`search_targets[key]`, else
  `getattr(controller, target)`, else a mapped label whose normalized text equals a translated label/title) with
  class `settings-search-target` for 3.5 s, focuses the first focusable control, scrolls it to 30 % of the view;
  retries every 100 ms ≤14 times; sub-pages (`page.tabs` stack child, or a ListBoxRow with `page_name`) first.

## 3. Embedded components — import map (Python → Rust)

Every page that Python built from another component's module is built from that component's Rust crate library
(lib targets exist for all of them). Pub APIs added for embedding are listed in each group's PARITY.md.

| GS module | Python import (component) | Python symbols used (incl. private) | Rust crate::path |
|---|---|---|---|
| applet_pages `_audio` | tessel-audio `audio_ui`, `audio_backend` | `PAGES`, `AudioSettingsWindow(app, backend)`, `.refresh() .set_error() .loopback_dialog .node_rows[*]._destroyed() .stack .status_label`, `AudioBackend(changed, failed).start()/.close()` | `audio_manager::ui::settings::AudioSettingsWindow`, `audio_manager::backend` |
| `_datetime` | tessel-calendar `date_settings`, `date_backend` | `DateSettingsWindow(app, backend)`, `.refresh() ._destroyed(view)`, `DateBackend(changed, failed)` | `calendar_manager::settings::DateSettingsWindow`, `calendar_manager::backend` |
| `_battery` | tessel-battery `battery_ui`; tessel-power `power_backend` | `BatterySettingsWindow(app, backend, open_bluetooth=)`, `._on_destroy`, `PowerBackend(..., batteries_only=True)` | `battery_manager::ui::BatterySettingsWindow`, `suite_power::backend::PowerBackend::batteries_only` |
| `_bluetooth` | tessel-bluetooth (repo root `tessel-bluetooth.py`) | `BT` subclass (subscribe/close), `OBJMGR_IFACE`, `App(application, embedded=True, host_window, backend_factory)`, `.window ._add_window ._on_window_destroyed ._sub_ids ._unwatch_object ._refresh_ui .bt` | `bluetooth_manager::{manager, backend, ui}` |
| `_network` | tessel-network `network_manager`, `network_popup`, `network_settings`, `network_preferences`, `network_style`, `network_backend` | `NetworkApplication` subclass (facade, NON_UNIQUE `org.tessel.GlobalSettings.NetworkDialogs`, no agent), `NetworkSettingsWindow(facade, backend)`, `NetworkPopup(facade, backend, embedded=True)` (`.window .rows_box .subtitle .settings_button .refresh ._on_destroy`), `NotificationPreferences()`, `style(widget, cls)`, `NetworkBackend(changed, error)` | `network_manager::{gui, ui::settings, ui::popup, preferences, backend}` |
| `_keepawake` | tessel-keep-awake `keep_awake_client`, `keep_awake_ui` | `KeepAwakeClient(changed)`, `KeepAwakePopup(app, client, embedded=True)` (`.window .status .refresh ._destroyed`) | `keep_awake::{client::KeepAwakeClient, ui::KeepAwakePopup}` |
| `_power` | tessel-power `power_client`, `power_config`, `power_ui`, `power_backend` | `PowerClient(changed)` (`.state .ready .error .set_settings`), `DEFAULTS`, `validate_settings`, `PowerSettingsWindow` subclass (`settings _saved controls lock_choice save_button status_label _message _syncing _destroyed _validation_error sync_extra_controls _on_destroy`), `duration_name` | `power_manager::{ui::PowerSettingsWindow, config, gui/client}` |
| `_notifications` | tessel-notifications `notification_client`, `notification_ui` | `NotificationClient(changed)`, `SettingsPage(client)` (`.refresh .status_label .destroy`) | `notification_manager::{client, ui::SettingsPage}` |
| weather_page | tessel-weather `weather_settings` | `WeatherSettings()` widget (`.close .activate .deactivate .search_targets`) | `weather_manager::settings::WeatherSettings` |
| language_page | tessel-language `language_backend`, `language_ui` | `LanguageWizard` subclass (`close` override, `busy page _show_page _message spinner progress_row _run _loaded cancel_button _unrealized _language_style_close _close_requested`), `LanguageBackend()` | `language_manager::{ui::LanguageWizard, backend}` |
| capture_page | tessel-capture `capture_settings` | `build_page(path)` (`.switches`), | `capture_manager::settings::build_page` |
| desktop_pages Displays | tessel-displays `tessel-displays` (loaded by path as `desktop_suite_displays_editor`), `color_config` | `TesselDisplaysApp(app, brightness_client_factory, embedded=True)`, `CONFIG_DATA`, `get_config_data()`, overlay, `read_status()` | `tessel_displays::{window::TesselDisplaysApp, color_config}` |
| desktop_pages Theme | tessel-dock `settings_api.APPEARANCE_FIELDS`, `companion_theme.attach`; shared `wallpaper_chooser.WallpaperChooser`; GS `dock_pages.DockSettingsBridge` | | `tessel-dock::dbus::settings_api`, `suite_gtk::theme`, `tessel-shell::wallpaper_chooser::WallpaperChooser` |
| dock_pages | tessel-dock `dock_config` (`ConfigStore`, `DockConfig`), `settings_api` (`BUS_NAME OBJECT_PATH INTERFACE DOCK_FIELDS DESKTOP_FIELDS APPEARANCE_FIELDS apply_patch`), `tessel-dock.SettingsWindow(app, context=bridge, section, embedded=True, reset, extra_controls)` (`result_label match_wallpaper transparency transparency_enabled frosted frost_strength refresh_tray_rows _switching_mode update_material_controls preview_appearance workspace_count gapless_switch …`), `monitor_label`, `dock_style.CSS`; tessel-tiling `tiling_settings.TilingControls` | | `tessel-dock::{core::config, dbus::settings_api, ui::settings, core::monitors}`, `tiling_manager::ui::settings::TilingControls` |
| layout_page / layout_presets | tessel-dock `dock_config.DockConfig`, `settings_api.SETTINGS_FIELDS`; tessel-tiling `tiling_config.TilingConfig`; shared `clock_preferences` | | `tessel-dock::core::config`, `tiling_manager::config::TilingConfig`, `suite_core::clock` |
| lock_page | tessel-lock `lock_settings.LockSettings` (+ `lock_config`, `lock_weather.configured_weather`, tessel-power `power_client`, `wallpaper_chooser`, tessel-files `media_chooser`) | not yet ported (wave 5) | ported inside `gs-desktop::lock` (to move into tessel-lock in wave 5) |
| login_page | tessel-login `login_model` (`Appearance`, `sessions`); helper `pkexec /usr/bin/python3 -I /usr/local/lib/tessel-login/login_settings_helper.py` | not yet ported (wave 5) | `gs-desktop::login` |
| keyboard_page | tessel-keyboard `osk_settings.KeyboardSettings` | | `on_screen_keyboard::settings::KeyboardSettings` |
| shortcuts_model | tessel-tiling `bindings.BINDINGS` | | `tiling_manager::bindings::BINDINGS` |
| input_page/mouse_polling | `pyudev` (optional), `openrazer_daemon.hardware.mouse`, ratbagd D-Bus | | local (udev via /sys, ratbag via gio) |
| global_style / page_widgets / page_api | shared `gtk4_utils`, tessel-dock `companion_theme` | | `gs_base::{style, widgets, page}`, `suite_gtk` |

## 4. Files written (all byte-identical to Python; verified by fake-HOME parity tests)
- rc.xml (labwc `-c/-C` of `LABWC_PID`, else `$XDG_CONFIG_HOME/labwc/rc.xml`): workspaces/input/keyboard/
  shortcuts/accessibility/night light blocks via `labwc_settings` / `workspace_config` / `input_config`
  (backups `$XDG_STATE_HOME/tessel/backups/{input,…}`, reload `labwc --reconfigure`, restore on failure).
- Keyboard "Turn on Num Lock at sign-in" (Rust-only): `<numlock>on|off</numlock>` inside rc.xml `<keyboard>`
  (spliced like repeatRate; other content kept), then `labwc --reconfigure`. Shown on unless the element holds a word
  labwc reads as off (`no/false/off/0`, any case); a missing element shows on because `tessel-configure-session`
  adds `<numlock>on</numlock>` to a `<keyboard>` without one (never changing an existing element or a `numlock=`
  attribute). labwc applies it only to keyboards it recognises after the reload (next sign-in, a newly plugged
  keyboard), not to attached ones; a Num Lock key press works as usual.
- Desktop Behavior "Snap layouts" (Rust-only, `gs_desktop::snap_layouts`): `$XDG_CONFIG_HOME/tessel/snap-layouts.json`
  (0600, atomic; `{"version": 1, "outputs": {"<connector>": {"columns": [..], "rows": [..]}}}`, integer percentages
  summing to 100, 1–6 columns, 1–4 rows, each area ≥ 10 %, unknown keys kept, invalid entries read as the default) is
  the source of truth; rc.xml gets the block `<!-- tessel snap layouts begin -->` `<regions>` `<region name="tessel-<col>-<row>"
  output="<connector>" x= y= width= height= (%)/>` … `<!-- tessel snap layouts end -->` before `</labwc_config>`, under
  the shared `rc.xml.tessel-lock`; removed when every screen keeps "Halves (default)"; `labwc --reconfigure` only when
  the block changed. A user's own `<regions>` is never touched. Applies immediately (keyboard divider moves after 300 ms,
  mouse drags on release). Not rewritten on page load; `snap_layouts::sync_rc()` regenerates it from the JSON.
- Theme & Wallpaper "Pointer" card (Rust-only, `gs_desktop::cursor_themes` over `tessel_core::cursor_theme`): lists
  every dir of the icon search path whose `cursors/` has `left_ptr` or `default` (never `default` itself; first dir
  wins an id; sorted by `Name=`), previews six cursors read from the theme's Xcursor files (aliases + `Inherits=`), and
  falls back to Adwaita without saving when the configured theme is not installed. Choosing a theme writes, each step
  independent and only when changed: gsettings `org.gnome.desktop.interface cursor-theme`; `gtk-cursor-theme-name` in
  `gtk-{3,4}.0/settings.ini`; `XCURSOR_THEME` in labwc's `environment` then `labwc --reconfigure`;
  `$XDG_DATA_HOME/icons/default/index.theme` `[Icon Theme] Inherits=` (only that line of an existing file; also
  `~/.icons/default` when it exists; symlinks out of `$HOME` and unwritable files skipped);
  `$XDG_CONFIG_HOME/tessel/cursor-theme`. Its Size row uses the Accessibility backend (`cursor-size`, `XCURSOR_SIZE`)
  and also reloads labwc. Search target `cursor_theme`.
- Theme & Wallpaper "Fonts" card (Rust-only, `gs_desktop::fonts`): four `GtkFontDialogButton`s, applied on choice,
  each step independent and only when changed: Interface text → gsettings `org.gnome.desktop.interface font-name` +
  `gtk-font-name` in `gtk-{3,4}.0/settings.ini`; Document text → `document-font-name`; Monospace text →
  `monospace-font-name` (dialog lists monospace families only); Window titles → rc.xml `<theme><font
  place="ActiveWindow">` and `place="InactiveWindow"` (`<name>` family, `<size>` rounded whole points, `<slant>`,
  `<weight>` nearest labwc keyword), edited in place (other `<font>`s, comments and bytes kept; created in `<theme>`,
  `<theme>` created when missing; moved after a later place-less `<font>`), under `rc.xml.tessel-lock`, then
  `labwc --reconfigure` (only under labwc). "Restore defaults" (sensitive only when something differs): gsettings
  `reset` of the three keys, `gtk-font-name` removed from both ini files, title fonts back to
  `tessel-configure-session`'s Sans 10 SemiBold. Samples in the interface and monospace fonts. Search targets
  `font_interface`, `font_document`, `font_monospace`, `font_titlebar`.
- Default Apps (`defaultapps`, also `org.tessel.DefaultApps.desktop`): Web browser, Mail, File manager, Terminal,
  Text editor, Document viewer, Image viewer, Video player, Music player, Archive manager (targets `default-<key>`:
  browser mail files terminal editor documents images video music archives). The first MIME type of a category gives
  the choices and the current app; the others are set only when the chosen app lists them in `MimeType=`. GIO writes
  `~/.config/mimeapps.list`, which beats the packaged `/usr/share/applications/labwc-mimeapps.list`; entries that a
  `~/.config/<desktop>-mimeapps.list` already has are pointed at the choice too. Browser: also
  `xdg-settings set default-web-browser` (apps that open `x-scheme-handler/http` only). Terminal: `TERMINAL` in
  labwc's environment (`flatpak run … <app id>` for a Flatpak), first line of `~/.config/xdg-terminals.list`, then
  `labwc --reconfigure`; the note asks to sign out and in only when that reload fails.
- `labwc/environment` keys (`XCURSOR_SIZE`, `TERMINAL`, XKB_*), `night-light` config, `mimeapps.list`
  (default apps), `~/.config/tessel-dock/config.json` (when tessel-dock is offline), `~/.config/tessel/clock.json`,
  `layout_presets` previous-layout state, tiling config, tessel-lock config, login appearance (via pkexec helper).
- Every embedded component keeps writing its own files exactly as its standalone Rust port does.

## 5. External commands (all faked in tests; never run for real)
`labwc --reconfigure`, `systemctl --user …` (night-light, orca, `try-restart` shell services, `is-active tessel-auth.service`),
`localectl list-x11-keymap-layouts`, `libinput list-devices`, `wlr-randr --json`, `lspci -mm`, `labwc --version`,
`setsid --ctty passwd` (PTY), `pkexec … login_settings_helper.py`, wallpaper import pkexec (tessel-shell chooser),
language wizard pkexec/localed flow, `tessel-hdr-calibration --output X`, `wlrctl toplevel focus`.

## 6. Tests to port
test_global_ui, test_settings_search, test_applet_pages, test_desktop_pages, test_dock_pages, test_input_config,
test_input_ui, test_keyboard?, test_layout_presets, test_mouse_polling, test_system_pages, test_system_ui,
test_workspace_config, test_accent_ui.
