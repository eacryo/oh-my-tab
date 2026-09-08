//! 鼠标位置提示:用遮罩突出当前鼠标位置，移动鼠标后自动消失。
//! Pointer locator: dim the desktop around the current cursor and disappear when it moves.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::ffi::{cg_overlay_window_level, release_obj, CallbackTarget};

const SPOTLIGHT_DIAMETER: f64 = 144.0;
const MASK_ALPHA: f64 = 0.62;
const POINTER_POLL_INTERVAL: f64 = 0.016;

struct Locator {
    window: CallbackTarget,
    content: CallbackTarget,
}

static LOCATOR: OnceLock<Locator> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static SPOTLIGHT_CENTER: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));

/// 在当前鼠标位置显示遮罩，鼠标发生移动后隐藏。调用方已经位于 AppKit 主线程。
/// Show the mask at the current cursor position and hide it when the cursor moves. The caller
/// is already on AppKit's main thread.
pub(crate) fn show() {
    unsafe {
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        *SPOTLIGHT_CENTER.lock().unwrap() = (cursor.x, cursor.y);
        ACTIVE.store(true, Ordering::Release);

        let locator = LOCATOR.get_or_init(|| create_locator());
        let window = locator.window.0;
        let content = locator.content.0;
        let frame = desktop_frame();

        let _: () = msg_send![window, setFrame: frame, display: false];
        let _: () = msg_send![class!(NSObject), cancelPreviousPerformRequestsWithTarget: window];
        let _: () = msg_send![content, setNeedsDisplay: true];
        let _: () = msg_send![window, orderFrontRegardless];
        schedule_pointer_check(window);
    }
}

/// Polling avoids installing another global event tap solely for this short-lived visual cue.
/// 通过短周期轮询避免仅为这个临时视觉提示再安装一个全局事件 tap。
unsafe extern "C" fn check_pointer(this: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    let (x, y) = *SPOTLIGHT_CENTER.lock().unwrap();
    if cursor.x != x || cursor.y != y {
        ACTIVE.store(false, Ordering::Release);
        let window = this as *mut AnyObject;
        let _: () = msg_send![class!(NSObject), cancelPreviousPerformRequestsWithTarget: window];
        let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
    } else {
        schedule_pointer_check(this as *mut AnyObject);
    }
}

unsafe fn schedule_pointer_check(window: *mut AnyObject) {
    let _: () = msg_send![
        window,
        performSelector: sel!(checkPointer:),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: POINTER_POLL_INTERVAL
    ];
}

/// Draw the dimming mask and punch a transparent hole around the cursor.
/// 绘制半透明遮罩，并在鼠标位置挖出透明圆孔。
unsafe extern "C" fn draw_mask(this: *mut c_void, _cmd: Sel, _dirty_rect: NSRect) {
    let view = this as *mut AnyObject;
    let bounds: NSRect = msg_send![view, bounds];
    let (cursor_x, cursor_y) = *SPOTLIGHT_CENTER.lock().unwrap();
    let window: *mut AnyObject = msg_send![view, window];
    let window_frame: NSRect = msg_send![window, frame];
    let hole = NSRect::new(
        NSPoint::new(
            cursor_x - window_frame.origin.x - SPOTLIGHT_DIAMETER / 2.0,
            cursor_y - window_frame.origin.y - SPOTLIGHT_DIAMETER / 2.0,
        ),
        NSSize::new(SPOTLIGHT_DIAMETER, SPOTLIGHT_DIAMETER),
    );
    let mask_color: *mut AnyObject =
        msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: MASK_ALPHA];
    let _: () = msg_send![mask_color, set];

    // Use an even-odd path so the spotlight is transparent in the same draw operation.
    // 使用偶奇填充路径，让聚光圆孔在同一次绘制中保持透明，避免事后擦除造成残留变暗。
    let path: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
    let _: () = msg_send![path, appendBezierPathWithRect: bounds];
    let _: () = msg_send![path, appendBezierPathWithOvalInRect: hole];
    let _: () = msg_send![path, setWindingRule: 1isize];
    let _: () = msg_send![path, fill];
}

unsafe fn desktop_frame() -> NSRect {
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    let count: usize = msg_send![screens, count];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for index in 0..count {
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index];
        let frame: NSRect = msg_send![screen, frame];
        min_x = min_x.min(frame.origin.x);
        min_y = min_y.min(frame.origin.y);
        max_x = max_x.max(frame.origin.x + frame.size.width);
        max_y = max_y.max(frame.origin.y + frame.size.height);
    }
    if min_x.is_infinite() {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        return msg_send![screen, frame];
    }
    NSRect::new(
        NSPoint::new(min_x, min_y),
        NSSize::new(max_x - min_x, max_y - min_y),
    )
}

unsafe fn create_locator() -> Locator {
    let frame = desktop_frame();
    let panel_class = locator_panel_class();
    let panel: *mut AnyObject = msg_send![panel_class, alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithContentRect: frame,
        styleMask: 1u64 << 7,
        backing: 2u64,
        defer: false
    ];
    let _: () = msg_send![panel, setLevel: cg_overlay_window_level()];
    let _: () = msg_send![panel, setOpaque: false];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![panel, setBackgroundColor: clear];
    let _: () = msg_send![panel, setHasShadow: false];
    let _: () = msg_send![panel, setIgnoresMouseEvents: true];
    let _: () = msg_send![panel, setHidesOnDeactivate: false];
    let behavior: usize = (1 << 0) | (1 << 3) | (1 << 6) | (1 << 8) | (1 << 18);
    let _: () = msg_send![panel, setCollectionBehavior: behavior];

    let content_class = locator_content_class();
    let content: *mut AnyObject = msg_send![content_class, alloc];
    let content: *mut AnyObject = msg_send![content, initWithFrame: frame];
    let _: () = msg_send![panel, setContentView: content];
    release_obj(content);

    Locator {
        window: CallbackTarget::new(panel),
        content: CallbackTarget::new(content),
    }
}

fn locator_panel_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabPointerLocatorWindow").unwrap();
        let superclass = class!(NSPanel) as *const _ as *mut AnyObject;
        let cls = crate::ffi::objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("v@:@").unwrap();
        crate::ffi::class_addMethod(
            cls,
            sel!(checkPointer:),
            check_pointer as *mut c_void,
            types.as_ptr(),
        );
        crate::ffi::objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

fn locator_content_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabPointerLocatorView").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = crate::ffi::objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("v@:{CGRect={CGPoint=dd}{CGSize=dd}}").unwrap();
        crate::ffi::class_addMethod(
            cls,
            sel!(drawRect:),
            draw_mask as *mut c_void,
            types.as_ptr(),
        );
        crate::ffi::objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}
