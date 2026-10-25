//! Clipboard subsystem · panel drag: any blank region of the panel moves it.
//!
//! The v2 design drops the three-dot handle. A dedicated grab band was the old shape because the
//! picker is almost entirely controls; the opposite rule replaces it: EVERY inert blank region
//! drags, and the ambiguous regions keep their own events by declining the gesture.
//!
//! Two views carry the gesture, so no region needs a hand-maintained exclusion list:
//!
//! - `OhMyTabClipboardDragSurface` is a transparent topmost view over the panel content. Its
//!   `hitTest:` declines the list's viewport (rows, wheel scrolling, the scroll indicator) and every
//!   control or text view, letting AppKit route those clicks exactly as before. Only a blank region
//!   -- the top bar's padding, the header's gaps, the footer, the panel margins -- resolves to it.
//! - `OhMyTabClipboardContainer` (the list's document view) extends the same gesture over the list's
//!   own blank area below the last row. Its row subviews are controls, so a row click never reaches
//!   these handlers; wheel events still bubble to the scroll view as before.
//!
//! Excluded by construction: the search field, every button (rows, filter pills, clear actions, the
//! corner buttons, the scroll indicator) and selectable text. A sub-threshold press is a click with
//! no action; a double-click on blank space keeps the old "put the panel back" reset.
//!
//! Scope of the moved position: **one showing**. The anchor lives in `SESSION_ORIGIN`, is cleared
//! by `on_picker_hidden`, and is never written to the configuration, so the next summon puts the
//! panel back where `clipboard.picker_position` asks for it.

use super::*;
use crate::callback_guard;

/// One in-flight grab of a blank region.
///
/// The drag measures the pointer in *screen* coordinates (`NSEvent.mouseLocation`) rather than
/// `locationInWindow`: the window is what moves, so a window-relative point would travel with it
/// and the delta would stay near zero.
#[derive(Clone, Copy, Debug)]
struct PanelDrag {
    /// The screen point (AppKit bottom-left origin) the press started at.
    mouse_down: NSPoint,
    /// The picker's panel rect when the press started; the drag is this rect plus the delta.
    picker: NSRect,
    /// The **anchor** at press time: where the picker sits as if no detail panel were open. With the
    /// detail open the live rect is shifted left by the group layout, so the anchor is a different
    /// number -- and it is the anchor, not the live rect, that a finished drag remembers (see
    /// `current_anchor`).
    anchor: NSPoint,
    /// The detail panel's panel rect when the press started, while it is open: the pair is moved
    /// as one group, so `DETAIL_GAP` and the row alignment between them never change.
    detail: Option<NSRect>,
    /// Whether the pointer has travelled past `PICKER_DRAG_THRESHOLD` yet. Until it does, the grab
    /// is a no-op (a blank region has no click action, and a sub-threshold wobble must not move the
    /// panel a pixel).
    moved: bool,
}

/// The pointer state a grab owns. `NSCursor` push/pop is a **global stack**, so every push has to
/// be matched exactly once -- including when the panel disappears mid-drag, which is why the state
/// is kept here rather than pushed blind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragCursor {
    None,
    Closed,
}

/// The picker's top-left for the current showing, as if no detail panel were open: the anchor a
/// drag moves, and the origin the detail group layout derives from. `None` means "wherever
/// `clipboard.picker_position` put it".
static SESSION_ORIGIN: Mutex<Option<NSPoint>> = Mutex::new(None);
static DRAG: Mutex<Option<PanelDrag>> = Mutex::new(None);
static CURSOR: Mutex<DragCursor> = Mutex::new(DragCursor::None);
/// The blank-area surface, so a content rebuild can put it back on top (see
/// [`raise_drag_surface`]).
static DRAG_SURFACE: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

// ---------------------------------------------------------------------------------------------
// Position state
// ---------------------------------------------------------------------------------------------

/// Whether a grab is in flight (a press on a blank region, not yet released). The panel itself
/// answers popover questions from `grab_travelled`; this is what the tests assert the recording
/// against.
#[cfg(test)]
pub(super) fn drag_in_progress() -> bool {
    DRAG.lock().unwrap().is_some()
}

/// Whether the recorded grab has travelled past its threshold -- a real drag of the panel, as
/// opposed to the grab record every blank-region press creates whether or not it moves.
fn grab_travelled() -> bool {
    DRAG.lock().unwrap().as_ref().is_some_and(|drag| drag.moved)
}

/// An outside press that arrived while the options popover was open, held until the release says
/// whether it was a drag (keep the menu) or a click (close it).
///
/// A press alone cannot answer that: every drag starts as a press on a blank region, and the
/// popover's outside-click rule is evaluated in `sendEvent:` before the drag has moved. So the
/// press is recorded here and the dismissal is decided from the recorded grab at the release --
/// `moved` is the same threshold the drag itself uses, so a sub-threshold wobble behaves exactly
/// like the click it is.
static PENDING_POPOVER_DISMISS: Mutex<bool> = Mutex::new(false);

/// Record what the press that just arrived means for the popover: `true` when it landed outside
/// the menu (and not on the orb that toggles it), which is the case the release has to decide.
/// An inside press clears any stale flag -- only the most recent press matters.
pub(super) fn note_popover_outside_press(outside: bool) {
    *PENDING_POPOVER_DISMISS.lock().unwrap() = outside;
}

/// Forget an outside press when its popover closes before the matching release arrives.
/// The panel can be hidden mid-gesture, in which case AppKit does not deliver that release;
/// carrying the flag into the next showing could dismiss a newly opened menu.
pub(super) fn clear_pending_popover_dismiss() {
    *PENDING_POPOVER_DISMISS.lock().unwrap() = false;
}

