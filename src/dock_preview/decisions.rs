//! Pure decisions for the Dock preview. The AppKit/observer code reads these answers instead of
//! re-deriving them, so the timing and hand-off rules are assertable without a screen or a Dock.

use crate::dock_preview::layout::{PointerZone, Rect};
use std::time::{Duration, Instant};

/// How long the pointer must rest on a Dock tile before the panel opens. Short enough to feel
/// immediate, long enough that crossing the Dock on the way somewhere else does not flash panels.
pub(crate) const OPEN_DELAY: Duration = Duration::from_millis(200);
/// How long the panel survives after the pointer leaves it and the Dock tile, so a small overshoot
/// while moving towards a card does not dismiss it.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_millis(250);
/// How long the pointer must rest on a truncated title before its full text is shown.
pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(450);
/// How often the main-thread tick runs while a Dock interaction is in progress.
pub(crate) const TICK_INTERVAL: Duration = Duration::from_millis(50);
/// Give up filling thumbnails for a panel after this long; the cards stay with icon placeholders.
pub(crate) const THUMB_FILL_TIMEOUT: Duration = Duration::from_millis(2000);
/// Dock-preview capture requests allowed in flight at once. Chosen low on purpose: the shared
/// capture queue also serves the switcher, and a request paints the system recording indicator.
pub(crate) const MAX_CAPTURE_IN_FLIGHT: usize = 4;
/// How long a close request waits for the window to disappear before the card is kept.
pub(crate) const CLOSE_CONFIRM_TIMEOUT: Duration = Duration::from_millis(1500);

/// Whether a Dock tile hover should open (or replace) the panel.
///
/// `pointer_on_tile` is re-read from the real pointer right before the decision: an auto-hidden Dock
/// can move its tiles between the AX event and this check, and a stale rectangle must not open a
/// panel over nothing.
pub(crate) fn should_open(
    enabled: bool,
    dock_revealed: bool,
    pointer_on_tile: bool,
    app_has_windows: bool,
    other_panel_visible: bool,
) -> bool {
    enabled && dock_revealed && pointer_on_tile && app_has_windows && !other_panel_visible
}

/// Whether a hover on another tile replaces the panel's content or is ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwitchDecision {
    /// The same app is still hovered: keep the panel and its frames.
    Keep,
    /// A different app: rebuild the panel content under the new request epoch.
    Switch,
}

pub(crate) fn switch_decision(current_pid: Option<i32>, hovered_pid: i32) -> SwitchDecision {
    match current_pid {
        Some(pid) if pid == hovered_pid => SwitchDecision::Keep,
        _ => SwitchDecision::Switch,
    }
}

/// What the close timer should do, given where the real pointer is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseDecision {
    /// Pointer still over the panel, its cards, the crossing or the anchor tile: keep it open and
    /// cancel any pending close.
    Keep,
    /// Pointer is elsewhere: schedule a close at this instant.
    Schedule(Instant),
    /// The grace period has elapsed: hide now.
    HideNow,
}

pub(crate) fn close_decision(
    zone: PointerZone,
    now: Instant,
    scheduled: Option<Instant>,
) -> CloseDecision {
    match zone {
        PointerZone::Close(_)
        | PointerZone::Title(_)
        | PointerZone::Card(_)
        | PointerZone::Panel
        | PointerZone::Transition
        | PointerZone::Anchor => CloseDecision::Keep,
        PointerZone::Outside => match scheduled {
            Some(deadline) if now >= deadline => CloseDecision::HideNow,
            Some(_) => CloseDecision::Keep,
            None => CloseDecision::Schedule(now + CLOSE_GRACE),
        },
    }
}

/// Whether the real pointer is resting on the Dock but no longer on the tile the open panel
/// belongs to.
///
/// The observer's selected-tile notification is not enough on its own: hovering a Dock tile whose
/// application is not running can leave the previously selected tile in place, and the panel would
/// then keep describing an app the pointer has already left. This check is geometric and uses the
/// pointer the tick already reads; a pointer above the Dock (crossing towards the cards) is not
/// "on another tile", so the panel survives that transit.
pub(crate) fn pointer_is_on_other_dock_tile(
    dock_list: Rect,
    anchor: Rect,
    pointer: (f64, f64),
) -> bool {
    dock_list.contains(pointer) && !anchor.contains(pointer)
}

