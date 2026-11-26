//! Pure decisions for the Dock preview. The AppKit/observer code reads these answers instead of
//! re-deriving them, so the timing and hand-off rules are assertable without a screen or a Dock.

use crate::dock_preview::layout::{PointerZone, Rect};
use std::time::{Duration, Instant};

/// How long the pointer must rest on a Dock tile before the panel opens. Short enough to feel
/// immediate, long enough that crossing the Dock on the way somewhere else does not flash panels.
pub(crate) const OPEN_DELAY: Duration = Duration::from_millis(200);
/// How long the panel survives while the pointer is still on the Dock but off the panel and the
/// anchor tile. The trip from the icon towards the cards is often diagonal and spends more than
/// one 50ms sample inside the Dock's list, so this crossing keeps the wide grace it always had.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_millis(250);
/// How long the panel survives once the pointer has really left -- off the panel, the crossing and
/// the Dock entirely. This is only a sampling safeguard, so one stray read cannot flash the panel
/// away; the close itself then starts almost at once instead of after the crossing's grace.
pub(crate) const CLOSE_PROTECT: Duration = Duration::from_millis(50);
/// The close is a fade, not a one-frame cut: once the protection or the grace decides, the panel
/// dissolves over this long. Windows takes 167-250ms; the shorter end keeps the snappy feel the
/// open delay already has. Under Reduce Motion the close stays instant.
pub(crate) const FADE_DURATION: Duration = Duration::from_millis(150);
/// How long the pointer must rest on a truncated title before its full text is shown.
pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(450);
/// How often the main-thread tick runs while a Dock interaction is in progress.
pub(crate) const TICK_INTERVAL: Duration = Duration::from_millis(50);
/// Give up filling thumbnails for a panel after this long; the cards stay with icon placeholders.
pub(crate) const THUMB_FILL_TIMEOUT: Duration = Duration::from_millis(2000);
/// Dock-preview capture requests allowed in flight at once. Chosen low on purpose: the shared
/// capture queue also serves the switcher, and a request paints the system recording indicator.
pub(crate) const MAX_CAPTURE_IN_FLIGHT: usize = 4;
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

/// Where the close of a visible panel stands. The two phases are distinct because the first is
/// cancellable protection while the second is already playing out: `Armed` carries the instant the
/// pointer first went outside (not a deadline, so the duration it is measured against can follow
/// where the pointer currently is), `Fading` the instant the dissolve began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosePhase {
    Armed(Instant),
    Fading(Instant),
}

/// What the close timer should do, given where the real pointer is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseDecision {
    /// Pointer still over the panel, its cards, the crossing or the anchor tile: keep it open and
    /// cancel any pending close, including a fade that already started.
    Keep,
    /// Pointer is elsewhere and a phase is already running: wait for it. This is distinct from
    /// `Keep` because the caller cancels on `Keep`; folding the two together made every tick
    /// cancel the deadline the previous tick had just scheduled, so the panel never closed after
    /// the pointer left.
    Wait,
    /// The pointer just went outside: arm the protection at this instant.
    Arm(Instant),
    /// The protection or the grace has elapsed: begin the fade.
    Fade,
    /// The fade has run its course: hide now.
    HideNow,
}

/// The close decision for one tick of a visible panel.
///
/// `on_dock` splits the two departures the old single grace lumped together. A pointer still on
/// the Dock's list is plausibly on its way to the cards -- the trip is often diagonal and the
/// panel's bottom edge sits on the icon's top edge, so the whole upward move happens inside the
/// Dock -- and keeps the wide [`CLOSE_GRACE`], which is also how a rest on a tile whose app is not
/// running still ends the preview (the Dock sends no selection change there). A pointer off the
/// Dock entirely has really left, and only gets [`CLOSE_PROTECT`], the one sampling period that
/// keeps a stray read from flashing the panel away. Either way the close itself is then a fade of
/// [`FADE_DURATION`], never a one-frame cut.
pub(crate) fn close_decision(
    zone: PointerZone,
    on_dock: bool,
    now: Instant,
    phase: Option<ClosePhase>,
) -> CloseDecision {
    if !matches!(zone, PointerZone::Outside) {
        return CloseDecision::Keep;
    }
    match phase {
        None => CloseDecision::Arm(now),
        Some(ClosePhase::Armed(since)) => {
            let allowance = if on_dock { CLOSE_GRACE } else { CLOSE_PROTECT };
            if now.saturating_duration_since(since) >= allowance {
                CloseDecision::Fade
            } else {
                CloseDecision::Wait
            }
        }
        Some(ClosePhase::Fading(started)) => {
            if now.saturating_duration_since(started) >= FADE_DURATION {
                CloseDecision::HideNow
            } else {
                CloseDecision::Wait
            }
        }
    }
}

