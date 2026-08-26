use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::time::{Duration, Instant};

use crate::config::CONFIG;
use crate::{log_debug, log_info};

#[derive(Debug, Clone, PartialEq)]
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
/// 仅作为新窗口第一次进入 MRU 表时的启动种子；已有窗口永不随 App 激活整体更新。
/// PID → last app activation time (via NSWorkspace notification).
/// Used only to seed a window the first time it enters the MRU map; existing windows
/// are never updated together when their app activates.
static LAST_ACTIVATED: std::sync::LazyLock<std::sync::Mutex<HashMap<i32, Instant>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// 通过 NSWorkspaceDidActivateApplicationNotification 发送，由 main.rs 调用。
/// 返回本次激活 token，供异步焦点查询丢弃迟到结果。
/// Called from the NSWorkspaceDidActivateApplicationNotification handler in main.rs.
/// Returns this activation's token so async focus queries can discard stale results.
pub fn note_app_activated(pid: i32) -> Instant {
    let activated_at = Instant::now();
    LAST_ACTIVATED.lock().unwrap().insert(pid, activated_at);
    activated_at
}

/// 异步激活查询是否仍对应 PID 的最新一次激活。
/// Whether an async activation query still belongs to the PID's latest activation.
pub fn app_activation_is_current(pid: i32, activated_at: Instant) -> bool {
    LAST_ACTIVATED.lock().unwrap().get(&pid).copied() == Some(activated_at)
}

/// App 退出时清除激活种子，避免 PID 复用继承旧进程的时间。
/// Clear the activation seed on termination so PID reuse cannot inherit old process state.
pub fn note_app_terminated(pid: i32) {
    LAST_ACTIVATED.lock().unwrap().remove(&pid);
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

/// 纯窗口级 MRU 排序:按最后激活时间升序(最近使用的在前),无 MRU 记录的窗口
/// 回退到 999 秒(视为极旧,排最后)。纯函数,`now` 由调用方给定,测试可注入固定时间。
/// Pure window-level MRU sort: ascending by last-activation time (most recent first);
/// windows without an MRU record fall back to 999s (treated as very old, sorted last).
/// Pure — `now` is supplied by the caller, so tests can inject a fixed clock.
pub(crate) fn sort_windows_by_mru(windows: &mut [WindowInfo], mru: &MruMap, now: Instant) {
    let age = |pid: i32, wid: u32| {
        mru.get(&(pid, wid))
            .map(|t| now.saturating_duration_since(*t))
            .unwrap_or(std::time::Duration::from_secs(999))
    };
    windows.sort_by_key(|a| age(a.pid, a.window_id));
}

/// 修剪 MRU:删除不在存活窗口集里的条目(防 CGWindowID 复用继承旧时间戳),返回清理数。
/// Prune MRU entries not in the live window set (prevents CGWindowID-reuse inheriting a dead
/// timestamp); returns how many were dropped.
fn prune_mru(mru: &mut MruMap, live_set: &HashSet<(i32, u32)>) -> usize {
    let before = mru.len();
    mru.retain(|k, _| live_set.contains(k));
    before - mru.len()
}

/// 清除指定进程的全部窗口 MRU；由 App 退出通知调用，立即切断 PID/CGWindowID 复用污染。
/// Remove every window MRU for a process; termination calls this to immediately prevent
/// PID/CGWindowID reuse from inheriting stale timestamps.
pub fn remove_pid_mru(mru: &mut MruMap, pid: i32) -> usize {
    let before = mru.len();
    mru.retain(|(entry_pid, _), _| *entry_pid != pid);
    before - mru.len()
}

/// 新窗口只初始化一次：优先使用 App 最近激活时间，否则退回 ancient；顺序偏移只用于
/// 同批首次发现窗口的稳定排序。`or_insert` 是纯窗口 MRU 不搭便车的关键。
/// Initialize a new window once: prefer the app's latest activation time, otherwise use
/// ancient; the order offset only stabilizes windows first seen in the same batch. `or_insert`
/// is what prevents existing sibling windows from riding an app activation.
fn initialize_window_mru(
    mru: &mut MruMap,
    pid: i32,
    cgwid: u32,
    app_activated_at: Option<Instant>,
    ancient_base: Instant,
    insertion_order: u32,
) {
    let base = app_activated_at.unwrap_or(ancient_base);
    let ordered_ts = base
        .checked_sub(Duration::from_millis(insertion_order as u64))
        .unwrap_or(base);
    mru.entry((pid, cgwid)).or_insert(ordered_ts);
}

/// AX 聚焦查询失败时只能在系统报告的前台 App 内回退，不能误提升全局 CG 列表首项。
/// If the AX focused query fails, fallback is restricted to the system-reported frontmost
/// app rather than accidentally bumping the first item in the global CG list.
fn frontmost_fallback(windows: &[WindowInfo], front_pid: Option<i32>) -> Option<(i32, u32)> {
    let pid = front_pid?;
    windows
        .iter()
        .find(|w| w.pid == pid && !w.minimized)
        .map(|w| (w.pid, w.window_id))
}

/// 启动时按窗口前→后顺序预种 MRU(同应用窗口分组,应用级顺序 = CG 前→后序)。
///
/// **重要(load-bearing):这个顺序只是一个启动占位,未必等于原生 Cmd+Tab 的顺序。**
/// macOS 没有公开 API 返回应用切换器的 App MRU,而 CG 的 z 序记录的是"窗口创建/点击
/// 抬升"的历史,与应用激活顺序是两回事——实测应用激活(包括 Cmd+Tab/Dock 切换)不会
/// 重排 z 序(Ghostty/Edge 激活后窗口仍在 z 序深处)。因此这里排出的顺序只是"我们
/// 随便定的一个初始顺序",保证:① 同 App 窗口聚在一起;② 顺序稳定可复现;③ 全部
/// 排在 999s 回退值之前。真正精确的顺序在应用运行起来后由实时激活通知(MRU bump)
/// 逐步修正。如果以后要精确恢复重启前的顺序,需要把 MRU 持久化到磁盘(见
/// clipboard-history 的持久化模式),而不是依赖这个种子。
///
/// 该方法在启动时调用一次(见 AppState::new),返回带种子的 MruMap。
/// Seed the MRU from the front-to-back window order at startup (same-app windows grouped,
/// app-level order = the CG front-to-back order).
///
/// **Important (load-bearing): this order is only a startup placeholder and is NOT
/// guaranteed to match the native Cmd+Tab order.** macOS exposes no public API for the
/// switcher's app MRU, and the CG z-order reflects "window created / clicked-raised"
/// history rather than app-activation order -- verified: activating an app (Cmd+Tab / Dock)
/// does NOT reorder the z-order (Ghostty/Edge stayed deep in the z-order after activation).
/// So this is just a plausible initial order we impose, ensuring: 1) same-app windows are
/// grouped; 2) the order is stable and reproducible; 3) everything sorts ahead of the 999s
/// fallback. The precise order is corrected live by the activation notifications (MRU bumps)
/// once the app is running. To exactly restore the pre-restart order, the MRU would need to
/// be persisted to disk (see the clipboard-history persistence pattern), not seeded.
pub fn seed_mru_from_system_order() -> MruMap {
    unsafe {
        let array = CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0);
        if array.is_null() {
            return MruMap::new();
        }
        let count = CFArrayGetCount(array);
        let now = Instant::now();
        // App 首次出现顺序(前到后)+ 每个 App 内的窗口 CG 顺序。
        // **只用可见(on-screen)窗口推导**:不可见/最小化/其他 Space 的窗口会穿插在
        // z 序里,把应用相对顺序搅乱(实测 Clash Verge 因离屏窗口靠前而虚高)。
        // App first-appearance order (front-to-back) + each app's windows in CG order.
        // ONLY on-screen windows rank: invisible/minimized/other-Space windows interleave
        // in the z-order and skew the app ranking (Clash Verge used to rank high thanks
        // to its off-screen windows).
        let mut app_order: Vec<i32> = Vec::new();
        let mut app_windows: HashMap<i32, Vec<u32>> = HashMap::new();
        let mut app_names: HashMap<i32, String> = HashMap::new();
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(array, i);
            if dict.is_null() {
                continue;
            }
            let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
            if layer != 0 {
                continue;
            }
            let onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false);
            if !onscreen {
                continue;
            }
            let pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
            if pid <= 0 {
                continue;
            }
            let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
            if owner_name.is_empty() || owner_name == "Dock" {
                continue;
            }
            if !app_order.contains(&pid) {
                app_order.push(pid);
                app_names.insert(pid, owner_name);
            }
            let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
            let list = app_windows.entry(pid).or_default();
            if cgwid != 0 && !list.contains(&cgwid) {
                list.push(cgwid);
            }
        }
        // 打印启动时的占位顺序(前→后,应用分组)。注意这只是近似占位,不是原生
        // Cmd+Tab 的应用序(激活不重排 z 序,见函数注释)——仅用于核对与排查。
        // Print the startup PLACEHOLDER order (front-to-back, app-grouped). This is only an
        // approximation, NOT the native Cmd+Tab order (activation does not reorder the
        // z-order; see the fn doc) -- printed for cross-checking and debugging.
        log_debug!("[seed] startup placeholder window order (front-to-back, on-screen only):");
        for pid in &app_order {
            let wins = app_windows.get(pid).map(|v| v.as_slice()).unwrap_or(&[]);
            let names: Vec<String> = wins.iter().map(|w| w.to_string()).collect();
            log_debug!(
                "  pid={} app=\"{}\" windows=[{}]",
                pid,
                app_names.get(pid).map(|s| s.as_str()).unwrap_or("?"),
                names.join(", ")
            );
        }
        seed_timestamps(&app_order, &app_windows, now)
    }
}

