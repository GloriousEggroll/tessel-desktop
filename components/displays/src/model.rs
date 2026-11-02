//! Pure logic of the Displays window: transforms, the arrangement model of
//! `MonitorLayout` (positions, snapping, recentering) and the `save_config`
//! CFG_SET / CFG_DEL computation (tessel-displays:1379-1431, 1432-2171,
//! 2669-2897).

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use tessel_core::pyjson::{py_str, truthy};

pub const SCALE: f64 = 0.08;

/// Python `str(value)` of a transform (`0` → `"0"`, `None` → `"None"`).
pub fn transform_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Bool(b)) => if *b { "True".into() } else { "False".into() },
        Some(other) => py_str(other),
    }
}

/// `(angle, is_flipped)` of `'90'`, `'FLIPPED'`, `'FLIPPED-270'`…
pub fn parse_transform(transform: &str) -> (i64, bool) {
    if transform.is_empty() {
        return (0, false);
    }
    let transform = transform.to_uppercase();
    let mut angle = 0;
    let mut flipped = false;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if transform.contains("FLIPPED") {
        flipped = true;
        if transform.contains('-') {
            let parts: Vec<&str> = transform.split('-').collect();
            if parts.len() == 2 && digits(parts[1]) {
                angle = parts[1].parse::<i64>().unwrap_or(0);
            }
        }
    } else if digits(&transform) {
        angle = transform.parse::<i64>().unwrap_or(0);
    }
    (angle.rem_euclid(360), flipped)
}

/// Width and height adjusted for rotation (`str(transform) in ("90", "270")`).
pub fn rotated_dimensions(width: i64, height: i64, transform: &str) -> (i64, i64) {
    if transform == "90" || transform == "270" { (height, width) } else { (width, height) }
}

pub fn snap_to_edges(mut x: i64, mut y: i64, rects: &[(i64, i64, i64, i64)], width: i64, height: i64) -> (i64, i64) {
    let (margin, threshold) = (0, 30);
    for &(rx, ry, rw, rh) in rects {
        if (x - (rx + rw)).abs() <= threshold {
            x = rx + rw + margin;
        } else if ((x + width) - rx).abs() <= threshold {
            x = rx - width - margin;
        }
        if (y - (ry + rh)).abs() <= threshold {
            y = ry + rh + margin;
        } else if ((y + height) - ry).abs() <= threshold {
            y = ry - height - margin;
        }
    }
    (x, y)
}

fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key)
}

/// `value.get(key, {})` as an object (missing → empty).
fn object<'a>(value: &'a Value, key: &str, empty: &'a Value) -> &'a Value {
    value.get(key).unwrap_or(empty)
}

fn int_of(value: Option<&Value>, default: i64) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(default as f64) as i64),
        Some(Value::Bool(b)) => *b as i64,
        _ => default,
    }
}

fn float_of(value: Option<&Value>, default: f64) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::Bool(b)) => *b as i64 as f64,
        _ => default,
    }
}

/// Python `key in container` for a dict (keys) or list (items).
pub fn contains(container: Option<&Value>, key: &str) -> bool {
    match container {
        Some(Value::Object(map)) => map.contains_key(key),
        Some(Value::Array(items)) => items.iter().any(|i| i.as_str() == Some(key)),
        Some(Value::String(text)) => text.contains(key),
        _ => false,
    }
}

/// One monitor shape of `MonitorLayout.monitor_data`.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub name: String,
    pub desc: String,
    pub width: i64,
    pub height: i64,
    pub x: i64,
    pub y: i64,
    pub orig_width: i64,
    pub orig_height: i64,
    pub transform: String,
    /// Put into the Gtk.Fixed (monitors in `CFG.DISABLED` are only tracked).
    pub placed: bool,
}

/// The arrangement data of `MonitorLayout` (widgets live in `layout.rs`).
#[derive(Clone, Debug, Default)]
pub struct LayoutModel {
    pub entries: Vec<Meta>,
    pub align: String,
    pub arrange: String,
    pub size_request: (i64, i64),
}

fn upper_text(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(Value::String(s)) => s.to_uppercase(),
        Some(other) => py_str(other).to_uppercase(),
        None => default.to_uppercase(),
    }
}

