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

extern "C" fn disabled_cursor_mouse_entered(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let event = event as *mut AnyObject;
        let tracking: *mut AnyObject = objc2::msg_send![event, trackingArea];
        let user_info: *mut AnyObject = objc2::msg_send![tracking, userInfo];
        let view: *mut AnyObject = if user_info.is_null() {
            std::ptr::null_mut()
        } else {
            let pointer: *mut c_void = objc2::msg_send![user_info, pointerValue];
            pointer as *mut AnyObject
        };
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

    unsafe fn hide_bubble() {
        Self::cancel_timer();
        Self::dismiss_bubble();
    }

    /// Tooltips disappear immediately so pointer movement remains direct.
    unsafe fn dismiss_bubble() {
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
        Self::show_bubble_in_window(window, text, false);
    }

    /// Show a transient success message in the settings window.
    pub(super) unsafe fn show_success_bubble(window: *mut AnyObject, text: &str) {
        Self::show_bubble_in_window(window, text, true);
    }

    unsafe fn show_bubble_in_window(window: *mut AnyObject, text: &str, success: bool) {
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
        // The example toast is 360x82; use roughly 80% of that footprint for this window.
        let bubble_width = 288.0;
        let bubble_size = NSSize::new(bubble_width, 66.0);
        let horizontal_padding = 28.0;
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
        let icon_size = 28.0;
        let icon_inner_size = 14.0;
        let icon_gap = 12.0;
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
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, (bubble_size.height - 20.0) / 2.0),
                NSSize::new(bubble_size.width, 20.0),
            )
        ];
        let text_ns = crate::ffi::make_nsstring(text);
        let _: () = objc2::msg_send![label, setStringValue: text_ns];
        crate::ffi::release_obj(text_ns);
        let _: () = objc2::msg_send![label, setBezeled: false];
        let _: () = objc2::msg_send![label, setDrawsBackground: false];
        let _: () = objc2::msg_send![label, setEditable: false];
        let _: () = objc2::msg_send![label, setAlignment: 0isize];
        let _: () = objc2::msg_send![label, setUsesSingleLineMode: true];
        let _: () = objc2::msg_send![label, setLineBreakMode: 4isize];
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
        let cell: *mut AnyObject = objc2::msg_send![label, cell];
        let measured: NSSize = if cell.is_null() {
            NSSize::new(0.0, 0.0)
        } else {
            objc2::msg_send![
                cell,
                cellSizeForBounds: NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(1000.0, 20.0),
                )
            ]
        };
        let max_text_width = bubble_size.width - horizontal_padding - icon_size - icon_gap;
        let text_width = measured.width.clamp(1.0, max_text_width);
        let group_width = icon_size + icon_gap + text_width;
        let group_x = ((bubble_size.width - group_width) / 2.0).max(horizontal_padding / 2.0);
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
                NSPoint::new(group_x + icon_size + icon_gap, (bubble_size.height - 20.0) / 2.0),
                NSSize::new(text_width, 20.0),
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
        // The tracking area must also be removed while the view is alive, or the registry keeps
        // a key for it after the view goes away.
        Self::set_disabled_tracking(view, false);
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
            Self::dismiss_bubble();
        }
        DISABLED_TRACKING_AREAS.lock().unwrap().clear();
        DISABLED_TOOLTIPS.lock().unwrap().clear();
    }
}
