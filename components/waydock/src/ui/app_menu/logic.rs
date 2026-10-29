//! Pure parts of the application menu, previews and pin drops (no widgets):
//! `app_menu_size`, the padded result grid and its arrow-key navigation,
//! `position_popup` arithmetic, `dropped_desktop_ids`, `pin_drop_index`.

use crate::core::app_catalog::{AppCatalog, AppEntry};
use gio::prelude::FileExt;
use suite_core::pyjson::py_strip;

/// Internal drag payload naming one desktop ID; external drops use file URIs.
pub const DRAG_PREFIX: &str = "waydock-app:";
/// `DockSurface.MENU_TILE_WIDTH`.
pub const MENU_TILE_WIDTH: i32 = 300;
/// `DockSurface.MENU_MAX_COLUMNS`.
pub const MENU_MAX_COLUMNS: i32 = 8;

/// What `app_menu_size` reads from the dock and its monitor.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MenuMetrics {
    /// `monitor.get_geometry()` width/height (logical pixels).
    pub monitor_width: i32,
    pub monitor_height: i32,
    /// Sum of the positive exclusive zones of every live *horizontal* dock
    /// clone on this monitor (`Layer.get_exclusive_zone(dock.reservation)`).
    pub reserved: i32,
    /// `self.horizontal`.
    pub horizontal: bool,
    /// `dock_dimensions()` height (the panel thickness on a horizontal edge).
    pub dock_height: i32,
    /// `self.edge_gap`.
    pub edge_gap: i32,
    /// `self.headroom` (magnifier headroom).
    pub headroom: i32,
    /// `self.window.get_allocated_width()` (vertical docks narrow the menu).
    pub dock_window_width: i32,
}

/// Python `//` for ints.
fn floor_div(a: i32, b: i32) -> i32 {
    a.div_euclid(b)
}

/// The width the menu may use before `app_menu_size` (toggle_app_menu).
pub fn available_width(metrics: &MenuMetrics) -> i32 {
    if metrics.horizontal {
        metrics.monitor_width - 32
    } else {
        metrics.monitor_width - metrics.dock_window_width - metrics.edge_gap - 7 - 16
    }
}

/// `DockSurface.app_menu_size(available_width, chrome)` → (body width, body
/// height, grid columns).
pub fn app_menu_size(metrics: &MenuMetrics, available_width: i32, chrome: i32) -> (i32, i32, i32) {
    let mut reserved = metrics.reserved;
    if metrics.horizontal {
        reserved = reserved.max(metrics.dock_height + metrics.edge_gap);
    }
    let usable_height = 160.max(metrics.monitor_height - reserved - metrics.headroom - 16);
    // Header, search and padding take about 160 logical pixels.
    let body_height = 120.max(((usable_height as f64 * 0.65) as i32).min(usable_height - 160));
    let width_cap = 1200.max(2400.min((metrics.monitor_width as f64 * 0.62) as i32));
    let body_width = 200.max(width_cap.min(available_width) - chrome);
    // Category sidebar (180), spacing and separator take about 210 pixels.
    let columns = 3.max(MENU_MAX_COLUMNS.min(floor_div(body_width - 210, MENU_TILE_WIDTH)));
    (body_width, body_height, columns)
}

/// Results read left to right, top to bottom; the incomplete final row is
/// padded with `None` so the FlowBox keeps its column count.
pub fn grid_cells<T: Clone>(entries: &[T], columns: usize) -> Vec<Option<T>> {
    let columns = columns.max(1);
    let mut cells = Vec::new();
    for row in entries.chunks(columns) {
        cells.extend(row.iter().cloned().map(Some));
        cells.extend(std::iter::repeat_n(None, columns - row.len()));
    }
    cells
}

/// Result of an arrow key on a grid tile (`grid_key`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridMove {
    /// Move focus to the search entry.
    Search,
    /// Select and focus the cell at this index.
    Select(usize),
    /// Consumed without moving.
    Stay,
}

/// Arrow direction `(row, column)` of a key name, as `grid_key` maps them.
pub fn arrow_direction(key: &str) -> Option<(i32, i32)> {
    Some(match key {
        "Left" | "KP_Left" => (0, -1),
        "Right" | "KP_Right" => (0, 1),
        "Up" | "KP_Up" => (-1, 0),
        "Down" | "KP_Down" => (1, 0),
        _ => return None,
    })
}

