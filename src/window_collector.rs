use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::time::Instant;
use objc2::{class, msg_send};
use objc2::runtime::AnyObject;

use crate::config::CONFIG;
use crate::{log_info, log_warn};

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub pid: i32,
    pub window_id: u32, // CGWindowID，用于精确 raise（SLPS）/配对
    pub app_name: String,
    pub window_title: String,
    pub icon_path: Option<String>,
    pub is_active: bool,
    pub minimized: bool, // 最小化窗口(show_minimized 打开时才收集)/ minimized (collected only when show_minimized is on)
}

/// 窗口级 MRU 时间戳，按 (pid, CGWindowID) 索引。
/// 每个窗口独立追踪最后被激活/选中的时间，不与其他窗口共享。
/// 排序时按 elapsed 升序——最近使用的窗口排在前面。
/// Window-level MRU timestamps, keyed by (pid, CGWindowID).
/// Each window is tracked independently — no app-level grouping.
/// Sorted by elapsed ascending — most recently used windows come first.
pub type MruMap = HashMap<(i32, u32), Instant>;

/// PID → 最后一次 App 激活时间（通过 NSWorkspace 通知）。
/// 仅用于诊断输出，不参与排序。排序已改为纯窗口级 MRU。
/// PID → last app activation time (via NSWorkspace notification).
/// Diagnostics only — sorting is now purely window-level MRU.
static LAST_ACTIVATED: std::sync::LazyLock<std::sync::Mutex<HashMap<i32, Instant>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// 通过 NSWorkspaceDidActivateApplicationNotification 发送，由 main.rs 调用。
/// 保留用于诊断——排序不再依赖此数据。
/// Called from the NSWorkspaceDidActivateApplicationNotification handler in main.rs.
/// Kept for diagnostics — sorting no longer depends on this data.
pub fn note_app_activated(pid: i32) {
    LAST_ACTIVATED.lock().unwrap().insert(pid, Instant::now());
}

/// 将指定窗口的 MRU 时间戳更新为当前时间。
/// 由三个路径调用：
/// 1. oh-my-tab 内选中窗口（on_cmd_released / card_mouse_down / KEY_RETURN）
/// 2. NSWorkspace 激活通知 → on_app_activated 后台线程解析焦点窗口后回主线程调用
///    这是让系统 Cmd+Tab / Dock 点击等外部焦点切换也反映在窗口排序中的关键。
/// Bump the MRU timestamp of a specific window to now. Called from three paths:
/// 1. Window selected inside oh-my-tab (on_cmd_released / card_mouse_down / KEY_RETURN)
/// 2. NSWorkspace activation notification → on_app_activated resolves the focused
///    window on a background thread, then calls this on the main thread — this is how
///    external focus switches (system Cmd+Tab, Dock clicks) feed into window ordering.
pub fn bump_window_mru(mru: &mut MruMap, pid: i32, cgwid: u32) {
    if cgwid != 0 {
        mru.insert((pid, cgwid), Instant::now());
    }
}

fn icon_cache_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{}/Library/Caches/oh-my-tab-icons", home)
}

const K_C_G_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
const K_C_G_WINDOW_LIST_OPTION_ALL: u32 = 0; // 含离屏窗口(最小化等)/ includes off-screen windows (minimized, etc.)

// AX types
type AXUIElementRef = *const c_void;
type AXError = i32;
const K_AX_SUCCESS: AXError = 0;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const i8,
        encoding: u32,
    ) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, the_type: isize, value: *mut c_void) -> bool;
    fn CFBooleanGetValue(boolean: *const c_void) -> bool;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut i8,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFRelease(cf: *const c_void);
    static kCFBooleanFalse: *const c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *const c_void,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: *const c_void) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *const c_void,
    ) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f64) -> AXError;
}

// ========== 私有 API（dlsym 运行时解析，和 BetterCmdTab 一致）==========
// Private APIs (resolved at runtime via dlsym, same as BetterCmdTab).
// 用来按 CGWindowID 精确配对 AX/CG 窗口、并在 WindowServer 层只抬起一个窗口。

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

