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

use crate::ffi::CGRect;

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

/// CoreGraphics 的私有 CGS 窗口几何 API 所在路径。
/// Path containing CoreGraphics' private CGS window-geometry APIs.
const COREGRAPHICS_PATH: &str = "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics";

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

/// CGAffineTransform 的 C ABI 布局；只读查询，不直接修改 WindowServer 状态。
/// C ABI layout of CGAffineTransform; used only for read-only WindowServer queries.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CGAffineTransform {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
    pub(crate) d: f64,
    pub(crate) tx: f64,
    pub(crate) ty: f64,
}

type CgsGetCatenatedWindowTransformFn = unsafe extern "C" fn(u32, u32, *mut CGAffineTransform);
type CgsGetWindowTransformFn = unsafe extern "C" fn(u32, u32, *mut CGAffineTransform) -> i32;
type CgsGetOnscreenWindowBoundsFn = unsafe extern "C" fn(u32, u32, *mut CGRect) -> i32;

static CGS_GET_CATENATED_WINDOW_TRANSFORM: LazyLock<Option<CgsGetCatenatedWindowTransformFn>> =
    LazyLock::new(|| unsafe {
        load_private_symbol::<CgsGetCatenatedWindowTransformFn>(
            COREGRAPHICS_PATH,
            "CGSGetCatenatedWindowTransform",
        )
    });
static CGS_GET_WINDOW_TRANSFORM: LazyLock<Option<CgsGetWindowTransformFn>> =
    LazyLock::new(|| unsafe {
        load_private_symbol::<CgsGetWindowTransformFn>(COREGRAPHICS_PATH, "CGSGetWindowTransform")
    });
static CGS_GET_ONSCREEN_WINDOW_BOUNDS: LazyLock<Option<CgsGetOnscreenWindowBoundsFn>> =
    LazyLock::new(|| unsafe {
        load_private_symbol::<CgsGetOnscreenWindowBoundsFn>(
            COREGRAPHICS_PATH,
            "CGSGetOnscreenWindowBounds",
        )
    });

/// 查询窗口的合成变换和当前屏幕可见 bounds。
/// Query a window's concatenated transform and its current onscreen bounds.
pub(crate) fn cgs_window_presentation_geometry(
    connection: u32,
    window_id: u32,
) -> Option<(CGAffineTransform, CGRect)> {
    let bounds_fn = (*CGS_GET_ONSCREEN_WINDOW_BOUNDS)?;
    let mut transform = CGAffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };
    let mut onscreen_bounds = CGRect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };
    unsafe {
        // 合成变换包含 WindowServer 动画使用的移动组/父窗口变换；如果未来 macOS
        // 不再导出更强的私有符号，则退回直接窗口变换。
        // Concatenated transform includes the movement-group/parent transform used by
        // WindowServer animations. Fall back to the direct window transform if the
        // stronger private symbol is absent on a future macOS release.
        if let Some(transform_fn) = *CGS_GET_CATENATED_WINDOW_TRANSFORM {
            transform_fn(connection, window_id, &mut transform);
        } else {
            let transform_fn = (*CGS_GET_WINDOW_TRANSFORM)?;
            if transform_fn(connection, window_id, &mut transform) != 0 {
                return None;
            }
        }
        if bounds_fn(connection, window_id, &mut onscreen_bounds) != 0 {
            return None;
        }
    }
    Some((transform, onscreen_bounds))
}
