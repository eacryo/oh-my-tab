//! 鼠标事件 event tap(最小验证版)。
//! 在独立线程上建一个 listen-only 的 CGEventTap(HID 层),监听鼠标按键与滚轮事件,
//! 仅在日志中输出事件信息,不修改任何事件。验证链路打通后再叠加 transformer。
//!
//! Mouse event tap (minimal verification).
//! Spawns a listen-only CGEventTap (HID level) on a dedicated thread that listens for mouse button
//! and scroll events and logs them without modification. Once the pipeline is validated, transformers
//! will be layered on top.

use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CGEventFlags, CGEventGetFlags,
    CGEventGetIntegerValueField, CGEventMask, CGEventRef, CGEventTapProxy, CGEventType,
};
use crate::log_info;
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

// ========== CGEvent 字段常量 / CGEvent field constants ==========
// 见 CGEventField.h。mouseEventButtonNumber=0,deltaX/Y=4/5,scroll deltaAxis1/2=11/12。
// See CGEventField.h. mouseEventButtonNumber=0, deltaX/Y=4/5, scroll deltaAxis1/2=11/12.
const K_CG_MOUSE_EVENT_BUTTON_NUMBER: i32 = 0;
const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1: i32 = 11;
const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2: i32 = 12;

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

unsafe extern "C" fn mouse_event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    let flags: CGEventFlags = CGEventGetFlags(event);

    if event_type == K_CG_EVENT_SCROLL_WHEEL {
        // deltaAxis1 = 垂直滚动量(正=向上),deltaAxis2 = 水平滚动量(正=向左)。
        // deltaAxis1 = vertical delta (positive = up), deltaAxis2 = horizontal delta (positive = left).
        let dy = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
        let dx = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
        log_info!("[mouse] scroll dy={} dx={} flags=0x{:x}", dy, dx, flags);
    } else {
        let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
        log_info!(
            "[mouse] {} button={}({}) flags=0x{:x}",
            event_type_name(event_type),
            button,
            button_name(button),
            flags
        );
    }

    // listen-only:原样透传,不修改事件。
    // listen-only: pass through unmodified.
    event
}

/// 启动鼠标事件监听线程。listen-only,HID 层,不修改任何事件。
/// Start the mouse event listener thread. Listen-only, HID level, no event modification.
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

    // HID 层 + listen-only:纯观察,调试安全。后续叠加 transformer 时切 default-tap。
    // HID level + listen-only: pure observation, debug-safe. Switch to default-tap when adding transformers.
    event_tap::start_event_tap_thread(
        tap_location::HID_EVENT_TAP,
        tap_placement::HEAD_INSERT,
        tap_options::LISTEN_ONLY,
        mask,
        Some(mouse_event_tap_callback),
        0,
        "mouse",
        || {
            log_info!("Mouse event tap started (listen-only).");
        },
    )
}