impl LayoutModel {
    /// `MonitorLayout.__init__` minus widgets.
    pub fn new(monitors: &[Value], cfg: &Value, align: Option<&str>, arrange: Option<&str>, order: Option<&[String]>,
               transform_overrides: &Map<String, Value>) -> Self {
        let empty = json!({});
        let align = upper_text(cfg.get("ALIGN"), &align.unwrap_or("TOP").to_uppercase());
        let arrange = upper_text(cfg.get("ARRANGE"), &arrange.unwrap_or("ROW").to_uppercase());
        let config_order: Vec<String> = cfg.get("ORDER").filter(|v| truthy(v)).and_then(Value::as_array)
            .map(|items| items.iter().map(py_str).collect()).unwrap_or_default();
        let disabled = cfg.get("DISABLED").filter(|v| truthy(v));
        let order: Vec<String> = match order {
            Some(order) if !order.is_empty() => order.to_vec(),
            _ => config_order,
        };
        // monitor_map = {m["DESCRIPTION"]: m}: first position, last value.
        let mut map: Vec<(String, Value)> = Vec::new();
        for monitor in monitors {
            let desc = py_str(&monitor["DESCRIPTION"]);
            match map.iter_mut().find(|(d, _)| *d == desc) {
                Some(entry) => entry.1 = monitor.clone(),
                None => map.push((desc, monitor.clone())),
            }
        }
        let prepare = |desc: &str, monitor: &Value| {
            let mut monitor = monitor.clone();
            let mut current = Map::new();
            if let Some(transform) = transform_overrides.get(desc) {
                current.insert("TRANSFORM".into(), transform.clone());
            }
            monitor["CURRENT"] = Value::Object(current);
            monitor
        };
        let mut ordered = Vec::new();
        let mut seen = Vec::new();
        for desc in &order {
            if let Some((_, monitor)) = map.iter().find(|(d, _)| d == desc) {
                ordered.push(prepare(desc, monitor));
                seen.push(desc.clone());
            }
        }
        for (desc, monitor) in &map {
            if !seen.contains(desc) {
                ordered.push(prepare(desc, monitor));
            }
        }
        let mut entries = Vec::new();
        for monitor in &ordered {
            let name = py_str(&monitor["NAME"]);
            let desc = py_str(&monitor["DESCRIPTION"]);
            let desired = object(monitor, "DESIRED", &empty);
            let current = object(monitor, "CURRENT", &empty);
            let mode = desired.get("MODE").unwrap_or_else(|| object(current, "MODE", &empty));
            let width = int_of(get(mode, "WIDTH"), 1920);
            let height = int_of(get(mode, "HEIGHT"), 1080);
            let fallback = current.get("TRANSFORM").cloned().unwrap_or(json!(0));
            let transform = desired.get("TRANSFORM").cloned().unwrap_or(fallback);
            let (angle, _) = parse_transform(&transform_text(Some(&transform)).to_uppercase());
            let (rw, rh) = rotated_dimensions(width, height, &angle.to_string());
            let (w, h) = ((rw as f64 * SCALE) as i64, (rh as f64 * SCALE) as i64);
            let placed = disabled.is_none() || !contains(disabled, &desc);
            entries.push(Meta { name, desc, width: w, height: h, x: 0, y: 0, orig_width: width, orig_height: height,
                                transform: transform_text(Some(&transform)), placed });
        }
        let mut model = LayoutModel { entries, align, arrange, size_request: (640, 280) };
        model.position();
        model.snap_monitors_to_edges();
        model
    }