extern "C" {
    fn dlopen(filename: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: i32 = 2;

/// dlopen 一个框架路径，返回句柄（失败返回 null）。
/// dlopen a framework path, returning the handle (null on failure).
unsafe fn dlopen_path(path: &str) -> *mut c_void {
    let c = std::ffi::CString::new(path).unwrap();
    dlopen(c.as_ptr(), RTLD_NOW)
}

type AxGetWindowFn = unsafe extern "C" fn(AXUIElementRef, *mut u32) -> AXError;
type GetProcessForPIDFn = unsafe extern "C" fn(i32, *mut ProcessSerialNumber) -> i32;
type SlpSetFrontFn = unsafe extern "C" fn(*mut ProcessSerialNumber, u32, i32) -> i32;

static AX_GET_WINDOW: std::sync::LazyLock<Option<AxGetWindowFn>> = std::sync::LazyLock::new(|| unsafe {
    // _AXUIElementGetWindow 是 HIServices 里的私有符号，dlopen 后 dlsym。
    let name = b"_AXUIElementGetWindow\0";
    let h = dlopen_path("/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices");
    if h.is_null() { return None; }
    let p = dlsym(h, name.as_ptr() as *const c_char);
    if p.is_null() { None } else { Some(std::mem::transmute(p)) }
});
static GET_PROCESS_FOR_PID: std::sync::LazyLock<Option<GetProcessForPIDFn>> = std::sync::LazyLock::new(|| unsafe {
    // GetProcessForPID 也在 HIServices，dlopen 后 dlsym。
    let name = b"GetProcessForPID\0";
    let h = dlopen_path("/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices");
    if h.is_null() { return None; }
    let p = dlsym(h, name.as_ptr() as *const c_char);
    if p.is_null() { None } else { Some(std::mem::transmute(p)) }
});
static SLP_SET_FRONT: std::sync::LazyLock<Option<SlpSetFrontFn>> = std::sync::LazyLock::new(|| unsafe {
    // SkyLight 是私有框架，必须先 dlopen 才能查到符号。
    let name = b"_SLPSSetFrontProcessWithOptions\0";
    let h = dlopen_path("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight");
    if h.is_null() { return None; }
    let p = dlsym(h, name.as_ptr() as *const c_char);
    if p.is_null() { None } else { Some(std::mem::transmute(p)) }
});

/// 取一个 AX 窗口的 CGWindowID（私有 API _AXUIElementGetWindow）。
/// 用它把 AX 窗口和 CG 窗口按 CGWindowID 精确配对，不再靠顺序/标题猜
/// （Edge 等 CG 无窗口名的 App 之前会配错，导致 mru/raise 跟错窗口）。
/// Get a CGWindowID for an AX window (private API _AXUIElementGetWindow).
/// Used to pair AX windows with CG windows by CGWindowID instead of guessing
/// by order/title (apps like Edge with no CG window name used to mismatch,
/// corrupting mru/raise targeting).
unsafe fn ax_window_cgwid(element: AXUIElementRef) -> Option<u32> {
    let f = (*AX_GET_WINDOW)?;
    let mut wid: u32 = 0;
    if f(element, &mut wid) == K_AX_SUCCESS && wid != 0 { Some(wid) } else { None }
}

/// 查某 App 当前聚焦窗口的 CGWindowID（kAXFocusedWindow -> _AXUIElementGetWindow）。
/// 比 AX[0] 可靠：AX 窗口数组顺序不一定是最前在前，但 kAXFocusedWindow 是明确聚焦的窗口。
/// Get the CGWindowID of an app's currently focused window (kAXFocusedWindow ->
/// _AXUIElementGetWindow). More reliable than AX[0]: the AX window array order
/// isn't always frontmost-first, but kAXFocusedWindow is the explicitly focused window.
pub(crate) unsafe fn focused_window_cgwid(pid: i32) -> Option<u32> {
    let app = AXUIElementCreateApplication(pid);
    if app.is_null() { return None; }
    AXUIElementSetMessagingTimeout(app, 0.05); // 50ms 超时，避免卡死在无响应的 App 上。
    let focused_key = cf_string_new("AXFocusedWindow");
    let mut focused: *const c_void = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(app, focused_key, &mut focused);
    CFRelease(focused_key);
    CFRelease(app);
    if err != K_AX_SUCCESS || focused.is_null() { return None; }
    let wid = ax_window_cgwid(focused);
    CFRelease(focused);
    wid
}

/// 用 SkyLight 私有 API _SLPSSetFrontProcessWithOptions 在 WindowServer 层
/// 只抬起指定 CGWindowID 的那一个窗口（不抬该 App 的所有窗口）。
/// Raise only one window (by CGWindowID) at the WindowServer level via the
/// SkyLight private API _SLPSSetFrontProcessWithOptions -- does NOT raise all
/// of the app's windows the way activate(AllWindows) does.
unsafe fn raise_window_slps(pid: i32, wid: u32) -> bool {
    let get_psn = match *GET_PROCESS_FOR_PID { Some(f) => f, None => return false };
    let set_front = match *SLP_SET_FRONT { Some(f) => f, None => return false };
    let mut psn = ProcessSerialNumber { high_long_of_psn: 0, low_long_of_psn: 0 };
    if get_psn(pid, &mut psn) != 0 { return false; }
    // mode 2 = userGenerated（BetterCmdTab 的取值）。
    set_front(&mut psn, wid, 2) == 0 // CGError success == 0
}

fn cf_string_new(s: &str) -> *const c_void {
    let c_str = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100) }
}

