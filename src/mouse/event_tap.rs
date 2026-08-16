//! 鼠标事件 event tap。
//! 在独立线程上建一个 default-tap 的 CGEventTap(HID 层),监听鼠标按键与滚轮事件。
//! 滚轮事件根据配置分两种模式处理:默认(透传+反转)、按行(固定行数)。
//! 两种模式统一使用合成事件方案:丢弃原始事件,合成新事件 post 到 session 层,
//! 绕过系统自然滚动在 HID 层的覆盖。
//!
//! Mouse event tap.
//! Spawns a default-tap CGEventTap (HID level) on a dedicated thread that listens for mouse
//! button and scroll events. Scroll events are processed in one of two modes: Default
//! (passthrough + optional reverse) or Line (fixed line count). Both use the synthetic-event
//! approach: drop the original, post a new event to the session level, bypassing the system
//! natural-scroll override at the HID layer.

use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent,
    CGEventCreateScrollWheelEvent2, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventMask, CGEventPost, CGEventRef, CGEventSetFlags, CGEventSetIntegerValueField,
    CGEventTapProxy, CGEventType, K_CG_EVENT_SOURCE_USER_DATA, K_CG_SCROLL_EVENT_UNIT_LINE,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, K_CG_SESSION_EVENT_TAP, SYNTHETIC_MARKER,
};
use crate::mouse::device;
use crate::mouse::keysim;
use crate::mouse::resolve;
use crate::mouse::scrolling::compute_delta;
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

// ========== 鼠标事件类型常量 / mouse event type constants ==========
// 见 CGEventType.h。
// See CGEventType.h.
const K_CG_EVENT_LEFT_MOUSE_DOWN: CGEventType = 1;
const K_CG_EVENT_LEFT_MOUSE_UP: CGEventType = 2;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: CGEventType = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: CGEventType = 4;
const K_CG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;
const K_CG_EVENT_SCROLL_WHEEL: CGEventType = 22;

// mouseEventButtonNumber 字段(field 3,不是 0 —— 0 是 kCGMouseEventNumber 事件编号,
// 用它拿到的是一路递增的事件号而非真实按钮号,导致录制/匹配全错)。
// mouseEventButtonNumber field (field 3, NOT 0 -- 0 is kCGMouseEventNumber, an ever-increasing
// event counter, so reading it yields the counter instead of the real button number, breaking
// recording and matching).
const K_CG_MOUSE_EVENT_BUTTON_NUMBER: i32 = 3;

/// 按键映射录制中标志:设置界面录制按钮/组合键期间置位,tap 跳过映射执行,
/// 避免录制时误触发绑定(LinearMouse 的 SettingsState.shared.recording 同款)。
/// Recording-in-progress flag: set while the settings UI records a button/combo; the tap
/// skips mapping execution so recording never fires a binding (same as LinearMouse's
/// SettingsState.shared.recording).
pub(crate) static RECORDING: AtomicBool = AtomicBool::new(false);

/// 合成一个滚轮事件并 post 到 session 层。
/// 行模式按"行"单位 post;默认模式透传原 delta。
///
/// Synthesize a scroll event and post it to the session level.
/// Line mode posts in "line" units; Default mode passes the raw delta through.
unsafe fn post_scroll_event(dy: i32, dx: i32, flags: CGEventFlags) {
    let synthetic =
        CGEventCreateScrollWheelEvent2(std::ptr::null(), K_CG_SCROLL_EVENT_UNIT_LINE, 2, dy, dx, 0);

    if synthetic.is_null() {
        log_info!("[mouse] failed to synthesize scroll event");
        return;
    }

    CGEventSetFlags(synthetic, flags);
    CGEventSetIntegerValueField(synthetic, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);

    CGEventPost(K_CG_SESSION_EVENT_TAP, synthetic);
}

