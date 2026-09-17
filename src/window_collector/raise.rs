//! 窗口收集 · raise:SLPS 私有 API 按窗口精确抬升。
//! Exact per-window raising through the SLPS private APIs.

use super::*;

// ========== 私有 API（统一经 skylight.rs 的 dlopen/dlsym 加载）==========
// Private APIs (loaded via skylight.rs's shared dlopen/dlsym helpers).
// 用来按 CGWindowID 精确配对 AX/CG 窗口、并在 WindowServer 层只抬起一个窗口。

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

type AxGetWindowFn = unsafe extern "C" fn(AXUIElementRef, *mut u32) -> AXError;
type GetProcessForPIDFn = unsafe extern "C" fn(i32, *mut ProcessSerialNumber) -> i32;
type SlpSetFrontFn = unsafe extern "C" fn(*mut ProcessSerialNumber, u32, i32) -> i32;
type SlpsPostEventRecordFn = unsafe extern "C" fn(*mut ProcessSerialNumber, *mut u8) -> i32;

static AX_GET_WINDOW: std::sync::LazyLock<Option<AxGetWindowFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // _AXUIElementGetWindow 是 HIServices 里的私有符号，dlopen 后 dlsym。
        skylight::load_private_symbol(skylight::HISERVICES_PATH, "_AXUIElementGetWindow")
    });
static GET_PROCESS_FOR_PID: std::sync::LazyLock<Option<GetProcessForPIDFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // GetProcessForPID 也在 HIServices，dlopen 后 dlsym。
        skylight::load_private_symbol(skylight::HISERVICES_PATH, "GetProcessForPID")
    });
static SLP_SET_FRONT: std::sync::LazyLock<Option<SlpSetFrontFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // SkyLight 是私有框架，必须先 dlopen 才能查到符号。
        skylight::load_private_symbol(skylight::SKYLIGHT_PATH, "_SLPSSetFrontProcessWithOptions")
    });
static SLPS_POST_EVENT_RECORD: std::sync::LazyLock<Option<SlpsPostEventRecordFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // make_key_window 的合成鼠标事件也走 SkyLight 的 SLPSPostEventRecordTo。
        // Synthetic mouse events for make_key_window go through SkyLight's
        // SLPSPostEventRecordTo as well.
        skylight::load_private_symbol(skylight::SKYLIGHT_PATH, "SLPSPostEventRecordTo")
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
        None => {
            log_missing_slps_symbol(
                &SLPS_GET_PROCESS_MISSING_LOGGED,
                "[raise] SLPS unavailable: GetProcessForPID symbol missing",
            );
            return false;
        }
    };
    let set_front = match *SLP_SET_FRONT {
        Some(f) => f,
        None => {
            log_missing_slps_symbol(
                &SLPS_SET_FRONT_MISSING_LOGGED,
                "[raise] SLPS unavailable: _SLPSSetFrontProcessWithOptions symbol missing",
            );
            return false;
        }
    };
    let mut psn = ProcessSerialNumber {
        high_long_of_psn: 0,
        low_long_of_psn: 0,
    };
    let psn_status = get_psn(pid, &mut psn);
    if psn_status != 0 {
        log_debug!(
            "[raise] SLPS failed: GetProcessForPID pid={} status={}",
            pid,
            psn_status
        );
        return false;
    }
    let status = set_front(&mut psn, wid, 0x200); // kCPSUserGenerated; CGError success == 0
    if status != 0 {
        log_debug!(
            "[raise] SLPS failed: _SLPSSetFrontProcessWithOptions pid={} cgwid={} status={}",
            pid,
            wid,
            status
        );
    }
    status == 0
}

