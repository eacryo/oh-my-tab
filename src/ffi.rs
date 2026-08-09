//! FFI 与 ObjC 桥接的基础工具:CF/CG 函数声明、裸指针的 Send/Sync 包装、
//! NSString 转换、颜色/图层 helper。被所有 UI 模块依赖,是叶子层。
//!
//! FFI and ObjC-bridging primitives: CF/CG function declarations, Send/Sync wrappers for raw
//! pointers, NSString conversion, and color/layer helpers. A leaf module depended on by all UI modules.

use crate::log_info;
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::ffi::{c_char, c_void, CString};

// ========== FFI 外部函数声明 / FFI extern declarations ==========

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    pub(crate) fn CFRelease(cf: *const c_void);
    // CFEqual:比较两个 CF 对象是否"相等"。IOHIDServiceClient 的相等语义由系统定义
    // (通常按底层对象身份),而非裸指针地址——CopyServiceForRegistryID 返回的对象与
    // CopyServices 枚举出的可能不是同一实例地址,必须用 CFEqual 判断。
    // CFEqual: compares two CF objects for equality. IOHIDServiceClient equality is defined
    // by the system (typically by underlying object identity), not by raw pointer address --
    // the object returned by CopyServiceForRegistryID may not be the same instance as the one
    // enumerated by CopyServices, so CFEqual must be used.
    pub(crate) fn CFEqual(cf1: *const c_void, cf2: *const c_void) -> bool;
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
            log_info!("CFStringCreateWithCString failed for '{}'", s);
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

// ========== 应用名 / app names ==========

/// 取 NSRunningApplication 的 localizedName(UTF-8 规范化,空 = 失败)。
/// 窗口切换(图标缓存)与剪贴板(来源应用)共用,避免各自手写 UTF8String 转换。
/// The NSRunningApplication's localizedName (canonical UTF-8; empty = failure). Shared by the
/// window switcher (icon cache) and the clipboard (source app), so the UTF8String conversion
/// isn't hand-rolled twice.
pub(crate) unsafe fn ns_running_app_name(app: *mut AnyObject) -> String {
    if app.is_null() {
        return String::new();
    }
    let name: *mut AnyObject = msg_send![app, localizedName];
    nsstring_to_rust(name)
}

/// 当前前台应用的 (名称, pid)。剪贴板记录来源时一次拿全:名称用于标题栏文字,
/// pid 用于解析图标缓存身份(resolve_app_identity)并提取小图标。
/// The frontmost app as (name, pid). The clipboard grabs both in one lookup at record time:
/// the name feeds the header text, the pid resolves the icon-cache identity
/// (resolve_app_identity) and extracts the small icon.
pub(crate) fn frontmost_app_info() -> (String, i32) {
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let name = ns_running_app_name(app);
        let pid: i32 = if app.is_null() {
            -1
        } else {
            msg_send![app, processIdentifier]
        };
        (name, pid)
    }
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

/// NSColor* -> CGColorRef。用 raw objc_msgSend,因为 objc2 的 msg_send! 无法编码 CF/CG 类型。
/// NSColor* -> CGColorRef. Uses raw objc_msgSend because objc2's msg_send! can't encode CF/CG types.
pub(crate) unsafe fn ns_color_to_cg(ns: *mut AnyObject) -> *mut c_void {
    let sel = sel!(CGColor);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(ns as *mut c_void, sel)
}

/// Convert hex u32 -> CGColorRef for use with CALayer.setBackgroundColor / setBorderColor.
pub(crate) fn hex_to_cg_color(hex: u32) -> *mut c_void {
    let ns = hex_to_ns_color(hex);
    unsafe { ns_color_to_cg(ns) }
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