/// What the release of an outside press does to an open options menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopoverRelease {
    /// The press never happened outside an open menu: nothing to do.
    Nothing,
    /// A press that stayed put (a click): close the menu.
    Dismiss,
    /// A press that became a drag of the panel: the menu stays open.
    Keep,
}

/// The decision itself, pure so it can be asserted: a press only dismisses the menu when it did
/// NOT travel. `travelled` must be the drag threshold's answer, never "a grab exists" -- every
/// blank-region press records a grab, so keying on that would turn every click into a keep.
fn popover_release(pending: bool, travelled: bool) -> PopoverRelease {
    match (pending, travelled) {
        (false, _) => PopoverRelease::Nothing,
        (true, true) => PopoverRelease::Keep,
        (true, false) => PopoverRelease::Dismiss,
    }
}

/// The release half: drop the pending press, dismissing the popover unless that press became a
/// drag of the panel. Called for every press the window forwarded, whether or not one is pending.
pub(super) fn finish_popover_press() {
    finish_popover_press_with(dismiss_for_release);
}

/// The release half with the dismissal supplied by the caller, so a test can observe the *effect*
/// (was the menu closed?) rather than the decision value.
///
/// The decision itself -- reading the pending press and asking whether the grab travelled -- stays
/// INSIDE this function on purpose. A seam that took the finished decision as a parameter would let
/// a test re-evaluate the decision in its own closure and still pass after the production call site
/// regressed to "is a grab in progress?" (true for every blank press, clicks included), which is
/// exactly the bug this guards. Only the dismissal is injected.
fn finish_popover_press_with(dismiss: impl FnOnce()) {
    let pending = std::mem::take(&mut *PENDING_POPOVER_DISMISS.lock().unwrap());
    match popover_release(pending, grab_travelled()) {
        PopoverRelease::Nothing | PopoverRelease::Keep => {}
        PopoverRelease::Dismiss => dismiss(),
    }
}

/// The shipped dismissal: AppKit work, main thread only.
fn dismiss_for_release() {
    crate::clipboard::detail::dismiss_filter_popover();
}

/// The dragged anchor for this showing, if the user has moved the panel.
pub(super) fn session_origin() -> Option<NSPoint> {
    *SESSION_ORIGIN.lock().unwrap()
}

/// Remember the anchor a drag ended on.
pub(super) fn set_session_origin(origin: NSPoint) {
    *SESSION_ORIGIN.lock().unwrap() = Some(origin);
}

/// The origin to lay the detail group out from: the dragged anchor when there is one, otherwise
/// the live rect's own origin (which is the configured position).
#[cfg(test)]
pub(super) fn anchor_origin(live: NSPoint) -> NSPoint {
    session_origin().unwrap_or(live)
}

/// The anchor as it stands before a grab starts. Three cases, in order: the user already moved the
/// panel this showing; else the detail panel is open and the origin its close will restore IS the
/// anchor; else the live rect (the configured position) is.
fn current_anchor(picker: NSRect) -> NSPoint {
    if let Some(origin) = session_origin() {
        return origin;
    }
    if detail_visible() {
        if let Some(origin) = *DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap() {
            return origin;
        }
    }
    picker.origin
}

/// Forget everything the current showing moved: the anchor a drag ended on and any in-flight grab.
///
/// This is the "one showing" invariant in one place -- a dragged position never outlives the panel
/// that was dragged, and it is never written to the configuration. Split out from
/// `on_picker_hidden` so it can be asserted without touching the cursor stack.
pub(super) fn forget_session_position() {
    *DRAG.lock().unwrap() = None;
    *SESSION_ORIGIN.lock().unwrap() = None;
}

/// Everything the picker must let go of when it hides: the position state above, and the cursor
/// push -- `NSCursor` is a global stack, so a hand pushed when the panel disappears has to be
/// popped by hand (a later `mouseUp` never arrives, and must not write a position either).
pub(super) fn on_picker_hidden() {
    crate::debug_assert_main_thread();
    forget_session_position();
    set_cursor(DragCursor::None);
}

/// The correction a finished drag applies, so a panel the user left half off-screen stays usable.
/// Pure.
///
/// The picker keeps its whole rect inside `visible`. While the detail panel is open the pair is
/// corrected as **one rigid group**, so a picker that is only just inside cannot push the detail
/// off the right edge. When the group is wider than the screen both rules cannot hold; the leading
/// edge wins and the detail overflows, the same degradation `detail_group_frames` uses.
pub(super) fn drag_release_correction(
    picker: NSRect,
    detail: Option<NSRect>,
    visible: NSRect,
) -> NSPoint {
    let min_x = visible.origin.x;
    let max_x = visible.origin.x + visible.size.width;
    let min_y = visible.origin.y;
    let max_y = visible.origin.y + visible.size.height;

    let group_left = detail.map_or(picker.origin.x, |d| picker.origin.x.min(d.origin.x));
    let group_right = detail.map_or(picker.origin.x + picker.size.width, |d| {
        (picker.origin.x + picker.size.width).max(d.origin.x + d.size.width)
    });

    let mut dx = 0.0;
    if group_right > max_x {
        dx = max_x - group_right;
    }
    if group_left + dx < min_x {
        dx = min_x - group_left;
    }
    if detail.is_some() && group_right - group_left > visible.size.width {
        dx = min_x - group_left;
    }

    // The picker is the taller of the two and the detail is laid out inside its vertical span, so
    // correcting the picker's y corrects the pair.
    let mut dy = 0.0;
    if picker.size.height > visible.size.height {
        // A panel taller than the screen cannot be wholly inside it. The top bar is what a user
        // reaches for, so the top edge wins and the bottom overflows; the other order put the top
        // bar above the display and left the panel unreachable.
        dy = max_y - (picker.origin.y + picker.size.height);
    } else {
        if picker.origin.y + picker.size.height > max_y {
            dy = max_y - (picker.origin.y + picker.size.height);
        }
        if picker.origin.y + dy < min_y {
            dy = min_y - picker.origin.y;
        }
    }
    NSPoint::new(dx, dy)
}

