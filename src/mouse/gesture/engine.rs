//! The gesture state machine, and the decision it hands back to the event tap.
//!
//! Pure: no FFI, no locks, no clock. The tap thread feeds events and acts on \`Decision\`. Every rule
//! here is a unit test, including the ones that only trip during a lifecycle failure.
//!
//! Two decisions are deliberately *separate*: owning the button's event stream (decided once, at
//! down) and whether to execute or replay (decided at up). Merging them is what produces the classic
//! bug where revoking a gesture's authorisation turns into a click nobody asked for.

use super::recognizer::{Params, Point, Recognizer, Sequence};

/// What happened to the button press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Nothing is held.
    Idle,
    /// The press is ours, but the gesture threshold has not been crossed. Events are swallowed and
    /// held in case the press turns out to be a plain click.
    Held,
    /// The threshold was crossed: the trail is drawn and directions accumulate.
    Engaged,
    /// Esc was pressed: the sequence is abandoned and the press will be replayed at release.
    Abandoned,
}

/// Why a gesture ended without running an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndReason {
    /// Released before crossing the path threshold: a plain click.
    ShortPress,
    /// A gesture, but no binding matched its sequence.
    NoMatch,
    /// The user pressed Esc.
    CancelledByUser,
    /// The configuration withdrew this gesture's authorisation while it was held. Not a click: the
    /// user asked for the gesture to stop existing, and turning that into a click would inject an
    /// event nobody asked for.
    Revoked,
    /// The module stopped, the tap timed out, the device went away or the permission was lost.
    /// Only ever invalidates: no replay, no injected input.
    Invalidated,
}

impl EndReason {
    /// Whether the press should be replayed as a click.
    pub(crate) fn replays(self) -> bool {
        matches!(
            self,
            Self::ShortPress | Self::NoMatch | Self::CancelledByUser
        )
    }
}

/// What the tap must do with the event it is currently handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Not ours: let the event through untouched (but still subject to the shared modifier merge).
    Pass,
    /// Consume it.
    Swallow,
    /// The press was one of ours and ended: replay it as a click, consuming this event.
    Replay,
    /// The release runs a binding: consume the event; the caller executes.
    Execute,
}

/// The button identity of one press. Kept so the session can refuse a second button's events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Button {
    /// CGEvent button number: 1 right, 2 middle, 3/4 side.
    pub(crate) number: u32,
    /// The profile key of the device that pressed it, so two mice never share a session.
    pub(crate) device: u64,
}

/// Everything the engine needs to remember about one press.
#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) phase: Phase,
    pub(crate) button: Button,
    pub(crate) origin: Point,
    /// The sequence at the moment the session ended, for the caller to resolve.
    pub(crate) sequence: Option<Sequence>,
    pub(crate) reason: Option<EndReason>,
    recognizer: Recognizer,
}

impl Session {
    /// Where the press started. The recognizer anchors on it, and it is the only record of the
    /// gesture's origin once the press data has been handed to the replay path.
    pub(crate) fn origin(&self) -> Point {
        self.origin
    }

    pub(crate) fn sequence_key(&self) -> Option<String> {
        self.sequence.as_ref().map(Sequence::to_key)
    }
}

/// The engine. One per tap thread; \`&mut\` because the tap serialises its own delivery.
#[derive(Debug, Default)]
pub(crate) struct Engine {
    session: Option<Session>,
    params: Params,
}

impl Engine {
    pub(crate) fn new(params: Params) -> Self {
        Self {
            session: None,
            params,
        }
    }

    pub(crate) fn phase(&self) -> Phase {
        self.session.as_ref().map_or(Phase::Idle, |s| s.phase)
    }

    pub(crate) fn sequence_key(&self) -> Option<String> {
        self.session.as_ref().and_then(Session::sequence_key)
    }

    /// The directions adopted so far, for the live HUD hint. Not the final answer: a pending turn is
    /// still unconfirmed, and the caller must treat it as a preview.
    pub(crate) fn live_sequence_key(&self) -> Option<String> {
        self.live().map(|session| session.recognizer.snapshot_key())
    }

    /// Whether \`button\` owns the current session. A different button is always foreign.
    pub(crate) fn owns(&self, button: Button) -> bool {
        self.live().is_some_and(|s| s.button == button)
    }

    /// The origin of the live session, for a caller that draws or logs the gesture.
    pub(crate) fn origin(&self) -> Option<Point> {
        self.live().map(Session::origin)
    }

    /// Whether a session is live (not merely recorded). A finished session keeps its outcome for the
    /// caller to read, but never blocks the next press.
    fn live(&self) -> Option<&Session> {
        self.session
            .as_ref()
            .filter(|session| session.phase != Phase::Idle)
    }

    /// Whether the session in flight has seen a sample gap (trail quality).
    pub(crate) fn sequence_degraded(&self) -> bool {
        self.live().is_some_and(|s| s.recognizer.degraded())
    }

