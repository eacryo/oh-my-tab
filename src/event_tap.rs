//! CGEventTap 公共基础设施:类型别名、FFI extern 声明、语义常量、通用启动流程。
//! 被窗口切换(event_monitor)与鼠标增强(mouse::event_tap)两个模块共用,是叶子层。
//!
//! Common CGEventTap infrastructure: type aliases, FFI extern declarations, semantic
//! constants, and a generic start helper. Shared by the window switcher (event_monitor)
//! and the mouse enhancement (mouse::event_tap) modules. A leaf module.

use crate::ffi::has_accessibility_permission;
use crate::log_info;
use std::ffi::c_void;
use std::thread;
use std::time::Duration;

// ========== 类型别名 / type aliases ==========

pub(crate) type CGEventRef = *mut c_void;
pub(crate) type CGEventTapProxy = *mut c_void;
pub(crate) type CFMachPortRef = *mut c_void;
pub(crate) type CFRunLoopSourceRef = *mut c_void;
pub(crate) type CFRunLoopTimerRef = *mut c_void;
pub(crate) type CFRunLoopRef = *mut c_void;
pub(crate) type CFStringRef = *mut c_void;
pub(crate) type CFAllocatorRef = *mut c_void;
pub(crate) type CGEventType = u32;
pub(crate) type CGEventFlags = u64;
pub(crate) type CGEventMask = u64;

pub(crate) type CGEventTapCallBack = Option<
    unsafe extern "C" fn(
        proxy: CGEventTapProxy,
        event_type: CGEventType,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef,
>;

/// CFRunLoopTimer 回调:参数为 (timer, info)。
/// CFRunLoopTimer callout: (timer, info).
pub(crate) type CFRunLoopTimerCallBack =
    Option<unsafe extern "C" fn(CFRunLoopTimerRef, *mut c_void)>;

// ========== CGEventTap 语义常量 / semantic constants ==========
// 用语义化枚举替代裸数字,降低各调用方硬编码出错概率。
// Semantic enums in place of raw magic numbers, reducing per-caller hardcoding errors.

/// CGEventTapCreate 的 tap location 参数。
/// Tap location for CGEventTapCreate.
#[allow(dead_code)]
pub(crate) mod tap_location {
    /// HID 层:最底层,能看到所有硬件事件(含 session 层合成的)。
    /// HID level: lowest, sees all hardware events (including session-synthesized ones).
    pub(crate) const HID_EVENT_TAP: i32 = 0;
    /// Session 层:能看到真实硬件事件 + session 层合成的 Cmd+Tab(鼠标映射软件注入)。
    /// Session level: sees real hardware events + session-synthesized Cmd+Tab (mouse-remapper injected).
    pub(crate) const SESSION_EVENT_TAP: i32 = 1;
    #[allow(dead_code)]
    pub(crate) const ANNOTATED_SESSION_EVENT_TAP: i32 = 2;
}

/// CGEventTapCreate 的 placement 参数。
/// Placement for CGEventTapCreate.
#[allow(dead_code)]
pub(crate) mod tap_placement {
    /// 队首插入:最先看到事件。
    /// Head insert: sees events first.
    pub(crate) const HEAD_INSERT: i32 = 0;
    /// 队尾插入:最后看到事件。
    /// Tail insert: sees events last.
    pub(crate) const TAIL_INSERT: i32 = 1;
}

/// CGEventTapCreate 的 options 参数。
/// 注意:枚举值与直觉相反(见 CGEventTypes.h),Default=0 可改事件,ListenOnly=1 只读。
/// Options for CGEventTapCreate. Note the counterintuitive values (see CGEventTypes.h):
/// Default=0 is mutable, ListenOnly=1 is read-only.
#[allow(dead_code)]
pub(crate) mod tap_options {
    /// 默认 tap:可修改/丢弃事件(需要 AX 权限)。用于要改写事件的场景(transformer 链)。
    /// Default tap: may modify/drop events (requires AX permission). Used when rewriting events.
    pub(crate) const DEFAULT_TAP: u32 = 0;
    /// 只听不改:不能修改事件。用于纯观察/日志验证阶段,调试安全(callback bug 不会吞事件)。
    /// Listen only: cannot modify events. For observation/logging; debug-safe (callback bugs won't swallow events).
    pub(crate) const LISTEN_ONLY: u32 = 1;
}

// ========== FFI extern 声明 / FFI extern declarations ==========

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub(crate) fn CGEventTapCreate(
        tap: i32,
        place: i32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    pub(crate) fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    // 查询 tap 是否被系统启用(看门狗用)。
    // Query whether the tap is enabled system-side (used by the watchdog).
    pub(crate) fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
    pub(crate) fn CGEventGetIntegerValueField(event: CGEventRef, field: i32) -> i64;
    pub(crate) fn CGEventSetIntegerValueField(event: CGEventRef, field: i32, value: i64);
    #[allow(dead_code)]
    pub(crate) fn CGEventGetDoubleValueField(event: CGEventRef, field: i32) -> f64;
    #[allow(dead_code)]
    pub(crate) fn CGEventSetDoubleValueField(event: CGEventRef, field: i32, value: f64);
    pub(crate) fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    pub(crate) fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    // 从 CGEvent 提取底层 IOHIDEvent(公开 API);用于事件归因(按设备匹配配置)。
    // Extract the underlying IOHIDEvent from a CGEvent (public API); used for event attribution
    // (matching events to the producing device for per-device config).
    pub(crate) fn CGEventCopyIOHIDEvent(event: CGEventRef) -> *mut c_void;

    // 合成全新的滚轮事件。
    // source 传 null 表示用默认 source;wheelCount 通常为 2(wheel1=垂直,wheel2=水平)。
    // Create a brand-new scroll wheel event.
    // source=null for default source; wheelCount typically 2 (wheel1=vertical, wheel2=horizontal).
    pub(crate) fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;

    // 将事件投递到指定 tap 层级。kCGSessionEventTap=1 投递到 session 层,
    // 不经过 HID 层 tap,绕过系统自然滚动的 HID 层覆盖。
    // Post an event to a tap level. kCGSessionEventTap=1 posts to the session level,
    // bypassing HID-level taps and thus the system's natural-scroll override at the HID layer.
    pub(crate) fn CGEventPost(tap: i32, event: CGEventRef);

    // 创建键盘事件(keyDown=1 / keyUp=0),供历史剪贴板模拟 Cmd+V 粘贴使用。
    // Create a keyboard event (keyDown=1 / keyUp=0), used by the history clipboard to
    // synthesize Cmd+V for pasting.
    pub(crate) fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        keycode: u16,
        key_down: bool,
    ) -> CGEventRef;
}

