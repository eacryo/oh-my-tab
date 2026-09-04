//! 设置项禁用提示组件：Tooltip、禁止操作指针与悬停 tracking。
//! Disabled-setting hint component: tooltips, the not-allowed cursor, and hover tracking.

use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::{LazyLock, Mutex, OnceLock};

/// Disabled rows own their tracking areas through the corresponding AppKit view. Store only
/// addresses so the registry never carries raw pointers across a thread boundary.
/// 禁用 row 的 tracking area 由对应 AppKit view 持有；这里只存地址，避免静态注册表跨线程
/// 携带裸指针。
static DISABLED_TRACKING_AREAS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Tooltip text is kept separately so a click can resolve the disabled view to its hint.
/// 单独保存 Tooltip 文案，点击时通过禁用 view 找到对应提示。
static DISABLED_TOOLTIPS: LazyLock<Mutex<HashMap<usize, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// At most one custom bubble is visible in the settings window at a time.
/// 设置窗口同一时间最多显示一个自绘气泡。
static ACTIVE_BUBBLE: Mutex<Option<(usize, usize)>> = Mutex::new(None);

/// The current dismissal timer; a new click replaces the old timer instead of racing it.
/// 当前自动消失定时器；新的点击会替换旧定时器，避免旧定时器误关新提示。
static ACTIVE_TIMER: Mutex<Option<usize>> = Mutex::new(None);

