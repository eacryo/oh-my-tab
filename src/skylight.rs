//! Central entry point for private-framework dlopen/dlsym. Every lazily resolved
//! private symbol goes through this module, so the dlopen boilerplate and the
//! SkyLight/HIServices paths aren't duplicated per module (thumbnail, window_server,
//! and window_collector each used to carry their own copy).

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::sync::LazyLock;

use crate::ffi::{CFArrayCreate, CFNumberCreate, CFRelease, CGRect};

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn dlopen(filename: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: i32 = 2;

/// The SkyLight private-framework path (window capture, lifecycle notifications,
/// and window raising all come from here).
pub(crate) const SKYLIGHT_PATH: &str =
    "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";

/// The HIServices framework path (home of _AXUIElementGetWindow / GetProcessForPID).
pub(crate) const HISERVICES_PATH: &str =
    "/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices";

/// Path containing CoreGraphics' private CGS window-geometry APIs.
const COREGRAPHICS_PATH: &str = "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics";

/// dlopen a framework path, returning the handle (null on failure).
pub(crate) unsafe fn dlopen_path(path: &str) -> *mut c_void {
    let c = CString::new(path).unwrap();
    dlopen(c.as_ptr(), RTLD_NOW)
}

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

// The connection ID is a process-wide constant; resolving it once is enough.
static CGS_MAIN_CONN: LazyLock<Option<u32>> = LazyLock::new(|| unsafe {
    load_private_symbol::<CgsMainConnFn>(SKYLIGHT_PATH, "CGSMainConnectionID").map(|f| f())
});

/// The process-wide WindowServer connection ID (None = the private symbol failed to
/// load; dependents should stay dormant).
pub(crate) fn cgs_main_connection() -> Option<u32> {
    *CGS_MAIN_CONN
}

type SlsWindowQueryWindowsFn = unsafe extern "C" fn(u32, *const c_void, i32) -> *const c_void;
type SlsWindowQueryResultCopyWindowsFn = unsafe extern "C" fn(*const c_void) -> *const c_void;
type SlsWindowIteratorAdvanceFn = unsafe extern "C" fn(*const c_void) -> bool;
type SlsWindowIteratorGetWindowIdFn = unsafe extern "C" fn(*const c_void) -> u32;
type SlsWindowIteratorGetParentIdFn = unsafe extern "C" fn(*const c_void) -> u32;

static SLS_WINDOW_QUERY_WINDOWS: LazyLock<Option<SlsWindowQueryWindowsFn>> =
    LazyLock::new(|| unsafe { load_private_symbol(SKYLIGHT_PATH, "SLSWindowQueryWindows") });
static SLS_WINDOW_QUERY_RESULT_COPY_WINDOWS: LazyLock<Option<SlsWindowQueryResultCopyWindowsFn>> =
    LazyLock::new(|| unsafe {
        load_private_symbol(SKYLIGHT_PATH, "SLSWindowQueryResultCopyWindows")
    });
static SLS_WINDOW_ITERATOR_ADVANCE: LazyLock<Option<SlsWindowIteratorAdvanceFn>> =
    LazyLock::new(|| unsafe { load_private_symbol(SKYLIGHT_PATH, "SLSWindowIteratorAdvance") });
static SLS_WINDOW_ITERATOR_GET_WINDOW_ID: LazyLock<Option<SlsWindowIteratorGetWindowIdFn>> =
    LazyLock::new(|| unsafe { load_private_symbol(SKYLIGHT_PATH, "SLSWindowIteratorGetWindowID") });
static SLS_WINDOW_ITERATOR_GET_PARENT_ID: LazyLock<Option<SlsWindowIteratorGetParentIdFn>> =
    LazyLock::new(|| unsafe { load_private_symbol(SKYLIGHT_PATH, "SLSWindowIteratorGetParentID") });

/// Query WindowServer parentage. A non-zero parent identifies an attached sheet/child surface,
/// not an independent switch destination; callers keep the parent and skip the child surface.
pub(crate) fn window_parent_ids(window_ids: &[u32]) -> HashMap<u32, u32> {
    let Some(connection) = cgs_main_connection() else {
        return HashMap::new();
    };
    let (
        Some(query_windows),
        Some(copy_windows),
        Some(advance),
        Some(get_window_id),
        Some(get_parent_id),
    ) = (
        *SLS_WINDOW_QUERY_WINDOWS,
        *SLS_WINDOW_QUERY_RESULT_COPY_WINDOWS,
        *SLS_WINDOW_ITERATOR_ADVANCE,
        *SLS_WINDOW_ITERATOR_GET_WINDOW_ID,
        *SLS_WINDOW_ITERATOR_GET_PARENT_ID,
    )
    else {
        return HashMap::new();
    };

    let ids: Vec<u32> = window_ids.iter().copied().filter(|id| *id != 0).collect();
    if ids.is_empty() {
        return HashMap::new();
    }

    let mut number_refs = Vec::with_capacity(ids.len());
    for id in &ids {
        let value = *id as i32;
        let number = unsafe { CFNumberCreate(std::ptr::null(), 3, (&value as *const i32).cast()) };
        if number.is_null() {
            unsafe {
                for number in number_refs {
                    CFRelease(number);
                }
            }
            return HashMap::new();
        }
        number_refs.push(number);
    }

    // The array is only a synchronous input to SLSWindowQueryWindows. The CFNumbers are retained
    // by this function until the query has copied the ids, so a null callback table is sufficient.
    let array = unsafe {
        CFArrayCreate(
            std::ptr::null(),
            number_refs.as_ptr(),
            number_refs.len() as isize,
            std::ptr::null(),
        )
    };
    if array.is_null() {
        unsafe {
            for number in number_refs {
                CFRelease(number);
            }
        }
        return HashMap::new();
    }

    let result = unsafe { query_windows(connection, array, ids.len() as i32) };
    unsafe {
        CFRelease(array);
        for number in number_refs {
            CFRelease(number);
        }
    }
    if result.is_null() {
        return HashMap::new();
    }

    let iterator = unsafe { copy_windows(result) };
    unsafe { CFRelease(result) };
    if iterator.is_null() {
        return HashMap::new();
    }

    let mut parents = HashMap::with_capacity(ids.len());
    unsafe {
        while advance(iterator) {
            let window_id = get_window_id(iterator);
            parents.insert(window_id, get_parent_id(iterator));
        }
        CFRelease(iterator);
    }
    parents
}

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