/// 把事件类型翻译成可读名称,便于日志阅读。
/// Translate an event type into a readable name for log readability.
fn event_type_name(t: CGEventType) -> &'static str {
    match t {
        K_CG_EVENT_LEFT_MOUSE_DOWN => "left down",
        K_CG_EVENT_LEFT_MOUSE_UP => "left up",
        K_CG_EVENT_RIGHT_MOUSE_DOWN => "right down",
        K_CG_EVENT_RIGHT_MOUSE_UP => "right up",
        K_CG_EVENT_OTHER_MOUSE_DOWN => "other down",
        K_CG_EVENT_OTHER_MOUSE_UP => "other up",
        K_CG_EVENT_SCROLL_WHEEL => "scroll",
        _ => "other",
    }
}

/// 把 buttonNumber(0-based)翻译成可读名称,便于日志阅读。
/// 0=左,1=右,2=中;3/4=标准 HID 后退/前进侧键;其余用裸数字。
/// Translate buttonNumber (0-based) to a readable name for log readability.
fn button_name(button: i64) -> String {
    match button {
        0 => "left".to_string(),
        1 => "right".to_string(),
        2 => "middle".to_string(),
        3 => "back".to_string(),
        4 => "forward".to_string(),
        n => format!("btn{}", n),
    }
}

unsafe extern "C" fn mouse_event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    let flags: CGEventFlags = CGEventGetFlags(event);

    if event_type == K_CG_EVENT_SCROLL_WHEEL {
        // 跳过自己合成的滚轮事件(防御性,post 到 session 层的事件理论上不经过 HID tap)。
        // Skip our own synthetic scroll events (defensive; session-posted events shouldn't reach HID tap).
        let user_data = CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA);
        if user_data == SYNTHETIC_MARKER {
            return event;
        }

        // continuous=1 表示连续(像素级)滚动事件,来自触摸板/Magic Mouse。
        // 我们的两种模式只处理鼠标滚轮(离散事件),触摸板跳过。
        // continuous=1 means a continuous (pixel-level) scroll event from a trackpad / Magic Mouse.
        // Both modes handle only discrete mouse wheel events; trackpad is skipped.
        let continuous = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
        if continuous != 0 {
            return event;
        }

        // deltaAxis1 = 垂直滚动量,deltaAxis2 = 水平滚动量。
        // deltaAxis1 = vertical delta, deltaAxis2 = horizontal delta.
        let dy = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
        let dx = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);

        // 归因:CGEvent -> 产生事件的设备 -> (VID,PID)。失败返回 None(回退"所有鼠标"档)。
        // Attribution: CGEvent -> producing device -> (VID, PID). None on failure (falls back to
        // the "All Mice" profile).
        let dev_key = device::device_from_cgevent(event);
        // 解析该设备的生效配置(合并"所有鼠标"档 + per-device 档)。
        // Resolve the effective config for this device (merging "All Mice" + per-device profiles).
        let resolved = resolve::resolve(dev_key);

        // 归因诊断放最前,与普通滚动日志区分开,便于排查 per-device 设置不生效。
        // Attribution diagnostics lead the line, distinguishing it from plain scroll logs so
        // per-device settings not applying is easy to spot.
        log_debug!(
            "[mouse] scroll dev={:?} dy={} dx={} flags=0x{:x} reverse={}",
            dev_key,
            dy,
            dx,
            flags,
            resolved.reverse_scroll
        );

        // Default / Line:计算 delta(透传或行数归一化 + 反转)-> post 合成事件 -> 丢弃原 event。
        // Default / Line: compute delta (passthrough or line-count normalization + reverse) ->
        // post synthetic event -> drop the original.
        let (ndy, ndx) = compute_delta(dy, dx, &resolved);
        post_scroll_event(ndy, ndx, flags);
        std::ptr::null_mut()
    } else {
        let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
        log_debug!(
            "[mouse] {} button={}({}) flags=0x{:x}",
            event_type_name(event_type),
            button,
            button_name(button),
            flags
        );
        // 按键映射:仅中键及侧键(button >= 2)参与;左键(0)/右键(1)永不绑定,
        // 防止用户把自己锁死(无法点击)。录制期间跳过执行。
        // Button mappings: only middle/side buttons (>= 2) take part; left (0)/right (1)
        // are never bound so the user can't lock themselves out of clicking. Skipped while
        // recording.
        if button >= 2 && !RECORDING.load(Ordering::Relaxed) {
            let dev_key = device::device_from_cgevent(event);
            let resolved = resolve::resolve(dev_key);
            if let Some(desc) = resolved.button_mappings.get(&button.to_string()) {
                // 按绑定类型分发:快捷键 -> 合成键盘事件;系统动作 -> Dock 私有通知;
                // none -> 吞事件不动作。两端都吞原始事件(应用看不到侧键点击)。
                // Dispatch by binding type: shortcut -> synthesized keys; system action ->
                // Dock private notification; none -> swallow without action. Both directions
                // swallow the original event (the app never sees the raw side-button click).
                match crate::mouse::shortcut::parse_binding(desc) {
                    Ok(crate::mouse::shortcut::Binding::Key(sc)) => match event_type {
                        K_CG_EVENT_OTHER_MOUSE_DOWN => {
                            keysim::press_down(sc.keycode, sc.flags, desc);
                            return std::ptr::null_mut();
                        }
                        K_CG_EVENT_OTHER_MOUSE_UP => {
                            keysim::release_up(sc.keycode, sc.flags, desc);
                            return std::ptr::null_mut();
                        }
                        _ => {}
                    },
                    Ok(crate::mouse::shortcut::Binding::System(notif)) => {
                        // 系统动作按下时触发一次(Dock 通知是 toggle 语义);释放只吞。
                        // System actions fire once on press (Dock notifications toggle);
                        // the release is only swallowed.
                        if event_type == K_CG_EVENT_OTHER_MOUSE_DOWN {
                            crate::mouse::system_action::fire(notif);
                        }
                        return std::ptr::null_mut();
                    }
                    Ok(crate::mouse::shortcut::Binding::None) => {
                        // 显式禁用:吞事件,无动作。
                        // Explicit none: swallow, no action.
                        return std::ptr::null_mut();
                    }
                    Err(_) => {
                        // 绑定存在但解析失败(配置被手改坏):提示。
                        // Mapping exists but failed to parse (hand-edited config): note it.
                        log_info!("[mouse] button {}: unparseable binding {:?}", button, desc);
                    }
                }
            } else {
                // 诊断:未命中映射时打印归因与当前设备的映射键,定位"绑定不生效"。
                // Diagnostic: on a miss, print the attribution and the device's mapping keys
                // to pinpoint "binding not working".
                log_debug!(
                    "[mouse] button {}: no mapping (dev={:?}, keys={:?})",
                    button,
                    dev_key,
                    resolved.button_mappings.keys().collect::<Vec<_>>()
                );
            }
        }
        event
    }
}

