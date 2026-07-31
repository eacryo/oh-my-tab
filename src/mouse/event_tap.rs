//! 鼠标事件 event tap。
//! 在独立线程上建一个 default-tap 的 CGEventTap(HID 层),监听鼠标按键与滚轮事件。
//! 滚轮事件根据配置分三种模式处理:默认(透传+反转)、按行(固定行数)、平滑(物理状态机+惯性)。
//! 三种模式统一使用合成事件方案:丢弃原始事件,合成新事件 post 到 session 层,
//! 绕过系统自然滚动在 HID 层的覆盖。
//!
//! Mouse event tap.
//! Spawns a default-tap CGEventTap (HID level) on a dedicated thread that listens for mouse
//! button and scroll events. Scroll events are processed in one of three modes: Default
//! (passthrough + optional reverse), Line (fixed line count), or Smooth (physics engine +
//! inertia). All three modes use the synthetic-event approach: drop the original, post a new
//! event to the session level, bypassing the system natural-scroll override at the HID layer.

use crate::config::CONFIG;
use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CFAbsoluteTimeGetCurrent, CFIndex,
    CFOptionFlags, CFRunLoopAddTimer, CFRunLoopGetCurrent, CFRunLoopTimerCreate, CFRunLoopTimerRef,
    CGEventCreateScrollWheelEvent2, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventMask, CGEventPost, CGEventRef, CGEventSetFlags, CGEventSetIntegerValueField,
    CGEventTapProxy, CGEventType, K_CG_EVENT_SOURCE_USER_DATA, K_CG_SCROLL_EVENT_UNIT_LINE,
    K_CG_SCROLL_EVENT_UNIT_PIXEL, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS,
    K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE, K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE,
    K_CG_SESSION_EVENT_TAP, SYNTHETIC_MARKER,
};
use crate::mouse::scrolling::{
    advance_engine, compute_delta, feed_engine, init_engine, ScrollMode,
};
use crate::{log_info, log_warn};
use std::ffi::c_void;
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

// mouseEventButtonNumber 字段(field 0),用于日志识别按键编号。
// mouseEventButtonNumber field (field 0), for log button-number identification.
const K_CG_MOUSE_EVENT_BUTTON_NUMBER: i32 = 0;

/// 合成一个滚轮事件并 post 到 session 层。
/// 三种模式统一走此函数:dy/dx 由各模式按需计算后传入。
///
/// Synthesize a scroll event and post it to the session level.
/// All three modes share this function: dy/dx are pre-computed per mode.
unsafe fn post_scroll_event(
    dy: i32,
    dx: i32,
    flags: CGEventFlags,
    continuous: bool,
    scroll_phase: i64,
    momentum_phase: i64,
) {
    let units = if continuous {
        K_CG_SCROLL_EVENT_UNIT_PIXEL
    } else {
        K_CG_SCROLL_EVENT_UNIT_LINE
    };
    let synthetic = CGEventCreateScrollWheelEvent2(std::ptr::null(), units, 2, dy, dx, 0);

    if synthetic.is_null() {
        log_warn!("[mouse] failed to synthesize scroll event");
        return;
    }

    CGEventSetFlags(synthetic, flags);
    CGEventSetIntegerValueField(synthetic, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventSetIntegerValueField(
        synthetic,
        K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS,
        continuous as i64,
    );
    if scroll_phase != 0 {
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_SCROLL_PHASE,
            scroll_phase,
        );
    }
    if momentum_phase != 0 {
        CGEventSetIntegerValueField(
            synthetic,
            K_CG_SCROLL_WHEEL_EVENT_MOMENTUM_PHASE,
            momentum_phase,
        );
    }

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

/// 120Hz 定时器回调:推进平滑引擎一次,若有发射值则 post 合成事件。
/// 120Hz timer callback: advance the smooth engine once; post synthetic event if there is an
/// emission.
unsafe extern "C" fn smooth_scroll_timer_callback(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    if let Some(emission) = advance_engine() {
        let flags: CGEventFlags = 0;
        // 将浮点 delta 转 i32(像素级)。用四舍五入避免微小值累积为 0。
        // Convert float delta to i32 (pixel level). Round to avoid near-zero accumulation.
        let dy = (emission.delta_y.round()) as i32;
        let dx = (emission.delta_x.round()) as i32;
        if dy == 0 && dx == 0 && emission.momentum_phase == 0 && emission.scroll_phase == 0 {
            return;
        }
        post_scroll_event(
            dy,
            dx,
            flags,
            true,
            emission.scroll_phase as i64,
            emission.momentum_phase as i64,
        );
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
        // 我们的三种模式只处理鼠标滚轮(离散事件),触摸板跳过。
        // continuous=1 means a continuous (pixel-level) scroll event from a trackpad / Magic Mouse.
        // All three modes handle only discrete mouse wheel events; trackpad is skipped.
        let continuous = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
        if continuous != 0 {
            return event;
        }

        // deltaAxis1 = 垂直滚动量,deltaAxis2 = 水平滚动量。
        // deltaAxis1 = vertical delta, deltaAxis2 = horizontal delta.
        let dy = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
        let dx = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);

        log_info!("[mouse] scroll dy={} dx={} flags=0x{:x}", dy, dx, flags);

        let mode = ScrollMode::current();
        match mode {
            ScrollMode::Smooth => {
                // 喂入平滑引擎,引擎在 120Hz 定时器内发射连续事件。
                // Feed the smooth engine; the 120Hz timer will emit continuous events.
                feed_engine(dy as f64, dx as f64);
                // 丢弃原始离散事件(由引擎的合成连续事件替代)。
                // Drop the original discrete event (replaced by the engine's synthetic continuous events).
                std::ptr::null_mut()
            }
            _ => {
                // Default / Line:计算 delta + 反转 → post 合成事件 → 丢弃原 event。
                // Default / Line: compute delta + reverse → post synthetic event → drop original.
                let (ndy, ndx) = compute_delta(dy, dx);
                post_scroll_event(ndy, ndx, flags, false, 0, 0);
                std::ptr::null_mut()
            }
        }
    } else {
        let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
        log_info!(
            "[mouse] {} button={}({}) flags=0x{:x}",
            event_type_name(event_type),
            button,
            button_name(button),
            flags
        );
        event
    }
}

