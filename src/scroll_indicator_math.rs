//! Pure scroll-thumb math shared by the custom scroll indicators.
//!
//! There is no AppKit/ObjC here on purpose: the clipboard capsules (`clipboard/notifications`)
//! and the switcher's thumbnail viewport (`overlay/callbacks`) each own their viewport, track
//! insets, corner reserve, colors and hit behavior, and hand the numbers in. This module owns
//! only what both do identically: turning an offset into a thumb position, and turning a pointer
//! drag back into an offset.
//!
//! This is unrelated to `crate::scroller`, which resynchronizes the *native* settings-page
//! scroll views; the custom indicators never use that path.
//!
//! Vertical orientation differs between the two hosts: the clipboard clip is non-flipped
//! (progress moves the thumb toward larger y), while the thumbnail viewport is flipped (progress
//! moves it toward smaller y). `ThumbDirection` carries that for both position and dragging, so
//! neither host has to fold a flip into one calculation and not the other.

/// Which way a thumb moves as scroll progress increases. Drawing and dragging must use the same
/// direction or the thumb jumps when it reaches the end of the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThumbDirection {
    /// Position grows with progress.
    Forward,
    /// Position shrinks as progress grows (a flipped host view).
    Reverse,
}

/// A thumb's low-end position and its remaining travel along the track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ThumbPlacement {
    /// The thumb's start coordinate on the track (the end with the smaller coordinate).
    pub(crate) position: f64,
    /// Distance the thumb can still move: `track_len - length`, never negative.
    pub(crate) travel: f64,
}

/// Place an already-sized thumb on a track for a given scroll offset.
///
/// `track_start`/`track_len` describe the usable track (insets and any corner reserve already
/// applied), `length` is the thumb length the caller computed, and `offset`/`max_offset` are the
/// scroll view's current and maximum content offset. Returns `None` when there is no track to
/// draw on or the content does not overflow, which the callers turn into "hide the indicator".
pub(crate) fn thumb_placement(
    track_start: f64,
    track_len: f64,
    length: f64,
    offset: f64,
    max_offset: f64,
    direction: ThumbDirection,
) -> Option<ThumbPlacement> {
    if !track_start.is_finite() || !track_len.is_finite() || !offset.is_finite() {
        return None;
    }
    if track_len <= 0.0 || max_offset <= 0.0 {
        return None;
    }
    let length = length.clamp(0.0, track_len);
    let travel = (track_len - length).max(0.0);
    let progress = (offset / max_offset).clamp(0.0, 1.0);
    let traveled = match direction {
        ThumbDirection::Forward => progress * travel,
        ThumbDirection::Reverse => (1.0 - progress) * travel,
    };
    Some(ThumbPlacement {
        position: track_start + traveled,
        travel,
    })
}

/// The thumb length for a track, from the scroll view's visible and total content lengths.
///
/// This is NSScroller's proportion: the thumb occupies `visible / total` of the usable track,
/// clamped to `[min_len, track_len]` so it is never shorter than a readable pill nor longer than
/// the track. Both custom indicators size their thumb with this one formula; their tracks still
/// differ (insets, corner reserve), and that is the intended remaining difference. `total` is the
/// content length and `visible` the scroll view's visible length, so `visible <= total` whenever
/// the content overflows (callers only ask when it does).
pub(crate) fn thumb_length(track_len: f64, visible: f64, total: f64, min_len: f64) -> f64 {
    let min_len = min_len.max(0.0).min(track_len.max(0.0));
    if track_len <= 0.0 || total <= 0.0 {
        return min_len;
    }
    (track_len * (visible / total)).clamp(min_len, track_len)
}

/// Map a pointer drag along the track axis to a clamped content offset.
///
/// `start_offset`/`start_axis` are captured on mouse-down, `current_axis` is the live pointer
/// coordinate, and `max_offset`/`travel` bound the result. Both hosts drive an explicit drag
/// (the native scroller is disabled), and `direction` matches the one passed to
/// [`thumb_placement`] so dragging stays consistent with drawing.
pub(crate) fn drag_offset(
    start_offset: f64,
    start_axis: f64,
    current_axis: f64,
    max_offset: f64,
    travel: f64,
    direction: ThumbDirection,
) -> f64 {
    if max_offset <= 0.0 || travel <= 0.0 {
        return start_offset.clamp(0.0, max_offset.max(0.0));
    }
    let delta = match direction {
        ThumbDirection::Forward => current_axis - start_axis,
        ThumbDirection::Reverse => start_axis - current_axis,
    };
    (start_offset + delta * max_offset / travel).clamp(0.0, max_offset)
}

