//! Disabled-setting hint component: the click hint, the not-allowed cursor, and the tracking
//! areas that detect a pointer over a disabled control.
//!
//! There is deliberately no hover tooltip. A pointer resting on a row's description used to raise a
//! bubble repeating that description; the user asked for it to go (2026-10-23), and a redundant
//! bubble also hid the page content the pointer was resting on.
//!
//! The click hint is a refusal -- the control is disabled, so the click performs no action -- and it
//! is shown through the shared floating toast (`crate::ui::toast`), the same surface every other
//! settings feedback uses. An earlier in-window bubble was retired with the success bubble: one
//! feedback shape, one surface.

use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::{NSPoint, NSRect};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex, OnceLock};

/// Disabled rows own their tracking areas through the corresponding AppKit view. Store only
/// addresses so the registry never carries raw pointers across a thread boundary.
static DISABLED_TRACKING_AREAS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Tooltip text is kept separately so a click can resolve the disabled view to its hint.
static DISABLED_TOOLTIPS: LazyLock<Mutex<HashMap<usize, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

extern "C" fn disabled_cursor_mouse_entered(
    _self: *mut AnyObject,
    _cmd: Sel,
    event: *mut AnyObject,
) {
    crate::callback_guard::void("disabled_cursor_mouse_entered", || unsafe {
        let view = tracking_area_view(event);
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
    });
}

extern "C" fn disabled_cursor_mouse_exited(
    _self: *mut AnyObject,
    _cmd: Sel,
    _event: *mut AnyObject,
) {
    crate::callback_guard::void("disabled_cursor_mouse_exited", || unsafe {
        let cursor: *mut AnyObject = objc2::msg_send![objc2::class!(NSCursor), arrowCursor];
        let _: () = objc2::msg_send![cursor, set];
    });
}

