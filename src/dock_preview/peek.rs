//! Pure decisions for the hover peek: hovering a preview card brings that window to the front and
//! leaving the card puts the application that was in front back.
//!
//! The interaction is not a z-order trick. Measured on macOS 26 with a probe outside this app
//! (2026-11-24): `_SLPSSetFrontProcessWithOptions` and the synthetic key click alone reorder
//! nothing while the target application is not active, and a window only comes above another
//! application once that application is ACTIVE -- the click then lands the exact window. So every
//! peek is a real application activation, and the restore is a second one. What is assertable
//! without a screen is the decision: when to arm, when to peek, and above all when NOT to restore,
//! because a restore that runs after the user has already switched away drags them back.
//!
//! `Mod` owns no timing state of its own: the armed instant and the identity that is currently
//! peeked travel with it, so every rule below is a function of values the caller can print.

use std::time::{Duration, Instant};

use crate::dock_preview::layout::PointerZone;

/// How long the pointer must rest on a card's body before that window is brought to the front.
///
/// Long enough that crossing a panel on the way to the Dock does not activate anything; short
/// enough to feel like the reference product's hover behaviour. It is deliberately below
/// `TOOLTIP_DELAY`: the tooltip needs the pointer on the title's own box, the peek needs it on the
/// card's body, so the two never describe the same moment.
pub(crate) const PEEK_DELAY: Duration = Duration::from_millis(180);

/// One window of the previewed application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PeekTarget {
    pub pid: i32,
    pub wid: u32,
}

/// Why a hover may not bring its window to the front. Each reason is a separate variant because the
/// log and the frame record name the rule that refused, not just "no".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeekRefusal {
    /// The feature or the whole Dock preview is off.
    Disabled,
    /// The pointer is not on a card's body: the title box (tooltip) and the close slot have their own
    /// meaning, and the panel's padding is not a card.
    NotOnCardBody,
    /// The card is already the frontmost application's window: bringing it forward would spend a
    /// real activation on a switch that changes nothing.
    AlreadyFrontmost,
    /// The card is a window of an application that is hidden (Command+H).
    AppHidden,
    /// The card is a minimized window.
    Minimized,
    /// The window lives on another desktop; a cross-Space peek also moves the Space, which is a
    /// bigger transition than the pointer resting on a card asks for.
    OtherDesktop,
    /// The Dock was pressed within `PRESS_OPEN_WINDOW`: that press is the user's switch, and a peek
    /// must not start one of its own between the press and the Dock's own activation.
    PressPending,
    /// The pointer is on a card whose target disappeared: nothing to show.
    NoWindow,
    /// A raise for this card is still being prepared, and the pointer has left it in the meantime:
    /// the landing switch must not run for a window nobody is hovering any more.
    LeftDuringRaise,
}

/// What the peek wants this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeekDecision {
    /// Nothing is armed and nothing is peeked: the tick does nothing.
    Idle,
    /// Nothing is peeked, but the pointer is resting on `target`: show this window if it is still
    /// here at `at`.
    Arm { target: PeekTarget, at: Instant },
    /// The pointer is resting on the armed card: bring it to the front now.
    Peek { target: PeekTarget },
    /// A peek is already up on another card and the pointer is resting on this one: switch the peek
    /// to it WITHOUT giving the front back in between. This is what keeps sliding from one thumbnail
    /// to the next from flashing the application the user came from.
    Retarget { target: PeekTarget },
    /// The pointer is no longer on the card that is peeked (or the peek configuration was turned
    /// off): give the front back to the application it was taken from.
    Restore,
    /// Stop describing a peek: either nothing was shown, or the front no longer belongs to us in a
    /// way that makes a restore harmful (the user switched somewhere else themselves, the origin
    /// application quit, our own window came forward).
    Forget,
}

/// How the peek got here: the card it is armed on, the card it has actually brought forward, and
/// why the last arm was refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mod {
    armed: Option<PeekTarget>,
    peeked: Option<PeekTarget>,
    /// The card a peek was abandoned on (the user switched somewhere themselves, or a raise was
    /// cancelled). Nothing may arm there until the pointer has left that card: without this the
    /// peek would restart on the very next tick and take the front back from the application the
    /// user just picked.
    hold_until_leave: Option<PeekTarget>,
}