/// What the title-hover tooltip should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TooltipDecision {
    Show,
    Hide,
}

pub(crate) fn tooltip_decision(
    zone: PointerZone,
    truncated_card: Option<usize>,
    hovered_since: Option<Instant>,
    now: Instant,
) -> TooltipDecision {
    let Some(card) = truncated_card else {
        return TooltipDecision::Hide;
    };
    let on_that_title = matches!(zone, PointerZone::Title(index) if index == card);
    if !on_that_title {
        return TooltipDecision::Hide;
    }
    match hovered_since {
        Some(since) if now.saturating_duration_since(since) >= TOOLTIP_DELAY => {
            TooltipDecision::Show
        }
        _ => TooltipDecision::Hide,
    }
}

/// The result of asking the system whether a window still exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowPresence {
    Gone,
    Present,
    /// The query itself failed: not evidence that the window is gone.
    Unknown,
}

/// What a close request does with its probe result. A card is removed only on `Gone`; a window that
/// shows a save dialog, or a failed query, keeps its card and re-enables the button after the
/// bounded wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseConfirmation {
    Remove,
    Keep,
    Wait,
}

pub(crate) fn close_confirmation(presence: WindowPresence, elapsed: Duration) -> CloseConfirmation {
    match presence {
        WindowPresence::Gone => CloseConfirmation::Remove,
        WindowPresence::Present | WindowPresence::Unknown => {
            if elapsed >= CLOSE_CONFIRM_TIMEOUT {
                CloseConfirmation::Keep
            } else {
                CloseConfirmation::Wait
            }
        }
    }
}

/// Whether another card may ask the capture queue for a frame: a small in-flight budget, and never
/// more requests than the panel has cards.
pub(crate) fn may_request_capture(in_flight: usize, cards_awaiting: usize) -> bool {
    in_flight < MAX_CAPTURE_IN_FLIGHT && cards_awaiting > 0
}

