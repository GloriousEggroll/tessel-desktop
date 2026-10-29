//! Waydock: the suite's dock/panel (Rust port of ~/btmanager/waydock; the
//! external contract is contracts/waydock.md). The binary replaces only the
//! `waydock.service` process; the Python modules other components import
//! stay installed.
//!
//! Module ownership during the port (one owner per directory):
//! - `core`       — W-a pure model: DockConfig/ConfigStore, group layouts,
//!                  dock model + hover, tray preferences, panel actions,
//!                  app catalog + launch argv, focused title, trash, monitors.
//! - `dbus`       — W-c owned D-Bus: org.waydock.Settings, SNI watcher/host,
//!                  dbusmenu → GMenu, sd_notify readiness.
//! - `status`     — W-c indicator sources (streams, wpctl, BlueZ, keep-awake,
//!                  discovery, capture, media).
//! - `wl`         — W-b Wayland backends (windows, workspaces, fullscreen guard,
//!                  show desktop).
//! - `appearance` — W-d publishers (palette.json, gtk.css, window buttons,
//!                  papirus, portal colour scheme, wallpaper sampling, CSS).
//! - `ui`, `app`, `cli` — W-e/W-f/W-g GTK surfaces, widgets, settings, app.

pub mod app;
pub mod appearance;
pub mod cli;
pub mod core;
pub mod dbus;
pub mod status;
pub mod ui;
pub mod wl;
