//! lock_media_icons.py: resolve MPRIS players to their standard installed
//! application icons. Cached by (bus name, DesktopEntry, Identity): title,
//! URL and artwork changes never re-scan the application registry.

use crate::mpris::{Media, PREFIX};
use gio::prelude::*;
use gio_unix::DesktopAppInfo;
use regex::Regex;
use std::cell::RefCell;
use std::sync::LazyLock;

pub fn fallback() -> gio::Icon { gio::ThemedIcon::new("multimedia-player").upcast() }

pub fn desktop_icon(name: &str) -> Option<gio::Icon> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let id = if name.ends_with(".desktop") { name.to_owned() } else { format!("{name}.desktop") };
    DesktopAppInfo::new(&id).and_then(|info| info.icon())
}

/// Use an exact, unambiguous launcher identity, never the media title.
pub fn matching_icon(apps: &[gio::AppInfo], identity: &str) -> Option<gio::Icon> {
    if identity.is_empty() {
        return None;
    }
    let identity = identity.strip_suffix(".desktop").unwrap_or(identity).to_lowercase();
    let mut matches: Vec<(String, gio::Icon)> = Vec::new();
    for info in apps {
        let (Some(ident), Some(icon)) = (info.id(), info.icon()) else { continue };
        let desktop = info.downcast_ref::<DesktopAppInfo>();
        if desktop.is_some_and(|d| d.is_hidden()) {
            continue;
        }
        let ident = ident.to_string();
        let wm_class = desktop.and_then(|d| d.startup_wm_class()).map(|s| s.to_string());
        let names = [Some(ident.strip_suffix(".desktop").unwrap_or(&ident).to_owned()), Some(info.name().to_string()), wm_class];
        if names.iter().flatten().any(|n| !n.is_empty() && n.to_lowercase() == identity) {
            matches.retain(|(known, _)| *known != ident);
            matches.push((ident, icon));
        }
    }
    if matches.len() == 1 { matches.pop().map(|m| m.1) } else { None }
}

static SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(?:instance|pid)[_.-]?\d+(?:\..*)?$").unwrap());

pub fn application_icon(media: &Media) -> gio::Icon {
    // DesktopEntry is MPRIS's explicit link to an installed launcher.
    if let Some(icon) = desktop_icon(&media.desktop_entry) {
        return icon;
    }
    // Identity is the application's own name. Prefer it to a bus name which
    // may identify the shared engine rather than the application using it.
    let apps = gio::AppInfo::all();
    if let Some(icon) = matching_icon(&apps, &media.identity) {
        return icon;
    }
    // Preserve reverse-DNS IDs, removing only a player's per-process suffix.
    let candidate = media.name.strip_prefix(PREFIX).unwrap_or(&media.name);
    let candidate = SUFFIX.replace(candidate, "").into_owned();
    if let Some(icon) = desktop_icon(&candidate) {
        return icon;
    }
    for name in [media.desktop_entry.as_str(), candidate.as_str()] {
        if let Some(icon) = matching_icon(&apps, name) {
            return icon;
        }
    }
    fallback()
}

pub struct MediaIcons {
    key: RefCell<Option<(String, String, String)>>,
    icon: RefCell<gio::Icon>,
}

impl Default for MediaIcons {
    fn default() -> Self { Self { key: RefCell::new(None), icon: RefCell::new(fallback()) } }
}

impl MediaIcons {
    pub fn resolve(&self, media: Option<&Media>) -> gio::Icon {
        let key = media.map(|m| (m.name.clone(), m.desktop_entry.clone(), m.identity.clone()));
        if key != *self.key.borrow() {
            self.icon.replace(media.map(application_icon).unwrap_or_else(fallback));
            self.key.replace(key);
        }
        self.icon.borrow().clone()
    }

    pub fn close(&self) { self.key.replace(None); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_process_suffix_is_removed_from_bus_names() {
        for (name, expected) in [("org.mozilla.firefox.instance_1_23", "org.mozilla.firefox.instance_1_23"), ("org.mozilla.firefox.instance-7", "org.mozilla.firefox"), ("vlc.instance1234", "vlc"),
                                 ("chromium.instance42.x", "chromium"), ("spotify", "spotify")] {
            assert_eq!(SUFFIX.replace(name, ""), expected);
        }
        assert!(desktop_icon("a/b").is_none());
        let media = Media { name: "org.mpris.MediaPlayer2.no-such-player-xyz".into(), ..Media::default() };
        assert!(application_icon(&media).equal(Some(&fallback())));
    }
}