fn cf_dict_get_string(dict: *const c_void, key: &str) -> Option<String> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() { return None; }
    cf_to_rust_string(value)
}

fn cf_dict_get_i32(dict: *const c_void, key: &str) -> Option<i32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() { return None; }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok { Some(num) } else { None }
}

fn cf_dict_get_u32(dict: *const c_void, key: &str) -> Option<u32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() { return None; }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok { Some(num as u32) } else { None }
}

// 读 CG dict 里的 CFBoolean(如 kCGWindowIsOnscreen)。/ Read a CFBoolean from a CG dict (e.g. kCGWindowIsOnscreen).
fn cf_dict_get_bool(dict: *const c_void, key: &str) -> Option<bool> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() { return None; }
    Some(unsafe { CFBooleanGetValue(value) })
}

pub fn ensure_icon_cache_dir() {
    let _ = std::fs::create_dir_all(icon_cache_dir());
}

/// 清空图标缓存目录(删除所有 {pid}.png),然后重建空目录。
/// 内存里 WindowInfo.icon_path 不会自动失效,调用方需自行将其置 None 并触发重提取。
///
/// Clear the icon cache directory (remove all {pid}.png), then recreate it empty.
/// In-memory WindowInfo.icon_path is NOT invalidated here; the caller must reset it to None
/// and trigger re-extraction.
pub fn clear_icon_cache() {
    let dir = icon_cache_dir();
    // remove_dir_all 在目录不存在时会报错,忽略即可 / errors if the dir doesn't exist; ignore
    let _ = std::fs::remove_dir_all(&dir);
    ensure_icon_cache_dir();
}

/// 图标缓存「文件存在即有效」，不设过期时间。
/// 缓存按 PID 索引：App 更新必然重启 -> 新 PID -> 自动重新提取，因此无需靠 TTL
/// 刷新；运行时改图标的 App（日历日期 / Dock 角标）会冻结到该 App 重启。
/// Icon cache is valid as long as the file exists - no expiry. The cache is keyed
/// by PID: an app update always relaunches with a new PID, which forces a
/// re-extract, so no TTL is needed. Apps that change their icon at runtime
/// (Calendar date, dock badge) stay frozen until that app is restarted.
pub fn check_icon_cache(pid: i32) -> Option<String> {
    let path = format!("{}/{}.png", icon_cache_dir(), pid);
    std::fs::metadata(&path).ok().map(|_| path)
}