/// Whether a press on the Dock must keep `tile`'s preview closed.
///
/// A press dismisses the preview, but the pointer stays on the icon it just pressed and the Dock
/// keeps reporting that tile as selected, so the notifications that follow would otherwise re-open
/// the preview the press closed. The latch names the app the tile belonged to: a notification for a
/// different app means the pointer has moved on, which ends the hover the press dismissed. The latch
/// is only true while the pointer is still on the pressed tile, and an unreadable pointer never
/// keeps the preview closed.
pub(crate) fn dismissed_tile_suppresses(
    latched: Option<(i32, Rect)>,
    pid: i32,
    pointer: Option<(f64, f64)>,
) -> bool {
    match (latched, pointer) {
        (Some((latched_pid, tile)), Some(point)) => latched_pid == pid && tile.contains(point),
        _ => false,
    }
}

/// A press as the decision functions see it: the feature session it belongs to, when it happened,
/// and where.
pub(crate) type PressAt = (u64, Instant, (f64, f64));

/// How long after a press the press itself still explains an open. The Dock's hover notification
/// for the tile that was pressed can reach the main thread in the same turn as the press, or just
/// after it, when there is no pending entry for the press to cancel yet.
pub(crate) const PRESS_OPEN_WINDOW: Duration = Duration::from_millis(500);

/// Whether a press that arrived while nothing was open explains this pending open: the press
/// happened on this tile, in this session, within the window, so the open is the notification for
/// that press and must be dropped instead of showing a preview the user just closed. The press is
/// one-shot: the caller consumes it, so a hover after the pointer has been away is not suppressed by
/// an older press.
pub(crate) fn press_suppresses_open(
    press: Option<PressAt>,
    session: u64,
    tile: Rect,
    now: Instant,
) -> bool {
    match press {
        Some((press_session, at, point)) => {
            press_session == session
                && now.duration_since(at) <= PRESS_OPEN_WINDOW
                && tile.contains(point)
        }
        None => false,
    }
}