/// The bubble is a passive overlay; keeping hit testing disabled ensures it never blocks the
/// controls underneath it while it is visible.
/// 气泡是被动提示层；关闭命中测试，确保显示期间也不会挡住下面的设置控件。
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
/// 统一的设置项禁用提示行为。
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

    unsafe fn remove_bubble() {
        let active = ACTIVE_BUBBLE.lock().unwrap().take();
        if let Some((_, bubble)) = active {
            let bubble = bubble as *mut AnyObject;
            let _: () = objc2::msg_send![bubble, removeFromSuperview];
        }
    }

    unsafe fn hide_bubble() {
        Self::cancel_timer();
        Self::remove_bubble();
    }

    /// Hide and remove the current bubble atomically so dismissal cannot flash a stale frame.
    /// 原子地隐藏并移除当前气泡，避免消失时闪回旧的可见帧。
    unsafe fn dismiss_bubble() {
        let active = ACTIVE_BUBBLE.lock().unwrap().take();
        let Some((_, bubble)) = active else {
            return;
        };
        let bubble = bubble as *mut AnyObject;
        let _: () = objc2::msg_send![bubble, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![bubble, removeFromSuperview];
    }

    unsafe fn show_bubble(view: *mut AnyObject, text: &str) {
        if view.is_null() || text.is_empty() {
            return;
        }
        Self::hide_bubble();

        let window: *mut AnyObject = objc2::msg_send![view, window];
        let content: *mut AnyObject = if window.is_null() {
            std::ptr::null_mut()
        } else {
            objc2::msg_send![window, contentView]
        };
        if content.is_null() {
            return;
        }

        // Keep the bubble centered at the bottom of the whole settings window, above the footer,
        // rather than following the pointer or the clicked control.
        // 气泡固定在整个设置窗口底部、footer 上方，不跟随鼠标或被点击的控件。
        let content_bounds: NSRect = objc2::msg_send![content, bounds];
        let palette = crate::theme::ui_palette();
        let bubble_width = 248.0;
        let bubble_size = NSSize::new(bubble_width, 36.0);
        let x = content_bounds.origin.x
            + ((content_bounds.size.width - bubble_size.width) / 2.0).clamp(8.0, f64::MAX);
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
        // 窗口尺寸变化时保持气泡水平居中并贴住 footer 上方的位置。
        let _: () = objc2::msg_send![bubble, setAutoresizingMask: 1u64 | 4u64 | 32u64];
        let _: () = objc2::msg_send![bubble, setOpaque: false];
        let _: () = objc2::msg_send![bubble, setAlphaValue: 1.0f64];
        let _: () = objc2::msg_send![bubble, setWantsLayer: true];
        let layer: *mut AnyObject = objc2::msg_send![bubble, layer];
        if !layer.is_null() {
            let background = if palette.dark { 0x3A3A3FDD } else { 0xF8F8F8D9 };
            crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
            let _: () = objc2::msg_send![layer, setCornerRadius: 10.0f64];
            let _: () = objc2::msg_send![layer, setMasksToBounds: true];
            crate::ffi::layer_set_border(
                layer,
                crate::ffi::hex_to_cg_color(if palette.dark { 0xFFFFFF2A } else { 0x00000016 }),
            );
            let _: () = objc2::msg_send![layer, setBorderWidth: 1.0f64];
        }

        let icon_frame = NSRect::new(NSPoint::new(14.0, 10.0), NSSize::new(16.0, 16.0));
        let symbol_ns = crate::ffi::make_nsstring("info.circle.fill");
        let image: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSImage),
            imageWithSystemSymbolName: symbol_ns,
            accessibilityDescription: std::ptr::null::<AnyObject>()
        ];
        crate::ffi::CFRelease(symbol_ns as *const c_void);
        if !image.is_null() {
            let icon: *mut AnyObject = objc2::msg_send![objc2::class!(NSImageView), alloc];
            let icon: *mut AnyObject = objc2::msg_send![icon, initWithFrame: icon_frame];
            let _: () = objc2::msg_send![icon, setImage: image];
            let _: () = objc2::msg_send![icon, setImageScaling: 3isize];
            let tint = crate::ffi::hex_to_ns_color(palette.accent);
            let _: () = objc2::msg_send![icon, setContentTintColor: tint];
            let _: () = objc2::msg_send![bubble, addSubview: icon];
            crate::ffi::release_obj(icon);
        }

        let label: *mut AnyObject = objc2::msg_send![objc2::class!(NSTextField), alloc];
        let label: *mut AnyObject = objc2::msg_send![
            label,
            initWithFrame: NSRect::new(
                // Keep the single-line field close to the font's line height; a taller field
                // makes AppKit place the baseline visibly above the bubble's center.
                // 单行文本框高度贴近字体行高；frame 过高会让 AppKit 的基线明显偏向气泡上方。
                NSPoint::new(38.0, 10.0),
                NSSize::new(bubble_size.width - 52.0, 16.0),
            )
        ];
        let text_ns = crate::ffi::make_nsstring(text);
        let _: () = objc2::msg_send![label, setStringValue: text_ns];
        crate::ffi::release_obj(text_ns);
        let _: () = objc2::msg_send![label, setBezeled: false];
        let _: () = objc2::msg_send![label, setDrawsBackground: false];
        let _: () = objc2::msg_send![label, setEditable: false];
        let _: () = objc2::msg_send![label, setAlignment: 1isize];
        let _: () = objc2::msg_send![label, setUsesSingleLineMode: true];
        let _: () = objc2::msg_send![label, setLineBreakMode: 4isize];
        let font: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSFont),
            systemFontOfSize: 12.5f64,
            weight: 0.23f64
        ];
        let _: () = objc2::msg_send![label, setFont: font];
        let color = crate::ffi::hex_to_ns_color(palette.primary_text);
        let _: () = objc2::msg_send![label, setTextColor: color];
        let _: () = objc2::msg_send![bubble, addSubview: label];
        crate::ffi::release_obj(label);
        let _: () = objc2::msg_send![content, addSubview: bubble];
        crate::ffi::release_obj(bubble);
        *ACTIVE_BUBBLE.lock().unwrap() = Some((view as usize, bubble as usize));

        // A short opacity animation makes the click feedback feel attached to the setting row
        // without moving the bubble away from its fixed bottom position.
        // 用短暂透明度动画反馈点击，同时保持气泡固定在窗口底部而不跟随鼠标移动。
        let layer: *mut AnyObject = objc2::msg_send![bubble, layer];
        if !layer.is_null() {
            let key_path = crate::ffi::make_nsstring("opacity");
            let animation: *mut AnyObject = objc2::msg_send![
                objc2::class!(CABasicAnimation),
                animationWithKeyPath: key_path
            ];
            crate::ffi::CFRelease(key_path as *const c_void);
            let from_value: *mut AnyObject =
                objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: 0.0f64];
            let to_value: *mut AnyObject =
                objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: 1.0f64];
            let _: () = objc2::msg_send![animation, setFromValue: from_value];
            let _: () = objc2::msg_send![animation, setToValue: to_value];
            let _: () = objc2::msg_send![animation, setDuration: 0.12f64];
            let key = crate::ffi::make_nsstring("settings-tooltip-appear");
            let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: key];
            crate::ffi::CFRelease(key as *const c_void);
        }

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
    /// 应用禁用状态的悬停行为并保存点击提示。
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

    /// Show the hint when a click lands on a disabled settings view; any other click hides it.
    /// 点击禁用设置项时显示提示，点击其它位置时隐藏提示。
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
        let candidates: Vec<(usize, String)> = DISABLED_TOOLTIPS
            .lock()
            .unwrap()
            .iter()
            .map(|(view, text)| (*view, text.clone()))
            .collect();
        for (view_address, text) in candidates {
            let view = view_address as *mut AnyObject;
            let view_window: *mut AnyObject = objc2::msg_send![view, window];
            if view_window != window {
                continue;
            }
            let bounds: NSRect = objc2::msg_send![view, bounds];
            let rect: NSRect = objc2::msg_send![view, convertRect: bounds, toView: content];
            let inside = content_point.x >= rect.origin.x
                && content_point.x <= rect.origin.x + rect.size.width
                && content_point.y >= rect.origin.y
                && content_point.y <= rect.origin.y + rect.size.height;
            if inside {
                Self::show_bubble(view, &text);
                return;
            }
        }
        Self::hide_bubble();
    }

    /// Drop tracking state before settings views are deallocated.
    /// 设置 view 释放前清理 tracking 状态。
    pub(super) fn clear_runtime_registries() {
        unsafe { Self::hide_bubble() };
        DISABLED_TRACKING_AREAS.lock().unwrap().clear();
        DISABLED_TOOLTIPS.lock().unwrap().clear();
    }
}
