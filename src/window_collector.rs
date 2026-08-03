use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::time::Instant;

use crate::config::CONFIG;
use crate::{log_debug, log_info};

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub pid: i32,
    pub window_id: u32, // CGWindowID，用于精确 raise（SLPS）/配对
    pub app_name: String,
    pub window_title: String,
    pub icon_path: Option<String>,
    pub is_active: bool,
    pub minimized: bool, // 最小化窗口(show_minimized 打开时才收集)/ minimized (collected only when show_minimized is on)
    // CG 窗口 bounds (x, y, w, h),用于确定激活窗口所在屏幕。全 0 表示未获取到。
    // CG window bounds (x, y, w, h), used to locate the active window's screen. All zeros = unavailable.
    pub bounds: (f64, f64, f64, f64),
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
///
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
type SlpsPostEventRecordFn = unsafe extern "C" fn(*mut ProcessSerialNumber, *mut u8) -> i32;

static AX_GET_WINDOW: std::sync::LazyLock<Option<AxGetWindowFn>> = std::sync::LazyLock::new(
    || unsafe {
        // _AXUIElementGetWindow 是 HIServices 里的私有符号，dlopen 后 dlsym。
        let name = b"_AXUIElementGetWindow\0";
        let h = dlopen_path("/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices");
        if h.is_null() {
            return None;
        }
        let p = dlsym(h, name.as_ptr() as *const c_char);
        if p.is_null() {
            None
        } else {
            Some(std::mem::transmute::<*mut c_void, AxGetWindowFn>(p))
        }
    },
);
static GET_PROCESS_FOR_PID: std::sync::LazyLock<Option<GetProcessForPIDFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // GetProcessForPID 也在 HIServices，dlopen 后 dlsym。
        let name = b"GetProcessForPID\0";
        let h = dlopen_path("/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices");
        if h.is_null() {
            return None;
        }
        let p = dlsym(h, name.as_ptr() as *const c_char);
        if p.is_null() {
            None
        } else {
            Some(std::mem::transmute::<*mut c_void, GetProcessForPIDFn>(p))
        }
    });
static SLP_SET_FRONT: std::sync::LazyLock<Option<SlpSetFrontFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // SkyLight 是私有框架，必须先 dlopen 才能查到符号。
        let name = b"_SLPSSetFrontProcessWithOptions\0";
        let h = dlopen_path("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight");
        if h.is_null() {
            return None;
        }
        let p = dlsym(h, name.as_ptr() as *const c_char);
        if p.is_null() {
            None
        } else {
            Some(std::mem::transmute::<*mut c_void, SlpSetFrontFn>(p))
        }
    });
static SLPS_POST_EVENT_RECORD: std::sync::LazyLock<Option<SlpsPostEventRecordFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // make_key_window 的合成鼠标事件也走 SkyLight 的 SLPSPostEventRecordTo。
        // Synthetic mouse events for make_key_window go through SkyLight's
        // SLPSPostEventRecordTo as well.
        let name = b"SLPSPostEventRecordTo\0";
        let h = dlopen_path("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight");
        if h.is_null() {
            return None;
        }
        let p = dlsym(h, name.as_ptr() as *const c_char);
        if p.is_null() {
            None
        } else {
            Some(std::mem::transmute::<*mut c_void, SlpsPostEventRecordFn>(p))
        }
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
    if f(element, &mut wid) == K_AX_SUCCESS && wid != 0 {
        Some(wid)
    } else {
        None
    }
}

/// 查某 App 当前聚焦窗口的 CGWindowID（kAXFocusedWindow -> _AXUIElementGetWindow）。
/// 比 AX[0] 可靠：AX 窗口数组顺序不一定是最前在前，但 kAXFocusedWindow 是明确聚焦的窗口。
/// Get the CGWindowID of an app's currently focused window (kAXFocusedWindow ->
/// _AXUIElementGetWindow). More reliable than AX[0]: the AX window array order
/// isn't always frontmost-first, but kAXFocusedWindow is the explicitly focused window.
pub(crate) unsafe fn focused_window_cgwid(pid: i32) -> Option<u32> {
    let app = AXUIElementCreateApplication(pid);
    if app.is_null() {
        return None;
    }
    AXUIElementSetMessagingTimeout(app, 0.05); // 50ms 超时，避免卡死在无响应的 App 上。
    let focused_key = cf_string_new("AXFocusedWindow");
    let mut focused: *const c_void = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(app, focused_key, &mut focused);
    CFRelease(focused_key);
    CFRelease(app);
    if err != K_AX_SUCCESS || focused.is_null() {
        return None;
    }
    let wid = ax_window_cgwid(focused);
    CFRelease(focused);
    wid
}

