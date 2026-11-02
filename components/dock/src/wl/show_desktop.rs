//! Show desktop (tessel-dock.py `toggle_show_desktop`, `changed()`, the
//! `--desktop-status` command and status_backend's `set_show_desktop`).
//!
//! The first toggle remembers the focused window and minimizes every window
//! that is not minimized yet, remembering exactly those it asked to minimize.
//! The second toggle unminimizes only those (queued even if the compositor
//! has not acknowledged the minimize yet), then re-activates the remembered
//! focus if it was among them. Pre-existing minimization and maximized state
//! are preserved (unminimize never touches maximization). Windows that close
//! are dropped from the set; once it is empty the state resets.
//!
//! Public API: [`ShowDesktop`] (`toggle(&backend, close_popups) -> Option<bool>`,
//! `windows_changed(&backend) -> bool`, `active()`, `windows()`, `focus()`),
//! the [`DesktopWindows`] trait (implemented by [`WindowBackend`]),
//! [`indicator`] (the `showdesktop` indicator state) and [`desktop_status`] /
//! [`desktop_status_line`] (Waybar JSON, Python `json.dumps` bytes).

use serde_json::json;
use tessel_core::tr;

use super::windows::WindowBackend;

/// What the state machine needs from a window backend.
pub trait DesktopWindows {
    fn ready(&self) -> bool;
    /// `(id, activated, minimized)` in backend (dict) order.
    fn window_states(&self) -> Vec<(String, bool, bool)>;
    fn contains(&self, id: &str) -> bool;
    fn minimize(&self, id: &str) -> bool;
    fn unminimize(&self, id: &str) -> bool;
    fn activate(&self, id: &str) -> bool;
}

impl DesktopWindows for WindowBackend {
    fn ready(&self) -> bool { WindowBackend::ready(self) }
    fn window_states(&self) -> Vec<(String, bool, bool)> {
        self.windows().values().map(|w| (w.id.clone(), w.activated, w.minimized)).collect()
    }
    fn contains(&self, id: &str) -> bool { self.windows().contains_key(id) }
    fn minimize(&self, id: &str) -> bool { WindowBackend::minimize(self, id) }
    fn unminimize(&self, id: &str) -> bool { WindowBackend::unminimize(self, id) }
    fn activate(&self, id: &str) -> bool { WindowBackend::activate(self, id) }
}

/// Python `_desktop_windows` (insertion ordered) + `_desktop_focus`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShowDesktop {
    windows: Vec<String>,
    focus: Option<String>,
}

impl ShowDesktop {
    pub fn new() -> Self { Self::default() }

    /// True while windows hidden by show-desktop wait to be restored.
    pub fn active(&self) -> bool { !self.windows.is_empty() }
    pub fn windows(&self) -> &[String] { &self.windows }
    pub fn focus(&self) -> Option<&str> { self.focus.as_deref() }

    /// `--toggle-desktop`. None when the backend is not ready (exit 1, nothing
    /// happens); otherwise `close_popups` ran and the new active state is
    /// returned — pass it to the indicator (`set_show_desktop`).
    pub fn toggle(&mut self, backend: &dyn DesktopWindows, close_popups: impl FnOnce()) -> Option<bool> {
        if !backend.ready() {
            return None;
        }
        close_popups();
        if !self.windows.is_empty() {
            let restore = std::mem::take(&mut self.windows);
            let focused = self.focus.take();
            // Queue unminimize even if a prior minimize has not been observed
            // yet, so rapid double-clicks leave windows restored in order.
            for id in &restore {
                if backend.contains(id) {
                    backend.unminimize(id);
                }
            }
            if let Some(focused) = focused.filter(|f| restore.contains(f) && backend.contains(f)) {
                backend.activate(&focused);
            }
        } else {
            let windows = backend.window_states();
            self.focus = windows.iter().find(|(_, activated, minimized)| *activated && !minimized).map(|w| w.0.clone());
            for (id, _, minimized) in windows {
                if !minimized && backend.minimize(&id) && !self.windows.contains(&id) {
                    self.windows.push(id);
                }
            }
        }
        Some(self.active())
    }

