//! Replaying a press that was not a gesture, so the application sees the click it expected.
//!
//! A press and its drag are swallowed the moment they arrive (see [[super::engine]]). When the
//! press turns out to be a plain click, or a gesture that matched nothing, a fresh down+up pair is
//! posted so the context menu or the click handler still runs.
//!
//! Two rules make the replay behave like the click it stands in for:
//!
//! * The multi-click count is **maintained here**, not copied from the physical
//!   \`kCGMouseEventClickState\`. A gesture press the application never saw must not count towards a
//!   double click: otherwise one short press right after a gesture arrives as \`clickState = 2\` and
//!   the application reads a double click out of a single click.
//! * Both events are built before either is posted. A failure while building releases both and posts
//!   nothing, so the application cannot receive a down without its up.

use crate::event_tap::CGPoint;
use crate::event_tap::{
    CFRelease, CGEventCreateMouseEvent, CGEventPost, CGEventSetIntegerValueField,
    CGEventSourceCreate, K_CG_EVENT_SOURCE_USER_DATA,
};

use super::recognizer::Point;

/// Marks a replayed click so this module's own tap does not treat it as a new press. Distinct from
/// [[crate::event_tap::SYNTHETIC_MARKER]] so diagnostics -- and a future tap that filters only its
/// own synthetic events -- can tell the two apart.
pub(crate) const REPLAY_MARKER: i64 = 0x4745_5354_5250; // "GESTRP"

/// kCGEventSourceStateHIDSystemState.
const HID_SYSTEM_STATE: i32 = 1;
/// kCGHIDEventTap.
const HID_EVENT_TAP: i32 = 0;
/// kCGMouseEventClickState.
const FIELD_CLICK_STATE: i32 = 1;
/// kCGMouseEventButtonNumber.
const FIELD_BUTTON_NUMBER: i32 = 3;

/// kCGEventMouseMoved is 5; the button drags and downs follow the same numbering as CGMouseButton.
const EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const EVENT_RIGHT_MOUSE_UP: u32 = 4;
const EVENT_OTHER_MOUSE_DOWN: u32 = 25;
const EVENT_OTHER_MOUSE_UP: u32 = 26;

/// The parts of a swallowed press a replay needs. Held by the tap thread; no Core Foundation object
/// is stored, so nothing here can leak or be used after the event that produced it is gone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PressData {
    /// CGEvent button number: 1 right, 2 middle, 3/4 side.
    pub(crate) button: u32,
    /// Where the press started, in global points (top-left origin, y growing downward).
    pub(crate) origin: Point,
    /// Modifier flags captured at press time; the release's flags win when the replay is posted.
    pub(crate) flags: u32,
}

/// Map a CGEvent button number to the down/up types and the CGMouseButton value to post.
///
/// Returns \`None\` for a button this module never claims (the left button), so a future caller cannot
/// accidentally replay a left click.
fn mouse_types(button: u32) -> Option<(u32, u32, i64)> {
    match button {
        1 => Some((EVENT_RIGHT_MOUSE_DOWN, EVENT_RIGHT_MOUSE_UP, 1)),
        2 => Some((EVENT_OTHER_MOUSE_DOWN, EVENT_OTHER_MOUSE_UP, 2)),
        3 | 4 => Some((EVENT_OTHER_MOUSE_DOWN, EVENT_OTHER_MOUSE_UP, button as i64)),
        _ => None,
    }
}

/// Build and post the pair. \`at\` is the release position: the replay lands where the pointer was
/// released, which is what the user's click would have done. \`click_state\` comes from [[ClickTracker]].
pub(crate) fn replay_click(press: &PressData, at: Point, click_state: i64, flags: u32) -> bool {
    let Some((down_type, up_type, cg_button)) = mouse_types(press.button) else {
        return false;
    };
    // Nothing is posted unless both events exist.
    // A null source is legal for CGEventCreateMouseEvent; only the release has to be skipped then.
    let source = unsafe { CGEventSourceCreate(HID_SYSTEM_STATE) };
    let down = unsafe {
        CGEventCreateMouseEvent(source, down_type, CGPoint { x: at.x, y: at.y }, cg_button)
    };
    let up = unsafe {
        CGEventCreateMouseEvent(source, up_type, CGPoint { x: at.x, y: at.y }, cg_button)
    };
    if down.is_null() || up.is_null() {
        // CFRelease does not accept null: releasing a pointer the constructor could not produce
        // would turn an allocation failure into a crash.
        unsafe {
            if !down.is_null() {
                CFRelease(down as *const _);
            }
            if !up.is_null() {
                CFRelease(up as *const _);
            }
            if !source.is_null() {
                CFRelease(source as *const _);
            }
        }
        return false;
    }
    unsafe {
        for event in [down, up] {
            CGEventSetIntegerValueField(event, FIELD_CLICK_STATE, click_state);
            CGEventSetIntegerValueField(event, FIELD_BUTTON_NUMBER, cg_button);
            CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, REPLAY_MARKER);
            crate::event_tap::CGEventSetFlags(event, flags as u64);
        }
        CGEventPost(HID_EVENT_TAP, down);
        CGEventPost(HID_EVENT_TAP, up);
        CFRelease(down as *const _);
        CFRelease(up as *const _);
        if !source.is_null() {
            CFRelease(source as *const _);
        }
    }
    true
}

