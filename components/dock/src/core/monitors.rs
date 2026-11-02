//! Monitor naming and selection, the pure part of tessel-dock.py
//! (`monitor_label`, `monitor_entries`, the `config.monitor` choice;
//! contract §5 "Monitor naming/selection").
//!
//! API: [`monitor_label`], [`MonitorInfo`], [`OutputInfo`],
//! [`monitor_entries`] (GDK monitors matched to helper outputs by exact
//! geometry; remembers the previous names per monitor key), [`MonitorEntry`],
//! [`select_monitors`].

use std::collections::HashMap;
use std::hash::Hash;

use tessel_core::pyjson::{casefold, is_py_space};

/// `monitor_label(name, description, model)`: compositor metadata rather
/// than GTK's placeholder model names.
pub fn monitor_label(name: &str, description: &str, model: &str) -> String {
    fn known(value: &str) -> String {
        let value = value.split(is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
        if ["unknown", "(null)", "none", "n/a"].contains(&casefold(&value).as_str()) { String::new() } else { value }
    }
    let mut detail = known(description);
    let suffix = format!(" ({name})");
    if detail.ends_with(&suffix) {
        detail = known(&detail[..detail.len() - suffix.len()]);
    }
    if detail.is_empty() {
        detail = known(model);
    }
    if !detail.is_empty() && detail != name { format!("{name} · {detail}") } else { name.to_owned() }
}

/// A GDK monitor: `key` identifies it across rebuilds (the GdkMonitor).
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo<K> {
    pub key: K,
    /// (x, y, width, height)
    pub geometry: (i32, i32, i32, i32),
    pub connector: Option<String>,
    pub model: Option<String>,
}

/// A helper (wl_output) output.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub geometry: (i32, i32, i32, i32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorEntry<K> {
    pub name: String,
    pub label: String,
    pub key: K,
}

/// `Tessel Dock.monitor_entries()`. `display_label(n)` is the translated
/// `tr('Display {value1}', value1=n)`. `previous` is the last call's
/// `key → (name, label)` and is replaced with this call's.
pub fn monitor_entries<K: Clone + Eq + Hash>(
    monitors: &[MonitorInfo<K>],
    outputs: &[OutputInfo],
    previous: &mut HashMap<K, (String, String)>,
    display_label: impl Fn(usize) -> String,
) -> Vec<MonitorEntry<K>> {
    let mut outputs: Vec<OutputInfo> = outputs.to_vec();
    let mut current = HashMap::new();
    let mut result: Vec<MonitorEntry<K>> = Vec::new();
    for (index, monitor) in monitors.iter().enumerate() {
        let matches: Vec<usize> = (0..outputs.len()).filter(|&i| outputs[i].geometry == monitor.geometry).collect();
        let saved = previous.get(&monitor.key);
        let chosen = matches
            .iter()
            .copied()
            .find(|&i| saved.is_some_and(|s| outputs[i].name == s.0))
            .or_else(|| matches.first().copied());
        let model = monitor.model.as_deref().unwrap_or("");
        let (mut name, label) = if let Some(i) = chosen {
            let output = outputs.remove(i);
            let label = monitor_label(&output.name, &output.description, model);
            (output.name, label)
        } else if let Some(saved) = saved {
            saved.clone()
        } else {
            let name = monitor.connector.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| format!("monitor-{}", index + 1));
            (name, monitor_label(&display_label(index + 1), "", model))
        };
        if result.iter().any(|entry| entry.name == name) {
            name.push_str(&format!("-{}", index + 1));
        }
        current.insert(monitor.key.clone(), (name.clone(), label.clone()));
        result.push(MonitorEntry { name, label, key: monitor.key.clone() });
    }
    *previous = current;
    result
}