/// 用 SkyLight 私有 API SLPSPostEventRecordTo 向目标窗口投递一次合成鼠标按下事件，
/// 使该窗口成为其 App 的 key window（红绿灯变彩色）。macOS 14+ 把 NSRunningApplication
/// activate 降级为「建议性请求」，跨 App 转移 key 焦点只剩这条可靠路径；0x200 的
/// userGenerated 前台切换只解决「抬到前面」，key 状态必须靠这个合成点击确立。
/// 点击点放到窗口右下方很远处，避免命中任何内容或 resize 区域。事件按 CGWindowID 定向投递，
/// 与坐标无关。
/// 字节布局来自 AltTab 对 CGSInternal/CGSEvent.h 的反向工程；缓冲必须 ≥0x100 并清零，
/// 否则 macOS 14.7.4+ 的 CGSEncodeEventRecord 会越界读取导致 SIGABRT（paneru#123）。
///
/// Make the window `wid` the key window of its app by posting a synthetic left-mouse-down
/// to the WindowServer via the SkyLight private API SLPSPostEventRecordTo.
/// macOS 14+ downgraded NSRunningApplication.activate to an advisory "request"; posting
/// this click is the only reliable way left to move key focus across apps. The 0x200
/// userGenerated front-switch only fronts the window -- the key state needs this click.
/// The click is aimed far beyond the window's bottom-right corner, so it hit-tests to no view
/// or resize edge (nothing is clicked). The event is delivered to the window by CGWindowID,
/// not by the click point.
/// layout is AltTab's reverse-engineering of CGSInternal/CGSEvent.h; the buffer must be
/// at least 0x100 bytes and zeroed, or CGSEncodeEventRecord reads past it on macOS 14.7.4+
/// and SIGABRTs (paneru issue 123).
unsafe fn make_key_window(pid: i32, wid: u32) -> bool {
    let get_psn = match *GET_PROCESS_FOR_PID {
        Some(f) => f,
        None => {
            log_missing_slps_symbol(
                &SLPS_GET_PROCESS_MISSING_LOGGED,
                "[raise] key click unavailable: GetProcessForPID symbol missing",
            );
            return false;
        }
    };
    let post = match *SLPS_POST_EVENT_RECORD {
        Some(f) => f,
        None => {
            log_missing_slps_symbol(
                &SLPS_POST_EVENT_MISSING_LOGGED,
                "[raise] key click unavailable: SLPSPostEventRecordTo symbol missing",
            );
            return false;
        }
    };
    let mut psn = ProcessSerialNumber {
        high_long_of_psn: 0,
        low_long_of_psn: 0,
    };
    let psn_status = get_psn(pid, &mut psn);
    if psn_status != 0 {
        log_debug!(
            "[raise] key click failed: GetProcessForPID pid={} status={}",
            pid,
            psn_status
        );
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
    // 窗口相对点击点 @ 0x20(16 字节 = CGPoint 两个 f64),远离内容和 resize 区域。
    bytes[0x20..0x28].copy_from_slice(&(300_000.0f64).to_le_bytes());
    bytes[0x28..0x30].copy_from_slice(&(300_000.0f64).to_le_bytes());
    // 0x08 = CGSEventType:一次左键按下即可让目标窗口变 key。
    // 0x08 = CGSEventType: one left-mouse-down makes the target window key.
    bytes[0x08] = 0x01;
    let status = post(&mut psn, bytes.as_mut_ptr());
    if status != 0 {
        log_debug!(
            "[raise] key click failed: SLPSPostEventRecordTo pid={} cgwid={} status={}",
            pid,
            wid,
            status
        );
    }
    status == 0
}

/// Ask AppKit to activate an application without raising all of its windows.
/// This is used only after the precise WindowServer path reports a transient failure.
pub(crate) fn activate_pid(pid: i32) -> bool {
    unsafe {
        let app: *mut AnyObject =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            log_debug!("[raise] activate fallback: no running app for pid={}", pid);
            return false;
        }
        let activated: bool = msg_send![app, activateWithOptions: 0usize];
        if !activated {
            log_debug!(
                "[raise] activate fallback failed: pid={} activateWithOptions=false",
                pid
            );
        }
        activated
    }
}

/// 由 &str 构造 CFString(+1 引用,调用方 CFRelease)。窗口控制模块复用。
/// Build a CFString from &str (+1 reference; caller CFReleases). Reused by window control.
pub(crate) fn cf_string_new(s: &str) -> *const c_void {
    let c_str = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100) }
}

