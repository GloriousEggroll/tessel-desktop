# gs-desktop — parity with the Python desktop pages

Python sources: `tessel-settings/{desktop_pages,dock_pages,layout_page,layout_presets,lock_page,login_page}.py`,
`tessel-lock/{lock_settings,lock_config,lock_weather}.py`, `tessel-power/power_client.py`,
`tessel-login/login_model.py`. Layout data: `layouts/{gnome,kde,mac,win}.{layout,svg}` copied byte-identical
into `components/settings/desktop/layouts/` and embedded with `include_bytes!`.

Tests: unit = `cargo test -p gs-desktop` (23, src/tests.rs + module tests); headless =
`AGENT=gs-desktop OUTPUTS=2 components/settings/tests/headless.sh target/debug/deps/desktop_headless-*`
(57; optional first argument runs one group: theme icons displays dock online layout lock build extra parity).
Byte parity drives `tests/parity_driver.py` (Python pages) inside the same private session with a fake HOME and
compares the written files (bytes + mode).

## Public API (shell / Welcome)
| Item | Done | Test |
|---|---|---|
| `SECTIONS`, `build_page(section, host)`, unknown section error | yes | `sections_and_unknown_page`, `every_section_builds_in_the_private_session` |
| `interface_settings()` (schema + `color-scheme` key check) | yes | `interface_settings_drive_the_style_selector` |
| Injection variants: `displays::build_displays_page(DisplaysOptions)`, `theme::build_theme_page(ThemeOptions)`, `dock::create_page(app, section, DockPageOptions)`, `layout::build_layout_page(LayoutOptions)`, `lock::build_lock_page(LockOptions)`, `login::build_login_page(LoginOptions)` | yes | headless groups |

## Displays (DisplaysPage)
| Item | Done | Test |
|---|---|---|
| Embeds `way_displays::window::WayDisplaysApp` embedded=true, child moved into page, own title hidden, 30→24 px gutter | yes | `displays_embeds_all_controls_without_applying_initial_values` |
| No write/apply on open (ipc, xwayland primary) | yes (hooks panic) | same |
| Brightness backend kept, Apply independent | yes | `embedded_brightness_retains_existing_backend_and_apply_independence` |
| Identify displays: explicit, 3 s, cancelled by deactivate | yes | `display_identify_is_explicit_and_deactivation_cancels_it` |
| Calibrate HDR… only with HDR outputs; `~/.local/bin/tessel-hdr-calibration --output X` (else `python3 ~/.local/share/tessel-hdr-calibration/hdr_calibration.py`) | yes | `calibrate_hdr_is_offered_only_for_hdr_monitors`, `hdr_outputs_lists_enabled_hdr_monitors_only` |
| Load on worker (`GET` + color status), shared error state ("Displays are unavailable" / "No displays found") | yes | `displays_failure_uses_the_shared_error_state` |
| close(): brightness, palette, display CSS provider, window | yes | same |