/// `grid_key` navigation: `has_app[i]` tells whether cell `i` is an
/// application (padding cells are not).
pub fn grid_move(index: usize, columns: usize, has_app: &[bool], direction: (i32, i32), search_at_bottom: bool) -> GridMove {
    let columns = columns.max(1) as i64;
    let (row, column) = (index as i64 / columns, index as i64 % columns);
    let (target_row, target_column) = (row + direction.0 as i64, column + direction.1 as i64);
    let count = has_app.len() as i64;
    if (search_at_bottom && target_row * columns >= count) || (!search_at_bottom && target_row < 0) {
        return GridMove::Search;
    }
    if target_row >= 0 && (0..columns).contains(&target_column) {
        let start = (target_row * columns).min(count) as usize;
        let end = (target_row * columns + columns).min(count) as usize;
        let candidates: Vec<usize> = (start..end).filter(|&i| has_app[i]).collect();
        if !candidates.is_empty() {
            let pick = (target_column as usize).min(candidates.len() - 1);
            return GridMove::Select(candidates[pick]);
        }
    }
    GridMove::Stay
}

/// `"{n} app"` / `"{n} apps"` header count (translated).
pub fn count_text(count: usize) -> String {
    if count == 1 {
        suite_core::tr!("{value1} app", value1 = count)
    } else {
        suite_core::tr!("{value1} apps", value1 = count)
    }
}

/// Row tooltip: name plus the description on a second line.
pub fn tile_tooltip(entry: &AppEntry) -> String {
    if entry.description.is_empty() {
        entry.name.clone()
    } else {
        format!("{}\n{}", entry.name, entry.description)
    }
}

// ---- Popup placement --------------------------------------------------------

/// Everything `DockSurface.position_popup(popup, anchor, alignment)` reads
/// from the dock. The host fills it; [`popup_margins`] does the arithmetic.
#[derive(Clone, Debug, PartialEq)]
pub struct PopupPlacement {
    /// `config.edge`: "top" | "bottom" | "left" | "right".
    pub edge: String,
    /// `self.edge_gap`.
    pub edge_gap: i32,
    /// Panel thickness: `dock_dimensions()` height (horizontal) or width.
    pub thickness: i32,
    /// `self.headroom`.
    pub headroom: i32,
    /// Monitor width (horizontal) or height (vertical).
    pub total: i32,
    /// `dock_dimensions()` width (horizontal) or height (vertical).
    pub dock_length: i32,
    /// `config.alignment`: "start" | "center" | "end".
    pub dock_alignment: String,
    /// Separate app dock: `_panel_frost_geometry()[2 or 3]` replaces the origin.
    pub app_dock_origin: Option<f64>,
    /// Anchor widget translated into the dock window: (coordinate along the
    /// dock, allocated extent). `None` = no anchor or translation failed.
    pub anchor: Option<(f64, f64)>,
    /// Popup alignment: "center" (default) or "start" (the app dock's menu
    /// button anchor).
    pub alignment: String,
    /// Smallest side margin: 8, or `_top_bar_span() + 8` for a vertical
    /// separate app dock.
    pub minimum: f64,
}

impl PopupPlacement {
    pub fn horizontal(&self) -> bool {
        matches!(self.edge.as_str(), "top" | "bottom")
    }
}

/// (margin on the dock edge, margin on the leading side) for a popup whose
/// natural length along the dock is `length`.
pub fn popup_margins(placement: &PopupPlacement, length: i32) -> (i32, i32) {
    let edge_margin = placement.edge_gap + placement.thickness + placement.headroom + 7;
    let total = placement.total as f64;
    let dock_length = placement.dock_length as f64;
    let mut origin = (total - dock_length) / 2.0;
    if placement.dock_alignment == "start" {
        origin = placement.edge_gap as f64;
    } else if placement.dock_alignment == "end" {
        origin = total - dock_length - placement.edge_gap as f64;
    }
    if let Some(app_dock) = placement.app_dock_origin {
        origin = app_dock;
    }
    let (coordinate, extent) = placement.anchor.unwrap_or((0.0, dock_length));
    let mut position = origin + coordinate;
    if placement.alignment != "start" {
        position += (extent - length as f64) / 2.0;
    }
    let margin = placement.minimum.max((total - length as f64 - 8.0).min(position));
    (edge_margin, margin as i32)
}