impl Mod {
    /// A peek that is holding nothing: the state of a fresh panel session.
    pub(crate) const fn new() -> Self {
        Self {
            armed: None,
            peeked: None,
            hold_until_leave: None,
        }
    }

    pub(crate) fn armed(&self) -> Option<PeekTarget> {
        self.armed
    }

    pub(crate) fn peeked(&self) -> Option<PeekTarget> {
        self.peeked
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.armed.is_none() && self.peeked.is_none() && self.hold_until_leave.is_none()
    }

    /// The card a cancelled peek is holding closed, if any.
    pub(crate) fn held(&self) -> Option<PeekTarget> {
        self.hold_until_leave
    }

    /// Record the arm the caller scheduled. Only a resting pointer may arm, so the instant is the
    /// caller's business (`now + PEEK_DELAY`); this keeps the card.
    pub(crate) fn arm(&mut self, target: PeekTarget) {
        self.armed = Some(target);
    }

    /// Drop an arm without touching a peek that is already up.
    pub(crate) fn disarm(&mut self) {
        self.armed = None;
    }

    /// The peek request for `target` succeeded: the application it belongs to is now in front.
    pub(crate) fn commit(&mut self, target: PeekTarget) {
        self.armed = None;
        self.hold_until_leave = None;
        self.peeked = Some(target);
    }

    /// Everything the peek was holding goes away. Called after the restore was issued, after a
    /// `Forget`, and from every teardown path (feature stopped, panel hidden, app terminated).
    pub(crate) fn clear(&mut self) {
        self.armed = None;
        self.peeked = None;
        self.hold_until_leave = None;
    }

    /// Remember that a peek ended on `target` for a reason the user owns: that card stays quiet
    /// until the pointer leaves it.
    pub(crate) fn hold(&mut self, target: PeekTarget) {
        self.armed = None;
        self.peeked = None;
        self.hold_until_leave = Some(target);
    }

    /// The pointer is somewhere else now, so a hold no longer describes it.
    pub(crate) fn release_hold(&mut self) {
        self.hold_until_leave = None;
    }

    /// Forget the peek without restoring: the `ClearReason` below says why that is the right answer.
    pub(crate) fn forget(&mut self) -> Option<PeekTarget> {
        let target = self.peeked.take();
        self.armed = None;
        target
    }
}

/// The card a pointer zone names, if it is one a peek can be armed on. A peek only ever starts (or
/// re-targets) from a card's BODY: the title box owns the tooltip and the close slot owns the close.
pub(crate) fn card_of(zone: PointerZone) -> Option<usize> {
    match zone {
        PointerZone::Card(index) => Some(index),
        PointerZone::Title(_) | PointerZone::Close(_) | PointerZone::Panel => None,
        PointerZone::Transition | PointerZone::Anchor | PointerZone::Outside => None,
    }
}

/// Whether the pointer is somewhere inside the preview panel.
///
/// This is the line a peek ends on. It used to be "the pointer left its card", which made sliding
/// from one thumbnail to the next give the front back to the application the user came from and then
/// take it again -- a visible flash of that application between two thumbnails of the same app.
pub(crate) fn inside_panel(zone: PointerZone) -> bool {
    matches!(
        zone,
        PointerZone::Card(_) | PointerZone::Title(_) | PointerZone::Close(_) | PointerZone::Panel
    )
}

/// Everything the decision reads. One struct so the caller cannot pass the fields in a different
/// order than it prints them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PeekInput {
    /// `[dock_preview] enabled` and `[dock_preview] peek_on_hover`.
    pub enabled: bool,
    /// Where the pointer is in the panel's own terms.
    pub zone: PointerZone,
    /// The card under the pointer, if any.
    pub hovered: Option<PeekTarget>,
    /// Whether the hovered card can be peeked at all (a window that exists and may be shown).
    pub hovered_previewable: Option<PeekRefusal>,
    /// The frontmost application, and the card that already belongs to it.
    pub frontmost: Frontmost,
    /// Whether a Dock press is still waiting to be answered.
    pub press_pending: bool,
    /// The arming deadline, if one was scheduled. The caller owns it because it is the only clock in
    /// the interaction, and it is passed in so every test controls time explicitly.
    pub armed_at: Option<Instant>,
    /// The current time.
    pub now: Instant,
    /// Whether the peek it describes is armed on this very card. A pointer that slides from one card
    /// to another must arm the second one; a pointer that stays on one card must not push that
    /// card's deadline forward on every tick.
    pub armed_on_this_card: bool,
}