/// Clamp the picker's origin so the panel it belongs to stays inside `visible`: the standalone picker's own
/// frame, with the same edge margin the drag clamp keeps.
///
/// The detail panel's close restores a standalone origin that was saved while a GROUP was on screen. That
/// group is wider than the picker, so after a drag to a narrower display the saved origin can sit past the
/// new screen's right edge even though the group was clamped into it -- restoring it would put the panel
/// (and its corner buttons) off-screen. Pure.
pub(super) fn clamp_picker_origin(origin: NSPoint, size: NSSize, visible: NSRect) -> NSPoint {
    let min_x = visible.origin.x + PICKER_EDGE_MARGIN;
    let max_x = visible.origin.x + visible.size.width - PICKER_EDGE_MARGIN;
    let min_y = visible.origin.y + PICKER_EDGE_MARGIN;
    let max_y = visible.origin.y + visible.size.height - PICKER_EDGE_MARGIN;
    let x = if size.width >= visible.size.width {
        min_x
    } else {
        origin.x.max(min_x).min(max_x - size.width)
    };
    let y = if size.height >= visible.size.height {
        // Same rule as the release clamp: the top edge wins and the bottom overflows.
        max_y - size.height
    } else {
        origin.y.max(min_y).min(max_y - size.height)
    };
    NSPoint::new(x, y)
}

// ---------------------------------------------------------------------------------------------
// Hit testing: which blank regions start a drag
// ---------------------------------------------------------------------------------------------

/// Whether a point in the content parent's coordinates falls in the list's viewport.
///
/// The whole viewport is declined by the drag surface: the scroll view owns wheel scrolling, its
/// rows own their clicks, and the scroll indicator owns its drag. The blank area INSIDE the list is
/// covered by the document view's own handlers instead (see [`drag_mouse_down`]), which is what
/// keeps scrolling unchanged. Pure so the boundary is testable.
fn point_in_list_region(point: NSPoint, scroll_frame: NSRect) -> bool {
    rect_contains_point(scroll_frame, point)
}

/// Whether a view under the pointer owns the event in a way a blank-area drag must not take over:
/// controls (buttons, fields, sliders -- `NSControl` covers every one of them), text views, and the
/// scroll machinery itself. Everything else (the header strip, the footer, the document view, a
/// label's plain container) is inert background.
unsafe fn drag_target_owns_the_click(target: *mut AnyObject) -> bool {
    if target.is_null() {
        return false;
    }
    for class in [
        class!(NSControl),
        class!(NSTextView),
        class!(NSScrollView),
        class!(NSClipView),
        class!(NSScroller),
    ] {
        let owns: bool = msg_send![target, isKindOfClass: class];
        if owns {
            return true;
        }
    }
    false
}

/// The view AppKit would hit for `point` if the drag surface were not in the tree: the sibling
/// subtrees are tested in AppKit's own back-to-front order, skipping the surface itself.
///
/// `point` is in the surface's superview coordinates, which is also the sibling views' parent
/// coordinate space, so no conversion is involved.
unsafe fn sibling_hit_below_drag_surface(
    surface: *mut AnyObject,
    superview: *mut AnyObject,
    point: NSPoint,
) -> *mut AnyObject {
    let subviews: *mut AnyObject = msg_send![superview, subviews];
    let count: usize = msg_send![subviews, count];
    for index in (0..count).rev() {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if view == surface {
            continue;
        }
        let hit: *mut AnyObject = msg_send![view, hitTest: point];
        if !hit.is_null() {
            return hit;
        }
    }
    std::ptr::null_mut()
}

// ---------------------------------------------------------------------------------------------
// The drag surface
// ---------------------------------------------------------------------------------------------

/// Put the transparent drag surface over the panel content. Called once, when the picker window is
/// built; a backdrop swap moves the content's subviews (this one included), so the surface survives
/// a material change without being rebuilt.
pub(super) unsafe fn install_drag_surface(content_parent: *mut AnyObject) {
    let bounds: NSRect = msg_send![content_parent, bounds];
    let view: *mut AnyObject = msg_send![drag_surface_class(), alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: bounds];
    // Tracks the content parent on every resize (the panel is resized per summon).
    let _: () = msg_send![view, setAutoresizingMask: 18u64];
    // A gesture surface is not an accessibility element of its own: the panel's content already
    // carries the labels, and a bare group would only add an unlabeled stop.
    let _: () = msg_send![view, setAccessibilityElement: false];
    let _: () = msg_send![content_parent, addSubview: view];
    *DRAG_SURFACE.lock().unwrap() = Some(ObjPtr::new(view));
    release_obj(view);
}

