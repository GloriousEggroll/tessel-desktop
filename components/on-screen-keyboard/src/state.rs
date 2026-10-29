//! Visibility policy, independent of GTK and hardware access (osk_state.py).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visibility {
    pub eligible: bool,
    pub manual: bool,
    pub active: bool,
    pub dismissed: bool,
    pub epoch: i64,
}

impl Default for Visibility {
    fn default() -> Self { Self { eligible: false, manual: false, active: false, dismissed: false, epoch: -1 } }
}

impl Visibility {
    pub fn eligible(eligible: bool) -> Self { Self { eligible, ..Self::default() } }
    pub fn visible(&self) -> bool { self.manual || (self.eligible && self.active && !self.dismissed) }
    pub fn needed(&self) -> bool { self.eligible || self.manual }
    /// A text-input `done`: a new epoch (newly activated field) clears `dismissed`.
    pub fn context(&mut self, active: bool, epoch: i64) {
        if epoch != self.epoch {
            self.dismissed = false;
        }
        self.active = active;
        self.epoch = epoch;
    }
    pub fn show(&mut self) {
        self.manual = true;
        self.dismissed = false;
    }
    pub fn hide(&mut self) {
        self.manual = false;
        self.dismissed = true;
    }
    pub fn hardware(&mut self, eligible: bool) {
        if self.eligible && !eligible {
            self.hide();
        }
        if eligible && !self.eligible {
            self.dismissed = false;
            self.epoch = -1;
        }
        self.eligible = eligible;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // tests/test_policy.py: test_hide_sticks_until_a_new_field_even_with_repeated_commits
    #[test]
    fn hide_sticks_until_a_new_field_even_with_repeated_commits() {
        let mut state = Visibility::eligible(true);
        state.context(true, 1);
        assert!(state.visible());
        state.hide();
        assert!(!state.visible());
        for _ in 0..5 {
            state.context(true, 1);
            assert!(!state.visible());
        }
        state.context(true, 2);
        assert!(state.visible());
        state.context(false, 2);
        assert!(!state.visible());
    }

    // tests/test_policy.py: test_manual_use_and_keyboard_hotplug
    #[test]
    fn manual_use_and_keyboard_hotplug() {
        let mut state = Visibility::default();
        state.show();
        assert!(state.visible());
        state.hide();
        assert!(!state.needed());
        state.hardware(true);
        state.context(true, 4);
        assert!(state.visible());
        state.hardware(false);
        assert!(!state.visible());
        assert!(!state.needed());
    }

    #[test]
    fn gaining_eligibility_resets_dismissal_and_epoch() {
        let mut state = Visibility { dismissed: true, epoch: 7, ..Default::default() };
        state.hardware(true);
        assert!(!state.dismissed);
        assert_eq!(state.epoch, -1);
        state.hardware(true);
        assert!(state.eligible);
    }
}