#[cfg(test)]
mod tests {
    use super::{drag_offset, thumb_length, thumb_placement, ThumbDirection};

    #[test]
    fn length_uses_the_visible_over_total_proportion() {
        // Half the content is visible: half the track, inside the clamps.
        assert_eq!(thumb_length(100.0, 50.0, 100.0, 24.0), 50.0);
        // Never shorter than the minimum, and never longer than the track.
        assert_eq!(thumb_length(100.0, 1.0, 100000.0, 24.0), 24.0);
        assert_eq!(thumb_length(20.0, 1.0, 100000.0, 24.0), 20.0);
        // A full viewport clamps to the whole track.
        assert_eq!(thumb_length(100.0, 100.0, 100.0, 24.0), 100.0);
    }

    #[test]
    fn placement_rejects_no_track_or_no_overflow() {
        assert!(thumb_placement(4.0, 0.0, 24.0, 0.0, 100.0, ThumbDirection::Forward).is_none());
        assert!(thumb_placement(4.0, 56.0, 24.0, 0.0, 0.0, ThumbDirection::Forward).is_none());
    }

    #[test]
    fn placement_pins_endpoints_in_both_directions() {
        let forward_top = thumb_placement(4.0, 56.0, 24.0, 0.0, 100.0, ThumbDirection::Forward)
            .expect("overflowing content");
        let forward_bottom =
            thumb_placement(4.0, 56.0, 24.0, 100.0, 100.0, ThumbDirection::Forward)
                .expect("overflowing content");
        assert_eq!(forward_top.position, 4.0);
        assert_eq!(forward_top.travel, 32.0);
        assert_eq!(forward_bottom.position, 36.0);

        let reverse_top = thumb_placement(22.0, 56.0, 24.0, 0.0, 100.0, ThumbDirection::Reverse)
            .expect("overflowing content");
        let reverse_bottom =
            thumb_placement(22.0, 56.0, 24.0, 100.0, 100.0, ThumbDirection::Reverse)
                .expect("overflowing content");
        assert_eq!(reverse_top.position, 54.0);
        assert_eq!(reverse_bottom.position, 22.0);
    }

    #[test]
    fn placement_clamps_an_oversized_thumb_to_the_track() {
        let placement = thumb_placement(4.0, 56.0, 500.0, 30.0, 100.0, ThumbDirection::Forward)
            .expect("overflowing content");
        assert_eq!(placement.travel, 0.0);
        assert_eq!(placement.position, 4.0);
    }

    #[test]
    fn drag_maps_and_clamps_in_both_directions() {
        // Forward: pointer moving toward larger coordinates increases the offset.
        assert_eq!(
            drag_offset(0.0, 50.0, 75.0, 100.0, 50.0, ThumbDirection::Forward),
            50.0
        );
        assert_eq!(
            drag_offset(20.0, 50.0, -200.0, 100.0, 50.0, ThumbDirection::Forward),
            0.0
        );
        assert_eq!(
            drag_offset(80.0, 50.0, 200.0, 100.0, 50.0, ThumbDirection::Forward),
            100.0
        );
        // Reverse: the same pointer move reduces the offset.
        assert_eq!(
            drag_offset(0.0, 50.0, 25.0, 100.0, 50.0, ThumbDirection::Reverse),
            50.0
        );
        assert_eq!(
            drag_offset(20.0, 50.0, 200.0, 100.0, 50.0, ThumbDirection::Reverse),
            0.0
        );
        assert_eq!(
            drag_offset(80.0, 50.0, -200.0, 100.0, 50.0, ThumbDirection::Reverse),
            100.0
        );
    }

    #[test]
    fn drag_without_travel_only_clamps_the_start() {
        assert_eq!(
            drag_offset(30.0, 50.0, 500.0, 100.0, 0.0, ThumbDirection::Forward),
            30.0
        );
        assert_eq!(
            drag_offset(-5.0, 50.0, 500.0, 100.0, 0.0, ThumbDirection::Forward),
            0.0
        );
        assert_eq!(
            drag_offset(150.0, 50.0, 500.0, 100.0, 0.0, ThumbDirection::Forward),
            100.0
        );
    }
}
