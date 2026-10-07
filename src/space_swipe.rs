//! A synthetic horizontal Dock swipe: the system's own Space-switch input path, used as an
//! alternative to moving a Space by fronting an application.
//!
//! Reference implementations use this because it commits the Space switch immediately and through
//! WindowServer's normal transition machinery (so the destination Space keeps its menu bar), while
//! setting the current Space directly skips that machinery. The gesture is a `CGEvent` carrying
//! undocumented gesture fields, so they are addressed by raw field number here, and all three
//! phases are required: a partial sequence leaves WindowServer mid-gesture and never commits.
//!
//! `progress` is `±FLT_TRUE_MIN` -- signed so the direction registers, far too small to render any
//! travel -- and the velocity is scaled by the number of Spaces to cross, so the system reads it as
//! an instant flick rather than a slow drag.
//!
//! Measured 2026-10-07 on macOS 26: the events are created and posted (`delivered=true`) both from a
//! plain process and from this app's own process, and the active Space does not change in either
//! case. The reference implementation that uses this sequence also installs a swipe-suppressor
//! event tap and gates the feature behind a preference, so the three events are not the whole
//! mechanism. It is kept behind `--space-swipe=left|right` as an instrument for the next attempt,
//! and nothing in the switch path calls it.

use crate::event_tap::{
    CGEventCreate, CGEventPost, CGEventRef, CGEventSetDoubleValueField, CGEventSetIntegerValueField,
};
use crate::ffi::CFRelease;
use crate::log_info;

/// `kCGSEventType` (field 55), set to `kCGSEventDockControl`.
const FIELD_EVENT_TYPE: i32 = 55;
/// `kCGEventGestureHIDType` (field 110), set to `kIOHIDEventTypeDockSwipe`.
const FIELD_HID_TYPE: i32 = 110;
/// `kCGEventGestureSwipeMotion` (field 123), set to horizontal.
const FIELD_MOTION: i32 = 123;
/// `kCGEventGestureSwipeProgress` (field 124).
const FIELD_PROGRESS: i32 = 124;
/// `kCGEventGestureSwipeVelocityX` / `...VelocityY` (fields 129 / 130).
const FIELD_VELOCITY_X: i32 = 129;
const FIELD_VELOCITY_Y: i32 = 130;
/// `kCGEventGesturePhase` (field 132).
const FIELD_PHASE: i32 = 132;

const EVENT_TYPE_DOCK_CONTROL: i64 = 30;
const HID_TYPE_DOCK_SWIPE: i64 = 23;
const MOTION_HORIZONTAL: i64 = 1;
const PHASE_BEGAN: i64 = 1;
const PHASE_CHANGED: i64 = 2;
const PHASE_ENDED: i64 = 4;

/// `kCGSessionEventTap`.
const SESSION_TAP: i32 = 1;
/// Velocity per Space crossed; the reference scales its 2000 by the jump distance.
const SWIPE_VELOCITY: f64 = 2000.0;

/// Post one complete horizontal Dock swipe. Returns whether all three events were created and
/// posted; `false` means no gesture was delivered at all (a partial sequence would leave
/// WindowServer mid-gesture, so nothing is posted unless the whole sequence exists).
pub(crate) fn post_dock_swipe(rightward: bool, spaces_to_cross: u32) -> bool {
    // `Float.leastNonzeroMagnitude`: signed direction, no visible travel.
    let tiny = f64::from(f32::from_bits(1));
    let progress = if rightward { tiny } else { -tiny };
    let velocity = SWIPE_VELOCITY * f64::from(spaces_to_cross.max(1));
    let vx = if rightward { velocity } else { -velocity };

    // Build the whole sequence first, then post it, then release it: `CGEventPost` does not take
    // ownership, so every created event must be released on both the success and the failure path.
    let mut events: Vec<CGEventRef> = Vec::with_capacity(3);
    for phase in [PHASE_BEGAN, PHASE_CHANGED, PHASE_ENDED] {
        let event = unsafe { CGEventCreate(std::ptr::null()) };
        if event.is_null() {
            break;
        }
        unsafe {
            CGEventSetIntegerValueField(event, FIELD_EVENT_TYPE, EVENT_TYPE_DOCK_CONTROL);
            CGEventSetIntegerValueField(event, FIELD_HID_TYPE, HID_TYPE_DOCK_SWIPE);
            CGEventSetIntegerValueField(event, FIELD_PHASE, phase);
            CGEventSetDoubleValueField(event, FIELD_PROGRESS, progress);
            CGEventSetIntegerValueField(event, FIELD_MOTION, MOTION_HORIZONTAL);
            CGEventSetDoubleValueField(event, FIELD_VELOCITY_X, vx);
            CGEventSetDoubleValueField(event, FIELD_VELOCITY_Y, vx);
        }
        events.push(event);
    }
    let complete = events.len() == 3;
    if complete {
        for event in &events {
            unsafe { CGEventPost(SESSION_TAP, *event) };
        }
    }
    for event in events {
        unsafe { CFRelease(event.cast_const().cast()) };
    }
    complete
}

/// The active Space per display, as the tracker last observed it. Used by the development probe and
/// by the assertion that a swipe landed.
pub(crate) fn current_spaces() -> Vec<(String, u64)> {
    crate::space_groups::with_tracker(|tracker| {
        let mut spaces: Vec<(String, u64)> = tracker
            .topology()
            .displays
            .iter()
            .map(|(display, spaces)| (display.clone(), spaces.current))
            .collect();
        spaces.sort();
        spaces
    })
}

/// The development probe behind `--space-swipe=left|right`: post one swipe after the app is up and
/// report the Space before and after, so the mechanism can be verified without a trackpad. It is an
/// instrument, not a feature: nothing in the switch path calls it yet.
pub(crate) fn probe(direction: &str) {
    let rightward = match direction {
        "right" => true,
        "left" => false,
        other => {
            log_info!("[space-swipe] unknown direction {other:?}: use left or right");
            return;
        }
    };
    let before = current_spaces();
    let delivered = post_dock_swipe(rightward, 1);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let after = current_spaces();
    log_info!(
        "[space-swipe] direction={} delivered={} before={:?} after={:?} changed={}",
        direction,
        delivered,
        before,
        after,
        before != after
    );
}