/// One tick's pointer sample against the press/latch state. Pure so the release rules are asserted
/// without a Dock:
///
/// - the latch dies when the pointer leaves its tile;
/// - the press dies with that latch when it landed on that tile: the pointer has left the tile the
///   press belonged to, so the press can no longer explain an open, and keeping it would suppress
///   the next hover of the tile the pointer comes back to;
/// - the press also dies when the pointer is off the Dock's tiles entirely.
///
/// A press that landed on a different tile than the latched one survives (the pointer may still be
/// pressing it while an older panel is dismissed), which is why the rule is keyed on the latch's
/// tile rather than on the latch alone. `on_dock` is `None` when the Dock boundary is unknown or was
/// observed before the press: an auto-hidden Dock reveals after the press, so an absent or older
/// rectangle must not read as "the pointer left".
pub(crate) fn released_by_pointer(
    latched: Option<(i32, Rect)>,
    press: Option<PressAt>,
    pointer: (f64, f64),
    on_dock: Option<bool>,
) -> (Option<(i32, Rect)>, Option<PressAt>) {
    let latched_now = match latched {
        Some((_, tile)) if !tile.contains(pointer) => None,
        other => other,
    };
    let press_died_with_tile = match (latched, latched_now) {
        (Some((_, tile)), None) => press.is_some_and(|(_, _, point)| tile.contains(point)),
        _ => false,
    };
    let press_now = if press_died_with_tile || on_dock == Some(false) {
        None
    } else {
        press
    };
    (latched_now, press_now)
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
    fn the_panel_survives_the_crossing_and_fades_after_the_grace_period() {
        let now = at(0);
        for zone in [
            PointerZone::Panel,
            PointerZone::Transition,
            PointerZone::Anchor,
            PointerZone::Card(0),
            PointerZone::Title(0),
            PointerZone::Close(0),
        ] {
            assert_eq!(close_decision(zone, true, now, None), CloseDecision::Keep);
            assert_eq!(
                close_decision(zone, true, now, Some(ClosePhase::Fading(now))),
                CloseDecision::Keep,
                "a fade already running is cancelled while the pointer comes back inside"
            );
        }
        // The first outside sample only arms the protection.
        assert_eq!(
            close_decision(PointerZone::Outside, false, now, None),
            CloseDecision::Arm(now)
        );
        // Off the Dock entirely, the protection is the single sampling period; on the Dock the
        // crossing's grace still decides, measured from the same first sample.
        assert_eq!(
            close_decision(
                PointerZone::Outside,
                false,
                now + CLOSE_PROTECT,
                Some(ClosePhase::Armed(now))
            ),
            CloseDecision::Fade,
            "really gone: the fade starts right after the protection"
        );
        assert_eq!(
            close_decision(
                PointerZone::Outside,
                true,
                now + CLOSE_PROTECT,
                Some(ClosePhase::Armed(now))
            ),
            CloseDecision::Wait,
            "still on the Dock: the grace is much wider than the protection"
        );
        assert_eq!(
            close_decision(
                PointerZone::Outside,
                true,
                now + CLOSE_GRACE,
                Some(ClosePhase::Armed(now))
            ),
            CloseDecision::Fade
        );
        assert_eq!(
            close_decision(
                PointerZone::Outside,
                false,
                now + CLOSE_PROTECT / 2,
                Some(ClosePhase::Armed(now))
            ),
            CloseDecision::Wait,
            "inside the protection nothing is decided yet"
        );
        // The fade takes its time and only then hides.
        let fading = ClosePhase::Fading(now + CLOSE_PROTECT);
        assert_eq!(
            close_decision(
                PointerZone::Outside,
                false,
                now + CLOSE_PROTECT,
                Some(fading)
            ),
            CloseDecision::Wait
        );
        let done = now + CLOSE_PROTECT + FADE_DURATION;
        assert_eq!(
            close_decision(PointerZone::Outside, false, done, Some(fading)),
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

    fn anchor() -> Rect {
        Rect::new(268.0, 894.0, 44.0, 56.0)
    }

    #[test]
    fn a_press_latches_the_tile_until_the_pointer_leaves_it() {
        let tile = anchor();
        let latched = Some((7, tile));
        // The pointer is still on the icon the press closed.
        assert!(dismissed_tile_suppresses(latched, 7, Some((290.0, 922.0))));
        // The strip above the icon is outside the tile: it cannot carry a press, and an open there
        // is already rejected by the hover check.
        assert!(!dismissed_tile_suppresses(latched, 7, Some((290.0, 891.0))));
        // Off the tile: the next hover may open the preview again.
        assert!(!dismissed_tile_suppresses(latched, 7, Some((336.0, 922.0))));
        assert!(!dismissed_tile_suppresses(latched, 7, Some((290.0, 400.0))));
        // A notification for another app means the pointer moved on: the latch no longer applies.
        assert!(!dismissed_tile_suppresses(latched, 8, Some((290.0, 922.0))));
        // No press, or no readable pointer: nothing is suppressed.
        assert!(!dismissed_tile_suppresses(None, 7, Some((290.0, 922.0))));
        assert!(!dismissed_tile_suppresses(latched, 7, None));
    }

    #[test]
    fn a_press_before_the_hover_notification_still_keeps_the_tile_closed() {
        let tile = anchor();
        let now = Instant::now();
        let press = |session: u64, at: Instant, point: (f64, f64)| Some((session, at, point));
        // The press was on this tile, in this session, and just happened: the open is its
        // notification.
        assert!(press_suppresses_open(
            press(4, now, (290.0, 922.0)),
            4,
            tile,
            now
        ));
        assert!(press_suppresses_open(
            press(4, now - PRESS_OPEN_WINDOW, (290.0, 922.0)),
            4,
            tile,
            now
        ));
        // A press on another tile, a stale press, another session, or no press does not suppress.
        assert!(!press_suppresses_open(
            press(4, now, (336.0, 922.0)),
            4,
            tile,
            now
        ));
        assert!(!press_suppresses_open(
            press(
                4,
                now - PRESS_OPEN_WINDOW - Duration::from_millis(1),
                (290.0, 922.0)
            ),
            4,
            tile,
            now
        ));
        assert!(!press_suppresses_open(
            press(3, now, (290.0, 922.0)),
            4,
            tile,
            now
        ));
        assert!(!press_suppresses_open(None, 4, tile, now));
    }

    #[test]
    fn a_pointer_sample_releases_the_latch_and_the_press_it_belongs_to() {
        let tile = anchor();
        let now = Instant::now();
        let latched = Some((7, tile));
        let press = Some((4, now, (290.0, 922.0))); // inside the tile
                                                    // Still on the tile: nothing is released.
        assert_eq!(
            released_by_pointer(latched, press, (290.0, 922.0), Some(true)),
            (latched, press)
        );
        // Left the tile and still on the Dock: the latch and the press that landed on it are gone.
        assert_eq!(
            released_by_pointer(latched, press, (336.0, 922.0), Some(true)),
            (None, None)
        );
        // The press landed on another tile (a stale panel was dismissed): it survives the latch.
        let press_elsewhere = Some((4, now, (336.0, 922.0)));
        assert_eq!(
            released_by_pointer(latched, press_elsewhere, (336.0, 922.0), Some(true)),
            (None, press_elsewhere)
        );
        // Off the Dock's tiles entirely: the press cannot explain an open there.
        assert_eq!(
            released_by_pointer(latched, press, (290.0, 400.0), Some(false)),
            (None, None)
        );
        // An unknown or pre-press boundary says nothing about the pointer: a press with no latch
        // survives even off the Dock, where a trusted boundary would have released it.
        assert_eq!(
            released_by_pointer(None, press, (290.0, 400.0), None),
            (None, press)
        );
        // Nothing recorded: nothing to release.
        assert_eq!(
            released_by_pointer(None, None, (290.0, 400.0), Some(false)),
            (None, None)
        );
        // No latch, a press on the Dock: the press waits for the tile's own notification.
        assert_eq!(
            released_by_pointer(None, press, (290.0, 922.0), Some(true)),
            (None, press)
        );
    }
}