/// 用 SkyLight 私有 API _SLPSSetFrontProcessWithOptions 在 WindowServer 层
/// 只抬起指定 CGWindowID 的那一个窗口（不抬该 App 的所有窗口）。
/// Raise only one window (by CGWindowID) at the WindowServer level via the
/// SkyLight private API _SLPSSetFrontProcessWithOptions -- does NOT raise all
/// of the app's windows the way activate(AllWindows) does.
///
/// mode 用 0x200(kCPSUserGenerated,yabai kCPS* 定义,AltTab 同款):把这次切换标记为
/// 「用户发起」。macOS 14+ 对非用户发起的程序化前台切换会抑制输入焦点转移(目标窗口
/// 变灰红绿灯,需要点击才能获得焦点),0x200 是绕过该抑制的关键。旧代码传的 2 不是
/// 有效标志位,正是灰红绿灯的根因之一。
/// The mode is 0x200 (kCPSUserGenerated, from yabai's kCPS* constants, same as AltTab):
/// it marks this front-switch as user-initiated. macOS 14+ suppresses input-focus transfer
/// for non-user-initiated programmatic front-switches (the target window's traffic lights go
/// grey until a click); 0x200 is what bypasses that suppression. The old code passed 2, which
/// is not a valid flag -- one root cause of the grey traffic lights.
unsafe fn raise_window_slps(pid: i32, wid: u32) -> bool {
    let get_psn = match *GET_PROCESS_FOR_PID {
        Some(f) => f,
        None => return false,
    };
    let set_front = match *SLP_SET_FRONT {
        Some(f) => f,
        None => return false,
    };
    let mut psn = ProcessSerialNumber {
        high_long_of_psn: 0,
        low_long_of_psn: 0,
    };
    if get_psn(pid, &mut psn) != 0 {
        return false;
    }
    set_front(&mut psn, wid, 0x200) == 0 // kCPSUserGenerated; CGError success == 0
}

/// 用 SkyLight 私有 API SLPSPostEventRecordTo 向目标窗口投递一对合成鼠标按下/抬起事件，
/// 使该窗口成为其 App 的 key window（红绿灯变彩色）。macOS 14+ 把 NSRunningApplication
/// activate 降级为「建议性请求」，跨 App 转移 key 焦点只剩这条可靠路径；0x200 的
/// userGenerated 前台切换只解决「抬到前面」，key 状态必须靠这个合成点击确立。
/// 点击点取窗口外缘 (-1,-1)：按下仍会使窗口变 key，但命中不到任何内容（不会误点控件，
/// 也避开 #5381 类全屏 UI 误触）。事件按 CGWindowID 定向投递，与坐标无关。
/// 字节布局来自 AltTab 对 CGSInternal/CGSEvent.h 的反向工程；缓冲必须 ≥0x100 并清零，
/// 否则 macOS 14.7.4+ 的 CGSEncodeEventRecord 会越界读取导致 SIGABRT（paneru#123）。
///
/// Make the window `wid` the key window of its app by posting a synthetic left-click
/// (down then up) to the WindowServer via the SkyLight private API SLPSPostEventRecordTo.
/// macOS 14+ downgraded NSRunningApplication.activate to an advisory "request"; posting
/// this click is the only reliable way left to move key focus across apps. The 0x200
/// userGenerated front-switch only fronts the window -- the key state needs this click.
/// The click is aimed just outside the window at (-1,-1): the press still makes the window
/// key, but it hit-tests to no view (nothing is clicked; avoids fullscreen-UI edge hits).
/// The event is delivered to the window by CGWindowID, not by the click point. The byte
/// layout is AltTab's reverse-engineering of CGSInternal/CGSEvent.h; the buffer must be
/// at least 0x100 bytes and zeroed, or CGSEncodeEventRecord reads past it on macOS 14.7.4+
/// and SIGABRTs (paneru issue 123).
unsafe fn make_key_window(pid: i32, wid: u32) -> bool {
    let get_psn = match *GET_PROCESS_FOR_PID {
        Some(f) => f,
        None => return false,
    };
    let post = match *SLPS_POST_EVENT_RECORD {
        Some(f) => f,
        None => return false,
    };
    let mut psn = ProcessSerialNumber {
        high_long_of_psn: 0,
        low_long_of_psn: 0,
    };
    if get_psn(pid, &mut psn) != 0 {
        return false;
    }
    // 0x100 字节清零缓冲:记录本身声明长度 0xf8(offset 0x04),多分配防止越界读崩溃。
    // Zeroed 0x100-byte buffer: the record declares 0xf8 (offset 0x04); the extra space
    // prevents the out-of-bounds read crash.
    let mut bytes = vec![0u8; 0x100];
    bytes[0x04] = 0xf8; // 记录长度 / record length
    bytes[0x3a] = 0x10; // 未公开标志(yabai/Hammerspoon 同款)/ undocumented flag (as yabai/Hammerspoon)
    // 目标 CGWindowID @ 0x3c(4 字节,小端)/ target CGWindowID @ 0x3c (4 bytes, LE)
    bytes[0x3c..0x40].copy_from_slice(&wid.to_le_bytes());
    // 窗口相对点击点 @ 0x20(16 字节 = CGPoint 两个 f64)/ window-relative click point @ 0x20
    bytes[0x20..0x28].copy_from_slice(&(-1.0f64).to_le_bytes());
    bytes[0x28..0x30].copy_from_slice(&(-1.0f64).to_le_bytes());
    // 0x08 = CGSEventType:先按下一(0x01)再抬起(0x02),一对合成点击使窗口变 key。
    // 0x08 = CGSEventType: post a left-mouse-down (0x01) then -up (0x02); the pair makes
    // the window key.
    bytes[0x08] = 0x01;
    let ok1 = post(&mut psn, bytes.as_mut_ptr()) == 0;
    bytes[0x08] = 0x02;
    let ok2 = post(&mut psn, bytes.as_mut_ptr()) == 0;
    if !ok1 || !ok2 {
        log_info!("make_key_window: SLPSPostEventRecordTo failed (down={} up={})", ok1, ok2);
    }
    ok1 && ok2
}