/// 纯逻辑:按显示顺序(应用分组 + 应用内 CG 序)给每个窗口赋一个单调递增的"年龄",
/// 让 sort_windows_by_mru(按年龄升序)排出该顺序;全部 < 1s,仍排在 999s 回退前。
/// 纯函数,单测覆盖排序结果。`now` 由调用方给定,测试注入固定时刻。
/// Pure logic: assign each window a monotonically increasing "age" in display order
/// (app-grouped + per-app CG order), so sort_windows_by_mru (ascending age) emits that
/// order; all < 1s, still ahead of the 999s fallback. Pure -- `now` is injected by tests.
fn seed_timestamps(
    app_order: &[i32],
    app_windows: &HashMap<i32, Vec<u32>>,
    now: Instant,
) -> MruMap {
    let mut mru = MruMap::new();
    let mut step: u64 = 0;
    for pid in app_order {
        if let Some(wins) = app_windows.get(pid) {
            for &cgwid in wins {
                let t = now - std::time::Duration::from_millis(step);
                mru.insert((*pid, cgwid), t);
                step += 1;
            }
        }
    }
    mru
}

fn icon_cache_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    // 测试构建使用与真实缓存同级的专用目录:冒烟测试里的 clear_icon_cache()
    // 只清测试目录,绝不触碰用户的真实图标缓存(曾清空真实缓存导致下次
    // summon 全部重提取,卡 ~400ms)。
    // Test builds use a dedicated sibling directory: the smoke tests' clear_icon_cache()
    // only clears the test dir, never the user's real icon cache (clearing the real one
    // used to force a full re-extract on the next summon, stalling ~400ms).
    let name = if cfg!(test) {
        "oh-my-tab-icons-test"
    } else {
        "oh-my-tab-icons"
    };
    format!("{}/Library/Caches/{}", home, name)
}

