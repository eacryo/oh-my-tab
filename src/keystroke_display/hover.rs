//! Pure hover-entry state for keeping an active keystroke panel visible.

use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CursorLocation {
    #[default]
    Unknown,
    Outside,
    Inside,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PanelHoverProtection {
    location: CursorLocation,
    protected: bool,
}

impl PanelHoverProtection {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn observe(
        &mut self,
        panel_visible: bool,
        cursor_inside: bool,
        now: Instant,
    ) -> Option<Instant> {
        if !panel_visible {
            self.reset();
            return None;
        }

        let next = if cursor_inside {
            CursorLocation::Inside
        } else {
            CursorLocation::Outside
        };
        match (self.location, next) {
            (CursorLocation::Unknown, next) => {
                self.location = next;
                self.protected = false;
                // The initial sample only establishes the baseline; a parked cursor is not intent.
                None
            }
            (CursorLocation::Outside, CursorLocation::Inside) => {
                self.location = CursorLocation::Inside;
                self.protected = true;
                Some(now)
            }
            (CursorLocation::Inside, CursorLocation::Inside) if self.protected => Some(now),
            (CursorLocation::Inside, CursorLocation::Outside) => {
                self.location = CursorLocation::Outside;
                let was_protected = std::mem::replace(&mut self.protected, false);
                was_protected.then_some(now)
            }
            (_, next) => {
                self.location = next;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PanelHoverProtection;
    use crate::keystroke_display::state::{DisplayMode, Input, KeyGlyph, StateMachine};
    use std::time::{Duration, Instant};

    fn state_with_key(now: Instant) -> StateMachine {
        let mut state = StateMachine::default();
        state.apply(
            Input::KeyDown {
                keycode: 0,
                flags: 0,
                autorepeat: false,
                unicode: "a".into(),
                glyph: KeyGlyph::EventUnicode,
            },
            DisplayMode::All,
            now,
        );
        state
    }

    fn observe(
        hover: &mut PanelHoverProtection,
        state: &mut StateMachine,
        visible: bool,
        inside: bool,
        now: Instant,
    ) -> Option<Instant> {
        let activity = hover.observe(visible, inside, now);
        if let Some(activity_at) = activity {
            state.note_activity(activity_at);
        }
        activity
    }

    #[test]
    fn preexisting_cursor_inside_does_not_protect_or_extend_idle_deadline() {
        let start = Instant::now();
        let mut state = state_with_key(start);
        let initial_deadline = state.deadline();
        let mut hover = PanelHoverProtection::default();

        assert_eq!(observe(&mut hover, &mut state, true, true, start), None);
        assert_eq!(
            observe(
                &mut hover,
                &mut state,
                true,
                true,
                start + Duration::from_millis(500)
            ),
            None
        );
        assert_eq!(state.deadline(), initial_deadline);
        assert!(state.tick(initial_deadline.unwrap()));
    }

    #[test]
    fn entering_from_outside_extends_deadline_on_entry_and_while_inside() {
        let start = Instant::now();
        let mut state = state_with_key(start);
        let mut hover = PanelHoverProtection::default();
        observe(&mut hover, &mut state, true, false, start);
        let entered = start + Duration::from_millis(900);
        assert_eq!(
            observe(&mut hover, &mut state, true, true, entered),
            Some(entered)
        );
        let held = start + Duration::from_millis(1400);
        assert_eq!(
            observe(&mut hover, &mut state, true, true, held),
            Some(held)
        );
        assert_eq!(
            state.deadline(),
            Some(held + super::super::state::IDLE_FADE)
        );
        assert!(!state.tick(start + super::super::state::IDLE_FADE));
    }

    #[test]
    fn leaving_protection_restarts_idle_deadline_from_the_leave_tick() {
        let start = Instant::now();
        let mut state = state_with_key(start);
        let mut hover = PanelHoverProtection::default();
        observe(&mut hover, &mut state, true, false, start);
        observe(
            &mut hover,
            &mut state,
            true,
            true,
            start + Duration::from_millis(500),
        );
        let left = start + Duration::from_millis(900);
        assert_eq!(
            observe(&mut hover, &mut state, true, false, left),
            Some(left)
        );
        assert_eq!(
            state.deadline(),
            Some(left + super::super::state::IDLE_FADE)
        );
        assert!(!state.tick(start + super::super::state::IDLE_FADE));
        assert_eq!(
            observe(
                &mut hover,
                &mut state,
                true,
                false,
                left + Duration::from_millis(100)
            ),
            None
        );
        assert!(state.tick(left + super::super::state::IDLE_FADE));
    }

    #[test]
    fn reentering_after_leaving_restarts_protection() {
        let start = Instant::now();
        let mut state = state_with_key(start);
        let mut hover = PanelHoverProtection::default();
        observe(&mut hover, &mut state, true, false, start);
        let entered = start + Duration::from_millis(100);
        assert_eq!(
            observe(&mut hover, &mut state, true, true, entered),
            Some(entered)
        );
        let left = start + Duration::from_millis(200);
        assert_eq!(
            observe(&mut hover, &mut state, true, false, left),
            Some(left)
        );
        let reentered = start + Duration::from_millis(400);
        assert_eq!(
            observe(&mut hover, &mut state, true, true, reentered),
            Some(reentered)
        );
        assert_eq!(
            state.deadline(),
            Some(reentered + super::super::state::IDLE_FADE)
        );
    }

    #[test]
    fn hide_resets_unknown_so_the_next_inside_sample_is_not_protected() {
        let start = Instant::now();
        let mut state = state_with_key(start);
        let mut hover = PanelHoverProtection::default();
        observe(&mut hover, &mut state, true, false, start);
        observe(
            &mut hover,
            &mut state,
            true,
            true,
            start + Duration::from_millis(100),
        );
        assert_eq!(
            observe(
                &mut hover,
                &mut state,
                false,
                true,
                start + Duration::from_millis(200)
            ),
            None
        );
        let deadline = state.deadline();
        assert_eq!(
            observe(
                &mut hover,
                &mut state,
                true,
                true,
                start + Duration::from_millis(300)
            ),
            None
        );
        assert_eq!(state.deadline(), deadline);
    }
}