    fn position(&mut self) {
        let placed: Vec<usize> = (0..self.entries.len()).filter(|i| self.entries[*i].placed).collect();
        if placed.is_empty() {
            // Python raises ValueError (max() of an empty sequence) here.
            return;
        }
        let max_width = placed.iter().map(|i| self.entries[*i].width).max().unwrap();
        let max_height = placed.iter().map(|i| self.entries[*i].height).max().unwrap();
        let (group_width, group_height) = if self.arrange == "ROW" {
            (placed.iter().map(|i| self.entries[*i].width).sum::<i64>(), max_height)
        } else {
            (max_width, placed.iter().map(|i| self.entries[*i].height).sum::<i64>())
        };
        let (layout_w, layout_h) = (640.max(group_width + 100), 280.max(group_height + 100));
        self.size_request = (layout_w, layout_h);
        let mut x_cursor = (layout_w - group_width).div_euclid(2);
        let mut y_cursor = (layout_h - group_height).div_euclid(2);
        for i in placed {
            let (w, h) = (self.entries[i].width, self.entries[i].height);
            let (x, y);
            if self.arrange == "ROW" {
                let mut yy = y_cursor;
                if self.align == "MIDDLE" {
                    yy += (max_height - h).div_euclid(2);
                } else if self.align == "BOTTOM" {
                    yy += max_height - h;
                }
                x = x_cursor;
                y = yy;
                x_cursor += w;
            } else {
                let mut xx = x_cursor;
                if self.align == "MIDDLE" {
                    xx += (max_width - w).div_euclid(2);
                } else if self.align == "RIGHT" {
                    xx += max_width - w;
                }
                x = xx;
                y = y_cursor;
                y_cursor += h;
            }
            self.entries[i].x = x;
            self.entries[i].y = y;
        }
    }

    /// `_snap_monitors_to_edges(tolerance=20)`.
    pub fn snap_monitors_to_edges(&mut self) {
        let tolerance = 20;
        for i in 0..self.entries.len() {
            let (mut ax, mut ay) = (self.entries[i].x, self.entries[i].y);
            let (aw, ah) = (self.entries[i].width, self.entries[i].height);
            for j in 0..self.entries.len() {
                if i == j {
                    continue;
                }
                let b = &self.entries[j];
                let (bx, by, bw, bh) = (b.x, b.y, b.width, b.height);
                if ((ax + aw) - bx).abs() <= tolerance {
                    ax = bx - aw;
                } else if (ax - (bx + bw)).abs() <= tolerance {
                    ax = bx + bw;
                }
                if ((ay + ah) - by).abs() <= tolerance {
                    ay = by - ah;
                } else if (ay - (by + bh)).abs() <= tolerance {
                    ay = by + bh;
                }
            }
            self.entries[i].x = ax;
            self.entries[i].y = ay;
        }
    }

    /// `recenter_all_monitors()` with the allocated layout size and `CFG`.
    pub fn recenter(&mut self, layout_width: i64, layout_height: i64, cfg: &Value) {
        if self.entries.is_empty() {
            return;
        }
        let arrange = cfg.get("ARRANGE").map(|v| py_str(v)).unwrap_or_else(|| "ROW".into());
        let align = cfg.get("ALIGN").map(|v| py_str(v)).unwrap_or_else(|| "TOP".into());
        // The Python code reads data["CURRENT"]["TRANSFORM"], which shape
        // metadata never has: sizes are used unrotated.
        let sizes: Vec<(i64, i64)> = self.entries.iter().map(|e| (e.width, e.height)).collect();
        if arrange == "ROW" {
            let total: i64 = sizes.iter().map(|s| s.0).sum();
            let start_x = (layout_width - total).div_euclid(2);
            let first_h = sizes[0].1;
            let base = match align.as_str() {
                "MIDDLE" => Some(None),
                "TOP" => Some(Some(layout_height.div_euclid(2) - first_h.div_euclid(2))),
                "BOTTOM" => Some(Some(layout_height.div_euclid(2) + first_h.div_euclid(2))),
                _ => None,
            };
            if let Some(base) = base {
                let mut x = start_x;
                for (i, (w, h)) in sizes.iter().enumerate() {
                    let y = match (align.as_str(), base) {
                        ("MIDDLE", _) => layout_height.div_euclid(2) - h.div_euclid(2),
                        ("TOP", Some(top)) => top,
                        (_, Some(bottom)) => bottom - h,
                        _ => unreachable!(),
                    };
                    self.entries[i].x = x;
                    self.entries[i].y = y;
                    x += w;
                }
            }
        } else if arrange == "COLUMN" {
            let total: i64 = sizes.iter().map(|s| s.1).sum();
            let start_y = (layout_height - total).div_euclid(2);
            let first_w = sizes[0].0;
            if matches!(align.as_str(), "MIDDLE" | "LEFT" | "RIGHT") {
                let mut y = start_y;
                for (i, (w, h)) in sizes.iter().enumerate() {
                    let x = match align.as_str() {
                        "MIDDLE" => layout_width.div_euclid(2) - w.div_euclid(2),
                        "LEFT" => layout_width.div_euclid(2) - first_w.div_euclid(2),
                        _ => layout_width.div_euclid(2) + first_w.div_euclid(2) - w,
                    };
                    self.entries[i].x = x;
                    self.entries[i].y = y;
                    y += h;
                }
            }
        }
    }

