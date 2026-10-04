//! Disabled-setting hint component: tooltips, the not-allowed cursor, and hover tracking.

use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::{LazyLock, Mutex, OnceLock};

/// Disabled rows own their tracking areas through the corresponding AppKit view. Store only
/// addresses so the registry never carries raw pointers across a thread boundary.
static DISABLED_TRACKING_AREAS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Tooltip text is kept separately so a click can resolve the disabled view to its hint.
static DISABLED_TOOLTIPS: LazyLock<Mutex<HashMap<usize, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// At most one custom bubble is visible in the settings window at a time.
static ACTIVE_BUBBLE: Mutex<Option<usize>> = Mutex::new(None);

/// The current dismissal timer; a new click replaces the old timer instead of racing it.
static ACTIVE_TIMER: Mutex<Option<usize>> = Mutex::new(None);

/// Views that expose their full text while the pointer rests on them (a truncated caption), and the
/// tracking areas that drive them. Kept by view address like the disabled hints. Native
/// `setToolTip:` is deliberately not used for these: AppKit owns the help-tag window it creates and
/// leaves it on screen when the owning page is hidden or rebuilt, so the tag can be stranded at the
/// screen origin (2026-10-04).
static HOVER_TOOLTIPS: LazyLock<Mutex<HashMap<usize, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static HOVER_TRACKING_AREAS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The pending hover reveal (view address) and its delay timer. Sweeping the pointer across rows
/// must not flash a bubble, so the reveal waits `HOVER_REVEAL_DELAY` and is cancelled on exit.
static HOVER_PENDING: Mutex<Option<usize>> = Mutex::new(None);
static HOVER_TIMER: Mutex<Option<usize>> = Mutex::new(None);
/// The view whose hover tooltip is currently on screen, so an exit event can tell whether the bubble
/// it sees belongs to the view the pointer is leaving.
static HOVER_SHOWN: Mutex<Option<usize>> = Mutex::new(None);
const HOVER_REVEAL_DELAY: f64 = 0.6;

/// The bubble is a passive overlay; keeping hit testing disabled ensures it never blocks the
/// controls underneath it while it is visible.
fn tooltip_bubble_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabSettingsTooltipBubble").unwrap();
        let superclass = objc2::class!(NSView) as *const _ as *mut AnyObject;
        let cls = crate::ffi::objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("@@:{CGPoint=dd}").unwrap();
        crate::ffi::class_addMethod(
            cls,
            objc2::sel!(hitTest:),
            tooltip_bubble_hit_test as *mut c_void,
            types.as_ptr(),
        );
        crate::ffi::objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

extern "C" fn tooltip_bubble_hit_test(
    _self: *mut c_void,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    std::ptr::null_mut()
}

struct DisabledCursorTarget(*mut AnyObject);
unsafe impl Send for DisabledCursorTarget {}
unsafe impl Sync for DisabledCursorTarget {}

static DISABLED_CURSOR_TARGET: OnceLock<DisabledCursorTarget> = OnceLock::new();

/// The view a tracking-area event names, or null when the userInfo did not carry a view pointer.
unsafe fn tracking_area_view(event: *mut AnyObject) -> *mut AnyObject {
    if event.is_null() {
        return std::ptr::null_mut();
    }
    let tracking: *mut AnyObject = objc2::msg_send![event, trackingArea];
    if tracking.is_null() {
        return std::ptr::null_mut();
    }
    let user_info: *mut AnyObject = objc2::msg_send![tracking, userInfo];
    if user_info.is_null() {
        return std::ptr::null_mut();
    }
    let pointer: *mut c_void = objc2::msg_send![user_info, pointerValue];
    pointer as *mut AnyObject
}

extern "C" fn disabled_cursor_mouse_entered(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let view = tracking_area_view(event as *mut AnyObject);
        let enabled = if view.is_null() {
            true
        } else if objc2::msg_send![view, respondsToSelector: objc2::sel!(isEnabled)] {
            let state: bool = objc2::msg_send![view, isEnabled];
            state
        } else {
            false
        };
        if !enabled {
            let cursor: *mut AnyObject =
                objc2::msg_send![objc2::class!(NSCursor), operationNotAllowedCursor];
            let _: () = objc2::msg_send![cursor, set];
        }
    }
}

extern "C" fn disabled_cursor_mouse_exited(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let cursor: *mut AnyObject = objc2::msg_send![objc2::class!(NSCursor), arrowCursor];
        let _: () = objc2::msg_send![cursor, set];
    }
}

extern "C" fn tooltip_timeout(_self: *mut c_void, _cmd: Sel, timer: *mut c_void) {
    unsafe {
        let timer = timer as *mut AnyObject;
        let is_active = ACTIVE_TIMER
            .lock()
            .unwrap()
            .is_some_and(|active| active == timer as usize);
        if is_active {
            ACTIVE_TIMER.lock().unwrap().take();
            SettingsTooltip::dismiss_bubble();
        }
    }
}

struct HoverTooltipTarget(*mut AnyObject);
unsafe impl Send for HoverTooltipTarget {}
unsafe impl Sync for HoverTooltipTarget {}

static HOVER_TOOLTIP_TARGET: OnceLock<HoverTooltipTarget> = OnceLock::new();

extern "C" fn hover_tooltip_mouse_entered(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let view = tracking_area_view(event as *mut AnyObject);
        if view.is_null() {
            return;
        }
        // A new hover replaces the pending reveal.
        SettingsTooltip::cancel_hover_timer();
        *HOVER_PENDING.lock().unwrap() = Some(view as usize);
        let timer: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSTimer),
            scheduledTimerWithTimeInterval: HOVER_REVEAL_DELAY,
            target: hover_tooltip_target(),
            selector: objc2::sel!(revealHoverTooltip:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: false
        ];
        *HOVER_TIMER.lock().unwrap() = Some(timer as usize);
    }
}

/// What an exit event is allowed to tear down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct HoverExitEffect {
    cancel_pending: bool,
    hide_bubble: bool,
}