// ---- Drops -------------------------------------------------------------------

/// A dropped value: a `G_TYPE_STRING` or a `GdkFileList` (local paths;
/// `None` for non-local files).
#[derive(Clone, Debug, PartialEq)]
pub enum DropValue {
    Text(String),
    Files(Vec<Option<String>>),
}

/// Python `str.splitlines()`.
pub fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let breaks = matches!(ch, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if breaks {
            lines.push(&text[start..index]);
            let mut next = index + ch.len_utf8();
            if ch == '\r' {
                if let Some(&(_, '\n')) = chars.peek() {
                    chars.next();
                    next += 1;
                }
            }
            start = next;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `DockSurface.dropped_desktop_ids(value)`: installed desktop IDs named by a
/// dropped value, in order.
pub fn dropped_desktop_ids(value: &DropValue, catalog: &AppCatalog) -> Vec<String> {
    let paths: Vec<String> = match value {
        DropValue::Text(text) => {
            if let Some(ident) = text.strip_prefix(DRAG_PREFIX) {
                return if ident.is_empty() { Vec::new() } else { vec![ident.to_owned()] };
            }
            py_splitlines(text)
                .into_iter()
                .map(py_strip)
                .filter(|line| line.starts_with("file://"))
                .map(|line| gio::File::for_uri(line).path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())
                .collect()
        }
        DropValue::Files(files) => files.iter().map(|p| p.clone().unwrap_or_default()).collect(),
    };
    let mut result: Vec<String> = Vec::new();
    for path in paths {
        if !path.ends_with(".desktop") {
            continue;
        }
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let found = catalog.apps().iter().find(|app| app.filename == path).or_else(|| catalog.get(&name));
        if let Some(app) = found {
            if !result.contains(&app.id) {
                result.push(app.id.clone());
            }
        }
    }
    result
}

/// `pin_drop_index(x, y)`: `buttons` are the visible pinned buttons as
/// (start coordinate in the panel along the dock, extent); a button whose
/// translation failed is `None` and does not count.
pub fn pin_drop_index(coordinate: f64, buttons: &[Option<(f64, f64)>]) -> usize {
    buttons.iter().flatten().filter(|(start, extent)| coordinate > start + extent / 2.0).count()
}

/// `_dropped`: the pin the drop goes before (`None` = append).
pub fn drop_before(index: usize, pinned: &[String]) -> Option<String> {
    pinned.get(index).cloned()
}

/// `scroll_horizontal`: the new adjustment value, or `None` when the strip
/// cannot scroll (Python returns False and leaves the event alone).
pub fn scroll_value(lower: f64, upper: f64, page: f64, value: f64, dx: f64, dy: f64, icon_size: i32) -> Option<f64> {
    let maximum = lower.max(upper - page);
    if maximum <= lower {
        return None;
    }
    let delta = if dx.abs() > dy.abs() { dx } else { dy };
    Some(lower.max(maximum.min(value + delta * (icon_size + 18) as f64)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::app_catalog::AppInfoData;

    fn metrics(width: i32, height: i32) -> MenuMetrics {
        MenuMetrics { monitor_width: width, monitor_height: height, horizontal: true, ..Default::default() }
    }

    /// test_app_menu_size_follows_monitor_and_frequent_row (size part).
    #[test]
    fn app_menu_size_follows_monitor() {
        for (width, height, columns) in [(1280, 720, Some(3)), (1920, 1080, Some(3)), (3840, 2160, None)] {
            let m = MenuMetrics { dock_height: 56, edge_gap: 8, ..metrics(width, height) };
            let (_body_width, body_height, computed) = app_menu_size(&m, width - 32, 0);
            assert!(body_height <= height - 160, "{width}");
            assert!(body_height >= 120);
            match columns {
                Some(c) => assert_eq!(computed, c, "{width}"),
                None => {
                    assert!(computed > 3);
                    assert!(body_height > 440);
                }
            }
        }
        // Python arithmetic, step by step (1920x1080, bottom dock 56 + gap 8).
        let m = MenuMetrics { dock_height: 56, edge_gap: 8, ..metrics(1920, 1080) };
        assert_eq!(app_menu_size(&m, 1888, 34), (1166, 650, 3));
        // 4K: width cap = int(3840 * .62) = 2380.
        assert_eq!(app_menu_size(&metrics(3840, 2160), 3808, 0), (2380, 1393, 7));
        // Tiny screens keep the floor values and three columns (Python // floors).
        assert_eq!(app_menu_size(&metrics(300, 200), 100, 50), (200, 120, 3));
        // Bars on the same output reserve their zones; a vertical dock does not add its own.
        let vertical = MenuMetrics { horizontal: false, reserved: 40, dock_height: 900, ..metrics(1920, 1080) };
        assert_eq!(app_menu_size(&vertical, 1500, 0).1, 665);
    }

    #[test]
    fn available_width_vertical_subtracts_dock() {
        assert_eq!(available_width(&metrics(1920, 1080)), 1888);
        let m = MenuMetrics { horizontal: false, dock_window_width: 64, edge_gap: 8, ..metrics(1920, 1080) };
        assert_eq!(available_width(&m), 1920 - 64 - 8 - 7 - 16);
    }

    #[test]
    fn grid_pads_final_row() {
        let cells = grid_cells(&["a", "b", "c", "d", "e", "f", "g"], 3);
        assert_eq!(cells, vec![Some("a"), Some("b"), Some("c"), Some("d"), Some("e"), Some("f"), Some("g"), None, None]);
        assert!(grid_cells::<&str>(&[], 3).is_empty());
        assert_eq!(grid_cells(&["a", "b", "c"], 3).len(), 3);
    }

    /// The keyboard half of test_app_grid_search_placement_spacing_order_and_keyboard_on_every_edge.
    #[test]
    fn grid_navigation_matches_grid_key() {
        // 4 apps + 2 padding cells in 3 columns.
        let apps = [true, true, true, true, false, false];
        // Top search: Up from the first row returns to the search entry.
        assert_eq!(grid_move(0, 3, &apps, (-1, 0), false), GridMove::Search);
        assert_eq!(grid_move(0, 3, &apps, (0, 1), false), GridMove::Select(1));
        assert_eq!(grid_move(0, 3, &apps, (1, 0), false), GridMove::Select(3));
        // Down from column 2 lands on the last real app of the next row.
        assert_eq!(grid_move(2, 3, &apps, (1, 0), false), GridMove::Select(3));
        // Bottom search: Down past the last row returns to search.
        assert_eq!(grid_move(0, 3, &apps, (1, 0), true), GridMove::Select(3));
        assert_eq!(grid_move(3, 3, &apps, (1, 0), true), GridMove::Search);
        // Bottom search, Up from the first row stays (the search is below).
        assert_eq!(grid_move(0, 3, &apps, (-1, 0), true), GridMove::Stay);
        // Left of the first column stays.
        assert_eq!(grid_move(3, 3, &apps, (0, -1), false), GridMove::Stay);
        // Row without apps beyond the end (top search): stays.
        assert_eq!(grid_move(3, 3, &apps, (1, 0), false), GridMove::Stay);
        assert_eq!(arrow_direction("KP_Down"), Some((1, 0)));
        assert_eq!(arrow_direction("Return"), None);
    }

    #[test]
    fn popup_margins_follow_position_popup() {
        let base = PopupPlacement {
            edge: "bottom".into(), edge_gap: 8, thickness: 56, headroom: 0, total: 1920, dock_length: 600,
            dock_alignment: "center".into(), app_dock_origin: None, anchor: None, alignment: "center".into(), minimum: 8.0,
        };
        // Centered over the dock.
        assert_eq!(popup_margins(&base, 400), (71, 760));
        // Over an anchor at 100..148 of the dock.
        let anchored = PopupPlacement { anchor: Some((100.0, 48.0)), ..base.clone() };
        assert_eq!(popup_margins(&anchored, 300), (71, 660 + 100 - 126));
        // Clamped to the screen ends.
        let start = PopupPlacement { dock_alignment: "start".into(), anchor: Some((0.0, 48.0)), ..base.clone() };
        assert_eq!(popup_margins(&start, 400).1, 8);
        let end = PopupPlacement { dock_alignment: "end".into(), anchor: Some((590.0, 48.0)), ..base.clone() };
        assert_eq!(popup_margins(&end, 400).1, 1920 - 400 - 8);
        // App dock origin + start alignment, top-bar minimum.
        let app = PopupPlacement { app_dock_origin: Some(30.0), alignment: "start".into(), anchor: Some((4.0, 40.0)),
                                   minimum: 40.0, edge: "left".into(), ..base.clone() };
        assert_eq!(popup_margins(&app, 500), (71, 40));
        assert!(!app.horizontal());
        // Headroom clears magnified icons.
        assert_eq!(popup_margins(&PopupPlacement { headroom: 30, ..base }, 10).0, 101);
    }

    #[test]
    fn splitlines_like_python() {
        assert_eq!(py_splitlines("a\r\nb\nc\rd"), vec!["a", "b", "c", "d"]);
        assert_eq!(py_splitlines("a\n"), vec!["a"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("\n\nx"), vec!["", "", "x"]);
    }

    fn catalog() -> AppCatalog {
        let mut terminal = AppInfoData::fake("terminal.desktop", "Terminal");
        terminal.filename = Some("/opt/apps/term.desktop".into());
        AppCatalog::from_infos(vec![
            AppInfoData::fake("browser.desktop", "Web Browser"),
            AppInfoData::fake("files.desktop", "Files"),
            terminal,
        ])
    }

    /// test_drops_pin_and_reorder_relative_to_pinned_icons (identity part).
    #[test]
    fn dropped_ids_follow_python() {
        let catalog = catalog();
        let text = |s: &str| DropValue::Text(s.into());
        assert_eq!(dropped_desktop_ids(&text("waydock-app:files.desktop"), &catalog), vec!["files.desktop"]);
        assert!(dropped_desktop_ids(&text("waydock-app:"), &catalog).is_empty());
        assert_eq!(
            dropped_desktop_ids(&text("file:///usr/share/applications/files.desktop\r\nfile:///tmp/notes.txt"), &catalog),
            vec!["files.desktop"]
        );
        assert!(dropped_desktop_ids(&text("file:///tmp/unknown.desktop"), &catalog).is_empty());
        assert!(dropped_desktop_ids(&text("plain text"), &catalog).is_empty());
        // A file list; filename match wins, basename fallback, duplicates once.
        let files = DropValue::Files(vec![Some("/opt/apps/term.desktop".into()), Some("/elsewhere/files.desktop".into()),
                                          Some("/usr/share/applications/files.desktop".into()), None]);
        assert_eq!(dropped_desktop_ids(&files, &catalog), vec!["terminal.desktop", "files.desktop"]);
        // Percent-encoded URIs decode like Gio.File.
        assert_eq!(dropped_desktop_ids(&text("  file:///usr/share/applications/%66iles.desktop  "), &catalog), vec!["files.desktop"]);
    }

    #[test]
    fn drop_index_counts_buttons_before_the_pointer() {
        let buttons = [Some((0.0, 50.0)), None, Some((50.0, 50.0)), Some((100.0, 50.0))];
        assert_eq!(pin_drop_index(-1000.0, &buttons), 0);
        assert_eq!(pin_drop_index(26.0, &buttons), 1);
        assert_eq!(pin_drop_index(25.0, &buttons), 0);
        assert_eq!(pin_drop_index(100000.0, &buttons), 3);
        let pinned = vec!["browser.desktop".to_owned(), "terminal.desktop".to_owned()];
        assert_eq!(drop_before(0, &pinned).as_deref(), Some("browser.desktop"));
        assert_eq!(drop_before(2, &pinned), None);
    }

    #[test]
    fn horizontal_scroll_value() {
        assert_eq!(scroll_value(0.0, 100.0, 100.0, 0.0, 1.0, 0.0, 48), None);
        assert_eq!(scroll_value(0.0, 1000.0, 200.0, 0.0, 0.0, 1.0, 48), Some(66.0));
        assert_eq!(scroll_value(0.0, 1000.0, 200.0, 790.0, 2.0, 1.0, 48), Some(800.0));
        assert_eq!(scroll_value(0.0, 1000.0, 200.0, 10.0, 0.5, -1.0, 48), Some(0.0));
    }

    #[test]
    fn count_and_tooltip_text() {
        assert_eq!(count_text(1), "1 app");
        assert_eq!(count_text(0), "0 apps");
        assert_eq!(count_text(12), "12 apps");
        let mut entry = AppEntry::new("a.desktop", "Alpha");
        assert_eq!(tile_tooltip(&entry), "Alpha");
        entry.description = "Does things".into();
        assert_eq!(tile_tooltip(&entry), "Alpha\nDoes things");
    }
}