fn cf_string_new(s: &str) -> *const c_void {
    let c_str = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100) }
}

fn cf_dict_get_string(dict: *const c_void, key: &str) -> Option<String> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    cf_to_rust_string(value)
}

fn cf_dict_get_i32(dict: *const c_void, key: &str) -> Option<i32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok {
        Some(num)
    } else {
        None
    }
}

fn cf_dict_get_u32(dict: *const c_void, key: &str) -> Option<u32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok {
        Some(num as u32)
    } else {
        None
    }
}

/// 读 CG dict 里的 CFNumber double(如 kCGWindowAlpha)。CFNumberGetValue type 13 =
/// kCFNumberDoubleType。
/// Read a CFNumber double from a CG dict (e.g. kCGWindowAlpha). CFNumberGetValue type 13 =
/// kCFNumberDoubleType.
fn cf_dict_get_f64(dict: *const c_void, key: &str) -> Option<f64> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: f64 = 0.0;
    let ok = unsafe { CFNumberGetValue(value, 13, &mut num as *mut f64 as *mut c_void) };
    if ok {
        Some(num)
    } else {
        None
    }
}

// 读 CG dict 里的 CFBoolean(如 kCGWindowIsOnscreen)。/ Read a CFBoolean from a CG dict (e.g. kCGWindowIsOnscreen).
fn cf_dict_get_bool(dict: *const c_void, key: &str) -> Option<bool> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    Some(unsafe { CFBooleanGetValue(value) })
}

/// 读 CG dict 里的 kCGWindowBounds(嵌套 dict:X/Y/Width/Height),返回 (x, y, w, h)。
/// 用于确定激活窗口所在屏幕(overlay 的"跟随激活窗口"定位)。
///
/// Read kCGWindowBounds (a nested dict: X/Y/Width/Height) from a CG dict, returning (x, y, w, h).
/// Used to determine the active window's screen (the overlay's "follow active window" placement).
fn cf_dict_get_bounds(dict: *const c_void, key: &str) -> Option<(f64, f64, f64, f64)> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let x = cf_dict_get_f64(value, "X")?;
    let y = cf_dict_get_f64(value, "Y")?;
    let w = cf_dict_get_f64(value, "Width")?;
    let h = cf_dict_get_f64(value, "Height")?;
    Some((x, y, w, h))
}

/// 一个运行中 App 的缓存身份:`key` 用作缓存文件名,`fingerprint` 用于检测 App 更新。
/// A running app's cache identity: `key` is the cache filename, `fingerprint` detects updates.
struct AppIdentity {
    key: String,
    /// 可执行文件 mtime(自 UNIX epoch 的秒数)。None 表示无法校验,退化为「文件存在即有效」。
    /// Executable mtime (seconds since UNIX epoch). None means unverified -> "file exists = valid".
    fingerprint: Option<String>,
}

/// 读一个 NSString 到 Rust String(nil -> None)。对象是 autoreleased,调用方需在池内。
/// Read an NSString into a Rust String (nil -> None). The object is autoreleased; caller must be in a pool.
unsafe fn read_nsstring(obj: *mut AnyObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![obj, UTF8String];
    if utf8.is_null() {
        return None;
    }
    Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
}

/// FNV-1a 64-bit -> 十六进制。给非 bundle 应用的可执行文件路径做确定性短键,
/// 避免路径里的 `/` 撞文件名空间。
/// FNV-1a 64-bit -> hex. Gives non-bundle apps a deterministic short key from their
/// exec path, avoiding the `/` in paths colliding with the filename namespace.
fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// 解析一个 PID 对应 App 的缓存身份。
/// 键优先级:bundleIdentifier(reverse-DNS,文件名安全)> 可执行文件路径哈希 > `pid_{pid}` 兜底。
/// 指纹取可执行文件 mtime;App 更新会换新 mtime -> 触发重提。
///
/// Resolve a PID's cache identity. Key priority: bundleIdentifier (reverse-DNS,
/// filename-safe) > hashed executable path > `pid_{pid}` fallback. Fingerprint is the
/// executable mtime; an app update gets a new mtime -> forces re-extract.
unsafe fn resolve_app_identity(pid: i32) -> AppIdentity {
    let app: *mut AnyObject =
        msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
    if app.is_null() {
        // PID 已失效(App 刚退出)-> 回退到 pid 键,无指纹(无法校验)。
        // PID stale (app just quit) -> fall back to pid key, no fingerprint (can't verify).
        return AppIdentity {
            key: format!("pid_{}", pid),
            fingerprint: None,
        };
    }

    let bid_obj: *mut AnyObject = msg_send![app, bundleIdentifier];
    let bundle_id = read_nsstring(bid_obj);

    let exec_url: *mut AnyObject = msg_send![app, executableURL];
    let exec_path = if exec_url.is_null() {
        None
    } else {
        let path_obj: *mut AnyObject = msg_send![exec_url, path];
        read_nsstring(path_obj)
    };

    // 指纹 = 可执行文件 mtime(秒)。取不到(路径为空 / stat 失败)-> None,退化为不校验。
    // Fingerprint = exec mtime (seconds). Unavailable (empty path / stat fail) -> None, no verification.
    let fingerprint = exec_path.as_ref().and_then(|p| {
        std::fs::metadata(p)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().to_string())
    });

    let key = if let Some(bid) = bundle_id {
        bid
    } else if let Some(p) = exec_path {
        format!("exec_{}", fnv1a_hex(&p))
    } else {
        format!("pid_{}", pid)
    };

    AppIdentity { key, fingerprint }
}