/// Resolve an exit event against the current hover state.
///
/// AppKit does not promise that the exit of the area the pointer left arrives before the enter of the
/// area it entered, so an exit may only tear down state that names *its own* view. Otherwise moving
/// from caption A to caption B, when A's exit is delivered last, would cancel the reveal B had
/// already armed and B's tooltip would never appear. Pure, so the ordering rule is unit-tested.
///
/// An exit with no attributable view (our tracking areas always carry one, so this is unexpected)
/// falls back to the unconditional cleanup: leaving a bubble stuck on screen is worse than a missing
/// hint.
fn hover_exit_effect(
    exiting: Option<usize>,
    pending: Option<usize>,
    shown: Option<usize>,
) -> HoverExitEffect {
    match exiting {
        None => HoverExitEffect {
            cancel_pending: true,
            hide_bubble: true,
        },
        Some(view) => HoverExitEffect {
            cancel_pending: pending == Some(view),
            hide_bubble: shown == Some(view),
        },
    }
}

extern "C" fn hover_tooltip_mouse_exited(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let exiting = {
            let view = tracking_area_view(event as *mut AnyObject);
            (!view.is_null()).then_some(view as usize)
        };
        let effect = hover_exit_effect(
            exiting,
            *HOVER_PENDING.lock().unwrap(),
            *HOVER_SHOWN.lock().unwrap(),
        );
        if effect.cancel_pending {
            SettingsTooltip::cancel_hover_timer();
        }
        if effect.hide_bubble {
            SettingsTooltip::hide_bubble();
        }
    }
}

/// Whether a text field's string is wider than its frame, i.e. the label is showing an ellipsis.
/// Only text that actually truncates needs a hover outlet; a fully visible caption must not pop a
/// redundant bubble.
unsafe fn text_is_truncated(view: *mut AnyObject) -> bool {
    let bounds: NSRect = objc2::msg_send![view, bounds];
    if bounds.size.width <= 0.0 {
        return false;
    }
    let cell: *mut AnyObject = objc2::msg_send![view, cell];
    if cell.is_null() {
        return false;
    }
    let measured: NSSize = objc2::msg_send![
        cell,
        cellSizeForBounds: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(1000.0, bounds.size.height),
        )
    ];
    measured.width > bounds.size.width + 0.5
}

/// The registered hover view under the pointer, resolved from the pointer rather than from the
/// tracking event that armed the timer. A tracking event can name a different label than the one
/// the pointer actually rests on (for example while content scrolls under a stationary pointer),
/// which would surface the wrong text.
unsafe fn hover_view_under_pointer(window: *mut AnyObject) -> Option<(usize, String)> {
    if window.is_null() {
        return None;
    }
    let point: NSPoint = objc2::msg_send![window, mouseLocationOutsideOfEventStream];
    let mut best: Option<(usize, String, f64)> = None;
    for (address, text) in HOVER_TOOLTIPS.lock().unwrap().iter() {
        let view = *address as *mut AnyObject;
        let view_window: *mut AnyObject = objc2::msg_send![view, window];
        if view_window != window {
            continue;
        }
        let hidden: bool = objc2::msg_send![view, isHiddenOrHasHiddenAncestor];
        if hidden || !text_is_truncated(view) {
            continue;
        }
        let local: NSPoint = objc2::msg_send![
            view,
            convertPoint: point,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let bounds: NSRect = objc2::msg_send![view, bounds];
        let inside = local.x >= bounds.origin.x
            && local.x <= bounds.origin.x + bounds.size.width
            && local.y >= bounds.origin.y
            && local.y <= bounds.origin.y + bounds.size.height;
        if inside {
            // The smallest matching label wins if two ever overlap.
            let area = bounds.size.width * bounds.size.height;
            if best
                .as_ref()
                .is_none_or(|(_, _, best_area)| area < *best_area)
            {
                best = Some((*address, text.clone(), area));
            }
        }
    }
    best.map(|(address, text, _)| (address, text))
}

/// Show the tooltip the pointer has been resting on. The delay only ever holds a view *address*;
/// the label under the pointer is resolved afresh here, so a page that was rebuilt or torn down
/// during the delay simply has no entry and nothing is shown.
extern "C" fn hover_tooltip_reveal(_self: *mut c_void, _cmd: Sel, timer: *mut c_void) {
    unsafe {
        let timer_addr = timer as usize;
        let is_active = HOVER_TIMER
            .lock()
            .unwrap()
            .is_some_and(|active| active == timer_addr);
        if !is_active {
            return;
        }
        HOVER_TIMER.lock().unwrap().take();
        let Some(pending) = HOVER_PENDING.lock().unwrap().take() else {
            return;
        };
        // Only proceed while the view that armed the timer is still one of ours; its window names
        // where to resolve the pointer. A torn-down page leaves no entry and shows nothing.
        if !HOVER_TOOLTIPS.lock().unwrap().contains_key(&pending) {
            return;
        }
        let pending_view = pending as *mut AnyObject;
        let window: *mut AnyObject = objc2::msg_send![pending_view, window];
        let Some((view_addr, text)) = hover_view_under_pointer(window) else {
            return;
        };
        SettingsTooltip::show_hover_bubble(view_addr as *mut AnyObject, &text);
        *HOVER_SHOWN.lock().unwrap() = Some(view_addr);
        // A hover tooltip stays while the pointer rests on the view; `mouseExited:` hides it (and
        // `dismiss`/`clear_runtime_registries` cover the teardown paths). The bubble's own
        // auto-dismiss timer belongs to the click/success toasts.
        SettingsTooltip::cancel_timer();
    }
}

fn hover_tooltip_target() -> *mut AnyObject {
    HOVER_TOOLTIP_TARGET
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabHoverTooltipTarget").unwrap();
            let superclass = objc2::class!(NSObject) as *const _ as *mut AnyObject;
            let cls = crate::ffi::objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            crate::ffi::class_addMethod(
                cls,
                objc2::sel!(mouseEntered:),
                hover_tooltip_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            crate::ffi::class_addMethod(
                cls,
                objc2::sel!(mouseExited:),
                hover_tooltip_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            crate::ffi::class_addMethod(
                cls,
                objc2::sel!(revealHoverTooltip:),
                hover_tooltip_reveal as *mut c_void,
                types.as_ptr(),
            );
            crate::ffi::objc_registerClassPair(cls);
            let target: *mut AnyObject = objc2::msg_send![cls, new];
            HoverTooltipTarget(target)
        })
        .0
}

fn disabled_cursor_target() -> *mut AnyObject {
    DISABLED_CURSOR_TARGET
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabDisabledCursorTarget").unwrap();
            let superclass = objc2::class!(NSObject) as *const _ as *mut AnyObject;
            let cls = crate::ffi::objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            crate::ffi::class_addMethod(
                cls,
                objc2::sel!(mouseEntered:),
                disabled_cursor_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            crate::ffi::class_addMethod(
                cls,
                objc2::sel!(mouseExited:),
                disabled_cursor_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            crate::ffi::class_addMethod(
                cls,
                objc2::sel!(hideTooltip:),
                tooltip_timeout as *mut c_void,
                types.as_ptr(),
            );
            crate::ffi::objc_registerClassPair(cls);
            let target: *mut AnyObject = objc2::msg_send![cls, new];
            DisabledCursorTarget(target)
        })
        .0
}

/// Shared disabled-setting hint behavior.
pub(super) struct SettingsTooltip;

/// Bubble shapes. The hint and the toast are the fixed footprint the design specifies; the hover
/// tooltip is the one that must grow, because its whole purpose is showing a string that did not
/// fit where it was drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BubbleKind {
    /// Disabled-setting click hint: fixed size, single line.
    Hint,
    /// Success toast: fixed size, single line.
    Success,
    /// Hover tooltip: wraps and sizes to its content.
    Hover,
}

