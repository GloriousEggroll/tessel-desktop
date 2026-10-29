//! Remember a title-button minimize through the automatic focus hand-off
//! (port of focused_title.py; contract §5 "Focused title").
//!
//! API: [`FocusedTitle`] (`clear`, `remember(id)`, `resolve(&windows)` →
//! the window the title shows/acts on). Share one instance across clones.

use super::model::WindowRecord;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FocusedTitle {
    pub restore_id: Option<String>,
    pub minimized: bool,
    pub fallback_id: Option<String>,
}

impl FocusedTitle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn remember(&mut self, window_id: &str) {
        self.clear();
        self.restore_id = Some(window_id.to_owned());
    }

    /// `windows` in the backend's (dict) order.
    pub fn resolve<'a, W: WindowRecord>(&mut self, windows: &'a [W]) -> Option<&'a W> {
        let current = windows.iter().find(|w| w.activated() && !w.minimized());
        let remembered = self.restore_id.as_ref().and_then(|id| windows.iter().find(|w| w.id() == *id));
        let Some(remembered) = remembered else {
            self.clear();
            return current;
        };
        if !self.minimized {
            // Focus and minimization can arrive in either order. Until the
            // minimize is confirmed, a second click still restores this ID.
            if remembered.minimized() {
                self.minimized = true;
                self.fallback_id = current.map(|w| w.id());
            }
            return Some(remembered);
        }
        if !remembered.minimized() {
            self.clear();
            return current;
        }
        let focused_id = current.map(|w| w.id());
        if focused_id != self.fallback_id {
            if current.is_some() {
                self.clear();
                return current;
            }
            self.fallback_id = None;
        }
        Some(remembered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::WindowData;

    fn window(id: &str, activated: bool, minimized: bool) -> WindowData {
        WindowData { activated, minimized, ..WindowData::new(id, "app", id) }
    }

    fn resolved(state: &mut FocusedTitle, windows: &[WindowData]) -> Option<String> {
        state.resolve(windows).map(|w| w.id.clone())
    }

    #[test]
    fn minimize_restore_and_tracking_the_focused_window() {
        let mut state = FocusedTitle::new();
        let mut windows = vec![window("browser-first", true, false), window("files-first", false, false)];
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-first"));
        state.remember("browser-first");
        // Before the compositor confirms the minimize the same ID stays the target.
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-first"));
        windows[0] = window("browser-first", false, true);
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-first"));
        assert!(state.minimized);
        // Restored by the second click: back to the current focus.
        windows[0] = window("browser-first", true, false);
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-first"));
        assert_eq!(state.restore_id, None);
        // A new focused application replaces the remembered target.
        state.remember("browser-first");
        windows[0] = window("browser-first", false, true);
        resolved(&mut state, &windows);
        windows[1] = window("files-first", true, false);
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("files-first"));
        // A remembered window that closes leaves no stale target.
        state.remember("files-first");
        windows.remove(1);
        assert_eq!(resolved(&mut state, &windows), None);
        assert_eq!(state, FocusedTitle::default());
    }

    #[test]
    fn minimized_title_survives_automatic_focus_handoff() {
        let mut state = FocusedTitle::new();
        let mut windows = vec![window("browser-first", true, false), window("files-first", false, false), window("browser-second", false, false)];
        state.remember("browser-first");
        // Minimize confirmed and the compositor hands focus to files-first together.
        windows[0] = window("browser-first", false, true);
        windows[1] = window("files-first", true, false);
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-first"));
        assert_eq!(state.fallback_id.as_deref(), Some("files-first"));
        // Title updates of the automatic successor keep the remembered window.
        windows[1].title = "Automatic successor updated".into();
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-first"));
        // Focus leaving entirely keeps it too, clearing the fallback.
        windows[1] = window("files-first", false, false);
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-first"));
        assert_eq!(state.fallback_id, None);
        // A user focusing another window replaces it.
        windows[2] = window("browser-second", true, false);
        assert_eq!(resolved(&mut state, &windows).as_deref(), Some("browser-second"));
        assert_eq!(state.restore_id, None);
    }
}
