# PARITY-G — Dock Settings window (W-g)

Files: `src/ui/settings.rs`, `src/ui/group_layout_settings.rs`,
`tests/g_settings_headless.rs` (harness=false), `tests/run_g_headless.sh`.
Run: `components/dock/tests/run_g_headless.sh [filter]` (private headless labwc, 2 outputs,
private D-Bus without service activation, fake HOME/XDG under
`/run/user/1000/cbt/rust-tessel-dock/g/n`). Result: 25/25 pass.

| Contract item | Done | Test |
|---|---|---|
| §12.7 every section/row/control of `SettingsWindow` (placement + per-monitor edge/alignment, separate app dock + its 13 controls, app-dock group layout, appearance, bar border, visible groups, group layout, always-visible applets, system tray, behavior, extra_controls slot, shortcuts, hot corners, hint, result label, Cancel/Reset + Apply) with the same ranges, steps, widget types, sizes, spacing, tooltips, accessible names | yes | screenshots (0 differing pixels vs Python) |
| Live preview while editing, restore on unrealize (Cancel / Apply / titlebar close) | yes | appearance_controls_preview…, appearance_failed_apply…, dock_frosted_preview… |
| Dock vs bar remembered rounded/float (`_mode_appearance`), alignment insensitive in bar | yes | mode_settings_remember_separate_appearance… |
| Apply = Python `dataclasses.replace(app.config, …)`; byte-identical config.json | yes | python_parity_noop_apply, python_parity_rich_edits (Python SettingsWindow run with the same edits in the same compositor; ~40 edits incl. layouts, colours, tray, hot corners, magnification) |
| Failed save keeps window and edits | yes | failed_save_keeps_edits…, appearance_failed_apply… |
| Hot corners: default off, disconnected monitors preserved, long labels | yes | hot_corner_* (3) |
| GroupLayoutControls (dropdown move, up/down, reset, hidden groups, app dock expanded default, context per mode) | yes | settings_buttons_modes_apply_and_cancel, position_dropdown_can_move_its_own_row, separate_layout_settings… |
| Separate app dock availability (bar + top edge of selected monitors), expand label, colour sensitivity | yes | settings_enable_only_on_top_bars… |
| Always-visible applets, application display caption | yes | default_apps_settings…, settings_apply_and_cancel_keep_saved_pins… |
| Tray list: offline choices, sort running-first/casefold, signature skip, refresh button, backend subscribe/unsubscribe | yes | preferences_cancel_apply_and_reopen…, empty_tray_subscription_and_refresh |
| `section`/`embedded`/`reset`/`extra_controls` | yes (ported, Rust test only) | sections_embedded_and_extra_controls |
| Single instance (`show_settings`), `show_error` dialog | yes (helpers) | show_settings_presents_one_window |
| §6 CSS classes `tessel-dock-settings settings-card settings-tile settings-muted settings-hint settings-section-heading settings-section-title` (+ `settings-title`, `suggested-action`) | yes | screenshots |

## Screenshots
`$TMPDIR/screens/{rust,python}-settings.png`, `diff.png`, `diff.txt`: whole 2560x720 layout,
identical config/monitors/CSS (`appearance::style::CSS` vs `dock_style.CSS`): **0 differing pixels**.

## Gaps / notes
- Python strings are translated with `tr!` on the same English sources; tests run in English only.
- `refresh_tray_entries` / `subscribe_tray` are trait hooks; Python duck-typing (`getattr`) became
  default methods. `monitor_entries` returns `(name, label)` (no GdkMonitor).
- `apply` of an alignment combo with no active id saves `""` (Python would save `None`); neither is
  reachable from the UI.
- The settings CSS provider is the app's (as in Python); a standalone process must load
  `appearance::style::appearance_css(is_dark)` at APPLICATION priority before showing the window.
- Global Settings still embeds the Python window; the embedded path here is ported but only
  covered by the Rust test.
- Private labwc logs a harmless Xwayland bind failure on `/tmp/.X11-unix/X0` (already in use).