impl BubbleKind {
    /// Whether the text wraps instead of being tail-truncated on one line.
    fn wrapping(self) -> bool {
        matches!(self, Self::Hover)
    }
}

/// Fixed bubble footprint (the design's toast: 288x66, ~80% of the 360x82 reference).
const TOAST_WIDTH: f64 = 288.0;
const TOAST_HEIGHT: f64 = 66.0;
/// Icon block inside every bubble.
const TOOLTIP_ICON_SIZE: f64 = 28.0;
const TOOLTIP_ICON_GAP: f64 = 12.0;
const TOOLTIP_LINE_HEIGHT: f64 = 20.0;
/// Hover bubble padding and the widest text it will lay out on one line before wrapping. The cap
/// keeps the whole bubble comfortably inside the fixed 820pt settings window, and the vertical pad is
/// the toast's own letterboxing (`TOAST_HEIGHT - line`) so the two bubbles share one rhythm.
const HOVER_HORIZONTAL_PADDING: f64 = 28.0;
const HOVER_VERTICAL_PADDING: f64 = (TOAST_HEIGHT - TOOLTIP_LINE_HEIGHT) / 2.0;
const HOVER_MAX_TEXT_WIDTH: f64 = 420.0;

/// The frame and footprint a bubble takes for a measured text extent.
#[derive(Clone, Copy, PartialEq, Debug)]
struct BubbleMetrics {
    size: NSSize,
    text_size: NSSize,
}

/// Bubble metrics for text whose single-line extent is `natural_width` and whose height after
/// wrapping at [`HOVER_MAX_TEXT_WIDTH`] is `wrapped_height` (both measured at the tooltip font; the
/// wrapped height is ignored unless the text actually wraps). Pure, so the sizing rule is unit-tested.
///
/// The invariant that matters: the hover bubble's text frame is never narrower than the text needs.
/// Under the cap the label is the natural single-line width, at the cap the text wraps and the height
/// grows instead. The previous fixed 288pt bubble left a 220pt text budget with tail truncation, so
/// the English and Traditional-Chinese "delete after paste" captions -- 313.8pt and 297.9pt, and
/// truncated in their 286pt row in the first place -- were truncated a second time and the outlet
/// revealed nothing.
fn bubble_metrics(kind: BubbleKind, natural_width: f64, wrapped_height: f64) -> BubbleMetrics {
    match kind {
        BubbleKind::Hint | BubbleKind::Success => BubbleMetrics {
            size: NSSize::new(TOAST_WIDTH, TOAST_HEIGHT),
            text_size: NSSize::new(
                natural_width.clamp(
                    1.0,
                    TOAST_WIDTH - HOVER_HORIZONTAL_PADDING - TOOLTIP_ICON_SIZE - TOOLTIP_ICON_GAP,
                ),
                TOOLTIP_LINE_HEIGHT,
            ),
        },
        BubbleKind::Hover => {
            let wraps = natural_width > HOVER_MAX_TEXT_WIDTH;
            let text_size = NSSize::new(
                natural_width.clamp(1.0, HOVER_MAX_TEXT_WIDTH),
                if wraps {
                    wrapped_height.ceil().max(TOOLTIP_LINE_HEIGHT)
                } else {
                    TOOLTIP_LINE_HEIGHT
                },
            );
            BubbleMetrics {
                size: NSSize::new(
                    HOVER_HORIZONTAL_PADDING
                        + TOOLTIP_ICON_SIZE
                        + TOOLTIP_ICON_GAP
                        + text_size.width,
                    (text_size.height + HOVER_VERTICAL_PADDING * 2.0).max(TOAST_HEIGHT),
                ),
                text_size,
            }
        }
    }
}

/// What a bubble's label needs for a given shape, measured on the live AppKit cell.
struct BubbleTextMetrics {
    natural_width: f64,
    wrapped_height: f64,
}