    /// The window list changed: forget closed windows. True when the set just
    /// became empty (Python then calls `set_show_desktop(False)`).
    pub fn windows_changed(&mut self, backend: &dyn DesktopWindows) -> bool {
        if self.windows.is_empty() {
            return false;
        }
        self.windows.retain(|id| backend.contains(id));
        if self.windows.is_empty() {
            self.focus = None;
            return true;
        }
        false
    }
}

/// The `showdesktop` indicator: (text, tooltip, classes).
pub fn indicator(active: bool) -> (String, String, Vec<&'static str>) {
    let tooltip = if active { tr!("Restore windows") } else { tr!("Show desktop") };
    let classes = if active { vec!["active", "solid"] } else { vec!["regular"] };
    ("\u{f2d2}".to_owned(), tooltip, classes)
}

/// `{"text": …, "tooltip": …, "class": […], "active": bool}` as Python's
/// `json.dumps` (ASCII escapes, `", "` / `": "` separators), no newline.
pub fn desktop_status(active: bool) -> String {
    let (text, tooltip, classes) = indicator(active);
    let value = json!({"text": text, "tooltip": tooltip, "class": classes, "active": active});
    tessel_core::pyfmt::json_dumps(&value, &tessel_core::pyfmt::JsonStyle::DEFAULT)
}

