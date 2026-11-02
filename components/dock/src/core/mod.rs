//! W-a: Tessel Dock's pure core model (no GTK widgets, no live session effects).
//!
//! Public API for the other tessel-dock modules:
//! - [`config`] — `DockConfig` (all Python fields, Python names; `Default`,
//!   `from_value` = `from_dict` with identical errors, `to_value` = `asdict`,
//!   `to_saved_value` = saved payload with `version`, `field_names()`,
//!   `effective_*()`, `is_group_visible`, `uses_separate_app_dock`,
//!   `for_app_dock`, `for_monitor`), `ConfigStore { path, error }` with
//!   `new(path)`, `default()`, `load(Option<&dyn PinCatalog>, Option<&Path>)`,
//!   `save(&DockConfig) -> Result<DockConfig, String>`; `save_bytes`,
//!   `cairo_pins`, `initial_pins`, constants `GROUPS`, `CORNERS`,
//!   `DEFAULT_APPS`, `ALWAYS_VISIBLE_APPS`, `BORDER_SIDES`.
//! - [`group_layout`] — `LAYOUT_GROUPS`, `POSITIONS`, `APP_DOCK_GROUPS`,
//!   `default_layout`, `default_app_dock_layout`, `validate_layouts`,
//!   `validate_app_dock_layout`, `Layout`/`Layouts`.
//! - [`ordered`] — `OrderedMap<V>` (Python dict order; used by dict fields).
//! - [`model`] — `DockItem`, `utility_partition`, `DockModel`, `HoverState`,
//!   the `WindowRecord`/`CatalogLookup` traits.
//! - [`tray_prefs`] — `tray_identity`, `tray_entries`.
//! - [`panel_actions`] — `application_command`, `PanelActions` (spawner-injected).
//! - [`app_catalog`] — `AppEntry`, `AppCategory`, `MENU_CATEGORIES`,
//!   `AppCatalog` (search/fuzzy/resolve), `LaunchHistory`, `launch_command`,
//!   `launch_environment`, `Launcher` (launch/launch_uri/launch_desktop_file/
//!   launch_process through a `Spawner`), `run_launch_helper`.
//! - [`focused_title`] — `FocusedTitle`.
//! - [`trash`] — `TrashEmptyOperation`, `DesktopActions` (GVFS on an injectable root).
//! - [`monitors`] — `monitor_label`, `monitor_entries`, `select_monitors`.
//! - [`spawn`] — `Spawner` trait, `GioSpawner` (real), `FakeSpawner` (tests).
//! - [`pyjson_loads`] — Python `json.loads`/UTF-8 decode error texts.

pub mod app_catalog;
pub mod config;
pub mod focused_title;
pub mod group_layout;
pub mod model;
pub mod monitors;
pub mod ordered;
pub mod panel_actions;
pub mod pyjson_loads;
pub mod spawn;
pub mod trash;
pub mod tray_prefs;

#[cfg(test)]
mod config_tests;
#[cfg(test)]
pub(crate) mod testutil;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod app_catalog_tests;