fn disabled_cursor_target() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    let target: *mut AnyObject =
        crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabDisabledCursorTarget", || {
            crate::ffi::build_class(
                c"OhMyTabDisabledCursorTarget",
                objc2::class!(NSObject),
                |builder| unsafe {
                    builder.add_method::<AnyObject, _>(
                        objc2::sel!(mouseEntered:),
                        disabled_cursor_mouse_entered
                            as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                    builder.add_method::<AnyObject, _>(
                        objc2::sel!(mouseExited:),
                        disabled_cursor_mouse_exited
                            as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                },
            )
        })
        .0 as *mut AnyObject;
    DISABLED_CURSOR_TARGET
        .get_or_init(|| unsafe { DisabledCursorTarget(objc2::msg_send![target, new]) })
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

    /// Apply disabled-state pointer behavior and remember the click hint.
    pub(super) unsafe fn apply(view: *mut AnyObject, enabled: bool, tooltip: Option<&str>) {
        if view.is_null() {
            return;
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

    /// Drop a view from the disabled-hint registries before it is destroyed.
    ///
    /// Both the hint map and the tracking-area map are keyed by the view's raw address and hold
    /// no ownership, so a view that is torn down without this call leaves a dangling key behind:
    /// the next mouse-down in the settings window (`handle_mouse_down`) then messages the freed
    /// object and traps.
    pub(super) unsafe fn forget(view: *mut AnyObject) {
        if view.is_null() {
            return;
        }
        DISABLED_TOOLTIPS.lock().unwrap().remove(&(view as usize));
        // The tracking area must also be removed while the view is alive, or the registry keeps
        // a key for it after the view goes away.
        Self::set_disabled_tracking(view, false);
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

    /// Show the hint when a click lands on a disabled settings view. The click performs no action,
    /// so the hint is a refusal and carries the shared toast's red badge rather than a success
    /// shape; the toast times itself out, so no click anywhere needs to hide it.
    pub(super) unsafe fn handle_mouse_down(window: *mut AnyObject, event: *mut AnyObject) {
        if window.is_null() || event.is_null() {
            return;
        }
        let content: *mut AnyObject = objc2::msg_send![window, contentView];
        if content.is_null() {
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
                        crate::ui::toast::show_refused(&text);
                        return;
                    }
                    break;
                }
                ancestor = objc2::msg_send![ancestor, superview];
            }
        }
    }

    /// Drop tracking state before settings views are deallocated.
    pub(super) fn clear_runtime_registries() {
        DISABLED_TRACKING_AREAS.lock().unwrap().clear();
        DISABLED_TOOLTIPS.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod shared_toast_wiring_tests {
    /// Settings action feedback must use the same floating toast as the Finder menu and the existing
    /// settings refusals. The old in-window success bubble must not come back as a second shape.
    #[test]
    fn settings_action_feedback_uses_the_shared_toast() {
        let tooltip_source = include_str!("tooltip.rs");
        assert!(
            !tooltip_source.contains(concat!("show", "_success", "_bubble")),
            "the retired in-window success bubble is still wired"
        );

        let restore_source = include_str!("restore.rs");
        let start = restore_source
            .find("fn show_restore_success")
            .expect("restore feedback exists");
        let body = &restore_source[start..];
        let body = &body[..body.find("\n}\n").unwrap_or(body.len())];
        assert!(
            body.contains("crate::ui::toast::show_done"),
            "restore feedback must use the shared success toast: {body}"
        );

        let dispatch_source = include_str!("dispatch.rs");
        let start = dispatch_source
            .find("fn show_settings_success")
            .expect("settings feedback exists");
        let body = &dispatch_source[start..];
        let body = &body[..body.find("\n}\n").unwrap_or(body.len())];
        assert!(
            body.contains("crate::ui::toast::show_done"),
            "settings feedback must use the shared success toast: {body}"
        );
        let start = dispatch_source
            .find("fn show_settings_refusal")
            .expect("settings refusal feedback exists");
        let body = &dispatch_source[start..];
        let body = &body[..body.find("\n}\n").unwrap_or(body.len())];
        assert!(
            body.contains("crate::ui::toast::show_refused"),
            "settings refusals must keep the shared red badge: {body}"
        );

        // Pin the actual production call sites as well as the helpers: deleting one of these lines
        // must fail the test rather than leaving an action with no feedback at all.
        for call_site in [
            "show_settings_success(&t(\"settings.toast_settings_exported\"));",
            "show_settings_success(&t(\"settings.toast_settings_imported\"));",
        ] {
            assert!(
                dispatch_source.contains(call_site),
                "the settings feedback call site is missing: {call_site}"
            );
        }
        for call_site in [
            "show_restore_success(false);",
            "show_restore_success(true);",
        ] {
            assert!(
                restore_source.contains(call_site),
                "the restore feedback call site is missing: {call_site}"
            );
        }
    }

    /// The disabled-control click hint was the last in-window bubble. It must show through the
    /// shared toast as a refusal -- the click performs no action -- and the window must not grow
    /// a bubble surface of its own again.
    #[test]
    fn the_disabled_click_hint_uses_the_shared_toast() {
        let source = include_str!("tooltip.rs");
        assert!(
            source.contains("crate::ui::toast::show_refused(&text);"),
            "the disabled-control click hint must use the shared refusal toast"
        );
        // The names are split so this test's own literals do not satisfy the scan.
        for retired in [
            concat!("ACTIVE", "_BUBBLE"),
            concat!("OhMyTabSettings", "TooltipBubble"),
            concat!("show", "_bubble"),
        ] {
            assert!(
                !source.contains(retired),
                "the retired in-window hint bubble is still wired: {retired}"
            );
        }
    }

    /// A recorder that cannot start is a refusal, not a successful action; it must keep the red
    /// badge rather than inheriting the green one from the shared success helper.
    #[test]
    fn gesture_recording_refusals_keep_the_error_badge() {
        let source = include_str!("dispatch.rs");
        assert!(
            source.contains("NoticeKind::Success => show_settings_success(text),"),
            "the recorder success branch must use the green success badge"
        );
        assert!(
            source.contains("NoticeKind::Refusal => show_settings_refusal(text),"),
            "the recorder refusal branch must use the red error badge"
        );
        for (reason, copy) in [
            (
                "recording not started: the mouse tap is not running",
                "settings.gestures_record_needs_tap",
            ),
            (
                "recording not started: the feature is off",
                "settings.gestures_record_needs_feature",
            ),
        ] {
            let position = source.find(reason).expect("the refusal branch exists");
            let branch = &source[position..position + 300.min(source.len() - position)];
            assert!(
                branch.contains(copy) && branch.contains("NoticeKind::Refusal"),
                "{reason} must be shown as a refusal: {branch}"
            );
        }
    }
}