/// The exact `--desktop-status` output (`json.dumps(result) + "\n"`).
pub fn desktop_status_line(active: bool) -> String { desktop_status(active) + "\n" }

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// The dock_ui test's stub backend: observed state only changes when the
    /// test says so (minimize acknowledgements can lag).
    #[derive(Default)]
    struct Stub {
        ready: bool,
        windows: RefCell<Vec<(String, bool, bool)>>,
        history: RefCell<Vec<(&'static str, String)>>,
    }
    impl DesktopWindows for Stub {
        fn ready(&self) -> bool { self.ready }
        fn window_states(&self) -> Vec<(String, bool, bool)> { self.windows.borrow().clone() }
        fn contains(&self, id: &str) -> bool { self.windows.borrow().iter().any(|w| w.0 == id) }
        fn minimize(&self, id: &str) -> bool { self.history.borrow_mut().push(("minimize", id.into())); true }
        fn unminimize(&self, id: &str) -> bool { self.history.borrow_mut().push(("restore", id.into())); true }
        fn activate(&self, id: &str) -> bool { self.history.borrow_mut().push(("activate", id.into())); true }
    }

    fn stub() -> Stub {
        Stub {
            ready: true,
            windows: RefCell::new(vec![("browser-first".into(), true, false), ("browser-second".into(), false, true),
                                       ("files-first".into(), false, false)]),
            ..Default::default()
        }
    }

    fn status(active: bool) -> serde_json::Value { serde_json::from_str(&desktop_status(active)).unwrap() }

    #[test]
    fn show_desktop_restores_only_its_windows_even_before_minimize_acknowledgement() {
        let backend = stub();
        let mut desktop = ShowDesktop::new();
        let mut statuses = Vec::new();
        let popups = std::cell::Cell::new(0);
        assert_eq!(status(desktop.active())["active"], false);
        assert_eq!(status(desktop.active())["class"], json!(["regular"]));
        assert!(status(false).get("classes").is_none());
        statuses.extend(desktop.toggle(&backend, || popups.set(popups.get() + 1)));
        assert_eq!(status(desktop.active())["active"], true);
        assert!(status(desktop.active())["class"].as_array().unwrap().contains(&json!("active")));
        assert_eq!(*backend.history.borrow(), [("minimize", "browser-first".to_owned()), ("minimize", "files-first".to_owned())]);
        statuses.extend(desktop.toggle(&backend, || popups.set(popups.get() + 1)));
        assert_eq!(status(desktop.active())["active"], false);
        let history = backend.history.borrow().clone();
        assert_eq!(history[2..4], [("restore", "browser-first".to_owned()), ("restore", "files-first".to_owned())]);
        assert_eq!(history[4], ("activate", "browser-first".to_owned()));
        assert_eq!(statuses, [true, false]);
        assert_eq!(popups.get(), 2);
        assert!(desktop.windows().is_empty());
        // Closing a hidden application before restoring never targets its old
        // ID or any subsequently opened window with a different ID.
        desktop.toggle(&backend, || {});
        backend.windows.borrow_mut().retain(|w| w.0 != "browser-first");
        backend.windows.borrow_mut().push(("new-window".into(), false, false));
        backend.history.borrow_mut().clear();
        desktop.toggle(&backend, || {});
        assert_eq!(*backend.history.borrow(), [("restore", "files-first".to_owned())]);
    }

    #[test]
    fn not_ready_backend_does_nothing() {
        let mut backend = stub();
        backend.ready = false;
        let mut desktop = ShowDesktop::new();
        assert_eq!(desktop.toggle(&backend, || panic!("popups stay")), None);
        assert!(backend.history.borrow().is_empty());
    }

    #[test]
    fn refused_minimize_is_not_remembered_and_closing_all_hidden_windows_resets() {
        struct Refusing(Stub);
        impl DesktopWindows for Refusing {
            fn ready(&self) -> bool { true }
            fn window_states(&self) -> Vec<(String, bool, bool)> { self.0.window_states() }
            fn contains(&self, id: &str) -> bool { self.0.contains(id) }
            fn minimize(&self, id: &str) -> bool { id != "files-first" }
            fn unminimize(&self, id: &str) -> bool { self.0.unminimize(id) }
            fn activate(&self, id: &str) -> bool { self.0.activate(id) }
        }
        let backend = Refusing(stub());
        let mut desktop = ShowDesktop::new();
        assert_eq!(desktop.toggle(&backend, || {}), Some(true));
        assert_eq!(desktop.windows(), ["browser-first"]);
        assert_eq!(desktop.focus(), Some("browser-first"));
        assert!(!desktop.windows_changed(&backend));
        backend.0.windows.borrow_mut().retain(|w| w.0 != "browser-first");
        assert!(desktop.windows_changed(&backend), "set emptied: status goes inactive");
        assert!(!desktop.active() && desktop.focus().is_none());
        assert!(!desktop.windows_changed(&backend));
    }

    #[test]
    fn desktop_status_bytes_match_python() {
        assert_eq!(desktop_status_line(false),
                   "{\"text\": \"\\uf2d2\", \"tooltip\": \"Show desktop\", \"class\": [\"regular\"], \"active\": false}\n");
        assert_eq!(desktop_status(true),
                   "{\"text\": \"\\uf2d2\", \"tooltip\": \"Restore windows\", \"class\": [\"active\", \"solid\"], \"active\": true}");
        // The real Python path: status_backend.indicator + tessel-dock.py's dict.
        let script = format!("import sys, json\nsys.path.insert(0, '{}/tessel-bluetooth/tessel-dock')\n\
            from status_backend import indicator, ICONS\n\
            for a in (False, True):\n\
             s = indicator({{'text': ICONS['showdesktop'], 'tooltip': 'Restore windows' if a else 'Show desktop', \
            'class': ['active', 'solid'] if a else 'regular'}})\n\
             print(json.dumps({{'text': s['text'], 'tooltip': s.get('tooltip', ''), 'class': list(s.get('classes', ())), 'active': a}}))",
            std::env::var("HOME").unwrap_or_default());
        if let Ok(out) = std::process::Command::new("python3").args(["-I", "-c", &script]).output() {
            if out.status.success() {
                assert_eq!(String::from_utf8(out.stdout).unwrap(), desktop_status_line(false) + &desktop_status_line(true));
            } else {
                eprintln!("python parity skipped: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
    }
}
