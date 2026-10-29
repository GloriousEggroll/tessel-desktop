//! Stable tray application choices, independent of transient D-Bus owners
//! (port of tray_preferences.py; `always_show_tray` identities).
//!
//! API: [`TrayItemInfo`] (what is read from a StatusNotifier item),
//! [`tray_identity`] (`id:` / `desktop:` / `name:` / `title:` prefix, value
//! stripped and cut to 500 characters; `""` if none), [`tray_entries`]
//! (ready items, deduplicated, sorted by casefolded label then id).

use suite_core::pyjson::{casefold, py_strip};

/// The StatusNotifier item fields used for identities. String properties
/// only (Python ignores non-string values); `name` is the bus name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrayItemInfo {
    pub name: String,
    pub ready: bool,
    pub id: Option<String>,
    pub desktop_entry: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrayEntry {
    pub id: String,
    pub label: String,
    pub running: bool,
}

pub fn tray_identity(item: &TrayItemInfo) -> String {
    let name = (!item.name.starts_with(':')).then_some(item.name.as_str());
    for (prefix, value) in [("id", item.id.as_deref()), ("desktop", item.desktop_entry.as_deref()), ("name", name), ("title", item.title.as_deref())] {
        if let Some(value) = value {
            let stripped = py_strip(value);
            if !stripped.is_empty() {
                let cut: String = stripped.chars().take(500).collect();
                return format!("{prefix}:{cut}");
            }
        }
    }
    String::new()
}

pub fn tray_entries<'a>(items: impl IntoIterator<Item = &'a TrayItemInfo>) -> Vec<TrayEntry> {
    let mut entries: Vec<TrayEntry> = Vec::new();
    for item in items {
        if !item.ready {
            continue;
        }
        let ident = tray_identity(item);
        if ident.is_empty() {
            continue;
        }
        let nonempty = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
        let label = nonempty(&item.title)
            .or_else(|| nonempty(&item.id))
            .unwrap_or_else(|| ident.split_once(':').map(|(_, rest)| rest.to_owned()).unwrap_or_else(|| ident.clone()));
        let entry = TrayEntry { id: ident.clone(), label, running: true };
        match entries.iter_mut().find(|e| e.id == ident) {
            Some(slot) => *slot = entry,
            None => entries.push(entry),
        }
    }
    entries.sort_by(|a, b| casefold(&a.label).cmp(&casefold(&b.label)).then_with(|| a.id.cmp(&b.id)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, id: Option<&str>, desktop: Option<&str>, title: Option<&str>) -> TrayItemInfo {
        TrayItemInfo { name: name.into(), ready: true, id: id.map(Into::into), desktop_entry: desktop.map(Into::into), title: title.map(Into::into) }
    }

    #[test]
    fn identities_prefer_stable_metadata_and_skip_unique_bus_names() {
        assert_eq!(tray_identity(&item(":1.42", Some(" Nextcloud "), Some("x"), None)), "id:Nextcloud");
        assert_eq!(tray_identity(&item(":1.42", Some("  "), Some("steam"), None)), "desktop:steam");
        assert_eq!(tray_identity(&item("org.kde.StatusNotifierItem-5-1", None, None, Some("T"))), "name:org.kde.StatusNotifierItem-5-1");
        assert_eq!(tray_identity(&item(":1.9", None, None, Some("Title"))), "title:Title");
        assert_eq!(tray_identity(&item(":1.9", None, None, None)), "");
        let long = "x".repeat(600);
        assert_eq!(tray_identity(&item(":1", Some(&long), None, None)).len(), 503);
    }

    #[test]
    fn entries_are_ready_unique_and_sorted() {
        let mut hidden = item(":1.1", Some("Hidden"), None, None);
        hidden.ready = false;
        let items = vec![
            item(":1.2", Some("zeta"), None, Some("Zeta App")),
            item(":1.3", Some("alpha"), None, None),
            hidden,
            item(":1.4", Some("alpha"), None, Some("Alpha again")),
            item(":1.5", None, Some("beta"), None),
        ];
        let entries = tray_entries(&items);
        assert_eq!(
            entries.iter().map(|e| (e.id.as_str(), e.label.as_str())).collect::<Vec<_>>(),
            vec![("id:alpha", "Alpha again"), ("desktop:beta", "beta"), ("id:zeta", "Zeta App")]
        );
        assert!(entries.iter().all(|e| e.running));
    }
}