impl SettingsTooltip {
    /// Measure bubble text at the tooltip font: its single-line extent, and its height when wrapped
    /// at [`HOVER_MAX_TEXT_WIDTH`].
    unsafe fn measure_bubble_text(text: &str) -> BubbleTextMetrics {
        let fallback = BubbleTextMetrics {
            natural_width: 1.0,
            wrapped_height: TOOLTIP_LINE_HEIGHT,
        };
        let probe: *mut AnyObject = objc2::msg_send![objc2::class!(NSTextField), alloc];
        let probe: *mut AnyObject = objc2::msg_send![
            probe,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0))
        ];
        if probe.is_null() {
            return fallback;
        }
        let text_ns = crate::ffi::make_nsstring(text);
        let _: () = objc2::msg_send![probe, setStringValue: text_ns];
        crate::ffi::release_obj(text_ns);
        let _: () = objc2::msg_send![probe, setBezeled: false];
        let _: () = objc2::msg_send![probe, setDrawsBackground: false];
        let _: () = objc2::msg_send![probe, setEditable: false];
        let font: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSFont),
            systemFontOfSize: crate::theme::FONT_CONTROL,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let _: () = objc2::msg_send![probe, setFont: font];
        let cell: *mut AnyObject = objc2::msg_send![probe, cell];
        if cell.is_null() {
            crate::ffi::release_obj(probe);
            return fallback;
        }

        // Unbounded single line: the natural extent of the string.
        let _: () = objc2::msg_send![probe, setUsesSingleLineMode: true];
        let _: () = objc2::msg_send![probe, setLineBreakMode: 4isize];
        let natural: NSSize = objc2::msg_send![
            cell,
            cellSizeForBounds: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1000.0, TOOLTIP_LINE_HEIGHT))
        ];

        // Then the same string with the wrapping settings the hover label renders with, so the frame
        // it is given cannot clip it.
        let _: () = objc2::msg_send![probe, setUsesSingleLineMode: false];
        let _: () = objc2::msg_send![probe, setLineBreakMode: 0isize];
        if objc2::msg_send![probe, respondsToSelector: objc2::sel!(setMaximumNumberOfLines:)] {
            let _: () = objc2::msg_send![probe, setMaximumNumberOfLines: 0isize];
        }
        let wrapped: NSSize = objc2::msg_send![
            cell,
            cellSizeForBounds: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(HOVER_MAX_TEXT_WIDTH, 1000.0),
            )
        ];
        crate::ffi::release_obj(probe);
        BubbleTextMetrics {
            natural_width: natural.width.max(1.0),
            wrapped_height: wrapped.height.max(TOOLTIP_LINE_HEIGHT),
        }
    }
}

impl SettingsTooltip {
    unsafe fn set_disabled_tracking(view: *mut AnyObject, disabled: bool) {
        if view.is_null() {
            return;
        }
        let key = view as usize;
        if disabled {
            let mut areas = DISABLED_TRACKING_AREAS.lock().unwrap();
            if areas.contains_key(&key) {
                return;
            }
            let bounds: NSRect = objc2::msg_send![view, bounds];
            let user_info: *mut AnyObject = objc2::msg_send![
                objc2::class!(NSValue),
                valueWithPointer: view as *mut c_void
            ];
            let area: *mut AnyObject = objc2::msg_send![objc2::class!(NSTrackingArea), alloc];
            let area: *mut AnyObject = objc2::msg_send![
                area,
                initWithRect: bounds,
                options: 0x01u64 | 0x80u64,
                owner: disabled_cursor_target(),
                userInfo: user_info
            ];
            let _: () = objc2::msg_send![view, addTrackingArea: area];
            crate::ffi::release_obj(area);
            areas.insert(key, area as usize);
        } else if let Some(area) = DISABLED_TRACKING_AREAS.lock().unwrap().remove(&key) {
            let _: () = objc2::msg_send![view, removeTrackingArea: area as *mut AnyObject];
            let cursor: *mut AnyObject = objc2::msg_send![objc2::class!(NSCursor), arrowCursor];
            let _: () = objc2::msg_send![cursor, set];
        }
    }

    unsafe fn cancel_timer() {
        let timer = ACTIVE_TIMER.lock().unwrap().take();
        if let Some(timer) = timer {
            let _: () = objc2::msg_send![timer as *mut AnyObject, invalidate];
        }
    }

    /// Invalidate a pending hover reveal and forget the view it named.
    unsafe fn cancel_hover_timer() {
        let timer = HOVER_TIMER.lock().unwrap().take();
        if let Some(timer) = timer {
            let _: () = objc2::msg_send![timer as *mut AnyObject, invalidate];
        }
        HOVER_PENDING.lock().unwrap().take();
    }

    unsafe fn hide_bubble() {
        Self::cancel_timer();
        Self::dismiss_bubble();
    }