/// 启动鼠标事件监听线程 + 120Hz 平滑定时器。由 main.rs 在启用鼠标时调用。
/// Start the mouse event listener thread + 120Hz smooth timer. Called by main.rs when mouse
/// control is enabled.
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
        // 初始化平滑引擎(确保 SMOOTH_ENGINE 已创建,即使当前不是 smooth 模式)。
        // Initialize the smooth engine so it is ready even if the current mode isn't Smooth.
        init_engine(
            &CONFIG
                .read()
                .map(|c| c.mouse.smooth_preset.clone())
                .unwrap_or_else(|_| "easeInOut".into()),
        );

        // 创建 event tap(HID 层,可修改/丢弃事件)。
        // Create event tap (HID level, mutable).
        let tap = event_tap::create_tap_with_retry(
            tap_location::HID_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(mouse_event_tap_callback),
            std::ptr::null_mut(),
            "mouse",
        );

        let tap = match tap {
            Some(t) => t,
            None => return,
        };

        let rl = CFRunLoopGetCurrent();
        let source = event_tap::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        event_tap::CFRunLoopAddSource(rl, source, event_tap::kCFRunLoopDefaultMode);
        event_tap::CGEventTapEnable(tap, true);

        // 创建 120Hz 定时器(约 8.3ms),驱动平滑引擎 advance。
        // Create 120Hz timer (~8.3ms) to drive the smooth engine advance.
        let timer = CFRunLoopTimerCreate(
            std::ptr::null_mut(),                     // allocator
            CFAbsoluteTimeGetCurrent() + 1.0 / 120.0, // fire date: ~8ms from now
            1.0 / 120.0,                              // interval: ~8.3ms
            0 as CFOptionFlags,                       // flags
            0 as CFIndex,                             // order
            Some(
                smooth_scroll_timer_callback
                    as unsafe extern "C" fn(CFRunLoopTimerRef, *mut c_void),
            ),
            std::ptr::null_mut(),
        );
        CFRunLoopAddTimer(rl, timer, event_tap::kCFRunLoopDefaultMode);

        log_info!("Mouse event tap + 120Hz smooth timer started.");

        // 阻塞运行 RunLoop,直到线程被终止。
        // Block on the RunLoop until the thread is terminated.
        event_tap::CFRunLoopRun();
    })
}
