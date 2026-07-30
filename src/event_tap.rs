//! CGEventTap 公共基础设施:类型别名、FFI extern 声明、语义常量、通用启动流程。
//! 被窗口切换(event_monitor)与鼠标增强(mouse::event_tap)两个模块共用,是叶子层。
//!
//! Common CGEventTap infrastructure: type aliases, FFI extern declarations, semantic
//! constants, and a generic start helper. Shared by the window switcher (event_monitor)
//! and the mouse enhancement (mouse::event_tap) modules. A leaf module.

use crate::ffi::has_accessibility_permission;
use crate::{log_error, log_info, log_warn};
use std::ffi::c_void;
use std::thread;
use std::time::Duration;

// ========== 类型别名 / type aliases ==========

pub(crate) type CGEventRef = *mut c_void;
pub(crate) type CGEventTapProxy = *mut c_void;
pub(crate) type CFMachPortRef = *mut c_void;
pub(crate) type CFRunLoopSourceRef = *mut c_void;
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
    pub(crate) fn CGEventGetIntegerValueField(event: CGEventRef, field: i32) -> i64;
    pub(crate) fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
}

// CFRunLoop 相关函数链接 CoreFoundation(与 event_monitor 原声明一致)。
// CFRunLoop functions link against CoreFoundation (matching event_monitor's original declaration).
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
unsafe fn create_tap_with_retry(
    location: i32,
    placement: i32,
    options: u32,
    mask: CGEventMask,
    callback: CGEventTapCallBack,
    user_info: *mut c_void,
    log_name: &str,
) -> Option<CFMachPortRef> {
    let mut tap = CGEventTapCreate(location, placement, options, mask, callback, user_info);

    // 首次创建失败(通常是缺 Accessibility 权限):有限次重试,给用户时间去系统设置授权。
    // First creation failed (usually missing Accessibility): retry a bounded number of times
    // to give the user time to grant permission in System Settings.
    if tap.is_null() {
        log_warn!(
            "[{}] No Accessibility permission yet; event tap will retry every {:?} up to {} times (~{}s).",
            log_name,
            RETRY_INTERVAL,
            RETRY_MAX,
            RETRY_INTERVAL.as_secs() * RETRY_MAX as u64
        );
        let mut granted = false;
        for _ in 0..RETRY_MAX {
            std::thread::sleep(RETRY_INTERVAL);
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
            log_error!(
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
        );

        if tap.is_none() {
            return;
        }

        on_started();
        CFRunLoopRun();
    })
}
