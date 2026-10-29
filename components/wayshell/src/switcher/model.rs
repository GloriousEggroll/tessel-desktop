//! Port of switcher_model.py: most-recently-used window order with a stable
//! in-progress switch selection.
//!
//! Use complete backend IDs; only observed activation changes update MRU.
//! Minimized windows and windows without an app ID or title remain selectable.
//! A switch operation freezes the order so title changes and focus events from
//! opening the switcher cannot move the selection beneath the user.

use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct MruSwitcher {
    mru: Vec<String>,
    activated: HashSet<String>,
    candidates: Vec<String>,
    index: Option<usize>,
    pub active: bool,
}

impl MruSwitcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ordered_ids(&self) -> Vec<String> {
        if self.active { self.candidates.clone() } else { self.mru.clone() }
    }

    pub fn selected_id(&self) -> Option<String> {
        if !self.active {
            return None;
        }
        self.index.map(|i| self.candidates[i].clone())
    }

    /// `windows`: (id, activated) in backend (dict) order.
    pub fn update<'a>(&mut self, windows: impl IntoIterator<Item = (&'a str, bool)>) {
        let windows: Vec<(&str, bool)> = windows.into_iter().collect();
        let identifiers: Vec<&str> = windows.iter().map(|(id, _)| *id).collect();
        let present: HashSet<&str> = identifiers.iter().copied().collect();
        self.mru.retain(|id| present.contains(id.as_str()));
        let known: HashSet<String> = self.mru.iter().cloned().collect();
        for id in &identifiers {
            if !known.contains(*id) && !self.mru.iter().any(|m| m == id) {
                self.mru.push(id.to_string());
            }
        }
        let activated: HashSet<String> = windows.iter().filter(|(_, a)| *a).map(|(id, _)| id.to_string()).collect();
        // Iterate deterministically if a compositor briefly reports more than
        // one activated window. Unchanged activation flags never reorder MRU.
        for id in &identifiers {
            if activated.contains(*id) && !self.activated.contains(*id) {
                if let Some(pos) = self.mru.iter().position(|m| m == id) {
                    let value = self.mru.remove(pos);
                    self.mru.insert(0, value);
                }
            }
        }
        self.activated = activated;

        if !self.active {
            return;
        }
        let previous = std::mem::take(&mut self.candidates);
        let selected = self.index.map(|i| previous[i].clone());
        let mut remaining: Vec<String> = previous.iter().filter(|id| present.contains(id.as_str())).cloned().collect();
        let previous_ids: HashSet<&str> = previous.iter().map(String::as_str).collect();
        for id in &identifiers {
            if !previous_ids.contains(*id) && !remaining.iter().any(|r| r == id) {
                remaining.push(id.to_string());
            }
        }
        if remaining.is_empty() {
            self.cancel();
            return;
        }
        let index = match selected.as_ref().and_then(|s| remaining.iter().position(|r| r == s)) {
            Some(index) => index,
            None => {
                // Choose the next surviving candidate in the old order, or the
                // previous one at the end.
                let before = previous[..self.index.unwrap_or(0)].iter().filter(|id| present.contains(id.as_str())).count();
                before.min(remaining.len() - 1)
            }
        };
        self.candidates = remaining;
        self.index = Some(index);
    }

    pub fn begin(&mut self, reverse: bool, select_current: bool) -> Option<String> {
        if self.active {
            return self.advance(reverse);
        }
        if self.mru.is_empty() {
            return None;
        }
        self.candidates = self.mru.clone();
        let current = self.mru.iter().find(|id| self.activated.contains(*id)).cloned();
        if let Some(current) = &current {
            self.candidates.retain(|id| id != current);
            self.candidates.insert(0, current.clone());
        }
        self.active = true;
        let n = self.candidates.len();
        self.index = Some(if select_current {
            0
        } else if reverse {
            n - 1
        } else if current.is_some() {
            1 % n
        } else {
            0
        });
        self.selected_id()
    }

    pub fn advance(&mut self, reverse: bool) -> Option<String> {
        if !self.active {
            return self.begin(reverse, false);
        }
        let n = self.candidates.len() as isize;
        let i = self.index.unwrap_or(0) as isize + if reverse { -1 } else { 1 };
        self.index = Some(i.rem_euclid(n) as usize);
        self.selected_id()
    }

    pub fn finish(&mut self) -> Option<String> {
        let selected = self.selected_id();
        self.cancel();
        selected
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.candidates.clear();
        self.index = None;
    }
}

#[cfg(test)]
mod tests {
    //! test_switcher_model.py
    use super::*;

    fn w<'a>(ids: &'a [&'a str], active: Option<&str>) -> Vec<(&'a str, bool)> {
        ids.iter().map(|id| (*id, Some(*id) == active)).collect()
    }

    fn ids(model: &MruSwitcher) -> Vec<String> {
        model.ordered_ids()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn some(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn activation_transitions_create_mru_and_forward_cycle_wraps() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b", "c"], Some("a")));
        m.update(w(&["a", "b", "c"], Some("c")));
        m.update(w(&["a", "b", "c"], Some("b")));
        assert_eq!(ids(&m), s(&["b", "c", "a"]));
        assert_eq!(m.begin(false, false), some("c"));
        assert_eq!(m.advance(false), some("a"));
        assert_eq!(m.advance(false), some("b"));
        assert_eq!(m.advance(false), some("c"));
    }

    #[test]
    fn reverse_starts_at_end_and_cycles_backwards() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b", "c"], Some("a")));
        assert_eq!(m.begin(true, false), some("c"));
        assert_eq!(m.advance(true), some("b"));
        assert_eq!(m.advance(true), some("a"));
        assert_eq!(m.advance(true), some("c"));
    }