/// What the front application's presence means to the peek.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Frontmost {
    /// The front application is one of ours: a peek has nothing to give back to.
    Own,
    /// The front application is a normal one; `focused_window` is its focused window when known.
    Other {
        pid: i32,
        focused_window: Option<u32>,
    },
    /// The front application could not be read. A peek is not armed on an unreadable baseline: its
    /// restore would not know what to give the front back to.
    Unknown,
}

/// The one decision the tick makes about the peek.
///
/// Order matters and is the whole design:
///
/// 1. a peek that is already up outranks an arm, so leaving a card restores before another card can
///    arm -- A -> C must never happen without the walk back through the application A was on;
/// 2. turning the feature off restores instead of forgetting, so the switch cannot strand a window
///    in front with the preview gone;
/// 3. every refusal clears the arm, so a refused card cannot keep a deadline that the next tick
///    would honour after the pointer moved on;
/// 4. `AlreadyFrontmost` is a refusal, not a restore: the card belongs to the application that is
///    already in front, so the baseline the peek would hold is the peek itself.
pub(crate) fn peek_decision(mod_: &Mod, input: &PeekInput) -> PeekDecision {
    let card = card_of(input.zone);
    let on_body = card.is_some() && input.hovered.is_some();

    // 1. Something is up.
    if let Some(peeked) = mod_.peeked() {
        if !input.enabled {
            return PeekDecision::Restore;
        }
        // Leaving the PANEL is what ends a peek. Inside it, moving between cards, onto a title, into
        // the padding or across the gap between two cards keeps the peek up: the front must not walk
        // back to the origin application on the way from one thumbnail to the next.
        if !inside_panel(input.zone) {
            return PeekDecision::Restore;
        }
        // Still on the card that is up: hold.
        if input.hovered == Some(peeked) {
            return PeekDecision::Idle;
        }
        // A different card's body: arm the switch to it, with the same dwell as an initial peek so a
        // fast slide across a row does not machine-gun activations.
        if let (true, Some(target)) = (on_body, input.hovered) {
            if target != peeked {
                if input.armed_on_this_card {
                    return match input.armed_at {
                        Some(at) if input.now >= at => PeekDecision::Retarget { target },
                        _ => PeekDecision::Idle,
                    };
                }
                return PeekDecision::Arm {
                    target,
                    at: input.now + PEEK_DELAY,
                };
            }
        }
        // The panel's padding, a title box, a close slot, or a card being closed: hold.
        return PeekDecision::Idle;
    }

    // 2. Nothing is up. A refusal ends the arm.
    let refusal = if !input.enabled {
        Some(PeekRefusal::Disabled)
    } else if mod_.held().is_some() && mod_.held() == input.hovered {
        // The pointer has not left the card whose peek the user ended: nothing arms there again.
        Some(PeekRefusal::LeftDuringRaise)
    } else if !on_body {
        Some(PeekRefusal::NotOnCardBody)
    } else if input.press_pending {
        Some(PeekRefusal::PressPending)
    } else if matches!(input.frontmost, Frontmost::Own) {
        Some(PeekRefusal::AlreadyFrontmost)
    } else if input.frontmost == Frontmost::Unknown {
        // An unreadable baseline is a refusal for the same reason it is not a `Frontmost`: there is
        // nothing to give the front back to once the peek ends.
        Some(PeekRefusal::NoWindow)
    } else {
        input.hovered_previewable
    };
    if let Some(_refusal) = refusal {
        return match mod_.armed() {
            Some(_) => PeekDecision::Forget,
            None => PeekDecision::Idle,
        };
    }
    let Some(target) = input.hovered else {
        return PeekDecision::Idle;
    };
    // The card of the application that is already in front needs no activation at all.
    if let Frontmost::Other { pid, .. } = input.frontmost {
        if pid == target.pid {
            return match mod_.armed() {
                Some(_) => PeekDecision::Forget,
                None => PeekDecision::Idle,
            };
        }
    }

    // 3. A resting pointer: arm on the first tick, hold while the deadline runs, peek after it. The
    // deadline belongs to the card, so a pointer that stays calls `Arm` exactly once.
    if input.armed_on_this_card {
        return match input.armed_at {
            Some(at) if input.now >= at => PeekDecision::Peek { target },
            _ => PeekDecision::Idle,
        };
    }
    PeekDecision::Arm {
        target,
        at: input.now + PEEK_DELAY,
    }
}