## Theme & Wallpaper
| Item | Done | Test |
|---|---|---|
| Style buttons ↔ `org.gnome.desktop.interface color-scheme` (Automatic/Light/Dark), external change sync, unavailable schema text | yes | `style_selector_writes_shared_color_scheme`, `interface_settings_drive_the_style_selector` |
| (addition) Icons: dropdown of installed icon themes (`suite_core::icon_theme::discover`: index.theme with `Directories=`, no cursor-only/hidden themes except Adwaita, no `hicolor`/`default`/dock mirrors; `Name=` shown), samples (folder, text file, settings) from the selected theme, Papirus-Dark when the configured theme is missing; a choice writes gsettings `icon-theme` and `gtk-icon-theme-name` in `~/.config/gtk-{3,4}.0/settings.ini` (injectable `IconThemeSettings`); external changes followed; search target `icon_theme` | yes | group `icons`: `icons_dropdown_lists_installed_themes_and_saves_through_the_backend`, `icons_row_without_themes_is_disabled`; `icon_themes::tests` |
| Match wallpaper, dock/app transparency (+enabled), Blur, Blur strength, rounded corners, window buttons left, colored buttons, shadows — APPEARANCE_FIELDS only | yes | 13 theme tests (`transparency_*`, `frost*`, `application_corners_*`, `failed_*`, `appearance_reloads_*`, `palette_reloads_*`, `closing_flushes_*`) |
| 250 ms debounce, flush on deactivate/close, async save, revert on failure | yes | `transparency_drag_is_debounced_and_flushed_on_navigation`, `frost_strength_drag_*` |
| Per-display wallpaper rows from `wallpapers.json`; `WallpaperChooser` (tessel-shell) with cancellable | yes | `wallpaper_has_per_monitor_controls_without_saving_on_open` |
| Apply: `list-monitors` preflight then `set` per dirty display over `$XDG_RUNTIME_DIR/tessel-shell.sock`; partial failure, missing file, disconnected display | yes | `wallpaper_apply_*`, `partial_wallpaper_failure_*`, `missing_wallpaper_file_*`, `disconnected_monitor_*`, `tessel-shell_socket_protocol_bytes_and_errors` |
| confirm_leave Apply/Discard/Cancel | yes | `unapplied_wallpaper_prompts_before_closing` |
| Byte parity: tessel-dock config.json + tessel-shell.sock request lines | yes | `parity_theme_page_writes_identical_tessel-dock_config_and_tessel-shell_requests` |

## DockSettingsBridge
| Item | Done | Test |
|---|---|---|
| Online: `NameHasOwner` (1 s), `Read`/`Apply` (2 s), `Preview` (75 ms coalesced), `Restore`, `GetTrayApps`, all NO_AUTO_START | yes | `online_bridge_reads_applies_previews_and_restores_through_dbus` (real Rust SettingsApi on a fake host, private bus) |
| Offline: ConfigStore + `apply_patch` (`changes`/`expected`), dock byte format | yes | `offline_bridge_saves_only_its_fields_in_the_dock_byte_format`, `request_lists_only_changed_page_fields` |
| refresh(fields=…) keeps the page baseline; worker-thread async variants | yes | dock/theme tests |
| Preview argument handling (optional args, key order) | yes | `preview_supports_previous_arguments_and_optional_transparency_toggle` |
| GLib errors shown as `str(GLib.Error)` (`domain: message (code)`) | yes | conflict text checked in online test |

## Dock/Panel and Desktop Behavior
| Item | Done | Test |
|---|---|---|
| tessel-dock `SettingsWindow` embedded (Reset, "Settings saved."), dock fields minus match_wallpaper / DESKTOP_FIELDS | yes | `dock_page_embeds_controls_and_saves_only_its_fields`, `behavior_page_retains_disconnected_monitor_and_other_settings` |
| Activate refreshes APPEARANCE_FIELDS, keeps local edits, tray rows, preview | yes | `returning_to_dock_page_*` (2), `theme_and_dock_pages_share_*`, `bar_border_controls_*` |
| Reset rebuilds controller; conflicts rejected | yes | `concurrent_pin_change_survives_*`, `workspace_reset_and_conflict_*` |
| DesktopControls: gs-system WorkspaceControls, tiling TilingControls (GaplessControls in tests), AuthenticationStatus (`systemctl --user is-active tessel-auth.service`) | yes | `gapless_shared_control_*`, `authentication_status_reads_the_polkitman_unit_state` |
| Section headings without icons | yes | (visual; `plain_section_headings`) |
| Byte parity: config.json (dock), config.json + rc.xml (behavior) | yes | `parity_dock_page_*`, `parity_behavior_page_*` |

## Desktop Layout
| Item | Done | Test |
|---|---|---|
| parse/load/capture/matches/apply/describe/save_previous/load_previous | yes | 9 unit tests (PresetTests port + clock) |
| Page: cards, current marker, confirm, Apply, Restore, Undo toast, errors | yes | `current_layout_is_marked_*`, `choosing_and_applying_another_layout`, `layout_failure_is_shown` |
| Byte parity: config.json, rc.xml, previous-layout.json (0600, dir 0700), tessel-tiling config.json, clock.json | yes | `parity_layout_apply_writes_identical_files`; describe_changes text `parity_previous_layout_and_presets_match_python` |