fn write_png_to_cache(png: *mut AnyObject, pid: i32) -> Option<String> {
    unsafe {
        let path = format!("{}/{}.png", icon_cache_dir(), pid);
        let path_cstr = std::ffi::CString::new(&*path).unwrap();
        let cf_path = CFStringCreateWithCString(std::ptr::null(), path_cstr.as_ptr(), 0x08000100);
        // 原子写入：先写临时文件再重命名，避免写一半崩溃留下半截 PNG。
        // Atomic write: write to a temp file then rename, so a mid-write crash
        // can't leave a half-written PNG that "file exists = valid" would trust.
        let ok: bool = msg_send![png, writeToFile: cf_path as *mut AnyObject, atomically: true];
        CFRelease(cf_path as *const c_void);
        if ok { Some(path) } else { None }
    }
}

pub fn extract_icon_to_cache(pid: i32) -> Option<String> {
    if let Some(path) = check_icon_cache(pid) {
        return Some(path);
    }
    unsafe {
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        // 包一个 autorelease 池：app/icon/tiff/rep/png 都是 autoreleased（+0）。
        // 启动早期在 NSApp run 之前调用时主线程还没有池子，这些对象（尤其源 icon，可达数 MB）
        // 会整体泄漏——这是启动 ~40MB 的主因。
        // Wrap in an autorelease pool: app/icon/tiff/rep/png are autoreleased (+0). At startup
        // this runs before NSApp run (no pool yet), so they'd all leak - the ~40MB startup cause.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];

        let cls = class!(NSRunningApplication);
        let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        let icon: *mut AnyObject = msg_send![app, icon];
        if icon.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        // Render at Retina resolution: 64pt display → 128px (2x) or 64px (1x)
        let scale: f64 = {
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() { 2.0 }
            else { msg_send![screen, backingScaleFactor] }
        };
        let px = 128.0 * scale;

        let target_img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let target_img: *mut AnyObject = msg_send![target_img, initWithSize: NSSize::new(px, px)];

        // Draw icon into target with high-quality interpolation (NSImageInterpolationHigh)
        let _: () = msg_send![target_img, lockFocus];
        let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px, px));
        let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let op: usize = 1; // NSCompositingOperationCopy
        let _: () = msg_send![icon, drawInRect: dst, fromRect: src, operation: op, fraction: 1.0f64];
        let _: () = msg_send![target_img, unlockFocus];

        // Convert to PNG at target size
        let tiff: *mut AnyObject = msg_send![target_img, TIFFRepresentation];
        let _: () = msg_send![target_img, release]; // target_img 是 alloc 出来的 +1，池子接不住，手动 release
        if tiff.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        let rep_cls = class!(NSBitmapImageRep);
        let rep: *mut AnyObject = msg_send![rep_cls, imageRepWithData: tiff];
        if rep.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        // NSBitmapImageFileTypePNG = 4
        let png: *mut AnyObject = msg_send![rep, representationUsingType: 4u64, properties: std::ptr::null::<AnyObject>()];
        if png.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        let result = write_png_to_cache(png, pid);
        let _: () = msg_send![pool, drain];
        result
    }
}