// IOKit 私有 API:读写 IOHIDEvent 的浮点字段。
// 当前合成事件方案未使用(保留以备用)。
// IOKit private API: read/write float fields of an IOHIDEvent.
// Unused by the current synthetic-event approach (kept for potential future use).
#[allow(dead_code)]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// 读 IOHIDEvent 的浮点字段。
    /// Read a float field from an IOHIDEvent.
    pub(crate) fn IOHIDEventGetFloatValue(event: *mut c_void, field: u32) -> f64;
    /// 写 IOHIDEvent 的浮点字段。
    /// Write a float field to an IOHIDEvent.
    pub(crate) fn IOHIDEventSetFloatValue(event: *mut c_void, field: u32, value: f64);
}

// CFRunLoop 相关函数 + 定时器,链接 CoreFoundation。
// CFRunLoop functions + timer, linking CoreFoundation.
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;

    pub(crate) fn CFRunLoopAddSource(
        rl: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: CFStringRef,
    );
    pub(crate) fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub(crate) fn CFRunLoopRun();
    pub(crate) fn CFRunLoopStop(rl: CFRunLoopRef);

    // 定时器(看门狗用)。fireDate 传 0 表示下一个 runloop 周期立即触发一次,interval 为周期(秒)。
    // 注意 context 参数是指向 CFRunLoopTimerContext 结构体的指针,Create 会拷贝其内容,
    // info 字段在回调时原样传回 —— 这里 info 就是 tap 指针。
    // Timer (for the watchdog). fireDate=0 fires on the next runloop pass, interval is the period
    // in seconds. The context argument points to a CFRunLoopTimerContext struct which Create copies;
    // its info field is passed back to the callback -- here info is the tap pointer.
    pub(crate) fn CFRunLoopTimerCreate(
        allocator: CFAllocatorRef,
        fire_date: f64,
        interval: f64,
        flags: u32,
        order: i64,
        callback: CFRunLoopTimerCallBack,
        context: *mut c_void,
    ) -> CFRunLoopTimerRef;
    pub(crate) fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);

    pub(crate) static kCFRunLoopDefaultMode: CFStringRef;
}

