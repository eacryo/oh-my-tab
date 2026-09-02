//! 私有框架 dlopen/dlsym 的统一入口:所有运行时解析的私有符号都经由本模块加载,
//! 避免 dlopen 样板与 SkyLight/HIServices 路径在各模块重复(此前 thumbnail、
//! window_server、window_collector 各自带一份)。
//!
//! Central entry point for private-framework dlopen/dlsym. Every lazily resolved
//! private symbol goes through this module, so the dlopen boilerplate and the
//! SkyLight/HIServices paths aren't duplicated per module (thumbnail, window_server,
//! and window_collector each used to carry their own copy).

use std::ffi::{c_char, c_void, CString};
use std::sync::LazyLock;

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn dlopen(filename: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: i32 = 2;

/// SkyLight 私有框架路径(窗口捕获/生命周期通知/窗口抬起均走它)。
/// The SkyLight private-framework path (window capture, lifecycle notifications,
/// and window raising all come from here).
pub(crate) const SKYLIGHT_PATH: &str =
    "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";

/// HIServices 框架路径(_AXUIElementGetWindow / GetProcessForPID 所在)。
/// The HIServices framework path (home of _AXUIElementGetWindow / GetProcessForPID).
pub(crate) const HISERVICES_PATH: &str =
    "/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices";

/// dlopen 一个框架路径,返回句柄(失败返回 null)。
/// dlopen a framework path, returning the handle (null on failure).
pub(crate) unsafe fn dlopen_path(path: &str) -> *mut c_void {
    let c = CString::new(path).unwrap();
    dlopen(c.as_ptr(), RTLD_NOW)
}

/// dlopen 框架并解析一个符号为函数指针;任一步失败返回 None。
/// `name` 不带结尾 NUL,由本函数补上。
///
/// dlopen a framework and resolve a symbol as a function pointer; returns None if
/// either step fails. `name` is given without a trailing NUL; this appends it.
pub(crate) unsafe fn load_private_symbol<T>(framework_path: &str, name: &str) -> Option<T> {
    let handle = dlopen_path(framework_path);
    if handle.is_null() {
        return None;
    }
    let symbol = CString::new(name).unwrap();
    let pointer = dlsym(handle, symbol.as_ptr());
    if pointer.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy(&pointer))
    }
}

type CgsMainConnFn = unsafe extern "C" fn() -> u32;

// 连接 ID 进程内常量,加载一次即可。
// The connection ID is a process-wide constant; resolving it once is enough.
static CGS_MAIN_CONN: LazyLock<Option<u32>> = LazyLock::new(|| unsafe {
    load_private_symbol::<CgsMainConnFn>(SKYLIGHT_PATH, "CGSMainConnectionID").map(|f| f())
});

/// 进程级 WindowServer 连接 ID(None = 私有符号加载失败,功能应整体休眠)。
/// The process-wide WindowServer connection ID (None = the private symbol failed to
/// load; dependents should stay dormant).
pub(crate) fn cgs_main_connection() -> Option<u32> {
    *CGS_MAIN_CONN
}