pub fn raise_ax_window(pid: i32, cgwid: u32) {
    if cgwid == 0 { return; }
    unsafe {
        // 1. WindowServer 层只抬这一个窗口（SkyLight 私有 API _SLPSSetFrontProcessWithOptions），
        //    避免 activate(AllWindows) 把该 App 的所有窗口都抬到前面。
        //    Raise only this one window at the WindowServer level (SkyLight private API),
        //    avoiding activate(AllWindows) raising every window of the app.
        let slps_ok = raise_window_slps(pid, cgwid);

        // 2. AX 层聚焦该窗口：找到 CGWindowID 对应的 AX 窗口，setFocusedWindow + AXRaise。
        //    Focus the window via AX: find the AX window with this CGWindowID, then
        //    setFocusedWindow + AXRaise.
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() { return; }

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        if err != K_AX_SUCCESS || windows_array.is_null() { CFRelease(app); return; }

        let count = CFArrayGetCount(windows_array);
        let raise_key = cf_string_new("AXRaise");
        let focused_key = cf_string_new("AXFocusedWindow");
        let minimized_key = cf_string_new("AXMinimized");
        let mut matched = false;

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() { continue; }
            if ax_window_cgwid(element) == Some(cgwid) {
                // 先还原最小化(若已最小化),否则 AXRaise 可能只是带到前面而仍停在 Dock。
                // Un-minimize first (if minimized); otherwise AXRaise may bring it forward
                // without restoring from the Dock. Setting false on a non-minimized window is a no-op.
                AXUIElementSetAttributeValue(element, minimized_key, kCFBooleanFalse);
                AXUIElementSetAttributeValue(app, focused_key, element);
                AXUIElementPerformAction(element, raise_key);
                matched = true;
                log_info!("raise_ax_window: matched cgwid={} (slps={}), raised", cgwid, slps_ok);
                break;
            }
        }
        if !matched {
            log_warn!("raise_ax_window: NO MATCH for pid={} cgwid={} (ax_windows={}, slps={})", pid, cgwid, count, slps_ok);
        }
        CFRelease(raise_key);
        CFRelease(focused_key);
        CFRelease(minimized_key);
        CFRelease(windows_array);
        CFRelease(app);
    }
}

fn get_ax_windows_for_pid(pid: i32) -> Vec<(u32, String, bool)> {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() { return vec![]; }

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || windows_array.is_null() { return vec![]; }

        let count = CFArrayGetCount(windows_array);
        let title_key = cf_string_new("AXTitle");
        let subrole_key = cf_string_new("AXSubrole");
        let minimized_key = cf_string_new("AXMinimized");
        let mut results = Vec::with_capacity(count as usize);

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() { continue; }

            // 只保留标准窗口（AXStandardWindow），过滤弹出面板/下拉菜单等非标准窗口
            // Only keep AXStandardWindow, filtering out popups/panels/dropdowns
            let mut subrole_value: *const c_void = std::ptr::null();
            let is_standard = if AXUIElementCopyAttributeValue(element, subrole_key, &mut subrole_value) == K_AX_SUCCESS && !subrole_value.is_null() {
                let s = cf_to_rust_string(subrole_value);
                CFRelease(subrole_value);
                s.map_or(true, |sr| sr == "AXStandardWindow")
            } else {
                // 无 subrole → 视为标准窗口（部分 App 不设置此属性）
                // No subrole means standard window for apps that don't set it
                true
            };
            if !is_standard { continue; }

            let mut title_value: *const c_void = std::ptr::null();
            let title = if AXUIElementCopyAttributeValue(element, title_key, &mut title_value) == K_AX_SUCCESS && !title_value.is_null() {
                let t = cf_to_rust_string(title_value);
                CFRelease(title_value);
                t.unwrap_or_default()
            } else {
                String::new()
            };
            // AXMinimized:窗口是否最小化。无此属性(部分 App)按 false 处理。
            // AXMinimized: whether the window is minimized. Absent attribute (some apps) -> false.
            let minimized = {
                let mut min_value: *const c_void = std::ptr::null();
                if AXUIElementCopyAttributeValue(element, minimized_key, &mut min_value) == K_AX_SUCCESS && !min_value.is_null() {
                    let m = CFBooleanGetValue(min_value);
                    CFRelease(min_value);
                    m
                } else {
                    false
                }
            };
            // 取该 AX 窗口的 CGWindowID（私有 API），用于和 CG 窗口精确配对。
            let cgwid = ax_window_cgwid(element).unwrap_or(0);
            results.push((cgwid, title, minimized));
        }
        CFRelease(title_key);
        CFRelease(subrole_key);
        CFRelease(minimized_key);
        CFRelease(windows_array);
        results
    }
}

fn cf_to_rust_string(cf_string: *const c_void) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let ok = unsafe { CFStringGetCString(cf_string, buf.as_mut_ptr() as *mut i8, buf.len() as isize, 0x08000100) };
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).to_string())
    } else {
        None
    }
}