// ========== 通用启动流程 / generic start helper ==========

// 缺 Accessibility 权限时,event tap 创建会失败。每隔 RETRY_INTERVAL 重试一次,最多 RETRY_MAX 次
// (约 2 分钟),期间用户可在系统设置里授权;超过上限就记日志放弃。
// 设上限是为了避免无限轮询;用户授权后下次重试即建成,无需重启。
//
// When Accessibility permission is missing, CGEventTapCreate fails. Retry every RETRY_INTERVAL up to
// RETRY_MAX times (~2 min), during which the user can grant permission in System Settings; once the
// limit is exhausted, log and give up. The cap avoids infinite polling; once granted, the next retry
// succeeds - no restart needed.
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const RETRY_MAX: u32 = 40;

/// 创建 event tap 并加入当前线程的 CFRunLoop。失败时按 RETRY_INTERVAL/RETRY_MAX 重试。
/// 返回创建好的 tap(或 None 表示重试耗尽)。
///
/// Create an event tap and add it to the current thread's CFRunLoop. Retries on failure
/// per RETRY_INTERVAL/RETRY_MAX. Returns the created tap (or None if retries exhausted).
///
/// # Safety
/// 调用方必须在专用线程上调用(后续 CFRunLoopRun 会阻塞该线程)。
/// Caller must invoke on a dedicated thread (CFRunLoopRun will block it afterwards).
///
/// `cancel` 为可选取消标志:置位时重试循环提前退出(用于运行时停用鼠标 tap,
/// 避免重试期间 join 阻塞调用线程)。None 表示不取消(如键盘 tap,App 生命周期内常驻)。
///
/// `cancel` is an optional cancellation flag: when set, the retry loop bails out early
/// (used when stopping the mouse tap at runtime so join() doesn't block the caller during
/// the retry window). None means never cancel (e.g. the keyboard tap, resident for the app's
/// lifetime).
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn create_tap_with_retry(
    location: i32,
    placement: i32,
    options: u32,
    mask: CGEventMask,
    callback: CGEventTapCallBack,
    user_info: *mut c_void,
    log_name: &str,
    cancel: Option<&'static std::sync::atomic::AtomicBool>,
) -> Option<CFMachPortRef> {
    let mut tap = CGEventTapCreate(location, placement, options, mask, callback, user_info);

    // 首次创建失败(通常是缺 Accessibility 权限):有限次重试,给用户时间去系统设置授权。
    // First creation failed (usually missing Accessibility): retry a bounded number of times
    // to give the user time to grant permission in System Settings.
    if tap.is_null() {
        log_info!(
            "[{}] No Accessibility permission yet; event tap will retry every {:?} up to {} times (~{}s).",
            log_name,
            RETRY_INTERVAL,
            RETRY_MAX,
            RETRY_INTERVAL.as_secs() * RETRY_MAX as u64
        );
        let mut granted = false;
        for _ in 0..RETRY_MAX {
            std::thread::sleep(RETRY_INTERVAL);
            // 取消请求(运行时停用):立即放弃,线程正常结束。
            // Stop requested (runtime disable): bail out so the thread exits promptly.
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                log_info!("[{}] Event tap cancelled by stop request.", log_name);
                return None;
            }
            if has_accessibility_permission() {
                tap = CGEventTapCreate(location, placement, options, mask, callback, user_info);
                if !tap.is_null() {
                    granted = true;
                    break;
                }
            }
        }
        if granted {
            log_info!(
                "[{}] Accessibility permission granted; event tap created.",
                log_name
            );
        } else {
            log_info!(
                "[{}] Event tap retry exhausted ({}x). Disabled until restart. \
                 Grant Accessibility in System Settings and relaunch.",
                log_name,
                RETRY_MAX
            );
            return None;
        }
    }

    let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
    CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
    CGEventTapEnable(tap, true);
    Some(tap)
}

// ========== tap 看门狗 / tap watchdog ==========