    pub fn index_of(&self, desc: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.desc == desc)
    }

    /// Initial position of `add_monitor` (`_current_arrange` is never set: ROW).
    pub fn add_position(&self) -> (i64, i64) {
        if self.entries.is_empty() {
            return (100, 100);
        }
        let max_x = self.entries.iter().map(|d| d.x + d.width).max().unwrap();
        let min_y = self.entries.iter().map(|d| d.y).min().unwrap();
        (max_x + 20, min_y)
    }

    /// `add_monitor(monitor)` data (the caller checks placed duplicates).
    pub fn add_meta(&self, monitor: &Value, transform_overrides: &Map<String, Value>) -> Meta {
        let empty = json!({});
        let desc = py_str(&monitor["DESCRIPTION"]);
        let current = object(monitor, "CURRENT", &empty);
        let mode = object(current, "MODE", &empty);
        let width = int_of(get(mode, "WIDTH"), 1920);
        let height = int_of(get(mode, "HEIGHT"), 1080);
        let transform = transform_overrides.get(&desc).filter(|v| truthy(v)).cloned()
            .unwrap_or_else(|| current.get("TRANSFORM").cloned().unwrap_or(json!(0)));
        let (angle, _) = parse_transform(&transform_text(Some(&transform)).to_uppercase());
        let (rw, rh) = rotated_dimensions(width, height, &angle.to_string());
        let (x, y) = self.add_position();
        Meta { name: py_str(&monitor["NAME"]), desc, width: (rw as f64 * SCALE) as i64, height: (rh as f64 * SCALE) as i64,
               x, y, orig_width: width, orig_height: height, transform: transform_text(Some(&transform)), placed: true }
    }

    /// `ARRANGE`, `ALIGN` (if decided) and `ORDER` from the shapes (save_config).
    pub fn arrangement(&self) -> Option<(String, Option<String>, Vec<String>)> {
        if self.entries.is_empty() {
            return None;
        }
        // positions = {desc: …}: a repeated description keeps its first slot
        // with the last shape's values.
        let mut positions: Vec<(String, i64, i64, i64, i64)> = Vec::new();
        let mut transforms: BTreeMap<String, String> = BTreeMap::new();
        for e in &self.entries {
            transforms.insert(e.desc.clone(), e.transform.clone());
            match positions.iter_mut().find(|p| p.0 == e.desc) {
                Some(p) => *p = (e.desc.clone(), e.x, e.y, e.width, e.height),
                None => positions.push((e.desc.clone(), e.x, e.y, e.width, e.height)),
            }
        }
        let xs: Vec<i64> = positions.iter().map(|p| p.1).collect();
        let ys: Vec<i64> = positions.iter().map(|p| p.2).collect();
        let spread = |v: &[i64]| v.iter().max().unwrap() - v.iter().min().unwrap();
        let arrange = if spread(&xs) >= spread(&ys) { "ROW" } else { "COLUMN" };
        let mut align = None;
        let mut sorted = positions.clone();
        if arrange == "ROW" {
            sorted.sort_by_key(|p| p.1);
        } else {
            sorted.sort_by_key(|p| p.2);
        }
        if positions.len() >= 2 {
            let (main, neighbor) = (&sorted[0], &sorted[1]);
            let t = |d: &str| transforms.get(d).cloned().unwrap_or_else(|| "0".into());
            let (mw, mh) = rotated_dimensions(main.3, main.4, &t(&main.0));
            let (nw, nh) = rotated_dimensions(neighbor.3, neighbor.4, &t(&neighbor.0));
            if arrange == "ROW" {
                let (mt, nt) = (main.2 as f64, neighbor.2 as f64);
                let top = (nt - mt).abs();
                let mid = ((nt + nh as f64 / 2.0) - (mt + mh as f64 / 2.0)).abs();
                let bottom = ((nt + nh as f64) - (mt + mh as f64)).abs();
                let min = top.min(mid).min(bottom);
                align = Some(if min == mid && mid < 100.0 { "MIDDLE" } else if min == bottom && bottom < 100.0 { "BOTTOM" } else { "TOP" });
            } else {
                let (ml, nl) = (main.1 as f64, neighbor.1 as f64);
                let left = (nl - ml).abs();
                let mid = ((nl + nw as f64 / 2.0) - (ml + mw as f64 / 2.0)).abs();
                let right = ((nl + nw as f64) - (ml + mw as f64)).abs();
                let min = left.min(mid).min(right);
                align = Some(if min == mid && mid < 25.0 { "MIDDLE" } else if min == right && right < 25.0 { "RIGHT" } else { "LEFT" });
            }
        }
        let order = sorted.into_iter().map(|p| p.0).collect();
        Some((arrange.to_owned(), align.map(String::from), order))
    }
}