/// The click count the application would see, maintained over *replayed* clicks only.
///
/// Kept per button and per pointer position: two clicks far apart, or on different buttons, never
/// chain into a double click. The interval is supplied by the caller from
/// \`NSEvent.doubleClickInterval\` so it follows the user's system setting, and the distance
/// tolerance is this project's own compatibility constant -- it is not the system's double-click
/// distance and is not claimed to be.
#[derive(Debug, Default)]
pub(crate) struct ClickTracker {
    last: [Option<(Point, f64, i64)>; 5],
}

impl ClickTracker {
    /// Distance within which two replayed clicks chain, in points. A project constant, not the
    /// system's double-click distance.
    pub(crate) const DISTANCE_TOLERANCE_PT: f64 = 5.0;

    /// The click count a replay at \`at\` would carry. Does **not** record it: a click that fails to
    /// reach the application must not chain into the next one, so the record is committed by
    /// [[record]] only after the events were actually built and posted.
    pub(crate) fn peek(&self, button: u32, at: Point, now_ms: f64, interval_ms: f64) -> i64 {
        let slot = (button as usize).min(4);
        match self.last[slot] {
            Some((previous, when, count))
                if now_ms - when <= interval_ms
                    && dist(previous, at) <= Self::DISTANCE_TOLERANCE_PT =>
            {
                count + 1
            }
            _ => 1,
        }
    }

    /// Commit a click the application actually received.
    pub(crate) fn record(&mut self, button: u32, at: Point, now_ms: f64, count: i64) {
        self.last[(button as usize).min(4)] = Some((at, now_ms, count));
    }

    /// Drop a remembered click without counting it: a gesture was recognized, or the press was
    /// cancelled because its authorization was withdrawn. Neither reached the application, so
    /// neither may chain into the next click.
    pub(crate) fn forget(&mut self, button: u32) {
        self.last[(button as usize).min(4)] = None;
    }
}

fn dist(a: Point, b: Point) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn two_nearby_clicks_chain() {
        let mut tracker = ClickTracker::default();
        let at = p(10.0, 10.0);
        let first = tracker.peek(1, at, 0.0, 500.0);
        assert_eq!(first, 1);
        tracker.record(1, at, 0.0, first);
        let second = tracker.peek(1, p(11.0, 10.0), 100.0, 500.0);
        assert_eq!(second, 2);
        tracker.record(1, p(11.0, 10.0), 100.0, second);
        assert_eq!(tracker.peek(1, p(12.0, 10.0), 200.0, 500.0), 3);
    }

    #[test]
    fn a_distant_click_starts_over() {
        let mut tracker = ClickTracker::default();
        let first = tracker.peek(1, p(10.0, 10.0), 0.0, 500.0);
        tracker.record(1, p(10.0, 10.0), 0.0, first);
        assert_eq!(tracker.peek(1, p(100.0, 10.0), 100.0, 500.0), 1);
    }

    #[test]
    fn a_slow_click_starts_over() {
        let mut tracker = ClickTracker::default();
        let first = tracker.peek(1, p(10.0, 10.0), 0.0, 500.0);
        tracker.record(1, p(10.0, 10.0), 0.0, first);
        assert_eq!(tracker.peek(1, p(10.0, 10.0), 900.0, 500.0), 1);
    }

    #[test]
    fn buttons_do_not_chain_into_each_other() {
        let mut tracker = ClickTracker::default();
        let first = tracker.peek(1, p(10.0, 10.0), 0.0, 500.0);
        tracker.record(1, p(10.0, 10.0), 0.0, first);
        assert_eq!(tracker.peek(2, p(10.0, 10.0), 50.0, 500.0), 1);
    }

    #[test]
    fn a_forgotten_click_does_not_count() {
        // The gesture case: the press never reached the application, so the next click is the first.
        let mut tracker = ClickTracker::default();
        let first = tracker.peek(1, p(10.0, 10.0), 0.0, 500.0);
        tracker.record(1, p(10.0, 10.0), 0.0, first);
        tracker.forget(1);
        let next = tracker.peek(1, p(10.0, 10.0), 100.0, 500.0);
        assert_eq!(next, 1);
        tracker.record(1, p(10.0, 10.0), 100.0, next);
        assert_eq!(tracker.peek(1, p(10.0, 10.0), 200.0, 500.0), 2);
    }

    #[test]
    fn an_unposted_click_does_not_chain() {
        // Review finding: the counter advanced before the events were built, so a failed replay
        // still made the next click look like a double click. peek() alone must not move it.
        let mut tracker = ClickTracker::default();
        let _ = tracker.peek(1, p(10.0, 10.0), 0.0, 500.0);
        assert_eq!(
            tracker.peek(1, p(10.0, 10.0), 100.0, 500.0),
            1,
            "nothing was recorded, so the next click is still the first"
        );
    }

    #[test]
    fn the_left_button_is_never_replayed() {
        assert!(mouse_types(0).is_none(), "button 0 is the left button");
        assert!(mouse_types(5).is_none(), "beyond the claimed range");
        assert_eq!(mouse_types(1), Some((3, 4, 1)));
        assert_eq!(mouse_types(2), Some((25, 26, 2)));
        assert_eq!(mouse_types(4), Some((25, 26, 4)));
    }
}