/// CFRunLoopTimerCreate 的 context 结构体(version=0,info 在回调时原样传回)。
/// Context struct for CFRunLoopTimerCreate (version=0; info is passed back to the callback).
#[repr(C)]
struct CFRunLoopTimerContext {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
}

/// 看门狗回调:周期性检查 tap 是否被系统禁用,禁用则重新启用(自愈)。
/// macOS 会对「启动期繁忙/调试器附着时未及时服务事件」的 tap 自动禁用 —— 禁用后
/// tap 线程仍在 runloop 里等待,但事件再也不送达(快捷键静默失效,表现为
/// "Event monitor started" 打过后按键无任何反应)。每 3s 检查一次,发现被禁用就
/// CGEventTapEnable 重新启用并打日志,无论禁用机制如何都能恢复。
///
/// Watchdog callback: periodically checks whether the system disabled the tap and re-enables it.
/// macOS auto-disables taps that fail to service events promptly (busy startup / debugger attach);
/// after that the tap thread keeps waiting in its runloop but events stop arriving (the shortcut
/// silently dies -- "Event monitor started" was logged yet keys do nothing). Checks every 3s and
/// re-enables via CGEventTapEnable when disabled, logging the recovery -- self-healing regardless
/// of what caused the disable.
unsafe extern "C" fn tap_watchdog_callback(_timer: CFRunLoopTimerRef, info: *mut c_void) {
    let tap = info as CFMachPortRef;
    if tap.is_null() {
        return;
    }
    if !CGEventTapIsEnabled(tap) {
        CGEventTapEnable(tap, true);
        log_info!("[tap] event tap was disabled by the system; re-enabled.");
    }
}

/// 在 tap 所在线程挂一个 3s 周期的看门狗定时器(info = tap 指针)。
/// Attach a 3s-period watchdog timer to the tap's thread (info = the tap pointer).
unsafe fn start_tap_watchdog(tap: CFMachPortRef) {
    let ctx = CFRunLoopTimerContext {
        version: 0,
        info: tap,
        retain: None,
        release: None,
        copy_description: None,
    };
    let timer = CFRunLoopTimerCreate(
        std::ptr::null_mut(),
        0.0, // 下一个 runloop 周期立即检查一次 / fire on the next runloop pass
        3.0, // 之后每 3s / then every 3s
        0,
        0,
        Some(tap_watchdog_callback),
        &ctx as *const CFRunLoopTimerContext as *mut c_void,
    );
    if !timer.is_null() {
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopDefaultMode);
    }
}

/// 在专用线程上启动一个 CGEventTap + CFRunLoop。
/// 封装通用的"起线程 -> 建 tap(带重试) -> 加 RunLoop source -> 阻塞"流程。
///
/// Start a CGEventTap + CFRunLoop on a dedicated thread.
/// Wraps the common "spawn thread -> create tap (with retry) -> add runloop source -> block" flow.
///
/// - `location` / `placement` / `options`:见 tap_location / tap_placement / tap_options 模块。
/// - `mask`:要监听的事件类型掩码(1u64 << event_type 的或)。
/// - `callback`:事件回调。
/// - `user_info`:传给 callback 的上下文指针(以 usize 承载以跨线程;0 = 不传)。
///   调用方负责所指对象的生命周期。
/// - `log_name`:日志标识(如 "kbd" / "mouse"),用于区分不同 tap 的日志。
/// - `on_started`:tap 成功创建后、CFRunLoopRun 之前的回调(用于打印 tap 专属的启动日志)。
///
/// 返回 JoinHandle。线程在 CFRunLoopRun 内阻塞,直到 tap 被移除或线程被杀。
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_event_tap_thread(
    location: i32,
    placement: i32,
    options: u32,
    mask: CGEventMask,
    callback: CGEventTapCallBack,
    user_info: usize,
    log_name: &'static str,
    on_started: impl FnOnce() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::spawn(move || unsafe {
        let tap = create_tap_with_retry(
            location,
            placement,
            options,
            mask,
            callback,
            user_info as *mut c_void,
            log_name,
            // 键盘 tap 常驻,不取消(App 退出才停)。
            // Keyboard tap is resident; never cancelled (only app exit stops it).
            None,
        );

        if tap.is_none() {
            return;
        }

        // 看门狗:系统可能在启动期/调试器下禁用 tap,挂定时器定期检查并自愈。
        // Watchdog: the system may disable the tap during busy startup or under a debugger;
        // attach a periodic check that self-heals it.
        start_tap_watchdog(tap.unwrap());
        on_started();
        CFRunLoopRun();
    })
}