    /// Tooltips disappear immediately so pointer movement remains direct.
    unsafe fn dismiss_bubble() {
        *HOVER_SHOWN.lock().unwrap() = None;
        let active = ACTIVE_BUBBLE.lock().unwrap().take();
        let Some(bubble) = active else {
            return;
        };
        let bubble = bubble as *mut AnyObject;
        let _: () = objc2::msg_send![bubble, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![bubble, removeFromSuperview];
    }

    unsafe fn show_bubble(view: *mut AnyObject, text: &str) {
        if view.is_null() {
            return;
        }
        let window: *mut AnyObject = objc2::msg_send![view, window];
        Self::show_bubble_in_window(window, text, BubbleKind::Hint);
    }

    /// Show a transient success message in the settings window.
    pub(super) unsafe fn show_success_bubble(window: *mut AnyObject, text: &str) {
        Self::show_bubble_in_window(window, text, BubbleKind::Success);
    }

    /// Show the full text of `view` while the pointer rests on it.
    ///
    /// Unlike the fixed-shape hint and toast, this bubble wraps its text and grows to fit it, so a
    /// caption that truncates in its row (or in the fixed-width settings window) is still readable
    /// in full here. A fixed bubble would truncate the same string a second time and the outlet
    /// would be worthless.
    unsafe fn show_hover_bubble(view: *mut AnyObject, text: &str) {
        if view.is_null() {
            return;
        }
        let window: *mut AnyObject = objc2::msg_send![view, window];
        Self::show_bubble_in_window(window, text, BubbleKind::Hover);
    }

    /// Assert on the rendered bubble that a hover tooltip shows each of `texts` in full.
    ///
    /// Debug smoke hook: it renders the real bubble for the given strings and compares the label's
    /// frame against the text's natural extent, so a future sizing change that reintroduces
    /// truncation fails a gate run instead of being noticed as a cut-off tooltip. Release builds
    /// report success without touching the view tree, matching `debug_validate_settings_page`; the
    /// check must be runtime-gated so the smoke runner keeps compiling in release.
    pub(super) unsafe fn debug_hover_bubble_holds_captions(
        window: *mut AnyObject,
        texts: &[String],
    ) -> bool {
        if !cfg!(debug_assertions) || window.is_null() {
            return true;
        }
        for text in texts {
            if text.is_empty() {
                continue;
            }
            // `window` is the window, not a view: go straight to the window-scoped entry point
            // (deriving the window from the argument would send `-window` to an NSWindow).
            Self::show_bubble_in_window(window, text, BubbleKind::Hover);
            let active = *ACTIVE_BUBBLE.lock().unwrap();
            let Some(bubble_addr) = active else {
                crate::log_info!("[tooltip] hover bubble missing for {text:?}");
                return false;
            };
            let bubble = bubble_addr as *mut AnyObject;
            let subviews: *mut AnyObject = objc2::msg_send![bubble, subviews];
            let count: usize = objc2::msg_send![subviews, count];
            let mut label: *mut AnyObject = std::ptr::null_mut();
            for index in 0..count {
                let child: *mut AnyObject =
                    objc2::msg_send![subviews, objectAtIndex: index as isize];
                if objc2::msg_send![child, isKindOfClass: objc2::class!(NSTextField)] {
                    label = child;
                }
            }
            if label.is_null() {
                crate::log_info!("[tooltip] hover bubble has no label for {text:?}");
                return false;
            }
            // The label must be big enough for the string at its rendered (possibly wrapped) size.
            let measured = Self::measure_bubble_text(text);
            let frame: NSRect = objc2::msg_send![label, frame];
            let expected = bubble_metrics(
                BubbleKind::Hover,
                measured.natural_width,
                measured.wrapped_height,
            );
            if frame.size.width + 0.5 < expected.text_size.width
                || frame.size.height + 0.5 < expected.text_size.height
            {
                crate::log_info!(
                    "[tooltip] hover bubble label {:.1}x{:.1} cannot hold {:.1}x{:.1} for {text:?}",
                    frame.size.width,
                    frame.size.height,
                    expected.text_size.width,
                    expected.text_size.height
                );
                return false;
            }
            Self::dismiss_bubble();
        }
        true
    }

    unsafe fn show_bubble_in_window(window: *mut AnyObject, text: &str, kind: BubbleKind) {
        if window.is_null() || text.is_empty() {
            return;
        }
        Self::hide_bubble();

        let content: *mut AnyObject = if window.is_null() {
            std::ptr::null_mut()
        } else {
            objc2::msg_send![window, contentView]
        };
        if content.is_null() {
            return;
        }

        // Center the bubble across the entire settings window and keep it near the lower edge,
        // matching the toast placement in the reference while staying above the footer area.
        let content_bounds: NSRect = objc2::msg_send![content, bounds];
        let palette = crate::theme::ui_palette();
        let success = kind == BubbleKind::Success;

        // Text metrics come first: the hover bubble is sized from its own text, so the frame can only
        // be computed once the text is measured in its final wrapping mode.
        let measured = Self::measure_bubble_text(text);
        let metrics = bubble_metrics(kind, measured.natural_width, measured.wrapped_height);
        let bubble_size = metrics.size;
        let text_size = metrics.text_size;
        let centered_x =
            content_bounds.origin.x + (content_bounds.size.width - bubble_size.width) / 2.0;
        let min_x = content_bounds.origin.x + 8.0;
        let max_x = (content_bounds.origin.x + content_bounds.size.width - bubble_size.width - 8.0)
            .max(min_x);
        let x = centered_x.clamp(min_x, max_x);
        let y = (content_bounds.origin.y + 74.0).clamp(
            content_bounds.origin.y + 8.0,
            (content_bounds.origin.y + content_bounds.size.height - bubble_size.height - 8.0)
                .max(content_bounds.origin.y + 8.0),
        );

        let bubble: *mut AnyObject = objc2::msg_send![tooltip_bubble_view_class(), alloc];
        let bubble: *mut AnyObject = objc2::msg_send![
            bubble,
            initWithFrame: NSRect::new(NSPoint::new(x, y), bubble_size)
        ];
        // Keep the centered bubble anchored to the bottom when the resizable settings window
        // changes height or width.
        let _: () = objc2::msg_send![bubble, setAutoresizingMask: 1u64 | 4u64 | 32u64];
        let _: () = objc2::msg_send![bubble, setOpaque: false];
        let _: () = objc2::msg_send![bubble, setAlphaValue: 1.0f64];
        let _: () = objc2::msg_send![bubble, setWantsLayer: true];
        let layer: *mut AnyObject = objc2::msg_send![bubble, layer];
        if !layer.is_null() {
            // A tooltip floats over page content, so it uses the shared card surface and med
            // elevation rather than a separate surface color.
            crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.card_bg));
            let _: () = objc2::msg_send![layer, setCornerRadius: crate::theme::RADIUS_PANEL];
            let _: () = objc2::msg_send![layer, setMasksToBounds: false];
            crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
            let _: () = objc2::msg_send![layer, setBorderWidth: 1.0f64];
            crate::ffi::layer_set_shadow_color(
                layer,
                crate::ffi::hex_to_cg_color(crate::theme::ELEVATION_MED_SHADOW_COLOR),
            );
            let _: () = objc2::msg_send![layer, setShadowOpacity: crate::theme::ELEVATION_MED_SHADOW_OPACITY];
            let _: () =
                objc2::msg_send![layer, setShadowRadius: crate::theme::ELEVATION_MED_SHADOW_RADIUS];
            let _: () = objc2::msg_send![layer, setShadowOffset: NSSize::new(0.0, crate::theme::ELEVATION_MED_SHADOW_OFFSET_Y)];
        }