/// 鼠标事件线程的 RunLoop 引用,供 stop() 调 CFRunLoopStop。
/// 由 start() 的线程在进入 CFRunLoopRun 前存入、结束后清空。
/// 用 Send+Sync 包装的 Mutex(static 需要 Send+Sync,与 device.rs 的 ManagerMutex 同模式)。
///
/// The mouse thread's RunLoop reference, for stop() to call CFRunLoopStop on.
/// Stored by the start() thread before CFRunLoopRun and cleared when it exits.
/// Wrapped Mutex with Send+Sync (same pattern as device.rs's ManagerMutex; statics need it).
struct RunLoopMutex(Mutex<Option<event_tap::CFRunLoopRef>>);
unsafe impl Send for RunLoopMutex {}
unsafe impl Sync for RunLoopMutex {}

static RUNLOOP: OnceLock<RunLoopMutex> = OnceLock::new();

fn runloop_static() -> &'static Mutex<Option<event_tap::CFRunLoopRef>> {
    &RUNLOOP.get_or_init(|| RunLoopMutex(Mutex::new(None))).0
}

/// 运行时停止请求标志:stop() 置位后,重试循环(thread::sleep 阻塞期)醒来即放弃,
/// 避免 stop() 的 join() 在缺权限重试窗口期间阻塞调用线程。
///
/// Runtime stop request flag: once set by stop(), the retry loop (blocked in thread::sleep)
/// bails out on wake-up, so stop()'s join() never blocks the caller during the missing-
/// permission retry window.
static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 运行时停止鼠标事件线程:置位取消标志 + CFRunLoopStop,线程自然结束。
/// 幂等:未运行时无操作。由 settings.rs 在取消"启用鼠标控制"时调用。
///
/// Stop the mouse event thread at runtime: set the cancel flag + CFRunLoopStop, the thread
/// exits naturally. Idempotent: no-op when not running. Called by settings.rs when the
/// "Enable mouse control" switch is turned off.
pub(crate) fn stop() {
    // 先置位标志再停 RunLoop:重试窗口内线程醒来即可见(不依赖 RunLoop)。
    // Set the flag first, then stop the RunLoop: during the retry window the thread sees the
    // flag on wake-up (no RunLoop involved yet).
    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
    let rl = runloop_static().lock().unwrap().take();
    if let Some(rl) = rl {
        unsafe {
            event_tap::CFRunLoopStop(rl);
        }
    }
}

