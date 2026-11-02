# PARITY-F2 — W-f part 1: panel widgets (indicators, tray, expander, media, capture controls)

Owner: sub-agent F2. Files: `src/ui/{host,indicators,tray,media,capture_controls}.rs`,
`tests/f2_ui_headless.rs` (harness=false), `tests/run_f2_headless.sh` (private headless labwc,
2 outputs, private D-Bus with service activation disabled, wlrctl virtual pointer on the private
compositor only).

Run: `cargo test -p tessel-dock --lib ui::` (unit + Python parity) and
`components/dock/tests/run_f2_headless.sh [filter]`.
Results (2026-10-29): unit 6/6 ui::indicators+host (incl. parity vs panel_ui.py via `python3 -I`);
headless 31/31.

## Seam for E (`ui::host`)
`trait DockView` (dock_ui.DockSurface, Python names), `trait DockApp` (the application:
status backend, tray backend, panel actions, workspace backend, fullscreen state, clock store),
`trait TrayBackend` (implemented for `dbus::sni::StatusTrayBackend`). Obligations of the surface
are listed in host.rs's module doc (refresh_groups / sync / popup_menu / destroy hooks).

## Contract items

| Item | Done | Test |
|---|---|---|
| §6 CSS classes `tessel-dock-indicators tessel-dock-bar-start tessel-dock-status status-<name> tessel-dock-status-glyph tessel-dock-status-icon tessel-dock-tray tessel-dock-tray-item tessel-dock-tray-expander tessel-dock-applications-expander tessel-dock-workspaces tessel-dock-workspace(.active .urgent) tessel-dock-clock tessel-dock-clock-time tessel-dock-clock-date tessel-dock-clock-weekday clock-inline tessel-dock-media tessel-dock-media-control tessel-dock-media-song idle needs-attention tessel-dock-context-menu` + script state classes added/removed | yes | `always_tray_native_tooltips`, `workspaces_and_clock`, `media_*`, `dbusmenu_popover_*` |
| §6 `tessel-dock-tray-dismiss` / `tessel-dock-<kind>-dismiss`: TOP, keyboard NONE, exclusive −1, all edges, one per monitor, input region = monitor − visible clones' panels − extra holes | yes | `expander_toggle_and_outside_click_on_each_monitor`, `expander_cloned_docks_remain_clickable_and_holes_follow_geometry`, `expander_preview_hole_keeps_layer_surface_clickable` |
| §7 glyph→icon map verbatim, TEXT_INDICATORS keep glyph, Pango absolute glyph size | yes | unit `glyph_and_class_icon_map_matches_python`, `icon_map_and_sizes_match_python_module` (4032 cases vs panel_ui.py), `status_visuals_and_vpn_badge` |
| §7 status icon size compact clamp(round(icon·.4),12,20) else clamp(round(icon·.5),14,32) (half-even) | yes | `icon_sizes_follow_python_rounding_and_clamps`, Python parity |
| §7 tray icons 16 compact (panel class or compact_bar && !app dock) else 22 | yes | `icon_sizes_follow_python_rounding_and_clamps` |
| §7 clicks: left popup / right settings, bluetooth no right handler, idle_inhibitor right toggles, capslock no click, showdesktop → status action, clock → calendar (right `--settings`), logout → session, offset `max(40, thickness+gap+8)`, closes expanders/app menu/preview first | yes | `indicators_clicks_offsets_and_record_presets` |
| §7 audio scroll → `audio_scroll(∓1)` | yes | same (real wheel via wlrctl) |
| §7 screen_record: idle → preset popover (none/desktop/desktop-mic) with heading + gear (settings); recording → stop; stopping → nothing; refresh while not idle pops menu down | yes | same, `default_apps_recording_menu_holds_expansion_until_closed` |
| §10.6 UI side (panel actions invoked with exact name/settings/edge/offset) | yes | same |
| `register_capture_controls`: `prepare-capture(b)` / `finish-screenshot`, autohide unmap/map dance | yes | `capture_controls_prepare_and_finish` |
| Default-apps collapsible tray: always-visible set, toggle only when something is hidden, hover/focus expand, 350 ms collapse held by pointer/always-tray menu/context menu anchored in apps/keyboard focus, Escape | yes (Escape/focus untested) | `default_apps_hover_expands_and_leaving_collapses` (16 layouts), `default_apps_empty_and_all_selections_keep_reveal_and_tray` |
| Tooltips: detail if it starts with title (casefold) else `title\ndetail`; accessible name = first line, description = full | yes | unit `tooltips_classes_titles_and_defaults`, `always_tray_native_tooltips` |
| network VPN badge, first glyph only | yes | `status_visuals_and_vpn_badge` |
| capslock hidden when text empty; weather availability | yes | same |
| Clock: clock.json prefs (stacked/date_first/show_date/show_weekday), 1 s tick, tooltip `"%A, %B %-d, %Y · %Z"` | yes | `workspaces_and_clock` |
| Workspaces: hidden filtered, labels name or index+1, `Workspace {n}` tooltip, `· Current`, sensitive, active/urgent, order, error tooltip, availability | yes | `workspaces_and_clock` |
| Always tray routing by `always_show_tray` identity; passive items excluded from regular tray; always tray visibility; selection change refreshes both | yes | `always_tray_routing_all_layouts_and_return_to_normal_tray` (16 layouts), `always_tray_restarted_application_keeps_placement_on_every_clone` |
| StatusTray icons: IconName (absolute file / theme lookup, private IconTheme per IconThemePath, bounded by current items), pixmaps, attention, overlay composite, `application-x-executable` fallback, signature cache (metadata never reloads; NewIcon / theme change do) | yes | `tray_refresh_*` (3), `always_tray_late_pixmap_item_fits_before_always_tray` |
| Tray clicks with SCREEN coords: right → ContextMenu (releases panel menus/catchers via menu_state(true/false)), left Activate (failure → DBusMenu), middle SecondaryActivate, ItemIsMenu left → menu, scroll ±120 | yes (scroll by unit logic only) | `coordinates_bar_actions_on_all_edges_and_both_trays`, `coordinates_second_monitor_and_menu_releases_catchers`, `coordinates_scaled_output_and_menu_only_left_click` (real private SNI item) |
| DBusMenu PopoverMenu (no arrow, context-menu + palette class, `remote` action group, capture ShortcutController rebuilt per render, AboutToShow+opened, clicked → popdown, closed → menu_state(false)) | yes (shortcut key press not injected: wlrctl cannot send modifiers) | `dbusmenu_popover_from_right_click_and_item_click` |
| §10.22 one dismissal set across clones, holes keep preview clickable, never covers fullscreen, released when hidden/emptied/destroyed, native menu owns dismissal | yes | `expander_*` (9 tests) |
| MediaControls: song label / VerticalText (rotated, swapped measure), prev/toggle/next, popup button stack music/player, `available_player_icon` (generic names, missing files), idle hides label & disabled controls | yes | `media_*` (4) |