pub fn collect_windows(mru: &mut MruMap) -> Vec<WindowInfo> {
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    // show_minimized 打开时用 All(含离屏最小化窗口),否则 OnScreenOnly(原行为)。
    // When show_minimized is on, use All (includes off-screen minimized windows); else OnScreenOnly (original behavior).
    let cg_option = if show_minimized {
        K_C_G_WINDOW_LIST_OPTION_ALL
    } else {
        K_C_G_WINDOW_LIST_OPTION_ON_SCREEN_ONLY
    };
    let array = unsafe { CGWindowListCopyWindowInfo(cg_option, 0) };
    if array.is_null() { return vec![]; }

    let self_pid = std::process::id() as i32;
    let mut windows: Vec<WindowInfo> = Vec::new();
    let count = unsafe { CFArrayGetCount(array) };
    let now = Instant::now();
    let mut insertion_order: u32 = 0;

    // 第一遍遍历：收集所有 PID，用于批量查询 AX 窗口
    // First pass: collect all PIDs to batch query AX windows
    let mut pids: HashSet<i32> = HashSet::new();
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() { continue; }
        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 { continue; }
        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 || owner_pid == self_pid { continue; }
        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" { continue; }
        pids.insert(owner_pid);
    }

    // 以 AX 窗口列表为主数据源（macOS App Switcher 的做法）
    // Use AX window list as primary source (same as macOS App Switcher)
    let mut ax_by_pid: HashMap<i32, Vec<String>> = HashMap::new();
    // pid -> (CGWindowID -> (AX 标题, 是否最小化)),用于按 CGWindowID 精确配对 CG 窗口。
    // pid -> (CGWindowID -> (AX title, minimized)), to pair CG windows by CGWindowID.
    let mut ax_wid_to_info: HashMap<i32, HashMap<u32, (String, bool)>> = HashMap::new();
    // AX 窗口「全部」为空标题的 App（如 Microsoft To Do：自绘标题栏 -> AXTitle 为空）。
    // 这类 App 的空标题窗口是真实主窗口，不能被当作弹出面板丢弃。
    // Apps whose AX windows are ALL untitled (e.g. Microsoft To Do, which has a
    // custom title bar and an empty AXTitle). Their titleless windows are real
    // main windows and must not be dropped as popups.
    let mut titleless_pids: HashSet<i32> = HashSet::new();
    for &pid in &pids {
        let ax_wins = get_ax_windows_for_pid(pid);
        if !ax_wins.is_empty() {
            if ax_wins.iter().all(|(_, t, _)| t.is_empty()) {
                titleless_pids.insert(pid);
            }
            ax_by_pid.insert(pid, ax_wins.iter().map(|(_, t, _)| t.clone()).collect());
            let mut wid_map: HashMap<u32, (String, bool)> = HashMap::new();
            for (cgwid, title, minimized) in &ax_wins {
                if *cgwid != 0 { wid_map.insert(*cgwid, (title.clone(), *minimized)); }
            }
            ax_wid_to_info.insert(pid, wid_map);
        }
    }

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() { continue; }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 { continue; }

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 || owner_pid == self_pid { continue; }

        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" { continue; }

        let cg_title = cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default();
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);

        // 按 CGWindowID 精确配对 AX 标题（不再按顺序/字符串猜）。
        // AX 为权威数据源：CG 窗口必须能在 AX 里按 CGWindowID 找到才保留。
        // Pair the AX title by CGWindowID (no more order/string guessing).
        // AX is authoritative: a CG window is kept only if AX has a window with
        // the same CGWindowID.
        let (window_title, minimized) = if let Some(wid_map) = ax_wid_to_info.get(&owner_pid) {
            match wid_map.get(&cgwid) {
                Some((t, m)) => (t.clone(), *m),
                None => continue, // CG 窗口在 AX 里没有 -> 弹出面板，跳过 / popup, skip
            }
        } else {
            // 该 App 无 AX 数据 -> 退回 CG 标题,最小化状态未知按 false。
            // No AX data -> fall back to CG title; minimized status unknown, assume false.
            (cg_title, false)
        };

        // show_minimized 打开时(用 CG All 枚举):保留在屏窗口 或 最小化窗口;
        // 其它离屏窗口(别的 Space、隐藏)跳过。关闭时走 OnScreenOnly,此过滤恒通过。
        // When show_minimized is on (CG All): keep on-screen OR minimized windows; skip other
        // off-screen windows (other Spaces, hidden). When off (OnScreenOnly), always passes.
        if show_minimized {
            let is_onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false);
            if !is_onscreen && !minimized {
                continue;
            }
        }

        if window_title.is_empty()
            && (!ax_by_pid.contains_key(&owner_pid) || titleless_pids.contains(&owner_pid))
        {
            // 空标题窗口仅对「无 AX 支持」或「AX 窗口全部无标题」的 App 保留
            // Keep titleless windows for apps with no AX support, or apps whose
            // AX windows are all untitled (e.g. Microsoft To Do).
        } else if window_title.is_empty() {
            // 有标题窗口的 App 出现空标题 -> 视为弹出面板/下拉菜单，跳过
            // Empty title on an app that has titled windows -> popup/panel, skip
            continue;
        }

        // 使用极旧的时间戳作为回退值，确保真实 MRU 条目（用户曾选中过的窗口）永远排在前面。
        // ordeded_ts 在 CG 枚举顺序上递减，作为"无 MRU 记录"的窗口之间的稳定排序依据。
        // Use very old timestamps as fallback so real MRU entries (windows the user has
        // explicitly selected) always sort first. ordered_ts decreasing within CG enumeration
        // order serves as a stable tiebreaker among "no MRU record" windows.
        let ancient_base = now.checked_sub(std::time::Duration::from_secs(86_400)).unwrap_or(now);
        let ordered_ts = ancient_base.checked_sub(std::time::Duration::from_millis(insertion_order as u64)).unwrap_or(ancient_base);
        // mru 按 (pid, CGWindowID) 索引——CGWindowID 在窗口生命周期内稳定不变，
        // 比 title 更可靠（title 会随浏览器标签页切换而变）。
        // Key mru by (pid, CGWindowID) — CGWindowID is stable for the window's
        // lifetime, more reliable than title (which changes with browser tabs).
        mru.entry((owner_pid, cgwid)).or_insert(ordered_ts);
        insertion_order += 1;
        let icon_path = check_icon_cache(owner_pid);
        windows.push(WindowInfo { pid: owner_pid, window_id: cgwid, app_name: owner_name, window_title, icon_path, is_active: false, minimized });
    }

    unsafe { CFRelease(array) };

    // summon 时刷新前台聚焦窗口的 MRU 为 now。
    // 焦点变化若不触发 App 激活通知(如终端切 tab、或前台 App 从未被系统激活过),
    // 当前窗口的 MRU 会停在 ancient 回退值,排序退化成 CG 枚举顺序。每次 summon 把
    // "用户正看的窗口"刷成最新即可纠正。
    // 前台窗口的识别不依赖 CG 枚举顺序（show_minimized 开启时 CG ALL 枚举顺序不保证
    // z-order），改用系统级 API NSWorkspace.frontmostApplication + focused_window_cgwid
    // 精确定位。获取失败时回退到 CG 枚举中首个非最小化窗口。
    // Refresh the MRU of the frontmost focused window to now on summon. Focus changes that
    // don't fire an app-activation notification (e.g. switching terminal tabs, or a frontmost
    // app never system-activated) leave the current window's MRU at the ancient fallback,
    // degrading sort to CG enumeration order. Bumping the window the user is looking at on
    // every summon corrects this. The frontmost window is identified via system APIs
    // (NSWorkspace.frontmostApplication + focused_window_cgwid) rather than CG enumeration
    // order, because CG's "All" option doesn't guarantee z-order when show_minimized is on.
    // Fall back to the first non-minimized window in CG enumeration if the system API fails.
    let frontmost: Option<(i32, u32)> = unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let front_app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if !front_app.is_null() {
            let front_pid: i32 = msg_send![front_app, processIdentifier];
            focused_window_cgwid(front_pid).map(|cgwid| (front_pid, cgwid))
        } else {
            None
        }
    };
    if let Some((pid, cgwid)) = frontmost {
        mru.insert((pid, cgwid), now);
        let w = windows.iter().find(|w| w.pid == pid && w.window_id == cgwid);
        log_info!(
            "summon-bump frontmost: pid={} app=\"{}\" cgwid={} title=\"{}\"",
            pid,
            w.map_or("?", |w| w.app_name.as_str()),
            cgwid,
            w.map_or("?", |w| w.window_title.as_str()),
        );
    } else if let Some(w) = windows.iter().find(|w| !w.minimized) {
        // 回退：系统 API 获取失败时，取 CG 枚举中首个非最小化窗口
        // Fallback: when the system API fails, use the first non-minimized window in CG enumeration
        mru.insert((w.pid, w.window_id), now);
        log_info!(
            "summon-bump frontmost (fallback): pid={} app=\"{}\" cgwid={} title=\"{}\"",
            w.pid, w.app_name, w.window_id, w.window_title
        );
    }

    // 纯窗口级 MRU 排序：每个窗口独立按最后被激活的时间排序。
    // 不再使用 App 级 LAST_ACTIVATED——避免从 App C 切到浏览器的窗口 A 时，
    // 浏览器的另一个窗口 B 也"搭便车"排到 C 前面。
    // Pure window-level MRU sort: each window is sorted independently by
    // when it was last activated. No app-level LAST_ACTIVATED grouping —
    // prevents browser window B from riding A's coattails when switching
    // from app C to browser window A.
    windows.sort_by(|a, b| {
        let wa = mru.get(&(a.pid, a.window_id)).map(|t| t.elapsed()).unwrap_or(std::time::Duration::from_secs(999));
        let wb = mru.get(&(b.pid, b.window_id)).map(|t| t.elapsed()).unwrap_or(std::time::Duration::from_secs(999));
        wa.cmp(&wb)
    });

    // 每次 summon 时打印排序后的窗口列表（= 实际显示顺序），含 mru 年龄。`*` 标记第 0 个(当前/前台窗口)。
    // Print the sorted window list on every summon (= display order), with MRU age. `*` marks index 0 (current/frontmost).
    log_info!("sorted: {} windows", windows.len());
    for (i, w) in windows.iter().enumerate() {
        let mru_ms = mru.get(&(w.pid, w.window_id)).map(|t| t.elapsed().as_millis());
        let mark = if i == 0 { "*" } else { " " };
        log_info!("  {} pid={} app=\"{}\" cgwid={} title=\"{}\" mru_ms={:?}", mark, w.pid, w.app_name, w.window_id, w.window_title, mru_ms);
    }

    if let Some(first) = windows.first_mut() { first.is_active = true; }
    windows
}