/// The reason a peek that was up is being dropped without a restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClearReason {
    /// The user switched somewhere themselves (another application, the switcher, a click on a
    /// window): giving the front back to the origin would undo their own choice.
    UserSwitched,
    /// The front belongs to this application: our own panel or settings window came forward.
    OwnFront,
    /// The application the peek took the front from quit, so the restore target is gone.
    OriginGone,
    /// The peeked window or its application disappeared, or its card became a window the peek may
    /// not show (minimized, hidden, closed, moved to another desktop).
    TargetGone,
}

/// What to do with a peek that is up this tick, given what the system looks like now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeekWatch {
    /// Still ours: keep it up. The pointer ends a live peek, never this watch.
    Keep,
    /// Drop it without restoring: the user went somewhere else, one of our own windows came
    /// forward, or either end of the peek quit.
    Forget(ClearReason),
}

/// Whether the peek that is up still describes the screen.
///
/// `front` is read on the main thread right before this decision. The restore is only ever issued
/// while the peeked application is still in front: a front that changed to a third application
/// means the user switched themselves, and a front that is ours means one of our own windows came
/// forward. Both make the restore wrong, and the rule is kept here rather than at the call site so
/// a new teardown path cannot forget it.
pub(crate) fn watch_peek(
    peeked: PeekTarget,
    origin: Option<i32>,
    origin_running: bool,
    target_running: bool,
    front: Frontmost,
) -> PeekWatch {
    if !target_running {
        return PeekWatch::Forget(ClearReason::TargetGone);
    }
    match front {
        Frontmost::Own => PeekWatch::Forget(ClearReason::OwnFront),
        Frontmost::Unknown => PeekWatch::Keep,
        Frontmost::Other { pid, .. } => {
            if pid == peeked.pid {
                // The peek is up and holding the front: the pointer, not the watch, decides when it
                // ends. The exception is a peek that could no longer be given back: no origin, or an
                // origin that quit, is dropped instead of restored.
                match origin {
                    Some(_) if origin_running => PeekWatch::Keep,
                    _ => PeekWatch::Forget(ClearReason::OriginGone),
                }
            } else if Some(pid) == origin {
                // The front is where the peek took it from: the restore already landed, or the
                // peek never actually took the front.
                PeekWatch::Keep
            } else {
                PeekWatch::Forget(ClearReason::UserSwitched)
            }
        }
    }
}

/// A peek the pointer left is a pending restore that has not happened yet (the front could not be
/// read, or the Dock's own activation is still arriving). It is retried for a short while and then
/// abandoned, because a restore that never runs leaves the peeked window in front, while a restore
/// that runs after the user has moved on is invisible and destructive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestoreStep {
    /// The front is where the restore expects it: perform it now.
    Proceed,
    /// The front cannot be read yet: try again on the next tick.
    Retry,
    /// Give up: nothing describes the peek any more.
    Abandon,
}

/// How long a pending restore may be retried.
pub(crate) const RESTORE_RETRY_WINDOW: Duration = Duration::from_millis(600);

/// What the front is right now, as the restore reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrontNow {
    pub pid: i32,
    /// The application's focused window, when it could be read. This is what tells "our peek still
    /// holds the front" from "the user picked another window of that application".
    pub focused_window: Option<u32>,
}