fn cache_path_for_key(key: &str) -> String {
    format!("{}/{}.png", icon_cache_dir(), key)
}

fn meta_path_for_key(key: &str) -> String {
    format!("{}/{}.meta", icon_cache_dir(), key)
}

/// 校验缓存是否有效:PNG 存在,且(若有指纹)sidecar 指纹与当前一致。
/// App 更新会换 mtime -> 指纹不符 -> 返回 None,触发重提。
///
/// Validate the cache: PNG exists, and (when a fingerprint is present) the sidecar
/// matches. An app update changes the mtime -> fingerprint mismatch -> None -> re-extract.
fn check_cache_for_identity(id: &AppIdentity) -> Option<String> {
    let png = cache_path_for_key(&id.key);
    if std::fs::metadata(&png).is_err() {
        return None;
    }
    match &id.fingerprint {
        Some(fp) => match std::fs::read_to_string(meta_path_for_key(&id.key)) {
            Ok(stored) if stored.trim() == *fp => Some(png),
            _ => None,
        },
        None => Some(png), // 无指纹(极端兜底)-> 文件存在即有效 / no fingerprint -> file exists = valid
    }
}

pub fn ensure_icon_cache_dir() {
    let _ = std::fs::create_dir_all(icon_cache_dir());
}

/// 一次性迁移:删除旧版按 PID 命名的缓存文件(文件名 stem 纯数字)。
/// 新版键为 bundle id(含字母/点)或 `exec_`/`pid_` 前缀,绝不会是纯数字,故不会误删。
/// One-shot migration: remove legacy PID-named cache files (purely-numeric filename stem).
/// New keys are bundle ids (letters/dots) or `exec_`/`pid_`-prefixed, never purely numeric,
/// so nothing legitimate is touched.
pub fn migrate_legacy_cache() {
    let Ok(entries) = std::fs::read_dir(icon_cache_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 只删 .png 且 stem 纯数字的旧 PID 文件。
        // Only remove .png files whose stem is purely numeric (legacy PID files).
        if path.extension().and_then(|e| e.to_str()) == Some("png") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// 清空图标缓存目录(删除所有 {key}.png + {key}.meta),然后重建空目录。
/// 内存里 WindowInfo.icon_path 不会自动失效,调用方需自行将其置 None 并触发重提取。
///
/// Clear the icon cache directory (remove all {key}.png + {key}.meta), then recreate it empty.
/// In-memory WindowInfo.icon_path is NOT invalidated here; the caller must reset it to None
/// and trigger re-extraction.
pub fn clear_icon_cache() {
    let dir = icon_cache_dir();
    // remove_dir_all 在目录不存在时会报错,忽略即可 / errors if the dir doesn't exist; ignore
    let _ = std::fs::remove_dir_all(&dir);
    ensure_icon_cache_dir();
}

/// 图标缓存按应用 bundle id 索引(非 bundle 应用回退到可执行文件路径),不再按 PID:
/// PID 复用不会再读到别的 App 的旧图标。每个条目配一个 `.meta` sidecar 存可执行文件
/// mtime,App 更新/重装会换 mtime -> 校验不符 -> 重新提取,因此无需 TTL。运行时改图标
/// 的 App(日历日期 / Dock 角标)仍会冻结到下次清缓存--这是已接受的次要限制。
///
/// The icon cache is keyed by the app's bundle id (falling back to the executable path for
/// non-bundle apps), NOT by PID: PID recycling can never serve another app's stale icon. Each
/// entry has a `.meta` sidecar storing the executable mtime; an app update/reinstall changes
/// the mtime -> verification fails -> re-extract, so no TTL is needed. Apps that change their
/// icon at runtime (Calendar date, dock badge) still freeze until the cache is cleared - an
/// accepted minor limitation.
pub fn check_icon_cache(pid: i32) -> Option<String> {
    let id = unsafe { resolve_app_identity(pid) };
    check_cache_for_identity(&id)
}

fn write_png_to_cache(png: *mut AnyObject, key: &str) -> Option<String> {
    unsafe {
        let path = cache_path_for_key(key);
        let path_cstr = std::ffi::CString::new(&*path).unwrap();
        let cf_path = CFStringCreateWithCString(std::ptr::null(), path_cstr.as_ptr(), 0x08000100);
        // 原子写入：先写临时文件再重命名，避免写一半崩溃留下半截 PNG。
        // Atomic write: write to a temp file then rename, so a mid-write crash
        // can't leave a half-written PNG that "file exists = valid" would trust.
        let ok: bool = msg_send![png, writeToFile: cf_path as *mut AnyObject, atomically: true];
        CFRelease(cf_path);
        if ok {
            Some(path)
        } else {
            None
        }
    }
}

pub fn extract_icon_to_cache(pid: i32) -> Option<String> {
    unsafe {
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        // 包一个 autorelease 池：app/icon/tiff/rep/png 都是 autoreleased（+0）。
        // 启动早期在 NSApp run 之前调用时主线程还没有池子，这些对象（尤其源 icon，可达数 MB）
        // 会整体泄漏——这是启动 ~40MB 的主因。
        // Wrap in an autorelease pool: app/icon/tiff/rep/png are autoreleased (+0). At startup
        // this runs before NSApp run (no pool yet), so they'd all leak - the ~40MB startup cause.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];

        let id = resolve_app_identity(pid);
        // 命中既有且有效的缓存(含 mtime 校验)-> 跳过提取。
        // Hit an existing valid cache (mtime-verified) -> skip extraction.
        if let Some(path) = check_cache_for_identity(&id) {
            let _: () = msg_send![pool, drain];
            return Some(path);
        }

        // 源图标:自身进程用编译期嵌入的 AppIcon.icns--cargo run 是裸 exec 无 bundle,
        // NSRunningApplication.icon 会返回通用 exec 图标(带 EXEC 字样);这里强制用我们
        // 自己的图标,开发与打包表现一致。其他进程仍走 NSRunningApplication.icon。
        //
        // Source icon: for our own process use the compile-time-embedded AppIcon.icns --
        // cargo run is a bare exec with no bundle, so NSRunningApplication.icon returns the
        // generic exec icon (the "EXEC" placeholder); this forces our own icon so dev and
        // bundled builds match. Other processes still go through NSRunningApplication.icon.
        let icon: *mut AnyObject = if pid == std::process::id() as i32 {
            let icns_bytes: &[u8] = include_bytes!("../assets/AppIcon.icns");
            let nsdata: *mut AnyObject = msg_send![
                class!(NSData),
                dataWithBytes: icns_bytes.as_ptr() as *const c_void,
                length: icns_bytes.len()
            ];
            // NSImage 没有 +imageWithData: 类方法,用 alloc + initWithData:(+1)再 autorelease,
            // 让它和下面的 app.icon 一样由池子回收,无需手动 release。
            // NSImage has no +imageWithData: class method; use alloc + initWithData: (+1) then
            // autorelease so it's pool-managed like app.icon below, with no manual release.
            let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
            let img: *mut AnyObject = msg_send![img, initWithData: nsdata];
            if !img.is_null() {
                let _: *mut AnyObject = msg_send![img, autorelease];
            }
            img
        } else {
            let cls = class!(NSRunningApplication);
            let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                let _: () = msg_send![pool, drain];
                return None;
            }
            msg_send![app, icon]
        };
        if icon.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        // Render at Retina resolution: 64pt display → 128px (2x) or 64px (1x)
        let scale: f64 = {
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() {
                2.0
            } else {
                msg_send![screen, backingScaleFactor]
            }
        };
        let px = 128.0 * scale;

        let target_img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let target_img: *mut AnyObject = msg_send![target_img, initWithSize: NSSize::new(px, px)];

        // Draw icon into target with high-quality interpolation (NSImageInterpolationHigh)
        let _: () = msg_send![target_img, lockFocus];
        let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px, px));
        let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let op: usize = 1; // NSCompositingOperationCopy
        let _: () =
            msg_send![icon, drawInRect: dst, fromRect: src, operation: op, fraction: 1.0f64];
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

        let result = write_png_to_cache(png, &id.key);
        // 写 mtime sidecar:下次命中时据此判断 App 是否更新过(mtime 变 -> 重提)。
        // 仅在 PNG 写成功时写,避免留下无 PNG 的孤儿 meta。
        // Write the mtime sidecar: next hit checks it to detect app updates (mtime change ->
        // re-extract). Only written when the PNG succeeds, so no orphan meta is left behind.
        if result.is_some() {
            if let Some(fp) = &id.fingerprint {
                let _ = std::fs::write(meta_path_for_key(&id.key), fp);
            }
        }
        let _: () = msg_send![pool, drain];
        result
    }
}