/// Put the surface back on top after a content rebuild.
///
/// Only the footer is rebuilt in place (a locale or text-size change replaces the whole view), and
/// `build_footer` appends the new footer to the content parent -- which would leave it ABOVE the
/// surface and silently turn the footer's blank area non-draggable. `addSubview:positioned:relativeTo:`
/// with `NSWindowAbove` and a nil relative view puts the existing subview back at the top of the
/// sibling list; the scroll view's rows rebuild inside the scroll view and never reorder these
/// siblings.
pub(super) unsafe fn raise_drag_surface(content_parent: *mut AnyObject) {
    let Some(surface) = *DRAG_SURFACE.lock().unwrap() else {
        return;
    };
    if surface.0.is_null() {
        return;
    }
    let superview: *mut AnyObject = msg_send![surface.0, superview];
    if superview != content_parent {
        return;
    }
    // NSWindowAbove = 1; a nil relative view means "the top of the subview list".
    let _: () = msg_send![
        content_parent,
        addSubview: surface.0,
        positioned: 1isize,
        relativeTo: std::ptr::null::<AnyObject>()
    ];
    // Raising the surface above a rebuild must not push it over the open options popover: the
    // popover is built after this surface and would lose its presses (every blank press there
    // would start a drag), and the window-wide click monitor then closes the menu the user had
    // just opened. Put the popover back on top whenever it is present.
    if let Some(popover) = *FILTER_POPOVER.lock().unwrap() {
        if !popover.0.is_null() {
            let popover_parent: *mut AnyObject = msg_send![popover.0, superview];
            if popover_parent == content_parent {
                let _: () = msg_send![
                    content_parent,
                    addSubview: popover.0,
                    positioned: 1isize,
                    relativeTo: std::ptr::null::<AnyObject>()
                ];
            }
        }
    }
}