/// Whether a restore is still worth attempting.
///
/// The front has to be the application the peek raised, AND -- when the focused window is readable
/// -- the very window the peek raised. The second half is the one a pid comparison cannot give: the
/// user can switch to another window of the previewed application themselves, and restoring then
/// would take them back to where the peek took the front from, undoing their own choice.
///
/// A front that is already the origin means nothing is owed: either the restore landed or the user
/// came back themselves. A third application is the user's own switch. An unreadable front is no
/// evidence at all, so the request survives for the rest of its window.
pub(crate) fn restore_step(
    requested_at: Instant,
    now: Instant,
    peeked: PeekTarget,
    origin: i32,
    front: Option<FrontNow>,
) -> RestoreStep {
    if now.duration_since(requested_at) > RESTORE_RETRY_WINDOW {
        return RestoreStep::Abandon;
    }
    match front {
        Some(FrontNow { pid, .. }) if pid == origin => RestoreStep::Abandon,
        Some(FrontNow {
            pid,
            focused_window,
        }) if pid == peeked.pid => match focused_window {
            // The peeked window is still the focused one: this restore undoes our own switch.
            Some(window) if window == peeked.wid => RestoreStep::Proceed,
            // Another window of the previewed application has the focus: the user moved within it.
            Some(_) => RestoreStep::Abandon,
            // The focused window could not be read: the application is still the one we raised, and
            // the request keeps its chance for the rest of its window.
            None => RestoreStep::Retry,
        },
        Some(_) => RestoreStep::Abandon,
        None => RestoreStep::Retry,
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;

    fn target() -> PeekTarget {
        PeekTarget { pid: 10, wid: 11 }
    }

    /// One clock reading for both instants: two `Instant::now()` calls can straddle the boundary the
    /// test is about, which made the pre-deadline case flaky under scheduler delay.
    fn step(age_ms: u64, origin: i32, front: Option<FrontNow>) -> RestoreStep {
        let requested_at = Instant::now();
        restore_step(
            requested_at,
            requested_at + Duration::from_millis(age_ms),
            target(),
            origin,
            front,
        )
    }

    fn front(pid: i32, focused: Option<u32>) -> FrontNow {
        FrontNow {
            pid,
            focused_window: focused,
        }
    }

    /// The front that lets a restore undo our own switch, and the two that must not.
    #[test]
    fn only_the_peeked_window_holding_the_front_lets_a_restore_run() {
        assert_eq!(
            step(10, 20, Some(front(10, Some(11)))),
            RestoreStep::Proceed
        );
        assert_eq!(
            step(10, 20, Some(front(10, Some(99)))),
            RestoreStep::Abandon,
            "the user moved to another window of the previewed application"
        );
        assert_eq!(
            step(10, 20, Some(front(20, None))),
            RestoreStep::Abandon,
            "already back"
        );
        assert_eq!(
            step(10, 20, Some(front(30, None))),
            RestoreStep::Abandon,
            "user switched"
        );
        assert_eq!(step(10, 20, None), RestoreStep::Retry, "unreadable front");
        assert_eq!(
            step(10, 20, Some(front(10, None))),
            RestoreStep::Retry,
            "the right application, but its focused window could not be read"
        );
    }

    /// A restore cannot wait forever: after the window it is abandoned, and an unreadable front
    /// inside the window is still retried. Both halves together, so the timeout cannot be met by a
    /// rule that ignores it.
    #[test]
    fn a_restore_is_bounded_and_an_unreadable_front_is_retried_inside_it() {
        let just_inside = RESTORE_RETRY_WINDOW.as_millis() as u64 - 1;
        let just_past = RESTORE_RETRY_WINDOW.as_millis() as u64 + 1;
        assert_eq!(step(just_inside, 20, None), RestoreStep::Retry);
        assert_eq!(step(just_past, 20, None), RestoreStep::Abandon);
        assert_eq!(
            step(just_past, 20, Some(front(10, Some(11)))),
            RestoreStep::Abandon,
            "past the window even the peeked application is not restored"
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    fn target(wid: u32) -> PeekTarget {
        PeekTarget { pid: 10, wid }
    }

    fn input(zone: PointerZone, hovered: Option<PeekTarget>, now: Instant) -> PeekInput {
        PeekInput {
            enabled: true,
            zone,
            hovered,
            hovered_previewable: None,
            frontmost: Frontmost::Other {
                pid: 20,
                focused_window: Some(200),
            },
            press_pending: false,
            armed_at: None,
            now,
            armed_on_this_card: false,
        }
    }

    /// A resting pointer arms on the first tick, holds through the delay, and peeks when it passes.
    #[test]
    fn a_resting_pointer_arms_holds_then_peeks() {
        let now = at(0);
        let mut m = Mod::default();
        let first = peek_decision(&m, &input(PointerZone::Card(0), Some(target(1)), now));
        let PeekDecision::Arm {
            target: t,
            at: deadline,
        } = first
        else {
            panic!("expected an arm, got {first:?}");
        };
        assert_eq!(t, target(1));
        assert_eq!(deadline, now + PEEK_DELAY);
        m.arm(t);

        // Before the deadline with the same card: hold, and the deadline is not pushed forward.
        assert_eq!(
            peek_decision(
                &m,
                &PeekInput {
                    armed_at: Some(deadline),
                    now: now + PEEK_DELAY / 2,
                    armed_on_this_card: true,
                    ..input(PointerZone::Card(0), Some(target(1)), now)
                }
            ),
            PeekDecision::Idle
        );
        // At the deadline: peek.
        assert_eq!(
            peek_decision(
                &m,
                &PeekInput {
                    armed_at: Some(deadline),
                    now: deadline,
                    armed_on_this_card: true,
                    ..input(PointerZone::Card(0), Some(target(1)), now)
                }
            ),
            PeekDecision::Peek { target: target(1) }
        );
    }

    /// A second arm for a different card starts its own clock instead of inheriting the first
    /// card's deadline: sliding across a panel would otherwise peek whichever card came first.
    #[test]
    fn a_new_card_arms_its_own_deadline() {
        let now = at(0);
        let mut m = Mod::default();
        m.arm(target(1));
        let decision = peek_decision(
            &m,
            &PeekInput {
                armed_at: Some(now + PEEK_DELAY),
                now: now + PEEK_DELAY,
                armed_on_this_card: false,
                ..input(PointerZone::Card(1), Some(target(2)), now)
            },
        );
        assert_eq!(
            decision,
            PeekDecision::Arm {
                target: target(2),
                at: now + PEEK_DELAY * 2,
            }
        );
    }

    /// Every rule that must never activate anything, each asserted on its own so a rule removed
    /// from the chain fails a named case rather than the whole set.
    #[test]
    fn the_non_activation_rules_each_refuse_alone() {
        let now = at(0);
        let m = Mod::default();
        let cases: [(PeekInput, &str); 8] = [
            (
                PeekInput {
                    enabled: false,
                    ..input(PointerZone::Card(0), Some(target(1)), now)
                },
                "the feature is off",
            ),
            (
                input(PointerZone::Title(0), Some(target(1)), now),
                "title box",
            ),
            (
                input(PointerZone::Close(0), Some(target(1)), now),
                "close slot",
            ),
            (input(PointerZone::Panel, None, now), "panel padding"),
            (input(PointerZone::Outside, None, now), "outside"),
            (
                PeekInput {
                    press_pending: true,
                    ..input(PointerZone::Card(0), Some(target(1)), now)
                },
                "a Dock press is waiting",
            ),
            (
                PeekInput {
                    frontmost: Frontmost::Own,
                    ..input(PointerZone::Card(0), Some(target(1)), now)
                },
                "our own window is in front",
            ),
            (
                PeekInput {
                    hovered_previewable: Some(PeekRefusal::Minimized),
                    ..input(PointerZone::Card(0), Some(target(1)), now)
                },
                "a minimized window",
            ),
        ];
        for (case, why) in cases {
            assert_eq!(peek_decision(&m, &case), PeekDecision::Idle, "{why}");
        }
    }

    /// The card of the application that is already in front is refused: peeking it would spend a
    /// real activation on a switch that changes nothing.
    #[test]
    fn a_card_of_the_front_application_is_not_peeked() {
        let now = at(0);
        let decision = peek_decision(
            &Mod::default(),
            &PeekInput {
                frontmost: Frontmost::Other {
                    pid: 10,
                    focused_window: Some(9),
                },
                ..input(PointerZone::Card(0), Some(target(1)), now)
            },
        );
        assert_eq!(decision, PeekDecision::Idle);
    }

    /// An unreadable baseline never arms: the restore would have no application to give the front
    /// back to.
    #[test]
    fn an_unreadable_baseline_never_arms() {
        let now = at(0);
        let decision = peek_decision(
            &Mod::default(),
            &PeekInput {
                frontmost: Frontmost::Unknown,
                ..input(PointerZone::Card(0), Some(target(1)), now)
            },
        );
        assert_eq!(decision, PeekDecision::Idle);
    }

    /// The rule the user's own testing changed: while the peek is up, moving between cards INSIDE the
    /// panel must not give the front back first. Restoring there flashed the application the user came
    /// from between two thumbnails of the same app.
    #[test]
    fn moving_between_cards_retargets_without_restoring() {
        let now = at(0);
        let mut m = Mod::default();
        m.commit(target(1));
        assert!(
            !m.is_idle(),
            "the peek must be recorded before the rule is read"
        );
        // Same card: the peek stands.
        assert_eq!(
            peek_decision(&m, &input(PointerZone::Card(0), Some(target(1)), now)),
            PeekDecision::Idle
        );
        // A different card's body: arm the switch to it, still no restore.
        assert_eq!(
            peek_decision(&m, &input(PointerZone::Card(1), Some(target(2)), now)),
            PeekDecision::Arm {
                target: target(2),
                at: now + PEEK_DELAY,
            },
            "moving to another card must arm a switch, not restore"
        );
        // Same card, deadline reached: the peek moves to the other window in place.
        assert_eq!(
            peek_decision(
                &m,
                &PeekInput {
                    armed_at: Some(now + PEEK_DELAY),
                    now: now + PEEK_DELAY,
                    armed_on_this_card: true,
                    ..input(PointerZone::Card(1), Some(target(2)), now)
                }
            ),
            PeekDecision::Retarget { target: target(2) }
        );
        // The panel's padding and a card's title/close slot hold the peek: they are inside the panel.
        for zone in [
            PointerZone::Panel,
            PointerZone::Title(1),
            PointerZone::Close(0),
        ] {
            assert_eq!(
                peek_decision(&m, &input(zone, None, now)),
                PeekDecision::Idle,
                "{zone:?} is inside the panel and must hold the peek"
            );
        }
    }

    /// Leaving the panel is what ends a peek: the crossing band, the Dock tile the panel is anchored
    /// to, and the outside all restore.
    #[test]
    fn leaving_the_panel_restores() {
        let now = at(0);
        let mut m = Mod::default();
        m.commit(target(1));
        for zone in [
            PointerZone::Transition,
            PointerZone::Anchor,
            PointerZone::Outside,
        ] {
            assert_eq!(
                peek_decision(&m, &input(zone, None, now)),
                PeekDecision::Restore,
                "{zone:?} must restore"
            );
        }
    }

    /// A peek the user ended leaves the card under the pointer quiet until the pointer leaves it.
    /// Without this the next tick armed the same card again and took the front back from the
    /// application the user had just chosen.
    #[test]
    fn a_cancelled_peek_does_not_restart_while_the_pointer_stays_on_its_card() {
        let now = at(0);
        let mut m = Mod::default();
        m.commit(target(1));
        m.hold(target(1));
        assert!(m.peeked().is_none(), "a hold holds nothing up");
        assert_eq!(
            peek_decision(&m, &input(PointerZone::Card(0), Some(target(1)), now)),
            PeekDecision::Idle
        );
        assert_eq!(
            peek_decision(
                &m,
                &input(PointerZone::Card(0), Some(target(1)), now + PEEK_DELAY)
            ),
            PeekDecision::Idle
        );
        // A different card is free to arm: the hold is keyed on one card, not on the panel.
        assert!(matches!(
            peek_decision(&m, &input(PointerZone::Card(1), Some(target(2)), now)),
            PeekDecision::Arm { target: t, .. } if t == target(2)
        ));
        // Once the pointer has left, the held card is free again.
        m.release_hold();
        assert!(matches!(
            peek_decision(&m, &input(PointerZone::Card(0), Some(target(1)), now)),
            PeekDecision::Arm { target: t, .. } if t == target(1)
        ));
    }

    /// A hold counts as state: the panel's release path must not treat a held card as idle and skip
    /// clearing it, which would leave the card quiet for the whole session.
    #[test]
    fn a_hold_is_not_idle() {
        let mut m = Mod::default();
        m.hold(target(1));
        assert!(!m.is_idle());
        m.release_hold();
        assert!(m.is_idle());
    }

    /// Turning the feature off restores rather than forgets: the switch must not strand a window in
    /// front with the preview gone.
    #[test]
    fn disabling_restores_instead_of_forgetting() {
        let now = at(0);
        let mut m = Mod::default();
        m.commit(target(1));
        assert!(!m.is_idle());
        let decision = peek_decision(
            &m,
            &PeekInput {
                enabled: false,
                ..input(PointerZone::Card(0), Some(target(1)), now)
            },
        );
        assert_eq!(decision, PeekDecision::Restore);
    }

    /// A refusal after an arm drops the arm, so the next tick cannot honour a stale deadline after
    /// the pointer left the card.
    #[test]
    fn a_refusal_after_an_arm_drops_the_arm() {
        let now = at(0);
        let mut m = Mod::default();
        m.arm(target(1));
        let decision = peek_decision(
            &m,
            &PeekInput {
                armed_at: Some(now + PEEK_DELAY),
                now: now + PEEK_DELAY,
                armed_on_this_card: true,
                ..input(PointerZone::Outside, None, now)
            },
        );
        assert_eq!(decision, PeekDecision::Forget);
    }

    /// The watch never restores over the user's own switch. Each way a peek can stop describing the
    /// screen is asserted on its own, and the two that must NOT restore come first.
    #[test]
    fn the_watch_never_restores_over_the_users_own_switch() {
        let peeked = target(1);
        let other = Frontmost::Other {
            pid: 30,
            focused_window: Some(300),
        };
        assert_eq!(
            watch_peek(peeked, Some(20), true, true, other),
            PeekWatch::Forget(ClearReason::UserSwitched)
        );
        assert_eq!(
            watch_peek(peeked, Some(20), true, true, Frontmost::Own),
            PeekWatch::Forget(ClearReason::OwnFront)
        );
        assert_eq!(
            watch_peek(
                peeked,
                Some(20),
                false,
                true,
                Frontmost::Other {
                    pid: 10,
                    focused_window: None
                }
            ),
            PeekWatch::Forget(ClearReason::OriginGone)
        );
        assert_eq!(
            watch_peek(
                peeked,
                Some(20),
                true,
                false,
                Frontmost::Other {
                    pid: 10,
                    focused_window: None
                }
            ),
            PeekWatch::Forget(ClearReason::TargetGone)
        );
        // The peeked application still in front with its origin alive: the peek HOLDS. The pointer,
        // not the watch, decides when a live peek ends; conflating the two restored a successful
        // peek on the very next tick.
        assert_eq!(
            watch_peek(
                peeked,
                Some(20),
                true,
                true,
                Frontmost::Other {
                    pid: 10,
                    focused_window: None
                }
            ),
            PeekWatch::Keep
        );
        // An unreadable front keeps the state: the next tick reads it again.
        assert_eq!(
            watch_peek(peeked, Some(20), true, true, Frontmost::Unknown),
            PeekWatch::Keep
        );
    }

    /// A peek with no recorded origin never restores: there is nothing to give the front back to.
    #[test]
    fn a_peek_without_an_origin_is_forgotten() {
        assert_eq!(
            watch_peek(
                target(1),
                None,
                true,
                true,
                Frontmost::Other {
                    pid: 10,
                    focused_window: None
                }
            ),
            PeekWatch::Forget(ClearReason::OriginGone)
        );
    }

    /// The module's life cycle: commit clears the arm, clear drops everything, forget reports what
    /// was peeked so the caller can log the window it abandoned.
    #[test]
    fn the_life_cycle_keeps_the_two_cards_apart() {
        let mut m = Mod::default();
        assert!(m.is_idle());
        m.arm(target(1));
        assert_eq!(m.armed(), Some(target(1)));
        assert_eq!(m.peeked(), None);
        m.commit(target(1));
        assert_eq!(m.armed(), None);
        assert_eq!(m.peeked(), Some(target(1)));
        assert_eq!(m.forget(), Some(target(1)));
        assert!(m.is_idle());
        m.arm(target(2));
        m.clear();
        assert!(m.is_idle());
    }
}
