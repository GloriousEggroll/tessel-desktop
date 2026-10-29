# PARITY-F1 — application menu, window previews, per-app audio (W-f part 2)

Files: `src/ui/app_menu.rs` (+ `app_menu/{logic,popup,drag}.rs`), `src/ui/preview.rs`,
`src/ui/app_audio.rs`, `tests/f1_ui_headless.rs`, `tests/run_f1_headless.sh`.

Run: `cargo test -p waydock --lib ui::` (30 unit tests) and
`components/waydock/tests/run_f1_headless.sh [filter]` (17 cases, private labwc, 2 outputs,
private bus without service dirs, wlrctl/wtype only inside it). Both pass.

| Contract item | Done | Test |
|---|---|---|
| §6 `waydock-applications` OVERLAY, keyboard EXCLUSIVE while open, `<popup_class>-applications` | yes | `native_dismissal_pin_and_keyboard`, `search_pin_unpin_and_launch` |
| §6 `waydock-menu-dismiss` per monitor, all edges, dismiss on release; hidden during menu drags | yes | `native_dismissal_pin_and_keyboard` (both outputs, then dock click not swallowed), `drag_hides_backdrops_and_drop_pins` |
| Escape / stop-search / close-request / focus loss (not while dragging) dismiss | yes | `native_dismissal_pin_and_keyboard` (real wtype Escape) |
| Search ranking (core `AppCatalog::search`), global search unselects category, empty state, activate without results inert | yes | `ranked_search_launches_with_gdk_context`, `search_is_global_and_restores_category`, `grid_placement_order_and_keyboard` |
| Categories (only populated; hidden-only absent; System icon `computer-symbolic`; All first, Other last), category click clears search, reopen resets | yes | `category_click_pin_launch_and_reopen`, `search_is_global…` |
| Frequent row (history, `columns` tiles + spacers, only when browsing All without query, drag source, click launches) | yes | `frequent_row` |
| Pin stars: `waydock-app-pin`, `.pinned`, hover/selected/focus-within CSS (APP+3, once per display), pin keeps menu open & category | yes | `search_pin_unpin_and_launch`, `category_click_pin…` (native pointer: pin click does not launch) |
| Bottom edge: search below, results valign START, A→Z top-to-bottom on every edge, scrolled to start | yes | `grid_placement_order_and_keyboard`, `three_columns_and_reading_order_on_every_edge` |
| `app_menu_size` exact arithmetic (Python `int()`/`//`) | yes | `logic::app_menu_size_follows_monitor`, `available_width_vertical_subtracts_dock` |
| Grid padding cells (`waydock-empty-cell`, insensitive, unfocusable, inert) | yes | `grid_pads_final_row`, `grid_placement…` |
| Keyboard: search Up/Down toward results, arrows between tiles, back to search, Return/KP_Enter/space launch, pin focus not a launch | yes | `grid_navigation_matches_grid_key`, `grid_placement…`, native Up/Right/Return |
| Launch via core `Launcher` with a GDK launch context (timestamp 0) | yes | `ranked_search_launches_with_gdk_context` (systemd-run argv, WAYLAND_DISPLAY from context, INVOCATION_ID stripped) |
| Drag string `waydock-app:<id>`, `dropped_desktop_ids` (prefix, file URIs, FileList, filename then basename, dedupe), `pin_drop_index`, `_dropped` → `place_pin(id, before)` | yes | `dropped_ids_follow_python`, `drop_index_counts…`, `drag_hides_backdrops_and_drop_pins` |
| §5 window click: focused & unminimized → minimize else activate (state at click time); error text | yes | `preview_minimizes_focused_and_restores` |
| Multi-window preview `waydock-windows` TOP, orientation per edge, 220 ms hover delay, blocked by menu/tray expander | yes | `preview_activates_exact_window`, `preview_refresh_mute_and_stale` |
| Close button per row closes only that window; stale/refused handling; Close All snapshots ids | yes | `native_preview_close_targets_one_window` (real pointer), `preview_close_requests` |
| Rows kept across refreshes; titles/tooltip/“Close {title}” label; `focused` class | yes | `preview_refresh_mute_and_stale`, `preview_activates_exact_window` |
| Per-app mute in previews | yes (subprocess) | `app_audio::*` (port of shared/test_application_audio.py + stream parser + fixture source), `preview_refresh_mute_and_stale` |
| `position_popup` arithmetic (edge margin, alignment, anchor, clamps, app-dock origin/min) | yes | `popup_margins_follow_position_popup`, edge margin asserted in `grid_placement…` |
| test_app_popup_style.py | yes | `app_menu::popup_style_tests::*` (against `appearance::style`) |

| Fix-up: `pin_drop_index` middle = translated origin + `get_allocated_width()/2` (CSS margin box) | yes | `drop_index_uses_margin_box` |
| Fix-up: menu tile icons reuse cached paintables (`popup::menu_icon_image`; GTK 4.22 re-loads leak per open) | yes | `menu_reuses_icon_paintables`, e_headless `menu_churn_does_not_grow_memory` |

## Gaps / notes (honest)
- **Audio backend**: no WirePlumber dev files → `pw-dump --monitor` (event-driven, one child
  per open preview) + `wpctl set-mute`. A write counts as successful once wpctl starts; the
  reused-id guard compares `object.serial`; "ready" = first dump. Documented in app_audio.rs.
- **SCALE_DOWN**: waydock previews have no thumbnails (icon + title only); `ContentFit.SCALE_DOWN`
  is the wayshell switcher's. Nothing to port here.
- **Running badge** (`waydock-count`, visible only when count > 1) lives in the dock button
  (`item_button`/`update_item_button`) → agent E.
- Not ported here (E/D): menu palette-follow test (`test_selected_category_follows_wallpaper_palette…`
  — needs D's CSS + E's set_background), `background_activate`, menu hotkey/probe, utility routing.
- `install_button_drag_source` / `install_drop_target` / `dropped` are provided for E's dock
  buttons and pinned strip; E must call `preview.queue/cancel_timer/update` from button hover/render.