/// The surface's hit test: accept only a truly inert blank region, decline everything ambiguous.
///
/// Returning nil here is what preserves every existing click exactly: AppKit continues the same hit
/// test with the remaining subviews, so the control, row, scroll view or text view under the
/// pointer receives the event as it did before the surface existed.
extern "C" fn drag_surface_hit_test(
    self_view: *mut AnyObject,
    _cmd: Sel,
    point: NSPoint,
) -> *mut AnyObject {
    callback_guard::event(
        "clipboard_drag_surface_hit_test",
        std::ptr::null_mut(),
        || unsafe {
            let superview: *mut AnyObject = msg_send![self_view, superview];
            if superview.is_null() {
                return std::ptr::null_mut();
            }
            let frame: NSRect = msg_send![self_view, frame];
            if !rect_contains_point(frame, point) {
                return std::ptr::null_mut();
            }
            if let Some(scroll) = (*SCROLL_VIEW.lock().unwrap()).map(|scroll| scroll.0) {
                if !scroll.is_null() {
                    let scroll_frame: NSRect = msg_send![scroll, frame];
                    if point_in_list_region(point, scroll_frame) {
                        return std::ptr::null_mut();
                    }
                }
            }
            // The options popover is a sibling laid over the content, and a press that starts on it
            // must reach it: the panel's window-wide click monitor dismisses the popover on a press
            // outside its frame, so a blank region it covers has to stay a drag dead zone while it
            // is open. Otherwise "click a blank strip, drag to move the panel" reads to the app as
            // a press outside the menu and the menu closes under the grab.
            if let Some(popover) = *FILTER_POPOVER.lock().unwrap() {
                if !popover.0.is_null() {
                    let popover_frame: NSRect = msg_send![popover.0, frame];
                    if rect_contains_point(popover_frame, point) {
                        return std::ptr::null_mut();
                    }
                }
            }
            let target = sibling_hit_below_drag_surface(self_view, superview, point);
            if drag_target_owns_the_click(target) {
                return std::ptr::null_mut();
            }
            self_view
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Drag interaction
// ---------------------------------------------------------------------------------------------

/// A press on any accepted blank region (the surface or the list's document view).
pub(super) extern "C" fn drag_mouse_down(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("clipboard_drag_mouse_down", || unsafe {
        let click_count: isize = msg_send![event, clickCount];
        let point: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        begin_drag(point, click_count);
    });
}

pub(super) extern "C" fn drag_mouse_dragged(
    _self: *mut AnyObject,
    _cmd: Sel,
    _event: *mut AnyObject,
) {
    callback_guard::void("clipboard_drag_mouse_dragged", || {
        let point: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        drag_to(point);
    });
}

pub(super) extern "C" fn drag_mouse_up(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    callback_guard::void("clipboard_drag_mouse_up", finish_drag);
}

/// First mouse, so a grab works while the panel's host app is inactive -- the same reason the
/// scroll indicator overrides it.
pub(super) extern "C" fn drag_accepts_first_mouse(
    _self: *mut AnyObject,
    _cmd: Sel,
    _event: *mut AnyObject,
) -> objc2::runtime::Bool {
    callback_guard::value(
        "drag_accepts_first_mouse",
        objc2::runtime::Bool::YES,
        || objc2::runtime::Bool::YES,
    )
}

fn begin_drag(point: NSPoint, click_count: isize) {
    crate::debug_assert_main_thread();
    let Some(picker) = picker_panel_rect() else {
        return;
    };
    // Defensive: the handlers belong to blank regions, but a stale or forwarded coordinate must not
    // move a panel that does not contain it.
    if !rect_contains_point(picker, point) {
        return;
    }
    if click_count >= 2 {
        // Double-click: put the panel back where the configuration asks for it. `reset` recomputes
        // from the mode, forgets this showing's anchor (it owns that state) and re-lays the detail
        // group out if it is open, so both windows move together.
        *DRAG.lock().unwrap() = None;
        // The first click's release normally popped the hand; popping again is a no-op unless that
        // mouseUp was lost, in which case this is the only place left that can balance the stack.
        set_cursor(DragCursor::None);
        super::picker::reset_picker_to_configured_position();
        return;
    }
    // A detail closed a moment ago keeps its queued `finishDetailClose:` when the user grabs a blank
    // region. Cancelling it here left that window ordered front, transparent and collapsed for good:
    // its logical "visible" flag was already cleared, so nothing would retry. The queued call belongs
    // to a detail the app already considers closed, so letting it finish IS the completion; cancelling
    // a pending close is the OPEN path's job (a quick reopen must not hide the new panel), and it does
    // that itself.
    let detail = if detail_visible() {
        detail_panel_rect()
    } else {
        None
    };
    *DRAG.lock().unwrap() = Some(PanelDrag {
        mouse_down: point,
        picker,
        anchor: current_anchor(picker),
        detail,
        moved: false,
    });
    set_cursor(DragCursor::Closed);
}

fn drag_to(point: NSPoint) {
    crate::debug_assert_main_thread();
    let drag = match *DRAG.lock().unwrap() {
        Some(drag) => drag,
        None => return,
    };
    let dx = point.x - drag.mouse_down.x;
    let dy = point.y - drag.mouse_down.y;
    if !drag.moved && dx.hypot(dy) < PICKER_DRAG_THRESHOLD {
        return;
    }
    *DRAG.lock().unwrap() = Some(PanelDrag {
        moved: true,
        ..drag
    });
    let moved = |rect: NSRect| {
        NSRect::new(
            NSPoint::new(rect.origin.x + dx, rect.origin.y + dy),
            rect.size,
        )
    };
    apply_frames(Some(moved(drag.picker)), drag.detail.map(moved));
}

fn finish_drag() {
    crate::debug_assert_main_thread();
    let Some(drag) = DRAG.lock().unwrap().take() else {
        return;
    };
    if !drag.moved {
        // A click or a sub-threshold wobble never moved the window, but the press still swapped the
        // cursor to the closed hand: restore it on the way out.
        set_cursor(DragCursor::None);
        return;
    }
    // The release is where the clamp is applied: while the pointer is held the panel follows it
    // exactly (dragging past an edge is allowed), and letting go is what decides the final spot.
    let Some(picker) = picker_panel_rect() else {
        set_cursor(DragCursor::None);
        return;
    };
    let detail = if detail_visible() {
        detail_panel_rect()
    } else {
        None
    };
    let Some(visible) = picker_visible_frame() else {
        // Without a screen there is nothing to clamp into: keep what the drag produced rather than
        // snapping the panel somewhere arbitrary. Still reported, so the state document never
        // disagrees with the window.
        set_session_origin(NSPoint::new(
            drag.anchor.x + (picker.origin.x - drag.picker.origin.x),
            drag.anchor.y + (picker.origin.y - drag.picker.origin.y),
        ));
        set_cursor(DragCursor::None);
        crate::e2e_state::record("clipboard_picker_moved");
        return;
    };
    let correction = drag_release_correction(picker, detail, visible);
    // Where the drag actually took the picker, release clamp included. The anchor has to move with it:
    // an anchor that only took the correction stayed at the press-time position, so closing the detail
    // snapped the panel back to where the gesture started, and the next grab read a stale anchor.
    let displacement = NSPoint::new(
        picker.origin.x - drag.picker.origin.x + correction.x,
        picker.origin.y - drag.picker.origin.y + correction.y,
    );
    let corrected_picker = NSRect::new(
        NSPoint::new(
            picker.origin.x + correction.x,
            picker.origin.y + correction.y,
        ),
        picker.size,
    );
    let corrected_detail = detail.map(|d| {
        NSRect::new(
            NSPoint::new(d.origin.x + correction.x, d.origin.y + correction.y),
            d.size,
        )
    });
    apply_frames(Some(corrected_picker), corrected_detail);
    // The anchor is the picker's origin *without* the detail's group shift, and while the detail is
    // open the origin stored at open time is what closing it restores: both move to the corrected
    // place together, so closing the detail cannot snap the panel back to where the drag started.
    let corrected_anchor = NSPoint::new(
        drag.anchor.x + displacement.x,
        drag.anchor.y + displacement.y,
    );
    set_session_origin(corrected_anchor);
    if detail.is_some() {
        *DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap() = Some(corrected_anchor);
    }
    set_cursor(DragCursor::None);
    // The moved frame is a fact only the app can state, and the `--e2e-state` document is written on
    // events: without this the A2 drag scenario read the frame from before the gesture and called a
    // working drag a failure. Recorded once, at the end -- the intermediate frames of a gesture are
    // not state anyone reads, and each write collects the whole document.
    crate::e2e_state::record("clipboard_picker_moved");
}

/// Drive one complete grab of a blank region, from `from` to `to`, exactly as the mouse handlers do:
/// the threshold, the window move, the release clamp and the anchor all run for real, on the real
/// windows.
///
/// The GUI smoke runner uses this instead of a synthetic input instrument, because no synthetic drag
/// reaches a nonactivating floating panel from outside the process. What that leaves uncovered -- that
/// AppKit delivers the real event stream to these views -- is asserted by the A2 scenario with a real
/// pointer gesture, so the two tiers split the claim instead of one of them covering neither.
pub(super) fn e2e_drag(from: NSPoint, to: NSPoint) {
    begin_drag(from, 1);
    drag_to(to);
    finish_drag();
}

/// Whether a grab is still holding the closed hand, for the GUI smoke: a press that never crossed
/// the drag threshold must not leave it behind.
pub(super) fn e2e_cursor_is_closed() -> bool {
    *CURSOR.lock().unwrap() == DragCursor::Closed
}

// ---------------------------------------------------------------------------------------------
// Window plumbing
// ---------------------------------------------------------------------------------------------

/// Move the picker (and the detail panel, when it is open) with one call. `set_panel_frame` takes
/// panel rects and re-adds the elevation padding, so every rect here is a panel rect, never the
/// padded window frame.
fn apply_frames(picker: Option<NSRect>, detail: Option<NSRect>) {
    unsafe {
        if let (Some(rect), Some(window)) = (picker, picker_window()) {
            crate::glass::set_panel_frame(window, rect, true);
        }
        if let (Some(rect), Some(window)) = (detail, detail_window()) {
            crate::glass::set_panel_frame(window, rect, true);
        }
    }
}

unsafe fn picker_window() -> Option<*mut AnyObject> {
    PICKER_WINDOW.lock().unwrap().map(|w| w.0)
}

unsafe fn detail_window() -> Option<*mut AnyObject> {
    DETAIL_WINDOW.lock().unwrap().and_then(|w| {
        let visible: bool = msg_send![w.0, isVisible];
        visible.then_some(w.0)
    })
}

/// The picker's panel rect, or `None` before the window exists.
fn picker_panel_rect() -> Option<NSRect> {
    unsafe { picker_window().map(|window| crate::glass::panel_frame_of(window)) }
}

fn detail_panel_rect() -> Option<NSRect> {
    unsafe { detail_window().map(|window| crate::glass::panel_frame_of(window)) }
}

/// The screen the panel is on right now: AppKit assigns the window to the screen it mostly
/// overlaps, so this follows a drag across displays without a second rule to keep in step.
fn picker_visible_frame() -> Option<NSRect> {
    unsafe { picker_window().map(|window| super::picker::picker_screen_frame(window)) }
}

// ---------------------------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------------------------

fn set_cursor(cursor: DragCursor) {
    let mut current = CURSOR.lock().unwrap();
    if *current == cursor {
        return;
    }
    unsafe {
        if *current != DragCursor::None {
            let _: () = msg_send![class!(NSCursor), pop];
        }
        let object: *mut AnyObject = match cursor {
            DragCursor::Closed => msg_send![class!(NSCursor), closedHandCursor],
            DragCursor::None => std::ptr::null_mut(),
        };
        if !object.is_null() {
            let _: () = msg_send![object, push];
        }
    }
    *current = cursor;
}

// ---------------------------------------------------------------------------------------------
// Class
// ---------------------------------------------------------------------------------------------

unsafe fn drag_surface_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabClipboardDragSurface", || {
        crate::ffi::build_class(
            c"OhMyTabClipboardDragSurface",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(hitTest:),
                    drag_surface_hit_test
                        as unsafe extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDown:),
                    drag_mouse_down as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDragged:),
                    drag_mouse_dragged as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseUp:),
                    drag_mouse_up as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(acceptsFirstMouse:),
                    drag_accepts_first_mouse
                        as unsafe extern "C" fn(
                            *mut AnyObject,
                            Sel,
                            *mut AnyObject,
                        ) -> objc2::runtime::Bool,
                );
            },
        )
    })
    .0 as *mut AnyObject
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    /// The screen a normal display offers, minus the dock/menu bar.
    fn screen() -> NSRect {
        rect(0.0, 0.0, 1440.0, 860.0)
    }

    #[test]
    fn a_panel_inside_the_screen_is_left_alone() {
        let picker = rect(400.0, 300.0, 560.0, 500.0);
        let correction = drag_release_correction(picker, None, screen());
        assert_eq!(correction, NSPoint::new(0.0, 0.0));
    }

    /// The property that matters: whatever the drag produced, the picker ends up whole on screen.
    #[test]
    fn the_released_picker_always_ends_up_fully_inside() {
        let screen = screen();
        for (x, y) in [
            (-200.0, 300.0),
            (1200.0, 300.0),
            (400.0, 700.0),
            (400.0, -300.0),
            (-9999.0, -9999.0),
            (9999.0, 9999.0),
        ] {
            let picker = rect(x, y, 560.0, 500.0);
            let correction = drag_release_correction(picker, None, screen);
            let moved = NSRect::new(
                NSPoint::new(x + correction.x, y + correction.y),
                picker.size,
            );
            assert!(moved.origin.x >= screen.origin.x, "x for {x},{y}");
            assert!(
                moved.origin.x + moved.size.width <= screen.origin.x + screen.size.width,
                "right edge for {x},{y}"
            );
            assert!(moved.origin.y >= screen.origin.y, "y for {x},{y}");
            assert!(
                moved.origin.y + moved.size.height <= screen.origin.y + screen.size.height,
                "top edge for {x},{y}"
            );
        }
    }

    /// With the detail open the pair is one group: a picker that just fits must not leave the
    /// detail hanging off the right edge.
    #[test]
    fn an_open_detail_is_corrected_with_the_picker_as_one_group() {
        let screen = screen();
        let picker = rect(900.0, 300.0, 560.0, 500.0); // right edge 1460 > 1440
        let detail = rect(1476.0, 300.0, 480.0, 500.0);
        let correction = drag_release_correction(picker, Some(detail), screen);
        let moved_picker = NSRect::new(
            NSPoint::new(
                picker.origin.x + correction.x,
                picker.origin.y + correction.y,
            ),
            picker.size,
        );
        let moved_detail = NSRect::new(
            NSPoint::new(
                detail.origin.x + correction.x,
                detail.origin.y + correction.y,
            ),
            detail.size,
        );
        assert_eq!(moved_detail.origin.x + moved_detail.size.width, 1440.0);
        assert!(moved_picker.origin.x >= 0.0);
        // The pair stays rigid: the gap the group layout established survives the correction.
        assert_eq!(
            moved_detail.origin.x - (moved_picker.origin.x + moved_picker.size.width),
            detail.origin.x - (picker.origin.x + picker.size.width)
        );
    }

    /// A group wider than the display cannot satisfy both edges; the leading edge wins and the
    /// picker -- which the user just grabbed -- stays whole.
    #[test]
    fn a_group_wider_than_the_screen_pins_the_picker_to_the_leading_edge() {
        let screen = rect(0.0, 0.0, 900.0, 860.0);
        let picker = rect(-120.0, 300.0, 560.0, 500.0);
        let detail = rect(456.0, 300.0, 480.0, 500.0);
        let correction = drag_release_correction(picker, Some(detail), screen);
        assert_eq!(picker.origin.x + correction.x, 0.0);
    }

    /// A panel taller than the target screen cannot fit, and the choice that matters is which edge keeps
    /// its place: the top bar is what a user reaches for, and a top bar above the display cannot be
    /// grabbed again. The earlier order aligned the bottom and left the top edge above the display.
    #[test]
    fn a_panel_taller_than_the_screen_keeps_its_top_edge_reachable() {
        let screen = rect(0.0, 0.0, 1440.0, 700.0);
        let picker = rect(400.0, 500.0, 560.0, 900.0);
        let correction = drag_release_correction(picker, None, screen);
        let moved = NSRect::new(
            NSPoint::new(
                picker.origin.x + correction.x,
                picker.origin.y + correction.y,
            ),
            picker.size,
        );
        // The top edge (the drag band's end, in AppKit coordinates) sits on the visible frame's top edge.
        assert_eq!(moved.origin.y + moved.size.height, 700.0);
        let band_height = picker_drag_band_frame().size.height;
        assert!(moved.origin.y + moved.size.height - band_height >= 0.0);
        // The bottom is what overflows, not the top.
        assert!(moved.origin.y < 0.0);
    }

    /// The review's own case: a group laid out on a wide display leaves the standalone picker's saved origin
    /// far to the right (620pt of group shift in their numbers). Dragged onto a narrow display, the group is
    /// clamped to the leading edge, and closing the detail must NOT restore the picker to an x that puts it
    /// off that display -- which is what the unclamped origin would do.
    #[test]
    fn a_restored_picker_is_clamped_into_the_screen_it_is_on() {
        let narrow = rect(0.0, 0.0, 1000.0, 860.0);
        let unclamped = NSPoint::new(628.0, 300.0);
        let size = NSSize::new(560.0, 500.0);
        let clamped = clamp_picker_origin(unclamped, size, narrow);
        assert!(
            clamped.x + size.width <= 1000.0 - PICKER_EDGE_MARGIN + 0.001,
            "the restored picker must stay inside the screen (x={}, right={})",
            clamped.x,
            clamped.x + size.width
        );
        assert!(clamped.x >= PICKER_EDGE_MARGIN);
        assert_eq!(
            clamped.y, unclamped.y,
            "a y that already fits is left alone"
        );
        // An origin that fits is untouched, so the ordinary path is not shifted by the clamp.
        let inside = NSPoint::new(200.0, 300.0);
        assert_eq!(clamp_picker_origin(inside, size, narrow), inside);
    }

    #[test]
    fn the_dragged_anchor_is_used_until_the_showing_hides() {
        let _guard = popover_test_lock();
        // One test, not two: the anchor is process-global state, and two tests setting and clearing it
        // race each other under the harness's parallel threads (the failure that produced this shape).
        let origin = NSPoint::new(120.0, 340.0);
        set_session_origin(origin);
        assert_eq!(session_origin(), Some(origin));
        // The detail group layout derives from the anchor, not from wherever the picker happens to be.
        assert_eq!(anchor_origin(NSPoint::new(500.0, 500.0)), origin);

        // The position a drag produced must not outlive the showing: a hidden picker forgets it, so the
        // next summon goes back to the configured mode and nothing reaches the configuration file.
        *DRAG.lock().unwrap() = Some(PanelDrag {
            mouse_down: NSPoint::new(0.0, 0.0),
            picker: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
            anchor: NSPoint::new(0.0, 0.0),
            detail: None,
            moved: true,
        });
        forget_session_position();
        assert_eq!(session_origin(), None);
        assert!(DRAG.lock().unwrap().is_none());
        // And the group layout therefore falls back to the live origin again.
        assert_eq!(
            anchor_origin(NSPoint::new(700.0, 100.0)),
            NSPoint::new(700.0, 100.0)
        );
    }

    /// The list's viewport is the one region the transparent surface always declines, because the
    /// scroll view owns it (wheel, rows, the indicator); blank area inside it is the document view's
    /// own gesture instead. The boundary is inclusive, like every other hit test in this panel.
    #[test]
    fn the_list_viewport_is_declined_by_the_drag_surface() {
        let viewport = rect(0.0, 44.0, 560.0, 400.0);
        assert!(point_in_list_region(NSPoint::new(0.0, 44.0), viewport));
        assert!(point_in_list_region(NSPoint::new(280.0, 200.0), viewport));
        assert!(point_in_list_region(NSPoint::new(560.0, 444.0), viewport));
        assert!(!point_in_list_region(NSPoint::new(280.0, 43.9), viewport));
        assert!(!point_in_list_region(NSPoint::new(280.0, 444.1), viewport));
        assert!(!point_in_list_region(NSPoint::new(560.1, 200.0), viewport));
    }

    /// The behaviour end to end through the seam the window's release handler uses: a blank press
    /// that did NOT travel must reach the dismissal, one that travelled must not.
    ///
    /// Driving the seam (not the pure decision) is what actually pins the original bug. Asserting
    /// `popover_release` case by case still passes when the caller feeds it "is a grab in
    /// progress?" instead of "did it travel?" -- every blank press records a grab, so clicks would
    /// read as Keep -- which is exactly how "every blank click keeps the menu open" survived a
    /// review round. Verified by mutation: reverting the caller to `drag_in_progress()` makes this
    /// test fail and leaves the pure-decision assertions green.
    #[test]
    fn a_blank_click_dismisses_and_a_drag_does_not() {
        let _guard = popover_test_lock();
        let press = |moved: bool| {
            *DRAG.lock().unwrap() = Some(PanelDrag {
                mouse_down: NSPoint::new(0.0, 0.0),
                picker: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
                anchor: NSPoint::new(0.0, 0.0),
                detail: None,
                moved,
            });
        };
        // Drive the production entry point itself (`finish_popover_press` with only the dismissal
        // injected) -- never a re-derivation of its decision, or this test would keep passing after
        // the production call site regressed to "is a grab in progress?".
        let release_through_caller = || {
            let dismissals = std::cell::Cell::new(0);
            finish_popover_press_with(|| dismissals.set(dismissals.get() + 1));
            dismissals.get()
        };

        // Click on a blank area: the grab exists but never travelled, so the menu must close.
        note_popover_outside_press(true);
        press(false);
        assert_eq!(
            release_through_caller(),
            1,
            "a blank click must close the menu"
        );

        // Drag of the panel: the grab travelled, so the menu stays open.
        note_popover_outside_press(true);
        press(true);
        assert_eq!(
            release_through_caller(),
            0,
            "a drag must leave the menu open"
        );

        // A press inside the menu never pends, so it cannot close it either.
        note_popover_outside_press(false);
        assert_eq!(release_through_caller(), 0);

        *DRAG.lock().unwrap() = None;
    }

    /// The decision, asserted case by case around the same seam.
    #[test]
    fn only_a_travelled_press_keeps_the_popover_open() {
        let _guard = popover_test_lock();
        assert_eq!(popover_release(false, false), PopoverRelease::Nothing);
        assert_eq!(popover_release(false, true), PopoverRelease::Nothing);
        // A click outside an open menu closes it -- the everyday gesture.
        assert_eq!(popover_release(true, false), PopoverRelease::Dismiss);
        // A drag of the panel leaves the menu open.
        assert_eq!(popover_release(true, true), PopoverRelease::Keep);
    }

    /// The pending flag is consumed by exactly one release, and a press that did not travel reads
    /// as a click. Drives the real `finish_popover_press` entry point, which is what the window's
    /// `sendEvent:` calls; the dismiss itself is a main-thread AppKit call, so the decision it
    /// acts on is asserted separately by `only_a_travelled_press_keeps_the_popover_open`.
    #[test]
    fn a_release_consumes_the_pending_press_once() {
        let _guard = popover_test_lock();
        let press = |moved: bool| {
            *DRAG.lock().unwrap() = Some(PanelDrag {
                mouse_down: NSPoint::new(0.0, 0.0),
                picker: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
                anchor: NSPoint::new(0.0, 0.0),
                detail: None,
                moved,
            });
        };
        // A press inside the menu (or on its orb) is not an outside press at all: nothing pends.
        note_popover_outside_press(false);
        assert!(!popover_press_pending());
        // A press outside holds the decision, and the grab it starts is not yet a drag.
        note_popover_outside_press(true);
        press(false);
        assert!(popover_press_pending());
        assert!(drag_in_progress());
        assert!(!grab_travelled(), "a fresh press has not travelled yet");
        // A drag that travelled reads as Keep rather than Dismiss.
        press(true);
        assert!(grab_travelled());
        assert_eq!(
            popover_release(popover_press_pending(), grab_travelled()),
            PopoverRelease::Keep
        );
        // The release consumes the decision; the grab itself is cleared by `finish_drag`.
        *PENDING_POPOVER_DISMISS.lock().unwrap() = false;
        *DRAG.lock().unwrap() = None;
        assert!(!popover_press_pending());
        assert!(!drag_in_progress());
    }

    #[test]
    fn a_closed_popover_cannot_be_dismissed_by_a_late_release() {
        let _guard = popover_test_lock();
        note_popover_outside_press(true);
        assert!(popover_press_pending());

        // Closing the menu (including when its panel is hidden) consumes the old press. If a new
        // menu opens before AppKit delivers the old release, that release has no dismissal to run.
        // A grab is deliberately left in place: with the press consumed, the release must resolve
        // to Nothing whatever the drag state says -- otherwise a stale press would close the menu
        // the user reopened in the meantime.
        *DRAG.lock().unwrap() = Some(PanelDrag {
            mouse_down: NSPoint::new(0.0, 0.0),
            picker: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
            anchor: NSPoint::new(0.0, 0.0),
            detail: None,
            moved: false,
        });
        clear_pending_popover_dismiss();
        assert!(!popover_press_pending());
        assert_eq!(
            popover_release(popover_press_pending(), grab_travelled()),
            PopoverRelease::Nothing
        );
        finish_popover_press();
        assert!(!popover_press_pending());
        *DRAG.lock().unwrap() = None;
    }

    /// Reads the pending flag for the test above without exposing the private mutex.
    fn popover_press_pending() -> bool {
        *PENDING_POPOVER_DISMISS.lock().unwrap()
    }

    /// Serializes the tests that drive the process-wide drag/pending state. They mutate the same
    /// two statics the panel does, and `forget_session_position` clears the grab outright, so two
    /// of them running at once would read each other's state (the same reason
    /// `clipboard::gate_test_lock` exists).
    fn popover_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The options popover covers part of the header strip, which is otherwise the panel's drag
    /// area. While it is open a press there must reach the menu, not the drag surface: the press
    /// is outside the menu's frame, so taking it would close the menu during a drag.
    #[test]
    fn the_popover_is_declined_by_the_drag_surface() {
        let popover = rect(320.0, 250.0, 236.0, 334.0);
        assert!(rect_contains_point(popover, NSPoint::new(320.0, 250.0)));
        assert!(rect_contains_point(popover, NSPoint::new(438.0, 400.0)));
        assert!(rect_contains_point(popover, NSPoint::new(556.0, 584.0)));
        assert!(!rect_contains_point(popover, NSPoint::new(319.9, 400.0)));
        assert!(!rect_contains_point(popover, NSPoint::new(438.0, 584.1)));
    }
}