/// `config.monitor`: `all`, `primary` (the first GDK monitor) or a name
/// (missing → the first entry, so controls stay reachable).
pub fn select_monitors<K: Clone + PartialEq>(entries: &[MonitorEntry<K>], monitor: &str, primary: Option<&K>) -> Vec<MonitorEntry<K>> {
    match monitor {
        "all" => entries.to_vec(),
        "primary" => {
            let primary = primary.or_else(|| entries.first().map(|e| &e.key));
            entries.iter().filter(|e| Some(&e.key) == primary).cloned().collect()
        }
        name => {
            let selected: Vec<_> = entries.iter().filter(|e| e.name == name).cloned().collect();
            if selected.is_empty() { entries.iter().take(1).cloned().collect() } else { selected }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_use_compositor_metadata() {
        assert_eq!(monitor_label("DP-1", "Dell Inc. U2720Q (DP-1)", "U2720Q"), "DP-1 · Dell Inc. U2720Q");
        assert_eq!(monitor_label("DP-1", "  Unknown  ", "Model  X"), "DP-1 · Model X");
        assert_eq!(monitor_label("DP-1", "(null) (DP-1)", "n/a"), "DP-1");
        assert_eq!(monitor_label("DP-1", "DP-1", ""), "DP-1");
        assert_eq!(monitor_label("HDMI-A-1", "", "NONE"), "HDMI-A-1");
        assert_eq!(monitor_label("Display 2", "", "Generic"), "Display 2 · Generic");
    }

    fn monitor(key: u32, x: i32, connector: Option<&str>) -> MonitorInfo<u32> {
        MonitorInfo { key, geometry: (x, 0, 1920, 1080), connector: connector.map(Into::into), model: Some("Model".into()) }
    }

    fn output(name: &str, x: i32) -> OutputInfo {
        OutputInfo { name: name.into(), description: format!("Vendor {name} ({name})"), geometry: (x, 0, 1920, 1080) }
    }

    #[test]
    fn entries_match_geometry_keep_names_and_suffix_duplicates() {
        let mut previous = HashMap::new();
        let label = |n: usize| format!("Display {n}");
        let monitors = [monitor(1, 0, Some("DP-1")), monitor(2, 1920, None), monitor(3, 3840, Some("DP-1"))];
        let entries = monitor_entries(&monitors, &[output("HDMI-A-1", 0)], &mut previous, label);
        assert_eq!(
            entries.iter().map(|e| (e.name.as_str(), e.label.as_str())).collect::<Vec<_>>(),
            vec![("HDMI-A-1", "HDMI-A-1 · Vendor HDMI-A-1"), ("monitor-2", "Display 2 · Model"), ("DP-1", "Display 3 · Model")]
        );
        // Helper output temporarily gone: the saved name/label is reused.
        let entries = monitor_entries(&monitors[..1], &[], &mut previous, label);
        assert_eq!(entries[0].name, "HDMI-A-1");
        // Duplicate names are suffixed by position.
        let twins = [monitor(1, 0, Some("X")), monitor(2, 1920, Some("X"))];
        let entries = monitor_entries(&twins, &[], &mut HashMap::new(), label);
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["X", "X-2"]);
        // Same geometry outputs: the previously chosen name wins.
        let mut previous = HashMap::from([(1u32, ("B".to_string(), "B".to_string()))]);
        let entries = monitor_entries(&[monitor(1, 0, None)], &[output("A", 0), output("B", 0)], &mut previous, label);
        assert_eq!(entries[0].name, "B");
    }

    #[test]
    fn selection_all_primary_named_and_missing() {
        let entries: Vec<MonitorEntry<u32>> = ["DP-1", "DP-2"]
            .iter()
            .enumerate()
            .map(|(i, n)| MonitorEntry { name: n.to_string(), label: n.to_string(), key: i as u32 })
            .collect();
        assert_eq!(select_monitors(&entries, "all", None).len(), 2);
        assert_eq!(select_monitors(&entries, "primary", Some(&1))[0].name, "DP-2");
        assert_eq!(select_monitors(&entries, "primary", None)[0].name, "DP-1");
        assert_eq!(select_monitors(&entries, "DP-2", None)[0].name, "DP-2");
        assert_eq!(select_monitors(&entries, "gone", None).iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["DP-1"]);
    }
}