/// Observer events carry the session they were produced under; anything older than the current
/// session belongs to a retired observer and must not reach the panel.
pub(crate) fn event_is_current(event_session: u64, current_session: u64) -> bool {
    event_session == current_session
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn a_hover_opens_only_with_a_revealed_dock_and_the_pointer_on_the_tile() {
        assert!(should_open(true, true, true, true, false));
        assert!(
            !should_open(true, true, true, true, true),
            "another panel owns the screen"
        );
        assert!(!should_open(false, true, true, true, false), "feature off");
        assert!(!should_open(true, false, true, true, false), "dock hidden");
        assert!(
            !should_open(true, true, false, true, false),
            "pointer left the tile"
        );
        assert!(
            !should_open(true, true, true, false, false),
            "app has no windows"
        );
    }

    #[test]
    fn hovering_the_same_tile_keeps_the_panel_and_another_tile_switches() {
        assert_eq!(switch_decision(Some(7), 7), SwitchDecision::Keep);
        assert_eq!(switch_decision(Some(7), 8), SwitchDecision::Switch);
        assert_eq!(switch_decision(None, 7), SwitchDecision::Switch);
    }

    #[test]
    fn the_panel_survives_the_crossing_and_closes_after_the_grace_period() {
        let now = at(0);
        for zone in [
            PointerZone::Panel,
            PointerZone::Transition,
            PointerZone::Anchor,
            PointerZone::Card(0),
            PointerZone::Title(0),
            PointerZone::Close(0),
        ] {
            assert_eq!(close_decision(zone, now, None), CloseDecision::Keep);
            assert_eq!(
                close_decision(zone, now, Some(now - CLOSE_GRACE)),
                CloseDecision::Keep,
                "a scheduled close is cancelled while the pointer stays inside"
            );
        }
        assert_eq!(
            close_decision(PointerZone::Outside, now, None),
            CloseDecision::Schedule(now + CLOSE_GRACE)
        );
        let scheduled = now + CLOSE_GRACE;
        assert_eq!(
            close_decision(PointerZone::Outside, now + CLOSE_GRACE / 2, Some(scheduled)),
            CloseDecision::Keep
        );
        assert_eq!(
            close_decision(PointerZone::Outside, now + CLOSE_GRACE, Some(scheduled)),
            CloseDecision::HideNow
        );
    }

    #[test]
    fn the_tooltip_needs_the_title_box_and_a_truncated_title() {
        let now = at(1000);
        let since = now - TOOLTIP_DELAY;
        assert_eq!(
            tooltip_decision(PointerZone::Title(0), Some(0), Some(since), now),
            TooltipDecision::Show
        );
        // Too early.
        assert_eq!(
            tooltip_decision(PointerZone::Title(0), Some(0), Some(now), now),
            TooltipDecision::Hide
        );
        // Not truncated.
        assert_eq!(
            tooltip_decision(PointerZone::Title(0), None, Some(since), now),
            TooltipDecision::Hide
        );
        // Over the thumbnail of the same card.
        assert_eq!(
            tooltip_decision(PointerZone::Card(0), Some(0), Some(since), now),
            TooltipDecision::Hide
        );
        // Over another card's title.
        assert_eq!(
            tooltip_decision(PointerZone::Title(1), Some(0), Some(since), now),
            TooltipDecision::Hide
        );
    }

    #[test]
    fn a_closing_card_is_removed_only_once_its_window_is_gone() {
        assert_eq!(
            close_confirmation(WindowPresence::Gone, Duration::ZERO),
            CloseConfirmation::Remove
        );
        assert_eq!(
            close_confirmation(WindowPresence::Present, Duration::from_millis(100)),
            CloseConfirmation::Wait
        );
        assert_eq!(
            close_confirmation(WindowPresence::Unknown, Duration::from_millis(100)),
            CloseConfirmation::Wait
        );
        assert_eq!(
            close_confirmation(WindowPresence::Present, CLOSE_CONFIRM_TIMEOUT),
            CloseConfirmation::Keep
        );
        assert_eq!(
            close_confirmation(WindowPresence::Unknown, CLOSE_CONFIRM_TIMEOUT),
            CloseConfirmation::Keep
        );
    }

    #[test]
    fn capture_requests_are_bounded() {
        assert!(may_request_capture(0, 1));
        assert!(may_request_capture(MAX_CAPTURE_IN_FLIGHT - 1, 1));
        assert!(!may_request_capture(MAX_CAPTURE_IN_FLIGHT, 1));
        assert!(!may_request_capture(0, 0));
    }

    #[test]
    fn a_retired_observer_session_cannot_post_events() {
        assert!(event_is_current(3, 3));
        assert!(!event_is_current(2, 3));
    }
}

#[cfg(test)]
mod dock_tile_tests {
    use super::*;

    fn list() -> Rect {
        Rect::new(37.0, 890.0, 1396.0, 56.0)
    }

    fn anchor() -> Rect {
        Rect::new(268.0, 894.0, 44.0, 56.0)
    }

    #[test]
    fn the_neighbouring_tile_ends_the_preview() {
        // One tile to the right: still inside the Dock's list, outside the hovered icon.
        assert!(pointer_is_on_other_dock_tile(
            list(),
            anchor(),
            (336.0, 922.0)
        ));
    }

    #[test]
    fn the_hovered_tile_and_the_crossing_keep_it() {
        assert!(!pointer_is_on_other_dock_tile(
            list(),
            anchor(),
            (290.0, 922.0)
        ));
        // Above the Dock, on the way to the cards: not "another tile".
        assert!(!pointer_is_on_other_dock_tile(
            list(),
            anchor(),
            (290.0, 850.0)
        ));
        // Somewhere else on the desktop.
        assert!(!pointer_is_on_other_dock_tile(
            list(),
            anchor(),
            (290.0, 400.0)
        ));
    }
}
