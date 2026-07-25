//! FFI 与 ObjC 桥接的基础工具:CF/CG 函数声明、裸指针的 Send/Sync 包装、
//! NSString 转换、颜色/图层 helper。被所有 UI 模块依赖,是叶子层。
//!
//! FFI and ObjC-bridging primitives: CF/CG function declarations, Send/Sync wrappers for raw
//! pointers, NSString conversion, and color/layer helpers. A leaf module depended on by all UI modules.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::ffi::{c_char, c_void, CString};
use crate::log_error;

// ========== FFI 外部函数声明 / FFI extern declarations ==========

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    pub(crate) fn CFRelease(cf: *const c_void);
    pub(crate) fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
    pub(crate) static kCFRunLoopDefaultMode: *mut c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub(crate) fn AXIsProcessTrusted() -> bool;
}

// AppKit 框架链接占位 / AppKit framework link placeholder
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc", kind = "dylib")]
extern "C" {
    pub(crate) fn objc_allocateClassPair(
        superclass: *mut AnyObject,
        name: *const c_char,
        extra_bytes: usize,
    ) -> *mut AnyObject;
    pub(crate) fn objc_registerClassPair(cls: *mut AnyObject);
    pub(crate) fn class_addMethod(
        cls: *mut AnyObject,
        name: Sel,
        imp: *mut c_void,
        types: *const c_char,
    ) -> bool;
}

// ========== 裸指针的 Send/Sync 包装 / Send+Sync wrappers for raw ObjC pointers ==========

/// 线程安全的 ObjC 对象指针包装。所有访问由 Mutex 守卫,仅为静态存储实现 Send/Sync。
/// 字段 pub(crate):各模块通过 .0 取裸指针,或用 ObjPtr(x) 构造。
///
/// Thread-safe wrapper for raw ObjC object pointers.
/// All accesses are guarded by a Mutex - only Send/Sync for static storage.
/// Field is pub(crate): modules read the raw pointer via .0 or construct via ObjPtr(x).
#[derive(Clone, Copy)]
pub(crate) struct ObjPtr(pub(crate) *mut AnyObject);
unsafe impl Send for ObjPtr {}
unsafe impl Sync for ObjPtr {}

/// 线程安全的 ObjC 类指针包装。
/// Thread-safe wrapper for raw ObjC class pointers.
#[derive(Clone, Copy)]
pub(crate) struct ObjClassPtr(pub(crate) *const objc2::runtime::AnyClass);
unsafe impl Send for ObjClassPtr {}
unsafe impl Sync for ObjClassPtr {}

// ========== NSString / 对象生命周期 helper ==========

/// 用 Rust &str 构造一个 NSString(CFStringCreateWithCString 返回 +1,调用方负责 release)。
/// Build an NSString from a Rust &str (CFStringCreateWithCString returns +1; caller must release).
pub(crate) fn make_nsstring(s: &str) -> *mut AnyObject {
    unsafe {
        let c_str = CString::new(s).unwrap();
        let cf = CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100u32);
        if cf.is_null() {
            log_error!("CFStringCreateWithCString failed for '{}'", s);
        }
        cf as *mut AnyObject
    }
}

/// 释放 alloc 出来的 +1 对象。objc2 的 msg_send! 是裸 MRC(无 ARC):
/// alloc/init 返回 +1,必须手动 release;addSubview:/setImage:/addTrackingArea:
/// 只是再加自己的 retain,不会抵消 alloc 的那 +1。交给父视图/子视图持有后即可 release。
/// Release a +1 object obtained via alloc. objc2's msg_send! is raw MRC (no ARC):
/// alloc/init return +1 and must be released; addSubview:/setImage:/addTrackingArea:
/// only add their own retain and don't balance the alloc +1. Once the owning view
/// retains it, we drop our alloc +1.
pub(crate) unsafe fn release_obj(obj: *mut AnyObject) {
    if !obj.is_null() {
        let _: () = msg_send![obj, release];
    }
}

/// 当前进程是否拥有辅助功能(AX)权限。
/// Whether the current process has Accessibility permission.
pub(crate) fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// 把 NSString 转成 Rust String。
/// Convert an NSString to a Rust String.
pub(crate) unsafe fn nsstring_to_rust(ns: *mut AnyObject) -> String {
    if ns.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![ns, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(utf8)
        .to_string_lossy()
        .into_owned()
}

// ========== 颜色 / 图层 helper ==========

/// hex u32 -> NSColor。
/// hex u32 -> NSColor.
pub(crate) fn hex_to_ns_color(hex: u32) -> *mut AnyObject {
    let r = ((hex >> 24) & 0xFF) as f64 / 255.0;
    let g = ((hex >> 16) & 0xFF) as f64 / 255.0;
    let b = ((hex >> 8) & 0xFF) as f64 / 255.0;
    let a = (hex & 0xFF) as f64 / 255.0;
    unsafe { msg_send![class!(NSColor), colorWithRed: r, green: g, blue: b, alpha: a] }
}

/// Convert hex u32 -> CGColorRef for use with CALayer.setBackgroundColor / setBorderColor.
/// Uses raw objc_msgSend because objc2's msg_send! doesn't handle CF/CG types.
pub(crate) fn hex_to_cg_color(hex: u32) -> *mut c_void {
    let ns = hex_to_ns_color(hex);
    unsafe {
        let sel = sel!(CGColor);
        extern "C" {
            fn objc_msgSend();
        }
        type F = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
        let f: F = std::mem::transmute(objc_msgSend as *const ());
        f(ns as *mut c_void, sel)
    }
}

/// Set CALayer.backgroundColor using raw objc_msgSend (CGColorRef, not NSColor*).
pub(crate) unsafe fn layer_set_background(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setBackgroundColor:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

/// Set CALayer.borderColor using raw objc_msgSend (CGColorRef, not NSColor*).
pub(crate) unsafe fn layer_set_border(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setBorderColor:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}