pub fn raise_ax_window(pid: i32, cgwid: u32) {
    if cgwid == 0 {
        return;
    }
    unsafe {
        // 1. WindowServer 层只抬这一个窗口（SkyLight 私有 API _SLPSSetFrontProcessWithOptions，
        //    mode=0x200 userGenerated），避免 activate(AllWindows) 把该 App 的所有窗口都抬到前面。
        //    Raise only this one window at the WindowServer level (SkyLight private API,
        //    mode=0x200 userGenerated), avoiding activate(AllWindows) raising every window.
        let slps_ok = raise_window_slps(pid, cgwid);

        // 1.5 合成鼠标点击确立 key window：SLPS 只负责「抬到前面 + 设为前台进程」，key 状态
        //    必须由这个合成点击授予（macOS 14+ 无公开 API 可跨 App 转移 key 焦点）。
        //    顺序对齐 AltTab：SLPS → makeKeyWindow → AXRaise。失败不影响窗口抬起，仅记日志。
        //    The synthetic click establishes the key window: SLPS only fronts the window and
        //    process; the key state is granted by this click (macOS 14+ has no public API to
        //    move key focus across apps). Order mirrors AltTab: SLPS → makeKeyWindow → AXRaise.
        //    Failure doesn't stop the raise, it is only logged.
        let _ = make_key_window(pid, cgwid);

        // 2. AX 层聚焦该窗口：找到 CGWindowID 对应的 AX 窗口，setFocusedWindow + AXRaise。
        //    Focus the window via AX: find the AX window with this CGWindowID, then
        //    setFocusedWindow + AXRaise.
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return;
        }

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        if err != K_AX_SUCCESS || windows_array.is_null() {
            CFRelease(app);
            return;
        }

        let count = CFArrayGetCount(windows_array);
        let raise_key = cf_string_new("AXRaise");
        let focused_key = cf_string_new("AXFocusedWindow");
        let minimized_key = cf_string_new("AXMinimized");
        let mut matched = false;

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() {
                continue;
            }
            if ax_window_cgwid(element) == Some(cgwid) {
                // 先还原最小化(若已最小化),否则 AXRaise 可能只是带到前面而仍停在 Dock。
                // Un-minimize first (if minimized); otherwise AXRaise may bring it forward
                // without restoring from the Dock. Setting false on a non-minimized window is a no-op.
                AXUIElementSetAttributeValue(element, minimized_key, kCFBooleanFalse);
                AXUIElementSetAttributeValue(app, focused_key, element);
                AXUIElementPerformAction(element, raise_key);
                matched = true;
                log_info!(
                    "raise_ax_window: matched cgwid={} (slps={}), raised",
                    cgwid,
                    slps_ok
                );
                break;
            }
        }
        if !matched {
            log_info!(
                "raise_ax_window: NO MATCH for pid={} cgwid={} (ax_windows={}, slps={})",
                pid,
                cgwid,
                count,
                slps_ok
            );
        }
        CFRelease(raise_key);
        CFRelease(focused_key);
        CFRelease(minimized_key);
        CFRelease(windows_array);
        CFRelease(app);
    }
}