// 枚举常量:All(0)= 含离屏窗口(orderOut 对话框/最小化/其他 Space),收集恒用它——
// 离屏窗口是否显示由 AX 语义决定(见 collect_windows 的过滤注释)。
// Enumeration constants: All (0) includes off-screen windows (orderOut'd dialogs /
// minimized / other Spaces); collection always uses it -- whether an off-screen window
// shows is decided by AX semantics (see collect_windows' filter comments).
const K_C_G_WINDOW_LIST_OPTION_ALL: u32 = 0;

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
/// 缩略图模块的 AXObserver 回调也用它解析新创建窗口的 cgwid。
/// Get a CGWindowID for an AX window (private API _AXUIElementGetWindow).
/// Used to pair AX windows with CG windows by CGWindowID instead of guessing
/// by order/title (apps like Edge with no CG window name used to mismatch,
/// corrupting mru/raise targeting). The thumbnail module's AXObserver callback
/// also uses it to resolve newly created windows' cgwids.
pub(crate) unsafe fn ax_window_cgwid(element: AXUIElementRef) -> Option<u32> {
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
        log_info!(
            "make_key_window: SLPSPostEventRecordTo failed (down={} up={})",
            ok1,
            ok2
        );
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
pub(crate) struct AppIdentity {
    pub(crate) key: String,
    /// 可执行文件 mtime(自 UNIX epoch 的秒数)。None 表示无法校验,退化为「文件存在即有效」。
    /// Executable mtime (seconds since UNIX epoch). None means unverified -> "file exists = valid".
    pub(crate) fingerprint: Option<String>,
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
/// 剪贴板记录来源时也复用此身份(与切换器同一套键/回退)。
///
/// Resolve a PID's cache identity. Key priority: bundleIdentifier (reverse-DNS,
/// filename-safe) > hashed executable path > `pid_{pid}` fallback. Fingerprint is the
/// executable mtime; an app update gets a new mtime -> forces re-extract. The clipboard
/// reuses this identity when recording a source (the same key/fallback chain as the switcher).
pub(crate) unsafe fn resolve_app_identity(pid: i32) -> AppIdentity {
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

/// 缓存 PNG 路径(支持后缀:"" = 切换器大图,".small" = 剪贴板小图)。
/// The cached PNG path (suffix-aware: "" = the switcher's big icon, ".small" = the
/// clipboard's small one).
fn cache_path_for_key_suffix(key: &str, suffix: &str) -> String {
    format!("{}/{}{}.png", icon_cache_dir(), key, suffix)
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
    check_cache_for_suffix(id, "")
}

/// 同上,支持小图后缀(.small);大小图共享同一份 .meta 指纹。
/// Same as above, suffix-aware for the small icon; both sizes share one .meta fingerprint.
fn check_cache_for_suffix(id: &AppIdentity, suffix: &str) -> Option<String> {
    let png = cache_path_for_key_suffix(&id.key, suffix);
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

fn write_png_to_cache(png: *mut AnyObject, key: &str, suffix: &str) -> Option<String> {
    unsafe {
        let path = cache_path_for_key_suffix(key, suffix);
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

/// 提取图标到缓存(按目标 pt 尺寸渲染):切换器大图(128pt)与剪贴板小图(16pt)共用管线。
/// `suffix`: 文件名后缀("" = {key}.png,".small" = {key}.small.png),大小图共享同一份
/// {key}.meta 指纹(同一可执行文件 mtime)。
/// Extract an app icon into the cache at the target point size: the switcher's big icon
/// (128pt) and the clipboard's small one (16pt) share this pipeline. `suffix`: the filename
/// suffix ("" -> {key}.png, ".small" -> {key}.small.png); both sizes share one {key}.meta
/// fingerprint (the same executable mtime).
fn extract_icon_to_cache_sized(pid: i32, pt_size: f64, suffix: &str) -> Option<String> {
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
        if let Some(path) = check_cache_for_suffix(&id, suffix) {
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

        // Render at Retina resolution: pt_size pt display → 2x (or 1x) pixels.
        let scale: f64 = {
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() {
                2.0
            } else {
                msg_send![screen, backingScaleFactor]
            }
        };
        let px = pt_size * scale;

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

        let result = write_png_to_cache(png, &id.key, suffix);
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

pub fn extract_icon_to_cache(pid: i32) -> Option<String> {
    extract_icon_to_cache_sized(pid, 128.0, "")
}

/// 剪贴板标题栏的小图标(16pt,2x = 32px)。记录来源时调用(app 此刻必存活);
/// 键与 .meta 指纹和切换器大图共用,`{key}.small.png` 独立于大图文件。
/// The clipboard header's small icon (16pt, 32px @2x). Called when the source is recorded
/// (the app is guaranteed alive then); the key and .meta fingerprint are shared with the
/// switcher's big icon, while `{key}.small.png` is a separate file.
pub fn extract_small_icon(pid: i32) -> Option<String> {
    extract_icon_to_cache_sized(pid, 16.0, ".small")
}

/// 剪贴板小图的路径(存在性检查用;key = resolve_app_identity 的缓存键)。
/// The clipboard small-icon path (for existence checks; key = resolve_app_identity's key).
pub fn small_icon_path_for_key(key: &str) -> String {
    cache_path_for_key_suffix(key, ".small")
}

/// AX 关闭窗口(等效点击窗口的关闭按钮):遍历 pid 的 AXWindows 匹配 cgwid,
/// 主方案 = 取窗口的 AXCloseButton 元素并执行 AXPress;兜底 = 窗口级 AXClose
/// action(macOS 26 上多数 app 不再暴露,故以按钮方案为主)。最小化窗口同样适用。
/// 返回是否找到并成功发起关闭;失败 → false。
/// Close a window via AX (equivalent to clicking its close button): scan the pid's
/// AXWindows for cgwid; the PRIMARY path grabs the window's AXCloseButton and presses it;
/// the fallback is the window-level AXClose action (most apps no longer expose it on
/// macOS 26, hence the button-first approach). Works for minimized windows too. Returns
/// whether the close was initiated.
pub(crate) fn close_ax_window(pid: i32, cgwid: u32) -> bool {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return false;
        }
        AXUIElementSetMessagingTimeout(app, 0.5);
        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || windows_array.is_null() {
            return false;
        }
        let count = CFArrayGetCount(windows_array);
        let close_btn_key = cf_string_new("AXCloseButton");
        let press_key = cf_string_new("AXPress");
        let close_key = cf_string_new("AXClose");
        let mut ok = false;
        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() {
                continue;
            }
            if ax_window_cgwid(element) != Some(cgwid) {
                continue;
            }
            // 主方案:关闭按钮 + AXPress(标准窗口通用,系统设置/Chrome 实测可用)。
            // Primary: the close button + AXPress (works on any standard window; verified
            // on System Settings and Chrome).
            let mut close_btn: *const c_void = std::ptr::null();
            if AXUIElementCopyAttributeValue(element, close_btn_key, &mut close_btn) == K_AX_SUCCESS
                && !close_btn.is_null()
            {
                ok = AXUIElementPerformAction(close_btn, press_key) == K_AX_SUCCESS;
                CFRelease(close_btn);
                break;
            }
            // 兜底:窗口级 AXClose。
            // Fallback: the window-level AXClose action.
            ok = AXUIElementPerformAction(element, close_key) == K_AX_SUCCESS;
            break;
        }
        CFRelease(close_btn_key);
        CFRelease(press_key);
        CFRelease(close_key);
        CFRelease(windows_array);
        ok
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
/// AX 窗口角色白名单:标准窗口(AXStandardWindow)任意;对话框(AXDialog——
/// JetBrains 系 IDE 的主窗口角色)必须有非空标题;其余角色(弹窗/面板/隐形窗口)
/// 一律过滤;无 subrole 视为标准窗口(部分 App 不设置此属性)。空标题的 AXDialog
/// 仍被过滤——BetterDisplay 隐形窗口防线的一部分(隐形窗口无标题,无论 subrole
/// 是什么都不该进入列表)。纯函数,单测覆盖。
/// AX-window subrole keep-rule: standard windows always pass; AXDialog (the subrole
/// JetBrains IDEs use for their MAIN windows) only when titled; everything else
/// (popups/panels/invisible windows) is filtered; a missing subrole counts as standard
/// (some apps don't set it). Untitled AXDialog stays filtered -- part of the
/// BetterDisplay invisible-window defense (invisible windows are untitled, so whatever
/// their subrole, they must not enter the list). Pure function, unit-tested.
fn ax_subrole_kept(subrole: Option<&str>, titled: bool) -> bool {
    match subrole {
        Some("AXStandardWindow") => true,
        Some("AXDialog") => titled,
        Some(_) => false,
        // 无 subrole → 视为标准窗口(部分 App 不设置此属性)。
        // A missing subrole counts as standard (some apps don't set it).
        None => true,
    }
}

/// 查某 PID 的全部标准 AX 窗口:(cgwid, 标题, 是否最小化)。collect_windows 与
/// 缩略图模块的启动预生成共用。
/// All standard AX windows for a PID: (cgwid, title, minimized). Shared between
/// collect_windows and the thumbnail module's startup pre-generation.
pub(crate) fn get_ax_windows_for_pid(pid: i32) -> Option<Vec<(u32, String, bool)>> {
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

            // 只保留标准窗口(AXStandardWindow)+ 有标题的对话框(AXDialog),过滤弹出
            // 面板/下拉菜单等非标准窗口。AXDialog 是 JetBrains 系 IDE 主窗口的角色
            // (WebStorm 实测),必须放行;空标题的 AXDialog 仍按弹出面板过滤(见
            // ax_subrole_kept 注释:BetterDisplay 隐形窗口防线)。
            // Keep AXStandardWindow + TITLED AXDialog; filter popups/panels/dropdowns.
            // AXDialog is the subrole JetBrains IDEs use for their main windows (measured
            // on WebStorm) and must pass; an UNTITLED AXDialog stays filtered (see
            // ax_subrole_kept: the BetterDisplay invisible-window defense).
            let mut subrole_value: *const c_void = std::ptr::null();
            let kept = if AXUIElementCopyAttributeValue(element, subrole_key, &mut subrole_value)
                == K_AX_SUCCESS
                && !subrole_value.is_null()
            {
                let s = cf_to_rust_string(subrole_value);
                CFRelease(subrole_value);
                // AXDialog 需额外判断标题(见 ax_subrole_kept:JetBrains 主窗口有标题,
                // 空标题对话框按弹出面板过滤)。
                // AXDialog needs the extra title check (see ax_subrole_kept: JetBrains
                // main windows are titled; untitled dialogs stay filtered as popups).
                let titled = if s.as_deref() == Some("AXDialog") {
                    let mut title_value: *const c_void = std::ptr::null();
                    if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                        == K_AX_SUCCESS
                        && !title_value.is_null()
                    {
                        let t = cf_to_rust_string(title_value);
                        CFRelease(title_value);
                        t.is_some_and(|t| !t.is_empty())
                    } else {
                        false
                    }
                } else {
                    false
                };
                ax_subrole_kept(s.as_deref(), titled)
            } else {
                // 无 subrole → 视为标准窗口(部分 App 不设置此属性)。
                // No subrole means standard window for apps that don't set it.
                true
            };
            if !kept {
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

// ========== per-PID AX 收集(可并行)/ per-PID AX collection (parallelizable) ==========

/// 一个工作线程处理一段 PID 后的部分结果集,线程间按 key 合并——合并后与串行版
/// 逐字段一致(卡片顺序由第二遍 CG 数组遍历决定,与收集顺序无关)。
/// One worker thread's partial result set for its PID chunk; merged by key afterwards --
/// the merge equals the serial version field-for-field (card order is decided by the
/// second pass over the CG array and never depends on collection order).
struct AxPartial {
    icon_ids: HashMap<i32, AppIdentity>,
    ax_queried_pids: HashSet<i32>,
    ax_wid_to_info: HashMap<i32, HashMap<u32, (String, bool)>>,
    titleless_pids: HashSet<i32>,
    /// 本段所有 PID 的 AX 查询工作耗时之和(诊断用;墙钟由调用方测)。
    /// Sum of AX query work time for this chunk (diagnostics; wall clock is measured by the caller).
    ax_work_ms: u128,
}

/// 处理一段 PID:逐个解析应用身份 + 查询该应用的 AX 窗口列表。AX 远程查询支持
/// 多线程(messaging timeout 按 element 隔离);resolve_app_identity 只读
/// NSRunningApplication 属性 + stat。整段包 autoreleasepool 回收 ObjC 临时对象。
/// Process a chunk of PIDs: resolve each app identity + query its AX window list. AX
/// remote messaging works from any thread (messaging timeouts are per-element);
/// resolve_app_identity only reads NSRunningApplication properties + stat. The whole
/// chunk runs inside an autoreleasepool to drain ObjC temporaries.
///
/// # Safety
/// 调用方需保证无并发的 AX/ObjC 环境冲突(与主线程的 AX 使用互不共享元素)。
/// The caller must ensure no conflicting concurrent use of shared AX/ObjC elements with
/// the main thread.
unsafe fn ax_collect_chunk(chunk: &[i32], pid_names: &HashMap<i32, String>) -> AxPartial {
    let mut partial = AxPartial {
        icon_ids: HashMap::new(),
        ax_queried_pids: HashSet::new(),
        ax_wid_to_info: HashMap::new(),
        titleless_pids: HashSet::new(),
        ax_work_ms: 0,
    };
    // AppKit 临时对象(NSRunningApplication 等)随池回收;图标提取后台线程同款先例。
    // AppKit temporaries (NSRunningApplication et al) drain with the pool; same precedent
    // as the background icon-extraction thread.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    for &pid in chunk {
        let t_pid = Instant::now();
        partial
            .icon_ids
            .insert(pid, unsafe { resolve_app_identity(pid) });
        let ax_wins = get_ax_windows_for_pid(pid);
        let pid_ms = t_pid.elapsed().as_millis();
        partial.ax_work_ms += pid_ms;
        // AX 查询结果汇总:定位“一会 1 个一会 2 个”——设置窗口抖动时看 windows 数
        // 是否变化(缺窗口/失败/超时)。多线程下日志行可能交错,但每行自带 pid/app
        // 标识,logger 的 flume 通道保证行内原子输出。
        // AX query summary: pin down the 1-vs-2 flicker -- watch whether the window count
        // changes (missing windows / failure / timeout). Lines from parallel workers may
        // interleave, but each carries its own pid/app tag, and the logger's flume channel
        // keeps every line atomic.
        match &ax_wins {
            Some(w) => log_debug!(
                "[collect] ax pid={} app=\"{}\" windows={} ({}ms)",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                w.len(),
                pid_ms
            ),
            None => log_debug!(
                "[collect] ax pid={} app=\"{}\" FAILED (CG fallback, {}ms)",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                pid_ms
            ),
        }
        // TIMING-DEBUG 慢 AX 查询(≥20ms)单独标记:定位卡顿来自哪个应用(如 Ghostty)。
        // TIMING-DEBUG Flag slow AX queries (>=20ms) individually: pin down which app stalls
        // the summon.
        if pid_ms >= 20 {
            log_debug!(
                "[collect] ax slow: pid={} app=\"{}\" {}ms",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                pid_ms
            );
        }
        match ax_wins {
            Some(wins) if !wins.is_empty() => {
                partial.ax_queried_pids.insert(pid);
                if wins.iter().all(|(_, t, _)| t.is_empty()) {
                    partial.titleless_pids.insert(pid);
                }
                let mut wid_map: HashMap<u32, (String, bool)> = HashMap::new();
                for (cgwid, title, minimized) in &wins {
                    if *cgwid != 0 {
                        wid_map.insert(*cgwid, (title.clone(), *minimized));
                    }
                }
                partial.ax_wid_to_info.insert(pid, wid_map);
            }
            // AX 查询成功但无标准窗口:该 App 没有调度中心可见的窗口,后续直接跳过。
            // AX query succeeded but no standard windows: the app has no Mission-Control-visible
            // windows; skip all its CG windows in the second pass.
            Some(_) => {
                partial.ax_queried_pids.insert(pid);
            }
            // AX 查询失败(无 AX 数据):保留 CG 回退路径。
            // AX query failed (no AX data): keep the CG fallback path.
            None => {}
        }
    }
    let _: () = msg_send![pool, drain];
    partial
}

pub fn collect_windows(mru: &mut MruMap) -> Vec<WindowInfo> {
    collect_windows_with_frontmost_bump(mru, true)
}

/// 收集窗口快照,可选择是否把当前前台窗口写入 MRU。
/// 生命周期事件触发的刷新只负责更新窗口集合,不能把自身误当成 summon。
/// Collect a window snapshot, optionally recording the current frontmost window in MRU.
/// Lifecycle-triggered refreshes only update the window set and must not act like a summon.
pub(crate) fn collect_windows_with_frontmost_bump(
    mru: &mut MruMap,
    bump_frontmost: bool,
) -> Vec<WindowInfo> {
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    // 始终用 All 枚举(含离屏窗口)。原因:部分应用(如 JetBrains 系 IDE)在"主窗口被
    // 激活"时会把设置对话框 orderOut(隐藏但保留窗口对象)——它 isOnscreen=false,
    // OnScreenOnly 枚举不到,切换器就会"切到主窗口后设置窗口消失"(BetterCmdTab 用
    // All 枚举,无此问题)。离屏窗口是否显示改由 AX 语义决定(见下文的过滤逻辑):
    // AX 仍报的窗口是合法可切换窗口(调度中心/系统 Cmd+Tab 也认),AX 不报的
    // 隐藏辅助窗口会被 subrole/空标题过滤拦掉。
    // Always enumerate with All (off-screen windows included). Some apps (JetBrains IDEs)
    // orderOut their settings dialog when the main window is activated -- it then has
    // isOnscreen=false, invisible to OnScreenOnly, so the switcher would lose it after
    // switching to the main window (BetterCmdTab uses All; no such issue). Whether an
    // off-screen window shows is now decided by AX semantics (see the filter below): a
    // window AX still reports is a legitimate switchable window (Mission Control and the
    // system Cmd+Tab agree); hidden helper surfaces AX never reports are dropped by the
    // subrole/empty-title filters.
    let cg_option = K_C_G_WINDOW_LIST_OPTION_ALL;
    let array = unsafe { CGWindowListCopyWindowInfo(cg_option, 0) };
    if array.is_null() {
        return vec![];
    }

    // 不再按 PID 排除本应用(own-PID)窗口:设置窗口也是 own-PID,排除它会导致设置
    // 开着时切不到它。浮窗自己不需要靠 PID 排除--它 setLevel:3(floating,
    // kCGWindowLayer != 0)已被下面的 layer 过滤挡掉(与枚举模式无关)。设置窗口
    // 关着时 orderOut 离屏,由下文的 own-PID isOnscreen 过滤排除,故
    // "开->显示为卡片、关->不显示"仍然成立。
    //
    // Own-PID windows are no longer excluded by PID: the settings window is own-PID too, and
    // excluding it would make it unswitchable while open. The overlay itself needs no PID
    // exclusion -- it's setLevel:3 (floating, kCGWindowLayer != 0), already dropped by the
    // layer check below (independent of the enumeration mode). The settings window, when
    // closed, is orderOut'd (off-screen) and excluded by the own-PID isOnscreen filter
    // below, so "open -> shown as a card, closed -> hidden" still holds.
    let mut windows: Vec<WindowInfo> = Vec::new();
    // CG 循环里已显示的窗口集合:AX 补漏时跳过(避免重复卡片)。
    // Windows already shown by the CG loop: skipped by the AX backfill (no duplicate rows).
    let mut shown: HashSet<(i32, u32)> = HashSet::new();
    // TIMING-DEBUG 阶段计时(debug 档):定位 summon 卡顿——CG 枚举 / 每 PID AX 查询 / frontmost。
    // TIMING-DEBUG Phase timings (debug tier): locate summon stalls -- CG enumeration /
    // per-PID AX queries / the frontmost lookup. Remove together with the [collect] logs.
    let t0 = Instant::now();
    let count = unsafe { CFArrayGetCount(array) };
    let now = Instant::now();
    let ancient_base = now.checked_sub(Duration::from_secs(86_400)).unwrap_or(now);
    // 一次收集使用同一激活快照，避免同批窗口因通知并发到达而得到两套时间基准。
    // One collection uses one activation snapshot so concurrent notifications cannot give
    // windows in the same batch two different time bases.
    let last_activated = LAST_ACTIVATED.lock().unwrap().clone();
    let t_cg_ms = t0.elapsed().as_millis();
    let mut insertion_order: u32 = 0;

    // 第一遍遍历：收集所有 PID，用于批量查询 AX 窗口
    // First pass: collect all PIDs to batch query AX windows
    let mut pids: HashSet<i32> = HashSet::new();
    // TIMING-DEBUG pid -> 应用名(慢 AX 日志用)/ pid -> app name (for the slow-AX log).
    let mut pid_names: HashMap<i32, String> = HashMap::new();
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
        pid_names.insert(owner_pid, owner_name);
        pids.insert(owner_pid);
    }

    // 以 AX 窗口列表为主数据源（macOS App Switcher 的做法）
    // Use AX window list as primary source (same as macOS App Switcher)
    // pid -> 缓存身份(bundle id + mtime)。AX 循环里按 pid 解析一次,供第二遍按窗口查缓存,
    // 避免每个窗口都做一次 NSRunningApplication 查找(一个 App 多窗口时尤其浪费)。
    // pid -> cache identity (bundle id + mtime). Resolved once per pid in the AX phase so the
    // second pass can look up the cache per-window without an NSRunningApplication call each time
    // (wasteful when one app has many windows).
    let mut icon_ids: HashMap<i32, AppIdentity> = HashMap::new();
    // AX 收集阶段(并行):把 PID 分成 K 组(K = min(逻辑核数, PID 数),运行时自适应
    // 任意 M 系列芯片),各组在工作线程同时查询——AX 远程消息支持多线程,每组结果
    // 按 key 合并后与串行版逐字段一致。墙钟时间从"所有 PID 之和"降为"最慢单 PID"
    // (微信这类对 AX 提问反复磨蹭的应用曾是串行总耗时的主导项,实测单 PID 可达 1.5s)。
    // The AX collection phase (parallel): split PIDs into K chunks (K = min(logical cores,
    // PID count), runtime-adaptive to any Apple Silicon variant) queried simultaneously on
    // worker threads -- AX remote messaging is multi-thread-capable and each chunk merges
    // by key into a result identical to the serial version. Wall clock drops from "sum of
    // all PIDs" to "slowest single PID" (apps like WeChat that stall on every AX question
    // used to dominate serial totals; up to 1.5s for one PID in logs).
    let mut ax_wid_to_info: HashMap<i32, HashMap<u32, (String, bool)>> = HashMap::new();
    // AX 查询「成功」的 pid 集合:成功但无标准窗口的 App 应整体跳过(调度中心不显示它),
    // 只有查询失败(None)才允许走 CG 回退。这是 BetterDisplay 隐形窗口 bug 的根因修复:
    // AX 成功但 subrole 过滤后为空,不能等同于「无 AX 数据」。
    // Pids whose AX query SUCCEEDED: an app with a successful query but no standard windows
    // must be skipped entirely (Mission Control doesn't show it); only a failed query (None)
    // allows the CG fallback. This fixes the BetterDisplay invisible-window bug: an AX query
    // that succeeds but yields no standard windows must not be treated as "no AX data".
    let mut ax_queried_pids: HashSet<i32> = HashSet::new();
    // AX 窗口「全部」为空标题的 App（如 Microsoft To Do：自绘标题栏 -> AXTitle 为空）。
    // 这类 App 的空标题窗口是真实主窗口，不能被当作弹出面板丢弃。
    // Apps whose AX windows are ALL untitled (e.g. Microsoft To Do, which has a
    // custom title bar and an empty AXTitle). Their titleless windows are real
    // main windows and must not be dropped as popups.
    let mut titleless_pids: HashSet<i32> = HashSet::new();
    let t_ax = Instant::now();
    {
        let pid_list: Vec<i32> = pids.iter().copied().collect();
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(pid_list.len())
            .max(1);
        let chunk_size = pid_list.len().div_ceil(workers);
        let partials: Vec<AxPartial> = std::thread::scope(|scope| {
            let handles: Vec<_> = pid_list
                .chunks(chunk_size)
                .map(|chunk| scope.spawn(|| unsafe { ax_collect_chunk(chunk, &pid_names) }))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("ax collect worker panicked"))
                .collect()
        });
        // 按 key 合并各分段;ax_wid_to_info 的键互不相交(每 PID 只属于一段),
        // 合并顺序不影响最终内容。
        // Merge chunks by key; ax_wid_to_info keys are disjoint (each PID lives in exactly
        // one chunk), so merge order cannot affect the outcome.
        for p in partials {
            icon_ids.extend(p.icon_ids);
            ax_queried_pids.extend(p.ax_queried_pids);
            ax_wid_to_info.extend(p.ax_wid_to_info);
            titleless_pids.extend(p.titleless_pids);
        }
    }
    // TIMING-DEBUG AX 阶段墙钟(并行后 ≠ 各 PID 工作耗时之和;单 PID 工作耗时见
    // 各 "ax pid=" 行的 ms 值)。
    // TIMING-DEBUG Wall clock of the parallel AX phase (NOT the sum of per-PID work
    // times anymore; per-PID work shows in each "ax pid=" line's ms value).
    let ax_total_ms = t_ax.elapsed().as_millis();

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
                None => {
                    // 配对失败:CG 窗口在 AX 列表里没有(弹出面板 或 AX 漏报)。
                    // 记日志定位设置窗口“时有时无”:抖动时这里应出现它的 cgwid。
                    // Pair miss: the CG window has no AX counterpart (a popup, or AX
                    // failed to report it). Logged to pin the Settings-window flicker.
                    log_debug!(
                        "[collect] ax pair-miss: pid={} app=\"{}\" cgwid={} cg_title=\"{}\" {:.0}x{:.0} -> dropped",
                        owner_pid,
                        owner_name,
                        cgwid,
                        cg_title,
                        bounds.2,
                        bounds.3
                    );
                    continue; // CG 窗口在 AX 里没有 -> 弹出面板，跳过 / popup, skip
                }
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

        // 枚举已改为 All(见 cg_option 注释),离屏窗口(orderOut 的对话框/最小化/其他
        // Space)全部在列表里,显示与否由以下规则决定:
        // - 本应用自己的窗口(设置窗口):orderOut = 关闭,按 isOnscreen 过滤——
        //   开->显示、关->不显示,维持原行为(其他应用的 orderOut 窗口不能用此规则,
        //   它们可能是 JetBrains 那种"隐藏但合法"的设置对话框)。
        // - 其他应用的窗口:AX 配对成功的都显示——orderOut 但 AX 仍报的窗口
        //   (JetBrains 激活主窗口时隐藏的设置对话框)是合法可切换窗口必须显示;
        //   最小化窗口由"显示最小化窗口"开关控制(AX minimized 标记,与 isOnscreen
        //   无关——最小化窗口 isOnscreen=false 但 minimized=true)。
        // The enumeration is now All (see the cg_option comment): off-screen windows
        // (orderOut'd dialogs / minimized / other Spaces) are all listed, and whether they
        // show is decided here:
        // - own windows (the settings window): orderOut = closed, filtered by isOnscreen --
        //   open -> shown, closed -> hidden, preserving the original behavior (other apps'
        //   orderOut'd windows can't use this rule; they may be JetBrains-style hidden-but-
        //   legitimate settings dialogs).
        // - other apps' windows: every AX-paired window shows -- an orderOut'd window AX
        //   still reports (JetBrains hiding its settings dialog on main-window activation)
        //   is a legitimate switchable window and must show; minimized windows are gated
        //   by the "show minimized windows" switch (the AX minimized flag -- unrelated to
        //   isOnscreen: a minimized window is isOnscreen=false but minimized=true).
        if owner_pid == std::process::id() as i32 {
            let is_onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false);
            if !is_onscreen {
                continue;
            }
        } else if !show_minimized && minimized {
            continue;
        }

        // 空标题窗口仅对「AX 确认过全部窗口无标题」的 App(titleless_pids)保留;
        // AX 查询失败回退的 App 不再享受空标题豁免(无法确认其窗口身份)。
        // Titleless windows are kept only for apps AX confirmed as all-untitled
        // (titleless_pids); the AX-failed fallback no longer exempts empty titles
        // (window identity can't be verified there).
        if window_title.is_empty() && !titleless_pids.contains(&owner_pid) {
            continue;
        }

        // mru 按 (pid, CGWindowID) 索引——CGWindowID 在窗口生命周期内稳定不变，
        // 比 title 更可靠（title 会随浏览器标签页切换而变）。新窗口优先以 App 最近
        // 激活时间初始化；已有条目绝不覆盖，因此旧同 App 窗口不会搭便车。
        // Key mru by (pid, CGWindowID) — CGWindowID is stable for the window's
        // lifetime, more reliable than title (which changes with browser tabs). New windows
        // prefer the app's latest activation as their seed; existing entries are never
        // overwritten, so old same-app windows cannot ride along.
        initialize_window_mru(
            mru,
            owner_pid,
            cgwid,
            last_activated.get(&owner_pid).copied(),
            ancient_base,
            insertion_order,
        );
        insertion_order += 1;
        let icon_path = icon_ids.get(&owner_pid).and_then(check_cache_for_identity);
        // TIMING-DEBUG 图标缓存 miss 标记:排查 summon 卡顿——哪些 app 会触发同步提取。
        // TIMING-DEBUG Flag icon-cache misses: which apps trigger the synchronous extract.
        if icon_path.is_none() {
            log_debug!(
                "[collect] icon miss: pid={} app=\"{}\"",
                owner_pid,
                owner_name
            );
        }
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
        shown.insert((owner_pid, cgwid));
    }

    // AX 补漏:AX 窗口列表报、但 CG 枚举没有的窗口。例:JetBrains 系 IDE 在“主窗口
    // 被激活”时把设置对话框 orderOut(隐藏但保留窗口对象)——orderOut 的窗口不在
    // CGWindowList 里(optionAll 也只含屏上窗口),按 CG 遍历永远看不到它;但 AX 仍
    // 报它,且它是合法可切换窗口(BetterCmdTab 以 AX 列表为主数据源,故稳定显示)。
    // 这里用 AX 的标题/minimized 补出条目;bounds 未知(离屏),调用方回退主屏幕。
    // AX backfill: windows AX reports but the CG enumeration lacks. E.g. JetBrains IDEs
    // orderOut their settings dialog when the main window is activated -- an orderOut'd
    // window is NOT in CGWindowList (optionAll only covers on-screen windows), so a
    // CG-driven loop can never see it; AX still reports it and it is a legitimate
    // switchable window (BetterCmdTab uses the AX list as its primary source and shows
    // it stably). Entries are built from AX title/minimized; bounds are unknown
    // (off-screen), callers fall back to the main screen.
    for (&pid, wid_map) in ax_wid_to_info.iter() {
        // 本应用窗口(设置窗口)走 CG 路径的 isOnscreen 过滤,这里不补:
        // 关闭(orderOut)时不应出现在切换器里。
        // Own windows (the settings window) go through the CG isOnscreen filter; not
        // backfilled here: when closed (orderOut) they must not show.
        if pid == std::process::id() as i32 {
            continue;
        }
        let mut entries: Vec<(u32, &String, bool)> =
            wid_map.iter().map(|(w, (t, m))| (*w, t, *m)).collect();
        entries.sort_by_key(|(w, _, _)| *w);
        for (cgwid, title, minimized) in entries {
            if shown.contains(&(pid, cgwid)) {
                continue;
            }
            // 与 CG 路径同款过滤:最小化由开关控制;空标题(非 titleless)无意义。
            // Same filters as the CG path: minimized gated by the switch; empty titles
            // (not titleless) are meaningless.
            if !show_minimized && minimized {
                continue;
            }
            if title.is_empty() && !titleless_pids.contains(&pid) {
                continue;
            }
            initialize_window_mru(
                mru,
                pid,
                cgwid,
                last_activated.get(&pid).copied(),
                ancient_base,
                insertion_order,
            );
            insertion_order += 1;
            let icon_path = icon_ids.get(&pid).and_then(check_cache_for_identity);
            log_debug!(
                "[collect] ax-only window restored: pid={} app=\"{}\" cgwid={} title=\"{}\"",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                cgwid,
                title
            );
            windows.push(WindowInfo {
                pid,
                window_id: cgwid,
                app_name: pid_names.get(&pid).cloned().unwrap_or_default(),
                window_title: title.clone(),
                icon_path,
                is_active: false,
                minimized,
                bounds: (0.0, 0.0, 0.0, 0.0),
            });
        }
    }

    // 修剪 MRU:只保留存活窗口的条目。存活集用 All 模式枚举(含最小化/离屏窗口),
    // 不用显示列表——OnScreenOnly 看不到最小化窗口,按显示列表修剪会清掉它们的
    // 排序记忆。已关闭窗口的残留条目不清的话,系统复用 CGWindowID 时新窗口会
    // or_insert 命中旧时间戳、按旧窗口的时间排序(继承污染)。存活集只做最保守
    // 过滤(layer 0 + 有效 pid),宁全勿缺;显示层的过滤(AX 配对/Dock/alpha)不适用。
    // 代价:show_minimized 关闭时每次 summon 补一次 All 枚举(亚毫秒级)。
    //
    // Prune MRU to the live window set. The live set is enumerated in All mode (includes
    // minimized/off-screen windows) rather than the display list -- OnScreenOnly can't see
    // minimized windows, so pruning against the display list would wipe their ordering
    // memory. Without pruning, a recycled CGWindowID would make a new window or_insert
    // the dead window's timestamp (inheritance pollution). The live set uses only the most
    // conservative filters (layer 0 + valid pid) -- better to keep than to drop; display
    // filters (AX pairing / Dock / alpha) don't apply here. Cost: one extra All-mode
    // enumeration per summon when show_minimized is off (sub-millisecond).
    let mut live_set: HashSet<(i32, u32)> = {
        let src = if show_minimized {
            array // 主查询已是 All 模式,直接复用 / main query is already All mode
        } else {
            unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0) }
        };
        let mut s: HashSet<(i32, u32)> = HashSet::new();
        if !src.is_null() {
            let n = unsafe { CFArrayGetCount(src) };
            for i in 0..n {
                let dict = unsafe { CFArrayGetValueAtIndex(src, i) };
                if dict.is_null() {
                    continue;
                }
                if cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999) != 0 {
                    continue;
                }
                let pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
                if pid <= 0 {
                    continue;
                }
                let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
                s.insert((pid, cgwid));
            }
        }
        if !show_minimized && !src.is_null() {
            unsafe { CFRelease(src) };
        }
        s
    };
    // AX 仍报但 CG 枚举不到的窗口(orderOut 的合法窗口)也是存活窗口:并入存活集,
    // 否则 MRU 修剪会清掉它们的排序记忆(下次 summon 又按新窗口排到末尾)。
    // AX-reported windows missing from the CG enumeration (orderOut'd but legitimate)
    // count as alive too: merge them in, or the MRU prune would wipe their ordering
    // memory (they would re-sort to the tail as "new" windows next summon).
    for (&pid, wid_map) in ax_wid_to_info.iter() {
        for &cgwid in wid_map.keys() {
            live_set.insert((pid, cgwid));
        }
    }
    let pruned = prune_mru(mru, &live_set);
    // 修剪数量可观测(debug 档):有残留才打,平时每次 summon 无噪音。
    // Pruning is observable (debug tier): logged only when entries were dropped, so a
    // normal summon stays silent.
    if pruned > 0 {
        log_debug!("[windows] pruned {} stale MRU entries", pruned);
    }

    unsafe { CFRelease(array) };

    let (front_pid, frontmost, fm_ms): (Option<i32>, Option<(i32, u32)>, u128) = if bump_frontmost {
        // 只有召唤刷新才读取并 bump 当前前台窗口;生命周期刷新不能因临时窗口创建而改写 MRU。
        // Only summon refreshes read and bump the frontmost window; a transient lifecycle
        // window must not rewrite MRU just because it caused a refresh.
        let t_fm = Instant::now();
        let result = unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front_app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if !front_app.is_null() {
                let front_pid: i32 = msg_send![front_app, processIdentifier];
                (
                    Some(front_pid),
                    focused_window_cgwid(front_pid).map(|cgwid| (front_pid, cgwid)),
                )
            } else {
                (None, None)
            }
        };
        (result.0, result.1, t_fm.elapsed().as_millis())
    } else {
        (None, None, 0)
    };
    if bump_frontmost {
        // 前台 app 的 AX 聚焦窗口查询也单独计时(从慢响应 app 切出时这里是第二个等待点)。
        // The frontmost app's AX focused-window query is timed too (a second wait when leaving a
        // slow-responding app).
        if let Some((pid, _)) = frontmost {
            log_debug!(
                "[collect] frontmost pid={} app=\"{}\" {}ms",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                fm_ms
            );
        }
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
        } else if let Some((pid, cgwid)) = frontmost_fallback(&windows, front_pid) {
            // 回退严格限制在系统前台 App 内,避免 AX 失败时把其他 App 的 CG 首项误刷为最新。
            // Restrict fallback to the system frontmost app so an AX failure cannot bump another
            // app's first CG item as most recent.
            mru.insert((pid, cgwid), now);
            if let Some(w) = windows
                .iter()
                .find(|w| w.pid == pid && w.window_id == cgwid)
            {
                log_debug!(
                    "summon-bump frontmost (fallback): pid={} app=\"{}\" cgwid={} title=\"{}\"",
                    w.pid,
                    w.app_name,
                    w.window_id,
                    w.window_title
                );
            }
        }
    }

    // 纯窗口级 MRU 排序：每个窗口独立按最后被激活的时间排序。
    // LAST_ACTIVATED 只为新窗口生成一次初始窗口时间；已有窗口不更新，避免从 App C
    // 切到浏览器窗口 A 时，浏览器的旧窗口 B 也搭便车排到 C 前面。
    // Pure window-level MRU sort: each window is sorted independently by
    // when it was last activated. LAST_ACTIVATED only seeds a newly discovered window once;
    // existing windows are not updated, preventing browser window B from riding A's coattails
    // when switching from app C to browser window A.
    sort_windows_by_mru(&mut windows, mru, now);

    // 每次完整刷新时打印排序后的窗口列表(=下一次显示顺序),含 mru 年龄。`*`标记第 0 个。
    // Print the sorted window list on every full refresh (= the next display order), with MRU age.
    // `*` marks index 0.
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
    // TIMING-DEBUG 汇总:总耗时 + 各阶段(排查 summon 卡顿用)。
    // TIMING-DEBUG Summary: total + per-phase timings (for summon-stall diagnosis).
    let total_ms = t0.elapsed().as_millis();
    log_debug!(
        "[collect] total={}ms cg={}ms ax={}ms frontmost={}ms",
        total_ms,
        t_cg_ms,
        ax_total_ms,
        fm_ms
    );
    windows
}

