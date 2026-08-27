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

// ========== 本进程内存指标(task_info) / own-process memory stats (task_info) ==========

/// kernel `task_vm_info` 的 C 数据布局是 372 字节(93 个 u32 word)。Rust 的 `repr(C)`
/// 会在末尾按自身 8 字节对齐补 4 字节,所以不能从 `size_of::<TaskVmInfo>()` 推导 count;
/// Mach count 必须按 C 布局明确写成 93。缓冲区保留额外尾部空间,绝不会被写穿。
/// The kernel `task_vm_info` C data layout is 372 bytes (93 u32 words). Rust's `repr(C)`
/// adds 4 bytes of trailing 8-byte alignment padding, so the Mach count must be based on
/// the C layout rather than `size_of::<TaskVmInfo>()`. The buffer has extra tail space and
/// cannot be overrun.
const TASK_VM_INFO_DATA_BYTES: usize = 372;
pub(crate) const TASK_VM_INFO_COUNT: u32 =
    (TASK_VM_INFO_DATA_BYTES / std::mem::size_of::<u32>()) as u32;

/// 偏移已用 C 编译器对照 mach/task_info.h 实测:
///   resident_size=16, resident_size_peak=24, internal=48, compressed=120,
///   phys_footprint=144(header 后第一组是 32 位的 basic_info 字段,不是 64 位)。
/// 布局必须与系统头一致;新增字段只能追加,不能重排。
/// Offsets verified against mach/task_info.h with a C compiler:
///   resident_size=16, resident_size_peak=24, internal=48, compressed=120,
///   phys_footprint=144 (the first group after the header holds the 32-bit basic_info
///   fields, not 64-bit ones). The layout must match the system header; new fields may
/// only be appended, never reordered.
#[repr(C)]
pub(crate) struct TaskVmInfo {
    // offset 0-15: mach_msg_type_number_t header × 4(bold/virtual_size 低半/... 32 位组)。
    // offset 0-15: four u32 header words (the 32-bit basic_info group).
    header: [u32; 4],
    /// 驻留物理页总字节(RSS)。offset 16。
    /// Total resident bytes (RSS). Offset 16.
    pub(crate) resident_size: u64,
    /// RSS 峰值(kernel 维护)。offset 24。
    /// Peak RSS (kernel-maintained). Offset 24.
    pub(crate) resident_size_peak: u64,
    // offset 32-47: region_count 等 32 位计数字段组。
    // offset 32-47: the 32-bit counter group (region_count etc.).
    _counters: [u32; 4],
    /// 匿名内存(我们的堆:Rust + malloc zone),字节。offset 48。
    /// Anonymous memory (our heap: Rust + malloc zones). Offset 48.
    pub(crate) internal: u64,
    // offset 56-119: internal 之后的 64 位字段组(purgeable/alternate...)。
    // offset 56-119: the 64-bit group after internal (purgeable/alternate/...).
    _middle: [u64; 8],
    /// 已被压缩器收编的内存,字节。offset 120。
    /// Memory absorbed by the compressor. Offset 120.
    pub(crate) compressed: u64,
    // offset 128-143: compressed 之后的 64 位字段组。
    // offset 128-143: the 64-bit group after compressed.
    _late: [u64; 2],
    /// 物理足迹 = 活动监视器「内存」列的口径(含压缩与 IOKit 映射)。字节。offset 144。
    /// Physical footprint = Activity Monitor's "Memory" column (compressed + IOKit included).
    /// Offset 144.
    pub(crate) phys_footprint: u64,
    // offset 152-371: 尾部未读字段。补齐 C 数据布局,防止 task_info 越界写缓冲区。
    // offset 152-371: trailing unread fields. Pads the C data layout so task_info
    // cannot write past the buffer.
    _tail: [u8; TASK_VM_INFO_DATA_BYTES - 152],
}

const _: () = assert!(std::mem::size_of::<TaskVmInfo>() >= TASK_VM_INFO_DATA_BYTES);

// [u8; 220] 超出 derive(Default) 支持的数组长度(≤32),手写。
// [u8; 220] exceeds the array length derive(Default) supports (<=32); hand-written.
impl Default for TaskVmInfo {
    fn default() -> Self {
        Self {
            header: [0; 4],
            resident_size: 0,
            resident_size_peak: 0,
            _counters: [0; 4],
            internal: 0,
            _middle: [0; 8],
            compressed: 0,
            _late: [0; 2],
            phys_footprint: 0,
            _tail: [0; 220],
        }
    }
}

/// 读当前进程的 task_vm_info。失败(理论上仅发生在 kernel 接口变化时)返回 None,调用方跳过本次采样。
/// Read the current process's task_vm_info. Returns None on failure (only plausible if the
/// kernel interface changes); the caller just skips that sample.
pub(crate) fn task_vm_info() -> Option<TaskVmInfo> {
    let mut info = TaskVmInfo::default();
    let mut count = TASK_VM_INFO_COUNT;
    let kr = task_info(std::ptr::addr_of_mut!(info), &mut count);
    if kr != 0 {
        return None;
    }
    Some(info)
}

#[cfg(test)]
impl TaskVmInfo {
    /// 测试用构造:占位字段保持默认,只设关心的指标(私有字段无法从模块外构造)。
    /// Test-only constructor: padding stays default, only the metrics of interest are set
    /// (private fields can't be constructed from outside the module).
    pub(crate) fn with_footprint(phys_footprint: u64) -> Self {
        Self {
            phys_footprint,
            ..Default::default()
        }
    }
}

fn task_info(info: *mut TaskVmInfo, count: &mut u32) -> i32 {
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            target: u32,
            flavor: u32,
            info_out: *mut TaskVmInfo,
            info_out_count: *mut u32,
        ) -> i32;
    }
    const TASK_VM_INFO_FLAVOR: u32 = 22;
    unsafe { task_info(mach_task_self(), TASK_VM_INFO_FLAVOR, info, count) }
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