    #[test]
    fn activities_begin_selects_current_then_advances_normally() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b", "c"], Some("c")));
        assert_eq!(m.begin(false, true), some("c"));
        assert_eq!(m.advance(false), some("a"));
        m.cancel();
        m.update(w(&["a", "b", "c"], None));
        assert_eq!(m.begin(false, true), some("c"));
    }

    #[test]
    fn single_window_and_empty_model() {
        let mut m = MruSwitcher::new();
        assert_eq!(m.begin(false, false), None);
        assert!(!m.active);
        m.update(w(&["only"], Some("only")));
        assert_eq!(m.begin(false, false), some("only"));
        assert_eq!(m.advance(true), some("only"));
        assert_eq!(m.finish(), some("only"));
        assert!(!m.active);
        assert_eq!(m.selected_id(), None);
        assert_eq!(m.finish(), None);
    }

    #[test]
    fn begin_without_focused_window_starts_with_most_recent() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b"], Some("b")));
        m.update(w(&["a", "b"], None));
        assert_eq!(m.begin(false, false), some("b"));
        m.cancel();
        assert_eq!(m.begin(true, false), some("a"));
    }

    #[test]
    fn active_order_survives_live_focus_and_rapid_title_changes() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b", "c"], Some("a")));
        assert_eq!(m.begin(false, false), some("b"));
        for _ in 0..50 {
            m.update(w(&["a", "b", "c"], Some("c")));
            assert_eq!(ids(&m), s(&["a", "b", "c"]));
            assert_eq!(m.selected_id(), some("b"));
        }
        m.cancel();
        assert_eq!(ids(&m), s(&["c", "a", "b"]));
    }

    #[test]
    fn finish_and_cancel_do_not_invent_activation() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b", "c"], Some("a")));
        m.begin(false, false);
        assert_eq!(m.finish(), some("b"));
        assert_eq!(ids(&m), s(&["a", "b", "c"]));
        m.begin(true, false);
        m.cancel();
        assert_eq!(ids(&m), s(&["a", "b", "c"]));
        m.update(w(&["a", "b", "c"], Some("b")));
        assert_eq!(ids(&m), s(&["b", "a", "c"]));
    }

    #[test]
    fn closed_selected_window_chooses_closest_surviving_successor() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b", "c", "d"], Some("a")));
        m.begin(false, false);
        assert_eq!(m.selected_id(), some("b"));
        m.update(w(&["c", "d"], None));
        assert_eq!(m.selected_id(), some("c"));
        assert_eq!(ids(&m), s(&["c", "d"]));
        m.advance(false);
        m.update(w(&["c"], None));
        assert_eq!(m.selected_id(), some("c"));
    }

    #[test]
    fn removal_before_selection_and_new_window_keep_selection_stable() {
        let mut m = MruSwitcher::new();
        m.update(w(&["a", "b", "c"], Some("a")));
        m.begin(true, false);
        m.update(w(&["b", "c", "new"], Some("new")));
        assert_eq!(ids(&m), s(&["b", "c", "new"]));
        assert_eq!(m.selected_id(), some("c"));
        assert_eq!(m.advance(false), some("new"));
        m.cancel();
        assert_eq!(ids(&m), s(&["new", "b", "c"]));
    }

    #[test]
    fn minimized_windows_and_empty_titles_or_app_ids_remain_selectable() {
        // The model never looks at minimized/app_id/title.
        let mut m = MruSwitcher::new();
        m.update(w(&["normal", "minimized"], Some("normal")));
        assert_eq!(m.begin(false, false), some("minimized"));
    }

    #[test]
    fn backend_clear_cancels_and_session_prefixes_are_not_discarded() {
        let mut m = MruSwitcher::new();
        m.update(w(&["old-session:1", "old-session:2"], Some("old-session:1")));
        m.begin(false, false);
        m.update(Vec::<(&str, bool)>::new());
        assert!(!m.active);
        assert!(ids(&m).is_empty());
        assert_eq!(m.finish(), None);
        m.update(w(&["new-session:1", "new-session:2"], Some("new-session:2")));
        assert_eq!(m.begin(false, false), some("new-session:1"));
        assert_eq!(m.finish(), some("new-session:1"));
    }

    #[test]
    fn replaced_session_handles_never_return_stale_selection() {
        let mut m = MruSwitcher::new();
        m.update(w(&["session-a:1", "session-a:2"], Some("session-a:1")));
        m.begin(false, false);
        m.update(w(&["session-b:1", "session-b:2"], Some("session-b:1")));
        assert!(m.active);
        assert_eq!(ids(&m), s(&["session-b:1", "session-b:2"]));
        assert_eq!(m.finish(), some("session-b:1"));
    }

    #[test]
    fn mutating_backend_objects_does_not_hide_activation_transitions() {
        let mut m = MruSwitcher::new();
        m.update(vec![("a", true), ("b", false)]);
        m.update(vec![("a", false), ("b", true)]);
        assert_eq!(ids(&m), s(&["b", "a"]));
    }
}