## Lock Screen (gs_desktop::lock, moves to tessel-lock in wave 5)
| Item | Done | Test |
|---|---|---|
| LockConfig.parse messages, ConfigStore (indent 2, ASCII, 0600) | yes | `lock::config::tests::*` (3) |
| Page: cards, picture/slideshow, follow appearance, HA card (detector), weather location (Weather ConfigStore), power lock delay via PowerClient (`org.tessel.PowerManager.Controls SetSettings`) | yes | `lock_settings_save_appearance_and_shared_lock_delay`, `lock_preview_*` |
| Preview: `systemd-run --user --collect --quiet --property=Type=exec /usr/bin/python3 ~/.local/share/tessel-lock/lock_screen.py --preview` | yes | `lock_preview_starts_lock_screen_preview_through_systemd_run` (fake systemd-run) |
| Byte parity tessel-lock.json | yes | `parity_lock_page_writes_identical_lock_config` |

## Login Screen (gs_desktop::login)
| Item | Done | Test |
|---|---|---|
| Appearance.from_dict messages, sessions() rules (TryExec, %-codes, Hidden, DesktopNames, first root wins) | yes | `login::model::tests::*` (3) |
| Helper request `{"expected","appearance"}` on stdin of injectable `pkexec … login_settings_helper.py` | yes | `login_settings_send_the_validated_request_to_the_helper`, `parity_login_page_sends_identical_helper_request` (fake pkexec only) |

## Search targets (settings_search.py names)
theme: match, dock_transparency, apps_transparency, frosted, apps_frosted, frost_strength, apps_frost_strength,
apps_rounded_corners · dock: mode, monitor, size, float_mode, rounded_corners, auto_hide, fullscreen,
application_display, separate_app_dock, background_color, bar_border_enabled, app_dock_layout_controls ·
behavior: workspace_count, gapless_switch, tiling_switch, focus_outline_switch, authentication_status,
hot_corner_delay, menu_hotkey_fullscreen · layout: presets, previous · lock/login: their `search_targets` keys.
Tests: `*_search_targets_*`, lock/login tests.

## Pub APIs added in other crates (additive; lib + headless suites re-run green)
- tessel-dock `ui::settings::SettingsWindow::{set_switching_mode, update_material_controls, preview_appearance, add_tray_choice}`
  (`cargo test -p tessel-dock --lib` 390 ok; `tests/run_g_headless.sh` 25 ok, 0 px screenshot diff).
- way-displays `window::WayDisplaysApp::{push_overlay, hide_overlays}`, `style::display_style(window)` (style_window
  stores its provider on the window) (`--lib` 29 ok; `tests/run_headless.sh` all ok).

## Gaps (honest)
- Search targets are registered when the page is built; after Dock/Behavior **Reset** the controller is rebuilt and
  the old widgets are unmapped, so the shell falls back to label matching. Wanted in gs-base: a dynamic resolver or a
  shared `Rc` targets map.
- `PageHost::show_page_error` returns nothing, so Displays always also shows its own empty state (Python skipped it
  when the host returned a page). Wanted in gs-base: `-> bool`.
- Presets are embedded; Python read `layouts/` next to the module (a user-added `.layout` there is not picked up;
  `load_presets_from(dir)` exists).
- JSON/UTF-8 decode error texts (corrupt `wallpapers.json`, `.layout`, lock config) are serde's, not Python's.
- `home_assistant_installed`: `importlib.util.find_spec('homeassistant')` approximated by scanning python3 site dirs.
- Not ported: test_dock_pages `test_workspace_count_reloads_private_compositor_and_updates_navigator` (headless.sh's
  labwc is a logging fake, no live reconfigure); the gapless test's injected `set_gapless` failure branch;
  test_accent_ui (it exercises the Date & Time applet page inside the shell window — gs-applets/shell scope).
- Pages are not screenshot-compared with Python.