pub(super) unsafe fn cache_ax_window_element(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
    element: AXUIElementRef,
) {
    let Some(process_start_time_us) = process_start_time_us else {
        return;
    };
    if cgwid == 0 || element.is_null() {
        return;
    }
    let key = AxWindowCacheKey {
        pid,
        process_start_time_us,
        cgwid,
    };
    CFRetain(element);
    let old = AX_WINDOW_CACHE
        .lock()
        .unwrap()
        .insert(key, CachedAxElement(element));
    if let Some(old) = old {
        CFRelease(old.0);
    }
}

pub(super) unsafe fn cached_ax_window_element(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
) -> Option<AXUIElementRef> {
    let key = AxWindowCacheKey {
        pid,
        process_start_time_us: process_start_time_us?,
        cgwid,
    };
    let cached = AX_WINDOW_CACHE.lock().unwrap().get(&key).copied()?;
    // The caller owns this temporary retain and must CFRelease it after the AX operations.
    CFRetain(cached.0);
    Some(cached.0)
}

pub(super) unsafe fn invalidate_cached_ax_window_element(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
    element: AXUIElementRef,
) {
    let Some(process_start_time_us) = process_start_time_us else {
        return;
    };
    let key = AxWindowCacheKey {
        pid,
        process_start_time_us,
        cgwid,
    };
    let removed = {
        let mut cache = AX_WINDOW_CACHE.lock().unwrap();
        if cache.get(&key).is_some_and(|cached| cached.0 == element) {
            cache.remove(&key)
        } else {
            None
        }
    };
    if let Some(removed) = removed {
        CFRelease(removed.0);
    }
}

pub(crate) fn clear_ax_window_cache_for_pid(pid: i32) {
    let keys: Vec<_> = AX_WINDOW_CACHE
        .lock()
        .unwrap()
        .keys()
        .filter(|key| key.pid == pid)
        .copied()
        .collect();
    let removed: Vec<_> = keys
        .into_iter()
        .filter_map(|key| AX_WINDOW_CACHE.lock().unwrap().remove(&key))
        .collect();
    for element in removed {
        unsafe { CFRelease(element.0) };
    }
    AX_SNAPSHOT_CACHE.lock().unwrap().remove(&pid);
}

pub(super) fn cached_ax_snapshot(
    pid: i32,
    process_start_time_us: Option<u64>,
) -> Option<Vec<AxWindowInfo>> {
    let start = process_start_time_us?;
    let cache = AX_SNAPSHOT_CACHE.lock().unwrap();
    let snapshot = cache.get(&pid)?;
    if snapshot.process_start_time_us != Some(start)
        || snapshot.refreshed_at.elapsed() > AX_SNAPSHOT_CACHE_TTL
    {
        return None;
    }
    Some(snapshot.windows.clone())
}

pub(super) fn cache_ax_snapshot(
    pid: i32,
    process_start_time_us: Option<u64>,
    windows: &[AxWindowInfo],
) {
    if process_start_time_us.is_none() {
        return;
    }
    AX_SNAPSHOT_CACHE.lock().unwrap().insert(
        pid,
        CachedAxSnapshot {
            process_start_time_us,
            refreshed_at: Instant::now(),
            windows: windows.to_vec(),
        },
    );
}

pub(super) fn cf_dict_get_string(dict: *const c_void, key: &str) -> Option<String> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    cf_to_rust_string(value)
}