        let mut icon_view: *mut AnyObject = std::ptr::null_mut();
        let icon_size = TOOLTIP_ICON_SIZE;
        let icon_inner_size = 14.0;
        let icon_gap = TOOLTIP_ICON_GAP;
        let symbol_ns = crate::ffi::make_nsstring(if success { "checkmark" } else { "info" });
        let image: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSImage),
            imageWithSystemSymbolName: symbol_ns,
            accessibilityDescription: std::ptr::null::<AnyObject>()
        ];
        crate::ffi::CFRelease(symbol_ns as *const c_void);
        if !image.is_null() {
            let tint_hex = if success {
                palette.success_text
            } else {
                palette.accent
            };
            let tint = crate::ffi::hex_to_ns_color(tint_hex);
            let icon_container: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
            let icon_container: *mut AnyObject = objc2::msg_send![
                icon_container,
                initWithFrame: NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(icon_size, icon_size),
                )
            ];
            let _: () = objc2::msg_send![icon_container, setWantsLayer: true];
            let icon_layer: *mut AnyObject = objc2::msg_send![icon_container, layer];
            if !icon_layer.is_null() {
                let tint_hex = if success {
                    palette.success_text & 0xFFFF_FF00 | if palette.dark { 0x26 } else { 0x20 }
                } else if palette.dark {
                    palette.accent & 0xFFFFFF00 | 0x26
                } else {
                    palette.accent & 0xFFFFFF00 | 0x20
                };
                crate::ffi::layer_set_background(icon_layer, crate::ffi::hex_to_cg_color(tint_hex));
                let _: () =
                    objc2::msg_send![icon_layer, setCornerRadius: crate::theme::RADIUS_CARD];
            }
            let icon: *mut AnyObject = objc2::msg_send![objc2::class!(NSImageView), alloc];
            let icon: *mut AnyObject = objc2::msg_send![
                icon,
                initWithFrame: NSRect::new(
                    NSPoint::new(
                        (icon_size - icon_inner_size) / 2.0,
                        (icon_size - icon_inner_size) / 2.0,
                    ),
                    NSSize::new(icon_inner_size, icon_inner_size),
                )
            ];
            let _: () = objc2::msg_send![icon, setImage: image];
            let _: () = objc2::msg_send![icon, setImageScaling: 3isize];
            let _: () = objc2::msg_send![icon, setContentTintColor: tint];
            let _: () = objc2::msg_send![icon_container, addSubview: icon];
            crate::ffi::release_obj(icon);
            let _: () = objc2::msg_send![bubble, addSubview: icon_container];
            icon_view = icon_container;
            crate::ffi::release_obj(icon_container);
        }

        let label: *mut AnyObject = objc2::msg_send![objc2::class!(NSTextField), alloc];
        let label: *mut AnyObject = objc2::msg_send![
            label,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), text_size)
        ];
        let text_ns = crate::ffi::make_nsstring(text);
        let _: () = objc2::msg_send![label, setStringValue: text_ns];
        crate::ffi::release_obj(text_ns);
        let _: () = objc2::msg_send![label, setBezeled: false];
        let _: () = objc2::msg_send![label, setDrawsBackground: false];
        let _: () = objc2::msg_send![label, setEditable: false];
        let _: () = objc2::msg_send![label, setAlignment: 0isize];
        // The hover bubble wraps and is measured with the same cell settings it renders with; the
        // fixed hint/toast keeps the single tail-truncated line it was designed as.
        let _: () = objc2::msg_send![label, setUsesSingleLineMode: !kind.wrapping()];
        let _: () = objc2::msg_send![label, setLineBreakMode: if kind.wrapping() { 0isize } else { 4isize }];
        if kind.wrapping()
            && objc2::msg_send![label, respondsToSelector: objc2::sel!(setMaximumNumberOfLines:)]
        {
            let _: () = objc2::msg_send![label, setMaximumNumberOfLines: 0isize];
        }
        let font: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSFont),
            systemFontOfSize: crate::theme::FONT_CONTROL,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let _: () = objc2::msg_send![label, setFont: font];
        let color = crate::ffi::hex_to_ns_color(palette.primary_text);
        let _: () = objc2::msg_send![label, setTextColor: color];

        // Center the icon and the measured text as one group, keeping their gap stable for every
        // localized message instead of centering the text in the remaining bubble width.
        let text_width = text_size.width;
        let text_height = text_size.height;
        let group_width = icon_size + icon_gap + text_width;
        let group_x = ((bubble_size.width - group_width) / 2.0).max(HOVER_HORIZONTAL_PADDING / 2.0);
        if !icon_view.is_null() {
            let _: () = objc2::msg_send![
                icon_view,
                setFrame: NSRect::new(
                    NSPoint::new(group_x, (bubble_size.height - icon_size) / 2.0),
                    NSSize::new(icon_size, icon_size),
                )
            ];
        }
        let _: () = objc2::msg_send![
            label,
            setFrame: NSRect::new(
                NSPoint::new(group_x + icon_size + icon_gap, (bubble_size.height - text_height) / 2.0),
                NSSize::new(text_width, text_height),
            )
        ];
        let _: () = objc2::msg_send![bubble, addSubview: label];
        crate::ffi::release_obj(label);
        let _: () = objc2::msg_send![content, addSubview: bubble];
        crate::ffi::release_obj(bubble);
        *ACTIVE_BUBBLE.lock().unwrap() = Some(bubble as usize);

        let timer: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSTimer),
            scheduledTimerWithTimeInterval: 2.5f64,
            target: disabled_cursor_target(),
            selector: objc2::sel!(hideTooltip:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: false
        ];
        *ACTIVE_TIMER.lock().unwrap() = Some(timer as usize);
    }

    /// Apply disabled-state hover behavior and remember the click hint.
    pub(super) unsafe fn apply(view: *mut AnyObject, enabled: bool, tooltip: Option<&str>) {
        if view.is_null() {
            return;
        }
        if enabled {
            Self::hide_bubble();
        }
        Self::set_disabled_tracking(view, !enabled);
        let tooltip = (!enabled).then_some(tooltip).flatten();
        if let Some(text) = tooltip {
            DISABLED_TOOLTIPS
                .lock()
                .unwrap()
                .insert(view as usize, text.to_owned());
        } else {
            DISABLED_TOOLTIPS.lock().unwrap().remove(&(view as usize));
        }
    }

    /// Expose a view's full text while the pointer rests on it, through the app's own bubble.
    ///
    /// Used for text that may truncate (a row caption). The view keeps no native `toolTip`, so
    /// AppKit can never strand a help-tag window for it.
    pub(super) unsafe fn attach_hover(view: *mut AnyObject, text: &str) {
        if view.is_null() || text.is_empty() {
            return;
        }
        HOVER_TOOLTIPS
            .lock()
            .unwrap()
            .insert(view as usize, text.to_owned());
        Self::set_hover_tracking(view, true);
    }

    unsafe fn set_hover_tracking(view: *mut AnyObject, active: bool) {
        if view.is_null() {
            return;
        }
        let key = view as usize;
        let mut areas = HOVER_TRACKING_AREAS.lock().unwrap();
        if active {
            if areas.contains_key(&key) {
                return;
            }
            let bounds: NSRect = objc2::msg_send![view, bounds];
            let user_info: *mut AnyObject = objc2::msg_send![
                objc2::class!(NSValue),
                valueWithPointer: view as *mut c_void
            ];
            let area: *mut AnyObject = objc2::msg_send![objc2::class!(NSTrackingArea), alloc];
            // MouseEnteredAndExited (0x01) | ActiveInKeyWindow (0x20) | InVisibleRect (0x200):
            // the area follows the label through page scrolling and only fires for the window the
            // user is actually using.
            let area: *mut AnyObject = objc2::msg_send![
                area,
                initWithRect: bounds,
                options: 0x01u64 | 0x20u64 | 0x200u64,
                owner: hover_tooltip_target(),
                userInfo: user_info
            ];
            let _: () = objc2::msg_send![view, addTrackingArea: area];
            crate::ffi::release_obj(area);
            areas.insert(key, area as usize);
        } else if let Some(area) = areas.remove(&key) {
            let _: () = objc2::msg_send![view, removeTrackingArea: area as *mut AnyObject];
        }
    }

    /// Drop a view from the disabled-hint and tracking registries before it is destroyed.
    ///
    /// Both registries are keyed by the view's raw address and hold no ownership, so a view that
    /// is torn down without this call leaves a dangling key behind: the next mouse-down in the
    /// settings window (`handle_mouse_down`) then messages the freed object and traps.
    pub(super) unsafe fn forget(view: *mut AnyObject) {
        if view.is_null() {
            return;
        }
        DISABLED_TOOLTIPS.lock().unwrap().remove(&(view as usize));
        HOVER_TOOLTIPS.lock().unwrap().remove(&(view as usize));
        // The tracking area must also be removed while the view is alive, or the registry keeps
        // a key for it after the view goes away.
        Self::set_disabled_tracking(view, false);
        Self::set_hover_tracking(view, false);
    }

    /// Dismiss the current hint when navigation changes the visible settings page.
    pub(super) unsafe fn dismiss() {
        Self::hide_bubble();
    }

    /// Every view address reachable from `root`, so a registry key can be checked for liveness
    /// before it is messaged.
    unsafe fn live_view_addresses(root: *mut AnyObject) -> std::collections::HashSet<usize> {
        let mut live = std::collections::HashSet::new();
        let mut stack = vec![root];
        while let Some(view) = stack.pop() {
            if view.is_null() {
                continue;
            }
            if !live.insert(view as usize) {
                continue;
            }
            let subviews: *mut AnyObject = objc2::msg_send![view, subviews];
            if subviews.is_null() {
                continue;
            }
            let count: usize = objc2::msg_send![subviews, count];
            for index in 0..count {
                let child: *mut AnyObject =
                    objc2::msg_send![subviews, objectAtIndex: index as isize];
                if !child.is_null() {
                    stack.push(child);
                }
            }
        }
        live
    }

    /// Show the hint when a click lands on a disabled settings view; any other click hides it.
    pub(super) unsafe fn handle_mouse_down(window: *mut AnyObject, event: *mut AnyObject) {
        if window.is_null() || event.is_null() {
            return;
        }
        let content: *mut AnyObject = objc2::msg_send![window, contentView];
        if content.is_null() {
            Self::hide_bubble();
            return;
        }
        let window_point: NSPoint = objc2::msg_send![event, locationInWindow];
        let content_point: NSPoint = objc2::msg_send![
            content,
            convertPoint: window_point,
            fromView: std::ptr::null::<AnyObject>()
        ];
        // The registry is keyed by raw view addresses and holds no ownership. A view that was torn
        // down without `forget` (the binding rows are rebuilt as the mapping list changes) would
        // leave a dangling key, and messaging it below used to trap with EXC_BREAKPOINT (SIGTRAP)
        // in `object_getClass` -- the 2026-09-28 crash report. Only views that are still in the
        // window's hierarchy may be messaged, and a key that is gone is dropped here.
        let live = Self::live_view_addresses(content);
        let mut candidates: Vec<(usize, String)> = Vec::new();
        {
            let mut registry = DISABLED_TOOLTIPS.lock().unwrap();
            registry.retain(|view, text| {
                if live.contains(view) {
                    candidates.push((*view, text.clone()));
                    true
                } else {
                    false
                }
            });
        }
        // Resolve the actual AppKit hit view before checking candidates. Comparing the click
        // point with every disabled label's converted frame is too broad: a label can span most
        // of a row and overlap an unrelated action button (for example, Restore Defaults).
        let hit_view: *mut AnyObject = objc2::msg_send![content, hitTest: content_point];
        for (view_address, text) in candidates {
            let view = view_address as *mut AnyObject;
            let view_window: *mut AnyObject = objc2::msg_send![view, window];
            if view_window != window {
                continue;
            }

            // All settings pages share the same window and are hidden rather than destroyed.
            // Skip controls whose page is hidden, otherwise a hidden page can win this manual
            // coordinate lookup because its frame overlaps the visible page.
            let hidden: bool = objc2::msg_send![view, isHiddenOrHasHiddenAncestor];
            if hidden {
                continue;
            }

            // A row label or a wrapped button title may be a child of the registered control,
            // so walk up from the hit view instead of requiring pointer equality.
            let mut ancestor = hit_view;
            while !ancestor.is_null() {
                if ancestor == view {
                    let enabled = if objc2::msg_send![view, respondsToSelector: objc2::sel!(isEnabled)]
                    {
                        objc2::msg_send![view, isEnabled]
                    } else {
                        false
                    };
                    if !enabled {
                        Self::show_bubble(view, &text);
                        return;
                    }
                    break;
                }
                ancestor = objc2::msg_send![ancestor, superview];
            }
        }
        Self::hide_bubble();
    }

    /// Drop tracking state before settings views are deallocated.
    pub(super) fn clear_runtime_registries() {
        unsafe {
            Self::cancel_timer();
            Self::cancel_hover_timer();
            Self::dismiss_bubble();
        }
        DISABLED_TRACKING_AREAS.lock().unwrap().clear();
        DISABLED_TOOLTIPS.lock().unwrap().clear();
        HOVER_TRACKING_AREAS.lock().unwrap().clear();
        HOVER_TOOLTIPS.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod hover_exit_tests {
    use super::{hover_exit_effect, HoverExitEffect};

    const A: usize = 0x1000;
    const B: usize = 0x2000;

    /// The regression: pointer moves A -> B and AppKit delivers A's exit after B's enter. A's exit
    /// must not cancel B's armed reveal, or hovering B would show nothing.
    #[test]
    fn a_stale_exit_does_not_cancel_the_newer_hover() {
        assert_eq!(
            hover_exit_effect(Some(A), Some(B), None),
            HoverExitEffect {
                cancel_pending: false,
                hide_bubble: false,
            }
        );
    }

    /// A's own exit still cancels A's pending reveal and hides A's visible bubble.
    #[test]
    fn an_exit_owns_its_own_view_only() {
        assert_eq!(
            hover_exit_effect(Some(A), Some(A), None),
            HoverExitEffect {
                cancel_pending: true,
                hide_bubble: false,
            }
        );
        assert_eq!(
            hover_exit_effect(Some(A), None, Some(A)),
            HoverExitEffect {
                cancel_pending: false,
                hide_bubble: true,
            }
        );
        // A bubble shown for B survives A's exit.
        assert_eq!(
            hover_exit_effect(Some(A), None, Some(B)),
            HoverExitEffect {
                cancel_pending: false,
                hide_bubble: false,
            }
        );
    }

    /// An unattributable exit cleans up everything: a stuck bubble is worse than a missing hint.
    #[test]
    fn an_unattributable_exit_cleans_up_everything() {
        assert_eq!(
            hover_exit_effect(None, Some(B), Some(B)),
            HoverExitEffect {
                cancel_pending: true,
                hide_bubble: true,
            }
        );
    }
}

#[cfg(test)]
mod bubble_metrics_tests {
    use super::{bubble_metrics, BubbleKind, HOVER_MAX_TEXT_WIDTH, TOAST_HEIGHT, TOAST_WIDTH};

    /// The regression this rule exists for: the English and Traditional-Chinese "delete after paste"
    /// captions (measured 313.8pt and 297.9pt at the 12pt caption font) truncate in their 286pt row,
    /// and the old fixed 288pt bubble left only a 220pt text budget with tail truncation -- so the
    /// hover outlet truncated the very string it was added to reveal. The hover text frame must cover
    /// the full natural extent of anything up to the wrap cap.
    #[test]
    fn hover_bubble_shows_a_truncated_caption_in_full() {
        for width in [220.1, 286.0, 297.9, 313.8, HOVER_MAX_TEXT_WIDTH] {
            let metrics = bubble_metrics(BubbleKind::Hover, width, 20.0);
            assert!(
                metrics.text_size.width >= width,
                "text frame {}pt cannot hold a {width}pt string",
                metrics.text_size.width
            );
            // The bubble is the icon block plus the text plus symmetric padding.
            assert!(metrics.size.width > metrics.text_size.width);
        }
    }

    /// Past the cap the bubble must wrap and grow in height, never clip: a taller bubble is the only
    /// outcome that still shows the whole string.
    #[test]
    fn hover_bubble_wraps_and_grows_instead_of_clipping() {
        let one_line = bubble_metrics(BubbleKind::Hover, 200.0, 20.0);
        let wrapped = bubble_metrics(BubbleKind::Hover, HOVER_MAX_TEXT_WIDTH + 40.0, 57.0);
        assert_eq!(wrapped.text_size.width, HOVER_MAX_TEXT_WIDTH);
        assert_eq!(wrapped.text_size.height, 57.0);
        assert!(wrapped.size.height > one_line.size.height);
        // A wrapped bubble still keeps its fixed minimum footprint when the text is short.
        assert_eq!(one_line.size.height, TOAST_HEIGHT);
    }

    /// The hint and the success toast keep the fixed footprint they were designed with.
    #[test]
    fn hint_and_toast_stay_a_fixed_single_line() {
        let hint = bubble_metrics(BubbleKind::Hint, 400.0, 60.0);
        let toast = bubble_metrics(BubbleKind::Success, 400.0, 60.0);
        for metrics in [hint, toast] {
            assert_eq!(metrics.size.width, TOAST_WIDTH);
            assert_eq!(metrics.size.height, TOAST_HEIGHT);
            assert!(metrics.text_size.width < TOAST_WIDTH);
        }
    }
}