/// `MonitorWidget.get_desired_state()`.
#[derive(Clone, Debug, PartialEq)]
pub struct DesiredState {
    pub desc: String,
    pub name: Option<String>,
    pub width: Value,
    pub height: Value,
    pub refresh: Value,
    pub enabled: bool,
    pub enabled_changed: bool,
    pub primary: bool,
    pub vrr: bool,
    pub vrr_changed: bool,
    pub hdr: bool,
    pub hdr_changed: bool,
    pub scale: f64,
    pub scale_auto: bool,
    pub transform: Option<String>,
}

/// The daemon and labwc changes of one Apply.
#[derive(Clone, Debug, PartialEq)]
pub struct SavePlan {
    pub cfg: Value,
    pub del_cfg: Value,
    pub hdr_overrides: BTreeMap<String, bool>,
    /// Indices (into the states) whose HDR switch changed.
    pub hdr_changed: Vec<usize>,
    /// Indices whose enable switch changed (their `initial_enabled` is reset).
    pub enabled_changed: Vec<usize>,
}

fn take_object(value: Option<&Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

/// `save_config` from the CFG of `CONFIG_DATA`, each tab's desired state
/// (`None` = invalid mode selection), the callback command and the layout.
pub fn save_plan(data_cfg: Option<&Value>, states: &[Option<DesiredState>], callback: &str,
                 layout: Option<&LayoutModel>, managed_hdr: &BTreeMap<String, bool>) -> SavePlan {
    let original_cfg = data_cfg.filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
    let mut cfg = match &original_cfg { Value::Object(map) => map.clone(), _ => Map::new() };
    let original_transforms = take_object(cfg.get("TRANSFORM").filter(|v| truthy(v)));
    let original_disabled = take_object(cfg.get("DISABLED").filter(|v| truthy(v)));
    let mut hdr_overrides = managed_hdr.clone();
    let mut hdr_changed = Vec::new();
    let mut enabled_changed_list = Vec::new();
    let mut mode_overrides = take_object(cfg.get("MODE").filter(|v| truthy(v)));
    let mut scale_overrides = take_object(cfg.get("SCALE").filter(|v| truthy(v)));
    let mut transform_overrides = original_transforms.clone();
    let mut disabled_overrides = original_disabled.clone();
    // CFG_SET adds members to VRR_OFF; sending an old full list would disable
    // other monitors again after a change in another editor.
    let mut vrr_off: Vec<Value> = Vec::new();
    let mut transforms_to_delete = Map::new();
    let mut disabled_to_delete = Map::new();
    let mut scales_to_delete = Map::new();
    let mut any_auto_scale = false;
    let mut vrr_to_delete: Vec<String> = Vec::new();

    for (index, state) in states.iter().enumerate() {
        let Some(state) = state else { continue };
        let desc = state.desc.clone();
        let hz = float_of(Some(&state.refresh), 0.0) / 1000.0;
        if state.hdr_changed {
            if let Some(name) = state.name.as_ref().filter(|n| !n.is_empty()) {
                hdr_overrides.insert(name.clone(), state.hdr);
                hdr_changed.push(index);
            }
        }
        mode_overrides.insert(desc.clone(), json!({
            "WIDTH": int_of(Some(&state.width), 0),
            "HEIGHT": int_of(Some(&state.height), 0),
            "HZ": hz,
        }));
        if state.vrr_changed {
            if state.vrr {
                vrr_to_delete.push(desc.clone());
                if let Some(name) = state.name.as_ref().filter(|n| !n.is_empty()) {
                    vrr_to_delete.push(name.clone());
                }
            } else {
                vrr_off.push(Value::String(desc.clone()));
            }
        }
        if state.scale_auto {
            any_auto_scale = true;
            if let Some(value) = scale_overrides.shift_remove(&desc) {
                scales_to_delete.insert(desc.clone(), value);
            }
        } else if state.scale > 0.0 {
            scale_overrides.insert(desc.clone(), json!(state.scale));
        }
        if let Some(transform) = state.transform.as_ref().filter(|t| !t.is_empty()) {
            if transform.contains("STANDARD") && !transform.contains("FLIPPED") {
                transform_overrides.shift_remove(&desc);
                if let Some(original) = original_transforms.get(&desc) {
                    transforms_to_delete.insert(desc.clone(), original.clone());
                }
            } else {
                let transform = if transform.contains("FLIPPED-STANDARD") { "FLIPPED".to_owned() } else { transform.clone() };
                transform_overrides.insert(desc.clone(), Value::String(transform));
            }
        }
        if state.enabled_changed {
            if state.enabled {
                disabled_overrides.shift_remove(&desc);
                disabled_to_delete.insert(desc.clone(), original_disabled.get(&desc).cloned().unwrap_or(Value::Null));
            } else {
                disabled_overrides.insert(desc.clone(), Value::Null);
            }
            enabled_changed_list.push(index);
        }
    }

    cfg.insert("MODE".into(), Value::Object(mode_overrides));
    let set_or_remove = |cfg: &mut Map<String, Value>, key: &str, value: Value, keep: bool| {
        if keep {
            cfg.insert(key.into(), value);
        } else {
            cfg.shift_remove(key);
        }
    };
    let keep = !scale_overrides.is_empty();
    set_or_remove(&mut cfg, "SCALE", Value::Object(scale_overrides), keep);
    if any_auto_scale {
        cfg.insert("AUTO_SCALE".into(), Value::Bool(true));
    }
    let keep = !transform_overrides.is_empty();
    set_or_remove(&mut cfg, "TRANSFORM", Value::Object(transform_overrides), keep);
    let keep = !disabled_overrides.is_empty();
    set_or_remove(&mut cfg, "DISABLED", Value::Object(disabled_overrides), keep);
    let keep = !vrr_off.is_empty();
    set_or_remove(&mut cfg, "VRR_OFF", Value::Array(vrr_off), keep);
    set_or_remove(&mut cfg, "CALLBACK_CMD", Value::String(callback.to_owned()), !callback.is_empty());

    let mut del_cfg = Map::new();
    if !transforms_to_delete.is_empty() {
        del_cfg.insert("TRANSFORM".into(), Value::Object(transforms_to_delete));
    }
    if !disabled_to_delete.is_empty() {
        del_cfg.insert("DISABLED".into(), Value::Object(disabled_to_delete));
    }
    if !scales_to_delete.is_empty() {
        del_cfg.insert("SCALE".into(), Value::Object(scales_to_delete));
    }
    if !vrr_to_delete.is_empty() {
        let mut unique: Vec<String> = Vec::new();
        for item in vrr_to_delete {
            if !unique.contains(&item) {
                unique.push(item);
            }
        }
        del_cfg.insert("VRR_OFF".into(), json!(unique));
    }
    if callback.is_empty() && original_cfg.get("CALLBACK_CMD").is_some() {
        del_cfg.insert("CALLBACK_CMD".into(), json!(""));
    }

    if let Some((arrange, align, order)) = layout.and_then(LayoutModel::arrangement) {
        if let Some(align) = align {
            cfg.insert("ALIGN".into(), Value::String(align));
        }
        cfg.insert("ORDER".into(), json!(order));
        cfg.insert("ARRANGE".into(), Value::String(arrange));
    }
    SavePlan { cfg: Value::Object(cfg), del_cfg: Value::Object(del_cfg), hdr_overrides, hdr_changed,
               enabled_changed: enabled_changed_list }
}
