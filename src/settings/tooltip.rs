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
static ACTIVE_BUBBLE: Mutex<Option<usize>> = Mutex::new(None);

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

/// Remove a bubble after its exit animation has finished.
/// 在退出动画结束后移除气泡。
extern "C" fn tooltip_remove_bubble(_self: *mut c_void, _cmd: Sel, timer: *mut c_void) {
    unsafe {
        let timer = timer as *mut AnyObject;
        let bubble: *mut AnyObject = objc2::msg_send![timer, userInfo];
        if bubble.is_null() {
            return;
        }
        let _: () = objc2::msg_send![bubble, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![bubble, removeFromSuperview];
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
            crate::ffi::class_addMethod(
                cls,
                objc2::sel!(removeTooltipBubble:),
                tooltip_remove_bubble as *mut c_void,
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

    unsafe fn remove_bubble_now() {
        let active = ACTIVE_BUBBLE.lock().unwrap().take();
        if let Some(bubble) = active {
            let bubble = bubble as *mut AnyObject;
            let _: () = objc2::msg_send![bubble, setAlphaValue: 0.0f64];
            let _: () = objc2::msg_send![bubble, removeFromSuperview];
        }
    }

    unsafe fn hide_bubble() {
        Self::cancel_timer();
        Self::dismiss_bubble();
    }

    /// Hide and remove the current bubble atomically so dismissal cannot flash a stale frame.
    /// 原子地隐藏并移除当前气泡，避免消失时闪回旧的可见帧。
    unsafe fn dismiss_bubble() {
        let active = ACTIVE_BUBBLE.lock().unwrap().take();
        let Some(bubble) = active else {
            return;
        };
        let bubble = bubble as *mut AnyObject;
        let layer: *mut AnyObject = objc2::msg_send![bubble, layer];
        if layer.is_null() || Self::accessibility_reduce_motion() {
            let _: () = objc2::msg_send![bubble, setAlphaValue: 0.0f64];
            let _: () = objc2::msg_send![bubble, removeFromSuperview];
            return;
        }

        let opacity = Self::presentation_scalar(layer, "opacity", 1.0);
        let x = Self::presentation_scalar(layer, "transform.translation.x", 0.0);
        let scale = Self::presentation_scalar(layer, "transform.scale", 1.0);
        Self::set_layer_model(layer, "opacity", 0.0);
        Self::set_layer_model(layer, "transform.translation.x", 32.0);
        Self::set_layer_model(layer, "transform.scale", 0.96);
        Self::add_basic_animation(
            layer,
            "opacity",
            opacity,
            0.0,
            0.14,
            "settings-tooltip-exit-opacity",
        );
        Self::add_basic_animation(
            layer,
            "transform.translation.x",
            x,
            32.0,
            0.18,
            "settings-tooltip-exit-x",
        );
        Self::add_basic_animation(
            layer,
            "transform.scale",
            scale,
            0.96,
            0.18,
            "settings-tooltip-exit-scale",
        );

        let _: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSTimer),
            scheduledTimerWithTimeInterval: 0.18f64,
            target: disabled_cursor_target(),
            selector: objc2::sel!(removeTooltipBubble:),
            // Retain the bubble through the timer so a rapid replacement or window rebuild
            // cannot leave the exit callback with a dangling pointer.
            // 让 timer 持有 bubble，避免快速替换或重建窗口后退出回调访问悬空指针。
            userInfo: bubble,
            repeats: false
        ];
    }

    unsafe fn set_layer_model(layer: *mut AnyObject, key_path: &str, value: f64) {
        let number: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: value];
        let key_path = crate::ffi::make_nsstring(key_path);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        let _: () = objc2::msg_send![layer, setValue: number, forKeyPath: key_path];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
        crate::ffi::CFRelease(key_path as *const c_void);
    }

    unsafe fn presentation_scalar(layer: *mut AnyObject, key_path: &str, fallback: f64) -> f64 {
        let presentation: *mut AnyObject = objc2::msg_send![layer, presentationLayer];
        if presentation.is_null() {
            return fallback;
        }
        let key_path = crate::ffi::make_nsstring(key_path);
        let value: *mut AnyObject = objc2::msg_send![presentation, valueForKeyPath: key_path];
        crate::ffi::CFRelease(key_path as *const c_void);
        if value.is_null() {
            fallback
        } else {
            objc2::msg_send![value, doubleValue]
        }
    }

    unsafe fn add_basic_animation(
        layer: *mut AnyObject,
        key_path: &str,
        from: f64,
        to: f64,
        duration: f64,
        animation_key: &str,
    ) {
        let key_path_ns = crate::ffi::make_nsstring(key_path);
        let animation: *mut AnyObject = objc2::msg_send![
            objc2::class!(CABasicAnimation),
            animationWithKeyPath: key_path_ns
        ];
        crate::ffi::CFRelease(key_path_ns as *const c_void);
        let from_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: from];
        let to_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: to];
        let _: () = objc2::msg_send![animation, setFromValue: from_value];
        let _: () = objc2::msg_send![animation, setToValue: to_value];
        let _: () = objc2::msg_send![animation, setDuration: duration];
        // Match the reference toast's fast-out, gentle-settle cubic curve.
        // 匹配参考 Toast 的快速出场、柔和落位三次贝塞尔曲线。
        extern "C" {
            fn objc_msgSend();
        }
        type TimingFunction =
            unsafe extern "C" fn(*mut AnyObject, Sel, f32, f32, f32, f32) -> *mut AnyObject;
        let make_timing: TimingFunction = std::mem::transmute(objc_msgSend as *const ());
        let timing = make_timing(
            objc2::class!(CAMediaTimingFunction) as *const _ as *mut AnyObject,
            objc2::sel!(functionWithControlPoints::::),
            0.22,
            1.0,
            0.36,
            1.0,
        );
        if !timing.is_null() {
            let _: () = objc2::msg_send![animation, setTimingFunction: timing];
        }
        let animation_key = crate::ffi::make_nsstring(animation_key);
        let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: animation_key];
        crate::ffi::CFRelease(animation_key as *const c_void);
    }

    unsafe fn accessibility_reduce_motion() -> bool {
        let workspace: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null()
            || !objc2::msg_send![
                workspace,
                respondsToSelector: objc2::sel!(accessibilityDisplayShouldReduceMotion)
            ]
        {
            return false;
        }
        objc2::msg_send![workspace, accessibilityDisplayShouldReduceMotion]
    }

    unsafe fn show_bubble(view: *mut AnyObject, text: &str) {
        if view.is_null() {
            return;
        }
        let window: *mut AnyObject = objc2::msg_send![view, window];
        Self::show_bubble_in_window(window, text, false);
    }

    /// Show a transient success message in the settings window.
    /// 在设置窗口中显示短暂的成功提示。
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
        // 气泡相对于整个设置窗口水平居中并靠近底部，匹配参考 Toast 的位置，同时避开 footer 区域。
        let content_bounds: NSRect = objc2::msg_send![content, bounds];
        let palette = crate::theme::ui_palette();
        // The example toast is 360x82; use roughly 80% of that footprint for this window.
        // 示例 Toast 尺寸约为 360×82，这里取其约 80% 的占地。
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
        // 窗口尺寸变化时保持气泡水平居中并贴住 footer 上方的位置。
        let _: () = objc2::msg_send![bubble, setAutoresizingMask: 1u64 | 4u64 | 32u64];
        let _: () = objc2::msg_send![bubble, setOpaque: false];
        let _: () = objc2::msg_send![bubble, setAlphaValue: 1.0f64];
        let _: () = objc2::msg_send![bubble, setWantsLayer: true];
        let layer: *mut AnyObject = objc2::msg_send![bubble, layer];
        if !layer.is_null() {
            // Explicitly anchor the transform at the bubble's bottom center so scale grows
            // upward from the footer instead of depending on the backing layer's default anchor.
            // 明确将变换原点设为气泡底部中心，让缩放从 footer 正上方向上展开，不依赖 backing
            // layer 的默认锚点。更新锚点时补偿 position，避免改变最终 frame 位置。
            let bounds: NSRect = objc2::msg_send![layer, bounds];
            let old_anchor: NSPoint = objc2::msg_send![layer, anchorPoint];
            let old_position: NSPoint = objc2::msg_send![layer, position];
            let anchor = NSPoint::new(0.5, 0.0);
            let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
            let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
            let _: () = objc2::msg_send![layer, setAnchorPoint: anchor];
            let _: () = objc2::msg_send![
                layer,
                setPosition: NSPoint::new(
                    old_position.x + (anchor.x - old_anchor.x) * bounds.size.width,
                    old_position.y + (anchor.y - old_anchor.y) * bounds.size.height,
                )
            ];
            let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
            // Match the reference card: a near-opaque surface, large radius, and a soft
            // downward shadow that remains visible outside the bubble bounds.
            // 匹配参考卡片：接近不透明的表面、较大圆角，以及向下延伸到气泡边界外的柔和阴影。
            let background = if palette.dark { 0x3A3A3FF2 } else { 0xF8F8F8F2 };
            crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
            let _: () = objc2::msg_send![layer, setCornerRadius: 16.0f64];
            let _: () = objc2::msg_send![layer, setMasksToBounds: false];
            crate::ffi::layer_set_border(
                layer,
                crate::ffi::hex_to_cg_color(if palette.dark { 0xFFFFFF20 } else { 0x00000010 }),
            );
            let _: () = objc2::msg_send![layer, setBorderWidth: 1.0f64];
            crate::ffi::layer_set_shadow_color(layer, crate::ffi::hex_to_cg_color(0x000000FF));
            let _: () = objc2::msg_send![layer, setShadowOpacity: 0.25f32];
            let _: () = objc2::msg_send![layer, setShadowRadius: 25.0f64];
            let _: () = objc2::msg_send![layer, setShadowOffset: NSSize::new(0.0, -12.0)];
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
                if palette.dark {
                    0x30D158FF
                } else {
                    0x34C759FF
                }
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
                    if palette.dark {
                        0x30D15826
                    } else {
                        0x34C75920
                    }
                } else if palette.dark {
                    palette.accent & 0xFFFFFF00 | 0x26
                } else {
                    palette.accent & 0xFFFFFF00 | 0x20
                };
                crate::ffi::layer_set_background(icon_layer, crate::ffi::hex_to_cg_color(tint_hex));
                let _: () = objc2::msg_send![icon_layer, setCornerRadius: 14.0f64];
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
            systemFontOfSize: 14.0f64,
            weight: 0.23f64
        ];
        let _: () = objc2::msg_send![label, setFont: font];
        let color = crate::ffi::hex_to_ns_color(palette.primary_text);
        let _: () = objc2::msg_send![label, setTextColor: color];

        // Center the icon and the measured text as one group, keeping their gap stable for every
        // localized message instead of centering the text in the remaining bubble width.
        // 将图标和按实际宽度测量出的文本作为整体居中，避免不同语言下文本在剩余宽度中单独居中。
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

        // Enter from below with the same scale/offset profile as the reference toast.
        // 从下方以与参考 Toast 一致的缩放和位移轮廓进入。
        let layer: *mut AnyObject = objc2::msg_send![bubble, layer];
        if !layer.is_null() {
            Self::set_layer_model(layer, "opacity", 1.0);
            Self::set_layer_model(layer, "transform.translation.y", 0.0);
            Self::set_layer_model(layer, "transform.scale", 1.0);
            if !Self::accessibility_reduce_motion() {
                Self::add_basic_animation(
                    layer,
                    "opacity",
                    0.0,
                    1.0,
                    0.4,
                    "settings-tooltip-enter-opacity",
                );
                Self::add_basic_animation(
                    layer,
                    "transform.translation.y",
                    -22.0,
                    0.0,
                    0.4,
                    "settings-tooltip-enter-y",
                );
                Self::add_basic_animation(
                    layer,
                    "transform.scale",
                    0.96,
                    1.0,
                    0.4,
                    "settings-tooltip-enter-scale",
                );
            }
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

    /// Dismiss the current hint when navigation changes the visible settings page.
    /// 切换当前可见设置页时关闭已有提示，避免上一页的气泡残留。
    pub(super) unsafe fn dismiss() {
        Self::hide_bubble();
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
        // Resolve the actual AppKit hit view before checking candidates. Comparing the click
        // point with every disabled label's converted frame is too broad: a label can span most
        // of a row and overlap an unrelated action button (for example, Restore Defaults).
        // 先通过 AppKit 命中测试得到真实点击 view，再检查候选项。逐一比较所有禁用 label 的
        // 转换 frame 范围过于宽泛：label 可能覆盖整行，从而误判旁边的恢复默认按钮。
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
            // 所有设置页共用同一个窗口，只通过隐藏切页。跳过隐藏页面中的控件，否则隐藏页的
            // frame 可能与当前页面重叠并在手动坐标命中时抢先匹配。
            let hidden: bool = objc2::msg_send![view, isHiddenOrHasHiddenAncestor];
            if hidden {
                continue;
            }

            // A row label or a wrapped button title may be a child of the registered control,
            // so walk up from the hit view instead of requiring pointer equality.
            // 行 label 或换行按钮标题可能是已注册控件的子 view，因此从命中 view 向上遍历，
            // 不要求指针必须完全相等。
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
    /// 设置 view 释放前清理 tracking 状态。
    pub(super) fn clear_runtime_registries() {
        unsafe {
            Self::cancel_timer();
            Self::remove_bubble_now();
        }
        DISABLED_TRACKING_AREAS.lock().unwrap().clear();
        DISABLED_TOOLTIPS.lock().unwrap().clear();
    }
}
