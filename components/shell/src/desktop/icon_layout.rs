//! Port of `desktop_icon_layout.py`: place desktop icon cells inside each
//! monitor's available workspace. Positions are global compositor pixels
//! keyed by file name, kept as a `serde_json::Map` (insertion order = Python
//! dict order) so the positions file is rewritten exactly like Python.

use serde_json::{Map, Value};

pub type Rect = (i64, i64, i64, i64);
pub type Point = (i64, i64);
/// `(monitor geometry, usable icon area)`.
pub type Region = (Rect, Rect);
pub type Positions = Map<String, Value>;

pub const CELL: (i64, i64) = (128, 128);
/// Keep the icon grid (and its labels) off the very edge of the usable area.
pub const EDGE_MARGIN: i64 = 12;

pub fn inset(area: Rect) -> Rect {
    inset_by(area, EDGE_MARGIN)
}

pub fn inset_by(area: Rect, margin: i64) -> Rect {
    let (x, y, width, height) = area;
    (x + margin, y + margin, (width - 2 * margin).max(0), (height - 2 * margin).max(0))
}

pub fn contains(area: Rect, point: Point) -> bool {
    contains_sized(area, point, CELL)
}

pub fn contains_sized(area: Rect, point: Point, size: (i64, i64)) -> bool {
    let (x, y, width, height) = area;
    let (px, py) = point;
    x <= px && y <= py && px + size.0 <= x + width && py + size.1 <= y + height
}

pub fn clamp_point(point: Point, area: Rect) -> Point {
    let (x, y, width, height) = area;
    (x.max(point.0.min(x + (width - CELL.0).max(0))), y.max(point.1.min(y + (height - CELL.1).max(0))))
}

/// Column-major cells of an area.
pub fn cells(area: Rect) -> impl Iterator<Item = Point> {
    let (x, y, width, height) = area;
    let columns = (width.div_euclid(CELL.0)).max(0);
    let rows = (height.div_euclid(CELL.1)).max(0);
    (0..columns).flat_map(move |column| (0..rows).map(move |row| (x + column * CELL.0, y + row * CELL.1)))
}

pub fn overlaps<'a>(point: Point, occupied: impl IntoIterator<Item = &'a Point>) -> bool {
    occupied.into_iter().any(|&(px, py)| (point.0 - px).abs() < CELL.0 && (point.1 - py).abs() < CELL.1)
}

/// A saved position as Python's validation sees it: a 2-list of finite
/// int/float (never bool) values, truncated like `int()`.
pub fn point_of(value: &Value) -> Option<Point> {
    let items = value.as_array()?;
    if items.len() != 2 {
        return None;
    }
    let mut out = [0i64; 2];
    for (slot, item) in out.iter_mut().zip(items) {
        let Value::Number(number) = item else { return None };
        *slot = if let Some(int) = number.as_i64() {
            int
        } else {
            let float = number.as_f64()?;
            if !float.is_finite() {
                return None;
            }
            float.trunc() as i64
        };
    }
    // Python ints are unbounded; keep arithmetic on huge saved values
    // overflow-free (they are clamped onto a monitor anyway).
    const LIMIT: i64 = 1 << 40;
    Some((out[0].clamp(-LIMIT, LIMIT), out[1].clamp(-LIMIT, LIMIT)))
}

pub fn point_value(point: Point) -> Value {
    Value::Array(vec![point.0.into(), point.1.into()])
}

fn occupied(positions: &Positions) -> Vec<Point> {
    positions.values().filter_map(point_of).collect()
}

/// Scan monitors vertically, avoiding screen gaps and occupied cells.
pub fn next_slot(positions: &Positions, regions: &[Region]) -> Point {
    next_slot_among(&occupied(positions), regions)
}

pub fn next_slot_among(occupied: &[Point], regions: &[Region]) -> Point {
    for (_geometry, area) in regions {
        for point in cells(*area) {
            if !overlaps(point, occupied) {
                return point;
            }
        }
    }
    // A completely full desktop cannot offer a non-overlapping cell. Keep new
    // items reachable on a real monitor instead of placing them off-screen.
    match regions.last() {
        Some((_, area)) => clamp_point((area.0 + area.2, area.1 + area.3), *area),
        None => (0, 0),
    }
}