## Honest gaps / differences
- `time.tzset()`: GLib caches the local zone; a changed /etc/localtime is only seen after
  restart (GLib has no exposed refresh). TZ env changes are honoured.
- Python's `gc`/RSS growth bound is ported (`< 64 MiB`), not a byte-level memory comparison.
- Not ported here (E's DockSurface): clock centering in the bar, activities button, focused title,
  application expander placement/strip, app-menu unpin test, settings windows (G), auto-hide.
- Media test uses a fake status map + icon instead of MediaStatus with a fake MPRIS player
  (B's real-services bus test covers the data source); the Helium icon case is approximated by
  ThemedIcon fallbacks and a BytesIcon.
- `Escape` collapse and focus-visible expansion are ported but untested (no key injection).
- A GTK warning ("allocate GtkImage with width -14") appears when trays are emptied inside the
  CSS-less fake dock; harmless, from default button padding.

## Integrator wiring
See host.rs module doc. App: implement `DockApp` (status = `StatusBackend::status()`, actions,
`media_icon()`, `tray_backend` = `Rc<StatusTrayBackend>`, `panel_open` → `PanelActions::open`
(errors shown like Python), workspaces → `WorkspaceBackend::sorted_workspaces(Some(monitor))`,
`fullscreen_active` from `WindowBackend` windows); on status change call
`dock.indicators().refresh()` on every dock; `register_capture_controls(&app, docks)` at startup.