/// 查一个 PID 的 AX 标准窗口列表。
/// 返回 None = AX 查询失败(该 App 无 AX 数据,可走 CG 回退);
/// Some(vec) = 查询成功,subrole 过滤后可能为空(无标准窗口 = 调度中心不显示它,直接跳过)。
///
/// Query an app's AX standard-window list.
/// None = AX query failed (app has no AX data; CG fallback allowed);
/// Some(vec) = query succeeded, possibly empty after subrole filtering (no standard windows =
/// Mission Control won't show it; skip entirely).
fn get_ax_windows_for_pid(pid: i32) -> Option<Vec<(u32, String, bool)>> {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }

        // 设 50ms 消息超时:慢/无响应 App 的 AX 查询会快速失败,而不是卡默认 10s 超时
        // (后者会让该 App 整体走 CG 回退,混入隐形窗口)。
        // Set a 50ms messaging timeout: AX queries on slow/unresponsive apps fail fast instead
        // of hitting the default 10s timeout (which would push the app to the CG fallback path
        // and let invisible windows through).
        AXUIElementSetMessagingTimeout(app, 0.05);

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || windows_array.is_null() {
            return None;
        }

        let count = CFArrayGetCount(windows_array);
        let title_key = cf_string_new("AXTitle");
        let subrole_key = cf_string_new("AXSubrole");
        let minimized_key = cf_string_new("AXMinimized");
        let mut results = Vec::with_capacity(count as usize);

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() {
                continue;
            }

            // 只保留标准窗口（AXStandardWindow），过滤弹出面板/下拉菜单等非标准窗口
            // Only keep AXStandardWindow, filtering out popups/panels/dropdowns
            let mut subrole_value: *const c_void = std::ptr::null();
            let is_standard =
                if AXUIElementCopyAttributeValue(element, subrole_key, &mut subrole_value)
                    == K_AX_SUCCESS
                    && !subrole_value.is_null()
                {
                    let s = cf_to_rust_string(subrole_value);
                    CFRelease(subrole_value);
                    s.is_none_or(|sr| sr == "AXStandardWindow")
                } else {
                    // 无 subrole → 视为标准窗口（部分 App 不设置此属性）
                    // No subrole means standard window for apps that don't set it
                    true
                };
            if !is_standard {
                continue;
            }

            let mut title_value: *const c_void = std::ptr::null();
            let title = if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                == K_AX_SUCCESS
                && !title_value.is_null()
            {
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
                if AXUIElementCopyAttributeValue(element, minimized_key, &mut min_value)
                    == K_AX_SUCCESS
                    && !min_value.is_null()
                {
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
        Some(results)
    }
}