// ========== 滚轮事件字段常量 / scroll wheel event field constants ==========
// 见 CGEventTypes.h。反转滚轮需要翻转 4 组字段,覆盖所有类型的消费者。
// See CGEventTypes.h. Scroll reversal flips 4 field groups to cover all consumer types.

/// 垂直滚动量(整数,行级)。field 11。
/// Vertical scroll delta (integer, line-level). field 11.
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1: i32 = 11;
/// 水平滚动量(整数,行级)。field 12。
/// Horizontal scroll delta (integer, line-level). field 12.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2: i32 = 12;

/// 垂直滚动量(定点浮点,16.16 格式)。field 93。
/// Vertical scroll delta (fixed-point, 16.16 format). field 93.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1: i32 = 93;
/// 水平滚动量(定点浮点,16.16 格式)。field 94。
/// Horizontal scroll delta (fixed-point, 16.16 format). field 94.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2: i32 = 94;

/// 垂直滚动量(像素级)。field 96。
/// Vertical scroll delta (pixel-level). field 96.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1: i32 = 96;
/// 水平滚动量(像素级)。field 97。
/// Horizontal scroll delta (pixel-level). field 97.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2: i32 = 97;

/// 是否为连续(像素级)滚动事件。field 88。0=离散(行级),1=连续(触控板式)。
/// Whether the event is continuous (pixel-level) scroll. field 88. 0=discrete (line), 1=continuous (trackpad).
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS: i32 = 88;

// IOHIDEvent 层的滚轮字段(私有 API)。kIOHIDEventTypeScroll=6,字段 = (type<<16)|offset。
// X(offset 0)= 393216, Y(offset 1)= 393217。
// IOHIDEvent-level scroll fields (private API). kIOHIDEventTypeScroll=6, field = (type<<16)|offset.
/// IOHIDEvent 垂直滚动字段。
/// IOHIDEvent vertical scroll field.
#[allow(dead_code)]
pub(crate) const K_IOHID_EVENT_FIELD_SCROLL_X: u32 = 6 << 16;
/// IOHIDEvent 水平滚动字段。
/// IOHIDEvent horizontal scroll field.
#[allow(dead_code)]
pub(crate) const K_IOHID_EVENT_FIELD_SCROLL_Y: u32 = (6 << 16) | 1;

// ========== 事件合成相关常量 / synthetic event constants ==========

/// CGEventPost 的 tap location:kCGSessionEventTap=1。
/// 合成事件 post 到 session 层,不经过 HID 层 tap,绕过系统自然滚动覆盖。
/// CGEventPost tap location: kCGSessionEventTap=1.
/// Synthetic events posted at session level bypass HID-level taps, avoiding the system's
/// natural-scroll override at the HID layer.
pub(crate) const K_CG_SESSION_EVENT_TAP: i32 = 1;

/// CGEventCreateScrollWheelEvent2 的 units:kCGScrollEventUnitLine=1(行级,离散滚动)。
/// CGEventCreateScrollWheelEvent2 units: kCGScrollEventUnitLine=1 (line-level, discrete scroll).
pub(crate) const K_CG_SCROLL_EVENT_UNIT_LINE: u32 = 1;

/// eventSourceUserData 字段(field 42)。用于在合成事件上打标记,防止自己的 tap 无限循环。
/// eventSourceUserData field (field 42). Used to tag synthetic events so our own tap can
/// recognize and skip them, preventing infinite loops.
pub(crate) const K_CG_EVENT_SOURCE_USER_DATA: i32 = 42;

/// 合成事件标记魔数(ASCII "OMTSCRL")。写入 eventSourceUserData,我们的 tap 据此跳过。
/// Synthetic-event marker magic (ASCII "OMTSCRL"). Written to eventSourceUserData so our tap
/// can recognize and skip our own synthetic events.
#[allow(clippy::unusual_byte_groupings)]
pub(crate) const SYNTHETIC_MARKER: i64 = 0x4F4D_5453_4352_4C;