/// Pre‑cache icons for every currently‑running regular application.
/// Called once at startup so the overlay never shows a missing icon.
///
/// 只处理 .regular 策略的应用:嵌套 helper 后台进程(如像素蛋糕的 pix-worker/
/// pix-camera-link)与主应用共用 bundle id,其 NSRunningApplication.icon 是
/// AppKit 通用占位图,提取会以相同缓存键污染主应用图标,且 helper 与主程序
/// 二进制 mtime 相同(同一安装包),meta 校验无法察觉——错误图标会整个会话
/// 有效。helper 无窗口,其图标本就不需要。
///
/// Pre-cache icons for every currently-running REGULAR application. Helper
/// background processes (e.g. PixCake's nested pix-worker/pix-camera-link) share
/// the main app's bundle id, and their NSRunningApplication.icon is the AppKit
/// generic placeholder -- extracting poisons the shared cache key, and since the
/// helpers' binary mtimes equal the main binary's (same install), the .meta check
/// can never detect it. Helpers have no windows; their icons are never needed.
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
            // NSApplicationActivationPolicyRegular = 0;后台/辅助进程跳过。
            // NSApplicationActivationPolicyRegular = 0; skip background/helper apps.
            let policy: i64 = msg_send![app, activationPolicy];
            if policy != 0 {
                skipped += 1;
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if check_icon_cache(pid).is_none() {
                let name_str = crate::ffi::ns_running_app_name(app);
                let name_str = if name_str.is_empty() {
                    "?".to_string()
                } else {
                    name_str
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
        "icon cache done: {} cached, {} skipped (already fresh / non-regular)",
        cached.len(),
        skipped,
    );
}

/// 启动时预热剪贴板标题栏的小图标(16pt)。仅当配置开启剪贴板时才调用(main.rs 门控),
/// 否则小图标缓存不会被生成——剪贴板功能关闭时没必要为每个运行应用提取。
/// Pre-warm the clipboard header's small icons (16pt) at startup. Only called when the
/// clipboard feature is enabled (gated in main.rs); the small cache stays ungenerated when
/// the feature is off -- extracting it for every running app would be wasted work.
pub fn cache_running_app_icons_small() {
    let mut cached: Vec<String> = Vec::new();
    unsafe {
        // 与 cache_running_app_icons 同理:NSApp run 之前主线程没有 autorelease 池。
        // Same as cache_running_app_icons: no autorelease pool before NSApp run.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            // 同 cache_running_app_icons:跳过后台/辅助进程(helper 图标会污染共享缓存键)。
            // Same as cache_running_app_icons: skip background/helper apps (their icons
            // would poison the shared cache key).
            let policy: i64 = msg_send![app, activationPolicy];
            if policy != 0 {
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            // extract_small_icon 内部按 {key}.small.png + mtime 指纹校验,命中即跳过。
            // extract_small_icon verifies {key}.small.png + the mtime fingerprint, hitting
            // the cache skips the work.
            if extract_small_icon(pid).is_some() {
                cached.push(pid.to_string());
            }
        }
        let _: () = msg_send![pool, drain];
    }
    log_debug!(
        "small icon cache done: {} cached/verified (clipboard)",
        cached.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ax_subrole_keep_rule_accepts_standard_and_titled_dialog() {
        use super::ax_subrole_kept;
        // 标准窗口:任意标题。/ Standard windows: any title.
        assert!(ax_subrole_kept(Some("AXStandardWindow"), false));
        assert!(ax_subrole_kept(Some("AXStandardWindow"), true));
        // AXDialog(JetBrains 主窗口):必须带非空标题。
        // AXDialog (JetBrains main windows): must be titled.
        assert!(ax_subrole_kept(Some("AXDialog"), true));
        assert!(!ax_subrole_kept(Some("AXDialog"), false));
        // 弹窗/面板/隐形窗口:一律过滤。
        // Popups/panels/invisible windows: always filtered.
        assert!(!ax_subrole_kept(Some("AXUnknown"), true));
        assert!(!ax_subrole_kept(Some("AXSheet"), true));
        assert!(!ax_subrole_kept(Some("AXDrawer"), true));
        // 无 subrole(部分 App 不设置)→ 视为标准窗口。
        // Missing subrole (some apps don't set it) -> standard.
        assert!(ax_subrole_kept(None, false));
    }

    fn window(pid: i32, wid: u32) -> WindowInfo {
        WindowInfo {
            pid,
            window_id: wid,
            app_name: String::new(),
            window_title: String::new(),
            icon_path: None,
            is_active: false,
            minimized: false,
            bounds: (0.0, 0.0, 0.0, 0.0),
        }
    }

    #[test]
    fn fnv1a_hex_is_deterministic_and_stable() {
        // 同一输入恒定;输出为 16 位十六进制,不含 `/`(文件名安全)。
        // Same input -> same output; 16 hex chars, no '/' (filename-safe).
        let a = fnv1a_hex("/Applications/Safari.app/Contents/MacOS/Safari");
        let b = fnv1a_hex("/Applications/Safari.app/Contents/MacOS/Safari");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // 不同路径应产生不同键(极低碰撞概率)。
        // Different paths should yield different keys (vanishingly low collision chance).
        assert_ne!(
            fnv1a_hex("/Applications/Safari.app"),
            fnv1a_hex("/Applications/Firefox.app")
        );
        assert_ne!(fnv1a_hex(""), fnv1a_hex("x"));
    }

    #[test]
    fn small_icon_path_uses_dot_small_suffix() {
        // 剪贴板小图 = {key}.small.png,与切换器大图({key}.png)同 key 同目录。
        // The clipboard small icon = {key}.small.png, same key and dir as the switcher's
        // big icon ({key}.png).
        let key = "com.apple.Safari";
        let path = small_icon_path_for_key(key);
        assert!(path.ends_with(&format!("{}.small.png", key)), "{}", path);
        assert!(!path.contains("..png"));
        // 大小图路径只差后缀。
        // The big/small paths differ only by the suffix.
        let big = cache_path_for_key_suffix(key, "");
        assert!(big.ends_with(&format!("{}.png", key)), "{}", big);
        assert_eq!(path, format!("{}.small.png", &big[..big.len() - 4]));
    }

    #[test]
    fn sort_windows_by_mru_orders_most_recent_first() {
        let now = Instant::now();
        let mut mru = MruMap::new();
        // 窗口 (1,100) 5 秒前激活,(2,200) 1 秒前激活 -> 后者在前。
        mru.insert((1, 100), now - std::time::Duration::from_secs(5));
        mru.insert((2, 200), now - std::time::Duration::from_secs(1));
        let mut ws = vec![window(1, 100), window(2, 200)];
        sort_windows_by_mru(&mut ws, &mru, now);
        assert_eq!((ws[0].pid, ws[0].window_id), (2, 200));
        assert_eq!((ws[1].pid, ws[1].window_id), (1, 100));
    }

    #[test]
    fn sort_windows_by_mru_no_record_sorted_last() {
        // 无 MRU 记录的窗口回退 999 秒,排在所有有记录的窗口之后。
        // Windows without an MRU record fall back to 999s — after all recorded ones.
        let now = Instant::now();
        let mut mru = MruMap::new();
        mru.insert((1, 100), now - std::time::Duration::from_secs(60));
        let mut ws = vec![window(9, 999), window(1, 100)];
        sort_windows_by_mru(&mut ws, &mru, now);
        assert_eq!((ws[0].pid, ws[0].window_id), (1, 100));
        assert_eq!((ws[1].pid, ws[1].window_id), (9, 999));
    }

    #[test]
    fn newly_discovered_windows_preserve_activation_timeline() {
        // Edge 启动时先使用新标签页，随后恢复并聚焦 ChatGPT：恢复窗口的显式焦点 bump
        // 最新，新标签页保留 Edge 激活种子，之前使用的其他 App 依次排后。
        // Edge starts on a new tab, then restores and focuses ChatGPT: the restored window's
        // explicit focus bump is newest, the new tab keeps Edge's activation seed, and apps
        // used earlier follow in order.
        let now = Instant::now();
        let ancient = now - Duration::from_secs(86_400);
        let edge_activated = now - Duration::from_secs(2);
        let mut mru = MruMap::new();
        mru.insert((20, 200), now - Duration::from_secs(10)); // RustRover
        mru.insert((30, 300), now - Duration::from_secs(20)); // Ghostty
        initialize_window_mru(&mut mru, 10, 101, Some(edge_activated), ancient, 0); // new tab
        initialize_window_mru(&mut mru, 10, 102, Some(edge_activated), ancient, 1); // ChatGPT
        mru.insert((10, 102), now); // restored focused window

        let mut ws = vec![
            window(30, 300),
            window(10, 101),
            window(20, 200),
            window(10, 102),
        ];
        sort_windows_by_mru(&mut ws, &mru, now);
        let order: Vec<(i32, u32)> = ws.iter().map(|w| (w.pid, w.window_id)).collect();
        assert_eq!(order, vec![(10, 102), (10, 101), (20, 200), (30, 300)]);
    }

    #[test]
    fn app_activation_does_not_bump_existing_sibling_window() {
        // `or_insert` 不覆盖已有窗口：激活浏览器窗口 A 时，旧窗口 B 保留自己的旧 MRU。
        // `or_insert` does not overwrite an existing window: activating browser window A
        // leaves old sibling B at its own older MRU.
        let now = Instant::now();
        let old_sibling_mru = now - Duration::from_secs(60);
        let mut mru = MruMap::from([((10, 100), old_sibling_mru)]);
        initialize_window_mru(
            &mut mru,
            10,
            100,
            Some(now - Duration::from_secs(1)),
            now - Duration::from_secs(86_400),
            0,
        );
        assert_eq!(mru.get(&(10, 100)), Some(&old_sibling_mru));
    }

    #[test]
    fn new_window_without_activation_uses_ancient_fallback() {
        let now = Instant::now();
        let ancient = now - Duration::from_secs(86_400);
        let mut mru = MruMap::new();
        initialize_window_mru(&mut mru, 10, 100, None, ancient, 3);
        assert_eq!(
            mru.get(&(10, 100)),
            Some(&(ancient - Duration::from_millis(3)))
        );
    }

    #[test]
    fn frontmost_fallback_never_selects_another_app() {
        let mut own_front = window(10, 100);
        own_front.minimized = true;
        let ws = vec![window(20, 200), own_front];
        assert_eq!(frontmost_fallback(&ws, Some(10)), None);
        assert_eq!(frontmost_fallback(&ws, Some(20)), Some((20, 200)));
        assert_eq!(frontmost_fallback(&ws, None), None);
    }

    #[test]
    fn seed_groups_windows_by_app_in_system_front_to_back_order() {
        // 启动种子:同 App 窗口分组、App 顺序 = 前到后首次出现、应用内 = CG 序,
        // 且全部排在无记录(999s 回退)之前。这是"重启后顺序与原生应用序一致"的核心。
        // Startup seed: same-app windows group together, apps follow the front-to-back
        // first-appearance order, per-app windows keep the CG z-order, and everything
        // sorts ahead of the 999s no-record fallback -- the core of "order matches the
        // native app order after restart".
        let now = Instant::now();
        // 呈现为乱序的 CG 前→后窗口流,经 app_order/app_windows 分组后应为:
        // App1(100,200) / App2(300) / App3(400,500) —— 应用内窗口按 CG 出现序。
        // A jumbled CG window stream exposes the grouping: after app_order/app_windows,
        // the display order must be App1(100,200) / App2(300) / App3(400,500), with each
        // app's windows in CG appearance order.
        let app_order = vec![1, 2, 3];
        let app_windows: HashMap<i32, Vec<u32>> = [
            (1, vec![200, 100]), // CG 序:200 在前(更靠前)
            (2, vec![300]),
            (3, vec![400, 500]),
        ]
        .into_iter()
        .collect();
        let mru = seed_timestamps(&app_order, &app_windows, now);

        let mut ws = vec![
            window(3, 500),
            window(1, 100),
            window(2, 300),
            window(3, 400),
            window(1, 200),
            window(9, 999), // 无记录回退 / no-record fallback
        ];
        sort_windows_by_mru(&mut ws, &mru, now);
        let order: Vec<(i32, u32)> = ws.iter().map(|w| (w.pid, w.window_id)).collect();
        assert_eq!(
            order,
            vec![(1, 200), (1, 100), (2, 300), (3, 400), (3, 500), (9, 999)],
            "seeded order must group apps and keep the per-app CG order"
        );
    }

    #[test]
    fn prune_mru_drops_only_dead_entries() {
        let now = Instant::now();
        let mut mru = MruMap::new();
        mru.insert((1, 100), now);
        mru.insert((2, 200), now); // 死条目 / dead entry
        mru.insert((3, 300), now); // 死条目 / dead entry
        let live: HashSet<(i32, u32)> = [(1, 100), (4, 400)].into_iter().collect();
        let pruned = prune_mru(&mut mru, &live);
        assert_eq!(pruned, 2);
        assert!(mru.contains_key(&(1, 100)));
        assert_eq!(mru.len(), 1);
        // 无死条目时返回 0 且不动 map。
        // Nothing to drop -> 0 and the map is untouched.
        assert_eq!(prune_mru(&mut mru, &live), 0);
        assert_eq!(mru.len(), 1);
    }

    #[test]
    fn remove_pid_mru_drops_only_terminated_process() {
        let now = Instant::now();
        let mut mru = MruMap::from([((1, 100), now), ((1, 101), now), ((2, 200), now)]);
        assert_eq!(remove_pid_mru(&mut mru, 1), 2);
        assert_eq!(mru, MruMap::from([((2, 200), now)]));
        assert_eq!(remove_pid_mru(&mut mru, 1), 0);
    }

    #[test]
    fn bump_window_mru_ignores_zero_cgwid() {
        // cgwid == 0(未配对成功)不写入 MRU。
        // cgwid == 0 (pairing failed) is not recorded.
        let mut mru = MruMap::new();
        bump_window_mru(&mut mru, 1, 0);
        assert!(mru.is_empty());
        bump_window_mru(&mut mru, 1, 42);
        assert!(mru.contains_key(&(1, 42)));
    }

    // ========== 冒烟测试(需要真实 GUI 会话 + 辅助功能权限,手动运行)==========
    // ========== Smoke tests (need a real GUI session + Accessibility grant; run manually) ==========
    // 运行:cargo test -- --ignored
    // 这些测试真实调用 CG/AX 栈,CI 上无 GUI 会话,默认跳过。

    #[test]
    #[ignore]
    fn collect_windows_smoke() {
        // 无辅助功能权限时直接跳过(不是失败)。
        // Skip (not fail) when Accessibility is not granted.
        if !crate::ffi::has_accessibility_permission() {
            eprintln!("[smoke] Accessibility not granted; skipping collect_windows");
            return;
        }
        let mut mru = MruMap::new();
        let wins = collect_windows(&mut mru);
        // 有 GUI 会话时至少应能看到若干窗口(通常 >2)。
        // With a GUI session we should see at least a few windows (usually >2).
        assert!(wins.len() >= 2, "expected >=2 windows, got {}", wins.len());
        // 不变式 1:(pid, window_id) 全局唯一。
        // Invariant 1: (pid, window_id) globally unique.
        let mut seen: HashSet<(i32, u32)> = HashSet::new();
        for w in &wins {
            assert!(w.window_id != 0, "window_id must be nonzero");
            assert!(
                seen.insert((w.pid, w.window_id)),
                "duplicate window: pid={} wid={}",
                w.pid,
                w.window_id
            );
            assert!(!w.app_name.is_empty(), "app_name must not be empty");
        }
        // 不变式 2:第一个窗口被标记为激活。
        // Invariant 2: the first window is marked active.
        assert!(wins[0].is_active);
        // 不变式 3:排序依赖 MRU —— 显示列表里每个窗口都应有 MRU 条目。
        // 反向(每个 MRU 条目都在显示列表)不成立:frontmost 聚焦窗口是单独经系统 API
        // bump 的,可能被 AX 配对过滤而不在显示列表,反向断言会偶发误报。
        // Invariant 3: sorting relies on MRU — every display-list window must have an entry.
        // The converse (every MRU entry in the display list) does NOT hold: the frontmost
        // focused window is bumped via a separate system-API path and may be filtered out of
        // the display list by AX pairing, so a reverse assertion would flake spuriously.
        let all_have_mru = wins.iter().all(|w| mru.contains_key(&(w.pid, w.window_id)));
        assert!(
            all_have_mru,
            "some display windows lack MRU entries (sorting would fall back)"
        );
    }

    #[test]
    #[ignore]
    fn icon_cache_roundtrip_smoke() {
        if !crate::ffi::has_accessibility_permission() {
            eprintln!("[smoke] Accessibility not granted; skipping icon roundtrip");
            return;
        }
        // 用 Finder(bundle id 稳定)做往返——测试二进制自身是裸 exec,身份解析不可靠。
        // Use Finder (stable bundle id) for the roundtrip; the test binary itself is a bare
        // exec whose identity resolution is unreliable.
        let pid = unsafe {
            let ns_key = crate::ffi::make_nsstring("com.apple.finder");
            let apps: *mut AnyObject = msg_send![
                class!(NSRunningApplication),
                runningApplicationsWithBundleIdentifier: ns_key
            ];
            CFRelease(ns_key as *const c_void);
            let count: usize = msg_send![apps, count];
            let mut pid: i32 = 0;
            if count > 0 {
                let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
                pid = msg_send![app, processIdentifier];
            }
            pid
        };
        assert!(pid > 0, "Finder must be running in a GUI session");
        // 清空缓存从干净状态开始(冒烟测试会重提图标,是可接受的副作用)。
        // Start clean by clearing the cache (the smoke test re-extracts; acceptable side effect).
        clear_icon_cache();
        let path = extract_icon_to_cache(pid).expect("Finder icon extraction failed");
        assert!(std::fs::metadata(&path).is_ok(), "extracted PNG must exist");
        // 再次查询应命中缓存,幂等返回同一路径。
        // A second query hits the cache; idempotent same path.
        assert_eq!(check_icon_cache(pid).as_deref(), Some(path.as_str()));
        assert_eq!(
            extract_icon_to_cache(pid).as_deref(),
            Some(path.as_str()),
            "re-extract must short-circuit on a valid cache"
        );
        clear_icon_cache();
    }
}
