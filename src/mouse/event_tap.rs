//! 鼠标事件 event tap。
//! 在独立线程上建一个 default-tap 的 CGEventTap(HID 层),监听鼠标按键与滚轮事件。
//! 滚轮反转采用"合成事件"方案:丢弃原始事件,合成反转后的新事件 post 到 session 层,
//! 绕过系统自然滚动在 HID 层的覆盖。此合成管线未来可被平滑滚动复用。
//!
//! Mouse event tap.
//! Spawns a default-tap CGEventTap (HID level) on a dedicated thread that listens for mouse button
//! and scroll events. Scroll reversal uses a "synthetic event" approach: drop the original event,
//! synthesize a new reversed event and post it to the session level, bypassing the system's
//! natural-scroll override at the HID layer. This synthetic pipeline is reused by future smoothed scrolling.

use crate::config::CONFIG;
use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CGEventCreateScrollWheelEvent2, CGEventFlags,
    CGEventGetFlags, CGEventGetIntegerValueField, CGEventMask, CGEventPost, CGEventRef,
    CGEventSetFlags, CGEventSetIntegerValueField, CGEventTapProxy, CGEventType,
    K_CG_EVENT_SOURCE_USER_DATA, K_CG_SCROLL_EVENT_UNIT_LINE, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS,
    K_CG_SESSION_EVENT_TAP, SYNTHETIC_MARKER,
};
use crate::{log_info, log_warn};
use std::ffi::c_void;

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
/// 注意:部分游戏鼠标(如 MCHOSE G3 V2)固件上报侧键用厂商自定义高编号
/// (如 152/153),不遵循 HID 标准 3/4 -- 这类值同样可作识别依据,只是不直观。
///
/// Translate buttonNumber (0-based) to a readable name for log readability.
/// 0=left, 1=right, 2=middle; 3/4=standard HID back/forward side buttons; others use the raw number.
/// Note: some gaming mice (e.g. MCHOSE G3 V2) report side buttons with vendor-specific high numbers
/// (e.g. 152/153) instead of the HID-standard 3/4 -- these values are equally usable for identification,
/// just less intuitive.
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

/// 合成一个反转方向的滚轮事件并 post 到 session 层。
/// 原始事件将被丢弃(callback 返回 null),由合成事件替代。
///
/// 未来平滑滚动引擎将复用此管线:把 delta 来源从"直接取反"换成"引擎计算"即可。
///
/// Synthesize a reversed scroll event and post it to the session level.
/// The original event is dropped (callback returns null), replaced by the synthetic one.
///
/// The future smoothed-scrolling engine will reuse this pipeline: swap the delta source from
/// "direct negation" to "engine output".
unsafe fn post_reversed_scroll(original: CGEventRef, delta_y: i64, delta_x: i64) {
    // 合成全新事件(行级单位,wheel1=垂直,wheel2=水平)。delta 取反实现反转。
    // Create a brand-new event (line units, wheel1=vertical, wheel2=horizontal). Negate deltas to reverse.
    let synthetic = CGEventCreateScrollWheelEvent2(
        std::ptr::null(),
        K_CG_SCROLL_EVENT_UNIT_LINE,
        2,
        (-delta_y) as i32,
        (-delta_x) as i32,
        0,
    );

    if synthetic.is_null() {
        log_warn!("[mouse] failed to synthesize reversed scroll event");
        return;
    }

    // 保留原始修饰键,使 Shift/Cmd 等修饰功能正常工作。
    // Preserve original modifier flags so Shift/Cmd etc. keep working.
    let flags: CGEventFlags = CGEventGetFlags(original);
    CGEventSetFlags(synthetic, flags);

    // 打上合成标记,防止我们的 HID tap 拦截到 post 的事件后再次处理(无限循环)。
    // 注意:post 到 session 层的事件不经过 HID 层 tap,但加标记是防御性措施。
    // Tag with synthetic marker to prevent our HID tap from re-processing the posted event (infinite
    // loop). Note: session-posted events don't pass through HID-level taps, but this is defensive.
    CGEventSetIntegerValueField(synthetic, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);

    // post 到 session 层 -- 不经过 HID 层,绕过系统自然滚动的 HID 层覆盖。
    // Post to session level -- bypasses HID layer, avoiding the system's natural-scroll override.
    CGEventPost(K_CG_SESSION_EVENT_TAP, synthetic);
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
        // 我们的 natural_scroll 只控制鼠标滚轮(离散事件),触摸板交给系统自然滚动处理,不干预。
        // continuous=1 means a continuous (pixel-level) scroll event from a trackpad / Magic Mouse.
        // Our natural_scroll controls only mouse wheel (discrete events); trackpad scrolling is left
        // to the system's natural scrolling, untouched.
        let continuous = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
        if continuous != 0 {
            return event;
        }

        // deltaAxis1 = 垂直滚动量,deltaAxis2 = 水平滚动量。
        // deltaAxis1 = vertical delta, deltaAxis2 = horizontal delta.
        let dy = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
        let dx = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);

        // 读配置:reverse_scroll 默认 false(跟随系统行为,不反转)。
        // Read config: reverse_scroll defaults to false (follow system, no reversal).
        let reverse = CONFIG
            .read()
            .map(|cfg| cfg.mouse.reverse_scroll)
            .unwrap_or(false);

        if reverse {
            // 合成事件方案:丢弃原事件(返回 null),合成反转事件 post 到 session 层。
            // Synthetic-event approach: drop original (return null), post reversed event to session level.
            post_reversed_scroll(event, dy, dx);
            log_info!("[mouse] scroll dy={} dx={} flags=0x{:x} (reversed, synthetic)", dy, dx, flags);
            // 返回 null 丢弃原始事件,由合成事件替代。
            // Return null to drop the original; the synthetic event replaces it.
            std::ptr::null_mut()
        } else {
            log_info!("[mouse] scroll dy={} dx={} flags=0x{:x}", dy, dx, flags);
            event
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

/// 启动鼠标事件监听线程。default-tap(HID 层),可修改事件。
/// Start the mouse event listener thread. default-tap (HID level), can modify events.
pub(crate) fn start() -> std::thread::JoinHandle<()> {
    // 监听掩码:左/右/其他按键 down/up + 滚轮。暂不含 mouseMoved(日志会爆炸)。
    // Listen mask: left/right/other button down/up + scroll wheel. Excludes mouseMoved (would flood logs).
    let mask: CGEventMask = (1u64 << K_CG_EVENT_LEFT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_LEFT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_UP)
        | (1u64 << K_CG_EVENT_SCROLL_WHEEL);

    // HID 层 + default-tap:可修改/丢弃事件(滚轮反转需要丢弃原事件)。
    // HID level + default-tap: can modify/drop events (scroll reversal needs to drop the original).
    event_tap::start_event_tap_thread(
        tap_location::HID_EVENT_TAP,
        tap_placement::HEAD_INSERT,
        tap_options::DEFAULT_TAP,
        mask,
        Some(mouse_event_tap_callback),
        0,
        "mouse",
        || {
            log_info!("Mouse event tap started (default-tap).");
        },
    )
}
