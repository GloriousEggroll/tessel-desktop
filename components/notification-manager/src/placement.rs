//! Stable per-output notification stacks; focus changes cannot move captures
//! (port of notification_placement.py).

use serde_json::Value;

/// What the grouping needs from a monitor.
pub trait MonitorLike: Clone + PartialEq {
    fn geometry(&self) -> (i32, i32, i32, i32);
    fn connector(&self) -> Option<String>;
}

impl MonitorLike for gtk::gdk::Monitor {
    fn geometry(&self) -> (i32, i32, i32, i32) {
        let g = gtk::prelude::MonitorExt::geometry(self);
        (g.x(), g.y(), g.width(), g.height())
    }
    fn connector(&self) -> Option<String> {
        gtk::prelude::MonitorExt::connector(self).map(String::from)
    }
}

/// The "workspace" monitor: bottom/right dock edge → leftmost (then lowest);
/// otherwise rightmost (then top-most).
pub fn workspace_monitor<M: MonitorLike>(monitors: &[M], edge: &str) -> Option<M> {
    if monitors.is_empty() {
        return None;
    }
    if matches!(edge, "bottom" | "right") {
        // min by (x, -y - height); Python's min keeps the first of equals.
        let mut best = &monitors[0];
        for m in &monitors[1..] {
            let (a, b) = (m.geometry(), best.geometry());
            if (a.0, -a.1 - a.3) < (b.0, -b.1 - b.3) {
                best = m;
            }
        }
        return Some(best.clone());
    }
    // max by (x + width, -y); Python's max keeps the first of equals.
    let mut best = &monitors[0];
    for m in &monitors[1..] {
        let (a, b) = (m.geometry(), best.geometry());
        if (a.0 + a.2, -a.1) > (b.0 + b.2, -b.1) {
            best = m;
        }
    }
    Some(best.clone())
}

/// Waydock's dock edge from `~/.config/waydock/config.json` (default bottom).
pub fn dock_edge() -> String {
    let path = suite_core::paths::home().join(".config/waydock/config.json");
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().map(|map| map.get("edge").cloned()))
        .map(|edge| match edge {
            None => "bottom".to_owned(),
            Some(Value::String(edge)) => edge,
            Some(other) => crate::compat::py_str(&other),
        }).unwrap_or_else(|| "bottom".into())
}

/// Group shown items into at most three per `(monitor, workspace)` stack.
pub fn group_notifications<M: MonitorLike>(items: &[Value], monitors: &[M], fallback: Option<&M>, edge: Option<&str>)
                                          -> Vec<((M, bool), Vec<Value>)> {
    let edge = edge.map(String::from).unwrap_or_else(dock_edge);
    let mut groups: Vec<((M, bool), Vec<Value>)> = Vec::new();
    for item in items {
        let workspace = item.get("workspace").is_some_and(crate::compat::truthy);
        let monitor = if workspace {
            workspace_monitor(monitors, &edge)
        } else {
            let output = item.get("output").and_then(Value::as_str).map(String::from);
            monitors.iter().find(|m| m.connector() == output && output.is_some()).cloned()
                .or_else(|| {
                    // Python compares get_connector() == item.get('output'): a
                    // monitor without a connector matches a missing output.
                    if output.is_none() { monitors.iter().find(|m| m.connector().is_none()).cloned() } else { None }
                })
                .or_else(|| fallback.cloned())
        };
        if let Some(monitor) = monitor {
            let key = (monitor, workspace);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, entries)) => entries.push(item.clone()),
                None => groups.push((key, vec![item.clone()])),
            }
        }
    }
    for (_, entries) in &mut groups {
        entries.truncate(3);
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Clone, Debug, PartialEq)]
    struct Fake(&'static str, (i32, i32, i32, i32));
    impl MonitorLike for Fake {
        fn geometry(&self) -> (i32, i32, i32, i32) { self.1 }
        fn connector(&self) -> Option<String> { Some(self.0.into()) }
    }

    #[test]
    fn workspace_and_output_routing() {
        let left = Fake("DP-1", (0, 0, 1920, 1080));
        let right = Fake("DP-2", (1920, 0, 1920, 1080));
        let monitors = [left.clone(), right.clone()];
        assert_eq!(workspace_monitor(&monitors, "bottom"), Some(left.clone()));
        assert_eq!(workspace_monitor(&monitors, "top"), Some(right.clone()));
        let items: Vec<Value> = (1..=5).map(|i| json!({"id": i, "output": "DP-2", "workspace": false})).collect();
        let mut all = items.clone();
        all.push(json!({"id": 9, "output": "", "workspace": true}));
        all.push(json!({"id": 10, "output": "missing", "workspace": false}));
        let groups = group_notifications(&all, &monitors, Some(&left), Some("bottom"));
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].0, (right.clone(), false));
        assert_eq!(groups[0].1.len(), 3, "at most three per stack");
        assert_eq!(groups[1].0, (left.clone(), true));
        assert_eq!(groups[2].0, (left.clone(), false));
        assert!(group_notifications::<Fake>(&all, &[], None, Some("bottom")).is_empty());
    }
}