/// 启动鼠标事件监听线程。由 main.rs / settings.rs 在启用鼠标时调用。
/// Start the mouse event listener thread. Called by main.rs / settings.rs when mouse control
/// is enabled.
pub(crate) fn start() -> thread::JoinHandle<()> {
    // 监听掩码:左/右/其他按键 down/up + 滚轮。暂不含 mouseMoved(日志会爆炸)。
    // Listen mask: left/right/other button down/up + scroll wheel. Excludes mouseMoved.
    let mask: CGEventMask = (1u64 << K_CG_EVENT_LEFT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_LEFT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_UP)
        | (1u64 << K_CG_EVENT_SCROLL_WHEEL);

    thread::spawn(move || unsafe {
        // 新线程首件事:清掉上次运行可能残留的停止标志。
        // First thing in the new thread: clear any stale stop flag from a previous run.
        STOP_REQUESTED.store(false, std::sync::atomic::Ordering::Relaxed);

        // 启动时枚举一次已连接设备(惰性:归因失败时也会重枚举)。
        // Enumerate connected devices once at startup (also lazily re-done on attribution failure).
        device::ensure_enumerated();
        // 创建 event tap(HID 层,可修改/丢弃事件)。传取消标志:停止请求(缺权限重试期)提前退出。
        // Create event tap (HID level, mutable). Pass the cancel flag: bails out early on a
        // stop request (even during the missing-permission retry window).
        let tap = event_tap::create_tap_with_retry(
            tap_location::HID_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(mouse_event_tap_callback),
            std::ptr::null_mut(),
            "mouse",
            Some(&STOP_REQUESTED),
        );

        let tap = match tap {
            Some(t) => t,
            None => return,
        };

        let rl = CFRunLoopGetCurrent();
        let source = event_tap::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        event_tap::CFRunLoopAddSource(rl, source, event_tap::kCFRunLoopDefaultMode);
        event_tap::CGEventTapEnable(tap, true);

        // 设备插拔监听:蓝牙断连重连时事件驱动地重建注册表(避免旧 client 缓存过期
        // 导致归因链失效,滚动方向/档位错乱)。回调与 event tap 同线程,安全。
        // Device plug/unplug monitor: event-driven registry rebuild on Bluetooth disconnect/
        // reconnect (avoids the stale-client attribution failure that breaks scroll direction
        // / profile matching). Callbacks run on this same thread, safe.
        device::start_plug_monitor(rl);

        // 存入 RunLoop 供 stop() 使用;存入后再查一次停止标志,消除"存入后、run 前置位
        // 标志"的竞态窗口(stop() 读 RUNLOOP 时要么读到 Some 而 CFRunLoopStop 有效,
        // 要么读到 None 而线程的检查会拦截)。
        // Store the RunLoop for stop(); re-check the stop flag after storing to close the race
        // where the flag is set between the store and the run (stop() either reads Some and
        // CFRunLoopStop works, or reads None and the thread's check catches it).
        *runloop_static().lock().unwrap() = Some(rl);
        if !STOP_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
            log_info!("Mouse event tap started.");
            // 阻塞运行 RunLoop,直到 stop() 触发 CFRunLoopStop 或线程被终止。
            // Block on the RunLoop until stop() fires CFRunLoopStop or the thread is killed.
            event_tap::CFRunLoopRun();
        }
        *runloop_static().lock().unwrap() = None;
    })
}