fn cf_to_rust_string(cf_string: *const c_void) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let ok = unsafe {
        CFStringGetCString(
            cf_string,
            buf.as_mut_ptr() as *mut i8,
            buf.len() as isize,
            0x08000100,
        )
    };
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
    if array.is_null() {
        return vec![];
    }

    // 不再按 PID 排除本应用(own-PID)窗口:设置窗口也是 own-PID,排除它会导致设置
    // 开着时切不到它。浮窗自己不需要靠 PID 排除--它 setLevel:3(floating,
    // kCGWindowLayer != 0)已被下面的 layer 过滤挡掉,且 summon 时 collect 先于
    // show_overlay 调用、浮窗尚离屏,OnScreenOnly 也不会枚举到。设置窗口关着时走
    // orderOut 离屏,OnScreenOnly 自然排除,故"开->显示为卡片、关->不显示"自动成立。
    //
    // Own-PID windows are no longer excluded by PID: the settings window is own-PID too, and
    // excluding it would make it unswitchable while open. The overlay itself needs no PID
    // exclusion -- it's setLevel:3 (floating, kCGWindowLayer != 0), already dropped by the
    // layer check below, and collect runs before show_overlay so the overlay is still off-screen
    // and OnScreenOnly won't enumerate it either. The settings window, when closed, is
    // orderOut'd (off-screen) so OnScreenOnly excludes it -- "open -> shown as a card, closed
    // -> hidden" holds automatically.
    let mut windows: Vec<WindowInfo> = Vec::new();
    let count = unsafe { CFArrayGetCount(array) };
    let now = Instant::now();
    let mut insertion_order: u32 = 0;

    // 第一遍遍历：收集所有 PID，用于批量查询 AX 窗口
    // First pass: collect all PIDs to batch query AX windows
    let mut pids: HashSet<i32> = HashSet::new();
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }
        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 {
            continue;
        }
        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
            continue;
        }
        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }
        pids.insert(owner_pid);
    }

    // 以 AX 窗口列表为主数据源（macOS App Switcher 的做法）
    // Use AX window list as primary source (same as macOS App Switcher)
    // AX 查询「成功」的 pid 集合:成功但无标准窗口的 App 应整体跳过(调度中心不显示它),
    // 只有查询失败(None)才允许走 CG 回退。这是 BetterDisplay 隐形窗口 bug 的根因修复:
    // AX 成功但 subrole 过滤后为空,不能等同于「无 AX 数据」。
    // Pids whose AX query SUCCEEDED: an app with a successful query but no standard windows
    // must be skipped entirely (Mission Control doesn't show it); only a failed query (None)
    // allows the CG fallback. This fixes the BetterDisplay invisible-window bug: an AX query
    // that succeeds but yields no standard windows must not be treated as "no AX data".
    let mut ax_queried_pids: HashSet<i32> = HashSet::new();
    // pid -> (CGWindowID -> (AX 标题, 是否最小化)),用于按 CGWindowID 精确配对 CG 窗口。
    // pid -> (CGWindowID -> (AX title, minimized)), to pair CG windows by CGWindowID.
    let mut ax_wid_to_info: HashMap<i32, HashMap<u32, (String, bool)>> = HashMap::new();
    // AX 窗口「全部」为空标题的 App（如 Microsoft To Do：自绘标题栏 -> AXTitle 为空）。
    // 这类 App 的空标题窗口是真实主窗口，不能被当作弹出面板丢弃。
    // Apps whose AX windows are ALL untitled (e.g. Microsoft To Do, which has a
    // custom title bar and an empty AXTitle). Their titleless windows are real
    // main windows and must not be dropped as popups.
    let mut titleless_pids: HashSet<i32> = HashSet::new();
    // pid -> 缓存身份(bundle id + mtime)。在此 AX 循环里按 pid 解析一次,供第二遍按窗口查缓存,
    // 避免每个窗口都做一次 NSRunningApplication 查找(一个 App 多窗口时尤其浪费)。
    // pid -> cache identity (bundle id + mtime). Resolved once per pid in this AX loop so the
    // second pass can look up the cache per-window without an NSRunningApplication call each time
    // (wasteful when one app has many windows).
    let mut icon_ids: HashMap<i32, AppIdentity> = HashMap::new();
    for &pid in &pids {
        icon_ids.insert(pid, unsafe { resolve_app_identity(pid) });
        let ax_wins = get_ax_windows_for_pid(pid);
        match ax_wins {
            Some(wins) if !wins.is_empty() => {
                ax_queried_pids.insert(pid);
                if wins.iter().all(|(_, t, _)| t.is_empty()) {
                    titleless_pids.insert(pid);
                }
                let mut wid_map: HashMap<u32, (String, bool)> = HashMap::new();
                for (cgwid, title, minimized) in &wins {
                    if *cgwid != 0 {
                        wid_map.insert(*cgwid, (title.clone(), *minimized));
                    }
                }
                ax_wid_to_info.insert(pid, wid_map);
            }
            // AX 查询成功但无标准窗口:该 App 没有调度中心可见的窗口,后续直接跳过。
            // AX query succeeded but no standard windows: the app has no Mission-Control-visible
            // windows; skip all its CG windows in the second pass.
            Some(_) => {
                ax_queried_pids.insert(pid);
            }
            // AX 查询失败(无 AX 数据):保留 CG 回退路径。
            // AX query failed (no AX data): keep the CG fallback path.
            None => {}
        }
    }

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 {
            continue;
        }

        // 全透明窗口(alpha=0)不可见,调度中心不显示,跳过。
        // Fully transparent windows (alpha=0) are invisible; Mission Control doesn't show them.
        let alpha = cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0);
        if alpha <= 0.0 {
            continue;
        }

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
            continue;
        }

        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }

        let cg_title = cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default();
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        // bounds (x, y, w, h);解析失败时全 0,调用方会回退到主屏幕。
        // bounds (x, y, w, h); all zeros on parse failure, caller falls back to the main screen.
        let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or((0.0, 0.0, 0.0, 0.0));

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
        } else if ax_queried_pids.contains(&owner_pid) {
            // AX 查询成功但该 App 无标准窗口(如 BetterDisplay 的隐形锚点窗口):
            // 调度中心不显示它,这里也整体跳过,不回退到 CG。
            // AX query succeeded but the app has no standard windows (e.g. BetterDisplay's
            // invisible anchor window): Mission Control doesn't show it, skip entirely —
            // no CG fallback.
            continue;
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

        // 空标题窗口仅对「AX 确认过全部窗口无标题」的 App(titleless_pids)保留;
        // AX 查询失败回退的 App 不再享受空标题豁免(无法确认其窗口身份)。
        // Titleless windows are kept only for apps AX confirmed as all-untitled
        // (titleless_pids); the AX-failed fallback no longer exempts empty titles
        // (window identity can't be verified there).
        if window_title.is_empty() && !titleless_pids.contains(&owner_pid) {
            continue;
        }

        // 使用极旧的时间戳作为回退值，确保真实 MRU 条目（用户曾选中过的窗口）永远排在前面。
        // ordeded_ts 在 CG 枚举顺序上递减，作为无 MRU 记录的窗口之间的稳定排序依据。
        // Use very old timestamps as fallback so real MRU entries (windows the user has
        // explicitly selected) always sort first. ordered_ts decreasing within CG enumeration
        // order serves as a stable tiebreaker among "no MRU record" windows.
        let ancient_base = now
            .checked_sub(std::time::Duration::from_secs(86_400))
            .unwrap_or(now);
        let ordered_ts = ancient_base
            .checked_sub(std::time::Duration::from_millis(insertion_order as u64))
            .unwrap_or(ancient_base);
        // mru 按 (pid, CGWindowID) 索引——CGWindowID 在窗口生命周期内稳定不变，
        // 比 title 更可靠（title 会随浏览器标签页切换而变）。
        // Key mru by (pid, CGWindowID) — CGWindowID is stable for the window's
        // lifetime, more reliable than title (which changes with browser tabs).
        mru.entry((owner_pid, cgwid)).or_insert(ordered_ts);
        insertion_order += 1;
        let icon_path = icon_ids.get(&owner_pid).and_then(check_cache_for_identity);
        windows.push(WindowInfo {
            pid: owner_pid,
            window_id: cgwid,
            app_name: owner_name,
            window_title,
            icon_path,
            is_active: false,
            minimized,
            bounds,
        });
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
        let w = windows
            .iter()
            .find(|w| w.pid == pid && w.window_id == cgwid);
        log_debug!(
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
        log_debug!(
            "summon-bump frontmost (fallback): pid={} app=\"{}\" cgwid={} title=\"{}\"",
            w.pid,
            w.app_name,
            w.window_id,
            w.window_title
        );
    }

    // 纯窗口级 MRU 排序：每个窗口独立按最后被激活的时间排序。
    // 不再使用 App 级 LAST_ACTIVATED——避免从 App C 切到浏览器的窗口 A 时，
    // 浏览器的另一个窗口 B 也搭便车排到 C 前面。
    // Pure window-level MRU sort: each window is sorted independently by
    // when it was last activated. No app-level LAST_ACTIVATED grouping —
    // prevents browser window B from riding A's coattails when switching
    // from app C to browser window A.
    windows.sort_by(|a, b| {
        let wa = mru
            .get(&(a.pid, a.window_id))
            .map(|t| t.elapsed())
            .unwrap_or(std::time::Duration::from_secs(999));
        let wb = mru
            .get(&(b.pid, b.window_id))
            .map(|t| t.elapsed())
            .unwrap_or(std::time::Duration::from_secs(999));
        wa.cmp(&wb)
    });

    // 每次 summon 时打印排序后的窗口列表（= 实际显示顺序），含 mru 年龄。`*` 标记第 0 个(当前/前台窗口)。
    // Print the sorted window list on every summon (= display order), with MRU age. `*` marks index 0 (current/frontmost).
    log_debug!("sorted: {} windows", windows.len());
    for (i, w) in windows.iter().enumerate() {
        let mru_ms = mru
            .get(&(w.pid, w.window_id))
            .map(|t| t.elapsed().as_millis());
        let mark = if i == 0 { "*" } else { " " };
        log_debug!(
            "  {} pid={} app=\"{}\" cgwid={} title=\"{}\" mru_ms={:?}",
            mark,
            w.pid,
            w.app_name,
            w.window_id,
            w.window_title,
            mru_ms
        );
    }

    if let Some(first) = windows.first_mut() {
        first.is_active = true;
    }
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
                log_debug!("cached icon: {} (pid {})", name_str, pid);
                cached.push(name_str);
                extract_icon_to_cache(pid);
            } else {
                skipped += 1;
            }
        }
        let _: () = msg_send![pool, drain];
    }
    log_debug!(
        "icon cache done: {} cached, {} skipped (already fresh)",
        cached.len(),
        skipped,
    );
}