fn shifted(value: &Value, delta: i64) -> Value {
    match value {
        Value::Number(n) if n.as_i64().is_some() => (n.as_i64().unwrap() + delta).into(),
        Value::Number(n) => serde_json::Number::from_f64(n.as_f64().unwrap_or(0.0) + delta as f64).map(Value::Number).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// Move every cell on a monitor by that monitor's workarea offset, so a bar
/// changing edges carries the whole arrangement with it.
pub fn shift_positions(positions: &Positions, moves: &[(Rect, (i64, i64))]) -> Positions {
    let mut result = Map::new();
    for (name, value) in positions {
        let mut value = value.clone();
        if let Some(point) = point_of(&value) {
            for (geometry, (dx, dy)) in moves {
                if contains_sized(*geometry, point, (1, 1)) {
                    let items = value.as_array().unwrap();
                    value = Value::Array(vec![shifted(&items[0], *dx), shifted(&items[1], *dy)]);
                    break;
                }
            }
        }
        result.insert(name.clone(), value);
    }
    result
}

/// Move only obscured/off-screen cells, retaining their monitor if possible.
pub fn constrain_positions(positions: &Positions, regions: &[Region]) -> Positions {
    if regions.is_empty() {
        return positions.clone();
    }
    let normalized: Vec<(String, Point)> = positions.iter().filter_map(|(name, value)| point_of(value).map(|p| (name.clone(), p))).collect();
    let mut result: Vec<(String, Point)> = normalized
        .iter()
        .filter(|(_, point)| regions.iter().any(|(_, area)| contains(*area, *point)))
        .cloned()
        .collect();
    for (name, point) in &normalized {
        if result.iter().any(|(known, _)| known == name) {
            continue;
        }
        let own = regions.iter().find(|(geometry, _)| contains_sized(*geometry, *point, (1, 1))).copied().unwrap_or_else(|| {
            let distance = |region: &Region| {
                let clamped = clamp_point(*point, region.1);
                (point.0 - clamped.0).pow(2) + (point.1 - clamped.1).pow(2)
            };
            // `min` keeps the first of equal candidates.
            let mut best = regions[0];
            for region in &regions[1..] {
                if distance(region) < distance(&best) {
                    best = *region;
                }
            }
            best
        });
        let mut candidate = clamp_point(*point, own.1);
        let taken: Vec<Point> = result.iter().map(|(_, p)| *p).collect();
        if overlaps(candidate, &taken) {
            let mut order = vec![own];
            order.extend(regions.iter().filter(|region| **region != own).copied());
            candidate = next_slot_among(&taken, &order);
        }
        result.push((name.clone(), candidate));
    }
    result.into_iter().map(|(name, point)| (name, point_value(point))).collect()
}

#[cfg(test)]
mod tests {
    //! test_desktop_icon_layout.py, 1:1.
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Positions {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn grid_keeps_a_margin_from_every_edge() {
        assert!(EDGE_MARGIN >= 12);
        let area = inset((0, 40, 1920, 1000));
        assert_eq!(area, (EDGE_MARGIN, 40 + EDGE_MARGIN, 1920 - 2 * EDGE_MARGIN, 1000 - 2 * EDGE_MARGIN));
        assert_eq!(cells(area).next(), Some((EDGE_MARGIN, 40 + EDGE_MARGIN)));
        assert_eq!(inset((0, 0, 10, 10)), (EDGE_MARGIN, EDGE_MARGIN, 0, 0));
    }

    #[test]
    fn bar_moving_edges_carries_the_whole_arrangement() {
        let (left, right) = ((0, 0, 1920, 1080), (1920, 0, 1920, 1080));
        let positions = map(json!({"a": [0, 0], "b": [0, 128], "c": [128, 0], "other": [1920, 0]}));
        let moved = shift_positions(&positions, &[(left, (70, 32))]);
        assert_eq!(Value::Object(moved.clone()), json!({"a": [70, 32], "b": [70, 160], "c": [198, 32], "other": [1920, 0]}));
        let regions = [(left, (70, 32, 1850, 1048)), (right, right)];
        assert_eq!(constrain_positions(&moved, &regions), moved);
    }

    #[test]
    fn all_edges_reflow_obscured_cells_without_moving_other_monitor() {
        let full = (0, 0, 1024, 768);
        let other = (1180, -100, 800, 600);
        for (edge, area, blocked) in [
            ("top", (0, 52, 1024, 716), (0, 0)),
            ("bottom", (0, 0, 1024, 716), (0, 640)),
            ("left", (52, 0, 972, 768), (0, 0)),
            ("right", (0, 0, 972, 768), (896, 0)),
        ] {
            let positions = map(json!({"obscured": [blocked.0, blocked.1], "clear": [256, 256], "other": [1308, 28]}));
            let result = constrain_positions(&positions, &[(full, area), (other, other)]);
            assert!(contains(area, point_of(&result["obscured"]).unwrap()), "{edge}");
            assert_eq!(result["clear"], positions["clear"], "{edge}");
            assert_eq!(result["other"], positions["other"], "{edge}");
            assert_eq!(positions["obscured"], json!([blocked.0, blocked.1]), "Input must remain unchanged");
        }
    }

    #[test]
    fn floating_gap_increase_reflows_only_newly_obscured_cells() {
        let full = (0, 0, 1024, 768);
        let positions = map(json!({"near_panel": [128, 40], "clear": [384, 256]}));
        let result = constrain_positions(&positions, &[(full, (0, 52, 1024, 716))]);
        assert_eq!(result["near_panel"], json!([128, 52]));
        assert_eq!(result["clear"], positions["clear"]);
        assert_eq!(constrain_positions(&result, &[(full, full)]), result, "Releasing a reservation must not shuffle valid icons");
    }

    #[test]
    fn new_cells_fill_real_workareas_instead_of_gaps() {
        let left = ((-700, -200, 512, 384), (-648, -200, 460, 384));
        let right = ((100, 50, 512, 384), (100, 102, 512, 332));
        let mut positions = Positions::new();
        let capacity = cells(left.1).count() + cells(right.1).count();
        for index in 0..capacity {
            let point = next_slot(&positions, &[left, right]);
            assert!([left, right].iter().any(|region| contains(region.1, point)));
            assert!(!overlaps(point, &occupied(&positions)));
            positions.insert(index.to_string(), point_value(point));
        }
        assert_eq!(positions["0"], json!([-648, -200]));
        assert!(contains(right.1, next_slot(&positions, &[left, right])), "Even a full desktop must keep new icons reachable");
    }

    #[test]
    fn reflow_avoids_existing_cells_and_prefers_original_monitor() {
        let left = ((0, 0, 768, 512), (0, 52, 768, 460));
        let right = ((768, 0, 768, 512), (768, 0, 768, 512));
        let result = constrain_positions(&map(json!({"fixed": [0, 52], "obscured": [0, 0]})), &[left, right]);
        assert_eq!(result["fixed"], json!([0, 52]));
        let obscured = point_of(&result["obscured"]).unwrap();
        assert!(contains(left.1, obscured));
        assert!(!overlaps(obscured, &[(0, 52)]));
    }

    #[test]
    fn offscreen_and_invalid_saved_positions_do_not_break_layout() {
        let full = (0, 0, 768, 512);
        // NaN cannot be stored in a serde_json value (Python's float("nan")
        // case); point_of rejects it below. Other malformed shapes drop too.
        let positions = map(json!({"removed_monitor": [3000, 800], "string": "bad", "bool": [true, 0], "short": [1], "huge": [1e300, 0]}));
        let result = constrain_positions(&positions, &[(full, (0, 40, 768, 472))]);
        assert_eq!(result.keys().cloned().collect::<Vec<_>>(), ["removed_monitor", "huge"]);
        assert!(contains((0, 40, 768, 472), point_of(&result["removed_monitor"]).unwrap()));
        assert!(point_of(&json!([f64::NAN, 0])).is_none());
    }

    #[test]
    fn floats_truncate_like_int() {
        assert_eq!(point_of(&json!([12.9, -3.7])), Some((12, -3)));
        assert_eq!(point_of(&json!([1, "2"])), None);
    }
}