/// Pre‑cache icons for every currently‑running application.
/// Called once at startup so the overlay never shows a missing icon.
pub fn cache_running_app_icons() {
    let mut cached: Vec<String> = Vec::new();
    let mut skipped: usize = 0;
    unsafe {
        // 本函数在 NSApp run 之前调用，主线程还没有 autorelease 池；
        // runningApplications / localizedName 都是 autoreleased，套个池子及时回收。
        // This runs before NSApp run, when the main thread has no autorelease pool yet;
        // runningApplications / localizedName are autoreleased, so wrap in a pool to drain them.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            let pid: i32 = msg_send![app, processIdentifier];
            if check_icon_cache(pid).is_none() {
                let name: *mut AnyObject = msg_send![app, localizedName];
                let utf8: *const c_char = msg_send![name, UTF8String];
                let name_str = if utf8.is_null() {
                    "?".to_string()
                } else {
                    CStr::from_ptr(utf8).to_string_lossy().into_owned()
                };
                log_info!("cached icon: {} (pid {})", name_str, pid);
                cached.push(name_str);
                extract_icon_to_cache(pid);
            } else {
                skipped += 1;
            }
        }
        let _: () = msg_send![pool, drain];
    }
    log_info!(
        "icon cache done: {} cached, {} skipped (already fresh)",
        cached.len(),
        skipped,
    );
}