    /// A press of a button the caller wants to claim. Returns whether the engine took it.
    pub(crate) fn press(
        &mut self,
        button: Button,
        origin: Point,
        now_ms: f64,
        claim: bool,
    ) -> bool {
        if self.live().is_some() {
            // One press at a time: a live session keeps ownership until its release. A finished one
            // is only a recorded outcome and is replaced here.
            return false;
        }
        if !claim {
            return false;
        }
        self.session = Some(Session {
            phase: Phase::Held,
            button,
            origin,
            sequence: None,
            reason: None,
            recognizer: Recognizer::new(self.params, origin, now_ms),
        });
        true
    }

    /// A drag sample for the button that owns the session.
    pub(crate) fn dragged(&mut self, button: Button, point: Point, now_ms: f64) -> Decision {
        let Some(session) = self.session.as_mut() else {
            return Decision::Pass;
        };
        if session.button != button || session.phase == Phase::Idle {
            return Decision::Pass;
        }
        if session.phase == Phase::Abandoned {
            return Decision::Swallow;
        }
        session.recognizer.push(point, now_ms);
        if session.phase == Phase::Held && session.recognizer.engaged() {
            session.phase = Phase::Engaged;
        }
        Decision::Swallow
    }

    /// Esc. Only a live session can be cancelled; returns whether the key belongs to us.
    pub(crate) fn escape(&mut self) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if matches!(session.phase, Phase::Held | Phase::Engaged) {
            session.phase = Phase::Abandoned;
            return true;
        }
        session.phase == Phase::Abandoned
    }

    /// The release. \`authorized\` is the caller's answer to "may this gesture still run"; it is
    /// consulted only for a sequence that matched, and a \`false\` cancels rather than replays.
    pub(crate) fn release(
        &mut self,
        button: Button,
        point: Point,
        now_ms: f64,
        matched: impl FnOnce(&str) -> bool,
        authorized: impl FnOnce() -> bool,
    ) -> Decision {
        let Some(mut session) = self.session.take() else {
            return Decision::Pass;
        };
        if session.button != button {
            // A foreign button's release: hand the session back untouched and pass the event.
            self.session = Some(session);
            return Decision::Pass;
        }
        // Revocation is decided FIRST. A gesture whose configuration was withdrawn must neither run
        // nor turn into a click: "the binding is gone, so nothing matched, so replay" is exactly the
        // bug this ordering prevents.
        let revoked = !authorized();
        if session.phase == Phase::Abandoned {
            session.reason = Some(if revoked {
                EndReason::Revoked
            } else {
                EndReason::CancelledByUser
            });
            let replays = !revoked;
            self.finish(session);
            return if replays {
                Decision::Replay
            } else {
                Decision::Swallow
            };
        }
        session.recognizer.push(point, now_ms);
        let sequence = session.recognizer.finish();
        let key = sequence.to_key();
        let outcome = if revoked {
            EndReason::Revoked
        } else if key.is_empty() {
            EndReason::ShortPress
        } else if sequence.overlong || !matched(&key) {
            // Too many directions to be a gesture, or one nothing is bound to: the same outcome.
            EndReason::NoMatch
        } else {
            session.sequence = Some(sequence);
            self.finish(session);
            return Decision::Execute;
        };
        session.sequence = Some(sequence);
        session.reason = Some(outcome);
        let replays = outcome.replays();
        self.finish(session);
        if replays {
            Decision::Replay
        } else {
            Decision::Swallow
        }
    }

    /// Invalidate whatever is in flight. \`immediate\` is true when the physical release may never
    /// arrive (a tap timeout, a device removal, a permission loss): the caller must not ask for a
    /// replay, because it would be a click at an unknown place in an app that never saw the press.
    pub(crate) fn invalidate(&mut self) -> Option<EndReason> {
        self.session.take()?;
        Some(EndReason::Invalidated)
    }

    fn finish(&mut self, mut session: Session) {
        session.phase = Phase::Idle;
        // Nothing is retained after a session ends except the recorded outcome, which the caller
        // reads before the next press.
        self.session = Some(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: Button = Button {
        number: 1,
        device: 7,
    };
    const MIDDLE: Button = Button {
        number: 2,
        device: 7,
    };

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn matches_rd(key: &str) -> bool {
        key == "RD"
    }

    fn always() -> bool {
        true
    }

    fn drag_right_then_down(engine: &mut Engine) -> Point {
        let mut at = p(100.0, 100.0);
        for i in 1..=5 {
            at = p(100.0 + i as f64 * 10.0, 100.0);
            engine.dragged(RIGHT, at, i as f64 * 8.0);
        }
        for i in 1..=5 {
            at = p(at.x, 100.0 + i as f64 * 10.0);
            engine.dragged(RIGHT, at, 40.0 + i as f64 * 8.0);
        }
        at
    }

    #[test]
    fn an_unclaimed_press_passes() {
        let mut engine = Engine::new(Params::default());
        assert!(!engine.press(RIGHT, p(0.0, 0.0), 0.0, false));
        assert_eq!(engine.phase(), Phase::Idle);
    }

    #[test]
    fn a_short_press_replays() {
        let mut engine = Engine::new(Params::default());
        assert!(engine.press(RIGHT, p(100.0, 100.0), 0.0, true));
        assert_eq!(engine.phase(), Phase::Held);
        assert_eq!(
            engine.release(RIGHT, p(103.0, 100.0), 10.0, matches_rd, always),
            Decision::Replay,
            "a press that never crossed the threshold is a click"
        );
    }

    #[test]
    fn crossing_the_threshold_engages() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        // 8pt steps: 3 steps = 24pt (still Held), the 4th crosses min_path_length = 30pt.
        let mut at = p(100.0, 100.0);
        for i in 1..=3 {
            at = p(at.x + 8.0, at.y);
            engine.dragged(RIGHT, at, i as f64 * 8.0);
            assert_eq!(
                engine.phase(),
                Phase::Held,
                "step {i} is under the threshold"
            );
        }
        at = p(at.x + 8.0, at.y);
        engine.dragged(RIGHT, at, 32.0);
        assert_eq!(engine.phase(), Phase::Engaged);
    }

    #[test]
    fn a_matching_gesture_executes() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        assert_eq!(
            engine.release(RIGHT, end, 80.0, matches_rd, always),
            Decision::Execute
        );
    }

    #[test]
    fn an_unmatched_gesture_replays() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        assert_eq!(
            engine.release(RIGHT, end, 80.0, |_| false, always),
            Decision::Replay
        );
    }

    #[test]
    fn revoked_authorization_cancels_without_replaying() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        assert_eq!(
            engine.release(RIGHT, end, 80.0, matches_rd, || false),
            Decision::Swallow,
            "a revoked gesture must not become a click the user did not make"
        );
    }

    #[test]
    fn escape_abandons_and_replays() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        assert!(engine.escape(), "Esc belongs to a live session");
        assert_eq!(engine.phase(), Phase::Abandoned);
        assert_eq!(
            engine.release(RIGHT, end, 80.0, matches_rd, always),
            Decision::Replay
        );
    }

    #[test]
    fn a_foreign_button_is_never_the_session() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        assert_eq!(engine.dragged(MIDDLE, p(200.0, 200.0), 5.0), Decision::Pass);
        assert_eq!(
            engine.release(MIDDLE, p(200.0, 200.0), 5.0, |_| true, always),
            Decision::Pass
        );
        assert_eq!(
            engine.phase(),
            Phase::Held,
            "the right-button session survives"
        );
    }

    #[test]
    fn invalidate_never_replays() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        let _ = end;
        assert!(engine.invalidate().is_some());
        assert_eq!(engine.phase(), Phase::Idle);
        assert!(
            !EndReason::Invalidated.replays(),
            "a lifecycle failure must not inject a click"
        );
    }

    #[test]
    fn a_second_gesture_works_after_the_first_one_ended() {
        // The regression the review found: a finished session used to stay in the slot and block
        // every later press.
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        assert_eq!(
            engine.release(RIGHT, end, 80.0, matches_rd, always),
            Decision::Execute
        );
        assert!(
            engine.press(RIGHT, p(300.0, 300.0), 1_000.0, true),
            "the second press must be claimed"
        );
        assert_eq!(engine.phase(), Phase::Held);
    }

    #[test]
    fn a_gesture_works_after_a_short_press() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        assert_eq!(
            engine.release(RIGHT, p(102.0, 100.0), 10.0, matches_rd, always),
            Decision::Replay
        );
        assert!(engine.press(RIGHT, p(300.0, 300.0), 1_000.0, true));
    }

    #[test]
    fn a_revoked_short_press_does_not_become_a_click() {
        // Review finding: revocation was only consulted for a matched sequence, so a revoked press
        // whose sequence was empty replayed.
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        assert_eq!(
            engine.release(RIGHT, p(102.0, 100.0), 10.0, matches_rd, || false),
            Decision::Swallow
        );
    }

    #[test]
    fn a_revoked_unmatched_gesture_does_not_become_a_click() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        assert_eq!(
            engine.release(RIGHT, end, 80.0, |_| false, || false),
            Decision::Swallow
        );
    }

    #[test]
    fn a_revoked_escape_does_not_become_a_click() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        let end = drag_right_then_down(&mut engine);
        assert!(engine.escape());
        assert_eq!(
            engine.release(RIGHT, end, 80.0, matches_rd, || false),
            Decision::Swallow
        );
    }

    #[test]
    fn a_second_press_cannot_steal_the_session() {
        let mut engine = Engine::new(Params::default());
        engine.press(RIGHT, p(100.0, 100.0), 0.0, true);
        assert!(!engine.press(MIDDLE, p(0.0, 0.0), 1.0, true));
        assert!(engine.owns(RIGHT));
        assert!(!engine.owns(MIDDLE));
    }
}