pub(super) fn cf_dict_get_i32(dict: *const c_void, key: &str) -> Option<i32> {
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

pub(super) fn cf_dict_get_u32(dict: *const c_void, key: &str) -> Option<u32> {
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
pub(super) fn cf_dict_get_f64(dict: *const c_void, key: &str) -> Option<f64> {
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
pub(super) fn cf_dict_get_bool(dict: *const c_void, key: &str) -> Option<bool> {
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
pub(super) fn cf_dict_get_bounds(dict: *const c_void, key: &str) -> Option<(f64, f64, f64, f64)> {
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

// AppIdentity / resolve_app_identity 已下沉到 crate::app_identity(消除与
// icon_cache 的模块环);本模块经上方 use 引入继续使用。
// AppIdentity / resolve_app_identity now live in crate::app_identity (breaking the
// module cycle with icon_cache); this module imports them above.

fn window_instance_key(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
) -> Option<WindowInstanceKey> {
    (pid > 0 && cgwid != 0).then_some(WindowInstanceKey {
        pid,
        process_start_time_us: process_start_time_us?,
        cgwid,
    })
}

pub(super) fn remember_non_normal_window(pid: i32, process_start_time_us: Option<u64>, cgwid: u32) {
    let Some(key) = window_instance_key(pid, process_start_time_us, cgwid) else {
        return;
    };
    let mut known = KNOWN_NON_NORMAL_WINDOWS.lock().unwrap();
    // A newly observed process incarnation with the same PID supersedes any stale entries from
    // the old incarnation. This also bounds the cache when a WindowServer destroy notification
    // is delayed or lost.
    known.retain(|entry| {
        entry.pid != key.pid || entry.process_start_time_us == key.process_start_time_us
    });
    known.insert(key);
}

pub(super) fn is_known_non_normal_window(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
) -> bool {
    window_instance_key(pid, process_start_time_us, cgwid)
        .is_some_and(|key| KNOWN_NON_NORMAL_WINDOWS.lock().unwrap().contains(&key))
}

/// Forget a destroyed window's sticky layer classification. CGWindowID is globally unique while
/// alive, so removing all entries with this ID is safe and prevents stale state on ID reuse.
pub(crate) fn forget_non_normal_window(cgwid: u32) {
    if cgwid == 0 {
        return;
    }
    KNOWN_NON_NORMAL_WINDOWS
        .lock()
        .unwrap()
        .retain(|entry| entry.cgwid != cgwid);
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

/// 抬升前半段:WindowServer 层抬窗(SLPS)+ 合成点击确立 key window。
/// 普通窗口在提交时调用;最小化窗口由后台 raiser 在解除最小化后调用。
///
/// First half of the raise: WindowServer-level raise (SLPS) plus the synthetic click that
/// establishes the key window. Normal windows call it at commit time; minimized windows call it
/// from the background raiser after being restored.
pub(crate) fn raise_window_fast(pid: i32, cgwid: u32) -> (bool, bool) {
    if cgwid == 0 {
        return (false, false);
    }
    unsafe {
        // 1. WindowServer 层只抬这一个窗口（SkyLight 私有 API _SLPSSetFrontProcessWithOptions，
        //    mode=0x200 userGenerated），避免 activate(AllWindows) 把该 App 的所有窗口都抬到前面。
        //    Raise only this one window at the WindowServer level (SkyLight private API,
        //    mode=0x200 userGenerated), avoiding activate(AllWindows) raising every window.
        let fast_started = Instant::now();
        let slps_started = Instant::now();
        let slps_ok = raise_window_slps(pid, cgwid);
        let slps_elapsed = slps_started.elapsed().as_micros();

        // 1.5 合成鼠标点击确立 key window：SLPS 只负责「抬到前面 + 设为前台进程」，key 状态
        //    必须由这个合成点击授予（macOS 14+ 无公开 API 可跨 App 转移 key 焦点）。
        //    The synthetic click establishes the key window: SLPS only fronts the window and
        //    process; the key state is granted by this click (macOS 14+ has no public API to
        //    move key focus across apps).
        let click_started = Instant::now();
        let click_ok = make_key_window(pid, cgwid);
        log_debug!(
            "[raise] fast stages: pid={} cgwid={} slps={} slps_us={} click={} click_us={} total_us={}",
            pid,
            cgwid,
            slps_ok,
            slps_elapsed,
            click_ok,
            click_started.elapsed().as_micros(),
            fast_started.elapsed().as_micros()
        );
        (slps_ok, click_ok)
    }
}
